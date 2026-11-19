//! Parent-owned worker handoffs. The proposal bytes remain in the private store;
//! public projections carry bounded change previews, conflicts, and receipt state.
use crate::{ui_jobs, ui_uploads, ui_workspace, webui::Database};
use kageha_contracts::execution::Control;
use kageha_workspace::handoff::{self, FileMapping, Proposal};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{io, path::Path, sync::atomic::{AtomicBool, Ordering}, time::{Duration, Instant}};

/// A project receipt proves an artifact only while the exact applied files remain
/// present in the original destination and belong to this chat and turn.
pub(crate) fn evidence(db: &Database, chat: &str, turn: &str, id: &str) -> io::Result<bool> {
    let encoded: Option<String> = {
        let c = db.lock().map_err(|_| io::Error::other("Handoff evidence unavailable"))?;
        c.query_row("SELECT proposal FROM ui_handoffs WHERE id=? AND owner_chat=? AND owner_turn=? AND target='project' AND status='applied'",
            params![id,chat,turn], |row| row.get(0)).optional().map_err(io::Error::other)?
    };
    let Some(encoded) = encoded else { return Ok(false) };
    let proposal: Proposal = serde_json::from_str(&encoded).map_err(io::Error::other)?;
    Ok(!proposal.changes.is_empty() && handoff::verify_applied(&proposal).unwrap_or(false))
}

pub(crate) fn initialize(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS ui_handoffs(
        id TEXT PRIMARY KEY, owner_chat TEXT NOT NULL, owner_turn TEXT NOT NULL,
        job_id TEXT NOT NULL, target TEXT NOT NULL, proposal TEXT NOT NULL,
        summary TEXT NOT NULL, status TEXT NOT NULL, receipt TEXT,
        created INTEGER NOT NULL DEFAULT (unixepoch()));
        CREATE INDEX IF NOT EXISTS ui_handoffs_job ON ui_handoffs(job_id,created);
        UPDATE ui_handoffs SET status='uncertain' WHERE status='applying';")?;
    // Reconcile interrupted mutations before accepting new work. The core restores
    // only verified prior bytes and refuses to overwrite any externally changed
    // destination. A committed operation stays uncertain until its receipt and
    // any task-workspace artifacts are registered by the owning chat.
    let mut cursor=0_i64;
    loop {
        let pending: Vec<(i64,String)> = {
            let mut query=db.prepare("SELECT rowid,id FROM ui_handoffs WHERE status='uncertain' AND rowid>? ORDER BY rowid LIMIT 128")?;
            let rows=query.query_map([cursor],|r|Ok((r.get(0)?,r.get(1)?)))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        if pending.is_empty() { break; }
        for (row,id) in pending {
            cursor=row;
            if let Ok(dir)=journal_path(db,&id) {
                if handoff::recover(&dir).is_ok_and(|recovery|recovery.state=="rolled_back") {
                    db.execute("UPDATE ui_handoffs SET status='rolled_back' WHERE id=? AND status='uncertain'",[id])?;
                }
            }
        }
    }
    Ok(())
}

/// The job projection contains only public summary data, never proposal contents.
pub(crate) fn latest(db: &Connection, job: &str) -> rusqlite::Result<Value> {
    let row = db.query_row(
        "SELECT id,target,summary,status FROM ui_handoffs WHERE job_id=? ORDER BY rowid DESC LIMIT 1",
        [job], |r| Ok((r.get::<_, String>(0)?,r.get::<_, String>(1)?,r.get::<_, String>(2)?,r.get::<_, String>(3)?)),
    ).optional();
    let row = match row {
        Ok(row) => row,
        Err(rusqlite::Error::SqliteFailure(_, Some(message))) if message.contains("no such table: ui_handoffs") => None,
        Err(error) => return Err(error),
    };
    let Some((id,target,summary,status)) = row else { return Ok(Value::Null) };
    let mut value: Value = serde_json::from_str(&summary).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    value["preview_id"] = json!(id); value["target"] = json!(target); value["status"] = json!(status);
    Ok(value)
}

fn publish(db: &Database, job: &str, turn: &str) -> io::Result<()> {
    let connection = db.lock().map_err(|_| io::Error::other("Database unavailable"))?;
    crate::ui_session_events::publish_worker(&connection, job, turn).map_err(io::Error::other)?;
    drop(connection);
    crate::ui_session_events::notify();
    Ok(())
}

fn owned_job(db: &Database, chat: &str, id: &str) -> io::Result<Value> {
    let job = ui_jobs::get(db, id).map_err(|(_,e)| io::Error::other(e))?;
    if job["parent_session"] != chat {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied,"Worker belongs to another chat"));
    }
    if job["state"] != "succeeded" {
        return Err(io::Error::other("Only a successfully completed worker can be handed off"));
    }
    Ok(job)
}

