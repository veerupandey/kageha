//! Persisted, resource-scoped failure and evidence tracking. No tool is replayed
//! solely because the model asks again or changes the spelling of a query.
use crate::execution::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// Checkpoint format written by this version. Version 1 still loads.
pub const RECOVERY_VERSION: u32 = 3;
/// Largest `best_candidate` accepted from a checkpoint, in bytes.
pub const MAX_BEST_CANDIDATE_BYTES: usize = 1 << 20;
/// Largest number of compacted tool_call_ids accepted from a checkpoint.
pub const MAX_COMPACTED: usize = 100_000;
/// Diagnostic novelty cannot buy an unbounded action/validation recovery loop.
pub const OUTCOME_STALL_LIMIT: u32 = 32;

/// Position on the Escalation_Ladder, in escalation order. The derived
/// ordering lets callers check "at least `ChangeMethod`".
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum LadderStep {
    #[default]
    Normal,
    Repair,
    ChangeMethod,
    StrongerModel,
    Replan,
    Conclude,
}

impl LadderStep {
    /// The step's checkpoint and event name (its serde form).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Repair => "repair",
            Self::ChangeMethod => "change_method",
            Self::StrongerModel => "stronger_model",
            Self::Replan => "replan",
            Self::Conclude => "conclude",
        }
    }
}

/// `Conclude` step instruction.
pub const CONCLUDE_ACTION: &str = "Deliver verified work. You can write the deliverable and record required outcomes, evidence or blockers for tracked requirements. Explain any remaining limitation in the answer.";

