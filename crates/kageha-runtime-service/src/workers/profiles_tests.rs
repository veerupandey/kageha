//! Profile-port conformance uses the real host/tool wrappers, not a UI fixture.
use super::*;
use kageha_contracts::policy::*;
use kageha_runtime_store::SqliteRunStore;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    RwLock,
};
struct Profiles(RwLock<kageha_agents::Definition>);
impl kageha_agents::Port for Profiles {
    fn get(&self, id: &str) -> io::Result<Option<kageha_agents::Definition>> {
        let d = self.0.read().unwrap();
        Ok((d.id() == id).then(|| d.clone()))
    }
    fn list(&self) -> io::Result<Vec<kageha_agents::Definition>> {
        Ok(vec![self.0.read().unwrap().clone()])
    }
}
fn definition(prompt: &str) -> kageha_agents::Definition {
    kageha_agents::Definition::parse(json!({"id":"reader","name":"Reader","system_prompt":prompt,"tool_policy":{"allow":["read_*"],"risk_class_overrides":{"read_file":"high"}}})).unwrap()
}
struct Base(Arc<AtomicUsize>);
impl ToolPort for Base {
    fn discover(&self) -> io::Result<Vec<Value>> {
        Ok(vec![json!({"function":{"name":"read_file"}})])
    }
    fn binding(&self, _: &ToolContext, _: &ToolCall) -> io::Result<ToolBinding> {
        Ok(ToolBinding {
            binding: "fixture".into(),
            generation: 1,
            resource: "file".into(),
            class: ActionClass::ReadOnly,
            detail: "Read fixture".into(),
            fingerprint: "fixed".into(),
        })
    }
    fn execute(&self, _: &ToolCall, _: &Control<'_>) -> io::Result<ToolReply> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ToolReply::success("read".into()))
    }
}
struct Flip(Arc<Profiles>);
impl ApprovalPort for Flip {
    fn authorize_once(&self, _: &ApprovalRequest, _: &Control<'_>) -> io::Result<bool> {
        *self.0 .0.write().unwrap() = definition("changed during approval");
        Ok(true)
    }
}
#[test]
fn changed_profile_during_approval_blocks_only_affected_profile() {
    let profiles = Arc::new(Profiles(RwLock::new(definition("original"))));
    exercise_profile_change(profiles.clone(), &Flip(profiles));
}
#[test]
fn persistent_profile_changes_fence_pending_authority() {
    use kageha_agents::repository::{sqlite::SqliteRepository, Kind, Repository};
    struct PersistedFlip(Arc<SqliteRepository>);
    impl ApprovalPort for PersistedFlip {
        fn authorize_once(&self, _: &ApprovalRequest, _: &Control<'_>) -> io::Result<bool> {
            self.0.write(
                Kind::Agent,
                "reader",
                1,
                Some(definition("changed in repository").body().clone()),
                2.0,
            )?;
            Ok(true)
        }
    }
    let d = tempfile::tempdir().unwrap();
    let profiles =
        Arc::new(SqliteRepository::open(&d.path().join("profiles.sqlite"), "test").unwrap());
    profiles
        .write(
            Kind::Agent,
            "reader",
            0,
            Some(definition("original").body().clone()),
            1.0,
        )
        .unwrap();
    exercise_profile_change(profiles.clone(), &PersistedFlip(profiles));
}
fn exercise_profile_change(
    profiles: Arc<dyn kageha_agents::Port>,
    approve: &(dyn ApprovalPort + Sync),
) {
    let d = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteRunStore::open(&d.path().join("runtime.sqlite")).unwrap());
    let composition = Composition::new_with_profiles(
        Config {
            source: d.path().into(),
            journal: d.path().join("worktrees.sqlite"),
            git: "/usr/bin/git".into(),
            document_worker: None,
            local_files: true,
            specialists: vec![],
        },
        profiles.clone(),
    )
    .unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let base = Arc::new(Base(count.clone()));
    let spec = RunSpec {
        id: "run".into(),
        session: "s".into(),
        turn: "t".into(),
        request: "r".into(),
        parent: None,
        parent_run: None,
        agent: "reader".into(),
        objective: "read".into(),
        depth: 0,
        input: json!({"messages":[{"role":"user","content":"read"}]}),
    };
    let spec = composition.normalize(&spec, store.as_ref()).unwrap();
    let run = store.stage(&spec, 4).unwrap();
    let scoped = composition
        .tools(base.clone(), &run, store.clone())
        .unwrap();
    let call = ToolCall {
        id: "read".into(),
        name: "read_file".into(),
        arguments: json!({}),
    };
    let context = ToolContext {
        session: "s".into(),
        turn: "t".into(),
    };
    assert_eq!(
        scoped.binding(&context, &call).unwrap().class,
        ActionClass::Restricted
    );
    assert!(scoped.mutation_binding(&context, &call).unwrap().is_none());
    let policy = kageha_policy::DefaultPolicy;
    let bound = crate::BoundTools {
        base: scoped.as_ref(),
        hooks: None,
        completed_calls: Default::default(),
        completed_mutations: Default::default(),
        journal: None,
        model: None,
        store: store.as_ref(),
        run: &run,
        policy: &policy,
        approvals: Some(approve),
    };
    let active = AtomicBool::new(false);
    let control = Control {
        cancelled: &active,
        deadline: None,
    };
    assert!(bound.execute(&call, &control).is_err());
    assert_eq!(count.load(Ordering::SeqCst), 0);
    assert!(store.effect_get("t", "tool:read").unwrap().is_none());
    let mut independent = spec.clone();
    independent.id = "other".into();
    independent.session = "other".into();
    independent.turn = "other".into();
    independent.request = "other".into();
    independent.agent = "main".into();
    independent
        .input
        .as_object_mut()
        .unwrap()
        .remove("specialist_binding");
    let independent = composition.normalize(&independent, store.as_ref()).unwrap();
    let run = store.stage(&independent, 4).unwrap();
    let tools = composition.tools(base, &run, store.clone()).unwrap();
    let bound = crate::BoundTools {
        base: tools.as_ref(),
        hooks: None,
        completed_calls: Default::default(),
        completed_mutations: Default::default(),
        journal: None,
        model: None,
        store: store.as_ref(),
        run: &run,
        policy: &policy,
        approvals: None,
    };
    assert!(!bound.execute(&call, &control).unwrap().is_error);
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

