//! Run_Journal entries from the events the Host already emits (spec autonomous-agent-loop,
//! task 21.2; design section 15; Requirements 2.7, 9.5, 10.6 and 15.1).
//!
//! The Host opens one [`SqliteLearning`] store per process ([`install_default`]: the
//! `learning.sqlite` under `KAGEHA_HOME`, else `~/.kageha`) and keeps it in its database
//! state (`agent_host::DatabaseState::learning`). Until a store is installed, journaling
//! is off; tests install one in a temporary directory with [`install`].
//!
//! Sources:
//! - Runtime frames, through [`observe`] (the `observe` callback of `webui::execute`):
//!   `tools` registers each call's tool and main argument, and `tool_results` writes a
//!   `tool` entry per result (status, call id, result excerpt, cached flag, effect).
//!   Browser and connector result bodies are omitted: private content cannot be
//!   reliably identified by pattern scrubbing. A
//!   failed result (an error, or a `shell`/`remote_exec` with a non-zero exit code) is
//!   also a `failure` entry. A later success of the same operation in the same request
//!   is a `repair` entry linking both call ids and the calls in between. The operation
//!   is the tool plus its main argument: the program for shell commands, the path, the
//!   URL, the integration and operation, or a browser target bound to an observation.
//!   Unbound browser input is not treated as a retry. `model` frames are counted; a closed
//!   request records the larger of that count and its request budget's model calls.
//! - Host events, through [`entry`] and [`offer`] (`webui::emit`, and the outcome events
//!   written in the outcome-contract transactions, offered after commit): `shell_command` → `shell_command`; the Host's
//!   `completion_check` (from the Completion_Gate) → `gate`; `escalation` →
//!   `escalation`; `recovery` (model, context and tool retries, `invalid_tool_call`,
//!   `interrupted_stream`, restart resolution, component restarts) and `model_fallback`
//!   → `recovery`; `outcome_rejected`, `outcome_amended` and `outcome_blocked` →
//!   `outcome`. Tool cards are not read: the `tool_results` frame they come from is.
//!   The gate decision's event is the runtime's `completion_check` frame without its
//!   `kind` (`webui::record_gate_decision`); a copy that keeps `kind` is not journaled.
//!   The bare `component_restarted` event is followed by its `recovery` event, so it is
//!   not journaled twice.
//!
//! Entries are keyed by the request id ([`request_key`]): the request budget id from the
//! turn's pinned configuration (`ui_task_completion::scope`), else a delegated worker's
//! `parent_request_id`, else the turn id. Delegated workers inherit the parent's budget id,
//! and `ui_jobs` also pins the parent's request id as `parent_request_id` (needed when
//! request persistence is off and there is no budget id), so their entries join the
//! parent's request; only a top-level turn closes its request ([`finish_turn`]).
//!
//! Failure index (task 22.1; Requirements 9.2, 9.5 and 17.2): every failed result gets a
//! [`FailureSignature`] ([`failure_signature`]). [`known_fix`] runs in
//! `HostedTools::execute` on each failed result: it hands the signature to the worker and
//! looks up the store's best fix, which [`attach_known_fix`] adds to the result as
//! `known_fix: {steps, successes, last_seen}`. When the worker journals a `repair` with 1
//! to [`kageha_learning::MAX_FIX_STEPS`] calls in between, those calls' [`step`]s
//! (`shell: uv pip install pdfplumber`) are recorded as a fix of the failure's signature
//! (`SqliteLearning::record_repair`). When an operation fails again with the same signature,
//! every stored fix whose steps ran since its previous failure counts a failure
//! (`SqliteLearning::record_fix_failure`).
//!
//! Journaling adds no model calls and never fails a turn. The Host side only builds a
//! small message and offers it to a bounded channel; a full channel drops the entry and
//! counts it. One worker thread resolves request ids, scrubs token-like values from text
//! ([`scrub`]), replaces values held in the credential vault with `[redacted]` (task 23.2;
//! `ui_learning_secrets::VaultSecrets`), writes entries and logs store errors. An entry is
//! not written when the vault cannot be read, and a fix whose steps hold a vault value is
//! not learned. Closing a request goes through the
//! same channel, so the request's entries are written before its run row, and the turn
//! waits for that at most [`FLUSH_WAIT`].
use crate::webui::Database;
use kageha_learning::secrets::{find_vault_value, redact_json, redact_values};
use kageha_learning::{FailureSignature, JournalEntry, JournalKind, LearningPort, OutcomeStatus, RequestSummary, SecretSource, SqliteLearning};
use kageha_runtime::execution::ToolCall;
use serde_json::{json, Map, Value};
use std::{
    collections::{HashMap, VecDeque},
    io,
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc, OnceLock, Weak,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Messages waiting for the worker. More are dropped and counted.
const QUEUE: usize = 4096;
/// Largest text excerpt in an entry, after scrubbing.
pub(crate) const EXCERPT_BYTES: usize = 2048;
/// Bytes of a tool result sent to the worker. The excerpt and error code come from them.
const HEAD_BYTES: usize = 64 * 1024;
/// Requests whose call state the worker keeps; the oldest is forgotten first.
const MAX_REQUESTS: usize = 256;
/// Calls remembered per request for the repair heuristic.
const MAX_CALLS: usize = 4096;
/// Call ids listed between a failure and its repair.
const MAX_BETWEEN: usize = 8;
/// Items kept per array in an entry body.
const MAX_ITEMS: usize = 100;
/// How long closing a request waits for the worker.
pub(crate) const FLUSH_WAIT: Duration = Duration::from_secs(5);
const REDACTED: &str = "[redacted]";

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// The Host's journal: empty until a store is installed.
#[derive(Default)]
pub(crate) struct Journal {
    handle: OnceLock<Handle>,
    dropped: AtomicU64,
    lookup_errors: AtomicU64,
}

struct Handle {
    sender: mpsc::SyncSender<Message>,
    store: Arc<SqliteLearning>,
}

/// One registered call of a `tools` frame.
struct Call {
    id: String,
    name: String,
    operation: String,
    /// The call as a fix step ([`step`]).
    step: String,
}

/// One result of a `tool_results` frame.
struct ToolResult {
    call_id: String,
    is_error: bool,
    /// At most [`HEAD_BYTES`] of the content.
    head: String,
    complete: bool,
    cached: bool,
    outcome_code: Option<String>,
}

enum Message {
    Calls { turn: String, calls: Vec<Call> },
    Results { turn: String, results: Vec<ToolResult>, at: u64 },
    Entry { turn: String, kind: JournalKind, body: Value, at: u64 },
    Model { turn: String },
    /// The signature the Host computed from a failed result before it was spilled.
    Signature { turn: String, call_id: String, signature: FailureSignature },
    /// A `skill_read` (or `load_skill`) that returned the skill (task 24.2).
    SkillUsed { turn: String, skill: String, revision: u64, call_id: String, tool: String, at: u64 },
    Close { key: String, summary: RequestSummary },
    Flush(mpsc::SyncSender<()>),
}

impl Journal {
    fn offer(&self, message: Message) {
        let Some(handle) = self.handle.get() else { return };
        if let Err(mpsc::TrySendError::Full(_)) = handle.sender.try_send(message) {
            let dropped = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
            if dropped == 1 || dropped % 1000 == 0 {
                eprintln!("kageha: run journal queue full; {dropped} entries dropped");
            }
        }
    }
    fn enabled(&self) -> bool {
        self.handle.get().is_some()
    }
}

/// Installs the store at `$KAGEHA_HOME/learning.sqlite` (else `~/.kageha`). A store that
/// cannot be opened leaves journaling off and is logged.
pub(crate) fn install_default(db: &Database) {
    match kageha_learning::default_path() {
        Some(path) => {
            if let Err(error) = install(db, &path) {
                eprintln!("kageha: run journal disabled: {error}");
            }
        }
        None => eprintln!("kageha: run journal disabled: KAGEHA_HOME or HOME is required"),
    }
}

/// Opens the store at `path` and starts the worker, which redacts the credential vault's
/// values from entries. A second install is ignored.
pub(crate) fn install(db: &Database, path: &Path) -> io::Result<()> {
    install_with(db, path, Box::new(crate::ui_learning_secrets::VaultSecrets::new(db)))
}

/// [`install`] with the values to redact taken from `secrets`.
pub(crate) fn install_with(db: &Database, path: &Path, secrets: Box<dyn SecretSource + Send>) -> io::Result<()> {
    if db.learning.enabled() {
        return Ok(());
    }
    let store = Arc::new(SqliteLearning::open(path)?);
    let (sender, receiver) = mpsc::sync_channel(QUEUE);
    let mut worker = Worker::new(Arc::downgrade(db), store.clone(), secrets);
    std::thread::Builder::new()
        .name("kageha-run-journal".into())
        .spawn(move || {
            for message in receiver {
                worker.handle(message);
            }
        })?;
    // Another thread may have installed first; its worker stays, this one ends with its channel.
    let _ = db.learning.handle.set(Handle { sender, store });
    Ok(())
}

/// The installed store, or `None` while journaling is off. The learned context
/// (`ui_learned_context`) reads it at request start; tests read entries back.
pub(crate) fn store(db: &Database) -> Option<Arc<SqliteLearning>> {
    db.learning.handle.get().map(|handle| handle.store.clone())
}

/// Waits until the worker has handled every message offered before this call, at most
/// `wait`. Returns whether it did.
pub(crate) fn flush(db: &Database, wait: Duration) -> bool {
    let Some(handle) = db.learning.handle.get() else { return true };
    let (ack, done) = mpsc::sync_channel(1);
    let deadline = Instant::now() + wait;
    let mut message = Message::Flush(ack);
    loop {
        match handle.sender.try_send(message) {
            Ok(()) => break,
            Err(mpsc::TrySendError::Full(back)) if Instant::now() < deadline => {
                message = back;
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => return false,
        }
    }
    done.recv_timeout(deadline.saturating_duration_since(Instant::now())).is_ok()
}

/// Journals a runtime frame of `turn`: tool calls, their results, and model calls.
pub(crate) fn observe(db: &Database, turn: &str, frame: &Value) {
    if !db.learning.enabled() {
        return;
    }
    match frame["kind"].as_str() {
        Some("model") => db.learning.offer(Message::Model { turn: turn.into() }),
        Some("tools") => {
            let calls = frame["calls"].as_array().map(|calls| {
                calls.iter().map(|call| {
                    let name = call["name"].as_str().unwrap_or("").to_owned();
                    let id = call["id"].as_str().unwrap_or("");
                    let args = &call["arguments"];
                    let safe_ref = args["target"].as_str().and_then(|s| s.strip_prefix('e'))
                        .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()));
                    let operation = if browser_input(&name) &&
                        (!safe_ref || args["observation_id"].as_str().is_none_or(str::is_empty)) {
                        // No observed identity: never infer that a later input was
                        // a retry of this call, even if it used the same tool.
                        format!("{name} unbound-call:{id}")
                    } else if matches!(name.as_str(), "browser_credential" | "browser_open" | "browse_logged_in") || name.starts_with("mcp_") {
                        // The credential tool does not identify its site. MCP
                        // arguments can be arbitrary private data, so do not
                        // fingerprint or compare them to infer retries either.
                        format!("{name} unbound-call:{id}")
                    } else { operation(&name, args) };
                    Call { id: id.into(), operation,
                        step: step(&name, &call["arguments"]), name }
                }).collect()
            }).unwrap_or_default();
            db.learning.offer(Message::Calls { turn: turn.into(), calls });
        }
        Some("tool_results") => {
            let results = frame["results"].as_array().map(|results| {
                results.iter().map(|result| {
                    let content = result["content"].as_str().unwrap_or("");
                    ToolResult {
                        call_id: result["tool_call_id"].as_str().unwrap_or("").into(),
                        is_error: result["is_error"] == true,
                        head: head(content, HEAD_BYTES).into(),
                        complete: content.len() <= HEAD_BYTES,
                        cached: content.starts_with(kageha_runtime::recovery::CACHED_NOTE_PREFIX)
                            || (content.contains("\"cached\":true") && content.contains("\"cached_at\":")),
                        outcome_code: result["outcome"]["code"].as_str().filter(|code| !code.is_empty()).map(str::to_owned),
                    }
                }).collect()
            }).unwrap_or_default();
            db.learning.offer(Message::Results { turn: turn.into(), results, at: now_ms() });
        }
        _ => {}
    }
}

