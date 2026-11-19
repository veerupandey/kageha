//! Host wiring for the per-chat Task_Workspace (Requirement 3).
//!
//! The workspace lives at `<chat root>/workspace/`, where the chat root is the directory
//! returned by [`ui_uploads::root`]. Every created or changed workspace file outside
//! `.kageha/` is registered in `ui_uploads` and `ui_turn_artifacts` under the chat-root
//! relative path `workspace/<path>`, so [`ui_uploads::pinned`] verifies the current bytes
//! against the registered SHA-256 (Requirement 3.4).
//!
//! Delegated workers (`agent_spawn`, design section 17, task 28.1) run in their own chat
//! (`ui_jobs.child_chat`) and get their own workspace ([`open_for_chat`]):
//!
//! - `isolation: shared`: `<parent workspace>/agents/<job_id>/`, a nested Task_Workspace
//!   with its own layout and manifest. The parent's `scan_changes` and file listing skip
//!   nested workspaces and the parent's file writes can't enter them, so a file is never
//!   registered twice; the parent can still read them with `workspace/agents/<job_id>/...`.
//! - `isolation: worktree`: the Git worktree is the root. Its state (manifest, spilled
//!   output, job logs, HOME, TMPDIR, the connector API socket and token) lives in the
//!   worker chat's `worktree-state/`, so the checkout gets no `.kageha/`, `todo.md` or
//!   layout directories, and worktree cleanup isn't blocked by Host files.
//!
//! A worker's files are registered as its own artifacts (`workspace/<path>` of the worker
//! chat). It reads its parent's workspace with `read_file` and the [`PARENT_PREFIX`];
//! writes there fail with `path_outside_workspace`. Its shell session ([`shell_roots`]) has
//! the parent workspace as a read root, without the parent's `.kageha/` and `agents/`.
//! A worker whose `<chat root>/workspace/` already exists keeps using it.
//!
//! Registration upserts the hash, so a later registration makes the newest bytes the pinned
//! version. Evidence keeps the hash it was attached with in `ui_artifact_evidence`; the
//! completion check treats evidence as stale when the file's registered hash moved on
//! (Requirement 3.5).
use crate::{ui_uploads, webui::Database};
use kageha_workspace::task::{FileChange, TaskWorkspace, STATE_DIR, WORKSPACE_DIR};
use rusqlite::{params, OptionalExtension};
use std::io;
use std::path::{Path, PathBuf};

pub(crate) fn initialize(db: &rusqlite::Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS ui_artifact_evidence(chat_id TEXT NOT NULL,turn_id TEXT NOT NULL,
            outcome_id TEXT NOT NULL,path TEXT NOT NULL,sha256 TEXT NOT NULL,
            PRIMARY KEY(chat_id,turn_id,outcome_id,path));",
    )
}

/// Read-only prefix under which a delegated worker's `read_file` reads its parent's
/// workspace. Writes under it are rejected with `path_outside_workspace`.
pub(crate) const PARENT_PREFIX: &str = "parent/";
/// Directory in a worktree worker's private chat root that holds its workspace state (the
/// `.kageha/` contents), so the Git checkout gets no Host files.
pub(crate) const WORKTREE_STATE_DIR: &str = "worktree-state";
/// Parent links followed when resolving nested workers (nesting is limited to 3 levels).
const MAX_NESTING: usize = 8;

/// Where a chat's Task_Workspace lives (spec autonomous-agent-loop, task 28.1).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Placement {
    /// `<chat root>/workspace`: an ordinary chat, or a worker whose workspace was created
    /// there before workers got their own placement.
    Chat,
    /// A worker with `isolation: shared`: `<parent workspace>/agents/<job_id>/`, a nested
    /// workspace with its own layout and manifest. The parent's scans skip it.
    Nested { parent: String, job: String },
    /// A worker with `isolation: worktree`: its Git worktree is the root, and the state
    /// lives in `<worker chat root>/worktree-state/`, outside the checkout.
    Worktree { parent: String, checkout: PathBuf },
}

impl Placement {
    fn parent(&self) -> Option<&str> {
        match self {
            Self::Chat => None,
            Self::Nested { parent, .. } | Self::Worktree { parent, .. } => Some(parent),
        }
    }
}

