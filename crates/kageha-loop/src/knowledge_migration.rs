//! Thin WebUI projection of portable private knowledge, using owned blob receipts.
use kageha_migration::documents::Document;
use rusqlite::{params, Connection};
use serde_json::json;
use std::{io, path::Path, sync::atomic::AtomicBool};
pub(crate) fn apply(db: &Connection, d: &Document) -> io::Result<()> {
    let value = d
        .value
        .as_ref()
        .ok_or_else(|| io::Error::other("Knowledge snapshot is missing"))?;
    if crate::ui_store::get(db, "agent", &d.id)
        .map_err(io::Error::other)?
        .is_none()
    {
        return Err(io::Error::other(
            "Import the agent definition before its knowledge",
        ));
    }
    let kind = format!("knowledge:{}", d.id);
    let existing = crate::ui_store::list(db, &kind).map_err(io::Error::other)?;
    let kb = crate::ui_agents::knowledge_chat(&d.id);
    db.execute("INSERT OR IGNORE INTO ui_sessions(id,title,created_at,updated_at) VALUES (?,'Agent knowledge',?,?)",params![kb,crate::ui_store::now(),crate::ui_store::now()]).map_err(io::Error::other)?;
    db.execute(
        "INSERT OR IGNORE INTO ui_internal_sessions VALUES (?)",
        [&kb],
    )
    .map_err(io::Error::other)?;
    for file in value["files"]
        .as_array()
        .ok_or_else(|| io::Error::other("Knowledge files are missing"))?
    {
        let name = file["filename"]
            .as_str()
            .ok_or_else(|| io::Error::other("Knowledge filename missing"))?;
        if existing.iter().any(|v| v["filename"] == name) {
            return Err(io::Error::other(
                "Existing knowledge source cannot be overwritten",
            ));
        }
        let hash = file["sha256"]
            .as_str()
            .ok_or_else(|| io::Error::other("Knowledge hash missing"))?;
        let source = file["owned_path"]
            .as_str()
            .ok_or_else(|| io::Error::other("Knowledge owned path missing"))?;
        kageha_workspace::file_snapshot(
            Path::new(source),
            Some(hash),
            &kageha_contracts::execution::Control {
                cancelled: &AtomicBool::new(false),
                deadline: None,
            },
        )
        .map_err(io::Error::other)?;
        let path = format!("inputs/migrated-{hash}-{name}");
        let metadata = json!({"path":path,"filename":name,"sha256":hash,"bytes":file["bytes"],"content_type":crate::ui_uploads::mime(name),"source_kb_id":value["manifest"]["id"],"source_digest":d.source_digest,"indexed":file["chunks"].as_array().is_some_and(|c|!c.is_empty()),"retrieval_engine":"rust-lexical"});
        crate::ui_store::put(db, &kind, &path, metadata).map_err(io::Error::other)?;
        crate::ui_store::put(
            db,
            &format!("knowledge-index:{}", d.id),
            &path,
            json!({"sha256":hash,"chunks":file["chunks"]}),
        )
        .map_err(io::Error::other)?;
        db.execute(
            "INSERT INTO ui_legacy_files VALUES (?,?,?,?)",
            params![kb, path, source, hash],
        )
        .map_err(io::Error::other)?;
    }
    Ok(())
}

struct WorkspaceProjection {
    store:kageha_knowledge::store::Store,
    workspace:String,
}
fn shared_base(d:&Document)->io::Result<kageha_knowledge::store::ImportBase>{
    let value=d.value.as_ref().ok_or_else(||io::Error::other("Missing knowledge snapshot"))?;
    let mut files=Vec::new();
    for file in value["files"].as_array().ok_or_else(||io::Error::other("Missing knowledge sources"))? {
        let name=file["filename"].as_str().ok_or_else(||io::Error::other("Missing source name"))?;
        let path=file["owned_path"].as_str().ok_or_else(||io::Error::other("Missing owned source path"))?;
        let hash=file["sha256"].as_str().ok_or_else(||io::Error::other("Missing source hash"))?;
        let snapshot=kageha_workspace::file_snapshot(Path::new(path),Some(hash),&kageha_contracts::execution::Control{cancelled:&AtomicBool::new(false),deadline:None}).map_err(io::Error::other)?;
        files.push(kageha_knowledge::store::ImportSource{name:name.into(),bytes:snapshot.bytes,mime:crate::ui_uploads::mime(name).into(),chunks:serde_json::from_value(file["chunks"].clone())?});
    }
    Ok(kageha_knowledge::store::ImportBase{id:d.id.clone(),source_id:d.source_id.clone(),source_digest:d.source_digest.clone(),manifest:value["manifest"].clone(),files})
}
impl kageha_migration::documents::DocumentSink for WorkspaceProjection {
    fn apply(&mut self,d:&Document)->io::Result<()> {self.store.import_base(&self.workspace,&shared_base(d)?)?;Ok(())}
    fn verify(&mut self,d:&Document)->io::Result<()> {self.store.verify_import(&self.workspace,&shared_base(d)?)}
}
pub(crate) fn run_workspace(source:&Path,state:&Path,workspace:&Path)->io::Result<kageha_migration::Report>{
    let destination=crate::history_migration::destination(source,state)?;
    let mut projection=WorkspaceProjection{store:kageha_knowledge::store::Store::open(&destination.state.join("components/knowledge/knowledge.sqlite"))?,workspace:crate::ui_knowledge::workspace_id(workspace)?};
    kageha_migration::Importer::open(&destination.source,&destination.state.join(format!("document-import-workspace-knowledge-v1-{}",projection.workspace.trim_start_matches("workspace:"))))?.run_documents(kageha_migration::documents::Domain::WorkspaceKnowledge,&mut projection)
}
