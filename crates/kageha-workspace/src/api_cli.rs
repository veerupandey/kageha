//! `kageha`: the command-line shim for the local connector API (design section 3,
//! Requirement 1.4).
//!
//! Scripts in the Task_Workspace run `kageha gmail search "<q>"` instead of speaking the
//! socket protocol themselves. The shim sends one JSON-lines request to
//! `.kageha/api.sock` and prints the reply's `result`.
//!
//! This file uses only `std`. It is compiled twice: as the `kageha_workspace::api_cli`
//! module (for tests and for hosts that want the parser), and by `build.rs` as a
//! standalone executable whose bytes are embedded in the library
//! ([`crate::shell::cli_shim`]) and installed as `~/.kageha/toolchains/bin/kageha` when a
//! shell session starts. So the shim has no runtime dependency and the Host never has to
//! find a separate file on disk.
//!
//! Connecting: socket addresses are limited to about 104 bytes, so the shim changes to
//! `$KAGEHA_WORKSPACE` and connects with the relative path `.kageha/api.sock`. If that
//! variable is missing or the socket isn't there, it falls back to `$KAGEHA_API_SOCKET`,
//! changing to that socket's directory first. The per-turn token is read fresh from
//! `api-token` next to the socket on every invocation and is never printed.
//!
//! Exit codes:
//!
//! - `0`: the API answered `ok:true`. This includes a read whose result reports
//!   `"status":"disconnected"`, which `integration_read` also treats as a successful read
//!   (the result carries the recovery steps). The shim adds a one-line note on stderr.
//! - `1`: the API answered `ok:false`. stderr is `kageha: <code>: <message>`.
//! - `2`: usage error. Nothing was sent.
//! - `3`: the API could not be reached or its reply could not be used (`api_unavailable`,
//!   `token_unavailable`, `invalid_reply`). stderr names the code.
use std::{
    env,
    fs,
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::{Component, Path, PathBuf},
    time::Duration,
};

pub const EXIT_OK: i32 = 0;
pub const EXIT_API_ERROR: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_UNAVAILABLE: i32 = 3;

/// The read operations the API accepts.
pub const OPERATIONS: [&str; 5] = ["status", "search", "message", "thread", "path"];
/// How long to wait for a reply. Connector reads can be slow; the shell tool has its own
/// timeout on top of this.
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(600);
/// Longest reply line accepted. The API spills replies over 4 MB to a file.
const REPLY_LIMIT: u64 = 64 * 1024 * 1024;
/// Longest token file accepted.
const TOKEN_LIMIT: u64 = 4096;
/// Deepest JSON nesting the shim parses.
const MAX_DEPTH: usize = 512;
const SPILL_PREFIX: &str = ".kageha/tool-output/";

pub const USAGE: &str = "\
kageha: read connected accounts from scripts through the local connector API.

Usage:
  kageha [--json] [--refresh] gmail search <query>
  kageha [--json] [--refresh] gmail message <id>
  kageha [--json] [--refresh] gmail thread <id>
  kageha [--json] <integration> status
  kageha [--json] [--refresh] <integration> path <path>   (github, linkedin)
  kageha --help

Options (anywhere on the line; use -- before an argument that starts with --):
  --json      Print the result as compact single-line JSON (for scripts).
              The default is pretty-printed JSON.
  --refresh   Bypass the read cache and read from the connector again.
  -h, --help  Show this help.

Output: the read's result JSON on stdout. Several search words are joined with spaces.
The API is read-only. Sends and other actions go through the agent's action tools.
It runs only while a chat turn is active; the token is read from .kageha/api-token.

Exit codes:
  0  ok. A result with \"status\":\"disconnected\" is a successful read: check .status
     and follow the recovery steps in the result (a note is also printed on stderr).
  1  the API refused or failed the read; stderr is 'kageha: <code>: <message>'.
  2  usage error.
  3  the API is unreachable or its reply is unusable (api_unavailable,
     token_unavailable, invalid_reply).
";