const FAILED: &str = "Tool failed: ";
/// Bytes of a failure message that are normalized into its signature.
const MESSAGE_BYTES: usize = 4096;
/// Largest fix step, after scrubbing.
const STEP_BYTES: usize = 300;

/// The last non-empty line of a string field.
fn last_line(value: &Value) -> Option<&str> {
    value.as_str()?.lines().map(str::trim).filter(|line| !line.is_empty()).next_back()
}

/// These tools can return message bodies, verification codes or page content.
/// Pattern scrubbing cannot reliably recognize short codes or personal data.
/// Keep only bounded error categories and receipt metadata in learning storage.
fn sensitive_result(tool: &str) -> bool {
    tool == "integration_read" || tool == "browse_logged_in"
        || tool.starts_with("browser_") || tool.starts_with("mcp_")
}

fn safe_category(value: &str) -> Option<&str> {
    (!value.is_empty() && value.len() <= 64 && value.bytes().any(|b| b.is_ascii_alphabetic())
        && value.bytes().all(|b| b.is_ascii_lowercase() || b == b'_' || b == b'-'))
        .then_some(value)
}

/// The [`FailureSignature`] of a failed tool result, or `None` when the result did not
/// fail. `content` is the result as `HostedTools::execute` returns it: `Tool failed: {...}`
/// for an error, the JSON result for `shell` and `remote_exec`.
/// - A command that ran and exited non-zero has the error code `exit_<code>` and, as its
///   message, the last non-empty line of stderr (else of stdout, else its summary).
/// - An error has the envelope's code (`error_code`, `error.code`, `detail.code` or
///   `code`, else `tool_failed`) and the first of `summary`, `message`, `error.message`,
///   `detail.message`, `error` or `excerpt`, else the text itself.
pub(crate) fn failure_signature(tool: &str, content: &str, is_error: bool) -> Option<FailureSignature> {
    let body = content.strip_prefix(FAILED).unwrap_or(content);
    let parsed = serde_json::from_str::<Value>(body).ok().filter(Value::is_object);
    let exit_code = matches!(tool, "shell" | "remote_exec")
        .then(|| parsed.as_ref().and_then(|value| value["exit_code"].as_i64()))
        .flatten();
    let (code, message) = match &parsed {
        Some(value) if !is_error => {
            let code = exit_code.filter(|code| *code != 0)?;
            let message = last_line(&value["stderr"]).or_else(|| last_line(&value["stdout"]))
                .or_else(|| value["summary"].as_str()).unwrap_or("");
            (format!("exit_{code}"), message)
        }
        Some(value) => {
            let code = [&value["error_code"], &value["error"]["code"], &value["detail"]["code"], &value["code"]]
                .into_iter()
                .find_map(|code| code.as_str().filter(|code| !code.is_empty() && code.len() <= 128))
                .unwrap_or("tool_failed");
            let message = [&value["summary"], &value["message"], &value["error"]["message"], &value["detail"]["message"],
                &value["error"], &value["excerpt"]]
                .into_iter()
                .find_map(|text| text.as_str().filter(|text| !text.trim().is_empty()))
                .unwrap_or(body);
            (code.to_owned(), message)
        }
        None if is_error => ("tool_failed".to_owned(), body),
        None => return None,
    };
    if sensitive_result(tool) {
        // Do not persist remote error text, even in the failure index: short
        // verification codes can occur in otherwise ordinary-looking messages.
        let code = safe_category(&code).unwrap_or("tool_failed");
        return Some(FailureSignature::from_failure(tool, code, "Sensitive tool failed"));
    }
    Some(FailureSignature::from_failure(tool, &code, head(message, MESSAGE_BYTES)))
}

