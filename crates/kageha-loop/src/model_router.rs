//! Model_Router (design section 10): picks the model for each request by its `role`.
//! The chains come from the chat's `model`/`model_roles` and the agent's
//! `model_preferences` (see `kageha_agents::model_roles`).
//!
//! [`setup`] turns each role's chain into an ordered provider list: every chain model
//! that can be configured, then the fallback-policy models of the first one
//! (`ui_settings::candidates`). A chain model that cannot be configured is skipped with
//! a `model_fallback` event (cause `unavailable`). A planner or reviewer chain with no
//! usable model falls back to the executor the same way.
//!
//! Retries and fallback at call time happen in one place, the retry loop of
//! `kageha_models::HttpModel::complete` (`kageha_models::retry`), which walks the
//! role's provider list. It reports a move to the next model as
//! `ModelActivity::Fallback`, which `ui_tools` emits with [`emit_fallback`].
use crate::ui_settings::Provider;
use crate::ui_store::ApiResult;
use crate::webui::Database;
use kageha_agents::model_roles::{ModelRef, RoleChains};
use kageha_contracts::execution::{AssistantMessage, Control, ModelPort, ModelRequest, Role};
use serde_json::{json, Value};
use std::io;

/// The chat-level role override of a turn request. `model` is the chat's executor
/// pick; `model_roles.executor` replaces it only when it lists a chain, because the
/// UI sends a string copy of `model` there. `planner` and `reviewer` come from
/// `model_roles` as a string or a list.
pub(crate) fn chat_override(payload: &Value) -> Value {
    let roles = &payload["model_roles"];
    let executor = if roles["executor"].as_array().is_some_and(|chain| !chain.is_empty()) {
        roles["executor"].clone()
    } else {
        payload["model"].as_str().map_or(Value::Null, Value::from)
    };
    json!({"executor":executor,"planner":roles["planner"],"reviewer":roles["reviewer"]})
}

/// The role chains of a run spec. Specs admitted by the chat carry `model_chains`;
/// others (jobs, channels, older specs) take `model`, `planner_model` and the agent's
/// `model_preferences`.
pub(crate) fn chains(spec: &Value, persona: &Value) -> RoleChains {
    if let Some(chains) = spec
        .get("model_chains")
        .and_then(|value| serde_json::from_value(value.clone()).ok())
    {
        return chains;
    }
    let chat = json!({"executor":spec["model"],"planner":spec["planner_model"]});
    RoleChains::resolve(&chat, &persona["model_preferences"])
        .or_else(|_| RoleChains::resolve(&json!({}), &persona["model_preferences"]))
        .unwrap_or_default()
}

/// Providers for the planner and reviewer roles. Empty means the role uses the
/// executor's providers.
#[derive(Clone, Default)]
pub(crate) struct RoleProviders {
    pub planner: Vec<Provider>,
    pub reviewer: Vec<Provider>,
    /// The executor is text-only and the planner accepts images: model steps whose
    /// context has image parts go to the planner (design section 18).
    pub images_to_planner: bool,
    /// The executor accepts images and the planner or reviewer doesn't: model steps whose
    /// context has image parts go to the executor (design section 18).
    pub images_to_executor: bool,
    /// In-process models used by host integration tests.
    #[cfg(test)]
    pub fakes: TestModels,
}
#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct TestModels {
    pub planner: Option<std::sync::Arc<dyn ModelPort + Send + Sync>>,
    pub reviewer: Option<std::sync::Arc<dyn ModelPort + Send + Sync>>,
}

/// The providers of every role for one turn.
pub(crate) struct RouteSetup {
    pub executor: ApiResult<Vec<Provider>>,
    pub roles: RoleProviders,
    /// `model_fallback` payloads for chain models that could not be set up.
    pub fallbacks: Vec<Value>,
}

/// The most providers one model call can try (`HttpModel` accepts 16).
const MAX_PROVIDERS: usize = 16;

/// A chain configured in order.
struct ConfiguredChain {
    providers: Vec<Provider>,
    /// Each chain model that could not be configured, with the index in `providers`
    /// of the model used instead.
    skipped: Vec<(ModelRef, usize)>,
    /// The first configuration error, reported when no model could be configured.
    error: Option<(u16, String)>,
}