pub(crate) fn preview(db: &Database, chat: &str, turn: &str, project: &Path, depth: u32,
    remote: &Value, args: &Value, cancel: &AtomicBool) -> io::Result<Value> {
    let job_id = args["job_id"].as_str().filter(|s| !s.is_empty()).ok_or_else(||io::Error::other("Worker ID is required"))?;
    let target = args["target"].as_str().ok_or_else(||io::Error::other("Handoff target is required"))?;
    let job = owned_job(db,chat,job_id)?;
    let child = job["session_id"].as_str().ok_or_else(||io::Error::other("Worker session missing"))?;
    let source = ui_workspace::existing_root(db,child)?.ok_or_else(||io::Error::other("Worker workspace unavailable"))?;
    let proposal = match target {
        "workspace" => {
            let mut files: Vec<FileMapping> = serde_json::from_value(args["files"].clone()).map_err(|_|io::Error::other("Explicit file mappings are required"))?;
            if files.is_empty() { return Err(io::Error::other("Explicit file mappings are required")); }
            let mut pinned_hashes=Vec::with_capacity(files.len());
            for file in &mut files {
                if let Some(relative) = file.source.strip_prefix("workspace/") { file.source = relative.to_owned(); }
                if let Some(relative) = file.destination.strip_prefix("workspace/") { file.destination = relative.to_owned(); }
                let artifact = ui_workspace::artifact_path(&file.source).ok_or_else(||io::Error::other("Invalid worker artifact path"))?;
                let (_,metadata)=ui_uploads::pinned(db,child,&artifact)?;
                pinned_hashes.push(metadata["sha256"].as_str().ok_or_else(||io::Error::other("Worker artifact hash missing"))?.to_owned());
            }
            let destination = ui_workspace::open_for_chat(db,chat)?;
            let proposal=handoff::preview_files(&source,destination.root(),&files)?;
            verify_pinned(&proposal,&files,&pinned_hashes)?;
            proposal
        }
        "project" => {
            if depth != 0 || !remote.is_null() { return Err(io::Error::new(io::ErrorKind::PermissionDenied,"Project handoff is only available to a local owning chat")); }
            if !crate::ui_shell::escalated_write_root_allows(project)? { return Err(io::Error::new(io::ErrorKind::PermissionDenied,"Project is outside the grantable write roots")); }
            if job["workspace"]["mode"] != "worktree" { return Err(io::Error::other("Project handoff requires a worker Git worktree")); }
            let source_project = job["workspace"]["source_root"].as_str().ok_or_else(||io::Error::other("Worker source binding missing"))?;
            if Path::new(source_project).canonicalize()? != project.canonicalize()? { return Err(io::Error::new(io::ErrorKind::PermissionDenied,"Worker belongs to another project")); }
            let base = job["workspace"]["operation"]["handle"]["head"].as_str().ok_or_else(||io::Error::other("Worker base revision unavailable"))?;
            let control = Control { cancelled:cancel, deadline:Some(Instant::now()+Duration::from_secs(30)) };
            handoff::preview_git(&source,project,base,&control)?
        }
        _ => return Err(io::Error::other("Invalid handoff target")),
    };
    let review_required = job["spec"]["review_patch"] == true;
    let status = if proposal.has_conflicts() {"conflict"} else if review_required {"review_required"} else {"preview"};
    let id = uuid::Uuid::new_v4().to_string();
    let mut summary = proposal.summary();
    summary["review_required"] = json!(review_required);
    let encoded = serde_json::to_string(&proposal)?;
    {
        let connection = db.lock().map_err(|_|io::Error::other("Database unavailable"))?;
        connection.execute("INSERT INTO ui_handoffs(id,owner_chat,owner_turn,job_id,target,proposal,summary,status) VALUES(?,?,?,?,?,?,?,?)",
            params![id,chat,turn,job_id,target,encoded,summary.to_string(),status]).map_err(io::Error::other)?;
    }
    publish(db,job_id,turn)?;
    let mut result = summary; result["preview_id"] = json!(id); result["job_id"] = json!(job_id); result["target"] = json!(target); result["status"] = json!(status);
    Ok(result)
}

