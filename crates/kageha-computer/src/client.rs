//! Synchronous, framework-independent protocol-2 child-process client.
//!
//! The client owns exactly one explicit executable and serializes calls through
//! `&mut self`. It never reconnects or replays a request. Unix pipe I/O has
//! a bounded request deadline; timeout closes the owned connection. Inspect
//! the durable receipt before any new input after an uncertain result.
//! A transport error once writing starts conservatively means outcome unknown.
use serde_json::{json, Value};
use std::{
    fmt,
    io::{self, BufRead, BufReader, Write},
    path::Path,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    time::{Duration, Instant},
};

const FRAME_LIMIT: usize = 8 * 1024 * 1024;

#[derive(Debug)]
pub enum ClientError {
    InvalidArgument(&'static str),
    Start(io::Error),
    Closed,
    NotDispatched(&'static str),
    OutcomeUnknown(&'static str),
    IncompatibleProtocol,
    /// A valid outer transport error, preserved without flattening its code.
    Server(Value),
}
impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArgument(message) | Self::NotDispatched(message) => f.write_str(message),
            Self::Start(error) => write!(f, "computer helper could not start: {error}"),
            Self::Closed => f.write_str("computer helper is closed; no request was sent"),
            Self::OutcomeUnknown(message) => write!(
                f,
                "{message}; recover the receipt before considering further input; do not replay"
            ),
            Self::IncompatibleProtocol => {
                f.write_str("computer helper did not advertise protocol 2")
            }
            Self::Server(error) => write!(
                f,
                "computer helper transport error: {}",
                error["code"].as_str().unwrap_or("unknown")
            ),
        }
    }
}
impl std::error::Error for ClientError {}

/// Host configuration, never a model-controlled protocol field.
#[derive(Clone, Copy, Debug)]
pub struct ClientOptions {
    pub allow_input: bool,
    /// Set only for an explicit operator return-control gesture.
    pub resume_control: bool,
    pub request_timeout: Duration,
}
impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            allow_input: false,
            resume_control: false,
            request_timeout: Duration::from_secs(10),
        }
    }
}
pub struct ComputerClient {
    child: Option<Child>,
    input: Option<ChildStdin>,
    output: Option<BufReader<ChildStdout>>,
    next_id: u64,
    workspace_id: String,
    client_id: String,
    epoch: String,
    info: Value,
    request_timeout: Duration,
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._:-".contains(&c))
}

impl ComputerClient {
    /// Start an explicit executable, never a PATH lookup or shell command.
    /// The journal must belong to this desktop controller; distinct untrusted
    /// tenants need distinct OS sessions, not merely different scope strings.
    pub fn launch(
        binary: &Path,
        journal: &Path,
        workspace_id: &str,
        client_id: &str,
    ) -> Result<Self, ClientError> {
        Self::launch_with_options(
            binary,
            journal,
            workspace_id,
            client_id,
            ClientOptions::default(),
        )
    }

    /// Host-only opt-in for a desktop whose input authority has already been
    /// granted. Never expose this constructor as a model-controlled operation.
    /// This does not grant macOS Accessibility or Screen Recording permission.
    pub fn launch_with_input(
        binary: &Path,
        journal: &Path,
        workspace_id: &str,
        client_id: &str,
    ) -> Result<Self, ClientError> {
        Self::launch_with_options(
            binary,
            journal,
            workspace_id,
            client_id,
            ClientOptions {
                allow_input: true,
                ..ClientOptions::default()
            },
        )
    }