fn missing_table(error: &rusqlite::Error) -> bool {
    matches!(error, rusqlite::Error::SqliteFailure(_, Some(message)) if message.starts_with("no such table"))
}

/// Optional rows: a missing table (a store without delegated workers) reads as no row.
fn optional<T>(result: rusqlite::Result<T>) -> io::Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(error) if missing_table(&error) => Ok(None),
        Err(error) => Err(io::Error::other(error)),
    }
}

/// The placement of `chat`'s workspace. A worker chat is the `child_chat` of a `ui_jobs`
/// row; its isolation is the `worker_workspace` policy pinned in its job spec.
fn placement(db: &Database, chat: &str) -> io::Result<Placement> {
    let worker = {
        let connection = db.lock().map_err(|_| io::Error::other("database unavailable"))?;
        let Some((job, parent)) = optional(connection.query_row(
            "SELECT id,parent_chat FROM ui_jobs WHERE child_chat=? LIMIT 1",
            [chat],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        ))?
        else {
            return Ok(Placement::Chat);
        };
        let spec: Option<String> = optional(connection.query_row(
            "SELECT spec FROM ui_run_specs WHERE chat_id=? AND request_id='job'",
            [chat],
            |row| row.get(0),
        ))?;
        let spec: serde_json::Value = match spec {
            Some(spec) => serde_json::from_str(&spec).map_err(io::Error::other)?,
            None => serde_json::Value::Null,
        };
        let options = kageha_workspace::worker::Options::parse(&spec["worker_workspace"])?;
        (job, parent, options)
    };
    let (job, parent, options) = worker;
    if parent == chat {
        return Err(io::Error::other("A worker cannot be its own parent"));
    }
    // A worker created before nested placements keeps the workspace its files are in.
    if ui_uploads::root_path(db, chat)?.join(WORKSPACE_DIR).join(STATE_DIR).is_dir() {
        return Ok(Placement::Chat);
    }
    if !options.isolated() {
        return Ok(Placement::Nested { parent, job });
    }
    let connection = db.lock().map_err(|_| io::Error::other("database unavailable"))?;
    let source: Option<String> = optional(connection.query_row(
        "SELECT json_extract(spec,'$.input.workspace') FROM ui_runtime_runs WHERE id=?",
        [&job],
        |row| row.get(0),
    ))?
    .flatten();
    let source = source.ok_or_else(|| io::Error::other("The worker's pinned source workspace is missing; no workspace fallback"))?;
    let checkout = crate::ui_job_worktrees::checkout(&connection, Path::new(&source), &job, &options)?;
    Ok(Placement::Worktree { parent, checkout })
}

/// Opens (creating if needed) the chat's Task_Workspace: `<chat root>/workspace` for an
/// ordinary chat, `<parent workspace>/agents/<job_id>/` for a shared delegated worker, or
/// the worker's Git worktree with its state in `<worker chat root>/worktree-state/`.
pub(crate) fn open_for_chat(db: &Database, chat: &str) -> io::Result<TaskWorkspace> {
    open_at(db, chat, 0)
}

fn open_at(db: &Database, chat: &str, depth: usize) -> io::Result<TaskWorkspace> {
    if depth > MAX_NESTING {
        return Err(io::Error::other("Worker nesting is too deep"));
    }
    Ok(match placement(db, chat)? {
        Placement::Chat => TaskWorkspace::open(&ui_uploads::root(db, chat)?)?,
        Placement::Nested { parent, job } => TaskWorkspace::open_nested(&open_at(db, &parent, depth + 1)?, &job)?,
        Placement::Worktree { checkout, .. } => {
            TaskWorkspace::open_with_state(&checkout, &ui_uploads::root(db, chat)?.join(WORKTREE_STATE_DIR))?
        }
    })
}

/// The chat's workspace root if it exists, without creating anything. `None` for a chat
/// that never used its workspace, or a worktree worker whose checkout is unavailable.
pub(crate) fn existing_root(db: &Database, chat: &str) -> io::Result<Option<PathBuf>> {
    existing_at(db, chat, 0)
}

