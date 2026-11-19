//! Supervisor for the Host's helper processes (spec autonomous-agent-loop, design section 13,
//! Requirement 10.1).
//!
//! Supervised components:
//!
//! | Name       | Process                                  | Health check                          | Restart |
//! |------------|------------------------------------------|---------------------------------------|---------|
//! | `browser`  | `kageha-browser` daemon                  | protocol `handshake` on `control.sock` | re-attach (see below) |
//! | `runtime`  | `kageha-runtime-service bridge`          | `GET /v1/info`                         | re-attach |
//! | `computer` | `kageha-computer control` child          | the child has not exited               | relaunch the helper |
//! | `shell`    | `kageha-shell-service` (process mode)    | `{"op":"ping"}` on its socket          | spawn it again |
//!
//! The browser daemon and the runtime service are started outside this process today (on the
//! live host by launchd jobs with `KeepAlive`, in tests by the test harness), and the Host only
//! connects to them. Starting a second copy would fight that manager for the state directory
//! and port, so their adapters return [`Started::Attached`]: the supervisor monitors them, and a
//! "restart" re-attaches (the next request reconnects) after the backoff. The process itself
//! is restarted by its manager. A new service epoch (browser) or PID (runtime) seen by a health
//! check means the process was restarted outside the supervisor; that is reported as
//! `component_restarted` and runs [`Component::on_restart`] like a restart of our own.
//!
//! The computer helper is a child the Host launches on first use (`computer_host`). Its
//! adapter detects the child's exit without sending a request, and relaunches it.
//!
//! The shell service runs only when `KAGEHA_SHELL_SERVICE=process`; see [`ShellMode`].
//!
//! Loop, per component (each on its own thread, so a hung component delays only itself):
//!
//! - Every [`Policy::interval`] (10 s) the health check runs on a separate thread and is given
//!   [`Policy::health_timeout`] (3 s). A check still running from last time counts as failed
//!   and no second one is started.
//! - [`Policy::failures_to_restart`] (2) failed checks in a row, or a process exit, trigger a
//!   restart. The process (if the supervisor owns it) is killed at once and started again
//!   after a backoff of 1, 2, 4 … s (capped at 60 s), doubling with each restart in the last
//!   [`Policy::degrade_window`] (10 minutes).
//! - A trigger when [`Policy::degrade_restarts`] (5) restarts already happened in that window
//!   marks the component degraded (and emits `component_degraded` once). It is retried every
//!   [`Policy::backoff_max`]; a successful start makes it healthy again.
//!
//! [`Supervisor::status`] never waits for a component: status lives behind its own lock,
//! which is held only to copy or replace it. Tools that need a degraded component return
//! `component_unavailable` with `retry_after` ([`require`]) instead of waiting for it.
//!
//! Events: there is no Host-wide event channel (chat events go to one chat and turn through
//! `webui::emit`), so `component_restarted` and `component_degraded` are kept in a bounded
//! in-memory log ([`Supervisor::events_since`]), written to stderr (the server log), and
//! passed to the [`Listener`] the server installs (`component_restarts::install`, task
//! 17.2), which re-establishes the component's state and notifies the running turns.
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    io,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Mutex, MutexGuard, OnceLock,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Events kept for [`Supervisor::events_since`].
const EVENT_LOG: usize = 256;
/// How often a component's thread wakes to check for process exit and due work.
const POLL: Duration = Duration::from_millis(250);

/// Timing and thresholds. [`Policy::default`] is the design's.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Policy {
    pub interval: Duration,
    pub health_timeout: Duration,
    pub failures_to_restart: u32,
    pub backoff_base: Duration,
    pub backoff_max: Duration,
    pub degrade_restarts: usize,
    pub degrade_window: Duration,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(10),
            health_timeout: Duration::from_secs(3),
            failures_to_restart: 2,
            backoff_base: Duration::from_secs(1),
            backoff_max: Duration::from_secs(60),
            degrade_restarts: 5,
            degrade_window: Duration::from_secs(600),
        }
    }
}

impl Policy {
    /// The delay before the next restart when `restarts` happened in the window.
    fn backoff(&self, restarts: usize) -> Duration {
        let factor = 1u32.checked_shl(restarts.min(31) as u32).unwrap_or(u32::MAX);
        self.backoff_base.saturating_mul(factor).min(self.backoff_max)
    }
}

/// The time source of the restart schedule. Tests inject their own.
pub(crate) trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}

pub(crate) struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// What [`Component::start`] produced.
pub(crate) enum Started {
    /// A process the supervisor owns: it watches for its exit and kills it before a restart.
    Child(Child),
    /// The process is owned elsewhere (another process manager, or a client that launches it
    /// on demand). The supervisor only checks its health.
    Attached,
}

