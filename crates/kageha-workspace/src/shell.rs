//! Shell service: one persistent, sandboxed `zsh` (macOS) or `bash` (Linux) session per chat.
//!
//! Each session runs under [`kageha_sandbox::ProcessSandbox`]. If the OS sandbox is
//! unavailable, opening a session fails; there is no unconfined fallback.
//!
//! Confinement:
//!
//! - write roots: the Task_Workspace, the Toolchain_Cache (`~/.kageha/toolchains`, mode 0700)
//!   and the per-chat temp directory;
//! - read roots: the write roots, the system directories a working shell needs
//!   ([`SYSTEM_READ_ROOTS`], on top of those [`ProcessSandbox`] always allows) and any paths
//!   the user granted;
//! - network: on.
//!
//! Per-chat directories live inside the workspace's host-owned state directory, so they are
//! inside the write roots and are skipped by `TaskWorkspace::scan_changes`:
//!
//! ```text
//! workspace/.kageha/home   HOME (dotfiles, shell history, per-chat tool config)
//! workspace/.kageha/tmp    TMPDIR
//! ```
//!
//! A workspace can keep its state directory outside the workspace ([`SessionRoots`],
//! `TaskWorkspace::open_with_state`): a delegated worker's Git worktree, so the checkout gets
//! no `.kageha/`. The state directory is then a write root of its own and holds what the
//! `.kageha/` paths above and below name. A delegated worker's session also reads its parent
//! workspace ([`ShellPaths::extra_read_roots`]) without the parent's `.kageha/` and other
//! workers' workspaces ([`ShellPaths::hidden_roots`]).
//!
//! The environment is built from scratch by [`shell_environment`]: only an allowlist is set,
//! and no variable whose name matches `*_TOKEN`, `*_KEY`, `*_SECRET` or
//! `KAGEHA_SERVER_TOKEN` (case-insensitive) is ever present.
//!
//! The service runs as its own process (`kageha-shell-service`) so that shells survive a
//! restart of the main server. It speaks JSON lines over a local Unix socket; see
//! [`ShellService::handle_line`].
//!
//! Command framing: each command is given a job id up front and is written to the shell as
//!
//! ```text
//! eval '<command>' < /dev/null > .kageha/jobs/<id>.log 2> .kageha/jobs/<id>.err
//! __kg_rc=$?; printf '%d\n' "$__kg_rc" > .kageha/jobs/<id>.exit
//! printf '\n__KG_END_%s_%d\n' <nonce> "$__kg_rc"         (stdout)
//! printf '\n__KG_END_%s_%d\n' <nonce> "$__kg_rc" >&2     (stderr)
//! ```
//!
//! (the job paths are absolute and quoted). The command runs in the session shell itself,
//! so `cd`, `export` and `source` persist. Its output goes to files rather than to the
//! session's pipes, so a command that outlives its timeout can keep running as a
//! background job without anyone reading its output (see [`jobs`]): on timeout the session
//! is detached and becomes that job, and the chat gets a new session on its next command.
//!
//! Session restore: after each finished foreground command the manager saves the session's
//! working directory and non-secret exported variables to `.kageha/shell-env`, and every new
//! session of the chat (after a timeout, an `exit`, or a restart of the shell service or
//! the server) starts from that saved state. See [`state`].
//!
//! The nonce is 128 bits from `/dev/urandom`, new for every call, and never appears in the
//! command text, so the command cannot print it. A pipe stream ends only at an exact
//! `__KG_END_<nonce>_<exit>` line. The newline printed before it is part of the framing and
//! is removed. Anything the shell itself prints on its pipes (for example a failed
//! redirection) is appended to the matching stream.
//!
//! When the command finishes, the Host reads the job files through `O_NOFOLLOW` directory
//! descriptors and removes them. A stream over [`SPILL_THRESHOLD`] is written in full to
//! `.kageha/tool-output/<call_id>-stdout.txt` / `-stderr.txt` with
//! [`TaskWorkspace::write_host_file`], and the result carries a head and tail excerpt. A
//! stream over [`OUTPUT_LIMIT`] (64 MiB) is not loaded into memory: its job file is left in
//! place and named in the result instead.
use crate::task::{TaskWorkspace, STATE_DIR, WORKSPACE_DIR};
use kageha_sandbox::ProcessSandbox;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, BufRead, BufReader, Read, Write},
    os::{
        fd::AsRawFd,
        unix::{fs::DirBuilderExt, fs::PermissionsExt, net::UnixStream, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio},
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

pub mod cli_shim;
pub mod denials;
pub mod jobs;
pub mod state;
pub use denials::{resolve as resolve_path, sandbox_denials, MAX_DENIED_PATHS};
pub use jobs::{
    process_identity, Foreground, JobLimits, JobOutput, JobRecord, JobState, JobStatus,
    ProcessIdentity, SessionInfo, SessionRoots, ShellManager, JOBS_DIR, JOB_OUTPUT_WINDOW, KILL_GRACE,
    MAX_JOB_RUNTIME,
};

/// Toolchain_Cache location relative to the user's home directory.
pub const TOOLCHAIN_DIR: &str = ".kageha/toolchains";
/// Per-chat `HOME`, relative to the workspace root.
pub const HOME_DIR: &str = ".kageha/home";
/// Per-chat `TMPDIR`, relative to the workspace root.
pub const TEMP_DIR: &str = ".kageha/tmp";
/// System `PATH` entries, after the Toolchain_Cache binaries.
pub const SYSTEM_PATH: &str = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";
/// Read-only system directories added to those `ProcessSandbox` always allows, when present.
pub const SYSTEM_READ_ROOTS: &[&str] = &[
    "/usr/libexec",
    "/usr/sbin",
    "/usr/local",
    "/opt/homebrew",
    "/Library/Developer/CommandLineTools",
    "/private/var/db/timezone",
    "/usr/share/zoneinfo",
    "/etc/alternatives",
];
/// Default and maximum foreground timeouts for the socket protocol.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);
pub const MAX_TIMEOUT: Duration = Duration::from_secs(3600);
/// Per-stream bound on output held in memory for one command. Exceeding it breaks the session.
pub const OUTPUT_LIMIT: usize = 64 * 1024 * 1024;
/// A stream longer than this (in bytes) is spilled to `.kageha/tool-output/`.
pub const SPILL_THRESHOLD: usize = 32 * 1024;
/// Size of each of the head and tail excerpts returned for a spilled stream.
pub const EXCERPT_BYTES: usize = 16 * 1024;
/// Longest accepted `call_id`; it becomes part of a file name.
pub const MAX_CALL_ID: usize = 128;
const REQUEST_LIMIT: usize = 16 * 1024 * 1024;
const DEFAULT_LANG: &str = "en_US.UTF-8";

/// The directories a chat's shell session uses. Fields are public so hosts and tests can
/// build one without touching the filesystem; [`ShellPaths::prepare`] creates them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellPaths {
    /// Task_Workspace root (canonical): the working directory and a write root.
    pub workspace: PathBuf,
    /// Toolchain_Cache (`~/.kageha/toolchains`): shared installs, a write root.
    pub toolchain_cache: PathBuf,
    /// Per-chat `TMPDIR`, a write root.
    pub temp_dir: PathBuf,
    /// Per-chat `HOME`, inside the state directory.
    pub home: PathBuf,
    /// Host state directory, the `.kageha/` of the workspace: `<workspace>/.kageha`, or a
    /// private directory outside the workspace (a delegated worker's Git worktree, see
    /// `TaskWorkspace::open_with_state`). Job files, spilled output, HOME and TMPDIR live
    /// here.
    pub state_dir: PathBuf,
    /// Extra read-only paths: those the user granted in permission scope, and a delegated
    /// worker's parent workspace.
    pub extra_read_roots: Vec<PathBuf>,
    /// Subtrees of the read roots the session can neither read nor write, for example the
    /// parent workspace's `.kageha/` and its other workers' workspaces.
    pub hidden_roots: Vec<PathBuf>,
    /// Extra write roots for one approved escalated command
    /// ([`ShellManager::run_once`]). Always empty for a chat's persistent session.
    pub extra_write_roots: Vec<PathBuf>,
    /// Host variables the operator chose to forward. Secret-named variables are still removed.
    pub passthrough: Vec<String>,
}

impl ShellPaths {
    /// Layout for `workspace` with HOME and TMPDIR under `workspace/.kageha/`. Pure.
    pub fn layout(workspace: &Path, toolchain_cache: &Path) -> Self {
        Self::layout_in(workspace, &workspace.join(STATE_DIR), toolchain_cache)
    }

    /// Layout for `workspace` whose Host state is `state_dir`, with HOME and TMPDIR there. Pure.
    pub fn layout_in(workspace: &Path, state_dir: &Path, toolchain_cache: &Path) -> Self {
        Self {
            workspace: workspace.to_path_buf(),
            toolchain_cache: toolchain_cache.to_path_buf(),
            temp_dir: state_dir.join("tmp"),
            home: state_dir.join("home"),
            state_dir: state_dir.to_path_buf(),
            extra_read_roots: Vec::new(),
            hidden_roots: Vec::new(),
            extra_write_roots: Vec::new(),
            passthrough: Vec::new(),
        }
    }

    /// Whether the state directory is outside the workspace (and so its own write root).
    fn external_state(&self) -> bool {
        !self.state_dir.starts_with(&self.workspace)
    }

    /// Canonicalizes the workspace, creates the Toolchain_Cache (mode 0700), HOME and
    /// TMPDIR, installs the `kageha` connector API shim into the Toolchain_Cache `bin`
    /// directory ([`cli_shim::install`]), and returns the resulting layout. A shim that
    /// can't be installed is logged and the session starts without it.
    pub fn prepare(workspace: &Path, toolchain_cache: &Path) -> io::Result<Self> {
        Self::prepare_in(workspace, None, toolchain_cache)
    }

    /// [`ShellPaths::prepare`] with the Host state in `state_dir` (default
    /// `<workspace>/.kageha`). An external state directory must already exist, be a real
    /// directory and lie outside the workspace.
    pub fn prepare_in(workspace: &Path, state_dir: Option<&Path>, toolchain_cache: &Path) -> io::Result<Self> {
        let workspace = workspace.canonicalize()?;
        if !workspace.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Shell workspace must be a directory",
            ));
        }
        let state_dir = match state_dir {
            None => workspace.join(STATE_DIR),
            Some(state) => {
                let canonical = state.canonicalize()?;
                let inside = canonical == workspace.join(STATE_DIR);
                if !fs::symlink_metadata(&canonical)?.is_dir()
                    || (!inside && (canonical.starts_with(&workspace) || workspace.starts_with(&canonical)))
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "The shell state directory must be the workspace's .kageha or lie outside the workspace",
                    ));
                }
                canonical
            }
        };
        let toolchain_cache = ensure_private_dir(toolchain_cache)?;
        let paths = Self::layout_in(&workspace, &state_dir, &toolchain_cache);
        ensure_private_dir(&paths.home)?;
        ensure_private_dir(&paths.temp_dir)?;
        if let Err(error) = cli_shim::install(&toolchain_cache) {
            eprintln!("kageha-workspace: the kageha CLI shim is not on the shell PATH: {error}");
        }
        if let Err(error) = ensure_python_venv(&toolchain_cache) {
            eprintln!("kageha-workspace: the shared Python venv is unavailable: {error}");
        }
        Ok(paths)
    }

    /// The sandbox the session runs under. Network is on. Extra write roots that don't
    /// exist, and `/`, are left out.
    pub fn sandbox(&self) -> ProcessSandbox {
        let mut write_roots = vec![
            self.workspace.clone(),
            self.toolchain_cache.clone(),
            self.temp_dir.clone(),
        ];
        if self.external_state() {
            write_roots.push(self.state_dir.clone());
        }
        for root in &self.extra_write_roots {
            if root.is_absolute() && root.parent().is_some() && root.exists() && !write_roots.contains(root) {
                write_roots.push(root.clone());
            }
        }
        write_roots.dedup();
        let read_roots = SYSTEM_READ_ROOTS
            .iter()
            .map(PathBuf::from)
            .chain(self.extra_read_roots.iter().cloned())
            .filter(|p| p.exists())
            .collect();
        let hidden_roots = self.hidden_roots.iter()
            .filter(|p| p.is_absolute() && p.parent().is_some() && p.exists())
            .cloned()
            .collect();
        ProcessSandbox {
            read_roots,
            write_roots,
            network: true,
            hidden_roots,
        }
    }
}

