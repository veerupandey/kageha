//! The `ShellBackend` over the `kageha-shell-service` socket (`KAGEHA_SHELL_SERVICE=process`,
//! see `supervisor::ShellMode`). The Supervisor starts and monitors the service; this client
//! speaks its JSON-lines protocol (`kageha_workspace::shell::ShellService`).
//!
//! Each call opens its own connection, so a slow command in one chat never waits behind
//! another. Replies are read in short slices: when the turn's `cancel` flag is set while a
//! command runs, a `cancel` request goes to the service on a second connection (repeated
//! until the service has seen it), and the command continues as a job exactly as with the
//! in-process `ShellManager`. Error replies carry their `io::ErrorKind`, so `shell_failure`
//! classifies them the same way in both modes.
use crate::ui_shell::{ShellBackend, ShellScope};
use kageha_workspace::shell::{
    error_kind, CommandOutput, Foreground, JobOutput, JobRecord, JobStatus,
};
use serde_json::{json, Value};
use std::{
    io::{self, BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

/// Time a reply may take beyond a command's own timeout (reading and spilling its output).
const REPLY_MARGIN: Duration = Duration::from_secs(120);
/// Deadline of requests that don't run a command.
const QUICK: Duration = Duration::from_secs(30);
/// Read slice between cancellation checks.
const SLICE: Duration = Duration::from_millis(200);

pub(crate) struct SocketShell {
    socket: PathBuf,
    container: Option<String>,
    /// The Toolchain_Cache the service was started with, for `write_roots`.
    toolchains: PathBuf,
}

/// A command's cancellation: the turn's flag and the request that would be cancelled.
struct Cancel<'a> {
    flag: &'a AtomicBool,
    chat: &'a str,
    call_id: &'a str,
}

fn unavailable(socket: &Path, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("Shell service unavailable at {}: {error}", socket.display()))
}

/// Sends `request` on a new connection and waits for the reply until `deadline`.
fn exchange(socket: &Path, request: &Value, deadline: Instant, cancel: Option<&Cancel<'_>>) -> io::Result<Value> {
    let stream = UnixStream::connect(socket).map_err(|e| unavailable(socket, e))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    stream.set_read_timeout(Some(SLICE))?;
    let mut writer = stream.try_clone()?;
    writeln!(writer, "{request}")?;
    writer.flush()?;
    let mut reader = BufReader::new(stream);
    let mut line = Vec::new();
    let mut cancel_seen = false;
    loop {
        match reader.read_until(b'\n', &mut line) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "The shell service closed the connection before replying")),
            Ok(_) if line.ends_with(b"\n") => break,
            Ok(_) => continue,
            Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted) => {}
            Err(error) => return Err(error),
        }
        if let Some(cancel) = cancel.filter(|cancel| !cancel_seen && cancel.flag.load(Ordering::Relaxed)) {
            let request = json!({"op":"cancel","chat":cancel.chat,"call_id":cancel.call_id});
            cancel_seen = exchange(socket, &request, Instant::now() + Duration::from_secs(3), None)
                .is_ok_and(|reply| reply["cancelled"] == true);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "The shell service did not reply in time"));
        }
    }
    let reply: Value = serde_json::from_slice(&line)
        .map_err(|e| io::Error::other(format!("Invalid shell service reply: {e}")))?;
    checked(reply)
}

fn checked(reply: Value) -> io::Result<Value> {
    if reply["ok"] != true {
        let message = reply["error"].as_str().unwrap_or("Shell service request failed");
        return Err(io::Error::new(error_kind(reply["kind"].as_str().unwrap_or("")), message.to_owned()));
    }
    Ok(reply)
}

/// `{"op":"ping"}` with `timeout`. The Supervisor's health check.
pub(crate) fn ping(socket: &Path, timeout: Duration) -> io::Result<Value> {
    exchange(socket, &json!({"op":"ping"}), Instant::now() + timeout, None)
}