/// A call as a step of a fix: the tool and its main argument, such as
/// `shell: uv pip install pdfplumber` or `write_file: outputs/a.md`, on one line,
/// scrubbed and at most [`STEP_BYTES`]. Shell steps keep the whole command.
pub(crate) fn step(name: &str, args: &Value) -> String {
    let main = match name {
        "shell" | "remote_exec" => args["command"].as_str().unwrap_or("").to_owned(),
        // Browser URLs can carry reset links or codes; future runs should learn
        // the method, not a previous user's destination or form values.
        name if name.starts_with("browser_") || name.starts_with("mcp_") || name == "browse_logged_in" => String::new(),
        _ => argument(name, args),
    };
    let main: String = main.split_whitespace().collect::<Vec<_>>().join(" ")
        .chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    if name.is_empty() {
        return String::new();
    }
    let text = if main.is_empty() { name.to_owned() } else { format!("{name}: {main}") };
    head(&scrub(&text), STEP_BYTES).trim_end().to_owned()
}

/// The learned fix for a failed result of `call`, as `{steps, successes, last_seen}`, or
/// `None`. Also hands the result's signature to the worker, so a later repair is recorded
/// under the signature of the full result even when the model sees a spilled excerpt.
/// One indexed lookup; a store error is logged and gives `None`.
fn outcome_evidence_failure(content: &str) -> bool {
    let parse = |text: &str| serde_json::from_str::<Value>(text.strip_prefix(FAILED).unwrap_or(text)).ok();
    let Some(value) = parse(content) else { return false };
    value["error_code"] == "invalid_outcome_evidence"
        || value["tool_result"].as_str().and_then(parse)
            .is_some_and(|inner| inner["error_code"] == "invalid_outcome_evidence")
}
pub(crate) fn known_fix(db: &Database, turn: &str, call: &ToolCall, content: &str, is_error: bool) -> Option<Value> {
    if outcome_evidence_failure(content) || call.name.starts_with("browser_") {return None;}
    let handle = db.learning.handle.get()?;
    let signature = failure_signature(&call.name, content, is_error)?;
    let fix = handle.store.known_fix(&signature);
    db.learning.offer(Message::Signature { turn: turn.into(), call_id: call.id.clone(), signature });
    match fix {
        Ok(fix) => fix.map(|fix| json!({"steps":fix.steps,"successes":fix.successes,"last_seen":fix.last_seen})),
        Err(error) => {
            let errors = db.learning.lookup_errors.fetch_add(1, Ordering::Relaxed) + 1;
            if errors <= 5 || errors % 1000 == 0 {
                eprintln!("kageha: known fix lookup failed ({errors} so far): {error}");
            }
            None
        }
    }
}