/// A supervised helper.
pub(crate) trait Component: Send + Sync + 'static {
    fn name(&self) -> &str;
    /// Starts (or re-attaches to) the helper. Called again for each restart.
    fn start(&self) -> io::Result<Started>;
    /// The helper's existing ping or health request. Runs on its own thread; the supervisor
    /// stops waiting after [`Policy::health_timeout`].
    fn health(&self) -> io::Result<()>;
    /// The identity of the running instance (service epoch or PID) after a successful
    /// health check, when the helper reports one. A change means it restarted.
    fn instance(&self) -> Option<String> {
        None
    }
    /// For [`Started::Attached`] helpers whose process exit can be seen without a request.
    /// Polled often, so it must not block.
    fn exited(&self) -> bool {
        false
    }
    /// Runs after each restart, to re-establish the helper's state (task 17.2).
    fn on_restart(&self) {}
}

/// A component's status as seen by tools.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    Healthy,
    /// A restart is scheduled or in progress.
    Restarting,
    /// Too many restarts. The next attempt is in `retry_after`.
    Degraded { retry_after: Duration },
}

impl Status {
    fn to_json(&self) -> Value {
        match self {
            Self::Healthy => json!({"state":"healthy"}),
            Self::Restarting => json!({"state":"restarting"}),
            Self::Degraded { retry_after } => {
                json!({"state":"degraded","retry_after":retry_seconds(*retry_after)})
            }
        }
    }
}

/// `component_restarted` or `component_degraded`.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub(crate) struct SupervisorEvent {
    pub seq: u64,
    pub kind: &'static str,
    pub component: String,
    /// Milliseconds since the Unix epoch.
    pub at_ms: u64,
    pub reason: String,
    pub restarts_in_window: usize,
    /// For `component_degraded`: seconds until the next attempt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after: Option<u64>,
}

#[derive(Clone, Copy)]
enum Shared {
    Healthy,
    Restarting,
    Degraded { until: Instant },
}

/// The schedule of one component. Only its monitor (one thread, or a test's `tick`) uses it.
struct Monitor {
    started: bool,
    child: Option<Child>,
    failures: u32,
    next_check: Instant,
    restart_at: Option<Instant>,
    restarts: VecDeque<Instant>,
    /// A health check that outlived its timeout.
    probe: Option<mpsc::Receiver<(io::Result<()>, Option<String>)>>,
    instance: Option<String>,
    degraded: bool,
    reason: String,
}

struct Entry {
    component: Arc<dyn Component>,
    shared: Mutex<Shared>,
    monitor: Mutex<Monitor>,
}

/// Called with every event right after it is logged (task 17.2: `component_restarts`).
/// It runs on the component's monitor thread while that component's schedule is held, so
/// it must return quickly (hand slow work to another thread).
pub(crate) type Listener = Arc<dyn Fn(&SupervisorEvent) + Send + Sync>;

pub(crate) struct Supervisor {
    clock: Arc<dyn Clock>,
    policy: Policy,
    entries: Vec<Arc<Entry>>,
    events: Mutex<VecDeque<SupervisorEvent>>,
    seq: AtomicU64,
    stop: AtomicBool,
    listener: Mutex<Option<Listener>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// Whole seconds, rounded up, at least 1.
fn retry_seconds(delay: Duration) -> u64 {
    delay.as_millis().div_ceil(1000).max(1) as u64
}

impl Supervisor {
    pub(crate) fn new(policy: Policy, clock: Arc<dyn Clock>, components: Vec<Arc<dyn Component>>) -> Arc<Self> {
        let now = clock.now();
        let entries = components
            .into_iter()
            .map(|component| {
                Arc::new(Entry {
                    component,
                    shared: Mutex::new(Shared::Healthy),
                    monitor: Mutex::new(Monitor {
                        started: false,
                        child: None,
                        failures: 0,
                        next_check: now,
                        restart_at: None,
                        restarts: VecDeque::new(),
                        probe: None,
                        instance: None,
                        degraded: false,
                        reason: String::new(),
                    }),
                })
            })
            .collect();
        Arc::new(Self {
            clock,
            policy,
            entries,
            events: Mutex::new(VecDeque::new()),
            seq: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            listener: Mutex::new(None),
        })
    }

    /// Sets the function that receives each event after it is logged (replacing any
    /// earlier one). See [`Listener`].
    pub(crate) fn set_listener(&self, listener: Listener) {
        *lock(&self.listener) = Some(listener);
    }

    /// The status of component `name`, or `None` when it isn't supervised. Never waits for
    /// the component.
    pub(crate) fn status(&self, name: &str) -> Option<Status> {
        let entry = self.entries.iter().find(|entry| entry.component.name() == name)?;
        let shared = *lock(&entry.shared);
        Some(match shared {
            Shared::Healthy => Status::Healthy,
            Shared::Restarting => Status::Restarting,
            Shared::Degraded { until } => Status::Degraded {
                retry_after: until.saturating_duration_since(self.clock.now()),
            },
        })
    }

    /// `component_unavailable` when `name` is degraded.
    pub(crate) fn require(&self, name: &str) -> io::Result<()> {
        match self.status(name) {
            Some(Status::Degraded { retry_after }) => Err(component_unavailable(name, retry_after)),
            _ => Ok(()),
        }
    }

    /// `{name: {state, retry_after?}}` for every component.
    pub(crate) fn snapshot(&self) -> Value {
        let mut out = serde_json::Map::new();
        for entry in &self.entries {
            let name = entry.component.name();
            if let Some(status) = self.status(name) {
                out.insert(name.to_owned(), status.to_json());
            }
        }
        Value::Object(out)
    }