    pub fn launch_with_options(
        binary: &Path,
        journal: &Path,
        workspace_id: &str,
        client_id: &str,
        options: ClientOptions,
    ) -> Result<Self, ClientError> {
        if options.request_timeout.is_zero() || options.request_timeout > Duration::from_secs(120) {
            return Err(ClientError::InvalidArgument(
                "request timeout must be greater than zero and at most 120 seconds",
            ));
        }
        if !binary.is_absolute() || !binary.is_file() {
            return Err(ClientError::InvalidArgument(
                "an existing absolute helper executable is required",
            ));
        }
        if !journal.is_absolute() || journal.file_name().is_none() {
            return Err(ClientError::InvalidArgument(
                "an absolute journal file path is required",
            ));
        }
        if !valid_identifier(workspace_id) || !valid_identifier(client_id) {
            return Err(ClientError::InvalidArgument(
                "workspace and client identifiers must be 1..128 safe ASCII bytes",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let metadata =
                std::fs::symlink_metadata(journal.parent().unwrap()).map_err(ClientError::Start)?;
            if !metadata.is_dir()
                || metadata.permissions().mode() & 0o077 != 0
                || metadata.uid() != unsafe { libc::geteuid() }
            {
                return Err(ClientError::InvalidArgument(
                    "journal parent must be an owned private directory",
                ));
            }
        }
        let mut command = Command::new(binary);
        command.arg("control").arg("--journal").arg(journal);
        if options.resume_control {
            command.arg("--resume-control");
        }
        if options.allow_input {
            command.arg("--allow-input");
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(ClientError::Start)?;
        let input = child.stdin.take().expect("piped stdin");
        let output = BufReader::new(child.stdout.take().expect("piped stdout"));
        let mut client = Self {
            child: Some(child),
            input: Some(input),
            output: Some(output),
            next_id: 1,
            workspace_id: workspace_id.to_owned(),
            client_id: client_id.to_owned(),
            epoch: String::new(),
            info: Value::Null,
            request_timeout: options.request_timeout,
        };
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            for fd in [
                client.input.as_ref().unwrap().as_raw_fd(),
                client.output.as_ref().unwrap().get_ref().as_raw_fd(),
            ] {
                nonblocking(fd).map_err(ClientError::Start)?;
            }
        }
        let info = client.exchange(
            json!({"protocol":2,"workspace_id":workspace_id,"client_id":client_id,
            "request_id":"connect","operation":"info","args":{}}),
        )?;
        if info["status"] != "ok"
            || info["protocol"] != 2
            || info["service_epoch"]
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 256)
                .is_none()
            || !info["operations"].is_array()
        {
            return Err(ClientError::IncompatibleProtocol);
        }
        client.epoch = info["service_epoch"].as_str().unwrap().to_owned();
        client.info = info;
        Ok(client)
    }

    pub fn is_closed(&self) -> bool {
        self.child.is_none()
    }
    /// Whether the helper process has exited, without sending a request. An exited helper
    /// is closed, so [`ComputerClient::is_closed`] is true afterwards.
    pub fn exited(&mut self) -> bool {
        let gone = match self.child.as_mut() {
            None => return true,
            Some(child) => !matches!(child.try_wait(), Ok(None)),
        };
        if gone {
            self.close();
        }
        gone
    }

    pub fn service_epoch(&self) -> &str {
        &self.epoch
    }
    pub fn info(&self) -> &Value {
        &self.info
    }

    /// Send once using fixed scope and the epoch pinned by launch. The caller
    /// owns the durable request ID; use receipt lookup after an uncertain result.
    /// Native operation errors remain structured values, not transport errors.
    pub fn call(
        &mut self,
        operation: &str,
        args: Value,
        request_id: &str,
    ) -> Result<Value, ClientError> {
        if !valid_identifier(request_id) || !args.is_object() {
            return Err(ClientError::InvalidArgument(
                "a safe request identifier and object args are required",
            ));
        }
        if operation == "info" {
            return Err(ClientError::InvalidArgument(
                "info is pinned at launch; construct a new client to reconnect",
            ));
        }
        if !self.info["operations"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item.as_str() == Some(operation)))
        {
            return Err(ClientError::InvalidArgument(
                "operation is not advertised by this helper",
            ));
        }
        self.exchange(
            json!({"protocol":2,"workspace_id":self.workspace_id,"client_id":self.client_id,
            "request_id":request_id,"service_epoch":self.epoch,"operation":operation,"args":args}),
        )
    }

