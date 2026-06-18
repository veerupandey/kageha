//! Component-owned shared knowledge storage. No application tables are shared.
use crate::{
    scope::{valid_id, Binding, Scope},
    Document,
};
use kageha_contracts::execution::Control;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::Path,
};
fn sql(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}
fn invalid(s: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, s)
}
fn missing() -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        "Knowledge base or source is unavailable",
    )
}
fn conflict() -> io::Error {
    io::Error::new(
        io::ErrorKind::AlreadyExists,
        "Knowledge identity or revision changed",
    )
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Base {
    pub id: String,
    pub generation: u64,
    pub revision: u64,
    pub chunking: String,
}
#[derive(Clone, Debug)]
pub struct Source {
    pub name: String,
    pub sha256: String,
    pub bytes: Vec<u8>,
    pub revision: u64,
    pub mime: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct SourceMetadata {
    pub filename: String,
    pub sha256: String,
    pub bytes: u64,
    pub revision: u64,
    pub chunks: usize,
}
mod import;
pub use import::{ImportBase,ImportSource};
pub struct Store {
    db: Connection,
}
fn filename(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 240
        && !matches!(name, "." | "..")
        && !name
            .chars()
            .any(|c| c == '/' || c == '\\' || c.is_control())
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn text_source(name: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| {
            matches!(
                s.to_ascii_lowercase().as_str(),
                "txt"
                    | "md"
                    | "markdown"
                    | "csv"
                    | "json"
                    | "jsonl"
                    | "yaml"
                    | "yml"
                    | "xml"
                    | "rs"
                    | "py"
                    | "js"
                    | "ts"
                    | "tsx"
                    | "jsx"
                    | "go"
                    | "java"
                    | "c"
                    | "cpp"
                    | "h"
                    | "sql"
                    | "sh"
                    | "bash"
                    | "zsh"
                    | "toml"
                    | "html"
                    | "htm"
            )
        })
}
impl Store {
    pub fn open(path: &Path) -> io::Result<Self> {
        let parent = path
            .parent()
            .ok_or_else(|| invalid("Knowledge store requires a parent directory"))?;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
        let meta = fs::symlink_metadata(parent)?;
        if !meta.is_dir() || meta.file_type().is_symlink() || meta.permissions().mode() & 0o077 != 0
        {
            return Err(invalid("Knowledge store directory must be private"));
        }
        OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?
            .metadata()
            .and_then(|m| {
                if m.is_file() {
                    Ok(m)
                } else {
                    Err(invalid("Knowledge database must be a regular file"))
                }
            })?;
        let db = Connection::open(path).map_err(sql)?;
        db.busy_timeout(std::time::Duration::from_secs(2))
            .map_err(sql)?;
        db.execute_batch("PRAGMA journal_mode=WAL;PRAGMA synchronous=FULL;CREATE TABLE IF NOT EXISTS knowledge_schema(version INTEGER NOT NULL);").map_err(sql)?;
        let version: Option<u64> = db
            .query_row("SELECT version FROM knowledge_schema LIMIT 1", [], |r| {
                r.get(0)
            })
            .optional()
            .map_err(sql)?;
        if version.is_some_and(|v| v != 1) {
            return Err(invalid("Unsupported knowledge store version"));
        }
        db.execute_batch("BEGIN IMMEDIATE;INSERT INTO knowledge_schema SELECT 1 WHERE NOT EXISTS(SELECT 1 FROM knowledge_schema);
        CREATE TABLE IF NOT EXISTS bases(workspace TEXT NOT NULL,id TEXT NOT NULL,generation INTEGER NOT NULL,revision INTEGER NOT NULL,chunking TEXT NOT NULL,deleted INTEGER NOT NULL,PRIMARY KEY(workspace,id));
        CREATE TABLE IF NOT EXISTS sources(workspace TEXT NOT NULL,kb TEXT NOT NULL,generation INTEGER NOT NULL,name TEXT NOT NULL,revision INTEGER NOT NULL,hash TEXT NOT NULL,mime TEXT NOT NULL,body BLOB NOT NULL,deleted INTEGER NOT NULL,PRIMARY KEY(workspace,kb,generation,name));
        CREATE TABLE IF NOT EXISTS source_history(workspace TEXT NOT NULL,kb TEXT NOT NULL,generation INTEGER NOT NULL,name TEXT NOT NULL,revision INTEGER NOT NULL,hash TEXT NOT NULL,mime TEXT NOT NULL,body BLOB NOT NULL,deleted INTEGER NOT NULL,PRIMARY KEY(workspace,kb,generation,name,revision));
        CREATE TABLE IF NOT EXISTS source_stats(workspace TEXT NOT NULL,kb TEXT NOT NULL,generation INTEGER NOT NULL,name TEXT NOT NULL,revision INTEGER NOT NULL,chunks INTEGER NOT NULL,PRIMARY KEY(workspace,kb,generation,name,revision));
        CREATE TABLE IF NOT EXISTS extracted_chunks(workspace TEXT NOT NULL,kb TEXT NOT NULL,generation INTEGER NOT NULL,name TEXT NOT NULL,revision INTEGER NOT NULL,hash TEXT NOT NULL,chunks TEXT NOT NULL,PRIMARY KEY(workspace,kb,generation,name,revision));
        CREATE TABLE IF NOT EXISTS import_receipts(workspace TEXT NOT NULL,kb TEXT NOT NULL,source TEXT NOT NULL,digest TEXT NOT NULL,snapshot TEXT NOT NULL,PRIMARY KEY(workspace,kb));COMMIT;").map_err(sql)?;
        Ok(Self { db })
    }
    pub fn create(&mut self, workspace: &str, id: &str, chunking: &str) -> io::Result<Base> {
        if workspace.is_empty()
            || workspace.len() > 4096
            || !valid_id(id)
            || !matches!(chunking, "fixed" | "hierarchical")
        {
            return Err(invalid("Invalid shared knowledge base"));
        }
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(sql)?;
        let old: Option<(u64, bool)> = tx
            .query_row(
                "SELECT generation,deleted FROM bases WHERE workspace=? AND id=?",
                params![workspace, id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(sql)?;
        if old.is_some_and(|(_, deleted)| !deleted) {
            return Err(conflict());
        }
        let generation = old.map(|(g, _)| g + 1).unwrap_or(1);
        tx.execute("INSERT INTO bases VALUES (?,?,?,1,?,0) ON CONFLICT(workspace,id) DO UPDATE SET generation=excluded.generation,revision=1,chunking=excluded.chunking,deleted=0",params![workspace,id,generation,chunking]).map_err(sql)?;
        tx.commit().map_err(sql)?;
        self.base(workspace, id)
    }
    pub fn base(&self, workspace: &str, id: &str) -> io::Result<Base> {
        self.db.query_row("SELECT generation,revision,chunking FROM bases WHERE workspace=? AND id=? AND deleted=0",params![workspace,id],|r|Ok(Base{id:id.into(),generation:r.get(0)?,revision:r.get(1)?,chunking:r.get(2)?})).optional().map_err(sql)?.ok_or_else(missing)
    }
    pub fn binding(&self, workspace: &str, id: &str) -> io::Result<Binding> {
        let b = self.base(workspace, id)?;
        Ok(Binding {
            id: b.id,
            generation: b.generation,
        })
    }
    pub fn list(&self, workspace: &str) -> io::Result<Vec<Base>> {
        let mut stmt=self.db.prepare("SELECT id,generation,revision,chunking FROM bases WHERE workspace=? AND deleted=0 ORDER BY id LIMIT 1025").map_err(sql)?;
        let rows = stmt
            .query_map([workspace], |r| {
                Ok(Base {
                    id: r.get(0)?,
                    generation: r.get(1)?,
                    revision: r.get(2)?,
                    chunking: r.get(3)?,
                })
            })
            .map_err(sql)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql)?;
        if rows.len() > 1024 {
            return Err(invalid("Too many workspace knowledge bases"));
        }
        Ok(rows)
    }
    pub fn delete(&mut self, workspace: &str, id: &str) -> io::Result<()> {
        let base = self.base(workspace, id)?;
        let changed=self.db.execute("UPDATE bases SET deleted=1,revision=revision+1 WHERE workspace=? AND id=? AND generation=? AND deleted=0",params![workspace,id,base.generation]).map_err(sql)?;
        if changed != 1 {
            return Err(conflict());
        }
        Ok(())
    }
    pub fn put(
        &mut self,
        workspace: &str,
        id: &str,
        name: &str,
        bytes: &[u8],
        mime: &str,
        expected: Option<u64>,
    ) -> io::Result<Source> {
        if !filename(name) || bytes.len() > 16 * 1024 * 1024 || mime.len() > 256 {
            return Err(invalid("Invalid or oversized knowledge source"));
        }

        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(sql)?;
        let (generation, chunking): (u64, String) = tx
            .query_row(
                "SELECT generation,chunking FROM bases WHERE workspace=? AND id=? AND deleted=0",
                params![workspace, id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(sql)?
            .ok_or_else(missing)?;
        let chunk_count = if text_source(name) {
            let text =
                std::str::from_utf8(bytes).map_err(|_| invalid("Text knowledge must be UTF-8"))?;
            crate::chunk_document(&hash(bytes), text, &chunking)?.len()
        } else {
            0
        };
        let old:Option<(u64,String,bool,String)>=tx.query_row("SELECT revision,hash,deleted,mime FROM sources WHERE workspace=? AND kb=? AND generation=? AND name=?",params![workspace,id,generation,name],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(sql)?;
        if expected.is_some_and(|r| r != old.as_ref().map(|v| v.0).unwrap_or(0)) {
            return Err(conflict());
        }
        let hash = hash(bytes);
        if old
            .as_ref()
            .is_some_and(|(_, h, d, m)| h == &hash && !*d && m == mime)
        {
            tx.commit().map_err(sql)?;
            return self.source(workspace, id, name);
        }
        let totals:(u64,u64)=tx.query_row("SELECT count(*),COALESCE(sum(length(body)),0) FROM sources WHERE workspace=? AND kb=? AND generation=? AND deleted=0 AND name!=?",params![workspace,id,generation,name],|r|Ok((r.get(0)?,r.get(1)?))).map_err(sql)?;
        if totals.0 >= 512 || totals.1 + bytes.len() as u64 > 64 * 1024 * 1024 {
            return Err(invalid("Knowledge base exceeds source or byte limits"));
        }
        let revision = old.map(|v| v.0 + 1).unwrap_or(1);
        tx.execute("INSERT INTO sources VALUES (?,?,?,?,?,?,?,?,0) ON CONFLICT(workspace,kb,generation,name) DO UPDATE SET revision=excluded.revision,hash=excluded.hash,mime=excluded.mime,body=excluded.body,deleted=0",params![workspace,id,generation,name,revision,hash,mime,bytes]).map_err(sql)?;
        tx.execute("INSERT INTO source_history SELECT * FROM sources WHERE workspace=? AND kb=? AND generation=? AND name=?",params![workspace,id,generation,name]).map_err(sql)?;
        tx.execute(
            "INSERT INTO source_stats VALUES (?,?,?,?,?,?)",
            params![workspace, id, generation, name, revision, chunk_count],
        )
        .map_err(sql)?;
        tx.execute(
            "UPDATE bases SET revision=revision+1 WHERE workspace=? AND id=?",
            params![workspace, id],
        )
        .map_err(sql)?;
        tx.commit().map_err(sql)?;
        self.source(workspace, id, name)
    }
    pub fn source(&self, workspace: &str, id: &str, name: &str) -> io::Result<Source> {
        let base = self.base(workspace, id)?;
        let source=self.db.query_row("SELECT revision,hash,mime,body FROM sources WHERE workspace=? AND kb=? AND generation=? AND name=? AND deleted=0",params![workspace,id,base.generation,name],|r|Ok(Source{name:name.into(),revision:r.get(0)?,sha256:r.get(1)?,mime:r.get(2)?,bytes:r.get(3)?})).optional().map_err(sql)?.ok_or_else(missing)?;
        if source.bytes.len() > 16 * 1024 * 1024 || hash(&source.bytes) != source.sha256 {
            return Err(invalid("Stored knowledge bytes changed"));
        }
        Ok(source)
    }
    pub fn metadata(&self, workspace: &str, id: &str) -> io::Result<Vec<SourceMetadata>> {
        let base = self.base(workspace, id)?;
        let mut q=self.db.prepare("SELECT s.name,s.hash,length(s.body),s.revision,t.chunks FROM sources s LEFT JOIN source_stats t ON t.workspace=s.workspace AND t.kb=s.kb AND t.generation=s.generation AND t.name=s.name AND t.revision=s.revision WHERE s.workspace=? AND s.kb=? AND s.generation=? AND s.deleted=0 ORDER BY s.name LIMIT 513").map_err(sql)?;
        let rows = q
            .query_map(params![workspace, id, base.generation], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, u64>(2)?,
                    r.get::<_, u64>(3)?,
                    r.get::<_, Option<usize>>(4)?,
                ))
            })
            .map_err(sql)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql)?;
        if rows.len() > 512 {
            return Err(invalid("Too many knowledge sources"));
        }
        rows.into_iter()
            .map(|(name, sha256, bytes, revision, count)| {
                if bytes > 16 * 1024 * 1024 || revision == 0 {
                    return Err(invalid("Stored source metadata exceeds limits"));
                }
                let chunks = match count {
                    Some(n) if n <= 512 => n,
                    Some(_) => return Err(invalid("Stored chunk metadata exceeds limits")),
                    None if text_source(&name) => {
                        let source = self.source(workspace, id, &name)?;
                        crate::chunk_document(
                            &source.sha256,
                            std::str::from_utf8(&source.bytes).map_err(sql)?,
                            &base.chunking,
                        )?
                        .len()
                    }
                    None => 0,
                };
                Ok(SourceMetadata {
                    filename: name,
                    sha256,
                    bytes,
                    revision,
                    chunks,
                })
            })
            .collect()
    }
    pub fn names(&self, workspace: &str, id: &str) -> io::Result<Vec<String>> {
        let base = self.base(workspace, id)?;
        let mut q=self.db.prepare("SELECT name FROM sources WHERE workspace=? AND kb=? AND generation=? AND deleted=0 ORDER BY name LIMIT 513").map_err(sql)?;
        let rows = q
            .query_map(params![workspace, id, base.generation], |r| r.get(0))
            .map_err(sql)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql)?;
        if rows.len() > 512 {
            return Err(invalid("Too many knowledge sources"));
        }
        Ok(rows)
    }
    pub fn remove(&mut self, workspace: &str, id: &str, name: &str) -> io::Result<()> {
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(sql)?;
        let generation: u64 = tx
            .query_row(
                "SELECT generation FROM bases WHERE workspace=? AND id=? AND deleted=0",
                params![workspace, id],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql)?
            .ok_or_else(missing)?;
        let changed=tx.execute("UPDATE sources SET deleted=1,revision=revision+1 WHERE workspace=? AND kb=? AND generation=? AND name=? AND deleted=0",params![workspace,id,generation,name]).map_err(sql)?;
        if changed != 1 {
            return Err(missing());
        }
        tx.execute("INSERT INTO source_history SELECT * FROM sources WHERE workspace=? AND kb=? AND generation=? AND name=?",params![workspace,id,generation,name]).map_err(sql)?;
        tx.execute(
            "UPDATE bases SET revision=revision+1 WHERE workspace=? AND id=?",
            params![workspace, id],
        )
        .map_err(sql)?;
        tx.commit().map_err(sql)
    }
    pub fn scoped_source(&self, scope: &Scope, id: &str, name: &str) -> io::Result<Source> {
        scope.validate()?;
        let binding = scope.shared.iter().find(|b| b.id == id).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Knowledge base is outside this run's scope",
            )
        })?;
        let tx = self.db.unchecked_transaction().map_err(sql)?;
        let base = self.base(&scope.workspace, id)?;
        if base.generation != binding.generation {
            return Err(conflict());
        }
        let source = self.source(&scope.workspace, id, name)?;
        tx.commit().map_err(sql)?;
        Ok(source)
    }
    /// Reads the source and its retained extraction under the same generation fence.
    pub fn scoped_content(&self, scope: &Scope, id: &str, name: &str) -> io::Result<(Source, Vec<crate::Chunk>)> {
        scope.validate()?;
        let binding = scope.shared.iter().find(|b| b.id == id).ok_or_else(|| io::Error::new(io::ErrorKind::PermissionDenied, "Knowledge base is outside this run's scope"))?;
        let tx = self.db.unchecked_transaction().map_err(sql)?;
        let base = self.base(&scope.workspace, id)?;
        if base.generation != binding.generation { return Err(conflict()); }
        let source = self.source(&scope.workspace, id, name)?;
        let chunks = self.source_chunks(&scope.workspace, id, &source)?;
        tx.commit().map_err(sql)?;
        Ok((source, chunks))
    }
    pub fn documents(&self, scope: &Scope, control: &Control<'_>) -> io::Result<Vec<Document>> {
        scope.validate()?;
        let tx = self.db.unchecked_transaction().map_err(sql)?;
        let mut result = Vec::new();
        let mut bytes = 0usize;
        for binding in &scope.shared {
            control.check()?;
            let base = self.base(&scope.workspace, &binding.id)?;
            if base.generation != binding.generation {
                return Err(conflict());
            }
            for name in self.names(&scope.workspace, &binding.id)? {
                control.check()?;
                let source = self.source(&scope.workspace, &binding.id, &name)?;
                bytes = bytes.saturating_add(source.bytes.len());
                if bytes > 64 * 1024 * 1024 || result.len() >= 2048 {
                    return Err(invalid("Attached knowledge exceeds retrieval limits"));
                }
                let chunks = self.source_chunks(&scope.workspace,&binding.id,&source)?;
                // Scope and generation were checked against the owned store. Only
                // this authorized corpus is projected into the requesting actor.
                result.push(Document {
                    id: format!("shared/{}/{}/{}", binding.id, binding.generation, name),
                    owner: scope.agent.clone(),
                    source: format!("knowledge-bases/{}/sources/{}", binding.id, name),
                    sha256: source.sha256,
                    chunks,
                    tombstone: false,
                });
            }
        }
        control.check()?;
        tx.commit().map_err(sql)?;
        Ok(result)
    }
}
