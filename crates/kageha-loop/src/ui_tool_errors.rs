//! Tool error envelopes and Host-level retry of transient read failures
//! (Requirements 9.1, 9.3; design "Error Handling").
//!
//! Every tool failure that reaches the model is
//! `Tool failed: {"error_code","retryable","summary","excerpt"?}`. An adapter error that
//! already is a JSON object with a code (`error_code`, `error.code` or `detail.code`) is
//! passed through unchanged. Any other failure is wrapped by [`normalize`] as `tool_failed`
//! with `retryable: false` and the first [`EXCERPT_BYTES`] of the message as `excerpt`.
//!
//! Read tools (`CapabilitySpec` effect `Read`, see `ToolHost::retries_transient`) that fail
//! with a transient error (HTTP 429 or 5xx, a timeout, a reset or refused connection) are
//! retried by [`retry_read`] inside the Host, before the result returns to the runtime. The
//! waits are 1 s, 4 s and 16 s plus up to 25 % jitter, or the upstream's `retry_after` when
//! that is longer, capped at 60 s. Cancellation and the turn deadline are checked between
//! attempts. After 3 retries the Host returns `transient_upstream`.
//!
//! Interaction with the runtime's `RetryRead` recovery (kageha-runtime `execution.rs`): the
//! runtime replays a read once when its outcome is `Recoverable / RetryRead`.
//! `ui_tool_outcomes::classify` maps `transient_upstream` to `Recoverable / ChangeStrategy`,
//! so a read the Host already retried is never replayed again by the runtime. `RetryRead`
//! still applies to adapter-declared replay-safe reads the Host does not retry itself
//! (known reads whose `CapabilitySpec` effect is not `Read`, such as `browser_validate`).
use kageha_contracts::execution::Control;
use serde_json::{json, Value};
use std::io;
use std::time::{Duration, Instant};

/// Largest `excerpt` of the underlying error, stderr or response body, in bytes.
pub(crate) const EXCERPT_BYTES: usize = 2048;
/// Retries after the first attempt of a transient read failure.
pub(crate) const MAX_RETRIES: usize = 3;
/// Base wait before each retry.
pub(crate) const BACKOFF: [Duration; MAX_RETRIES] =
    [Duration::from_secs(1), Duration::from_secs(4), Duration::from_secs(16)];
/// Longest wait, also for an upstream `retry_after`.
pub(crate) const RETRY_AFTER_CAP: Duration = Duration::from_secs(60);
const SUMMARY_CHARS: usize = 300;

fn code(value: &Value) -> Option<&str> {
    value["error_code"].as_str().or_else(|| value["error"]["code"].as_str()).or_else(|| value["detail"]["code"].as_str())
}

/// Whether `text` already is a structured tool error: a JSON object with an error code.
pub(crate) fn is_structured(text: &str) -> bool {
    serde_json::from_str::<Value>(text).is_ok_and(|value| value.is_object() && code(&value).is_some())
}

