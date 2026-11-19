//! A local, read-only chat host that does not require the Python application.
use crate::turn::Start;
use kageha_runtime::execution::{
    execute_turn, Control, DispatchAccess, ModelPort, ModelRequest, ToolCall, ToolPort, ToolReply,
};
use serde_json::{json, Value};
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

pub(crate) trait ToolHost: Sync {
    fn persistence_policy(&self)->io::Result<Option<kageha_contracts::execution::PersistencePolicy>> {Ok(None)}
    fn recovery_checkpoint(&self)->io::Result<Value> {Ok(Value::Null)}
    fn save_recovery(&self,_state:&Value)->io::Result<()> {Ok(())}
    fn objective_revision(&self)->io::Result<Option<String>> {Ok(None)}
    fn progress_inputs(&self)->io::Result<kageha_contracts::execution::ProgressInputs> {Ok(Default::default())}
    /// Writes the progress note for a context compaction (Requirement 11.3).
    fn on_compaction(&self,_summary:&kageha_contracts::execution::CompactionSummary)->io::Result<()> {Ok(())}
    /// The agent's `todo.md`, restated by the runtime when it changes (Requirement 11.6).
    fn plan_snapshot(&self)->io::Result<Option<String>> {Ok(None)}
    /// User messages appended after the batch's tool results (`ToolPort::take_attachments`).
    fn take_attachments(&self)->io::Result<Vec<Value>> {Ok(Vec::new())}
    /// Whether the Host dispatches `name` although it is not in the turn's tool list
    /// (`ToolPort::accepts_unlisted`). `Some(false)` makes the call an `invalid_tool_call`.
    fn accepts_unlisted(&self,_name:&str)->Option<bool> {None}
    fn plan_context(&self) -> io::Result<Option<Value>> { Ok(None) }
    fn completion_check(&self, _candidate:&str, _control:&Control<'_>) -> io::Result<kageha_contracts::execution::CompletionCheck> {
        Ok(kageha_contracts::execution::CompletionCheck::default())
    }
    fn model_activity(&self) -> Option<kageha_models::ModelActivityObserver> {
        None
    }
    fn browser_decision(&self,_request:&ModelRequest,_control:&Control<'_>)->io::Result<Option<kageha_contracts::execution::AssistantMessage>>{Ok(None)}
    fn browser_decision_failure(&self, _call: &ToolCall, _error: &io::Error) {}
    fn workflow(&self)->Option<&dyn kageha_workflow::Port>{None}
    fn prepare_browser_inputs(&self, _calls: &[ToolCall]) {}
    fn form_input_held(&self, _call: &ToolCall) -> bool { false }
    fn before_tool(&self,_call:&ToolCall,_control:&Control<'_>)->io::Result<()>{Ok(())}
    fn recovery_revision(&self,_call:&ToolCall)->io::Result<Option<String>> {Ok(None)}
    fn recovery_context(&self,_call:&ToolCall)->io::Result<Option<kageha_contracts::execution::RecoveryContext>> {Ok(None)}
    /// How the read cache handles this call (Requirement 12). Asked after `before_tool`;
    /// a hit is returned without calling `invoke`.
    fn read_cache(&self,_call:&ToolCall)->crate::ui_read_cache::Plan {crate::ui_read_cache::Plan::Dispatch}
    fn tool_outcome(&self,_call:&ToolCall,_result:&io::Result<String>)->Option<kageha_contracts::execution::ToolOutcome> {None}
    /// Whether a transient failure of this call is retried by the Host before it returns
    /// (Requirement 9.3, `ui_tool_errors::retry_read`). Only replay-safe reads; never actions.
    fn retries_transient(&self,_call:&ToolCall)->bool {false}
    /// Waits between transient-read retries. Tests override it so no real time passes.
    fn retry_pause(&self,delay:Duration,control:&Control<'_>)->io::Result<()> {crate::ui_tool_errors::sleep(delay,control)}
    /// The model-facing copy of a result larger than the context bound, or `None` to keep it
    /// as is. Called after evidence and the outcome were computed from the full result.
    fn spill_output(&self,_call:&ToolCall,_content:&str)->Option<String> {None}
    /// The learned fix `{steps, successes, last_seen}` for a failed result (Requirements 9.2
    /// and 17.2, `ui_learning::known_fix`), or `None`. Never fails the call: a lookup error
    /// is `None`.
    fn known_fix(&self,_call:&ToolCall,_content:&str,_is_error:bool)->Option<Value> {None}

    fn runtime_scope(&self) -> io::Result<Option<kageha_runtime::lifecycle::RunSpec>> { Ok(None) }
    fn dispatch_access(&self, _call: &ToolCall) -> io::Result<DispatchAccess> {
        Ok(DispatchAccess::Exclusive)
    }
    fn specs(&self) -> Vec<Value>;
    fn invoke(&self, call: &Value) -> io::Result<String>;
    fn providers(&self) -> Vec<crate::ui_settings::Provider> {
        Vec::new()
    }
    /// Planner and reviewer providers; an empty role uses `providers`.
    fn role_providers(&self) -> crate::model_router::RoleProviders {
        Default::default()
    }
    fn steering(&self) -> io::Result<Vec<String>> {
        Ok(Vec::new())
    }
}

pub(crate) fn read_file(workspace:&Path,path:&str)->io::Result<String>{
 kageha_workspace::ReadWorkspace::new(workspace)?.read_text(path,&Control{cancelled:&AtomicBool::new(false),deadline:None}).map_err(|error|{
  if error.kind()==io::ErrorKind::PermissionDenied {io::Error::other("expected a file inside the workspace")}
  else if error.to_string().contains("read limit"){io::Error::other("file exceeds the 64 KiB read limit")}
  else{error}
 })
}

pub fn run(workspace: &Path, mut input: impl Read, mut output: impl Write) -> io::Result<()> {
    let mut prompt = String::new();
    input.by_ref().take(262_145).read_to_string(&mut prompt)?;
    if prompt.trim().is_empty() || prompt.len() > 262_144 {
        return Err(io::Error::other(
            "expected a prompt of 1 to 262144 bytes on stdin",
        ));
    }
    let answer = converse(workspace, vec![json!({"role":"user","content":prompt})])?;
    writeln!(output, "{answer}")
}

pub(crate) fn converse(workspace: &Path, messages: Vec<Value>) -> io::Result<String> {
    converse_controlled(workspace, messages, &AtomicBool::new(false), |_| Ok(()))
}

pub(crate) fn converse_controlled(
    workspace: &Path,
    messages: Vec<Value>,
    cancel: &AtomicBool,
    observe: impl FnMut(&Value) -> io::Result<()>,
) -> io::Result<String> {
    converse_budgeted(workspace, messages, cancel, 8, observe)
}

pub(crate) fn converse_budgeted(
    workspace: &Path,
    messages: Vec<Value>,
    cancel: &AtomicBool,
    max_model_calls: u32,
    observe: impl FnMut(&Value) -> io::Result<()>,
) -> io::Result<String> {
    converse_hosted(workspace, messages, cancel, max_model_calls, observe, None)
}

pub(crate) fn converse_hosted(
    workspace: &Path,
    messages: Vec<Value>,
    cancel: &AtomicBool,
    max_model_calls: u32,
    mut observe: impl FnMut(&Value) -> io::Result<()>,
    host: Option<&dyn ToolHost>,
) -> io::Result<String> {
    let workspace = workspace.canonicalize()?;
    if !workspace.is_dir() {
        return Err(io::Error::other("Expected a workspace directory"));
    }
    let mut providers = host.map(|h| h.providers()).unwrap_or_default();
    if providers.is_empty() {
        providers.push(crate::ui_settings::Provider {
            url: std::env::var("KAGEHA_PROVIDER_URL")
                .map_err(|_| io::Error::other("Set KAGEHA_PROVIDER_URL"))?,
            model: std::env::var("KAGEHA_MODEL")
                .map_err(|_| io::Error::other("Set KAGEHA_MODEL"))?,
            key: std::env::var("KAGEHA_API_KEY").unwrap_or_default(),
            protocol: "openai_compat".into(),
            source: None,
            context_window: None,
        });
    }
    let activity = host.and_then(ToolHost::model_activity);
    let model = crate::ui_credentials::model(providers)?.with_activity(activity.clone());
    // Model_Router: requests go to their role's model (design section 10).
    let roles = host.map(ToolHost::role_providers).unwrap_or_default();
    let role_model = |providers: Vec<crate::ui_settings::Provider>| -> io::Result<Option<kageha_models::HttpModel>> {
        if providers.is_empty() {
            return Ok(None);
        }
        Ok(Some(crate::ui_credentials::model(providers)?.with_activity(activity.clone())))
    };
    let (planner, reviewer) = (role_model(roles.planner)?, role_model(roles.reviewer)?);
    let by_role = crate::model_router::RoutedModel {
        executor: &model,
        planner: planner.as_ref().map(|m| m as &(dyn ModelPort + Sync)),
        reviewer: reviewer.as_ref().map(|m| m as &(dyn ModelPort + Sync)),
        images_to_planner: roles.images_to_planner,
        images_to_executor: roles.images_to_executor,
    };
    let routed=BrowserModel{model:&by_role,host};
    let guarded = host.and_then(|h|h.workflow()).map(|workflow|kageha_workflow::GuardedModel{model:&routed,workflow});
    let selected_model:&(dyn ModelPort+Sync)=guarded.as_ref().map(|v|v as &(dyn ModelPort+Sync)).unwrap_or(&routed);
    let tools = HostedTools {
        workspace: &workspace,
        host,
    };
    let start=Start { version:1,max_model_calls,messages };
    if let Some(client)=kageha_runtime_service::bridge_client::Client::from_env()? {
        let scope = match host {
            Some(host) => host.runtime_scope()?.ok_or_else(||io::Error::other("Standalone runtime requires a persisted run scope"))?,
            None => {
                // The read-only CLI has no WebUI session; the daemon owns its journal.
                let id = uuid::Uuid::new_v4().to_string();
                kageha_runtime::lifecycle::RunSpec { id:id.clone(), session:id.clone(), turn:id.clone(), request:id,
                    parent:None, parent_run:None, agent:"read-only-cli".into(), objective:"Read-only workspace conversation".into(), depth:0, input:Value::Null }
            }
        };
        return client.execute(scope,start,selected_model,&tools,&Control{cancelled:cancel,deadline:None},&mut observe);
    }
    execute_turn(
        start,
        selected_model,
        &tools,
        &Control {
            cancelled: cancel,
            deadline: None,
        },
        &mut observe,
    )
}

/// [`converse_hosted`] with an injected model instead of the configured providers, for
/// in-crate integration tests that drive the real Host tools and Completion_Gate with a
/// scripted `ModelPort`. The wiring (browser routing, workflow guard, `HostedTools`,
/// `execute_turn`) is the same as the local execution path.
#[cfg(test)]
pub(crate) fn execute_hosted_with_model(
    workspace: &Path,
    start: Start,
    model: &(dyn ModelPort + Sync),
    host: &dyn ToolHost,
    cancel: &AtomicBool,
    observe: impl FnMut(&Value) -> io::Result<()>,
) -> io::Result<String> {
    let workspace = workspace.canonicalize()?;
    let routed = BrowserModel { model, host: Some(host) };
    let guarded = host.workflow().map(|workflow| kageha_workflow::GuardedModel { model: &routed, workflow });
    let selected_model: &(dyn ModelPort + Sync) = guarded.as_ref().map(|v| v as &(dyn ModelPort + Sync)).unwrap_or(&routed);
    let tools = HostedTools { workspace: &workspace, host: Some(host) };
    execute_turn(start, selected_model, &tools, &Control { cancelled: cancel, deadline: None }, observe)
}

/// Dispatches one call through the same Host path as a turn's tool batch (`before_tool`,
/// read cache, transient-read retry, outcome, spill). Used to re-dispatch an in-flight read
/// after a restart (`ui_turn_resume`).
pub(crate) fn dispatch_one(workspace: &Path, host: &dyn ToolHost, call: &ToolCall, cancel: &AtomicBool) -> io::Result<ToolReply> {
    HostedTools { workspace, host: Some(host) }.execute(call, &Control { cancelled: cancel, deadline: None })
}

// Browser routing stays outside the generic runtime and uses the same ToolPort.
struct BrowserModel<'a>{model:&'a (dyn ModelPort+Sync),host:Option<&'a dyn ToolHost>}
impl ModelPort for BrowserModel<'_>{
    fn accounts_invocations(&self)->bool {true}
    fn context_window(&self)->Option<u64> {self.model.context_window()}
    fn complete(&self,mut request:ModelRequest,control:&Control<'_>)->io::Result<kageha_contracts::execution::AssistantMessage>{
        control.check()?;
        if !request.tools.is_empty() {
            if let Some(host) = self.host {
                if let Some(reply) = host.browser_decision(&request, control)? {
                    host.prepare_browser_inputs(&reply.tool_calls);
                    return Ok(reply);
                }
                // The task_plan_* definitions stay as built for the turn (Requirement
                // 11.4): current step and evidence IDs are given in this message, not
                // as per-step enums in the schemas. The Host validates the IDs.
                if request.tools.iter().any(|tool| tool["function"]["name"] == "task_plan_update" || tool["function"]["name"] == "task_plan_revise") {
                    if let Some(context) = host.plan_context()? {
                        request.messages.insert(0,json!({"role":"system","content":format!(
                            "Current approved plan and confirmed tool evidence. Copy exact step IDs, revision and evidence references when updating the plan. Send outcome_ids only for listed outcomes, and only on completion: {context}"
                        )}));
                    }
                }
            }
        }
        control.check()?;
        let reply = self.model.complete(request, control)?;
        if let Some(host) = self.host { host.prepare_browser_inputs(&reply.tool_calls); }
        Ok(reply)
    }
}

struct HostedTools<'a> {
    workspace: &'a Path,
    host: Option<&'a dyn ToolHost>,
}
impl ToolPort for HostedTools<'_> {
    fn persistence_policy(&self)->io::Result<Option<kageha_contracts::execution::PersistencePolicy>> {self.host.map(|h|h.persistence_policy()).unwrap_or(Ok(None))}
    fn recovery_checkpoint(&self)->io::Result<Value> {self.host.map(|h|h.recovery_checkpoint()).unwrap_or(Ok(Value::Null))}
    fn save_recovery(&self,state:&Value)->io::Result<()> {self.host.map(|h|h.save_recovery(state)).unwrap_or(Ok(()))}
    fn objective_revision(&self)->io::Result<Option<String>> {self.host.map(|h|h.objective_revision()).unwrap_or(Ok(None))}
    fn progress_inputs(&self)->io::Result<kageha_contracts::execution::ProgressInputs> {self.host.map(|h|h.progress_inputs()).unwrap_or_else(||Ok(Default::default()))}
    fn on_compaction(&self,summary:&kageha_contracts::execution::CompactionSummary)->io::Result<()> {self.host.map_or(Ok(()),|h|h.on_compaction(summary))}
    fn plan_snapshot(&self)->io::Result<Option<String>> {self.host.map_or(Ok(None),|h|h.plan_snapshot())}
    fn take_attachments(&self)->io::Result<Vec<Value>> {self.host.map_or(Ok(Vec::new()),|h|h.take_attachments())}
    fn accepts_unlisted(&self,name:&str)->Option<bool> {self.host.and_then(|h|h.accepts_unlisted(name))}
    fn execute_batch(
        &self,
        calls: &[ToolCall],
        control: &Control<'_>,
    ) -> io::Result<Vec<ToolReply>> {
        if !calls.is_empty()&&calls.iter().all(|c|c.id.starts_with("browser-decision-")&&matches!(c.name.as_str(),"browser_fill"|"browser_check"|"browser_select"|"browser_click"|"browser_press")) {
            let mut stopped=false;let mut replies=Vec::new();
            for call in calls {
                control.check()?;
                if stopped {replies.push(ToolReply::failure(json!({"error_code":"decision_batch_stopped","retryable":false,
                    "summary":"decision_batch_stopped: an earlier input in this batch failed. Inspect fresh state before further input."})));continue;}
                let reply=match self.execute(call,control){Ok(reply)=>reply,Err(error) if kageha_contracts::policy::is_stop(&error)||kageha_contracts::execution::is_uncertain(&error)=>return Err(error),Err(error)=>ToolReply::failure(crate::ui_tool_errors::normalize_call(call,error))};
                stopped=reply.is_error && !self.host.is_some_and(|host| host.form_input_held(call));
                replies.push(reply);
            }
            return Ok(replies);
        }
        kageha_runtime::fanout::execute_parallel(self, calls, control, 4)
    }
    fn recovery_context(&self,call:&ToolCall)->io::Result<Option<kageha_contracts::execution::RecoveryContext>> {
        self.host.map_or(Ok(None),|host|host.recovery_context(call))
    }
    fn recovery_revision(&self, call:&ToolCall)->io::Result<Option<String>> {
        self.host.map_or(Ok(None),|host|host.recovery_revision(call))
    }
    fn dispatch_access(&self, call: &ToolCall) -> io::Result<DispatchAccess> {
        self.host
            .map(|host| host.dispatch_access(call))
            .unwrap_or_else(|| Ok(DispatchAccess::Resource(String::new())))
    }

