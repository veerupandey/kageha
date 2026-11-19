use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::VecDeque;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
};
use tokio_tungstenite::{
    client_async_with_config,
    tungstenite::{http::Uri, protocol::WebSocketConfig, Message},
    WebSocketStream,
};

pub type Result<T> = std::result::Result<T, &'static str>;

pub struct Cdp {
    socket: WebSocketStream<TcpStream>,
    next_id: u64,
    allow_web: bool,
    downloads: Option<VecDeque<Value>>,
    dialogs: Option<VecDeque<Value>>,
    console: Option<(String, VecDeque<Value>)>,
}

fn local_port(uri: &Uri) -> Option<u16> {
    if !matches!(uri.host(), Some("localhost" | "127.0.0.1"))
        || uri.authority()?.as_str().contains('@')
        || uri.query().is_some()
    {
        return None;
    }
    uri.port_u16().filter(|port| *port > 0)
}

pub fn debugger_path(path: &str) -> bool {
    path.strip_prefix("/devtools/browser/")
        .is_some_and(|id| id.len() == 36 && uuid::Uuid::parse_str(id).is_ok())
}

fn websocket_endpoint(uri: &Uri) -> Option<(u16, String)> {
    if uri.scheme_str() != Some("ws") || !debugger_path(uri.path()) {
        return None;
    }
    Some((local_port(uri)?, uri.path().to_owned()))
}

pub(crate) fn endpoint_port(endpoint: &str) -> Option<u16> {
    let uri: Uri = endpoint.parse().ok()?;
    match uri.scheme_str() {
        Some("http") if uri.path() == "/" => local_port(&uri),
        Some("ws") => websocket_endpoint(&uri).map(|(port, _)| port),
        _ => None,
    }
}

