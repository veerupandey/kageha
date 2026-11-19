//! Reviewed plans for standalone/embedded runtime consumers. Domain semantics and
//! persistence are supplied by kageha-workflow; execution evidence by RunStore.
use crate::{workers::Composition, BoundTools};
use kageha_contracts::{
    execution::*,
    policy::{stop, ActionClass},
};
use kageha_runtime::lifecycle::{RunRecord, RunStore};
use kageha_workflow::planning::{self as core, Key, Service};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{io, sync::Arc};
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    #[serde(default)]
    pub document: Option<Value>,
    #[serde(default)]
    pub required_outcomes: Vec<Value>,
}
impl Request {
    pub fn parse(value: &Value) -> io::Result<Self> {
        let request: Self = serde_json::from_value(value.clone())?;
        if value.to_string().len() > 131072 || request.required_outcomes.len() > 64 {
            return Err(io::Error::other("Planning request exceeds limits"));
        }
        // Validate caller requirements independently of generated plan content.
        core::validate(&json!({"steps":[{"id":"validate","title":"Validate requirements","depends_on":[]}],"outcomes":request.required_outcomes})).map_err(io::Error::other)?;
        if let Some(body) = &request.document {
            core::validate(body).map_err(io::Error::other)?;
        }
        Ok(request)
    }
}
pub struct Coordinator {
    pub service: Service,
}
impl Coordinator {
    pub fn new(store: Arc<dyn core::Store>) -> Self {
        Self {
            service: Service::new(store),
        }
    }
    pub fn key(&self, run: &RunRecord, store: &dyn RunStore) -> io::Result<Key> {
        let mut run = run.clone();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..128 {
            if !seen.insert(run.spec.id.clone()) {
                return Err(io::Error::other("Cyclic plan continuation"));
            }
            let Some(id) = run.spec.input["resume_from"].as_str() else {
                return Ok(Key {
                    session: run.spec.session,
                    turn: run.spec.turn,
                });
            };
            let old = store
                .get(id)
                .map_err(io::Error::other)?
                .ok_or_else(|| io::Error::other("Plan continuation source is missing"))?;
            if old.spec.session != run.spec.session
                || old.spec.input["planning"] != run.spec.input["planning"]
            {
                return Err(stop(
                    "Plan continuation cannot change scope or requirements",
                ));
            }
            run = old;
        }
        Err(io::Error::other("Plan continuation depth exceeded"))
    }
    /// Returns the number of planner model calls consumed by this run.
    pub(crate) fn prepare(
        &self,
        run: &RunRecord,
        store: &dyn RunStore,
        model: &dyn ModelPort,
        tools: &BoundTools<'_>,
        messages: &[Value],
        control: &Control<'_>,
    ) -> io::Result<u32> {
        let request = Request::parse(&run.spec.input["planning"])?;
        let key = self.key(run, store)?;
        let mut calls = 0;
        if self.service.load(&key).map_err(io::Error::other)?.is_none() {
            if run.spec.input.get("resume_from").is_some() {
                return Err(stop(
                    "Continuation has no saved plan; create a new assignment",
                ));
            }
            let mut body = if let Some(body) = request.document {
                body
            } else {
                let mut messages = messages.to_vec();
                messages.insert(0,json!({"role":"system","content":"Plan this task without executing it. Return only JSON with objective, steps, and outcomes. Every step has a unique id, title, depends_on array and acceptance_criteria array. Outcomes have unique id, text, required boolean. Preserve all requested outcomes. Maximum 64 steps. Do not call tools."}));
                messages.push(json!({"role":"system","content":format!("Mandatory outcomes from the caller: {}",json!(request.required_outcomes))}));
                control.check()?;
                let reply = model.complete(
                    ModelRequest {
                        messages,
                        tools: vec![],
                        role: Default::default(),
                    },
                    control,
                )?;
                control.check()?;
                reply.validate()?;
                if !reply.tool_calls.is_empty() {
                    return Err(stop(
                        "Planner attempted execution instead of returning a plan",
                    ));
                }
                let text = reply
                    .content
                    .as_str()
                    .ok_or_else(|| io::Error::other("Planner returned no plan"))?
                    .trim();
                let text = text
                    .strip_prefix("```json")
                    .or_else(|| text.strip_prefix("```"))
                    .unwrap_or(text)
                    .trim()
                    .trim_end_matches("```")
                    .trim();
                calls = 1;
                serde_json::from_str::<Value>(text)
                    .map_err(|_| io::Error::other("Planner must return a JSON plan"))?
            };
            core::validate(&body).map_err(io::Error::other)?;
            let mut outcomes = body["outcomes"].as_array().cloned().unwrap_or_default();
            for mut required in request.required_outcomes {
                required["required"] = json!(true);
                if let Some(old) = outcomes.iter_mut().find(|o| o["id"] == required["id"]) {
                    if old["text"] != required["text"] {
                        return Err(stop("Planner changed a mandatory outcome"));
                    }
                    *old = required;
                } else {
                    outcomes.push(required);
                }
            }
            body["outcomes"] = json!(outcomes);
            self.service
                .propose(&key, "runtime:proposal", None, &body)
                .map_err(io::Error::other)?;
        }
        for _ in 0..64 {
            control.check()?;
            let current = self
                .service
                .load(&key)
                .map_err(io::Error::other)?
                .ok_or_else(|| io::Error::other("Plan disappeared"))?;
            if current.approved {
                return Ok(calls);
            }
            let port = ApprovalTools {
                owner: self,
                key: key.clone(),
            };
            let bound = BoundTools {
                base: &port,
                hooks: None,
                completed_calls: Default::default(),
                completed_mutations: Default::default(),
                journal: tools.journal,
                model: tools.model,
                store: tools.store,
                run: tools.run,
                policy: tools.policy,
                approvals: tools.approvals,
            };
            let call = ToolCall {
                id: format!("approve-plan:{}", current.revision),
                name: "approve_plan".into(),
                arguments: json!({"revision":current.revision,"digest":core::digest(&current.document)}),
            };
            match bound.execute(&call, control) {
                Ok(reply) if !reply.is_error => return Ok(calls),
                result => {
                    let now = self
                        .service
                        .load(&key)
                        .map_err(io::Error::other)?
                        .ok_or_else(|| io::Error::other("Plan disappeared"))?;
                    if now.revision != current.revision && !now.approved {
                        continue;
                    }
                    return Err(match result {
                        Err(e) => e,
                        Ok(_) => stop("The plan was not approved; no task tools were dispatched"),
                    });
                }
            }
        }
        Err(stop("Plan kept changing during approval"))
    }
}
pub fn specs() -> Vec<Value> {
    vec![
        json!({"type":"function","function":{"name":"task_plan_read","description":"Read the approved plan, its revision and required outcomes.","parameters":{"type":"object","properties":{},"additionalProperties":false}}}),
        json!({"type":"function","function":{"name":"task_plan_update","description":"Update a plan step at its expected revision. Completion requires successful tool-call ids from this assignment. outcome_ids identifies evidenced required outcomes.","parameters":{"type":"object","properties":{"expected_revision":{"type":"integer","minimum":1},"step_id":{"type":"string"},"status":{"enum":["in_progress","completed","blocked"]},"evidence":{"type":"array","items":{"type":"string"}},"outcome_ids":{"type":"array","items":{"type":"string"}}},"required":["expected_revision","step_id","status"],"additionalProperties":false}}}),
    ]
}
struct ApprovalTools<'a> {
    owner: &'a Coordinator,
    key: Key,
}
impl ToolPort for ApprovalTools<'_> {
    fn discover(&self) -> io::Result<Vec<Value>> {
        Ok(vec![json!({"function":{"name":"approve_plan"}})])
    }
    fn binding(&self, _: &ToolContext, _: &ToolCall) -> io::Result<ToolBinding> {
        let r = self
            .owner
            .service
            .load(&self.key)
            .map_err(io::Error::other)?
            .ok_or_else(|| io::Error::other("Plan missing"))?;
        Ok(ToolBinding {
            binding: "planning.default".into(),
            generation: 1,
            resource: core::digest(&json!(self.key)),
            class: ActionClass::Plan,
            detail: format!(
                "Approve immutable plan revision {}:\n{}\nExact plan document: {}",
                r.revision,
                core::markdown(&r.value()),
                r.value()
            ),
            fingerprint: core::digest(&r.value()),
        })
    }
    fn execute(&self, _: &ToolCall, _: &Control<'_>) -> io::Result<ToolReply> {
        Err(io::Error::other(
            "Plan approval requires an execution scope",
        ))
    }
    fn execute_in(
        &self,
        context: &ToolContext,
        call: &ToolCall,
        control: &Control<'_>,
    ) -> io::Result<ToolReply> {
        control.check()?;
        let r = self
            .owner
            .service
            .approve(
                &self.key,
                &core::digest(&json!([context.turn, call.id])),
                call.arguments["revision"]
                    .as_u64()
                    .ok_or_else(|| io::Error::other("Approval revision missing"))?,
                call.arguments["digest"]
                    .as_str()
                    .ok_or_else(|| io::Error::other("Approval digest missing"))?,
            )
            .map_err(io::Error::other)?;
        Ok(ToolReply::success(r.value().to_string()))
    }
}
pub(crate) struct Tools<'a> {
    pub base: &'a (dyn ToolPort + Send + Sync),
    pub coordinator: &'a Coordinator,
    pub run: &'a RunRecord,
    pub store: &'a dyn RunStore,
    pub workers: Option<&'a Composition>,
    pub key: Key,
}
impl Tools<'_> {
    fn own(name: &str) -> bool {
        matches!(name, "task_plan_read" | "task_plan_update")
    }
    fn allowed(&self, name: &str) -> io::Result<bool> {
        self.workers
            .map(|w| w.permits(self.run, self.store, name))
            .unwrap_or(Ok(true))
    }
    fn scope(&self, context: &ToolContext) -> io::Result<()> {
        if context.session != self.run.spec.session || context.turn != self.run.spec.turn {
            return Err(stop("Plan tool scope does not match its assignment"));
        }
        Ok(())
    }
}
impl ToolPort for Tools<'_> {
    fn discover(&self) -> io::Result<Vec<Value>> {
        self.discover_in(&ToolContext {
            session: self.run.spec.session.clone(),
            turn: self.run.spec.turn.clone(),
        })
    }
    fn discover_in(&self, context: &ToolContext) -> io::Result<Vec<Value>> {
        self.scope(context)?;
        let mut specs = self.base.discover_in(context)?;
        if specs.iter().any(|s| {
            Self::own(s["function"]["name"].as_str().unwrap_or(""))
                || s["function"]["name"] == "approve_plan"
        }) {
            return Err(stop(
                "Configured tool collides with the planning capability",
            ));
        }
        for spec in self::specs() {
            if self.allowed(spec["function"]["name"].as_str().unwrap())? {
                specs.push(spec);
            }
        }
        Ok(specs)
    }
    fn dispatch_access_in(
        &self,
        context: &ToolContext,
        call: &ToolCall,
    ) -> io::Result<DispatchAccess> {
        self.scope(context)?;
        if Self::own(&call.name) {
            Ok(DispatchAccess::Resource(format!(
                "planning:{}",
                core::digest(&json!(self.key))
            )))
        } else {
            self.base.dispatch_access_in(context, call)
        }
    }
    fn binding(&self, context: &ToolContext, call: &ToolCall) -> io::Result<ToolBinding> {
        self.scope(context)?;
        if !Self::own(&call.name) {
            return self.base.binding(context, call);
        }
        if !self.allowed(&call.name)? {
            return Err(stop("Specialist policy excludes this planning tool"));
        }
        let r = self
            .coordinator
            .service
            .load(&self.key)
            .map_err(io::Error::other)?
            .ok_or_else(|| io::Error::other("No plan"))?;
        Ok(ToolBinding {
            binding: "planning.default".into(),
            generation: 1,
            resource: core::digest(&json!(self.key)),
            class: if self
                .workers
                .map(|w| w.review(self.run, self.store, &call.name))
                .transpose()?
                .unwrap_or(false)
            {
                ActionClass::Restricted
            } else {
                ActionClass::ReadOnly
            },
            detail: "Update local plan metadata using confirmed task evidence".into(),
            fingerprint: core::digest(&json!([self.key, r.approved])),
        })
    }
    fn mutation_binding(
        &self,
        context: &ToolContext,
        call: &ToolCall,
    ) -> io::Result<Option<ToolBinding>> {
        self.scope(context)?;
        if Self::own(&call.name) {
            if !self.allowed(&call.name)? {
                return Err(stop("Specialist policy excludes planning"));
            }
            Ok(None)
        } else {
            self.base.mutation_binding(context, call)
        }
    }
    fn execute(&self, call: &ToolCall, control: &Control<'_>) -> io::Result<ToolReply> {
        self.execute_in(
            &ToolContext {
                session: self.run.spec.session.clone(),
                turn: self.run.spec.turn.clone(),
            },
            call,
            control,
        )
    }
    fn execute_in(
        &self,
        context: &ToolContext,
        call: &ToolCall,
        control: &Control<'_>,
    ) -> io::Result<ToolReply> {
        self.scope(context)?;
        if !Self::own(&call.name) {
            return self.base.execute_in(context, call, control);
        }
        if !self.allowed(&call.name)? {
            return Err(stop("Specialist policy excludes this planning tool"));
        }
        control.check()?;
        let r = if call.name == "task_plan_read" {
            self.coordinator
                .service
                .load(&self.key)
                .map_err(io::Error::other)?
                .ok_or_else(|| io::Error::other("No plan"))?
        } else {
            self.coordinator
                .service
                .update(
                    &self.key,
                    &format!("{}:{}", context.turn, call.id),
                    &call.arguments,
                    &Evidence {
                        run: self.run,
                        store: self.store,
                        key: &self.key,
                    },
                )
                .map_err(io::Error::other)?
        };
        Ok(ToolReply::success(r.value().to_string()))
    }
    fn execute_authorized_in(
        &self,
        context: &ToolContext,
        call: &ToolCall,
        binding: &ToolBinding,
        control: &Control<'_>,
    ) -> io::Result<ToolReply> {
        self.scope(context)?;
        if Self::own(&call.name) {
            if self.binding(context, call)? != *binding {
                return Err(stop("Plan authority changed"));
            }
            self.execute_in(context, call, control)
        } else {
            self.base
                .execute_authorized_in(context, call, binding, control)
        }
    }
}
struct Evidence<'a> {
    run: &'a RunRecord,
    store: &'a dyn RunStore,
    key: &'a Key,
}
impl core::EvidencePort for Evidence<'_> {
    fn successful(&self, key: &Key, call: &str) -> core::Result<bool> {
        if key != self.key || key.session != self.run.spec.session {
            return Ok(false);
        }
        let mut run = self.run.clone();
        for _ in 0..128 {
            if let Some(effect) = self
                .store
                .effect_get(&run.spec.turn, &format!("tool:{call}"))
                .map_err(|e| core::failure(core::Code::Unavailable, e.to_string()))?
            {
                let Some(reply) = effect.receipt else {
                    return Ok(false);
                };
                let name = reply["_tool_name"].as_str().unwrap_or("");
                return Ok(reply["is_error"] == false
                    && !name.is_empty()
                    && !name.starts_with("task_plan_")
                    && !name.starts_with("agent_")
                    && !matches!(name, "approve_plan" | "project_hook"));
            }
            let Some(id) = run.spec.input["resume_from"].as_str() else {
                return Ok(false);
            };
            run = self
                .store
                .get(id)
                .map_err(|e| core::failure(core::Code::Unavailable, e.to_string()))?
                .ok_or_else(|| {
                    core::failure(core::Code::NotFound, "Plan evidence source missing")
                })?;
            if run.spec.session != key.session {
                return Ok(false);
            }
        }
        Err(core::failure(
            core::Code::Invalid,
            "Plan evidence ancestry exceeded",
        ))
    }
}
pub(crate) struct Model<'a> {
    pub base: &'a dyn ModelPort,
    pub coordinator: &'a Coordinator,
    pub key: Key,
    pub gate: std::sync::Mutex<(u64, u32)>,
}
impl ModelPort for Model<'_> {
    fn accounts_invocations(&self) -> bool { self.base.accounts_invocations() }
    fn context_window(&self) -> Option<u64> { self.base.context_window() }
    fn complete(
        &self,
        mut request: ModelRequest,
        control: &Control<'_>,
    ) -> io::Result<AssistantMessage> {
        let before = self
            .coordinator
            .service
            .load(&self.key)
            .map_err(io::Error::other)?
            .ok_or_else(|| io::Error::other("Plan missing"))?;
        request.messages.push(json!({"role":"system","content":format!("Execute this approved plan. Keep all required outcomes. Use task_plan_update with successful tool-call evidence before claiming completion. Current plan: {}",before.value())}));
        let can_read = request
            .tools
            .iter()
            .any(|t| t["function"]["name"] == "task_plan_read");
        let reply = self.base.complete(request, control)?;
        // Malformed tool calls pass through to the runtime, which answers them
        // with `invalid_tool_call` without dispatching them.
        reply.validate_envelope()?;
        if reply.tool_calls.is_empty() {
            let current = self
                .coordinator
                .service
                .load(&self.key)
                .map_err(io::Error::other)?
                .ok_or_else(|| io::Error::other("Plan missing"))?;
            if !current.complete() {
                let mut gate = self
                    .gate
                    .lock()
                    .map_err(|_| io::Error::other("Plan completion gate unavailable"))?;
                if gate.0 != current.revision {
                    *gate = (current.revision, 0);
                }
                gate.1 += 1;
                if gate.1 > 4 {
                    return Err(stop(
                        "The approved plan is incomplete and no evidence-backed progress was made",
                    ));
                }

                if !can_read {
                    return Err(stop("Approved plan remains incomplete and specialist policy excludes plan progress"));
                }
                return Ok(AssistantMessage{ continuation: None, content:json!("The approved plan still has unfinished steps or required outcomes. Inspect it and continue using confirmed evidence."),tool_calls:vec![ToolCall{id:format!("plan-gate:{}",uuid::Uuid::new_v4()),name:"task_plan_read".into(),arguments:json!({})}]});
            }
        }
        Ok(reply)
    }
}
