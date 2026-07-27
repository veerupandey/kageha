//! Scoped client for the standalone Rust browser service.
pub mod profiles;
use kageha_contracts::execution::Control;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::{fs::MetadataExt, net::UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

static REQUESTS: AtomicU64 = AtomicU64::new(0);
// Protocol v1 closes a connection after this many requests, including handshake.
const REQUESTS_PER_CONNECTION: usize = 64;
// The daemon closes a connection that is idle for 5 seconds (x-limits.idle_ms in
// the protocol schema). Replace a connection before it can reach that limit; a
// request written to a closed socket fails with EPIPE.
const IDLE_RECONNECT: Duration = Duration::from_secs(3);

pub fn milliseconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub fn unique_id() -> String {
    format!(
        "{:x}",
        Sha256::digest(format!(
            "{}:{}:{}",
            std::process::id(),
            milliseconds(),
            REQUESTS.fetch_add(1, Ordering::Relaxed)
        ))
    )[..32]
        .into()
}

pub fn configured() -> bool {
    std::env::var_os("KAGEHA_BROWSER_STATE_DIR").is_some()
}

pub fn private(path: &Path) -> io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(io::Error::other(
            "browser state must be private and owned by this user",
        ));
    }
    Ok(())
}

pub struct Client {
    io: BufReader<UnixStream>,
    pub root: PathBuf,
    descriptor: Value,
    pub epoch: String,
    chat: String,
    client: String,
    actor: String,
    connection_requests: usize,
    read_only: bool,
    last_used: Instant,
}