fn existing_at(db: &Database, chat: &str, depth: usize) -> io::Result<Option<PathBuf>> {
    if depth > MAX_NESTING {
        return Err(io::Error::other("Worker nesting is too deep"));
    }
    let root = match placement(db, chat) {
        Ok(Placement::Chat) => Some(ui_uploads::root_path(db, chat)?.join(WORKSPACE_DIR)),
        Ok(Placement::Nested { parent, job }) => {
            existing_at(db, &parent, depth + 1)?.map(|root| root.join(kageha_workspace::task::AGENTS_DIR).join(job))
        }
        Ok(Placement::Worktree { checkout, .. }) => Some(checkout),
        // An isolated checkout that is missing or settled has no files to show.
        Err(error) if error.to_string().contains("checkout is unavailable") => None,
        Err(error) => return Err(error),
    };
    Ok(root.filter(|root| std::fs::symlink_metadata(root).is_ok_and(|meta| meta.is_dir())))
}

/// The parent chat of a delegated worker chat, `None` for any other chat.
pub(crate) fn parent_chat(db: &Database, chat: &str) -> io::Result<Option<String>> {
    let connection = db.lock().map_err(|_| io::Error::other("database unavailable"))?;
    optional(connection.query_row(
        "SELECT parent_chat FROM ui_jobs WHERE child_chat=? LIMIT 1",
        [chat],
        |row| row.get::<_, String>(0),
    ))
}

/// For a delegated worker, its parent's workspace, which the worker reads (never writes)
/// under [`PARENT_PREFIX`]. `None` for any other chat.
pub(crate) fn parent_workspace(db: &Database, chat: &str) -> io::Result<Option<TaskWorkspace>> {
    match parent_chat(db, chat)? {
        Some(parent) if parent != chat => Ok(Some(open_for_chat(db, &parent)?)),
        _ => Ok(None),
    }
}

/// Whether a workspace-relative path names the parent's workspace in a worker chat
/// (`parent` or `parent/...`), which file writes reject.
pub(crate) fn names_parent(db: &Database, chat: &str, relative: &str) -> io::Result<bool> {
    // Lexically normalized, so `a/../parent/x` and `./parent` count too.
    let mut parts: Vec<&str> = Vec::new();
    for part in relative.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    let named = parts.first() == Some(&PARENT_PREFIX.trim_end_matches('/'));
    Ok(named && parent_chat(db, chat)?.is_some())
}

/// Resolves `rel` in a parent's workspace for a worker's read. The parent's Host state
/// (`.kageha/`, which holds its connector API token) and its other workers' nested
/// workspaces are not readable.
pub(crate) fn resolve_in_parent(parent: &TaskWorkspace, rel: &str) -> Result<PathBuf, kageha_workspace::task::WorkspaceError> {
    use kageha_workspace::task::{is_nested_root, WorkspaceError, AGENTS_DIR};
    let resolved = parent.resolve(rel)?;
    let relative = resolved
        .strip_prefix(parent.root())
        .map_err(|_| WorkspaceError::PathOutsideWorkspace { path: rel.into() })?;
    let mut parts = relative.components().map(|part| part.as_os_str());
    let first = parts.next();
    let hidden = (parent.state_inside() && first == Some(STATE_DIR.as_ref()))
        || (first == Some(AGENTS_DIR.as_ref())
            && parts.next().is_some_and(|name| is_nested_root(&parent.root().join(AGENTS_DIR).join(name))));
    if hidden {
        return Err(WorkspaceError::PathOutsideWorkspace { path: format!("{PARENT_PREFIX}{rel}") });
    }
    Ok(resolved)
}

/// The shell session roots of `chat` in `workspace`: an external state directory, and for
/// a delegated worker its parent's workspace as a read root with the parent's `.kageha/`
/// and `agents/` hidden. The worker's own nested workspace inside `agents/` stays writable.
pub(crate) fn shell_roots(db: &Database, chat: &str, workspace: &TaskWorkspace) -> io::Result<kageha_workspace::shell::SessionRoots> {
    let mut roots = kageha_workspace::shell::SessionRoots {
        state_dir: (!workspace.state_inside()).then(|| workspace.state_dir().to_path_buf()),
        ..Default::default()
    };
    if let Some(parent) = parent_workspace(db, chat)? {
        roots.read_roots.push(parent.root().to_path_buf());
        if parent.state_inside() {
            roots.hidden_roots.push(parent.state_dir().to_path_buf());
        }
        let agents = parent.root().join(kageha_workspace::task::AGENTS_DIR);
        if std::fs::symlink_metadata(&agents).is_ok_and(|meta| meta.is_dir()) {
            roots.hidden_roots.push(agents);
        }
    }
    Ok(roots)
}

