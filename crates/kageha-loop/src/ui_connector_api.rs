//! Local connector API (design section 3, Requirement 1.4).
//!
//! While a chat turn is active the Host listens on `workspace/.kageha/api.sock`, a Unix
//! socket with mode 0600, so scripts in the Task_Workspace can read connected accounts
//! (Gmail, GitHub, LinkedIn) without connector tokens ever entering the shell.
//!
//! Protocol: one JSON object per line in each direction. A request is
//! `{"token", "integration", "operation", "query"?, "id"?, "path"?, "refresh"?}`; the reply
//! is `{"ok":true,"result":...}` or `{"ok":false,"error":{"code","message"}}`. Only the
//! read operations `status`, `search`, `message`, `thread` and `path` are accepted; any
//! other operation is `operation_not_allowed`.
//!
//! The capability token is per turn. It is written to `workspace/.kageha/api-token`
//! (mode 0600), because the shell environment drops every `*_TOKEN` variable; the shell only
//! gets the non-secret `KAGEHA_API_SOCKET` path. When the turn ends the token is revoked in
//! memory, its file is removed and the socket is closed and unlinked.
//!
//! Every read goes through [`crate::ui_tools::integration_read`], the body of the
//! `integration_read` tool, after the same specialist and managed-policy gates the tool
//! dispatch applies. So access checks, agent assignment and connector revisions are
//! identical for scripts and the tool.
use crate::webui::Database;
use kageha_workspace::task::{TaskWorkspace, API_TOKEN};
#[cfg(test)]
use kageha_workspace::task::{API_SOCKET, STATE_DIR};
use serde_json::{json, Value};
use std::{
    fs,
    io::{self, BufRead, BufReader, Write},
    os::unix::{
        fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread::JoinHandle,
    time::Duration,
};

/// The operations the API accepts. All are reads.
pub(crate) const READ_OPERATIONS: [&str; 5] = ["status", "search", "message", "thread", "path"];
/// Longest accepted request line.
const REQUEST_BYTES: usize = 64 * 1024;
/// Replies larger than this are written to `.kageha/tool-output/` and the reply names the file.
pub(crate) const RESPONSE_BYTES: usize = 4 * 1024 * 1024;
/// Simultaneous client connections.
const MAX_CONNECTIONS: usize = 16;
const POLL: Duration = Duration::from_millis(20);
const READ_POLL: Duration = Duration::from_millis(250);

/// Whose access the API reads with: the turn's chat, agent and cancel flag. `turn` selects
/// the request whose read cache the API shares with the `integration_read` tool.
pub(crate) struct Scope {
    pub db: Database,
    pub chat: String,
    pub turn: String,
    pub persona: Value,
    pub cancel: Arc<AtomicBool>,
}

struct Shared {
    scope: Scope,
    workspace: TaskWorkspace,
    /// The current capability; `None` once revoked.
    token: Mutex<Option<String>>,
    stop: AtomicBool,
    connections: AtomicUsize,
    spills: AtomicUsize,
}

/// A running connector API for one turn. Dropping it (or [`ConnectorApi::stop`]) revokes
/// the token, removes its file, stops the listener and unlinks the socket.
pub(crate) struct ConnectorApi {
    shared: Arc<Shared>,
    socket: PathBuf,
    /// `(dev, ino)` of the bound socket; only that file is unlinked at the end.
    identity: (u64, u64),
    thread: Option<JoinHandle<()>>,
}

#[derive(Debug)]
struct ApiError {
    code: &'static str,
    message: String,
}

fn api_error(code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError { code, message: message.into() }
}

/// Starts the API for the chat's Task_Workspace. Fails, leaving any existing socket alone,
/// when a live server is already bound there or the path is not a socket.
pub(crate) fn start(workspace: TaskWorkspace, scope: Scope) -> io::Result<ConnectorApi> {
    let token = new_token();
    let (listener, socket, identity) = bind(workspace.state_dir())?;
    if let Err(error) = workspace.write_host_file(API_TOKEN, token.as_bytes()) {
        let _ = fs::remove_file(&socket);
        return Err(error.into());
    }
    listener.set_nonblocking(true)?;
    let shared = Arc::new(Shared {
        scope,
        workspace,
        token: Mutex::new(Some(token)),
        stop: AtomicBool::new(false),
        connections: AtomicUsize::new(0),
        spills: AtomicUsize::new(0),
    });
    let worker = shared.clone();
    let thread = std::thread::Builder::new()
        .name("kageha-connector-api".into())
        .spawn(move || accept_loop(&listener, &worker))?;
    Ok(ConnectorApi { shared, socket, identity, thread: Some(thread) })
}

impl ConnectorApi {
    /// Root of the workspace this API serves.
    #[cfg(test)]
    fn root(&self) -> &Path {
        self.shared.workspace.root()
    }

    /// Revokes the token and shuts the server down. Idempotent.
    pub(crate) fn stop(&mut self) {
        *self.shared.token.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let _ = self.shared.workspace.remove_host_file(API_TOKEN);
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // Unlink only the socket this server bound, inside a real `.kageha` directory.
        let state = self.shared.workspace.state_dir();
        let real_dir = fs::symlink_metadata(&state).is_ok_and(|meta| meta.is_dir());
        if real_dir && fs::symlink_metadata(&self.socket).is_ok_and(|meta| (meta.dev(), meta.ino()) == self.identity) {
            let _ = fs::remove_file(&self.socket);
        }
    }
}

impl Drop for ConnectorApi {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Starts the API for a local turn, or returns `None` (with a log line) when it can't run.
/// The turn continues without the API in that case; the `integration_read` tool still works.
pub(crate) fn start_for_turn(scope: Scope) -> Option<ConnectorApi> {
    if !crate::ui_shell::available() {
        return None;
    }
    let started = crate::ui_workspace::open_for_chat(&scope.db, &scope.chat)
        .and_then(|workspace| start(workspace, scope));
    match started {
        Ok(api) => Some(api),
        Err(error) => {
            eprintln!("kageha: local connector API unavailable for this turn: {error}");
            None
        }
    }
}

/// 256 random bits as hex.
fn new_token() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

fn same_token(expected: &str, given: &str) -> bool {
    expected.len() == given.len()
        && expected.bytes().zip(given.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

/// Binds `.kageha/api.sock` with mode 0600. The socket is bound inside a fresh 0700
/// directory, set to 0600, then renamed into place, so it is never reachable with a wider
/// mode and no process-wide umask change is needed. A stale socket (nobody listening) is
/// replaced; a live one or a non-socket file is refused.
fn bind(state: &Path) -> io::Result<(UnixListener, PathBuf, (u64, u64))> {
    if !fs::symlink_metadata(state)?.is_dir() {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "Workspace state directory is not a real directory"));
    }
    let alias = Alias::new(state)?;
    let socket = state.join(SOCKET_NAME);
    match fs::symlink_metadata(&socket) {
        Ok(meta) if !meta.file_type().is_socket() => {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, "The connector API path exists and is not a socket"));
        }
        Ok(_) => match UnixStream::connect(alias.base.join(SOCKET_NAME)) {
            Ok(_) => return Err(io::Error::new(io::ErrorKind::AddrInUse, "A connector API is already serving this workspace")),
            Err(error) if matches!(error.kind(), io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound) => fs::remove_file(&socket)?,
            Err(error) => return Err(error),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let name = (0..8)
        .map(|_| format!(".a{}", &uuid::Uuid::new_v4().simple().to_string()[..8]))
        .find(|name| fs::DirBuilder::new().mode(0o700).create(state.join(name)).is_ok())
        .ok_or_else(|| io::Error::other("Could not create a private directory for the connector API socket"))?;
    let private = state.join(&name);
    let bound = (|| {
        let staged = private.join("s");
        let listener = UnixListener::bind(alias.base.join(&name).join("s"))?;
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o600))?;
        fs::rename(&staged, &socket)?;
        let meta = fs::symlink_metadata(&socket)?;
        if !meta.file_type().is_socket() || meta.permissions().mode() & 0o777 != 0o600 {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "Connector API socket has an unexpected mode"));
        }
        Ok((listener, socket.clone(), (meta.dev(), meta.ino())))
    })();
    let _ = fs::remove_file(private.join("s"));
    let _ = fs::remove_dir(&private);
    bound
}