/// `content` with `"known_fix": fix` added as the last member of its JSON object (after
/// the `Tool failed: ` prefix of an error), leaving the rest of the text unchanged. Text
/// that is not a JSON object gets a `known_fix:` line. A result that already has a
/// `known_fix` is unchanged.
pub(crate) fn attach_known_fix(content: &str, fix: &Value) -> String {
    if outcome_evidence_failure(content) { return content.to_owned(); }
    let (prefix, body) = content.strip_prefix(FAILED).map_or(("", content), |body| (FAILED, body));
    match serde_json::from_str::<Value>(body) {
        Ok(Value::Object(object)) if object.get("error_code").is_some_and(|v|v=="invalid_outcome_evidence") => content.to_owned(),
        Ok(Value::Object(object)) if object.contains_key("known_fix") => content.to_owned(),
        Ok(Value::Object(object)) => {
            let open = body.trim_end().strip_suffix('}').unwrap_or(body);
            let separator = if object.is_empty() { "" } else { "," };
            format!("{prefix}{open}{separator}\"known_fix\":{fix}}}")
        }
        _ => format!("{content}\nknown_fix: {fix}"),
    }
}

/// Records that call `call_id` of `turn` read `skill` at registry `revision` with `tool`
/// (`skill_read` or `load_skill`): a `skill_used` journal entry, whose position orders the
/// read against the request's evidence, and an undecided `learn_skill_uses` row. The use
/// is decided when the request closes (`ui_skill_uses`).
pub(crate) fn skill_used(db: &Database, turn: &str, call_id: &str, skill: &str, revision: u64, tool: &str) {
    db.learning.offer(Message::SkillUsed { turn: turn.into(), skill: skill.into(), revision, call_id: call_id.into(),
        tool: tool.into(), at: now_ms() });
}

/// A Host event to journal once it is committed.
pub(crate) struct Pending {
    kind: JournalKind,
    body: Value,
}

/// Offers a Host entry of `kind` for `turn` that no event carries, such as the
/// `lesson_used` and `skill_used` entries of the learned context.
pub(crate) fn journal(db: &Database, turn: &str, kind: JournalKind, body: Value) {
    db.learning.offer(Message::Entry { turn: turn.into(), kind, body, at: now_ms() });
}

/// Offers an entry built by [`entry`] for `turn`, after its event is committed.
pub(crate) fn offer(db: &Database, turn: &str, pending: Pending) {
    db.learning.offer(Message::Entry { turn: turn.into(), kind: pending.kind, body: pending.body, at: now_ms() });
}

/// The journal entry of a Host event (see the module documentation for the kinds), if
/// journaling is on and the event is journaled.
pub(crate) fn entry(db: &Database, event: &str, payload: &Value) -> Option<Pending> {
    if event != "event" || !db.learning.enabled() {
        return None;
    }
    let detail = &payload["payload"];
    let kind = payload["kind"].as_str().unwrap_or("");
    let (kind, body) = match kind {
        "shell_command" => (JournalKind::ShellCommand, detail.clone()),
        // The runtime's frame copy carries its own `kind`; the Host's gate decision does not.
        "completion_check" if detail.get("kind").is_none() => (JournalKind::Gate, detail.clone()),
        "escalation" => (JournalKind::Escalation, without_kind(detail)),
        "recovery" => {
            let mut body = without_kind(detail);
            let cause = ["cause", "category", "phase"].iter().find_map(|key| detail[*key].as_str()).unwrap_or("recovery");
            body["cause"] = json!(cause);
            (JournalKind::Recovery, body)
        }
        "model_fallback" => (JournalKind::Recovery, json!({"cause":"model_fallback","detail":detail})),
        "outcome_rejected" | "outcome_amended" | "outcome_blocked" => (JournalKind::Outcome, json!({"event":kind,"detail":detail})),
        _ => return None,
    };
    Some(Pending { kind, body })
}

fn without_kind(value: &Value) -> Value {
    let mut value = if value.is_object() { value.clone() } else { json!({"detail":value}) };
    if let Some(object) = value.as_object_mut() {
        object.remove("kind");
    }
    value
}

/// The request status recorded for a finished turn: `cancelled`, `failed`, or the
/// runtime's task status (`verified`, `partial`, `blocked`, `budget_limited`,
/// `not_required`). A turn the Host marked unverified is never `verified` or
/// `not_required`; it is `partial`.
pub(crate) fn turn_status(cancelled: bool, native: &str, task: Option<&str>) -> &'static str {
    if cancelled || native == "cancelled" {
        return "cancelled";
    }
    if native == "failed" {
        return "failed";
    }
    let status = match task {
        Some("verified") => "verified",
        Some("partial") => "partial",
        Some("blocked") => "blocked",
        Some("budget_limited") => "budget_limited",
        Some("not_required") => "not_required",
        _ if native == "completed" => "not_required",
        _ => "blocked",
    };
    if native != "completed" && matches!(status, "verified" | "not_required") {
        "partial"
    } else {
        status
    }
}

/// The Run_Journal request id of `turn`: the request budget id from its pinned
/// configuration, else the `parent_request_id` a delegated worker was spawned with (the
/// parent's request id, also when request persistence is off and there is no budget to
/// inherit), else the turn id. So a worker's entries join its parent's request.
pub(crate) fn request_key(db: &Database, turn: &str) -> String {
    match crate::ui_persistence::configuration_for_turn(db, turn) {
        Ok(Some(configuration)) => configuration["request_budget_id"].as_str()
            .or_else(|| configuration["parent_request_id"].as_str())
            .filter(|key| !key.is_empty())
            .unwrap_or(turn)
            .to_owned(),
        _ => turn.to_owned(),
    }
}

/// Closes the request of a finished top-level turn with its status, model calls, cached
/// tokens, outcome statuses and task type. Delegated workers (`depth > 0`) never close:
/// their entries belong to the parent's request, which the parent's turn closes. Errors
/// are logged; the turn is not affected.
pub(crate) fn finish_turn(db: &Database, chat: &str, turn: &str, depth: u64, agent: &str, status: &str, started_ms: u64) {
    if depth > 0 || !db.learning.enabled() {
        return;
    }
    match summary(db, chat, turn, agent, status, started_ms) {
        Ok((key, summary)) => {
            db.learning.offer(Message::Close { key, summary });
            if !flush(db, FLUSH_WAIT) {
                eprintln!("kageha: run journal did not finish closing the request in time");
            }
        }
        Err(error) => eprintln!("kageha: run journal close failed: {error}"),
    }
}

