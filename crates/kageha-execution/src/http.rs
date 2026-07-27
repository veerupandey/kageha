//! Operator-only receipt inspection and explicit continuation decisions; never agent tools.
use crate::journal::Journal;
use axum::{
    extract::{Path, Query, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
#[derive(Clone)]
struct Host {
    journal: Arc<Journal>,
    token: Arc<str>,
}
async fn auth(State(host): State<Host>, request: Request, next: Next) -> Response {
    let supplied = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
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
#[derive(Deserialize)]
struct Scope {
    session: Option<String>,
}
async fn receipts(
    State(host): State<Host>,
    Query(scope): Query<Scope>,
) -> Result<Json<Value>, StatusCode> {
    tokio::task::spawn_blocking(move || {
        host.journal
            .receipts(scope.session.as_deref())
            .map(|v| Json(json!({"receipts":v})))
    })
    .await
    .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
    .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Decision {
    target: String,
    allow_continuation_without_replay: bool,
}
async fn acknowledge(
    State(host): State<Host>,
    Path(id): Path<String>,
    Json(decision): Json<Decision>,
) -> Result<Json<Value>, StatusCode> {
    if !decision.allow_continuation_without_replay {
        return Err(StatusCode::FORBIDDEN);
    }
    tokio::task::spawn_blocking(move || host.journal.acknowledge(&id, &decision.target))
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .map_err(|_| StatusCode::CONFLICT)?;
    Ok(Json(
        json!({"acknowledged":true,"historical_outcome":"unknown","replayed":false}),
    ))
}
pub fn router(journal: Arc<Journal>, token: String) -> std::io::Result<Router> {
    if token.len() < 32 || token.chars().any(char::is_whitespace) {
        return Err(std::io::Error::other(
            "Operator token must have at least 32 non-whitespace bytes",
        ));
    }
    let host = Host {
        journal,
        token: token.into(),
    };
    Ok(Router::new()
        .route("/v1/execution/receipts", get(receipts))
        .route("/v1/execution/receipts/{id}/acknowledge", post(acknowledge))
        .layer(axum::extract::DefaultBodyLimit::max(4096))
        .layer(middleware::from_fn_with_state(host.clone(), auth))
        .with_state(host))
}