/// File name of `task::API_SOCKET` inside the state directory.
const SOCKET_NAME: &str = "api.sock";
/// `sockaddr_un` path capacity, including the terminating NUL.
const SUN_PATH: usize = if cfg!(target_os = "macos") { 104 } else { 108 };
/// Room needed below the state directory for the staged socket path (`/.aXXXXXXXX/s`).
const STAGED_SUFFIX: usize = 16;

/// A short path to the state directory for socket calls. Socket addresses are limited to
/// [`SUN_PATH`] bytes, which a deep workspace path can exceed. Then a symlink to the
/// directory is made inside a fresh 0700 directory under the system temp directory, and
/// is removed when this is dropped. Otherwise `base` is the directory itself.
struct Alias {
    dir: Option<PathBuf>,
    base: PathBuf,
}

impl Alias {
    fn new(state: &Path) -> io::Result<Self> {
        if state.as_os_str().len() + STAGED_SUFFIX < SUN_PATH {
            return Ok(Self { dir: None, base: state.to_path_buf() });
        }
        let dir = (0..8)
            .map(|_| std::env::temp_dir().join(format!("kg-{}", &uuid::Uuid::new_v4().simple().to_string()[..10])))
            .find(|dir| fs::DirBuilder::new().mode(0o700).create(dir).is_ok())
            .ok_or_else(|| io::Error::other("Could not create a private directory for the connector API socket"))?;
        let alias = Self { dir: Some(dir.clone()), base: dir.join("w") };
        std::os::unix::fs::symlink(state, &alias.base)?;
        if alias.base.as_os_str().len() + STAGED_SUFFIX >= SUN_PATH {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "The temp directory path is too long for a socket"));
        }
        Ok(alias)
    }
}