fn summary(db: &Database, chat: &str, turn: &str, agent: &str, status: &str, started_ms: u64) -> io::Result<(String, RequestSummary)> {
    let key = crate::ui_task_completion::scope(db, turn)?;
    let (budget, task_type) = {
        let connection = db.lock().map_err(|_| io::Error::other("database unavailable"))?;
        let budget: Option<(i64, i64, i64)> = rusqlite::OptionalExtension::optional(connection.query_row(
            "SELECT used_model_calls,cached_tokens,started_at_ms FROM ui_request_budgets WHERE id=?",
            [&key],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ))
        .unwrap_or(None);
        let task_type = crate::ui_task_completion::load(&connection, chat, &key)?.task_type;
        (budget, task_type)
    };
    let outcomes = match crate::ui_progress::inputs(db, chat, turn, std::iter::empty()) {
        Ok(progress) => progress.outcome_statuses.into_iter().map(|(id, status)| OutcomeStatus { id, status }).collect(),
        Err(error) => {
            eprintln!("kageha: run journal outcome statuses unavailable: {error}");
            Vec::new()
        }
    };
    let ended = now_ms();
    let started = budget.map_or(started_ms, |(_, _, started)| (started.max(0) as u64).min(started_ms)).min(ended);
    let summary = RequestSummary {
        agent: if agent.is_empty() { "main".into() } else { agent.into() },
        chat_id: chat.into(),
        task_type: task_type.filter(|slug| kageha_learning::valid_task_type(slug)),
        status: status.into(),
        model_calls: budget.map_or(0, |(calls, _, _)| calls.max(0) as u64),
        // The Host does not track model cost yet.
        cost_usd: 0.0,
        cached_tokens: budget.map_or(0, |(_, cached, _)| cached.max(0) as u64),
        outcomes,
        started,
        ended,
    };
    Ok((key, summary))
}

/// Calls and failures seen in one request.
#[derive(Default)]
struct RequestState {
    /// Call id -> tool, operation and fix step.
    calls: HashMap<String, (String, String, String)>,
    /// Call id -> signature the Host computed for its failed result.
    signatures: HashMap<String, FailureSignature>,
    /// Result call ids in order, for the calls between a failure and its repair.
    order: Vec<String>,
    /// Operation -> latest unrepaired failure.
    failures: HashMap<String, Failed>,
    model_calls: u64,
}

struct Failed {
    call_id: String,
    error_code: String,
    /// Index in `order`.
    index: usize,
    signature: Option<FailureSignature>,
}

impl RequestState {
    /// Fix steps of the results after `from` and before `to` in `order`, at most
    /// [`MAX_BETWEEN`]. An unregistered call is an empty step.
    fn steps_between(&self, from: usize, to: usize) -> Vec<String> {
        self.order.iter().skip(from + 1).take(to.saturating_sub(from + 1).min(MAX_BETWEEN))
            .map(|id| self.calls.get(id).map(|(_, _, step)| step.clone()).unwrap_or_default())
            .collect()
    }
}

/// What a result teaches the failure index.
enum Learned {
    Repair { signature: FailureSignature, steps: Vec<String> },
    FixFailed { signature: FailureSignature, between: Vec<String> },
}

struct Worker {
    db: Weak<crate::agent_host::DatabaseState>,
    store: Arc<SqliteLearning>,
    /// The credential vault's values, redacted from every entry.
    secrets: Box<dyn SecretSource + Send>,
    /// Entries not written because the vault could not be read.
    withheld: u64,
    keys: HashMap<String, String>,
    requests: HashMap<String, RequestState>,
    recent: VecDeque<String>,
    errors: u64,
}

impl Worker {
    fn new(db: Weak<crate::agent_host::DatabaseState>, store: Arc<SqliteLearning>, secrets: Box<dyn SecretSource + Send>) -> Self {
        Self { db, store, secrets, withheld: 0, keys: HashMap::new(), requests: HashMap::new(), recent: VecDeque::new(), errors: 0 }
    }

    fn key(&mut self, turn: &str) -> String {
        if let Some(key) = self.keys.get(turn) {
            return key.clone();
        }
        let key = self.db.upgrade()
            .map(|db| request_key(&db, turn))
            .unwrap_or_else(|| turn.to_owned());
        if self.keys.len() >= 4096 {
            self.keys.clear();
        }
        self.keys.insert(turn.to_owned(), key.clone());
        key
    }

    fn state(&mut self, key: &str) -> &mut RequestState {
        if !self.requests.contains_key(key) {
            if self.requests.len() >= MAX_REQUESTS {
                if let Some(oldest) = self.recent.pop_front() {
                    self.requests.remove(&oldest);
                }
            }
            self.recent.push_back(key.to_owned());
        }
        self.requests.entry(key.to_owned()).or_default()
    }

    fn write(&mut self, key: &str, kind: JournalKind, body: Value, at: u64) {
        self.write_signed(key, kind, body, None, None, at);
    }

    /// [`Self::write`] with the failure signature digest set after scrubbing, which would
    /// take the hex digest for a token.
    fn write_signed(&mut self, key: &str, kind: JournalKind, body: Value, signature: Option<String>, origin: Option<&str>, at: u64) {
        // Bind call and receipt references without storing their raw values outside
        // the normal scrubber; token-like provider IDs may otherwise be redacted.
        let call_ref = body["call_id"].as_str();
        let failed_ref = body["failed_call_id"].as_str();
        let repair_ref = body["repair_call_id"].as_str();
        let receipts: Vec<&str> = body["produced"].as_array().into_iter().flatten()
            .filter_map(|value| value.as_str().filter(|s| s.starts_with("receipt:")))
            .collect();
        let mut body = scrub_value(&body, 0);
        // Vault values are replaced after the pattern scrub (Requirement 19.6). Without
        // the vault the entry is not written.
        let values = match self.secrets.values() {
            Ok(values) => { redact_json(&mut body, &values); values }
            Err(error) => return self.withhold(&error),
        };
        if let Some(signature) = signature {
            body["signature"] = json!(signature);
        }
        if let Some(turn) = origin {
            body["origin_turn"] = json!(kageha_learning::lessons::lesson_origin(turn));
            let safe_hash = |s: &str| {
                if find_vault_value(s, &values).is_some() || kageha_learning::secrets::find_pattern(s).is_some_and(|finding| finding.kind != "high_entropy") {
                    return None;
                }
                self.store.lesson_reference(turn, s).ok()
            };
            if let Some(value) = call_ref.and_then(safe_hash) { body["call_ref"] = json!(value); }
            if let Some(value) = failed_ref.and_then(safe_hash) { body["failed_ref"] = json!(value); }
            if let Some(value) = repair_ref.and_then(safe_hash) { body["repair_ref"] = json!(value); }
            let receipts: Vec<String> = receipts.into_iter().filter_map(safe_hash).collect();
            if !receipts.is_empty() { body["receipt_refs"] = json!(receipts); }
        }
        let entry = JournalEntry { kind, body, at };
        if let Err(error) = self.store.journal(key, entry) {
            self.log(&error);
        }
    }

