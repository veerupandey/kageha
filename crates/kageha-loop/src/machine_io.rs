//! Host-only transports to services listening inside a chat-owned Docker container.
use std::{
    collections::HashMap,
    io::{self, Write},
    net::{Ipv4Addr, Shutdown, TcpListener, TcpStream},
    process::{Command, Stdio},
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use kageha_contracts::execution::Control;

pub(crate) const SHELL_SOCKET: &str = "UNIX-CONNECT:/tmp/kageha-shell.sock";
const CDP: &str = "TCP4:127.0.0.1:9222";
const MAX_CONNECTIONS: usize = 16;

fn connect(container: &str, destination: &str) -> Command {
    let mut command = Command::new("docker");
    command.args([
        "exec",
        "-i",
        container,
        "socat",
        "-t",
        "3720",
        "STDIO",
        destination,
    ]);
    command
}

pub(crate) fn shell(
    container: &str,
    request: &serde_json::Value,
    deadline: Instant,
) -> io::Result<serde_json::Value> {
    let mut input = serde_json::to_vec(request)?;
    input.push(b'\n');
    let control = Control {
        cancelled: &AtomicBool::new(false),
        deadline: Some(deadline),
    };
    let output = kageha_workspace::process::run(
        &mut connect(container, SHELL_SOCKET),
        input,
        &control,
        1024 * 1024,
    )?;
    if !output.success {
        return Err(io::Error::other("The Docker shell service is unavailable"));
    }
    serde_json::from_slice(&output.bytes)
        .map_err(|_| io::Error::other("Invalid reply from the Docker shell service"))
}

#[derive(Default)]
struct Connections {
    stopped: AtomicBool,
    streams: Mutex<HashMap<u64, TcpStream>>,
}

struct Relay {
    port: u16,
    generation: String,
    connections: Arc<Connections>,
}

static RELAYS: LazyLock<Mutex<HashMap<String, Relay>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Clone)]
enum BrowserTransport {
    Docker(String),
    Linux(crate::linux_session::Endpoint, String, String),
}

impl BrowserTransport {
    fn command(&self) -> io::Result<Command> {
        match self {
            Self::Docker(container) => Ok(connect(container, CDP)),
            Self::Linux(endpoint, key, epoch) => endpoint.command(&["cdp", key, epoch], false),
        }
    }
}

fn cdp_port(key: &str, generation: &str, transport: BrowserTransport) -> io::Result<u16> {
    let mut relays = RELAYS.lock().unwrap_or_else(|error| error.into_inner());
    if let Some(relay) = relays.get(key).filter(|relay| relay.generation == generation) {
        return Ok(relay.port);
    }
    if let Some(relay) = relays.remove(key) { close(&relay); }
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();
    let connections = Arc::new(Connections::default());
    let active = connections.clone();
    std::thread::Builder::new()
        .name("machine-cdp".into())
        .spawn(move || {
            let mut next = 0;
            while !active.stopped.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        next += 1;
                        let mut streams = active
                            .streams
                            .lock()
                            .unwrap_or_else(|error| error.into_inner());
                        if streams.len() == MAX_CONNECTIONS
                            || active.stopped.load(Ordering::Acquire)
                        {
                            continue;
                        }
                        let Ok(shutdown) = stream.try_clone() else {
                            continue;
                        };
                        streams.insert(next, shutdown);
                        let connections = active.clone();
                        let transport = transport.clone();
                        let id = next;
                        std::thread::spawn(move || {
                            let _ = relay(&transport, stream);
                            connections
                                .streams
                                .lock()
                                .unwrap_or_else(|error| error.into_inner())
                                .remove(&id);
                        });
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        })?;
    relays.insert(key.into(), Relay { port, generation:generation.into(), connections });
    Ok(port)
}