    /// Events with a sequence number above `after`, oldest first. Running turns get them
    /// through the [`Listener`].
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn events_since(&self, after: u64) -> Vec<SupervisorEvent> {
        lock(&self.events).iter().filter(|event| event.seq > after).cloned().collect()
    }

    /// One round over every component, in order. The background threads call the same step
    /// per component; tests drive the schedule with this and an injected clock.
    #[cfg(test)]
    pub(crate) fn tick(&self) {
        for entry in &self.entries {
            self.step(entry);
        }
    }

    /// Starts one monitor thread per component.
    pub(crate) fn spawn(self: &Arc<Self>) -> io::Result<()> {
        for entry in &self.entries {
            let supervisor = Arc::clone(self);
            let entry = Arc::clone(entry);
            std::thread::Builder::new()
                .name(format!("supervise-{}", entry.component.name()))
                .spawn(move || {
                    while !supervisor.stop.load(Ordering::Relaxed) {
                        supervisor.step(&entry);
                        std::thread::sleep(POLL);
                    }
                })?;
        }
        Ok(())
    }

    /// Stops the monitor threads and kills the processes the supervisor owns.
    #[cfg(test)]
    pub(crate) fn shutdown(&self) {
        self.stop.store(true, Ordering::Relaxed);
        for entry in &self.entries {
            if let Some(mut child) = lock(&entry.monitor).child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    fn set(&self, entry: &Entry, shared: Shared) {
        *lock(&entry.shared) = shared;
    }

    fn emit(&self, kind: &'static str, entry: &Entry, monitor: &Monitor, retry_after: Option<Duration>) {
        let event = SupervisorEvent {
            seq: self.seq.fetch_add(1, Ordering::Relaxed) + 1,
            kind,
            component: entry.component.name().to_owned(),
            at_ms: SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64,
            reason: monitor.reason.clone(),
            restarts_in_window: monitor.restarts.len(),
            retry_after: retry_after.map(retry_seconds),
        };
        eprintln!("supervisor: {}", serde_json::to_string(&event).unwrap_or_default());
        {
            let mut events = lock(&self.events);
            if events.len() == EVENT_LOG {
                events.pop_front();
            }
            events.push_back(event.clone());
        }
        let listener = lock(&self.listener).clone();
        if let Some(listener) = listener {
            listener(&event);
        }
    }

    fn step(&self, entry: &Entry) {
        let mut monitor = lock(&entry.monitor);
        let now = self.clock.now();
        if !monitor.started {
            monitor.started = true;
            self.launch(entry, &mut monitor, now, false);
            return;
        }
        if let Some(at) = monitor.restart_at {
            if now >= at {
                self.launch(entry, &mut monitor, now, true);
            }
            return;
        }
        let exited = match monitor.child.as_mut() {
            Some(child) => !matches!(child.try_wait(), Ok(None)),
            None => entry.component.exited(),
        };
        if exited {
            self.trigger(entry, &mut monitor, now, "process exited".into());
            return;
        }
        if now < monitor.next_check {
            return;
        }
        monitor.next_check = now + self.policy.interval;
        match self.probe(entry, &mut monitor) {
            Ok(instance) => {
                monitor.failures = 0;
                if !matches!(*lock(&entry.shared), Shared::Healthy) {
                    self.set(entry, Shared::Healthy);
                }
                if let Some(instance) = instance {
                    let replaced = monitor.instance.as_ref().is_some_and(|old| *old != instance);
                    monitor.instance = Some(instance);
                    if replaced {
                        monitor.reason = "restarted outside the supervisor (new instance)".into();
                        entry.component.on_restart();
                        self.emit("component_restarted", entry, &monitor, None);
                    }
                }
            }
            Err(error) => {
                monitor.failures += 1;
                if monitor.failures >= self.policy.failures_to_restart {
                    let reason = format!("{} failed health checks in a row: {error}", monitor.failures);
                    self.trigger(entry, &mut monitor, now, reason);
                }
            }
        }
    }

    /// Runs the health check on its own thread and waits up to the health timeout.
    fn probe(&self, entry: &Entry, monitor: &mut Monitor) -> io::Result<Option<String>> {
        if let Some(previous) = monitor.probe.take() {
            if let Err(mpsc::TryRecvError::Empty) = previous.try_recv() {
                monitor.probe = Some(previous);
                return Err(io::Error::new(io::ErrorKind::TimedOut, "the previous health check is still running"));
            }
        }
        let (sender, receiver) = mpsc::channel();
        let component = Arc::clone(&entry.component);
        std::thread::Builder::new()
            .name(format!("health-{}", component.name()))
            .spawn(move || {
                let result = component.health();
                let instance = result.as_ref().ok().and_then(|_| component.instance());
                let _ = sender.send((result, instance));
            })?;
        match receiver.recv_timeout(self.policy.health_timeout) {
            Ok((result, instance)) => result.map(|_| instance),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                monitor.probe = Some(receiver);
                Err(io::Error::new(io::ErrorKind::TimedOut, format!(
                    "no answer within {} ms", self.policy.health_timeout.as_millis())))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(io::Error::other("the health check panicked")),
        }
    }

    fn launch(&self, entry: &Entry, monitor: &mut Monitor, now: Instant, restart: bool) {
        if restart {
            monitor.restarts.push_back(now);
        }
        match entry.component.start() {
            Ok(started) => {
                monitor.child = match started {
                    Started::Child(child) => Some(child),
                    Started::Attached => None,
                };
                monitor.restart_at = None;
                monitor.failures = 0;
                monitor.next_check = now + self.policy.interval;
                // The next check records the new instance without reporting it again.
                monitor.instance = None;
                monitor.degraded = false;
                self.set(entry, Shared::Healthy);
                if restart {
                    entry.component.on_restart();
                    self.emit("component_restarted", entry, monitor, None);
                }
            }
            Err(error) => {
                let reason = format!("start failed: {error}");
                self.trigger(entry, monitor, now, reason);
            }
        }
    }

    /// Schedules a restart, or marks the component degraded.
    fn trigger(&self, entry: &Entry, monitor: &mut Monitor, now: Instant, reason: String) {
        if let Some(mut child) = monitor.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        while monitor.restarts.front().is_some_and(|at| now.saturating_duration_since(*at) >= self.policy.degrade_window) {
            monitor.restarts.pop_front();
        }
        monitor.failures = 0;
        monitor.reason = reason;
        let restarts = monitor.restarts.len();
        if restarts >= self.policy.degrade_restarts {
            let until = now + self.policy.backoff_max;
            monitor.restart_at = Some(until);
            self.set(entry, Shared::Degraded { until });
            if !monitor.degraded {
                monitor.degraded = true;
                self.emit("component_degraded", entry, monitor, Some(self.policy.backoff_max));
            }
        } else {
            monitor.restart_at = Some(now + self.policy.backoff(restarts));
            if !monitor.degraded {
                self.set(entry, Shared::Restarting);
            } else {
                let until = now + self.policy.backoff(restarts);
                self.set(entry, Shared::Degraded { until });
            }
        }
    }
}

/// The tool error for a degraded component.
pub(crate) fn component_unavailable(name: &str, retry_after: Duration) -> io::Error {
    let seconds = retry_seconds(retry_after);
    io::Error::other(json!({"error_code":"component_unavailable","component":name,"retryable":true,
        "retry_after":seconds,
        "summary":format!("The {name} helper keeps failing and is restarting with a delay. Try again in {seconds} s, or continue with other tools.")}).to_string())
}

static GLOBAL: OnceLock<Arc<Supervisor>> = OnceLock::new();
static SHELL: OnceLock<crate::ui_shell_socket::SocketShell> = OnceLock::new();

/// The Host's supervisor, once [`start`] ran (only `kageha-loop serve` starts it).
pub(crate) fn global() -> Option<&'static Arc<Supervisor>> {
    GLOBAL.get()
}

/// `component_unavailable` when the Host's supervisor has marked `name` degraded.
pub(crate) fn require(name: &str) -> io::Result<()> {
    global().map_or(Ok(()), |supervisor| supervisor.require(name))
}

/// `{name: {state, retry_after?}}`, or `{}` without a supervisor.
pub(crate) fn snapshot() -> Value {
    global().map_or_else(|| json!({}), |supervisor| supervisor.snapshot())
}

/// The socket shell backend, when the shell service runs as a supervised process.
pub(crate) fn shell_backend() -> Option<&'static crate::ui_shell_socket::SocketShell> {
    SHELL.get()
}

/// Where shells run: `KAGEHA_SHELL_SERVICE=inprocess|process`.
///
/// The default is `inprocess` (the `ShellManager` in the server process) for now:
///
/// - the live deployment doesn't ship the `kageha-shell-service` binary next to
///   `kageha-loop` yet, and process mode needs it.
///
/// Both modes restore a chat's session from `.kageha/shell-env` (working directory and
/// non-secret exported variables) when it starts again, and re-adopt background jobs from
/// `ui_shell_jobs` at Host startup and after the shell service restarts (task 17.2). The
/// socket backend is complete (every `ShellBackend` operation, including `run_once` and
/// cancellation), so the switch is safe to flip once the binary is deployed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShellMode {
    InProcess,
    Process,
}

