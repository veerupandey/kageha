//! Standalone service composition over the same execution/lifecycle ports used by Kageha.
pub mod activation;
mod skill_tools;
pub mod http;
pub mod planning;
#[derive(Default)]
pub struct Components {pub workers:Option<Arc<workers::Composition>>,pub hooks:Option<Arc<hooks::Lifecycle>>,pub planning:Option<Arc<planning::Coordinator>>}
pub mod hooks;
pub mod workers;
mod continuation;
pub mod bridge;
pub mod bridge_client;
use kageha_contracts::{policy::*, ErrorCode, Result, ServiceError};
use kageha_runtime::{execution::*, lifecycle::*, turn::Start};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    io,
    sync::{atomic::AtomicBool, Arc},
    time::{Duration, Instant},
};
fn error(e: impl std::fmt::Display) -> ServiceError {
    ServiceError::new(ErrorCode::Unavailable, e.to_string())
}
pub struct ExecutionHost {
    pub tools: Arc<dyn ToolPort + Send + Sync>,
    pub supervisor: Arc<Supervisor>,
    pub journal: Option<Arc<kageha_policy::journal::Journal>>,
    pub workers: Option<Arc<workers::Composition>>,
    pub hooks: Option<Arc<hooks::Lifecycle>>,
    pub planning: Option<Arc<planning::Coordinator>>,
}
struct Executor {
    store: Arc<dyn RunStore>,
    model: Arc<dyn ModelPort + Send + Sync>,
    tools: Arc<dyn ToolPort + Send + Sync>,
    policy: Arc<dyn PolicyPort + Send + Sync>,
    approvals: Option<Arc<dyn ApprovalPort + Send + Sync>>,
    journal: Option<Arc<kageha_policy::journal::Journal>>,
    workers: Option<Arc<workers::Composition>>,
    hooks: Option<Arc<hooks::Lifecycle>>,
    planning: Option<Arc<planning::Coordinator>>,
}
impl ExecutionHost {
    pub fn new(
        store: Arc<dyn RunStore>,
        model: Arc<dyn ModelPort + Send + Sync>,
        tools: Arc<dyn ToolPort + Send + Sync>,
        limits: Limits,
    ) -> Result<Arc<Self>> {
        Self::with_policy(
            store,
            model,
            tools,
            limits,
            Arc::new(kageha_policy::DefaultPolicy),
            None,
        )
    }
    pub fn with_policy(
        store: Arc<dyn RunStore>,
        model: Arc<dyn ModelPort + Send + Sync>,
        tools: Arc<dyn ToolPort + Send + Sync>,
        limits: Limits,
        policy: Arc<dyn PolicyPort + Send + Sync>,
        approvals: Option<Arc<dyn ApprovalPort + Send + Sync>>,
    ) -> Result<Arc<Self>> {
        Self::compose(store, model, tools, limits, policy, approvals, None, None, None, None)
    }
    pub fn with_journal(
        store: Arc<dyn RunStore>,
        model: Arc<dyn ModelPort + Send + Sync>,
        tools: Arc<dyn ToolPort + Send + Sync>,
        limits: Limits,
        policy: Arc<dyn PolicyPort + Send + Sync>,
        journal: Arc<kageha_policy::journal::Journal>,
    ) -> Result<Arc<Self>> {
        Self::compose(
            store,
            model,
            tools,
            limits,
            policy,
            Some(journal.clone()),
            Some(journal),
            None,
            None,
            None,
        )
    }
    pub fn with_workers(
        store:Arc<dyn RunStore>,model:Arc<dyn ModelPort+Send+Sync>,tools:Arc<dyn ToolPort+Send+Sync>,limits:Limits,
        policy:Arc<dyn PolicyPort+Send+Sync>,journal:Arc<kageha_policy::journal::Journal>,workers:Arc<workers::Composition>,
    )->Result<Arc<Self>>{Self::compose(store,model,tools,limits,policy,Some(journal.clone()),Some(journal),Some(workers),None,None)}
    /// Compose reusable workers and lifecycle hooks without a WebUI host.
    pub fn with_lifecycle(
        store:Arc<dyn RunStore>,model:Arc<dyn ModelPort+Send+Sync>,tools:Arc<dyn ToolPort+Send+Sync>,limits:Limits,
        policy:Arc<dyn PolicyPort+Send+Sync>,journal:Arc<kageha_policy::journal::Journal>,workers:Option<Arc<workers::Composition>>,hooks:Arc<hooks::Lifecycle>,
    )->Result<Arc<Self>>{Self::compose(store,model,tools,limits,policy,Some(journal.clone()),Some(journal),workers,Some(hooks),None)}
    pub fn with_components(
        store:Arc<dyn RunStore>,model:Arc<dyn ModelPort+Send+Sync>,tools:Arc<dyn ToolPort+Send+Sync>,limits:Limits,
        policy:Arc<dyn PolicyPort+Send+Sync>,journal:Arc<kageha_policy::journal::Journal>,components:Components,
    )->Result<Arc<Self>>{Self::compose(store,model,tools,limits,policy,Some(journal.clone()),Some(journal),components.workers,components.hooks,components.planning)}
    fn compose(
        store: Arc<dyn RunStore>,
        model: Arc<dyn ModelPort + Send + Sync>,
        tools: Arc<dyn ToolPort + Send + Sync>,
        limits: Limits,
        policy: Arc<dyn PolicyPort + Send + Sync>,
        approvals: Option<Arc<dyn ApprovalPort + Send + Sync>>,
        journal: Option<Arc<kageha_policy::journal::Journal>>,
        workers: Option<Arc<workers::Composition>>,
        hooks: Option<Arc<hooks::Lifecycle>>,
        planning: Option<Arc<planning::Coordinator>>,
    ) -> Result<Arc<Self>> {
        let executor = Arc::new(Executor {
            journal: journal.clone(),
            workers: workers.clone(),
            hooks: hooks.clone(),
            planning: planning.clone(),
            store: store.clone(),
            model,
            tools: tools.clone(),
            policy,
            approvals,
        });
        let supervisor = Supervisor::new(store.clone(), executor, limits)?;
        supervisor.recover()?;
        let host = Arc::new(Self {
            supervisor,
            tools,
            journal,
            workers: workers.clone(),
            hooks: hooks.clone(),
            planning: planning.clone(),
        });
        if let Some(workers)=workers {workers.attach(&host)?;}
        // Preparation may have crashed before policy delegation. Revalidate it;
        // durable staging alone is never proof of authorization to activate.
        for run in store.active()? {
            if run.state == RunState::Staged {
                if let Err(error) = host.submit(&run.spec) {
                    if store
                        .get(&run.spec.id)?
                        .is_some_and(|r| !r.state.terminal())
                    {
                        return Err(error);
                    }
                }
            }
        }
        Ok(host)
    }
    pub fn decide_approval(&self, id: &str, decision: &ApprovalDecision) -> Result<ApprovalRecord> {
        let journal = self.journal.as_ref().ok_or_else(|| {
            ServiceError::new(ErrorCode::Unsupported, "No policy journal is configured")
        })?;
        let record = journal
            .get(id)?
            .ok_or_else(|| ServiceError::new(ErrorCode::Invalid, "Approval not found"))?;
        if record.state == ApprovalState::Pending {
            let active = self
                .supervisor
                .store
                .find_turn(&record.request.authority.turn)?
                .is_some_and(|run| {
                    run.spec.session == record.request.authority.session
                        && run.state == RunState::Running
                        && !run.cancel_requested
                });
            if !active {
                journal.cancel(id)?;
                return Err(ServiceError::new(
                    ErrorCode::Conflict,
                    "Approval run is no longer active",
                ));
            }
        }
        journal.decide(id, decision)
    }
    pub fn submit(&self, spec: &RunSpec) -> Result<RunRecord> {
        validate(spec)?;
        if spec.input.get("planning").is_some()&&self.planning.is_none(){return Err(ServiceError::new(ErrorCode::Unsupported,"Planning composition is not configured"));}
        let mut normalized=if let Some(workers)=&self.workers{workers.normalize(spec,self.supervisor.store.as_ref())?}else{
            if spec.input.get("worker_workspace").is_some()||spec.input.get("specialist_binding").is_some()||spec.input.get("workspace_binding").is_some()||spec.input.get("profile_activation").is_some(){return Err(ServiceError::new(ErrorCode::Unsupported,"Worker workspace composition is not configured"));}spec.clone()
        };
        // Pin policy only at new admission. Recovered legacy runs retain their
        // original contract; retries of admission reuse the persisted policy.
        if let Some(existing)=self.supervisor.store.get(&normalized.id)? {
            for field in ["persistence","request_budget_id"] {
                if normalized.input.get(field).is_none() {
                    if let Some(value)=existing.spec.input.get(field) { normalized.input[field]=value.clone(); }
                }
            }
        } else if let Some(parent_id)=&normalized.parent_run {
            let parent=self.supervisor.store.get(parent_id)?.ok_or_else(||ServiceError::new(ErrorCode::Invalid,"Parent run not found"))?;
            for field in ["persistence","request_budget_id"] {
                if let Some(value)=parent.spec.input.get(field) {
                    if normalized.input.get(field).is_some_and(|claimed|claimed!=value) { return Err(ServiceError::new(ErrorCode::Unauthorized,"Worker cannot change the request persistence budget")); }
                    normalized.input[field]=value.clone();
                } else if normalized.input.get(field).is_some() { return Err(ServiceError::new(ErrorCode::Unauthorized,"Worker cannot create a separate persistence budget")); }
            }
        } else if normalized.input.get("resume_from").is_none() {
            if normalized.input.get("persistence").is_none() { normalized.input["persistence"]=json!(PersistencePolicy::default()); }
            if normalized.input["persistence"]["enabled"]==true {
                if normalized.input.get("request_budget_id").is_some_and(|id|id!=&json!(normalized.id)) { return Err(ServiceError::new(ErrorCode::Unauthorized,"A root request owns its own persistence budget")); }
                normalized.input["request_budget_id"]=json!(normalized.id);
            }
        }
        validate(&normalized)?;
        let spec=&normalized;
        self.check_continuation(spec)?;
        if let (Some(parent), Some(parent_id)) = (&spec.parent, &spec.parent_run) {
            let parent_run = self
                .supervisor
                .store
                .get(parent_id)?
                .ok_or_else(|| ServiceError::new(ErrorCode::Invalid, "Parent run not found"))?;
            if &parent_run.spec.session != parent || spec.depth != parent_run.spec.depth + 1 {
                return Err(ServiceError::new(
                    ErrorCode::Unauthorized,
                    "Parent run scope does not match",
                ));
            }
        }
        let staged = self.supervisor.stage(spec)?;
        if staged.state != RunState::Staged {
            return Ok(staged);
        }
        if let (Some(parent), Some(journal)) = (&spec.parent, &self.journal) {
            if let Err(error) = journal.delegate(parent, &spec.session) {
                // If this write fails, recovery will revalidate delegation again.
                let _ = self.supervisor.store.recover(
                    &spec.id,
                    Completion {
                        state: RunState::Failed,
                        result: json!({"message":"Policy delegation rejected during admission"}),
                    },
                );
                return Err(error);
            }
        }
        self.supervisor.activate(&spec.id)
    }
}
fn validate(spec: &RunSpec) -> Result<()> {
    spec.validate()?;
    if spec.parent.is_some() != spec.parent_run.is_some() {
        return Err(ServiceError::new(
            ErrorCode::Invalid,
            "Parent session and run identity must be supplied together",
        ));
    }
    let input = spec
        .input
        .as_object()
        .ok_or_else(|| ServiceError::new(ErrorCode::Invalid, "Expected execution input"))?;
    if input
        .keys()
        .any(|k| !matches!(k.as_str(), "messages" | "max_model_calls" | "timeout_ms" | "worker_workspace" | "specialist_binding" | "resume_from" | "workspace_binding" | "planning" | "profile_activation" | "persistence" | "request_budget_id"))
    {
        return Err(ServiceError::new(
            ErrorCode::Invalid,
            "Execution input cannot override adapters, credentials or workspace",
        ));
    }
    if let Some(value)=input.get("persistence") {
        let policy:PersistencePolicy=serde_json::from_value(value.clone()).map_err(error)?;policy.validate().map_err(error)?;
    }
    if input.get("request_budget_id").is_some_and(|id|id.as_str().is_none_or(|id|id.is_empty()||id.len()>128||id.chars().any(char::is_control))) { return Err(ServiceError::new(ErrorCode::Invalid,"Invalid request budget identity")); }
    if let Some(request)=input.get("planning"){planning::Request::parse(request).map_err(|e|ServiceError::new(ErrorCode::Invalid,e.to_string()))?;}
    let max = input
        .get("max_model_calls")
        .map(Value::as_u64)
        .unwrap_or(Some(128));
    let timeout = input
        .get("timeout_ms")
        .map(Value::as_u64)
        .unwrap_or(Some(120000));
    let messages = input.get("messages").and_then(Value::as_array);
    if max.is_none_or(|v| !(1..=1000).contains(&v))
        || timeout.is_none_or(|v| !(1..=7200000).contains(&v))
        || messages.is_none_or(|v| v.is_empty() || v.len() > 2048)
    {
        return Err(ServiceError::new(
            ErrorCode::Invalid,
            "Invalid execution budget or conversation",
        ));
    }
    Ok(())
}
fn digest(value: &Value) -> String {
    format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
}
struct BoundTools<'a> {
    base: &'a (dyn ToolPort + Sync),
    journal: Option<&'a kageha_policy::journal::Journal>,
    model: Option<&'a (dyn ModelPort + Sync)>,
    store: &'a dyn RunStore,
    run: &'a RunRecord,
    policy: &'a (dyn PolicyPort + Sync),
    approvals: Option<&'a (dyn ApprovalPort + Sync)>,
    hooks: Option<&'a hooks::RunHooks<'a>>,
    completed_calls: std::collections::HashSet<String>,
    completed_mutations:continuation::MutationGuard,
}
impl ToolPort for BoundTools<'_> {
    fn on_compaction(&self,summary:&CompactionSummary)->io::Result<()> {
        self.base.on_compaction(summary)
    }
    fn plan_snapshot(&self)->io::Result<Option<String>> {
        self.base.plan_snapshot()
    }
    fn take_attachments(&self)->io::Result<Vec<Value>> {
        self.base.take_attachments()
    }
    fn recovery_revision(&self,call:&ToolCall)->io::Result<Option<String>> {
        let context=ToolContext{session:self.run.spec.session.clone(),turn:self.run.spec.turn.clone()};
        if let Some(revision)=self.base.recovery_revision(call)? { return Ok(Some(revision)); }
        Ok(self.base.binding(&context,call).ok().map(|binding|json!([binding.binding,binding.generation,binding.resource,binding.fingerprint]).to_string()))
    }
    fn persistence_policy(&self)->io::Result<Option<PersistencePolicy>> {
        if let Some(value)=self.run.spec.input.get("persistence") {
            let policy:PersistencePolicy=serde_json::from_value(value.clone())?;policy.validate()?;
            return Ok(policy.enabled.then_some(policy));
        }
        self.base.persistence_policy()
    }
    fn recovery_checkpoint(&self)->io::Result<Value> {
        if self.run.spec.input["persistence"]["enabled"]==true {
            return self.store.recovery_checkpoint(&json!([self.run.spec.input["request_budget_id"],self.run.spec.session]).to_string()).map_err(io::Error::other);
        }
        self.base.recovery_checkpoint()
    }
    fn save_recovery(&self,state:&Value)->io::Result<()> {
        if self.run.spec.input["persistence"]["enabled"]==true {
            return self.store.save_recovery_checkpoint(&json!([self.run.spec.input["request_budget_id"],self.run.spec.session]).to_string(),state).map_err(io::Error::other);
        }
        self.base.save_recovery(state)
    }
    fn completion_check(&self,candidate:&str,control:&Control<'_>)->io::Result<CompletionCheck> { self.base.completion_check(candidate,control) }

    fn execute_batch(
        &self,
        calls: &[ToolCall],
        control: &Control<'_>,
    ) -> io::Result<Vec<ToolReply>> {
        kageha_runtime::fanout::execute_parallel(self, calls, control, 4)
    }
    fn dispatch_access(&self, call: &ToolCall) -> io::Result<DispatchAccess> {
        let context=ToolContext{session:self.run.spec.session.clone(),turn:self.run.spec.turn.clone()};
        // An excluded/stale tool receives a known denial in execute(), allowing
        // the model to choose an allowed alternative. Scheduling admits no input.
        if !self.base.discover_in(&context)?.iter().any(|spec|spec["function"]["name"]==call.name){return Ok(DispatchAccess::Exclusive);}
        self.base.dispatch_access_in(&context,call)
    }

    fn discover(&self) -> io::Result<Vec<Value>> {
        self.base.discover_in(&ToolContext {
            session: self.run.spec.session.clone(),
            turn: self.run.spec.turn.clone(),
        })
    }
    fn checkpoint(&self) -> io::Result<Vec<String>> {
        self.store
            .steering(&self.run.spec.id)
            .map(|rows| rows.into_iter().map(|r| r.body).collect())
            .map_err(io::Error::other)
    }
    fn execute(&self, call: &ToolCall, control: &Control<'_>) -> io::Result<ToolReply> {
        control.check()?;
        call.validate()?;
        if self.completed_calls.contains(&call.id){return Err(stop("This tool call belongs to the saved checkpoint and must not be repeated"));}
        if let Some(journal) = self.journal {
            journal.check_action(&call.name)?;
            journal
                .profile(&self.run.spec.session)
                .map_err(|_| stop("Permission scope is no longer available"))?;
        }
        let context = ToolContext {
            session: self.run.spec.session.clone(),
            turn: self.run.spec.turn.clone(),
        };
        if !self
            .base
            .discover_in(&context)?
            .iter()
            .any(|spec| spec["function"]["name"] == call.name)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Tool is not exposed by the configured binding",
            ));
        }
        let binding = self.base.binding(&context, call)?;
        let mutation=self.base.mutation_binding(&context,call)?;
        if let Some(identity)=&mutation{self.completed_mutations.check(call,identity)?;}
        let id = format!("tool:{}", call.id);
        let key = digest(&json!({"tool":call,"session":context.session,"binding":binding}));
        if let Some(effect) = self
            .store
            .effect_get(&self.run.spec.turn, &id)
            .map_err(io::Error::other)?
        {
            if effect.digest != key {
                return Err(io::Error::other("Receipt input or authority changed"));
            }
            return match effect.receipt {
                Some(receipt) => serde_json::from_value(receipt).map_err(io::Error::other),
                None => Err(uncertain_stop(
                    "Earlier tool dispatch is uncertain; inspect its receipt before retrying",
                )),
            };
        }
        let mut hook_context=Vec::new();
        if let Some(hooks)=self.hooks {
            hook_context.push(hooks.run("preToolUse",Some(call),None,self,control)?.extra_context);
            if matches!(call.name.as_str(),"bash"|"remote_exec"){hook_context.push(hooks.run("beforeShell",Some(call),None,self,control)?.extra_context);}
        }
        let authority = Authority {
            session: context.session.clone(),
            turn: context.turn.clone(),
            binding: binding.binding.clone(),
            generation: binding.generation,
            resource: binding.resource.clone(),
            input_digest: kageha_policy::input_digest(
                &json!({"name":call.name,"arguments":call.arguments,"configuration":binding.fingerprint}),
            ),
        };
        let request = ApprovalRequest {
            authority: authority.clone(),
            action: call.name.clone(),
            class: binding.class,
            detail: binding.detail.clone(),
        };
        if let Some(journal) = self.journal {
            journal.check_request(&request)?;
            if journal
                .denial_limit(&self.run.spec.session, &self.run.spec.turn)
                .map_err(io::Error::other)?
            {
                return Err(stop(
                    "Automatic review denial limit reached; this turn has stopped",
                ));
            }
        }
        let profile_journal = match self.journal {
            Some(j)
                if j.profile(&self.run.spec.session)
                    .map_err(io::Error::other)?
                    .is_some() =>
            {
                Some(j)
            }
            _ => None,
        };
        let authorized = if let Some(journal) = profile_journal {
            match if journal.requires_manual(&request.action) {
                Authorization::RequireApproval
            } else {
                self.policy.evaluate(&request, PermissionMode::Ask)?
            } {
                Authorization::Allow => true,
                Authorization::Deny => false,
                Authorization::RequireApproval => {
                    let evidence = vec![ReviewEvidence {
                        source: "configured-tool-binding".into(),
                        digest: binding.fingerprint.clone(),
                        summary: format!(
                            "Binding {} generation {} resource {}",
                            binding.binding, binding.generation, binding.resource
                        ),
                    }];
                    let intent =
                        kageha_runtime::review_context::user_intent(self.store, self.run).ok();
                    let reviewer = self.model.zip(intent.as_deref()).map(|(model, intent)| {
                        kageha_models::reviewer::ModelReviewer {
                            model,
                            intent,
                            evidence: &evidence,
                        }
                    });
                    let record = journal.prepare_profile(
                        &request,
                        reviewer.as_ref().map(|r| r as &dyn ReviewPort),
                        control,
                    )?;
                    if record.state == ApprovalState::Denied {
                        return Err(io::Error::new(io::ErrorKind::PermissionDenied,"Automatic review denied this action. Do not circumvent through another tool; choose a materially safer alternative or ask the user."));
                    }
                    kageha_contracts::request_budget::waiting(||journal.wait(&record.id, &request.authority, control))?
                }
            }
        } else {
            kageha_policy::authorize(
                self.policy,
                self.approvals.map(|p| p as &dyn ApprovalPort),
                &request,
                PermissionMode::Ask,
                control,
            )?
        };
        if !authorized {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Tool requires an exact approval decision",
            ));
        }
        if self.base.binding(&context, call)? != binding {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Tool binding changed during approval",
            ));
        }
        if self.base.mutation_binding(&context,call)?!=mutation{return Err(stop("Mutation authority changed during approval"));}
        control.check()?;
        if let Some(receipt) = self
            .store
            .effect_begin(&self.run.spec.turn, &id, &key)
            .map_err(io::Error::other)?
        {
            return serde_json::from_value(receipt).map_err(io::Error::other);
        }
        control.check()?;
        let reply = match self
            .base
            .execute_authorized_in(&context, call, &binding, control)
        {
            Ok(reply) => reply,
            Err(e) => {
                if is_uncertain(&e) {
                    return Err(e);
                }
                control.check()?;
                ToolReply::failure(e)
            }
        };
        control.check()?;
        let mut published_reply=serde_json::to_value(&reply)?;published_reply["_binding"]=serde_json::to_value(&binding)?;published_reply["_tool_name"]=json!(call.name);published_reply["_mutation_binding"]=serde_json::to_value(&mutation)?;
        self.store
            .effect_finish(
                &self.run.spec.turn,
                &id,
                &key,
                published_reply,
            )
            .map_err(|_| uncertain_stop("Tool input may have completed, but its receipt could not be published. Stop and inspect the original call receipt; do not repeat it with a new call ID."))?;
        if let Some(hooks)=self.hooks {
            let out=hooks.run("postToolUse",Some(call),Some(&reply),self,control)?;
            hook_context.push(out.extra_context);
            let context=hook_context.into_iter().filter(|s|!s.is_empty()).collect::<Vec<_>>().join("\n");
            if !context.is_empty(){return Ok(ToolReply{content:format!("{}\n\nHook context: {}",reply.content,context),is_error:reply.is_error,outcome:reply.outcome});}
        }
        Ok(reply)
    }
}
impl RunExecutor for Executor {
    fn execute(&self, run: &RunRecord, cancel: Arc<AtomicBool>) -> Result<Completion> {
        let budget=self.store.request_budget_begin(run)?;
        struct Finish(Option<kageha_contracts::request_budget::BudgetHandle>);
        impl Drop for Finish { fn drop(&mut self) { if let Some(budget)=&self.0 { let _=budget.finish(); } } }
        let _finish=Finish(budget.clone());
        kageha_contracts::request_budget::inherited(budget,||self.execute_inner(run,cancel))
    }
    fn inspect(&self, run: &RunRecord) -> Result<Option<Completion>> { self.inspect_inner(run) }
    fn publish(&self, record:&RunRecord)->Result<()> { self.publish_inner(record) }
}
impl Executor {
    fn execute_inner(&self, run: &RunRecord, cancel: Arc<AtomicBool>) -> Result<Completion> {
        validate(&run.spec)?;
        let input = &run.spec.input;
        let key = digest(&json!(run.spec));
        if let Some(saved) = self
            .store
            .effect_begin(&run.spec.turn, "runtime:completion", &key)?
        {
            return serde_json::from_value(saved).map_err(error);
        }
        let control = Control {
            cancelled: &cancel,
            deadline: input["timeout_ms"].as_u64().or_else(||(input["persistence"]["enabled"]!=true).then_some(120000))
                .map(|timeout|Instant::now()+Duration::from_millis(timeout)),
        };
        let mut messages:Vec<Value> = serde_json::from_value(input["messages"].clone()).map_err(error)?;
        let (active, profile_context) = self.workers.as_ref()
            .map(|w| w.activation(run, self.store.as_ref(), &control)).transpose()?.unwrap_or_default();
        if input.get("resume_from").is_none() {
            messages.splice(0..0, profile_context);
        }
        let executor = active.as_ref().and_then(|a| a.executor.as_deref()).unwrap_or(self.model.as_ref());
        let planner = active.as_ref().and_then(|a| a.planner.as_deref()).unwrap_or(self.model.as_ref());
        let reviewer = active.as_ref().and_then(|a| a.fast_worker.as_deref()).unwrap_or(executor);
        let counted_executor=BudgetedModel{base:executor};
        let counted_planner=BudgetedModel{base:planner};
        let counted_reviewer=BudgetedModel{base:reviewer};
        let executor=&counted_executor as &(dyn ModelPort+Sync);
        let planner=&counted_planner as &(dyn ModelPort+Sync);
        let reviewer=&counted_reviewer as &(dyn ModelPort+Sync);
        let mut hook_root=None;
        let scoped=if let Some(workers)=&self.workers{
            let workspace=workers.prepare(run,self.store.as_ref(),&control).map_err(error)?;
            if workspace["mode"]=="worktree"&&input.get("resume_from").is_none(){messages.insert(0,json!({"role":"system","content":format!("This worker uses an isolated Git checkout at {}. Workspace file tools cannot fall back to the source directory.",workspace["root"].as_str().unwrap_or(""))}));}
            hook_root=workspace["root"].as_str().map(str::to_owned);
            workers.tools(self.tools.clone(),run,self.store.clone()).map_err(error)?
        }else{self.tools.clone()};
        if input.get("resume_from").is_none(){if let Some(workers)=&self.workers{if let Some(prompt)=workers.instructions(run,self.store.as_ref()).map_err(error)?{if !messages.iter().any(|m|m["role"]=="system"&&m["content"]==prompt){messages.insert(0,json!({"role":"system","content":prompt}));}}}}
        let lifecycle=self.hooks.as_ref().map(|hooks|hooks.prepare(run,self.store.as_ref(),hook_root.as_deref(),&control)).transpose().map_err(error)?;
        let planning=self.planning.as_ref().filter(|_|input.get("planning").is_some());
        let plan_key=planning.map(|p|p.key(run,self.store.as_ref())).transpose().map_err(error)?;
        let plan_tools=planning.zip(plan_key.as_ref()).map(|(coordinator,key)|planning::Tools{base:scoped.as_ref(),coordinator,run,store:self.store.as_ref(),workers:self.workers.as_deref(),key:key.clone()});
        let base:&(dyn ToolPort+Sync)=plan_tools.as_ref().map(|t|t as &(dyn ToolPort+Sync)).unwrap_or(scoped.as_ref());
        let tools = BoundTools {
            base,
            hooks: lifecycle.as_ref(),
            completed_calls: continuation::previous_calls(self.store.as_ref(),run)?,
            completed_mutations: continuation::previous_mutations(self.store.as_ref(),run,&control)?,
            journal: self.journal.as_deref(),
            model: Some(reviewer),
            store: self.store.as_ref(),
            run,
            policy: self.policy.as_ref(),
            approvals: self
                .approvals
                .as_deref()
                .map(|p| p as &(dyn ApprovalPort + Sync)),
        };
        if input.get("resume_from").is_none(){
            if let Some(hooks)=&lifecycle{let out=hooks.run("sessionStart",None,None,&tools,&control).map_err(error)?;if !out.extra_context.is_empty(){messages.insert(0,json!({"role":"system","content":out.extra_context}));}}
        }
        let planner_calls=planning.map(|p|p.prepare(run,self.store.as_ref(),planner,&tools,&messages,&control)).transpose().map_err(error)?.unwrap_or(0);
        let model_limit=input["max_model_calls"].as_u64().unwrap_or(128) as u32;
        if model_limit<=planner_calls{return Err(error("The planner consumed the model-call budget; no task tools were dispatched"));}
        let plan_model=planning.zip(plan_key).map(|(coordinator,key)|planning::Model{base:executor,coordinator,key,gate:Default::default()});
        let model:&dyn ModelPort=plan_model.as_ref().map(|m|m as &dyn ModelPort).unwrap_or(executor);
        let mut checkpoints=continuation::Observer::new(self.store.as_ref(),&run.spec.turn);
        let mut task_completion=Value::Null;
        let output = execute_turn(
            Start {
                version: 1,
                max_model_calls: model_limit-planner_calls,
                messages,
            },
            model,
            &tools,
            &control,
            |frame| { if frame["kind"]=="task_completion" { task_completion=frame.clone(); } checkpoints.observe(frame) },
        );
        if let Some(budget)=kageha_contracts::request_budget::current() { budget.finish().map_err(error)?; }
        let request_budget=run.spec.input["request_budget_id"].as_str().map(|root|self.store.request_budget_snapshot(root)).transpose()?.unwrap_or(Value::Null);
        let completion = match output {
            Ok(message) => Completion {
                state: RunState::Succeeded,
                result: json!({"message":message,"task_completion":task_completion,"request_budget":request_budget}),
            },
            Err(e) => Completion {
                state: if e.kind() == io::ErrorKind::Interrupted {
                    RunState::Cancelled
                } else {
                    RunState::Failed
                },
                result: json!({"message":e.to_string(),"request_budget":request_budget,"error_code":if e.kind()==io::ErrorKind::TimedOut{"deadline"}else if kageha_contracts::request_budget::is_exceeded(&e){"request_budget_exhausted"}else{"execution_failed"}}),
            },
        };
        self.store.effect_finish(
            &run.spec.turn,
            "runtime:completion",
            &key,
            serde_json::to_value(&completion).map_err(error)?,
        )?;
        Ok(completion)
    }
    fn inspect_inner(&self, run: &RunRecord) -> Result<Option<Completion>> {
        let receipt = self
            .store
            .effect_get(&run.spec.turn, "runtime:completion")?;
        if receipt
            .as_ref()
            .is_some_and(|e| e.digest != digest(&json!(run.spec)))
        {
            return Err(ServiceError::new(
                ErrorCode::Conflict,
                "Completion receipt does not match this run",
            ));
        }
        receipt
            .and_then(|e| e.receipt)
            .map(serde_json::from_value)
            .transpose()
            .map_err(error)
    }
    fn publish_inner(&self, record: &RunRecord) -> Result<()> {
        if let Some(workers)=&self.workers{workers.settle(record,self.store.as_ref())?;}
        if record.state.terminal() {
            if let Some(journal) = &self.journal {
                journal.cancel_turn(&record.spec.session, &record.spec.turn)?;
            }
        }
        Ok(())
    } // API reads the owned journal directly.
}
/// Compatibility export of the independent workspace adapter.
pub use kageha_workspace::ReadWorkspace;

