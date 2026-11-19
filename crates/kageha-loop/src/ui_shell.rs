//! The `shell` and `shell_job` tools (spec autonomous-agent-loop, task 10.3; Requirements
//! 2.1–2.4, 2.7 and 3.4).
//!
//! Each chat has one persistent, sandboxed shell session, kept by a [`ShellBackend`] and
//! keyed by chat. Commands run in the chat's Task_Workspace. By default the backend is a
//! [`ShellManager`] in this process. With `KAGEHA_SHELL_SERVICE=process` the Supervisor runs
//! the shell service as its own process, so shells and jobs survive a server restart, and the
//! backend is `ui_shell_socket::SocketShell`, which implements the same trait over the
//! service's socket protocol (`kageha_workspace::shell::ShellService`). See
//! `supervisor::ShellMode` for why in-process is the default for now.
//!
//! After every command the Host diffs the workspace with `TaskWorkspace::scan_changes`,
//! registers each created or changed file as an artifact of the turn, and reports them as
//! `files_changed`. Each command, its exit code and the files it changed are published as a
//! `shell_command` event for the Run_Journal. Jobs are recorded in `ui_shell_jobs` with
//! their PID and process start time, so a later host process can adopt them.
//!
//! A foreground command that is still running at `timeout_s` is not killed. It continues
//! as a background job and the call fails with `command_timeout` and the `job_id`.
//!
//! Escalation (task 10.4, Requirement 2.6, design section 2):
//!
//! 1. When a command fails and its stderr shows the sandbox refused a path outside the
//!    write roots (`kageha_workspace::shell::sandbox_denials`), the call fails with
//!    `policy_denied`, the `denied_paths`, the command's output and, when the user can grant
//!    those paths, `retry_with: {"escalate": true}`.
//! 2. `escalate: true` (with a `reason`) is classified by the Approval_Policy as
//!    `external_write` with the command prefix as scope. An allowlist entry for the prefix
//!    runs it without a prompt. Otherwise an approval request shows the exact command, the
//!    working directory, the reason and the write roots, through the chat's usual approval
//!    path (`ui_approvals::require_recorded`), so a chat with Full access approves it
//!    automatically. A `full` decision on the prompt also allowlists the prefix
//!    (`ui_approvals::decide`). A denial fails with `approval_denied` and the approval id,
//!    which `report_blocker` accepts as a failed attempt.
//! 3. An approved command runs once in a new sandboxed shell (not the chat's session)
//!    whose write roots add the user's granted scope ([`Escalation`]): the home directory
//!    and the system temp directories, never `/`. Its dispatch is recorded with
//!    `ui_effects::run`, so it is never sent twice for one tool call.
use crate::ui_tools::{ensure_disk_space, tool_error, workspace_failure};
use crate::{ui_approvals, ui_workspace, webui::Database};
use kageha_workspace::shell::{
    resolve_path, sandbox_denials, CommandOutput, Foreground, JobOutput, JobRecord, JobState,
    JobStatus, SessionRoots, ShellManager, ShellPaths,
};
use kageha_workspace::task::TaskWorkspace;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use std::{
    io,
    path::{Path, PathBuf},
    sync::{atomic::AtomicBool, OnceLock},
    time::Duration,
};

pub(crate) const DEFAULT_TIMEOUT_S: u64 = 600;
pub(crate) const MAX_TIMEOUT_S: u64 = 3600;
const MAX_COMMAND_BYTES: usize = 64 * 1024;
const MAX_REASON_BYTES: usize = 1024;
/// One `shell_job` output window. Below the 32 KiB spill bound, with room for JSON
/// escaping, so a window is not spilled again.
const OUTPUT_WINDOW: usize = 24 * 1024;
/// `files_changed` entries listed in one result. All changes are registered either way.
const FILES_LISTED: usize = 100;

pub(crate) fn initialize(db: &rusqlite::Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS ui_shell_jobs(job_id TEXT PRIMARY KEY,chat_id TEXT NOT NULL,
            command TEXT NOT NULL,pid INTEGER NOT NULL,started_ns INTEGER NOT NULL,
            state TEXT NOT NULL,exit_code INTEGER,log_path TEXT NOT NULL,machine TEXT);
         CREATE INDEX IF NOT EXISTS ui_shell_jobs_chat ON ui_shell_jobs(chat_id);",
    )?;
    let has_machine: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('ui_shell_jobs') WHERE name='machine')", [], |row| row.get(0))?;
    if !has_machine {
        db.execute("ALTER TABLE ui_shell_jobs ADD COLUMN machine TEXT", [])?;
    }
    Ok(())
}

/// The chat whose session a command runs in, that chat's Task_Workspace root, and the
/// session's state directory and extra read and hidden roots (`ui_workspace::shell_roots`:
/// a delegated worker reads its parent's workspace).
pub(crate) struct ShellScope<'a> {
    pub chat: &'a str,
    pub workspace: &'a Path,
    pub roots: SessionRoots,
}

impl<'a> ShellScope<'a> {
    /// The scope of `chat`'s session in `workspace`.
    pub(crate) fn of(db: &Database, chat: &'a str, workspace: &'a TaskWorkspace) -> io::Result<Self> {
        Ok(Self { chat, workspace: workspace.root(), roots: ui_workspace::shell_roots(db, chat, workspace)? })
    }
}