impl ShellMode {
    pub(crate) fn from_env() -> Self {
        match std::env::var("KAGEHA_SHELL_SERVICE").as_deref() {
            Ok("process") => Self::Process,
            Ok("inprocess") | Ok("") | Err(_) => Self::InProcess,
            Ok(other) => {
                eprintln!("supervisor: ignoring KAGEHA_SHELL_SERVICE={other:?}; expected process or inprocess");
                Self::InProcess
            }
        }
    }
}

/// Starts the Host's supervisor with the configured helpers. `state` is the server's state
/// directory (the default home of the shell service socket). Runs once per process.
pub(crate) fn start(state: &Path) -> io::Result<()> {
    if GLOBAL.get().is_some() {
        return Ok(());
    }
    let mut components: Vec<Arc<dyn Component>> = Vec::new();
    if let Some(root) = std::env::var_os("KAGEHA_BROWSER_STATE_DIR") {
        components.push(Arc::new(Browser { root: PathBuf::from(root), epoch: Mutex::new(None) }));
    }
    if std::env::var_os("KAGEHA_RUNTIME_SERVICE_URL").is_some() {
        components.push(Arc::new(Runtime { pid: Mutex::new(None) }));
    }
    if crate::computer_host::configured() {
        components.push(Arc::new(Computer));
    }
    if ShellMode::from_env() == ShellMode::Process {
        match ShellService::from_env(state) {
            Ok(service) => {
                let _ = SHELL.set(crate::ui_shell_socket::SocketShell::new(service.socket.clone(), service.toolchains.clone()));
                components.push(Arc::new(service));
            }
            Err(error) => eprintln!("supervisor: KAGEHA_SHELL_SERVICE=process but the shell service can't be configured ({error}); shells run in this process"),
        }
    }
    let supervisor = Supervisor::new(Policy::default(), Arc::new(SystemClock), components);
    if GLOBAL.set(Arc::clone(&supervisor)).is_ok() {
        supervisor.spawn()?;
    }
    Ok(())
}

