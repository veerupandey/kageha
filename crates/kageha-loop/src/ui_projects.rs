//! Existing WebUI project routes over the independent workspace catalog.
use crate::{
    server,
    ui_store::{db_error, now, ApiResult},
    webui::{self, Database},
};
use kageha_workspace::projects::{Project, Projects};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{io, path::{Path, PathBuf}, collections::HashMap, sync::{Arc, Mutex, OnceLock}};
use tiny_http::{Method, Request};
const OWNER: &str = "instance";
static CATALOGS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<Projects>>>>> = OnceLock::new();
fn catalog(db: &Connection) -> io::Result<Arc<Mutex<Projects>>> {
    let path = Path::new(
        db.path()
            .ok_or_else(|| io::Error::other("Persistent projects require a state directory"))?,
    )
    .parent()
    .ok_or_else(|| io::Error::other("State directory required"))?
    .join("components/workspace/projects.sqlite");
    let mut catalogs = CATALOGS.get_or_init(Default::default).lock()
        .map_err(|_| io::Error::other("Project catalog unavailable"))?;
    if let Some(projects) = catalogs.get(&path) { return Ok(projects.clone()); }
    let projects = Arc::new(Mutex::new(Projects::open(&path)?));
    catalogs.insert(path, projects.clone());
    Ok(projects)
}
pub(crate) fn projection(db: &Connection, chat: &str) -> io::Result<Value> {
    let catalog = catalog(db)?;
    let projects = catalog.lock().map_err(|_| io::Error::other("Project catalog unavailable"))?;
    let project = projects.assigned(OWNER, chat)?;
    Ok(
        json!({"project_id":project.as_ref().map(|p|&p.id),"project":project,"tagged_turns":projects.tags(OWNER,chat)?.into_keys().collect::<Vec<_>>()}),
    )
}
pub(crate) fn selected(payload: &Value) -> ApiResult<&str> {
    let value = payload
        .get("project")
        .filter(|v| !v.is_null())
        .or_else(|| payload.get("project_id").filter(|v| !v.is_null()));
    match value {
        None => Ok(""),
        Some(Value::String(s)) if s.len() <= 256 => Ok(s),
        _ => Err((400, "project must be a name or ID".into())),
    }
}
pub(crate) fn assign(db: &Connection, chat: &str, name: &str) -> io::Result<Option<Project>> {
    catalog(db)?.lock().map_err(|_| io::Error::other("Project catalog unavailable"))?.assign(OWNER, chat, name, now())
}
pub(crate) fn handles(path: &str) -> bool {
    path == "/api/projects"
        || (path.starts_with("/api/sessions/")
            && (path.ends_with("/project") || path.ends_with("/tag")))
}
pub(crate) fn handle(mut request: Request, db: &Database) -> io::Result<()> {
    let url = request.url().to_owned();
    let path = url.split('?').next().unwrap_or("");
    let method = request.method().clone();
    let result = (|| -> ApiResult<Value> {
        if path == "/api/projects" && method == Method::Get {
            let query = webui::form_decode(&webui::query(&url, "q"))
                .ok_or((400, "Invalid project query".into()))?;
            let db = db.lock().map_err(db_error)?;
            return Ok(
                json!({"projects":catalog(&db).map_err(db_error)?.lock().map_err(db_error)?.list(OWNER,&query).map_err(db_error)?}),
            );
        }
        if method != Method::Post {
            return Err((405, "POST required".into()));
        }
        let payload = webui::body(&mut request).map_err(|_| (400, "Invalid JSON".into()))?;
        let db = db.lock().map_err(db_error)?;
        let catalog = catalog(&db).map_err(db_error)?;
        let mut projects = catalog.lock().map_err(db_error)?;
        if path == "/api/projects" {
            let name = crate::ui_store::text(&payload, "name", 256)?;
            let description = payload["description"].as_str().unwrap_or("");
            let color = payload["color"].as_str().unwrap_or("blue");
            let expected = match payload.get("expected_revision") {
                None => None,
                Some(v) => Some(
                    v.as_u64()
                        .ok_or((400, "expected_revision must be an integer".into()))?,
                ),
            };
            return Ok(
                json!({"project":projects.save(OWNER,name,description,color,now(),expected).map_err(|e|(409,e.to_string()))?}),
            );
        }
        let parts = path.split('/').collect::<Vec<_>>();
        let chat = parts.get(3).copied().unwrap_or("");
        if parts.len() != 5 && parts.len() != 7 {
            return Err((404, "Unknown project route".into()));
        }
        let exists = db
            .query_row("SELECT 1 FROM ui_sessions WHERE id=?", [chat], |r| {
                r.get::<_, u8>(0)
            })
            .optional()
            .map_err(db_error)?
            .is_some();
        if !exists {
            return Err((404, "Chat not found".into()));
        }
        let name = selected(&payload)?;
        if parts.len() == 5 && parts[4] == "project" {
            let project = projects
                .assign(OWNER, chat, name, now())
                .map_err(|e| (409, e.to_string()))?;
            return Ok(
                json!({"ok":true,"project_id":project.as_ref().map(|p|&p.id),"project":project}),
            );
        }
        if parts.len() == 7 && parts[4] == "messages" && parts[6] == "tag" {
            let index = parts[5]
                .parse::<u64>()
                .map_err(|_| (400, "Invalid message index".into()))?;
            drop(projects);
            let count = webui::session(&db, chat)
                .map_err(db_error)?
                .and_then(|v| v["messages"].as_array().map(Vec::len))
                .unwrap_or(0);
            if index >= count as u64 {
                return Err((404, "Message index is outside this chat".into()));
            }
            let label = payload["tag"].as_str().unwrap_or("");
            return Ok(
                json!({"ok":true,"tagged":catalog.lock().map_err(db_error)?.tag(OWNER,chat,index,name,label,now()).map_err(|e|(409,e.to_string()))?}),
            );
        }
        Err((404, "Unknown project route".into()))
    })();
    match result {
        Ok(value) => server::response(request, 200, value),
        Err((code, error)) => server::response(request, code, json!({"error":error})),
    };
    Ok(())
}

