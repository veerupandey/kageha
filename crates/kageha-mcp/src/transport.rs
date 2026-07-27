use crate::Config;
use kageha_contracts::execution::Control;
use serde_json::{json, Value};
use std::{
    io::{self, Read, Write},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};
const LIMIT: usize = 1048576;
fn rpc(method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":uuid::Uuid::new_v4().to_string(),"method":method,"params":params})
}
fn result(value: Value, id: &Value) -> io::Result<Value> {
    if value["jsonrpc"] != "2.0" || value["id"] != *id {
        return Err(io::Error::other("MCP response identity mismatch"));
    }
    if value.get("error").is_some() {
        return Ok(
            json!({"isError":true,"content":[{"type":"text","text":"MCP server returned an error"}]}),
        );
    }
    value
        .get("result")
        .cloned()
        .ok_or_else(|| io::Error::other("MCP result missing"))
}
struct Http {
    runtime: Option<tokio::runtime::Runtime>,
    client: reqwest::Client,
    allowed_origin: Option<String>,
}
impl Drop for Http {
    fn drop(&mut self) { if let Some(runtime)=self.runtime.take() {runtime.shutdown_background();} }
}
fn http() -> io::Result<Arc<Http>> {
    static POOL: OnceLock<Result<Arc<Http>, String>> = OnceLock::new();
    POOL.get_or_init(|| build_http(None).map(Arc::new).map_err(|e|e.to_string()))
    .as_ref().cloned().map_err(|e| io::Error::other(e.clone()))
}
fn build_http(mutual_tls: Option<(&str,&str)>) -> io::Result<Http> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|_| io::Error::other("MCP I/O runtime unavailable"))?;
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .pool_max_idle_per_host(8);
        let allowed_origin=if let Some((pem,endpoint))=mutual_tls {
            let url=reqwest::Url::parse(endpoint).map_err(|_|io::Error::other("Invalid mutual TLS endpoint"))?;
            if url.scheme()!="https" {return Err(io::Error::other("Mutual TLS requires HTTPS"));}
            let identity=reqwest::Identity::from_pem(pem.as_bytes()).map_err(|_|io::Error::other("Invalid mutual TLS identity"))?;
            builder=builder.no_proxy().identity(identity);
            Some(url.origin().ascii_serialization())
        } else {None};
        if let Some(path) = std::env::var_os("KAGEHA_CA_BUNDLE")
            .or_else(|| std::env::var_os("CURL_CA_BUNDLE"))
            .or_else(|| std::env::var_os("SSL_CERT_FILE"))
        {
            let mut bytes = Vec::new();
            std::fs::File::open(path)
                .map_err(|_| io::Error::other("MCP trust bundle unavailable"))?
                .take((LIMIT + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|_| io::Error::other("MCP trust bundle unreadable"))?;
            if bytes.len() > LIMIT {
                return Err(io::Error::other("MCP trust bundle exceeds limit"));
            }
            let certs = reqwest::Certificate::from_pem_bundle(&bytes)
                .map_err(|_| io::Error::other("Invalid MCP trust bundle"))?;
            if certs.is_empty() {
                return Err(io::Error::other("Empty MCP trust bundle"));
            }
            builder = builder.tls_built_in_root_certs(false);
            for cert in certs {
                builder = builder.add_root_certificate(cert);
            }
        }
        let client = builder
            .build()
            .map_err(|_| io::Error::other("MCP HTTP client unavailable"))?;
        Ok(Http { runtime:Some(runtime), client, allowed_origin })
}
fn http_request(
    config: &Config,
    scoped_http: Option<&Arc<Http>>,
    body: Option<&Value>,
    session: Option<&str>,
    protocol: &str,
    control: &Control<'_>,
) -> io::Result<(Value, Option<String>)> {
    if tokio::runtime::Handle::try_current().is_ok() {
        return std::thread::scope(|scope| {
            scope
                .spawn(|| http_request_inner(config, scoped_http, body, session, protocol, control))
                .join()
                .map_err(|_| io::Error::other("MCP worker failed"))?
        });
    }
    http_request_inner(config, scoped_http, body, session, protocol, control)
}
fn http_request_inner(
    config: &Config,
    scoped_http: Option<&Arc<Http>>,
    body: Option<&Value>,
    session: Option<&str>,
    protocol: &str,
    control: &Control<'_>,
) -> io::Result<(Value, Option<String>)> {
    control.check()?;
    let pool = match scoped_http {Some(client)=>client.clone(),None=>http()?};
    if let Some(origin)=&pool.allowed_origin {
        let url=reqwest::Url::parse(&config.url).map_err(|_|io::Error::other("Invalid mutual TLS endpoint"))?;
        if url.scheme()!="https" || url.origin().ascii_serialization()!=*origin {return Err(io::Error::new(io::ErrorKind::PermissionDenied,"Mutual TLS request is outside its configured origin"));}
    }
    pool.runtime.as_ref().expect("live MCP runtime").block_on(async{
  let operation=async{
   let mut request=if body.is_some(){pool.client.post(&config.url)}else{pool.client.delete(&config.url)};
   request=request.header("Content-Type","application/json").header("Accept","application/json, text/event-stream");
   for(key,value)in &config.headers{request=request.header(key,value);}
   if let Some(session)=session{request=request.header("Mcp-Session-Id",session);}
   if !protocol.is_empty(){request=request.header("MCP-Protocol-Version",protocol);}
   if let Some(body)=body {let data=serde_json::to_vec(body)?;if data.len()>LIMIT{return Err(io::Error::other("MCP request exceeds limit"));}request=request.body(data);}
   let mut response=request.send().await.map_err(|_|io::Error::other("MCP HTTP transport failed"))?;
   if !response.status().is_success(){return Err(io::Error::other(format!("MCP HTTP returned {}",response.status().as_u16())));}
   let session=response.headers().get("mcp-session-id").map(|v|v.to_str().map(str::to_owned)).transpose().map_err(|_|io::Error::other("Invalid MCP session identity"))?;
   if session.as_ref().is_some_and(|s|s.is_empty()||s.len()>1024||s.chars().any(char::is_control)){return Err(io::Error::other("Invalid MCP session identity"));}
   if body.is_none_or(|b|b.get("id").is_none()) {return Ok((Value::Null,session));}
   if response.content_length().is_some_and(|n|n>LIMIT as u64){return Err(io::Error::other("MCP response exceeds limit"));}
   let expected=&body.unwrap()["id"];let mut bytes=Vec::new();
   while let Some(chunk)=response.chunk().await.map_err(|_|io::Error::other("MCP response interrupted"))?{
    if bytes.len()+chunk.len()>LIMIT{return Err(io::Error::other("MCP response exceeds limit"));}bytes.extend_from_slice(&chunk);
    if let Ok(value)=serde_json::from_slice::<Value>(&bytes){return Ok((value,session));}
    let text=std::str::from_utf8(&bytes).unwrap_or("").replace("\r\n","\n");
    for frame in text.split("\n\n").take(text.matches("\n\n").count()) {
     let data=frame.lines().filter_map(|l|l.strip_prefix("data:")).map(str::trim_start).collect::<Vec<_>>().join("\n");
     if let Ok(value)=serde_json::from_str::<Value>(&data){if value["id"]==*expected{return Ok((value,session));}}
    }
   }
   Err(io::Error::other("MCP response did not contain matching JSON-RPC data"))
  };
  tokio::select!{result=operation=>result,_=async{loop{if control.check().is_err(){break;}tokio::time::sleep(Duration::from_millis(10)).await;}}=>{control.check()?;Err(io::Error::other("MCP call interrupted"))}}
 })
}
fn nonblocking<T: std::os::fd::AsRawFd>(file: &T) -> io::Result<()> {
    let fd = file.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
struct Process {
    child: Child,
    input: ChildStdin,
    output: ChildStdout,
    buffer: Vec<u8>,
}
impl Process {
    fn start(config: &Config) -> io::Result<Self> {
        use std::os::unix::process::CommandExt;
        let mut command = if let Some(sandbox)=&config.sandbox {sandbox.command(&config.command,&config.args)?}else{let mut command=Command::new(&config.command);command.args(&config.args);command};
        if !config.inherit_env {
            command.env_clear();
            for key in ["PATH", "HOME", "TMPDIR", "LANG"] {
                if let Some(v) = std::env::var_os(key) {
                    command.env(key, v);
                }
            }
        }
        command
            .envs(&config.env)
            .env_remove("KAGEHA_SERVER_TOKEN")
            .env_remove("KAGEHA_API_KEY")
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command
            .spawn()
            .map_err(|_| io::Error::other("MCP process could not start"))?;
        let input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        if let Err(error) = nonblocking(&input).and_then(|_| nonblocking(&output)) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        Ok(Self {
            child,
            input,
            output,
            buffer: Vec::new(),
        })
    }
    fn send(&mut self, value: &Value, control: &Control<'_>) -> io::Result<()> {
        let mut bytes = serde_json::to_vec(value)?;
        bytes.push(b'\n');
        if bytes.len() > LIMIT {
            return Err(io::Error::other("MCP request exceeds limit"));
        }
        let mut sent = 0;
        while sent < bytes.len() {
            control.check()?;
            match self.input.write(&bytes[sent..]) {
                Ok(0) => return Err(io::Error::other("MCP input closed")),
                Ok(n) => sent += n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(_) => return Err(io::Error::other("MCP input failed")),
            }
        }
        Ok(())
    }
    fn receive(&mut self, id: &Value, control: &Control<'_>) -> io::Result<Value> {
        let mut ignored = 0;
        loop {
            control.check()?;
            while let Some(end) = self.buffer.iter().position(|b| *b == b'\n') {
                let line = self.buffer.drain(..=end).collect::<Vec<_>>();
                if let Ok(value) = serde_json::from_slice::<Value>(&line) {
                    if value["id"] == *id {
                        return Ok(value);
                    }
                }
                ignored += 1;
                if ignored > 128 {
                    return Err(io::Error::other("MCP response identity did not arrive"));
                }
            }
            let mut part = [0u8; 8192];
            match self.output.read(&mut part) {
                Ok(0) => return Err(io::Error::other("MCP process disconnected")),
                Ok(n) => {
                    if self.buffer.len() + n > LIMIT {
                        return Err(io::Error::other("MCP response exceeds limit"));
                    }
                    self.buffer.extend_from_slice(&part[..n]);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(_) => return Err(io::Error::other("MCP output failed")),
            }
        }
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
pub(crate) struct Connection {
    process: Option<Process>,
    scoped_http: Option<Arc<Http>>,
    session: Option<String>,
    protocol: String,
}
impl Connection {
    pub fn connect(config: &Config, control: &Control<'_>) -> io::Result<Self> {
        let mut state = Self {
            process: if config.transport == "stdio" {
                Some(Process::start(config)?)
            } else {
                None
            },
            scoped_http: config.mutual_tls_identity.as_ref().map(|pem|build_http(Some((pem.as_str(),&config.url))).map(Arc::new)).transpose()?,
            session: None,
            protocol: String::new(),
        };
        let init = rpc(
            "initialize",
            json!({"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"kageha-rust-mcp","version":"0.1.0"}}),
        );
        let response = state.request(config, &init, control)?;
        let initialized = result(response, &init["id"])?;
        let protocol = initialized["protocolVersion"]
            .as_str()
            .ok_or_else(|| io::Error::other("MCP protocol version missing"))?;
        if protocol.len() > 64 || !protocol.bytes().all(|b| b.is_ascii_digit() || b == b'-') {
            return Err(io::Error::other("Invalid MCP protocol version"));
        }
        state.protocol = protocol.into();
        state.request(
            config,
            &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            control,
        )?;
        Ok(state)
    }
    fn request(
        &mut self,
        config: &Config,
        body: &Value,
        control: &Control<'_>,
    ) -> io::Result<Value> {
        if let Some(process) = &mut self.process {
            process.send(body, control)?;
            if body.get("id").is_none() {
                return Ok(Value::Null);
            }
            return process.receive(&body["id"], control);
        }
        let (value, session) = http_request(
            config,
            self.scoped_http.as_ref(),
            Some(body),
            self.session.as_deref(),
            &self.protocol,
            control,
        )?;
        if let Some(session) = session {
            if self.session.as_ref().is_some_and(|old| old != &session) {
                return Err(io::Error::other("MCP session changed during a call"));
            }
            self.session = Some(session);
        }
        Ok(value)
    }
    pub fn call(
        &mut self,
        config: &Config,
        method: &str,
        params: Value,
        control: &Control<'_>,
    ) -> io::Result<Value> {
        let request = rpc(method, params);
        let value = self.request(config, &request, control)?;
        result(value, &request["id"])
    }
    pub fn close(&mut self, config: &Config) {
        if self.process.is_none() && self.session.is_some() {
            let flag = std::sync::atomic::AtomicBool::new(false);
            let control = Control {
                cancelled: &flag,
                deadline: Some(Instant::now() + Duration::from_millis(250)),
            };
            let _ = http_request(
                config,
                self.scoped_http.as_ref(),
                None,
                self.session.as_deref(),
                &self.protocol,
                &control,
            );
        }
        self.process = None;
        self.session = None;
    }
}
