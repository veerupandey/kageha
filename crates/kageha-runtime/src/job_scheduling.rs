//! Select a durable worker from a snapshot ordered by queue fairness.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Deserialize)]
pub struct ClaimRequest {
    version: u32,
    agent_limit: i64,
    active: Vec<Job>,
    candidates: Vec<Job>,
}

#[derive(Deserialize)]
struct Job {
    id: String,
    kind: String,
    provider: Value,
    session_id: String,
    parent_turn: String,
    max_parallel: i64,
}

#[derive(Serialize)]
pub struct ClaimReply {
    version: u32,
    job_id: Option<String>,
}

pub fn select_job(request: ClaimRequest) -> Result<ClaimReply, &'static str> {
    if request.version != 1 {
        return Err("unsupported job scheduling version");
    }
    let selected = request.candidates.into_iter().find(|job| {
        let capacity = match job.kind.as_str() {
            "agent" => request.agent_limit.clamp(0, 8),
            "tts" => 2,
            _ => 1,
        };
        let pool_size = request
            .active
            .iter()
            .filter(|active| {
                active.kind == job.kind && (job.kind != "tts" || active.provider == job.provider)
            })
            .count() as i64;
        if pool_size >= capacity {
            return false;
        }
        if job.kind == "agent"
            && request
                .active
                .iter()
                .any(|active| active.kind == "agent" && active.session_id == job.session_id)
        {
            return false;
        }
        let parent_agents = request
            .active
            .iter()
            .filter(|active| active.kind == "agent" && active.parent_turn == job.parent_turn)
            .count() as i64;
        parent_agents < job.max_parallel
    });
    Ok(ClaimReply {
        version: 1,
        job_id: selected.map(|job| job.id),
    })
}
