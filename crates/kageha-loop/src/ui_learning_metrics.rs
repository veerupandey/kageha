//! `GET /api/learning/metrics` (design section 15 "Metrics"; Requirement 17.5; task 25.2).
//!
//! Reports the learning store's per-task-type, per-ISO-week metrics
//! (`kageha_learning::metrics`): requests, verified requests, the verified rate, and the
//! mean model calls, cost and cached tokens per request. The route is behind the server's
//! token check like every `/api/` route (`server::handle`).
//!
//! Query parameters:
//! - `weeks`: ISO weeks reported, the current one included, `1..=104` (default 12).
//! - `task_type`: only this task type (a slug, see `kageha_learning::valid_task_type`).
//!
//! The response also includes observational `speed_metrics` for named task types, grouped
//! by agent and week. Elapsed times include human/offline waits and only verified requests
//! are used for latency percentiles; changes are not proof of controlled speedups.
use crate::server;
use crate::webui::{form_decode, query, Database};
use kageha_learning::{DEFAULT_METRICS_WEEKS, MAX_METRICS_WEEKS};
use serde_json::json;
use tiny_http::{Method, Request};

pub(crate) const PATH: &str = "/api/learning/metrics";

pub(crate) fn handles(path: &str) -> bool {
    path == PATH
}

pub(crate) fn handle(request: Request, db: &Database) -> std::io::Result<()> {
    if request.method() != &Method::Get {
        server::response(request, 405, json!({"error":"Method not allowed"}));
        return Ok(());
    }
    let url = request.url().to_owned();
    let weeks = match form_decode(&query(&url, "weeks")).as_deref() {
        Some("") => DEFAULT_METRICS_WEEKS,
        Some(text) if text.parse::<u32>().is_ok_and(|weeks| (1..=MAX_METRICS_WEEKS).contains(&weeks)) => {
            text.parse().unwrap_or(DEFAULT_METRICS_WEEKS)
        }
        _ => {
            server::response(request, 400,
                json!({"error":format!("weeks must be an integer from 1 to {MAX_METRICS_WEEKS}")}));
            return Ok(());
        }
    };
    let task_type = match form_decode(&query(&url, "task_type")) {
        Some(slug) if slug.is_empty() => None,
        Some(slug) if kageha_learning::valid_task_type(&slug) => Some(slug),
        _ => {
            server::response(request, 400,
                json!({"error":"task_type must be 1-64 characters of a-z, 0-9, '-' or '_'"}));
            return Ok(());
        }
    };
    let Some(store) = crate::ui_learning::store(db) else {
        server::response(request, 200, json!({"enabled":false,"weeks":weeks,"task_type":task_type,"metrics":[],"speed_metrics":[],"speed_metrics_note":"Elapsed wall time includes human and offline waits; groups do not control task difficulty or model. Only verified requests contribute elapsed samples.","cost_available":false}));
        return Ok(());
    };
    match store.metrics(weeks, task_type.as_deref()).and_then(|metrics|
        store.speed_metrics(weeks, task_type.as_deref()).map(|speed| (metrics, speed))) {
        Ok((metrics, speed_metrics)) => server::response(request, 200,
            json!({"enabled":true,"weeks":weeks,"task_type":task_type,"metrics":metrics,"speed_metrics":speed_metrics,
                "speed_metrics_note":"Elapsed wall time includes human and offline waits; groups do not control task difficulty or model. Only verified requests contribute elapsed samples.","cost_available":false})),
        Err(error) => {
            eprintln!("kageha: learning metrics unavailable: {error}");
            server::response(request, 500, json!({"error":"Learning metrics unavailable"}));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "ui_learning_metrics_tests.rs"]
mod tests;