/// One parsed invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Invocation {
    Help,
    Read { request: Request, compact: bool },
}

/// A connector API read request, without the token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub integration: String,
    pub operation: &'static str,
    pub query: Option<String>,
    pub id: Option<String>,
    pub path: Option<String>,
    pub refresh: bool,
}

/// Parses the arguments after the program name. `Err` is a usage message.
pub fn parse(args: &[String]) -> Result<Invocation, String> {
    let (mut compact, mut refresh, mut help, mut literal) = (false, false, false, false);
    let mut words: Vec<&str> = Vec::new();
    for arg in args {
        if literal {
            words.push(arg);
            continue;
        }
        match arg.as_str() {
            "--" => literal = true,
            "--json" => compact = true,
            "--refresh" => refresh = true,
            "-h" | "--help" => help = true,
            // A single dash is a normal argument: Gmail queries use `-label:x` for negation.
            option if option.starts_with("--") => {
                return Err(format!("Unknown option {option}. Options are --json, --refresh and --help."))
            }
            word => words.push(word),
        }
    }
    if help || words == ["help"] {
        return Ok(Invocation::Help);
    }
    let Some((&integration, rest)) = words.split_first() else {
        return Err("Name an integration and an operation, for example: kageha gmail search \"from:alice\"".into());
    };
    let valid_name = (1..=64).contains(&integration.len())
        && integration.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'));
    if !valid_name || integration.starts_with('-') {
        return Err(format!("'{integration}' is not an integration name, for example gmail, github or linkedin."));
    }
    let Some((&operation, rest)) = rest.split_first() else {
        return Err(format!("Name an operation for {integration}: status, search, message, thread or path."));
    };
    let Some(operation) = OPERATIONS.iter().copied().find(|op| *op == operation) else {
        return Err(format!(
            "Unknown operation '{operation}'. The local connector API is read-only: status, search, message, thread or path. Sends and other actions go through the agent's action tools."
        ));
    };
    let mut request = Request {
        integration: integration.to_string(),
        operation,
        query: None,
        id: None,
        path: None,
        refresh,
    };
    let one = |what: &str| -> Result<String, String> {
        match rest {
            [value] if !value.is_empty() => Ok(value.to_string()),
            [] | [_] => Err(format!("{integration} {operation} needs one {what}.")),
            _ => Err(format!("{integration} {operation} takes one {what}; quote it if it contains spaces.")),
        }
    };
    match operation {
        "status" if rest.is_empty() => {}
        "status" => return Err(format!("{integration} status takes no arguments.")),
        "search" => {
            let query = rest.join(" ");
            if query.trim().is_empty() {
                return Err(format!("{integration} search needs a query, for example: kageha {integration} search \"is:unread\""));
            }
            request.query = Some(query);
        }
        "message" | "thread" => request.id = Some(one("id")?),
        _ => request.path = Some(one("path")?),
    }
    Ok(Invocation::Read { request, compact })
}

/// The request line sent to the API (with its trailing newline).
pub fn request_line(request: &Request, token: &str) -> String {
    let mut line = format!(
        "{{\"token\":{},\"integration\":{},\"operation\":{}",
        quote(token),
        quote(&request.integration),
        quote(request.operation)
    );
    for (name, value) in [("query", &request.query), ("id", &request.id), ("path", &request.path)] {
        if let Some(value) = value {
            line.push_str(&format!(",\"{name}\":{}", quote(value)));
        }
    }
    if request.refresh {
        line.push_str(",\"refresh\":true");
    }
    line.push_str("}\n");
    line
}

/// A failure the shim reports on stderr as `kageha: <code>: <message>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub code: String,
    pub message: String,
    pub exit: i32,
}

fn unavailable(code: &str, message: impl Into<String>) -> Failure {
    Failure { code: code.into(), message: message.into(), exit: EXIT_UNAVAILABLE }
}

/// Where the shim looks for the API: `KAGEHA_WORKSPACE`, then `KAGEHA_API_SOCKET`.
#[derive(Clone, Debug, Default)]
pub struct Location {
    pub workspace: Option<PathBuf>,
    pub api_socket: Option<PathBuf>,
}