pub async fn discover(endpoint: &str) -> Result<(u16, String)> {
    let uri: Uri = endpoint.parse().map_err(|_| "cdp_invalid_endpoint")?;
    if let Some(address) = websocket_endpoint(&uri) {
        return Ok(address);
    }
    let port = local_port(&uri).ok_or("cdp_invalid_endpoint")?;
    if uri.scheme_str() != Some("http") || uri.path() != "/" {
        return Err("cdp_invalid_endpoint");
    }
    let mut tcp = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
        .await
        .map_err(|_| "cdp_unavailable")?;
    tcp.write_all(
        format!(
            "GET /json/version HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )
    .await
    .map_err(|_| "cdp_unavailable")?;
    // Chromium can leave the socket open after its HTTP response.
    let mut reply = BufReader::new(tcp).take(16 * 1024);
    let mut line = String::new();
    reply
        .read_line(&mut line)
        .await
        .map_err(|_| "cdp_invalid_reply")?;
    if !line.starts_with("HTTP/1.1 200 ") {
        return Err("cdp_invalid_reply");
    }
    let mut content_length = None;
    loop {
        line.clear();
        reply
            .read_line(&mut line)
            .await
            .map_err(|_| "cdp_invalid_reply")?;
        if line == "\r\n" {
            break;
        }
        let (name, value) = line.split_once(':').ok_or("cdp_invalid_reply")?;
        if name.eq_ignore_ascii_case("content-length") {
            content_length = value.trim().parse::<usize>().ok();
        }
    }
    let size = content_length
        .filter(|size| *size as u64 <= reply.limit())
        .ok_or("cdp_invalid_reply")?;
    let mut body = vec![0; size];
    reply
        .read_exact(&mut body)
        .await
        .map_err(|_| "cdp_invalid_reply")?;
    let version: Value = serde_json::from_slice(&body).map_err(|_| "cdp_invalid_reply")?;
    let debugger: Uri = field(&version, "webSocketDebuggerUrl")?
        .parse()
        .map_err(|_| "cdp_invalid_endpoint")?;
    websocket_endpoint(&debugger)
        .filter(|(debugger_port, _)| *debugger_port == port)
        .ok_or("cdp_invalid_endpoint")
}

impl Cdp {
    pub async fn connect(port: u16, path: &str, allow_web: bool) -> Result<Self> {
        let tcp = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .map_err(|_| "cdp_unavailable")?;
        let config = WebSocketConfig::default()
            .max_message_size(Some(8 * 1024 * 1024))
            .max_frame_size(Some(8 * 1024 * 1024));
        let (socket, _) =
            client_async_with_config(format!("ws://127.0.0.1:{port}{path}"), tcp, Some(config))
                .await
                .map_err(|_| "cdp_unavailable")?;
        Ok(Self {
            socket,
            next_id: 0,
            allow_web,
            downloads: None,
            dialogs: None,
            console: None,
        })
    }

    pub fn check_url(&self, url: &str) -> Result<()> {
        if crate::browser::fixture_url(url) {
            return Ok(());
        }
        if !self.allow_web {
            return Err("fixture_url_required");
        }
        if url.len() <= 2048
            && url.parse::<Uri>().is_ok_and(|uri| {
                matches!(uri.scheme_str(), Some("http" | "https"))
                    && uri.host().is_some()
                    && uri
                        .authority()
                        .is_some_and(|authority| !authority.as_str().contains('@'))
            })
        {
            return Ok(());
        }
        Err("unsupported_page_url")
    }

    pub async fn call(
        &mut self,
        session: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value> {
        let id = self.send_call(session, method, params).await?;
        self.wait_for_reply(id).await
    }

    pub async fn send_call(
        &mut self,
        session: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<u64> {
        self.next_id += 1;
        let id = self.next_id;
        let mut command = json!({"id": id, "method": method, "params": params});
        if let Some(session) = session {
            command["sessionId"] = json!(session);
        }
        self.socket
            .send(Message::Text(command.to_string().into()))
            .await
            .map_err(|_| "cdp_disconnected")?;
        Ok(id)
    }

    async fn wait_for_reply(&mut self, id: u64) -> Result<Value> {
        loop {
            let response = self.receive().await?;
            if response["id"].as_u64() != Some(id) {
                if let Some((session, logs)) = self.console.as_mut() {
                    if response["sessionId"] == session.as_str()
                        && matches!(response["method"].as_str(),Some("Log.entryAdded"|"Runtime.exceptionThrown")) {
                        let entry=if response["method"]=="Log.entryAdded" {&response["params"]["entry"]}else{&response["params"]["exceptionDetails"]};
                        let text=entry["exception"]["description"].as_str().or_else(||entry["text"].as_str()).unwrap_or("").chars().take(1280).collect::<String>();
                        if logs.len()==24 {logs.pop_front();}
                        logs.push_back(json!({"level":entry["level"].as_str().unwrap_or("error"),"text":text,"timestamp":entry["timestamp"],"source":entry["source"]}));
                        continue;
                    }
                    if response["sessionId"] == session.as_str()
                        && response["method"] == "Runtime.consoleAPICalled"
                    {
                        let params = &response["params"];
                        let text = params["args"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .take(8)
                            .map(|arg| match arg.get("value") {
                                Some(Value::String(text)) => {
                                    text.chars().take(160).collect::<String>()
                                }
                                Some(value @ (Value::Null | Value::Bool(_) | Value::Number(_))) => {
                                    value.to_string()
                                }
                                _ => "[object]".into(),
                            })
                            .collect::<Vec<_>>()
                            .join(" ");
                        if logs.len() == 24 {
                            logs.pop_front();
                        }
                        logs.push_back(json!({"level": params["type"], "text": text, "timestamp": params["timestamp"]}));
                        continue;
                    }
                }
                if let Some(dialogs) = self.dialogs.as_mut() {
                    if response["method"] == "Page.javascriptDialogOpening" {
                        if dialogs.len() >= 4 {
                            return Err("dialog_event_overflow");
                        }
                        dialogs.push_back(response);
                        continue;
                    }
                }
                if let Some(events) = self.downloads.as_mut() {
                    if is_download_event(&response) {
                        if events.len() >= 256 {
                            return Err("download_event_overflow");
                        }
                        events.push_back(response);
                    }
                }
                continue;
            }
            if response.get("error").is_some() {
                return Err("cdp_command_failed");
            }
            return response.get("result").cloned().ok_or("cdp_invalid_reply");
        }
    }

    pub async fn console_messages(&mut self, session: &str) -> Result<VecDeque<Value>> {
        self.console = Some((session.to_owned(), VecDeque::new()));
        let log_enabled=self.call(Some(session), "Log.enable", json!({})).await.is_ok();
        self.call(Some(session), "Runtime.enable", json!({}))
            .await?;
        self.call(Some(session), "Runtime.disable", json!({}))
            .await?;
        if log_enabled {self.call(Some(session), "Log.disable", json!({})).await?;}
        Ok(self
            .console
            .take()
            .map(|(_, messages)| messages)
            .unwrap_or_default())
    }

    async fn receive(&mut self) -> Result<Value> {
        while let Some(message) = self.socket.next().await {
            match message.map_err(|_| "cdp_disconnected")? {
                Message::Text(text) => {
                    return serde_json::from_str(&text).map_err(|_| "cdp_invalid_reply")
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
        Err("cdp_disconnected")
    }

    pub fn watch_downloads(&mut self) {
        self.downloads = Some(VecDeque::new());
    }

    pub fn watch_dialogs(&mut self) {
        self.dialogs = Some(VecDeque::new());
    }

    pub fn stop_watching_dialogs(&mut self) {
        self.dialogs = None;
    }

    pub async fn dialog_message(&mut self) -> Result<Value> {
        if let Some(event) = self.dialogs.as_mut().and_then(VecDeque::pop_front) {
            return Ok(event);
        }
        self.receive().await
    }

    pub async fn next_download_message(&mut self, input_reply: u64) -> Result<Value> {
        if let Some(event) = self.downloads.as_mut().and_then(VecDeque::pop_front) {
            return Ok(event);
        }
        loop {
            let event = self.receive().await?;
            if is_download_event(&event) || event["id"].as_u64() == Some(input_reply) {
                return Ok(event);
            }
        }
    }

    pub async fn attach(&mut self, target: &str) -> Result<String> {
        let result = self
            .call(
                None,
                "Target.attachToTarget",
                json!({"targetId": target, "flatten": true}),
            )
            .await?;
        Ok(field(&result, "sessionId")?.to_owned())
    }

    pub async fn detach(&mut self, session: &str) -> Result<()> {
        self.call(
            None,
            "Target.detachFromTarget",
            json!({"sessionId": session}),
        )
        .await?;
        Ok(())
    }
}

fn is_download_event(event: &Value) -> bool {
    matches!(
        event["method"].as_str(),
        Some("Browser.downloadWillBegin" | "Browser.downloadProgress")
    )
}

pub fn field<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value[name]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("cdp_invalid_reply")
}
