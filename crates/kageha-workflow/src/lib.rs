//! Evidence-driven workflow completion. No browser, framework or provider dependency.
pub mod planning;
pub mod completion;
use kageha_contracts::execution::{AssistantMessage, Control, ModelPort, ModelRequest};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub version: u32,
    pub id: String,
    pub namespace: String,
    pub resource: String,
    pub origin: String,
    pub goals: BTreeMap<String, String>,
    #[serde(default)]
    pub limits: Limits,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    pub continuations: u32,
    pub no_progress: u32,
    pub checkpoints: u32,
    pub stalled_model_calls: u32,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            continuations: 24,
            no_progress: 4,
            checkpoints: 1000,
            stalled_model_calls: 4,
        }
    }
}
impl Spec {
    pub fn validate(&self) -> io::Result<()> {
        if self.version != 1
            || [&self.id, &self.namespace, &self.resource, &self.origin]
                .iter()
                .any(|v| v.is_empty() || v.len() > 2048 || v.chars().any(char::is_control))
            || self.goals.is_empty()
            || self.goals.len() > 32
            || self.goals.iter().any(|(id, text)| {
                id.is_empty() || id.len() > 128 || text.is_empty() || text.len() > 2048
            })
            || self.limits.continuations > 100
            || self.limits.no_progress == 0
            || self.limits.no_progress > 100
            || self.limits.checkpoints == 0
            || self.limits.checkpoints > 10000
            || self.limits.stalled_model_calls == 0
            || self.limits.stalled_model_calls > 1000
        {
            return Err(io::Error::other("Invalid workflow contract"));
        }
        Ok(())
    }
    pub fn digest(&self) -> io::Result<String> {
        self.validate()?;
        Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(self)?)))
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Executing,
    Repairing,
    AwaitingInput,
    Blocked,
    Verified,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct State {
    pub spec: Spec,
    pub spec_digest: String,
    pub revision: u64,
    pub phase: Phase,
    pub checkpoints: u32,
    pub continuations: u32,
    pub no_progress: u32,
    #[serde(default)]
    pub stalled_model_calls: u32,
    pub progress_token: String,
    pub satisfied: BTreeSet<String>,
    pub pending: Vec<String>,
    pub reason: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Blocker {
    pub question: String,
    pub awaiting_input: bool,
}
/// Produced by a trusted adapter, never by the model's completion claim.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Observation {
    pub resource: String,
    pub origin: String,
    pub progress_token: String,
    pub satisfied: BTreeSet<String>,
    pub pending: Vec<String>,
    pub blocker: Option<Blocker>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Checkpoint {
    BeforeModel,
    CandidateFinal,
    BeforeMutation,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Decision {
    Proceed { instruction: String },
    Continue { instruction: String },
    Verified { summary: String },
    Stop { phase: Phase, reason: String },
}
/// Storage is replaceable. A revision conflict must never overwrite a newer owner.
pub trait Store: Send + Sync {
    fn load(&self, namespace: &str, id: &str) -> io::Result<Option<State>>;
    fn save(&self, previous: Option<u64>, state: &State) -> io::Result<()>;
}
/// A controller can be attached to any model/tool loop through this port.
pub trait Port: Sync {
    fn checkpoint(&self, at: Checkpoint, control: &Control<'_>) -> io::Result<Decision>;
}

pub fn evaluate(
    spec: &Spec,
    previous: Option<&State>,
    observation: &Observation,
    at: Checkpoint,
) -> io::Result<(State, Decision)> {
    let digest = spec.digest()?;
    if previous.is_some_and(|s| s.spec_digest != digest) {
        return Err(io::Error::other(
            "Workflow contract changed; resume cannot replace its goals or authority",
        ));
    }
    if observation.progress_token.len() > 256
        || observation.pending.len() > 64
        || observation
            .satisfied
            .iter()
            .any(|id| !spec.goals.contains_key(id))
    {
        return Err(io::Error::other("Invalid workflow evidence"));
    }
    let mut state = previous.cloned().unwrap_or_else(|| State {
        spec: spec.clone(),
        spec_digest: digest,
        revision: 0,
        phase: Phase::Executing,
        checkpoints: 0,
        continuations: 0,
        no_progress: 0,
        stalled_model_calls: 0,
        progress_token: String::new(),
        satisfied: BTreeSet::new(),
        pending: vec![],
        reason: String::new(),
    });
    let progressed = state.progress_token != observation.progress_token;
    state.revision += 1;
    state.checkpoints += 1;
    state.progress_token = observation.progress_token.clone();
    state.satisfied = observation.satisfied.clone();
    state.pending = observation.pending.clone();
    if progressed {
        state.no_progress = 0;
        state.stalled_model_calls = 0;
    } else if at == Checkpoint::BeforeModel {
        state.stalled_model_calls += 1;
    }
    let decision = if observation.resource != spec.resource || observation.origin != spec.origin {
        Decision::Stop {
            phase: Phase::Blocked,
            reason: "Workflow resource or origin changed; no input authorized".into(),
        }
    } else if let Some(blocker) = &observation.blocker {
        Decision::Stop {
            phase: if blocker.awaiting_input {
                Phase::AwaitingInput
            } else {
                Phase::Blocked
            },
            reason: blocker.question.clone(),
        }
    } else if observation.satisfied.len() == spec.goals.len() && observation.pending.is_empty() {
        Decision::Verified{summary:format!("Workflow verified from fresh evidence. {} Stop boundary reached; no further mutation is authorized.",spec.goals.values().cloned().collect::<Vec<_>>().join("; "))}
    } else if state.checkpoints > spec.limits.checkpoints {
        Decision::Stop {
            phase: Phase::Blocked,
            reason: "Workflow checkpoint budget exhausted; completion remains unverified".into(),
        }
    } else if state.stalled_model_calls >= spec.limits.stalled_model_calls {
        Decision::Stop{phase:Phase::Blocked,reason:"Workflow model calls made no observed progress within the bounded recovery budget; completion remains unverified".into()}
    } else {
        let missing: Vec<_> = spec
            .goals
            .iter()
            .filter(|(id, _)| !observation.satisfied.contains(*id))
            .map(|(_, text)| text.as_str())
            .collect();
        let instruction=format!("WORKFLOW CONTROLLER: Required outcomes are not yet verified: {}. Pending original action receipts: {}. Continue the existing task; an intermediate success or your final message is not completion. Reobserve and reconcile pending actions without replaying them. Retry only undispatched input with fresh control identity. Preserve the pinned resource and caller permissions. Stop only for verified completion, a real missing fact/permission, cancellation or exhausted recovery budget.",missing.join("; "),json!(observation.pending));
        if at == Checkpoint::CandidateFinal {
            state.continuations += 1;
            if !progressed {
                state.no_progress += 1;
            }
            if state.continuations > spec.limits.continuations
                || state.no_progress >= spec.limits.no_progress
            {
                Decision::Stop{phase:Phase::Blocked,reason:"Workflow made no verified progress within its bounded recovery budget; completion remains unverified".into()}
            } else {
                Decision::Continue { instruction }
            }
        } else {
            Decision::Proceed { instruction }
        }
    };
    match &decision {
        Decision::Verified { .. } => {
            state.phase = Phase::Verified;
            state.reason.clear();
        }
        Decision::Stop { phase, reason } => {
            state.phase = phase.clone();
            state.reason = reason.clone();
        }
        _ => {
            state.phase = if state.pending.is_empty() {
                Phase::Executing
            } else {
                Phase::Repairing
            };
            state.reason.clear();
        }
    }
    Ok((state, decision))
}

pub fn checkpoint(
    store: &dyn Store,
    spec: &Spec,
    observation: &Observation,
    at: Checkpoint,
) -> io::Result<Decision> {
    let previous = store.load(&spec.namespace, &spec.id)?;
    let (state, decision) = evaluate(spec, previous.as_ref(), observation, at)?;
    store.save(previous.as_ref().map(|s| s.revision), &state)?;
    Ok(decision)
}

#[derive(Debug)]
pub struct Stop {
    pub phase: Phase,
    pub reason: String,
}
impl std::fmt::Display for Stop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.reason)
    }
}
impl std::error::Error for Stop {}
pub fn resolve(decision: Decision) -> io::Result<Option<String>> {
    match decision {
        Decision::Verified { summary } => Ok(Some(summary)),
        Decision::Stop { phase, reason } => Err(io::Error::other(Stop { phase, reason })),
        _ => Ok(None),
    }
}