/// What observing one tool result did to a [`RecoveryState`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Observation {
    /// The result added new evidence.
    pub progress: bool,
    /// The result was the failure that made its operation exhausted (the
    /// second failure at the same resource and revision).
    pub exhausted: bool,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DiagnosticRead {
    pub revision: String,
    pub count: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RecoveryState {
    pub version: u32,
    pub blocked_browser_steps: u32,
    pub failures: BTreeMap<String, Failure>,
    /// Hashed read signatures only; no page text or personal data is persisted.
    pub diagnostic_reads: BTreeMap<String, DiagnosticRead>,
    pub stalled_read_scope: Option<String>,
    /// Branches requiring user input, retained across checkpoint recovery.
    pub input_blockers: BTreeMap<String, String>,
    pub evidence: BTreeSet<String>,
    pub no_progress: u32,
    pub model_recoveries: u32,
    pub context_recoveries: u32,
    pub extensions: u32,
    pub extension_evidence: usize,
    // Version 2. Every field defaults, so a v1 checkpoint loads unchanged.
    pub fingerprint: String,
    /// Progress in recorded obligations, independent of diagnostic/log churn.
    pub objective_fingerprint: String,
    pub objective_milestones: BTreeSet<String>,
    pub stalled_batches: u32,
    pub conclude_batches: u32,
    /// Consecutive steps with an unchanged fingerprint.
    pub stale_steps: u32,
    pub ladder: LadderStep,
    /// Gate rejections with no tool call in between.
    pub consecutive_rejections: u32,
    pub best_candidate: Option<String>,
    /// tool_call_ids whose results were compacted to a reference.
    pub compacted: BTreeSet<String>,
    /// sha256 (hex) of the plan last appended to the context as a `[todo.md]`
    /// message, or `None` when none was. See [`crate::turn::restated_plan_hash`].
    pub plan_hash: Option<String>,
    /// The request's first model call, which uses the planner (Requirement
    /// 14.3), has been made. A new request starts with `false`; a checkpoint
    /// saved before this field existed belongs to a request that already ran,
    /// so it loads as `true` and a resumed request is not planned again.
    #[serde(default = "already_planned")]
    pub planned: bool,
}
fn already_planned() -> bool {
    true
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Failure {
    pub operation: String,
    pub resource: String,
    pub code: String,
    pub revision: String,
    pub count: u32,
    pub input_specific: bool,
    pub arguments_fingerprint: String,
    #[serde(skip_serializing_if="Option::is_none")]
    pub context: Option<RecoveryContext>,
}
fn context(call:&ToolCall)->Option<RecoveryContext> {serde_json::from_value(call.arguments["_runtime_context"].clone()).ok()}
fn journal_failure(code:&str)->bool {matches!(code,"action_journal_capacity"|"journal_storage_full"|"journal_storage_unavailable"|"journal_corrupt"|"journal_migration_failed")}
fn context_revision<'a>(ctx:&'a RecoveryContext,code:&str)->&'a str {
    if journal_failure(code) {&ctx.journal_revision}
    else if matches!(code,"stale_observation"|"observation_stale"|"observation_expired"|"stale_document"|"stale_binding"|"document_changed"|"browser_binding_changed"|"observation_changed") {&ctx.observation_revision}
    else {&ctx.element_revision}
}
fn argument_fingerprint(call:&ToolCall)->String {
    let mut args=call.arguments.clone();
    if let Some(map)=args.as_object_mut() {
        for key in ["_runtime_context","_runtime_revision"] {map.remove(key);}
        if context(call).is_some() {for key in ["observation_id","binding_id","request_id","target"] {map.remove(key);}}
    }
    fingerprint(args.to_string().as_bytes())
}
/// Snapshots, refreshed references and diagnostic files do not reset a blocked
/// action's completion window. Accepted deliverables and verified effects do.
pub fn objective_result(call:&ToolCall,reply:&ToolReply)->bool {
    if reply.is_error || reply.outcome.as_ref().is_some_and(|o|o.kind!=ToolOutcomeKind::Success) {return false;}
    let value=body(reply);
    // A historical receipt and an acknowledged pointer sequence do not show
    // that the current workflow advanced. A later observation can establish an
    // actual panel change; a declared postcondition can establish click success.
    if call.name=="browser_receipts" || value["receipt"]["action"]=="click" && value["receipt"]["expected"].is_null()
        && value["receipt"]["confirmation"].is_null() && !value["receipt"]["result_document_id"].is_string() {return false;}
    (call.name=="record_outcome_evidence" && value["verified_reference"].is_string())
        || reply.outcome.as_ref().is_some_and(|o|!o.evidence.is_empty())
        || value["receipt"]["state"]=="verified"
}
impl RecoveryState {
    fn diagnostic_key(call:&ToolCall)->Option<(String,String)> {
        if !matches!(call.name.as_str(),"browser_snapshot"|"browser_inspect") {return None;}
        let scope=context(call)?.observation_scope;
        if scope.is_empty() {return None;}
        // Hash only fields the read actually executes. Unsupported arguments or
        // invented tab handles must not buy another identical read.
        let query=if call.name=="browser_snapshot" {Value::Null} else {json!({
            "selector":call.arguments["selector"].as_str().unwrap_or("").trim(),
            "text":call.arguments["text"],"frame_id":call.arguments["frame_id"],
            "console":call.arguments["console"].as_bool().unwrap_or(false),
            "offset":call.arguments["offset"].as_u64().unwrap_or(0),
            "limit":call.arguments["limit"].as_u64().unwrap_or(4)
        })};
        Some((format!("{scope}:{}:{}",call.name,fingerprint(query.to_string().as_bytes())),scope))
    }
    /// Four unchanged reads may diagnose a page; a fifth identical read cannot
    /// purchase progress with a fresh observation ID or spilled-output filename.
    pub fn diagnostic_suppression(&mut self,call:&ToolCall)->Option<ToolReply> {
        let (key,scope)=Self::diagnostic_key(call)?;
        if self.diagnostic_reads.get(&key)?.count<4 {return None;}
        self.stalled_read_scope=Some(scope.clone());
        Some(ToolReply {content:json!({"error_code":"observation_loop_exhausted","retryable":false,
            "summary":"This same browser query returned unchanged state four times without an intervening verified action or user answer. It was not dispatched again. Use the existing observed field mapping, continue independent work, or ask for the missing fact. Do not remap or repeat this query."}).to_string(),is_error:true,
            outcome:Some(ToolOutcome{kind:ToolOutcomeKind::Blocked,code:"observation_loop_exhausted".into(),resource:scope,
                revision:String::new(),recovery:Some(RecoveryAction::ChangeStrategy),evidence:vec![]})})
    }
    pub fn browser_batch(&mut self,suppressed:bool,objective:bool) {
        if objective {self.blocked_browser_steps=0;}
        else if suppressed || self.blocked_browser_steps>0 {self.blocked_browser_steps=self.blocked_browser_steps.saturating_add(1);}
    }
}
pub fn operation(call: &ToolCall) -> String {
    if let Some(ctx)=context(call) {return if ctx.evidence_scope {format!("submission-proof:{}",ctx.resource_key)}else{format!("{}:{}",call.name,ctx.resource_key)};}
    if matches!(call.name.as_str(), "report_blocker" | "record_outcome_evidence") {
        return format!("{}:{}", call.name, json!([call.arguments["outcome_id"],call.arguments["_runtime_revision"]]));
    }
    let mut resource = serde_json::Map::new();
    for key in [
        "integration",
        "connector",
        "operation",
        "method",
        "action",
        "resource",
        "resource_id",
        "path",
        "url",
        "source_id",
        "browser_id",
        "tab_id",
        "target_id",
        "frame_id",
        "document_id",
        "binding_id",
        "observation_id",
        "revision",
        "state_revision",
    ] {
        if call.name.starts_with("browser_") && call.arguments.get("_runtime_revision").is_some()
            && matches!(key,"observation_id"|"binding_id"|"revision"|"state_revision") {continue;}
        if let Some(value) = call.arguments.get(key) {
            resource.insert(
                key.into(),
                if value.is_string() || value.is_number() || value.is_boolean() {
                    value.clone()
                } else {
                    json!({"sha256":fingerprint(value.to_string().as_bytes())})
                },
            );
        }
    }
    if resource.is_empty() {
        let mut arguments = call.arguments.as_object().cloned().unwrap_or_default();
        for key in [
            "query",
            "search",
            "selector",
            "locator",
            "search_phrase",
            "_runtime_revision",
        ] {
            arguments.remove(key);
        }
        resource.insert(
            "input_sha256".into(),
            json!(fingerprint(Value::Object(arguments).to_string().as_bytes())),
        );
    }
    if let Some(revision) = call.arguments.get("_runtime_revision") {
        resource.insert("_runtime_revision".into(), revision.clone());
    }
    format!("{}:{}", call.name, Value::Object(resource))
}
fn body(reply: &ToolReply) -> Value {
    let mut value = serde_json::from_str::<Value>(without_cached_note(
        reply.content.strip_prefix("Tool failed: ").unwrap_or(&reply.content),
    )).unwrap_or(Value::Null);
    // Hook commentary is context, not new tool evidence. Hash the underlying
    // adapter result, including when a plain-text result is wrapped by hooks.
    if let Some(inner) = value["tool_result"].as_str() {
        value = serde_json::from_str(inner).unwrap_or_else(|_| json!(without_cached_note(inner)));
    }
    value
}
/// First characters of the one-line note a host puts before a result that is not a JSON
/// object when it serves the result from its read cache. JSON objects get `cached` and
/// `cached_at` fields instead. Evidence ignores both, so a cache hit is not new evidence.
pub const CACHED_NOTE_PREFIX: &str = "[cached result ";
fn without_cached_note(content: &str) -> &str {
    match content.strip_prefix(CACHED_NOTE_PREFIX) {
        Some(rest) => rest.split_once('\n').map_or(content, |(_, original)| original),
        None => content,
    }
}
impl RecoveryState {
    pub fn load(value: Value) -> std::io::Result<Self> {
        if value.is_null() {
            return Ok(Self {
                version: RECOVERY_VERSION,
                ..Self::default()
            });
        }
        let mut state: Self = serde_json::from_value(value)?;
        if !matches!(state.version, 1 | 2 | RECOVERY_VERSION)
            || state.failures.len() > 256
            || state.evidence.len() > 4096
            || state.diagnostic_reads.len() > 256
            || state.objective_milestones.len() > 4096
            || state
                .best_candidate
                .as_ref()
                .is_some_and(|candidate| candidate.len() > MAX_BEST_CANDIDATE_BYTES)
            || state.compacted.len() > MAX_COMPACTED
            || state.plan_hash.as_ref().is_some_and(|hash| hash.len() != 64)
        {
            return Err(std::io::Error::other("Invalid recovery checkpoint"));
        }
        // v1 fields are kept as-is; the new fields already hold their defaults.
        state.version = RECOVERY_VERSION;
        Ok(state)
    }
    /// One explicit user continuation permits a fresh bounded observation window.
    /// Admission persists this once; process restarts never reset the counters.
    /// Original operation failures and evidence remain intact, so this grants no
    /// permission to replay an uncertain or already completed effect.
    pub fn resume_after_stall(&mut self) -> bool {
        if self.stalled_batches < OUTCOME_STALL_LIMIT && self.blocked_browser_steps < 2 {return false}
        self.blocked_browser_steps=0;
        self.stalled_batches=0;
        self.conclude_batches=0;
        self.stale_steps=0;
        self.consecutive_rejections=0;
        self.diagnostic_reads.clear();
        self.stalled_read_scope=None;
        self.objective_fingerprint.clear();
        self.ladder=LadderStep::Repair;
        self.failures.retain(|_,failure|!failure.operation.starts_with("report_blocker:")
            && !failure.operation.starts_with("record_outcome_evidence:"));
        true
    }
    pub fn blocked(&self, call: &ToolCall) -> Option<&Failure> {
        let key = operation(call);
        let current=context(call);
        self.failures.values().find(|failure| {
            let same=match (&failure.context,&current) {
                (Some(old),Some(now)) if journal_failure(&failure.code)=>now.uses_journal&&old.journal_key==now.journal_key&&(now.journal_unavailable||failure.revision==context_revision(now,&failure.code)),
                (Some(_),Some(now))=>failure.operation==key&&failure.revision==context_revision(now,&failure.code),
                _=>failure.operation==key,
            };
            same && failure.count>=2 && (!failure.input_specific || failure.arguments_fingerprint==argument_fingerprint(call))
        })
    }
    pub fn suppression(&self, call: &ToolCall) -> Option<ToolReply> {
        let blocked = self.blocked(call)?;
        let summary = "This operation already failed without progress. It was not dispatched again. Change the method, refresh the relevant access or observation revision, or continue independent work.";
        Some(ToolReply {
            content:json!({"error_code":"recovery_exhausted","resource":blocked.resource,
                "original_error_code":blocked.code,"retryable":false,
                "ladder":self.ladder,"summary":summary}).to_string(),
            is_error:true,
            outcome:Some(ToolOutcome{kind:ToolOutcomeKind::Blocked,code:"recovery_exhausted".into(),resource:blocked.resource.clone(),revision:blocked.revision.clone(),recovery:Some(RecoveryAction::ChangeStrategy),evidence:vec![]}),
        })
    }
    /// Records one tool result and returns whether it added new evidence.
    pub fn observe(&mut self, call: &ToolCall, reply: &ToolReply) -> bool {
        self.observe_result(call, reply).progress
    }
    /// Records one tool result. Besides progress, reports whether this result
    /// made its operation exhausted.
    pub fn observe_result(&mut self, call: &ToolCall, reply: &ToolReply) -> Observation {
        let value = body(reply);
        let connector = &value["connector_outcome"];
        let connector_blocked = matches!(
            connector["state"].as_str(),
            Some("disconnected" | "needs_verification" | "agent_not_assigned")
        );
        let failed = reply.is_error
            || connector_blocked
            || reply
                .outcome
                .as_ref()
                .is_some_and(|o| o.kind != ToolOutcomeKind::Success);
        let op = operation(call);
        if failed {
            // Synthetic suppressions and step-unavailable rejections must not
            // grow the failure map or create evidence that can purchase a continuation.
            if reply.outcome.as_ref().is_some_and(|o| {
                matches!(o.code.as_str(), "recovery_exhausted" | "tool_unavailable_now" | "observation_loop_exhausted")
            }) {
                return Observation::default();
            }
            let code = reply
                .outcome
                .as_ref()
                .map(|o| o.code.clone())
                .unwrap_or_else(|| {
                    value["error_code"]
                        .as_str()
                        .or_else(|| value["error"]["code"].as_str())
                        .or_else(|| value["detail"]["code"].as_str())
                        .or_else(|| connector["state"].as_str())
                        .unwrap_or("tool_failed")
                        .to_owned()
                });
            let resource = reply
                .outcome
                .as_ref()
                .map(|o| o.resource.clone())
                .unwrap_or_else(|| op.clone());
            let revision = reply
                .outcome
                .as_ref()
                .map(|o| o.revision.clone())
                .unwrap_or_else(|| {
                    json!([
                        connector["revision"],
                        connector["agents"],
                        value["revision"],
                        call.arguments["revision"],
                        call.arguments["state_revision"]
                    ])
                    .to_string()
                });
            let trusted=context(call);
            let revision=trusted.as_ref().map(|ctx|context_revision(ctx,&code).to_owned()).unwrap_or(revision);
            let resource=trusted.as_ref().map(|ctx|if journal_failure(&code){format!("browser-journal:{}",ctx.journal_key)}else{format!("browser-resource:{}",ctx.resource_key)}).unwrap_or(resource);
            let input_specific = (call.name == "discover_capabilities" && code == "capability_not_found") || matches!(
                code.as_str(),
                "invalid_params"
                    | "invalid_arguments"
                    | "invalid_request"
                    | "invalid_locator"
                    | "numeric_input_invalid"
                    | "locator_ambiguous"
                    | "research_invalid_url"
                    | "unsupported_operation"
            );
            let arguments_fingerprint = argument_fingerprint(call);
            let key = json!([op, if trusted.as_ref().is_some_and(|c|c.evidence_scope){"submission_proof_unavailable"}else{&code}, revision, if input_specific { &arguments_fingerprint } else { "" }]).to_string();
            // Keep the most recent observation for this operation; a trusted
            // access revision or a changed operation starts a fresh attempt.
            self.failures
                .retain(|_, entry| entry.operation != op || entry.code != code || entry.revision == revision);
            if !self.failures.contains_key(&key) && self.failures.len() >= 256 {
                return Observation::default();
            }
            let entry = self.failures.entry(key).or_insert(Failure {
                operation: op,
                resource,
                code,
                revision,
                count: 0,
                input_specific,
                arguments_fingerprint: arguments_fingerprint.clone(),
                context:trusted,
            });
            entry.count = if journal_failure(&entry.code) || matches!(entry.code.as_str(),"confirmation_document_changed"|"confirmation_unavailable") {2} else {entry.count.saturating_add(1)};
            return Observation {
                progress: false,
                exhausted: entry.count == 2,
            };
        }
        let current=context(call);
        let answered=call.name=="ask_human" && value["status"]=="answered";
        let effect=matches!(call.name.as_str(),"browser_click"|"browser_fill"|"browser_fill_credential"|"browser_check"|"browser_select"|"browser_press"|"browser_upload"|"browser_navigate"|"browser_reload"|"browser_close") && objective_result(call,reply);
        let independent=current.as_ref().is_some_and(|c|!c.observation_scope.is_empty()
            && self.stalled_read_scope.as_ref().is_some_and(|s|s!=&c.observation_scope))
            || call.name=="browser_open" && (value["target_id"].is_string() || value["tab_handle"].is_string());
        if answered || effect {
            if answered {self.diagnostic_reads.clear();}
            else if let Some(c)=&current {self.diagnostic_reads.retain(|key,_|!key.starts_with(&format!("{}:",c.observation_scope)));}
        }
        if self.stalled_read_scope.is_some() && (answered || effect || independent) {
            self.blocked_browser_steps=0;
            self.stalled_read_scope=None;
        }
        if let Some((key,_))=Self::diagnostic_key(call) {
            let revision=reply.outcome.as_ref().map(|o|o.revision.clone()).filter(|s|!s.is_empty())
                .unwrap_or_else(||semantic_revision(&value));
            if !self.diagnostic_reads.contains_key(&key) && self.diagnostic_reads.len()>=256 {
                if let Some(old)=self.diagnostic_reads.keys().next().cloned() {self.diagnostic_reads.remove(&old);}
            }
            let read=self.diagnostic_reads.entry(key).or_default();
            read.count=if read.revision==revision {read.count.saturating_add(1)} else {1};
            read.revision=revision;
        }
        self.failures.retain(|_, entry| entry.operation != op);
        // Content and operation determine evidence, never the model's call ID.
        // Bound stored material; the complete result remains with the host.
        let evidence_content = if let Some(text) = value.as_str() {
            text.to_owned()
        } else if value.is_null() {
            without_cached_note(&reply.content).to_owned()
        } else {
            stable_evidence(value).to_string()
        };
        let signature = format!("{}:{}", op, fingerprint(evidence_content.as_bytes()));
        let mut progress = false;
        if self.evidence.len() < 4096 {
            progress |= self.evidence.insert(signature);
        }
        if let Some(outcome) = &reply.outcome {
            for reference in outcome.evidence.iter().take(64) {
                if self.evidence.len() < 4096 {
                    progress |= self.evidence.insert(format!("evidence:{reference}"));
                }
            }
        }
        Observation {
            progress,
            exhausted: false,
        }
    }
    /// Recomputes the Progress_Fingerprint from host state and this state's
    /// evidence signatures and returns whether it changed.
    pub fn record_progress(&mut self, inputs: &ProgressInputs) -> bool {
        let next = progress_fingerprint(inputs, &self.evidence);
        let changed = next != self.fingerprint;
        self.fingerprint = next;
        changed
    }
    /// Keeps the most recent non-empty candidate answer as `best_candidate`,
    /// trimmed and cut at a character boundary to [`MAX_BEST_CANDIDATE_BYTES`]
    /// so the checkpoint still loads. An empty candidate leaves it unchanged.
    /// Returns whether a candidate was stored.
    pub fn remember_candidate(&mut self, candidate: &str) -> bool {
        let candidate = candidate.trim();
        if candidate.is_empty() {
            return false;
        }
        let mut end = candidate.len().min(MAX_BEST_CANDIDATE_BYTES);
        while !candidate.is_char_boundary(end) {
            end -= 1;
        }
        self.best_candidate = Some(candidate[..end].to_owned());
        true
    }
    /// Budget wrap-up: moves straight to `Conclude`. Returns `(from, to)` when it moved.
    pub fn conclude(&mut self) -> Option<(LadderStep, LadderStep)> {
        let from = self.ladder;
        if from == LadderStep::Conclude {
            return None;
        }
        self.ladder = LadderStep::Conclude;
        self.stale_steps = 0;
        Some((from, LadderStep::Conclude))
    }
}
pub fn semantic_revision(value:&Value)->String {fingerprint(stable_evidence(value.clone()).to_string().as_bytes())}
fn stable_evidence(mut value: Value) -> Value {
    match &mut value {
        Value::Object(object) => {
            for key in [
                "request_id",
                "tool_call_id",
                "observation_id",
                "binding_id",
                "revision",
                "state_revision",
                "backend_node_id",
                "backendNodeId",
                "ref",
                "service_epoch",
                "bounds",
                "browser_guidance",
                "timing",
                "browser_timing",
                "script_timing",
                "duration_ms",
                "wall_ms",
                "recovery",
                // Read-cache markers (see CACHED_NOTE_PREFIX).
                "cached",
                "cached_at",
            ] {
                object.remove(key);
            }
            for child in object.values_mut() {
                *child = stable_evidence(std::mem::take(child));
            }
        }
        Value::Array(values) => {
            for child in values {
                *child = stable_evidence(std::mem::take(child));
            }
        }
        _ => {}
    }
    value
}
pub(crate) fn fingerprint(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
/// The Progress_Fingerprint: sha256 of the canonical JSON array
/// `[outcome statuses, verified evidence, evidence signatures, workspace, capabilities, blockers]`.
/// Every part is a sorted `BTree` collection, so the result does not depend on
/// the order the host reported entries in.
pub fn progress_fingerprint(
    inputs: &ProgressInputs,
    evidence_signatures: &BTreeSet<String>,
) -> String {
    let canonical = serde_json::to_vec(&(
        &inputs.outcome_statuses,
        &inputs.verified_evidence,
        evidence_signatures,
        &inputs.workspace,
        &inputs.capabilities,
        &inputs.blockers,
    ))
    .expect("string collections always serialize");
    fingerprint(&canonical)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diagnostic_call(scope: &str, selector: &str, observation: &str) -> ToolCall {
        ToolCall { id: observation.into(), name: "browser_inspect".into(), arguments: json!({
            "selector": selector, "observation_id": observation,
            "_runtime_context": RecoveryContext { resource_key: scope.into(), observation_scope: scope.into(), ..Default::default() }
        }) }
    }
    #[test]
    fn unchanged_reads_are_bounded_across_observation_churn_and_checkpoints() {
        let mut state = RecoveryState {version:RECOVERY_VERSION,..Default::default()};
        for i in 0..4 {
            let call = diagnostic_call("tab-document", "input", &i.to_string());
            assert!(state.diagnostic_suppression(&call).is_none());
            state.observe(&call, &ToolReply::success(json!({"observation_id":i,"controls":[{"ref":format!("e{i}"),"value":"unchanged"}]}).to_string()));
        }
        let mut state = RecoveryState::load(serde_json::to_value(state).unwrap()).unwrap();
        assert!(state.diagnostic_suppression(&diagnostic_call("tab-document", "input", "fresh")).is_some());
        let mut forged=diagnostic_call("tab-document"," input ","another");
        forged.arguments["tab_handle"]=json!("invented");forged.arguments["unused"]=json!("novel");
        assert!(state.diagnostic_suppression(&forged).is_some());
        assert!(state.diagnostic_suppression(&diagnostic_call("other-document", "input", "fresh")).is_none());
        assert!(state.diagnostic_suppression(&diagnostic_call("tab-document", "select", "fresh")).is_none());
        // Fresh spill filenames do not matter: the host's semantic revision was
        // computed before spilling the full observation.
        let mut state = RecoveryState::default();
        for i in 0..4 {
            let mut reply = ToolReply::success(json!({"artifact_path":format!("read-{i}.json")}).to_string());
            reply.outcome = Some(ToolOutcome {kind:ToolOutcomeKind::Success,code:"ok".into(),resource:"tab".into(),revision:"same-full-observation".into(),recovery:None,evidence:vec![]});
            state.observe(&diagnostic_call("tab-document", "input", &i.to_string()), &reply);
        }
        assert!(state.diagnostic_suppression(&diagnostic_call("tab-document", "input", "last")).is_some());
    }
    #[test]
    fn read_windows_reset_on_real_change_or_new_work_but_not_old_receipts() {
        let mut state = RecoveryState::default();
        let call = diagnostic_call("tab-document", "input", "first");
        for _ in 0..3 { state.observe(&call,&ToolReply::success(json!({"value":"old"}).to_string())); }
        state.observe(&call,&ToolReply::success(json!({"value":"new"}).to_string()));
        assert_eq!(state.diagnostic_reads.values().next().unwrap().count,1);
        for _ in 0..3 { state.observe(&call,&ToolReply::success(json!({"value":"new"}).to_string())); }
        assert!(state.diagnostic_suppression(&call).is_some());
        state.browser_batch(true,false);
        let mut receipt=call.clone();receipt.name="browser_receipts".into();
        let verified=ToolReply::success(json!({"receipt":{"state":"verified"}}).to_string());
        state.observe(&receipt,&verified);
        assert!(state.diagnostic_suppression(&call).is_some());
        assert_eq!(state.blocked_browser_steps,1);
        let mut mutation=call.clone();mutation.name="browser_fill".into();
        state.observe(&mutation,&verified);
        assert!(state.diagnostic_suppression(&call).is_none());
        assert_eq!(state.blocked_browser_steps,0);
        for _ in 0..4 { state.observe(&call,&ToolReply::success("same".into())); }
        assert!(state.diagnostic_suppression(&call).is_some());state.browser_batch(true,false);
        state.observe(&diagnostic_call("other-document","input","new"),&ToolReply::success("independent".into()));
        assert_eq!(state.blocked_browser_steps,0);
        assert!(state.diagnostic_suppression(&call).is_some());
        let ask=ToolCall{id:"answer".into(),name:"ask_human".into(),arguments:json!({})};
        state.observe(&ask,&ToolReply::success(json!({"status":"answered"}).to_string()));
        assert!(state.diagnostic_suppression(&call).is_none());
    }
    #[test]
    fn click_delivery_cannot_refresh_an_unchanged_read_window() {
        let mut state=RecoveryState::default();let read=diagnostic_call("tab-document","#apply-section","first");
        for _ in 0..4 {state.observe(&read,&ToolReply::success(json!({"text":"Sign in"}).to_string()));}
        assert!(state.diagnostic_suppression(&read).is_some());state.browser_batch(true,false);
        let mut click=read.clone();click.name="browser_click".into();
        let mut delivered=ToolReply::success(json!({"receipt":{"request_id":"new-click","state":"verified","action":"click"}}).to_string());
        delivered.outcome=Some(ToolOutcome{kind:ToolOutcomeKind::Success,code:"ok".into(),resource:"login".into(),revision:String::new(),recovery:None,evidence:vec!["receipt:new-click".into()]});
        assert!(!objective_result(&click,&delivered));state.observe(&click,&delivered);
        let mut script=click.clone();script.name="browser_script".into();assert!(!objective_result(&script,&delivered));
        assert!(state.diagnostic_suppression(&read).is_some());assert_eq!(state.blocked_browser_steps,1);
        let mut history=click.clone();history.name="browser_receipts".into();assert!(!objective_result(&history,&delivered));
        let verified=ToolReply::success(json!({"receipt":{"request_id":"verified-panel","state":"verified","action":"click","expected":{"selector":"#login","text":"Welcome"}}}).to_string());
        assert!(objective_result(&click,&verified));state.observe(&click,&verified);
        assert!(state.diagnostic_suppression(&read).is_none());assert_eq!(state.blocked_browser_steps,0);
    }

    fn browser_call(method:&str,resource:&str,obs:&str,element:&str,journal:&str)->ToolCall {
        ToolCall{id:obs.into(),name:method.into(),arguments:json!({"observation_id":obs,"target":format!("e{obs}"),"_runtime_context":RecoveryContext{resource_key:resource.into(),observation_revision:obs.into(),element_revision:element.into(),journal_key:"global".into(),journal_revision:journal.into(),uses_journal:!matches!(method,"browser_snapshot"|"browser_receipts"),..Default::default()}})}
    }
    fn browser_failure(code:&str)->ToolReply {ToolReply::failure(json!({"error_code":code}))}
    #[test]
    fn browser_journal_failure_blocks_other_methods_tabs_and_survives_reload() {
        let mut state=RecoveryState::default();state.version=RECOVERY_VERSION;
        state.observe(&browser_call("browser_fill","budget","obs1","editable","full"),&browser_failure("journal_storage_full"));
        let state=RecoveryState::load(serde_json::to_value(state).unwrap()).unwrap();
        assert!(state.suppression(&browser_call("browser_click","other-tab","obs2","button","full")).is_some());
        assert!(state.suppression(&browser_call("browser_press","other-tab","obs3","","full")).is_some());
        assert!(state.suppression(&browser_call("browser_snapshot","budget","obs4","","full")).is_none());
        assert!(state.suppression(&browser_call("browser_fill","budget","obs4","editable","ready2")).is_none());
    }
    #[test]
    fn submission_recovery_retains_the_global_storage_fence() {
        let mut state=RecoveryState::default();let mut call=browser_call("browser_reconcile","submission:one","obs1","proof","full");call.arguments["_runtime_context"]["evidence_scope"]=json!(true);
        state.observe(&call,&browser_failure("journal_storage_full"));
        assert!(state.suppression(&browser_call("browser_fill","other-tab","new-obs","editable","full")).is_some());
        assert!(state.suppression(&browser_call("browser_receipts","submission:one","new-obs","proof","full")).is_none());
    }
    #[test]
    fn lost_confirmation_document_exhausts_the_proof_route_immediately() {
        for code in ["confirmation_document_changed","confirmation_unavailable"] {
            let mut state=RecoveryState::default();let mut call=browser_call("browser_reconcile","submission:one","obs1","same-proof","ready");
            call.arguments["_runtime_context"]["evidence_scope"]=json!(true);
            state.observe(&call,&browser_failure(code));
            assert!(state.suppression(&call).is_some(),"{code}");
            call.name="record_outcome_evidence".into();
            assert!(state.suppression(&call).is_some(),"{code}");
            assert!(state.suppression(&browser_call("browser_fill","unrelated-draft","obs2","editable","ready")).is_none());
        }
    }
    #[test]
    fn fresh_observations_do_not_reset_ineligible_fields_but_real_state_changes_do() {
        let mut state=RecoveryState::default();
        for obs in ["1","2"] {state.observe(&browser_call("browser_fill","budget",obs,"readonly","ready"),&browser_failure("input_readonly"));}
        assert!(state.suppression(&browser_call("browser_fill","budget","3","readonly","ready")).is_some());
        assert!(state.suppression(&browser_call("browser_fill","budget","3","editable","ready")).is_none());
        assert!(state.suppression(&browser_call("browser_fill","other","3","readonly","ready")).is_none());
        let mut state=RecoveryState::default();
        for _ in 0..2 {state.observe(&browser_call("browser_navigate","tab","stale","","ready"),&browser_failure("stale_observation"));}
        assert!(state.suppression(&browser_call("browser_navigate","tab","fresh","","ready")).is_none());
    }
    #[test]
    fn blocked_browser_completion_window_ignores_snapshot_churn() {
        let mut state=RecoveryState::default();state.browser_batch(true,false);state.browser_batch(false,false);assert_eq!(state.blocked_browser_steps,2);
        state.browser_batch(false,true);assert_eq!(state.blocked_browser_steps,0);
        state.browser_batch(true,false);state.browser_batch(false,false);state.browser_batch(false,false);state.browser_batch(false,false);assert_eq!(state.blocked_browser_steps,4);
    }
    #[test]
    fn submission_proof_attempts_share_a_budget_across_tools_and_diagnostic_churn() {
        let mut state=RecoveryState::default();
        let call=|name:&str,reference:&str|ToolCall{id:reference.into(),name:name.into(),arguments:json!({"reference":reference,"_runtime_context":RecoveryContext{resource_key:"submission:original".into(),element_revision:"same-proof".into(),evidence_scope:true,..Default::default()}})};
        state.observe(&call("record_outcome_evidence","guess1"),&browser_failure("invalid_outcome_evidence"));
        state.observe(&call("browser_reconcile","guess2"),&browser_failure("postcondition_unobserved"));
        assert!(state.suppression(&call("record_outcome_evidence","guess3")).is_some());
        let diagnostic=ToolCall{id:"report".into(),name:"write_file".into(),arguments:json!({"path":"outputs/another-report.md"})};
        assert!(!objective_result(&diagnostic,&ToolReply::success("created".into())));
        let mut changed=call("browser_reconcile","fresh");changed.arguments["_runtime_context"]["element_revision"]=json!("actual-confirmation-changed");assert!(state.suppression(&changed).is_none());
    }

    #[test]
    fn blocker_repairs_are_bounded_per_outcome_not_per_invented_reference() {
        let mut state=RecoveryState::default();
        let call=|id|ToolCall{id:"c".into(),name:"report_blocker".into(),arguments:json!({"outcome_id":id,"references":["invented"],"_runtime_revision":"same"})};
        let mut failed=ToolReply::failure("invalid");
        failed.outcome=Some(ToolOutcome{kind:ToolOutcomeKind::Recoverable,code:"invalid_blocker".into(),resource:"submit".into(),revision:"same".into(),recovery:Some(RecoveryAction::ChangeStrategy),evidence:vec![]});
        state.observe(&call("submit"),&failed);
        let mut second=call("submit");second.arguments["references"]=json!(["different guess"]);
        state.observe(&second,&failed);
        assert!(state.suppression(&call("submit")).is_some());
        assert!(state.suppression(&call("other")).is_none());
        let mut fresh=call("submit");fresh.arguments["_runtime_revision"]=json!("new failed attempt");
        assert!(state.suppression(&fresh).is_none());
    }
    #[test]
    fn locator_failures_keep_history_per_input_and_browser_target() {
        let mut state=RecoveryState::default();
        let call=|target,revision|ToolCall{id:"c".into(),name:"browser_upload".into(),arguments:json!({"path":"resume.pdf","target":target,"_runtime_revision":revision})};
        let mut failed=ToolReply::failure("ambiguous");
        failed.outcome=Some(ToolOutcome{kind:ToolOutcomeKind::Recoverable,code:"locator_ambiguous".into(),resource:"resume.pdf".into(),revision:"doc".into(),recovery:Some(RecoveryAction::ChangeStrategy),evidence:vec![]});
        state.observe(&call("input","tab-one"),&failed);
        state.observe(&call("#resume","tab-one"),&failed);
        state.observe(&call("input","tab-one"),&failed);
        assert!(state.suppression(&call("input","tab-one")).is_some());
        assert!(state.suppression(&call("#resume","tab-one")).is_none());
        assert!(state.suppression(&call("input","tab-two")).is_none());
    }
    #[test]
    fn capability_misses_exhaust_only_the_same_query() {
        let mut state = RecoveryState::default();
        let call = |query: &str| ToolCall { id:"c".into(), name:"discover_capabilities".into(), arguments:json!({"query":query,"kind":"tool"}) };
        let mut failed = ToolReply::failure("not found");
        failed.outcome = Some(ToolOutcome { kind:ToolOutcomeKind::Blocked, code:"capability_not_found".into(), resource:"discover_capabilities".into(), revision:"same".into(), recovery:Some(RecoveryAction::ChangeStrategy), evidence:vec![] });
        state.observe(&call("mail search"), &failed);
        state.observe(&call("mail search"), &failed);
        assert!(state.suppression(&call("mail search")).is_some());
        assert!(state.suppression(&call("calendar events")).is_none());
        state.observe(&call("calendar events"), &failed);
        assert!(state.suppression(&call("calendar events")).is_none());
        assert!(state.suppression(&call("mail search")).is_some());
    }

    #[test]
    fn suppression_reports_failure_without_prescribing_a_ladder() {
        let mut state = RecoveryState::default();
        let call = ToolCall{id:"read".into(),name:"read_file".into(),arguments:json!({"path":"missing.pdf"})};
        let failed = ToolReply::failure("unreadable file");
        assert_eq!(state.observe_result(&call, &failed), Observation { progress: false, exhausted: false });
        assert_eq!(state.observe_result(&call, &failed), Observation { progress: false, exhausted: true });
        // A third failure is not a new exhaustion.
        assert!(!state.observe_result(&call, &failed).exhausted);
        let summary = |state: &RecoveryState| {
            let reply = state.suppression(&call).unwrap();
            let body: Value = serde_json::from_str(&reply.content).unwrap();
            assert_eq!(body["error_code"], "recovery_exhausted");
            assert_eq!(body["ladder"], state.ladder.as_str());
            body["summary"].as_str().unwrap().to_owned()
        };
        let message = summary(&state);
        assert!(message.contains("already failed without progress"));
        assert!(!message.contains("Current step:"));
        state.ladder = LadderStep::Conclude;
        assert_eq!(summary(&state), message);
    }


    #[test]
    fn ladder_step_names_match_their_serialized_form() {
        use LadderStep::*;
        for step in [Normal, Repair, ChangeMethod, StrongerModel, Replan, Conclude] {
            assert_eq!(serde_json::to_value(step).unwrap(), step.as_str());
        }
    }

    #[test]
    fn hook_context_changes_cannot_reset_recovery_or_create_progress() {
        let mut state=RecoveryState::default();
        let call=ToolCall{id:"read".into(),name:"read_file".into(),arguments:json!({"path":"a"})};
        for content in [json!({"items":["same"]}).to_string(), "plain text".into()] {
            assert!(state.observe(&call,&ToolReply::success(content.clone())));
            for hook in ["first", "different guidance"] {
                let wrapped=json!({"tool_result":content,"hook_context":hook});
                assert!(!state.observe(&call,&ToolReply::success(wrapped.to_string())));
            }
        }
    }
    #[test]
    fn observation_metadata_changes_do_not_count_as_progress() {
        let mut state=RecoveryState::default();
        let call=ToolCall{id:"inspect".into(),name:"browser_inspect".into(),arguments:json!({"selector":"input"})};
        assert!(state.observe(&call,&ToolReply::success(json!({"observation_id":"one","binding_id":"one","revision":1,"items":[{"ref":"a","value":"same"}]}).to_string())));
        assert!(!state.observe(&call,&ToolReply::success(json!({"observation_id":"two","binding_id":"two","revision":2,"items":[{"ref":"b","value":"same"}]}).to_string())));
        assert!(state.observe(&call,&ToolReply::success(json!({"items":[{"value":"changed"}]}).to_string())));
    }
    #[test]
    fn read_cache_hits_are_not_new_evidence() {
        let mut state=RecoveryState::default();
        let call=ToolCall{id:"read".into(),name:"integration_read".into(),arguments:json!({"integration":"gmail","id":"m1"})};
        assert!(state.observe(&call,&ToolReply::success(json!({"status":"ok","data":{"id":"m1"}}).to_string())));
        assert!(!state.observe(&call,&ToolReply::success(json!({"status":"ok","data":{"id":"m1"},"cached":true,"cached_at":12.5}).to_string())));
        let text=ToolCall{id:"skill".into(),name:"skill_read".into(),arguments:json!({"name":"s"})};
        assert!(state.observe(&text,&ToolReply::success("# Skill\nbody".into())));
        assert!(!state.observe(&text,&ToolReply::success(format!("{CACHED_NOTE_PREFIX}from 12.5; pass refresh: true to read again]\n# Skill\nbody"))));
        assert!(state.observe(&text,&ToolReply::success(format!("{CACHED_NOTE_PREFIX}from 12.5]\n# Skill\nchanged"))));
    }
    #[test]
    fn repeated_failures_suppress_only_the_same_operation_and_new_evidence_counts_once() {
        let mut state = RecoveryState::load(Value::Null).unwrap();
        let call = ToolCall {
            id: "a".into(),
            name: "read".into(),
            arguments: json!({"path":"a","revision":1}),
        };
        for _ in 0..2 {
            assert!(!state.observe(&call, &ToolReply::failure("failed")));
        }
        assert!(state.suppression(&call).is_some());
        let mut other = call.clone();
        other.arguments["path"] = json!("b");
        assert!(state.suppression(&other).is_none());
        let mut fresh = call.clone();
        fresh.arguments["revision"] = json!(2);
        assert!(state.suppression(&fresh).is_none());
        assert!(state.observe(&other, &ToolReply::success("data".into())));
        other.id = "another".into();
        assert!(!state.observe(&other, &ToolReply::success("data".into())));
        assert!(RecoveryState::load(serde_json::to_value(state).unwrap())
            .unwrap()
            .suppression(&call)
            .is_some());
    }

    #[test]
    fn v1_checkpoint_loads_with_defaults_and_saves_as_current_keeping_v1_fields() {
        let v1 = json!({
            "version": 1,
            "failures": {"key": {"operation":"read:{}","resource":"r","code":"tool_failed",
                "revision":"[]","count":2,"input_specific":true,"arguments_fingerprint":"abc"}},
            // Sorted, as a BTreeSet serializes it.
            "evidence": ["evidence:two", "read:one"],
            "no_progress": 3,
            "model_recoveries": 1,
            "context_recoveries": 1,
            "extensions": 2,
            "extension_evidence": 7
        });
        let state = RecoveryState::load(v1.clone()).unwrap();
        assert_eq!(state.version, RECOVERY_VERSION);
        assert_eq!(state.fingerprint, "");
        assert_eq!(state.stale_steps, 0);
        assert_eq!(state.ladder, LadderStep::Normal);
        assert_eq!(state.consecutive_rejections, 0);
        assert_eq!(state.best_candidate, None);
        assert!(state.compacted.is_empty());
        let saved = serde_json::to_value(&state).unwrap();
        assert_eq!(saved["version"], RECOVERY_VERSION);
        assert_eq!(saved["ladder"], "normal");
        for (key, value) in v1.as_object().unwrap() {
            if key != "version" {
                assert_eq!(&saved[key], value, "v1 field {key} changed");
            }
        }
    }

    #[test]
    fn v2_checkpoint_round_trips() {
        let mut state = RecoveryState::load(Value::Null).unwrap();
        assert_eq!(state.version, RECOVERY_VERSION);
        state.evidence.insert("read:one".into());
        state.no_progress = 1;
        state.fingerprint = "f".repeat(64);
        state.stale_steps = 2;
        state.ladder = LadderStep::ChangeMethod;
        state.consecutive_rejections = 1;
        state.best_candidate = Some("draft answer".into());
        state.compacted.insert("call-1".into());
        let saved = serde_json::to_value(&state).unwrap();
        assert_eq!(saved["ladder"], "change_method");
        let loaded = RecoveryState::load(saved.clone()).unwrap();
        assert_eq!(serde_json::to_value(&loaded).unwrap(), saved);
        assert!(loaded.ladder >= LadderStep::ChangeMethod);
        assert!(LadderStep::Repair < LadderStep::ChangeMethod);
    }

    fn progress() -> (ProgressInputs, BTreeSet<String>) {
        let inputs = ProgressInputs {
            outcome_statuses: BTreeMap::from([
                ("brief".into(), "verified".into()),
                ("submit".into(), "unmet".into()),
            ]),
            verified_evidence: BTreeSet::from(["brief:workspace/outputs/brief.md".into()]),
            workspace: BTreeMap::from([
                ("outputs/brief.md".into(), "a".repeat(64)),
                ("todo.md".into(), "b".repeat(64)),
            ]),
            capabilities: BTreeSet::from(["read_file".into(), "write_file".into()]),
            blockers: BTreeSet::from(["submit:missing_access".into()]),
        };
        let signatures = BTreeSet::from(["read:one".into(), "evidence:two".into()]);
        (inputs, signatures)
    }

    #[test]
    fn progress_fingerprint_ignores_reporting_order() {
        let (inputs, signatures) = progress();
        let mut reversed = ProgressInputs::default();
        for (key, value) in inputs.outcome_statuses.iter().rev() {
            reversed.outcome_statuses.insert(key.clone(), value.clone());
        }
        for (key, value) in inputs.workspace.iter().rev() {
            reversed.workspace.insert(key.clone(), value.clone());
        }
        reversed.verified_evidence.extend(inputs.verified_evidence.iter().rev().cloned());
        reversed.capabilities.extend(inputs.capabilities.iter().rev().cloned());
        reversed.blockers.extend(inputs.blockers.iter().rev().cloned());
        let reversed_signatures: BTreeSet<String> = signatures.iter().rev().cloned().collect();
        let expected = progress_fingerprint(&inputs, &signatures);
        assert_eq!(expected.len(), 64);
        assert_eq!(progress_fingerprint(&reversed, &reversed_signatures), expected);
    }

    #[test]
    fn progress_fingerprint_changes_with_every_single_element() {
        let (inputs, signatures) = progress();
        let base = progress_fingerprint(&inputs, &signatures);
        let mut variants: Vec<(&str, ProgressInputs, BTreeSet<String>)> = Vec::new();
        let mut changed = inputs.clone();
        changed.outcome_statuses.insert("submit".into(), "blocked".into());
        variants.push(("outcome status", changed, signatures.clone()));
        let mut changed = inputs.clone();
        changed.verified_evidence.insert("submit:receipt-1".into());
        variants.push(("verified evidence", changed, signatures.clone()));
        let mut changed_signatures = signatures.clone();
        changed_signatures.insert("read:three".into());
        variants.push(("evidence signature", inputs.clone(), changed_signatures));
        let mut changed = inputs.clone();
        changed.workspace.insert("todo.md".into(), "c".repeat(64));
        variants.push(("file hash", changed, signatures.clone()));
        let mut changed = inputs.clone();
        changed.capabilities.insert("shell".into());
        variants.push(("capability", changed, signatures.clone()));
        let mut changed = inputs.clone();
        changed.blockers.clear();
        variants.push(("blocker", changed, signatures.clone()));
        // The same string in a different part is a different state.
        let mut moved = inputs.clone();
        moved.capabilities.insert("submit:missing_access".into());
        moved.blockers.clear();
        variants.push(("element moved between parts", moved, signatures.clone()));
        let mut seen = BTreeSet::from([base]);
        for (name, inputs, signatures) in variants {
            assert!(
                seen.insert(progress_fingerprint(&inputs, &signatures)),
                "{name} did not change the fingerprint"
            );
        }
    }

    fn run_turn(
        model: &crate::test_support::ScriptedModel,
        tools: &crate::test_support::FakeTools,
    ) -> String {
        let cancel = std::sync::atomic::AtomicBool::new(false);
        crate::execution::execute_turn(
            crate::turn::Start {
                version: 1,
                max_model_calls: 8,
                messages: vec![json!({"role":"user","content":"work"})],
            },
            model,
            tools,
            &Control {
                cancelled: &cancel,
                deadline: None,
            },
            |_| Ok(()),
        )
        .unwrap()
    }

    fn stale_history(tools: &crate::test_support::FakeTools) -> Vec<u64> {
        tools
            .checkpoint_history()
            .iter()
            .map(|state| state["stale_steps"].as_u64().unwrap())
            .collect()
    }

    #[test]
    fn unchanged_batches_and_gate_decisions_keep_progress_fingerprint() {
        use crate::test_support::*;
        let read = |id: &str| tool_calls_reply([tool_call(id, "read", json!({"path":"a"}))]);
        let model = ScriptedModel::new([read("r1"), read("r2"), text_reply("done")]);
        let tools = FakeTools::new().with_tool("read", ToolReply::success("same".into()));
        let (inputs, _) = progress();
        tools.set_progress(inputs.clone());
        assert_eq!(run_turn(&model, &tools), "done");
        // Progress is sampled only at the gate, not after tool batches.
        assert_eq!(tools.progress_reads(), 1);
        // Checkpoints retain compatibility fields without driving escalation.
        assert_eq!(stale_history(&tools), [0, 0, 0]);
        let state = RecoveryState::load(tools.checkpoint_state()).unwrap();
        assert_eq!(state.fingerprint, progress_fingerprint(&inputs, &state.evidence));
    }

    #[test]
    fn a_changed_fingerprint_is_still_recorded() {
        use crate::test_support::*;
        let read = |id: &str| tool_calls_reply([tool_call(id, "read", json!({"path":"a"}))]);
        let model = ScriptedModel::new([
            read("r1"),
            text_reply("early"),
            read("r2"),
            read("r3"),
            text_reply("done"),
        ]);
        let tools = FakeTools::new().with_tool("read", ToolReply::success("same".into()));
        tools.queue_gate(CompletionCheck {
            status: CompletionStatus::Continue,
            instruction: "Keep going".into(),
            evidence: vec![],
            ..Default::default()
        });
        let (inputs, _) = progress();
        let mut written = inputs.clone();
        written.workspace.insert("outputs/new.md".into(), "d".repeat(64));
        // A rejected gate then a Ready gate sample progress.
        for step in [&inputs, &written] {
            tools.queue_progress(step.clone());
        }
        tools.set_progress(written);
        assert_eq!(run_turn(&model, &tools), "done");
        assert_eq!(tools.progress_reads(), 2);
        assert_eq!(stale_history(&tools), [0, 0, 0, 0, 0]);
        let state = RecoveryState::load(tools.checkpoint_state()).unwrap();
        assert_ne!(state.fingerprint, progress_fingerprint(&inputs, &state.evidence));
    }

    #[test]
    fn unknown_versions_and_oversized_v2_fields_are_rejected() {
        assert!(RecoveryState::load(json!({"version": 99})).is_err());
        assert!(RecoveryState::load(json!({})).is_err());
        let large = "x".repeat(MAX_BEST_CANDIDATE_BYTES + 1);
        assert!(RecoveryState::load(json!({"version": 2, "best_candidate": large})).is_err());
        let ids: Vec<String> = (0..=MAX_COMPACTED).map(|i| i.to_string()).collect();
        assert!(RecoveryState::load(json!({"version": 2, "compacted": ids})).is_err());
    }

    #[test]
    fn remember_candidate_keeps_the_latest_non_empty_answer_within_the_load_limit() {
        let mut state = RecoveryState::default();
        assert!(!state.remember_candidate(" \n"));
        assert_eq!(state.best_candidate, None);
        assert!(state.remember_candidate("  first\n"));
        assert!(state.remember_candidate("second"));
        assert!(!state.remember_candidate(""));
        assert_eq!(state.best_candidate.as_deref(), Some("second"));
        // A multi-byte character straddling the limit is dropped, not split.
        let large = format!("{}é", "x".repeat(MAX_BEST_CANDIDATE_BYTES - 1));
        state.version = RECOVERY_VERSION;
        state.remember_candidate(&large);
        assert_eq!(state.best_candidate.as_ref().unwrap().len(), MAX_BEST_CANDIDATE_BYTES - 1);
        RecoveryState::load(serde_json::to_value(&state).unwrap()).unwrap();
    }
}