impl Location {
    pub fn from_env() -> Self {
        let path = |name: &str| env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
        Self { workspace: path("KAGEHA_WORKSPACE"), api_socket: path("KAGEHA_API_SOCKET") }
    }

    /// `(directory to change to, socket file name relative to it)` candidates, in order.
    fn candidates(&self) -> Vec<(PathBuf, PathBuf)> {
        let mut out = Vec::new();
        if let Some(workspace) = &self.workspace {
            out.push((workspace.clone(), PathBuf::from(".kageha/api.sock")));
        }
        if let Some(socket) = &self.api_socket {
            if let (Some(dir), Some(name)) = (socket.parent(), socket.file_name()) {
                let dir = if dir.as_os_str().is_empty() { Path::new(".") } else { dir };
                out.push((dir.to_path_buf(), PathBuf::from(name)));
            }
        }
        out
    }
}

/// A connection to the API and the absolute state directory (`.kageha`) holding the token.
struct Connection {
    stream: UnixStream,
    state: PathBuf,
}

fn connect(location: &Location) -> Result<Connection, Failure> {
    let candidates = location.candidates();
    if candidates.is_empty() {
        return Err(unavailable(
            "api_unavailable",
            "KAGEHA_WORKSPACE and KAGEHA_API_SOCKET are not set. Run kageha inside the agent's shell, or set KAGEHA_WORKSPACE to the Task_Workspace.",
        ));
    }
    // Relative locations are resolved against the starting directory, not against a
    // candidate the loop already changed to.
    let start = env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let mut first: Option<Failure> = None;
    for (dir, name) in candidates {
        let dir = start.join(dir);
        let shown = dir.join(&name);
        let attempt = env::set_current_dir(&dir)
            .map_err(|error| unavailable("api_unavailable", format!("Cannot enter {}: {error}", dir.display())))
            .and_then(|()| {
                UnixStream::connect(&name).map_err(|error| {
                    let why = match error.kind() {
                        io::ErrorKind::NotFound => "no socket there".to_string(),
                        io::ErrorKind::ConnectionRefused => "nothing is listening".to_string(),
                        _ => error.to_string(),
                    };
                    unavailable("api_unavailable", format!(
                        "The local connector API is not running at {} ({why}). It runs only while a chat turn is active; use the integration_read tool otherwise.",
                        shown.display()
                    ))
                })
            });
        match attempt {
            Ok(stream) => {
                let state = shown.parent().map(Path::to_path_buf).unwrap_or(dir);
                return Ok(Connection { stream, state });
            }
            Err(failure) => {
                first.get_or_insert(failure);
            }
        }
    }
    Err(first.expect("at least one candidate"))
}

fn read_token(state: &Path) -> Result<String, Failure> {
    let path = state.join("api-token");
    let mut text = String::new();
    fs::File::open(&path)
        .and_then(|file| file.take(TOKEN_LIMIT).read_to_string(&mut text))
        .map_err(|error| unavailable("token_unavailable", format!(
            "Cannot read the connector API token at {} ({error}). The turn that served the API may have ended.",
            path.display()
        )))?;
    let token = text.trim();
    if token.is_empty() || !token.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(unavailable("token_unavailable", format!("The connector API token at {} is empty or malformed.", path.display())));
    }
    Ok(token.to_string())
}

