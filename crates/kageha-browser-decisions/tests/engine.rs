use kageha_browser_decisions::*;
use kageha_contracts::{execution::*, policy::*};
use serde_json::json;
use std::{
    collections::BTreeMap,
    io,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
};
struct Model {
    reply: &'static str,
    calls: AtomicUsize,
}
impl ModelPort for Model {
    fn complete(&self, r: ModelRequest, c: &Control<'_>) -> io::Result<AssistantMessage> {
        c.check()?;
        assert!(r.tools.is_empty());
        assert!(c.deadline.is_some());
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(AssistantMessage {
            continuation: None,
            content: json!(self.reply),
            tool_calls: vec![],
        })
    }
}
fn model(reply: &'static str) -> Arc<Model> {
    Arc::new(Model {
        reply,
        calls: AtomicUsize::new(0),
    })
}
fn scope() -> Scope {
    Scope {
        workspace: "chat".into(),
        run: "run".into(),
        client: "agent".into(),
    }
}
fn policy(mode: Mode) -> Policy {
    Policy {
        mode,
        model: "primary".into(),
        ..Policy::default()
    }
}
fn guidance() -> Guidance {
    Guidance {
        version: 1,
        purpose: Purpose::Guidance,
        observation: Observation {
            resource: "tab".into(),
            binding: "tab:1".into(),
            document: "doc".into(),
            service_epoch: "epoch".into(),
            evidence: json!({"text":"Article visible"}),
        },
        allowed: [Advice::ReadContent, Advice::Reconcile]
            .into_iter()
            .collect(),
    }
}
const GOOD: &str = r#"{"recommendation":"read_content","confidence":0.98,"rationale":"The requested content is visible"}"#;
#[test]
fn off_has_no_model_calls_and_shadow_never_applies() {
    for mode in [Mode::Off, Mode::Shadow, Mode::Active] {
        let m = model(GOOD);
        let s = Session::new(
            scope(),
            policy(mode),
            BTreeMap::from([(
                "primary".into(),
                m.clone() as Arc<dyn ModelPort + Send + Sync>,
            )]),
        )
        .unwrap();
        let flag = AtomicBool::new(false);
        let o = s
            .guide(
                "Read article",
                &guidance(),
                &Control {
                    cancelled: &flag,
                    deadline: None,
                },
            )
            .unwrap();
        assert_eq!(o.applied.is_some(), mode == Mode::Active);
        assert_eq!(
            m.calls.load(Ordering::SeqCst),
            if mode == Mode::Off { 0 } else { 1 }
        );
    }
}
#[test]
fn unchanged_evidence_is_cached_and_new_states_are_bounded() {
    let m = model(GOOD);
    let s = Session::new(
        scope(),
        policy(Mode::Active),
        BTreeMap::from([(
            "primary".into(),
            m.clone() as Arc<dyn ModelPort + Send + Sync>,
        )]),
    )
    .unwrap();
    let flag = AtomicBool::new(false);
    let c = Control {
        cancelled: &flag,
        deadline: None,
    };
    assert!(!s.guide("Read", &guidance(), &c).unwrap().cached);
    assert!(s.guide("Read", &guidance(), &c).unwrap().cached);
    for i in 1..4 {
        let mut g = guidance();
        g.observation.document = i.to_string();
        let o = s.guide("Read", &g, &c).unwrap();
        assert_eq!(o.status == "budget_exhausted", i == 3);
    }
    assert_eq!(m.calls.load(Ordering::SeqCst), 3);
}
#[test]
fn low_confidence_invalid_and_forbidden_advice_use_backup() {
    for reply in [
        "not JSON",
        r#"{"recommendation":"wait","confidence":1,"rationale":"Wait"}"#,
        r#"{"recommendation":"read_content","confidence":0.6,"rationale":"Maybe"}"#,
    ] {
        let a = model(reply);
        let b = model(GOOD);
        let mut p = policy(Mode::Active);
        p.fallback_models.push("backup".into());
        let s = Session::new(
            scope(),
            p,
            BTreeMap::from([
                ("primary".into(), a as Arc<dyn ModelPort + Send + Sync>),
                ("backup".into(), b as Arc<dyn ModelPort + Send + Sync>),
            ]),
        )
        .unwrap();
        let flag = AtomicBool::new(false);
        let r = s
            .guide(
                "Read",
                &guidance(),
                &Control {
                    cancelled: &flag,
                    deadline: None,
                },
            )
            .unwrap();
        assert_eq!(r.model.as_deref(), Some("backup"));
        assert_eq!(r.applied, Some(Advice::ReadContent));
    }
}
fn approval() -> ApprovalRequest {
    ApprovalRequest {
        authority: Authority {
            session: "chat".into(),
            turn: "run".into(),
            binding: "tab:1".into(),
            generation: 1,
            resource: "tab".into(),
            input_digest: "a".repeat(64),
        },
        action: "browser_fill".into(),
        class: ActionClass::RoutineFill,
        detail: "Fill City".into(),
    }
}
#[test]
fn global_active_does_not_enable_review_and_stale_reviews_cannot_allow() {
    let m = model(r#"{"input_digest":"wrong","verdict":"allow","rationale":"Approved"}"#);
    let mut p = policy(Mode::Active);
    let flag = AtomicBool::new(false);
    let c = Control {
        cancelled: &flag,
        deadline: None,
    };
    let s = Session::new(
        scope(),
        p.clone(),
        BTreeMap::from([(
            "primary".into(),
            m.clone() as Arc<dyn ModelPort + Send + Sync>,
        )]),
    )
    .unwrap();
    assert_eq!(
        s.review(&approval(), "Fill City", &[], &c).unwrap().verdict,
        ReviewVerdict::Ask
    );
    assert_eq!(m.calls.load(Ordering::SeqCst), 0);
    p.overrides.insert(
        "browser_review".into(),
        Override {
            mode: Some(Mode::Active),
            ..Override::default()
        },
    );
    let s = Session::new(
        scope(),
        p,
        BTreeMap::from([("primary".into(), m as Arc<dyn ModelPort + Send + Sync>)]),
    )
    .unwrap();
    assert_eq!(
        s.review(&approval(), "Fill City", &[], &c).unwrap().verdict,
        ReviewVerdict::Ask
    );
}
#[test]
fn cancellation_and_invalid_policy_do_not_call_models() {
    let m = model(GOOD);
    let s = Session::new(
        scope(),
        policy(Mode::Active),
        BTreeMap::from([(
            "primary".into(),
            m.clone() as Arc<dyn ModelPort + Send + Sync>,
        )]),
    )
    .unwrap();
    let flag = AtomicBool::new(true);
    assert_eq!(
        s.guide(
            "Read",
            &guidance(),
            &Control {
                cancelled: &flag,
                deadline: None
            }
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::Interrupted
    );
    assert_eq!(m.calls.load(Ordering::SeqCst), 0);
    let mut p = policy(Mode::Active);
    p.timeout_s = f64::NAN;
    assert!(p.validate().is_err());
}

#[test]
fn restored_runs_keep_cache_and_budget_but_cannot_cross_scope() {
    let m = model(GOOD);
    let models = BTreeMap::from([(
        "primary".into(),
        m.clone() as Arc<dyn ModelPort + Send + Sync>,
    )]);
    let mut p = policy(Mode::Active);
    p.max_states = 1;
    let first = Session::new(scope(), p.clone(), models.clone()).unwrap();
    let flag = AtomicBool::new(false);
    let c = Control {
        cancelled: &flag,
        deadline: None,
    };
    first.guide("Read", &guidance(), &c).unwrap();
    let checkpoint = first.checkpoint().unwrap();
    let resumed = Session::new(scope(), p.clone(), models.clone()).unwrap();
    resumed.restore(&checkpoint).unwrap();
    assert!(resumed.guide("Read", &guidance(), &c).unwrap().cached);
    let mut changed = guidance();
    changed.observation.binding = "another-profile".into();
    assert_eq!(
        resumed.guide("Read", &changed, &c).unwrap().status,
        "budget_exhausted"
    );
    assert_eq!(m.calls.load(Ordering::SeqCst), 1);
    let mut foreign = scope();
    foreign.workspace = "another-chat".into();
    assert!(Session::new(foreign, p, models)
        .unwrap()
        .restore(&checkpoint)
        .is_err());
    let mut corrupted = checkpoint;
    corrupted["state"]["states"] = json!(999);
    assert!(resumed.restore(&corrupted).is_err());
}

#[test]
fn semantic_evidence_ignores_rotating_refs_and_masks_input_values() {
    let mut page = json!({"observation_id":"old", "controls":[{"ref":"e1", "id":"city", "role":"textbox", "name":"City", "value":"Vancouver"},{"id":"password", "secure_input":true,"value":"a-secret-password"}]});
    let first = page_evidence(&page);
    let serialized = first.to_string();
    assert!(!serialized.contains("Vancouver"));
    assert!(!serialized.contains("a-secret-password"));
    page["observation_id"] = json!("new");
    page["controls"][0]["ref"] = json!("e22");
    assert_eq!(first, page_evidence(&page));
    page["controls"][0]["value"] = json!("Toronto");
    assert_ne!(first, page_evidence(&page));
}

struct ReviewModel;
impl ModelPort for ReviewModel {
    fn complete(
        &self,
        request: ModelRequest,
        control: &Control<'_>,
    ) -> io::Result<AssistantMessage> {
        control.check()?;
        assert!(request.tools.is_empty());
        Ok(AssistantMessage { continuation: None, content: json!(json!({"input_digest":"a".repeat(64),"verdict":"allow","rationale":"Explicitly authorized city field"}).to_string()), tool_calls: vec![] })
    }
}
#[test]
fn review_is_exact_scoped_and_shadow_never_authorizes() {
    let flag = AtomicBool::new(false);
    let c = Control {
        cancelled: &flag,
        deadline: None,
    };
    for mode in [Mode::Shadow, Mode::Active] {
        let mut p = policy(Mode::Off);
        p.overrides.insert(
            "browser_review".into(),
            Override {
                mode: Some(mode),
                model: Some("primary".into()),
                ..Override::default()
            },
        );
        let s = Session::new(
            scope(),
            p,
            BTreeMap::from([(
                "primary".into(),
                Arc::new(ReviewModel) as Arc<dyn ModelPort + Send + Sync>,
            )]),
        )
        .unwrap();
        let result = s.review(&approval(), "Fill City", &[], &c).unwrap();
        assert_eq!(
            result.verdict,
            if mode == Mode::Active {
                ReviewVerdict::Allow
            } else {
                ReviewVerdict::Ask
            }
        );
        let mut foreign = approval();
        foreign.authority.turn = "another-turn".into();
        assert!(s.review(&foreign, "Fill City", &[], &c).is_err());
    }
}
