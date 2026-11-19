//! Independent specialist composition over the same supervisor, tool and workspace ports.
mod workspace;
use crate::ExecutionHost;
use kageha_contracts::{execution::*, policy::ActionClass, ErrorCode, Result, ServiceError};
use kageha_runtime::lifecycle::*;
use kageha_workspace::worker::{Manager, Options};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    io,
    path::PathBuf,
    sync::{Arc, OnceLock, Weak},
    time::{Duration, Instant},
};
pub use workspace::{LocalWorkspace, WorkspacePort};
fn failure(code: ErrorCode, m: impl Into<String>) -> ServiceError {
    ServiceError::new(code, m)
}
fn io_error(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}
fn hash(v: &Value) -> String {
    format!("{:x}", Sha256::digest(v.to_string().as_bytes()))
}
fn identifier(s: &str) -> bool {
    !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control)
}
fn tool_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control)
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Specialist {
    pub id: String,
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
}
/// Server-owned configuration, never sourced from model arguments or run payloads.
pub struct Config {
    pub source: PathBuf,
    pub journal: PathBuf,
    pub git: PathBuf,
    pub document_worker: Option<PathBuf>,
    pub local_files: bool,
    pub specialists: Vec<Specialist>,
}
struct Resolved {
    specialist: Specialist,
    fingerprint: String,
    policy: Value,
    definition: Option<kageha_agents::Definition>,
}
pub struct Composition {
    pub workspace: Arc<dyn WorkspacePort>,
    catalog: BTreeMap<String, Specialist>,
    host: OnceLock<Weak<ExecutionHost>>,
    profiles: Option<Arc<dyn kageha_agents::Port>>,
    resources: Option<crate::activation::Resources>,
}
impl Composition {
    pub fn new(config: Config) -> Result<Arc<Self>> {
        let manager = Manager::new(&config.source, &config.journal, &config.git)
            .map_err(|e| failure(ErrorCode::Invalid, e.to_string()))?;
        Self::with_workspace(
            Arc::new(LocalWorkspace::new(
                manager,
                config.document_worker,
                config.local_files,
            )),
            config.specialists,
        )
    }
    pub fn with_workspace(
        workspace: Arc<dyn WorkspacePort>,
        specialists: Vec<Specialist>,
    ) -> Result<Arc<Self>> {
        if specialists.len() > 128 {
            return Err(failure(ErrorCode::Invalid, "Too many specialists"));
        }
        let mut catalog = BTreeMap::new();
        for s in specialists {
            if !identifier(&s.id)
                || s.instructions.len() > 65536
                || s.allowed_tools
                    .as_ref()
                    .is_some_and(|v| v.len() > 512 || v.iter().any(|n| !tool_name(n)))
            {
                return Err(failure(ErrorCode::Invalid, "Invalid specialist definition"));
            }
            if catalog.insert(s.id.clone(), s).is_some() {
                return Err(failure(ErrorCode::Invalid, "Duplicate specialist identity"));
            }
        }
        Ok(Arc::new(Self {
            workspace,
            catalog,
            host: OnceLock::new(),
            profiles: None,
            resources: None,
        }))
    }
    /// Add a portable profile port without changing the legacy configuration DTO.
    pub fn with_profiles(
        workspace: Arc<dyn WorkspacePort>,
        specialists: Vec<Specialist>,
        profiles: Arc<dyn kageha_agents::Port>,
    ) -> Result<Arc<Self>> {
        let mut instance = Self::with_workspace(workspace, specialists)?;
        let values = kageha_agents::list(profiles.as_ref())
            .map_err(|e| failure(ErrorCode::Invalid, e.to_string()))?;
        if values.len() + instance.catalog.len() > 128
            || values.iter().any(|d| instance.catalog.contains_key(d.id()))
        {
            return Err(failure(
                ErrorCode::Conflict,
                "Profile and legacy specialist identities overlap",
            ));
        }
        Arc::get_mut(&mut instance)
            .expect("new unshared composition")
            .profiles = Some(profiles);
        Ok(instance)
    }
    pub fn new_with_profiles(
        config: Config,
        profiles: Arc<dyn kageha_agents::Port>,
    ) -> Result<Arc<Self>> {
        let manager = Manager::new(&config.source, &config.journal, &config.git)
            .map_err(|e| failure(ErrorCode::Invalid, e.to_string()))?;
        Self::with_profiles(
            Arc::new(LocalWorkspace::new(
                manager,
                config.document_worker,
                config.local_files,
            )),
            config.specialists,
            profiles,
        )
    }
    fn resolve(&self, id: &str) -> io::Result<Option<Resolved>> {
        let profile = self
            .profiles
            .as_ref()
            .map(|p| kageha_agents::get(p.as_ref(), id))
            .transpose()?
            .flatten();
        if profile.is_some() && self.catalog.contains_key(id) {
            return Err(io_error("Profile identity conflicts with legacy catalog"));
        }
        if let Some(definition) = profile {
            return Ok(Some(Resolved {
                specialist: Specialist {
                    id: id.into(),
                    instructions: definition.instructions().into(),
                    allowed_tools: None,
                },
                fingerprint: definition.fingerprint(),
                policy: definition.policy().clone(),
                definition: Some(definition),
            }));
        }
        Ok(self.catalog.get(id).map(|s| Resolved {
            specialist: s.clone(),
            fingerprint: hash(&json!(s)),
            policy: Value::Null,
            definition: None,
        }))
    }
    pub fn configure_resources(
        this: &mut Arc<Self>,
        resources: crate::activation::Resources,
    ) -> Result<()> {
        Arc::get_mut(this)
            .ok_or_else(|| failure(ErrorCode::Conflict, "Worker composition is already shared"))?
            .resources = Some(resources);
        Ok(())
    }