#[cfg(test)]
mod tests {
    use super::*;
    use kageha_runtime_store::SqliteRunStore;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Model {
        calls: Arc<AtomicUsize>,
    }
    impl ModelPort for Model {
        fn complete(&self, request: ModelRequest, _: &Control<'_>) -> io::Result<AssistantMessage> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if request.messages.last().is_some_and(|m| m["role"] == "tool") {
                Ok(AssistantMessage {
                    continuation: None,
                    content: request.messages.last().unwrap()["content"].clone(),
                    tool_calls: vec![],
                })
            } else {
                Ok(AssistantMessage {
                    continuation: None,
                    content: Value::Null,
                    tool_calls: vec![ToolCall {
                        id: "call".into(),
                        name: "custom".into(),
                        arguments: json!({}),
                    }],
                })
            }
        }
    }
    struct Tool {
        label: String,
        calls: Arc<AtomicUsize>,
    }
    impl ToolPort for Tool {
        fn discover(&self) -> io::Result<Vec<Value>> {
            Ok(vec![json!({"function":{"name":"custom"}})])
        }
        fn execute(&self, _: &ToolCall, _: &Control<'_>) -> io::Result<ToolReply> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ToolReply::success(self.label.clone()))
        }
    }
    struct FixturePolicy;
    impl PolicyPort for FixturePolicy {
        fn evaluate(
            &self,
            request: &ApprovalRequest,
            _: PermissionMode,
        ) -> io::Result<Authorization> {
            request.authority.validate()?;
            Ok(if request.action == "custom" {
                Authorization::Allow
            } else {
                Authorization::Deny
            })
        }
    }
    fn spec() -> RunSpec {
        RunSpec {
            id: "run".into(),
            session: "chat".into(),
            turn: "turn".into(),
            request: "request".into(),
            parent: None,
            parent_run: None,
            agent: "main".into(),
            objective: "work".into(),
            depth: 0,
            input: json!({"messages":[{"role":"user","content":"work"}],"max_model_calls":4}),
        }
    }
    #[test]
    fn independently_supplied_model_and_tool_ports_use_identical_hosting() {
        for label in ["adapter one", "adapter two"] {
            let root = tempfile::tempdir().unwrap();
            let store =
                Arc::new(SqliteRunStore::open(&root.path().join("journal.sqlite")).unwrap());
            let calls = Arc::new(AtomicUsize::new(0));
            let tool_calls = Arc::new(AtomicUsize::new(0));
            let host = ExecutionHost::with_policy(
                store.clone(),
                Arc::new(Model {
                    calls: calls.clone(),
                }),
                Arc::new(Tool {
                    label: label.into(),
                    calls: tool_calls.clone(),
                }),
                Limits {
                    per_depth: 1,
                    queue_per_depth: 4,
                },
                Arc::new(FixturePolicy),
                None,
            )
            .unwrap();
            host.submit(&spec()).unwrap();
            host.supervisor.start().unwrap();
            let end = Instant::now() + Duration::from_secs(3);
            loop {
                let record = store.get("run").unwrap().unwrap();
                if record.state.terminal() {
                    let result=record.completion.unwrap().result;
                    assert_eq!(result["message"], label);
                    assert_eq!(result["request_budget"]["used_model_calls"],2);
                    assert_eq!(result["request_budget"]["max_model_calls"],kageha_contracts::execution::PersistencePolicy::default().max_model_calls);
                    break;
                }
                assert!(Instant::now() < end);
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(calls.load(Ordering::SeqCst), 2);
            assert_eq!(tool_calls.load(Ordering::SeqCst), 1);
            host.submit(&spec()).unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), 2);
            assert_eq!(store.request_budget_snapshot("run").unwrap()["used_model_calls"],2);
            host.supervisor.shutdown().unwrap();
            host.supervisor.join(Duration::from_secs(3)).unwrap();
        }
    }
    #[test]
    fn standalone_recovery_uses_committed_execution_receipt_before_publication() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("journal.sqlite");
        let calls = Arc::new(AtomicUsize::new(0));
        let tool_calls = Arc::new(AtomicUsize::new(0));
        {
            let store = Arc::new(SqliteRunStore::open(&path).unwrap());
            store.stage(&spec(), 4).unwrap();
            store.activate("run").unwrap();
            let run = store.claim(0).unwrap().unwrap();
            let executor = Executor {
                workers:None,
                hooks:None,
                planning:None,
                store: store.clone(),
                model: Arc::new(Model {
                    calls: calls.clone(),
                }),
                tools: Arc::new(Tool {
                    label: "durable result".into(),
                    calls: tool_calls.clone(),
                }),
                policy: Arc::new(FixturePolicy),
                approvals: None,
                journal: None,
            };
            executor
                .execute(&run, Arc::new(AtomicBool::new(false)))
                .unwrap();
            assert_eq!(store.get("run").unwrap().unwrap().state, RunState::Running);
        }
        let store = Arc::new(SqliteRunStore::open(&path).unwrap());
        let _host = ExecutionHost::new(
            store.clone(),
            Arc::new(Model {
                calls: calls.clone(),
            }),
            Arc::new(Tool {
                label: "must not execute".into(),
                calls: tool_calls.clone(),
            }),
            Limits::default(),
        )
        .unwrap();
        let recovered = store.get("run").unwrap().unwrap();
        assert_eq!(recovered.state, RunState::Succeeded);
        assert_eq!(
            recovered.completion.unwrap().result["message"],
            "durable result"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(tool_calls.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn unknown_tool_receipts_are_not_replayed() {
        let root = tempfile::tempdir().unwrap();
        let store = SqliteRunStore::open(&root.path().join("journal.sqlite")).unwrap();
        store.stage(&spec(), 4).unwrap();
        let run = store.get("run").unwrap().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let base = Tool {
            label: "no".into(),
            calls: calls.clone(),
        };
        let call = ToolCall {
            id: "call".into(),
            name: "custom".into(),
            arguments: json!({}),
        };
        let key = digest(
            &json!({"tool":call,"session":run.spec.session,"binding":base.binding(&ToolContext{session:run.spec.session.clone(),turn:run.spec.turn.clone()},&call).unwrap()}),
        );
        store
            .effect_begin(&run.spec.turn, "tool:call", &key)
            .unwrap();
        let bound = BoundTools {
            hooks: None,
            completed_calls:Default::default(),
            completed_mutations:Default::default(),
            base: &base,
            journal: None,
            model: None,
            store: &store,
            run: &run,
            policy: &FixturePolicy,
            approvals: None,
        };
        let cancel = AtomicBool::new(false);
        assert!(bound
            .execute(
                &call,
                &Control {
                    cancelled: &cancel,
                    deadline: None
                }
            )
            .is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn read_adapter_cannot_escape_its_bound_workspace() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("workspace")).unwrap();
        std::fs::write(root.path().join("secret"), "outside").unwrap();
        let tools = ReadWorkspace::new(&root.path().join("workspace")).unwrap();
        let cancel = AtomicBool::new(false);
        let call = ToolCall {
            id: "call".into(),
            name: "read_file".into(),
            arguments: json!({"path":"../secret"}),
        };
        assert!(tools
            .execute(
                &call,
                &Control {
                    cancelled: &cancel,
                    deadline: None
                }
            )
            .is_err());
    }
}

#[cfg(test)]
mod policy_tests {
    use super::*;
    use kageha_runtime_store::SqliteRunStore;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Tool {
        calls: AtomicUsize,
    }
    impl ToolPort for Tool {
        fn discover(&self) -> io::Result<Vec<Value>> {
            Ok(vec![json!({"function":{"name":"custom"}})])
        }
        fn execute(&self, _: &ToolCall, _: &Control<'_>) -> io::Result<ToolReply> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ToolReply::success("approved input".into()))
        }
    }
    struct Approvals {
        calls: AtomicUsize,
    }
    impl ApprovalPort for Approvals {
        fn authorize_once(
            &self,
            request: &ApprovalRequest,
            control: &Control<'_>,
        ) -> io::Result<bool> {
            control.check()?;
            assert_eq!(request.authority.session, "session");
            assert_eq!(request.authority.binding, "tools.default");
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(true)
        }
    }
    #[test]
    fn service_policy_and_approval_ports_guard_dispatch_and_receipts() {
        let root = tempfile::tempdir().unwrap();
        let store = SqliteRunStore::open(&root.path().join("journal.sqlite")).unwrap();
        let spec = RunSpec {
            id: "run".into(),
            session: "session".into(),
            turn: "turn".into(),
            request: "request".into(),
            parent: None,
            parent_run: None,
            agent: "main".into(),
            objective: "work".into(),
            depth: 0,
            input: json!({}),
        };
        store.stage(&spec, 4).unwrap();
        let run = store.get("run").unwrap().unwrap();
        let tool = Tool {
            calls: AtomicUsize::new(0),
        };
        let approvals = Approvals {
            calls: AtomicUsize::new(0),
        };
        let cancel = AtomicBool::new(false);
        let control = Control {
            cancelled: &cancel,
            deadline: None,
        };
        let call = ToolCall {
            id: "input".into(),
            name: "custom".into(),
            arguments: json!({"destination":"exact"}),
        };
        let blocked = BoundTools {
            hooks: None,
            completed_calls:Default::default(),
            completed_mutations:Default::default(),
            base: &tool,
            journal: None,
            model: None,
            store: &store,
            run: &run,
            policy: &kageha_policy::DefaultPolicy,
            approvals: None,
        };
        assert!(blocked.execute(&call, &control).is_err());
        assert_eq!(tool.calls.load(Ordering::SeqCst), 0);
        assert!(store.effect_get("turn", "tool:input").unwrap().is_none());
        let allowed = BoundTools {
            hooks: None,
            completed_calls:Default::default(),
            completed_mutations:Default::default(),
            base: &tool,
            journal: None,
            model: None,
            store: &store,
            run: &run,
            policy: &kageha_policy::DefaultPolicy,
            approvals: Some(&approvals),
        };
        assert_eq!(
            allowed.execute(&call, &control).unwrap().content,
            "approved input"
        );
        assert_eq!(tool.calls.load(Ordering::SeqCst), 1);
        assert_eq!(approvals.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            allowed.execute(&call, &control).unwrap().content,
            "approved input"
        );
        assert_eq!(tool.calls.load(Ordering::SeqCst), 1);
        assert_eq!(approvals.calls.load(Ordering::SeqCst), 1);
        let hidden = ToolCall {
            id: "hidden".into(),
            name: "read_file".into(),
            arguments: json!({}),
        };
        assert!(allowed.execute(&hidden, &control).is_err());
        assert_eq!(tool.calls.load(Ordering::SeqCst), 1);
    }
}

#[cfg(test)]
mod binding_tests {
    use super::*;
    use kageha_runtime_store::SqliteRunStore;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    struct Changing {
        generation: AtomicUsize,
        calls: AtomicUsize,
    }
    impl ToolPort for Changing {
        fn discover(&self) -> io::Result<Vec<Value>> {
            Ok(vec![
                json!({"type":"function","function":{"name":"change","parameters":{"type":"object"}}}),
            ])
        }
        fn execute(&self, _: &ToolCall, _: &Control<'_>) -> io::Result<ToolReply> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ToolReply::success("sent".into()))
        }
        fn binding(&self, _: &ToolContext, _: &ToolCall) -> io::Result<ToolBinding> {
            Ok(ToolBinding {
                binding: "mcp.replaceable".into(),
                generation: self.generation.load(Ordering::SeqCst) as u64,
                resource: "change".into(),
                class: ActionClass::Mutation,
                detail: "Change destination".into(),
                fingerprint: "destination".into(),
            })
        }
    }
    struct Decision<'a>(&'a Changing);
    impl ApprovalPort for Decision<'_> {
        fn authorize_once(&self, _: &ApprovalRequest, _: &Control<'_>) -> io::Result<bool> {
            self.0.generation.fetch_add(1, Ordering::SeqCst);
            Ok(true)
        }
    }
    #[test]
    fn changed_adapter_authority_after_approval_never_dispatches() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteRunStore::open(&dir.path().join("runs.sqlite")).unwrap();
        let spec = RunSpec {
            id: "run".into(),
            session: "chat".into(),
            turn: "turn".into(),
            request: "request".into(),
            agent: "main".into(),
            objective: "change".into(),
            parent: None,
            parent_run: None,
            depth: 0,
            input: json!({}),
        };
        store.stage(&spec, 4).unwrap();
        let run = store.get("run").unwrap().unwrap();
        let tool = Changing {
            generation: AtomicUsize::new(1),
            calls: AtomicUsize::new(0),
        };
        let approval = Decision(&tool);
        let bound = BoundTools {
            hooks: None,
            completed_calls:Default::default(),
            completed_mutations:Default::default(),
            base: &tool,
            journal: None,
            model: None,
            store: &store,
            run: &run,
            policy: &kageha_policy::DefaultPolicy,
            approvals: Some(&approval),
        };
        assert!(bound
            .execute(
                &ToolCall {
                    id: "one".into(),
                    name: "change".into(),
                    arguments: json!({})
                },
                &Control {
                    cancelled: &AtomicBool::new(false),
                    deadline: None
                }
            )
            .unwrap_err()
            .to_string()
            .contains("binding changed"));
        assert_eq!(tool.calls.load(Ordering::SeqCst), 0);
        assert!(store.effect_get("turn", "tool:one").unwrap().is_none());
    }
}