/// The first [`EXCERPT_BYTES`] of `text`, cut at a character boundary.
pub(crate) fn excerpt(text: &str) -> &str {
    let mut end = text.len().min(EXCERPT_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Wraps a plain-text failure of `tool` in the standard envelope, keeping its kind.
/// Structured errors are returned unchanged.
pub(crate) fn normalize(tool: &str, error: io::Error) -> io::Error {
    let text = error.to_string();
    if is_structured(&text) {
        return error;
    }
    let first = text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("");
    let summary = if first.is_empty() {
        format!("{tool} failed without an error message.")
    } else {
        let mut line: String = first.chars().take(SUMMARY_CHARS).collect();
        if line.len() < first.len() {
            line.push('…');
        }
        format!("{tool} failed: {line}")
    };
    let mut envelope = json!({"error_code":"tool_failed","retryable":false,"summary":summary});
    if !text.trim().is_empty() {
        envelope["excerpt"] = json!(excerpt(&text));
    }
    io::Error::new(error.kind(), envelope.to_string())
}

/// Expose the exact failed-attempt reference in the result body as well as in
/// protocol metadata. Some providers do not make their generated call IDs visible
/// to the model; a browser RPC request_id is a different identity.
pub(crate) fn normalize_call(call: &kageha_contracts::execution::ToolCall, error: io::Error) -> io::Error {
    let error = normalize(&call.name, error);
    let mut value: Value = serde_json::from_str(&error.to_string()).expect("normalized tool error");
    value["tool_call_id"] = json!(call.id);
    if call.name.starts_with("browser_") {
        if value["request_id"].is_string() && value["receipt"].is_null() {
            value["request_id_kind"]=json!("rpc_request");
        }
        if call.name == "browser_close" && matches!(code(&value),Some("action_outcome_uncertain"|"close_outcome_unknown")) {
            value["recovery_hint"]=json!("List browser_tabs and check the exact target. Never automatically repeat this close or open a replacement. A close RPC request_id is not an input receipt.");
        } else if call.name == "browser_navigate" && code(&value)==Some("action_outcome_uncertain") {
            value["recovery_hint"]=json!("Navigation may have committed. Select the original tab using browser_tabs and take a fresh browser_snapshot to inspect its URL and state. Do not automatically repeat navigation. The RPC request_id is not an input receipt.");
        } else if matches!(code(&value),Some("action_receipt_pending"|"action_outcome_uncertain"|"script_upload_unconfirmed")) {
            value["recovery_hint"]=json!("Input may already have been sent. Do not repeat the action. Use upload_receipt_ids when present; otherwise call browser_receipts without request_id to list the original pending actions for this tab. The error's RPC request_id is not necessarily an action receipt. Use tool_call_id for report_blocker.");
        }
    }
    io::Error::new(error.kind(), value.to_string())
}

/// A transient failure and the wait the upstream asked for, if any.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Transient {
    pub retry_after: Option<Duration>,
}

fn transient_status(status: u64) -> bool {
    status == 429 || (500..=599).contains(&status)
}

const TRANSIENT_CODES: &[&str] = &[
    "network_error", "transport_error", "temporarily_unavailable", "rate_limited", "timeout", "upstream_timeout",
];
const TRANSIENT_PHRASES: &[&str] = &[
    "too many requests", "rate limit", "rate-limit", "timed out", "deadline exceeded", "request timeout",
    "gateway timeout", "read timeout", "connect timeout", "connection reset", "connection refused",
    "connection aborted", "broken pipe", "service unavailable", "bad gateway", "temporarily unavailable",
    "integration transport failed",
];

fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric()).filter(|token| !token.is_empty()).map(str::to_ascii_lowercase).collect()
}

/// HTTP statuses named in free text, such as `HTTP 503`, `HTTP/1.1 429` or `status 502`.
fn text_statuses(tokens: &[String]) -> impl Iterator<Item = u64> + '_ {
    tokens.iter().enumerate().filter_map(|(index, token)| {
        let status = token.parse::<u64>().ok().filter(|_| token.len() == 3)?;
        tokens[index.saturating_sub(3)..index].iter().any(|before| before == "http" || before == "status").then_some(status)
    })
}

fn text_retry_after(tokens: &[String]) -> Option<Duration> {
    tokens.windows(3).find(|window| window[0] == "retry" && window[1] == "after")
        .and_then(|window| window[2].parse::<u64>().ok()).map(Duration::from_secs)
}

fn text_transient(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    TRANSIENT_PHRASES.iter().any(|phrase| lower.contains(phrase)) || text_statuses(&tokens(text)).any(transient_status)
}

fn seconds(value: &Value) -> Option<Duration> {
    let secs = |v: &Value| v.as_f64().filter(|s| s.is_finite() && *s >= 0.0).map(|s| Duration::from_secs_f64(s.min(3600.0)));
    let millis = |v: &Value| v.as_u64().map(|ms| Duration::from_millis(ms.min(3_600_000)));
    secs(&value["retry_after"]).or_else(|| millis(&value["retry_after_ms"]))
}

/// Classifies a tool failure as transient. Structured errors are transient when they name
/// a 429 or 5xx status, carry a transient code, or say `retryable: true` and describe a
/// transient cause. An explicit `retryable: false` is never transient. Plain-text errors
/// are transient when their kind or text names a timeout, a dropped connection, a rate
/// limit or a 5xx status.
pub(crate) fn transient(error: &io::Error) -> Option<Transient> {
    use io::ErrorKind::*;
    if matches!(error.kind(), ConnectionReset | ConnectionRefused | ConnectionAborted | BrokenPipe | TimedOut) {
        return Some(Transient { retry_after: None });
    }
    let text = error.to_string();
    match serde_json::from_str::<Value>(&text) {
        Ok(value) if value.is_object() => {
            if value["retryable"] == false || value["detail"]["retryable"] == false {
                return None;
            }
            let retry_after = seconds(&value).or_else(|| seconds(&value["detail"]));
            let status = [&value["http_status"], &value["status"], &value["detail"]["http_status"], &value["detail"]["status"]]
                .into_iter().find_map(Value::as_u64);
            let described = || {
                [&value["summary"], &value["message"], &value["detail"]["message"], &value["error"]["message"]]
                    .into_iter().filter_map(Value::as_str).any(text_transient)
            };
            (status.is_some_and(transient_status)
                || code(&value).is_some_and(|code| TRANSIENT_CODES.contains(&code))
                || (value["retryable"] == true && described()))
                .then_some(Transient { retry_after })
        }
        _ => text_transient(&text).then(|| Transient { retry_after: text_retry_after(&tokens(&text)) }),
    }
}