fn debugger_endpoint(container: &str) -> io::Result<String> {
    let control = Control {
        cancelled: &AtomicBool::new(false),
        deadline: Some(Instant::now() + Duration::from_secs(5)),
    };
    let output = kageha_workspace::process::run(
        Command::new("docker").args([
            "exec",
            container,
            "curl",
            "-fsS",
            "--max-time",
            "3",
            "http://127.0.0.1:9222/json/version",
        ]),
        vec![],
        &control,
        16384,
    )?;
    if !output.success {
        return Err(io::Error::other(
            "Chromium is starting or disconnected in the Docker workstation",
        ));
    }
    let value: serde_json::Value = serde_json::from_slice(&output.bytes)?;
    let debugger = value["webSocketDebuggerUrl"]
        .as_str()
        .and_then(|address| address.strip_prefix("ws://127.0.0.1:9222/devtools/browser/"))
        .filter(|id| id.len() == 36 && uuid::Uuid::parse_str(id).is_ok())
        .ok_or_else(|| io::Error::other("Unexpected Docker Chromium debugger address"))?;
    let port = cdp_port(container, debugger, BrowserTransport::Docker(container.into()))?;
    Ok(format!("ws://127.0.0.1:{port}/devtools/browser/{debugger}"))
}

fn relay(transport: &BrowserTransport, mut stream: TcpStream) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    let mut reader = stream.try_clone()?;
    let mut child = transport.command()?
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut input = child.stdin.take().expect("piped stdin");
    let mut output = child.stdout.take().expect("piped stdout");
    std::thread::scope(|scope| {
        scope.spawn(move || {
            let _ = io::copy(&mut reader, &mut input);
        });
        let _ = io::copy(&mut output, &mut stream);
        let _ = stream.shutdown(Shutdown::Both);
    });
    let _ = child.kill();
    child.wait()?;
    Ok(())
}

pub(crate) fn release(container: &str) {
    let mut relays = RELAYS.lock().unwrap_or_else(|error| error.into_inner());
    if let Some(relay) = relays.remove(container) {
        close(&relay);
    }
}

fn close(relay: &Relay) {
    relay.connections.stopped.store(true, Ordering::Release);
    for stream in relay.connections.streams.lock().unwrap_or_else(|error| error.into_inner()).values() {
        let _ = stream.shutdown(Shutdown::Both);
    }
}

fn registry() -> Option<std::path::PathBuf> {
    std::env::var_os("KAGEHA_BROWSER_STATE_DIR")
        .map(std::path::PathBuf::from)
        .map(|root| root.join("machine-bindings"))
}

pub(crate) fn register(db: &crate::webui::Database, chat: &str, container: &str) -> io::Result<()> {
    let transfer = crate::docker_desktop::private_directory(db, chat, "machine-browser-transfer")?;
    let endpoint = debugger_endpoint(container)?;
    write_binding(chat, &serde_json::json!({"endpoint":endpoint,"transfer":transfer}))
}

pub(crate) fn register_linux(db: &crate::webui::Database, chat: &str, session: &crate::linux_session::Session, running: &crate::linux_session::Running) -> io::Result<()> {
    use crate::linux_session::Endpoint;
    let key = format!("linux-{chat}");
    let port = cdp_port(&key, &running.epoch, BrowserTransport::Linux(session.endpoint.clone(), session.key.clone(), running.epoch.clone()))?;
    let endpoint = format!("ws://127.0.0.1:{port}{}", running.browser);
    let transfer = crate::docker_desktop::private_directory(db, chat, "linux-browser-transfer")?;
    let (guest, remote) = match &session.endpoint {
        Endpoint::Local { .. } => (transfer.clone(), serde_json::Value::Null),
        Endpoint::Ssh(config) => (running.workspace.join(".kageha-browser-transfer"), serde_json::to_value(config)?),
    };
    write_binding(chat, &serde_json::json!({"endpoint":endpoint,"transfer":transfer,"guest_transfer":guest,"remote":remote}))
}

fn write_binding(chat: &str, binding: &serde_json::Value) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    if !crate::server::identifier(chat) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid Docker chat",
        ));
    }
    let dir =
        registry().ok_or_else(|| io::Error::other("The Rust browser service is not configured"))?;
    crate::browser_client::private(dir.parent().expect("browser root"))?;
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    crate::browser_client::private(&dir)?;
    let content = serde_json::to_vec(binding)?;
    let path = dir.join(chat);
    if std::fs::read(&path).is_ok_and(|old| old == content) {
        return Ok(());
    }
    let mut temporary = tempfile::NamedTempFile::new_in(&dir)?;
    temporary.write_all(&content)?;
    temporary.persist(path)?;
    Ok(())
}

pub(crate) fn unregister(chat: &str) -> io::Result<()> {
    release(&format!("linux-{chat}"));
    if let Some(dir) = registry().filter(|_| crate::server::identifier(chat)) {
        match std::fs::remove_file(dir.join(chat)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
