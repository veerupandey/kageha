//! Retry and fallback policy for one model call (design section 10, Requirements 10.3
//! and 14.5).
//!
//! `HttpModel::complete` holds the only retry loop for provider calls. It walks the
//! providers of one role's chain in order. Each provider gets 1 attempt plus up to
//! [`MAX_RETRIES`] retries for rate limits, 5xx replies, attempt timeouts and connection
//! errors, then the loop moves to the next provider and reports
//! `ModelActivity::Fallback`. Hosts only build the chain (kageha-loop `model_router`).
//! The runtime handles what this loop surfaces without retrying: interrupted streams,
//! output and context limits, and anything with a partially streamed tool call
//! (`kageha_runtime::execution`).
use kageha_contracts::execution::{model_failure, Control, ModelFailureKind};
use std::{
    io,
    sync::Arc,
    time::{Duration, Instant},
};

/// Waits before retries 1, 2 and 3 of the same model, before jitter.
pub const BACKOFF: [Duration; 3] = [Duration::from_secs(1), Duration::from_secs(4), Duration::from_secs(16)];
/// Retries of the same model after its first attempt.
pub const MAX_RETRIES: usize = BACKOFF.len();
/// Longest wait between attempts, also for a provider's `Retry-After`.
pub const RETRY_AFTER_CAP: Duration = Duration::from_secs(60);

/// Waits between attempts. It must return early with the control's error when the call
/// is cancelled. Tests inject one that records the delay without sleeping.
pub type ModelWait = Arc<dyn Fn(Duration, &Control<'_>) -> io::Result<()> + Send + Sync>;

/// What the loop does after a failed attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Next {
    /// Retry the same model after a backoff, then fall back.
    Retry,
    /// Move to the next model of the chain now.
    Fallback,
    /// Return the error to the caller: the runtime owns this recovery.
    Surface,
}

/// The next step and the `model_fallback` cause for a failed attempt. The caller has
/// already returned on cancellation and on the call's own deadline.
pub(crate) fn classify(error: &io::Error) -> (Next, &'static str) {
    if kageha_contracts::request_budget::is_exceeded(error) {
        return (Next::Surface, "budget");
    }
    let failure = model_failure(error);
    // A partially streamed tool call is never retried or replayed on another model.
    if failure.is_some_and(|failure| failure.partial_tool_calls) {
        return (Next::Surface, "partial_tool_call");
    }
    match failure.map(|failure| failure.kind) {
        Some(ModelFailureKind::ContextLimit) => return (Next::Surface, "context_limit"),
        Some(ModelFailureKind::OutputLimit) => return (Next::Surface, "output_limit"),
        Some(ModelFailureKind::InterruptedStream) => return (Next::Surface, "interrupted_stream"),
        _ => {}
    }
    if error.kind() == io::ErrorKind::Interrupted {
        return (Next::Surface, "cancelled");
    }
    let status = failure.and_then(|failure| failure.http_status);
    if error.kind() == io::ErrorKind::PermissionDenied
        || matches!(status, Some(401 | 403))
        || failure.is_some_and(|failure| failure.kind == ModelFailureKind::Authentication)
    {
        return (Next::Fallback, "auth");
    }
    match status {
        Some(429) => return (Next::Retry, "rate_limited"),
        Some(500..=599) => return (Next::Retry, "server_error"),
        Some(400 | 404 | 405 | 422) => return (Next::Fallback, "unavailable"),
        _ => {}
    }
    if error.kind() == io::ErrorKind::TimedOut {
        return (Next::Retry, "timeout");
    }
    match failure.map(|failure| failure.kind) {
        Some(ModelFailureKind::RateLimited) => (Next::Retry, "rate_limited"),
        Some(ModelFailureKind::Transport) if status.is_none() => (Next::Retry, "connection"),
        _ => (Next::Fallback, "invalid_response"),
    }
}

/// True when a failed provider should not be tried again for the rest of this model's life.
pub(crate) fn permanent(cause: &str) -> bool {
    matches!(cause, "auth" | "unavailable")
}