/// The wait before retry `attempt` (0-based): the base backoff plus `jitter`, or the
/// upstream's `retry_after` when that is longer, capped at [`RETRY_AFTER_CAP`].
pub(crate) fn delay(attempt: usize, retry_after: Option<Duration>, jitter: impl FnOnce(Duration) -> Duration) -> Duration {
    let base = BACKOFF[attempt.min(MAX_RETRIES - 1)];
    let backoff = base + jitter(base / 4).min(base / 4);
    backoff.max(retry_after.unwrap_or_default()).min(RETRY_AFTER_CAP)
}

/// A uniformly random duration in `[0, max]`, at millisecond resolution.
pub(crate) fn random_jitter(max: Duration) -> Duration {
    let range = max.as_millis() as u64 + 1;
    Duration::from_millis((uuid::Uuid::new_v4().as_u128() % u128::from(range)) as u64)
}

/// Sleeps for `delay` in short slices, returning early with the control's error when the
/// turn is cancelled or its deadline passes.
pub(crate) fn sleep(delay: Duration, control: &Control<'_>) -> io::Result<()> {
    let end = Instant::now() + delay;
    loop {
        control.check()?;
        let now = Instant::now();
        if now >= end {
            return Ok(());
        }
        std::thread::sleep((end - now).min(Duration::from_millis(100)));
    }
}

fn exhausted(error: &io::Error, attempts: usize) -> io::Error {
    io::Error::other(json!({
        "error_code":"transient_upstream","retryable":true,"attempts":attempts,
        "summary":format!("The read failed {attempts} times with a temporary upstream error (rate limit, server error or timeout). Try another source, or wait and try again later."),
        "excerpt":excerpt(&error.to_string()),
    }).to_string())
}