/// Where shell sessions and jobs live. Implemented in-process by [`ShellManager`]; the
/// separate shell service will implement it over its socket.
pub(crate) trait ShellBackend: Send + Sync {
    fn machine(&self) -> Option<&str> { None }
    /// Runs `command` in the chat's session, opening the session if needed.
    fn run(&self, scope: &ShellScope<'_>, command: &str, call_id: &str, timeout: Duration, cancel: &AtomicBool) -> io::Result<Foreground>;
    /// Starts `command` as a background job in the chat's session environment.
    fn start_job(&self, scope: &ShellScope<'_>, command: &str) -> io::Result<JobStatus>;
    fn job_status(&self, chat: &str, job_id: &str) -> io::Result<JobStatus>;
    fn job_output(&self, chat: &str, job_id: &str, offset: u64, max: usize) -> io::Result<JobOutput>;
    fn stop_job(&self, chat: &str, job_id: &str) -> io::Result<JobStatus>;
    /// Takes over a job recorded by an earlier host process.
    fn adopt_job(&self, scope: &ShellScope<'_>, record: JobRecord) -> io::Result<JobStatus>;
    /// The write roots of the chat's session sandbox.
    fn write_roots(&self, scope: &ShellScope<'_>) -> Vec<PathBuf>;
    /// The session's working directory, or `workspace` when it has no idle session.
    fn current_dir(&self, chat: &str, workspace: &Path) -> PathBuf;
    /// Runs one escalated command in a new sandboxed shell starting in `cwd`, with
    /// `write_roots` added. The chat's session is not used or changed.
    #[allow(clippy::too_many_arguments)]
    fn run_once(&self, scope: &ShellScope<'_>, cwd: &Path, write_roots: Vec<PathBuf>, command: &str, call_id: &str,
        timeout: Duration, cancel: &AtomicBool) -> io::Result<Foreground>;
}

impl ShellBackend for ShellManager {
    fn run(&self, scope: &ShellScope<'_>, command: &str, call_id: &str, timeout: Duration, cancel: &AtomicBool) -> io::Result<Foreground> {
        self.open_session_in(scope.chat, scope.workspace, &scope.roots)?;
        ShellManager::run(self, scope.chat, command, call_id, timeout, cancel)
    }
    fn start_job(&self, scope: &ShellScope<'_>, command: &str) -> io::Result<JobStatus> {
        self.open_session_in(scope.chat, scope.workspace, &scope.roots)?;
        ShellManager::start_job(self, scope.chat, command)
    }
    fn job_status(&self, chat: &str, job_id: &str) -> io::Result<JobStatus> {
        ShellManager::job_status(self, chat, job_id)
    }
    fn job_output(&self, chat: &str, job_id: &str, offset: u64, max: usize) -> io::Result<JobOutput> {
        ShellManager::job_output(self, chat, job_id, offset, max)
    }
    fn stop_job(&self, chat: &str, job_id: &str) -> io::Result<JobStatus> {
        ShellManager::stop_job(self, chat, job_id)
    }
    fn adopt_job(&self, scope: &ShellScope<'_>, record: JobRecord) -> io::Result<JobStatus> {
        ShellManager::adopt_job_in(self, scope.workspace, scope.roots.state_dir.as_deref(), record)
    }
    fn write_roots(&self, scope: &ShellScope<'_>) -> Vec<PathBuf> {
        session_write_roots(scope, self.toolchain_cache())
    }
    fn current_dir(&self, chat: &str, workspace: &Path) -> PathBuf {
        self.session_dir(chat).unwrap_or_else(|| workspace.to_path_buf())
    }
    fn run_once(&self, scope: &ShellScope<'_>, cwd: &Path, write_roots: Vec<PathBuf>, command: &str, call_id: &str,
        timeout: Duration, cancel: &AtomicBool) -> io::Result<Foreground> {
        ShellManager::run_once_in(self, scope.chat, scope.workspace, &scope.roots, cwd, write_roots, command, call_id, timeout, cancel)
    }
}

/// The Approval_Policy inputs of escalated commands. Tests build their own.
pub(crate) struct Escalation {
    /// `approvals_allowlist.json`. `None` uses the default (`~/.kageha/`).
    pub allowlist: Option<PathBuf>,
    /// The user's granted scope: extra write roots of an approved escalated command.
    /// Roots that don't exist, and `/`, are left out.
    pub granted_roots: Vec<PathBuf>,
    /// Host directories the session sandbox doesn't mount. "No such file or directory" on a
    /// path under one of them is a sandbox denial.
    pub hidden_roots: Vec<PathBuf>,
}

impl Escalation {
    /// This host's policy. No setting stores path grants yet (`ui_permissions` and the
    /// chat permission profiles only choose who approves), so approving an escalated
    /// command grants the user's home directory and the system temp directories for that
    /// one command. The approval prompt lists them. On Linux `bwrap` doesn't mount the
    /// home directory at all, so a missing path there is a denial.
    fn host() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from).filter(|home| home.is_absolute());
        let mut granted_roots: Vec<PathBuf> = home.iter().cloned().collect();
        granted_roots.extend([std::env::temp_dir(), PathBuf::from("/tmp")]);
        let hidden_roots = if cfg!(target_os = "linux") { home.into_iter().collect() } else { Vec::new() };
        Self { allowlist: None, granted_roots, hidden_roots }
    }

    /// The granted roots that exist, canonical and without duplicates. Never `/`.
    fn roots(&self) -> Vec<PathBuf> {
        let mut roots = Vec::new();
        for root in &self.granted_roots {
            if let Ok(root) = root.canonicalize() {
                if root.parent().is_some() && root.is_dir() && !roots.contains(&root) {
                    roots.push(root);
                }
            }
        }
        roots
    }
}

/// Whether the host could grant an escalated shell write to this project root.
/// Handoffs use the same boundary and must not widen the shell's write scope.
pub(crate) fn escalated_write_root_allows(path: &Path) -> io::Result<bool> {
    let path = path.canonicalize()?;
    Ok(Escalation::host().roots().iter().any(|root| path.starts_with(root)))
}

/// The write roots of the session sandbox for `scope`.
pub(crate) fn session_write_roots(scope: &ShellScope<'_>, toolchain_cache: &Path) -> Vec<PathBuf> {
    let state = scope.roots.state_dir.clone().unwrap_or_else(|| scope.workspace.join(kageha_workspace::task::STATE_DIR));
    ShellPaths::layout_in(scope.workspace, &state, toolchain_cache).sandbox().write_roots
}

fn display(paths: &[PathBuf]) -> Vec<String> {
    paths.iter().map(|path| path.display().to_string()).collect()
}

/// The process-wide backend: the supervised shell service when it runs as a process,
/// else one [`ShellManager`] using the Toolchain_Cache.
fn backend() -> &'static dyn ShellBackend {
    if let Some(socket) = crate::supervisor::shell_backend() {
        return socket;
    }
    static BACKEND: OnceLock<ShellManager> = OnceLock::new();
    BACKEND.get_or_init(|| ShellManager::new(toolchain_cache()))
}

/// `$KAGEHA_HOME/toolchains`, else `~/.kageha/toolchains`.
pub(crate) fn toolchain_cache() -> PathBuf {
    #[cfg(test)]
    {
        std::env::temp_dir().join(format!("kageha-loop-test-toolchains-{}", std::process::id()))
    }
    #[cfg(not(test))]
    {
        std::env::var_os("KAGEHA_HOME")
            .map(|home| PathBuf::from(home).join("toolchains"))
            .or_else(|| kageha_workspace::shell::default_toolchain_cache(&kageha_workspace::shell::host_environment()).ok())
            .unwrap_or_else(|| std::env::temp_dir().join("kageha-toolchains"))
    }
}

/// Whether `shell` and `shell_job` can run on this host (the OS sandbox is required).
pub(crate) fn available() -> bool {
    kageha_workspace::shell::sandbox_available()
}

/// Dispatches `shell` or `shell_job` for `call` (`{id, name, arguments}`).
pub(crate) fn tool(db: &Database, chat: &str, turn: &str, call: &Value, cancel: &AtomicBool) -> io::Result<String> {
    let args = &call["arguments"];
    crate::supervisor::require("shell")?;
    match call["name"].as_str().unwrap_or("") {
        "shell" => shell(backend(), &Escalation::host(), db, chat, turn, &file_call_id(call["id"].as_str().unwrap_or("")), args, cancel),
        "shell_job" => shell_job(backend(), db, chat, turn, args),
        _ => Err(invalid("unknown shell tool")),
    }
}

pub(crate) fn docker_tool(db: &Database, chat: &str, turn: &str, machine: &str, call: &Value, cancel: &AtomicBool) -> io::Result<String> {
    crate::docker_desktop::validate(db, chat, machine)?;
    let backend = crate::ui_shell_socket::SocketShell::docker(machine);
    let args = &call["arguments"];
    match call["name"].as_str().unwrap_or("") {
        "shell" => shell(&backend, &Escalation { allowlist: None, granted_roots: vec![], hidden_roots: vec![] },
            db, chat, turn, &file_call_id(call["id"].as_str().unwrap_or("")), args, cancel),
        "shell_job" => shell_job(&backend, db, chat, turn, args),
        _ => Err(invalid("unknown shell tool")),
    }
}

fn invalid(summary: &str) -> io::Error {
    tool_error(io::ErrorKind::InvalidInput, "invalid_arguments", false, summary)
}

/// A provider call id as a spill file name part (`[A-Za-z0-9_.-]`, at most 128 bytes, not
/// starting with `.`).
pub(crate) fn file_call_id(id: &str) -> String {
    let mut name: String = id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.') { c } else { '_' })
        .take(kageha_workspace::shell::MAX_CALL_ID)
        .collect();
    if name.is_empty() || name.starts_with('.') {
        name.insert(0, 'c');
        name.truncate(kageha_workspace::shell::MAX_CALL_ID);
    }
    name
}

fn flag(args: &Value, name: &str) -> io::Result<bool> {
    match &args[name] {
        Value::Null => Ok(false),
        Value::Bool(value) => Ok(*value),
        _ => Err(invalid(&format!("{name} must be true or false"))),
    }
}

pub(crate) fn free_bytes(workspace: &TaskWorkspace) -> io::Result<u64> {
    #[cfg(test)]
    if let Some(free) = crate::ui_tools::test_free_disk::get(workspace.root()) {
        return Ok(free);
    }
    workspace.free_disk_bytes().map_err(workspace_failure)
}

/// The tool error for a backend failure.
fn shell_failure(error: io::Error) -> io::Error {
    match error.kind() {
        io::ErrorKind::InvalidInput => invalid(&error.to_string()),
        io::ErrorKind::NotFound if error.to_string().starts_with("No job") => tool_error(
            io::ErrorKind::NotFound, "job_not_found", false,
            &format!("{error}. Use a job_id returned by shell in this chat."),
        ),
        kind => tool_error(kind, "shell_error", false, &format!(
            "{error}. If the shell session ended (for example after `exit`), the next shell call starts a new session in the directory and with the exported variables saved after the last finished command."
        )),
    }
}

/// Files the last command created or changed, registered as artifacts of `turn`.
struct Changes {
    /// `{artifact_path, sha256, size}` for the first [`FILES_LISTED`] files.
    listed: Vec<Value>,
    paths: Vec<String>,
    omitted: usize,
}

impl Changes {
    fn add_to(&self, result: &mut Value) {
        result["files_changed"] = json!(self.listed);
        if self.omitted > 0 {
            result["files_changed_omitted"] = json!(self.omitted);
        }
    }
}

fn register_changes(db: &Database, chat: &str, turn: &str, workspace: &TaskWorkspace) -> io::Result<Changes> {
    let changes = workspace.scan_changes().map_err(workspace_failure)?;
    ui_workspace::register_changes(db, chat, turn, &changes)?;
    let files: Vec<_> = changes.iter()
        .filter_map(|change| ui_workspace::artifact_path(&change.path).map(|path| (path, change)))
        .collect();
    Ok(Changes {
        listed: files.iter().take(FILES_LISTED)
            .map(|(path, change)| json!({"artifact_path":path,"sha256":change.sha256,"size":change.size}))
            .collect(),
        paths: files.iter().take(FILES_LISTED).map(|(path, _)| path.clone()).collect(),
        omitted: files.len().saturating_sub(FILES_LISTED),
    })
}

/// Publishes a `shell_command` event for the Run_Journal. Advisory: a failed publication
/// does not change the command's result.
fn journal(db: &Database, chat: &str, turn: &str, payload: Value) {
    let _ = crate::webui::emit(db, chat, turn, "event", json!({"kind":"shell_command","payload":payload}));
}

fn record_job(db: &Database, chat: &str, job: &JobStatus, machine: Option<&str>) -> io::Result<()> {
    let connection = db.lock().map_err(|_| io::Error::other("database unavailable"))?;
    connection.execute(
        "INSERT INTO ui_shell_jobs(job_id,chat_id,command,pid,started_ns,state,exit_code,log_path,machine)
         VALUES (?,?,?,?,?,?,?,?,?)
         ON CONFLICT(job_id) DO UPDATE SET state=excluded.state,exit_code=excluded.exit_code",
        params![job.record.job_id, chat, job.record.command, job.record.pid, job.record.started_ns as i64,
            job.state.as_str(), job.exit_code, job.record.log_path, machine],
    ).map_err(io::Error::other)?;
    Ok(())
}

/// The stored record of `job_id` in `chat`, for adoption after a restart.
fn stored_job(db: &Database, chat: &str, job_id: &str, workspace: &TaskWorkspace, machine: Option<&str>) -> io::Result<Option<JobRecord>> {
    let connection = db.lock().map_err(|_| io::Error::other("database unavailable"))?;
    let row = connection.query_row(
        "SELECT command,pid,started_ns,log_path FROM ui_shell_jobs WHERE job_id=? AND chat_id=? AND machine IS ?",
        params![job_id, chat, machine],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?, row.get::<_, String>(3)?)),
    ).optional().map_err(io::Error::other)?;
    Ok(row.map(|(command, pid, started_ns, log_path)| {
        let (_, stderr_path) = JobRecord::paths_for(job_id, true);
        let stderr_path = stderr_path.filter(|path| workspace.host_path(path).is_some_and(|file| file.is_file()));
        JobRecord { job_id: job_id.into(), chat: chat.into(), command, pid: pid.max(0) as u32,
            started_ns: started_ns.max(0) as u64, log_path, stderr_path }
    }))
}

