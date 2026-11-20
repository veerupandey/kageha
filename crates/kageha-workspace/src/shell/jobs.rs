//! Background shell jobs (Requirement 2.2) and [`ShellManager`], which holds one shell
//! session per chat plus that chat's jobs.
//!
//! A job is a command that runs apart from the chat's persistent session. There are two
//! kinds:
//!
//! - **Background jobs** ([`ShellManager::start_job`]) start in a new session (`setsid`),
//!   under the same [`ProcessSandbox`](kageha_sandbox::ProcessSandbox) and environment as the
//!   chat's shell, in the shell's current directory. A wrapper runs the command in a
//!   subshell (so `exit` in the command still records a status) and writes its exit status
//!   to `.kageha/jobs/<id>.exit`. stdout and stderr both go to `.kageha/jobs/<id>.log`.
//! - **Timed-out foreground commands** ([`ShellManager::run`]). Every foreground command
//!   already writes its output to job files (see the framing in [`super`]). When it is still
//!   running at its timeout, the session that runs it is detached and becomes the job: its
//!   stdout is `<id>.log`, its stderr `<id>.err`, and the session shell writes `<id>.exit`
//!   when the command ends. It is never killed silently. The chat's next command gets a new
//!   session.
//!
//! State: a job is `running` while its leader process is alive. Once the leader is gone it
//! is `exited` with the status from `<id>.exit`, or `killed` when there is none. The exit
//! file is read only after the leader is gone, so a command cannot end its own job early by
//! writing a fake one. The final state is cached.
//!
//! Limits: a job may run for [`MAX_JOB_RUNTIME`] (24 hours) from its start. Then its process
//! group gets SIGTERM, and SIGKILL [`KILL_GRACE`] (10 s) later if any member remains. A
//! watchdog thread per job enforces this and reaps owned children. `stop` uses the same
//! sequence.
//!
//! Re-adoption: a [`JobRecord`] stores the leader's PID and its process start time from
//! [`process_identity`] (`proc_pidinfo` on macOS, `/proc` on Linux). A later host process
//! adopts the job with [`ShellManager::adopt_job`] only when both still match, so a reused
//! PID is never mistaken for the job.
//!
//! Trade-offs of this design:
//!
//! - Short foreground commands keep full session semantics (`cd`, `export`, activated
//!   environments). A command that outlives its timeout takes its session with it: that
//!   session becomes the job, and the next command gets a new session that starts from the
//!   state saved after the last finished command (directory and non-secret exported
//!   variables, see [`super::state`]). Shell functions, aliases and unexported variables
//!   are not carried over.
//! - A background job inherits the session's working directory, but not variables exported
//!   in the session (it is a new process, not a child of the session shell).
//! - Processes that leave the job's process group (for example by calling `setsid`) are not
//!   tracked or stopped.
//! - On Linux, `bwrap --die-with-parent` ends jobs when the process hosting the
//!   [`ShellManager`] exits. They survive a main-server restart only when the manager runs in
//!   the separate shell service.
//! - When a job is re-adopted, its 24-hour limit counts from the leader's process start. For
//!   a timed-out foreground command that is when its session started, which can be earlier
//!   than the command.
use super::{
    host_environment, new_nonce, shell_environment, shell_program, single_quoted, valid_chat,
    CommandOutput, RunStep, ShellPaths, ShellSession, EXCERPT_BYTES,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    ffi::CString,
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{fs::OpenOptionsExt, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{atomic::AtomicBool, Arc, Mutex, MutexGuard, TryLockError},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Workspace-relative directory of job logs and exit files.
pub const JOBS_DIR: &str = ".kageha/jobs";
/// Longest a job may run before it is terminated (Requirement 2.2).
pub const MAX_JOB_RUNTIME: Duration = Duration::from_secs(24 * 60 * 60);
/// Time between SIGTERM and SIGKILL.
pub const KILL_GRACE: Duration = Duration::from_secs(10);
/// Largest output window returned by [`ShellManager::job_output`].
pub const JOB_OUTPUT_WINDOW: usize = 32 * 1024;
/// How long `stop` waits for the job to end before returning its state.
const STOP_WAIT: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(25);
const WATCH_INTERVAL: Duration = Duration::from_millis(500);
const EXIT_FILE_LIMIT: u64 = 32;
const MIN_WINDOW: usize = 16;

/// Runs `$1` in a subshell with stdin `/dev/null`, then writes its status to the file `$2`.
/// Positional parameters are cleared before the command runs.
const WRAPPER: &str = "__kg_exit=$2; ( __kg_cmd=$1; set --; unset __kg_exit; eval \"$__kg_cmd\" ) < /dev/null; __kg_rc=$?; printf '%d\\n' \"$__kg_rc\" > \"$__kg_exit\"";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Running,
    Exited,
    Killed,
}

impl JobState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Exited => "exited",
            Self::Killed => "killed",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "running" => Some(Self::Running),
            "exited" => Some(Self::Exited),
            "killed" => Some(Self::Killed),
            _ => None,
        }
    }
}

/// What a host stores to find a job again after a restart.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobRecord {
    pub job_id: String,
    pub chat: String,
    pub command: String,
    /// Leader process: the job's process group and session (background) or process group
    /// (timed-out foreground) have this ID.
    pub pid: u32,
    /// Start time of process `pid` in nanoseconds since the Unix epoch, as read by
    /// [`process_identity`]. 0 when it could not be read; such a job cannot be re-adopted.
    pub started_ns: u64,
    /// Workspace-relative log: stdout and stderr of a background job, stdout of a timed-out
    /// foreground command.
    pub log_path: String,
    /// Workspace-relative stderr of a timed-out foreground command.
    #[serde(default)]
    pub stderr_path: Option<String>,
}

