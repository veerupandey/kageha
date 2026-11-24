//! Portable execution ports, controls and normalized messages. No runtime or vendor imports.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

pub struct Control<'a> {
    pub cancelled: &'a AtomicBool,
    pub deadline: Option<Instant>,
}
impl Control<'_> {
    pub fn check(&self) -> io::Result<()> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Execution cancelled",
            ));
        }
        if self.deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Execution deadline exceeded",
            ));
        }
        crate::request_budget::check()?;
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}
impl ToolCall {
    pub fn validate(&self) -> io::Result<()> {
        if [&self.id, &self.name]
            .iter()
            .any(|s| s.is_empty() || s.len() > 256 || s.chars().any(char::is_control))
            || !self.arguments.is_object()
        {
            return Err(io::Error::other("Invalid tool call"));
        }
        Ok(())
    }
}
/// The model role a request asks for (design section 10). The host's model router
/// picks that role's model chain; a role without its own chain uses the executor's.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Routine steps.
    #[default]
    Executor,
    /// The first planning step, the ladder's stronger-model and replan steps and the
    /// `report_blocker` check.
    Planner,
    /// The completion gate's review of `answer` and `artifact` outcomes.
    Reviewer,
}
impl Role {
    pub const ALL: [Role; 3] = [Role::Executor, Role::Planner, Role::Reviewer];
    /// The `model_preferences` / `model_roles` key for this role.
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Executor => "executor",
            Role::Planner => "planner",
            Role::Reviewer => "reviewer",
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRequest {
    pub messages: Vec<Value>,
    pub tools: Vec<Value>,
    /// Absent in older payloads, which means `executor`.
    #[serde(default)]
    pub role: Role,
}

/// Host instruction for a standalone, tool-free answer. Provider adapters use
/// it to recognize the compact request without changing the runtime wire format.
pub const DIRECT_ANSWER_INSTRUCTION: &str = "Answer directly and concisely.";
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssistantMessage {
    pub content: Value,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<ModelContinuation>,
}

/// Opaque provider state travels with the assistant message, outside tool input.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ModelContinuation {
    pub route: String,
    pub tool_signatures: std::collections::BTreeMap<String, String>,
}

/// Token counts reported by the provider for one model call. A field is `None`
/// when the provider did not report it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    /// Prompt tokens served from the provider's prompt cache.
    pub cached_tokens: Option<u64>,
}

impl std::fmt::Debug for ModelContinuation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelContinuation")
            .field("signature_count", &self.tool_signatures.len())
            .finish_non_exhaustive()
    }
}