/// Works with embedded loops and remote model-port bridges alike. Rejected final
/// messages stay internal; no extra user request or fabricated tool result is used.
pub struct GuardedModel<'a> {
    pub model: &'a (dyn ModelPort + Sync),
    pub workflow: &'a dyn Port,
}
impl ModelPort for GuardedModel<'_> {
    fn accounts_invocations(&self)->bool {self.model.accounts_invocations()}
    fn context_window(&self)->Option<u64> {self.model.context_window()}
    fn complete(
        &self,
        request: ModelRequest,
        control: &Control<'_>,
    ) -> io::Result<AssistantMessage> {
        loop {
            control.check()?;
            let decision = self.workflow.checkpoint(Checkpoint::BeforeModel, control)?;
            let instruction = match &decision {
                Decision::Proceed { instruction } | Decision::Continue { instruction } => {
                    instruction.clone()
                }
                _ => String::new(),
            };
            if let Some(summary) = resolve(decision)? {
                return Ok(AssistantMessage {
                    continuation: None,
                    content: json!(summary),
                    tool_calls: vec![],
                });
            }
            let mut next = request.clone();
            next.messages
                .push(json!({"role":"system","content":instruction}));
            let reply = self.model.complete(next, control)?;
            // Malformed tool calls pass through to the runtime, which answers
            // them with `invalid_tool_call` without dispatching them.
            reply.validate_envelope()?;
            control.check()?;
            if !reply.tool_calls.is_empty() {
                return Ok(reply);
            }
            let decision = self
                .workflow
                .checkpoint(Checkpoint::CandidateFinal, control)?;
            if let Some(summary) = resolve(decision)? {
                return Ok(AssistantMessage {
                    continuation: None,
                    content: json!(summary),
                    tool_calls: vec![],
                });
            }
        }
    }
}

#[cfg(feature = "sqlite")]
pub mod sqlite;

pub mod verification;