fn verify_pinned(proposal: &Proposal, mappings: &[FileMapping], hashes: &[String]) -> io::Result<()> {
    if proposal.changes.len()!=mappings.len() || hashes.len()!=mappings.len() || proposal.changes.iter().zip(mappings).zip(hashes).any(|((change,mapping),hash)|
        change.source!=mapping.source || change.path!=mapping.destination || change.after.as_ref().is_none_or(|after|after.sha256!=*hash)) {
        return Err(io::Error::other("Worker artifact changed after registration; refresh its artifact before handoff"));
    }
    Ok(())
}

/// Scope of an existing preview, used to enforce the same tool permissions as a
/// direct write before dispatch. Neither model-provided paths nor a forged ID
/// may select a different scope.
pub(crate) fn target(db: &Database, chat: &str, id: &str) -> io::Result<(String,String)> {
    let c = db.lock().map_err(|_|io::Error::other("Database unavailable"))?;
    c.query_row("SELECT target,status FROM ui_handoffs WHERE id=? AND owner_chat=?",params![id,chat],|r|Ok((r.get(0)?,r.get(1)?)))
        .optional().map_err(io::Error::other)?.ok_or_else(||io::Error::new(io::ErrorKind::PermissionDenied,"Handoff not found for this chat"))
}

/// Reject review-only handoffs before asking for any write approval.
pub(crate) fn require_applicable(db: &Database, chat: &str, id: &str) -> io::Result<()> {
    let c = db.lock().map_err(|_|io::Error::other("Database unavailable"))?;
    let row: Option<(String,bool,bool)> = c.query_row(
        "SELECT h.status, COALESCE(json_extract(s.spec,'$.review_patch'),0), COALESCE(json_extract(h.summary,'$.review_required'),0) FROM ui_handoffs h LEFT JOIN ui_jobs j ON j.id=h.job_id LEFT JOIN ui_run_specs s ON s.chat_id=j.child_chat AND s.request_id='job' WHERE h.id=? AND h.owner_chat=?",
        params![id,chat], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(io::Error::other)?;
    let (status, worker_review, preview_review) = row.ok_or_else(||io::Error::new(io::ErrorKind::PermissionDenied,"Handoff not found for this chat"))?;
    if status == "review_required" || worker_review || preview_review {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied,
            "This patch is retained for human review and cannot be applied by the agent."));
    }
    Ok(())
}

pub(crate) fn summary(db: &Database, chat: &str, id: &str) -> io::Result<Value> {
    let c = db.lock().map_err(|_|io::Error::other("Database unavailable"))?;
    let raw: String = c.query_row("SELECT summary FROM ui_handoffs WHERE id=? AND owner_chat=?",params![id,chat],|r|r.get(0))
        .optional().map_err(io::Error::other)?.ok_or_else(||io::Error::new(io::ErrorKind::PermissionDenied,"Handoff not found for this chat"))?;
    serde_json::from_str(&raw).map_err(io::Error::other)
}

fn journal_path(db: &Connection, id: &str) -> io::Result<std::path::PathBuf> {
    uuid::Uuid::parse_str(id).map_err(|_|io::Error::other("Invalid handoff identity"))?;
    let state=Path::new(db.path().ok_or_else(||io::Error::other("Persistent handoff state required"))?)
        .parent().ok_or_else(||io::Error::other("Handoff state directory missing"))?;
    Ok(state.join("components/workspace/handoffs").join(id))
}

fn journal_dir(db: &Database, id: &str) -> io::Result<std::path::PathBuf> {
    let c=db.lock().map_err(|_|io::Error::other("Database unavailable"))?;
    journal_path(&c,id)
}

