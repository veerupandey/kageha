//! Shared-session transport. Execution and approval authority stay on the server.
pub mod cli;
mod sse;
use reqwest::{
    blocking::{Client, Response},
    Method, Url,
};
use serde_json::{json, Value};
pub use sse::{Feed, Packet};
use std::{
    fs::File,
    io::{self, Read},
    path::Path,
    time::Duration,
};
const LIMIT: u64 = 8 * 1024 * 1024;
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
pub fn identifier(value: &str) -> io::Result<&str> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        Err(invalid("Invalid session, request or turn ID"))
    } else {
        Ok(value)
    }
}
pub fn endpoint(raw: &str) -> io::Result<Url> {
    let mut url = Url::parse(raw).map_err(|_| invalid("Invalid server URL"))?;
    let local = matches!(
        url.host_str(),
        Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
    );
    if raw.len() > 4096
        || raw.chars().any(char::is_control)
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !(url.scheme() == "https" || url.scheme() == "http" && local)
    {
        return Err(invalid(
            "Use HTTPS or a loopback URL, without credentials, query or fragment",
        ));
    }
    if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path()));
    }
    Ok(url)
}
pub fn read_token(path: &Path) -> io::Result<String> {
    let mut options = File::options();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > 8192 {
        return Err(invalid("Token must be a bounded regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o077 != 0 || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(invalid(
                "Token file must be owned by you and private (chmod 600)",
            ));
        }
    }
    let mut token = String::new();
    file.take(8193).read_to_string(&mut token)?;
    validate_token(token.trim())
}
fn validate_token(token: &str) -> io::Result<String> {
    if !(32..=8192).contains(&token.len()) || !token.bytes().all(|b| b.is_ascii_graphic()) {
        Err(invalid(
            "A server token of at least 32 printable characters is required",
        ))
    } else {
        Ok(token.into())
    }
}
#[derive(Clone)]
pub struct SessionClient {
    base: Url,
    token: String,
    http: Client,
}
impl SessionClient {
    pub fn new(base: &str, token: &str) -> io::Result<Self> {
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .build()
            .map_err(io::Error::other)?;
        Ok(Self {
            base: endpoint(base)?,
            token: validate_token(token)?,
            http,
        })
    }
    fn url(&self, path: &str) -> io::Result<Url> {
        let target = self
            .base
            .join(path)
            .map_err(|_| invalid("Invalid API path"))?;
        if target.origin() != self.base.origin()
            || !target.username().is_empty()
            || target.password().is_some()
        {
            return Err(invalid("API requests must remain on the configured server"));
        }
        Ok(target)
    }
    fn response(&self, method: Method, path: &str, body: Option<&Value>) -> io::Result<Response> {
        let mutation = method != Method::GET;
        let mut request = self
            .http
            .request(method, self.url(path)?)
            .bearer_auth(&self.token);
        if let Some(body) = body {
            request = request.json(body);
        }
        request.send().map_err(|_| {
            io::Error::other(if mutation {
                "Request outcome is unknown. Inspect this session/request; nothing was retried."
            } else {
                "Session server is unavailable. No action was sent."
            })
        })
    }
    pub fn json(&self, method: Method, path: &str, body: Option<&Value>) -> io::Result<Value> {
        let response = self.response(method, path, body)?;
        let status = response.status();
        let mut bytes = Vec::new();
        response
            .take(LIMIT + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| {
                io::Error::other(
                    "Response interrupted; inspect the original request before sending new input",
                )
            })?;
        if bytes.len() as u64 > LIMIT {
            return Err(io::Error::other("Session response exceeds 8 MiB"));
        }
        let packet: Value = serde_json::from_slice(&bytes)
            .map_err(|_| io::Error::other("Invalid session response; no request was retried"))?;
        if !status.is_success() {
            return Err(io::Error::other(format!(
                "HTTP {}: {}",
                status.as_u16(),
                cli::clean(
                    packet["error"]
                        .as_str()
                        .unwrap_or("Session request rejected")
                )
            )));
        }
        Ok(packet)
    }
    pub fn create(&self) -> io::Result<String> {
        let value = self.json(Method::POST, "api/sessions", Some(&json!({})))?;
        Ok(identifier(
            value["session_id"]
                .as_str()
                .ok_or_else(|| io::Error::other("Session identity missing"))?,
        )?
        .into())
    }
    pub fn session(&self, id: &str) -> io::Result<Value> {
        self.json(
            Method::GET,
            &format!("api/sessions/{}", identifier(id)?),
            None,
        )
    }
    pub fn admit(&self, session: &str, request: &str, message: &str) -> io::Result<Value> {
        identifier(session)?;
        identifier(request)?;
        if message.trim().is_empty() || message.len() > 262144 {
            return Err(invalid("Message must contain 1 to 262144 bytes"));
        }
        self.json(Method::POST,"api/chat/stream",Some(&json!({"session_id":session,"client_request_id":request,"message":message,"event_stream":"session"})))
    }
    pub fn receipt(&self, session: &str, request: &str) -> io::Result<Value> {
        self.json(
            Method::GET,
            &format!(
                "api/sessions/{}/requests/{}",
                identifier(session)?,
                identifier(request)?
            ),
            None,
        )
    }
    pub fn feed(&self, session: &str, after: u64) -> io::Result<Feed> {
        if after > i64::MAX as u64 {
            return Err(invalid("Invalid event cursor"));
        }
        let response = self.response(
            Method::GET,
            &format!(
                "api/sessions/{}/live/stream?after_sequence={after}",
                identifier(session)?
            ),
            None,
        )?;
        if !response.status().is_success() {
            return Err(io::Error::new(
                if response.status().is_client_error() {
                    io::ErrorKind::PermissionDenied
                } else {
                    io::ErrorKind::Other
                },
                format!("Session feed returned HTTP {}", response.status().as_u16()),
            ));
        }
        if !response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.split(';').next() == Some("text/event-stream"))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Expected a session event stream",
            ));
        }
        Ok(Feed::new(response, session.into(), after))
    }
    pub fn computer(
        &self,
        session: &str,
        operation: &str,
        body: Option<&Value>,
    ) -> io::Result<Value> {
        if !["state", "lease", "access"].contains(&operation) {
            return Err(invalid("Unsupported operator computer control"));
        }
        let suffix = if operation == "state" {
            String::new()
        } else {
            format!("/{operation}")
        };
        self.json(
            if body.is_some() {
                Method::POST
            } else {
                Method::GET
            },
            &format!("api/sessions/{}/computer{suffix}", identifier(session)?),
            body,
        )
    }
    pub fn approval(&self, session: &str, id: &str, approved: bool) -> io::Result<Value> {
        let snapshot = self.session(session)?;
        if snapshot["pending_approval"]["approval_id"] != id {
            return Err(invalid(
                "Approval is no longer pending in this session; inspect again",
            ));
        }
        self.json(
            Method::POST,
            "api/approvals",
            Some(&json!({"approval_id":id,"approved":approved,"scope":"once"})),
        )
    }
    pub fn panel_url(&self, session: &str, ui_base: Option<&str>) -> io::Result<String> {
        let mut url = if let Some(base) = ui_base {
            endpoint(base)?
        } else {
            self.base.clone()
        };
        url.query_pairs_mut()
            .append_pair("session", identifier(session)?)
            .append_pair("surface", "computer");
        Ok(url.into())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_endpoints_and_secret_free_panel_link() {
        for bad in [
            "http://example.com",
            "http://user:pass@localhost",
            "https://example.com/?token=secret",
            "file:///tmp/x",
            "https://example.com/#secret",
        ] {
            assert!(endpoint(bad).is_err());
        }
        let client = SessionClient::new("http://localhost:8788", &"x".repeat(32)).unwrap();
        assert_eq!(
            client
                .panel_url("chat", Some("http://localhost:5173"))
                .unwrap(),
            "http://localhost:5173/?session=chat&surface=computer"
        );
        assert!(client.panel_url("../other", None).is_err());
        assert!(client.url("https://other.test/api").is_err());
        assert!(client.url("//other.test/api").is_err());
    }
}

