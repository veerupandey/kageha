//! Append-only portable checkpoints; a dispatched batch without a confirmed
//! checkpoint is never resumed or reconstructed by replaying its calls.
use crate::{validate, ExecutionHost};
use kageha_contracts::{ErrorCode, Result, ServiceError};
use kageha_runtime::lifecycle::*;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::HashSet, io};
fn hash(v: &Value) -> String {
    format!("{:x}", Sha256::digest(v.to_string().as_bytes()))
}
fn bad(message: &str) -> ServiceError {
    ServiceError::new(ErrorCode::Conflict, message)
}
fn save(store: &dyn RunStore, turn: &str, id: &str, value: Value) -> io::Result<()> {
    if value.to_string().len() > 8 * 1024 * 1024 {
        return Err(io::Error::other("Continuation checkpoint exceeds limit"));
    }
    let digest = hash(&value);
    if store
        .effect_begin(turn, id, &digest)
        .map_err(io::Error::other)?
        .is_none()
    {
        store
            .effect_finish(turn, id, &digest, value)
            .map_err(io::Error::other)?;
    }
    Ok(())
}
pub(crate) struct Observer<'a> {
    store: &'a dyn RunStore,
    turn: &'a str,
    step: u64,
    messages: Vec<Value>,
    calls: Value,
}
impl<'a> Observer<'a> {
    pub fn new(store: &'a dyn RunStore, turn: &'a str) -> Self {
        Self {
            store,
            turn,
            step: 0,
            messages: vec![],
            calls: json!([]),
        }
    }
    pub fn observe(&mut self, frame: &Value) -> io::Result<()> {
        match frame["kind"].as_str() {
            Some("model") => {
                self.step = frame["step"]
                    .as_u64()
                    .ok_or_else(|| io::Error::other("Checkpoint step missing"))?;
                self.messages = serde_json::from_value(frame["messages"].clone())?;
                save(
                    self.store,
                    self.turn,
                    &format!("checkpoint:{}:model", self.step),
                    json!({"messages":self.messages}),
                )?;
            }
            Some("tools") => {
                self.calls = frame["calls"].clone();
                save(
                    self.store,
                    self.turn,
                    &format!("checkpoint:{}:dispatch", self.step),
                    json!({"calls":self.calls}),
                )?;
            }
            Some("tool_results") => {
                let assistant = frame.get("assistant").filter(|message| {
                    message["role"] == "assistant" && message["tool_calls"] == self.calls
                });
                self.messages.push(assistant.cloned().unwrap_or_else(|| {
                    json!({"role":"assistant","content":null,"tool_calls":self.calls})
                }));
                self.messages.extend(
                    frame["results"]
                        .as_array()
                        .ok_or_else(|| io::Error::other("Checkpoint results missing"))?
                        .iter()
                        .cloned(),
                );
                kageha_runtime::turn::compact_tool_output(&mut self.messages);
                save(
                    self.store,
                    self.turn,
                    &format!("checkpoint:{}:ready", self.step),
                    json!({"messages":self.messages}),
                )?;
            }
            _ => {}
        }
        Ok(())
    }
}
pub(crate) fn checkpoint(store: &dyn RunStore, run: &RunRecord) -> Result<Vec<Value>> {
    let limit = if run.spec.input["persistence"]["enabled"]==true {
        run.spec.input["persistence"]["max_model_calls"].as_u64().unwrap_or(1000)
    } else { run.spec.input["max_model_calls"].as_u64().unwrap_or(128) };
    for step in (1..=limit).rev() {
        let model = store.effect_get(&run.spec.turn, &format!("checkpoint:{step}:model"))?;
        if let Some(model) = model {
            let dispatch =
                store.effect_get(&run.spec.turn, &format!("checkpoint:{step}:dispatch"))?;
            let value = if dispatch.is_some() {
                store.effect_get(&run.spec.turn,&format!("checkpoint:{step}:ready"))?.and_then(|v|v.receipt).ok_or_else(||bad("A tool batch has no confirmed checkpoint. Inspect its receipts; it cannot be replayed."))?
            } else {
                model
                    .receipt
                    .ok_or_else(|| bad("Checkpoint publication is unverified"))?
            };
            return serde_json::from_value(value["messages"].clone())
                .map_err(|_| bad("Invalid conversation checkpoint"));
        }
    }
    Err(bad(
        "No portable checkpoint exists; start a new assignment explicitly",
    ))
}
pub(crate) fn previous_calls(store: &dyn RunStore, run: &RunRecord) -> Result<HashSet<String>> {
    let Some(source) = run.spec.input["resume_from"].as_str() else {
        return Ok(HashSet::new());
    };
    let source = store
        .get(source)?
        .ok_or_else(|| bad("Continuation source missing"))?;
    Ok(checkpoint(store, &source)?
        .iter()
        .flat_map(|m| m["tool_calls"].as_array().into_iter().flatten())
        .filter_map(|c| c["id"].as_str().map(str::to_owned))
        .collect())
}
impl ExecutionHost {
    pub fn resume(&self, id: &str, command: &str, confirmed: bool) -> Result<RunRecord> {
        if !confirmed
            || command.is_empty()
            || command.len() > 128
            || command.chars().any(char::is_control)
        {
            return Err(ServiceError::new(
                ErrorCode::Invalid,
                "Continuation requires confirmation and a stable bounded command_id",
            ));
        }
        let old = self
            .supervisor
            .store
            .get(id)?
            .ok_or_else(|| bad("Unknown continuation source"))?;
        if !matches!(
            old.state,
            RunState::Failed | RunState::Interrupted | RunState::Cancelled
        ) {
            return Err(bad("Only a stopped run can continue"));
        }
        let key = hash(&json!([id, command]));
        if let Some(effect) = self
            .supervisor
            .store
            .effect_get(&old.spec.turn, "runtime:continuation")?
        {
            if effect.digest != key {
                return Err(bad(
                    "A different continuation has already reserved this stopped run",
                ));
            }
            if let Some(value) = effect.receipt {
                let spec: RunSpec = serde_json::from_value(value)
                    .map_err(|_| bad("Invalid continuation admission receipt"))?;
                return self.submit(&spec);
            }
        }
        let mut messages = checkpoint(self.supervisor.store.as_ref(), &old)?;
        if let Some(workers) = &self.workers {
            let workspace = workers.workspace(&old, self.supervisor.store.as_ref())?;
            if workspace["mode"] == "worktree" && workspace["ready_for_input"] != true {
                return Err(bad(
                    "Isolated checkout is missing, unverified or settled; no shared fallback",
                ));
            }
        }
        messages.push(json!({"role":"user","content":"Continue the existing assignment from this saved checkpoint. Confirmed tool calls above already happened; do not repeat them. Take fresh observations before browser or device input."}));
        let next = hash(&json!(["resume", id, command]))[..32].to_owned();
        let mut spec = old.spec.clone();
        spec.id = next.clone();
        spec.turn = format!("resume-{next}");
        spec.request = format!("continue-{next}");
        // Preserve active ancestry so parent cancellation still reaches the
        // continuation. Explicit continuation can outlive a terminal parent.
        let parent_usable = if let Some(id) = &old.spec.parent_run {
            self.supervisor
                .store
                .get(id)?
                .is_some_and(|r| !r.cancel_requested && !r.state.terminal())
        } else {
            false
        };
        if !parent_usable {
            spec.parent = None;
            spec.parent_run = None;
        }
        spec.input["messages"] = json!(messages);
        spec.input["resume_from"] = json!(id);
        validate(&spec)?;
        let prior = self
            .supervisor
            .store
            .effect_get(&old.spec.turn, "runtime:continuation")?;
        if prior.is_none() {
            if let Err(e) =
                self.supervisor
                    .store
                    .effect_begin(&old.spec.turn, "runtime:continuation", &key)
            {
                let actual = self
                    .supervisor
                    .store
                    .effect_get(&old.spec.turn, "runtime:continuation")?;
                if actual.as_ref().is_none_or(|p| p.digest != key) {
                    return Err(e);
                }
            }
        }
        if let Err(e) = self.supervisor.store.effect_finish(
            &old.spec.turn,
            "runtime:continuation",
            &key,
            json!(spec),
        ) {
            let actual = self
                .supervisor
                .store
                .effect_get(&old.spec.turn, "runtime:continuation")?;
            if actual
                .as_ref()
                .is_none_or(|p| p.digest != key || p.receipt.as_ref() != Some(&json!(spec)))
            {
                return Err(e);
            }
        }
        self.submit(&spec)
    }
    pub(crate) fn check_continuation(&self, spec: &RunSpec) -> Result<()> {
        if let Some(id) = spec.input.get("resume_from") {
            let id = id
                .as_str()
                .ok_or_else(|| bad("Invalid continuation source"))?;
            let source = self
                .supervisor
                .store
                .get(id)?
                .ok_or_else(|| bad("Continuation source missing"))?;
            let receipt = self
                .supervisor
                .store
                .effect_get(&source.spec.turn, "runtime:continuation")?
                .and_then(|e| e.receipt)
                .ok_or_else(|| {
                    ServiceError::new(
                        ErrorCode::Unauthorized,
                        "Continuation must use the confirmed resume API",
                    )
                })?;
            if receipt != json!(spec) {
                return Err(ServiceError::new(
                    ErrorCode::Unauthorized,
                    "Run does not match its continuation admission receipt",
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kageha_runtime_store::SqliteRunStore;
    fn record() -> RunRecord {
        RunRecord {
            spec: RunSpec {
                id: "run".into(),
                session: "session".into(),
                turn: "turn".into(),
                request: "request".into(),
                parent: None,
                parent_run: None,
                agent: "main".into(),
                objective: "objective".into(),
                depth: 0,
                input: json!({"messages":[{"role":"user","content":"start"}],"max_model_calls":4}),
            },
            state: RunState::Interrupted,
            revision: 1,
            lease: None,
            completion: None,
            cancel_requested: false,
        }
    }
    #[test]
    fn checkpoints_preserve_confirmed_results_and_block_partial_batches() {
        let root = tempfile::tempdir().unwrap();
        let store = SqliteRunStore::open(&root.path().join("runtime.sqlite")).unwrap();
        let run = record();
        let mut observer = Observer::new(&store, &run.spec.turn);
        observer
            .observe(&json!({"kind":"model","step":1,"messages":run.spec.input["messages"]}))
            .unwrap();
        assert_eq!(checkpoint(&store, &run).unwrap().len(), 1);
        observer.observe(&json!({"kind":"tools","calls":[{"id":"known","name":"read_file","arguments":{"path":"proof"}}]})).unwrap();
        assert!(checkpoint(&store, &run).is_err());
        let assistant = json!({"role":"assistant","content":null,
            "tool_calls":[{"id":"known","name":"read_file","arguments":{"path":"proof"}}],
            "continuation":{"route":"a".repeat(64),"tool_signatures":{"known":"opaque"}}});
        observer.observe(&json!({"kind":"tool_results","assistant":assistant,
            "results":[{"role":"tool","tool_call_id":"known","content":"confirmed proof"}]})).unwrap();
        let messages = checkpoint(&store, &run).unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1], assistant);
        assert_eq!(messages[2]["content"], "confirmed proof");
        observer
            .observe(&json!({"kind":"model","step":2,"messages":messages}))
            .unwrap();
        observer.observe(&json!({"kind":"tools","calls":[{"id":"unknown","name":"mutation","arguments":{}}]})).unwrap();
        assert!(checkpoint(&store, &run)
            .unwrap_err()
            .message
            .contains("cannot be replayed"));
    }
    #[test]
    fn an_unpublished_checkpoint_is_not_reconstructed_or_treated_as_ready() {
        let root = tempfile::tempdir().unwrap();
        let store = SqliteRunStore::open(&root.path().join("runtime.sqlite")).unwrap();
        let run = record();
        store
            .effect_begin("turn", "checkpoint:1:model", "digest")
            .unwrap();
        assert!(checkpoint(&store, &run).is_err());
    }
}

/// Prevent a provider from repeating a confirmed mutation under a fresh call ID.
/// Matching includes the original adapter authority, so another profile or binding
/// is not blocked merely because its tool arguments happen to be the same.
#[derive(Default)]
pub(crate) struct MutationGuard {
    confirmed: HashSet<String>,
    legacy: HashSet<String>,
}
impl MutationGuard {
    pub fn check(
        &self,
        call: &kageha_contracts::execution::ToolCall,
        binding: &kageha_contracts::execution::ToolBinding,
    ) -> io::Result<()> {
        if binding.class == kageha_contracts::policy::ActionClass::Mutation
            && (self
                .confirmed
                .contains(&hash(&json!([call.name, call.arguments, binding])))
                || self
                    .legacy
                    .contains(&hash(&json!([call.name, call.arguments]))))
        {
            return Err(kageha_contracts::policy::stop("This mutation already has a confirmed result in the continuation checkpoint. Do not repeat it with a new call ID."));
        }
        Ok(())
    }
}
pub(crate) fn previous_mutations(
    store: &dyn RunStore,
    run: &RunRecord,
    control: &kageha_contracts::execution::Control<'_>,
) -> Result<MutationGuard> {
    let mut guard = MutationGuard::default();
    let Some(id) = run.spec.input["resume_from"].as_str() else {
        return Ok(guard);
    };
    let source = store
        .get(id)?
        .ok_or_else(|| bad("Continuation source missing"))?;
    let messages = checkpoint(store, &source)?;
    let mut turns = vec![];
    let mut record = source;
    let mut seen = HashSet::new();
    for _ in 0..128 {
        if !seen.insert(record.spec.id.clone()) {
            return Err(bad("Cyclic continuation authority"));
        }
        turns.push(record.spec.turn.clone());
        let Some(id) = record.spec.input["resume_from"].as_str() else {
            break;
        };
        record = store
            .get(id)?
            .ok_or_else(|| bad("Continuation authority missing"))?;
    }
    let mut queries = 0;
    for call in messages
        .iter()
        .flat_map(|m| m["tool_calls"].as_array().into_iter().flatten())
    {
        let Some(id) = call["id"].as_str() else {
            continue;
        };
        for turn in &turns {
            control.check().map_err(|e| bad(&e.to_string()))?;
            queries += 1;
            if queries > 8192 {
                return Err(bad("Continuation authority lookup exceeds its bounded budget; inspect the checkpoint before a new assignment"));
            }
            let Some(effect) = store.effect_get(turn, &format!("tool:{id}"))? else {
                continue;
            };
            let receipt = effect
                .receipt
                .ok_or_else(|| bad("Prior tool outcome remains unverified"))?;
            if let Some(identity) = receipt.get("_mutation_binding") {
                if !identity.is_null() {
                    let identity: kageha_contracts::execution::ToolBinding =
                        serde_json::from_value(identity.clone())
                            .map_err(|_| bad("Prior mutation identity is invalid"))?;
                    guard.confirmed.insert(hash(&json!([
                        call["name"],
                        call["arguments"],
                        identity
                    ])));
                }
            } else if let Some(binding) = receipt.get("_binding") {
                let binding: kageha_contracts::execution::ToolBinding =
                    serde_json::from_value(binding.clone())
                        .map_err(|_| bad("Prior tool authority is invalid"))?;
                if binding.class == kageha_contracts::policy::ActionClass::Mutation {
                    guard.confirmed.insert(hash(&json!([
                        call["name"],
                        call["arguments"],
                        binding
                    ])));
                }
            } else {
                guard
                    .legacy
                    .insert(hash(&json!([call["name"], call["arguments"]])));
            }
            break;
        }
    }
    Ok(guard)
}

#[cfg(test)]
mod mutation_tests {
    use super::*;
    use kageha_contracts::{
        execution::{ToolBinding, ToolCall},
        policy::ActionClass,
    };
    #[test]
    fn repeated_mutations_are_fenced_without_blocking_other_profiles_or_generations() {
        let call = ToolCall {
            id: "fresh-id".into(),
            name: "click".into(),
            arguments: json!({"target":"submit"}),
        };
        let binding = ToolBinding {
            binding: "browser.workspace".into(),
            generation: 1,
            resource: "profile-a/tab-1".into(),
            class: ActionClass::Mutation,
            detail: "Click submit".into(),
            fingerprint: "profile-a".into(),
        };
        let mut guard = MutationGuard::default();
        guard
            .confirmed
            .insert(hash(&json!([call.name, call.arguments, binding])));
        assert!(guard.check(&call, &binding).is_err());
        let mut other = binding.clone();
        other.resource = "profile-b/tab-1".into();
        assert!(guard.check(&call, &other).is_ok());
        other = binding.clone();
        other.generation = 2;
        assert!(guard.check(&call, &other).is_ok());
        other = binding.clone();
        other.class = ActionClass::ReadOnly;
        assert!(guard.check(&call, &other).is_ok());
    }
}