    fn exchange(&mut self, request: Value) -> Result<Value, ClientError> {
        if self.child.is_none() {
            return Err(ClientError::Closed);
        }
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(ClientError::NotDispatched(
                "transport request IDs exhausted",
            ))?;
        let mut frame = serde_json::to_vec(&json!({"id":id,"method":"control","args":request}))
            .map_err(|_| ClientError::NotDispatched("request could not be encoded"))?;
        frame.push(b'\n');
        if frame.len() > FRAME_LIMIT {
            return Err(ClientError::NotDispatched("request exceeds 8 MiB"));
        }
        // Once write_all starts, even an error may follow a partial/full write.
        let deadline = Instant::now() + self.request_timeout;
        let write = write_frame(self.input.as_mut().unwrap(), &frame, deadline);
        if write.is_err() {
            self.close();
            return Err(ClientError::OutcomeUnknown(
                "helper transport failed or its deadline expired while sending a request",
            ));
        }
        let reply =
            read_frame_deadline(self.output.as_mut().unwrap(), deadline).and_then(|bytes| {
                serde_json::from_slice::<Value>(&bytes)
                    .map_err(|_| io::Error::other("invalid JSON response"))
            });
        let reply = match reply {
            Ok(value) => value,
            Err(_) => {
                self.close();
                return Err(ClientError::OutcomeUnknown(
                    "helper returned no complete valid response before its deadline",
                ));
            }
        };
        let has_result = reply.get("result").is_some();
        let has_error = reply.get("error").is_some();
        if !reply.is_object()
            || reply["id"].as_u64() != Some(id)
            || has_result == has_error
            || (has_result && !reply["result"].is_object())
            || (has_error && !reply["error"].is_object())
            || reply
                .as_object()
                .unwrap()
                .keys()
                .any(|key| !["id", "result", "error"].contains(&key.as_str()))
        {
            self.close();
            return Err(ClientError::OutcomeUnknown(
                "helper response identity or envelope is invalid",
            ));
        }
        if has_error {
            return Err(ClientError::Server(reply["error"].clone()));
        }
        Ok(reply["result"].clone())
    }