/// One job [`readopt_jobs`] handed to the backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Readopted {
    pub chat: String,
    pub job_id: String,
    /// `running` when the recorded PID is alive with the recorded start time; otherwise
    /// the job ended while no host watched it, and its end state comes from its exit file.
    pub state: JobState,
}

/// Re-adopts every job `ui_shell_jobs` records as running into the process-wide backend
/// (Requirement 10.2). Runs at Host startup and after the shell service restarts.
pub(crate) fn readopt_jobs(db: &Database) -> io::Result<Vec<Readopted>> {
    readopt_jobs_with(backend(), db)
}

/// [`readopt_jobs`] with an explicit backend. A job is adopted only when its leader PID is
/// alive and started at the recorded time (`ShellManager::adopt_job`), so a reused PID is
/// never taken for the job. The row is not changed here: the first `shell_job` call that
/// sees the job ended records its end state and registers the files it changed, as for a
/// job that ended under this process. A chat whose workspace can't be opened is skipped.
pub(crate) fn readopt_jobs_with(backend: &dyn ShellBackend, db: &Database) -> io::Result<Vec<Readopted>> {
    let rows: Vec<(String, String)> = {
        let connection = db.lock().map_err(|_| io::Error::other("database unavailable"))?;
        let mut query = connection
            .prepare("SELECT chat_id,job_id FROM ui_shell_jobs WHERE state='running' AND machine IS NULL ORDER BY chat_id,job_id")
            .map_err(io::Error::other)?;
        let rows = query.query_map([], |row| Ok((row.get(0)?, row.get(1)?))).map_err(io::Error::other)?;
        rows.collect::<rusqlite::Result<_>>().map_err(io::Error::other)?
    };
    let mut adopted = Vec::new();
    for (chat, job_id) in rows {
        let Ok(workspace) = ui_workspace::open_for_chat(db, &chat) else { continue };
        let Ok(scope) = ShellScope::of(db, &chat, &workspace) else { continue };
        let Some(record) = stored_job(db, &chat, &job_id, &workspace, None)? else { continue };
        match backend.adopt_job(&scope, record) {
            Ok(status) => adopted.push(Readopted { chat, job_id, state: status.state }),
            Err(error) => eprintln!("shell: job {job_id} of chat {chat} was not re-adopted: {error}"),
        }
    }
    Ok(adopted)
}

/// Saves a job's new state. On its first observed end, registers the files it changed
/// and journals the end. Returns those changes then.
fn observe(db: &Database, chat: &str, turn: &str, workspace: &TaskWorkspace, job_id: &str, command: &str,
    state: JobState, exit_code: Option<i32>) -> io::Result<Option<Changes>> {
    if state == JobState::Running {
        return Ok(None);
    }
    let ended = db.lock().map_err(|_| io::Error::other("database unavailable"))?.execute(
        "UPDATE ui_shell_jobs SET state=?,exit_code=? WHERE job_id=? AND chat_id=? AND state='running'",
        params![state.as_str(), exit_code, job_id, chat],
    ).map_err(io::Error::other)?;
    if ended == 0 {
        return Ok(None);
    }
    let changes = register_changes(db, chat, turn, workspace)?;
    journal(db, chat, turn, json!({"job_id":job_id,"command":command,"state":state.as_str(),
        "exit_code":exit_code,"files_changed":changes.paths,"completed":true}));
    Ok(Some(changes))
}

