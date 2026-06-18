//! Restartable import receipts commit with data in the component's own store.
use super::*;
use crate::Chunk;
use serde_json::{json, Value};
#[derive(Clone, Debug)]
pub struct ImportSource {
    pub name: String,
    pub bytes: Vec<u8>,
    pub mime: String,
    pub chunks: Vec<Chunk>,
}
#[derive(Clone, Debug)]
pub struct ImportBase {
    pub id: String,
    pub source_id: String,
    pub source_digest: String,
    pub manifest: Value,
    pub files: Vec<ImportSource>,
}
fn snapshot(base: &ImportBase) -> io::Result<Value> {
    if !valid_id(&base.id)
        || base.source_id.len() > 256
        || base.source_id.is_empty()
        || base.source_digest.len() != 64
        || !base.source_digest.bytes().all(|b| b.is_ascii_hexdigit())
        || base.files.len() > 512
    {
        return Err(invalid("Invalid knowledge import identity"));
    }
    let mut names = std::collections::BTreeSet::new();
    let mut total = 0usize;
    let mut files = Vec::new();
    for file in &base.files {
        if !filename(&file.name)
            || !names.insert(&file.name)
            || file.bytes.len() > 16 * 1024 * 1024
            || file.mime.len() > 256
        {
            return Err(invalid("Invalid imported knowledge source"));
        }
        let index = validate_chunks(&file.chunks)?;
        total = total
            .saturating_add(file.bytes.len())
            .saturating_add(index.len());
        if total > 64 * 1024 * 1024 {
            return Err(invalid("Knowledge import exceeds byte limits"));
        }
        files.push(json!({"name":file.name,"sha256":hash(&file.bytes),"mime":file.mime,"chunks_sha256":hash(&index),"chunks":file.chunks.len()}));
    }
    if serde_json::to_vec(&base.manifest)?.len() > 2 * 1024 * 1024 {
        return Err(invalid("Knowledge manifest exceeds limits"));
    }
    Ok(json!({"format_version":1,"manifest":base.manifest,"files":files}))
}
pub(super) fn validate_chunks(chunks: &[Chunk]) -> io::Result<Vec<u8>> {
    let mut ids = std::collections::BTreeSet::new();
    if chunks.len() > 512 {
        return Err(invalid("Too many extracted chunks"));
    }
    for c in chunks {
        if c.id.is_empty()
            || c.id.len() > 256
            || c.id.chars().any(char::is_control)
            || !ids.insert(&c.id)
            || c.text.len() > 256 * 1024
            || c.heading.len() > 4096
            || c.parent_id.as_ref().is_some_and(|s| s.len() > 4096)
        {
            return Err(invalid("Invalid extracted knowledge chunk"));
        }
    }
    let bytes = serde_json::to_vec(chunks)?;
    if bytes.len() > 16 * 1024 * 1024 {
        return Err(invalid("Extracted index exceeds limits"));
    }
    Ok(bytes)
}
fn verify(db: &Connection, workspace: &str, base: &ImportBase, expected: &Value) -> io::Result<()> {
    let row: Option<(String, String, String)> = db
        .query_row(
            "SELECT source,digest,snapshot FROM import_receipts WHERE workspace=? AND kb=?",
            params![workspace, base.id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(sql)?;
    let Some((source, digest, value)) = row else {
        return Err(missing());
    };
    if source != base.source_id
        || digest != base.source_digest
        || serde_json::from_str::<Value>(&value)? != *expected
    {
        return Err(conflict());
    }
    // Imported generation-one revisions remain immutable even after operator edits
    // or deleting/recreating a base. Verify them without restoring active rows.
    for file in &base.files {
        let row:Option<(String,Vec<u8>)>=db.query_row("SELECT hash,body FROM source_history WHERE workspace=? AND kb=? AND generation=1 AND name=? AND revision=1",params![workspace,base.id,file.name],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(sql)?;
        if row != Some((hash(&file.bytes), file.bytes.clone())) {
            return Err(invalid("Imported historical source changed"));
        }
        let index:Option<(String,String)>=db.query_row("SELECT hash,chunks FROM extracted_chunks WHERE workspace=? AND kb=? AND generation=1 AND name=? AND revision=1",params![workspace,base.id,file.name],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(sql)?;
        let expected_chunks = String::from_utf8(validate_chunks(&file.chunks)?).map_err(sql)?;
        if index != Some((hash(&file.bytes), expected_chunks)) {
            return Err(invalid("Imported extraction provenance changed"));
        }
    }
    Ok(())
}
impl Store {
    /// Returns false for an already committed import. Never overwrites native data.
    pub fn import_base(&mut self, workspace: &str, base: &ImportBase) -> io::Result<bool> {
        if workspace.is_empty() || workspace.len() > 4096 || workspace.chars().any(char::is_control)
        {
            return Err(invalid("Invalid knowledge workspace identity"));
        }
        let expected = snapshot(base)?;
        let chunking = base.manifest["chunking"].as_str().unwrap_or("hierarchical");
        if !matches!(chunking, "fixed" | "hierarchical") {
            return Err(invalid("Unsupported imported chunking mode"));
        }
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(sql)?;
        let prior: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM import_receipts WHERE workspace=? AND kb=?)",
                params![workspace, base.id],
                |r| r.get(0),
            )
            .map_err(sql)?;
        if prior {
            verify(&tx, workspace, base, &expected)?;
            tx.commit().map_err(sql)?;
            return Ok(false);
        }
        let used: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM bases WHERE workspace=? AND id=?)",
                params![workspace, base.id],
                |r| r.get(0),
            )
            .map_err(sql)?;
        if used {
            return Err(conflict());
        }
        tx.execute(
            "INSERT INTO bases VALUES (?,?,1,1,?,0)",
            params![workspace, base.id, chunking],
        )
        .map_err(sql)?;
        for file in &base.files {
            let hash = hash(&file.bytes);
            let chunks = String::from_utf8(validate_chunks(&file.chunks)?).map_err(sql)?;
            tx.execute(
                "INSERT INTO sources VALUES (?,?,1,?,1,?,?,?,0)",
                params![workspace, base.id, file.name, hash, file.mime, file.bytes],
            )
            .map_err(sql)?;
            tx.execute("INSERT INTO source_history SELECT * FROM sources WHERE workspace=? AND kb=? AND generation=1 AND name=?",params![workspace,base.id,file.name]).map_err(sql)?;
            tx.execute(
                "INSERT INTO source_stats VALUES (?,?,1,?,1,?)",
                params![workspace, base.id, file.name, file.chunks.len()],
            )
            .map_err(sql)?;
            tx.execute(
                "INSERT INTO extracted_chunks VALUES (?,?,1,?,1,?,?)",
                params![workspace, base.id, file.name, hash, chunks],
            )
            .map_err(sql)?;
        }
        tx.execute(
            "INSERT INTO import_receipts VALUES (?,?,?,?,?)",
            params![
                workspace,
                base.id,
                base.source_id,
                base.source_digest,
                expected.to_string()
            ],
        )
        .map_err(sql)?;
        verify(&tx, workspace, base, &expected)?;
        tx.commit().map_err(sql)?;
        Ok(true)
    }
    pub fn verify_import(&self, workspace: &str, base: &ImportBase) -> io::Result<()> {
        if workspace.is_empty() || workspace.len() > 4096 || workspace.chars().any(char::is_control)
        {
            return Err(invalid("Invalid knowledge workspace identity"));
        }
        let expected = snapshot(base)?;
        let tx = self.db.unchecked_transaction().map_err(sql)?;
        verify(&tx, workspace, base, &expected)?;
        tx.commit().map_err(sql)
    }
    pub fn source_chunks(
        &self,
        workspace: &str,
        id: &str,
        source: &Source,
    ) -> io::Result<Vec<Chunk>> {
        let base = self.base(workspace, id)?;
        let row:Option<(String,String)>=self.db.query_row("SELECT hash,chunks FROM extracted_chunks WHERE workspace=? AND kb=? AND generation=? AND name=? AND revision=?",params![workspace,id,base.generation,source.name,source.revision],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(sql)?;
        if let Some((hash, index)) = row {
            if hash != source.sha256 || index.len() > 16 * 1024 * 1024 {
                return Err(invalid("Extracted source identity changed"));
            }
            let chunks: Vec<Chunk> = serde_json::from_str(&index)?;
            validate_chunks(&chunks)?;
            return Ok(chunks);
        }
        if text_source(&source.name) {
            crate::chunk_document(
                &source.sha256,
                std::str::from_utf8(&source.bytes).map_err(sql)?,
                &base.chunking,
            )
        } else {
            Ok(vec![])
        }
    }
}