impl Drop for Alias {
    fn drop(&mut self) {
        if let Some(dir) = &self.dir {
            let _ = fs::remove_file(&self.base);
            let _ = fs::remove_dir(dir);
        }
    }
}

/// Connects to a workspace's connector API socket from the Host process, whatever the
/// length of its path.
#[cfg(test)]
fn connect(workspace_root: &Path) -> io::Result<UnixStream> {
    let alias = Alias::new(&workspace_root.join(STATE_DIR))?;
    UnixStream::connect(alias.base.join(SOCKET_NAME))
}

fn accept_loop(listener: &UnixListener, shared: &Arc<Shared>) {
    while !shared.stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                // Accepted sockets inherit the listener's non-blocking mode on BSDs.
                if stream.set_nonblocking(false).is_err() {
                    continue;
                }
                if shared.connections.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                    shared.connections.fetch_sub(1, Ordering::SeqCst);
                    let mut stream = stream;
                    let _ = reply(&mut stream, &failure(&api_error("busy", "Too many open connector API connections. Retry after others close.")));
                    continue;
                }
                let worker = shared.clone();
                let spawned = std::thread::Builder::new()
                    .name("kageha-connector-api-conn".into())
                    .spawn(move || {
                        let _ = serve(stream, &worker);
                        worker.connections.fetch_sub(1, Ordering::SeqCst);
                    });
                if spawned.is_err() {
                    shared.connections.fetch_sub(1, Ordering::SeqCst);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(POLL),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => std::thread::sleep(POLL),
        }
    }
}

/// Serves request lines on one connection until the client closes or the server stops.
fn serve(stream: UnixStream, shared: &Shared) -> io::Result<()> {
    stream.set_read_timeout(Some(READ_POLL))?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let mut line = Vec::new();
    loop {
        if shared.stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        match reader.read_until(b'\n', &mut line) {
            Ok(0) => return Ok(()),
            Ok(_) if line.last() != Some(&b'\n') && line.len() <= REQUEST_BYTES => continue,
            Ok(_) => {}
            Err(error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted) => {
                if line.len() <= REQUEST_BYTES {
                    continue;
                }
            }
            Err(error) => return Err(error),
        }
        if line.len() > REQUEST_BYTES {
            reply(&mut writer, &failure(&api_error("request_too_large", "Connector API requests are limited to 64 KB")))?;
            return Ok(());
        }
        let response = match serde_json::from_slice::<Value>(&line) {
            Ok(request) if request.is_object() => respond(shared, &request),
            _ => failure(&api_error("invalid_request", "Send one JSON object per line")),
        };
        line.clear();
        reply(&mut writer, &response)?;
    }
}

fn reply(stream: &mut UnixStream, response: &Value) -> io::Result<()> {
    let mut text = response.to_string();
    text.push('\n');
    stream.write_all(text.as_bytes())?;
    stream.flush()
}

