//! Optional transport for one trusted tenant. All browser policy stays in Service.
use crate::{protocol, Service};
use axum::{
    body::to_bytes,
    extract::State,
    http::{header, Request, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Json, Router,
};
use data_encoding::BASE64;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    future::Future,
    io,
    net::SocketAddr,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{TcpListener, TcpStream},
    sync::{OwnedSemaphorePermit, Semaphore},
    time::Sleep,
};

// Bound accepted sockets, including clients that never finish HTTP headers.
pub struct Listener {
    inner: TcpListener,
    permits: Arc<Semaphore>,
}
pub struct Stream {
    inner: TcpStream,
    _permit: OwnedSemaphorePermit,
    idle: Pin<Box<Sleep>>,
}
impl axum::serve::Listener for Listener {
    type Io = Stream;
    type Addr = SocketAddr;
    async fn accept(&mut self) -> (Stream, SocketAddr) {
        loop {
            let permit = self
                .permits
                .clone()
                .acquire_owned()
                .await
                .expect("open semaphore");
            match self.inner.accept().await {
                Ok((inner, address)) => {
                    return (
                        Stream {
                            inner,
                            _permit: permit,
                            idle: Box::pin(tokio::time::sleep(Duration::from_secs(5))),
                        },
                        address,
                    )
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    }
    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }
}
impl AsyncRead for Stream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match Pin::new(&mut self.inner).poll_read(cx, buf) {
            Poll::Pending => match self.idle.as_mut().poll(cx) {
                Poll::Ready(_) => Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "HTTP read idle timeout",
                ))),
                Poll::Pending => Poll::Pending,
            },
            value => {
                self.idle
                    .as_mut()
                    .reset(tokio::time::Instant::now() + Duration::from_secs(5));
                value
            }
        }
    }
}
impl AsyncWrite for Stream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

pub struct Config {
    pub address: SocketAddr,
    token: String,
}