/// The `kageha-browser` daemon at `KAGEHA_BROWSER_STATE_DIR`.
struct Browser {
    root: PathBuf,
    epoch: Mutex<Option<String>>,
}

impl Component for Browser {
    fn name(&self) -> &str {
        "browser"
    }
    fn start(&self) -> io::Result<Started> {
        // Started by its own process manager; clients reconnect on their next request.
        Ok(Started::Attached)
    }
    fn health(&self) -> io::Result<()> {
        // The protocol handshake is the daemon's ping: it authenticates and reports the
        // service epoch, and has no effect. The read-only client allows nothing else.
        let client = crate::browser_client::Client::connect_readonly(&self.root, "supervisor")?;
        *lock(&self.epoch) = Some(client.epoch.clone());
        Ok(())
    }
    fn instance(&self) -> Option<String> {
        lock(&self.epoch).clone()
    }
}

/// The standalone runtime service at `KAGEHA_RUNTIME_SERVICE_URL`.
struct Runtime {
    pid: Mutex<Option<String>>,
}

impl Component for Runtime {
    fn name(&self) -> &str {
        "runtime"
    }
    fn start(&self) -> io::Result<Started> {
        Ok(Started::Attached)
    }
    fn health(&self) -> io::Result<()> {
        let client = kageha_runtime_service::bridge_client::Client::from_env()?
            .ok_or_else(|| io::Error::other("runtime service is not configured"))?;
        let info = client.info()?;
        *lock(&self.pid) = info["pid"].as_u64().map(|pid| pid.to_string());
        Ok(())
    }
    fn instance(&self) -> Option<String> {
        lock(&self.pid).clone()
    }
}

/// The `kageha-computer` helper child of `computer_host`.
struct Computer;

impl Component for Computer {
    fn name(&self) -> &str {
        "computer"
    }
    fn start(&self) -> io::Result<Started> {
        crate::computer_host::relaunch_helper()?;
        Ok(Started::Attached)
    }
    fn health(&self) -> io::Result<()> {
        if crate::computer_host::helper_down() {
            return Err(io::Error::other("the computer helper is not running"));
        }
        Ok(())
    }
    fn exited(&self) -> bool {
        crate::computer_host::helper_down()
    }
}

/// `kageha-shell-service` on a private Unix socket.
pub(crate) struct ShellService {
    binary: PathBuf,
    socket: PathBuf,
    toolchains: PathBuf,
    pid: Mutex<Option<String>>,
}

/// Unix socket paths are limited to about 104 bytes.
const MAX_SOCKET_PATH: usize = 100;

impl ShellService {
    pub(crate) fn new(binary: PathBuf, socket: PathBuf, toolchains: PathBuf) -> Self {
        Self { binary, socket, toolchains, pid: Mutex::new(None) }
    }

    /// `KAGEHA_SHELL_SERVICE_BIN` (else `kageha-shell-service` next to this executable),
    /// `KAGEHA_SHELL_SERVICE_SOCKET` (else `state/shell-service.sock`, or a name in the temp
    /// directory when that path is too long) and the shell Toolchain_Cache.
    fn from_env(state: &Path) -> io::Result<Self> {
        let binary = match std::env::var_os("KAGEHA_SHELL_SERVICE_BIN") {
            Some(path) => PathBuf::from(path),
            None => std::env::current_exe()?
                .parent()
                .ok_or_else(|| io::Error::other("executable has no directory"))?
                .join("kageha-shell-service"),
        };
        if !binary.is_absolute() || !binary.is_file() {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("{} is not an executable file", binary.display())));
        }
        let socket = match std::env::var_os("KAGEHA_SHELL_SERVICE_SOCKET") {
            Some(path) => PathBuf::from(path),
            None => {
                let socket = state.join("shell-service.sock");
                if socket.as_os_str().len() <= MAX_SOCKET_PATH {
                    socket
                } else {
                    use sha2::{Digest, Sha256};
                    let digest = Sha256::digest(state.as_os_str().as_encoded_bytes());
                    std::env::temp_dir().join(format!("kageha-shell-{}.sock", &format!("{digest:x}")[..16]))
                }
            }
        };
        if !socket.is_absolute() || socket.as_os_str().len() > MAX_SOCKET_PATH {
            return Err(io::Error::other("the shell service socket needs an absolute path of at most 100 bytes"));
        }
        let toolchains = kageha_workspace::shell::ensure_private_dir(&crate::ui_shell::toolchain_cache())?;
        Ok(Self::new(binary, socket, toolchains))
    }

    fn ping(&self, timeout: Duration) -> io::Result<Value> {
        let reply = crate::ui_shell_socket::ping(&self.socket, timeout)?;
        *lock(&self.pid) = reply["pid"].as_u64().map(|pid| pid.to_string());
        Ok(reply)
    }
}