/// Sends `request` and returns the reply envelope and the state directory (`.kageha`).
fn exchange(location: &Location, request: &Request) -> Result<(Json, PathBuf), Failure> {
    let Connection { mut stream, state } = connect(location)?;
    let token = read_token(&state)?;
    let io_failure = |error: io::Error| {
        let message = if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) {
            format!("No reply from the connector API within {} s.", REPLY_TIMEOUT.as_secs())
        } else {
            format!("Connector API connection failed: {error}")
        };
        unavailable("api_unavailable", message)
    };
    stream.set_read_timeout(Some(REPLY_TIMEOUT)).map_err(io_failure)?;
    stream.write_all(request_line(request, &token).as_bytes()).map_err(io_failure)?;
    drop(token);
    let mut line = Vec::new();
    BufReader::new(stream.take(REPLY_LIMIT)).read_until(b'\n', &mut line).map_err(io_failure)?;
    if line.is_empty() {
        return Err(unavailable("api_unavailable", "The connector API closed the connection without a reply."));
    }
    let reply = String::from_utf8(line)
        .map_err(|_| unavailable("invalid_reply", "The connector API reply is not UTF-8."))
        .and_then(|text| parse_json(&text).map_err(|error| unavailable("invalid_reply", format!("The connector API reply is not JSON: {error}"))))?;
    Ok((reply, state))
}

/// The `result` of a reply, loading a spilled reply from `.kageha/tool-output/`, which is
/// `tool-output/` in the state directory `state` (the socket's directory).
fn result_of(reply: &Json, state: &Path) -> Result<Json, Failure> {
    if !matches!(reply, Json::Obj(_)) {
        return Err(unavailable("invalid_reply", "The connector API reply is not a JSON object."));
    }
    match reply.get("ok") {
        Some(Json::Bool(true)) => {}
        Some(Json::Bool(false)) => {
            let error = reply.get("error");
            let field = |name: &str| error.and_then(|e| e.get(name)).and_then(Json::as_str);
            return Err(Failure {
                code: field("code").unwrap_or_else(|| "unknown_error".into()),
                message: field("message").unwrap_or_else(|| "The connector API refused the request.".into()),
                exit: EXIT_API_ERROR,
            });
        }
        _ => return Err(unavailable("invalid_reply", "The connector API reply has no ok field.")),
    }
    if let Some(spilled) = reply.get("spilled_path").and_then(Json::as_str) {
        let relative = Path::new(&spilled);
        let confined = spilled.starts_with(SPILL_PREFIX)
            && relative.components().all(|c| matches!(c, Component::Normal(_)));
        if !confined {
            return Err(unavailable("invalid_reply", "The connector API named a spill file outside .kageha/tool-output/."));
        }
        let inside = relative.strip_prefix(".kageha").unwrap_or(relative);
        let text = fs::read_to_string(state.join(inside))
            .map_err(|error| unavailable("invalid_reply", format!("Cannot read the spilled reply {spilled}: {error}")))?;
        let full = parse_json(&text).map_err(|error| unavailable("invalid_reply", format!("The spilled reply {spilled} is not JSON: {error}")))?;
        return result_of(&full, state);
    }
    Ok(reply.get("result").cloned().unwrap_or(Json::Null))
}

/// Runs one invocation against the API at `location`. Returns the exit code; writes the
/// result to `out` and failures and notes to `err`. To reach the socket by a short relative
/// path it changes the process's working directory, so in-process callers other than the
/// standalone executable should only use it for help and usage errors.
pub fn run(args: &[String], location: &Location, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let (request, compact) = match parse(args) {
        Ok(Invocation::Help) => {
            let _ = out.write_all(USAGE.as_bytes());
            return EXIT_OK;
        }
        Ok(Invocation::Read { request, compact }) => (request, compact),
        Err(message) => {
            let _ = writeln!(err, "kageha: usage: {message}\nRun 'kageha --help' for usage.");
            return EXIT_USAGE;
        }
    };
    let outcome = exchange(location, &request).and_then(|(reply, state)| result_of(&reply, &state));
    match outcome {
        Ok(result) => {
            let mut text = String::new();
            result.write(&mut text, if compact { None } else { Some(0) });
            text.push('\n');
            if out.write_all(text.as_bytes()).and_then(|()| out.flush()).is_err() {
                return EXIT_UNAVAILABLE;
            }
            if result.get("status").and_then(Json::as_str).as_deref() == Some("disconnected") {
                let _ = writeln!(
                    err,
                    "kageha: note: {} is not connected (status: disconnected). This is a successful read; the result explains how to reconnect.",
                    request.integration
                );
            }
            EXIT_OK
        }
        Err(failure) => {
            let _ = writeln!(err, "kageha: {}: {}", failure.code, failure.message);
            failure.exit
        }
    }
}