fn failure(error: &ApiError) -> Value {
    json!({"ok":false,"error":{"code":error.code,"message":error.message}})
}

/// Handles one request: token, then operation, then the tool gates, then the read.
fn respond(shared: &Shared, request: &Value) -> Value {
    let result = (|| -> Result<Value, ApiError> {
        authorize(shared, request)?;
        let (args, refresh) = read_arguments(request)?;
        gate(&shared.scope)?;
        let text = connector_read(&shared.scope, &args, refresh)
            .map_err(|error| api_error("read_failed", error.to_string()))?;
        let result = serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text));
        Ok(json!({"ok":true,"result":result}))
    })();
    let response = match result {
        Ok(response) => response,
        Err(error) => return failure(&error),
    };
    bounded(shared, response)
}

fn authorize(shared: &Shared, request: &Value) -> Result<(), ApiError> {
    let current = shared.token.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let given = request["token"].as_str().unwrap_or("");
    match current {
        Some(token) if same_token(&token, given) => Ok(()),
        _ => Err(api_error("unauthorized", "Missing, wrong or expired token. Read the current token from .kageha/api-token.")),
    }
}

/// The `integration_read` arguments for a request, and its `refresh` flag.
fn read_arguments(request: &Value) -> Result<(Value, bool), ApiError> {
    let integration = request["integration"].as_str()
        .filter(|name| (1..=64).contains(&name.len()))
        .ok_or_else(|| api_error("invalid_request", "Name the integration, for example gmail or github"))?;
    let operation = request["operation"].as_str()
        .ok_or_else(|| api_error("invalid_request", "Name the operation: status, search, message, thread or path"))?;
    if !READ_OPERATIONS.contains(&operation) {
        return Err(api_error("operation_not_allowed", format!(
            "The local connector API is read-only and accepts status, search, message, thread or path, not {operation}. Sends and other actions go through the Host's action tools."
        )));
    }
    let refresh = match &request["refresh"] {
        Value::Null => false,
        Value::Bool(value) => *value,
        _ => return Err(api_error("invalid_request", "refresh must be true or false")),
    };
    let mut args = json!({"integration":integration});
    if operation == "path" {
        args["path"] = json!(request["path"].as_str()
            .ok_or_else(|| api_error("invalid_request", "The path operation needs a path"))?);
    } else {
        args["operation"] = json!(operation);
        for field in ["query", "id"] {
            if !request[field].is_null() {
                args[field] = request[field].clone();
            }
        }
    }
    Ok((args, refresh))
}

/// The checks the tool dispatch applies to an `integration_read` call before its body:
/// the agent's specialist tool policy and the managed approval policy. Calls that would
/// need a review prompt are refused, because a script cannot answer one.
fn gate(scope: &Scope) -> Result<(), ApiError> {
    const TOOL: &str = "integration_read";
    if scope.cancel.load(Ordering::SeqCst) {
        return Err(api_error("cancelled", "The turn was cancelled"));
    }
    let policies = kageha_policy::specialist::policies(&scope.persona)
        .map_err(|error| api_error("policy_denied", error.to_string()))?;
    let decision = kageha_policy::specialist::evaluate(&policies, TOOL)
        .map_err(|error| api_error("policy_denied", error.to_string()))?;
    if !decision.allowed {
        return Err(api_error("tool_not_allowed", "integration_read is excluded by this specialist or its delegating agent"));
    }
    let policy = {
        let db = scope.db.lock().map_err(|_| api_error("unavailable", "Database unavailable"))?;
        crate::ui_approvals::owned(&db).map_err(|error| api_error("unavailable", error.to_string()))?
    };
    policy.profile(&scope.chat)
        .map_err(|_| api_error("policy_denied", "Permission scope is no longer available"))?;
    policy.check_action(TOOL).map_err(|error| api_error("policy_denied", error.to_string()))?;
    if decision.review || policy.requires_manual(TOOL) {
        return Err(api_error("approval_required", "integration_read needs approval for this agent. Call the integration_read tool instead."));
    }
    Ok(())
}