#[cfg(test)]
mod admission_regressions {
    use super::*;
    use kageha_runtime_store::SqliteRunStore;
    struct Model;
    impl ModelPort for Model {
        fn complete(&self, _: ModelRequest, _: &Control<'_>) -> io::Result<AssistantMessage> {
            Ok(AssistantMessage {
                continuation: None,
                content: json!("done"),
                tool_calls: vec![],
            })
        }
    }
    struct Tools;
    impl ToolPort for Tools {
        fn discover(&self) -> io::Result<Vec<Value>> {
            Ok(vec![])
        }
        fn execute(&self, _: &ToolCall, _: &Control<'_>) -> io::Result<ToolReply> {
            panic!("no tool dispatch expected")
        }
    }
    fn spec(id: &str) -> RunSpec {
        RunSpec {
            id: id.into(),
            session: id.into(),
            turn: format!("turn-{id}"),
            request: id.into(),
            parent: None,
            parent_run: None,
            agent: "main".into(),
            objective: "read only".into(),
            depth: 0,
            input: json!({"messages":[{"role":"user","content":"work"}]}),
        }
    }
    fn host(
        store: Arc<SqliteRunStore>,
        journal: Arc<kageha_policy::journal::Journal>,
    ) -> Arc<ExecutionHost> {
        ExecutionHost::with_journal(
            store,
            Arc::new(Model),
            Arc::new(Tools),
            Limits::default(),
            Arc::new(kageha_policy::DefaultPolicy),
            journal,
        )
        .unwrap()
    }
    #[test]
    fn crash_after_staging_revalidates_policy_before_recovery_admission() {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(SqliteRunStore::open(&root.path().join("runs.sqlite")).unwrap());
        let journal = Arc::new(
            kageha_policy::journal::Journal::open(&root.path().join("policy.sqlite")).unwrap(),
        );
        let initial = host(store.clone(), journal.clone());
        initial.submit(&spec("parent")).unwrap();
        let mut child = spec("child");
        child.parent = Some("parent".into());
        child.parent_run = Some("parent".into());
        child.depth = 1;
        store.stage(&child, 8).unwrap();
        journal.erase_session("child").unwrap();
        let _recovered = host(store.clone(), journal);
        assert_eq!(store.get("child").unwrap().unwrap().state, RunState::Failed);
    }
    #[test]
    fn rejected_policy_delegation_cannot_be_activated_by_recovery() {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(SqliteRunStore::open(&root.path().join("runs.sqlite")).unwrap());
        let journal = Arc::new(
            kageha_policy::journal::Journal::open(&root.path().join("policy.sqlite")).unwrap(),
        );
        let initial = host(store.clone(), journal.clone());
        initial.submit(&spec("parent")).unwrap();
        journal.erase_session("child").unwrap();
        let mut child = spec("child");
        child.parent = Some("parent".into());
        child.parent_run = Some("parent".into());
        child.depth = 1;
        assert!(initial.submit(&child).is_err());
        let _recovered = host(store.clone(), journal);
        assert!(
            store
                .get("child")
                .unwrap()
                .is_none_or(|r| r.state.terminal()),
            "recovery must not activate a rejected staged request"
        );
    }
    #[test]
    fn service_rejects_partial_parent_identity_before_persistence() {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(SqliteRunStore::open(&root.path().join("runs.sqlite")).unwrap());
        let journal = Arc::new(
            kageha_policy::journal::Journal::open(&root.path().join("policy.sqlite")).unwrap(),
        );
        let host = host(store.clone(), journal);
        let mut child = spec("partial");
        child.parent = Some("parent".into());
        child.depth = 1;
        assert!(host.submit(&child).is_err());
        assert!(store.get("partial").unwrap().is_none());
    }
    #[test]
    fn wrong_session_or_depth_is_already_rejected_by_the_store() {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(SqliteRunStore::open(&root.path().join("runs.sqlite")).unwrap());
        let journal = Arc::new(
            kageha_policy::journal::Journal::open(&root.path().join("policy.sqlite")).unwrap(),
        );
        let host = host(store.clone(), journal);
        host.submit(&spec("parent")).unwrap();
        for (id, parent, depth) in [("wrong-session", "other", 1), ("wrong-depth", "parent", 2)] {
            let mut child = spec(id);
            child.parent = Some(parent.into());
            child.parent_run = Some("parent".into());
            child.depth = depth;
            assert!(host.submit(&child).is_err());
            assert!(store.get(id).unwrap().is_none());
        }
    }
}