/// `shell(command, timeout_s, background, escalate, reason)`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn shell(backend: &dyn ShellBackend, escalation: &Escalation, db: &Database, chat: &str, turn: &str,
    call_id: &str, args: &Value, cancel: &AtomicBool) -> io::Result<String> {
    let command = args["command"].as_str().filter(|command| !command.trim().is_empty())
        .ok_or_else(|| invalid("command must be a non-empty string"))?;
    if command.len() > MAX_COMMAND_BYTES || command.contains('\0') {
        return Err(invalid("command must be at most 64 KiB and contain no NUL bytes. Write long scripts to a file and run the file."));
    }
    let timeout_s = match &args["timeout_s"] {
        Value::Null => DEFAULT_TIMEOUT_S,
        value => value.as_u64().filter(|value| (1..=MAX_TIMEOUT_S).contains(value))
            .ok_or_else(|| invalid("timeout_s must be an integer from 1 to 3600"))?,
    };
    let background = flag(args, "background")?;
    let escalate = flag(args, "escalate")?;
    let reason = match &args["reason"] {
        Value::Null => None,
        Value::String(reason) if reason.len() <= MAX_REASON_BYTES => Some(reason.as_str()),
        _ => return Err(invalid("reason must be a string of at most 1024 bytes")),
    };
    let reason = reason.map(str::trim).filter(|reason| !reason.is_empty());
    if escalate && background {
        return Err(invalid("escalate can't be combined with background. Run the escalated command in the foreground; it may take up to timeout_s."));
    }
    if escalate && reason.is_none() {
        return Err(invalid("escalate needs a reason: say why this command must write outside the task workspace. The user sees it with the approval request."));
    }
    if backend.machine().is_some() && escalate {
        return Err(invalid("Container commands use the Docker workspace and cannot request host escalation"));
    }
    if backend.machine().is_none() && !available() {
        return Err(tool_error(io::ErrorKind::Unsupported, "sandbox_unavailable", false,
            "The OS process sandbox is not available on this host, so shell commands cannot run. Use the file tools instead."));
    }
    let workspace = ui_workspace::open_for_chat(db, chat)?;
    ensure_disk_space(db, chat, turn, "shell", free_bytes(&workspace)?)?;
    let scope = ShellScope::of(db, chat, &workspace)?;
    if escalate {
        let reason = reason.unwrap_or_default();
        return escalated(backend, escalation, db, &scope, turn, call_id, args, command, reason, timeout_s, &workspace, cancel);
    }
    if background {
        let job = backend.start_job(&scope, command).map_err(shell_failure)?;
        record_job(db, chat, &job, backend.machine())?;
        journal(db, chat, turn, json!({"call_id":call_id,"command":command,"reason":reason,
            "background":true,"job_id":job.record.job_id,"state":job.state.as_str()}));
        return Ok(json!({"job_id":job.record.job_id,"state":job.state.as_str(),"log_path":job.record.log_path}).to_string());
    }
    let ran = backend.run(&scope, command, call_id, Duration::from_secs(timeout_s), cancel).map_err(shell_failure)?;
    let changes = register_changes(db, chat, turn, &workspace)?;
    match ran {
        Foreground::Finished(output) => {
            journal(db, chat, turn, json!({"call_id":call_id,"command":command,"reason":reason,
                "background":false,"exit_code":output.exit_code,"duration_ms":output.duration_ms,
                "files_changed":changes.paths}));
            let mut result = output_result(&output);
            changes.add_to(&mut result);
            if output.exit_code != 0 {
                let denied = sandbox_denials(&output.stderr, &backend.write_roots(&scope), &escalation.hidden_roots);
                if !denied.is_empty() {
                    return Err(sandbox_denied(result, &denied, escalation, false));
                }
            }
            Ok(result.to_string())
        }
        Foreground::Detached { job, cancelled } => {
            record_job(db, chat, &job, backend.machine())?;
            journal(db, chat, turn, json!({"call_id":call_id,"command":command,"reason":reason,
                "background":false,"timed_out":!cancelled,"cancelled":cancelled,"job_id":job.record.job_id,
                "files_changed":changes.paths}));
            Err(detached(&job, cancelled, timeout_s, &changes,
                "The next shell call gets a new session in the directory and with the exported variables saved after the last finished command; shell functions and unexported variables are not kept."))
        }
    }
}

fn output_result(output: &CommandOutput) -> Value {
    let mut result = json!({"exit_code":output.exit_code,"stdout":output.stdout,
        "stderr":output.stderr,"duration_ms":output.duration_ms});
    if let Some(path) = &output.stdout_path { result["stdout_path"] = json!(path); }
    if let Some(path) = &output.stderr_path { result["stderr_path"] = json!(path); }
    result
}

/// The error for a foreground command that continues as `job`.
fn detached(job: &JobStatus, cancelled: bool, timeout_s: u64, changes: &Changes, session_note: &str) -> io::Error {
    let id = &job.record.job_id;
    if cancelled {
        return io::Error::new(io::ErrorKind::Interrupted,
            format!("Turn cancelled. The command was not stopped; it continues as background job {id}."));
    }
    let mut detail = json!({"error_code":"command_timeout","retryable":false,
        "summary":format!("The command was still running after {timeout_s} s. It was not stopped: it continues as background job {id}. Check it with shell_job (action status or output), and stop it with action stop if it is stuck. {session_note}"),
        "timed_out":true,"job_id":id,"state":job.state.as_str(),"log_path":job.record.log_path});
    if let Some(path) = &job.record.stderr_path { detail["stderr_path"] = json!(path); }
    changes.add_to(&mut detail);
    io::Error::new(io::ErrorKind::TimedOut, detail.to_string())
}

/// `policy_denied` for a command the sandbox refused `denied` paths. `result` is the
/// command's result (exit code, output, `files_changed`), kept in the error. It offers
/// `retry_with: {"escalate": true}` only when every denied path is in the user's granted
/// scope and the command was not already escalated.
fn sandbox_denied(mut result: Value, denied: &[PathBuf], escalation: &Escalation, escalated: bool) -> io::Error {
    let roots = escalation.roots();
    let grantable = !escalated && denied.iter()
        .all(|path| { let path = resolve_path(path); roots.iter().any(|root| path.starts_with(root)) });
    let paths = display(denied).join(", ");
    let granted = if roots.is_empty() { "nothing".to_owned() } else { display(&roots).join(", ") };
    result["error_code"] = json!("policy_denied");
    result["retryable"] = json!(false);
    result["denied_paths"] = json!(display(denied));
    result["summary"] = json!(if grantable {
        result["retry_with"] = json!({"escalate": true});
        format!("Access to {paths} was denied. For named host input files, use host_import_files to request read access. For a write under {granted}, request this exact command once with escalate: true and a reason.")
    } else if escalated {
        format!("Access to {paths} was denied with the granted write roots ({granted}). For named inputs use host_import_files. If the host OS denies that read, ask the user to grant file access or attach the file.")
    } else {
        format!("Access to {paths} was denied. Shell write escalation covers {granted}. Use host_import_files for named inputs or request an accessible destination for writes.")
    });
    if denied.iter().any(|path| path.starts_with("/tmp") || path.starts_with("/private/tmp")) {
        let prior = result["summary"].as_str().unwrap_or("");
        result["summary"] = json!(format!("For temporary scripts or output, use the task's writable $TMPDIR or workspace. Check any partial output before retrying. {prior}"));
    }
    io::Error::new(io::ErrorKind::PermissionDenied, result.to_string())
}

/// `approval_denied` for an escalated command the user (or the chat's automatic reviewer)
/// did not approve. Not retryable. `report_blocker` accepts the approval id, or this tool
/// call's id, as a failed attempt.
fn approval_denied(approval: &ui_approvals::Approval, command: &str) -> io::Error {
    let mut detail = json!({"error_code":"approval_denied","retryable":false,"approval_id":approval.id,
        "state":approval.state,"command":command,
        "summary":"The user did not approve running this command outside the sandbox. Don't retry it or work around the decision. Continue with a method that stays inside the task workspace, or call report_blocker and list this approval_id (or this tool call's id) in references."});
    if let Some(rationale) = &approval.review_denial {
        detail["rationale"] = json!(rationale);
        detail["summary"] = json!(format!("The automatic approval reviewer denied running this command outside the sandbox: {rationale}. Don't retry it or work around the decision. Continue with a method that stays inside the task workspace, or call report_blocker and list this approval_id (or this tool call's id) in references."));
    }
    io::Error::new(io::ErrorKind::PermissionDenied, detail.to_string())
}

fn error_kind_name(kind: io::ErrorKind) -> &'static str {
    match kind {
        io::ErrorKind::PermissionDenied => "permission_denied",
        io::ErrorKind::TimedOut => "timed_out",
        io::ErrorKind::Interrupted => "interrupted",
        io::ErrorKind::InvalidInput => "invalid_input",
        io::ErrorKind::NotFound => "not_found",
        io::ErrorKind::Unsupported => "unsupported",
        _ => "other",
    }
}

fn error_kind(name: &str) -> io::ErrorKind {
    match name {
        "permission_denied" => io::ErrorKind::PermissionDenied,
        "timed_out" => io::ErrorKind::TimedOut,
        "interrupted" => io::ErrorKind::Interrupted,
        "invalid_input" => io::ErrorKind::InvalidInput,
        "not_found" => io::ErrorKind::NotFound,
        "unsupported" => io::ErrorKind::Unsupported,
        _ => io::ErrorKind::Other,
    }
}