    /// Close this owned helper only. No shutdown is sent to another process.
    /// An already-dispatched effect remains unknown until receipt inspection.
    pub fn close(&mut self) {
        self.input.take();
        self.output.take();
        if let Some(mut child) = self.child.take() {
            if !matches!(child.try_wait(), Ok(Some(_))) {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
    }
}
impl Drop for ComputerClient {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(unix)]
fn nonblocking(fd: std::os::fd::RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
#[cfg(unix)]
fn wait_ready(fd: std::os::fd::RawFd, events: libc::c_short, deadline: Instant) -> io::Result<()> {
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::TimedOut, "computer request deadline expired")
            })?;
        let millis = remaining
            .as_millis()
            .saturating_add(1)
            .min(i32::MAX as u128) as i32;
        let mut item = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut item, 1, millis) };
        if result > 0 {
            return Ok(());
        } // Read/write exposes EOF/HUP/ERR without waiting again.
        if result == 0 {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "computer request deadline expired",
            ));
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}
fn write_frame(writer: &mut ChildStdin, mut frame: &[u8], deadline: Instant) -> io::Result<()> {
    while !frame.is_empty() {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "computer send deadline expired",
            ));
        }
        match writer.write(frame) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "computer pipe closed",
                ))
            }
            Ok(n) => frame = &frame[n..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                #[cfg(unix)]
                {
                    use std::os::fd::AsRawFd;
                    wait_ready(writer.as_raw_fd(), libc::POLLOUT, deadline)?;
                }
                #[cfg(not(unix))]
                {
                    return Err(error);
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
fn read_frame<R: BufRead>(reader: &mut R) -> io::Result<Vec<u8>> {
    read_frame_with(reader, Instant::now() + Duration::from_secs(60), || {
        Err(io::Error::from(io::ErrorKind::WouldBlock))
    })
}
fn read_frame_deadline(
    reader: &mut BufReader<ChildStdout>,
    deadline: Instant,
) -> io::Result<Vec<u8>> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let fd = reader.get_ref().as_raw_fd();
        read_frame_with(reader, deadline, || wait_ready(fd, libc::POLLIN, deadline))
    }
    #[cfg(not(unix))]
    {
        read_frame_with(reader, deadline, || {
            Err(io::Error::from(io::ErrorKind::WouldBlock))
        })
    }
}
fn read_frame_with<R: BufRead>(
    reader: &mut R,
    deadline: Instant,
    mut wait: impl FnMut() -> io::Result<()>,
) -> io::Result<Vec<u8>> {
    let mut frame = Vec::new();
    loop {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "computer response deadline expired",
            ));
        }
        let part = match reader.fill_buf() {
            Ok(part) => part,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                #[cfg(unix)]
                {
                    wait()?;
                }
                #[cfg(not(unix))]
                {
                    return Err(error);
                }
                continue;
            }
            Err(error) => return Err(error),
        };
        if part.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "incomplete response",
            ));
        }
        let count = part
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|i| i + 1)
            .unwrap_or(part.len());
        if frame.len() + count > FRAME_LIMIT {
            return Err(io::Error::other("response exceeds 8 MiB"));
        }
        let complete = part[count - 1] == b'\n';
        frame.extend_from_slice(&part[..count]);
        reader.consume(count);
        if complete {
            return Ok(frame);
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{DirBuilderExt, PermissionsExt},
        path::PathBuf,
    };
    struct Fixture {
        root: PathBuf,
        executable: PathBuf,
    }
    impl Fixture {
        fn new(after_info: &str) -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "kg-client-{}-{}",
                crate::instance_nonce(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
            let executable = root.join("helper");
            let script = format!("#!/bin/sh\n[ \"$1\" = control ] && [ \"$2\" = --journal ] || exit 2\nIFS= read -r request || exit 3\nprintf '%s\\n' '{{\"id\":1,\"result\":{{\"status\":\"ok\",\"protocol\":2,\"service_epoch\":\"fixture-epoch\",\"operations\":[\"execute\",\"receipt\",\"permissions\"]}}}}'\n{after_info}\n");
            fs::write(&executable, script).unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
            Self { root, executable }
        }
        fn launch(&self) -> ComputerClient {
            ComputerClient::launch(
                &self.executable,
                &self.root.join("journal"),
                "workspace",
                "agent",
            )
            .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn pins_scope_and_epoch_preserves_native_errors() {
        let fixture = Fixture::new("IFS= read -r request || exit 3\ncase \"$request\" in *'\"client_id\":\"agent\"'*'\"service_epoch\":\"fixture-epoch\"'*'\"workspace_id\":\"workspace\"'*) ;; *) exit 4;; esac\nprintf '%s\\n' '{\"id\":2,\"result\":{\"status\":\"error\",\"error\":{\"code\":\"observation_stale\"},\"execution_phase\":\"not_dispatched\"}}'");
        let mut client = fixture.launch();
        assert_eq!(client.service_epoch(), "fixture-epoch");
        let reply = client.call("execute", json!({}), "action-1").unwrap();
        assert_eq!(reply["error"]["code"], "observation_stale");
        assert_eq!(reply["execution_phase"], "not_dispatched");
    }
    #[test]
    fn lost_response_is_unknown_never_retried_and_closes_owned_helper() {
        let fixture =
            Fixture::new("IFS= read -r request || exit 3\nprintf '%s' once > \"$3.sent\"\nexit 0");
        let mut client = fixture.launch();
        assert!(matches!(
            client.call("execute", json!({}), "a"),
            Err(ClientError::OutcomeUnknown(_))
        ));
        assert!(matches!(
            client.call("execute", json!({}), "a"),
            Err(ClientError::Closed)
        ));
        assert_eq!(
            fs::read_to_string(fixture.root.join("journal.sent")).unwrap(),
            "once"
        );
    }
    #[test]
    fn mismatched_identity_is_unknown() {
        let fixture = Fixture::new(
            "IFS= read -r request\nprintf '%s\\n' '{\"id\":9,\"result\":{\"status\":\"ok\"}}'",
        );
        assert!(matches!(
            fixture.launch().call("permissions", json!({}), "a"),
            Err(ClientError::OutcomeUnknown(_))
        ));
    }
    #[test]
    fn validates_before_dispatch_and_keeps_transport_usable() {
        let fixture = Fixture::new(
            "IFS= read -r request\nprintf '%s\\n' '{\"id\":2,\"result\":{\"status\":\"ok\"}}'",
        );
        let mut client = fixture.launch();
        assert!(matches!(
            client.call("info", json!({}), "a"),
            Err(ClientError::InvalidArgument(_))
        ));
        assert!(matches!(
            client.call("shutdown", json!({}), "a"),
            Err(ClientError::InvalidArgument(_))
        ));
        assert!(matches!(
            client.call("execute", json!([]), "a"),
            Err(ClientError::InvalidArgument(_))
        ));
        assert_eq!(
            client.call("permissions", json!({}), "a").unwrap()["status"],
            "ok"
        );
    }
    #[test]
    fn enforces_frame_boundary_without_merging_responses() {
        let mut reader = io::Cursor::new(b"{}\n[]\n");
        assert_eq!(read_frame(&mut reader).unwrap(), b"{}\n");
        assert_eq!(read_frame(&mut reader).unwrap(), b"[]\n");
        assert!(read_frame(&mut io::Cursor::new(b"{}")).is_err());
        assert!(read_frame(&mut io::Cursor::new(vec![b'a'; FRAME_LIMIT + 1])).is_err());
    }
    #[test]
    fn input_authority_is_host_launch_flag_only() {
        let fixture = Fixture::new("[ \"$4\" = --allow-input ] || exit 4\nIFS= read -r request\nprintf '%s\\n' '{\"id\":2,\"result\":{\"status\":\"ok\"}}'");
        let mut client = ComputerClient::launch_with_input(
            &fixture.executable,
            &fixture.root.join("journal"),
            "workspace",
            "agent",
        )
        .unwrap();
        assert_eq!(
            client.call("permissions", json!({}), "a").unwrap()["status"],
            "ok"
        );
    }

    #[test]
    fn oversized_request_never_reaches_helper() {
        let fixture = Fixture::new(
            "IFS= read -r request\nprintf '%s\\n' '{\"id\":3,\"result\":{\"status\":\"ok\"}}'",
        );
        let mut client = fixture.launch();
        assert!(matches!(
            client.call("execute", json!({"text":"x".repeat(FRAME_LIMIT)}), "a"),
            Err(ClientError::NotDispatched(_))
        ));
        assert_eq!(
            client.call("permissions", json!({}), "b").unwrap()["status"],
            "ok"
        );
    }

    #[test]
    fn stalled_response_and_backpressure_are_bounded_and_never_replayed() {
        for (body, args) in [
            (
                "IFS= read -r request\nprintf once > \"$3.sent\"\nexec /bin/sleep 5",
                json!({}),
            ),
            ("exec /bin/sleep 5", json!({"text":"x".repeat(1024*1024)})),
        ] {
            let fixture = Fixture::new(body);
            let mut client = ComputerClient::launch_with_options(
                &fixture.executable,
                &fixture.root.join("journal"),
                "workspace",
                "agent",
                ClientOptions {
                    request_timeout: Duration::from_secs(2),
                    ..Default::default()
                },
            )
            .unwrap();
            client.request_timeout = Duration::from_millis(100);
            let started = Instant::now();
            assert!(matches!(
                client.call("execute", args, "once"),
                Err(ClientError::OutcomeUnknown(_))
            ));
            assert!(started.elapsed() < Duration::from_secs(2));
            assert!(client.is_closed());
            assert!(matches!(
                client.call("execute", json!({}), "once"),
                Err(ClientError::Closed)
            ));
        }
    }
    #[test]
    fn stalled_handshake_is_bounded() {
        let fixture = Fixture::new("exit 0");
        fs::write(&fixture.executable, "#!/bin/sh\nexec /bin/sleep 5\n").unwrap();
        let started = Instant::now();
        assert!(ComputerClient::launch_with_options(
            &fixture.executable,
            &fixture.root.join("journal"),
            "workspace",
            "agent",
            ClientOptions {
                request_timeout: Duration::from_millis(100),
                ..Default::default()
            }
        )
        .is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn refuses_relative_executable_and_journal() {
        assert!(matches!(
            ComputerClient::launch(Path::new("helper"), Path::new("/tmp/journal"), "a", "b"),
            Err(ClientError::InvalidArgument(_))
        ));
        let fixture = Fixture::new("exit 0");
        assert!(matches!(
            ComputerClient::launch(&fixture.executable, Path::new("journal"), "a", "b"),
            Err(ClientError::InvalidArgument(_))
        ));
    }
}