/// Entry point of the standalone `kageha` executable. Returns the exit code.
pub fn main() -> i32 {
    let args: Result<Vec<String>, _> = env::args_os().skip(1).map(|arg| arg.into_string()).collect();
    let mut stderr = io::stderr();
    let Ok(args) = args else {
        let _ = writeln!(stderr, "kageha: usage: arguments must be UTF-8 text.");
        return EXIT_USAGE;
    };
    let stdout = io::stdout();
    let mut out = stdout.lock();
    run(&args, &Location::from_env(), &mut out, &mut stderr)
}

/// A JSON value. Numbers and strings keep their source text (strings without the quotes,
/// escapes included), so output reproduces the server's values exactly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(String),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    /// The member named `key` of an object.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(members) => members.iter().find(|(name, _)| unescape(name) == key).map(|(_, value)| value),
            _ => None,
        }
    }

    /// The decoded text of a string.
    pub fn as_str(&self) -> Option<String> {
        match self {
            Json::Str(raw) => Some(unescape(raw)),
            _ => None,
        }
    }

    /// Appends the value to `out`: compact when `indent` is `None`, else pretty-printed with
    /// two spaces per level starting at `indent`.
    pub fn write(&self, out: &mut String, indent: Option<usize>) {
        let newline = |out: &mut String, level: usize| {
            out.push('\n');
            for _ in 0..level {
                out.push_str("  ");
            }
        };
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
            Json::Num(raw) => out.push_str(raw),
            Json::Str(raw) => {
                out.push('"');
                out.push_str(raw);
                out.push('"');
            }
            Json::Arr(items) if items.is_empty() => out.push_str("[]"),
            Json::Obj(members) if members.is_empty() => out.push_str("{}"),
            Json::Arr(items) => {
                out.push('[');
                for (n, item) in items.iter().enumerate() {
                    if n > 0 {
                        out.push(',');
                    }
                    if let Some(level) = indent {
                        newline(out, level + 1);
                    }
                    item.write(out, indent.map(|level| level + 1));
                }
                if let Some(level) = indent {
                    newline(out, level);
                }
                out.push(']');
            }
            Json::Obj(members) => {
                out.push('{');
                for (n, (name, value)) in members.iter().enumerate() {
                    if n > 0 {
                        out.push(',');
                    }
                    if let Some(level) = indent {
                        newline(out, level + 1);
                    }
                    out.push('"');
                    out.push_str(name);
                    out.push_str(if indent.is_some() { "\": " } else { "\":" });
                    value.write(out, indent.map(|level| level + 1));
                }
                if let Some(level) = indent {
                    newline(out, level);
                }
                out.push('}');
            }
        }
    }
}

/// Parses one JSON document (surrounding whitespace allowed).
pub fn parse_json(text: &str) -> Result<Json, String> {
    let mut parser = Parser { text, bytes: text.as_bytes(), at: 0 };
    parser.space();
    let value = parser.value(0)?;
    parser.space();
    if parser.at != parser.bytes.len() {
        return Err(format!("unexpected text at byte {}", parser.at));
    }
    Ok(value)
}