/// The `model_failure` category shown for a failed attempt.
pub(crate) fn category(cause: &'static str) -> &'static str {
    match cause {
        "auth" => "authentication",
        "unavailable" => "configuration",
        "rate_limited" => "rate_limit",
        "server_error" | "invalid_response" => "provider_unavailable",
        "connection" => "transport",
        other => other,
    }
}

/// The wait before retry `retry` (0-based): the base backoff plus `jitter` (at most a
/// quarter of the base), or the provider's `Retry-After` when that is longer, capped at
/// [`RETRY_AFTER_CAP`].
pub fn delay(retry: usize, retry_after: Option<Duration>, jitter: Duration) -> Duration {
    let base = BACKOFF[retry.min(MAX_RETRIES - 1)];
    let backoff = base + jitter.min(base / 4);
    backoff.max(retry_after.unwrap_or_default()).min(RETRY_AFTER_CAP)
}

/// A random duration in `[0, max]`, at millisecond resolution.
pub(crate) fn jitter(max: Duration) -> Duration {
    use std::hash::{BuildHasher, Hasher};
    let random = std::collections::hash_map::RandomState::new().build_hasher().finish();
    Duration::from_millis(random % (max.as_millis() as u64 + 1))
}

/// Sleeps for `delay` in short slices and stops with the control's error on cancellation.
pub fn wait_for_model(delay: Duration, control: &Control<'_>) -> io::Result<()> {
    let end = Instant::now() + delay;
    while Instant::now() < end {
        control.check()?;
        std::thread::sleep(end.saturating_duration_since(Instant::now()).min(Duration::from_millis(50)));
    }
    control.check()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kageha_contracts::execution::ModelFailure;

    fn status(code: u16) -> io::Error {
        io::Error::other(ModelFailure { http_status: Some(code), ..ModelFailure::new(ModelFailureKind::Transport, "x") })
    }

    #[test]
    fn transient_failures_retry_and_others_fall_back_or_surface() {
        assert_eq!(classify(&status(429)), (Next::Retry, "rate_limited"));
        assert_eq!(classify(&status(503)), (Next::Retry, "server_error"));
        assert_eq!(classify(&io::Error::other(ModelFailure::new(ModelFailureKind::Transport, "x"))), (Next::Retry, "connection"));
        assert_eq!(classify(&io::Error::new(io::ErrorKind::TimedOut, "attempt")), (Next::Retry, "timeout"));
        assert_eq!(classify(&status(401)), (Next::Fallback, "auth"));
        assert_eq!(classify(&io::Error::new(io::ErrorKind::PermissionDenied, "lease")), (Next::Fallback, "auth"));
        assert_eq!(classify(&status(404)), (Next::Fallback, "unavailable"));
        assert_eq!(classify(&io::Error::other("bad JSON")), (Next::Fallback, "invalid_response"));
        let mut partial = ModelFailure { http_status: Some(503), ..ModelFailure::new(ModelFailureKind::Transport, "x") };
        partial.partial_tool_calls = true;
        assert_eq!(classify(&io::Error::new(io::ErrorKind::TimedOut, partial)).0, Next::Surface);
        assert_eq!(classify(&io::Error::other(ModelFailure::new(ModelFailureKind::ContextLimit, "x"))), (Next::Surface, "context_limit"));
        assert_eq!(classify(&io::Error::new(io::ErrorKind::Interrupted, "cancelled")).0, Next::Surface);
        assert!(permanent("auth") && permanent("unavailable") && !permanent("rate_limited"));
    }

    #[test]
    fn delays_follow_the_schedule_with_bounded_jitter_and_capped_retry_after() {
        let none = Duration::ZERO;
        let schedule: Vec<_> = (0..3).map(|retry| delay(retry, None, none).as_secs()).collect();
        assert_eq!(schedule, [1, 4, 16]);
        assert_eq!(delay(0, None, Duration::from_secs(9)), Duration::from_millis(1250));
        assert_eq!(delay(2, Some(Duration::from_secs(30)), none), Duration::from_secs(30));
        assert_eq!(delay(0, Some(Duration::from_secs(600)), none), RETRY_AFTER_CAP);
        for _ in 0..100 {
            assert!(jitter(Duration::from_millis(250)) <= Duration::from_millis(250));
        }
        let cancelled = std::sync::atomic::AtomicBool::new(true);
        let error = wait_for_model(Duration::from_secs(60), &Control { cancelled: &cancelled, deadline: None }).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    }
}
