//! Authenticated transport shared by the standalone service and pooled HTTP client.
use axum::{
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use kageha_contracts::*;

use serde_json::Value;
use std::{sync::Arc, time::Duration};
const LIMIT: usize = 32 * 1024 * 1024;
pub use kageha_contracts::MemoryCall as Call;
#[derive(Clone)]
struct Host {
    port: Arc<dyn MemoryPort>,
    token: Arc<str>,
}
fn error(e: impl std::fmt::Display) -> ServiceError {
    ServiceError::new(
        ErrorCode::Unavailable,
        format!("Memory transport failed: {e}"),
    )
}
fn authorized(h: &HeaderMap, token: &str) -> bool {
    let supplied = h
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    let mut difference = supplied.len() ^ token.len();
    for (i, b) in token.bytes().enumerate() {
        difference |= (b ^ supplied.as_bytes().get(i).copied().unwrap_or(0)) as usize;
    }
    difference == 0
}
fn rejection() -> (StatusCode, Json<ServiceError>) {
    (
        StatusCode::UNAUTHORIZED,
        Json(ServiceError::new(
            ErrorCode::Unauthorized,
            "An operator credential is required",
        )),
    )
}
async fn info(
    State(s): State<Host>,
    h: HeaderMap,
) -> std::result::Result<Json<Descriptor>, (StatusCode, Json<ServiceError>)> {
    if !authorized(&h, &s.token) {
        return Err(rejection());
    }
    s.port
        .descriptor()
        .await
        .map(Json)
        .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, Json(e)))
}
async fn call(
    State(s): State<Host>,
    h: HeaderMap,
    Json(c): Json<Call>,
) -> std::result::Result<Json<Value>, (StatusCode, Json<ServiceError>)> {
    if !authorized(&h, &s.token) {
        return Err(rejection());
    }
    let result: Result<Value> = async {
        let port = match h.get("x-kageha-writer") {
            None => s.port.clone(),
            Some(value) => {
                let fence: MemoryFence = serde_json::from_slice(value.as_bytes())
                    .map_err(|_| ServiceError::new(ErrorCode::Invalid, "Invalid writer context"))?;
                s.port.with_fence(fence)?
            }
        };

        Ok(match c {
            Call::Put { request } => serde_json::to_value(port.put(request).await?),
            Call::Get { access, id } => serde_json::to_value(port.get(&access, &id).await?),
            Call::Delete { request } => serde_json::to_value(port.delete(request).await?),
            Call::Search {
                access,
                query,
                limit,
            } => serde_json::to_value(port.search(&access, &query, limit).await?),
            Call::List {
                access,
                after,
                limit,
            } => serde_json::to_value(port.list(&access, after, limit).await?),
            Call::Snapshot => serde_json::to_value(port.snapshot().await?),
            Call::Import { snapshot } => {
                port.import(snapshot).await?;
                Ok(Value::Null)
            }
        }
        .map_err(error)?)
    }
    .await;
    result.map(Json).map_err(|e| {
        let status = match e.code {
            ErrorCode::Unauthorized => StatusCode::FORBIDDEN,
            ErrorCode::Conflict => StatusCode::CONFLICT,
            ErrorCode::Invalid => StatusCode::BAD_REQUEST,
            _ => StatusCode::SERVICE_UNAVAILABLE,
        };
        (status, Json(e))
    })
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum FenceCall {
    Claim {
        owner: String,
        expected_epoch: u64,
        request_id: String,
    },
    Retire {
        fence: MemoryFence,
    },
}
async fn fence_status(
    State(s): State<Host>,
    h: HeaderMap,
) -> std::result::Result<Json<Option<MemoryFence>>, (StatusCode, Json<ServiceError>)> {
    if !authorized(&h, &s.token) {
        return Err(rejection());
    }
    s.port
        .fence_status()
        .await
        .map(Json)
        .map_err(|e| (StatusCode::SERVICE_UNAVAILABLE, Json(e)))
}
async fn fence_call(
    State(s): State<Host>,
    h: HeaderMap,
    Json(call): Json<FenceCall>,
) -> std::result::Result<Json<Value>, (StatusCode, Json<ServiceError>)> {
    if !authorized(&h, &s.token) {
        return Err(rejection());
    }
    let result: Result<Value> = async {
        match call {
            FenceCall::Claim {
                owner,
                expected_epoch,
                request_id,
            } => serde_json::to_value(
                s.port
                    .claim_fence(&owner, expected_epoch, &request_id)
                    .await?,
            )
            .map_err(error),
            FenceCall::Retire { fence } => {
                s.port.retire_fence(&fence).await?;
                Ok(Value::Null)
            }
        }
    }
    .await;
    result
        .map(Json)
        .map_err(|e| (StatusCode::CONFLICT, Json(e)))
}
/// This endpoint is operator-only: its credential permits full portable export/import.
/// Expose agent memory through a host that derives actor and namespace, never this token.
pub fn router(port: Arc<dyn MemoryPort>, token: String) -> Result<Router> {
    if token.len() < 32 || token.chars().any(char::is_whitespace) {
        return Err(ServiceError::new(
            ErrorCode::Invalid,
            "Operator token must be at least 32 non-whitespace bytes",
        ));
    }
    Ok(Router::new()
        .route("/v1/info", get(info))
        .route("/v1/calls", post(call))
        .route("/v1/fence", get(fence_status).post(fence_call))
        .layer(DefaultBodyLimit::max(LIMIT))
        .with_state(Host {
            port,
            token: token.into(),
        }))
}
#[derive(Clone)]
pub struct HttpMemory {
    client: reqwest::Client,
    endpoint: String,
    token: Arc<str>,
    fence: Option<MemoryFence>,
}
impl HttpMemory {
    pub fn new(endpoint: &str, token: String) -> Result<Self> {
        let url = reqwest::Url::parse(endpoint)
            .map_err(|_| ServiceError::new(ErrorCode::Invalid, "Invalid memory endpoint"))?;
        let loopback = url.host_str().is_some_and(|h| {
            h == "localhost"
                || h.parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        if !(url.scheme() == "https" || url.scheme() == "http" && loopback)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || token.len() < 32
        {
            return Err(ServiceError::new(
                ErrorCode::Invalid,
                "Memory endpoint requires HTTPS (or loopback HTTP) and an operator token",
            ));
        }
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(error)?;
        Ok(Self {
            client,
            endpoint: endpoint.trim_end_matches('/').into(),
            token: token.into(),
            fence: None,
        })
    }
    async fn response<T: serde::de::DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<T> {
        let request = if let Some(fence) = &self.fence {
            request.header(
                "x-kageha-writer",
                serde_json::to_string(fence).map_err(error)?,
            )
        } else {
            request
        };
        let mut response = request
            .bearer_auth(&*self.token)
            .send()
            .await
            .map_err(|e| error(e.without_url()))?;
        let status = response.status();
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| error(e.without_url()))? {
            if bytes.len() + chunk.len() > LIMIT {
                return Err(ServiceError::new(
                    ErrorCode::Uncertain,
                    "Memory response exceeds size limit; a dispatched mutation may have committed",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            return Err(serde_json::from_slice(&bytes).unwrap_or_else(|_| {
                ServiceError::new(
                    ErrorCode::Unavailable,
                    format!("Memory endpoint returned HTTP {status}"),
                )
            }));
        }
        serde_json::from_slice(&bytes).map_err(|_| {
            ServiceError::new(
                ErrorCode::Uncertain,
                "Memory endpoint returned an invalid response; a dispatched mutation may have committed",
            )
        })
    }
    async fn call<T: serde::de::DeserializeOwned>(&self, call: Call) -> Result<T> {
        self.response(
            self.client
                .post(format!("{}/v1/calls", self.endpoint))
                .json(&call),
        )
        .await
    }
}
#[async_trait::async_trait]
impl MemoryPort for HttpMemory {
    async fn fence_status(&self) -> Result<Option<MemoryFence>> {
        self.response(self.client.get(format!("{}/v1/fence", self.endpoint)))
            .await
    }
    async fn claim_fence(
        &self,
        owner: &str,
        expected_epoch: u64,
        request_id: &str,
    ) -> Result<MemoryFence> {
        self.response(
            self.client
                .post(format!("{}/v1/fence", self.endpoint))
                .json(&FenceCall::Claim {
                    owner: owner.into(),
                    expected_epoch,
                    request_id: request_id.into(),
                }),
        )
        .await
    }
    async fn retire_fence(&self, fence: &MemoryFence) -> Result<()> {
        self.response(
            self.client
                .post(format!("{}/v1/fence", self.endpoint))
                .json(&FenceCall::Retire {
                    fence: fence.clone(),
                }),
        )
        .await
    }
    fn with_fence(&self, fence: MemoryFence) -> Result<Arc<dyn MemoryPort>> {
        let mut port = self.clone();
        port.fence = Some(fence);
        Ok(Arc::new(port))
    }
    async fn descriptor(&self) -> Result<Descriptor> {
        self.response(self.client.get(format!("{}/v1/info", self.endpoint)))
            .await
    }
    async fn put(&self, request: PutMemory) -> Result<MemoryRecord> {
        self.call(Call::Put { request }).await
    }
    async fn get(&self, access: &MemoryAccess, id: &str) -> Result<Option<MemoryRecord>> {
        self.call(Call::Get {
            access: access.clone(),
            id: id.into(),
        })
        .await
    }
    async fn delete(&self, request: DeleteMemory) -> Result<MemoryRecord> {
        self.call(Call::Delete { request }).await
    }
    async fn search(
        &self,
        access: &MemoryAccess,
        query: &str,
        limit: usize,
    ) -> Result<Vec<MemoryRecord>> {
        self.call(Call::Search {
            access: access.clone(),
            query: query.into(),
            limit,
        })
        .await
    }
    async fn list(
        &self,
        access: &MemoryAccess,
        after: Option<String>,
        limit: usize,
    ) -> Result<MemoryPage> {
        self.call(Call::List {
            access: access.clone(),
            after,
            limit,
        })
        .await
    }
    async fn snapshot(&self) -> Result<MemorySnapshot> {
        self.call(Call::Snapshot).await
    }
    async fn import(&self, snapshot: MemorySnapshot) -> Result<()> {
        self.call(Call::Import { snapshot }).await
    }
}