struct Parser<'a> {
    text: &'a str,
    bytes: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn space(&mut self) {
        while matches!(self.bytes.get(self.at), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn fail<T>(&self, what: &str) -> Result<T, String> {
        Err(format!("{what} at byte {}", self.at))
    }

    fn eat(&mut self, byte: u8) -> bool {
        if self.bytes.get(self.at) == Some(&byte) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, String> {
        if depth > MAX_DEPTH {
            return self.fail("nesting too deep");
        }
        match self.bytes.get(self.at) {
            Some(b'{') => {
                self.at += 1;
                let mut members = Vec::new();
                self.space();
                if self.eat(b'}') {
                    return Ok(Json::Obj(members));
                }
                loop {
                    self.space();
                    if self.bytes.get(self.at) != Some(&b'"') {
                        return self.fail("expected a member name");
                    }
                    let name = self.string()?;
                    self.space();
                    if !self.eat(b':') {
                        return self.fail("expected ':'");
                    }
                    self.space();
                    members.push((name, self.value(depth + 1)?));
                    self.space();
                    if self.eat(b'}') {
                        return Ok(Json::Obj(members));
                    }
                    if !self.eat(b',') {
                        return self.fail("expected ',' or '}'");
                    }
                }
            }
            Some(b'[') => {
                self.at += 1;
                let mut items = Vec::new();
                self.space();
                if self.eat(b']') {
                    return Ok(Json::Arr(items));
                }
                loop {
                    self.space();
                    items.push(self.value(depth + 1)?);
                    self.space();
                    if self.eat(b']') {
                        return Ok(Json::Arr(items));
                    }
                    if !self.eat(b',') {
                        return self.fail("expected ',' or ']'");
                    }
                }
            }
            Some(b'"') => Ok(Json::Str(self.string()?)),
            Some(b't') => self.literal("true", Json::Bool(true)),
            Some(b'f') => self.literal("false", Json::Bool(false)),
            Some(b'n') => self.literal("null", Json::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => self.fail("expected a value"),
        }
    }

    fn literal(&mut self, word: &str, value: Json) -> Result<Json, String> {
        if self.bytes[self.at..].starts_with(word.as_bytes()) {
            self.at += word.len();
            Ok(value)
        } else {
            self.fail("invalid literal")
        }
    }

    fn digits(&mut self) -> usize {
        let start = self.at;
        while matches!(self.bytes.get(self.at), Some(b'0'..=b'9')) {
            self.at += 1;
        }
        self.at - start
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.at;
        self.eat(b'-');
        match self.bytes.get(self.at) {
            Some(b'0') => self.at += 1,
            Some(b'1'..=b'9') => {
                self.digits();
            }
            _ => return self.fail("invalid number"),
        }
        if self.eat(b'.') && self.digits() == 0 {
            return self.fail("invalid number");
        }
        if matches!(self.bytes.get(self.at), Some(b'e' | b'E')) {
            self.at += 1;
            if !self.eat(b'+') {
                self.eat(b'-');
            }
            if self.digits() == 0 {
                return self.fail("invalid number");
            }
        }
        Ok(Json::Num(self.text[start..self.at].to_string()))
    }

    /// A string's raw text between the quotes, validated.
    fn string(&mut self) -> Result<String, String> {
        self.at += 1;
        let start = self.at;
        loop {
            match self.bytes.get(self.at) {
                None => return self.fail("unterminated string"),
                Some(b'"') => break,
                Some(b'\\') => {
                    self.at += 1;
                    match self.bytes.get(self.at) {
                        Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => self.at += 1,
                        Some(b'u') => {
                            let hex = self.bytes.get(self.at + 1..self.at + 5);
                            if !hex.is_some_and(|hex| hex.iter().all(u8::is_ascii_hexdigit)) {
                                return self.fail("invalid \\u escape");
                            }
                            self.at += 5;
                        }
                        _ => return self.fail("invalid escape"),
                    }
                }
                Some(byte) if *byte < 0x20 => return self.fail("control character in string"),
                Some(_) => self.at += 1,
            }
        }
        let raw = self.text[start..self.at].to_string();
        self.at += 1;
        Ok(raw)
    }
}

/// Decodes a validated raw JSON string body. Unpaired surrogates become U+FFFD.
fn unescape(raw: &str) -> String {
    if !raw.contains('\\') {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    let hex4 = |chars: &mut std::str::Chars<'_>| -> Option<u32> {
        let text: String = chars.by_ref().take(4).collect();
        u32::from_str_radix(&text, 16).ok()
    };
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('b') => out.push('\u{8}'),
            Some('f') => out.push('\u{c}'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('u') => {
                let Some(high) = hex4(&mut chars) else {
                    out.push('\u{fffd}');
                    continue;
                };
                if (0xD800..0xDC00).contains(&high) {
                    let mut ahead = chars.clone();
                    let low = (ahead.next() == Some('\\') && ahead.next() == Some('u'))
                        .then(|| hex4(&mut ahead))
                        .flatten()
                        .filter(|low| (0xDC00..0xE000).contains(low));
                    if let Some(low) = low {
                        chars = ahead;
                        let code = 0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00);
                        out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                        continue;
                    }
                }
                out.push(char::from_u32(high).unwrap_or('\u{fffd}'));
            }
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// `text` as a JSON string literal.
pub fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn read(list: &[&str]) -> (Request, bool) {
        match parse(&args(list)).unwrap() {
            Invocation::Read { request, compact } => (request, compact),
            Invocation::Help => panic!("help for {list:?}"),
        }
    }

    #[test]
    fn parses_reads_flags_and_help() {
        let (request, compact) = read(&["gmail", "search", "from:alice is:unread", "--json"]);
        assert!(compact);
        assert_eq!(request, Request {
            integration: "gmail".into(), operation: "search", query: Some("from:alice is:unread".into()),
            id: None, path: None, refresh: false,
        });
        // Flags anywhere; unquoted search words are joined; a single dash is a query word.
        let (request, compact) = read(&["--refresh", "gmail", "search", "-in:spam", "invoice"]);
        assert!(!compact && request.refresh);
        assert_eq!(request.query.as_deref(), Some("-in:spam invoice"));
        // `--` passes a literal option-looking word.
        assert_eq!(read(&["gmail", "search", "--", "--json"]).0.query.as_deref(), Some("--json"));
        assert_eq!(read(&["gmail", "message", "18c2a"]).0.id.as_deref(), Some("18c2a"));
        assert_eq!(read(&["gmail", "thread", "t1"]).0.operation, "thread");
        let (request, _) = read(&["gmail", "status"]);
        assert_eq!((request.operation, request.query, request.id), ("status", None, None));
        let (request, _) = read(&["github", "path", "repos/o/r/issues"]);
        assert_eq!((request.operation, request.path.as_deref()), ("path", Some("repos/o/r/issues")));
        for help in [&["--help"][..], &["-h"], &["help"], &["gmail", "search", "--help"]] {
            assert_eq!(parse(&args(help)).unwrap(), Invocation::Help, "{help:?}");
        }
    }

    #[test]
    fn rejects_bad_invocations_with_usage_messages() {
        for (list, needle) in [
            (&[][..], "Name an integration"),
            (&["gmail"], "Name an operation"),
            (&["gmail", "send", "x"], "read-only"),
            (&["gmail", "search"], "needs a query"),
            (&["gmail", "search", " "], "needs a query"),
            (&["gmail", "message"], "needs one id"),
            (&["gmail", "thread", "a", "b"], "takes one id"),
            (&["gmail", "status", "x"], "takes no arguments"),
            (&["github", "path"], "needs one path"),
            (&["gmail", "status", "--verbose"], "Unknown option --verbose"),
            (&["../x", "status"], "not an integration name"),
            (&["-x", "status"], "not an integration name"),
        ] {
            let error = parse(&args(list)).unwrap_err();
            assert!(error.contains(needle), "{list:?}: {error}");
        }
    }

    #[test]
    fn request_lines_are_json_with_only_the_needed_fields() {
        let (request, _) = read(&["gmail", "search", "subject:\"Q3 \\ plan\"\n", "--refresh"]);
        let line = request_line(&request, "tok");
        assert!(line.ends_with("}\n") && !line[..line.len() - 1].contains('\n'));
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value, serde_json::json!({"token":"tok","integration":"gmail","operation":"search",
            "query":"subject:\"Q3 \\ plan\"\n","refresh":true}));
        let (request, _) = read(&["github", "path", "repos/o/r"]);
        let value: serde_json::Value = serde_json::from_str(&request_line(&request, "t")).unwrap();
        assert_eq!(value, serde_json::json!({"token":"t","integration":"github","operation":"path","path":"repos/o/r"}));
    }

    #[test]
    fn json_round_trips_pretty_and_compact() {
        let source = serde_json::json!({
            "status":"disconnected", "n":[1, -2.5e-3, 0, 12345678901234567890u64], "empty":{}, "none":[],
            "text":"quote \" slash \\ nl \n tab \t ctrl \u{1} é 😀", "nested":{"a":[{"b":null,"c":true,"d":false}]}
        });
        let text = source.to_string();
        let parsed = parse_json(&text).unwrap();
        let mut compact = String::new();
        parsed.write(&mut compact, None);
        assert_eq!(compact, text, "compact output reproduces the server's line");
        let mut pretty = String::new();
        parsed.write(&mut pretty, Some(0));
        assert_eq!(pretty, serde_json::to_string_pretty(&source).unwrap());
        assert_eq!(parsed.get("text").unwrap().as_str().unwrap(), source["text"].as_str().unwrap());
        assert_eq!(parse_json(r#""\ud83d\ude00 \u00e9 \ud800x""#).unwrap().as_str().unwrap(), "😀 é \u{fffd}x");
        for bad in ["", "{", "[1,]", "{\"a\" 1}", "01", "1.", "\"\u{1}\"", "\"\\x\"", "nul", "1 2", "-"] {
            assert!(parse_json(bad).is_err(), "{bad:?}");
        }
        assert!(parse_json(&"[".repeat(MAX_DEPTH + 2)).is_err());
    }

    #[test]
    fn help_and_missing_api_give_clear_results() {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(run(&args(&["--help"]), &Location::default(), &mut out, &mut err), EXIT_OK);
        assert!(String::from_utf8(out).unwrap().contains("kageha [--json] [--refresh] gmail search <query>"));
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(run(&args(&["gmail", "send"]), &Location::default(), &mut out, &mut err), EXIT_USAGE);
        assert!(String::from_utf8(err).unwrap().starts_with("kageha: usage: Unknown operation 'send'"));
        let (mut out, mut err) = (Vec::new(), Vec::new());
        assert_eq!(run(&args(&["gmail", "status"]), &Location::default(), &mut out, &mut err), EXIT_UNAVAILABLE);
        assert!(out.is_empty());
        assert!(String::from_utf8(err).unwrap().starts_with("kageha: api_unavailable: KAGEHA_WORKSPACE and KAGEHA_API_SOCKET are not set"));
    }

    #[test]
    fn error_and_spilled_replies_are_unwrapped() {
        let dir = tempfile::tempdir().unwrap();
        let refused = parse_json(r#"{"ok":false,"error":{"code":"operation_not_allowed","message":"read \"only\""}}"#).unwrap();
        assert_eq!(result_of(&refused, dir.path()).unwrap_err(), Failure {
            code: "operation_not_allowed".into(), message: "read \"only\"".into(), exit: EXIT_API_ERROR,
        });
        fs::create_dir_all(dir.path().join(".kageha/tool-output")).unwrap();
        fs::write(dir.path().join(".kageha/tool-output/api-1.json"), r#"{"ok":true,"result":{"big":[1,2]}}"#).unwrap();
        let spilled = parse_json(r#"{"ok":true,"spilled_path":".kageha/tool-output/api-1.json","original_bytes":5000000}"#).unwrap();
        let state = dir.path().join(".kageha");
        assert_eq!(result_of(&spilled, &state).unwrap(), parse_json(r#"{"big":[1,2]}"#).unwrap());
        for path in ["../x.json", ".kageha/tool-output/../../x.json", "/etc/passwd"] {
            let reply = parse_json(&format!(r#"{{"ok":true,"spilled_path":{}}}"#, quote(path))).unwrap();
            assert_eq!(result_of(&reply, dir.path()).unwrap_err().code, "invalid_reply", "{path}");
        }
        assert_eq!(result_of(&parse_json("[]").unwrap(), dir.path()).unwrap_err().code, "invalid_reply");
    }
}