    fn discover(&self) -> io::Result<Vec<Value>> {
        Ok(self.host.map(|h|h.specs()).unwrap_or_else(||vec![json!({"type":"function","function":{"name":"read_file","description":"Read a UTF-8 file inside the bound workspace","parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}}})]))
    }
    fn completion_check(&self, candidate:&str, control:&Control<'_>) -> io::Result<kageha_contracts::execution::CompletionCheck> {
        self.host.map_or_else(|| Ok(kageha_contracts::execution::CompletionCheck::default()), |host| host.completion_check(candidate, control))
    }
    fn checkpoint(&self) -> io::Result<Vec<String>> {
        self.host
            .map(|h| h.steering())
            .unwrap_or_else(|| Ok(Vec::new()))
    }
    fn execute(&self, call: &ToolCall, control: &Control<'_>) -> io::Result<ToolReply> {
        control.check()?;
        let result = if let Some(host) = self.host {
            match host.before_tool(call,control) {
                Ok(())=>{
                    // Read cache (design section 11): a hit still gets its result and tool
                    // card, marked cached, but is not dispatched.
                    let cache=host.read_cache(call);
                    match cache.hit() {
                        Some(hit)=>Ok(hit),
                        None=>{
                            let dispatch=cache.dispatch_call(&serde_json::to_value(call)?);
                            // Transient read failures are retried here, so the runtime's
                            // RetryRead never sees them (see ui_tool_errors).
                            let result=if host.retries_transient(call) {
                                crate::ui_tool_errors::retry_read(control,&|delay,control|host.retry_pause(delay,control),
                                    &crate::ui_tool_errors::random_jitter,||host.invoke(&dispatch))
                            } else {host.invoke(&dispatch)};
                            cache.finish(&result);
                            result
                        }
                    }
                }
                Err(error)=>Err(error),
            }
        } else {
            match (call.name.as_str(), call.arguments["path"].as_str()) {
                ("read_file", Some(path)) => read_file(self.workspace, path),
                _ => Err(io::Error::other("Unsupported tool or missing path")),
            }
        };
        if self.host.is_none() {
            return Ok(match result {
                Ok(content) => ToolReply::success(content),
                Err(e) => ToolReply {
                    content: format!("Read failed: {e}"),
                    is_error: true,
                    outcome: None,
                },
            });
        }
        if let Err(error) = &result {
            if let Some(host) = self.host {
                host.browser_decision_failure(call, error);
            }
            if kageha_contracts::policy::is_stop(error) || kageha_contracts::execution::is_uncertain(error)
                || kageha_contracts::request_budget::is_exceeded(error)
                || matches!(error.kind(),io::ErrorKind::Interrupted|io::ErrorKind::TimedOut) {
                return Err(result.unwrap_err());
            }
        }
        // Every failure reaching the model uses the structured envelope (Requirement 9.1).
        let result=result.map_err(|error|crate::ui_tool_errors::normalize_call(call,error));
        let outcome=self.host.and_then(|host|host.tool_outcome(call,&result));
        let mut reply=match result {Ok(content)=>ToolReply::success(content),Err(error)=>ToolReply::failure(error)};
        reply.outcome=outcome;
        // A failure with a learned fix carries it (Requirement 9.2); the spill excerpt keeps it.
        if let Some(fix)=self.host.and_then(|host|host.known_fix(call,&reply.content,reply.is_error)) {
            reply.content=crate::ui_learning::attach_known_fix(&reply.content,&fix);
        }
        // Spill at the source: only the copy sent to the model is shortened.
        if reply.content.len()>crate::ui_tool_spill::SPILL_BYTES {
            if let Some(excerpt)=self.host.and_then(|host|host.spill_output(call,&reply.content)) {
                reply.content=excerpt;
            }
        }
        Ok(reply)
    }
}

pub(crate) fn complete_timed(
    config: &crate::ui_settings::Provider,
    messages: Vec<Value>,
    cancel: &AtomicBool,
    timeout_s: f64,
) -> io::Result<Value> {
    if !timeout_s.is_finite() {
        return Err(io::Error::other("Invalid model timeout"));
    }
    let model = crate::ui_credentials::model(vec![config.clone()])?
        .with_timeout(Duration::from_secs_f64(timeout_s.clamp(0.1, 120.0)))?;
    model
        .complete(
            ModelRequest {
                messages,
                tools: vec![],
                role: Default::default(),
            },
            &Control {
                cancelled: cancel,
                deadline: None,
            },
        )
        .map(|reply| reply.legacy())
}

pub(crate) fn complete_with(
    config: &crate::ui_settings::Provider,
    messages: Vec<Value>,
    cancel: &AtomicBool,
    tools: Vec<Value>,
) -> io::Result<Value> {
    crate::ui_credentials::model(vec![config.clone()])?
        .complete(
            ModelRequest { messages, tools, role: Default::default() },
            &Control {
                cancelled: cancel,
                deadline: None,
            },
        )
        .map(|reply| reply.legacy())
}

#[cfg(test)]
mod browser_model_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize,Ordering};
    struct Main(AtomicUsize);
    impl ModelPort for Main {fn complete(&self,_r:ModelRequest,_c:&Control<'_>)->io::Result<kageha_contracts::execution::AssistantMessage>{self.0.fetch_add(1,Ordering::SeqCst);Ok(kageha_contracts::execution::AssistantMessage{ continuation: None, content:json!("main"),tool_calls:vec![]})}}
    struct Browser(bool);
    impl ToolHost for Browser {
        fn browser_decision(&self,_r:&ModelRequest,_c:&Control<'_>)->io::Result<Option<kageha_contracts::execution::AssistantMessage>>{Ok(self.0.then(||kageha_contracts::execution::AssistantMessage{ continuation: None, content:Value::Null,tool_calls:vec![ToolCall{id:"decision".into(),name:"browser_click".into(),arguments:json!({"target":"e1","observation_id":"obs"})}]}))}
        fn specs(&self)->Vec<Value>{vec![]}
        fn invoke(&self,_call:&Value)->io::Result<String>{unreachable!()}
    }
    #[test]
    fn browser_decisions_respect_the_advertised_tools() {
        let flag = AtomicBool::new(false);
        let control = Control { cancelled: &flag, deadline: None };
        for (confident, has_tools, expect_browser) in [
            (false, true, false), (true, true, true), (true, false, false),
        ] {
            let main = Main(AtomicUsize::new(0));
            let host = Browser(confident);
            let routed = BrowserModel { model: &main, host: Some(&host) };
            let tools = if has_tools {
                vec![json!({"function":{"name":"browser_click"}})]
            } else {
                vec![]
            };
            let reply = routed.complete(ModelRequest { messages: vec![], tools, role: Default::default() }, &control).unwrap();
            assert_eq!(main.0.load(Ordering::SeqCst), usize::from(!expect_browser));
            assert_eq!(!reply.tool_calls.is_empty(), expect_browser);
        }
    }
}

#[cfg(test)]
mod decision_batch_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize,Ordering};
    struct Host(AtomicUsize);
    impl ToolHost for Host {
        fn specs(&self)->Vec<Value>{vec![]}
        fn invoke(&self,_call:&Value)->io::Result<String>{let index=self.0.fetch_add(1,Ordering::SeqCst);if index==1{Err(io::Error::other("observed control changed"))}else{Ok("verified".into())}}
    }
    #[test] fn typed_batch_stops_after_first_error_without_replaying_or_dispatching_tail(){
        let root=std::env::temp_dir();let host=Host(AtomicUsize::new(0));let tools=HostedTools{workspace:&root,host:Some(&host)};
        let calls=(0..3).map(|i|ToolCall{id:format!("browser-decision-{i}"),name:"browser_fill".into(),arguments:json!({"target":format!("e{i}"),"observation_id":"obs","text":"known"})}).collect::<Vec<_>>();
        let flag=AtomicBool::new(false);let result=tools.execute_batch(&calls,&Control{cancelled:&flag,deadline:None}).unwrap();
        assert_eq!(host.0.load(Ordering::SeqCst),2);assert_eq!(result.len(),3);assert!(!result[0].is_error);assert!(result[1].is_error);assert!(result[2].content.contains("decision_batch_stopped"));
    }
}

#[cfg(test)]
mod read_cache_tests {
    use super::*;
    use crate::ui_read_cache::{Lookup, Plan};
    use std::sync::Mutex;
    /// A host whose `web_search` is a cacheable read and whose `write_file` is not.
    struct Host { db: crate::webui::Database, dispatched: Mutex<Vec<Value>> }
    impl ToolHost for Host {
        fn specs(&self)->Vec<Value>{vec![]}
        fn read_cache(&self,call:&ToolCall)->Plan {
            if call.name=="web_search" {Plan::Read(Lookup::new(&self.db,"request","web_search",&call.arguments,""))} else {Plan::Dispatch}
        }
        fn invoke(&self,call:&Value)->io::Result<String>{
            let mut dispatched=self.dispatched.lock().unwrap();dispatched.push(call["arguments"].clone());
            Ok(json!({"results":[call["arguments"]["query"]],"n":dispatched.len()}).to_string())
        }
    }
    #[test]
    fn hits_are_returned_marked_without_dispatch_and_refresh_dispatches_without_the_flag() {
        let dir=tempfile::tempdir().unwrap();
        let connection=rusqlite::Connection::open(dir.path().join("native.sqlite")).unwrap();
        crate::ui_read_cache::initialize(&connection).unwrap();
        let host=Host{db:crate::agent_host::DatabaseState::open(connection).unwrap(),dispatched:Mutex::default()};
        let root=dir.path().to_path_buf();
        let tools=HostedTools{workspace:&root,host:Some(&host)};
        let flag=AtomicBool::new(false);let control=Control{cancelled:&flag,deadline:None};
        let run=|name:&str,arguments:Value|tools.execute(&ToolCall{id:"c".into(),name:name.into(),arguments},&control).unwrap();
        let first=run("web_search",json!({"query":"seequent"}));
        let hit=run("web_search",json!({"query":"seequent"}));
        assert!(!hit.is_error);
        let hit:Value=serde_json::from_str(&hit.content).unwrap();
        assert_eq!((hit["cached"].clone(),hit["n"].clone()),(json!(true),json!(1)),"{hit}");
        assert!(serde_json::from_str::<Value>(&first.content).unwrap()["cached"].is_null());
        assert_eq!(host.dispatched.lock().unwrap().len(),1);
        let refreshed:Value=serde_json::from_str(&run("web_search",json!({"query":"seequent","refresh":true})).content).unwrap();
        assert_eq!((refreshed["cached"].clone(),refreshed["n"].clone()),(Value::Null,json!(2)));
        assert_eq!(host.dispatched.lock().unwrap()[1],json!({"query":"seequent"}),"refresh is not sent to the tool");
        // The refreshed result replaced the entry.
        assert_eq!(serde_json::from_str::<Value>(&run("web_search",json!({"query":"seequent"})).content).unwrap()["n"],2);
        // Calls the host does not plan as reads are always dispatched.
        run("write_file",json!({"path":"a"}));run("write_file",json!({"path":"a"}));
        assert_eq!(host.dispatched.lock().unwrap().len(),4);
    }
}

#[cfg(test)]
mod spill_tests {
    use super::*;
    use std::sync::Mutex;
    /// Records what evidence and outcome classification saw, and spills like the WebUI host.
    struct Host { workspace: kageha_workspace::task::TaskWorkspace, output: String, evidence: Mutex<Vec<String>>, outcomes: Mutex<Vec<String>> }
    impl ToolHost for Host {
        fn specs(&self)->Vec<Value>{vec![]}
        fn invoke(&self,_call:&Value)->io::Result<String>{self.evidence.lock().unwrap().push(self.output.clone());Ok(self.output.clone())}
        fn tool_outcome(&self,_call:&ToolCall,result:&io::Result<String>)->Option<kageha_contracts::execution::ToolOutcome>{
            self.outcomes.lock().unwrap().push(result.as_ref().unwrap().clone());None
        }
        fn spill_output(&self,call:&ToolCall,content:&str)->Option<String>{crate::ui_tool_spill::spill(&self.workspace,&call.id,content).unwrap()}
    }
    fn run(output: String) -> (tempfile::TempDir, Host, ToolReply) {
        let dir = tempfile::tempdir().unwrap();
        let workspace = kageha_workspace::task::TaskWorkspace::open(&dir.path().join("chat")).unwrap();
        let host = Host { workspace, output, evidence: Mutex::default(), outcomes: Mutex::default() };
        let root = dir.path().to_path_buf();
        let reply = HostedTools { workspace: &root, host: Some(&host) }
            .execute(&ToolCall { id: "call-9".into(), name: "remote_exec".into(), arguments: json!({}) },
                &Control { cancelled: &AtomicBool::new(false), deadline: None })
            .unwrap();
        (dir, host, reply)
    }
    #[test]
    fn large_results_reach_the_model_as_an_excerpt_and_evidence_sees_them_in_full() {
        let full = json!({"ok":true,"exit_code":0,"stdout":"out\n".repeat(20_000)}).to_string();
        let (_dir, host, reply) = run(full.clone());
        assert!(reply.content.len() <= crate::ui_tool_spill::SPILL_BYTES);
        let excerpt: Value = serde_json::from_str(&reply.content).unwrap();
        assert_eq!(excerpt["spilled_path"], ".kageha/tool-output/call-9.txt");
        assert_eq!(excerpt["original_bytes"], full.len());
        assert_eq!(std::fs::read_to_string(host.workspace.root().join(".kageha/tool-output/call-9.txt")).unwrap(), full);
        assert_eq!(*host.evidence.lock().unwrap(), [full.clone()]);
        assert_eq!(*host.outcomes.lock().unwrap(), [full]);
    }
    #[test]
    fn results_within_the_bound_reach_the_model_unchanged() {
        let full = "y".repeat(crate::ui_tool_spill::SPILL_BYTES);
        let (_dir, host, reply) = run(full.clone());
        assert_eq!(reply.content, full);
        assert!(std::fs::read_dir(host.workspace.root().join(".kageha/tool-output")).unwrap().next().is_none());
    }
}

#[cfg(test)]
mod tool_error_tests {
    //! Requirements 9.1 and 9.3 through the real `HostedTools::execute` path.
    use super::*;
    use std::sync::Mutex;
    /// Replies with queued results. `read_*` tools are retryable reads, others are actions.
    /// Pauses are recorded instead of slept, and can cancel the turn.
    struct Host { results: Mutex<Vec<io::Result<String>>>, calls: Mutex<usize>, pauses: Mutex<Vec<Duration>>, cancel: Option<&'static AtomicBool> }
    impl ToolHost for Host {
        fn specs(&self)->Vec<Value>{vec![]}
        fn invoke(&self,_call:&Value)->io::Result<String>{*self.calls.lock().unwrap()+=1;self.results.lock().unwrap().remove(0)}
        fn retries_transient(&self,call:&ToolCall)->bool{call.name.starts_with("read_")}
        fn retry_pause(&self,delay:Duration,_control:&Control<'_>)->io::Result<()>{
            self.pauses.lock().unwrap().push(delay);
            if let Some(flag)=self.cancel {flag.store(true,std::sync::atomic::Ordering::Release);}
            Ok(())
        }
    }
    fn host(results: Vec<io::Result<String>>) -> Host {
        Host { results: Mutex::new(results), calls: Mutex::default(), pauses: Mutex::default(), cancel: None }
    }
    fn execute(host: &Host, name: &str, cancel: &AtomicBool) -> io::Result<ToolReply> {
        let root = std::env::temp_dir();
        HostedTools { workspace: &root, host: Some(host) }
            .execute(&ToolCall { id: "c1".into(), name: name.into(), arguments: json!({}) }, &Control { cancelled: cancel, deadline: None })
    }
    fn envelope(reply: &ToolReply) -> Value {
        assert!(reply.is_error);
        serde_json::from_str(reply.content.strip_prefix("Tool failed: ").expect("failure prefix")).unwrap()
    }
    fn err(text: &str) -> io::Result<String> { Err(io::Error::other(text.to_owned())) }

    #[test]
    fn plain_string_failures_reach_the_model_as_the_envelope_with_an_excerpt() {
        let host = host(vec![err("Unsupported thing\nstderr: boom")]);
        let value = envelope(&execute(&host, "act_send", &AtomicBool::new(false)).unwrap());
        assert_eq!((value["error_code"].as_str(), value["retryable"].as_bool()), (Some("tool_failed"), Some(false)));
        assert_eq!(value["summary"], "act_send failed: Unsupported thing");
        assert_eq!(value["excerpt"], "Unsupported thing\nstderr: boom");
    }

    #[test]
    fn structured_failures_are_not_wrapped_twice() {
        let body = json!({"error_code":"edit_not_found","retryable":false,"summary":"Read the file again."}).to_string();
        let host = host(vec![Err(io::Error::other(body.clone()))]);
        let reply = execute(&host, "act_edit", &AtomicBool::new(false)).unwrap();
        let mut value = envelope(&reply);
        assert_eq!(value["tool_call_id"], "c1");
        value.as_object_mut().unwrap().remove("tool_call_id");
        assert_eq!(value, serde_json::from_str::<Value>(&body).unwrap());
    }

    #[test]
    fn a_read_failing_with_503_twice_then_succeeding_returns_success_after_two_retries() {
        let host = host(vec![err("Provider returned HTTP 503"), err("Provider returned HTTP 503"), Ok("fresh".into())]);
        let reply = execute(&host, "read_source", &AtomicBool::new(false)).unwrap();
        assert_eq!((reply.is_error, reply.content.as_str(), *host.calls.lock().unwrap()), (false, "fresh", 3));
        let pauses = host.pauses.lock().unwrap().clone();
        assert_eq!(pauses.len(), 2);
        for (pause, base) in pauses.iter().zip(crate::ui_tool_errors::BACKOFF) {
            assert!(*pause >= base && *pause <= base + base / 4, "{pause:?} for {base:?}");
        }
    }

    #[test]
    fn four_rate_limits_return_transient_upstream() {
        let host = host((0..4).map(|_| err("brave search returned HTTP 429")).collect());
        let value = envelope(&execute(&host, "read_search", &AtomicBool::new(false)).unwrap());
        assert_eq!((value["error_code"].as_str(), value["attempts"].as_u64()), (Some("transient_upstream"), Some(4)));
        assert_eq!(value["excerpt"], "brave search returned HTTP 429");
        assert_eq!((*host.calls.lock().unwrap(), host.pauses.lock().unwrap().len()), (4, 3));
    }

    #[test]
    fn actions_are_never_retried() {
        let host = host(vec![err("Provider returned HTTP 503"), Ok("second dispatch".into())]);
        let value = envelope(&execute(&host, "act_submit", &AtomicBool::new(false)).unwrap());
        assert_eq!((value["error_code"].as_str(), value["retryable"].as_bool()), (Some("tool_failed"), Some(false)));
        assert_eq!((*host.calls.lock().unwrap(), host.pauses.lock().unwrap().len()), (1, 0));
    }

    #[test]
    fn cancellation_stops_retrying() {
        static CANCEL: AtomicBool = AtomicBool::new(false);
        let mut host = host(vec![err("HTTP 503"), Ok("never".into())]);
        host.cancel = Some(&CANCEL);
        let error = execute(&host, "read_source", &CANCEL).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert_eq!((*host.calls.lock().unwrap(), host.pauses.lock().unwrap().len()), (1, 1));
    }
}
