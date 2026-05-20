//! Deterministic port fakes for tests. Enabled by `cfg(test)` or the
//! `test-support` feature, so other crates can use them as a dev-dependency.
//!
//! - [`ScriptedModel`] returns queued replies in order and records every request.
//! - [`FakeTools`] returns configured replies per tool, queued gate results and
//!   keeps the recovery checkpoint in memory. It records every dispatch.
use kageha_contracts::execution::{
    AssistantMessage, CompactionSummary, CompletionCheck, Control, ModelFailure, ModelPort,
    ModelRequest,
    PersistencePolicy, ProgressInputs, ToolCall, ToolPort, ToolReply,
};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    io,
    sync::{Mutex, MutexGuard},
};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panicking test must not poison the fake for later assertions.
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// One scripted model step.
#[derive(Debug, Clone)]
pub enum ModelStep {
    Reply(AssistantMessage),
    Fail(ModelFailure),
}

/// A `ModelPort` that returns queued replies and records requests.
/// A call with an empty queue fails with `unexpected model call`.
#[derive(Debug, Default)]
pub struct ScriptedModel {
    steps: Mutex<VecDeque<ModelStep>>,
    requests: Mutex<Vec<ModelRequest>>,
    context_window: Mutex<Option<u64>>,
}

impl ScriptedModel {
    pub fn new(replies: impl IntoIterator<Item = AssistantMessage>) -> Self {
        let model = Self::default();
        for reply in replies {
            model.push(reply);
        }
        model
    }

    /// Builds replies from JSON such as `{"content":"done"}`. Panics on invalid JSON,
    /// because a broken script is a test bug.
    pub fn from_json(replies: impl IntoIterator<Item = Value>) -> Self {
        Self::new(replies.into_iter().map(|reply| {
            serde_json::from_value(reply).expect("scripted reply must be an AssistantMessage")
        }))
    }

    pub fn push(&self, reply: AssistantMessage) {
        lock(&self.steps).push_back(ModelStep::Reply(reply));
    }

    pub fn push_failure(&self, failure: ModelFailure) {
        lock(&self.steps).push_back(ModelStep::Fail(failure));
    }

    /// Final text answer with no tool calls.
    pub fn push_text(&self, text: impl Into<String>) {
        self.push(text_reply(text));
    }

    /// Requests received so far, in order.
    pub fn requests(&self) -> Vec<ModelRequest> {
        lock(&self.requests).clone()
    }

    pub fn remaining(&self) -> usize {
        lock(&self.steps).len()
    }

    /// Sets the context window the model reports, in tokens.
    pub fn set_context_window(&self, tokens: Option<u64>) {
        *lock(&self.context_window) = tokens;
    }
}

impl ModelPort for ScriptedModel {
    fn context_window(&self) -> Option<u64> {
        *lock(&self.context_window)
    }

    fn complete(
        &self,
        request: ModelRequest,
        _control: &Control<'_>,
    ) -> io::Result<AssistantMessage> {
        lock(&self.requests).push(request);
        match lock(&self.steps).pop_front() {
            Some(ModelStep::Reply(reply)) => Ok(reply),
            Some(ModelStep::Fail(failure)) => Err(io::Error::other(failure)),
            None => Err(io::Error::other("unexpected model call")),
        }
    }
}

/// Assistant message with only text content.
pub fn text_reply(text: impl Into<String>) -> AssistantMessage {
    AssistantMessage {
        content: Value::String(text.into()),
        tool_calls: Vec::new(),
        continuation: None,
    }
}

/// Assistant message that requests the given tool calls.
pub fn tool_calls_reply(calls: impl IntoIterator<Item = ToolCall>) -> AssistantMessage {
    AssistantMessage {
        content: Value::Null,
        tool_calls: calls.into_iter().collect(),
        continuation: None,
    }
}

pub fn tool_call(id: impl Into<String>, name: impl Into<String>, arguments: Value) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: name.into(),
        arguments,
    }
}

#[derive(Debug, Default)]
struct ToolScript {
    queued: VecDeque<ToolReply>,
    fallback: Option<ToolReply>,
}

