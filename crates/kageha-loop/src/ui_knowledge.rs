//! WebUI compatibility and scoped-run wiring for the independent knowledge store.
use crate::{
    server,
    ui_store::{db_error, ApiResult},
    webui::{self, Database},
};
use kageha_knowledge::{
    scope::{Config, Scope},
    store::{Base, Source, Store},
    Lexical, Retriever,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{io, path::Path, sync::atomic::AtomicBool};
use tiny_http::{Method, Request};
pub(crate) fn workspace_id(workspace: &Path) -> io::Result<String> {
    use std::os::unix::ffi::OsStrExt;
    Ok(format!(
        "workspace:{:x}",
        Sha256::digest(workspace.canonicalize()?.as_os_str().as_bytes())
    ))
}
pub(crate) fn store(db: &Database) -> io::Result<Store> {
    let path = {
        let db = db
            .lock()
            .map_err(|_| io::Error::other("Database unavailable"))?;
        Path::new(
            db.path()
                .ok_or_else(|| io::Error::other("Persistent state required"))?,
        )
        .parent()
        .ok_or_else(|| io::Error::other("State directory required"))?
        .join("components/knowledge/knowledge.sqlite")
    };
    Store::open(&path)
}
pub(crate) fn resolve(
    db: &Database,
    workspace: &Path,
    persona: &Value,
    selected: &Value,
) -> io::Result<Scope> {
    let connection = db.lock().map_err(|_| io::Error::other("Database unavailable"))?;
    resolve_connection(&connection, workspace, persona, selected)
}
pub(crate) fn pinned(workspace: &Path, persona: &Value, value: &Value) -> io::Result<Scope> {
    let expected = workspace_id(workspace)?;
    if value.is_null() {
        return Ok(Scope {
            workspace: expected,
            agent: persona["id"].as_str().unwrap_or("main").into(),
            shared: vec![],
        });
    }
    let scope: Scope = serde_json::from_value(value.clone())?;
    scope.validate()?;
    if scope.workspace != expected || scope.agent != persona["id"] {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Knowledge scope belongs to another workspace or agent",
        ));
    }
    Ok(scope)
}
fn mapped(e: io::Error) -> (u16, String) {
    (
        match e.kind() {
            io::ErrorKind::NotFound => 404,
            io::ErrorKind::AlreadyExists => 409,
            io::ErrorKind::InvalidInput => 400,
            io::ErrorKind::FileTooLarge => 413,
            _ => 500,
        },
        e.to_string(),
    )
}
fn decode(s: &str) -> ApiResult<String> {
    webui::form_decode(&s.replace('+', "%2B")).ok_or((400, "Invalid URL encoding".into()))
}
fn summary(store: &Store, workspace: &str, base: Base) -> ApiResult<Value> {
    let metadata = store.metadata(workspace, &base.id).map_err(mapped)?;
    let names: Vec<_> = metadata.iter().map(|s| s.filename.clone()).collect();
    let chunks: usize = metadata.iter().map(|s| s.chunks).sum();
    Ok(
        json!({"id":base.id,"generation":base.generation,"revision":base.revision,"engine":"rust-lexical","chunking":base.chunking,"sources":names,"stats":{"sources":names.len(),"chunks":chunks},"files":metadata,"capabilities":kageha_knowledge::capabilities()}),
    )
}
fn hits(store: &Store, workspace: &str, id: &str, query: &str, mode: &str) -> ApiResult<Value> {
    if !matches!(mode, "auto" | "lexical" | "broad") {
        return Err((
            400,
            "This adapter supports lexical search; semantic retrieval is unavailable".into(),
        ));
    }
    let scope = Scope {
        workspace: workspace.into(),
        agent: "operator".into(),
        shared: vec![store.binding(workspace, id).map_err(mapped)?],
    };
    let cancel = AtomicBool::new(false);
    let control = kageha_contracts::execution::Control {
        cancelled: &cancel,
        deadline: None,
    };
    let docs = store.documents(&scope, &control).map_err(mapped)?;
    let result = Lexical
        .search("operator", &docs, query, 10, &control)
        .map_err(|e| (400, e.to_string()))?;
    Ok(json!(result.into_iter().map(|h|json!({"id":h.chunk_id,"document_id":h.document_id,"kb":id,"text":h.text,"score":h.score,"source":h.source,"sha256":h.sha256,"heading":h.heading,"citation":format!("[{id}:{}]",h.source)})).collect::<Vec<_>>()))
}
pub(crate) fn handle(mut request: Request, db: &Database, workspace: &Path) -> io::Result<()> {
    let url = request.url().to_owned();
    let (path, query) = url.split_once('?').unwrap_or((&url, ""));
    let method = request.method().clone();
    let result = (|| -> ApiResult<(u16, Value)> {
        let owner = workspace_id(workspace).map_err(mapped)?;
        let mut store = store(db).map_err(mapped)?;
        let params: std::collections::BTreeMap<_, _> =
            url::form_urlencoded::parse(query.as_bytes())
                .into_owned()
                .collect();
        if path == "/api/knowledge-bases" {
            if method == Method::Get {
                let items = store
                    .list(&owner)
                    .map_err(mapped)?
                    .into_iter()
                    .map(|b| summary(&store, &owner, b))
                    .collect::<ApiResult<Vec<_>>>()?;
                return Ok((200, json!({"engines":["rust-lexical"],"items":items})));
            }
            if method != Method::Post {
                return Err((405, "GET or POST required".into()));
            }
            let payload = webui::body(&mut request).map_err(|_| (400, "Invalid JSON".into()))?;
            if payload["engine"]
                .as_str()
                .is_some_and(|e| e != "rust-lexical")
            {
                return Err((
                    400,
                    "Only rust-lexical is available; vendor indexes are not active".into(),
                ));
            }
            let id = payload["id"]
                .as_str()
                .or_else(|| payload["name"].as_str())
                .unwrap_or("");
            let base = store
                .create(
                    &owner,
                    id,
                    payload["chunking"].as_str().unwrap_or("hierarchical"),
                )
                .map_err(mapped)?;
            return Ok((201, summary(&store, &owner, base)?));
        }
        let parts: Vec<_> = path.split('/').collect();
        let id = decode(parts.get(3).unwrap_or(&""))?;
        if !kageha_knowledge::scope::valid_id(&id) {
            return Err((400, "Invalid or private knowledge base id".into()));
        }
        store.base(&owner, &id).map_err(mapped)?;
        if parts.len() == 4 {
            return match method {
                Method::Get => Ok((
                    200,
                    summary(&store, &owner, store.base(&owner, &id).map_err(mapped)?)?,
                )),
                Method::Delete => {
                    store.delete(&owner, &id).map_err(mapped)?;
                    Ok((200, json!({"ok":true,"id":id,"deleted":true})))
                }
                _ => Err((405, "GET or DELETE required".into())),
            };
        }
        if parts.len() == 5 && parts[4] == "upload" && method == Method::Post {
            let (name, bytes) = crate::ui_uploads::read_upload(&mut request).map_err(mapped)?;
            if !kageha_knowledge::store::text_source(&name) {
                return Err((
                    415,
                    "Binary and media extraction is unavailable in this knowledge adapter".into(),
                ));
            }
            let saved = store
                .put(
                    &owner,
                    &id,
                    &name,
                    &bytes,
                    crate::ui_uploads::mime(&name),
                    None,
                )
                .map_err(mapped)?;
            return Ok((
                200,
                json!({"ok":true,"filename":name,"revision":saved.revision,"sha256":saved.sha256,"indexed":kageha_knowledge::store::text_source(&name),"stats":{"files":1},"capabilities":kageha_knowledge::capabilities()}),
            ));
        }
        if parts.len() == 5 && parts[4] == "chunks" && method == Method::Get {
            let offset = match params.get("cursor") {
                None => 0,
                Some(v) => v
                    .parse::<usize>()
                    .ok()
                    .filter(|v| *v <= 262144)
                    .ok_or((400, "Invalid chunk cursor".into()))?,
            };
            let limit = match params.get("limit") {
                None => 64,
                Some(v) => v
                    .parse::<usize>()
                    .ok()
                    .filter(|v| *v > 0 && *v <= 128)
                    .ok_or((400, "Chunk limit must be 1–128".into()))?,
            };
            let base = store.base(&owner, &id).map_err(mapped)?;
            let snapshot = format!("{}:{}", base.generation, base.revision);
            if params.get("snapshot").is_some_and(|v| v != &snapshot) {
                return Err((409, "Knowledge sources changed during chunk loading".into()));
            }
            let mut items = Vec::new();
            let mut total = 0usize;
            for name in store.names(&owner, &id).map_err(mapped)? {
                if params.get("source").is_some_and(|v| v != &name) {
                    continue;
                }
                let source = store.source(&owner, &id, &name).map_err(mapped)?;
                for c in store.source_chunks(&owner,&id,&source).map_err(mapped)? {
                    if total>=offset&&items.len()<limit {items.push(json!({"id":c.id,"source":name,"text":c.text,"heading":c.heading,"parent_id":c.parent_id}));}
                    total+=1;
                }
            }
            let current = store.base(&owner, &id).map_err(mapped)?;
            if snapshot != format!("{}:{}", current.generation, current.revision) {
                return Err((409, "Knowledge sources changed during chunk loading".into()));
            }
            let next = offset + items.len();
            return Ok((
                200,
                json!({"total":total,"items":items,"snapshot":snapshot,"next_cursor":if next<total{Some(next)}else{None},"capabilities":kageha_knowledge::capabilities()}),
            ));
        }
        if parts.len() == 5 && parts[4] == "search" && method == Method::Get {
            return Ok((
                200,
                json!({"items":hits(&store,&owner,&id,params.get("q").map(String::as_str).unwrap_or(""),params.get("mode").map(String::as_str).unwrap_or("auto"))?,"capabilities":kageha_knowledge::capabilities()}),
            ));
        }
        if parts.len() == 5 && parts[4] == "query" && method == Method::Post {
            let p = webui::body(&mut request).map_err(|_| (400, "Invalid JSON".into()))?;
            let passages = hits(
                &store,
                &owner,
                &id,
                p["query"]
                    .as_str()
                    .or_else(|| p["q"].as_str())
                    .unwrap_or(""),
                "lexical",
            )?;
            let answer = passages
                .as_array()
                .unwrap()
                .iter()
                .take(3)
                .map(|h| {
                    format!(
                        "{} {}",
                        h["text"].as_str().unwrap_or(""),
                        h["citation"].as_str().unwrap_or("")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n\n");
            return Ok((
                200,
                json!({"answer":if answer.is_empty(){"No matching passages were found.".into()}else{answer},"passages":passages,"engine":"rust-lexical","answer_kind":"extractive"}),
            ));
        }
        if parts.len() == 6 && parts[4] == "sources" {
            let name = decode(parts[5])?;
            if method == Method::Delete {
                store.remove(&owner, &id, &name).map_err(mapped)?;
                return Ok((200, json!({"ok":true,"id":id,"source":name})));
            }
            let source = if matches!(method, Method::Post | Method::Put) {
                let p = webui::body(&mut request).map_err(|_| (400, "Invalid JSON".into()))?;
                let text = p["content"]
                    .as_str()
                    .ok_or((400, "content must be text".into()))?;
                if p.get("expected_revision").is_some_and(|v| !v.is_u64()) {
                    return Err((400, "expected_revision must be an unsigned integer".into()));
                }
                if !kageha_knowledge::store::text_source(&name) {
                    return Err((415, "Binary sources cannot be edited as text".into()));
                }
                store
                    .put(
                        &owner,
                        &id,
                        &name,
                        text.as_bytes(),
                        crate::ui_uploads::mime(&name),
                        p["expected_revision"].as_u64(),
                    )
                    .map_err(mapped)?
            } else if method == Method::Get {
                store.source(&owner, &id, &name).map_err(mapped)?
            } else {
                return Err((405, "Unsupported source method".into()));
            };
            if !kageha_knowledge::store::text_source(&name) {
                return Err((415, "Binary source text extraction is unavailable".into()));
            }
            return Ok((200, source_value(source)?));
        }
        Err((404, "Knowledge operation not found".into()))
    })();
    match result {
        Ok((status, value)) => server::response(request, status, value),
        Err((status, error)) => server::response(request, status, json!({"error":error})),
    };
    Ok(())
}
fn source_value(s: Source) -> ApiResult<Value> {
    Ok(
        json!({"filename":s.name,"content":String::from_utf8(s.bytes).map_err(db_error)?,"sha256":s.sha256,"revision":s.revision}),
    )
}

pub(crate) fn resolve_connection(
    db: &rusqlite::Connection,
    workspace: &Path,
    persona: &Value,
    selected: &Value,
) -> io::Result<Scope> {
    let owner = workspace_id(workspace)?;
    let config = Config::parse(&persona["knowledge"])?;
    let selected = kageha_knowledge::scope::ids(selected)?;
    let path = Path::new(
        db.path()
            .ok_or_else(|| io::Error::other("Persistent state required"))?,
    )
    .parent()
    .ok_or_else(|| io::Error::other("State directory required"))?
    .join("components/knowledge/knowledge.sqlite");
    let store = Store::open(&path)?;
    let selected = if persona["id"] == "main" {
        store.list(&owner)?.into_iter().map(|base| base.id).collect()
    } else {
        selected
    };
    let config = if persona["id"] == "main" {
        Config::parse(&json!({"inherit_workspace":true,"extra_kb_ids":[]}))?
    } else {
        config
    };
    Scope::resolve(
        &owner,
        persona["id"].as_str().unwrap_or("main"),
        &config,
        &selected,
        |id| store.binding(&owner, id),
    )
}
pub(crate) fn list_scoped(db: &Database, scope: &Scope) -> io::Result<Value> {
    scope.validate()?;
    if scope.shared.is_empty() {
        return Ok(json!([]));
    }
    let store = store(db)?;
    let mut rows = Vec::new();
    for binding in &scope.shared {
        let base = store.base(&scope.workspace, &binding.id)?;
        if base.generation != binding.generation {
            return Err(io::Error::other("Attached knowledge base was replaced"));
        }
        rows.push(json!({"id":base.id,"generation":base.generation,"sources":store.names(&scope.workspace,&base.id)?,"capabilities":kageha_knowledge::capabilities()}));
    }
    Ok(json!(rows))
}
pub(crate) fn read_scoped(
    db: &Database,
    scope: &Scope,
    path: &str,
    cancel: &AtomicBool,
) -> io::Result<String> {
    let parts: Vec<_> = path.splitn(4, '/').collect();
    if parts.len() != 4 || parts[0] != "knowledge-bases" || parts[2] != "sources" {
        return Err(io::Error::other("Invalid shared knowledge source"));
    }
    let control = kageha_contracts::execution::Control {
        cancelled: cancel,
        deadline: None,
    };
    control.check()?;
    let (source, chunks) = store(db)?.scoped_content(scope, parts[1], parts[3])?;
    control.check()?;
    if !kageha_knowledge::store::text_source(&source.name) {
        if chunks.is_empty() { return Err(io::Error::other("This source has no retained text extraction")); }
        return Ok(chunks.into_iter().map(|c| c.text).collect::<Vec<_>>().join("\n\n"));
    }
    String::from_utf8(source.bytes).map_err(io::Error::other)
}

#[cfg(test)]
mod reliability_tests {
    use super::*;
    #[test]
    fn main_inherits_current_workspace_bases_and_revocations_remain_effective() {
        let root = std::env::temp_dir().join(format!("kageha-main-knowledge-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let db = rusqlite::Connection::open(root.join("application.sqlite")).unwrap();
        let owner = workspace_id(&root).unwrap();
        let mut store = Store::open(&root.join("components/knowledge/knowledge.sqlite")).unwrap();
        store.create(&owner, "first", "fixed").unwrap();
        store.create("other-workspace", "foreign", "fixed").unwrap();
        let main = json!({"id":"main","knowledge":{"inherit_workspace":false}});
        let first = resolve_connection(&db, &root, &main, &json!([])).unwrap();
        assert_eq!(first.shared.len(), 1);
        store.create(&owner, "newly-connected", "fixed").unwrap();
        let second = resolve_connection(&db, &root, &main, &json!([])).unwrap();
        assert_eq!(second.shared.len(), 2);
        store.delete(&owner, "first").unwrap();
        assert!(store.documents(&first, &kageha_contracts::execution::Control { cancelled: &AtomicBool::new(false), deadline: None }).is_err());
        assert_eq!(resolve_connection(&db, &root, &main, &json!([])).unwrap().shared.len(), 1);
        let specialist = json!({"id":"research","knowledge":{"inherit_workspace":false}});
        assert!(resolve_connection(&db, &root, &specialist, &json!([])).unwrap().shared.is_empty());
        drop(store); drop(db);
        std::fs::remove_dir_all(root).unwrap();
    }
}