    /// Counts and logs an entry that was not written because the vault could not be read.
    /// The error never holds a value.
    fn withhold(&mut self, error: &io::Error) {
        self.withheld += 1;
        if self.withheld <= 5 || self.withheld % 1000 == 0 {
            eprintln!("kageha: run journal entry not written ({} so far): {error}", self.withheld);
        }
    }

    /// Fills `verified` on the request's lesson uses from its closing status (task 25.1;
    /// Requirement 17.3). The uses were recorded when the learned context was shown.
    fn decide_lesson_uses(&mut self, key: &str, verified: bool) {
        if let Err(error) = self.store.decide_lesson_uses(key, verified) {
            self.log(&error);
        }
    }

    /// Retires the lessons the request showed that now do worse than requests of the same
    /// task type without them (task 25.2; Requirement 17.4). Runs after
    /// [`Self::decide_lesson_uses`], so the request's own uses count.
    fn retire_lessons(&mut self, key: &str) {
        match self.store.retire_underperforming(key) {
            Ok(retired) => {
                for id in retired {
                    eprintln!("kageha: lesson L-{id} retired: its requests were verified less often than comparable requests without it");
                }
            }
            Err(error) => self.log(&error),
        }
    }

    fn log(&mut self, error: &io::Error) {
        self.errors += 1;
        if self.errors <= 5 || self.errors % 1000 == 0 {
            eprintln!("kageha: run journal write failed ({} so far): {error}", self.errors);
        }
    }

    fn handle(&mut self, message: Message) {
        match message {
            Message::Calls { turn, calls } => {
                let key = self.key(&turn);
                let state = self.state(&key);
                for call in calls {
                    if state.calls.len() >= MAX_CALLS {
                        break;
                    }
                    state.calls.insert(call.id, (call.name, call.operation, call.step));
                }
            }
            Message::Results { turn, results, at } => {
                let key = self.key(&turn);
                for result in results {
                    self.result(&key, &turn, result, at);
                }
            }
            Message::Entry { turn, kind, body, at } => {
                let key = self.key(&turn);
                self.write(&key, kind, body, at);
            }
            Message::Model { turn } => {
                let key = self.key(&turn);
                self.state(&key).model_calls += 1;
            }
            Message::Signature { turn, call_id, signature } => {
                let key = self.key(&turn);
                let state = self.state(&key);
                if state.signatures.len() < MAX_CALLS {
                    state.signatures.insert(call_id, signature);
                }
            }
            Message::SkillUsed { turn, skill, revision, call_id, tool, at } => {
                let key = self.key(&turn);
                // The entry first, so the read has a journal position before the row exists.
                self.write(&key, JournalKind::SkillUsed,
                    kageha_learning::skill_uses::skill_used_body(&skill, revision, &call_id, &tool), at);
                if let Err(error) = self.store.record_skill_use(&skill, revision, &key, at) {
                    self.log(&error);
                }
            }
            Message::Close { key, mut summary } => {
                let state = self.requests.remove(&key);
                self.recent.retain(|item| item != &key);
                // The request budget counts every turn of the request; model frames seen
                // here count requests that run without one.
                summary.model_calls = summary.model_calls.max(state.map_or(0, |state| state.model_calls));
                let verified = summary.status == "verified";
                let closed = summary.clone();
                match self.store.close_request(&key, summary) {
                    // The request is queued for reflection (task 23.1).
                    Ok(()) => {
                        if verified {
                            if let Err(error) = self.store.verify_agent_lessons(&key) { self.log(&error); }
                        }
                        self.decide_lesson_uses(&key, verified);
                        self.retire_lessons(&key);
                        // Skill uses are decided before reflection runs (task 24.2).
                        if let Some(db) = self.db.upgrade() {
                            if let Err(error) = crate::ui_skill_uses::decide(&db, &self.store, &key, &closed) {
                                self.log(&error);
                            }
                        }
                        if let Some(db) = self.db.upgrade() { crate::ui_reflection::wake(&db) }
                    }
                    Err(error) => self.log(&error),
                }
            }
            Message::Flush(ack) => {
                let _ = ack.try_send(());
            }
        }
    }