/// A `ToolPort` with configurable replies, gate results and checkpoint storage.
///
/// Replies for a tool come from its queue first, then from its fallback reply.
/// A tool with neither returns a failed `ToolReply`. Gate results are returned
/// in order; once the queue is empty the gate returns the default (`Ready`).
#[derive(Debug, Default)]
pub struct FakeTools {
    specs: Mutex<Vec<Value>>,
    scripts: Mutex<HashMap<String, ToolScript>>,
    calls: Mutex<Vec<ToolCall>>,
    gate: Mutex<VecDeque<CompletionCheck>>,
    candidates: Mutex<Vec<String>>,
    checkpoint: Mutex<Value>,
    saved: Mutex<Vec<Value>>,
    persistence: Mutex<Option<PersistencePolicy>>,
    steering: Mutex<VecDeque<Vec<String>>>,
    progress: Mutex<ProgressInputs>,
    queued_progress: Mutex<VecDeque<ProgressInputs>>,
    progress_reads: Mutex<usize>,
    compactions: Mutex<Vec<CompactionSummary>>,
    plan: Mutex<Option<String>>,
    queued_plans: Mutex<VecDeque<Option<String>>>,
    plan_reads: Mutex<usize>,
}

impl FakeTools {
    pub fn new() -> Self {
        Self::default()
    }

    /// Advertises a tool with an empty object schema and a fixed reply.
    pub fn with_tool(self, name: &str, reply: ToolReply) -> Self {
        lock(&self.specs).push(json!({
            "type": "function",
            "function": {
                "name": name,
                "parameters": {"type": "object", "properties": {}},
            },
        }));
        lock(&self.scripts).entry(name.into()).or_default().fallback = Some(reply);
        self
    }

    /// Replaces the advertised tool specs.
    pub fn set_specs(&self, specs: Vec<Value>) {
        *lock(&self.specs) = specs;
    }

    /// Sets the reply used when the tool's queue is empty.
    pub fn set_reply(&self, name: &str, reply: ToolReply) {
        lock(&self.scripts).entry(name.into()).or_default().fallback = Some(reply);
    }

    /// Queues a reply that is returned once, before the fallback.
    pub fn queue_reply(&self, name: &str, reply: ToolReply) {
        lock(&self.scripts)
            .entry(name.into())
            .or_default()
            .queued
            .push_back(reply);
    }

    /// Queues a Completion_Gate result for the next `completion_check`.
    pub fn queue_gate(&self, check: CompletionCheck) {
        lock(&self.gate).push_back(check);
    }

    pub fn set_persistence(&self, policy: Option<PersistencePolicy>) {
        *lock(&self.persistence) = policy;
    }

    /// Seeds the stored recovery checkpoint, e.g. to simulate a restart.
    pub fn set_checkpoint(&self, state: Value) {
        *lock(&self.checkpoint) = state;
    }

    /// Sets the `progress_inputs()` result used when the progress queue is empty.
    pub fn set_progress(&self, inputs: ProgressInputs) {
        *lock(&self.progress) = inputs;
    }

    /// Queues a `progress_inputs()` result that is returned once, before the fallback.
    pub fn queue_progress(&self, inputs: ProgressInputs) {
        lock(&self.queued_progress).push_back(inputs);
    }

    /// How many times `progress_inputs()` was called.
    pub fn progress_reads(&self) -> usize {
        *lock(&self.progress_reads)
    }

    /// Queues steering messages returned by the next `checkpoint()`.
    pub fn queue_steering(&self, messages: Vec<String>) {
        lock(&self.steering).push_back(messages);
    }

    /// Every dispatched call, in dispatch order.
    pub fn calls(&self) -> Vec<ToolCall> {
        lock(&self.calls).clone()
    }

    pub fn call_ids(&self) -> Vec<String> {
        lock(&self.calls)
            .iter()
            .map(|call| call.id.clone())
            .collect()
    }

    /// How many times a tool with this name was dispatched.
    pub fn dispatch_count(&self, name: &str) -> usize {
        lock(&self.calls)
            .iter()
            .filter(|call| call.name == name)
            .count()
    }

    /// Candidate answers passed to the gate, in order.
    pub fn gate_candidates(&self) -> Vec<String> {
        lock(&self.candidates).clone()
    }

    /// The last saved recovery checkpoint (`Null` if none).
    pub fn checkpoint_state(&self) -> Value {
        lock(&self.checkpoint).clone()
    }

    pub fn checkpoint_saves(&self) -> usize {
        lock(&self.saved).len()
    }

    /// Every `on_compaction` summary, in call order.
    pub fn compactions(&self) -> Vec<CompactionSummary> {
        lock(&self.compactions).clone()
    }

