//! Decide whether a failed model attempt may retry within the route budget.
//!
//! This is only the `kageha-loop decide-model-retry` command for the Python host's
//! own model client. It does not retry anything itself, and Rust model calls do not
//! use it: their one retry loop is `kageha_models::HttpModel::complete`
//! (`kageha_models::retry`).
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct RetryRequest {
    version: u32,
    retryable: bool,
    failure_class: String,
    attempt: usize,
    total_attempts: usize,
    max_attempts: usize,
    delay_s: f64,
    remaining_s: f64,
}

#[derive(Serialize)]
pub struct RetryReply {
    version: u32,
    retry_same_model: bool,
}

pub fn decide_model_retry(request: RetryRequest) -> Result<RetryReply, &'static str> {
    if request.version != 1 {
        return Err("unsupported model retry version");
    }
    Ok(RetryReply {
        version: 1,
        retry_same_model: request.retryable
            && request.failure_class != "timeout"
            && request.attempt < 2
            && request.total_attempts == 1
            && request.total_attempts < request.max_attempts
            && request.delay_s <= request.remaining_s,
    })
}