    fn unsupported(&self, definition: &kageha_agents::Definition) -> Vec<String> {
        match &self.resources {
            Some(resources) => resources
                .resolve(definition, None)
                .err()
                .map(|e| {
                    if e.to_string().starts_with("model_preferences:") {
                        vec!["model_preferences".into()]
                    } else {
                        vec![e.to_string()]
                    }
                })
                .unwrap_or_default(),
            None => definition.unsupported(&Default::default()),
        }
    }

    pub fn profile_settings(&self) -> io::Result<Vec<Value>> {
        let Some(port) = &self.profiles else {
            return Ok(vec![]);
        };
        Ok(kageha_agents::list(port.as_ref())?
            .into_iter()
            .map(|d| {
                json!({
                    "id":d.id(), "fingerprint":d.fingerprint(), "effective":d.effective(),
                    "unsupported_execution_settings":self.unsupported(&d)
                })
            })
            .collect())
    }

    fn resolve_activation(
        &self,
        spec: &RunSpec,
        store: &dyn RunStore,
        d: &kageha_agents::Definition,
    ) -> Result<Option<crate::activation::Active>> {
        let Some(resources) = &self.resources else {
            d.require(&Default::default())
                .map_err(|e| failure(ErrorCode::Unsupported, e.to_string()))?;
            return Ok(None);
        };
        let parent = spec
            .parent_run
            .as_ref()
            .map(|id| {
                store
                    .get(id)?
                    .ok_or_else(|| failure(ErrorCode::Invalid, "Parent run missing"))
            })
            .transpose()?;
        let empty = Value::Null;
        let parent_pin = parent
            .as_ref()
            .map(|p| p.spec.input.get("profile_activation").unwrap_or(&empty));
        resources
            .resolve(d, parent_pin)
            .map(|a| {
                let has_models = a.executor.is_some()
                    || a.planner.is_some()
                    || a.fast_worker.is_some()
                    || a.embedding.is_some();
                (resources.skills.is_some() || resources.knowledge.is_some() || has_models)
                    .then_some(a)
            })
            .map_err(|e| failure(ErrorCode::Unsupported, e.to_string()))
    }

    pub(crate) fn activation(
        &self,
        run: &RunRecord,
        store: &dyn RunStore,
        control: &Control<'_>,
    ) -> Result<(Option<crate::activation::Active>, Vec<Value>)> {
        let source = self.origin(&run.spec, store)?;
        let resolved = self.resolve(&source.agent).map_err(crate::error)?;
        if match &resolved {
            Some(r) => source.input["specialist_binding"] != r.fingerprint,
            None => source.input.get("specialist_binding").is_some(),
        } {
            return Err(failure(
                ErrorCode::Conflict,
                "Pinned specialist configuration changed",
            ));
        }
        let active = resolved
            .and_then(|r| r.definition)
            .map(|d| self.resolve_activation(&source, store, &d))
            .transpose()?
            .flatten();
        if source.input.get("profile_activation") != active.as_ref().map(|a| &a.pin) {
            return Err(failure(
                ErrorCode::Conflict,
                "Pinned profile resources changed",
            ));
        }
        if run.spec.input.get("resume_from").is_some() {
            return Ok((active, vec![]));
        }
        let messages = self
            .resources
            .as_ref()
            .zip(active.as_ref())
            .map(|(r, a)| r.context(a, &source.objective, control))
            .transpose()
            .map_err(crate::error)?
            .unwrap_or_default();
        Ok((active, messages))
    }