impl Component for ShellService {
    fn name(&self) -> &str {
        "shell"
    }
    fn start(&self) -> io::Result<Started> {
        // A service started by an earlier server process kept running with its shells and
        // jobs: use it.
        if self.ping(Duration::from_secs(1)).is_ok() {
            return Ok(Started::Attached);
        }
        use std::os::unix::process::CommandExt;
        let mut child = Command::new(&self.binary)
            .arg("--socket")
            .arg(&self.socket)
            .arg("--toolchains")
            .arg(&self.toolchains)
            .stdin(Stdio::null())
            // Its own process group, so a signal to the server's group doesn't reach it.
            .process_group(0)
            .spawn()?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait()? {
                return Err(io::Error::other(format!("kageha-shell-service exited at start ({status})")));
            }
            if self.ping(Duration::from_millis(500)).is_ok() {
                return Ok(Started::Child(child));
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::new(io::ErrorKind::TimedOut, "kageha-shell-service did not answer within 10 s"));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    fn health(&self) -> io::Result<()> {
        self.ping(Duration::from_secs(3)).map(|_| ())
    }
    fn instance(&self) -> Option<String> {
        lock(&self.pid).clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    struct FakeClock {
        base: Instant,
        offset: Mutex<Duration>,
    }

    impl FakeClock {
        fn new() -> Arc<Self> {
            Arc::new(Self { base: Instant::now(), offset: Mutex::new(Duration::ZERO) })
        }
        fn advance(&self, by: Duration) {
            *lock(&self.offset) += by;
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> Instant {
            self.base + *lock(&self.offset)
        }
    }

    fn policy() -> Policy {
        Policy { health_timeout: Duration::from_millis(300), ..Policy::default() }
    }

    /// A process that exits right after it starts.
    struct Exiting {
        starts: AtomicU32,
        restarts: AtomicU32,
    }

    impl Component for Exiting {
        fn name(&self) -> &str {
            "exiting"
        }
        fn start(&self) -> io::Result<Started> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            Ok(Started::Child(Command::new("/bin/sh").args(["-c", "exit 3"]).spawn()?))
        }
        fn health(&self) -> io::Result<()> {
            Ok(())
        }
        fn on_restart(&self) {
            self.restarts.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Ticks until the component leaves `Healthy` (the child's exit is seen).
    fn tick_until_unhealthy(supervisor: &Supervisor, name: &str) {
        for _ in 0..400 {
            supervisor.tick();
            if supervisor.status(name) != Some(Status::Healthy) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("{name} stayed healthy");
    }

    #[test]
    fn exited_process_restarts_with_backoff_then_degrades() {
        let clock = FakeClock::new();
        let component = Arc::new(Exiting { starts: AtomicU32::new(0), restarts: AtomicU32::new(0) });
        let supervisor = Supervisor::new(policy(), clock.clone(), vec![component.clone()]);
        supervisor.tick();
        assert_eq!(component.starts.load(Ordering::SeqCst), 1);
        for (restart, delay) in [1u64, 2, 4, 8, 16].into_iter().enumerate() {
            tick_until_unhealthy(&supervisor, "exiting");
            assert_eq!(supervisor.status("exiting"), Some(Status::Restarting));
            // Not before the backoff delay.
            clock.advance(Duration::from_millis(delay * 1000 - 1));
            supervisor.tick();
            assert_eq!(component.starts.load(Ordering::SeqCst), restart as u32 + 1, "restart {restart} came early");
            clock.advance(Duration::from_millis(1));
            supervisor.tick();
            assert_eq!(component.starts.load(Ordering::SeqCst), restart as u32 + 2);
            assert_eq!(component.restarts.load(Ordering::SeqCst), restart as u32 + 1);
        }
        // Five restarts within ten minutes: the next exit marks it degraded.
        tick_until_unhealthy(&supervisor, "exiting");
        assert_eq!(supervisor.status("exiting"), Some(Status::Degraded { retry_after: Duration::from_secs(60) }));
        let events = supervisor.events_since(0);
        let kinds: Vec<_> = events.iter().map(|event| event.kind).collect();
        assert_eq!(kinds, [vec!["component_restarted"; 5], vec!["component_degraded"]].concat());
        assert_eq!(events[5].restarts_in_window, 5);
        assert_eq!(events[5].retry_after, Some(60));
        assert_eq!(events[0].reason, "process exited");
        assert!(supervisor.events_since(events[4].seq).len() == 1);
        // Tools get component_unavailable with retry_after.
        clock.advance(Duration::from_secs(20));
        let error = supervisor.require("exiting").unwrap_err();
        let body: Value = serde_json::from_str(&error.to_string()).unwrap();
        assert_eq!((body["error_code"].clone(), body["retry_after"].clone()), (json!("component_unavailable"), json!(40)));
        assert_eq!(supervisor.require("unknown").ok(), Some(()));
        // It is retried after 60 s; a successful start makes it usable again.
        clock.advance(Duration::from_secs(40));
        supervisor.tick();
        assert_eq!(component.starts.load(Ordering::SeqCst), 7);
        assert_eq!(supervisor.status("exiting"), Some(Status::Healthy));
        // Still six restarts in the window: the next exit degrades it again, and since it
        // had recovered, that is reported again.
        tick_until_unhealthy(&supervisor, "exiting");
        assert!(matches!(supervisor.status("exiting"), Some(Status::Degraded { .. })));
        let kinds: Vec<_> = supervisor.events_since(events[5].seq).iter().map(|event| event.kind).collect();
        assert_eq!(kinds, ["component_restarted", "component_degraded"]);
        supervisor.shutdown();
    }

    /// A helper whose health check fails while `failing` is set.
    struct Flaky {
        failing: AtomicBool,
        checks: AtomicU32,
        starts: AtomicU32,
        instance: Mutex<Option<String>>,
        restarts: AtomicU32,
    }

    impl Component for Flaky {
        fn name(&self) -> &str {
            "flaky"
        }
        fn start(&self) -> io::Result<Started> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            Ok(Started::Attached)
        }
        fn health(&self) -> io::Result<()> {
            self.checks.fetch_add(1, Ordering::SeqCst);
            if self.failing.load(Ordering::SeqCst) {
                Err(io::Error::other("refused"))
            } else {
                Ok(())
            }
        }
        fn instance(&self) -> Option<String> {
            lock(&self.instance).clone()
        }
        fn on_restart(&self) {
            self.restarts.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn two_failed_checks_restart_and_old_restarts_leave_the_window() {
        let clock = FakeClock::new();
        let flaky = Arc::new(Flaky {
            failing: AtomicBool::new(false),
            checks: AtomicU32::new(0),
            starts: AtomicU32::new(0),
            instance: Mutex::new(Some("epoch-1".into())),
            restarts: AtomicU32::new(0),
        });
        let supervisor = Supervisor::new(policy(), clock.clone(), vec![flaky.clone()]);
        supervisor.tick(); // start
        clock.advance(Duration::from_secs(10));
        supervisor.tick();
        assert_eq!(flaky.checks.load(Ordering::SeqCst), 1);
        // Checks run every 10 s, not on every tick.
        clock.advance(Duration::from_secs(9));
        supervisor.tick();
        assert_eq!(flaky.checks.load(Ordering::SeqCst), 1);
        flaky.failing.store(true, Ordering::SeqCst);
        clock.advance(Duration::from_secs(1));
        supervisor.tick();
        assert_eq!(supervisor.status("flaky"), Some(Status::Healthy), "one failure is not enough");
        clock.advance(Duration::from_secs(10));
        supervisor.tick();
        assert_eq!(supervisor.status("flaky"), Some(Status::Restarting));
        assert!(supervisor.events_since(0).is_empty());
        clock.advance(Duration::from_secs(1));
        flaky.failing.store(false, Ordering::SeqCst);
        supervisor.tick();
        assert_eq!(flaky.starts.load(Ordering::SeqCst), 2);
        assert_eq!(supervisor.status("flaky"), Some(Status::Healthy));
        let restarted = supervisor.events_since(0);
        assert_eq!(restarted.len(), 1);
        assert!(restarted[0].reason.starts_with("2 failed health checks in a row: refused"), "{}", restarted[0].reason);
        // A new instance seen by a check is a restart outside the supervisor.
        clock.advance(Duration::from_secs(10));
        supervisor.tick();
        *lock(&flaky.instance) = Some("epoch-2".into());
        clock.advance(Duration::from_secs(10));
        supervisor.tick();
        let events = supervisor.events_since(restarted[0].seq);
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].kind, "component_restarted");
        assert_eq!(flaky.restarts.load(Ordering::SeqCst), 2);
        // Ten minutes later the earlier restart no longer counts: the backoff is 1 s again.
        clock.advance(Duration::from_secs(600));
        flaky.failing.store(true, Ordering::SeqCst);
        supervisor.tick();
        clock.advance(Duration::from_secs(10));
        supervisor.tick();
        assert_eq!(supervisor.status("flaky"), Some(Status::Restarting));
        flaky.failing.store(false, Ordering::SeqCst);
        clock.advance(Duration::from_secs(1));
        supervisor.tick();
        assert_eq!(flaky.starts.load(Ordering::SeqCst), 3);
    }

    /// A helper whose health check hangs until released.
    struct Hung {
        release: Mutex<Option<mpsc::Receiver<()>>>,
        checks: AtomicU32,
    }

    impl Component for Hung {
        fn name(&self) -> &str {
            "hung"
        }
        fn start(&self) -> io::Result<Started> {
            Ok(Started::Attached)
        }
        fn health(&self) -> io::Result<()> {
            self.checks.fetch_add(1, Ordering::SeqCst);
            if let Some(release) = lock(&self.release).take() {
                let _ = release.recv_timeout(Duration::from_secs(30));
            }
            Ok(())
        }
    }

    #[test]
    fn a_hung_health_check_does_not_block_status_reads_or_other_components() {
        let clock = FakeClock::new();
        let (release, wait) = mpsc::channel();
        let hung = Arc::new(Hung { release: Mutex::new(Some(wait)), checks: AtomicU32::new(0) });
        let flaky = Arc::new(Flaky {
            failing: AtomicBool::new(false),
            checks: AtomicU32::new(0),
            starts: AtomicU32::new(0),
            instance: Mutex::new(None),
            restarts: AtomicU32::new(0),
        });
        let supervisor = Supervisor::new(policy(), clock.clone(), vec![hung.clone(), flaky.clone()]);
        supervisor.tick();
        clock.advance(Duration::from_secs(10));
        let ticking = {
            let supervisor = Arc::clone(&supervisor);
            std::thread::spawn(move || {
                let started = Instant::now();
                supervisor.tick();
                started.elapsed()
            })
        };
        std::thread::sleep(Duration::from_millis(50));
        let read = Instant::now();
        assert_eq!(supervisor.status("hung"), Some(Status::Healthy));
        assert!(supervisor.require("hung").is_ok());
        let _ = supervisor.snapshot();
        assert!(read.elapsed() < Duration::from_millis(50), "status read waited {:?}", read.elapsed());
        // The check gave up at the health timeout and the round went on to the next component.
        let took = ticking.join().unwrap();
        assert!(took >= Duration::from_millis(300) && took < Duration::from_secs(5), "{took:?}");
        assert_eq!(flaky.checks.load(Ordering::SeqCst), 1);
        // The next check, with the first still hung, fails without starting another: two
        // failures, so a restart is scheduled.
        clock.advance(Duration::from_secs(10));
        let second = Instant::now();
        supervisor.tick();
        assert!(second.elapsed() < Duration::from_millis(300));
        assert_eq!(hung.checks.load(Ordering::SeqCst), 1);
        assert_eq!(supervisor.status("hung"), Some(Status::Restarting));
        assert_eq!(supervisor.status("flaky"), Some(Status::Healthy));
        release.send(()).unwrap();
    }

    #[test]
    fn backoff_doubles_up_to_the_cap() {
        let policy = Policy::default();
        let delays: Vec<u64> = (0..9).map(|n| policy.backoff(n).as_secs()).collect();
        assert_eq!(delays, [1, 2, 4, 8, 16, 32, 60, 60, 60]);
        assert_eq!(policy.backoff(usize::MAX), Duration::from_secs(60));
    }

    #[test]
    fn shell_service_component_spawns_restarts_and_adopts() {
        // Uses the real binary when it has been built (cargo build -p kageha-workspace).
        let binary = std::env::current_exe().unwrap().parent().and_then(Path::parent)
            .map(|dir| dir.join("kageha-shell-service")).filter(|path| path.is_file());
        let Some(binary) = binary else {
            eprintln!("skipping: kageha-shell-service is not built");
            return;
        };
        if !kageha_workspace::shell::sandbox_available() {
            eprintln!("skipping: OS process sandbox unavailable");
            return;
        }
        let dir = tempfile::Builder::new().prefix("kg-sup").tempdir_in("/tmp").unwrap();
        let toolchains = kageha_workspace::shell::ensure_private_dir(&dir.path().join("tc")).unwrap();
        let socket = dir.path().join("s.sock");
        let service = Arc::new(ShellService::new(binary, socket.clone(), toolchains));
        let clock = FakeClock::new();
        let supervisor = Supervisor::new(policy(), clock.clone(), vec![service.clone()]);
        supervisor.tick();
        assert_eq!(supervisor.status("shell"), Some(Status::Healthy));
        let first = crate::ui_shell_socket::ping(&socket, Duration::from_secs(3)).unwrap()["pid"].as_u64().unwrap();
        // Kill it: the exit is seen and it is started again after 1 s.
        unsafe { libc::kill(first as i32, libc::SIGKILL) };
        tick_until_unhealthy(&supervisor, "shell");
        clock.advance(Duration::from_secs(1));
        supervisor.tick();
        let second = crate::ui_shell_socket::ping(&socket, Duration::from_secs(3)).unwrap()["pid"].as_u64().unwrap();
        assert_ne!(first, second);
        assert_eq!(supervisor.events_since(0)[0].kind, "component_restarted");
        // A second supervisor (a restarted server) adopts the running service.
        let adopting = Supervisor::new(policy(), clock.clone(), vec![Arc::new(ShellService::new(
            PathBuf::from("/nonexistent"), socket.clone(), dir.path().join("tc")))]);
        adopting.tick();
        assert_eq!(adopting.status("shell"), Some(Status::Healthy));
        clock.advance(Duration::from_secs(10));
        adopting.tick();
        assert_eq!(adopting.status("shell"), Some(Status::Healthy));
        supervisor.shutdown();
    }
}