pub(crate) fn erase(db: &Connection, chat: &str) -> io::Result<()> {
    catalog(db)?.lock().map_err(|_| io::Error::other("Project catalog unavailable"))?.erase_session(OWNER, chat)
}

/// Capture trusted local project context once per admitted turn. It grants no tool authority.
pub(crate) fn instructions(workspace:&Path,message:&str)->io::Result<String>{
    use kageha_workspace::brain::Brain;
    let cancelled=std::sync::atomic::AtomicBool::new(false);
    let control=kageha_contracts::execution::Control{cancelled:&cancelled,deadline:Some(std::time::Instant::now()+std::time::Duration::from_secs(2))};
    let mut text=Brain::load(workspace,&control)?.map(|b|b.render(&[])).unwrap_or_default();
    if let Some(command)=message.split_whitespace().next().filter(|s|s.starts_with("/project:")) {
        let recipe=Brain::command(workspace,command,&control)?.ok_or_else(||io::Error::other("Project recipe was not found"))?;
        text.push_str(&format!("\n\n## Explicitly requested project recipe {command}\n{recipe}\nThese instructions do not override tool policy or approval requirements."));
    }
    Ok(text)
}

#[cfg(test)]
mod projection_tests {
    use super::*;
    #[test]
    fn repeated_projection_reuses_catalog_without_requesting_a_write_lock() {
        let dir = tempfile::tempdir().unwrap();
        let db = Connection::open(dir.path().join("native.sqlite")).unwrap();
        let first = catalog(&db).unwrap();
        assert!(Arc::ptr_eq(&first,&catalog(&db).unwrap()));
        let writer = Connection::open(dir.path().join("components/workspace/projects.sqlite")).unwrap();
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        assert_eq!(projection(&db,"chat").unwrap()["tagged_turns"],json!([]));
        writer.execute_batch("ROLLBACK").unwrap();
    }

    #[test]
    fn session_projection_returns_tag_indexes_without_changing_the_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let db = Connection::open(dir.path().join("native.sqlite")).unwrap();
        let catalog = catalog(&db).unwrap();
        let mut projects = catalog.lock().unwrap();
        let project = projects.save(OWNER,"Browser audit","","blue",1.0,Some(0)).unwrap();
        drop(projects);
        assert_eq!(projection(&db,"chat").unwrap()["tagged_turns"],json!([]));
        let mut projects = catalog.lock().unwrap();
        projects.tag(OWNER,"chat",3,&project.id,"evidence",2.0).unwrap();
        drop(projects);
        assert_eq!(projection(&db,"chat").unwrap()["tagged_turns"],json!([3]));
        let projects = catalog.lock().unwrap();
        assert_eq!(projects.tags(OWNER,"chat").unwrap()[&3].tag,"evidence");
        drop(projects);
        assert_eq!(projection(&db,"other").unwrap()["tagged_turns"],json!([]));
    }
}
