//! Explicit operator worktree controls; agent input uses its separate approval port.
use crate::{
    server,
    ui_store::{db_error, ApiResult},
    webui::{self, Database},
};
use kageha_workspace::worktrees::{Git, Operation, Worktrees};
use serde_json::{json, Value};
use std::{
    io,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};
use tiny_http::{Method, Request};
pub(crate) fn handles(path: &str) -> bool {
    path == "/api/project"
        || path == "/api/worktrees"
        || path.starts_with("/api/worktrees/operations/")
        || path == "/api/worktrees/cleanup"
        || path.starts_with("/api/worktrees/cleanup/")
}
fn git(workspace: &Path) -> io::Result<Git> {
    let program = std::env::var_os("KAGEHA_GIT_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/usr/bin/git"));
    if !program.is_absolute() {
        return Err(io::Error::other("Configure an absolute Git executable"));
    }
    Git::new(&program, workspace)
}
pub(crate) fn available(workspace: &Path) -> bool {
    git(workspace).is_ok()
}
fn journal(db: &Database) -> io::Result<Worktrees> {
    let path = {
        let db = db
            .lock()
            .map_err(|_| io::Error::other("State unavailable"))?;
        Path::new(
            db.path()
                .ok_or_else(|| io::Error::other("Persistent state required"))?,
        )
        .parent()
        .ok_or_else(|| io::Error::other("State directory required"))?
        .join("components/workspace/worktrees.sqlite")
    };
    Worktrees::open(&path)
}
fn result(operation: Operation) -> Value {
    json!({"path":operation.handle.path,"root":operation.handle.root,"branch":operation.handle.branch,"head":operation.handle.head,"operation":operation,"checkout_filters":"disabled"})
}
pub(crate) fn handle(mut request: Request, db: &Database, workspace: &Path) -> io::Result<()> {
    let url = request.url().to_owned();
    let path = url.split('?').next().unwrap_or("");
    let method = request.method().clone();
    let outcome = (|| -> ApiResult<Value> {
        let git = git(workspace).map_err(|_| {
            (
                503,
                "Git is unavailable; configure the declared Git executable".into(),
            )
        })?;
        let cancel = AtomicBool::new(false);
        let control = kageha_contracts::execution::Control {
            cancelled: &cancel,
            deadline: Some(Instant::now() + Duration::from_secs(30)),
        };
        if path == "/api/project" && method == Method::Get {
            let selected = webui::form_decode(&webui::query(&url, "project_root"))
                .ok_or((400, "Invalid project root".into()))?;
            if !selected.is_empty()
                && Path::new(&selected).canonicalize().ok().as_deref() != Some(git.root())
            {
                return Err((403, "Project root is outside this workspace".into()));
            }
            let brain=kageha_workspace::brain::Brain::load(workspace,&control).map_err(db_error)?.map(|b|json!({"root_file":b.root_file,"rules":b.rules.iter().map(|r|json!({"name":r.name,"globs":r.globs})).collect::<Vec<_>>(),"commands":b.command_names,"rendered":b.render(&[]).chars().take(8000).collect::<String>()}));
            let hooks: kageha_workspace::hooks::Set =
                serde_json::from_value(crate::ui_hooks::capture(db,workspace).map_err(db_error)?)
                    .map_err(db_error)?;
            return Ok(
                json!({"project_root":git.root(),"git":git.is_repository(&control).map_err(db_error)?,"brain":brain,"worktrees":git.list(&control).map_err(db_error)?,"hooks":hooks.hooks.iter().map(|h|json!({"event":h.event,"matcher":h.matcher,"command":h.command,"http":h.http,"active":matches!(h.event.as_str(),"sessionStart"|"preToolUse"|"beforeShell"|"postToolUse")})).collect::<Vec<_>>() }),
            );
        }
        let mut store = journal(db).map_err(db_error)?;
        if path == "/api/worktrees" && method == Method::Get {
            let selected = webui::form_decode(&webui::query(&url, "project_root"))
                .ok_or((400, "Invalid project root".into()))?;
            if !selected.is_empty()
                && Path::new(&selected).canonicalize().ok().as_deref() != Some(git.root())
            {
                return Err((403, "Project root is outside this workspace".into()));
            }
            return Ok(
                json!({"project_root":git.root(),"worktrees":git.list(&control).map_err(db_error)?.into_iter().map(|h|{let id=store.creation_id(&h).map_err(db_error)?;let mut v=serde_json::to_value(h).map_err(db_error)?;v["creation_id"]=json!(id);Ok(v)}).collect::<ApiResult<Vec<Value>>>()?,"cleanup_operations":store.cleanup_pending(git.root()).map_err(db_error)?,"operations":store.pending(git.root()).map_err(db_error)?,"checkout_filters":"disabled"}),
            );
        }
        if let Some(id) = path.strip_prefix("/api/worktrees/cleanup/") {
            if !crate::server::identifier(id) {
                return Err((400, "Invalid cleanup identity".into()));
            }
            let op = store
                .cleanup_status(id)
                .map_err(db_error)?
                .filter(|o| o.handle.root == git.root())
                .ok_or((404, "Cleanup receipt not found".into()))?;
            return match method {
                Method::Get => Ok(json!({"cleanup":op})),
                Method::Post => Ok(
                    json!({"cleanup":store.reconcile_cleanup(&git,id,&control).map_err(db_error)?}),
                ),
                _ => Err((405, "GET or POST required".into())),
            };
        }
        if path == "/api/worktrees/cleanup" {
            if method != Method::Post {
                return Err((405, "POST required".into()));
            }
            let payload = webui::body(&mut request).map_err(|_| (400, "Invalid JSON".into()))?;
            let id = payload["request_id"]
                .as_str()
                .filter(|v| crate::server::identifier(v))
                .ok_or((400, "Explicit cleanup request_id required".into()))?;
            let creation = payload["creation_id"]
                .as_str()
                .filter(|v| crate::server::identifier(v))
                .ok_or((400, "creation_id required".into()))?;
            if let Some(job) = creation.strip_prefix("worker-") {
                use rusqlite::OptionalExtension;
                let active: Option<String> = db
                    .lock()
                    .map_err(db_error)?
                    .query_row("SELECT state FROM ui_jobs WHERE id=?", [job], |r| r.get(0))
                    .optional()
                    .map_err(db_error)?;
                if active.as_deref().is_some_and(|s| {
                    matches!(s, "queued" | "running" | "cancelling" | "waiting_input")
                }) {
                    return Err((409,"This checkout belongs to an active specialist. Stop the worker before removing it.".into()));
                }
            }
            let op = store
                .cleanup(&git, id, creation, &control)
                .map_err(|e| (409, e.to_string()))?;
            return Ok(json!({"cleanup":op}));
        }
        if let Some(id) = path.strip_prefix("/api/worktrees/operations/") {
            if !crate::server::identifier(id) {
                return Err((400, "Invalid worktree operation ID".into()));
            }
            let original = store
                .status(id)
                .map_err(db_error)?
                .filter(|o| o.handle.root == git.root())
                .ok_or((404, "Worktree operation not found".into()))?;
            return if method == Method::Get {
                Ok(result(original))
            } else if method == Method::Post {
                Ok(result(
                    store.reconcile(&git, id, &control).map_err(db_error)?,
                ))
            } else {
                Err((405, "GET or POST required".into()))
            };
        }
        if path != "/api/worktrees" || method != Method::Post {
            return Err((405, "POST required".into()));
        }
        let payload = webui::body(&mut request).map_err(|_| (400, "Invalid JSON".into()))?;
        if let Some(root) = payload.get("project_root").filter(|v| !v.is_null()) {
            if root
                .as_str()
                .and_then(|p| Path::new(p).canonicalize().ok())
                .as_deref()
                != Some(git.root())
            {
                return Err((403, "Project root is outside this workspace".into()));
            }
        }
        let label = payload
            .get("label")
            .map(|v| v.as_str().ok_or((400, "label must be text".into())))
            .transpose()?
            .unwrap_or("agent");
        let base = payload
            .get("base")
            .map(|v| v.as_str().ok_or((400, "base must be text".into())))
            .transpose()?
            .unwrap_or("HEAD");
        let id = payload
            .get("request_id")
            .map(|v| v.as_str().ok_or((400, "request_id must be text".into())))
            .transpose()?
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        if !crate::server::identifier(&id) {
            return Err((400, "Invalid request_id".into()));
        }
        let operation = store
            .create(&git, &id, label, base, &control)
            .map_err(|e| (409, e.to_string()))?;
        Ok(result(operation))
    })();
    match outcome {
        Ok(value) => {
            let cleanup_unresolved = value["cleanup"]["state"]
                .as_str()
                .is_some_and(|s| s != "completed");
            let unresolved = value["operation"]["state"]
                .as_str()
                .is_some_and(|s| s != "completed");
            if (unresolved && path == "/api/worktrees"
                || cleanup_unresolved && path == "/api/worktrees/cleanup")
                && method == Method::Post
            {
                let mut value = value;
                value["error"] =
                    json!("Worktree operation needs review. Check its receipt before continuing.");
                server::response(request, 409, value);
            } else {
                server::response(request, 200, value);
            }
        }
        Err((status, error)) => server::response(request, status, json!({"error":error})),
    };
    Ok(())
}