    fn result(&mut self, key: &str, turn: &str, result: ToolResult, at: u64) {
        let state = self.state(key);
        let (tool, operation, _) = state.calls.get(&result.call_id).cloned().unwrap_or_default();
        let parsed = result.complete.then(|| serde_json::from_str::<Value>(&result.head).ok()).flatten();
        let exit_code = matches!(tool.as_str(), "shell" | "remote_exec")
            .then(|| parsed.as_ref().and_then(|value| value["exit_code"].as_i64()))
            .flatten();
        let failed = result.is_error || exit_code.is_some_and(|code| code != 0);
        let excerpt = if sensitive_result(&tool) { "[sensitive result omitted]".to_owned() } else { excerpt(&result.head) };
        let index = state.order.len();
        let host_signature = state.signatures.remove(&result.call_id);
        // Operation keys can encode sensitive arguments even when hashed (short
        // secrets are guessable). They are needed only in memory for pairing.
        let journal_operation = if sensitive_result(&tool) { tool.clone() } else { operation.clone() };
        let mut entries = Vec::new();
        let mut learned = None;
        if failed {
            let mut error_code = result.outcome_code.clone()
                .or_else(|| parsed.as_ref().and_then(error_code))
                .unwrap_or_else(|| if result.is_error { "tool_error" } else { "nonzero_exit" }.into());
            if sensitive_result(&tool) {
                error_code = safe_category(&error_code).unwrap_or("tool_error").to_owned();
            }
            // The Host's signature is from the full result; the frame may hold a spilled excerpt.
            let signature = host_signature.or_else(|| failure_signature(&tool, &result.head, result.is_error));
            if let (Some(signature), Some(previous)) = (&signature, state.failures.get(&operation)) {
                // The same failure again: fixes that ran since the previous one did not work.
                if previous.signature.as_ref().is_some_and(|before| before.digest() == signature.digest()) {
                    let between = state.steps_between(previous.index, index);
                    if !between.is_empty() {
                        learned = Some(Learned::FixFailed { signature: signature.clone(), between });
                    }
                }
            }
            entries.push((JournalKind::Failure, json!({"call_id":result.call_id,"tool":tool,"operation":journal_operation,
                "error_code":error_code,"exit_code":exit_code,"excerpt":excerpt}), signature.as_ref().map(FailureSignature::digest)));
            state.failures.insert(operation.clone(), Failed { call_id: result.call_id.clone(), error_code, index, signature });
        } else if let Some(failure) = state.failures.remove(&operation) {
            let between: Vec<&String> = state.order.iter().skip(failure.index + 1).take(MAX_BETWEEN).collect();
            let between_count = index.saturating_sub(failure.index + 1);
            entries.push((JournalKind::Repair, json!({"tool":tool,"operation":journal_operation,"failed_call_id":failure.call_id,
                "repair_call_id":result.call_id,"error_code":failure.error_code,"between":between,
                "between_count":between_count}), failure.signature.as_ref().map(FailureSignature::digest)));
            // The 1–3 calls between the failure and this success are its fix.
            if let Some(signature) = failure.signature.filter(|_| !tool.starts_with("browser_") && (1..=kageha_learning::MAX_FIX_STEPS).contains(&between_count)) {
                let steps = state.steps_between(failure.index, index);
                if steps.len() == between_count && steps.iter().all(|step| !step.is_empty()) {
                    learned = Some(Learned::Repair { signature, steps });
                }
            }
        }
        let effect = match crate::ui_tools::capability(&tool).effect {
            crate::ui_tools::Effect::Read => "read",
            crate::ui_tools::Effect::Workspace => "workspace",
            crate::ui_tools::Effect::Action => "action",
            crate::ui_tools::Effect::Validation => "validation",
        };
        let mut body = json!({"call_id":result.call_id,"tool":tool,"status":if result.is_error {"failed"} else {"done"},
            "cached":result.cached,"effect":effect,"excerpt":excerpt});
        if let Some(code) = exit_code {
            body["exit_code"] = json!(code);
        }
        // Connector payloads are untrusted mail/page data. Browser receipts
        // carry only the Host-created request id and may still support ordering.
        let references = parsed.as_ref().filter(|_| !failed).map(|value| {
            if tool.starts_with("browser_") {
                let mut receipt_only = json!({});
                receipt_only["receipt"]["request_id"] = value["receipt"]["request_id"].clone();
                produced(&receipt_only)
            } else if sensitive_result(&tool) { Vec::new() } else { produced(value) }
        }).filter(|items| !items.is_empty());
        if let Some(produced) = references {
            body["produced"] = json!(produced);
        }
        entries.push((JournalKind::Tool, body, None));
        if state.order.len() < MAX_CALLS {
            state.order.push(result.call_id);
        }
        for (kind, body, signature) in entries {
            self.write_signed(key, kind, body, signature, Some(turn), at);
        }
        let stored = match learned {
            // A fix whose steps hold a vault value is not learned; the example is redacted.
            Some(Learned::Repair { signature, steps }) => self.secrets.values().and_then(|values| {
                if steps.iter().any(|step| find_vault_value(step, &values).is_some()) {
                    return Ok(());
                }
                // The signature's message is lowercased (`normalize`), so the values are
                // redacted lowercased too.
                let lowered: Vec<_> = values.iter().map(|value| zeroize::Zeroizing::new(value.to_lowercase())).collect();
                let example = scrub(&signature.message);
                let example = redact_values(&example, &values).unwrap_or(example);
                let example = redact_values(&example, &lowered).unwrap_or(example);
                self.store.record_repair(&signature, &steps, &example)
            }),
            Some(Learned::FixFailed { signature, between }) => self.store.record_fix_failure(&signature, &between).map(|_| ()),
            None => Ok(()),
        };
        if let Err(error) = stored {
            self.log(&error);
        }
    }
}

/// The evidence references a successful Host tool result produced, for ordering evidence
/// against skill reads (task 24.2): its `artifact_path`, the artifact paths of its
/// `files_changed`, and its receipt id (also as `receipt:<id>`). At most [`MAX_ITEMS`].
fn produced(value: &Value) -> Vec<String> {
    let mut references = Vec::new();
    if let Some(path) = value["artifact_path"].as_str() {
        references.push(path.to_owned());
    }
    for item in value["files_changed"].as_array().into_iter().flatten() {
        if let Some(path) = item.as_str().or_else(|| item["artifact_path"].as_str()) {
            references.push(path.to_owned());
        }
    }
    if let Some(receipt) = value["receipt"]["request_id"].as_str().filter(|id| !id.is_empty()) {
        references.push(receipt.to_owned());
        references.push(format!("receipt:{receipt}"));
    }
    references.retain(|reference| !reference.is_empty() && reference.len() <= 1024);
    references.truncate(MAX_ITEMS);
    references
}

/// The error code of a Host tool result: `error_code`, `code`, or `error.code`.
fn error_code(value: &Value) -> Option<String> {
    [&value["error_code"], &value["code"], &value["error"]["code"], &value["outcome"]["code"]]
        .into_iter()
        .find_map(|code| code.as_str().filter(|code| !code.is_empty() && code.len() <= 128))
        .map(str::to_owned)
}

/// The operation a call performs, for matching a repair to its failure: the tool plus its
/// main argument.
pub(crate) fn operation(name: &str, args: &Value) -> String {
    let main = match name {
        "shell" | "remote_exec" => program(args["command"].as_str().unwrap_or("")),
        // References are snapshot-local. Match a retry only within the same
        // observation as well as the same target, never by e-number alone.
        name if browser_input(name) => format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(
            json!([args["target"], args["observation_id"]]).to_string().as_bytes())),
        _ => argument(name, args),
    };
    let main = head(&main, 512);
    if main.is_empty() { name.to_owned() } else { format!("{name} {main}") }
}

fn browser_input(name: &str) -> bool {
    matches!(name, "browser_click" | "browser_fill" | "browser_fill_credential" | "browser_fill_email_code"
        | "browser_press" | "browser_reload" | "browser_navigate" | "browser_close" | "browser_select" | "browser_check" | "browser_upload")
}