impl AssistantMessage {
    /// Checks the message and every tool call. Use this before dispatching calls.
    pub fn validate(&self) -> io::Result<()> {
        self.validate_envelope()?;
        for call in &self.tool_calls {
            call.validate()?;
        }
        Ok(())
    }
    /// Checks everything [`validate`](Self::validate) checks except each call's
    /// name and arguments: the content, the number of calls, unique valid call
    /// ids and the continuation. A model adapter uses it so that a reply with a
    /// malformed call (arguments that are not a JSON object, for example the raw
    /// text of invalid JSON) reaches the runtime, which answers that call with
    /// `invalid_tool_call` and never dispatches it (Requirement 10.4).
    pub fn validate_envelope(&self) -> io::Result<()> {
        if self.tool_calls.len() > 16
            || (!self.content.is_null() && !self.content.is_string())
            || self.content.as_str().is_some_and(|s| s.len() > 262_144)
        {
            return Err(io::Error::other("Invalid or oversized assistant message"));
        }
        let mut ids = std::collections::HashSet::new();
        for call in &self.tool_calls {
            if call.id.is_empty() || call.id.len() > 256 || call.id.chars().any(char::is_control) {
                return Err(io::Error::other("Invalid tool call"));
            }
            if !ids.insert(&call.id) {
                return Err(io::Error::other("Duplicate tool call identity"));
            }
        }
        if let Some(state) = &self.continuation {
            if state.route.len() != 64
                || !state.route.bytes().all(|byte| byte.is_ascii_hexdigit())
                || state.tool_signatures.is_empty()
                || state.tool_signatures.len() > ids.len()
                || state.tool_signatures.iter().any(|(id, signature)| {
                    !ids.contains(id) || signature.is_empty() || signature.len() > 64 * 1024
                })
            {
                return Err(io::Error::other("Invalid model continuation"));
            }
        }
        Ok(())
    }
    pub fn from_legacy(reply: &Value) -> io::Result<Self> {
        let result = Self::from_model_output(reply)?;
        result.validate()?;
        Ok(result)
    }
    /// Like [`from_legacy`](Self::from_legacy), but checks only the envelope
    /// (see [`validate_envelope`](Self::validate_envelope)), so malformed tool
    /// calls are kept for the runtime to answer with `invalid_tool_call`.
    pub fn from_model_output(reply: &Value) -> io::Result<Self> {
        let value = &reply["message"];
        if value["role"] != "assistant" {
            return Err(io::Error::other("Expected assistant message"));
        }
        let result = Self {
            content: value["content"].clone(),
            tool_calls: serde_json::from_value(value["tool_calls"].clone())?,
            continuation: serde_json::from_value(value["continuation"].clone())?,
        };
        result.validate_envelope()?;
        Ok(result)
    }
    pub fn legacy(&self) -> Value {
        let mut message =
            json!({"role":"assistant","content":self.content,"tool_calls":self.tool_calls});
        if let Some(state) = &self.continuation {
            message["continuation"] = json!(state);
        }
        json!({"message":message})
    }
}
pub trait ModelPort {
    /// True only when every actual provider attempt is charged by this adapter.
    fn accounts_invocations(&self) -> bool {
        false
    }
    /// The model's context window in tokens, from the `context_window` field of its
    /// `models.yaml` entry. `None` when unknown; the runtime then assumes
    /// [`DEFAULT_CONTEXT_WINDOW`].
    fn context_window(&self) -> Option<u64> {
        None
    }
    /// One complete reply. Never combine partial tool arguments across adapters.
    fn complete(
        &self,
        request: ModelRequest,
        control: &Control<'_>,
    ) -> io::Result<AssistantMessage>;
}
/// Counts injected/custom models without double-charging provider adapters that
/// already reserve every network retry and fallback attempt.
pub struct BudgetedModel<'a> {
    pub base: &'a (dyn ModelPort + Sync),
}
impl ModelPort for BudgetedModel<'_> {
    fn accounts_invocations(&self) -> bool {
        true
    }
    fn context_window(&self) -> Option<u64> {
        self.base.context_window()
    }
    fn complete(
        &self,
        request: ModelRequest,
        control: &Control<'_>,
    ) -> io::Result<AssistantMessage> {
        control.check()?;
        if !self.base.accounts_invocations() {
            crate::request_budget::charge()?;
        }
        self.base.complete(request, control)
    }
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelFailureKind {
    OutputLimit,
    ContextLimit,
    InterruptedStream,
    Authentication,
    InvalidResponse,
    Transport,
    RateLimited,
}
#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
#[error("{message}")]
pub struct ModelFailure {
    pub kind: ModelFailureKind,
    pub message: String,
    pub retry_after_ms: Option<u64>,
    pub partial_tool_calls: bool,
    #[serde(default)]
    pub http_status: Option<u16>,
}
impl ModelFailure {
    pub fn new(kind: ModelFailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            retry_after_ms: None,
            partial_tool_calls: false,
            http_status: None,
        }
    }
}
pub fn model_failure(error: &io::Error) -> Option<&ModelFailure> {
    error.get_ref()?.downcast_ref::<ModelFailure>()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct PersistencePolicy {
    pub enabled: bool,
    pub max_model_calls: u32,
    pub max_duration_seconds: u64,
}
impl Default for PersistencePolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            max_model_calls: Self::DEFAULT_MODEL_CALLS,
            max_duration_seconds: Self::DEFAULT_DURATION_SECONDS,
        }
    }
}
impl PersistencePolicy {
    pub const DEFAULT_MODEL_CALLS: u32 = 5_000;
    pub const DEFAULT_DURATION_SECONDS: u64 = 86_400;
    pub const MAX_MODEL_CALLS: u32 = 100_000;
    pub const MAX_DURATION_SECONDS: u64 = 604_800;
    pub fn validate(&self) -> io::Result<()> {
        if !(1..=Self::MAX_MODEL_CALLS).contains(&self.max_model_calls)
            || !(1..=Self::MAX_DURATION_SECONDS).contains(&self.max_duration_seconds)
        {
            return Err(io::Error::other(
                "Persistence limits must be 1–100000 model calls and 1–604800 seconds",
            ));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompletionStatus {
    #[default]
    Ready,
    Continue,
    /// Every outcome is verified or has an accepted blocker, and at least one is blocked.
    Partial,
    Blocked,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompletionCheck {
    pub status: CompletionStatus,
    pub instruction: String,
    #[serde(default)]
    pub evidence: Vec<String>,
    /// Outcomes that are neither verified nor blocked. The runtime lists them
    /// under "Still unmet" when a turn ends without `Ready`. Empty for hosts
    /// that predate the field.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unmet: Vec<OutcomeSummary>,
    /// Unverified outcomes with an accepted blocker.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked: Vec<BlockedOutcome>,
    /// One line per weakened amendment of a user-origin outcome, in the order
    /// made. The runtime lists them under "Changed requirements" in every
    /// final message, including `Ready` answers (Requirement 5.3).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed_requirements: Vec<String>,
    /// Completion hook denied or could not safely complete; never publish the
    /// rejected candidate when the runtime runs out of continuation budget.
    #[serde(default, skip_serializing_if = "is_false")]
    pub stop_hook_blocked: bool,
    /// An effect was attempted but its original receipt is unresolved. The
    /// candidate cannot be safely delivered as it may falsely claim success.
    #[serde(default, skip_serializing_if = "is_false")]
    pub uncertain_effect: bool,
    /// A fresh completion check failed after state changed; prior evidence is historical.
    #[serde(default, skip_serializing_if = "is_false")]
    pub verification_unavailable: bool,
    /// Hook output is untrusted data and travels separately from instructions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_hook_feedback: Option<String>,
}
fn is_false(value:&bool)->bool {!*value}
/// A required outcome named in a [`CompletionCheck`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutcomeSummary {
    pub id: String,
    pub description: String,
}
/// A required outcome with an accepted blocker.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlockedOutcome {
    pub id: String,
    pub description: String,
    /// `missing_access`, `missing_input`, `external_failure` or `policy_denied`.
    pub category: String,
    pub reason: String,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutcomeKind {
    Success,
    Recoverable,
    Blocked,
    Uncertain,
    Fatal,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryAction {
    RetryRead,
    ChangeStrategy,
    RefreshObservation,
    InspectReceipt,
    RequestInput,
}
/// Adapter-owned recovery identity. Never accepted from model arguments.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RecoveryContext {
    pub resource_key: String,
    /// Adapter-owned tab/document scope, independent of selectors and read IDs.
    pub observation_scope: String,
    pub observation_revision: String,
    pub element_revision: String,
    pub journal_key: String,
    pub journal_revision: String,
    pub journal_unavailable: bool,
    pub uses_journal: bool,
    pub evidence_scope: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutcome {
    pub kind: ToolOutcomeKind,
    pub code: String,
    pub resource: String,
    #[serde(default)]
    pub revision: String,
    #[serde(default)]
    pub recovery: Option<RecoveryAction>,
    #[serde(default)]
    pub evidence: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolReply {
    pub content: String,
    pub is_error: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<ToolOutcome>,
}
impl ToolReply {
    pub fn terminal_error(&self) -> Option<io::Error> {
        match self.outcome.as_ref()?.kind {
            ToolOutcomeKind::Uncertain => Some(uncertain_stop(
                "Tool outcome is uncertain; inspect its original receipt before continuing",
            )),
            ToolOutcomeKind::Fatal => {
                Some(crate::policy::stop("Fatal tool outcome stopped execution"))
            }
            _ => None,
        }
    }
    pub fn success(content: String) -> Self {
        Self {
            content,
            is_error: false,
            outcome: None,
        }
    }
    pub fn failure(error: impl std::fmt::Display) -> Self {
        Self {
            content: format!("Tool failed: {error}"),
            is_error: true,
            outcome: None,
        }
    }
}
/// Host-owned state the runtime's Progress_Fingerprint is computed over. Sorted
/// collections make the fingerprint independent of the order the host reports in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProgressInputs {
    /// Required outcome id -> `verified`, `blocked` or `unmet`.
    pub outcome_statuses: BTreeMap<String, String>,
    /// Evidence that currently verifies an outcome, as `outcome_id:reference`.
    pub verified_evidence: BTreeSet<String>,
    /// Task_Workspace file path -> sha256, from the workspace manifest.
    pub workspace: BTreeMap<String, String>,
    /// Tool names offered to the model in this turn.
    pub capabilities: BTreeSet<String>,
    /// Accepted blocker ids.
    pub blockers: BTreeSet<String>,
}
/// Context window assumed for a model whose `models.yaml` entry has no
/// `context_window`, in tokens. Conservative for current chat models.
pub const DEFAULT_CONTEXT_WINDOW: u64 = 128_000;

/// Workspace-relative path of the progress note written at compaction.
pub const PROGRESS_NOTE_PATH: &str = "notes/progress.md";

/// Workspace-relative path of the agent's plan (Requirement 11.6).
pub const PLAN_PATH: &str = "todo.md";
/// Largest plan a Host returns from [`ToolPort::plan_snapshot`]. A longer plan
/// is cut at this many bytes and ends with a truncation note.
pub const PLAN_SNAPSHOT_BYTES: usize = 16 * 1024;

/// One tool call that completed before a compaction, for `notes/progress.md`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CompletedStep {
    pub tool_call_id: String,
    pub tool: String,
    /// The start of the result, on one line.
    pub result: String,
    pub is_error: bool,
}

/// What the runtime knew when it compacted tool results (Requirement 11.3). The
/// Host writes it to [`PROGRESS_NOTE_PATH`] in the Task_Workspace.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CompactionSummary {
    /// `threshold` (the context reached 70% of the window) or `context_limit`
    /// (the provider rejected the request as too long).
    pub reason: String,
    pub context_window: u64,
    /// Estimated tokens (bytes / 4) before and after compaction.
    pub estimated_tokens_before: u64,
    pub estimated_tokens_after: u64,
    /// tool_call_ids compacted by this compaction.
    pub compacted: Vec<String>,
    /// Completed tool calls in the turn, oldest first. Only the most recent
    /// [`CompactionSummary::MAX_STEPS`] are kept; `omitted_steps` counts the rest.
    pub steps: Vec<CompletedStep>,
    pub omitted_steps: usize,
    /// Workspace files written so far.
    pub files: Vec<String>,
    /// Required outcomes the Host reports as verified, with their evidence.
    pub verified: Vec<String>,
}
impl CompactionSummary {
    pub const MAX_STEPS: usize = 200;
    /// The content of `notes/progress.md`.
    pub fn progress_markdown(&self) -> String {
        let mut text = String::from("# Progress\n\n");
        text.push_str(&format!(
            "Written when the conversation context was compacted ({}). Older tool results in the conversation now hold a reference and a short summary; this note lists the work done so far.\n",
            if self.reason == "context_limit" {
                "the model rejected the request as too long"
            } else {
                "the context reached 70% of the model's window"
            }
        ));
        text.push_str("\n## Completed steps\n\n");
        if self.omitted_steps > 0 {
            text.push_str(&format!("({} earlier steps not listed.)\n", self.omitted_steps));
        }
        if self.steps.is_empty() {
            text.push_str("None.\n");
        }
        for (index, step) in self.steps.iter().enumerate() {
            let status = if step.is_error { " (failed)" } else { "" };
            text.push_str(&format!(
                "{}. `{}`{status}: {}\n",
                index + 1 + self.omitted_steps,
                step.tool,
                step.result
            ));
        }
        for (title, items) in [
            ("Files written", &self.files),
            ("Verified outcomes", &self.verified),
        ] {
            text.push_str(&format!("\n## {title}\n\n"));
            if items.is_empty() {
                text.push_str("None.\n");
            }
            for item in items {
                text.push_str(&format!("- {item}\n"));
            }
        }
        text
    }
}

pub trait ToolPort {
    /// Called once per context compaction, after the compacted ids are saved
    /// with the recovery state. Hosts write [`CompactionSummary::progress_markdown`]
    /// to [`PROGRESS_NOTE_PATH`]. A failure is reported as an event and does not
    /// stop the turn.
    fn on_compaction(&self, _summary: &CompactionSummary) -> io::Result<()> {
        Ok(())
    }
    /// The current contents of the agent's plan, [`PLAN_PATH`] in the
    /// Task_Workspace, read before each model call (Requirement 11.6). The
    /// runtime appends it to the context when it changed since the last plan
    /// it appended. Hosts return `None` when there is no workspace or plan
    /// (without creating either) and cap the text at [`PLAN_SNAPSHOT_BYTES`].
    /// A failure skips the plan for that step and does not stop the turn.
    fn plan_snapshot(&self) -> io::Result<Option<String>> {
        Ok(None)
    }
    /// User messages to append right after the results of the batch just
    /// executed, taken once (for example an image `view_image` shows: several
    /// providers reject images inside tool results). Each is
    /// `{"role":"user","content":[...]}`; the runtime appends them after every
    /// result of the batch, so call and result pairs stay intact.
    fn take_attachments(&self) -> io::Result<Vec<Value>> {
        Ok(Vec::new())
    }
    /// Whether the Host can dispatch a tool that is not in the turn's tool list,
    /// such as a compatibility alias or a tool found by discovery. The runtime
    /// asks only for names missing from that list. `Some(false)` makes a call
    /// to `name` an `invalid_tool_call` that is never dispatched (Requirement
    /// 10.4). `None`, the default, means the Host does not say, and the call is
    /// dispatched so the Host's own error reaches the model. Must be cheap: no
    /// I/O beyond in-memory lookups.
    fn accepts_unlisted(&self, _name: &str) -> Option<bool> {
        None
    }
    /// Current trusted authority/observation revision, read without dispatching
    /// the requested operation. A refreshed binding invalidates old blockers.
    fn recovery_context(&self, _call: &ToolCall) -> io::Result<Option<RecoveryContext>> { Ok(None) }
    fn objective_revision(&self)->io::Result<Option<String>> {Ok(None)}
    fn recovery_revision(&self, _call: &ToolCall) -> io::Result<Option<String>> {
        Ok(None)
    }
    fn persistence_policy(&self) -> io::Result<Option<PersistencePolicy>> {
        Ok(None)
    }
    fn recovery_checkpoint(&self) -> io::Result<Value> {
        Ok(Value::Null)
    }
    fn save_recovery(&self, _state: &Value) -> io::Result<()> {
        Ok(())
    }
    /// Advisory state sampled for completion reporting or compaction.
    /// Adapters without host state report nothing.
    fn progress_inputs(&self) -> io::Result<ProgressInputs> {
        Ok(ProgressInputs::default())
    }
    fn completion_check(
        &self,
        _candidate: &str,
        _control: &Control<'_>,
    ) -> io::Result<CompletionCheck> {
        Ok(CompletionCheck::default())
    }
    /// Host-owned batch execution. Legacy adapters remain serial by default.
    fn execute_batch(
        &self,
        calls: &[ToolCall],
        control: &Control<'_>,
    ) -> io::Result<Vec<ToolReply>> {
        calls
            .iter()
            .map(|call| {
                control.check()?;
                let reply = match self.execute(call, control) {
                    Ok(reply) => reply,
                    Err(error)
                        if crate::policy::is_stop(&error)
                            || is_uncertain(&error)
                            || crate::request_budget::is_exceeded(&error) =>
                    {
                        return Err(error)
                    }
                    Err(error) => ToolReply::failure(error),
                };
                if let Some(error) = reply.terminal_error() {
                    return Err(error);
                }
                control.check()?;
                Ok(reply)
            })
            .collect()
    }
    /// Scheduling metadata is supplied by the trusted adapter, never by model arguments.
    fn dispatch_access(&self, _call: &ToolCall) -> io::Result<DispatchAccess> {
        Ok(DispatchAccess::Exclusive)
    }
    fn dispatch_access_in(
        &self,
        _context: &ToolContext,
        call: &ToolCall,
    ) -> io::Result<DispatchAccess> {
        self.dispatch_access(call)
    }
    fn discover_in(&self, _context: &ToolContext) -> io::Result<Vec<Value>> {
        self.discover()
    }
    fn execute_in(
        &self,
        _context: &ToolContext,
        call: &ToolCall,
        control: &Control<'_>,
    ) -> io::Result<ToolReply> {
        self.execute(call, control)
    }
    /// Bind dispatch to the authority approved by the host, including its generation.
    fn execute_authorized_in(
        &self,
        context: &ToolContext,
        call: &ToolCall,
        authority: &ToolBinding,
        control: &Control<'_>,
    ) -> io::Result<ToolReply> {
        if self.binding(context, call)? != *authority {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Tool authority changed before dispatch",
            ));
        }
        self.execute_in(context, call, control)
    }
    fn binding(&self, _context: &ToolContext, call: &ToolCall) -> io::Result<ToolBinding> {
        Ok(ToolBinding {
            binding: "tools.default".into(),
            generation: 1,
            resource: call.name.clone(),
            class: crate::policy::ActionClass::Mutation,
            detail: format!("Run {} in the configured tool binding", call.name),
            fingerprint: String::new(),
        })
    }
    /// Canonical non-replayable mutation identity, independent of review risk.
    /// Risk wrappers delegate to the original adapter; reviewed reads return None.
    fn mutation_binding(
        &self,
        context: &ToolContext,
        call: &ToolCall,
    ) -> io::Result<Option<ToolBinding>> {
        let binding = self.binding(context, call)?;
        Ok(
            if matches!(
                binding.class,
                crate::policy::ActionClass::Mutation
                    | crate::policy::ActionClass::Upload
                    | crate::policy::ActionClass::Restricted
            ) {
                Some(binding)
            } else {
                None
            },
        )
    }
    fn release_context(&self, _session: &str) -> io::Result<()> {
        Ok(())
    }
    fn erase_context(&self, session: &str) -> io::Result<()> {
        self.release_context(session)
    }
    fn discover(&self) -> io::Result<Vec<Value>>;
    fn execute(&self, call: &ToolCall, control: &Control<'_>) -> io::Result<ToolReply>;
    /// Consume durable user steering only at an execution checkpoint.
    fn checkpoint(&self) -> io::Result<Vec<String>> {
        Ok(Vec::new())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum DispatchAccess {
    /// A barrier against all calls in the batch, including reads.
    Exclusive,
    /// Ordered against other calls with this key. Empty means independent.
    Resource(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolContext {
    pub session: String,
    pub turn: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolBinding {
    pub binding: String,
    pub generation: u64,
    pub resource: String,
    pub class: crate::policy::ActionClass,
    pub detail: String,
    /// Adapter configuration/resource fingerprint; never raw credentials.
    pub fingerprint: String,
}
/// Transport ambiguity is distinct from a confirmed negative tool result.
#[derive(Debug)]
pub(crate) struct UncertainStop(String);
impl std::fmt::Display for UncertainStop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for UncertainStop {}
/// Input may have completed, but its durable publication failed. Stop the turn
/// rather than letting the model retry the mutation under a new call identity.
pub fn uncertain_stop(message: impl Into<String>) -> io::Error {
    io::Error::other(UncertainStop(message.into()))
}
pub fn is_uncertain(error: &io::Error) -> bool {
    if error.get_ref().is_some_and(|e| e.is::<UncertainStop>()) {
        return true;
    }
    error
        .get_ref()
        .and_then(|e| e.downcast_ref::<crate::ServiceError>())
        .is_some_and(|e| e.code == crate::ErrorCode::Uncertain)
}

#[cfg(test)]
mod assistant_message_tests {
    use super::{AssistantMessage, ToolCall};
    use serde_json::json;

    fn reply(calls: Vec<ToolCall>) -> AssistantMessage {
        AssistantMessage { content: json!(null), tool_calls: calls, continuation: None }
    }
    fn call(id: &str, name: &str, arguments: serde_json::Value) -> ToolCall {
        ToolCall { id: id.into(), name: name.into(), arguments }
    }

    #[test]
    fn the_envelope_allows_malformed_calls_but_not_bad_ids() {
        let malformed = reply(vec![call("a", "", json!("{\"n\":")), call("b", "read", json!({}))]);
        malformed.validate_envelope().unwrap();
        assert!(malformed.validate().is_err());
        for calls in [
            vec![call("", "read", json!({}))],
            vec![call("a", "read", json!({})), call("a", "read", json!({}))],
        ] {
            assert!(reply(calls).validate_envelope().is_err());
        }
        reply(vec![call("a", "read", json!({}))]).validate().unwrap();
    }
}

#[cfg(test)]
mod persistence_policy_tests {
    use super::PersistencePolicy;

    fn policy(max_model_calls: u32, max_duration_seconds: u64) -> PersistencePolicy {
        PersistencePolicy {
            enabled: true,
            max_model_calls,
            max_duration_seconds,
        }
    }

    #[test]
    fn defaults_are_5000_calls_and_24_hours() {
        let default = PersistencePolicy::default();
        assert!(default.enabled);
        assert_eq!(default.max_model_calls, 5_000);
        assert_eq!(default.max_duration_seconds, 86_400);
        default.validate().unwrap();
        let parsed: PersistencePolicy = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed, default);
    }

    #[test]
    fn limits_allow_100000_calls_and_7_days() {
        policy(1, 1).validate().unwrap();
        policy(100_000, 604_800).validate().unwrap();
        assert!(policy(0, 86_400).validate().is_err());
        assert!(policy(100_001, 86_400).validate().is_err());
        assert!(policy(5_000, 0).validate().is_err());
        assert!(policy(5_000, 604_801).validate().is_err());
    }
}

#[cfg(test)]
mod completion_status_tests {
    use super::{CompletionCheck, CompletionStatus};

    #[test]
    fn partial_round_trips_as_snake_case() {
        assert_eq!(serde_json::to_value(CompletionStatus::Partial).unwrap(), "partial");
        let check: CompletionCheck =
            serde_json::from_str(r#"{"status":"partial","instruction":"x"}"#).unwrap();
        assert_eq!(check.status, CompletionStatus::Partial);
        assert!(check.evidence.is_empty());
        assert!(check.unmet.is_empty() && check.blocked.is_empty());
        assert!(check.changed_requirements.is_empty());
    }

    #[test]
    fn outcome_summaries_round_trip_and_are_omitted_when_empty() {
        use super::{BlockedOutcome, OutcomeSummary};
        let empty = serde_json::to_value(CompletionCheck::default()).unwrap();
        assert_eq!(empty, serde_json::json!({"status":"ready","instruction":"","evidence":[]}));
        let check = CompletionCheck {
            status: CompletionStatus::Continue,
            unmet: vec![OutcomeSummary { id: "file".into(), description: "Write it".into() }],
            blocked: vec![BlockedOutcome {
                id: "submit".into(),
                description: "Submit it".into(),
                category: "missing_access".into(),
                reason: "No login".into(),
            }],
            changed_requirements: vec!["file: weakened".into()],
            ..Default::default()
        };
        // The runtime bridge sends the check as JSON with these same types.
        let back: CompletionCheck =
            serde_json::from_value(serde_json::to_value(&check).unwrap()).unwrap();
        assert_eq!(back.unmet, check.unmet);
        assert_eq!(back.blocked, check.blocked);
        assert_eq!(back.changed_requirements, check.changed_requirements);
    }
}

#[cfg(test)]
mod compaction_summary_tests {
    use super::{CompactionSummary, CompletedStep};

    #[test]
    fn progress_markdown_lists_steps_files_and_verified_outcomes() {
        let summary = CompactionSummary {
            reason: "threshold".into(),
            steps: vec![
                CompletedStep {
                    tool_call_id: "c1".into(),
                    tool: "web_fetch".into(),
                    result: "200 OK".into(),
                    is_error: false,
                },
                CompletedStep {
                    tool_call_id: "c2".into(),
                    tool: "shell".into(),
                    result: "exit 1".into(),
                    is_error: true,
                },
            ],
            omitted_steps: 3,
            files: vec!["outputs/report.md".into()],
            verified: vec![],
            ..Default::default()
        };
        let text = summary.progress_markdown();
        assert!(text.starts_with("# Progress\n"));
        assert!(text.contains("70% of the model's window"));
        assert!(text.contains("(3 earlier steps not listed.)"));
        assert!(text.contains("4. `web_fetch`: 200 OK\n"));
        assert!(text.contains("5. `shell` (failed): exit 1\n"));
        assert!(text.contains("## Files written\n\n- outputs/report.md\n"));
        assert!(text.contains("## Verified outcomes\n\nNone.\n"));
        let back: CompactionSummary =
            serde_json::from_value(serde_json::to_value(&summary).unwrap()).unwrap();
        assert_eq!(back, summary);
    }
}

#[cfg(test)]
mod model_role_tests {
    use super::{ModelRequest, Role};
    use serde_json::json;

    #[test]
    fn role_is_snake_case_and_defaults_to_executor() {
        let old: ModelRequest = serde_json::from_value(json!({"messages":[],"tools":[]})).unwrap();
        assert_eq!(old.role, Role::Executor);
        let planned: ModelRequest =
            serde_json::from_value(json!({"messages":[],"tools":[],"role":"planner"})).unwrap();
        assert_eq!(planned.role, Role::Planner);
        assert_eq!(serde_json::to_value(Role::Reviewer).unwrap(), "reviewer");
        assert!(serde_json::from_value::<Role>(json!("Planner")).is_err());
        for role in Role::ALL {
            assert_eq!(serde_json::to_value(role).unwrap(), role.as_str());
        }
    }
}
