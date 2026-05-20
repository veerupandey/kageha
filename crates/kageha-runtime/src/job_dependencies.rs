//! Classify durable job dependencies without touching the journal.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Deserialize)]
pub struct DependencyRequest {
    version: u32,
    jobs: BTreeMap<String, Vec<String>>,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum DependencyStatus {
    Blocked,
    Ready,
    Waiting,
}

#[derive(Serialize)]
pub struct DependencyReply {
    version: u32,
    decisions: BTreeMap<String, DependencyStatus>,
}

pub fn plan_dependencies(request: DependencyRequest) -> Result<DependencyReply, &'static str> {
    if request.version != 1 {
        return Err("unsupported job dependency version");
    }
    let decisions = request
        .jobs
        .into_iter()
        .map(|(id, states)| {
            let status = if states
                .iter()
                .any(|state| matches!(state.as_str(), "failed" | "cancelled" | "reconciling"))
            {
                DependencyStatus::Blocked
            } else if !states.is_empty() && states.iter().all(|state| state == "succeeded") {
                DependencyStatus::Ready
            } else {
                DependencyStatus::Waiting
            };
            (id, status)
        })
        .collect();
    Ok(DependencyReply {
        version: 1,
        decisions,
    })
}