#[test]
fn scoped_skill_reader_uses_specialist_policy_and_pinned_package() {
    use kageha_skills::{registry::Registry, Package};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("skills/registry.sqlite");
    let mut registry = Registry::open(&path).unwrap();
    registry
        .install(
            &Package::plain("review", "Review checklist").unwrap(),
            "fixture",
            None,
        )
        .unwrap();
    let profile = kageha_agents::Definition::parse(json!({
        "id":"reader","name":"Reader","pinned_skills":["review"],
        "tool_policy":{"allow":["read_skill_resource"],"risk_class_overrides":{"read_skill_resource":"high"}}
    })).unwrap();
    let mut composition = Composition::new_with_profiles(
        Config {
            source: dir.path().into(),
            journal: dir.path().join("worktrees.sqlite"),
            git: "/usr/bin/git".into(),
            document_worker: None,
            local_files: true,
            specialists: vec![],
        },
        Arc::new(kageha_agents::Catalog::new(vec![profile]).unwrap()),
    )
    .unwrap();
    Composition::configure_resources(
        &mut composition,
        crate::activation::Resources {
            skills: Some(path),
            ..Default::default()
        },
    )
    .unwrap();
    let store = Arc::new(SqliteRunStore::open(&dir.path().join("runtime.sqlite")).unwrap());
    let spec = composition
        .normalize(
            &RunSpec {
                id: "run".into(),
                session: "s".into(),
                turn: "t".into(),
                request: "r".into(),
                parent: None,
                parent_run: None,
                agent: "reader".into(),
                objective: "review".into(),
                depth: 0,
                input: json!({"messages":[{"role":"user","content":"review"}]}),
            },
            store.as_ref(),
        )
        .unwrap();
    let run = store.stage(&spec, 4).unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let tools = composition
        .tools(Arc::new(Base(count.clone())), &run, store)
        .unwrap();
    let names = tools.discover().unwrap();
    assert_eq!(names.len(), 1);
    assert_eq!(names[0]["function"]["name"], crate::skill_tools::READ);
    let call = ToolCall {
        id: "read".into(),
        name: crate::skill_tools::READ.into(),
        arguments: json!({"skill":"review","path":"SKILL.md"}),
    };
    let context = ToolContext {
        session: "s".into(),
        turn: "t".into(),
    };
    let authority = tools.binding(&context, &call).unwrap();
    assert_eq!(authority.class, ActionClass::Restricted);
    assert!(tools.mutation_binding(&context, &call).unwrap().is_none());
    let cancelled = AtomicBool::new(false);
    let control = Control {
        cancelled: &cancelled,
        deadline: None,
    };
    assert_eq!(
        tools
            .execute_authorized_in(&context, &call, &authority, &control)
            .unwrap()
            .content,
        "Review checklist"
    );
    registry.change("review", "archive", None).unwrap();
    assert!(tools
        .execute_authorized_in(&context, &call, &authority, &control)
        .is_err());
    assert_eq!(count.load(Ordering::SeqCst), 0);
}