#[cfg(test)]
mod transport_tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };
    fn read_request(stream: &std::net::TcpStream) -> String {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut raw = String::new();
        let mut line = String::new();
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            raw.push_str(&line);
            if line == "\r\n" || line.is_empty() {
                break;
            }
        }
        raw
    }
    #[test]
    fn redirect_and_lost_response_never_repeat_mutation_or_forward_credentials() {
        for redirect in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let count = Arc::new(AtomicUsize::new(0));
            let seen = count.clone();
            let thread = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_request(&stream);
                assert!(request.starts_with("POST /api/chat/stream"));
                assert!(request.to_lowercase().contains("authorization: bearer "));
                seen.fetch_add(1, Ordering::SeqCst);
                if redirect {
                    write!(stream,"HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{address}/stolen\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}").unwrap();
                }
                drop(stream);
                listener.set_nonblocking(true).unwrap();
                std::thread::sleep(Duration::from_millis(100));
                assert!(listener.accept().is_err());
            });
            let client = SessionClient::new(&format!("http://{address}"), &"x".repeat(32)).unwrap();
            assert!(client.admit("session", "one-request", "hello").is_err());
            thread.join().unwrap();
            assert_eq!(count.load(Ordering::SeqCst), 1);
        }
    }
    #[test]
    fn snapshot_watermark_does_not_skip_admitted_events_and_reconnect_deduplicates() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let thread = std::thread::spawn(move || {
            for (index, after) in [0, 7].into_iter().enumerate() {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_request(&stream);
                assert!(request.starts_with(&format!(
                    "GET /api/sessions/session/live/stream?after_sequence={after} "
                )));
                let body = if index == 0 {
                    "event: snapshot\ndata: {\"session_id\":\"session\",\"sequence\":99}\n\nevent: update\ndata: {\"events\":[{\"sequence\":7,\"turn_id\":\"turn\",\"kind\":\"tool_card\"}]}\n\n"
                } else {
                    "event: update\ndata: {\"events\":[{\"sequence\":7},{\"sequence\":8,\"kind\":\"done\"}]}\n\n"
                };
                write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            }
        });
        let client = SessionClient::new(&format!("http://{address}"), &"x".repeat(32)).unwrap();
        let mut feed = client.feed("session", 0).unwrap();
        assert_eq!(feed.next_packet().unwrap().event, "snapshot");
        assert_eq!(feed.last_sequence(), 0);
        assert_eq!(feed.next_packet().unwrap().data["events"][0]["sequence"], 7);
        assert!(feed.next_packet().is_err());
        let mut resumed = client.feed("session", feed.last_sequence()).unwrap();
        let packet = resumed.next_packet().unwrap();
        assert_eq!(packet.data["events"].as_array().unwrap().len(), 1);
        assert_eq!(resumed.last_sequence(), 8);
        thread.join().unwrap();
    }
    #[test]
    fn wrong_session_snapshot_is_rejected() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let thread = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_request(&stream);
            let body = "event: snapshot\ndata: {\"session_id\":\"different\",\"sequence\":1}\n\n";
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        });
        let client = SessionClient::new(&format!("http://{address}"), &"x".repeat(32)).unwrap();
        assert_eq!(
            client
                .feed("session", 0)
                .unwrap()
                .next_packet()
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        thread.join().unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn token_file_refuses_shared_permissions_and_symlinks() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let root = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir(&root).unwrap();
        let token = root.join("token");
        std::fs::write(&token, "x".repeat(32)).unwrap();
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(read_token(&token).is_ok());
        let link = root.join("link");
        symlink(&token, &link).unwrap();
        assert!(read_token(&link).is_err());
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_token(&token).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