/// The main argument of a call that is not a shell command: the integration and
/// operation, the MCP server and tool, or the first of `path`, `url`, `artifact_path`,
/// `integration`, `name` and `id`.
fn argument(name: &str, args: &Value) -> String {
    let text = |key: &str| args[key].as_str().unwrap_or("").trim().to_owned();
    match name {
        "integration_read" => format!("{}:{}", text("integration"), text("operation")),
        "mcp_call" => format!("{}:{}", text("server"), text("tool")),
        _ => ["path", "url", "artifact_path", "integration", "name", "id"]
            .iter()
            .map(|key| text(key))
            .find(|value| !value.is_empty())
            .unwrap_or_default(),
    }
}

/// The program a shell command runs: the first command of the line that is not a
/// directory change or variable setting, without environment assignments or wrappers,
/// as a file name.
fn program(command: &str) -> String {
    const SETUP: &[&str] = &["cd", "pushd", "export", "source", ".", "set", "unset", "true"];
    const WRAPPERS: &[&str] = &["sudo", "env", "time", "nohup", "exec", "command"];
    let mut first = None;
    for segment in command.split(['&', ';', '|', '\n', '(', ')']) {
        let mut words = segment.split_whitespace()
            .filter(|word| !(word.contains('=') && word.split('=').next().is_some_and(|name| !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))))
            .skip_while(|word| WRAPPERS.contains(word));
        let Some(word) = words.next() else { continue };
        let word = word.trim_matches(['"', '\'']);
        let word = word.rsplit('/').next().unwrap_or(word);
        if word.is_empty() {
            continue;
        }
        first.get_or_insert_with(|| word.to_owned());
        if !SETUP.contains(&word) {
            return word.to_owned();
        }
    }
    first.unwrap_or_default()
}

fn head(text: &str, limit: usize) -> &str {
    if text.len() <= limit {
        return text;
    }
    let mut cut = limit;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    &text[..cut]
}

/// The scrubbed first [`EXCERPT_BYTES`] of `text`.
pub(crate) fn excerpt(text: &str) -> String {
    // Scrub a little more than the excerpt so a token at the cut is still recognized.
    head(&scrub(head(text, EXCERPT_BYTES + 512)), EXCERPT_BYTES).to_owned()
}

/// Names whose value is secret, for `name=value` and `"name": "value"`.
fn secret_name(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    kageha_workspace::shell::is_secret_name(word)
        || ["token", "secret", "password", "passwd", "api_key", "apikey", "api-key", "access_key", "private_key", "authorization", "credential", "cookie"]
            .iter()
            .any(|needle| lower.contains(needle))
}

/// Whether `word` looks like a credential: a known token prefix, a JWT, or a long string
/// mixing letters and digits.
fn token_like(word: &str) -> bool {
    const PREFIXES: &[&str] = &["sk-", "sk_", "pk_live_", "rk_live_", "ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_",
        "glpat-", "xoxb-", "xoxp-", "xoxa-", "xoxs-", "AKIA", "ASIA", "AIza", "ya29.", "npm_", "hf_", "eyJ"];
    let word = word.trim_matches(['.', '=', '-']);
    if word.len() >= 16 && PREFIXES.iter().any(|prefix| word.starts_with(prefix)) {
        return true;
    }
    if word.len() < 32 {
        return false;
    }
    let (lower, upper, digit) = word.chars().fold((false, false, false), |(l, u, d), c| {
        (l || c.is_ascii_lowercase(), u || c.is_ascii_uppercase(), d || c.is_ascii_digit())
    });
    if word.contains('/') {
        // Paths are not tokens; base64 with slashes still mixes cases and digits.
        return word.len() >= 40 && !word.contains('.') && lower && upper && digit;
    }
    digit && (lower || upper)
}

/// Replaces token-like values in `text` with `[redacted]`: `Bearer <token>`, values of
/// secret-named keys (`API_KEY=...`, `"password": "..."`), known token prefixes, JWTs and
/// long letter-digit strings. The worker redacts vault values after this (task 23.2).
pub(crate) fn scrub(text: &str) -> String {
    let word_char = |c: char| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '+' | '/' | '=' | '~');
    let mut out = String::with_capacity(text.len());
    // A secret name or `Bearer` was seen; the next word is its value.
    let mut pending = false;
    let mut rest = text;
    while let Some(first) = rest.chars().next() {
        let is_word = word_char(first);
        let end = rest.find(|c: char| word_char(c) != is_word).unwrap_or(rest.len());
        let (segment, tail) = rest.split_at(end);
        rest = tail;
        if !is_word {
            // A value follows its name after `=`, `:`, quotes and spaces only.
            if pending && (segment.len() > 6 || !segment.chars().all(|c| matches!(c, ':' | '"' | '\'' | ' ' | '\t'))) {
                pending = false;
            }
            out.push_str(segment);
            continue;
        }
        if pending {
            if matches!(segment.to_ascii_lowercase().as_str(), "bearer" | "basic" | "token") {
                out.push_str(segment);
                continue;
            }
            out.push_str(REDACTED);
            pending = false;
            continue;
        }
        if let Some((name, value)) = segment.split_once('=') {
            if secret_name(name) {
                out.push_str(name);
                out.push('=');
                if !value.is_empty() {
                    out.push_str(REDACTED);
                } else {
                    pending = true;
                }
                continue;
            }
        }
        if token_like(segment) {
            out.push_str(REDACTED);
            continue;
        }
        out.push_str(segment);
        pending = segment.eq_ignore_ascii_case("bearer") || secret_name(segment);
    }
    out
}

/// `value` with every string scrubbed and cut to [`EXCERPT_BYTES`], values of
/// secret-named keys redacted, and arrays cut to [`MAX_ITEMS`].
fn scrub_value(value: &Value, depth: usize) -> Value {
    match value {
        Value::String(text) => Value::String(excerpt(text)),
        Value::Array(items) if depth < 8 => Value::Array(items.iter().take(MAX_ITEMS).map(|item| scrub_value(item, depth + 1)).collect()),
        Value::Object(object) if depth < 8 => Value::Object(object.iter().map(|(key, item)| {
            let item = if secret_name(key) && item.is_string() { json!(REDACTED) } else { scrub_value(item, depth + 1) };
            (key.clone(), item)
        }).collect::<Map<_, _>>()),
        Value::Array(_) | Value::Object(_) => json!("[truncated]"),
        other => other.clone(),
    }
}

#[cfg(test)]
#[path = "ui_learning_tests.rs"]
mod tests;