    /// Every saved recovery checkpoint, in save order.
    pub fn checkpoint_history(&self) -> Vec<Value> {
        lock(&self.saved).clone()
    }

    /// Sets the `plan_snapshot()` result used when the plan queue is empty.
    pub fn set_plan(&self, plan: Option<&str>) {
        *lock(&self.plan) = plan.map(str::to_owned);
    }

    /// Queues `plan_snapshot()` results that are returned once each, in order,
    /// before the fallback.
    pub fn queue_plans<'a>(&self, plans: impl IntoIterator<Item = Option<&'a str>>) {
        lock(&self.queued_plans).extend(plans.into_iter().map(|plan| plan.map(str::to_owned)));
    }

    /// How many times `plan_snapshot()` was called.
    pub fn plan_reads(&self) -> usize {
        *lock(&self.plan_reads)
    }
}

impl ToolPort for FakeTools {
    fn plan_snapshot(&self) -> io::Result<Option<String>> {
        *lock(&self.plan_reads) += 1;
        Ok(lock(&self.queued_plans)
            .pop_front()
            .unwrap_or_else(|| lock(&self.plan).clone()))
    }

    fn on_compaction(&self, summary: &CompactionSummary) -> io::Result<()> {
        lock(&self.compactions).push(summary.clone());
        Ok(())
    }

    fn persistence_policy(&self) -> io::Result<Option<PersistencePolicy>> {
        Ok(lock(&self.persistence).clone())
    }

    fn recovery_checkpoint(&self) -> io::Result<Value> {
        Ok(lock(&self.checkpoint).clone())
    }

    fn save_recovery(&self, state: &Value) -> io::Result<()> {
        *lock(&self.checkpoint) = state.clone();
        lock(&self.saved).push(state.clone());
        Ok(())
    }

    fn progress_inputs(&self) -> io::Result<ProgressInputs> {
        *lock(&self.progress_reads) += 1;
        Ok(lock(&self.queued_progress)
            .pop_front()
            .unwrap_or_else(|| lock(&self.progress).clone()))
    }

    fn completion_check(
        &self,
        candidate: &str,
        _control: &Control<'_>,
    ) -> io::Result<CompletionCheck> {
        lock(&self.candidates).push(candidate.to_owned());
        Ok(lock(&self.gate).pop_front().unwrap_or_default())
    }

    fn discover(&self) -> io::Result<Vec<Value>> {
        Ok(lock(&self.specs).clone())
    }

    /// A tool with a scripted reply is dispatchable even when it is not
    /// advertised; any other unlisted name is unknown.
    fn accepts_unlisted(&self, name: &str) -> Option<bool> {
        Some(lock(&self.scripts).contains_key(name))
    }

    fn execute(&self, call: &ToolCall, _control: &Control<'_>) -> io::Result<ToolReply> {
        lock(&self.calls).push(call.clone());
        let mut scripts = lock(&self.scripts);
        let reply = scripts.get_mut(&call.name).and_then(|script| {
            script
                .queued
                .pop_front()
                .or_else(|| script.fallback.clone())
        });
        Ok(reply
            .unwrap_or_else(|| ToolReply::failure(format!("no scripted reply for {}", call.name))))
    }

