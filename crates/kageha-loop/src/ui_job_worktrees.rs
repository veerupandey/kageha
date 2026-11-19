//! Thin workspace-policy composition for WebUI workers. Lifecycle lives in the public library.
use crate::{
    ui_store::{db_error, ApiResult},
    webui::Database,
};
use kageha_contracts::execution::Control;
use kageha_runtime::lifecycle::{RunRecord, RunState, RunStore};
use kageha_workspace::worker::{Manager, Options, Snapshot};
use serde_json::{json, Value};
use std::{
    io,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};
fn manager_connection(db: &rusqlite::Connection, source: &Path) -> io::Result<Manager> {
    let state = Path::new(
        db.path()
            .ok_or_else(|| io::Error::other("Persistent worktree state required"))?,
    )
    .parent()
    .ok_or_else(|| io::Error::other("State directory missing"))?;
    let git = std::env::var_os("KAGEHA_GIT_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| "/usr/bin/git".into());
    Manager::new(
        source,
        &state.join("components/workspace/worktrees.sqlite"),
        &git,
    )
}
fn manager(db: &Database, source: &Path) -> io::Result<Manager> {
    let connection = db
        .lock()
        .map_err(|_| io::Error::other("Workspace state unavailable"))?;
    manager_connection(&connection, source)
}
fn key(run: &RunRecord) -> &str {
    run.spec.input["configuration"]["job_id"]
        .as_str()
        .unwrap_or(&run.spec.id)
}
fn options(run: &RunRecord) -> io::Result<Options> {
    Options::parse(&run.spec.input["configuration"]["worker_workspace"])
}
fn view(snapshot: Snapshot) -> io::Result<Value> {
    let mut value = serde_json::to_value(&snapshot)?;
    value["schema_version"] = json!(1);
    value["revision"] = json!("native");
    value["legacy"] = json!(false);
    if let Some(op) = snapshot.operation {
        value["branch"] = json!(op.handle.branch);
        value["creation_id"] = json!(op.id);
        value["creation_state"] = json!(op.state);
    }
    Ok(value)
}
pub(crate) fn validate(root: &Path, value: &Value, remote: bool) -> ApiResult<Value> {
    let options = Options::parse(value).map_err(|e| (400, e.to_string()))?;
    if options.isolated() {
        if remote {
            return Err((
                409,
                "Local Git isolation cannot be combined with an active remote execution target"
                    .into(),
            ));
        }
        let git = std::env::var_os("KAGEHA_GIT_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|| "/usr/bin/git".into());
        let flag = AtomicBool::new(false);
        Manager::validate_source(
            root,
            &git,
            &options,
            &Control {
                cancelled: &flag,
                deadline: Some(Instant::now() + Duration::from_secs(30)),
            },
        )
        .map_err(|e| (409, e.to_string()))?;
    }
    serde_json::to_value(options).map_err(db_error)
}
pub(crate) fn prepare(db: &Database, run: &RunRecord, cancel: &AtomicBool) -> io::Result<()> {
    let options = options(run)?;
    if !options.isolated() {
        return Ok(());
    }
    let source = Path::new(
        run.spec.input["workspace"]
            .as_str()
            .ok_or_else(|| io::Error::other("Pinned workspace missing"))?,
    );
    manager(db, source)?.prepare(
        key(run),
        &options,
        &Control {
            cancelled: cancel,
            deadline: Some(Instant::now() + Duration::from_secs(30)),
        },
    )?;
    Ok(())
}
pub(crate) fn execution_root(db: &Database, turn: &str, source: &Path) -> io::Result<PathBuf> {
    let store = crate::runtime_host::store(db).map_err(|(_, e)| io::Error::other(e))?;
    let run = store
        .find_turn(turn)
        .map_err(io::Error::other)?
        .ok_or_else(|| io::Error::other("Pinned runtime scope missing; no workspace fallback"))?;
    let pinned = Path::new(
        run.spec.input["workspace"]
            .as_str()
            .ok_or_else(|| io::Error::other("Pinned source missing"))?,
    )
    .canonicalize()?;
    if pinned != source.canonicalize()? {
        return Err(io::Error::other("Source differs from admitted workspace"));
    }
    let options = options(&run)?;
    if !options.isolated() {
        return Ok(source.into());
    }
    manager(db, source)?.execution_root(key(&run), &options)
}
/// The ready Git worktree of worker `key` from `source`, which is that worker's Task_Workspace
/// root (`ui_workspace`). Fails when the checkout is missing, unverified or settled.
pub(crate) fn checkout(db: &rusqlite::Connection, source: &Path, key: &str, options: &Options) -> io::Result<PathBuf> {
    manager_connection(db, source)?.execution_root(key, options)
}
pub(crate) fn settle(db: &Database, run: &RunRecord) -> io::Result<Option<Value>> {
    let options = options(run)?;
    if !options.isolated() {
        return Ok(None);
    }
    let source = Path::new(
        run.spec.input["workspace"]
            .as_str()
            .ok_or_else(|| io::Error::other("Pinned source missing"))?,
    );
    let flag = AtomicBool::new(false);
    view(manager(db, source)?.settle(
        key(run),
        &options,
        run.state == RunState::Succeeded,
        run.state.terminal(),
        &Control {
            cancelled: &flag,
            deadline: Some(Instant::now() + Duration::from_secs(30)),
        },
    )?)
    .map(Some)
}
pub(crate) fn describe(db: &rusqlite::Connection, run_id: &str) -> rusqlite::Result<Value> {
    use rusqlite::OptionalExtension;
    let result = (|| -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        let raw: Option<String> = db
            .query_row(
                "SELECT spec FROM ui_runtime_runs WHERE id=?",
                [run_id],
                |r| r.get(0),
            )
            .optional()?;
        let Some(raw) = raw else {
            return Ok(Value::Null);
        };
        let spec: kageha_runtime::lifecycle::RunSpec = serde_json::from_str(&raw)?;
        let opts = Options::parse(&spec.input["configuration"]["worker_workspace"])?;
        let source = Path::new(
            spec.input["workspace"]
                .as_str()
                .ok_or_else(|| io::Error::other("Pinned source missing"))?,
        );
        if !opts.isolated() {
            return Ok(json!({"mode":"shared","root":source,"options":opts}));
        }
        let key = spec.input["configuration"]["job_id"]
            .as_str()
            .unwrap_or(&spec.id);
        Ok(view(manager_connection(db, source)?.snapshot(key, &opts)?)?)
    })();
    result.map_err(rusqlite::Error::ToSqlConversionFailure)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn worker_manager_preserves_source_scope() {
        let root = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let manager = Manager::new(
            root.path(),
            &state.path().join("worker/worktrees.sqlite"),
            Path::new("/usr/bin/git"),
        )
        .unwrap();
        assert_eq!(
            manager
                .execution_root("worker", &Options::default())
                .unwrap(),
            root.path().canonicalize().unwrap()
        );
    }
}