/// Runs a read, retrying transient failures up to [`MAX_RETRIES`] times. `pause` waits
/// between attempts (tests inject one that does not sleep). A cancelled or expired turn,
/// a policy stop, an uncertain outcome or an exhausted request budget is returned at once.
/// A non-transient failure is returned as is. Exhausted retries return `transient_upstream`.
pub(crate) fn retry_read(
    control: &Control<'_>,
    pause: &dyn Fn(Duration, &Control<'_>) -> io::Result<()>,
    jitter: &dyn Fn(Duration) -> Duration,
    mut attempt: impl FnMut() -> io::Result<String>,
) -> io::Result<String> {
    let mut retries = 0;
    loop {
        let error = match attempt() {
            Ok(content) => return Ok(content),
            Err(error) => error,
        };
        // A cancelled or expired turn is not an upstream failure.
        control.check()?;
        if error.kind() == io::ErrorKind::Interrupted
            || kageha_contracts::policy::is_stop(&error)
            || kageha_contracts::execution::is_uncertain(&error)
            || kageha_contracts::request_budget::is_exceeded(&error)
        {
            return Err(error);
        }
        let Some(transient) = transient(&error) else { return Err(error) };
        if retries == MAX_RETRIES {
            return Err(exhausted(&error, retries + 1));
        }
        pause(delay(retries, transient.retry_after, jitter), control)?;
        retries += 1;
        control.check()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn body(error: &io::Error) -> Value {
        serde_json::from_str(&error.to_string()).unwrap()
    }

    #[test]
    fn an_uncertain_upload_exposes_the_original_receipt_without_suggesting_a_retry() {
        let call=kageha_contracts::execution::ToolCall{id:"call-upload".into(),name:"browser_upload".into(),arguments:json!({})};
        let error=normalize_call(&call,io::Error::other(json!({"error_code":"script_upload_unconfirmed",
            "input_dispatched":true,"upload_receipt_ids":["original:upload:0"],"request_id":"rpc"}).to_string()));
        let value=body(&error);
        assert_eq!(value["upload_receipt_ids"][0],"original:upload:0");
        assert_eq!(value["tool_call_id"],"call-upload");
        assert_eq!(value["request_id_kind"],"rpc_request");
        assert!(value["recovery_hint"].as_str().unwrap().contains("Do not repeat"));
    }
    #[test]
    fn failed_attempt_reference_keeps_tool_and_adapter_identities_distinct() {
        let call = kageha_contracts::execution::ToolCall{id:"call-original".into(),name:"browser_open".into(),arguments:json!({})};
        let error=normalize_call(&call,io::Error::other(json!({"error_code":"browser_binding_changed","request_id":"rpc-only"}).to_string()));
        assert_eq!(body(&error)["tool_call_id"],"call-original");
        assert_eq!(body(&error)["request_id"],"rpc-only");
        let error=normalize_call(&call,io::Error::other("Observed control changed"));
        assert_eq!(body(&error)["tool_call_id"],"call-original");
    }
    #[test]
    fn plain_failures_become_the_envelope_with_an_excerpt_and_keep_their_kind() {
        let long = format!("permission check failed\n{}", "é".repeat(3000));
        let error = normalize("web_fetch", io::Error::new(io::ErrorKind::PermissionDenied, long.clone()));
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        let value = body(&error);
        assert_eq!((value["error_code"].as_str(), value["retryable"].as_bool()), (Some("tool_failed"), Some(false)));
        assert_eq!(value["summary"], "web_fetch failed: permission check failed");
        let excerpt = value["excerpt"].as_str().unwrap();
        assert!(excerpt.len() <= EXCERPT_BYTES && excerpt.len() > EXCERPT_BYTES - 4 && long.starts_with(excerpt));
        // A JSON body without a code is not structured, so it is wrapped too.
        let value = body(&normalize("mcp_call", io::Error::other(r#"{"error":"boom"}"#)));
        assert_eq!((value["error_code"].as_str(), value["excerpt"].as_str()), (Some("tool_failed"), Some(r#"{"error":"boom"}"#)));
        assert!(body(&normalize("x", io::Error::other(""))).get("excerpt").is_none());
    }

    #[test]
    fn structured_errors_are_not_wrapped_again() {
        for text in [
            json!({"error_code":"edit_not_found","retryable":false,"summary":"no"}).to_string(),
            json!({"error":{"code":"invalid_params"}}).to_string(),
            json!({"error_code":"browser_request_failed","detail":{"code":"stale"}}).to_string(),
        ] {
            assert_eq!(normalize("t", io::Error::other(text.clone())).to_string(), text);
        }
        let once = normalize("t", io::Error::other("plain"));
        let twice = normalize("t", io::Error::other(once.to_string()));
        assert_eq!(once.to_string(), twice.to_string());
    }

    #[test]
    fn transient_failures_are_classified_from_status_kind_and_text() {
        let yes = |error: io::Error| assert!(transient(&error).is_some(), "{error}");
        let no = |error: io::Error| assert!(transient(&error).is_none(), "{error}");
        yes(io::Error::other("Provider returned HTTP 503"));
        yes(io::Error::other("tavily search returned HTTP 429"));
        yes(io::Error::other("Integration transport failed; outcome may be unknown"));
        yes(io::Error::new(io::ErrorKind::TimedOut, "Integration deadline exceeded"));
        yes(io::Error::new(io::ErrorKind::ConnectionReset, "reset"));
        yes(io::Error::other(json!({"error_code":"x","http_status":502}).to_string()));
        yes(io::Error::other(json!({"error_code":"network_error","retryable":true}).to_string()));
        yes(io::Error::other(json!({"error_code":"web_search_unavailable","retryable":true,"summary":"Web search failed: ddg search returned HTTP 500."}).to_string()));
        no(io::Error::other("Provider returned HTTP 404"));
        no(io::Error::other("Integration read failed: Resource is outside this integration's grant"));
        no(io::Error::other(json!({"error_code":"research_source_unavailable","http_status":503,"retryable":false}).to_string()));
        no(io::Error::other(json!({"error_code":"web_search_unavailable","retryable":true,"summary":"Web search failed: tavily search has no API key."}).to_string()));
        no(io::Error::other(json!({"error_code":"command_timeout","retryable":false,"summary":"timed out"}).to_string()));
        assert_eq!(transient(&io::Error::other(json!({"error_code":"rate_limited","retry_after":7}).to_string())),
            Some(Transient { retry_after: Some(Duration::from_secs(7)) }));
        assert_eq!(transient(&io::Error::other("HTTP 429 Too Many Requests; Retry-After: 30")),
            Some(Transient { retry_after: Some(Duration::from_secs(30)) }));
    }

    #[test]
    fn delays_back_off_with_bounded_jitter_and_honor_capped_retry_after() {
        assert_eq!((0..3).map(|i| delay(i, None, |_| Duration::ZERO)).collect::<Vec<_>>(), BACKOFF);
        assert_eq!(delay(1, None, |max| max), Duration::from_secs(5));
        assert_eq!(delay(0, None, |_| Duration::from_secs(99)), Duration::from_millis(1250), "jitter is bounded");
        assert_eq!(delay(0, Some(Duration::from_secs(30)), |_| Duration::ZERO), Duration::from_secs(30));
        assert_eq!(delay(2, Some(Duration::from_secs(1)), |_| Duration::ZERO), Duration::from_secs(16));
        assert_eq!(delay(0, Some(Duration::from_secs(600)), |_| Duration::ZERO), RETRY_AFTER_CAP);
        for _ in 0..50 {
            assert!(random_jitter(Duration::from_millis(250)) <= Duration::from_millis(250));
        }
    }

    fn run(results: Vec<io::Result<String>>, cancel_on_pause: bool) -> (io::Result<String>, usize, Vec<Duration>) {
        let flag = AtomicBool::new(false);
        let control = Control { cancelled: &flag, deadline: None };
        let results = RefCell::new(results.into_iter());
        let calls = RefCell::new(0);
        let pauses = RefCell::new(Vec::new());
        let result = retry_read(&control, &|delay, _| {
            pauses.borrow_mut().push(delay);
            if cancel_on_pause { flag.store(true, Ordering::Release); }
            Ok(())
        }, &|_| Duration::ZERO, || { *calls.borrow_mut() += 1; results.borrow_mut().next().unwrap() });
        (result, calls.into_inner(), pauses.into_inner())
    }

    #[test]
    fn retries_transient_failures_until_success() {
        let (result, calls, pauses) = run(vec![
            Err(io::Error::other("Provider returned HTTP 503")), Err(io::Error::other("Provider returned HTTP 503")), Ok("data".into()),
        ], false);
        assert_eq!((result.unwrap(), calls), ("data".to_owned(), 3));
        assert_eq!(pauses, BACKOFF[..2]);
    }

    #[test]
    fn exhausted_retries_return_transient_upstream_with_attempts_and_excerpt() {
        let (result, calls, pauses) = run((0..4).map(|_| Err(io::Error::other("brave search returned HTTP 429"))).collect(), false);
        let value = body(&result.unwrap_err());
        assert_eq!((calls, pauses), (4, BACKOFF.to_vec()));
        assert_eq!((value["error_code"].as_str(), value["attempts"].as_u64()), (Some("transient_upstream"), Some(4)));
        assert_eq!(value["excerpt"], "brave search returned HTTP 429");
    }

    #[test]
    fn non_transient_failures_and_cancellation_stop_retrying() {
        let (result, calls, _) = run(vec![Err(io::Error::other("HTTP 503")), Err(io::Error::other("HTTP 404"))], false);
        assert_eq!((result.unwrap_err().to_string(), calls), ("HTTP 404".to_owned(), 2));
        let (result, calls, pauses) = run(vec![Err(io::Error::other("HTTP 503")), Ok("late".into())], true);
        assert_eq!((result.unwrap_err().kind(), calls, pauses.len()), (io::ErrorKind::Interrupted, 1, 1));
    }

    #[test]
    fn sleep_returns_when_the_turn_is_cancelled() {
        let flag = AtomicBool::new(true);
        let started = Instant::now();
        let error = sleep(Duration::from_secs(30), &Control { cancelled: &flag, deadline: None }).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(sleep(Duration::ZERO, &Control { cancelled: &AtomicBool::new(false), deadline: None }).is_ok());
    }
}

#[cfg(test)]
mod lifecycle_recovery_tests {
    use super::*;
    #[test]
    fn uncertain_lifecycle_actions_do_not_invent_input_receipts() {
        for tool in ["browser_close", "browser_navigate"] {
            let call=kageha_contracts::execution::ToolCall{id:"call".into(),name:tool.into(),arguments:json!({})};
            let normalized=normalize_call(&call,io::Error::other(json!({"error_code":"action_outcome_uncertain","request_id":"rpc","retryable":false}).to_string()));
            let value:Value=serde_json::from_str(&normalized.to_string()).unwrap();
            let hint=value["recovery_hint"].as_str().unwrap();
            assert!(hint.contains("browser_tabs") && !hint.contains("browser_receipts"));
            assert_eq!(value["request_id_kind"],"rpc_request");
            let pending=normalize_call(&call,io::Error::other(json!({"error_code":"action_receipt_pending"}).to_string()));
            let value:Value=serde_json::from_str(&pending.to_string()).unwrap();
            assert!(value["recovery_hint"].as_str().unwrap().contains("browser_receipts"));
        }
    }
}