impl Client {
    pub fn workspace_id(&self) -> &str {
        &self.chat
    }
    pub fn connect(chat: &str, actor: &str) -> io::Result<Self> {
        let root = PathBuf::from(
            std::env::var_os("KAGEHA_BROWSER_STATE_DIR")
                .ok_or_else(|| io::Error::other("Rust browser is not configured"))?,
        );
        Self::connect_to(chat, actor, root, false)
    }
    pub fn connect_readonly(root: &Path, chat: &str) -> io::Result<Self> {
        Self::connect_to(chat, "recovery", root.into(), true)
    }
    pub fn connect_to(chat: &str, actor: &str, root: PathBuf, read_only: bool) -> io::Result<Self> {
        private(&root)?;
        let root = root.canonicalize()?;
        for name in ["daemon.json", "auth.token"] {
            private(&root.join(name))?
        }
        let descriptor: Value = serde_json::from_slice(&std::fs::read(root.join("daemon.json"))?)?;
        if descriptor["protocol_version"] != 1
            || descriptor["state_root"] != root.to_string_lossy().as_ref()
        {
            return Err(io::Error::other("browser instance identity mismatch"));
        }
        let token = std::fs::read_to_string(root.join("auth.token"))?;
        if token.len() != 32 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(io::Error::other("invalid browser authentication token"));
        }
        let path = descriptor["socket"].as_str().map(PathBuf::from).unwrap_or_else(||root.join("control.sock"));
        let directory = path.parent().filter(|_|path.is_absolute()).ok_or_else(||io::Error::other("Invalid browser socket path"))?;
        private(directory)?;
        private(&path)?;
        let socket = UnixStream::connect(path)?;
        socket.set_read_timeout(Some(Duration::from_secs(30)))?;
        socket.set_write_timeout(Some(Duration::from_secs(5)))?;
        let mut client = Self {
            io: BufReader::new(socket),
            root,
            epoch: String::new(),
            descriptor,
            chat: chat.into(),
            client: format!("native-{actor}-{chat}"),
            actor: actor.into(),
            connection_requests: 0,
            read_only,
            last_used: Instant::now(),
        };
        let (reply, _) = client.exchange("handshake", json!({"token":token}), None)?;
        if reply["service_epoch"] != client.descriptor["service_epoch"]
            || !(reply["result"]["action_backend"] == "standalone"
                || read_only && reply["result"]["action_backend"] == "application")
        {
            return Err(io::Error::other(
                "configure the browser with standalone authority",
            ));
        }
        client.epoch = reply["service_epoch"]
            .as_str()
            .ok_or_else(|| io::Error::other("invalid service epoch"))?
            .into();
        Ok(client)
    }

    fn exchange(
        &mut self,
        method: &str,
        params: Value,
        authority: Option<&Value>,
    ) -> io::Result<(Value, Vec<u8>)> {
        self.exchange_attempt(method, params, authority, None, None)
    }

    fn exchange_attempt(
        &mut self,
        method: &str,
        params: Value,
        authority: Option<&Value>,
        attempt: Option<(&str, &Control<'_>)>,
        timeout_override: Option<Duration>,
    ) -> io::Result<(Value, Vec<u8>)> {
        if let Some((_, control)) = attempt {
            control.check()?;
        }
        if self.read_only
            && !matches!(
                method,
                "handshake"
                    | "browser_selection"
                    | "observe_tab"
                    | "query_tab"
                    | "list_tabs"
                    | "action_receipt"
            )
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Recovery client is read-only",
            ));
        }
        if method != "handshake"
            && (self.connection_requests >= REQUESTS_PER_CONNECTION
                || self.last_used.elapsed() >= IDLE_RECONNECT)
        {
            let _ = self.io.get_ref().shutdown(std::net::Shutdown::Both);
            let replacement =
                Self::connect_to(&self.chat, &self.actor, self.root.clone(), self.read_only)?;
            if replacement.root != self.root || replacement.epoch != self.epoch {
                return Err(io::Error::other("browser service restarted"));
            }
            *self = replacement;
        }
        let id = attempt
            .map(|(id, _)| id.to_owned())
            .unwrap_or_else(unique_id);
        if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
            return Err(io::Error::other("Invalid browser attempt identity"));
        }
        let timeout = if let Some((_, control)) = attempt {
            control.deadline
                .map(|d| d.saturating_duration_since(std::time::Instant::now()))
                .unwrap_or(Duration::from_secs(25))
                .min(Duration::from_secs(25))
        } else {
            timeout_override.unwrap_or(Duration::from_secs(25))
        };
        let request = json!({"protocol_version":1,"request_id":id,
            "instance_id":self.descriptor["instance_id"],"application_id":self.descriptor["application_id"],
            "workspace_id":self.chat,"client_id":self.client,"state_root":self.root,
            "service_epoch":if self.epoch.is_empty(){Value::Null}else{json!(self.epoch)},
            "deadline_ms":milliseconds()+timeout.as_millis() as u64,"expected_binding":null,"authority":authority,
            "method":method,"params":params});
        let mut encoded = serde_json::to_vec(&request)?;
        if encoded.len() > 1024 * 1024 {
            return Err(io::Error::other(
                "browser request exceeds the protocol limit",
            ));
        }
        encoded.push(b'\n');
        self.connection_requests += 1;
        let mut line = Vec::new();
        if let Some((_, control)) = attempt {
            self.io
                .get_mut()
                .set_read_timeout(Some(Duration::from_millis(50)))?;
            self.io
                .get_mut()
                .set_write_timeout(Some(Duration::from_millis(50)))?;
            let mut remaining = encoded.as_slice();
            while !remaining.is_empty() {
                control.check()?;
                match self.io.get_mut().write(remaining) {
                    Ok(0) => return Err(io::Error::other("Browser disconnected")),
                    Ok(n) => remaining = &remaining[n..],
                    Err(e) if waiting(&e) => {}
                    Err(e) => return Err(e),
                }
            }
            while line.len() <= 1024 * 1024 {
                control.check()?;
                match self.io.fill_buf() {
                    Ok([]) => break,
                    Ok(bytes) => {
                        let n = bytes
                            .iter()
                            .position(|b| *b == b'\n')
                            .map(|n| n + 1)
                            .unwrap_or(bytes.len())
                            .min(1024 * 1024 + 1 - line.len());
                        let done = bytes.get(n.wrapping_sub(1)) == Some(&b'\n');
                        line.extend_from_slice(&bytes[..n]);
                        self.io.consume(n);
                        if done {
                            break;
                        }
                    }
                    Err(e) if waiting(&e) => {}
                    Err(e) => return Err(e),
                }
            }
        } else {
            self.io.get_mut().write_all(&encoded)?;
            (&mut self.io)
                .take(1024 * 1024 + 1)
                .read_until(b'\n', &mut line)?;
        }
        self.last_used = Instant::now();
        if line.len() > 1024 * 1024 || line.last() != Some(&b'\n') {
            return Err(io::Error::other("invalid browser reply"));
        }
        let reply: Value = serde_json::from_slice(&line)?;
        for field in [
            "request_id",
            "workspace_id",
            "client_id",
            "instance_id",
            "application_id",
        ] {
            if reply[field] != request[field] {
                return Err(io::Error::other("browser reply identity mismatch"));
            }
        }
        if !self.epoch.is_empty() && reply["service_epoch"] != self.epoch {
            return Err(io::Error::other("browser service restarted"));
        }
        if reply["status"] != "ok" {
            let code = reply["error"]["code"]
                .as_str()
                .unwrap_or("browser request failed");
            return Err(io::Error::other(json!({"error_code":code,"request_id":id,
                "execution_phase":reply["execution_phase"],"detail":reply["error"],
                "browser_id":request["params"]["browser_id"],"target_id":request["params"]["target_id"]}).to_string()));
        }
        let mut bytes = Vec::new();
        if method == "preview_tab" || method == "capture_tab" {
            let length = reply["result"]["byte_length"]
                .as_u64()
                .filter(|length| *length > 0 && *length <= 16 * 1024 * 1024)
                .ok_or_else(|| io::Error::other("invalid browser frame length"))?;
            bytes.resize(length as usize, 0);
            self.io.read_exact(&mut bytes)?;
            if reply["result"]["binding_id"] != request["params"]["binding_id"]
                && method == "preview_tab"
            {
                return Err(io::Error::other("browser frame binding changed"));
            }
        }
        Ok((reply, bytes))
    }

    pub fn call(
        &mut self,
        method: &str,
        params: Value,
        authority: Option<&Value>,
    ) -> io::Result<Value> {
        self.exchange(method, params, authority)
            .map(|(reply, _)| reply["result"].clone())
    }

    /// For an explicitly user-initiated operation that can wait on an OS dialog.
    /// Ordinary browser operations keep their shorter timeout.
    pub fn call_user_interactive(
        &mut self,
        method: &str,
        params: Value,
        authority: Option<&Value>,
    ) -> io::Result<Value> {
        self.io.get_mut().set_read_timeout(Some(Duration::from_secs(300)))?;
        let result = self.exchange_attempt(method, params, authority, None, Some(Duration::from_secs(300)))
            .map(|(reply, _)| reply["result"].clone());
        if result.is_err() {
            let _ = self.io.get_ref().shutdown(std::net::Shutdown::Both);
            return result;
        }
        self.io.get_mut().set_read_timeout(Some(Duration::from_secs(30)))?;
        result
    }

    /// Exact attempt identity and cooperative cancellation. Never retries a sent request.
    pub fn call_controlled(
        &mut self,
        method: &str,
        params: Value,
        authority: Option<&Value>,
        id: &str,
        control: &Control<'_>,
    ) -> io::Result<Value> {
        let bounded = Control {
            cancelled: control.cancelled,
            deadline: Some(
                control
                    .deadline
                    .unwrap_or(std::time::Instant::now() + Duration::from_secs(25))
                    .min(std::time::Instant::now() + Duration::from_secs(25)),
            ),
        };
        let result = self
            .exchange_attempt(method, params, authority, Some((id, &bounded)), None)
            .map(|(reply, _)| reply["result"].clone());
        // An interrupted stream must not be reused: it may contain a late reply.
        if result.is_err() {
            let _ = self.io.get_ref().shutdown(std::net::Shutdown::Both);
            return result;
        }
        self.io
            .get_mut()
            .set_read_timeout(Some(Duration::from_secs(30)))?;
        self.io
            .get_mut()
            .set_write_timeout(Some(Duration::from_secs(5)))?;
        result
    }

    pub fn selection(&mut self) -> io::Result<Value> {
        self.call("browser_selection", json!({}), None)
    }

    pub fn frame(&mut self, selection: &Value) -> io::Result<Vec<u8>> {
        self.exchange("preview_tab", target(selection), None)
            .map(|(_, bytes)| bytes)
    }
}