fn parse<T: serde::de::DeserializeOwned>(value: &Value) -> io::Result<T> {
    serde_json::from_value(value.clone()).map_err(|e| io::Error::other(format!("Invalid shell service reply: {e}")))
}

fn foreground(reply: &Value) -> io::Result<Foreground> {
    if reply["timed_out"] == true {
        Ok(Foreground::Detached { job: parse::<JobStatus>(&reply["job"])?, cancelled: reply["cancelled"] == true })
    } else {
        Ok(Foreground::Finished(parse::<CommandOutput>(reply)?))
    }
}

impl SocketShell {
    pub(crate) fn new(socket: PathBuf, toolchains: PathBuf) -> Self {
        Self { socket, toolchains, container: None }
    }

    pub(crate) fn docker(container: &str) -> Self {
        Self {
            socket: PathBuf::new(),
            toolchains: PathBuf::from("/home/desktop/.cache/kageha/toolchains"),
            container: Some(container.into()),
        }
    }

    fn exchange(&self, request: &Value, deadline: Instant, cancel: Option<Cancel<'_>>) -> io::Result<Value> {
        let Some(container) = self.container.as_deref() else {
            return exchange(&self.socket, request, deadline, cancel.as_ref());
        };
        let Some(cancel) = cancel else {
            return checked(crate::machine_io::shell(container, request, deadline)?);
        };
        std::thread::scope(|scope| {
            let (done, completed) = std::sync::mpsc::channel();
            scope.spawn(move || {
                while completed.recv_timeout(SLICE).is_err_and(|error| error == std::sync::mpsc::RecvTimeoutError::Timeout) {
                    if !cancel.flag.load(Ordering::Relaxed) {
                        continue;
                    }
                    let request = json!({"op":"cancel","chat":cancel.chat,"call_id":cancel.call_id});
                    if crate::machine_io::shell(container, &request, Instant::now() + Duration::from_secs(3))
                        .is_ok_and(|reply| reply["cancelled"] == true) {
                        break;
                    }
                }
            });
            let reply = crate::machine_io::shell(container, request, deadline);
            let _ = done.send(());
            checked(reply?)
        })
    }

    /// Whether the service answers a `ping` within `timeout`.
    pub(crate) fn reachable(&self, timeout: Duration) -> bool {
        ping(&self.socket, timeout).is_ok()
    }

    fn quick(&self, request: Value) -> io::Result<Value> {
        self.exchange(&request, Instant::now() + QUICK, None)
    }

    fn open(&self, scope: &ShellScope<'_>) -> io::Result<()> {
        if self.container.is_some() {
            return self.quick(json!({"op":"open_session","chat":scope.chat,"workspace":"/work",
                "state_dir":"/machine-state"})).map(|_| ());
        }
        self.quick(json!({"op":"open_session","chat":scope.chat,"workspace":scope.workspace,
            "read_roots":scope.roots.read_roots,"state_dir":scope.roots.state_dir,"hidden_roots":scope.roots.hidden_roots}))
            .map(|_| ())
    }

    fn command(&self, request: Value, chat: &str, call_id: &str, timeout: Duration, cancel: &AtomicBool) -> io::Result<Foreground> {
        let cancel = Cancel { flag: cancel, chat, call_id };
        let reply = self.exchange(&request, Instant::now() + timeout + REPLY_MARGIN, Some(cancel))?;
        foreground(&reply)
    }
}

impl ShellBackend for SocketShell {
    fn machine(&self) -> Option<&str> {
        self.container.as_deref()
    }