/// The shared Python venv, relative to the Toolchain_Cache. Its `bin` is first on `PATH`,
/// so `python`, `pip` and a `pip install uv` land there (Requirement 20.7).
pub const PYTHON_VENV: &str = "python/venv";
/// Interpreters tried, in order, to create [`PYTHON_VENV`].
pub const PYTHON_CANDIDATES: &[&str] = &["/opt/homebrew/bin/python3", "/usr/bin/python3"];
const VENV_READY: &str = ".kageha-ready";

/// Creates `<toolchain_cache>/python/venv` with the first available [`PYTHON_CANDIDATES`]
/// interpreter, once: concurrent sessions serialize on a lock file, and a venv left
/// half-made (no ready marker) is removed and rebuilt. Returns the venv path.
pub fn ensure_python_venv(toolchain_cache: &Path) -> io::Result<PathBuf> {
    let venv = toolchain_cache.join(PYTHON_VENV);
    if venv.join(VENV_READY).is_file() {
        return Ok(venv);
    }
    let parent = ensure_private_dir(&toolchain_cache.join("python"))?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(parent.join(".venv.lock"))?;
    lock.lock()?;
    if venv.join(VENV_READY).is_file() {
        return Ok(venv);
    }
    if fs::symlink_metadata(&venv).is_ok() {
        fs::remove_dir_all(&venv)?;
    }
    let python = PYTHON_CANDIDATES
        .iter()
        .map(Path::new)
        .find(|p| p.is_file())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no python3 interpreter found"))?;
    let output = Command::new(python)
        .args(["-m", "venv"])
        .arg(&venv)
        .env_remove("PYTHONPATH")
        .env_remove("VIRTUAL_ENV")
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() || !venv.join("bin/python").exists() {
        let _ = fs::remove_dir_all(&venv);
        return Err(io::Error::other(format!(
            "{} -m venv failed: {}",
            python.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    fs::write(venv.join(VENV_READY), b"")?;
    Ok(venv)
}

/// Whether the OS process sandbox shell sessions require is available on this host.
pub fn sandbox_available() -> bool {
    kageha_sandbox::available()
}

/// `$HOME/.kageha/toolchains` for the given host environment.
pub fn default_toolchain_cache(host_env: &BTreeMap<String, String>) -> io::Result<PathBuf> {
    match host_env.get("HOME") {
        Some(home) if Path::new(home).is_absolute() => Ok(Path::new(home).join(TOOLCHAIN_DIR)),
        _ => Err(io::Error::new(
            io::ErrorKind::NotFound,
            "HOME is not set to an absolute path",
        )),
    }
}

/// Creates `path` (and missing parents) with mode 0700 and tightens an existing directory
/// to 0700. Rejects a symlink at `path`. Returns the canonical path.
pub fn ensure_private_dir(path: &Path) -> io::Result<PathBuf> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Shell directory is not a real directory",
        ));
    }
    if meta.permissions().mode() & 0o777 != 0o700 {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    path.canonicalize()
}

/// True for names matching `*_TOKEN`, `*_KEY`, `*_SECRET` or `KAGEHA_SERVER_TOKEN`,
/// ignoring case.
pub fn is_secret_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    upper == "KAGEHA_SERVER_TOKEN"
        || upper.ends_with("_TOKEN")
        || upper.ends_with("_KEY")
        || upper.ends_with("_SECRET")
}

fn valid_lang(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-' | b'@'))
}

/// Builds the complete session environment. Pure: nothing is read from the process.
///
/// Only an allowlist is set (`PATH`, `HOME`, `LANG`, `TERM`, `TMPDIR`, `TMPPREFIX`,
/// `CFFIXED_USER_HOME`, `CLANG_MODULE_CACHE_PATH`, the Toolchain_Cache
/// variables, `KAGEHA_WORKSPACE` and `KAGEHA_API_SOCKET`) plus `paths.passthrough` copied from `host_env`. Any
/// variable with a secret-pattern name is removed from the result, whatever its source.
pub fn shell_environment(
    host_env: &BTreeMap<String, String>,
    paths: &ShellPaths,
) -> BTreeMap<String, String> {
    let tc = &paths.toolchain_cache;
    let at = |rel: &str| tc.join(rel).to_string_lossy().into_owned();
    let mut env = BTreeMap::new();
    for name in &paths.passthrough {
        if let Some(value) = host_env.get(name) {
            env.insert(name.clone(), value.clone());
        }
    }
    // The macOS /usr/bin/ar shim writes xcrun cache errors before its usage text.
    // Meson probes that text to select the archiver. Use the installed CLT binary directly.
    #[cfg(target_os = "macos")]
    {
        let ar = "/Library/Developer/CommandLineTools/usr/bin/ar";
        if Path::new(ar).is_file() {
            env.entry("AR".into()).or_insert_with(|| ar.into());
        }
    }
    let path = [
        at("python/venv/bin"),
        at("bin"),
        at("cargo/bin"),
        at("npm/bin"),
        at("python/bin"),
        SYSTEM_PATH.to_string(),
    ]
    .join(":");
    let lang = host_env
        .get("LANG")
        .filter(|v| valid_lang(v))
        .cloned()
        .unwrap_or_else(|| DEFAULT_LANG.into());
    let fixed = [
        ("PATH", path),
        ("HOME", paths.home.to_string_lossy().into_owned()),
        ("LANG", lang),
        ("TERM", "dumb".into()),
        ("TMPDIR", paths.temp_dir.to_string_lossy().into_owned()),
        // zsh writes here-documents to `${TMPPREFIX}XXXXXX` (default `/tmp/zsh`), not TMPDIR.
        ("TMPPREFIX", paths.temp_dir.join("zsh").to_string_lossy().into_owned()),
        // CoreFoundation resolves the home directory from the user record, not HOME, unless
        // this is set; framework caches (`~/Library/Caches`, for example Vision's model
        // cache) would otherwise land outside the write roots and fail.
        ("CFFIXED_USER_HOME", paths.home.to_string_lossy().into_owned()),
        // clang and swiftc keep their module cache in the per-user `/var/folders/.../C`
        // directory (from `confstr`, not HOME or TMPDIR), which is outside the write roots.
        (
            "CLANG_MODULE_CACHE_PATH",
            paths.home.join("Library/Caches/clang/ModuleCache").to_string_lossy().into_owned(),
        ),
        ("CARGO_HOME", at("cargo")),
        ("RUSTUP_HOME", at("rustup")),
        ("npm_config_prefix", at("npm")),
        ("npm_config_cache", at("npm-cache")),
        ("UV_CACHE_DIR", at("uv-cache")),
        ("PIP_CACHE_DIR", at("pip-cache")),
        ("PYTHONUSERBASE", at("python")),
        ("VIRTUAL_ENV", at(PYTHON_VENV)),
        ("PIP_DISABLE_PIP_VERSION_CHECK", "1".into()),
        (
            "KAGEHA_WORKSPACE",
            paths.workspace.to_string_lossy().into_owned(),
        ),
        // A path, not a secret: the capability token is read from `.kageha/api-token`.
        (
            "KAGEHA_API_SOCKET",
            paths.state_dir.join("api.sock").to_string_lossy().into_owned(),
        ),
    ];
    for (name, value) in fixed {
        env.insert(name.into(), value);
    }
    env.retain(|name, _| !is_secret_name(name));
    env
}

/// The host process environment as UTF-8 pairs (non-UTF-8 entries are skipped).
pub fn host_environment() -> BTreeMap<String, String> {
    std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .collect()
}

/// The shell program and its arguments: no startup files, commands read from stdin.
pub fn shell_program() -> (&'static str, Vec<String>) {
    if cfg!(target_os = "macos") {
        ("/bin/zsh", vec!["-f".into()])
    } else {
        ("/bin/bash", vec!["--noprofile".into(), "--norc".into()])
    }
}

/// Builds the confined command for a session. The default is [`ProcessSandbox::command`],
/// which fails when the OS sandbox is unavailable.
pub type Launcher = dyn Fn(&ProcessSandbox, &str, &[String]) -> io::Result<Command>;

/// Result of one foreground command.
///
/// `stdout` and `stderr` are the full stream (lossy UTF-8) when it is at most
/// [`SPILL_THRESHOLD`] bytes. Otherwise the stream is `truncated`, the text is the first and
/// last [`EXCERPT_BYTES`] around a one-line marker, and `*_path` names the workspace-relative
/// file holding the exact bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub stdout_path: Option<String>,
    pub stderr_path: Option<String>,
}

/// A persistent shell. State (working directory, variables, activated environments)
/// carries over between [`ShellSession::run`] calls.
pub struct ShellSession {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: ChildStdout,
    stderr: ChildStderr,
    paths: ShellPaths,
    /// Set when the session can no longer frame commands reliably (timeout, exit, I/O error).
    broken: Option<String>,
    reaped: bool,
    /// The session became a background job: dropping it must not kill its process group.
    detached: bool,
    /// Opened on the first spill; `paths.workspace` must be a Task_Workspace root.
    task: Option<TaskWorkspace>,
}

/// What [`ShellSession::run_or_detach`] did with a command.
#[derive(Debug)]
pub enum RunStep {
    Finished(CommandOutput),
    /// The command was still running at the deadline (or when `cancel` was set). The
    /// session is detached: its stdin is closed, it accepts no more commands, and the
    /// command keeps running with its output in the job files. Drop no longer kills it.
    Detached { cancelled: bool },
}

/// How one framed exchange ended.
enum Exchange {
    Done(i32, Vec<u8>, Vec<u8>),
    Deadline,
    Cancelled,
}

impl ShellSession {
    /// Starts the chat's shell under the OS sandbox. Fails if the sandbox is unavailable.
    pub fn open(paths: ShellPaths) -> io::Result<Self> {
        Self::open_with(paths, &|sandbox, program, args| sandbox.command(program, args))
    }