fn waiting(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}

pub fn target(selection: &Value) -> Value {
    json!({"binding_id":selection["binding_id"],"browser_id":selection["browser_id"],
        "target_id":selection["target_id"].as_str().unwrap_or("")})
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    #[test]
    fn interactive_call_extends_deadline_and_restores_regular_timeout() {
        let (socket, peer) = UnixStream::pair().unwrap();
        socket.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        let server = std::thread::spawn(move || {
            let mut peer = BufReader::new(peer);
            let mut deadlines = Vec::new();
            for _ in 0..2 {
                let mut line = String::new();
                peer.read_line(&mut line).unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                deadlines.push(request["deadline_ms"].as_u64().unwrap().saturating_sub(milliseconds()));
                let response = json!({"request_id":request["request_id"],"workspace_id":request["workspace_id"],
                    "client_id":request["client_id"],"instance_id":request["instance_id"],
                    "application_id":request["application_id"],"service_epoch":"epoch","status":"ok","result":{"ok":true}});
                writeln!(peer.get_mut(), "{response}").unwrap();
            }
            deadlines
        });
        let mut client = Client { io: BufReader::new(socket), root: "/tmp".into(), epoch: "epoch".into(),
            descriptor: json!({"instance_id":"instance","application_id":"app"}), chat: "settings".into(),
            client: "native-user-settings".into(), actor: "user".into(), connection_requests: 0, read_only: false, last_used: Instant::now() };
        assert_eq!(client.call_user_interactive("credential_vault_authorize", json!({}), None).unwrap()["ok"], true);
        assert_eq!(client.io.get_ref().read_timeout().unwrap(), Some(Duration::from_secs(30)));
        client.call("credential_vault_status", json!({}), None).unwrap();
        let deadlines = server.join().unwrap();
        assert!(deadlines[0] >= 299_000, "{deadlines:?}");
        assert!(deadlines[1] <= 25_000, "{deadlines:?}");
    }

    /// The daemon closes a connection after 5 idle seconds. A client that kept
    /// using it failed with EPIPE (seen as `browser_open failed: Broken pipe`).
    #[test]
    fn a_connection_idle_past_the_reconnect_threshold_is_replaced_before_sending() {
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::net::UnixListener;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let write_private = |name: &str, body: String| {
            let path = root.join(name);
            std::fs::write(&path, body).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        };
        write_private("auth.token", "0123456789abcdef0123456789abcdef".into());
        write_private("daemon.json", json!({"protocol_version":1,"state_root":root,
            "instance_id":"instance","application_id":"app","service_epoch":"epoch"}).to_string());
        let listener = UnixListener::bind(root.join("control.sock")).unwrap();
        std::fs::set_permissions(root.join("control.sock"), std::fs::Permissions::from_mode(0o600)).unwrap();
        let server = std::thread::spawn(move || {
            let mut connections = 0;
            for stream in listener.incoming().take(2) {
                connections += 1;
                let mut peer = BufReader::new(stream.unwrap());
                // Handshake plus one request, then close, as the daemon does when idle.
                for _ in 0..2 {
                    let mut line = String::new();
                    if peer.read_line(&mut line).unwrap() == 0 { break; }
                    let request: Value = serde_json::from_str(&line).unwrap();
                    let response = json!({"request_id":request["request_id"],"workspace_id":request["workspace_id"],
                        "client_id":request["client_id"],"instance_id":request["instance_id"],
                        "application_id":request["application_id"],"service_epoch":"epoch","status":"ok",
                        "result":{"action_backend":"standalone","ok":true}});
                    writeln!(peer.get_mut(), "{response}").unwrap();
                }
            }
            connections
        });
        let mut client = Client::connect_to("chat", "agent", root, false).unwrap();
        client.call("browser_selection", json!({}), None).unwrap();
        std::thread::sleep(IDLE_RECONNECT + Duration::from_millis(200));
        assert_eq!(client.call("browser_selection", json!({}), None).unwrap()["ok"], true);
        assert_eq!(server.join().unwrap(), 2, "the idle connection must be replaced, not reused");
    }
}