    fn run(&self, scope: &ShellScope<'_>, command: &str, call_id: &str, timeout: Duration, cancel: &AtomicBool) -> io::Result<Foreground> {
        self.open(scope)?;
        let request = json!({"op":"run","chat":scope.chat,"command":command,"call_id":call_id,"timeout_s":timeout.as_secs()});
        self.command(request, scope.chat, call_id, timeout, cancel)
    }
    fn start_job(&self, scope: &ShellScope<'_>, command: &str) -> io::Result<JobStatus> {
        self.open(scope)?;
        parse(&self.quick(json!({"op":"start_job","chat":scope.chat,"command":command}))?["job"])
    }
    fn job_status(&self, chat: &str, job_id: &str) -> io::Result<JobStatus> {
        parse(&self.quick(json!({"op":"job_status","chat":chat,"job_id":job_id}))?["job"])
    }
    fn job_output(&self, chat: &str, job_id: &str, offset: u64, max: usize) -> io::Result<JobOutput> {
        parse(&self.quick(json!({"op":"job_output","chat":chat,"job_id":job_id,"offset":offset,"max_bytes":max}))?["output"])
    }
    fn stop_job(&self, chat: &str, job_id: &str) -> io::Result<JobStatus> {
        parse(&self.quick(json!({"op":"stop_job","chat":chat,"job_id":job_id}))?["job"])
    }
    fn adopt_job(&self, scope: &ShellScope<'_>, record: JobRecord) -> io::Result<JobStatus> {
        if self.container.is_some() {
            return parse(&self.quick(json!({"op":"adopt_job","workspace":"/work","record":record,
                "state_dir":"/machine-state"}))?["job"]);
        }
        parse(&self.quick(json!({"op":"adopt_job","workspace":scope.workspace,"record":record,
            "state_dir":scope.roots.state_dir}))?["job"])
    }
    fn write_roots(&self, scope: &ShellScope<'_>) -> Vec<PathBuf> {
        if self.container.is_some() {
            return ["/work", "/machine-state", "/tmp", "/home/desktop"].map(PathBuf::from).into();
        }
        crate::ui_shell::session_write_roots(scope, &self.toolchains)
    }
    fn current_dir(&self, chat: &str, workspace: &Path) -> PathBuf {
        self.exchange(&json!({"op":"session_dir","chat":chat}), Instant::now() + Duration::from_secs(3), None)
            .ok()
            .and_then(|reply| reply["dir"].as_str().map(PathBuf::from))
            .unwrap_or_else(|| workspace.to_path_buf())
    }
    fn run_once(&self, scope: &ShellScope<'_>, cwd: &Path, write_roots: Vec<PathBuf>, command: &str, call_id: &str,
        timeout: Duration, cancel: &AtomicBool) -> io::Result<Foreground> {
        let request = json!({"op":"run_once","chat":scope.chat,"workspace":scope.workspace,"roots":scope.roots,"cwd":cwd,
            "write_roots":write_roots,"command":command,"call_id":call_id,"timeout_s":timeout.as_secs()});
        self.command(request, scope.chat, call_id, timeout, cancel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kageha_workspace::shell::{JobState, ShellPaths, ShellService};
    use kageha_workspace::task::TaskWorkspace;
    use std::{os::unix::net::UnixListener, sync::Arc};

    /// A shell service on a socket in a short temp directory, served by this process.
    fn serve(dir: &Path) -> (PathBuf, PathBuf) {
        let toolchains = kageha_workspace::shell::ensure_private_dir(&dir.join("tc")).unwrap();
        let socket = dir.join("s.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let service = Arc::new(ShellService::new(toolchains.clone()));
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let service = service.clone();
                std::thread::spawn(move || service.serve_connection(stream));
            }
        });
        (socket, toolchains)
    }

