//! Optional standalone decision adapter. Configuration is operator-owned, never a tool.
use crate::{browser::Browsers, cdp::Result, protocol::Request};
use kageha_browser_decisions::{
    Advice, Audit, DecisionConfig, Guidance, Mode, Models, Observation, Policy, Purpose, Scope, Session,
};
use kageha_contracts::{
    execution::Control,
    policy::{ActionClass, ApprovalRequest, Authority, ReviewEvidence, ReviewVerdict},
};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
    sync::{atomic::AtomicBool, Arc, Mutex},
    time::{Duration, Instant},
};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    version: u32,
    #[serde(default)]
    policy: Policy,
    #[serde(default)]
    decision: Option<DecisionConfig>,
    #[serde(default)]
    models: BTreeMap<String, Provider>,
    #[serde(default)]
    contexts: Vec<Context>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Provider {
    url: String,
    model: String,
    #[serde(default = "protocol")]
    protocol: String,
    api_key_env: Option<String>,
}
fn protocol() -> String {
    "openai_compat".into()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Context {
    workspace: String,
    client: String,
    run: String,
    intent: String,
    #[serde(default)]
    values: BTreeMap<String, Value>,
    policy: Option<Policy>,
    #[serde(default)]
    decision: Option<DecisionConfig>,
}
struct Run {
    session: Arc<Session>,
    values: BTreeMap<String, Value>,
    intent: String,
}
type Jobs = Arc<Mutex<BTreeMap<(String, String, String), Arc<AtomicBool>>>>;
struct Job {
    key: (String, String, String),
    flag: Arc<AtomicBool>,
    jobs: Jobs,
}
impl Drop for Job {
    fn drop(&mut self) {
        self.flag.store(true, std::sync::atomic::Ordering::Release);
        if let Ok(mut jobs) = self.jobs.lock() {
            jobs.remove(&self.key);
        }
    }
}
pub struct Runtime {
    runs: BTreeMap<(String, String), Run>,
    require_review: bool,
    audit: Arc<Events>,
    jobs: Jobs,
}
struct Events(Mutex<std::fs::File>);
impl Audit for Events {
    fn record(&self, scope: &Scope, purpose: Purpose, event: &Value) -> io::Result<()> {
        let mut file = self
            .0
            .lock()
            .map_err(|_| io::Error::other("Decision journal unavailable"))?;
        if file.metadata()?.len() > 16 * 1024 * 1024 {
            return Err(io::Error::other("Decision journal capacity exceeded"));
        }
        serde_json::to_writer(
            &mut *file,
            &json!({"scope":scope,"purpose":purpose,"event":event}),
        )?;
        file.write_all(b"\n")?;
        file.sync_data()
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    run_id: String,
    purpose: Purpose,
    observation_id: String,
}
impl Runtime {
    pub fn load(path: &Path, root: &Path) -> io::Result<Self> {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.mode() & 0o077 != 0
            || metadata.uid() != crate::state::uid()
            || metadata.len() > 262144
        {
            return Err(io::Error::other(
                "Decision config must be a private regular file, at most 256 KiB",
            ));
        }
        let config: Config = serde_json::from_slice(&fs::read(path)?)?;
        config.policy.validate()?;
        if config.version != 1 || config.models.len() > 32 || config.contexts.len() > 64 {
            return Err(io::Error::other("Invalid decision config limits"));
        }
        let audit = Arc::new(Events(Mutex::new(
            OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(root.join("browser-decisions.jsonl"))?,
        )));
        let mut models = Models::new();
        let mut backends = kageha_browser_decisions::typed::Backends::new();
        for (alias, p) in config.models {
            if alias.is_empty() || alias.len() > 256 {
                return Err(io::Error::other("Invalid model alias"));
            }
            let key = if let Some(name) = p.api_key_env {
                if name.is_empty()
                    || name.len() > 128
                    || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                {
                    return Err(io::Error::other(
                        "Invalid model credential environment reference",
                    ));
                }
                let Ok(key) = std::env::var(name) else {
                    continue;
                };
                if key.is_empty() {
                    continue;
                }
                key
            } else {
                String::new()
            };
            let typed = kageha_models::decisions::is_typed_protocol(&p.protocol);
            let model = kageha_models::HttpModel::new(vec![kageha_models::Provider {
                url: p.url,
                model: p.model,
                protocol: p.protocol,
                key,
            }])?;
            if typed {
                backends.insert(
                    alias,
                    Arc::new(kageha_models::decisions::HttpDecisions::new(model)?)
                        as Arc<dyn kageha_browser_decisions::typed::DecisionPort + Send + Sync>,
                );
            } else {
                models.insert(alias, Arc::new(model));
            }
        }
        // All configuration is operator-owned; RPC/tool parameters cannot create providers,
        // replace context intent/facts, or change decision thresholds.
        for decision in config.decision.iter().chain(config.contexts.iter().filter_map(|c|c.decision.as_ref())) {
            if !backends.contains_key(&decision.decision_model) {
                backends.insert(decision.decision_model.clone(),Arc::new(kageha_models::decisions::HttpDecisions::from_env(&decision.decision_model)?));
            }
        }
        let root_policy=match &config.decision {Some(d)=>d.apply_to(&config.policy)?,None=>config.policy.clone()};
        let mut require_review = root_policy.effective(Purpose::Review).mode == Mode::Active;
        let mut runs = BTreeMap::new();
        for context in config.contexts {
            let scope = Scope {
                workspace: context.workspace.clone(),
                run: context.run,
                client: context.client.clone(),
            };
            scope.validate()?;
            if context.intent.trim().is_empty() || context.intent.len() > 16000 {
                return Err(io::Error::other(
                    "A bounded operator intent is required for each decision context",
                ));
            }
            let base=context.policy.unwrap_or_else(||root_policy.clone());
            let policy=match context.decision {Some(d)=>d.apply_to(&base)?,None=>base};
            require_review |= policy.effective(Purpose::Review).mode == Mode::Active;
            let session = Session::new(scope, policy, models.clone())?
                .with_decision_backends(backends.clone())
                .with_audit(audit.clone());
            if runs
                .insert(
                    (context.workspace, context.client),
                    Run {
                        session: Arc::new(session),
                        intent: context.intent,
                        values: context.values,
                    },
                )
                .is_some()
            {
                return Err(io::Error::other(
                    "Duplicate decision workspace/client context",
                ));
            }
        }
        Ok(Self {
            runs,
            require_review,
            audit,
            jobs: Arc::new(Mutex::new(BTreeMap::new())),
        })
    }
    fn job(&self, request: &Request) -> Result<Job> {
        let key = (
            request.workspace_id.clone(),
            request.client_id.clone(),
            request.request_id.clone(),
        );
        let mut jobs = self.jobs.lock().map_err(|_| "decision_unavailable")?;
        if jobs.contains_key(&key) {
            return Err("decision_already_running");
        }
        if jobs.len() >= 64 {return Err("decision_capacity_exceeded");}
        let flag = Arc::new(AtomicBool::new(false));
        jobs.insert(key.clone(), flag.clone());
        Ok(Job {
            key,
            flag,
            jobs: self.jobs.clone(),
        })
    }
    pub fn cancel(&self, request: &Request, id: &str) -> bool {
        self.jobs
            .lock()
            .ok()
            .and_then(|jobs| {
                jobs.get(&(
                    request.workspace_id.clone(),
                    request.client_id.clone(),
                    id.into(),
                ))
                .cloned()
            })
            .is_some_and(|flag| {
                flag.store(true, std::sync::atomic::Ordering::Release);
                true
            })
    }
    pub fn info(&self, request: &Request) -> Value {
        let run = self
            .runs
            .get(&(request.workspace_id.clone(), request.client_id.clone()));
        let policy=run.map(|r|r.session.action_settings());
        let confidence=run.and_then(|r|r.session.action_confidence_cutoff());
        json!({"version":1,"configured":true,"owner":"browser-service","context_ready":run.is_some(),"run_id":run.map(|r|&r.session.scope.run),"purposes":["browser_guidance","recovery","browser_review","browser_action"],"mode":run.map(|r|r.session.mode(Purpose::Guidance)),"review_mode":run.map(|r|r.session.mode(Purpose::Review)),"action_mode":policy.as_ref().map(|p|p.mode),"decision_model":policy.as_ref().map(|p|&p.model),"score_metric":if confidence.is_some(){"provider_confidence"}else{"selected_probability"},"cutoff":confidence.or_else(||policy.as_ref().map(|p|p.threshold)),"model_execution_has_tools":false})
    }
    pub async fn decide(&self, request: &Request, browsers: &Browsers) -> Result<Value> {
        let params: Params =
            serde_json::from_value(request.params.clone()).map_err(|_| "invalid_params")?;
        if params.purpose == Purpose::Review {
            return Err("decision_review_is_not_a_tool");
        }
        let run = self
            .runs
            .get(&(request.workspace_id.clone(), request.client_id.clone()))
            .ok_or("decision_context_required")?;
        if params.run_id != run.session.scope.run {
            return Err("decision_run_changed");
        }
        let (selection, snapshot) = browsers
            .decision_evidence(request, Some(&params.observation_id))
            .await?;
        let guidance = guidance(request, params.purpose, &selection, &snapshot);
        let mut pending = false;
        if params.purpose == Purpose::Action {
            let mut query = request.clone();
            query.method = "pending_actions".into();
            query.params =
                json!({"browser_id":selection["browser_id"],"target_id":selection["target_id"]});
            let receipts = browsers.dispatch(&query).await?;
            pending = receipts["receipts"]
                .as_array()
                .is_none_or(|v| !v.is_empty());
        }
        let job = self.job(request)?;
        let session = run.session.clone();
        let intent = run.intent.clone();
        let values = run.values.clone();
        let action_purpose = params.purpose == Purpose::Action;
        let observation_id = params.observation_id.clone();
        let flag = job.flag.clone();
        let deadline = Instant::now()
            + Duration::from_millis(
                request
                    .deadline_ms
                    .saturating_sub(crate::protocol::now_ms()),
            );
        let worker = isolated_worker(move || {
            let control = Control {
                cancelled: &flag,
                deadline: Some(deadline),
            };
            if action_purpose {
                let main = snapshot["main_frame_id"].as_str().unwrap_or("");

                let action = kageha_browser_decisions::routing::Request {
                    observation_id,
                    observation: Observation {
                        resource: snapshot["target_id"].as_str().unwrap_or("").into(),
                        binding: selection["binding_id"].as_str().unwrap_or("").into(),
                        document: snapshot["frame_scopes"][main]["document_id"]
                            .as_str()
                            .unwrap_or("")
                            .into(),
                        service_epoch: guidance.observation.service_epoch.clone(),
                        evidence: kageha_browser_decisions::page_evidence(&snapshot),
                    },
                    controls: snapshot["controls"].as_array().cloned().unwrap_or_default(),
                    values,
                    allowed_actions: ["click", "fill", "check", "select", "press"]
                        .into_iter()
                        .map(str::to_owned)
                        .collect(),
                    trusted_click_targets: Default::default(),
                    pending_actions: pending,
                };
                serde_json::to_value(session.route(&intent, &action, &control)?)
                    .map_err(io::Error::other)
            } else {
                serde_json::to_value(session.guide(&intent, &guidance, &control)?)
                    .map_err(io::Error::other)
            }
        });
        let outcome = worker
            .await
            .map_err(|_| "decision_unavailable")?
            .map_err(|_| "decision_unavailable")?;
        serde_json::to_value(outcome).map_err(|_| "decision_unavailable")
    }
    pub async fn review(&self, request: &Request, browsers: &Browsers) -> Result<()> {
        if !reviewed_method(request) {
            return Ok(());
        }
        let Some(run) = self
            .runs
            .get(&(request.workspace_id.clone(), request.client_id.clone()))
        else {
            return if self.require_review {
                Err("browser_decision_approval_required")
            } else {
                Ok(())
            };
        };
        let mode = run.session.mode(Purpose::Review);
        if mode == Mode::Off {
            return Ok(());
        }
        // Opaque programs and coordinate actions cannot be approved from a model's
        // interpretation of their source. A host must explicitly authorize them.
        if matches!(
            request.method.as_str(),
            "run_script" | "panel_action" | "batch_observed"
        ) {
            let audit=self.audit.record(&run.session.scope,Purpose::Review,&json!({"verdict":"ask","reason":"typed_action_review_required","applied":mode==Mode::Active}));
            return if mode == Mode::Active {
                audit.map_err(|_| "decision_journal_unavailable")?;
                Err("browser_decision_approval_required")
            } else {
                Ok(())
            };
        }
        let (selection, snapshot) = browsers
            .decision_evidence(request, request.params["observation_id"].as_str())
            .await?;
        let binding = selection["binding_id"]
            .as_str()
            .ok_or("browser_binding_changed")?
            .to_owned();
        let resource = selection["target_id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .or_else(|| selection["browser_id"].as_str())
            .ok_or("browser_binding_changed")?
            .to_owned();
        // OTPs have too little entropy for a digest to keep them secret. Bind the
        // review to their destination and control, not to a recoverable hash.
        let mut review_params = request.params.clone();
        if review_params["otp_origin"].is_string() {
            if let Some(params) = review_params.as_object_mut() {
                params.remove("text");
            }
        }
        let digest = data_encoding::HEXLOWER.encode(&Sha256::digest(review_params.to_string()));
        let control = snapshot["controls"]
            .as_array()
            .and_then(|v| v.iter().find(|c| c["ref"] == request.params["target"]));
        let summary=json!({"method":request.method,"destination_origin":request.params["url"].as_str().and_then(|s|url::Url::parse(s).ok()).map(|u|u.origin().ascii_serialization()),"control":control.map(|c|json!({"name":c["name"],"role":c["role"],"secure_input":c["secure_input"]}))}).to_string();
        let proposed = ApprovalRequest {
            authority: Authority {
                session: run.session.scope.workspace.clone(),
                turn: run.session.scope.run.clone(),
                binding: binding.clone(),
                generation: request.authority.as_ref().map_or(1, |a| a.generation),
                resource,
                input_digest: digest.clone(),
            },
            action: request.method.clone(),
            class: if request.method == "fill_observed" {
                ActionClass::RoutineFill
            } else {
                ActionClass::Mutation
            },
            detail: summary.clone(),
        };
        let job = self.job(request)?;
        let evidence = vec![ReviewEvidence {
            source: "native-browser".into(),
            digest,
            summary,
        }];
        let intent = run.intent.clone();
        let session = run.session.clone();
        let flag = job.flag.clone();
        let deadline = Instant::now()
            + Duration::from_millis(
                request
                    .deadline_ms
                    .saturating_sub(crate::protocol::now_ms()),
            );
        let decision = isolated_worker(move || {
            session.review(
                &proposed,
                &intent,
                &evidence,
                &Control {
                    cancelled: &flag,
                    deadline: Some(deadline),
                },
            )
        })
        .await;
        if job.flag.load(std::sync::atomic::Ordering::Acquire) {
            return Err("decision_cancelled");
        }
        if mode == Mode::Shadow {
            return Ok(());
        }
        let decision = decision
            .map_err(|_| "decision_unavailable")?
            .map_err(|_| "browser_decision_approval_required")?;
        let now = browsers
            .decision_evidence(request, request.params["observation_id"].as_str())
            .await?;
        if now.0["binding_id"] != binding {
            return Err("browser_binding_changed");
        }
        if mode == Mode::Shadow {
            return Ok(());
        }
        match decision.verdict {
            ReviewVerdict::Allow => Ok(()),
            ReviewVerdict::Deny => Err("browser_decision_denied"),
            ReviewVerdict::Ask => Err("browser_decision_approval_required"),
        }
    }
}
// Model ports own their transport runtime. Tokio's spawn_blocking retains an
// entered Handle, so use a bounded, cancellation/deadline-aware OS worker instead.
async fn isolated_worker<T,F>(work:F)->Result<io::Result<T>>
where T:Send+'static,F:FnOnce()->io::Result<T>+Send+'static {
    let (sender,receiver)=tokio::sync::oneshot::channel();
    std::thread::Builder::new().name("browser-decision".into()).spawn(move||{let _=sender.send(work());}).map_err(|_|"decision_unavailable")?;
    receiver.await.map_err(|_|"decision_unavailable")
}

fn reviewed_method(request: &Request) -> bool {
    if request.method == "panel_action" && request.params["input"]["action"] == "layout" {
        return false;
    }
    crate::actions::DISPATCH_METHODS.contains(&request.method.as_str())
        || matches!(
            request.method.as_str(),
            "run_script" | "panel_action" | "open_tab"
        )
}
fn guidance(request: &Request, purpose: Purpose, selection: &Value, snapshot: &Value) -> Guidance {
    let main = snapshot["main_frame_id"].as_str().unwrap_or("");
    Guidance {
        version: 1,
        purpose,
        observation: Observation {
            resource: selection["target_id"].as_str().unwrap_or("").into(),
            binding: selection["binding_id"].as_str().unwrap_or("").into(),
            document: snapshot["frame_scopes"][main]["document_id"]
                .as_str()
                .unwrap_or("")
                .into(),
            service_epoch: request.service_epoch.clone().unwrap_or_default(),
            evidence: kageha_browser_decisions::page_evidence(snapshot),
        },
        allowed: [
            Advice::UseObservedControls,
            Advice::ReadContent,
            Advice::Wait,
            Advice::InspectBlocker,
            Advice::Reconcile,
        ]
        .into_iter()
        .collect::<BTreeSet<_>>(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn model_worker_does_not_enter_the_calling_async_runtime() {
        assert!(tokio::runtime::Handle::try_current().is_ok());
        let outside=isolated_worker(||Ok(tokio::runtime::Handle::try_current().is_err())).await.unwrap().unwrap();
        assert!(outside,"Model ports must not inherit an entered Tokio Handle");
    }
    #[test]
    fn tool_parameters_cannot_replace_host_model_cutoff_intent_or_values() {
        for field in ["decision_model","confidence_cutoff","intent","values"] {
            let mut value=json!({"run_id":"run","purpose":"browser_action","observation_id":"observation"});
            value[field]=json!("untrusted override");
            assert!(serde_json::from_value::<Params>(value).is_err());
        }
    }
}