/// `shell` with `escalate: true`: Approval_Policy check, approval, then one run in a new
/// process with the granted write roots (see the module documentation).
#[allow(clippy::too_many_arguments)]
fn escalated(backend: &dyn ShellBackend, escalation: &Escalation, db: &Database, scope: &ShellScope<'_>, turn: &str,
    call_id: &str, args: &Value, command: &str, reason: &str, timeout_s: u64, workspace: &TaskWorkspace,
    cancel: &AtomicBool) -> io::Result<String> {
    use kageha_policy::classes::{ClassifyContext, Decision, Scope};
    let chat = scope.chat;
    let context = ClassifyContext { outside_workspace: true, ..Default::default() };
    let (class, command_scope, decision) = {
        let connection = db.lock().map_err(|_| io::Error::other("database unavailable"))?;
        ui_approvals::action_class(&connection, chat, "shell", args, &context, escalation.allowlist.as_deref())?
    };
    let cwd = backend.current_dir(chat, workspace.root());
    let roots = escalation.roots();
    let prefix = match &command_scope { Scope::Command(prefix) => prefix.as_str(), _ => "" };
    // Bound into the approval's input digest: a different command, directory, reason or
    // set of roots needs its own approval.
    let request = json!({"command":command,"cwd":cwd,"reason":reason,"write_roots":display(&roots),
        "class":class.as_str(),"resource":format!("{}{prefix}", ui_approvals::COMMAND_RESOURCE)});
    let allowlisted = decision == Decision::Allow;
    let approval_id = if allowlisted { None } else {
        let granted = if roots.is_empty() { "no directories outside the task workspace".to_owned() } else { display(&roots).join(", ") };
        let detail = format!("Run this command once outside the sandbox, with write access to {granted}:\n\n{command}\n\nWorking directory: {}\nReason: {reason}", cwd.display());
        let approval = ui_approvals::require_recorded(db, chat, turn, "shell", &detail, &request, cancel, None)?;
        if !approval.allowed {
            journal(db, chat, turn, json!({"call_id":call_id,"command":command,"reason":reason,"escalated":true,
                "approval_id":approval.id,"approved":false}));
            return Err(approval_denied(&approval, command));
        }
        approval.id
    };
    let call = json!({"id":call_id,"name":"shell","arguments":args});
    // The effect record keeps one tool call from running the command twice, for example
    // after a restart; a replay returns the recorded outcome.
    let outcome = crate::ui_effects::run(db, turn, &call, &request, || {
        let ran = (|| -> io::Result<String> {
            let ran = backend.run_once(scope, &cwd, roots.clone(), command, call_id, Duration::from_secs(timeout_s), cancel)
                .map_err(shell_failure)?;
            let changes = register_changes(db, chat, turn, workspace)?;
            let base = json!({"call_id":call_id,"command":command,"reason":reason,"background":false,"escalated":true,
                "approval_id":approval_id,"allowlisted":allowlisted,"cwd":cwd,"write_roots":display(&roots),
                "files_changed":changes.paths});
            match ran {
                Foreground::Finished(output) => {
                    let mut event = base;
                    event["exit_code"] = json!(output.exit_code);
                    event["duration_ms"] = json!(output.duration_ms);
                    journal(db, chat, turn, event);
                    let mut result = output_result(&output);
                    changes.add_to(&mut result);
                    result["escalated"] = json!(true);
                    result["approval_id"] = json!(approval_id);
                    result["allowlisted"] = json!(allowlisted);
                    result["write_roots"] = json!(display(&roots));
                    if output.exit_code != 0 {
                        let all_roots = [backend.write_roots(scope), roots.clone()].concat();
                        let denied = sandbox_denials(&output.stderr, &all_roots, &[]);
                        if !denied.is_empty() {
                            return Err(sandbox_denied(result, &denied, escalation, true));
                        }
                    }
                    Ok(result.to_string())
                }
                Foreground::Detached { job, cancelled } => {
                    record_job(db, chat, &job, backend.machine())?;
                    let mut event = base;
                    event["timed_out"] = json!(!cancelled);
                    event["cancelled"] = json!(cancelled);
                    event["job_id"] = json!(job.record.job_id);
                    journal(db, chat, turn, event);
                    Err(detached(&job, cancelled, timeout_s, &changes, "The chat's shell session was not affected."))
                }
            }
        })();
        Ok(match ran {
            Ok(body) => json!({"kind":"ok","body":body}),
            Err(error) => json!({"kind":error_kind_name(error.kind()),"body":error.to_string()}),
        })
    })?;
    let body = outcome["body"].as_str().unwrap_or_default().to_owned();
    match outcome["kind"].as_str() {
        Some("ok") => Ok(body),
        kind => Err(io::Error::new(error_kind(kind.unwrap_or("other")), body)),
    }
}

