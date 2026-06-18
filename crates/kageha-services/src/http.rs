//! Optional Axum hosting adapter. Composition registers implementations before serving.
use crate::Registry;
use axum::{
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{
        sse::{Event, KeepAlive},
        IntoResponse, Response, Sse,
    },
    routing::{get, post},
    Json, Router,
};
use kageha_contracts::*;
use serde::Deserialize;
use std::{convert::Infallible, sync::Arc, time::Duration};
#[derive(Clone)]
struct Host {
    registry: Arc<Registry>,
    token: Arc<str>,
}
async fn authenticate(State(host): State<Host>, request: Request, next: Next) -> Response {
    let supplied = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    let mut diff = supplied.len() ^ host.token.len();
    for (i, b) in host.token.bytes().enumerate() {
        diff |= (b ^ supplied.as_bytes().get(i).copied().unwrap_or(0)) as usize;
    }
    if diff != 0 {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    next.run(request).await
}
fn failure(e: ServiceError) -> (StatusCode, Json<ServiceError>) {
    let code = match e.code {
        ErrorCode::Invalid => StatusCode::BAD_REQUEST,
        ErrorCode::Conflict | ErrorCode::StaleGeneration => StatusCode::CONFLICT,
        ErrorCode::Unauthorized => StatusCode::FORBIDDEN,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    };
    (code, Json(e))
}
async fn providers(
    State(host): State<Host>,
) -> std::result::Result<
    Json<std::collections::BTreeMap<String, Descriptor>>,
    (StatusCode, Json<ServiceError>),
> {
    host.registry.descriptors().await.map(Json).map_err(failure)
}
async fn binding(
    State(host): State<Host>,
) -> std::result::Result<Json<Option<Binding>>, (StatusCode, Json<ServiceError>)> {
    host.registry.binding().map(Json).map_err(failure)
}
#[derive(Deserialize)]
struct Submit {
    expected_generation: Option<u64>,
    call: MemoryCall,
}
async fn call(
    State(host): State<Host>,
    Json(input): Json<Submit>,
) -> std::result::Result<Json<Routed<serde_json::Value>>, (StatusCode, Json<ServiceError>)> {
    host.registry
        .call(input.expected_generation, input.call)
        .await
        .map(Json)
        .map_err(failure)
}
#[derive(Deserialize)]
struct Replace {
    request_id: String,
    expected_generation: u64,
    decision: Decision,
}
#[derive(Deserialize)]
struct Decision {
    destination_id: String,
    instance_id: String,
    allow_data_transfer: bool,
}
async fn replace(
    State(host): State<Host>,
    Json(input): Json<Replace>,
) -> std::result::Result<Json<Replacement>, (StatusCode, Json<ServiceError>)> {
    if !input.decision.allow_data_transfer {
        return Err(failure(ServiceError::new(
            ErrorCode::Unauthorized,
            "Explicit destination-scoped data-transfer decision is required",
        )));
    }
    host.registry
        .replace(
            &input.request_id,
            &input.decision.destination_id,
            &input.decision.instance_id,
            input.expected_generation,
        )
        .await
        .map(Json)
        .map_err(failure)
}
async fn job(
    State(host): State<Host>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> std::result::Result<Json<Option<Replacement>>, (StatusCode, Json<ServiceError>)> {
    host.registry.job(&id).map(Json).map_err(failure)
}
async fn resume(
    State(host): State<Host>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> std::result::Result<Json<Replacement>, (StatusCode, Json<ServiceError>)> {
    host.registry.resume(&id).await.map(Json).map_err(failure)
}
async fn events(
    State(host): State<Host>,
    headers: HeaderMap,
) -> Sse<impl futures_util::Stream<Item = std::result::Result<Event, Infallible>>> {
    let cursor = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    let stream = futures_util::stream::unfold(
        (host.registry, cursor, std::collections::VecDeque::new()),
        |(registry, mut cursor, mut queued)| async move {
            loop {
                if let Some((id, job)) = queued.pop_front() {
                    cursor = id;
                    let event = Event::default()
                        .id(id.to_string())
                        .event("replacement")
                        .json_data(job)
                        .unwrap_or_else(|_| {
                            Event::default().event("error").data("serialization_failed")
                        });
                    return Some((Ok(event), (registry, cursor, queued)));
                }
                match registry.events(cursor) {
                    Ok(events) => queued.extend(events),
                    Err(_) => {
                        return Some((
                            Ok(Event::default().event("error").data("registry_unavailable")),
                            (registry, cursor, queued),
                        ))
                    }
                }
                if queued.is_empty() {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        },
    );
    Sse::new(stream).keep_alive(KeepAlive::default())
}
/// Operator API; an application adapter must derive actor/namespace for untrusted agent callers.
pub fn router(registry: Arc<Registry>, token: String) -> Result<Router> {
    if token.len() < 32 || token.chars().any(char::is_whitespace) {
        return Err(ServiceError::new(
            ErrorCode::Invalid,
            "Operator token must contain at least 32 non-whitespace bytes",
        ));
    }
    let host = Host {
        registry,
        token: token.into(),
    };
    Ok(Router::new()
        .route("/v1/bindings", get(binding))
        .route("/v1/providers", get(providers))
        .route("/v1/calls", post(call))
        .route("/v1/replacements", post(replace))
        .route("/v1/replacements/{id}", get(job))
        .route("/v1/replacements/{id}/resume", post(resume))
        .route("/v1/events", get(events))
        .layer(axum::extract::DefaultBodyLimit::max(256 * 1024))
        .layer(middleware::from_fn_with_state(host.clone(), authenticate))
        .with_state(host))
}