    /// Like [`ShellSession::open`] with a caller-chosen launcher. A launcher error is
    /// returned as is; the shell is never started without it.
    pub fn open_with(paths: ShellPaths, launch: &Launcher) -> io::Result<Self> {
        let (program, args) = shell_program();
        let mut command = launch(&paths.sandbox(), program, &args)?;
        command
            .env_clear()
            .envs(shell_environment(&host_environment(), &paths))
            .current_dir(&paths.workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut child = command.spawn()?;
        let stdin = child.stdin.take();
        let (stdout, stderr) = match (child.stdout.take(), child.stderr.take()) {
            (Some(out), Some(err)) => (out, err),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::other("Shell pipes unavailable"));
            }
        };
        nonblocking(&stdout)?;
        nonblocking(&stderr)?;
        Ok(Self {
            child,
            stdin,
            stdout,
            stderr,
            paths,
            broken: None,
            reaped: false,
            detached: false,
            task: None,
        })
    }

    pub fn paths(&self) -> &ShellPaths {
        &self.paths
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Why the session stopped accepting commands, if it did.
    pub fn broken(&self) -> Option<&str> {
        self.broken.as_deref()
    }

    /// Runs `command` in the session and waits up to `timeout` for it to finish.
    ///
    /// `call_id` names the spill files (`.kageha/tool-output/<call_id>-stdout.txt` and
    /// `-stderr.txt`); it must be 1 to [`MAX_CALL_ID`] bytes of `[A-Za-z0-9_.-]` and not start
    /// with `.`. The command's stdin is `/dev/null`.
    ///
    /// On timeout, exit of the shell or an I/O error the session is marked broken and later
    /// calls fail; the caller opens a new session. A timed-out command is killed with the
    /// session when it is dropped; use [`ShellSession::run_or_detach`] (or
    /// [`ShellManager::run`]) to keep it running as a job instead. A failed spill write is
    /// returned as an error but leaves the session usable.
    pub fn run(
        &mut self,
        command: &str,
        call_id: &str,
        timeout: Duration,
    ) -> io::Result<CommandOutput> {
        let job_id = jobs::new_job_id()?;
        match self.run_or_detach(command, call_id, &job_id, timeout, &AtomicBool::new(false))? {
            RunStep::Finished(output) => Ok(output),
            RunStep::Detached { .. } => {
                // Not handed to a job table: dropping the session kills the command.
                self.detached = false;
                self.broken = Some("Shell command timed out".into());
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Shell command timed out",
                ))
            }
        }
    }

    /// Runs `command` like [`ShellSession::run`], writing its output to the job files of
    /// `job_id` under `.kageha/jobs/`. If it is still running at `timeout`, or when
    /// `cancel` is set, the session is detached instead of broken: the command keeps
    /// running and [`RunStep::Detached`] is returned. The caller then owns the session as a
    /// job ([`ShellManager`] does this) and must open a new session for the chat.
    pub fn run_or_detach(
        &mut self,
        command: &str,
        call_id: &str,
        job_id: &str,
        timeout: Duration,
        cancel: &AtomicBool,
    ) -> io::Result<RunStep> {
        if let Some(reason) = &self.broken {
            return Err(io::Error::other(format!(
                "Shell session is no longer usable: {reason}"
            )));
        }
        if command.contains('\0') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Shell command contains a NUL byte",
            ));
        }
        if !valid_call_id(call_id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid call id",
            ));
        }
        if !jobs::valid_job_id(job_id) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Invalid job id"));
        }
        let jobs_dir = jobs::open_jobs_dir(&self.paths.state_dir, true)
            .map_err(|e| io::Error::other(format!("Cannot open {}: {e}", jobs::JOBS_DIR)))?;
        let files = jobs::JobFiles::new(job_id);
        let base = self.paths.state_dir.join("jobs");
        let nonce = new_nonce()?;
        let framed = frame(
            command,
            &nonce,
            [&files.log, &files.err, &files.exit].map(|name| base.join(name)),
        );
        let started = Instant::now();
        let (exit_code, pipe_out, pipe_err) =
            match self.exchange(&framed, &nonce, started + timeout, cancel) {
                Ok(Exchange::Done(code, out, err)) => (code, out, err),
                Ok(end @ (Exchange::Deadline | Exchange::Cancelled)) => {
                    // The shell reads EOF once the command ends, and exits.
                    self.stdin = None;
                    self.detached = true;
                    self.broken = Some(format!("The command continues as job {job_id}"));
                    return Ok(RunStep::Detached {
                        cancelled: matches!(end, Exchange::Cancelled),
                    });
                }
                Err(error) => {
                    self.broken = Some(error.to_string());
                    files.remove_except(&jobs_dir, &[]);
                    return Err(error);
                }
            };
        let duration_ms = started.elapsed().as_millis() as u64;
        let stdout = self.collect(&jobs_dir, &files.log, pipe_out, call_id, "stdout");
        let stderr = self.collect(&jobs_dir, &files.err, pipe_err, call_id, "stderr");
        // A stream left in place (over OUTPUT_LIMIT) is the only full copy; keep its file.
        let kept = |result: &io::Result<(String, Option<String>)>| {
            matches!(result, Ok((_, Some(path))) if path.starts_with(jobs::JOBS_DIR))
        };
        let keep: Vec<&str> = [(&stdout, files.log.as_str()), (&stderr, files.err.as_str())]
            .into_iter()
            .filter(|(result, _)| kept(result))
            .map(|(_, name)| name)
            .collect();
        files.remove_except(&jobs_dir, &keep);
        let ((stdout, stdout_path), (stderr, stderr_path)) = (stdout?, stderr?);
        Ok(RunStep::Finished(CommandOutput {
            exit_code,
            stdout,
            stderr,
            duration_ms,
            stdout_truncated: stdout_path.is_some(),
            stderr_truncated: stderr_path.is_some(),
            stdout_path,
            stderr_path,
        }))
    }

    /// One finished stream: its job file (if any) followed by what the shell printed on the
    /// pipe, bounded by [`ShellSession::bound`]. A file over [`OUTPUT_LIMIT`] is kept in
    /// place and named in the excerpt instead of being loaded.
    fn collect(
        &mut self,
        jobs_dir: &fs::File,
        name: &str,
        pipe: Vec<u8>,
        call_id: &str,
        stream: &str,
    ) -> io::Result<(String, Option<String>)> {
        match jobs::read_job_file(jobs_dir, name, OUTPUT_LIMIT)? {
            jobs::JobFile::Missing => self.bound(call_id, stream, &pipe),
            jobs::JobFile::Bytes(mut bytes) => {
                bytes.extend_from_slice(&pipe);
                self.bound(call_id, stream, &bytes)
            }
            jobs::JobFile::Large { head, tail, size } => {
                let rel = format!("{}/{name}", jobs::JOBS_DIR);
                let text = format!(
                    "{}\n[... {} bytes omitted; full output in {rel} ...]\n{}",
                    String::from_utf8_lossy(&head),
                    size.saturating_sub((head.len() + tail.len()) as u64),
                    String::from_utf8_lossy(&tail),
                );
                Ok((text, Some(rel)))
            }
        }
    }

    /// The session's current directory when it is inside the workspace, else the
    /// workspace root. Runs `pwd -P` in the session.
    pub fn current_dir(&mut self) -> PathBuf {
        let root = self.paths.workspace.clone();
        match self.run("pwd -P", "cwd", Duration::from_secs(5)) {
            Ok(out) if out.exit_code == 0 => {
                let dir = PathBuf::from(out.stdout.trim_end_matches('\n'));
                if dir.starts_with(&root) && dir.is_dir() {
                    dir
                } else {
                    root
                }
            }
            _ => root,
        }
    }

    /// Saves the session's working directory and non-secret exported variables to
    /// `.kageha/shell-env` (see [`state`]). Runs [`state::CAPTURE`] in the session, so the
    /// workspace must be a Task_Workspace root. Secret-named variables are filtered in the
    /// shell before any value is printed, and again before the file is written.
    pub fn save_state(&mut self) -> io::Result<()> {
        if self.task_workspace().is_err() {
            // Only a Task_Workspace has a `.kageha/` for the state; nothing to save.
            return Ok(());
        }
        let out = self.run(state::CAPTURE, state::CAPTURE_CALL_ID, Duration::from_secs(5))?;
        if out.stdout_truncated || out.stderr_truncated || out.exit_code != 0 {
            // A spilled capture is not used; don't leave the variables in tool-output.
            for path in [&out.stdout_path, &out.stderr_path].into_iter().flatten() {
                if path.starts_with(".kageha/tool-output/") {
                    let _ = self.task_workspace()?.remove_host_file(path);
                }
            }
            return Err(io::Error::other("The shell state could not be captured"));
        }
        let base = shell_environment(&host_environment(), &self.paths);
        let saved = state::parse_capture(&out.stdout, &self.paths.workspace, &base)
            .ok_or_else(|| io::Error::other("The shell state capture is malformed"))?;
        state::save(self.task_workspace()?, &saved)
    }

    /// Applies the state saved in `.kageha/shell-env` to this (new) session: changes to the
    /// saved directory and exports the saved variables. Returns whether a saved state was
    /// applied; `false` when there is none (or the workspace is not a Task_Workspace).
    pub fn restore_state(&mut self) -> io::Result<bool> {
        let Ok(task) = self.task_workspace() else {
            return Ok(false);
        };
        let Some(saved) = state::load(task) else {
            return Ok(false);
        };
        let Some(command) = state::restore_command(&saved, &self.paths.workspace) else {
            return Ok(false);
        };
        let out = self.run(&command, "kg-shell-restore", Duration::from_secs(10))?;
        Ok(out.exit_code == 0)
    }

    /// True once the shell process has exited (reaping it). Never blocks.
    pub fn try_reap(&mut self) -> bool {
        if self.reaped {
            return true;
        }
        match self.child.try_wait() {
            Ok(None) => false,
            Ok(Some(_)) | Err(_) => {
                self.reaped = true;
                true
            }
        }
    }

    /// The stream as text, or, over [`SPILL_THRESHOLD`], its excerpt and spill file path.
    fn bound(
        &mut self,
        call_id: &str,
        stream: &str,
        bytes: &[u8],
    ) -> io::Result<(String, Option<String>)> {
        let text = String::from_utf8_lossy(bytes);
        if bytes.len() <= SPILL_THRESHOLD {
            return Ok((text.into_owned(), None));
        }
        let rel = format!("{STATE_DIR}/tool-output/{call_id}-{stream}.txt");
        self.task_workspace()?
            .write_host_file(&rel, bytes)
            .map_err(|e| io::Error::other(format!("Cannot spill shell {stream}: {e}")))?;
        Ok((excerpt(&text, &rel), Some(rel)))
    }

    fn task_workspace(&mut self) -> io::Result<&TaskWorkspace> {
        if self.task.is_none() {
            let workspace = &self.paths.workspace;
            let not_task = || io::Error::other("Shell workspace is not a Task_Workspace");
            // A chat's `<chat root>/workspace`, or any workspace the Host already opened
            // (a nested worker workspace or a worktree with external state).
            let task = match (workspace.file_name(), workspace.parent()) {
                _ if crate::task::has_manifest(&self.paths.state_dir) => {
                    TaskWorkspace::attach(workspace, &self.paths.state_dir)
                }
                (Some(name), Some(parent)) if name == WORKSPACE_DIR => TaskWorkspace::open(parent),
                _ => return Err(not_task()),
            }
            .map_err(|e| io::Error::other(format!("Cannot open Task_Workspace: {e}")))?;
            if task.root() != workspace.as_path() || task.state_dir() != self.paths.state_dir {
                return Err(not_task());
            }
            self.task = Some(task);
        }
        Ok(self.task.as_ref().expect("opened above"))
    }

    fn exchange(
        &mut self,
        framed: &str,
        nonce: &str,
        deadline: Instant,
        cancel: &AtomicBool,
    ) -> io::Result<Exchange> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| io::Error::other("Shell stdin closed"))?;
        stdin.write_all(framed.as_bytes())?;
        stdin.flush()?;
        let (mut out_end, mut err_end) = (Sentinel::new(nonce), Sentinel::new(nonce));
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let (mut out_done, mut err_done) = (None, None);
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            if out_done.is_none() {
                out_done = out_end.scan(&out);
            }
            if err_done.is_none() {
                err_done = err_end.scan(&err);
            }
            if let (Some((at, code)), Some((err_at, _))) = (out_done, err_done) {
                out.truncate(at);
                err.truncate(err_at);
                return Ok(Exchange::Done(code, out, err));
            }
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return Ok(Exchange::Cancelled);
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(Exchange::Deadline);
            }
            let wait = (deadline - now).min(Duration::from_millis(200));
            let mut fds = [
                libc::pollfd {
                    fd: self.stdout.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: self.stderr.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, wait.as_millis() as libc::c_int) };
            if ready < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            for (pipe, target) in [(0, &mut out), (1, &mut err)] {
                if fds[pipe].revents == 0 {
                    continue;
                }
                let read = if pipe == 0 {
                    self.stdout.read(&mut buffer)
                } else {
                    self.stderr.read(&mut buffer)
                };
                match read {
                    Ok(0) => return Err(self.exited()),
                    Ok(n) => {
                        if target.len() + n > OUTPUT_LIMIT {
                            return Err(io::Error::other("Shell output exceeds 64 MiB"));
                        }
                        target.extend_from_slice(&buffer[..n]);
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) => {}
                    Err(e) => return Err(e),
                }
            }
        }
    }

    fn exited(&mut self) -> io::Error {
        match self.child.try_wait() {
            Ok(Some(status)) => {
                self.reaped = true;
                io::Error::other(format!("Shell session exited ({status})"))
            }
            _ => io::Error::other("Shell session closed its output"),
        }
    }

    /// Ends the session: closes stdin, waits briefly, then kills the process group.
    pub fn close(mut self) {
        self.stdin = None;
        let end = Instant::now() + Duration::from_secs(2);
        while Instant::now() < end {
            if let Ok(Some(_)) = self.child.try_wait() {
                self.reaped = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for ShellSession {
    fn drop(&mut self) {
        // A detached session is a running job; its lifetime is the job table's business.
        if self.detached {
            return;
        }
        // The shell leads its own process group; take foreground children down with it.
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        if !self.reaped {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn nonblocking(value: &impl AsRawFd) -> io::Result<()> {
    let fd = value.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// 128 random bits from the kernel CSPRNG as 32 lowercase hex digits.
fn new_nonce() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn valid_call_id(call_id: &str) -> bool {
    !call_id.is_empty()
        && call_id.len() <= MAX_CALL_ID
        && !call_id.starts_with('.')
        && call_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

fn single_quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// The command runs through `eval` in the session shell so `cd`, `export` and `source`
/// persist; a syntax error yields a non-zero status instead of a hung parser. Redirections
/// on `eval` apply only to it, in the current shell. `files` are the absolute stdout,
/// stderr and exit-status job files. The status is held in `__kg_rc` so the exit file and
/// both sentinels carry it.
fn frame(command: &str, nonce: &str, files: [PathBuf; 3]) -> String {
    let [out, err, exit] = files.map(|path| single_quoted(&path.to_string_lossy()));
    format!(
        "eval {} < /dev/null > {out} 2> {err}; __kg_rc=$?; \
         printf '%d\\n' \"$__kg_rc\" > {exit} 2>/dev/null; \
         printf '\\n__KG_END_%s_%d\\n' {nonce} \"$__kg_rc\"; \
         printf '\\n__KG_END_%s_%d\\n' {nonce} \"$__kg_rc\" >&2; unset __kg_rc\n",
        single_quoted(command)
    )
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Incremental search for the end sentinel line `\n__KG_END_<nonce>_<exit>\n` in one
/// stream. Only that exact line matches: a different nonce, text before the marker on the
/// same line, a non-numeric exit or a missing final newline do not end the stream.
struct Sentinel {
    /// `\n__KG_END_<nonce>_`; the leading newline is the one the framing prints.
    marker: Vec<u8>,
    /// Bytes before this offset cannot start the sentinel.
    from: usize,
}

/// Longest exit-status field accepted (fits any `i32`).
const MAX_EXIT_DIGITS: usize = 10;

impl Sentinel {
    fn new(nonce: &str) -> Self {
        Self {
            marker: format!("\n__KG_END_{nonce}_").into_bytes(),
            from: 0,
        }
    }

    /// Once `buf` holds the sentinel line, returns where the command's output ends (the
    /// framing newline) and the exit status. `buf` must only grow between calls.
    fn scan(&mut self, buf: &[u8]) -> Option<(usize, i32)> {
        while let Some(rel) = find(&buf[self.from..], &self.marker) {
            let at = self.from + rel;
            let rest = &buf[at + self.marker.len()..];
            let digits = rest
                .iter()
                .take(MAX_EXIT_DIGITS + 1)
                .take_while(|b| b.is_ascii_digit())
                .count();
            match rest.get(digits) {
                Some(b'\n') if (1..=MAX_EXIT_DIGITS).contains(&digits) => {
                    let code = std::str::from_utf8(&rest[..digits])
                        .ok()
                        .and_then(|s| s.parse().ok());
                    if let Some(code) = code {
                        return Some((at, code));
                    }
                }
                // Still arriving: wait for more bytes without moving past this candidate.
                None if digits <= MAX_EXIT_DIGITS => {
                    self.from = at;
                    return None;
                }
                _ => {}
            }
            self.from = at + 1;
        }
        let tail = buf.len().saturating_sub(self.marker.len() - 1);
        self.from = self.from.max(tail);
        None
    }
}

fn floor_boundary(text: &str, mut at: usize) -> usize {
    at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

fn ceil_boundary(text: &str, mut at: usize) -> usize {
    at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at += 1;
    }
    at
}

/// The first and last [`EXCERPT_BYTES`] of `text` (at char boundaries) around a marker line
/// naming the spill file.
fn excerpt(text: &str, path: &str) -> String {
    let head = floor_boundary(text, EXCERPT_BYTES);
    let tail = ceil_boundary(text, text.len().saturating_sub(EXCERPT_BYTES)).max(head);
    format!(
        "{}\n[... {} bytes omitted; full output in {path} ...]\n{}",
        &text[..head],
        tail - head,
        &text[tail..]
    )
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    OpenSession {
        chat: String,
        workspace: PathBuf,
        #[serde(default)]
        read_roots: Vec<PathBuf>,
        #[serde(default)]
        state_dir: Option<PathBuf>,
        #[serde(default)]
        hidden_roots: Vec<PathBuf>,
    },
    Run {
        chat: String,
        command: String,
        call_id: String,
        #[serde(default)]
        timeout_s: Option<u64>,
    },
    Close {
        chat: String,
    },
    StartJob {
        chat: String,
        command: String,
    },
    JobStatus {
        chat: String,
        job_id: String,
    },
    JobOutput {
        chat: String,
        job_id: String,
        #[serde(default)]
        offset: u64,
        #[serde(default)]
        max_bytes: Option<usize>,
    },
    StopJob {
        chat: String,
        job_id: String,
    },
    AdoptJob {
        workspace: PathBuf,
        record: JobRecord,
        #[serde(default)]
        state_dir: Option<PathBuf>,
    },
    RunOnce {
        chat: String,
        workspace: PathBuf,
        #[serde(default)]
        roots: SessionRoots,
        cwd: PathBuf,
        #[serde(default)]
        write_roots: Vec<PathBuf>,
        command: String,
        call_id: String,
        #[serde(default)]
        timeout_s: Option<u64>,
    },
    Cancel {
        chat: String,
        call_id: String,
    },
    SessionDir {
        chat: String,
    },
    Ping,
}

fn valid_chat(chat: &str) -> bool {
    !chat.is_empty()
        && chat.len() <= 128
        && chat
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        && chat != "."
        && chat != ".."
}

/// Serves a [`ShellManager`] (one shell session per chat plus its jobs) as JSON-lines
/// requests:
///
/// ```text
/// {"op":"open_session","chat":"c1","workspace":"/abs/workspace","read_roots":[]}
///   -> {"ok":true,"chat":"c1","pid":123,"reused":false,"restored":false}
///      (`restored`: a new session started from the saved `.kageha/shell-env`)
/// {"op":"run","chat":"c1","command":"echo hi","call_id":"call_1","timeout_s":600}
///   -> {"ok":true,"timed_out":false,"exit_code":0,"stdout":"hi\n","stderr":"",
///       "duration_ms":4,"stdout_truncated":false,"stderr_truncated":false,
///       "stdout_path":null,"stderr_path":null}
///   -> {"ok":true,"timed_out":true,"job":{"job_id":"job-…","state":"running",…}}
/// {"op":"start_job","chat":"c1","command":"npm run dev"}
///   -> {"ok":true,"job":{"job_id":"job-…","state":"running","pid":…,…}}
/// {"op":"job_status","chat":"c1","job_id":"job-…"}       -> {"ok":true,"job":{…}}
/// {"op":"job_output","chat":"c1","job_id":"job-…","offset":0}
///   -> {"ok":true,"output":{"output":"…","offset":0,"next_offset":120,…}}
/// {"op":"stop_job","chat":"c1","job_id":"job-…"}         -> {"ok":true,"job":{…}}
/// {"op":"adopt_job","workspace":"/abs/workspace","record":{…}} -> {"ok":true,"job":{…}}
/// {"op":"close","chat":"c1"}
///   -> {"ok":true,"closed":true}
/// {"op":"run_once","chat":"c1","workspace":"/abs/workspace","cwd":"/abs/workspace/sub",
///  "write_roots":["/Users/me"],"command":"cp a ~/b","call_id":"call_2","timeout_s":600}
///   -> same replies as `run` (see `ShellManager::run_once`)
/// {"op":"cancel","chat":"c1","call_id":"call_1"}
///   -> {"ok":true,"cancelled":true}   (false when no such call is running)
/// {"op":"session_dir","chat":"c1"}
///   -> {"ok":true,"dir":"/abs/workspace/sub"}   (null without an idle session)
/// {"op":"ping"}
///   -> {"ok":true,"pid":123,"toolchain_cache":"/Users/me/.kageha/toolchains"}
/// ```
///
/// `open_session` also takes optional `state_dir` (a state directory outside the workspace)
/// and `hidden_roots`, `adopt_job` an optional `state_dir`, and `run_once` an optional
/// `roots` object with the same three fields ([`SessionRoots`]).
///
/// A `run` stream over 32 KiB comes back as an excerpt with `*_truncated: true` and
/// `*_path` set to the workspace-relative spill file
/// (`.kageha/tool-output/<call_id>-stdout.txt`). A `run` still going at `timeout_s`
/// continues as a job. A `cancel` from another connection makes a running `run` or
/// `run_once` with that chat and `call_id` continue as a job right away
/// (`"cancelled":true`), like setting `cancel` on [`ShellManager::run`]. Errors are
/// `{"ok":false,"error":"...","kind":"not_found"}`, where `kind` names the
/// [`io::ErrorKind`] ([`error_kind_name`]).
pub struct ShellService {
    manager: ShellManager,
    /// Cancellation flags of the `run` and `run_once` requests in progress, by chat and
    /// call id.
    running: std::sync::Mutex<std::collections::HashMap<(String, String), std::sync::Arc<AtomicBool>>>,
}

/// Removes a request's cancellation flag when the request ends.
struct Running<'a> {
    service: &'a ShellService,
    key: (String, String),
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.service
            .running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.key);
    }
}

/// The `kind` of a socket protocol error reply, so a client can rebuild the error's
/// [`io::ErrorKind`] with [`error_kind`].
pub fn error_kind_name(kind: io::ErrorKind) -> &'static str {
    match kind {
        io::ErrorKind::InvalidInput => "invalid_input",
        io::ErrorKind::NotFound => "not_found",
        io::ErrorKind::PermissionDenied => "permission_denied",
        io::ErrorKind::TimedOut => "timed_out",
        io::ErrorKind::Interrupted => "interrupted",
        io::ErrorKind::Unsupported => "unsupported",
        _ => "other",
    }
}

/// The [`io::ErrorKind`] named by [`error_kind_name`]; `Other` for anything else.
pub fn error_kind(name: &str) -> io::ErrorKind {
    match name {
        "invalid_input" => io::ErrorKind::InvalidInput,
        "not_found" => io::ErrorKind::NotFound,
        "permission_denied" => io::ErrorKind::PermissionDenied,
        "timed_out" => io::ErrorKind::TimedOut,
        "interrupted" => io::ErrorKind::Interrupted,
        "unsupported" => io::ErrorKind::Unsupported,
        _ => io::ErrorKind::Other,
    }
}

fn foreground_reply(foreground: Foreground) -> io::Result<Value> {
    match foreground {
        Foreground::Finished(output) => {
            let mut value = serde_json::to_value(output).map_err(io::Error::other)?;
            value["timed_out"] = json!(false);
            Ok(value)
        }
        Foreground::Detached { job, cancelled } => {
            Ok(json!({"timed_out": true, "cancelled": cancelled, "job": job}))
        }
    }
}

impl ShellService {
    pub fn new(toolchain_cache: PathBuf) -> Self {
        Self {
            manager: ShellManager::new(toolchain_cache),
            running: Default::default(),
        }
    }

    /// Docker supplies process and filesystem isolation for this service.
    pub fn in_container(toolchain_cache: PathBuf) -> Self {
        Self {
            manager: ShellManager::in_container(toolchain_cache),
            running: Default::default(),
        }
    }

    /// Registers the cancellation flag of a `run` or `run_once` request. A second request
    /// with the same chat and call id while the first runs is refused.
    fn running(&self, chat: &str, call_id: &str) -> io::Result<(Running<'_>, std::sync::Arc<AtomicBool>)> {
        let key = (chat.to_owned(), call_id.to_owned());
        let flag = std::sync::Arc::new(AtomicBool::new(false));
        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        if running.contains_key(&key) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "A command with this call_id is already running",
            ));
        }
        running.insert(key.clone(), flag.clone());
        Ok((Running { service: self, key }, flag))
    }

    pub fn manager(&self) -> &ShellManager {
        &self.manager
    }

    pub fn handle_line(&self, line: &str) -> String {
        let response = match serde_json::from_str::<Request>(line) {
            Ok(request) => self.handle(request),
            Err(error) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Invalid request: {error}"),
            )),
        };
        match response {
            Ok(mut value) => {
                value["ok"] = json!(true);
                value.to_string()
            }
            Err(error) => {
                json!({"ok": false, "error": error.to_string(), "kind": error_kind_name(error.kind())})
                    .to_string()
            }
        }
    }

    fn handle(&self, request: Request) -> io::Result<Value> {
        let job = |status: JobStatus| Ok(json!({ "job": status }));
        match request {
            Request::OpenSession {
                chat,
                workspace,
                read_roots,
                state_dir,
                hidden_roots,
            } => {
                let roots = SessionRoots { state_dir, read_roots, hidden_roots };
                let info = self.manager.open_session_in(&chat, &workspace, &roots)?;
                Ok(json!({"chat": chat, "pid": info.pid, "reused": info.reused, "restored": info.restored}))
            }
            Request::Run {
                chat,
                command,
                call_id,
                timeout_s,
            } => {
                let timeout = timeout_s
                    .map(Duration::from_secs)
                    .unwrap_or(DEFAULT_TIMEOUT)
                    .min(MAX_TIMEOUT);
                let (_running, cancel) = self.running(&chat, &call_id)?;
                foreground_reply(self.manager.run(&chat, &command, &call_id, timeout, &cancel)?)
            }
            Request::RunOnce {
                chat,
                workspace,
                roots,
                cwd,
                write_roots,
                command,
                call_id,
                timeout_s,
            } => {
                let timeout = timeout_s
                    .map(Duration::from_secs)
                    .unwrap_or(DEFAULT_TIMEOUT)
                    .min(MAX_TIMEOUT);
                let (_running, cancel) = self.running(&chat, &call_id)?;
                foreground_reply(self.manager.run_once_in(
                    &chat, &workspace, &roots, &cwd, write_roots, &command, &call_id, timeout, &cancel,
                )?)
            }
            Request::Cancel { chat, call_id } => {
                let flag = self
                    .running
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&(chat, call_id))
                    .cloned();
                if let Some(flag) = &flag {
                    flag.store(true, std::sync::atomic::Ordering::SeqCst);
                }
                Ok(json!({"cancelled": flag.is_some()}))
            }
            Request::SessionDir { chat } => Ok(json!({"dir": self.manager.session_dir(&chat)})),
            Request::Ping => Ok(json!({
                "pid": std::process::id(),
                "toolchain_cache": self.manager.toolchain_cache(),
            })),
            Request::Close { chat } => Ok(json!({"closed": self.manager.close_session(&chat)})),
            Request::StartJob { chat, command } => job(self.manager.start_job(&chat, &command)?),
            Request::JobStatus { chat, job_id } => job(self.manager.job_status(&chat, &job_id)?),
            Request::JobOutput {
                chat,
                job_id,
                offset,
                max_bytes,
            } => {
                let max = max_bytes.unwrap_or(JOB_OUTPUT_WINDOW);
                let output = self.manager.job_output(&chat, &job_id, offset, max)?;
                Ok(json!({ "output": output }))
            }
            Request::StopJob { chat, job_id } => job(self.manager.stop_job(&chat, &job_id)?),
            Request::AdoptJob { workspace, record, state_dir } => {
                job(self.manager.adopt_job_in(&workspace, state_dir.as_deref(), record)?)
            }
        }
    }

    /// Answers requests on one connection until it closes. One request per line.
    pub fn serve_connection(&self, stream: UnixStream) -> io::Result<()> {
        let mut writer = stream.try_clone()?;
        let mut reader = BufReader::new(stream);
        loop {
            let mut line = Vec::new();
            let n = (&mut reader)
                .take(REQUEST_LIMIT as u64 + 1)
                .read_until(b'\n', &mut line)?;
            if n == 0 {
                return Ok(());
            }
            if line.len() > REQUEST_LIMIT {
                let reply = json!({"ok": false, "error": "Request exceeds 16 MiB"});
                writeln!(writer, "{reply}")?;
                return Ok(());
            }
            let reply = match std::str::from_utf8(&line) {
                Ok(text) if text.trim().is_empty() => continue,
                Ok(text) => self.handle_line(text.trim_end()),
                Err(_) => json!({"ok": false, "error": "Request is not UTF-8"}).to_string(),
            };
            writeln!(writer, "{reply}")?;
            writer.flush()?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn fake_paths() -> ShellPaths {
        ShellPaths::layout(Path::new("/w/chat"), Path::new("/h/.kageha/toolchains"))
    }

    #[test]
    fn environment_sets_allowlist_and_cache_vars() {
        let host = BTreeMap::from([
            ("LANG".to_string(), "de_DE.UTF-8".to_string()),
            ("PATH".to_string(), "/evil/bin".to_string()),
            ("HOME".to_string(), "/Users/me".to_string()),
            ("SSH_AUTH_SOCK".to_string(), "/tmp/agent".to_string()),
        ]);
        let env = shell_environment(&host, &fake_paths());
        let tc = "/h/.kageha/toolchains";
        assert_eq!(env["HOME"], "/w/chat/.kageha/home");
        assert_eq!(env["TMPDIR"], "/w/chat/.kageha/tmp");
        assert_eq!(env["TMPPREFIX"], "/w/chat/.kageha/tmp/zsh");
        assert_eq!(env["CFFIXED_USER_HOME"], "/w/chat/.kageha/home");
        assert_eq!(env["CLANG_MODULE_CACHE_PATH"], "/w/chat/.kageha/home/Library/Caches/clang/ModuleCache");
        assert_eq!(env["KAGEHA_WORKSPACE"], "/w/chat");
        assert_eq!(env["KAGEHA_API_SOCKET"], "/w/chat/.kageha/api.sock");
        assert_eq!(env["LANG"], "de_DE.UTF-8");
        assert_eq!(env["TERM"], "dumb");
        assert_eq!(env["CARGO_HOME"], format!("{tc}/cargo"));
        assert_eq!(env["RUSTUP_HOME"], format!("{tc}/rustup"));
        assert_eq!(env["npm_config_prefix"], format!("{tc}/npm"));
        assert_eq!(env["npm_config_cache"], format!("{tc}/npm-cache"));
        assert_eq!(env["UV_CACHE_DIR"], format!("{tc}/uv-cache"));
        assert_eq!(env["PIP_CACHE_DIR"], format!("{tc}/pip-cache"));
        assert_eq!(env["PYTHONUSERBASE"], format!("{tc}/python"));
        assert!(env["PATH"].starts_with(&format!(
            "{tc}/python/venv/bin:{tc}/bin:{tc}/cargo/bin:{tc}/npm/bin:{tc}/python/bin:"
        )));
        assert_eq!(env["VIRTUAL_ENV"], format!("{tc}/python/venv"));
        assert_eq!(env["PIP_DISABLE_PIP_VERSION_CHECK"], "1");
        assert!(env["PATH"].ends_with(SYSTEM_PATH));
        assert!(!env.contains_key("SSH_AUTH_SOCK"));
        // Values derived from the toolchain cache or workspace, never the host PATH/HOME.
        assert!(!env.values().any(|v| v.contains("/evil") || v == "/Users/me"));
        let bad_lang = BTreeMap::from([("LANG".to_string(), "x;rm -rf".to_string())]);
        assert_eq!(shell_environment(&bad_lang, &fake_paths())["LANG"], DEFAULT_LANG);
    }

    #[test]
    fn environment_strips_secret_names_even_when_passed_through() {
        let mut paths = fake_paths();
        paths.passthrough = [
            "GITHUB_TOKEN",
            "openai_api_key",
            "Aws_Secret",
            "kageha_server_token",
            "TZ",
        ]
        .map(String::from)
        .to_vec();
        let host: BTreeMap<String, String> = paths
            .passthrough
            .iter()
            .map(|k| (k.clone(), "value".to_string()))
            .collect();
        let env = shell_environment(&host, &paths);
        assert!(env.keys().all(|k| !is_secret_name(k)), "{env:?}");
        assert_eq!(env["TZ"], "value");
        for name in ["X_TOKEN", "x_key", "A_Secret", "KAGEHA_SERVER_TOKEN"] {
            assert!(is_secret_name(name));
        }
        for name in ["TOKENIZER", "KEYCHAIN", "PATH", "SECRETS_DIR"] {
            assert!(!is_secret_name(name));
        }
    }

    #[test]
    fn sandbox_unavailable_is_an_error_not_a_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ShellPaths::prepare(dir.path(), &dir.path().join("tc")).unwrap();
        let unavailable: &Launcher = &|_, _, _| {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Required process sandbox unavailable",
            ))
        };
        let error = ShellSession::open_with(paths.clone(), unavailable)
            .err()
            .expect("must not start an unconfined shell");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        if !kageha_sandbox::available() {
            assert!(ShellSession::open(paths).is_err());
        }
    }

    #[test]
    fn prepare_creates_private_toolchain_cache_and_chat_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let tc = dir.path().join("home/.kageha/toolchains");
        let paths = ShellPaths::prepare(dir.path(), &tc).unwrap();
        let mode = fs::metadata(&tc).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        assert!(paths.home.is_dir() && paths.temp_dir.is_dir());
        assert!(paths.home.starts_with(&paths.workspace));
        let sandbox = paths.sandbox();
        assert!(sandbox.network);
        assert!(sandbox.write_roots.contains(&paths.workspace));
        assert!(sandbox.write_roots.contains(&paths.toolchain_cache));
        assert!(sandbox.write_roots.contains(&paths.temp_dir));
    }

    #[test]
    fn an_external_state_directory_holds_home_tmp_and_the_socket_and_is_writable() {
        let dir = tempfile::tempdir().unwrap();
        let checkout = dir.path().join("checkout");
        let state = dir.path().join("state");
        fs::create_dir_all(&checkout).unwrap();
        fs::create_dir_all(&state).unwrap();
        let tc = dir.path().join("tc");
        let mut paths = ShellPaths::prepare_in(&checkout, Some(&state), &tc).unwrap();
        let state = state.canonicalize().unwrap();
        assert_eq!(paths.state_dir, state);
        assert_eq!((paths.home.clone(), paths.temp_dir.clone()), (state.join("home"), state.join("tmp")));
        assert!(!checkout.join(".kageha").exists());
        let env = shell_environment(&BTreeMap::new(), &paths);
        assert_eq!(env["KAGEHA_API_SOCKET"], state.join("api.sock").to_string_lossy());
        assert_eq!(env["KAGEHA_WORKSPACE"], paths.workspace.to_string_lossy());
        let parent = dir.path().join("parent");
        fs::create_dir_all(parent.join(".kageha")).unwrap();
        paths.extra_read_roots = vec![parent.clone()];
        paths.hidden_roots = vec![parent.join(".kageha"), parent.join("missing")];
        let sandbox = paths.sandbox();
        assert!(sandbox.write_roots.contains(&paths.workspace) && sandbox.write_roots.contains(&state));
        assert!(sandbox.read_roots.contains(&parent) && !sandbox.write_roots.contains(&parent));
        assert_eq!(sandbox.hidden_roots, [parent.join(".kageha")]);
        // A state directory inside the checkout (other than `.kageha`) is refused.
        fs::create_dir_all(checkout.join("inner")).unwrap();
        assert!(ShellPaths::prepare_in(&checkout, Some(&checkout.join("inner")), &tc).is_err());
    }

    const NONCE: &str = "0123456789abcdef0123456789abcdef";

    fn scan(buf: &[u8]) -> Option<(usize, i32)> {
        Sentinel::new(NONCE).scan(buf)
    }

    #[test]
    fn sentinel_matches_only_the_exact_line() {
        let end = |code: &str| format!("\n__KG_END_{NONCE}_{code}\n");
        // Output without a trailing newline: the framing newline is the boundary.
        assert_eq!(scan(format!("hi{}", end("3")).as_bytes()), Some((2, 3)));
        assert_eq!(scan(format!("hi\n{}", end("0")).as_bytes()), Some((3, 0)));
        assert_eq!(scan(end("127").as_bytes()), Some((0, 127)));
        // Fake sentinels the command could print do not end the stream.
        let fakes = [
            format!("\n__KG_END_{}_0\n", "f".repeat(32)),
            format!("\n__KG_END_{}_0\n", &NONCE[..31]),
            format!("x__KG_END_{NONCE}_0\n"),
            format!("\n__KG_END_{NONCE}_0x\n"),
            format!("\n__KG_END_{NONCE}_\n"),
            format!("\n__KG_END_{NONCE}_-1\n"),
            format!("\n__KG_END_{NONCE}_99999999999\n"),
        ];
        for fake in &fakes {
            assert_eq!(scan(fake.as_bytes()), None, "{fake:?}");
            let real = format!("{fake}after{}", end("1"));
            assert_eq!(scan(real.as_bytes()), Some((fake.len() + 5, 1)), "{fake:?}");
        }
        // Incomplete sentinel (no final newline yet) is not an end.
        assert_eq!(scan(format!("hi\n__KG_END_{NONCE}_1").as_bytes()), None);
        assert_eq!(single_quoted("it's"), r"'it'\''s'");
    }

    #[test]
    fn sentinel_scan_is_incremental() {
        let fake = format!("\n__KG_END_{}_0\n", "e".repeat(32));
        let stream = format!("partial{fake}line\n__KG_END_{NONCE}_42\ntrailing");
        let mut sentinel = Sentinel::new(NONCE);
        let mut found = None;
        for len in 0..=stream.len() {
            found = sentinel.scan(&stream.as_bytes()[..len]);
            if found.is_some() {
                break;
            }
        }
        assert_eq!(found, Some((7 + fake.len() + 4, 42)));
    }

    #[test]
    fn nonce_is_128_bit_hex_and_fresh() {
        let (a, b) = (new_nonce().unwrap(), new_nonce().unwrap());
        assert_eq!(a.len(), 32);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_ne!(a, b);
        let files = ["o", "e", "x"].map(|name| PathBuf::from(format!("/w/.kageha/jobs/{name}")));
        let framed = frame("echo hi", &a, files);
        assert!(framed.contains(&a) && !framed.contains(&b));
        assert!(framed.starts_with("eval 'echo hi' < /dev/null > '/w/.kageha/jobs/o' 2> '/w/.kageha/jobs/e';"));
    }

    #[test]
    fn excerpt_keeps_head_and_tail_at_char_boundaries() {
        // 3-byte chars so 16 KiB never lands on a boundary.
        let text = "é".repeat(5) + &"界".repeat(20_000);
        let out = excerpt(&text, ".kageha/tool-output/c-stdout.txt");
        let (head, rest) = out.split_once('\n').unwrap();
        let (marker, tail) = rest.split_once('\n').unwrap();
        assert!(head.len() <= EXCERPT_BYTES && head.len() > EXCERPT_BYTES - 4);
        assert!(tail.len() <= EXCERPT_BYTES && tail.len() > EXCERPT_BYTES - 4);
        assert!(text.starts_with(head) && text.ends_with(tail));
        let omitted = text.len() - head.len() - tail.len();
        assert_eq!(
            marker,
            format!("[... {omitted} bytes omitted; full output in .kageha/tool-output/c-stdout.txt ...]")
        );
    }

    #[test]
    fn call_id_must_be_a_plain_file_name_part() {
        for good in ["call_1", "toolu_01AbC-x.y", &"a".repeat(MAX_CALL_ID)] {
            assert!(valid_call_id(good), "{good}");
        }
        for bad in ["", ".hidden", "a/b", "../x", "a b", "a\n", &"a".repeat(MAX_CALL_ID + 1)] {
            assert!(!valid_call_id(bad), "{bad:?}");
        }
    }

    #[test]
    fn sandboxed_framing_exit_codes_partial_lines_and_spill() {
        if !kageha_sandbox::available() {
            eprintln!("skipping: OS process sandbox unavailable");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let task = TaskWorkspace::open(&dir.path().join("chat")).unwrap();
        let paths = ShellPaths::prepare(task.root(), &dir.path().join("toolchains")).unwrap();
        let mut shell = ShellSession::open(paths.clone()).unwrap();
        let t = Duration::from_secs(30);

        for (command, code) in [("true", 0), ("false", 1), ("kg_no_such_command_x", 127)] {
            let out = shell.run(command, "codes", t).unwrap();
            assert_eq!(out.exit_code, code, "{command}: {out:?}");
        }

        // A fake sentinel (guessed nonce) on stdout and stderr does not end the command.
        let fake = format!("__KG_END_{}_0", "a".repeat(32));
        let out = shell
            .run(&format!("echo {fake}; echo {fake} >&2; echo real; exit_with() {{ return 3; }}; exit_with"), "fake", t)
            .unwrap();
        assert_eq!(out.exit_code, 3);
        assert_eq!(out.stdout, format!("{fake}\nreal\n"));
        assert_eq!(out.stderr, format!("{fake}\n"));

        // Output without a trailing newline comes back unchanged, and the session continues.
        let out = shell.run("printf 'no newline'; printf 'err' >&2", "partial", t).unwrap();
        assert_eq!((out.stdout.as_str(), out.stderr.as_str()), ("no newline", "err"));
        assert_eq!(shell.run("echo next", "next", t).unwrap().stdout, "next\n");

        // Duration covers the command.
        let out = shell.run("sleep 0.3", "sleep", t).unwrap();
        assert!(out.duration_ms >= 300 && out.duration_ms < 10_000, "{out:?}");

        // Large output spills; the file holds exactly the full stream.
        let out = shell
            .run("i=0; while [ $i -lt 6000 ]; do echo \"line $i\"; echo \"e$i\" >&2; i=$((i+1)); done", "big_1", t)
            .unwrap();
        let expected_out: String = (0..6000).map(|i| format!("line {i}\n")).collect();
        let expected_err: String = (0..6000).map(|i| format!("e{i}\n")).collect();
        assert!(expected_out.len() > SPILL_THRESHOLD && expected_err.len() > SPILL_THRESHOLD);
        assert_eq!(out.exit_code, 0);
        assert!(out.stdout_truncated && out.stderr_truncated);
        assert_eq!(out.stdout_path.as_deref(), Some(".kageha/tool-output/big_1-stdout.txt"));
        assert_eq!(out.stderr_path.as_deref(), Some(".kageha/tool-output/big_1-stderr.txt"));
        let root = task.root();
        assert_eq!(fs::read(root.join(".kageha/tool-output/big_1-stdout.txt")).unwrap(), expected_out.as_bytes());
        assert_eq!(fs::read(root.join(".kageha/tool-output/big_1-stderr.txt")).unwrap(), expected_err.as_bytes());
        assert!(out.stdout.len() < SPILL_THRESHOLD + 200);
        assert!(out.stdout.starts_with("line 0\nline 1\n"));
        assert!(out.stdout.ends_with("line 5999\n"));
        assert!(out.stdout.contains("full output in .kageha/tool-output/big_1-stdout.txt"));
        // Spill files are Host-owned: not reported as workspace changes.
        assert!(task
            .scan_changes()
            .unwrap()
            .iter()
            .all(|c| !c.path.starts_with(".kageha/")));

        // Exactly 32 KiB is not spilled.
        let out = shell.run("printf '%32768s' '' | tr ' ' x", "edge", t).unwrap();
        assert_eq!((out.stdout.len(), out.stdout_truncated), (SPILL_THRESHOLD, false));
        assert!(shell.run("echo x", "../bad", t).is_err());
        assert!(shell.broken().is_none());
        shell.close();
    }

    #[test]
    fn service_rejects_bad_requests() {
        let dir = tempfile::tempdir().unwrap();
        let service = ShellService::new(dir.path().join("tc"));
        let reply: Value = serde_json::from_str(&service.handle_line("{\"op\":\"nope\"}")).unwrap();
        assert_eq!(reply["ok"], false);
        let reply: Value = serde_json::from_str(
            &service.handle_line(r#"{"op":"run","chat":"c","command":"true","call_id":"c"}"#),
        )
        .unwrap();
        assert_eq!(reply["error"], "No session for chat");
        let reply: Value = serde_json::from_str(&service.handle_line(
            r#"{"op":"open_session","chat":"../x","workspace":"/tmp"}"#,
        ))
        .unwrap();
        assert_eq!(reply["error"], "Invalid chat id");
    }

    #[test]
    fn pip_installs_into_the_shared_toolchain_venv() {
        if !kageha_sandbox::available() || !PYTHON_CANDIDATES.iter().any(|p| Path::new(p).is_file()) {
            eprintln!("skipping: OS process sandbox or python3 unavailable");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let paths = ShellPaths::prepare(&workspace, &dir.path().join("toolchains")).unwrap();
        let venv = paths.toolchain_cache.join(PYTHON_VENV);
        // Idempotent: a second prepare keeps the ready venv.
        assert_eq!(ensure_python_venv(&paths.toolchain_cache).unwrap(), venv);
        let mut shell = ShellSession::open(paths.clone()).unwrap();
        let t = Duration::from_secs(120);
        let version = shell.run("pip --version", "call", t).unwrap();
        assert_eq!(version.exit_code, 0, "{version:?}");
        assert!(version.stdout.contains(venv.to_str().unwrap()), "{version:?}");
        let prefix = shell.run("python -c 'import sys; print(sys.prefix)'", "call", t).unwrap();
        assert_eq!(prefix.stdout.trim_end(), venv.to_str().unwrap(), "{prefix:?}");
        // A tiny pure wheel built offline in the workspace, installed with no index.
        let build = "python - <<'EOF'\nimport zipfile\nd='kgprobe-0.1.dist-info'\nwith zipfile.ZipFile('kgprobe-0.1-py3-none-any.whl','w') as z:\n    z.writestr('kgprobe.py','VALUE = 42\\n')\n    z.writestr(d+'/METADATA','Metadata-Version: 2.1\\nName: kgprobe\\nVersion: 0.1\\n')\n    z.writestr(d+'/WHEEL','Wheel-Version: 1.0\\nGenerator: test\\nRoot-Is-Purelib: true\\nTag: py3-none-any\\n')\n    z.writestr(d+'/RECORD','')\nEOF";
        let built = shell.run(build, "call", t).unwrap();
        assert_eq!(built.exit_code, 0, "{built:?}");
        let install = shell.run("pip install --no-index ./kgprobe-0.1-py3-none-any.whl", "call", t).unwrap();
        assert_eq!(install.exit_code, 0, "{install:?}");
        let location = shell.run("python -c 'import kgprobe; print(kgprobe.__file__, kgprobe.VALUE)'", "call", t).unwrap();
        assert!(location.stdout.starts_with(venv.to_str().unwrap()), "{location:?}");
        assert!(location.stdout.trim_end().ends_with(" 42"), "{location:?}");
        assert!(!fs::read_dir(&workspace).unwrap().any(|e| {
            let name = e.unwrap().file_name();
            name == "venv" || name == "lib" || name == "kgprobe.py"
        }));
    }

    #[test]
    fn sandboxed_session_runs_in_workspace_and_denies_outside_writes() {
        if !kageha_sandbox::available() {
            eprintln!("skipping: OS process sandbox unavailable");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        fs::create_dir_all(workspace.join("notes")).unwrap();
        let paths = ShellPaths::prepare(&workspace, &dir.path().join("toolchains")).unwrap();
        let mut shell = ShellSession::open(paths.clone()).unwrap();
        let t = Duration::from_secs(20);

        let hi = shell.run("echo hi; echo err >&2", "call", t).unwrap();
        assert_eq!((hi.exit_code, hi.stdout.as_str(), hi.stderr.as_str()), (0, "hi\n", "err\n"));
        assert!(!hi.stdout_truncated && !hi.stderr_truncated && hi.stdout_path.is_none());
        let pwd = shell.run("pwd", "call", t).unwrap();
        assert_eq!(pwd.stdout.trim_end(), paths.workspace.to_str().unwrap());

        // Session state carries over between calls.
        shell.run("cd notes && export KG_PROBE=1", "call", t).unwrap();
        let state = shell.run("pwd; echo $KG_PROBE; exit_code_test() { return 7; }; exit_code_test", "call", t).unwrap();
        assert_eq!(state.exit_code, 7);
        assert_eq!(
            state.stdout,
            format!("{}\n1\n", paths.workspace.join("notes").display())
        );
        shell.run("cd \"$KAGEHA_WORKSPACE\"", "call", t).unwrap();

        // Toolchain_Cache, TMPDIR and the workspace are writable.
        let writes = shell
            .run("mkdir -p \"$CARGO_HOME\" && echo ok > \"$CARGO_HOME/probe\" && echo t > \"$TMPDIR/t\" && echo w > out.txt", "call", t)
            .unwrap();
        assert_eq!(writes.exit_code, 0, "{writes:?}");
        assert!(paths.toolchain_cache.join("cargo/probe").exists());
        assert!(paths.workspace.join("out.txt").exists());

        // Writing outside the write roots fails.
        let target = outside.path().join("escape");
        let denied = shell
            .run(&format!("printf x > {}", single_quoted(target.to_str().unwrap())), "call", t)
            .unwrap();
        assert_ne!(denied.exit_code, 0);
        assert!(!target.exists());

        // No secret-named variable reaches the shell.
        let env = shell.run("env", "call", t).unwrap();
        for line in env.stdout.lines() {
            let name = line.split('=').next().unwrap_or_default();
            assert!(!is_secret_name(name), "{line}");
        }

        // Syntax errors fail the command without hanging the session.
        let syntax = shell.run("if then", "call", t).unwrap();
        assert_ne!(syntax.exit_code, 0);
        assert_eq!(shell.run("echo after", "call", t).unwrap().stdout, "after\n");
        shell.close();
    }

    /// A session in `<dir>/chat/workspace` that also reads `<dir>/parent` without its
    /// `.kageha/`, next to a sibling `<dir>/chat/uploads` it can't list or read.
    fn framework_fixture(dir: &Path) -> (ShellPaths, PathBuf) {
        let parent = dir.join("parent");
        fs::create_dir_all(parent.join(".kageha")).unwrap();
        fs::write(parent.join("brief.md"), "brief").unwrap();
        fs::write(parent.join(".kageha/secret"), "secret").unwrap();
        fs::create_dir_all(dir.join("chat/uploads")).unwrap();
        fs::write(dir.join("chat/uploads/note"), "note").unwrap();
        let workspace = dir.join("chat/workspace");
        fs::create_dir_all(&workspace).unwrap();
        let mut paths = ShellPaths::prepare(&workspace, &dir.join("toolchains")).unwrap();
        let parent = parent.canonicalize().unwrap();
        paths.extra_read_roots = vec![parent.clone()];
        paths.hidden_roots = vec![parent.join(".kageha")];
        (paths, parent)
    }

    #[test]
    fn sandboxed_session_runs_heredocs_process_substitution_and_lists_its_directory() {
        if !kageha_sandbox::available() {
            eprintln!("skipping: OS process sandbox unavailable");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let (paths, parent) = framework_fixture(dir.path());
        let mut shell = ShellSession::open(paths.clone()).unwrap();
        let t = Duration::from_secs(30);
        let mut run = |command: &str| shell.run(command, "call", t).unwrap();
        let ok = |out: CommandOutput, command: &str| {
            assert_eq!(out.exit_code, 0, "{command}: {out:?}");
            out.stdout
        };
        let bash = if Path::new("/bin/bash").exists() { "/bin/bash" } else { "bash" };
        let zsh = Path::new("/bin/zsh").exists();
        // Here-documents in the session shell, in bash and (when present) in zsh.
        let mut cases = vec![
            ("cat <<'EOF'\nsession\nEOF".to_string(), "session\n".to_string()),
            (format!("{bash} -c 'cat <<EOF\nbash\nEOF'"), "bash\n".into()),
            // Process substitution through /dev/fd, and the other device files.
            ("diff <(echo a) <(echo a) && echo same".into(), "same\n".into()),
            (format!("{bash} -c 'diff <(echo a) <(echo a) && echo same'"), "same\n".into()),
            (
                "echo x > /dev/null && for d in urandom random zero; do head -c 4 /dev/$d | wc -c | tr -d ' '; done && echo in | cat /dev/stdin && echo err > /dev/stderr".into(),
                "4\n4\n4\nin\n".into(),
            ),
        ];
        if zsh {
            cases.push(("/bin/zsh -f -c 'cat <<EOF\nzsh\nEOF'".into(), "zsh\n".into()));
            // zsh's here-document files go to the per-chat temp directory.
            cases.push(("echo \"$TMPPREFIX\"".into(), format!("{}\n", paths.temp_dir.join("zsh").display())));
        }
        for (command, expected) in &cases {
            assert_eq!(&ok(run(command), command), expected, "{command}");
        }

        // The working directory and its parent list; nothing below the parent opens.
        ok(run("touch listed.txt"), "touch");
        assert!(ok(run("ls ."), "ls .").contains("listed.txt"));
        assert!(ok(run("find . -maxdepth 1"), "find").contains("./listed.txt"));
        ok(run("ls . && find . -maxdepth 1 && ls .."), "ls . && find . && ls ..");
        ok(run("ls -ld .. ../.. && stat .. > /dev/null"), "ls -ld ..");
        for command in ["cat ../uploads/note", "ls ../uploads"] {
            let out = run(command);
            assert_ne!(out.exit_code, 0, "{command}: {out:?}");
            assert!(!out.stdout.contains("note"), "{command}: {out:?}");
        }

        // Granted reads work; hidden roots stay hidden; writes stay in the write roots.
        let p = single_quoted(parent.to_str().unwrap());
        assert_eq!(ok(run(&format!("cat {p}/brief.md")), "brief"), "brief");
        for command in [
            format!("cat {p}/.kageha/secret"),
            format!("printf x > {p}/new"),
            format!("printf x > {}", single_quoted(outside.path().join("escape").to_str().unwrap())),
        ] {
            let out = run(&command);
            assert_ne!(out.exit_code, 0, "{command}: {out:?}");
            assert!(out.stdout.is_empty(), "{command}: {out:?}");
        }
        // Bubblewrap hides the files behind an empty mount. Seatbelt denies the listing.
        let hidden = run(&format!("ls {p}/.kageha"));
        assert!(hidden.stdout.is_empty(), "{hidden:?}");
        #[cfg(target_os = "macos")]
        assert_ne!(hidden.exit_code, 0, "{hidden:?}");

        let adjacent = run("printf x > ../escape");
        assert!(adjacent.stdout.is_empty(), "{adjacent:?}");
        #[cfg(target_os = "macos")]
        assert_ne!(adjacent.exit_code, 0, "{adjacent:?}");
        assert!(!parent.join("new").exists() && !outside.path().join("escape").exists());
        assert!(!dir.path().join("chat/escape").exists());
        shell.close();
    }

    /// Text recognition with Vision in a Swift script that draws its own image.
    const VISION_SCRIPT: &str = r#"
import CoreGraphics
import CoreText
import Foundation
import Vision
let context = CGContext(data: nil, width: 640, height: 160, bitsPerComponent: 8, bytesPerRow: 0,
    space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
context.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 1))
context.fill(CGRect(x: 0, y: 0, width: 640, height: 160))
let text = NSAttributedString(string: "KAGEHA 42", attributes: [
    NSAttributedString.Key(kCTFontAttributeName as String): CTFontCreateWithName("Helvetica" as CFString, 72, nil),
    NSAttributedString.Key(kCTForegroundColorAttributeName as String): CGColor(red: 0, green: 0, blue: 0, alpha: 1),
])
context.textPosition = CGPoint(x: 40, y: 50)
CTLineDraw(CTLineCreateWithAttributedString(text), context)
let request = VNRecognizeTextRequest()
try VNImageRequestHandler(cgImage: context.makeImage()!, options: [:]).perform([request])
print((request.results ?? []).compactMap { $0.topCandidates(1).first?.string }.joined(separator: " "))
"#;

    #[test]
    #[cfg(target_os = "macos")]
    fn sandboxed_session_runs_macos_framework_tools() {
        if !kageha_sandbox::available() {
            eprintln!("skipping: OS process sandbox unavailable");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let (paths, _) = framework_fixture(dir.path());
        let mut shell = ShellSession::open(paths.clone()).unwrap();
        let mut run = |command: &str, timeout: u64| {
            let out = shell.run(command, "call", Duration::from_secs(timeout)).unwrap();
            assert_eq!(out.exit_code, 0, "{command}: {out:?}");
            out
        };
        assert_eq!(run("osascript -e 'return 1'", 60).stdout, "1\n");
        run("sips --help > /dev/null", 60);
        if Path::new("/usr/bin/swift").exists() && Command::new("/usr/bin/xcrun").args(["--find", "swift"]).output().is_ok_and(|o| o.status.success()) {
            fs::write(paths.workspace.join("ocr.swift"), VISION_SCRIPT).unwrap();
            let out = run("swiftc -O ocr.swift -o ocr 2>&1 && ./ocr", 600);
            assert!(out.stdout.contains("KAGEHA"), "{out:?}");
            // Framework caches land in the per-chat HOME.
            assert!(paths.home.join("Library/Caches").is_dir());
        } else {
            eprintln!("skipping the Vision part: swift unavailable");
        }
        shell.close();
    }

    #[test]
    fn service_round_trip_over_socket() {
        if !kageha_sandbox::available() {
            eprintln!("skipping: OS process sandbox unavailable");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let workspace = TaskWorkspace::open(&dir.path().join("chat"))
            .unwrap()
            .root()
            .to_path_buf();
        let service = Arc::new(ShellService::new(dir.path().join("tc")));
        let (client, server) = UnixStream::pair().unwrap();
        let worker = {
            let service = service.clone();
            std::thread::spawn(move || service.serve_connection(server))
        };
        let mut writer = client.try_clone().unwrap();
        let mut reader = BufReader::new(client);
        let mut ask = |request: Value| -> Value {
            writeln!(writer, "{request}").unwrap();
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            serde_json::from_str(&line).unwrap()
        };
        let open = ask(json!({"op":"open_session","chat":"c1","workspace":workspace}));
        assert_eq!(open["ok"], true, "{open}");
        let again = ask(json!({"op":"open_session","chat":"c1","workspace":workspace}));
        assert_eq!(again["reused"], true, "{again}");
        let run = ask(json!({"op":"run","chat":"c1","command":"echo hi","call_id":"c_1","timeout_s":20}));
        assert_eq!((run["exit_code"].clone(), run["stdout"].clone()), (json!(0), json!("hi\n")));
        assert_eq!(run["stdout_truncated"], false, "{run}");
        assert_eq!(run["stdout_path"], Value::Null);
        assert!(run["duration_ms"].is_u64());
        let big = ask(json!({
            "op":"run","chat":"c1","call_id":"c_2","timeout_s":20,
            "command":"printf '%40000s' '' | tr ' ' y"
        }));
        assert_eq!(big["stdout_truncated"], true, "{big}");
        assert_eq!(big["stdout_path"], ".kageha/tool-output/c_2-stdout.txt");
        assert_eq!(big["stderr_truncated"], false);
        let spilled = fs::read(workspace.join(".kageha/tool-output/c_2-stdout.txt")).unwrap();
        assert_eq!(spilled, vec![b'y'; 40000]);
        let missing = ask(json!({"op":"run","chat":"c1","command":"true"}));
        assert_eq!(missing["ok"], false, "{missing}");
        // Jobs over the socket: start, read, stop.
        let job = ask(json!({"op":"start_job","chat":"c1","command":"echo bg; sleep 30"}));
        assert_eq!(job["job"]["state"], "running", "{job}");
        let id = job["job"]["job_id"].as_str().unwrap().to_owned();
        let mut output = Value::Null;
        for _ in 0..200 {
            output = ask(json!({"op":"job_output","chat":"c1","job_id":id,"offset":0}));
            if output["output"]["output"] == "bg\n" {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(output["output"]["output"], "bg\n", "{output}");
        assert_eq!(output["output"]["next_offset"], 3);
        let stopped = ask(json!({"op":"stop_job","chat":"c1","job_id":id}));
        assert_eq!(stopped["job"]["state"], "killed", "{stopped}");
        let status = ask(json!({"op":"job_status","chat":"c2","job_id":id}));
        assert_eq!(status["ok"], false, "{status}");
        // A run past its timeout continues as a job.
        let slow = ask(json!({"op":"run","chat":"c1","command":"sleep 30","call_id":"c_3","timeout_s":0}));
        assert_eq!(slow["timed_out"], true, "{slow}");
        let slow_id = slow["job"]["job_id"].as_str().unwrap().to_owned();
        let stopped = ask(json!({"op":"stop_job","chat":"c1","job_id":slow_id}));
        assert_eq!(stopped["job"]["state"], "killed", "{stopped}");
        // That session became the job; the chat opens a new one.
        let reopened = ask(json!({"op":"open_session","chat":"c1","workspace":workspace}));
        assert_eq!(reopened["reused"], false, "{reopened}");
        let close = ask(json!({"op":"close","chat":"c1"}));
        assert_eq!(close["closed"], true);
        drop(writer);
        drop(reader);
        worker.join().unwrap().unwrap();
    }
    #[test]
    fn service_ping_session_dir_run_once_and_cancel() {
        let dir = tempfile::tempdir().unwrap();
        let service = Arc::new(ShellService::new(dir.path().join("tc")));
        let ask = |request: Value| -> Value { serde_json::from_str(&service.handle_line(&request.to_string())).unwrap() };
        let ping = ask(json!({"op":"ping"}));
        assert_eq!((ping["ok"].clone(), ping["pid"].clone()), (json!(true), json!(std::process::id())), "{ping}");
        assert_eq!(ask(json!({"op":"session_dir","chat":"c1"}))["dir"], Value::Null);
        assert_eq!(ask(json!({"op":"cancel","chat":"c1","call_id":"none"}))["cancelled"], false);
        if !kageha_sandbox::available() {
            eprintln!("skipping the shell part: OS process sandbox unavailable");
            return;
        }
        let workspace = TaskWorkspace::open(&dir.path().join("chat")).unwrap().root().to_path_buf();
        assert_eq!(ask(json!({"op":"open_session","chat":"c1","workspace":workspace}))["ok"], true);
        assert_eq!(ask(json!({"op":"run","chat":"c1","command":"mkdir -p sub && cd sub","call_id":"c_1","timeout_s":20}))["exit_code"], 0);
        let sub = workspace.join("sub").canonicalize().unwrap();
        let session_dir = |ask: &dyn Fn(Value) -> Value| {
            ask(json!({"op":"session_dir","chat":"c1"}))["dir"].as_str().map(|dir| Path::new(dir).canonicalize().unwrap())
        };
        assert_eq!(session_dir(&ask), Some(sub.clone()));
        // run_once: a new shell in `cwd` with an extra write root; the session is untouched.
        let extra = tempfile::tempdir().unwrap();
        let extra_root = extra.path().canonicalize().unwrap();
        let once = ask(json!({"op":"run_once","chat":"c1","workspace":workspace,"cwd":workspace.join("sub"),
            "write_roots":[extra_root],"command":format!("pwd; echo x > {}/granted; cd /", single_quoted(extra_root.to_str().unwrap())),
            "call_id":"c_2","timeout_s":20}));
        assert_eq!(once["exit_code"], 0, "{once}");
        assert_eq!(Path::new(once["stdout"].as_str().unwrap().trim_end()).canonicalize().unwrap(), sub);
        assert!(extra_root.join("granted").exists());
        assert_eq!(session_dir(&ask), Some(sub.clone()));
        let outside = ask(json!({"op":"run_once","chat":"c1","workspace":workspace,"cwd":"/","command":"true","call_id":"c_3"}));
        assert_eq!(outside["ok"], false, "{outside}");
        // A cancel from another connection makes a running command continue as a job.
        let running = {
            let service = service.clone();
            std::thread::spawn(move || service.handle_line(&json!({"op":"run","chat":"c1","command":"sleep 30","call_id":"c_4","timeout_s":60}).to_string()))
        };
        let mut cancelled = false;
        for _ in 0..200 {
            if ask(json!({"op":"cancel","chat":"c1","call_id":"c_4"}))["cancelled"] == true {
                cancelled = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(cancelled);
        let reply: Value = serde_json::from_str(&running.join().unwrap()).unwrap();
        assert_eq!((reply["timed_out"].clone(), reply["cancelled"].clone()), (json!(true), json!(true)), "{reply}");
        let job = reply["job"]["job_id"].as_str().unwrap();
        assert_eq!(ask(json!({"op":"stop_job","chat":"c1","job_id":job}))["job"]["state"], "killed");
        // The flag is removed once the request ends.
        assert_eq!(ask(json!({"op":"cancel","chat":"c1","call_id":"c_4"}))["cancelled"], false);
    }
}