/// `shell_job(job_id, action, offset)` with action `status`, `output` or `stop`.
pub(crate) fn shell_job(backend: &dyn ShellBackend, db: &Database, chat: &str, turn: &str, args: &Value) -> io::Result<String> {
    let job_id = args["job_id"].as_str().filter(|id| !id.is_empty() && id.len() <= 64)
        .ok_or_else(|| invalid("job_id must be a job id returned by shell"))?;
    let action = args["action"].as_str().filter(|action| matches!(*action, "status" | "output" | "stop"))
        .ok_or_else(|| invalid("action must be status, output or stop"))?;
    let offset = match &args["offset"] {
        Value::Null => 0,
        value => value.as_u64().ok_or_else(|| invalid("offset must be a non-negative integer"))?,
    };
    let workspace = ui_workspace::open_for_chat(db, chat)?;
    let attempt = |backend: &dyn ShellBackend| -> io::Result<Result<JobStatus, JobOutput>> {
        match action {
            "status" => backend.job_status(chat, job_id).map(Ok),
            "stop" => backend.stop_job(chat, job_id).map(Ok),
            _ => backend.job_output(chat, job_id, offset, OUTPUT_WINDOW).map(Err),
        }
    };
    let reply = match attempt(backend) {
        // Not known to this process (it restarted): adopt the job from its stored record.
        Err(error) if error.kind() == io::ErrorKind::NotFound => match stored_job(db, chat, job_id, &workspace, backend.machine())? {
            Some(record) => {
                backend.adopt_job(&ShellScope::of(db, chat, &workspace)?, record).map_err(shell_failure)?;
                attempt(backend)
            }
            None => Err(error),
        },
        other => other,
    }.map_err(shell_failure)?;
    let command = |db: &Database| -> io::Result<String> {
        db.lock().map_err(|_| io::Error::other("database unavailable"))?
            .query_row("SELECT command FROM ui_shell_jobs WHERE job_id=? AND chat_id=?", params![job_id, chat], |row| row.get(0))
            .optional().map_err(io::Error::other).map(Option::unwrap_or_default)
    };
    match reply {
        Ok(status) => {
            let changes = observe(db, chat, turn, &workspace, job_id, &status.record.command, status.state, status.exit_code)?;
            let mut result = json!({"job_id":job_id,"state":status.state.as_str(),"exit_code":status.exit_code,
                "pid":status.record.pid,"command":status.record.command,"log_path":status.record.log_path});
            if let Some(path) = &status.record.stderr_path { result["stderr_path"] = json!(path); }
            if let Some(changes) = changes { changes.add_to(&mut result); }
            Ok(result.to_string())
        }
        Err(output) => {
            let changes = observe(db, chat, turn, &workspace, job_id, &command(db)?, output.state, output.exit_code)?;
            let mut result = json!({"job_id":job_id,"state":output.state.as_str(),"exit_code":output.exit_code,
                "offset":output.offset,"next_offset":output.next_offset,"size":output.size,
                "complete":output.complete,"output":output.output});
            if let Some(path) = &output.stderr_path { result["stderr_path"] = json!(path); }
            if let Some(changes) = changes { changes.add_to(&mut result); }
            Ok(result.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ui_task_completion::proof, ui_uploads};
    use kageha_workflow::completion::OutcomeKind;
    use kageha_workspace::task::DISK_LOW_BYTES;
    use std::time::Instant;

    struct Fixture {
        directory: tempfile::TempDir,
        db: Database,
        backend: ShellManager,
        escalation: Escalation,
    }

    fn fixture() -> Fixture {
        let directory = tempfile::tempdir().unwrap();
        let connection = rusqlite::Connection::open(directory.path().join("native.sqlite")).unwrap();
        ui_uploads::initialize(&connection).unwrap();
        crate::native_media::initialize(&connection).unwrap();
        ui_workspace::initialize(&connection).unwrap();
        initialize(&connection).unwrap();
        crate::ui_approvals::initialize(&connection).unwrap();
        connection.execute_batch(
            "CREATE TABLE ui_sessions(id TEXT PRIMARY KEY);INSERT INTO ui_sessions VALUES ('chat');
             CREATE TABLE ui_events(sequence INTEGER PRIMARY KEY AUTOINCREMENT,chat_id TEXT,turn_id TEXT,event TEXT,payload TEXT);
             CREATE TABLE ui_jobs(id TEXT,parent_chat TEXT,child_chat TEXT,created INTEGER);",
        ).unwrap();
        let backend = ShellManager::new(directory.path().join("toolchains"));
        // The user's granted scope is one directory outside the workspace.
        std::fs::create_dir_all(directory.path().join("granted")).unwrap();
        let escalation = Escalation {
            allowlist: Some(directory.path().join("approvals_allowlist.json")),
            granted_roots: vec![directory.path().join("granted"), PathBuf::from("/")],
            hidden_roots: Vec::new(),
        };
        Fixture { db: crate::agent_host::DatabaseState::open(connection).unwrap(), backend, escalation, directory }
    }

    impl Fixture {
        fn shell(&self, args: Value) -> io::Result<Value> {
            self.shell_as("call_1", args)
        }
        fn shell_as(&self, call_id: &str, args: Value) -> io::Result<Value> {
            shell(&self.backend, &self.escalation, &self.db, "chat", "turn", call_id, &args, &AtomicBool::new(false))
                .map(|text| serde_json::from_str(&text).unwrap())
        }
        fn granted(&self, name: &str) -> PathBuf {
            self.directory.path().join("granted").join(name)
        }
        fn journal(&self) -> std::sync::Arc<kageha_policy::journal::Journal> {
            crate::ui_approvals::owned(&self.db.lock().unwrap()).unwrap()
        }
        /// Waits for the chat's pending approval, checks it, and decides it.
        fn decide(&self, approved: bool) -> kageha_contracts::policy::ApprovalRecord {
            use kageha_contracts::policy::{ApprovalDecision, GrantScope};
            let journal = self.journal();
            let mut pending = Vec::new();
            assert!(wait_for(Duration::from_secs(20), || { pending = journal.pending_family("chat").unwrap(); !pending.is_empty() }));
            let record = pending.pop().unwrap();
            journal.decide(&record.id, &ApprovalDecision { expected_revision: record.revision,
                authority: record.request.authority.clone(), approved, scope: GrantScope::Once }).unwrap();
            record
        }
        fn job(&self, args: Value) -> Value {
            serde_json::from_str(&shell_job(&self.backend, &self.db, "chat", "turn", &args).unwrap()).unwrap()
        }
        fn workspace(&self) -> TaskWorkspace {
            ui_workspace::open_for_chat(&self.db, "chat").unwrap()
        }
        fn events(&self) -> Vec<Value> {
            let db = self.db.lock().unwrap();
            let mut statement = db.prepare("SELECT payload FROM ui_events WHERE event='event' ORDER BY sequence").unwrap();
            let rows = statement.query_map([], |row| row.get::<_, String>(0)).unwrap();
            rows.map(|row| serde_json::from_str(&row.unwrap()).unwrap()).collect()
        }
        fn row(&self, job_id: &str) -> (String, String, i64, i64, String, Option<i64>, String) {
            self.db.lock().unwrap().query_row(
                "SELECT chat_id,command,pid,started_ns,state,exit_code,log_path FROM ui_shell_jobs WHERE job_id=?",
                [job_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
            ).unwrap()
        }
    }

    fn error(error: io::Error) -> Value {
        serde_json::from_str(&error.to_string()).unwrap()
    }

    fn wait_for(within: Duration, mut done: impl FnMut() -> bool) -> bool {
        let end = Instant::now() + within;
        while Instant::now() < end {
            if done() { return true; }
            std::thread::sleep(Duration::from_millis(50));
        }
        done()
    }

    #[test]
    fn call_ids_become_safe_file_name_parts() {
        assert_eq!(file_call_id("call_1"), "call_1");
        assert_eq!(file_call_id("toolu/../x y"), "toolu_.._x_y");
        assert_eq!(file_call_id(""), "c");
        assert_eq!(file_call_id(".hidden"), "c.hidden");
        assert_eq!(file_call_id(&"a".repeat(300)).len(), kageha_workspace::shell::MAX_CALL_ID);
    }

    #[test]
    fn invalid_arguments_escalation_and_low_disk_are_refused_before_running() {
        let fixture = fixture();
        for args in [json!({}), json!({"command":"  "}), json!({"command":"ls","timeout_s":0}),
            json!({"command":"ls","timeout_s":3601}), json!({"command":"ls","background":"yes"})] {
            assert_eq!(error(fixture.shell(args.clone()).unwrap_err())["error_code"], "invalid_arguments", "{args}");
        }
        // Escalation needs a reason and can't run in the background.
        for args in [json!({"command":"touch /etc/x","escalate":true}), json!({"command":"touch /etc/x","escalate":true,"reason":"  "}),
            json!({"command":"touch /etc/x","escalate":true,"reason":"x","background":true})] {
            assert_eq!(error(fixture.shell(args.clone()).unwrap_err())["error_code"], "invalid_arguments", "{args}");
        }
        assert!(fixture.journal().pending_family("chat").unwrap().is_empty());
        if !available() { return; }
        let workspace = fixture.workspace();
        crate::ui_tools::test_free_disk::set(workspace.root(), DISK_LOW_BYTES - 1);
        let low = error(fixture.shell(json!({"command":"echo x > outputs/x.txt"})).unwrap_err());
        assert_eq!(low["error_code"], "disk_low");
        assert!(!workspace.root().join("outputs/x.txt").exists());
        assert!(fixture.events().iter().any(|event| event["kind"] == "disk_low"));
    }

    #[test]
    fn shell_runs_in_the_workspace_keeps_its_session_and_registers_files() {
        if !available() { eprintln!("skipping: OS process sandbox unavailable"); return; }
        let fixture = fixture();
        let workspace = fixture.workspace();
        let pwd = fixture.shell(json!({"command":"pwd -P; echo oops >&2; exit_with() { return 4; }; exit_with"})).unwrap();
        assert_eq!(pwd["exit_code"], 4);
        assert_eq!(pwd["stdout"], format!("{}\n", workspace.root().display()));
        assert_eq!(pwd["stderr"], "oops\n");
        assert!(pwd["duration_ms"].is_u64());
        assert_eq!(pwd["files_changed"], json!([]));
        // The session keeps its directory and variables between calls.
        fixture.shell(json!({"command":"cd outputs && export KG_NAME=report"})).unwrap();
        let created = fixture.shell(json!({"command":"echo \"# $KG_NAME\" > \"$KG_NAME.md\"; pwd -P"})).unwrap();
        assert_eq!(created["exit_code"], 0);
        assert_eq!(created["stdout"], format!("{}\n", workspace.root().join("outputs").display()));
        let files = created["files_changed"].as_array().unwrap();
        assert_eq!(files.len(), 1, "{created}");
        assert_eq!(files[0]["artifact_path"], "workspace/outputs/report.md");
        assert_eq!(files[0]["size"], 9);
        assert_eq!(files[0]["sha256"].as_str().unwrap().len(), 64);
        assert!(proof(&fixture.db, "chat", "turn", &OutcomeKind::Artifact, "workspace/outputs/report.md").unwrap());
        // Unchanged files are not reported again; journal events carry command, exit and files.
        assert_eq!(fixture.shell(json!({"command":"true"})).unwrap()["files_changed"], json!([]));
        let journal: Vec<_> = fixture.events().into_iter().filter(|event| event["kind"] == "shell_command").collect();
        assert_eq!(journal.len(), 4);
        assert_eq!(journal[2]["payload"]["exit_code"], 0);
        assert_eq!(journal[2]["payload"]["files_changed"], json!(["workspace/outputs/report.md"]));
        assert!(journal[2]["payload"]["command"].as_str().unwrap().contains("KG_NAME"));
        // Large output is spilled and named in the result.
        let big = fixture.shell(json!({"command":"printf '%40000s' '' | tr ' ' y"})).unwrap();
        assert_eq!(big["stdout_path"], ".kageha/tool-output/call_1-stdout.txt");
        assert!(big.get("stderr_path").is_none());
    }

    #[test]
    fn background_jobs_report_status_output_and_stop() {
        if !available() { eprintln!("skipping: OS process sandbox unavailable"); return; }
        let fixture = fixture();
        let started = fixture.shell(json!({"command":"echo started; sleep 30","background":true})).unwrap();
        assert_eq!(started["state"], "running");
        let id = started["job_id"].as_str().unwrap().to_owned();
        let (chat, command, pid, started_ns, state, exit_code, log_path) = fixture.row(&id);
        assert_eq!((chat.as_str(), command.as_str(), state.as_str(), exit_code), ("chat", "echo started; sleep 30", "running", None));
        assert!(pid > 1 && started_ns > 0);
        assert_eq!(log_path, format!(".kageha/jobs/{id}.log"));
        assert!(wait_for(Duration::from_secs(10), || fixture.job(json!({"job_id":id,"action":"output"}))["output"] == "started\n"));
        let output = fixture.job(json!({"job_id":id,"action":"output","offset":0}));
        assert_eq!((output["next_offset"].clone(), output["complete"].clone()), (json!(8), json!(false)));
        assert_eq!(fixture.job(json!({"job_id":id,"action":"status"}))["state"], "running");
        let stopped = fixture.job(json!({"job_id":id,"action":"stop"}));
        assert_eq!((stopped["state"].clone(), stopped["exit_code"].clone()), (json!("killed"), Value::Null));
        assert_eq!(fixture.row(&id).4, "killed");
        // A job that writes a file and exits reports its code and registers the file once.
        let job = fixture.shell(json!({"command":"echo done > outputs/bg.txt; exit 2","background":true})).unwrap();
        let id = job["job_id"].as_str().unwrap().to_owned();
        let mut ended = Value::Null;
        assert!(wait_for(Duration::from_secs(10), || { ended = fixture.job(json!({"job_id":id,"action":"status"})); ended["state"] != "running" }));
        assert_eq!((ended["state"].clone(), ended["exit_code"].clone()), (json!("exited"), json!(2)));
        assert_eq!(ended["files_changed"][0]["artifact_path"], "workspace/outputs/bg.txt");
        assert!(proof(&fixture.db, "chat", "turn", &OutcomeKind::Artifact, "workspace/outputs/bg.txt").unwrap());
        assert_eq!(fixture.row(&id).5, Some(2));
        assert!(fixture.job(json!({"job_id":id,"action":"status"})).get("files_changed").is_none());
        // Unknown jobs and other chats' jobs are not found.
        let missing = shell_job(&fixture.backend, &fixture.db, "chat", "turn", &json!({"job_id":"job-0000000000000000","action":"status"})).unwrap_err();
        assert_eq!(error(missing)["error_code"], "job_not_found");
        let bad = shell_job(&fixture.backend, &fixture.db, "chat", "turn", &json!({"job_id":id,"action":"kill"})).unwrap_err();
        assert_eq!(error(bad)["error_code"], "invalid_arguments");
    }

    #[test]
    fn a_foreground_timeout_returns_the_job_and_the_command_continues() {
        if !available() { eprintln!("skipping: OS process sandbox unavailable"); return; }
        let fixture = fixture();
        let timed_out = error(fixture.shell(json!({"command":"sleep 2; echo late > outputs/late.txt; echo finished","timeout_s":1})).unwrap_err());
        assert_eq!(timed_out["error_code"], "command_timeout");
        assert_eq!((timed_out["timed_out"].clone(), timed_out["state"].clone()), (json!(true), json!("running")));
        let id = timed_out["job_id"].as_str().unwrap().to_owned();
        assert_eq!(fixture.row(&id).4, "running");
        // The chat gets a new session at once.
        assert_eq!(fixture.shell(json!({"command":"echo next"})).unwrap()["stdout"], "next\n");
        let mut ended = Value::Null;
        assert!(wait_for(Duration::from_secs(15), || { ended = fixture.job(json!({"job_id":id,"action":"output"})); ended["complete"] == true }));
        assert_eq!((ended["state"].clone(), ended["exit_code"].clone()), (json!("exited"), json!(0)));
        assert_eq!(ended["output"], "finished\n");
        assert_eq!(ended["files_changed"][0]["artifact_path"], "workspace/outputs/late.txt");
        assert_eq!(fixture.row(&id).5, Some(0));
        let journal: Vec<_> = fixture.events().into_iter().filter(|event| event["kind"] == "shell_command").collect();
        assert!(journal.iter().any(|event| event["payload"]["timed_out"] == true && event["payload"]["job_id"] == id.as_str()));
        assert!(journal.iter().any(|event| event["payload"]["completed"] == true && event["payload"]["exit_code"] == 0));
    }

    #[test]
    fn jobs_from_an_earlier_process_are_adopted_from_their_row() {
        if !available() { eprintln!("skipping: OS process sandbox unavailable"); return; }
        let fixture = fixture();
        let started = fixture.shell(json!({"command":"sleep 30","background":true})).unwrap();
        let id = started["job_id"].as_str().unwrap().to_owned();
        // A restarted host has a new backend that does not know the job.
        let restarted = ShellManager::new(fixture.directory.path().join("toolchains"));
        let status: Value = serde_json::from_str(&shell_job(&restarted, &fixture.db, "chat", "turn", &json!({"job_id":id,"action":"status"})).unwrap()).unwrap();
        assert_eq!(status["state"], "running");
        let stopped: Value = serde_json::from_str(&shell_job(&restarted, &fixture.db, "chat", "turn", &json!({"job_id":id,"action":"stop"})).unwrap()).unwrap();
        assert_eq!(stopped["state"], "killed");
        assert_eq!(fixture.row(&id).4, "killed");
    }

    #[test]
    fn startup_readoption_takes_over_running_jobs_by_pid_and_start_time() {
        if !available() { eprintln!("skipping: OS process sandbox unavailable"); return; }
        let fixture = fixture();
        let live = fixture.shell(json!({"command":"echo up; sleep 30","background":true})).unwrap();
        let live = live["job_id"].as_str().unwrap().to_owned();
        let reused = fixture.shell(json!({"command":"sleep 30","background":true})).unwrap();
        let reused = reused["job_id"].as_str().unwrap().to_owned();
        // The second row's PID now belongs to a different process start (a reused PID).
        let (_, _, _, started_ns, ..) = fixture.row(&reused);
        fixture.db.lock().unwrap().execute("UPDATE ui_shell_jobs SET started_ns=? WHERE job_id=?",
            params![started_ns + 1, reused]).unwrap();
        // An ended row is not touched.
        fixture.db.lock().unwrap().execute(
            "INSERT INTO ui_shell_jobs(job_id,chat_id,command,pid,started_ns,state,exit_code,log_path)
             VALUES ('job-00000000000000aa','chat','true',1,1,'exited',0,'.kageha/jobs/job-00000000000000aa.log')", []).unwrap();
        // The restarted Host's backend knows no jobs until the startup pass adopts them.
        let restarted = ShellManager::new(fixture.directory.path().join("toolchains"));
        let mut adopted = readopt_jobs_with(&restarted, &fixture.db).unwrap();
        adopted.sort_by(|a, b| a.job_id.cmp(&b.job_id));
        let mut expected = vec![
            Readopted { chat: "chat".into(), job_id: live.clone(), state: JobState::Running },
            Readopted { chat: "chat".into(), job_id: reused.clone(), state: JobState::Killed },
        ];
        expected.sort_by(|a, b| a.job_id.cmp(&b.job_id));
        assert_eq!(adopted, expected);
        assert_eq!(restarted.jobs_for("chat").len(), 2);
        // The adopted job is the live process: its output and stop go through the new backend.
        assert!(wait_for(Duration::from_secs(10), || restarted.job_output("chat", &live, 0, 64).unwrap().output == "up\n"));
        let stopped: Value = serde_json::from_str(&shell_job(&restarted, &fixture.db, "chat", "turn", &json!({"job_id":live,"action":"stop"})).unwrap()).unwrap();
        assert_eq!(stopped["state"], "killed");
        assert!(wait_for(Duration::from_secs(5), || fixture.backend.job_status("chat", &live).unwrap().state == JobState::Killed));
        // The process behind the mismatched row was never signalled by the new backend.
        assert_eq!(fixture.backend.job_status("chat", &reused).unwrap().state, JobState::Running);
        fixture.backend.stop_job("chat", &reused).unwrap();
    }

    #[test]
    fn sandbox_denials_fail_with_policy_denied_and_keep_the_output() {
        if !available() { eprintln!("skipping: OS process sandbox unavailable"); return; }
        let fixture = fixture();
        let target = fixture.granted("denied.txt");
        let denied = error(fixture.shell(json!({"command":format!("echo before; printf x > {}", target.display())})).unwrap_err());
        assert_eq!(denied["error_code"], "policy_denied");
        assert_eq!(denied["retry_with"], json!({"escalate": true}));
        assert_eq!(denied["denied_paths"], json!([target.display().to_string()]), "{denied}");
        assert_eq!(denied["stdout"], "before\n");
        assert_ne!(denied["exit_code"], 0);
        assert!(denied["stderr"].as_str().unwrap().contains(&target.display().to_string()));
        assert!(denied["summary"].as_str().unwrap().contains("escalate: true"));
        assert!(!target.exists());
        // Outside the granted scope, escalating would not help: no retry_with.
        let elsewhere = tempfile::tempdir().unwrap();
        let outside = elsewhere.path().join("x.txt");
        let refused = error(fixture.shell(json!({"command":format!("touch {}", outside.display())})).unwrap_err());
        assert_eq!(refused["error_code"], "policy_denied");
        assert!(refused.get("retry_with").is_none(), "{refused}");
        assert_eq!(refused["denied_paths"], json!([outside.display().to_string()]));
        let scratch = error(fixture.shell(json!({"command":"touch /tmp/kageha-test-scratch-denied"})).unwrap_err());
        assert!(scratch["summary"].as_str().unwrap().contains("$TMPDIR"));
        assert_eq!(fixture.shell(json!({"command":r#"touch "$TMPDIR/ok.py""#})).unwrap()["exit_code"], 0);
        // An ordinary failure is a normal result.
        assert_eq!(fixture.shell(json!({"command":"ls missing-file"})).unwrap()["exit_code"], if cfg!(target_os = "macos") { 1 } else { 2 });
    }

    #[test]
    fn escalation_with_an_allowlisted_prefix_runs_without_approval() {
        if !available() { eprintln!("skipping: OS process sandbox unavailable"); return; }
        use kageha_policy::classes::{add_allow, ActionClass, AllowEntry, Scope};
        let fixture = fixture();
        add_allow(fixture.escalation.allowlist.as_ref().unwrap(),
            AllowEntry { class: ActionClass::ExternalWrite, scope: Scope::Command("touch".into()) }, false).unwrap();
        let target = fixture.granted("allowed.txt");
        let ran = fixture.shell(json!({"command":format!("touch {}", target.display()),"escalate":true,"reason":"Create the marker the user asked for"})).unwrap();
        assert_eq!((ran["exit_code"].clone(), ran["escalated"].clone(), ran["allowlisted"].clone(), ran["approval_id"].clone()),
            (json!(0), json!(true), json!(true), Value::Null));
        assert!(target.exists());
        assert!(fixture.journal().list(Some("chat"), None, false, 0, 10).unwrap().is_empty());
        assert!(!fixture.events().iter().any(|event| event["kind"] == "approval_required"));
        // A command the entry doesn't cover still asks (and is denied here).
        let other = fixture.granted("other.txt");
        std::thread::scope(|scope| {
            let call = scope.spawn(|| fixture.shell_as("call_2", json!({"command":format!("cp /dev/null {}", other.display()),"escalate":true,"reason":"r"})));
            fixture.decide(false);
            assert_eq!(error(call.join().unwrap().unwrap_err())["error_code"], "approval_denied");
        });
        assert!(!other.exists());
    }

    #[test]
    fn approved_escalation_runs_the_exact_command_once_with_the_granted_roots() {
        if !available() { eprintln!("skipping: OS process sandbox unavailable"); return; }
        let fixture = fixture();
        let workspace = fixture.workspace();
        std::fs::create_dir_all(workspace.root().join("outputs/sub")).unwrap();
        fixture.shell(json!({"command":"cd outputs/sub && export KG_STATE=session"})).unwrap();
        let target = fixture.granted("approved.txt");
        let command = format!("printf ok > {}; pwd -P; echo \"[$KG_STATE]\"; export KG_STATE=once; cd /", target.display());
        let reason = "Save the report where the user asked";
        let (result, record) = std::thread::scope(|scope| {
            let call = scope.spawn(|| fixture.shell(json!({"command":command,"escalate":true,"reason":reason})));
            let record = fixture.decide(true);
            (call.join().unwrap(), record)
        });
        // The prompt showed the exact command, the session's directory, the reason and the roots.
        let detail = &record.request.detail;
        let cwd = workspace.root().join("outputs/sub");
        assert!(detail.contains(&command), "{detail}");
        assert!(detail.contains(&format!("Working directory: {}", cwd.display())), "{detail}");
        assert!(detail.contains(reason));
        assert!(detail.contains(&fixture.granted("").canonicalize().unwrap().display().to_string()), "{detail}");
        assert_eq!(record.request.action, "shell");
        assert!(fixture.events().iter().any(|event| event["kind"] == "approval_required" && event["payload"]["approval_id"] == record.id.as_str()));
        let result = result.unwrap();
        assert_eq!(result["exit_code"], 0, "{result}");
        assert_eq!(result["stdout"], format!("{}\n[]\n", cwd.display()));
        assert_eq!((result["escalated"].clone(), result["approval_id"].clone()), (json!(true), json!(record.id)));
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "ok");
        // `/` is never granted.
        assert_eq!(result["write_roots"], json!([fixture.granted("").canonicalize().unwrap().display().to_string()]));
        // The one-off process left the chat's session alone.
        let after = fixture.shell(json!({"command":"pwd -P; echo \"[$KG_STATE]\""})).unwrap();
        assert_eq!(after["stdout"], format!("{}\n[session]\n", cwd.display()));
        // The same tool call is never run twice: even approved again, its recorded outcome
        // comes back and the command doesn't run.
        std::fs::remove_file(&target).unwrap();
        let replay = std::thread::scope(|scope| {
            let call = scope.spawn(|| fixture.shell(json!({"command":command,"escalate":true,"reason":reason})));
            fixture.decide(true);
            call.join().unwrap().unwrap()
        });
        assert_eq!(replay, result);
        assert!(!target.exists());
        let journal: Vec<_> = fixture.events().into_iter().filter(|event| event["kind"] == "shell_command" && event["payload"]["escalated"] == true).collect();
        assert_eq!(journal.len(), 1);
        assert_eq!(journal[0]["payload"]["approval_id"], json!(record.id));
    }

    #[test]
    fn denied_escalation_returns_approval_denied_with_the_approval_id() {
        if !available() { eprintln!("skipping: OS process sandbox unavailable"); return; }
        use kageha_contracts::policy::ApprovalState;
        let fixture = fixture();
        let target = fixture.granted("never.txt");
        let (result, record) = std::thread::scope(|scope| {
            let call = scope.spawn(|| fixture.shell(json!({"command":format!("touch {}", target.display()),"escalate":true,"reason":"r"})));
            let record = fixture.decide(false);
            (call.join().unwrap(), record)
        });
        let denied = error(result.unwrap_err());
        assert_eq!((denied["error_code"].clone(), denied["retryable"].clone()), (json!("approval_denied"), json!(false)));
        assert_eq!(denied["approval_id"], json!(record.id));
        assert!(!target.exists());
        // report_blocker accepts a denied approval of this chat by id.
        let stored = fixture.journal().get(&record.id).unwrap().unwrap();
        assert_eq!((stored.state, stored.request.authority.session.as_str()), (ApprovalState::Denied, "chat"));
    }
}