    pub fn catalog_entries(&self) -> io::Result<Vec<Specialist>> {
        let mut result = self.catalog();
        if let Some(port) = &self.profiles {
            for profile in kageha_agents::list(port.as_ref())? {
                if self.catalog.contains_key(profile.id()) {
                    return Err(io_error("Profile identity conflicts with legacy catalog"));
                }
                result.push(Specialist {
                    id: profile.id().into(),
                    instructions: profile.instructions().into(),
                    allowed_tools: None,
                });
            }
        }
        if result.len() > 128 {
            return Err(io_error("Too many combined specialist definitions"));
        }
        result.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(result)
    }
    fn eligible_catalog(&self) -> io::Result<Vec<Specialist>> {
        let mut result = Vec::new();
        for entry in self.catalog_entries()? {
            let current = self
                .resolve(&entry.id)?
                .ok_or_else(|| io_error("Specialist disappeared"))?;
            if current
                .definition
                .as_ref()
                .is_none_or(|d| self.unsupported(d).is_empty())
            {
                result.push(current.specialist);
            }
        }
        Ok(result)
    }
    pub(crate) fn instructions(
        &self,
        run: &RunRecord,
        store: &dyn RunStore,
    ) -> io::Result<Option<String>> {
        let source = self.origin(&run.spec, store).map_err(io_error)?;
        let resolved = self.resolve(&source.agent)?;
        if let Some(r) = &resolved {
            if source.input["specialist_binding"] != r.fingerprint {
                return Err(io_error("Pinned specialist configuration changed"));
            }
        }
        Ok(resolved
            .map(|r| r.specialist.instructions)
            .filter(|s| !s.is_empty()))
    }
    pub(crate) fn attach(&self, host: &Arc<ExecutionHost>) -> Result<()> {
        self.host.set(Arc::downgrade(host)).map_err(|_| {
            failure(
                ErrorCode::Conflict,
                "Worker composition is already attached",
            )
        })
    }
    fn host(&self) -> Result<Arc<ExecutionHost>> {
        self.host
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| failure(ErrorCode::Unavailable, "Worker host unavailable"))
    }
    /// Legacy configuration DTOs; catalog_entries includes portable profiles too.
    pub fn catalog(&self) -> Vec<Specialist> {
        self.catalog.values().cloned().collect()
    }
    fn origin(&self, spec: &RunSpec, store: &dyn RunStore) -> Result<RunSpec> {
        let mut source = spec.clone();
        let mut visited = HashSet::new();
        for _ in 0..128 {
            if !visited.insert(source.id.clone()) {
                return Err(failure(ErrorCode::Conflict, "Cyclic continuation history"));
            }
            let Some(id) = source.input["resume_from"].as_str() else {
                return Ok(source);
            };
            source = store
                .get(id)?
                .ok_or_else(|| failure(ErrorCode::Conflict, "Continuation source missing"))?
                .spec;
        }
        Err(failure(
            ErrorCode::Conflict,
            "Continuation chain exceeds limit",
        ))
    }
    fn key(&self, spec: &RunSpec, store: &dyn RunStore) -> Result<String> {
        Ok(hash(&json!(self.origin(spec, store)?.id))[..32].into())
    }
    pub(crate) fn normalize(&self, spec: &RunSpec, store: &dyn RunStore) -> Result<RunSpec> {
        let mut spec = spec.clone();
        let source_binding = self
            .workspace
            .binding()
            .map_err(|e| failure(ErrorCode::Unavailable, e.to_string()))?;
        if spec
            .input
            .get("workspace_binding")
            .is_some_and(|v| v != &json!(source_binding))
        {
            return Err(failure(
                ErrorCode::Conflict,
                "Pinned source workspace binding changed",
            ));
        }
        spec.input["workspace_binding"] = json!(source_binding);
        let mut options = Options::parse(&spec.input["worker_workspace"])
            .map_err(|e| failure(ErrorCode::Invalid, e.to_string()))?;
        if spec.input.get("worker_workspace").is_some_and(|v| {
            !v.is_object()
                || v.as_object()
                    .unwrap()
                    .keys()
                    .any(|k| !matches!(k.as_str(), "isolation" | "keep_worktree" | "base_ref"))
        }) {
            return Err(failure(
                ErrorCode::Invalid,
                "Invalid worker workspace policy",
            ));
        }
        if let Some(parent) = spec.parent_run.as_ref() {
            let p = store
                .get(parent)?
                .ok_or_else(|| failure(ErrorCode::Invalid, "Parent run missing"))?;
            let po = Options::parse(&p.spec.input["worker_workspace"])
                .map_err(io_error)
                .map_err(|e| failure(ErrorCode::Invalid, e.to_string()))?;
            if po.isolated() {
                if spec.input["worker_workspace"].get("isolation").is_none() {
                    options.isolation = "worktree".into();
                } else if !options.isolated() {
                    return Err(failure(
                        ErrorCode::Unauthorized,
                        "An isolated specialist cannot delegate into the shared source workspace",
                    ));
                }
            }
        }
        let active = std::sync::atomic::AtomicBool::new(false);
        self.workspace
            .validate(
                &options,
                &Control {
                    cancelled: &active,
                    deadline: Some(Instant::now() + Duration::from_secs(30)),
                },
            )
            .map_err(|e| failure(ErrorCode::Conflict, e.to_string()))?;
        if spec.input.get("worker_workspace").is_some() || options.isolated() {
            spec.input["worker_workspace"] = json!(options);
        }
        if let Some(resolved) = self
            .resolve(&spec.agent)
            .map_err(|e| failure(ErrorCode::Invalid, e.to_string()))?
        {
            let active = resolved
                .definition
                .as_ref()
                .map(|d| {
                    self.origin(&spec, store)
                        .and_then(|source| self.resolve_activation(&source, store, d))
                })
                .transpose()?
                .flatten();
            if (spec.input.get("profile_activation").is_some()
                || spec.input.get("resume_from").is_some())
                && spec.input.get("profile_activation") != active.as_ref().map(|a| &a.pin)
            {
                return Err(failure(
                    ErrorCode::Conflict,
                    "Pinned profile resources changed",
                ));
            }
            if let Some(active) = active {
                spec.input["profile_activation"] = active.pin;
            }
            let binding = resolved.fingerprint;
            if spec
                .input
                .get("specialist_binding")
                .is_some_and(|v| v != &json!(binding))
            {
                return Err(failure(
                    ErrorCode::Conflict,
                    "Pinned specialist definition changed",
                ));
            }
            spec.input["specialist_binding"] = json!(binding);
        } else if spec.input.get("specialist_binding").is_some()
            || spec.input.get("profile_activation").is_some()
        {
            return Err(failure(ErrorCode::Invalid, "Specialist is not configured"));
        }
        Ok(spec)
    }
    pub(crate) fn prepare(
        &self,
        run: &RunRecord,
        store: &dyn RunStore,
        control: &Control<'_>,
    ) -> io::Result<Value> {
        if self.normalize(&run.spec, store).map_err(io_error)? != run.spec {
            return Err(io_error(
                "Execution does not match its admitted workspace binding",
            ));
        }
        let options = Options::parse(&run.spec.input["worker_workspace"])?;
        let key = self.key(&run.spec, store).map_err(io_error)?;
        let snapshot = self.workspace.prepare(&key, &options, control)?;
        if snapshot.options != options
            || snapshot.mode != options.isolation
            || !snapshot.ready_for_input
        {
            return Err(io_error(
                "Workspace adapter did not provide the requested workspace isolation",
            ));
        }
        serde_json::to_value(snapshot).map_err(io_error)
    }
    pub fn workspace(&self, run: &RunRecord, store: &dyn RunStore) -> Result<Value> {
        let options = Options::parse(&run.spec.input["worker_workspace"])
            .map_err(|e| failure(ErrorCode::Invalid, e.to_string()))?;
        serde_json::to_value(
            self.workspace
                .snapshot(&self.key(&run.spec, store)?, &options)
                .map_err(|e| failure(ErrorCode::Unavailable, e.to_string()))?,
        )
        .map_err(|e| failure(ErrorCode::Unavailable, e.to_string()))
    }
    pub(crate) fn settle(&self, run: &RunRecord, store: &dyn RunStore) -> Result<()> {
        let options = Options::parse(&run.spec.input["worker_workspace"])
            .map_err(|e| failure(ErrorCode::Invalid, e.to_string()))?;
        if !run.state.terminal() || !options.isolated() {
            return Ok(());
        }
        let active = std::sync::atomic::AtomicBool::new(false);
        let result = self
            .workspace
            .settle(
                &self.key(&run.spec, store)?,
                &options,
                run.state == RunState::Succeeded,
                true,
                &Control {
                    cancelled: &active,
                    deadline: Some(Instant::now() + Duration::from_secs(30)),
                },
            )
            .map_err(|e| failure(ErrorCode::Unavailable, e.to_string()))?;
        let key = hash(&json!(run.spec));
        let prior = store.effect_get(&run.spec.turn, "workspace:settlement")?;
        if prior.as_ref().is_none_or(|e| e.receipt.is_none()) {
            if prior.is_none() {
                store.effect_begin(&run.spec.turn, "workspace:settlement", &key)?;
            }
            store.effect_finish(&run.spec.turn, "workspace:settlement", &key, json!(result))?;
        }
        Ok(())
    }
    pub fn reconcile_workspace(&self, run: &RunRecord, store: &dyn RunStore) -> Result<Value> {
        let options = Options::parse(&run.spec.input["worker_workspace"])
            .map_err(|e| failure(ErrorCode::Invalid, e.to_string()))?;
        let active = std::sync::atomic::AtomicBool::new(false);
        serde_json::to_value(
            self.workspace
                .reconcile(
                    &self.key(&run.spec, store)?,
                    &options,
                    &Control {
                        cancelled: &active,
                        deadline: Some(Instant::now() + Duration::from_secs(30)),
                    },
                )
                .map_err(|e| failure(ErrorCode::Unavailable, e.to_string()))?,
        )
        .map_err(|e| failure(ErrorCode::Unavailable, e.to_string()))
    }
    pub fn cleanup_workspace(
        &self,
        run: &RunRecord,
        store: &dyn RunStore,
        confirmed: bool,
    ) -> Result<Value> {
        if !confirmed || !run.state.terminal() {
            return Err(failure(
                ErrorCode::Conflict,
                "Stop the worker and explicitly confirm workspace cleanup",
            ));
        }
        let key = self.key(&run.spec, store)?;
        for active in store.active()? {
            if self.key(&active.spec, store)? == key {
                return Err(failure(
                    ErrorCode::Conflict,
                    "The checkout is still used by an active continuation",
                ));
            }
        }
        let mut options = Options::parse(&run.spec.input["worker_workspace"])
            .map_err(|e| failure(ErrorCode::Invalid, e.to_string()))?;
        if !options.isolated() {
            return Err(failure(
                ErrorCode::Unsupported,
                "Only journal-owned worker checkouts can be cleaned",
            ));
        }
        options.keep_worktree = false;
        let active = std::sync::atomic::AtomicBool::new(false);
        serde_json::to_value(
            self.workspace
                .settle(
                    &key,
                    &options,
                    true,
                    true,
                    &Control {
                        cancelled: &active,
                        deadline: Some(Instant::now() + Duration::from_secs(30)),
                    },
                )
                .map_err(|e| failure(ErrorCode::Unavailable, e.to_string()))?,
        )
        .map_err(|e| failure(ErrorCode::Unavailable, e.to_string()))
    }
    pub(crate) fn review(
        &self,
        run: &RunRecord,
        store: &dyn RunStore,
        name: &str,
    ) -> io::Result<bool> {
        Ok(self.decision(run, store, name)?.review)
    }
    pub(crate) fn permits(
        &self,
        run: &RunRecord,
        store: &dyn RunStore,
        name: &str,
    ) -> io::Result<bool> {
        Ok(self.decision(run, store, name)?.allowed)
    }
    fn allowed(
        &self,
        run: &RunRecord,
        store: &dyn RunStore,
    ) -> io::Result<Option<HashSet<String>>> {
        let mut current = self.origin(&run.spec, store).map_err(io_error)?;
        let mut allowed: Option<HashSet<String>> = None;
        let mut seen = HashSet::new();
        for _ in 0..5 {
            if !seen.insert(current.id.clone()) {
                return Err(io_error("Cyclic specialist scope"));
            }
            let resolved = self.resolve(&current.agent)?;
            if current.input.get("specialist_binding").is_some() || resolved.is_some() {
                let config =
                    resolved.ok_or_else(|| io_error("Specialist configuration unavailable"))?;
                if current.input["specialist_binding"] != config.fingerprint {
                    return Err(io_error("Pinned specialist configuration changed"));
                }
                if let Some(names) = &config.specialist.allowed_tools {
                    let names: HashSet<String> = names.iter().cloned().collect();
                    allowed = Some(match allowed {
                        Some(old) => old.intersection(&names).cloned().collect(),
                        None => names,
                    });
                }
            }
            let Some(parent) = current.parent_run.as_ref() else {
                return Ok(allowed);
            };
            current = self
                .origin(
                    &store
                        .get(parent)
                        .map_err(io_error)?
                        .ok_or_else(|| io_error("Parent scope missing"))?
                        .spec,
                    store,
                )
                .map_err(io_error)?;
        }
        Err(io_error("Specialist ancestry exceeds limit"))
    }
    fn decision(
        &self,
        run: &RunRecord,
        store: &dyn RunStore,
        tool: &str,
    ) -> io::Result<kageha_policy::specialist::Decision> {
        let exact = self.allowed(run, store)?.is_none_or(|a| a.contains(tool));
        let mut current = self.origin(&run.spec, store).map_err(io_error)?;
        let mut policies = vec![];
        for _ in 0..5 {
            if let Some(config) = self.resolve(&current.agent)? {
                if current.input["specialist_binding"] != config.fingerprint {
                    return Err(io_error("Pinned specialist configuration changed"));
                }
                policies.push(config.policy);
            }
            let Some(parent) = current.parent_run.as_ref() else {
                let mut decision = kageha_policy::specialist::evaluate(&policies, tool)?;
                decision.allowed &= exact;
                return Ok(decision);
            };
            current = self
                .origin(
                    &store
                        .get(parent)
                        .map_err(io_error)?
                        .ok_or_else(|| io_error("Parent scope missing"))?
                        .spec,
                    store,
                )
                .map_err(io_error)?;
        }
        Err(io_error("Specialist ancestry exceeds limit"))
    }
    pub(crate) fn tools(
        self: &Arc<Self>,
        base: Arc<dyn ToolPort + Send + Sync>,
        run: &RunRecord,
        store: Arc<dyn RunStore>,
    ) -> io::Result<Arc<dyn ToolPort + Send + Sync>> {
        let options = Options::parse(&run.spec.input["worker_workspace"])?;
        let scoped = self.workspace.tools(
            &self.key(&run.spec, store.as_ref()).map_err(io_error)?,
            &options,
        )?;
        if options.isolated() && scoped.is_none() {
            return Err(io_error(
                "Isolated workspace adapter supplied no scoped tool capabilities",
            ));
        }
        self.allowed(run, store.as_ref())?;
        let skills = self
            .resources
            .as_ref()
            .and_then(|r| r.skills.as_ref())
            .map(|path| {
                crate::skill_tools::Tools::new(
                    path.clone(),
                    &run.spec.input["profile_activation"]["skills"],
                )
            })
            .transpose()?
            .flatten();
        Ok(Arc::new(Tools {
            skills,
            composition: self.clone(),
            base,
            scoped,
            run: run.clone(),
            store,
        }))
    }
    fn child(&self, parent: &RunRecord, id: &str) -> Result<RunRecord> {
        let host = self.host()?;
        let record = host
            .supervisor
            .store
            .get(id)?
            .ok_or_else(|| failure(ErrorCode::Invalid, "Unknown worker"))?;
        let origin = self.origin(&record.spec, host.supervisor.store.as_ref())?;
        let owner = self.origin(&parent.spec, host.supervisor.store.as_ref())?;
        let parent_origin = origin
            .parent_run
            .as_ref()
            .map(|id| {
                host.supervisor
                    .store
                    .get(id)
                    .and_then(|r| {
                        r.ok_or_else(|| failure(ErrorCode::Unauthorized, "Worker owner missing"))
                    })
                    .and_then(|r| self.origin(&r.spec, host.supervisor.store.as_ref()))
            })
            .transpose()?;
        if parent_origin.as_ref().map(|p| p.id.as_str()) != Some(owner.id.as_str())
            || origin.parent.as_deref() != Some(owner.session.as_str())
        {
            return Err(failure(
                ErrorCode::Unauthorized,
                "Worker is outside this parent scope",
            ));
        }
        let mut record = record;
        let mut seen = HashSet::new();
        for _ in 0..128 {
            if !seen.insert(record.spec.id.clone()) {
                return Err(failure(ErrorCode::Conflict, "Cyclic worker continuation"));
            }
            let next = host
                .supervisor
                .store
                .effect_get(&record.spec.turn, "runtime:continuation")?
                .and_then(|e| e.receipt);
            let Some(next) = next else { return Ok(record) };
            let spec: RunSpec = serde_json::from_value(next)
                .map_err(|_| failure(ErrorCode::Conflict, "Invalid worker continuation receipt"))?;
            let Some(next) = host.supervisor.store.get(&spec.id)? else {
                return Ok(record);
            };
            if next.spec.session != record.spec.session
                || next.spec.input["resume_from"] != record.spec.id
            {
                return Err(failure(
                    ErrorCode::Conflict,
                    "Worker continuation scope changed",
                ));
            }
            record = next;
        }
        Err(failure(
            ErrorCode::Conflict,
            "Worker continuation exceeds limit",
        ))
    }
    fn spawn(&self, parent: &RunRecord, call: &ToolCall, pin: Option<&str>) -> Result<Value> {
        let host = self.host()?;
        if parent.spec.depth >= 3 {
            return Err(failure(
                ErrorCode::Conflict,
                "Specialist nesting limit reached",
            ));
        }
        let args = &call.arguments;
        let agent = args["agent_id"]
            .as_str()
            .ok_or_else(|| failure(ErrorCode::Invalid, "agent_id required"))?;
        let resolved = self
            .resolve(agent)
            .map_err(|e| failure(ErrorCode::Invalid, e.to_string()))?
            .ok_or_else(|| failure(ErrorCode::Invalid, "Unknown specialist"))?;
        if pin.is_some_and(|p| p != resolved.fingerprint) {
            return Err(failure(
                ErrorCode::Conflict,
                "Specialist target changed during approval",
            ));
        }
        let target_pin = resolved.fingerprint;
        let specialist = resolved.specialist;
        let objective = args["objective"]
            .as_str()
            .filter(|s| !s.trim().is_empty() && s.len() <= 65536)
            .ok_or_else(|| failure(ErrorCode::Invalid, "Bounded objective required"))?;
        let id = hash(&json!([parent.spec.id, call.id]))[..32].to_owned();
        let mut input = json!({"messages":[{"role":"system","content":specialist.instructions},{"role":"user","content":objective}],"max_model_calls":parent.spec.input["max_model_calls"].as_u64().unwrap_or(128),"timeout_ms":parent.spec.input["timeout_ms"].as_u64().unwrap_or(120000)});
        input["specialist_binding"] = json!(target_pin);
        let mut options = json!({});
        for field in ["isolation", "keep_worktree", "base_ref"] {
            if let Some(value) = args.get(field) {
                options[field] = value.clone();
            }
        }
        if options.as_object().is_some_and(|v| !v.is_empty()) {
            input["worker_workspace"] = options;
        }
        let child = RunSpec {
            id: id.clone(),
            session: format!("worker-{id}"),
            turn: id.clone(),
            request: format!("spawn-{id}"),
            parent: Some(parent.spec.session.clone()),
            parent_run: Some(parent.spec.id.clone()),
            agent: agent.into(),
            objective: objective.into(),
            depth: parent.spec.depth + 1,
            input,
        };
        Ok(json!(host.submit(&child)?))
    }
}
pub fn specs() -> Vec<Value> {
    let make = |name: &str, description: &str, properties: Value, required: Vec<&str>| json!({"type":"function","function":{"name":name,"description":description,"parameters":{"type":"object","properties":properties,"required":required,"additionalProperties":false}}});
    vec![
 make("agent_spawn","Start a configured specialist and return its job ID. Independent workers can run concurrently while you continue other work; collect and integrate their results with agent_wait_any or agent_wait_all before completing the task. Workspace isolation and inherited tool policy are enforced.",json!({"agent_id":{"type":"string"},"objective":{"type":"string"},"isolation":{"type":"string","enum":["shared","worktree"]},"keep_worktree":{"type":"boolean"},"base_ref":{"type":"string"}}),vec!["agent_id","objective"]),
 make("agent_status","Inspect a worker owned by this parent run.",json!({"job_id":{"type":"string"}}),vec!["job_id"]),make("agent_cancel","Cancel a worker owned by this parent run and its descendants.",json!({"job_id":{"type":"string"}}),vec!["job_id"]),
 make("agent_wait_any","Wait up to 30 seconds for one owned worker; unrelated workers continue.",json!({"job_ids":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":32,"uniqueItems":true},"timeout_ms":{"type":"integer","minimum":0,"maximum":30000}}),vec!["job_ids"]),
 make("agent_wait_all","When every result is needed, wait up to 30 seconds for all owned workers. On timeout returns finished workers and pending_job_ids; wait again for the pending jobs or continue useful work. Use agent_wait_any to act on results as they arrive.",json!({"job_ids":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":32,"uniqueItems":true},"timeout_ms":{"type":"integer","minimum":0,"maximum":30000}}),vec!["job_ids"]) ]
}
struct Tools {
    skills: Option<crate::skill_tools::Tools>,
    composition: Arc<Composition>,
    base: Arc<dyn ToolPort + Send + Sync>,
    scoped: Option<Arc<dyn ToolPort + Send + Sync>>,
    run: RunRecord,
    store: Arc<dyn RunStore>,
}
impl Tools {
    fn worker(&self, name: &str) -> bool {
        specs().iter().any(|s| s["function"]["name"] == name)
    }
    fn port(
        &self,
        context: &ToolContext,
        call: &ToolCall,
    ) -> io::Result<&(dyn ToolPort + Send + Sync)> {
        if call.name == crate::skill_tools::READ
            && self
                .composition
                .resources
                .as_ref()
                .is_some_and(|r| r.skills.is_some())
        {
            return self
                .skills
                .as_ref()
                .map(|skills| skills as &(dyn ToolPort + Send + Sync))
                .ok_or_else(|| io_error("Run has no admitted skill resources"));
        }
        if let Some(scoped) = &self.scoped {
            if scoped
                .discover_in(context)?
                .iter()
                .any(|s| s["function"]["name"] == call.name)
            {
                return Ok(scoped.as_ref());
            }
        }
        Ok(self.base.as_ref())
    }
    fn scoped(&self, context: &ToolContext) -> io::Result<()> {
        if context.session != self.run.spec.session || context.turn != self.run.spec.turn {
            return Err(io_error("Worker tool context differs from admitted run"));
        }
        if self.run.spec.input["workspace_binding"] != self.composition.workspace.binding()? {
            // A stop, not a tool failure: the run's admitted authority no longer
            // holds, so the turn ends. The runtime discovers tools once per turn,
            // so the next step would not catch the change.
            return Err(kageha_contracts::policy::stop(
                "Workspace binding generation changed before dispatch",
            ));
        }
        Ok(())
    }
    fn exposed(&self, name: &str) -> io::Result<()> {
        if !self
            .composition
            .decision(&self.run, self.store.as_ref(), name)?
            .allowed
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Specialist tool is excluded by inherited policy",
            ));
        }
        Ok(())
    }
    fn elevate(&self, mut binding: ToolBinding, call: &ToolCall) -> io::Result<ToolBinding> {
        if self
            .composition
            .review(&self.run, self.store.as_ref(), &call.name)?
        {
            binding.class = ActionClass::Restricted;
            binding.detail = format!("Specialist policy raised risk: {}", binding.detail);
        }
        Ok(binding)
    }
}
impl ToolPort for Tools {
    fn discover(&self) -> io::Result<Vec<Value>> {
        self.discover_in(&ToolContext {
            session: self.run.spec.session.clone(),
            turn: self.run.spec.turn.clone(),
        })
    }
    fn discover_in(&self, context: &ToolContext) -> io::Result<Vec<Value>> {
        self.scoped(context)?;
        let mut tools = self.base.discover_in(context)?;
        if let Some(scoped) = &self.scoped {
            let overlay = scoped.discover_in(context)?;
            let names = overlay
                .iter()
                .filter_map(|t| t["function"]["name"].as_str())
                .collect::<HashSet<_>>();
            tools.retain(|t| !names.contains(t["function"]["name"].as_str().unwrap_or("")));
            tools.extend(overlay);
        }

        if tools
            .iter()
            .any(|t| self.worker(t["function"]["name"].as_str().unwrap_or("")))
        {
            return Err(io_error(
                "Configured tool collides with specialist operations",
            ));
        }
        if self
            .composition
            .resources
            .as_ref()
            .is_some_and(|r| r.skills.is_some())
            && tools
                .iter()
                .any(|t| t["function"]["name"] == crate::skill_tools::READ)
        {
            return Err(io_error(
                "Configured tool collides with the skill resource reader",
            ));
        }
        if let Some(skills) = &self.skills {
            tools.extend(skills.discover()?);
        }
        let mut worker_specs = specs();
        for spec in &mut worker_specs {
            if spec["function"]["name"] == "agent_spawn" {
                spec["function"]["parameters"]["properties"]["agent_id"]["enum"] = json!(self
                    .composition
                    .eligible_catalog()?
                    .iter()
                    .map(|s| s.id.clone())
                    .collect::<Vec<_>>());
            }
        }
        tools.extend(
            worker_specs
                .into_iter()
                .filter(|s| self.run.spec.depth < 3 || s["function"]["name"] != "agent_spawn"),
        );
        let mut names = HashSet::new();
        if tools.len() > 512
            || tools.iter().any(|t| {
                let Some(name) = t["function"]["name"].as_str() else {
                    return true;
                };
                !tool_name(name) || !names.insert(name.to_owned())
            })
        {
            return Err(io_error(
                "Malformed, ambiguous or oversized workspace tool catalog",
            ));
        }
        let mut filtered = Vec::new();
        for tool in tools {
            if self
                .composition
                .decision(
                    &self.run,
                    self.store.as_ref(),
                    tool["function"]["name"].as_str().unwrap_or(""),
                )?
                .allowed
            {
                filtered.push(tool);
            }
        }
        let tools = filtered;
        Ok(tools)
    }
    fn dispatch_access_in(
        &self,
        context: &ToolContext,
        call: &ToolCall,
    ) -> io::Result<DispatchAccess> {
        self.scoped(context)?;
        self.exposed(&call.name)?;
        if self.worker(&call.name) {
            return Ok(DispatchAccess::Exclusive);
        }
        self.port(context, call)?.dispatch_access_in(context, call)
    }
    fn binding(&self, context: &ToolContext, call: &ToolCall) -> io::Result<ToolBinding> {
        self.scoped(context)?;
        self.exposed(&call.name)?;
        let binding = if self.worker(&call.name) {
            ToolBinding {
                binding: "runtime.workers".into(),
                generation: 1,
                resource: self
                    .composition
                    .origin(&self.run.spec, self.store.as_ref())
                    .map_err(io_error)?
                    .id,
                class: if matches!(call.name.as_str(), "agent_status" | "agent_wait_any" | "agent_wait_all") {
                    ActionClass::ReadOnly
                } else {
                    ActionClass::Mutation
                },
                detail: format!(
                    "{} within parent run {}",
                    call.name,
                    self.composition
                        .origin(&self.run.spec, self.store.as_ref())
                        .map_err(io_error)?
                        .id
                ),
                fingerprint: if self.composition.profiles.is_none() {
                    hash(&json!([self.composition.catalog(), self.run.spec.agent]))
                } else {
                    let target = if call.name == "agent_spawn" {
                        let id = call.arguments["agent_id"].as_str().unwrap_or("");
                        Some(
                            self.composition
                                .resolve(id)?
                                .ok_or_else(|| io_error("Unknown specialist"))?
                                .fingerprint,
                        )
                    } else {
                        None
                    };
                    hash(&json!([
                        self.run.spec.input["specialist_binding"],
                        self.run.spec.agent,
                        target
                    ]))
                },
            }
        } else {
            self.port(context, call)?.binding(context, call)?
        };
        self.elevate(binding, call)
    }
    fn mutation_binding(
        &self,
        context: &ToolContext,
        call: &ToolCall,
    ) -> io::Result<Option<ToolBinding>> {
        self.scoped(context)?;
        self.exposed(&call.name)?;
        if self.worker(&call.name) {
            return Ok(
                if matches!(call.name.as_str(), "agent_spawn" | "agent_cancel") {
                    Some(self.binding(context, call)?)
                } else {
                    None
                },
            );
        }
        self.port(context, call)?.mutation_binding(context, call)
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
        self.scoped(context)?;
        self.exposed(&call.name)?;
        control.check()?;
        if !self.worker(&call.name) {
            return self.port(context, call)?.execute_in(context, call, control);
        }
        let args = &call.arguments;
        let value = match call.name.as_str() {
            "agent_spawn" => self
                .composition
                .spawn(&self.run, call, None)
                .map_err(io_error)?,
            "agent_status" => {
                let child = self
                    .composition
                    .child(&self.run, args["job_id"].as_str().unwrap_or(""))
                    .map_err(io_error)?;
                json!({"run":child,"workspace":self.composition.workspace(&child,self.store.as_ref()).map_err(io_error)?})
            }
            "agent_cancel" => {
                let child = self
                    .composition
                    .child(&self.run, args["job_id"].as_str().unwrap_or(""))
                    .map_err(io_error)?;
                self.composition
                    .host()
                    .map_err(io_error)?
                    .supervisor
                    .cancel(&child.spec.id)
                    .map_err(io_error)?;
                json!({"cancelled":true,"id":child.spec.id})
            }
            "agent_wait_any" | "agent_wait_all" => {
                let ids: Vec<String> = serde_json::from_value(args["job_ids"].clone())?;
                if ids.is_empty()
                    || ids.len() > 32
                    || ids.iter().collect::<HashSet<_>>().len() != ids.len()
                {
                    return Err(io_error("Invalid worker wait identities"));
                }
                let timeout = args
                    .get("timeout_ms")
                    .map(|v| {
                        v.as_u64()
                            .filter(|n| *n <= 30000)
                            .ok_or_else(|| io_error("Invalid wait timeout"))
                    })
                    .transpose()?
                    .unwrap_or(30000);
                let until = Instant::now() + Duration::from_millis(timeout);
                loop {
                    control.check()?;
                    let mut finished = vec![];
                    let mut pending = vec![];
                    for id in &ids {
                        let child = self.composition.child(&self.run, id).map_err(io_error)?;
                        if child.state.terminal() {
                            finished.push(child);
                        } else {
                            pending.push(id);
                        }
                    }
                    if (call.name == "agent_wait_all" && pending.is_empty())
                        || (call.name == "agent_wait_any" && !finished.is_empty())
                        || Instant::now() >= until {
                        break if call.name == "agent_wait_all" {
                            json!({"finished":finished,"timed_out":!pending.is_empty(),"pending_job_ids":pending})
                        } else { json!({"finished":finished}) };
                    }
                    std::thread::sleep(Duration::from_millis(20).min(until.saturating_duration_since(Instant::now())));
                }
            }
            _ => return Err(io_error("Unknown worker tool")),
        };
        Ok(ToolReply::success(value.to_string()))
    }
    fn execute_authorized_in(
        &self,
        context: &ToolContext,
        call: &ToolCall,
        authority: &ToolBinding,
        control: &Control<'_>,
    ) -> io::Result<ToolReply> {
        self.scoped(context)?;
        self.exposed(&call.name)?;
        control.check()?;
        if self.worker(&call.name) {
            let target = if call.name == "agent_spawn" {
                Some(
                    self.composition
                        .resolve(call.arguments["agent_id"].as_str().unwrap_or(""))?
                        .ok_or_else(|| io_error("Unknown specialist"))?
                        .fingerprint,
                )
            } else {
                None
            };
            if self.binding(context, call)? != *authority {
                return Err(io_error("Worker authority changed"));
            }
            if let Some(pin) = target {
                return self
                    .composition
                    .spawn(&self.run, call, Some(&pin))
                    .map(|v| ToolReply::success(v.to_string()))
                    .map_err(io_error);
            }
            return self.execute_in(context, call, control);
        }
        let port = self.port(context, call)?;
        let original = port.binding(context, call)?;
        if self.elevate(original.clone(), call)? != *authority {
            return Err(io_error("Worker authority changed"));
        }
        self.exposed(&call.name)?;
        port.execute_authorized_in(context, call, &original, control)
    }
}

#[cfg(test)]
mod profiles_tests;