    #[test]
    fn socket_backend_round_trip_including_run_once_and_cancel() {
        if !kageha_workspace::shell::sandbox_available() {
            eprintln!("skipping: OS process sandbox unavailable");
            return;
        }
        let dir = tempfile::Builder::new().prefix("kg-sock").tempdir_in("/tmp").unwrap();
        let (socket, toolchains) = serve(dir.path());
        let backend = SocketShell::new(socket.clone(), toolchains.clone());
        assert!(ping(&socket, Duration::from_secs(3)).unwrap()["pid"].is_u64());
        let workspace = TaskWorkspace::open(&dir.path().join("chat")).unwrap().root().to_path_buf();
        let scope = ShellScope { chat: "c1", workspace: &workspace, roots: Default::default() };
        let never = AtomicBool::new(false);
        let t = Duration::from_secs(20);
        // The session keeps its state between calls.
        let Foreground::Finished(out) = backend.run(&scope, "mkdir -p sub && cd sub && export KG_X=1", "c_1", t, &never).unwrap() else { panic!() };
        assert_eq!(out.exit_code, 0, "{out:?}");
        let Foreground::Finished(out) = backend.run(&scope, "echo $KG_X; pwd", "c_2", t, &never).unwrap() else { panic!() };
        let sub = workspace.join("sub").canonicalize().unwrap();
        let mut lines = out.stdout.lines();
        assert_eq!(lines.next(), Some("1"));
        assert_eq!(Path::new(lines.next().unwrap()).canonicalize().unwrap(), sub);
        assert_eq!(backend.current_dir("c1", &workspace).canonicalize().unwrap(), sub);
        assert_eq!(backend.current_dir("other", &workspace), workspace);
        assert_eq!(backend.write_roots(&scope), ShellPaths::layout(&workspace, &toolchains).sandbox().write_roots);
        // run_once: a new shell in cwd with a widened write root, apart from the session.
        let extra = tempfile::Builder::new().prefix("kg-extra").tempdir_in("/tmp").unwrap();
        let extra_root = extra.path().canonicalize().unwrap();
        let target = extra_root.join("granted");
        let Foreground::Finished(once) = backend.run_once(&scope, &sub, vec![extra_root.clone()],
            &format!("echo ok > '{}'; cd /; echo $KG_X", target.display()), "c_3", t, &never).unwrap() else { panic!() };
        assert_eq!((once.exit_code, once.stdout.as_str()), (0, "\n"), "{once:?}");
        assert!(target.exists());
        assert_eq!(backend.current_dir("c1", &workspace).canonicalize().unwrap(), sub);
        let outside = backend.run_once(&scope, Path::new("/"), vec![], "true", "c_4", t, &never).unwrap_err();
        assert_eq!(outside.kind(), io::ErrorKind::InvalidInput, "{outside}");
        // Jobs.
        let job = backend.start_job(&scope, "echo bg; sleep 30").unwrap();
        assert_eq!(job.state, JobState::Running);
        let mut output = None;
        for _ in 0..200 {
            let window = backend.job_output("c1", &job.record.job_id, 0, 1024).unwrap();
            if window.output == "bg\n" {
                output = Some(window);
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(output.unwrap().next_offset, 3);
        assert_eq!(backend.job_status("c1", &job.record.job_id).unwrap().state, JobState::Running);
        assert_eq!(backend.stop_job("c1", &job.record.job_id).unwrap().state, JobState::Killed);
        let missing = backend.job_status("c1", "job-0123456789abcdef").unwrap_err();
        assert_eq!(missing.kind(), io::ErrorKind::NotFound, "{missing}");
        // A set cancel flag makes the running command continue as a job.
        let cancel = AtomicBool::new(false);
        let started = Instant::now();
        let ran = std::thread::scope(|threads| {
            let running = threads.spawn(|| backend.run(&scope, "sleep 30", "c_5", Duration::from_secs(60), &cancel));
            std::thread::sleep(Duration::from_millis(300));
            cancel.store(true, Ordering::Relaxed);
            running.join().unwrap()
        }).unwrap();
        assert!(started.elapsed() < Duration::from_secs(20));
        let Foreground::Detached { job, cancelled } = ran else { panic!("{ran:?}") };
        assert!(cancelled);
        assert_eq!(backend.stop_job("c1", &job.record.job_id).unwrap().state, JobState::Killed);
        // The service is gone: calls fail promptly with a clear error.
        let gone = SocketShell::new(dir.path().join("missing.sock"), toolchains);
        let error = gone.job_status("c1", "x").unwrap_err();
        assert!(error.to_string().starts_with("Shell service unavailable"), "{error}");
    }
}