/// Configures `chain` in order with `configure`, then appends `policy(first configured
/// model)`: the fallback-policy models. Duplicates are dropped.
fn configure_chain(
    chain: &[ModelRef],
    mut configure: impl FnMut(&str) -> ApiResult<Provider>,
    policy: impl FnOnce(&str) -> Vec<Provider>,
) -> ConfiguredChain {
    let same = |a: &Provider, b: &Provider| a.protocol == b.protocol && a.url == b.url && a.model == b.model;
    let mut configured = ConfiguredChain { providers: Vec::new(), skipped: Vec::new(), error: None };
    let mut head = None;
    for model in chain {
        match configure(model) {
            Ok(provider) => {
                head.get_or_insert_with(|| model.clone());
                if !configured.providers.iter().any(|known| same(known, &provider)) {
                    configured.providers.push(provider);
                }
            }
            Err(error) => {
                configured.skipped.push((model.clone(), configured.providers.len()));
                configured.error.get_or_insert(error);
            }
        }
    }
    if let Some(head) = head {
        for provider in policy(&head) {
            if !configured.providers.iter().any(|known| same(known, &provider)) {
                configured.providers.push(provider);
            }
        }
    }
    configured.providers.truncate(MAX_PROVIDERS);
    configured
}

/// The `model_fallback` payloads of a chain's skipped models. A skipped model with no
/// later model in its chain falls back to `otherwise` (the executor, for other roles).
fn skipped_fallbacks(role: Role, chain: &ConfiguredChain, otherwise: Option<&str>) -> Vec<Value> {
    chain
        .skipped
        .iter()
        .map(|(model, next)| {
            let to = chain.providers.get(*next).map(|provider| provider.model.as_str()).or(otherwise);
            json!({"role":role.as_str(),"from":model,"to":to,"cause":"unavailable"})
        })
        .collect()
}

/// Configures the providers of each role. `requested` is the executor model picked for
/// the turn and `setting` its settings role (`default` or `worker`). When `requested`
/// is the head of a longer executor chain, the whole chain is used; otherwise the
/// executor keeps `requested` with the fallback policy. Planner and reviewer chains
/// that differ from the executor's are configured the same way.
pub(crate) fn setup(db: &Database, requested: &str, setting: &str, chains: &RoleChains) -> RouteSetup {
    let configure = |chain: &[ModelRef], setting: &str| {
        configure_chain(chain, |model| crate::ui_settings::configured(db, model, setting), |head| {
            crate::ui_settings::candidates(db, head, setting).map(|all| all.into_iter().skip(1).collect()).unwrap_or_default()
        })
    };
    let mut fallbacks = Vec::new();
    let executor = if chains.executor.len() > 1 && chains.executor.first().is_some_and(|head| head == requested) {
        let configured = configure(&chains.executor, setting);
        fallbacks.extend(skipped_fallbacks(Role::Executor, &configured, None));
        if configured.providers.is_empty() {
            Err(configured.error.unwrap_or((400, "Configure a ready model or select a default model".into())))
        } else {
            Ok(configured.providers)
        }
    } else {
        crate::ui_settings::candidates(db, requested, setting)
    };
    let executor_head = executor.as_ref().ok().and_then(|providers| providers.first()).map(|provider| provider.model.clone());
    let mut role = |role: Role| -> Vec<Provider> {
        if !chains.distinct(role) {
            return Vec::new();
        }
        let configured = configure(chains.chain(role), role.as_str());
        fallbacks.extend(skipped_fallbacks(role, &configured, executor_head.as_deref()));
        configured.providers
    };
    let roles = RoleProviders {
        planner: role(Role::Planner),
        reviewer: role(Role::Reviewer),
        images_to_planner: false,
        images_to_executor: false,
        #[cfg(test)]
        fakes: TestModels::default(),
    };
    RouteSetup { executor, roles, fallbacks }
}

/// Emits `model_fallback {role, from, to, cause}` for a turn (design section 16).
pub(crate) fn emit_fallback(db: &Database, chat: &str, turn: &str, role: &str, from: &str, to: &str, cause: &str) -> io::Result<()> {
    emit_fallback_payload(db, chat, turn, json!({"role":role,"from":from,"to":to,"cause":cause}))
}

pub(crate) fn emit_fallback_payload(db: &Database, chat: &str, turn: &str, payload: Value) -> io::Result<()> {
    crate::webui::emit(db, chat, turn, "event", json!({"kind":"model_fallback","payload":payload}))
}

/// A `ModelPort` that sends each request to the model for its `role`. A role
/// without its own model uses the executor.
pub(crate) struct RoutedModel<'a> {
    pub executor: &'a (dyn ModelPort + Sync),
    pub planner: Option<&'a (dyn ModelPort + Sync)>,
    pub reviewer: Option<&'a (dyn ModelPort + Sync)>,
    /// Requests with image parts go to the planner ([`RoleProviders::images_to_planner`]).
    pub images_to_planner: bool,
    /// Requests with image parts go to the executor ([`RoleProviders::images_to_executor`]).
    pub images_to_executor: bool,
}