/// A system note for a delegated worker's turn naming its workspace and the read-only
/// parent prefix. `None` for other chats.
pub(crate) fn worker_note(db: &Database, chat: &str) -> io::Result<Option<String>> {
    let placement = placement(db, chat)?;
    if placement.parent().is_none() {
        return Ok(None);
    }
    let place = match placement {
        Placement::Worktree { .. } => "Your task workspace is your Git worktree: write_file, edit_file and shell work in that checkout, and the host keeps its own state outside it.",
        _ => "You have your own task workspace: write_file, edit_file and shell work there, and the files you create are your artifacts.",
    };
    Ok(Some(format!("{place} You can read your parent's task workspace, without changing it, with read_file and the {PARENT_PREFIX}<path> prefix (for example {PARENT_PREFIX}outputs/brief.md), and your shell can read it too. Writes there are rejected.")))
}

/// Places every upload of `chat` (`ui_uploads` rows under `inputs/`) in its Task_Workspace
/// at `inputs/<original name>`, or `inputs/<stored name>` when the name is taken
/// (Requirement 20.1, [`TaskWorkspace::place_input`]). The originals stay in
/// `<chat root>/inputs/`, where `read_file` and the UI keep finding them. Placed inputs are
/// not changes, so they are never registered as artifacts or evidence. Called at turn start
/// and after an upload to a chat whose workspace exists ([`place_new_inputs`]). Delegated
/// workers get none: they read their parent's under `parent/inputs/`. Returns the inputs
/// the workspace holds; an upload that can't be placed is logged and skipped.
pub(crate) fn place_inputs(db: &Database, chat: &str) -> io::Result<Vec<kageha_workspace::task::PlacedInput>> {
    if placement(db, chat)? != Placement::Chat {
        return Ok(Vec::new());
    }
    let uploads: Vec<(String, String, String)> = {
        let connection = db.lock().map_err(|_| io::Error::other("database unavailable"))?;
        let mut statement = connection
            .prepare("SELECT path,filename,sha256 FROM ui_uploads WHERE chat_id=? AND path LIKE 'inputs/%' ORDER BY rowid")
            .map_err(io::Error::other)?;
        let rows = statement
            .query_map([chat], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .map_err(io::Error::other)?;
        rows.collect::<rusqlite::Result<_>>().map_err(io::Error::other)?
    };
    let root = ui_uploads::root(db, chat)?;
    let workspace = TaskWorkspace::open(&root)?;
    for (path, filename, sha256) in &uploads {
        // Uploads are stored as `inputs/<id>-<name>`, one plain segment under `inputs/`.
        let Some(stored) = path.strip_prefix("inputs/").filter(|name| !name.contains('/')) else { continue };
        let file = root.join(path);
        let source = kageha_workspace::task::InputSource { key: path, name: filename, fallback: stored, file: &file, sha256 };
        if let Err(error) = workspace.place_input(&source) {
            eprintln!("kageha: attachment {path} was not placed in the task workspace: {error}");
        }
    }
    Ok(workspace.inputs()?)
}

/// [`place_inputs`] for a chat whose workspace already exists, after a new upload, so a
/// running turn can read it. A chat without a workspace gets its inputs at turn start.
pub(crate) fn place_new_inputs(db: &Database, chat: &str) {
    let placed = match existing_root(db, chat) {
        Ok(Some(_)) => place_inputs(db, chat).map(drop),
        Ok(None) => Ok(()),
        Err(error) => Err(error),
    };
    if let Err(error) = placed {
        eprintln!("kageha: attachments were not placed in the task workspace: {error}");
    }
}

/// A system note for the turn naming where its attachments are in the Task_Workspace, so
/// the shell and other workspace tools use the right paths. `None` when the turn has no
/// placed attachments. Built once at turn start from the manifest, so it is stable.
pub(crate) fn inputs_note(db: &Database, chat: &str, request: &str) -> Option<serde_json::Value> {
    let paths: Vec<String> = db
        .lock()
        .ok()?
        .query_row(
            "SELECT paths FROM ui_turn_uploads WHERE chat_id=? AND request_id=?",
            params![chat, request],
            |row| row.get::<_, String>(0),
        )
        .ok()
        .and_then(|paths| serde_json::from_str(&paths).ok())?;
    if paths.is_empty() || existing_root(db, chat).ok()?.is_none() {
        return None;
    }
    let inputs = open_for_chat(db, chat).ok()?.inputs().ok()?;
    let lines: Vec<String> = paths
        .iter()
        .filter_map(|path| inputs.iter().find(|input| input.key == *path))
        .map(|input| format!("- `{}` is `{}`", input.key, input.path))
        .collect();
    if lines.is_empty() {
        return None;
    }
    Some(serde_json::json!({"role":"system","content":format!(
        "The attached files are also in your task workspace, where list_workspace_files and the shell (working directory: the workspace) can read them:\n{}\nThey are the user's files, not your artifacts. read_file accepts either path.",
        lines.join("\n")
    )}))
}

/// Chat-root relative artifact path for a workspace-relative path, or `None` for paths that
/// are Host-owned (`.kageha/`) or not a plain relative path.
pub(crate) fn artifact_path(workspace_path: &str) -> Option<String> {
    // Only canonical `a/b/c` forms, so one file has exactly one artifact path.
    let mut segments = workspace_path.split('/');
    // Installed tools and package caches are workspace infrastructure, not user
    // deliverables. In particular pip --target pylibs must not publish thousands
    // of native libraries alongside a report.
    let mut parts=workspace_path.split('/').peekable();
    while let Some(part)=parts.next() {
        if parts.peek().is_some() && (matches!(part,"node_modules"|".venv"|"venv"|"pylibs"|"__pycache__"|".git"|".cache")
            || part.ends_with(".dist-info") || part.ends_with(".egg-info")) {return None}
    }
    if workspace_path.split('/').next() == Some(STATE_DIR)
        || !segments.all(|part| !part.is_empty() && part != "." && part != "..")
    {
        return None;
    }
    Some(format!("{WORKSPACE_DIR}/{workspace_path}"))
}

/// Registers workspace file changes as artifacts of `turn`. Idempotent: an unchanged file is
/// left as is and a changed file gets its new hash. Returns the registered artifact paths.
pub(crate) fn register_changes(
    db: &Database,
    chat: &str,
    turn: &str,
    changes: &[FileChange],
) -> io::Result<Vec<String>> {
    let mut connection = db
        .lock()
        .map_err(|_| io::Error::other("database unavailable"))?;
    let tx = connection.transaction().map_err(io::Error::other)?;
    let mut registered = Vec::new();
    for change in changes {
        if kageha_workspace::task::is_dependency_path(&change.path) {
            continue;
        }
        let Some(path) = artifact_path(&change.path) else {
            continue;
        };
        let filename = change.path.rsplit('/').next().unwrap_or(&change.path);
        let size = i64::try_from(change.size).map_err(io::Error::other)?;
        tx.execute(
            "INSERT INTO ui_uploads(chat_id,path,filename,mime,sha256,size) VALUES (?,?,?,?,?,?)
             ON CONFLICT(chat_id,path) DO UPDATE SET filename=excluded.filename,mime=excluded.mime,
             sha256=excluded.sha256,size=excluded.size",
            params![chat, path, filename, ui_uploads::mime(filename), change.sha256, size],
        )
        .map_err(io::Error::other)?;
        tx.execute(
            "INSERT OR IGNORE INTO ui_turn_artifacts(chat_id,turn_id,path) VALUES (?,?,?)",
            params![chat, turn, path],
        )
        .map_err(io::Error::other)?;
        registered.push(path);
    }
    tx.commit().map_err(io::Error::other)?;
    Ok(registered)
}

/// Writes `notes/progress.md` when the runtime compacts the context (Requirement 11.3) and
/// registers it as an artifact of `turn`. It is an ordinary, user-visible workspace file: it
/// shows in the Task_Workspace file tree and the turn's files, and the agent can read it.
/// Each compaction replaces the note with the latest summary. Returns the artifact path.
pub(crate) fn write_progress_note(
    db: &Database,
    chat: &str,
    turn: &str,
    summary: &kageha_contracts::execution::CompactionSummary,
) -> io::Result<Option<String>> {
    let workspace = open_for_chat(db, chat)?;
    // A worktree worker's root is a Git checkout: the Host adds no files of its own there.
    if !workspace.state_inside() {
        return Ok(None);
    }
    let change = workspace.write(
        kageha_contracts::execution::PROGRESS_NOTE_PATH,
        summary.progress_markdown().as_bytes(),
    )?;
    Ok(register_changes(db, chat, turn, &[change])?.into_iter().next())
}

/// The agent's plan, `todo.md` in the chat's Task_Workspace, for the runtime to restate before
/// each model call (Requirement 11.6). `None` when the workspace does not exist; it is not
/// created. In an existing workspace, opening it restores a deleted `todo.md` as an empty
/// file, which the runtime skips as blank. Invalid UTF-8 is replaced. A plan over
/// [`PLAN_SNAPSHOT_BYTES`](kageha_contracts::execution::PLAN_SNAPSHOT_BYTES) is cut at a
/// character boundary and ends with a note giving the shown and total sizes.
pub(crate) fn read_plan(db: &Database, chat: &str) -> io::Result<Option<String>> {
    use kageha_contracts::execution::{PLAN_PATH, PLAN_SNAPSHOT_BYTES};
    use std::io::Read;
    if existing_root(db, chat)?.is_none() {
        return Ok(None);
    }
    let workspace = open_for_chat(db, chat)?;
    let path = workspace.resolve(PLAN_PATH)?;
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let total = file.metadata()?.len();
    let mut bytes = Vec::new();
    file.take(PLAN_SNAPSHOT_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if bytes.len() > PLAN_SNAPSHOT_BYTES {
        let mut end = PLAN_SNAPSHOT_BYTES.min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str(&format!(
            "\n[{PLAN_PATH} truncated: showing the first {end} of {} bytes. Read the file for the rest.]",
            total.max(bytes.len() as u64)
        ));
    }
    Ok(Some(text))
}

fn registered_hash(db: &rusqlite::Connection, chat: &str, path: &str) -> io::Result<Option<String>> {
    db.query_row(
        "SELECT sha256 FROM ui_uploads WHERE chat_id=? AND path=?",
        params![chat, path],
        |row| row.get(0),
    )
    .optional()
    .map_err(io::Error::other)
}

/// Records the hash a workspace artifact had when it was attached as evidence. Re-attaching
/// the same reference replaces the pin. Non-workspace references are not pinned.
pub(crate) fn pin_evidence(
    db: &Database,
    chat: &str,
    turn: &str,
    outcome: &str,
    path: &str,
) -> io::Result<()> {
    if !path.starts_with(&format!("{WORKSPACE_DIR}/")) {
        return Ok(());
    }
    let connection = db
        .lock()
        .map_err(|_| io::Error::other("database unavailable"))?;
    let Some(hash) = registered_hash(&connection, chat, path)? else {
        return Ok(());
    };
    connection
        .execute(
            "INSERT INTO ui_artifact_evidence(chat_id,turn_id,outcome_id,path,sha256) VALUES (?,?,?,?,?)
             ON CONFLICT(chat_id,turn_id,outcome_id,path) DO UPDATE SET sha256=excluded.sha256",
            params![chat, turn, outcome, path, hash],
        )
        .map_err(io::Error::other)?;
    Ok(())
}

/// False when the evidence was pinned to a hash the artifact no longer has. Evidence without
/// a pin (generated media, legacy files, other outcome kinds) is unaffected.
pub(crate) fn evidence_current(
    db: &Database,
    chat: &str,
    turn: &str,
    outcome: &str,
    path: &str,
) -> io::Result<bool> {
    if !path.starts_with(&format!("{WORKSPACE_DIR}/")) {
        return Ok(true);
    }
    let connection = db
        .lock()
        .map_err(|_| io::Error::other("database unavailable"))?;
    let pinned: Option<String> = connection
        .query_row(
            "SELECT sha256 FROM ui_artifact_evidence WHERE chat_id=? AND turn_id=? AND outcome_id=? AND path=?",
            params![chat, turn, outcome, path],
            |row| row.get(0),
        )
        .optional()
        .map_err(io::Error::other)?;
    let Some(pinned) = pinned else {
        return Ok(true);
    };
    Ok(registered_hash(&connection, chat, path)?.is_some_and(|hash| hash == pinned))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui_task_completion::proof;
    use kageha_workflow::completion::OutcomeKind;

    fn database(directory: &Path) -> Database {
        let connection = rusqlite::Connection::open(directory.join("native.sqlite")).unwrap();
        ui_uploads::initialize(&connection).unwrap();
        crate::native_media::initialize(&connection).unwrap();
        initialize(&connection).unwrap();
        connection
            .execute_batch("CREATE TABLE ui_sessions(id TEXT PRIMARY KEY);INSERT INTO ui_sessions VALUES ('chat');")
            .unwrap();
        crate::agent_host::DatabaseState::open(connection).unwrap()
    }

    fn artifact(db: &Database, turn: &str, path: &str) -> bool {
        proof(db, "chat", turn, &OutcomeKind::Artifact, path).unwrap()
    }

    #[test]
    fn dependencies_are_not_published_as_deliverables() {
        for path in ["pylibs/PIL/image.py","node_modules/pkg/index.js","tools/.venv/lib/site.py","outputs/__pycache__/a.pyc","pkg.dist-info/METADATA"] {
            assert!(artifact_path(path).is_none(),"{path}");
        }
        assert_eq!(artifact_path("outputs/report.md").as_deref(),Some("workspace/outputs/report.md"));
    }
    #[test]
    fn workspace_lives_under_the_chat_root() {
        let directory = tempfile::tempdir().unwrap();
        let db = database(directory.path());
        let workspace = open_for_chat(&db, "chat").unwrap();
        assert_eq!(
            workspace.root(),
            ui_uploads::root(&db, "chat").unwrap().join(WORKSPACE_DIR)
        );
        assert!(workspace.root().join("outputs").is_dir());
        assert!(open_for_chat(&db, "unknown").is_err());
    }

    #[test]
    fn the_progress_note_is_written_and_registered_as_an_artifact() {
        let directory = tempfile::tempdir().unwrap();
        let db = database(directory.path());
        let summary = kageha_contracts::execution::CompactionSummary {
            reason: "threshold".into(),
            files: vec!["outputs/report.md".into()],
            ..Default::default()
        };
        let path = write_progress_note(&db, "chat", "turn", &summary).unwrap();
        assert_eq!(path.as_deref(), Some("workspace/notes/progress.md"));
        let workspace = open_for_chat(&db, "chat").unwrap();
        let text = std::fs::read_to_string(workspace.root().join("notes/progress.md")).unwrap();
        assert_eq!(text, summary.progress_markdown());
        assert!(artifact(&db, "turn", "workspace/notes/progress.md"));
    }

    #[test]
    fn the_plan_is_read_without_creating_the_workspace_and_capped() {
        use kageha_contracts::execution::PLAN_SNAPSHOT_BYTES;
        let directory = tempfile::tempdir().unwrap();
        let db = database(directory.path());
        assert_eq!(read_plan(&db, "chat").unwrap(), None);
        assert!(!ui_uploads::root_path(&db, "chat").unwrap().exists());
        assert!(read_plan(&db, "unknown").is_err());
        let workspace = open_for_chat(&db, "chat").unwrap();
        // A new workspace has an empty plan; the runtime skips blank plans.
        assert_eq!(read_plan(&db, "chat").unwrap().as_deref(), Some(""));
        workspace.write("todo.md", b"- [ ] draft\n- [ ] verify\n").unwrap();
        assert_eq!(read_plan(&db, "chat").unwrap().as_deref(), Some("- [ ] draft\n- [ ] verify\n"));
        let long = "é".repeat(PLAN_SNAPSHOT_BYTES);
        workspace.write("todo.md", long.as_bytes()).unwrap();
        let text = read_plan(&db, "chat").unwrap().unwrap();
        let (head, note) = text.split_once("\n[todo.md truncated").unwrap();
        assert_eq!(head, "é".repeat(PLAN_SNAPSHOT_BYTES / 2));
        assert!(note.contains(&format!("first {PLAN_SNAPSHOT_BYTES} of {} bytes", long.len())));
        // Opening the workspace restores a deleted plan as an empty file.
        std::fs::remove_file(workspace.root().join("todo.md")).unwrap();
        assert_eq!(read_plan(&db, "chat").unwrap().as_deref(), Some(""));
    }

    #[test]
    fn registered_files_prove_artifacts_until_they_change() {
        let directory = tempfile::tempdir().unwrap();
        let db = database(directory.path());
        let workspace = open_for_chat(&db, "chat").unwrap();
        let change = workspace.write("outputs/report.md", b"first").unwrap();
        assert!(!artifact(&db, "turn", "workspace/outputs/report.md"));
        let paths = register_changes(&db, "chat", "turn", &[change.clone()]).unwrap();
        assert_eq!(paths, ["workspace/outputs/report.md"]);
        assert!(artifact(&db, "turn", "workspace/outputs/report.md"));
        assert!(!artifact(&db, "other", "workspace/outputs/report.md"));
        // Registering the same change again is a no-op.
        register_changes(&db, "chat", "turn", &[change]).unwrap();
        assert!(artifact(&db, "turn", "workspace/outputs/report.md"));
        // An unregistered change (e.g. a shell write before the next scan) invalidates it.
        std::fs::write(workspace.root().join("outputs/report.md"), b"changed").unwrap();
        assert!(!artifact(&db, "turn", "workspace/outputs/report.md"));
        // Registering the new bytes upserts the hash.
        let changes = workspace.scan_changes().unwrap();
        register_changes(&db, "chat", "turn", &changes).unwrap();
        assert!(artifact(&db, "turn", "workspace/outputs/report.md"));
        let rows: i64 = db
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM ui_uploads WHERE path='workspace/outputs/report.md'", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[test]
    fn host_state_and_unsafe_paths_are_not_registered() {
        let directory = tempfile::tempdir().unwrap();
        let db = database(directory.path());
        open_for_chat(&db, "chat").unwrap();
        let change = |path: &str| FileChange {
            path: path.into(),
            sha256: "0".repeat(64),
            size: 1,
        };
        let changes = [
            change(".kageha/manifest.sqlite"),
            change(".kageha/tool-output/call.txt"),
            change("../escape.txt"),
            change("/etc/passwd"),
            change("notes/./x.md"),
            change(""),
            change("venv/lib/libpython.dylib"),
            change("node_modules/a/index.js"),
            change("pylibs/pkg-1.0.dist-info/RECORD"),
        ];
        assert!(register_changes(&db, "chat", "turn", &changes).unwrap().is_empty());
        let rows: i64 = db
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM ui_turn_artifacts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 0);
        assert_eq!(artifact_path("notes/.kageha/x.md").as_deref(), Some("workspace/notes/.kageha/x.md"));
    }

    #[test]
    fn evidence_is_stale_once_the_artifact_is_reregistered_with_new_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let db = database(directory.path());
        let workspace = open_for_chat(&db, "chat").unwrap();
        let path = "workspace/outputs/brief.md";
        let first = workspace.write("outputs/brief.md", b"draft").unwrap();
        register_changes(&db, "chat", "turn", &[first]).unwrap();
        assert!(evidence_current(&db, "chat", "turn", "brief", path).unwrap());
        pin_evidence(&db, "chat", "turn", "brief", path).unwrap();
        assert!(evidence_current(&db, "chat", "turn", "brief", path).unwrap());
        let second = workspace.write("outputs/brief.md", b"rewritten").unwrap();
        register_changes(&db, "chat", "turn", &[second]).unwrap();
        // The file itself is a valid current artifact, but the earlier evidence is stale.
        assert!(artifact(&db, "turn", path));
        assert!(!evidence_current(&db, "chat", "turn", "brief", path).unwrap());
        // Attaching again pins the new version.
        pin_evidence(&db, "chat", "turn", "brief", path).unwrap();
        assert!(evidence_current(&db, "chat", "turn", "brief", path).unwrap());
        // Generated (non-workspace) artifacts are never pinned.
        pin_evidence(&db, "chat", "turn", "media", "artifacts/x.png").unwrap();
        assert!(evidence_current(&db, "chat", "turn", "media", "artifacts/x.png").unwrap());
    }
}