pub(crate) fn apply(db: &Database, chat: &str, turn: &str, project: &Path, args: &Value,
    call: &Value, cancel: &AtomicBool) -> io::Result<Value> {
    let id = args["preview_id"].as_str().filter(|s| !s.is_empty()).ok_or_else(||io::Error::other("Preview ID is required"))?;
    let (job,target,encoded,status,receipt,original_turn): (String,String,String,String,Option<String>,String) = {
        let c = db.lock().map_err(|_|io::Error::other("Database unavailable"))?;
        c.query_row("SELECT job_id,target,proposal,status,receipt,owner_turn FROM ui_handoffs WHERE id=? AND owner_chat=?",params![id,chat],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional().map_err(io::Error::other)?
            .ok_or_else(||io::Error::new(io::ErrorKind::PermissionDenied,"Handoff not found for this chat"))?
    };
    require_applicable(db,chat,id)?;
    if status == "applied" { return serde_json::from_str(receipt.as_deref().ok_or_else(||io::Error::other("Handoff receipt missing"))?).map_err(io::Error::other); }
    if status == "conflict" { return Err(io::Error::other("Handoff has conflicts; no changes were applied")); }
    let job_row = owned_job(db,chat,&job)?;
    if target == "project" {
        let source = job_row["workspace"]["source_root"].as_str().ok_or_else(||io::Error::other("Worker source binding missing"))?;
        if Path::new(source).canonicalize()? != project.canonicalize()? { return Err(io::Error::new(io::ErrorKind::PermissionDenied,"Project binding changed")); }
        if !crate::ui_shell::escalated_write_root_allows(project)? { return Err(io::Error::new(io::ErrorKind::PermissionDenied,"Project is outside the grantable write roots")); }
    }
    if status == "applying" { return Err(kageha_contracts::execution::uncertain_stop("Handoff is already being applied; inspect its receipt")); }
    if status == "uncertain" {
        let recovery=handoff::recover(&journal_dir(db,id)?).map_err(|_|kageha_contracts::execution::uncertain_stop("Handoff recovery could not verify the destination; inspect the original receipt"))?;
        if recovery.state == "committed" {
            let mut value=json!({"preview_id":id,"job_id":job,"target":target,"status":"applied","paths":recovery.paths,"recovered":true});
            if target=="project" { value["evidence_reference"]=json!(format!("handoff:{id}")); }
            if target=="workspace" {
                let workspace=ui_workspace::open_for_chat(db,chat)?;
                let changes=workspace.scan_changes().map_err(io::Error::other)?;
                value["artifacts"]=json!(ui_workspace::register_changes(db,chat,&original_turn,&changes)?);
            }
            let c=db.lock().map_err(|_|io::Error::other("Database unavailable"))?;
            c.execute("UPDATE ui_handoffs SET status='applied',receipt=? WHERE id=? AND status='uncertain'",params![value.to_string(),id]).map_err(io::Error::other)?;
            drop(c); publish(db,&job,turn)?; return Ok(value);
        }
        if recovery.state == "rolled_back" {
            let c=db.lock().map_err(|_|io::Error::other("Database unavailable"))?;
            c.execute("UPDATE ui_handoffs SET status='rolled_back' WHERE id=? AND status='uncertain'",[id]).map_err(io::Error::other)?;
            drop(c); publish(db,&job,turn)?;
            return Err(io::Error::other("Interrupted handoff was rolled back. Create a fresh preview before trying again."));
        }
        return Err(kageha_contracts::execution::uncertain_stop("Handoff recovery remains uncertain; inspect the original receipt"));
    }
    if status != "preview" { return Err(io::Error::other("Handoff is not available for application; create a fresh preview")); }
    if cancel.load(Ordering::Relaxed) { return Err(io::Error::new(io::ErrorKind::Interrupted,"Turn cancelled")); }
    let proposal: Proposal = serde_json::from_str(&encoded)?;
    let transaction_dir=journal_dir(db,id)?;
    let scope = json!({"preview_id":id,"job_id":job,"target":target});
    crate::ui_effects::run(db,turn,call,&scope,|| {
        {
            let c = db.lock().map_err(|_|io::Error::other("Database unavailable"))?;
            if c.execute("UPDATE ui_handoffs SET status='applying' WHERE id=? AND owner_chat=? AND status='preview'",params![id,chat]).map_err(io::Error::other)? != 1 {
                return Err(kageha_contracts::execution::uncertain_stop("Handoff already dispatched; inspect its receipt"));
            }
        }
        let _ = publish(db,&job,turn);
        let result = (|| {
            let receipt = handoff::apply_with_journal(&proposal,&transaction_dir)?;
            let mut value = serde_json::to_value(receipt)?;
            value["preview_id"] = json!(id); value["job_id"] = json!(job); value["target"] = json!(target); value["status"] = json!("applied");
            if target == "project" { value["evidence_reference"] = json!(format!("handoff:{id}")); }
            if target == "workspace" {
                let workspace = ui_workspace::open_for_chat(db,chat)?;
                let changes = workspace.scan_changes().map_err(io::Error::other)?;
                value["artifacts"] = json!(ui_workspace::register_changes(db,chat,&original_turn,&changes)?);
            }
            Ok::<Value,io::Error>(value)
        })();
        match result {
            Ok(value) => {
                let c = db.lock().map_err(|_|kageha_contracts::execution::uncertain_stop("Handoff applied but its receipt could not be saved"))?;
                c.execute("UPDATE ui_handoffs SET status='applied',receipt=? WHERE id=? AND status='applying'",params![value.to_string(),id])
                    .map_err(|_|kageha_contracts::execution::uncertain_stop("Handoff applied but its receipt could not be saved"))?;
                drop(c); publish(db,&job,turn)?; Ok(value)
            }
            Err(error) => {
                // The core writes a durable journal before touching the destination.
                // A missing journal proves this failed during preflight, so callers
                // can review the stale/conflicting state and make a fresh preview.
                let no_journal=matches!(std::fs::symlink_metadata(&transaction_dir),Err(ref e) if e.kind()==io::ErrorKind::NotFound);
                if let Ok(c) = db.lock() {
                    if no_journal {
                        let mut summary: Value=c.query_row("SELECT summary FROM ui_handoffs WHERE id=?",[id],|r|r.get::<_,String>(0)).ok()
                            .and_then(|raw|serde_json::from_str(&raw).ok()).unwrap_or_else(||json!({"files":[],"conflicts":[]}));
                        if let Some(conflicts)=summary["conflicts"].as_array_mut() { conflicts.push(json!("Handoff preflight failed; review the current source and destination and create a fresh preview.")); }
                        let _=c.execute("UPDATE ui_handoffs SET status='conflict',summary=? WHERE id=? AND status='applying'",params![summary.to_string(),id]);
                    } else { let _=c.execute("UPDATE ui_handoffs SET status='uncertain' WHERE id=? AND status='applying'",[id]); }
                }
                let _ = publish(db,&job,turn);
                if no_journal {Err(io::Error::other(format!("Handoff preflight failed without applying changes: {error}. Create a fresh preview.")))}
                else {Err(kageha_contracts::execution::uncertain_stop(&format!("Handoff did not complete cleanly: {error}. Inspect the destination and receipt before retrying.")))}
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_exposes_only_public_summary_and_survives_old_databases() {
        let db=Connection::open_in_memory().unwrap();
        assert!(latest(&db,"worker").unwrap().is_null());
        initialize(&db).unwrap();
        db.execute("INSERT INTO ui_handoffs(id,owner_chat,owner_turn,job_id,target,proposal,summary,status) VALUES(?1,'owner','turn','worker','workspace',?2,?3,'preview')",
            params!["preview-1","sensitive file contents",json!({"files":[{"path":"result.py","change":"add"}],"conflicts":[]}).to_string()]).unwrap();
        let result=latest(&db,"worker").unwrap();
        assert_eq!(result["preview_id"],"preview-1");
        assert_eq!(result["files"][0]["path"],"result.py");
        assert!(!result.to_string().contains("sensitive file contents"));
        assert!(latest(&db,"other").unwrap().is_null());
    }

    #[test]
    fn review_patch_preview_is_visible_and_cannot_apply_even_if_status_is_preview() {
        let dir=tempfile::tempdir().unwrap();
        let source=dir.path().join("source"); let target=dir.path().join("target");
        std::fs::create_dir(&source).unwrap(); std::fs::create_dir(&target).unwrap();
        std::fs::write(source.join("fix.rs"),"fixed\n").unwrap();
        let proposal=handoff::preview_files(&source,&target,&[FileMapping{source:"fix.rs".into(),destination:"fix.rs".into()}]).unwrap();
        let mut summary=proposal.summary(); summary["review_required"]=json!(true);
        let connection=Connection::open_in_memory().unwrap(); initialize(&connection).unwrap();
        connection.execute_batch("CREATE TABLE ui_jobs(id TEXT PRIMARY KEY,child_chat TEXT NOT NULL); CREATE TABLE ui_run_specs(chat_id TEXT,request_id TEXT,spec TEXT);").unwrap();
        connection.execute("INSERT INTO ui_jobs VALUES ('worker','child')",[]).unwrap();
        connection.execute("INSERT INTO ui_run_specs VALUES ('child','job',?)",[json!({"review_patch":true}).to_string()]).unwrap();
        connection.execute("INSERT INTO ui_handoffs(id,owner_chat,owner_turn,job_id,target,proposal,summary,status) VALUES ('p','owner','turn','worker','project',?,?,'preview')",
            params![serde_json::to_string(&proposal).unwrap(),summary.to_string()]).unwrap();
        let projected=latest(&connection,"worker").unwrap();
        assert_eq!(projected["review_required"],true);
        assert_eq!(projected["files"][0]["path"],"fix.rs");
        let db=crate::agent_host::DatabaseState::open(connection).unwrap();
        let error=apply(&db,"owner","turn",&target,&json!({"preview_id":"p"}),&json!({}),&AtomicBool::new(false)).unwrap_err();
        assert_eq!(error.kind(),io::ErrorKind::PermissionDenied);
        assert!(!target.join("fix.rs").exists());
        assert_eq!(target_status(&db,"owner","p"),"preview");
    }

    fn target_status(db:&Database, chat:&str, id:&str) -> String { super::target(db,chat,id).unwrap().1 }

    #[test]
    fn artifact_preview_must_match_the_registered_snapshot() {
        let mappings=vec![FileMapping{source:"outputs/a.py".into(),destination:"outputs/a.py".into()}];
        let root=handoff::Root{path:"/tmp".into(),device:1,inode:1};
        let proposal=Proposal{version:1,source:root.clone(),target:root,base:None,conflicts:vec![],changes:vec![handoff::Change{
            source:"outputs/a.py".into(),path:"outputs/a.py".into(),source_snapshot:None,before:None,
            after:Some(handoff::FileVersion{sha256:"registered".into(),bytes:vec![],executable:false,mode:0o644,uid:0,gid:0,provenance:None})}]};
        assert!(verify_pinned(&proposal,&mappings,&["registered".into()]).is_ok());
        assert!(verify_pinned(&proposal,&mappings,&["changed".into()]).is_err());
        assert!(verify_pinned(&proposal,&mappings,&[]).is_err());
    }

    #[test]
    fn project_receipt_proof_is_scoped_and_rechecks_current_files() {
        let dir=tempfile::tempdir().unwrap();
        let source=dir.path().join("source"); let target=dir.path().join("target");
        std::fs::create_dir(&source).unwrap(); std::fs::create_dir(&target).unwrap();
        std::fs::write(source.join("code.py"),"answer = 42\n").unwrap();
        let proposal=handoff::preview_files(&source,&target,&[FileMapping{source:"code.py".into(),destination:"code.py".into()}]).unwrap();
        handoff::apply_with_journal(&proposal,&dir.path().join("journal")).unwrap();
        let connection=Connection::open_in_memory().unwrap(); initialize(&connection).unwrap();
        connection.execute("INSERT INTO ui_handoffs(id,owner_chat,owner_turn,job_id,target,proposal,summary,status) VALUES('p','owner','turn','worker','project',?,'{}','applied')",[serde_json::to_string(&proposal).unwrap()]).unwrap();
        let db=crate::agent_host::DatabaseState::open(connection).unwrap();
        assert!(evidence(&db,"owner","turn","p").unwrap());
        assert!(!evidence(&db,"other","turn","p").unwrap());
        assert!(!evidence(&db,"owner","other","p").unwrap());
        std::fs::write(target.join("code.py"),"user edit\n").unwrap();
        assert!(!evidence(&db,"owner","turn","p").unwrap());
    }

    #[test]
    fn restart_does_not_replay_an_unresolved_dispatch() {
        let dir=tempfile::tempdir().unwrap();
        let db=Connection::open(dir.path().join("state.db")).unwrap(); initialize(&db).unwrap();
        db.execute("INSERT INTO ui_handoffs(id,owner_chat,owner_turn,job_id,target,proposal,summary,status) VALUES('22f44453-3111-4cb2-8cb9-ff68f7165702','owner','turn','worker','project','{}','{}','applying')",[]).unwrap();
        initialize(&db).unwrap();
        let status:String=db.query_row("SELECT status FROM ui_handoffs",[],|r|r.get(0)).unwrap();
        assert_eq!(status,"uncertain");
    }
}