impl Config {
    pub fn load(address: &str, path: &Path) -> io::Result<Self> {
        use std::io::Read;
        let address = address
            .parse()
            .map_err(|_| io::Error::other("HTTP listen requires IP:PORT"))?;
        if !path.is_absolute() {
            return Err(io::Error::other(
                "HTTP token requires an absolute private file",
            ));
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        let meta = file.metadata()?;
        if !meta.is_file()
            || meta.uid() != crate::state::uid()
            || meta.mode() & 0o077 != 0
            || meta.len() > 258
        {
            return Err(io::Error::other(
                "HTTP token file must be private and owned",
            ));
        }
        let mut bytes = Vec::new();
        file.take(259).read_to_end(&mut bytes)?;
        let token = String::from_utf8(bytes).map_err(|_| io::Error::other("invalid HTTP token"))?;
        let token = token.trim_end_matches(['\r', '\n']).to_owned();
        if !(32..=256).contains(&token.len()) || !token.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(io::Error::other(
                "HTTP token must contain 32 to 256 printable ASCII characters",
            ));
        }
        Ok(Self { address, token })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Call {
    workspace_id: String,
    client_id: String,
    request_id: String,
    service_epoch: String,
    method: String,
    params: Value,
    #[serde(default)]
    authority: Option<protocol::Authority>,
    #[serde(default)]
    expected_binding: Option<String>,
    #[serde(default = "timeout_default")]
    timeout_ms: u64,
}
fn timeout_default() -> u64 {
    10000
}

#[derive(Clone)]
struct Api {
    service: Arc<Service>,
    token: Arc<String>,
    permits: Arc<Semaphore>,
}

fn failure(status: StatusCode, code: &str) -> Response {
    (
        status,
        Json(json!({"error":{"code":code,"message":crate::protocol::describe(code),"retryable":false}})),
    )
        .into_response()
}

async fn handle(State(api): State<Api>, request: Request<axum::body::Body>) -> Response {
    // This is a server-to-server API. Never grant ambient browser-origin access.
    if request.headers().contains_key(header::ORIGIN) {
        return failure(StatusCode::FORBIDDEN, "browser_origin_forbidden");
    }
    let expected = format!("Bearer {}", api.token);
    if !request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|value| crate::constant_time_equal(value, &expected))
    {
        return failure(StatusCode::UNAUTHORIZED, "authentication_required");
    }
    let Ok(permit) = api.permits.clone().try_acquire_owned() else {
        return failure(StatusCode::TOO_MANY_REQUESTS, "request_capacity_exceeded");
    };
    let path = request.uri().path();
    let method = request.method().as_str();
    match (method, path) {
        ("GET", "/health") => {
            Json(json!({"ready":true,"api_version":1,"service_epoch":api.service.state.epoch,
                "input_status":crate::journal_store::status(&api.service.state.root)}))
                .into_response()
        }
        ("GET", "/v1/info") => {
            let mut methods = vec!["health", "cancel"];
            if let Some(browsers) = &api.service.browsers {
                methods.extend_from_slice(crate::browser::METHODS);
                methods.extend_from_slice(crate::actions::METHODS);
                methods.extend_from_slice(crate::action_journal::CONTROL_METHODS);
                if browsers.scripts_available() {
                    methods.push("run_script");
                }
            }
            Json(json!({"api_version":1,"protocol_version":protocol::version(),"service_epoch":api.service.state.epoch,
                "methods":methods,"limits":{"message_bytes":protocol::limit("message_bytes"),"deadline_ms":protocol::limit("deadline_ms"),"concurrent_requests":protocol::limit("connections")}})).into_response()
        }
        ("GET", "/v1/schema") => (
            [(header::CONTENT_TYPE, "application/json")],
            protocol::SCHEMA,
        )
            .into_response(),
        ("POST", "/v1/rpc") => {
            if request
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .is_none_or(|v| v.split(';').next().unwrap_or("").trim() != "application/json")
            {
                return failure(StatusCode::UNSUPPORTED_MEDIA_TYPE, "json_required");
            }
            let bytes = match tokio::time::timeout(
                std::time::Duration::from_secs(5),
                to_bytes(request.into_body(), protocol::limit("message_bytes")),
            )
            .await
            {
                Err(_) => return failure(StatusCode::REQUEST_TIMEOUT, "request_body_timeout"),
                Ok(Err(_)) => return failure(StatusCode::PAYLOAD_TOO_LARGE, "message_too_large"),
                Ok(Ok(bytes)) => bytes,
            };
            let call: Call = match serde_json::from_slice(&bytes) {
                Ok(call) => call,
                Err(_) => return failure(StatusCode::BAD_REQUEST, "invalid_request"),
            };
            if call.timeout_ms == 0 || call.timeout_ms > protocol::limit("deadline_ms") as u64 {
                return failure(StatusCode::BAD_REQUEST, "invalid_deadline");
            }
            if matches!(call.method.as_str(), "handshake" | "shutdown" | "credential_vault_authorize") {
                return failure(StatusCode::FORBIDDEN, "administrative_method_forbidden");
            }
            let request = protocol::Request {
                protocol_version: protocol::version(),
                request_id: call.request_id,
                instance_id: api.service.state.instance_id.clone(),
                application_id: api.service.state.application_id.clone(),
                state_root: api.service.state.root.to_string_lossy().into_owned(),
                workspace_id: call.workspace_id,
                client_id: call.client_id,
                service_epoch: Some(call.service_epoch),
                deadline_ms: protocol::now_ms().saturating_add(call.timeout_ms),
                expected_binding: call.expected_binding,
                authority: call.authority,
                method: call.method,
                params: call.params,
            };
            // Once dispatched, preserve the action and its permit if the HTTP client
            // disconnects. The caller can recover through the existing receipt API.
            let service = api.service.clone();
            let reply = match tokio::spawn(async move {
                let _permit = permit;
                let mut authenticated =
                    Some((request.client_id.clone(), request.workspace_id.clone()));
                service.handle(&request, &mut authenticated).await.0
            })
            .await
            {
                Ok(reply) => reply,
                Err(_) => {
                    return failure(StatusCode::INTERNAL_SERVER_ERROR, "request_worker_failed")
                }
            };
            let mut value = serde_json::to_value(&reply).expect("serializable protocol reply");
            if !reply.binary.is_empty() {
                value["binary_base64"] = json!(BASE64.encode(&reply.binary));
            }
            Json(value).into_response()
        }
        _ => failure(StatusCode::NOT_FOUND, "endpoint_not_found"),
    }
}

pub async fn bind(config: Config, service: Arc<Service>) -> io::Result<(Listener, Router)> {
    if crate::constant_time_equal(&config.token, &service.state.token) {
        return Err(io::Error::other(
            "HTTP token must differ from the local daemon token",
        ));
    }
    let listener = Listener {
        inner: TcpListener::bind(config.address).await?,
        permits: Arc::new(Semaphore::new(protocol::limit("connections") * 2)),
    };
    let api = Api {
        service,
        token: Arc::new(config.token),
        permits: Arc::new(Semaphore::new(protocol::limit("connections"))),
    };
    let router = Router::new().fallback(any(handle)).with_state(api);
    Ok((listener, router))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn envelope_is_strict_and_epoch_is_required() {
        let mut call = json!({"workspace_id":"w","client_id":"c","request_id":"r","service_epoch":"e","method":"health","params":{}});
        assert!(serde_json::from_value::<Call>(call.clone()).is_ok());
        call["token"] = json!("leak");
        assert!(serde_json::from_value::<Call>(call.clone()).is_err());
        call.as_object_mut().unwrap().remove("token");
        call.as_object_mut().unwrap().remove("service_epoch");
        assert!(serde_json::from_value::<Call>(call).is_err());
    }
    #[test]
    fn token_file_requires_private_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("api.token");
        std::fs::write(&file, "x".repeat(64)).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(Config::load("127.0.0.1:0", &file).is_err());
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(Config::load("127.0.0.1:0", &file).is_ok());
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(Config::load("127.0.0.1:0", &link).is_err());
    }
}