    fn checkpoint(&self) -> io::Result<Vec<String>> {
        Ok(lock(&self.steering).pop_front().unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{execution::execute_turn, turn::Start};
    use kageha_contracts::execution::{CompletionStatus, ModelFailureKind};
    use std::sync::atomic::AtomicBool;

    fn start() -> Start {
        Start {
            version: 1,
            max_model_calls: 4,
            messages: vec![json!({"role":"user","content":"work"})],
        }
    }

    fn assert_sync<T: Sync>() {}

    #[test]
    fn fakes_are_sync() {
        assert_sync::<ScriptedModel>();
        assert_sync::<FakeTools>();
    }

    #[test]
    fn scripted_model_returns_replies_in_order_then_fails() {
        let model = ScriptedModel::from_json([json!({"content":"one"})]);
        model.push_failure(ModelFailure::new(ModelFailureKind::Transport, "down"));
        let cancel = AtomicBool::new(false);
        let control = Control {
            cancelled: &cancel,
            deadline: None,
        };
        let request = || ModelRequest {
            messages: vec![json!({"role":"user","content":"hi"})],
            tools: vec![],
            role: Default::default(),
        };
        assert_eq!(model.complete(request(), &control).unwrap().content, "one");
        let error = model.complete(request(), &control).unwrap_err();
        assert_eq!(
            kageha_contracts::execution::model_failure(&error)
                .unwrap()
                .kind,
            ModelFailureKind::Transport
        );
        assert_eq!(
            model.complete(request(), &control).unwrap_err().to_string(),
            "unexpected model call"
        );
        assert_eq!(model.requests().len(), 3);
        assert_eq!(model.remaining(), 0);
    }

    #[test]
    fn fake_tools_prefer_queued_replies_and_fail_unknown_tools() {
        let tools = FakeTools::new().with_tool("read", ToolReply::success("fallback".into()));
        tools.queue_reply("read", ToolReply::success("first".into()));
        let cancel = AtomicBool::new(false);
        let control = Control {
            cancelled: &cancel,
            deadline: None,
        };
        let read = tool_call("a", "read", json!({}));
        assert_eq!(tools.execute(&read, &control).unwrap().content, "first");
        assert_eq!(tools.execute(&read, &control).unwrap().content, "fallback");
        assert!(
            tools
                .execute(&tool_call("b", "nope", json!({})), &control)
                .unwrap()
                .is_error
        );
        assert_eq!(tools.call_ids(), ["a", "a", "b"]);
        assert_eq!(tools.dispatch_count("read"), 2);
        assert_eq!(tools.discover().unwrap().len(), 1);
    }

    #[test]
    fn fake_tools_store_checkpoints_and_steering() {
        let tools = FakeTools::new();
        assert_eq!(tools.recovery_checkpoint().unwrap(), Value::Null);
        tools.save_recovery(&json!({"phase":"executing"})).unwrap();
        assert_eq!(tools.recovery_checkpoint().unwrap()["phase"], "executing");
        assert_eq!(tools.checkpoint_saves(), 1);
        tools.queue_steering(vec!["stop".into()]);
        assert_eq!(tools.checkpoint().unwrap(), ["stop"]);
        assert!(tools.checkpoint().unwrap().is_empty());
    }

    #[test]
    fn fakes_drive_a_turn_through_the_gate() {
        let model = ScriptedModel::new([
            text_reply("too early"),
            tool_calls_reply([tool_call("w1", "write", json!({"path":"outputs/a.md"}))]),
            text_reply("done"),
        ]);
        let tools = FakeTools::new().with_tool("write", ToolReply::success("written".into()));
        tools.queue_gate(CompletionCheck {
            status: CompletionStatus::Continue,
            instruction: "Write the file first".into(),
            evidence: vec![],
            ..Default::default()
        });
        let cancel = AtomicBool::new(false);
        let answer = execute_turn(
            start(),
            &model,
            &tools,
            &Control {
                cancelled: &cancel,
                deadline: None,
            },
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(answer, "done");
        assert_eq!(tools.call_ids(), ["w1"]);
        assert_eq!(tools.gate_candidates(), ["too early", "done"]);
        assert_eq!(model.remaining(), 0);
        assert!(model.requests()[1]
            .messages
            .iter()
            .any(|m| m["content"] == "Write the file first"));
    }

    #[test]
    fn partial_gate_ends_the_turn_with_blocked_outcomes_listed() {
        let model = ScriptedModel::new([text_reply("Report written")]);
        let tools = FakeTools::new();
        tools.queue_gate(CompletionCheck {
            status: CompletionStatus::Partial,
            instruction: "Blocked outcomes: submit: Submit it (missing_access: No login).".into(),
            evidence: vec!["report.md".into()],
            blocked: vec![kageha_contracts::execution::BlockedOutcome {
                id: "submit".into(),
                description: "Submit it".into(),
                category: "missing_access".into(),
                reason: "No login".into(),
            }],
            ..Default::default()
        });
        let cancel = AtomicBool::new(false);
        let mut status = Value::Null;
        let answer = execute_turn(
            start(),
            &model,
            &tools,
            &Control {
                cancelled: &cancel,
                deadline: None,
            },
            |frame| {
                if frame["kind"] == "task_completion" {
                    status = frame["status"].clone();
                }
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            answer,
            "Report written\n\nStill unmet:\n- submit: Submit it (blocked): No login"
        );
        assert_eq!(model.remaining(), 0);
        assert_eq!(tools.gate_candidates(), ["Report written"]);
        assert_eq!(status, "blocked");
    }
}