#[cfg(test)]
mod receipt_regressions {
    use super::*;
    use kageha_runtime_store::SqliteRunStore;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Tool(AtomicUsize);
    impl ToolPort for Tool {
        fn discover(&self) -> io::Result<Vec<Value>> {
            Ok(vec![
                json!({"function":{"name":"read"}}),
                json!({"function":{"name":"deny"}}),
            ])
        }
        fn execute(&self, _: &ToolCall, _: &Control<'_>) -> io::Result<ToolReply> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(ToolReply::success("committed".into()))
        }
    }
    struct Policy;
    impl PolicyPort for Policy {
        fn evaluate(&self, r: &ApprovalRequest, _: PermissionMode) -> io::Result<Authorization> {
            if r.action == "deny" {
                Err(kageha_contracts::policy::stop("denied"))
            } else {
                Ok(Authorization::Allow)
            }
        }
    }
    fn setup() -> (tempfile::TempDir, SqliteRunStore, RunRecord) {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteRunStore::open(&dir.path().join("runs.sqlite")).unwrap();
        let spec = RunSpec {
            id: "run".into(),
            session: "chat".into(),
            turn: "turn".into(),
            request: "request".into(),
            parent: None,
            parent_run: None,
            agent: "main".into(),
            objective: "read".into(),
            depth: 0,
            input: json!({}),
        };
        store.stage(&spec, 8).unwrap();
        let run = store.get("run").unwrap().unwrap();
        (dir, store, run)
    }
    #[test]
    fn successful_sibling_receipt_survives_terminal_batch_stop_and_is_not_replayed() {
        let (_dir, store, run) = setup();
        let tool = Tool(AtomicUsize::new(0));
        let flag = AtomicBool::new(false);
        let ctl = Control {
            cancelled: &flag,
            deadline: None,
        };
        let bound = BoundTools {
            hooks: None,
            completed_calls:Default::default(),
            completed_mutations:Default::default(),
            base: &tool,
            journal: None,
            model: None,
            store: &store,
            run: &run,
            policy: &Policy,
            approvals: None,
        };
        let first = ToolCall {
            id: "first".into(),
            name: "read".into(),
            arguments: json!({}),
        };
        let second = ToolCall {
            id: "second".into(),
            name: "deny".into(),
            arguments: json!({}),
        };
        assert!(kageha_contracts::policy::is_stop(
            &bound
                .execute_batch(&[first.clone(), second], &ctl)
                .unwrap_err()
        ));
        assert!(store
            .effect_get("turn", "tool:first")
            .unwrap()
            .unwrap()
            .receipt
            .is_some());
        assert_eq!(bound.execute(&first, &ctl).unwrap().content, "committed");
        assert_eq!(tool.0.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn receipt_publication_failure_stops_the_turn_with_typed_uncertainty() {
        let (dir, store, run) = setup();
        let sql = rusqlite::Connection::open(dir.path().join("runs.sqlite")).unwrap();
        sql.execute_batch("CREATE TRIGGER fail_receipt BEFORE UPDATE ON effects WHEN NEW.id='tool:first' BEGIN SELECT RAISE(FAIL,'injected receipt failure'); END;").unwrap();
        let tool = Tool(AtomicUsize::new(0));
        let flag = AtomicBool::new(false);
        let ctl = Control {
            cancelled: &flag,
            deadline: None,
        };
        let bound = BoundTools {
            hooks: None,
            completed_calls:Default::default(),
            completed_mutations:Default::default(),
            base: &tool,
            journal: None,
            model: None,
            store: &store,
            run: &run,
            policy: &Policy,
            approvals: None,
        };
        let first = ToolCall {
            id: "first".into(),
            name: "read".into(),
            arguments: json!({}),
        };
        let second = ToolCall {
            id: "second".into(),
            name: "read".into(),
            arguments: json!({}),
        };
        let result = bound.execute_batch(&[first.clone(), second], &ctl);
        assert_eq!(
            tool.0.load(Ordering::SeqCst),
            1,
            "no automatic fresh-ID dispatch after receipt failure"
        );
        let error = result.unwrap_err();
        assert!(is_uncertain(&error));
        assert!(kageha_contracts::policy::is_stop(&error));
        assert!(store
            .effect_get("turn", "tool:first")
            .unwrap()
            .unwrap()
            .receipt
            .is_none());
        sql.execute_batch("DROP TRIGGER fail_receipt").unwrap();
        assert!(bound.execute(&first, &ctl).is_err());
        assert_eq!(tool.0.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn managed_manual_action_does_not_require_a_preexisting_permission_profile() {
        struct ReadOnlyTool(AtomicUsize);
        impl ToolPort for ReadOnlyTool {
            fn discover(&self) -> io::Result<Vec<Value>> {
                Ok(vec![json!({"function":{"name":"read"}})])
            }
            fn binding(&self, _: &ToolContext, _: &ToolCall) -> io::Result<ToolBinding> {
                Ok(ToolBinding {
                    binding: "tools.workspace".into(),
                    generation: 1,
                    resource: "source".into(),
                    class: ActionClass::ReadOnly,
                    detail: "read source".into(),
                    fingerprint: String::new(),
                })
            }
            fn execute(&self, _: &ToolCall, _: &Control<'_>) -> io::Result<ToolReply> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(ToolReply::success("read".into()))
            }
        }
        let (dir, store, run) = setup();
        let rules = kageha_policy::requirements::Requirements {
            manual_actions: vec!["read".into()],
            ..Default::default()
        };
        let journal = kageha_policy::journal::Journal::open_with_requirements(
            &dir.path().join("policy.sqlite"),
            rules,
        )
        .unwrap();
        let tool = ReadOnlyTool(AtomicUsize::new(0));
        let flag = AtomicBool::new(false);
        let bound = BoundTools {
            hooks: None,
            completed_calls:Default::default(),
            completed_mutations:Default::default(),
            base: &tool,
            journal: Some(&journal),
            model: None,
            store: &store,
            run: &run,
            policy: &kageha_policy::DefaultPolicy,
            approvals: Some(&journal),
        };
        let result = bound.execute(
            &ToolCall {
                id: "read".into(),
                name: "read".into(),
                arguments: json!({}),
            },
            &Control {
                cancelled: &flag,
                deadline: Some(Instant::now() + Duration::from_millis(50)),
            },
        );
        assert_eq!(
            tool.0.load(Ordering::SeqCst),
            0,
            "managed manual action must wait even with no profile"
        );
        assert!(result.is_err());
    }
    #[test]
    fn cancellation_during_approval_prevents_dispatch_and_receipt_reservation() {
        struct Decision<'a>(&'a AtomicBool);
        impl ApprovalPort for Decision<'_> {
            fn authorize_once(&self, _: &ApprovalRequest, _: &Control<'_>) -> io::Result<bool> {
                self.0.store(true, Ordering::SeqCst);
                Ok(true)
            }
        }
        let (_dir, store, run) = setup();
        let tool = Tool(AtomicUsize::new(0));
        let flag = AtomicBool::new(false);
        let approval = Decision(&flag);
        let bound = BoundTools {
            hooks: None,
            completed_calls:Default::default(),
            completed_mutations:Default::default(),
            base: &tool,
            journal: None,
            model: None,
            store: &store,
            run: &run,
            policy: &kageha_policy::DefaultPolicy,
            approvals: Some(&approval),
        };
        let error = bound
            .execute(
                &ToolCall {
                    id: "first".into(),
                    name: "read".into(),
                    arguments: json!({}),
                },
                &Control {
                    cancelled: &flag,
                    deadline: None,
                },
            )
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert_eq!(tool.0.load(Ordering::SeqCst), 0);
        assert!(store.effect_get("turn", "tool:first").unwrap().is_none());
    }
}