/// The single read path for API requests, through the request's read cache: the key is
/// the one the `integration_read` tool uses for the same arguments, so reads by scripts
/// and by the tool share entries. `refresh` reads again and overwrites the entry.
fn connector_read(scope: &Scope, args: &Value, refresh: bool) -> io::Result<String> {
    let agent = scope.persona["id"].as_str().unwrap_or("main");
    let read = || crate::ui_tools::integration_read(&scope.db, agent, args, &scope.cancel);
    let request = crate::ui_task_completion::scope(&scope.db, &scope.turn);
    let revision = crate::ui_read_cache::connector_revision(&scope.db, agent, args["integration"].as_str().unwrap_or(""));
    let (Ok(request), Ok(revision)) = (request, revision) else { return read() };
    let mut keyed = args.clone();
    if refresh {
        keyed["refresh"] = Value::Bool(true);
    }
    crate::ui_read_cache::Lookup::new(&scope.db, &request, "integration_read", &keyed, &revision).read(read)
}

/// A reply within [`RESPONSE_BYTES`]; a larger one is saved to `.kageha/tool-output/` and
/// the reply names the file.
fn bounded(shared: &Shared, response: Value) -> Value {
    let text = response.to_string();
    if text.len() <= RESPONSE_BYTES {
        return response;
    }
    let n = shared.spills.fetch_add(1, Ordering::SeqCst);
    let path = format!(".kageha/tool-output/api-{}-{n}.json", &uuid::Uuid::new_v4().simple().to_string()[..12]);
    match shared.workspace.write_host_file(&path, text.as_bytes()) {
        Ok(_) => json!({"ok":true,"spilled_path":path,"original_bytes":text.len(),
            "note":"The reply was larger than 4 MB. The full reply JSON is saved at spilled_path, relative to the workspace."}),
        Err(error) => failure(&api_error("response_too_large", format!("The reply was larger than 4 MB and could not be saved: {error}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webui;

    const CHAT: &str = "chatapi00001";
    const TURN: &str = "chatapi00001/request-1";

    struct Fixture {
        _directory: tempfile::TempDir,
        db: Database,
        cancel: Arc<AtomicBool>,
    }

    fn fixture() -> Fixture {
        let directory = tempfile::tempdir().unwrap();
        let connection = rusqlite::Connection::open(directory.path().join("native.sqlite")).unwrap();
        connection.execute_batch("CREATE TABLE native_chat_turns(sequence INTEGER PRIMARY KEY,chat_id TEXT,request_id TEXT,message TEXT,answer TEXT,status TEXT);").unwrap();
        webui::initialize(&connection).unwrap();
        connection.execute("INSERT INTO ui_sessions(id,title,created_at,updated_at) VALUES (?,?,0,0)", [CHAT, "API"]).unwrap();
        crate::ui_approvals::main_default(&connection, CHAT, "main").unwrap();
        let db = crate::agent_host::DatabaseState::open(connection).unwrap();
        Fixture { _directory: directory, db, cancel: Arc::new(AtomicBool::new(false)) }
    }

    impl Fixture {
        fn workspace(&self) -> TaskWorkspace {
            crate::ui_workspace::open_for_chat(&self.db, CHAT).unwrap()
        }
        fn start(&self, persona: Value) -> io::Result<ConnectorApi> {
            start(self.workspace(), Scope { db: self.db.clone(), chat: CHAT.into(), turn: TURN.into(), persona, cancel: self.cancel.clone() })
        }
        fn token(&self) -> String {
            fs::read_to_string(self.workspace().root().join(API_TOKEN)).unwrap()
        }
    }

    /// One request and its reply over a fresh connection.
    fn call(api: &ConnectorApi, request: Value) -> Value {
        let mut stream = connect(api.root()).unwrap();
        stream.write_all(format!("{request}\n").as_bytes()).unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    #[test]
    fn socket_and_token_file_are_private_and_removed_when_the_turn_ends() {
        let fixture = fixture();
        let mut api = fixture.start(json!({"id":"main"})).unwrap();
        let root = fixture.workspace().root().to_path_buf();
        let socket = fs::symlink_metadata(root.join(API_SOCKET)).unwrap();
        assert!(socket.file_type().is_socket());
        assert_eq!(socket.permissions().mode() & 0o777, 0o600);
        let token = fs::metadata(root.join(API_TOKEN)).unwrap();
        assert_eq!(token.permissions().mode() & 0o777, 0o600);
        assert_eq!(fixture.token().len(), 64);
        // No staging directory is left behind and host files are not workspace changes.
        assert!(fs::read_dir(root.join(STATE_DIR)).unwrap().flatten()
            .all(|entry| !entry.file_name().to_string_lossy().starts_with(".a")));
        assert!(fixture.workspace().scan_changes().unwrap().is_empty());
        // A second server for the same workspace is refused and leaves the live one alone.
        let second = fixture.start(json!({"id":"main"})).err().unwrap();
        assert_eq!(second.kind(), io::ErrorKind::AddrInUse);
        let old = fixture.token();
        assert_eq!(call(&api, json!({"token":old,"integration":"gmail","operation":"status"}))["ok"], true);
        api.stop();
        assert!(!root.join(API_TOKEN).exists());
        assert!(!root.join(API_SOCKET).exists());
        // A new turn gets a new token; the old one no longer works.
        let api = fixture.start(json!({"id":"main"})).unwrap();
        assert_ne!(fixture.token(), old);
        assert_eq!(call(&api, json!({"token":old,"integration":"gmail","operation":"status"}))["error"]["code"], "unauthorized");
    }

    #[test]
    fn a_stale_socket_is_replaced_but_other_files_are_not() {
        let fixture = fixture();
        let root = fixture.workspace().root().to_path_buf();
        let alias = Alias::new(&root.join(STATE_DIR)).unwrap();
        drop(UnixListener::bind(alias.base.join(SOCKET_NAME)).unwrap());
        assert!(fs::symlink_metadata(root.join(API_SOCKET)).unwrap().file_type().is_socket());
        let api = fixture.start(json!({"id":"main"})).unwrap();
        drop(api);
        fs::write(root.join(API_SOCKET), b"not a socket").unwrap();
        assert_eq!(fixture.start(json!({"id":"main"})).err().unwrap().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(root.join(API_SOCKET)).unwrap(), b"not a socket");
        assert!(!root.join(API_TOKEN).exists());
    }

    #[test]
    fn wrong_or_missing_tokens_and_write_operations_are_rejected() {
        let fixture = fixture();
        let api = fixture.start(json!({"id":"main"})).unwrap();
        let token = fixture.token();
        for request in [
            json!({"integration":"gmail","operation":"status"}),
            json!({"token":"","integration":"gmail","operation":"status"}),
            json!({"token":format!("{}x", &token[..63]),"integration":"gmail","operation":"status"}),
            json!({"token":42,"integration":"gmail","operation":"send"}),
        ] {
            let reply = call(&api, request.clone());
            assert_eq!(reply["ok"], false, "{request}");
            assert_eq!(reply["error"]["code"], "unauthorized", "{request}");
        }
        for operation in ["send", "draft", "delete", "modify", "write", "STATUS"] {
            let reply = call(&api, json!({"token":token,"integration":"gmail","operation":operation,"id":"m1"}));
            assert_eq!(reply["error"]["code"], "operation_not_allowed", "{operation}");
        }
        assert_eq!(call(&api, json!({"token":token,"integration":"gmail"}))["error"]["code"], "invalid_request");
        // Malformed lines get an error and the connection keeps serving.
        let mut stream = connect(api.root()).unwrap();
        stream.write_all(format!("not json\n{}\n", json!({"token":token,"integration":"gmail","operation":"status"})).as_bytes()).unwrap();
        let mut lines = BufReader::new(stream).lines();
        let first: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
        let second: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
        assert_eq!(first["error"]["code"], "invalid_request");
        assert_eq!(second["ok"], true);
    }

    #[test]
    fn reads_go_through_the_integration_read_tool_body() {
        let fixture = fixture();
        let api = fixture.start(json!({"id":"main"})).unwrap();
        let token = fixture.token();
        let cancel = AtomicBool::new(false);
        // Gmail is not connected: the same recovery envelope as the tool, including the
        // connector revision and outcome.
        for (request, args) in [
            (json!({"operation":"search","query":"Seequent interview"}), json!({"operation":"search","query":"Seequent interview"})),
            (json!({"operation":"status"}), json!({"operation":"status"})),
            (json!({"operation":"thread","id":"t1"}), json!({"operation":"thread","id":"t1"})),
        ] {
            let mut request = request;
            request["token"] = json!(token);
            request["integration"] = json!("gmail");
            let mut args = args;
            args["integration"] = json!("gmail");
            let expected: Value = serde_json::from_str(&crate::ui_tools::integration_read(&fixture.db, "main", &args, &cancel).unwrap()).unwrap();
            assert_eq!(expected["status"], "disconnected");
            let reply = call(&api, request.clone());
            assert_eq!(reply, json!({"ok":true,"result":expected}), "{request}");
            assert_eq!(reply["result"]["connector_outcome"]["recovery"], "manage_access");
        }
        // The tool's own argument errors and access refusals come back as read failures.
        let expected = crate::ui_tools::integration_read(&fixture.db, "main", &json!({"integration":"github","path":"repos/o/r/issues"}), &cancel).unwrap_err().to_string();
        let reply = call(&api, json!({"token":token,"integration":"github","operation":"path","path":"repos/o/r/issues"}));
        assert_eq!(reply["error"], json!({"code":"read_failed","message":expected}));
        let expected = crate::ui_tools::integration_read(&fixture.db, "main", &json!({"integration":"gmail","operation":"message","id":"../profile"}), &cancel).unwrap_err().to_string();
        let reply = call(&api, json!({"token":token,"integration":"gmail","operation":"message","id":"../profile"}));
        assert_eq!(reply["error"], json!({"code":"read_failed","message":expected}));
    }

    #[test]
    fn reads_share_the_tools_read_cache_and_refresh_reads_again() {
        let fixture = fixture();
        let api = fixture.start(json!({"id":"main"})).unwrap();
        let token = fixture.token();
        // An entry stored by the integration_read tool for the same request and arguments.
        let args = json!({"integration":"gmail","operation":"message","id":"m1"});
        let revision = crate::ui_read_cache::connector_revision(&fixture.db, "main", "gmail").unwrap();
        let scope = crate::ui_task_completion::scope(&fixture.db, TURN).unwrap();
        crate::ui_read_cache::Lookup::new(&fixture.db, &scope, "integration_read", &args, &revision)
            .store(&json!({"status":"ok","data":{"id":"m1","subject":"Interview"}}).to_string());
        let request = json!({"token":token,"integration":"gmail","operation":"message","id":"m1"});
        let reply = call(&api, request.clone());
        assert_eq!(reply["result"]["data"]["subject"], "Interview", "{reply}");
        assert_eq!(reply["result"]["cached"], true);
        // refresh reads through: Gmail is not connected here, so the live envelope comes back
        // and, being a blocked outcome, does not replace the entry.
        let mut refresh = request.clone();
        refresh["refresh"] = json!(true);
        let reply = call(&api, refresh);
        assert_eq!(reply["result"]["status"], "disconnected", "{reply}");
        assert!(reply["result"]["cached"].is_null());
        assert_eq!(call(&api, request)["result"]["cached"], true);
    }

    #[test]
    fn the_agents_tool_policy_applies_to_the_api() {
        let fixture = fixture();
        let api = fixture.start(json!({"id":"research","tool_policy":{"deny":["integration_read"]}})).unwrap();
        let reply = call(&api, json!({"token":fixture.token(),"integration":"gmail","operation":"status"}));
        assert_eq!(reply["error"]["code"], "tool_not_allowed");
    }

    #[test]
    fn a_sandboxed_shell_session_can_call_the_api() {
        use kageha_workspace::shell::{ShellPaths, ShellSession};
        if !kageha_workspace::shell::sandbox_available() {
            eprintln!("skipping: OS process sandbox unavailable");
            return;
        }
        let fixture = fixture();
        let _api = fixture.start(json!({"id":"main"})).unwrap();
        let workspace = fixture.workspace();
        let toolchains = fixture._directory.path().join("toolchains");
        let paths = ShellPaths::prepare(workspace.root(), &toolchains).unwrap();
        let mut shell = ShellSession::open(paths).unwrap();
        let output = shell.run(
            r#"test "$KAGEHA_API_SOCKET" = "$PWD/.kageha/api.sock" && kageha --json gmail status"#,
            "api-call",
            Duration::from_secs(30),
        ).unwrap();
        assert_eq!(output.exit_code, 0, "{output:?}");
        let result: Value = serde_json::from_str(output.stdout.trim()).expect("connector CLI must return JSON");
        assert_eq!(result["status"], "disconnected");
    }
}