impl JobRecord {
    /// Workspace-relative paths of `job_id`'s log and, for a foreground command, stderr.
    pub fn paths_for(job_id: &str, foreground: bool) -> (String, Option<String>) {
        let files = JobFiles::new(job_id);
        (
            format!("{JOBS_DIR}/{}", files.log),
            foreground.then(|| format!("{JOBS_DIR}/{}", files.err)),
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobStatus {
    #[serde(flatten)]
    pub record: JobRecord,
    pub state: JobState,
    /// Set when `state` is `exited`.
    pub exit_code: Option<i32>,
}

/// One window of a job's log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobOutput {
    pub job_id: String,
    pub state: JobState,
    pub exit_code: Option<i32>,
    pub offset: u64,
    /// Where the next window starts. Never splits a UTF-8 character of a running job.
    pub next_offset: u64,
    /// Log size in bytes when it was read.
    pub size: u64,
    pub output: String,
    /// The job has ended and this window reaches the end of its log.
    pub complete: bool,
    pub stderr_path: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JobLimits {
    pub max_runtime: Duration,
    pub kill_grace: Duration,
}

impl Default for JobLimits {
    fn default() -> Self {
        Self {
            max_runtime: MAX_JOB_RUNTIME,
            kill_grace: KILL_GRACE,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub pid: u32,
    pub reused: bool,
    /// A new session started from the state saved in `.kageha/shell-env`.
    #[serde(default)]
    pub restored: bool,
}

/// Result of [`ShellManager::run`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Foreground {
    Finished(CommandOutput),
    /// The command was still running at its timeout (or when `cancel` was set) and
    /// continues as `job`.
    Detached { job: JobStatus, cancelled: bool },
}

/// Start time and zombie state of a live process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessIdentity {
    /// Nanoseconds since the Unix epoch.
    pub start_ns: u64,
    pub zombie: bool,
}

/// The start time of process `pid`, or `None` if there is no such process.
pub fn process_identity(pid: u32) -> Option<ProcessIdentity> {
    if pid == 0 || pid > i32::MAX as u32 {
        return None;
    }
    #[cfg(target_os = "macos")]
    {
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
        let read = unsafe {
            libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDTBSDINFO,
                0,
                &mut info as *mut libc::proc_bsdinfo as *mut libc::c_void,
                size,
            )
        };
        if read != size {
            return None;
        }
        Some(ProcessIdentity {
            start_ns: info
                .pbi_start_tvsec
                .saturating_mul(1_000_000_000)
                .saturating_add(info.pbi_start_tvusec.saturating_mul(1_000)),
            zombie: info.pbi_status == libc::SZOMB,
        })
    }
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // Fields after the parenthesized command name, which may contain spaces.
        let fields: Vec<&str> = stat[stat.rfind(')')? + 1..].split_whitespace().collect();
        let state = *fields.first()?;
        let ticks: u64 = fields.get(19)?.parse().ok()?;
        let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        let hz = u64::try_from(hz).ok().filter(|hz| *hz > 0)?;
        let boot: u64 = std::fs::read_to_string("/proc/stat")
            .ok()?
            .lines()
            .find_map(|line| line.strip_prefix("btime "))?
            .trim()
            .parse()
            .ok()?;
        Some(ProcessIdentity {
            start_ns: boot
                .saturating_mul(1_000_000_000)
                .saturating_add(ticks.saturating_mul(1_000_000_000) / hz),
            zombie: matches!(state, "Z" | "X"),
        })
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

/// True when `pid` is a live (non-zombie) process that started at `started_ns`.
fn same_process(pid: u32, started_ns: u64) -> bool {
    started_ns != 0
        && process_identity(pid).is_some_and(|p| p.start_ns == started_ns && !p.zombie)
}

/// True while any process is in process group `pgid`.
fn group_exists(pgid: u32) -> bool {
    if pgid <= 1 || pgid > i32::MAX as u32 {
        return false;
    }
    let sent = unsafe { libc::kill(-(pgid as i32), 0) } == 0;
    sent || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// `job-` and 16 random lowercase hex digits.
pub(crate) fn new_job_id() -> io::Result<String> {
    Ok(format!("job-{}", &new_nonce()?[..16]))
}

pub(crate) fn valid_job_id(id: &str) -> bool {
    id.strip_prefix("job-").is_some_and(|hex| {
        hex.len() == 16 && hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// File names of one job inside [`JOBS_DIR`].
pub(crate) struct JobFiles {
    pub log: String,
    pub err: String,
    pub exit: String,
}

impl JobFiles {
    pub(crate) fn new(job_id: &str) -> Self {
        Self {
            log: format!("{job_id}.log"),
            err: format!("{job_id}.err"),
            exit: format!("{job_id}.exit"),
        }
    }

    /// Removes the job's files except the names in `keep`. Best effort.
    pub(crate) fn remove_except(&self, dir: &File, keep: &[&str]) {
        for name in [&self.log, &self.err, &self.exit] {
            if !keep.contains(&name.as_str()) {
                if let Ok(name) = CString::new(name.as_str()) {
                    unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0) };
                }
            }
        }
    }
}

fn cstring(name: &str) -> io::Result<CString> {
    CString::new(name).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in file name"))
}

fn open_at(dir: &File, name: &str, flags: libc::c_int, mode: libc::c_uint) -> io::Result<File> {
    let name = cstring(name)?;
    let fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags | libc::O_CLOEXEC, mode) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn sub_dir(parent: &File, name: &str, create: bool) -> io::Result<File> {
    if create {
        let c_name = cstring(name)?;
        if unsafe { libc::mkdirat(parent.as_raw_fd(), c_name.as_ptr(), 0o755 as libc::mode_t) } != 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::AlreadyExists {
                return Err(error);
            }
        }
    }
    open_at(parent, name, libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW, 0)
}

/// `<state>/jobs` (the workspace's `.kageha/jobs`), opened from the state directory
/// without following symlinks, so a symlink the sandboxed shell swapped in cannot redirect
/// the Host's reads or writes.
pub(crate) fn open_jobs_dir(state: &Path, create: bool) -> io::Result<File> {
    let state = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(state)?;
    sub_dir(&state, "jobs", create)
}

/// Opens a regular file in `dir` without following symlinks. `None` if it does not exist.
fn open_regular(dir: &File, name: &str) -> io::Result<Option<File>> {
    match open_at(dir, name, libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK, 0) {
        Ok(file) if file.metadata()?.is_file() => Ok(Some(file)),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{JOBS_DIR}/{name} is not a regular file"),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub(crate) enum JobFile {
    Missing,
    Bytes(Vec<u8>),
    /// Over the limit: the first and last [`EXCERPT_BYTES`] and the size.
    Large { head: Vec<u8>, tail: Vec<u8>, size: u64 },
}

/// Reads a job file of at most `limit` bytes, or its head and tail when it is larger.
pub(crate) fn read_job_file(dir: &File, name: &str, limit: usize) -> io::Result<JobFile> {
    let Some(mut file) = open_regular(dir, name)? else {
        return Ok(JobFile::Missing);
    };
    let size = file.metadata()?.len();
    if size <= limit as u64 {
        let mut bytes = Vec::with_capacity(size as usize);
        (&mut file).take(limit as u64).read_to_end(&mut bytes)?;
        return Ok(JobFile::Bytes(bytes));
    }
    let mut head = Vec::with_capacity(EXCERPT_BYTES);
    (&mut file).take(EXCERPT_BYTES as u64).read_to_end(&mut head)?;
    file.seek(SeekFrom::Start(size - EXCERPT_BYTES as u64))?;
    let mut tail = Vec::with_capacity(EXCERPT_BYTES);
    (&mut file).take(EXCERPT_BYTES as u64).read_to_end(&mut tail)?;
    Ok(JobFile::Large { head, tail, size })
}

fn read_exit(dir: &File, name: &str) -> Option<i32> {
    let mut text = String::new();
    open_regular(dir, name)
        .ok()??
        .take(EXIT_FILE_LIMIT)
        .read_to_string(&mut text)
        .ok()?;
    text.trim().parse().ok()
}

/// Bytes of `bytes` to return now: all of them, except that an incomplete UTF-8 character
/// at the end is held back for the next window unless the log is complete.
fn utf8_prefix(bytes: &[u8], at_end: bool) -> usize {
    match std::str::from_utf8(bytes) {
        Err(error) if error.error_len().is_none() && !at_end => error.valid_up_to(),
        _ => bytes.len(),
    }
}

enum Process {
    /// A background job this process started.
    Child(Child),
    /// A detached session running a timed-out foreground command.
    Session(Arc<Mutex<ShellSession>>),
    /// Started by an earlier host process; identified by PID and start time.
    Adopted,
    Gone,
}

struct Job {
    record: JobRecord,
    /// Canonical state directory (`.kageha`) of the job's Task_Workspace.
    state_dir: PathBuf,
    deadline_ns: u64,
    process: Mutex<Process>,
    ended: Mutex<Option<(JobState, Option<i32>)>>,
}

impl Job {
    /// Whether the leader is alive. Reaps an owned leader that has exited.
    fn alive(&self) -> bool {
        let mut process = lock(&self.process);
        let alive = match &mut *process {
            Process::Child(child) => matches!(child.try_wait(), Ok(None)),
            Process::Session(session) => !lock(session).try_reap(),
            Process::Adopted => same_process(self.record.pid, self.record.started_ns),
            Process::Gone => false,
        };
        if !alive {
            *process = Process::Gone;
        }
        alive
    }

    fn status(&self) -> JobStatus {
        let ended = *lock(&self.ended);
        let (state, exit_code) = match ended {
            Some(ended) => ended,
            None if self.alive() => (JobState::Running, None),
            None => {
                let code = open_jobs_dir(&self.state_dir, false)
                    .ok()
                    .and_then(|dir| read_exit(&dir, &JobFiles::new(&self.record.job_id).exit));
                let ended = match code {
                    Some(code) => (JobState::Exited, Some(code)),
                    None => (JobState::Killed, None),
                };
                *lock(&self.ended) = Some(ended);
                ended
            }
        };
        JobStatus {
            record: self.record.clone(),
            state,
            exit_code,
        }
    }

    /// Sends `signal` to the job's process group. Only while the leader is alive, or, with
    /// `group_only`, while the group still exists (the group ID cannot be reused while it
    /// does, so it is still this job's group).
    fn signal(&self, signal: libc::c_int, group_only: bool) -> bool {
        let pid = self.record.pid;
        let leader = self.alive();
        if pid <= 1 || pid > i32::MAX as u32 || !(leader || group_only && group_exists(pid)) {
            return false;
        }
        unsafe { libc::kill(-(pid as i32), signal) == 0 || leader && libc::kill(pid as i32, signal) == 0 }
    }

    /// Waits until the leader and its process group are gone.
    fn wait_gone(&self, within: Duration) -> bool {
        let end = Instant::now() + within;
        loop {
            let leader = self.alive();
            if !leader && !group_exists(self.record.pid) {
                return true;
            }
            if Instant::now() >= end {
                return false;
            }
            thread::sleep(POLL);
        }
    }

    /// SIGTERM, then SIGKILL after `grace` if any group member remains.
    fn terminate(&self, grace: Duration) {
        if !self.signal(libc::SIGTERM, false) {
            return;
        }
        if !self.wait_gone(grace) {
            self.signal(libc::SIGKILL, true);
            self.wait_gone(Duration::from_secs(5));
        }
    }
}

/// Enforces the runtime limit and reaps the job when it ends.
fn watch(job: Arc<Job>, grace: Duration) {
    let spawned = thread::Builder::new()
        .name("kageha-shell-job".into())
        .spawn(move || {
            loop {
                if !job.alive() {
                    let _ = job.status();
                    return;
                }
                let now = now_ns();
                if now >= job.deadline_ns {
                    break;
                }
                thread::sleep(Duration::from_nanos(job.deadline_ns - now).min(WATCH_INTERVAL));
            }
            job.terminate(grace);
            let _ = job.status();
        });
    if let Err(error) = spawned {
        eprintln!("kageha-workspace: cannot watch shell job: {error}");
    }
}

/// Where a session's Host state lives and what else it may read, besides the defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SessionRoots {
    /// The workspace's state directory (`.kageha`), when it is not `<workspace>/.kageha`:
    /// a delegated worker's Git worktree keeps its state outside the checkout.
    pub state_dir: Option<PathBuf>,
    /// Extra read-only roots, for example a delegated worker's parent workspace.
    pub read_roots: Vec<PathBuf>,
    /// Subtrees of the read roots the session can't read or write.
    pub hidden_roots: Vec<PathBuf>,
}

impl SessionRoots {
    fn absolute(&self) -> bool {
        self.state_dir.iter().chain(&self.read_roots).chain(&self.hidden_roots).all(|p| p.is_absolute())
    }
}

struct SessionEntry {
    paths: ShellPaths,
    session: Arc<Mutex<ShellSession>>,
}

/// One persistent shell session per chat and the chat's jobs, in this process.
pub struct ShellManager {
    toolchain_cache: PathBuf,
    limits: JobLimits,
    container: bool,
    sessions: Mutex<HashMap<String, SessionEntry>>,
    jobs: Mutex<HashMap<String, Arc<Job>>>,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.to_owned())
}

impl ShellManager {
    pub fn new(toolchain_cache: PathBuf) -> Self {
        Self::with_limits(toolchain_cache, JobLimits::default())
    }

    pub fn with_limits(toolchain_cache: PathBuf, limits: JobLimits) -> Self {
        Self {
            toolchain_cache,
            limits,
            container: false,
            sessions: Mutex::new(HashMap::new()),
            jobs: Mutex::new(HashMap::new()),
        }
    }

    pub(super) fn in_container(toolchain_cache: PathBuf) -> Self {
        Self { container: true, ..Self::new(toolchain_cache) }
    }

    fn command(container: bool, sandbox: &kageha_sandbox::ProcessSandbox, program: &str, args: &[String]) -> io::Result<Command> {
        if container {
            let mut command = Command::new(program);
            command.args(args);
            return Ok(command);
        }
        sandbox.command(program, args)
    }

    fn open_shell(&self, paths: ShellPaths) -> io::Result<ShellSession> {
        let container = self.container;
        ShellSession::open_with(paths, &move |sandbox, program, args| Self::command(container, sandbox, program, args))
    }

    pub fn toolchain_cache(&self) -> &Path {
        &self.toolchain_cache
    }

    /// The prepared session paths for `workspace` and `roots`.
    fn prepare(&self, workspace: &Path, roots: &SessionRoots) -> io::Result<ShellPaths> {
        let mut paths = ShellPaths::prepare_in(workspace, roots.state_dir.as_deref(), &self.toolchain_cache)
            .map_err(|e| io::Error::new(e.kind(), format!("Cannot prepare shell directories: {e}")))?;
        paths.extra_read_roots = roots.read_roots.clone();
        paths.hidden_roots = roots.hidden_roots.clone();
        Ok(paths)
    }

    fn sessions(&self) -> MutexGuard<'_, HashMap<String, SessionEntry>> {
        lock(&self.sessions)
    }

    fn jobs(&self) -> MutexGuard<'_, HashMap<String, Arc<Job>>> {
        lock(&self.jobs)
    }

    /// Opens the chat's shell session, or reuses it when it is usable and has the same
    /// paths. A broken session is replaced (and its process group killed).
    pub fn open_session(
        &self,
        chat: &str,
        workspace: &Path,
        read_roots: Vec<PathBuf>,
    ) -> io::Result<SessionInfo> {
        self.open_session_in(chat, workspace, &SessionRoots { read_roots, ..SessionRoots::default() })
    }

    /// [`ShellManager::open_session`] with the workspace's state directory and the extra
    /// read and hidden roots in `roots`.
    pub fn open_session_in(
        &self,
        chat: &str,
        workspace: &Path,
        roots: &SessionRoots,
    ) -> io::Result<SessionInfo> {
        if !valid_chat(chat) {
            return Err(invalid("Invalid chat id"));
        }
        if !workspace.is_absolute() || !roots.absolute() {
            return Err(invalid("Paths must be absolute"));
        }
        let paths = self.prepare(workspace, roots)?;
        // Do not hold the map lock while waiting for a session that is running a command.
        let existing = self.sessions().get(chat).map(|entry| entry.session.clone());
        if let Some(existing) = existing {
            let session = lock(&existing);
            if session.broken().is_none() {
                if session.paths() != &paths {
                    return Err(io::Error::other("Chat already has a session with other paths"));
                }
                return Ok(SessionInfo {
                    pid: session.pid(),
                    reused: true,
                    restored: false,
                });
            }
        }
        let mut session = self.open_shell(paths.clone()).map_err(|e| {
            io::Error::new(e.kind(), format!("Cannot start sandboxed shell: {e}"))
        })?;
        // A chat that had a session before (it broke, became a job, or this process is a
        // restarted shell service or server) continues in its saved directory and variables.
        let restored = match session.restore_state() {
            Ok(restored) => restored,
            Err(error) => {
                eprintln!("kageha-workspace: shell state for chat {chat} not restored: {error}");
                false
            }
        };
        let restored = if session.broken().is_some() {
            // The restore itself ended the session: start clean in the workspace root.
            eprintln!("kageha-workspace: shell state for chat {chat} ended its session; starting without it");
            session = self.open_shell(paths.clone()).map_err(|e| {
                io::Error::new(e.kind(), format!("Cannot start sandboxed shell: {e}"))
            })?;
            false
        } else {
            restored
        };
        let pid = session.pid();
        let entry = SessionEntry {
            paths,
            session: Arc::new(Mutex::new(session)),
        };
        self.sessions().insert(chat.to_owned(), entry);
        Ok(SessionInfo { pid, reused: false, restored })
    }

    /// Closes the chat's session. Its jobs keep running.
    pub fn close_session(&self, chat: &str) -> bool {
        let Some(entry) = self.sessions().remove(chat) else {
            return false;
        };
        // Close once no `run` holds it; otherwise dropping the last Arc kills it.
        if let Ok(session) = Arc::try_unwrap(entry.session) {
            session.into_inner().unwrap_or_else(|e| e.into_inner()).close();
        }
        true
    }

    fn session(&self, chat: &str) -> io::Result<(ShellPaths, Arc<Mutex<ShellSession>>)> {
        self.sessions()
            .get(chat)
            .map(|entry| (entry.paths.clone(), entry.session.clone()))
            .ok_or_else(|| io::Error::other("No session for chat"))
    }

    /// Runs `command` in the chat's session (see [`ShellSession::run`]). If it is still
    /// running after `timeout`, or when `cancel` is set, it continues as a job and the
    /// session is removed; call [`ShellManager::open_session`] before the next command.
    pub fn run(
        &self,
        chat: &str,
        command: &str,
        call_id: &str,
        timeout: Duration,
        cancel: &AtomicBool,
    ) -> io::Result<Foreground> {
        let (_, session) = self.session(chat)?;
        self.run_in(chat, session, command, call_id, timeout, cancel, true)
    }
    /// The working directory of the chat's session (see [`ShellSession::current_dir`]),
    /// or `None` when the chat has no usable session or it is busy. Never waits.
    pub fn session_dir(&self, chat: &str) -> Option<PathBuf> {
        let (_, session) = self.session(chat).ok()?;
        let mut session = session.try_lock().ok()?;
        if session.broken().is_some() {
            return None;
        }
        Some(session.current_dir())
    }
    /// Runs `command` once in a new sandboxed shell for `chat`, apart from the chat's
    /// session: its `cd`, `export` and other state changes don't reach the session, and the
    /// session need not exist. `write_roots` widen the sandbox of this one process (roots
    /// that don't exist, and `/`, are left out). The shell starts in `cwd`, which must be
    /// inside the workspace. Like [`ShellManager::run`], a command still running at
    /// `timeout`, or when `cancel` is set, continues as a job of `chat`.
    #[allow(clippy::too_many_arguments)]
    pub fn run_once(
        &self,
        chat: &str,
        workspace: &Path,
        cwd: &Path,
        write_roots: Vec<PathBuf>,
        command: &str,
        call_id: &str,
        timeout: Duration,
        cancel: &AtomicBool,
    ) -> io::Result<Foreground> {
        self.run_once_in(chat, workspace, &SessionRoots::default(), cwd, write_roots, command, call_id, timeout, cancel)
    }

    /// [`ShellManager::run_once`] with the workspace's state directory and the extra read
    /// and hidden roots in `roots`, as for the chat's session.
    #[allow(clippy::too_many_arguments)]
    pub fn run_once_in(
        &self,
        chat: &str,
        workspace: &Path,
        roots: &SessionRoots,
        cwd: &Path,
        write_roots: Vec<PathBuf>,
        command: &str,
        call_id: &str,
        timeout: Duration,
        cancel: &AtomicBool,
    ) -> io::Result<Foreground> {
        if !valid_chat(chat) {
            return Err(invalid("Invalid chat id"));
        }
        if !workspace.is_absolute() || !cwd.is_absolute() || write_roots.iter().any(|p| !p.is_absolute()) || !roots.absolute() {
            return Err(invalid("Paths must be absolute"));
        }
        let mut paths = self.prepare(workspace, roots)?;
        let cwd = cwd.canonicalize()?;
        if !cwd.starts_with(&paths.workspace) {
            return Err(invalid("The working directory must be inside the workspace"));
        }
        paths.extra_write_roots = write_roots;
        let mut session = self.open_shell(paths).map_err(|e| {
            io::Error::new(e.kind(), format!("Cannot start sandboxed shell: {e}"))
        })?;
        if cwd != session.paths().workspace {
            let moved = session.run(
                &format!("cd {}", single_quoted(&cwd.to_string_lossy())),
                call_id,
                Duration::from_secs(10),
            )?;
            if moved.exit_code != 0 {
                return Err(io::Error::other(format!("Cannot change to {}", cwd.display())));
            }
        }
        // Not in the session map: dropping it after the command ends its process group.
        self.run_in(chat, Arc::new(Mutex::new(session)), command, call_id, timeout, cancel, false)
    }
    /// Runs `command` in `session`. With `persist` (the chat's own session), a finished
    /// command is followed by [`ShellSession::save_state`], so a later session of the chat
    /// starts where this one left off. Saving is advisory: a failure is logged and the
    /// command's result returned as is.
    #[allow(clippy::too_many_arguments)]
    fn run_in(
        &self,
        chat: &str,
        session: Arc<Mutex<ShellSession>>,
        command: &str,
        call_id: &str,
        timeout: Duration,
        cancel: &AtomicBool,
        persist: bool,
    ) -> io::Result<Foreground> {
        let job_id = new_job_id()?;
        let started_ns = now_ns();
        let mut guard = lock(&session);
        let step = guard.run_or_detach(command, call_id, &job_id, timeout, cancel)?;
        let cancelled = match step {
            RunStep::Finished(output) => {
                if persist && guard.broken().is_none() {
                    if let Err(error) = guard.save_state() {
                        if guard.broken().is_none() {
                            eprintln!("kageha-workspace: shell state for chat {chat} not saved: {error}");
                        }
                    }
                }
                return Ok(Foreground::Finished(output));
            }
            RunStep::Detached { cancelled } => cancelled,
        };
        let pid = guard.pid();
        let state_dir = guard.paths().state_dir.clone();
        drop(guard);
        {
            let mut sessions = self.sessions();
            if sessions
                .get(chat)
                .is_some_and(|entry| Arc::ptr_eq(&entry.session, &session))
            {
                sessions.remove(chat);
            }
        }
        let (log_path, stderr_path) = JobRecord::paths_for(&job_id, true);
        let record = JobRecord {
            job_id,
            chat: chat.to_owned(),
            command: command.to_owned(),
            pid,
            started_ns: process_identity(pid).map_or(0, |p| p.start_ns),
            log_path,
            stderr_path,
        };
        let deadline = started_ns.saturating_add(self.limits.max_runtime.as_nanos() as u64);
        let job = self.register(record, state_dir, Process::Session(session), deadline);
        Ok(Foreground::Detached {
            job: job.status(),
            cancelled,
        })
    }

    /// Starts `command` as a background job of `chat`, which must have a session.
    pub fn start_job(&self, chat: &str, command: &str) -> io::Result<JobStatus> {
        if command.trim().is_empty() || command.contains('\0') {
            return Err(invalid("A non-empty command without NUL bytes is required"));
        }
        let (paths, session) = self.session(chat)?;
        let cwd = match session.try_lock() {
            Ok(mut session) if session.broken().is_none() => session.current_dir(),
            Ok(_) | Err(TryLockError::Poisoned(_)) | Err(TryLockError::WouldBlock) => {
                paths.workspace.clone()
            }
        };
        let job_id = new_job_id()?;
        let files = JobFiles::new(&job_id);
        let dir = open_jobs_dir(&paths.state_dir, true)?;
        let log = open_at(
            &dir,
            &files.log,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_APPEND,
            0o600,
        )?;
        let exit_path = paths.state_dir.join("jobs").join(&files.exit);
        let (program, mut args) = shell_program();
        args.extend([
            "-c".into(),
            WRAPPER.into(),
            "kageha-job".into(),
            command.into(),
            exit_path.to_string_lossy().into_owned(),
        ]);
        let spawned = (|| {
            let mut child = Self::command(self.container, &paths.sandbox(), program, &args)?;
            child
                .env_clear()
                .envs(shell_environment(&host_environment(), &paths))
                .current_dir(&cwd)
                .stdin(Stdio::null())
                .stdout(log.try_clone()?)
                .stderr(log);
            // A new session and process group, so the job outlives the chat's shell and
            // can be signalled as a group.
            unsafe {
                child.pre_exec(|| {
                    if libc::setsid() < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            child.spawn()
        })();
        let started_ns = now_ns();
        let child = match spawned {
            Ok(child) => child,
            Err(error) => {
                files.remove_except(&dir, &[]);
                return Err(io::Error::new(
                    error.kind(),
                    format!("Cannot start background job: {error}"),
                ));
            }
        };
        let pid = child.id();
        let (log_path, stderr_path) = JobRecord::paths_for(&job_id, false);
        let record = JobRecord {
            job_id,
            chat: chat.to_owned(),
            command: command.to_owned(),
            pid,
            started_ns: process_identity(pid).map_or(0, |p| p.start_ns),
            log_path,
            stderr_path,
        };
        let deadline = started_ns.saturating_add(self.limits.max_runtime.as_nanos() as u64);
        let job = self.register(record, paths.state_dir.clone(), Process::Child(child), deadline);
        Ok(job.status())
    }

    fn register(
        &self,
        record: JobRecord,
        state_dir: PathBuf,
        process: Process,
        deadline_ns: u64,
    ) -> Arc<Job> {
        let job = Arc::new(Job {
            record,
            state_dir,
            deadline_ns,
            process: Mutex::new(process),
            ended: Mutex::new(None),
        });
        self.jobs().insert(job.record.job_id.clone(), job.clone());
        watch(job.clone(), self.limits.kill_grace);
        job
    }

    fn job(&self, chat: &str, job_id: &str) -> io::Result<Arc<Job>> {
        if !valid_job_id(job_id) {
            return Err(invalid("Invalid job id"));
        }
        self.jobs()
            .get(job_id)
            .filter(|job| job.record.chat == chat)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("No job {job_id} in this chat")))
    }

    pub fn job_status(&self, chat: &str, job_id: &str) -> io::Result<JobStatus> {
        Ok(self.job(chat, job_id)?.status())
    }

    /// Up to `max` bytes (at most [`JOB_OUTPUT_WINDOW`]) of the job's log from `offset`.
    pub fn job_output(
        &self,
        chat: &str,
        job_id: &str,
        offset: u64,
        max: usize,
    ) -> io::Result<JobOutput> {
        let job = self.job(chat, job_id)?;
        // State first: once it is not running, the log read below is final.
        let status = job.status();
        let max = max.clamp(MIN_WINDOW, JOB_OUTPUT_WINDOW);
        let dir = open_jobs_dir(&job.state_dir, false)?;
        let (bytes, size) = match open_regular(&dir, &JobFiles::new(job_id).log)? {
            None => (Vec::new(), 0),
            Some(mut file) => {
                let size = file.metadata()?.len();
                if offset > size {
                    return Err(invalid(&format!(
                        "offset {offset} is past the end of the log ({size} bytes)"
                    )));
                }
                file.seek(SeekFrom::Start(offset))?;
                let mut bytes = Vec::with_capacity(max);
                (&mut file).take(max as u64).read_to_end(&mut bytes)?;
                let end = offset + bytes.len() as u64;
                (bytes, size.max(end))
            }
        };
        if size == 0 && offset > 0 {
            return Err(invalid(&format!("offset {offset} is past the end of the log (0 bytes)")));
        }
        let ended = status.state != JobState::Running;
        let keep = utf8_prefix(&bytes, ended && offset + bytes.len() as u64 >= size);
        let next_offset = offset + keep as u64;
        Ok(JobOutput {
            job_id: job_id.to_owned(),
            state: status.state,
            exit_code: status.exit_code,
            offset,
            next_offset,
            size,
            output: String::from_utf8_lossy(&bytes[..keep]).into_owned(),
            complete: ended && next_offset >= size,
            stderr_path: status.record.stderr_path,
        })
    }

    /// Sends SIGTERM to the job's process group, waits up to 2 seconds, and returns the
    /// state. A job still running after the kill grace gets SIGKILL.
    pub fn stop_job(&self, chat: &str, job_id: &str) -> io::Result<JobStatus> {
        let job = self.job(chat, job_id)?;
        if job.signal(libc::SIGTERM, false) {
            let grace = self.limits.kill_grace;
            let waited = STOP_WAIT.min(grace);
            if !job.wait_gone(waited) {
                let job = job.clone();
                let rest = grace.saturating_sub(waited);
                thread::spawn(move || {
                    if !job.wait_gone(rest) {
                        job.signal(libc::SIGKILL, true);
                        job.wait_gone(Duration::from_secs(5));
                    }
                    let _ = job.status();
                });
            }
        }
        Ok(job.status())
    }

    /// Takes over a job started by an earlier host process. The job is `running` only if
    /// process `record.pid` is alive and started at `record.started_ns`; otherwise its end
    /// state comes from its exit file. Adopting a job this manager already has returns its
    /// current status.
    pub fn adopt_job(&self, workspace: &Path, record: JobRecord) -> io::Result<JobStatus> {
        self.adopt_job_in(workspace, None, record)
    }

    /// [`ShellManager::adopt_job`] for a workspace whose state directory is `state_dir`
    /// (default `<workspace>/.kageha`).
    pub fn adopt_job_in(&self, workspace: &Path, state_dir: Option<&Path>, record: JobRecord) -> io::Result<JobStatus> {
        if !valid_chat(&record.chat) || !valid_job_id(&record.job_id) {
            return Err(invalid("Invalid chat or job id"));
        }
        let foreground = record.stderr_path.is_some();
        if (record.log_path.clone(), record.stderr_path.clone())
            != JobRecord::paths_for(&record.job_id, foreground)
        {
            return Err(invalid("Job paths do not match the job id"));
        }
        let existing = self.jobs().get(&record.job_id).cloned();
        if let Some(job) = existing {
            if job.record.chat != record.chat {
                return Err(io::Error::new(io::ErrorKind::NotFound, "No such job in this chat"));
            }
            return Ok(job.status());
        }
        let state_dir = match state_dir {
            Some(state) => state.canonicalize()?,
            None => workspace.canonicalize()?.join(crate::task::STATE_DIR),
        };
        let process = if same_process(record.pid, record.started_ns) {
            Process::Adopted
        } else {
            Process::Gone
        };
        let deadline = record
            .started_ns
            .saturating_add(self.limits.max_runtime.as_nanos() as u64);
        Ok(self.register(record, state_dir, process, deadline).status())
    }

    /// Status of every job of `chat`, by job id.
    pub fn jobs_for(&self, chat: &str) -> Vec<JobStatus> {
        let jobs: Vec<_> = self
            .jobs()
            .values()
            .filter(|job| job.record.chat == chat)
            .cloned()
            .collect();
        let mut statuses: Vec<_> = jobs.iter().map(|job| job.status()).collect();
        statuses.sort_by(|a, b| a.record.job_id.cmp(&b.record.job_id));
        statuses
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::TaskWorkspace;
    use std::fs;

    fn sandboxed() -> bool {
        let available = kageha_sandbox::available();
        if !available {
            eprintln!("skipping: OS process sandbox unavailable");
        }
        available
    }

    struct Setup {
        _dir: tempfile::TempDir,
        workspace: PathBuf,
        manager: ShellManager,
    }

    fn setup(limits: JobLimits) -> Setup {
        let dir = tempfile::tempdir().unwrap();
        let workspace = TaskWorkspace::open(&dir.path().join("chat")).unwrap().root().to_path_buf();
        let manager = ShellManager::with_limits(dir.path().join("tc"), limits);
        manager.open_session("c1", &workspace, vec![]).unwrap();
        Setup { _dir: dir, workspace, manager }
    }

    fn wait_until(within: Duration, mut done: impl FnMut() -> bool) -> bool {
        let end = Instant::now() + within;
        while Instant::now() < end {
            if done() {
                return true;
            }
            thread::sleep(Duration::from_millis(50));
        }
        done()
    }

    fn log(manager: &ShellManager, job: &str) -> String {
        manager.job_output("c1", job, 0, JOB_OUTPUT_WINDOW).unwrap().output
    }

    fn finished(step: Foreground) -> CommandOutput {
        match step {
            Foreground::Finished(output) => output,
            Foreground::Detached { .. } => panic!("command detached"),
        }
    }

    #[test]
    fn run_once_widens_write_roots_for_one_process_and_leaves_the_session_alone() {
        if !sandboxed() {
            return;
        }
        let s = setup(JobLimits::default());
        let t = Duration::from_secs(20);
        let never = AtomicBool::new(false);
        let granted = tempfile::tempdir().unwrap();
        let target = granted.path().join("approved.txt");
        let write = format!("printf ok > {}", single_quoted(&target.to_string_lossy()));
        fs::create_dir_all(s.workspace.join("sub")).unwrap();
        finished(s.manager.run("c1", "cd sub && export KG_STATE=session", "c", t, &never).unwrap());
        assert_eq!(s.manager.session_dir("c1"), Some(s.workspace.join("sub")));
        // The session's sandbox denies the write.
        let denied = finished(s.manager.run("c1", &write, "c", t, &never).unwrap());
        assert_ne!(denied.exit_code, 0);
        assert!(!target.exists());
        #[cfg(target_os = "macos")]
        {
            let roots = ShellPaths::layout(&s.workspace, s.manager.toolchain_cache()).sandbox().write_roots;
            assert_eq!(super::super::sandbox_denials(&denied.stderr, &roots, &[]), [target.clone()], "{}", denied.stderr);
        }
        // A one-off process with the granted root can write, starting in the given directory.
        let once = finished(s.manager.run_once("c1", &s.workspace, &s.workspace.join("sub"),
            vec![granted.path().to_path_buf(), PathBuf::from("/")],
            &format!("{write}; pwd -P; echo \"[$KG_STATE]\"; cd /; export KG_STATE=once"), "c", t, &never).unwrap());
        assert_eq!(once.exit_code, 0, "{once:?}");
        assert_eq!(fs::read_to_string(&target).unwrap(), "ok");
        assert_eq!(once.stdout, format!("{}\n[]\n", s.workspace.join("sub").display()));
        // Requesting `/` must not expose the host's root filesystem.
        let root_probe = PathBuf::from(format!("/kg-root-probe-{}", target.parent().unwrap().file_name().unwrap().to_string_lossy()));
        let root = finished(s.manager.run_once("c1", &s.workspace, &s.workspace, vec![PathBuf::from("/")],
            &format!("touch {}", single_quoted(&root_probe.to_string_lossy())), "c", t, &never).unwrap());
        assert!(!root_probe.exists(), "{root:?}");
        #[cfg(target_os = "macos")]
        assert_ne!(root.exit_code, 0);
        // The session keeps its own directory and variables.
        let after = finished(s.manager.run("c1", "pwd -P; echo \"[$KG_STATE]\"", "c", t, &never).unwrap());
        assert_eq!(after.stdout, format!("{}\n[session]\n", s.workspace.join("sub").display()));
        assert!(s.manager.run_once("c1", &s.workspace, Path::new("/tmp"), vec![], "true", "c", t, &never).is_err());
    }

    /// Every file under `.kageha/` whose bytes contain `needle`.
    fn state_files_containing(workspace: &Path, needle: &str) -> Vec<PathBuf> {
        fn walk(dir: &Path, needle: &str, found: &mut Vec<PathBuf>) {
            for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
                let path = entry.path();
                let kind = entry.file_type().unwrap();
                if kind.is_dir() {
                    walk(&path, needle, found);
                } else if kind.is_file()
                    && fs::read(&path).is_ok_and(|bytes| bytes.windows(needle.len()).any(|w| w == needle.as_bytes()))
                {
                    found.push(path);
                }
            }
        }
        let mut found = Vec::new();
        walk(&workspace.join(".kageha"), needle, &mut found);
        found
    }

    #[test]
    fn a_new_session_starts_in_the_saved_directory_with_saved_non_secret_variables() {
        if !sandboxed() {
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let Setup { workspace, manager, _dir } = setup(JobLimits::default());
        let t = Duration::from_secs(20);
        let never = AtomicBool::new(false);
        let set = finished(manager.run("c1",
            "mkdir -p sub/deeper/bin && cd sub/deeper && export KG_KEEP='a b' KG_QUOTE=\"it's\" && \
             export OPENAI_API_KEY=sk-kg-secret-1 GH_TOKEN=kg-secret-2 Db_Secret=kg-secret-3 && \
             export PATH=\"$PWD/bin:$PATH\" && KG_LOCAL=unexported",
            "c", t, &never).unwrap());
        assert_eq!(set.exit_code, 0, "{set:?}");
        let file = workspace.join(crate::task::SHELL_ENV);
        let saved: super::super::state::SavedShell = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        assert_eq!(fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(saved.cwd, "sub/deeper");
        assert_eq!(saved.env.get("KG_KEEP").map(String::as_str), Some("a b"));
        assert_eq!(saved.env.get("KG_QUOTE").map(String::as_str), Some("it's"));
        assert!(saved.env["PATH"].starts_with(&format!("{}/sub/deeper/bin:", workspace.display())));
        assert!(!saved.env.contains_key("KG_LOCAL") && !saved.env.contains_key("PWD") && !saved.env.contains_key("HOME"));
        // Secret-named variables are never written: not in shell-env, nor left in any
        // other state file (job logs, spills) by the capture.
        for name in ["OPENAI_API_KEY", "GH_TOKEN", "Db_Secret"] {
            assert!(!saved.env.contains_key(name), "{name}");
        }
        assert_eq!(state_files_containing(&workspace, "kg-secret"), Vec::<PathBuf>::new());

        let check = "pwd -P; echo \"[$KG_KEEP][$KG_QUOTE][${OPENAI_API_KEY-unset}][${KG_LOCAL-unset}]\"; echo \"$PATH\"";
        let expected_dir = format!("{}\n[a b][it's][unset][unset]\n{}/sub/deeper/bin:", workspace.join("sub/deeper").display(), workspace.display());
        // The session ends (a forced break): the next session restores the saved state.
        assert!(manager.run("c1", "exit 3", "x", t, &never).is_err());
        let reopened = manager.open_session("c1", &workspace, vec![]).unwrap();
        assert!(!reopened.reused && reopened.restored, "{reopened:?}");
        let out = finished(manager.run("c1", check, "c", t, &never).unwrap());
        assert!(out.stdout.starts_with(&expected_dir), "{}", out.stdout);
        // A restarted shell service (a new manager) restores it too, and its jobs start
        // in the restored directory.
        let restarted = ShellManager::new(_dir.path().join("tc"));
        assert!(restarted.open_session("c1", &workspace, vec![]).unwrap().restored);
        let out = finished(restarted.run("c1", check, "c", t, &never).unwrap());
        assert!(out.stdout.starts_with(&expected_dir), "{}", out.stdout);
        // A directory that no longer exists is skipped; the variables still apply.
        finished(restarted.run("c1", "cd .. && mkdir gone && cd gone", "c", t, &never).unwrap());
        fs::remove_dir(workspace.join("sub/gone")).unwrap();
        let fresh = ShellManager::new(_dir.path().join("tc"));
        assert!(fresh.open_session("c1", &workspace, vec![]).unwrap().restored);
        let out = finished(fresh.run("c1", "pwd -P; echo \"$KG_KEEP\"", "c", t, &never).unwrap());
        assert_eq!(out.stdout, format!("{}\na b\n", workspace.display()));
    }

    #[test]
    fn process_identity_distinguishes_reused_pids() {
        let me = process_identity(std::process::id()).unwrap();
        assert!(!me.zombie);
        assert_eq!(process_identity(std::process::id()), Some(me));
        let now = now_ns();
        assert!(me.start_ns <= now && now - me.start_ns < 24 * 3600 * 1_000_000_000, "{me:?}");
        assert!(same_process(std::process::id(), me.start_ns));
        // Same PID, different start time: a reused PID is not the job.
        assert!(!same_process(std::process::id(), me.start_ns + 1));
        assert!(!same_process(std::process::id(), 0));
        let mut child = std::process::Command::new("/bin/sh").args(["-c", "exit 0"]).spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert_eq!(process_identity(pid), None);
        assert_eq!(process_identity(0), None);
    }

    #[test]
    fn job_ids_and_output_windows_are_well_formed() {
        let id = new_job_id().unwrap();
        assert!(valid_job_id(&id), "{id}");
        assert_ne!(id, new_job_id().unwrap());
        for bad in ["", "job-", "job-0123456789ABCDEF", "job-0123456789abcde", "../job-0123456789abcdef", "x-0123456789abcdef"] {
            assert!(!valid_job_id(bad), "{bad}");
        }
        assert_eq!(JobRecord::paths_for(&id, true).1, Some(format!(".kageha/jobs/{id}.err")));
        let text = "aé".as_bytes();
        // An incomplete trailing character waits for more output unless the log is complete.
        assert_eq!(utf8_prefix(&text[..2], false), 1);
        assert_eq!(utf8_prefix(&text[..2], true), 2);
        assert_eq!(utf8_prefix(text, false), 3);
        assert_eq!(JobState::parse(JobState::Killed.as_str()), Some(JobState::Killed));
    }

    #[test]
    fn background_job_lifecycle_status_output_and_stop() {
        if !sandboxed() {
            return;
        }
        let Setup { workspace, manager, _dir } = setup(JobLimits::default());
        // The job starts in the session's working directory.
        let never = AtomicBool::new(false);
        let cd = manager.run("c1", "cd notes", "cd", Duration::from_secs(20), &never).unwrap();
        assert!(matches!(cd, Foreground::Finished(ref out) if out.exit_code == 0), "{cd:?}");
        let started = manager.start_job("c1", "pwd -P; echo started; sleep 30; echo never").unwrap();
        let id = started.record.job_id.clone();
        assert_eq!(started.state, JobState::Running);
        assert!(started.record.pid > 1 && started.record.started_ns > 0);
        assert_eq!(started.record.log_path, format!(".kageha/jobs/{id}.log"));
        assert!(wait_until(Duration::from_secs(10), || log(&manager, &id).contains("started")));
        assert_eq!(log(&manager, &id), format!("{}\nstarted\n", workspace.join("notes").display()));
        assert_eq!(manager.job_status("c1", &id).unwrap().state, JobState::Running);
        let window = manager.job_output("c1", &id, 0, JOB_OUTPUT_WINDOW).unwrap();
        assert!(!window.complete && window.next_offset == window.size);
        // Other chats cannot see the job; bad offsets are rejected.
        assert_eq!(manager.job_status("c2", &id).unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(manager.job_output("c1", &id, 1 << 20, 64).unwrap_err().kind(), io::ErrorKind::InvalidInput);
        let begin = Instant::now();
        let stopped = manager.stop_job("c1", &id).unwrap();
        assert_eq!((stopped.state, stopped.exit_code), (JobState::Killed, None));
        assert!(begin.elapsed() < Duration::from_secs(5));
        assert!(!log(&manager, &id).contains("never"));
        assert!(!same_process(started.record.pid, started.record.started_ns));

        // `exit` in the command still records its status; output reads back in windows.
        let job = manager.start_job("c1", "echo out; echo err >&2; printf 'é%.0s' 1 2 3 4 5; exit 3").unwrap();
        let id = job.record.job_id;
        assert!(wait_until(Duration::from_secs(10), || manager.job_status("c1", &id).unwrap().state != JobState::Running));
        let done = manager.job_status("c1", &id).unwrap();
        assert_eq!((done.state, done.exit_code), (JobState::Exited, Some(3)));
        let mut joined = String::new();
        let mut offset = 0;
        loop {
            let window = manager.job_output("c1", &id, offset, 5).unwrap();
            assert!(window.next_offset - offset <= MIN_WINDOW as u64);
            joined.push_str(&window.output);
            offset = window.next_offset;
            if window.complete {
                break;
            }
        }
        assert_eq!(joined, "out\nerr\nééééé");
        assert!(workspace.join(format!(".kageha/jobs/{id}.exit")).is_file());
        assert_eq!(manager.jobs_for("c1").len(), 2);
    }

    #[test]
    fn foreground_timeout_continues_as_a_job() {
        if !sandboxed() {
            return;
        }
        let Setup { workspace, manager, _dir } = setup(JobLimits::default());
        let never = AtomicBool::new(false);
        let quick = manager.run("c1", "echo quick", "q", Duration::from_secs(20), &never).unwrap();
        assert!(matches!(quick, Foreground::Finished(ref out) if out.stdout == "quick\n"), "{quick:?}");
        let slow = manager
            .run("c1", "cd notes; echo before; echo warn >&2; sleep 1.5; echo after; echo done > done.txt", "slow",
                Duration::from_millis(300), &never)
            .unwrap();
        let Foreground::Detached { job, cancelled } = slow else { panic!("{slow:?}") };
        assert!(!cancelled);
        assert_eq!(job.state, JobState::Running);
        let id = job.record.job_id.clone();
        assert_eq!(job.record.stderr_path, Some(format!(".kageha/jobs/{id}.err")));
        // The session became the job; the chat needs a new session.
        assert!(manager.run("c1", "true", "t", Duration::from_secs(5), &never).is_err());
        assert!(!manager.open_session("c1", &workspace, vec![]).unwrap().reused);
        assert!(wait_until(Duration::from_secs(15), || manager.job_status("c1", &id).unwrap().state != JobState::Running));
        let done = manager.job_status("c1", &id).unwrap();
        assert_eq!((done.state, done.exit_code), (JobState::Exited, Some(0)));
        assert_eq!(log(&manager, &id), "before\nafter\n");
        assert_eq!(fs::read_to_string(workspace.join(&job.record.stderr_path.unwrap())).unwrap(), "warn\n");
        assert_eq!(fs::read_to_string(workspace.join("notes/done.txt")).unwrap(), "done\n");
        // The new session starts in the workspace root and still works.
        let pwd = manager.run("c1", "pwd -P", "p", Duration::from_secs(20), &never).unwrap();
        assert!(matches!(pwd, Foreground::Finished(ref out) if out.stdout == format!("{}\n", workspace.display())), "{pwd:?}");
        // Finished foreground commands leave no job files behind.
        let mut names: Vec<_> = fs::read_dir(workspace.join(JOBS_DIR)).unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap()).collect();
        names.sort();
        assert_eq!(names, [format!("{id}.err"), format!("{id}.exit"), format!("{id}.log")]);

        // Cancellation also detaches instead of killing.
        let cancel = AtomicBool::new(true);
        let cancelled = manager.run("c1", "sleep 30", "c", Duration::from_secs(60), &cancel).unwrap();
        let Foreground::Detached { job, cancelled: true } = cancelled else { panic!("{cancelled:?}") };
        assert_eq!(manager.stop_job("c1", &job.record.job_id).unwrap().state, JobState::Killed);
    }

    #[test]
    fn runtime_limit_ends_both_responsive_and_term_ignoring_jobs() {
        if !sandboxed() {
            return;
        }
        let limits = JobLimits { max_runtime: Duration::from_millis(500), kill_grace: Duration::from_millis(1500) };
        let Setup { manager, _dir, .. } = setup(limits);
        let begin = Instant::now();
        let plain = manager.start_job("c1", "sleep 30").unwrap();
        let stubborn = manager.start_job("c1", "trap '' TERM; sleep 30").unwrap();
        let ended = |job: &JobStatus| manager.job_status("c1", &job.record.job_id).unwrap().state != JobState::Running;
        assert!(wait_until(Duration::from_secs(10), || ended(&plain)));
        assert_eq!(manager.job_status("c1", &plain.record.job_id).unwrap().state, JobState::Killed);
        // Seatbelt leaves the TERM-ignoring process alive until SIGKILL.
        // Bubblewrap can tear down its PID namespace as soon as its wrapper exits.
        let pgid = stubborn.record.pid;
        assert!(wait_until(Duration::from_secs(15), || !group_exists(pgid)));
        #[cfg(target_os = "macos")]
        let minimum = Duration::from_millis(1900);
        #[cfg(not(target_os = "macos"))]
        let minimum = limits.max_runtime;
        assert!(begin.elapsed() >= minimum, "{:?}", begin.elapsed());
        assert_eq!(manager.job_status("c1", &stubborn.record.job_id).unwrap().state, JobState::Killed);
    }

    #[test]
    fn jobs_are_readopted_only_by_pid_and_start_time() {
        if !sandboxed() {
            return;
        }
        let Setup { workspace, manager, _dir } = setup(JobLimits::default());
        let job = manager.start_job("c1", "echo hi; sleep 30").unwrap();
        let record = job.record.clone();
        // A host restart: a new manager adopts the job from its stored record.
        let restarted = ShellManager::new(_dir.path().join("tc"));
        assert_eq!(restarted.adopt_job(&workspace, record.clone()).unwrap().state, JobState::Running);
        assert!(wait_until(Duration::from_secs(10), || log(&restarted, &record.job_id).contains("hi")));
        assert_eq!(restarted.adopt_job(&workspace, record.clone()).unwrap().state, JobState::Running);
        // A PID whose start time differs is a different process: not running, never signalled.
        let other = ShellManager::new(_dir.path().join("tc"));
        let reused = JobRecord { started_ns: record.started_ns + 1, ..record.clone() };
        assert_eq!(other.adopt_job(&workspace, reused).unwrap().state, JobState::Killed);
        assert_eq!(other.stop_job("c1", &record.job_id).unwrap().state, JobState::Killed);
        assert_eq!(manager.job_status("c1", &record.job_id).unwrap().state, JobState::Running);
        let forged = JobRecord { log_path: "../x.log".into(), ..record.clone() };
        assert_eq!(ShellManager::new(_dir.path().join("tc")).adopt_job(&workspace, forged).unwrap_err().kind(), io::ErrorKind::InvalidInput);
        // Stopping through the adopting manager ends the original job.
        assert_eq!(restarted.stop_job("c1", &record.job_id).unwrap().state, JobState::Killed);
        assert!(wait_until(Duration::from_secs(5), || manager.job_status("c1", &record.job_id).unwrap().state == JobState::Killed));
    }
}