impl RoutedModel<'_> {
    fn models(&self) -> impl Iterator<Item = &(dyn ModelPort + Sync)> {
        let models: [Option<&(dyn ModelPort + Sync)>; 3] = [Some(self.executor), self.planner, self.reviewer];
        models.into_iter().flatten()
    }
    fn select(&self, role: Role) -> &(dyn ModelPort + Sync) {
        match role {
            Role::Executor => None,
            Role::Planner => self.planner,
            Role::Reviewer => self.reviewer,
        }
        .unwrap_or(self.executor)
    }
}

impl ModelPort for RoutedModel<'_> {
    fn accounts_invocations(&self) -> bool {
        self.models().all(|model| model.accounts_invocations())
    }
    /// The smallest known window, so compaction keeps the context usable by every role.
    fn context_window(&self) -> Option<u64> {
        self.models().filter_map(|model| model.context_window()).min()
    }
    fn complete(&self, mut request: ModelRequest, control: &Control<'_>) -> io::Result<AssistantMessage> {
        if self.images_to_planner && self.planner.is_some() && crate::ui_vision::has_images(&request.messages) {
            request.role = Role::Planner;
        } else if self.images_to_executor && crate::ui_vision::has_images(&request.messages) {
            request.role = Role::Executor;
        }
        self.select(request.role).complete(request, control)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{atomic::AtomicBool, Mutex};

    struct Fake {
        name: &'static str,
        window: Option<u64>,
        roles: Mutex<Vec<Role>>,
    }
    impl Fake {
        fn new(name: &'static str, window: Option<u64>) -> Self {
            Self { name, window, roles: Mutex::new(Vec::new()) }
        }
    }
    impl ModelPort for Fake {
        fn accounts_invocations(&self) -> bool {
            true
        }
        fn context_window(&self) -> Option<u64> {
            self.window
        }
        fn complete(&self, request: ModelRequest, _: &Control<'_>) -> io::Result<AssistantMessage> {
            self.roles.lock().unwrap().push(request.role);
            Ok(AssistantMessage { content: json!(self.name), tool_calls: vec![], continuation: None })
        }
    }
    fn ask(model: &dyn ModelPort, role: Role) -> Value {
        let cancelled = AtomicBool::new(false);
        let request = ModelRequest { messages: vec![], tools: vec![], role };
        model.complete(request, &Control { cancelled: &cancelled, deadline: None }).unwrap().content
    }

    #[test]
    fn requests_go_to_the_chain_for_their_role() {
        let (executor, planner) = (Fake::new("executor", Some(200_000)), Fake::new("planner", Some(128_000)));
        let routed = RoutedModel { executor: &executor, planner: Some(&planner), reviewer: None, images_to_planner: false, images_to_executor: false };
        assert_eq!(ask(&routed, Role::Executor), "executor");
        assert_eq!(ask(&routed, Role::Planner), "planner");
        // No reviewer chain: the executor reviews.
        assert_eq!(ask(&routed, Role::Reviewer), "executor");
        assert_eq!(*planner.roles.lock().unwrap(), [Role::Planner]);
        assert_eq!(*executor.roles.lock().unwrap(), [Role::Executor, Role::Reviewer]);
        assert!(routed.accounts_invocations());
        assert_eq!(routed.context_window(), Some(128_000));

        let alone = RoutedModel { executor: &executor, planner: None, reviewer: None, images_to_planner: false, images_to_executor: false };
        assert_eq!(ask(&alone, Role::Planner), "executor");
        // Rule 2 of design section 18: a text-only executor's image steps go to the planner.
        let seeing = RoutedModel { executor: &executor, planner: Some(&planner), reviewer: None, images_to_planner: true, images_to_executor: false };
        let cancelled = AtomicBool::new(false);
        let image = json!({"role":"user","content":[{"type":"text","text":"look"},{"type":"image_url","image_url":{"url":"data:image/png;base64,AA=="}}]});
        let ask_with = |messages: Vec<Value>| seeing.complete(ModelRequest { messages, tools: vec![], role: Role::Executor },
            &Control { cancelled: &cancelled, deadline: None }).unwrap().content;
        assert_eq!(ask_with(vec![image.clone()]), "planner");
        assert_eq!(ask_with(vec![json!({"role":"user","content":"text only"})]), "executor");
        // A seeing executor keeps the image steps of a text-only planner (task 31.8).
        let keeping = RoutedModel { executor: &executor, planner: Some(&planner), reviewer: None, images_to_planner: false, images_to_executor: true };
        let planned = |messages: Vec<Value>| keeping.complete(ModelRequest { messages, tools: vec![], role: Role::Planner },
            &Control { cancelled: &cancelled, deadline: None }).unwrap().content;
        assert_eq!(planned(vec![image.clone()]), "executor");
        assert_eq!(planned(vec![json!({"role":"user","content":"text only"})]), "planner");
    }

    #[test]
    fn the_chat_override_maps_model_and_model_roles() {
        let payload = json!({"model":"kimi","model_roles":{"planner":"glm","executor":"kimi"}});
        assert_eq!(chat_override(&payload), json!({"executor":"kimi","planner":"glm","reviewer":null}));
        let payload = json!({"model":"kimi","model_roles":{"executor":["a","b"],"reviewer":["r"]}});
        let chains = RoleChains::resolve(&chat_override(&payload), &json!({"planner":"p"})).unwrap();
        assert_eq!((chains.executor.as_slice(), chains.planner.as_slice(), chains.reviewer.as_slice()),
            (&["a".to_owned(), "b".into()][..], &["p".to_owned()][..], &["r".to_owned()][..]));
        // No chat pick: the agent's executor chain, as before.
        let agent = json!({"executor":["cheap","backup"]});
        let chains = RoleChains::resolve(&chat_override(&json!({"model":""})), &agent).unwrap();
        assert_eq!(chains.executor, ["cheap", "backup"]);
    }

    fn provider(model: &str) -> Provider {
        Provider { url: "https://models.example/v1".into(), key: String::new(), model: model.into(),
            protocol: "openai_compat".into(), source: None, context_window: None }
    }
    fn configure(chain: &[&str], ready: &[&str], policy: &[&str]) -> ConfiguredChain {
        let chain: Vec<ModelRef> = chain.iter().map(|model| (*model).to_owned()).collect();
        let policy: Vec<Provider> = policy.iter().map(|model| provider(model)).collect();
        configure_chain(
            &chain,
            |model| if ready.contains(&model) { Ok(provider(model)) } else { Err((400, format!("{model} is not configured"))) },
            |_| policy,
        )
    }
    fn models(chain: &ConfiguredChain) -> Vec<&str> {
        chain.providers.iter().map(|provider| provider.model.as_str()).collect()
    }

    #[test]
    fn every_chain_model_is_tried_in_order_before_the_fallback_policy() {
        let chain = configure(&["a", "b", "c"], &["a", "b", "c", "d"], &["b", "d"]);
        assert_eq!(models(&chain), ["a", "b", "c", "d"]);
        assert!(chain.skipped.is_empty() && chain.error.is_none());
        assert!(skipped_fallbacks(Role::Executor, &chain, None).is_empty());
    }

    #[test]
    fn unavailable_chain_models_are_skipped_with_a_fallback_event() {
        let chain = configure(&["gone", "b"], &["b"], &[]);
        assert_eq!(models(&chain), ["b"]);
        assert_eq!(skipped_fallbacks(Role::Planner, &chain, Some("exec")),
            [json!({"role":"planner","from":"gone","to":"b","cause":"unavailable"})]);
        // A planner chain with no usable model falls back to the executor.
        let none = configure(&["gone"], &[], &["policy"]);
        assert!(none.providers.is_empty());
        assert_eq!(none.error, Some((400, "gone is not configured".into())));
        assert_eq!(skipped_fallbacks(Role::Reviewer, &none, Some("exec")),
            [json!({"role":"reviewer","from":"gone","to":"exec","cause":"unavailable"})]);
        // At most 16 providers reach one model.
        let long: Vec<String> = (0..20).map(|n| format!("m{n}")).collect();
        let names: Vec<&str> = long.iter().map(String::as_str).collect();
        assert_eq!(configure(&names[..8], &names, &names[8..]).providers.len(), MAX_PROVIDERS);
    }

    #[test]
    fn specs_without_chains_use_their_model_fields_and_the_agent() {
        let persona = json!({"model_preferences":{"executor":["a","b"],"reviewer":"r"}});
        let chains = chains(&json!({"model":"picked","planner_model":""}), &persona);
        assert_eq!((chains.chain(Role::Executor), chains.chain(Role::Planner), chains.chain(Role::Reviewer)),
            (&["picked".to_owned()][..], &["picked".to_owned()][..], &["r".to_owned()][..]));
        let stored = json!({"model_chains":{"executor":["x"],"planner":["y"],"reviewer":[]}});
        assert_eq!(super::chains(&stored, &persona).chain(Role::Planner), ["y"]);
    }
}
