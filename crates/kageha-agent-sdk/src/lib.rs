//! Independent runtime client. No WebUI, host database or backend process dependency.
mod sse;
pub use kageha_contracts::policy::{ApprovalDecision, ApprovalRecord, Authority, GrantScope};
pub use kageha_runtime::lifecycle::{RunEvent, RunRecord, RunSpec, RunState, Steering};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};
pub use sse::Events;
use std::{fmt, time::Duration};
const LIMIT: usize = 32 * 1024 * 1024;
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug)]
pub struct Error {
    pub status: Option<u16>,
    pub code: String,
    pub message: String,
    pub retryable: bool,
}
impl Error {
    pub(crate) fn protocol(message: &str) -> Self {
        Self {
            status: None,
            code: "sdk_protocol".into(),
            message: message.into(),
            retryable: false,
        }
    }
    fn transport() -> Self {
        Self::protocol("Runtime transport failed; request outcome may be unknown. Inspect status before retrying input.")
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}
#[derive(Clone)]
pub struct AgentClient {
    endpoint: reqwest::Url,
    token: String,
    http: reqwest::Client,
    timeout: Duration,
}
impl fmt::Debug for AgentClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentClient").finish_non_exhaustive()
    }
}
#[derive(Debug, Deserialize)]
pub struct ApprovalPage {
    pub approvals: Vec<ApprovalRecord>,
    pub next_cursor: Option<u64>,
}
#[derive(Default)]
pub struct ApprovalFilter<'a> {
    pub session: Option<&'a str>,
    pub turn: Option<&'a str>,
    pub after: Option<u64>,
    pub pending: Option<bool>,
}
fn id(value: &str) -> Result<&str> {
    if value.is_empty()
        || value.len() > 128
        || value.chars().any(char::is_control)
        || matches!(value, "." | "..")
    {
        return Err(Error::protocol("Invalid resource identity"));
    }
    Ok(value)
}
impl AgentClient {
    pub fn new(endpoint: &str, token: &str) -> Result<Self> {
        let url = reqwest::Url::parse(endpoint)
            .map_err(|_| Error::protocol("Invalid runtime endpoint"))?;
        let local = url.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        if !(url.scheme() == "https" || url.scheme() == "http" && local)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || token.len() < 24
            || !token.bytes().all(|v| v.is_ascii_graphic())
        {
            return Err(Error::protocol(
                "HTTPS or loopback endpoint and an operator token required",
            ));
        }
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .build()
            .map_err(|_| Error::transport())?;
        Ok(Self {
            endpoint: url,
            token: token.into(),
            http,
            timeout: Duration::from_secs(30),
        })
    }
    pub fn with_timeout(mut self, timeout: Duration) -> Result<Self> {
        if timeout.is_zero() {
            return Err(Error::protocol("Positive timeout required"));
        }
        self.timeout = timeout;
        Ok(self)
    }
    fn url(&self, parts: &[&str]) -> Result<reqwest::Url> {
        let mut url = self.endpoint.clone();
        let mut path = url
            .path_segments_mut()
            .map_err(|_| Error::protocol("Endpoint cannot hold API paths"))?;
        path.pop_if_empty();
        for part in parts {
            path.push(part);
        }
        drop(path);
        Ok(url)
    }
    async fn request<T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        parts: &[&str],
        body: Option<Value>,
        query: &[(&str, String)],
    ) -> Result<T> {
        let mut request = self
            .http
            .request(method, self.url(parts)?)
            .bearer_auth(&self.token)
            .timeout(self.timeout)
            .query(query);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.map_err(|_| Error::transport())?;
        let status = response.status();
        let bytes = bounded(response).await?;
        if !status.is_success() {
            return Err(api_error(status.as_u16(), &bytes));
        }
        serde_json::from_slice(&bytes).map_err(|_| Error::protocol("Invalid runtime JSON response"))
    }
    pub async fn info(&self) -> Result<Value> {
        self.request(reqwest::Method::GET, &["v1", "info"], None, &[])
            .await
    }
    /// Stable IDs make exact retries inspectable. This method never retries automatically.
    pub async fn run(&self, spec: &RunSpec) -> Result<RunRecord> {
        spec.validate()
            .map_err(|_| Error::protocol("Invalid run specification"))?;
        id(&spec.id)?;
        self.request(
            reqwest::Method::POST,
            &["v1", "runs"],
            Some(json!(spec)),
            &[],
        )
        .await
    }
    pub async fn status(&self, run: &str) -> Result<RunRecord> {
        self.request(reqwest::Method::GET, &["v1", "runs", id(run)?], None, &[])
            .await
    }
    pub async fn cancel(&self, run: &str) -> Result<RunRecord> {
        self.request(
            reqwest::Method::POST,
            &["v1", "runs", id(run)?, "cancel"],
            Some(json!({})),
            &[],
        )
        .await
    }
    pub async fn steer(&self, run: &str, message: &Steering) -> Result<Value> {
        id(&message.id)?;
        self.request(
            reqwest::Method::POST,
            &["v1", "runs", id(run)?, "steer"],
            Some(json!(message)),
            &[],
        )
        .await
    }
    pub async fn approvals(&self, filter: ApprovalFilter<'_>) -> Result<ApprovalPage> {
        let mut q = Vec::new();
        if let Some(v) = filter.session {
            q.push(("session", v.into()));
        }
        if let Some(v) = filter.turn {
            q.push(("turn", v.into()));
        }
        if let Some(v) = filter.after {
            q.push(("after", v.to_string()));
        }
        if let Some(v) = filter.pending {
            q.push(("pending", v.to_string()));
        }
        self.request(reqwest::Method::GET, &["v1", "approvals"], None, &q)
            .await
    }
    pub async fn approval(&self, approval: &str) -> Result<ApprovalRecord> {
        self.request(
            reqwest::Method::GET,
            &["v1", "approvals", id(approval)?],
            None,
            &[],
        )
        .await
    }
    /// Caller supplies the reviewed authority, revision and explicit decision unchanged.
    pub async fn decide(
        &self,
        approval: &str,
        decision: &ApprovalDecision,
    ) -> Result<ApprovalRecord> {
        self.request(
            reqwest::Method::POST,
            &["v1", "approvals", id(approval)?, "decision"],
            Some(json!(decision)),
            &[],
        )
        .await
    }
    /// Dropping Events closes the stream; it does not cancel the server run.
    pub async fn events(&self, run: &str, after: u64) -> Result<Events> {
        if after > i64::MAX as u64 {
            return Err(Error::protocol("Invalid event cursor"));
        }
        let response = tokio::time::timeout(
            self.timeout,
            self.http
                .get(self.url(&["v1", "runs", id(run)?, "events"])?)
                .bearer_auth(&self.token)
                .header("Accept", "text/event-stream")
                .query(&[("after", after)])
                .send(),
        )
        .await
        .map_err(|_| Error::transport())?
        .map_err(|_| Error::transport())?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            return Err(api_error(
                status,
                &tokio::time::timeout(self.timeout, bounded(response))
                    .await
                    .map_err(|_| Error::transport())??,
            ));
        }
        if !response
            .headers()
            .get("content-type")
            .and_then(|s| s.to_str().ok())
            .is_some_and(|s| s.split(';').next() == Some("text/event-stream"))
        {
            return Err(Error::protocol("Expected runtime event stream"));
        }
        Ok(Events::new(response, run.into(), after, self.timeout))
    }
}
async fn bounded(response: reqwest::Response) -> Result<Vec<u8>> {
    use futures_util::StreamExt;
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| Error::transport())?;
        if bytes.len() + chunk.len() > LIMIT {
            return Err(Error::protocol("Runtime response exceeds size limit"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
fn api_error(status: u16, bytes: &[u8]) -> Error {
    let value = serde_json::from_slice::<Value>(bytes).unwrap_or(Value::Null);
    Error {
        status: Some(status),
        code: value["code"].as_str().unwrap_or("http_error").into(),
        message: value["message"]
            .as_str()
            .unwrap_or("Runtime request rejected")
            .into(),
        retryable: value["retryable"].as_bool().unwrap_or(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn endpoints_and_credentials_are_validated_and_hidden() {
        for endpoint in [
            "http://remote.example",
            "https://user:pass@example.com",
            "https://example.com?q=1",
            "https://example.com#secret",
        ] {
            assert!(AgentClient::new(endpoint, "test-operator-token-more-than-24").is_err());
        }
        assert!(AgentClient::new("http://localhost", "short").is_err());
        let client =
            AgentClient::new("http://localhost", "test-operator-token-more-than-24").unwrap();
        assert!(!format!("{client:?}").contains("test-operator"));
        assert!(id("..").is_err());
        assert!(client
            .url(&["v1", "runs", "x /?#"])
            .unwrap()
            .as_str()
            .ends_with("/v1/runs/x%20%2F%3F%23"));
    }
    #[tokio::test]
    async fn redirects_and_plain_http_failures_do_not_forward_credentials() {
        use axum::{http::StatusCode, routing::get, Router};
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        for status in [StatusCode::UNAUTHORIZED, StatusCode::FOUND] {
            let hits = Arc::new(AtomicUsize::new(0));
            let count = hits.clone();
            let router = Router::new().route(
                "/v1/info",
                get(move || async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    (status, [("location", "/secret-target")], "plain error")
                }),
            );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                axum::serve(listener, router).await.unwrap();
            });
            let client = AgentClient::new(
                &format!("http://{address}"),
                "test-operator-token-more-than-24",
            )
            .unwrap();
            let error = client.info().await.unwrap_err();
            assert_eq!(error.status, Some(status.as_u16()));
            assert_eq!(hits.load(Ordering::SeqCst), 1);
            server.abort();
        }
    }
}
