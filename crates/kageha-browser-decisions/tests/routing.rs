use kageha_browser_decisions::{
    routing::{self, Action},
    typed::*,
    *,
};
use kageha_contracts::execution::*;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
};
struct Backend {
    calls: AtomicUsize,
    probability: f64,
    malformed: bool,
}
impl DecisionPort for Backend {
    fn decide(&self, r: &typed::Request, c: &Control<'_>) -> io::Result<typed::Result> {
        c.check()?;
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut answers = BTreeMap::new();
        for (id, q) in &r.questions {
            let choice = if id == "batch_safe" || id.starts_with("eligible_") {
                "true".into()
            } else if id.starts_with("nav_") {
                "false".into()
            } else {
                q.criteria
                    .keys()
                    .find(|k| k.starts_with('v'))
                    .unwrap()
                    .clone()
            };
            let probability = if id == "field_0" {
                self.probability
            } else {
                0.99
            };
            let rest = (1. - probability) / (q.criteria.len() - 1) as f64;
            answers.insert(
                id.clone(),
                Answer {
                    confidence: Some(0.99),
                    choice: choice.clone(),
                    probabilities: q
                        .criteria
                        .keys()
                        .map(|k| (k.clone(), if *k == choice { probability } else { rest }))
                        .collect(),
                },
            );
        }
        if self.malformed {
            answers.clear();
        }
        Ok(typed::Result {
            answers,
            usage: json!({}),
        })
    }
}
fn request() -> routing::Request {
    routing::Request {
        observation_id: "obs".into(),
        observation: Observation {
            resource: "tab".into(),
            binding: "profile:tab".into(),
            document: "doc".into(),
            service_epoch: "epoch".into(),
            evidence: json!({"text":"City"}),
        },
        controls: vec![json!({"ref":"e1","role":"textbox","name":"City","value":""})],
        values: BTreeMap::from([("city".into(), json!("Vancouver"))]),
        allowed_actions: BTreeSet::from([
            "fill".into(),
            "click".into(),
            "check".into(),
            "press".into(),
        ]),
        trusted_click_targets: Default::default(),
        pending_actions: false,
    }
}
fn session(mode: Mode, p: f64, malformed: bool) -> (Session, Arc<Backend>) {
    let b = Arc::new(Backend {
        calls: AtomicUsize::new(0),
        probability: p,
        malformed,
    });
    let mut policy = Policy {
        mode: Mode::Active,
        model: "typed".into(),
        ..Policy::default()
    };
    policy.overrides.insert(
        "browser_action".into(),
        Override {
            mode: Some(mode),
            ..Override::default()
        },
    );
    (
        Session::new(
            Scope {
                workspace: "chat".into(),
                run: "run".into(),
                client: "framework".into(),
            },
            policy,
            Models::new(),
        )
        .unwrap()
        .with_decision_backends(BTreeMap::from([(
            "typed".into(),
            b.clone() as Arc<dyn DecisionPort + Send + Sync>,
        )])),
        b,
    )
}
#[test]
fn active_routes_off_and_shadow_cannot() {
    let flag = AtomicBool::new(false);
    let c = Control {
        cancelled: &flag,
        deadline: None,
    };
    for mode in [Mode::Off, Mode::Shadow, Mode::Active] {
        let (s, b) = session(mode, 0.99, false);
        assert_eq!(
            !s.route("Fill City", &request(), &c)
                .unwrap()
                .actions
                .is_empty(),
            mode == Mode::Active
        );
        assert_eq!(
            b.calls.load(Ordering::SeqCst),
            usize::from(mode != Mode::Off)
        );
    }
}
#[test]
fn multiple_questions_produce_one_independent_batch() {
    let (s, b) = session(Mode::Active, 0.99, false);
    let mut r = request();
    r.controls
        .push(json!({"ref":"e2","role":"textbox","name":"Other city","value":""}));
    let flag = AtomicBool::new(false);
    let result = s
        .route(
            "Fill the matching cities",
            &r,
            &Control {
                cancelled: &flag,
                deadline: None,
            },
        )
        .unwrap();
    assert_eq!(result.actions.len(), 2);
    assert_eq!(b.calls.load(Ordering::SeqCst), 1);
}
#[test]
fn low_probability_in_one_answer_does_not_discard_other_fields() {
    let (s, _) = session(Mode::Active, 0.6, false);
    let mut r = request();
    r.controls
        .push(json!({"ref":"e2","role":"textbox","name":"City","value":""}));
    let flag = AtomicBool::new(false);
    let out = s
        .route(
            "Fill City",
            &r,
            &Control {
                cancelled: &flag,
                deadline: None,
            },
        )
        .unwrap();
    assert_eq!(out.actions.len(), 1);
    assert_eq!(out.actions[0].target(), "e2");
}
#[test]
fn malformed_provider_distributions_fall_back() {
    let (s, _) = session(Mode::Active, 0.99, true);
    let flag = AtomicBool::new(false);
    assert!(s
        .route(
            "Fill City",
            &request(),
            &Control {
                cancelled: &flag,
                deadline: None
            }
        )
        .unwrap()
        .actions
        .is_empty());
}
#[test]
fn uncertain_receipt_never_calls_model() {
    let (s, b) = session(Mode::Active, 0.99, false);
    let mut r = request();
    r.pending_actions = true;
    let flag = AtomicBool::new(false);
    let o = s
        .route(
            "Fill City",
            &r,
            &Control {
                cancelled: &flag,
                deadline: None,
            },
        )
        .unwrap();
    assert_eq!(o.status, "pending_action");
    assert_eq!(b.calls.load(Ordering::SeqCst), 0);
}
#[test]
fn same_state_cannot_replay_after_refs_rotate_or_restart() {
    let (s, b) = session(Mode::Active, 0.99, false);
    let flag = AtomicBool::new(false);
    let c = Control {
        cancelled: &flag,
        deadline: None,
    };
    assert_eq!(
        s.route("Fill City", &request(), &c).unwrap().actions.len(),
        1
    );
    let saved = s.checkpoint().unwrap();
    let (resumed, _) = session(Mode::Active, 0.99, false);
    resumed.restore(&saved).unwrap();
    let mut r = request();
    r.observation_id = "new".into();
    r.controls[0]["ref"] = json!("e22");
    assert_eq!(
        resumed.route("Fill City", &r, &c).unwrap().status,
        "unchanged_state"
    );
    assert_eq!(b.calls.load(Ordering::SeqCst), 1);
}
#[test]
fn protected_controls_keys_and_missing_capabilities_rejected() {
    let mut r = request();
    let fill = Action::Fill {
        target: "e1".into(),
        value_id: "city".into(),
    };
    assert!(fill.validate(&r).is_ok());
    r.controls[0]["secure_input"] = json!(true);
    assert!(fill.validate(&r).is_err());
    r.controls[0]["secure_input"] = json!(false);
    r.allowed_actions.clear();
    assert!(fill.validate(&r).is_err());
    r.allowed_actions.insert("press".into());
    assert!(Action::Press {
        target: "e1".into(),
        key: "Enter".into()
    }
    .validate(&r)
    .is_err());
}

#[test]
fn existing_value_presence_is_preserved_without_disclosing_it() {
    let (s, _) = session(Mode::Active, 0.99, false);
    let mut r = request();
    r.controls[0]["value_present"] = json!(true);
    let flag = AtomicBool::new(false);
    assert!(s
        .route(
            "Fill City",
            &r,
            &Control {
                cancelled: &flag,
                deadline: None
            }
        )
        .unwrap()
        .actions
        .is_empty());
}

#[test]
fn matching_value_does_not_override_a_negative_task_scope_decision() {
    struct NotRequested;
    impl DecisionPort for NotRequested {
        fn decide(&self, r: &typed::Request, c: &Control<'_>) -> io::Result<typed::Result> {
            let backend = Backend {
                calls: AtomicUsize::new(0),
                probability: 0.99,
                malformed: false,
            };
            let mut result = backend.decide(r, c)?;
            for (id, answer) in &mut result.answers {
                if id.starts_with("eligible_") {
                    answer.choice = "false".into();
                    answer.probabilities =
                        BTreeMap::from([("false".into(), 1.0), ("true".into(), 0.0)]);
                }
            }
            Ok(result)
        }
    }
    let (session, _) = session(Mode::Active, 0.99, false);
    let session = session.with_decision_backends(BTreeMap::from([(
        "typed".into(),
        Arc::new(NotRequested) as Arc<dyn DecisionPort + Send + Sync>,
    )]));
    let flag = AtomicBool::new(false);
    let result = session
        .route(
            "Inspect the city, do not change it",
            &request(),
            &Control {
                cancelled: &flag,
                deadline: None,
            },
        )
        .unwrap();
    assert!(result.actions.is_empty());
}

#[test]
fn selected_probability_cutoff_is_inclusive() {
    for (probability, expected) in [(0.899999, 0), (0.9, 1), (0.900001, 1)] {
        let (session, _) = session(Mode::Active, probability, false);
        let flag = AtomicBool::new(false);
        let out = session
            .route(
                "Fill City",
                &request(),
                &Control {
                    cancelled: &flag,
                    deadline: None,
                },
            )
            .unwrap();
        assert_eq!(out.actions.len(), expected, "probability={probability}");
    }
}

#[test]
fn high_field_probabilities_do_not_override_doubtful_batch_readiness() {
    struct NotIndependent;
    impl DecisionPort for NotIndependent {
        fn decide(&self, r: &typed::Request, c: &Control<'_>) -> io::Result<typed::Result> {
            let mut answer = Backend {
                calls: AtomicUsize::new(0),
                probability: 0.99,
                malformed: false,
            }
            .decide(r, c)?;
            answer.answers.insert(
                "batch_safe".into(),
                Answer {
                    confidence: Some(0.99),
                    choice: "false".into(),
                    probabilities: BTreeMap::from([("false".into(), 0.99), ("true".into(), 0.01)]),
                },
            );
            Ok(answer)
        }
    }
    let (s, _) = session(Mode::Active, 0.99, false);
    let s = s.with_decision_backends(BTreeMap::from([(
        "typed".into(),
        Arc::new(NotIndependent) as Arc<dyn DecisionPort + Send + Sync>,
    )]));
    let mut r = request();
    r.controls
        .push(json!({"ref":"e2","role":"textbox","name":"Other city","value":""}));
    let flag = AtomicBool::new(false);
    assert_eq!(
        s.route(
            "Fill both cities",
            &r,
            &Control {
                cancelled: &flag,
                deadline: None
            }
        )
        .unwrap()
        .actions
        .len(),
        1
    );
}

#[test]
fn input_and_navigation_never_share_a_decision_batch() {
    struct AllRoutine;
    impl DecisionPort for AllRoutine {
        fn decide(&self, r: &typed::Request, c: &Control<'_>) -> io::Result<typed::Result> {
            let mut answer = Backend {
                calls: AtomicUsize::new(0),
                probability: 0.99,
                malformed: false,
            }
            .decide(r, c)?;
            for (id, a) in &mut answer.answers {
                if id.starts_with("nav_") {
                    a.choice = "true".into();
                    a.probabilities =
                        BTreeMap::from([("true".into(), 0.99), ("false".into(), 0.01)]);
                }
            }
            Ok(answer)
        }
    }
    let (s, _) = session(Mode::Active, 0.99, false);
    let s = s.with_decision_backends(BTreeMap::from([(
        "typed".into(),
        Arc::new(AllRoutine) as Arc<dyn DecisionPort + Send + Sync>,
    )]));
    let mut r = request();
    r.allowed_actions.insert("click".into());
    r.trusted_click_targets.insert("e2".into());
    r.controls
        .push(json!({"ref":"e2","role":"button","name":"Next"}));
    let flag = AtomicBool::new(false);
    let out = s
        .route(
            "Fill City then continue",
            &r,
            &Control {
                cancelled: &flag,
                deadline: None,
            },
        )
        .unwrap();
    assert_eq!(
        out.actions,
        vec![Action::Fill {
            target: "e1".into(),
            value_id: "city".into()
        }]
    );
}

#[test]
fn page_flags_cannot_grant_fast_click_authority_even_with_perfect_model_probability() {
    let (s, b) = session(Mode::Active, 1.0, false);
    let mut r = request();
    r.values.clear();
    r.allowed_actions.insert("click".into());
    r.controls = vec![
        json!({"ref":"e1","role":"button","name":"Delete draft","trusted_click":true,"safe":true}),
    ];
    let flag = AtomicBool::new(false);
    let out = s
        .route(
            "Delete draft",
            &r,
            &Control {
                cancelled: &flag,
                deadline: None,
            },
        )
        .unwrap();
    assert!(out.actions.is_empty());
    assert_eq!(out.status, "no_supported_candidates");
    assert_eq!(b.calls.load(Ordering::SeqCst), 0);
    assert!(Action::Click {
        target: "e1".into()
    }
    .validate(&r)
    .is_err());
}

#[test]
fn an_embedding_host_can_grant_a_known_routine_click_target() {
    struct Yes;
    impl DecisionPort for Yes {
        fn decide(&self, r: &typed::Request, c: &Control<'_>) -> io::Result<typed::Result> {
            c.check()?;
            Ok(typed::Result {
                answers: r
                    .questions
                    .keys()
                    .map(|id| {
                        (
                            id.clone(),
                            Answer {
                                confidence: Some(0.99),
                                choice: "true".into(),
                                probabilities: BTreeMap::from([
                                    ("true".into(), 1.0),
                                    ("false".into(), 0.0),
                                ]),
                            },
                        )
                    })
                    .collect(),
                usage: json!({}),
            })
        }
    }
    let (s, _) = session(Mode::Active, 1.0, false);
    let s = s.with_decision_backends(BTreeMap::from([(
        "typed".into(),
        Arc::new(Yes) as Arc<dyn DecisionPort + Send + Sync>,
    )]));
    let mut r = request();
    r.values.clear();
    r.allowed_actions.insert("click".into());
    r.controls = vec![json!({"ref":"e1","role":"button","name":"Open details"})];
    r.trusted_click_targets.insert("e1".into());
    let flag = AtomicBool::new(false);
    assert_eq!(
        s.route(
            "Open details",
            &r,
            &Control {
                cancelled: &flag,
                deadline: None
            }
        )
        .unwrap()
        .actions,
        vec![Action::Click {
            target: "e1".into()
        }]
    );
}

#[test]
fn batching_requires_the_stronger_cutoff_for_each_field_not_just_batch_readiness() {
    let (s, _) = session(Mode::Active, 0.98, false);
    let mut r = request();
    r.controls
        .push(json!({"ref":"e2","role":"textbox","name":"Other city","value":""}));
    let flag = AtomicBool::new(false);
    assert_eq!(
        s.route(
            "Fill both cities",
            &r,
            &Control {
                cancelled: &flag,
                deadline: None
            }
        )
        .unwrap()
        .actions
        .len(),
        1
    );
    assert!(Policy {
        batch_threshold: 0.49,
        ..Policy::default()
    }
    .validate()
    .is_err());
}

#[test]
fn framework_confidence_default_and_request_override_use_the_provider_score() {
    let backend = Arc::new(Backend {
        calls: AtomicUsize::new(0),
        probability: 0.8,
        malformed: false,
    });
    let make = |config: DecisionConfig| {
        Session::from_decision_config(
            Scope {
                workspace: "chat".into(),
                run: "request".into(),
                client: "external-framework".into(),
            },
            &config,
            BTreeMap::from([(
                "typed".into(),
                backend.clone() as Arc<dyn DecisionPort + Send + Sync>,
            )]),
        )
        .unwrap()
    };
    let config = DecisionConfig::new("typed");
    assert_eq!(config.policy().unwrap().confidence_cutoff, Some(0.90));
    let flag = AtomicBool::new(false);
    let control = Control {
        cancelled: &flag,
        deadline: None,
    };
    let out = make(config.clone())
        .route("Fill City", &request(), &control)
        .unwrap();
    assert_eq!(out.actions.len(), 1);
    assert_eq!(out.score_metric, "provider_confidence");
    assert_eq!(out.minimum_score, Some(0.99));
    assert_eq!(out.minimum_selected_probability, Some(0.8));
    let mut strict = config;
    strict.confidence_cutoff = Some(1.0);
    assert!(make(strict.clone())
        .route("Fill City", &request(), &control)
        .unwrap()
        .actions
        .is_empty());
    assert_eq!(
        make(strict)
            .route_with_overrides(
                "Fill City",
                &request(),
                &DecisionOverrides {
                    decision_model: None,
                    confidence_cutoff: Some(0.9)
                },
                &control
            )
            .unwrap()
            .actions
            .len(),
        1
    );
}

#[test]
fn missing_confidence_never_substitutes_probability_and_model_override_is_exact() {
    struct Missing;
    impl DecisionPort for Missing {
        fn decide(&self, r: &typed::Request, c: &Control<'_>) -> io::Result<typed::Result> {
            let mut out = Backend {
                calls: AtomicUsize::new(0),
                probability: 1.0,
                malformed: false,
            }
            .decide(r, c)?;
            for a in out.answers.values_mut() {
                a.confidence = None;
            }
            Ok(out)
        }
    }
    let good = Arc::new(Backend {
        calls: AtomicUsize::new(0),
        probability: 1.0,
        malformed: false,
    });
    let make = || {
        Session::from_decision_config(
            Scope {
                workspace: "chat".into(),
                run: "request".into(),
                client: "framework".into(),
            },
            &DecisionConfig::new("missing"),
            BTreeMap::from([
                (
                    "missing".into(),
                    Arc::new(Missing) as Arc<dyn DecisionPort + Send + Sync>,
                ),
                (
                    "good".into(),
                    good.clone() as Arc<dyn DecisionPort + Send + Sync>,
                ),
            ]),
        )
        .unwrap()
    };
    let flag = AtomicBool::new(false);
    let control = Control {
        cancelled: &flag,
        deadline: None,
    };
    assert!(make()
        .route("Fill City", &request(), &control)
        .unwrap()
        .actions
        .is_empty());
    assert_eq!(
        make()
            .route_with_overrides(
                "Fill City",
                &request(),
                &DecisionOverrides {
                    decision_model: Some("good".into()),
                    confidence_cutoff: None
                },
                &control
            )
            .unwrap()
            .actions
            .len(),
        1
    );
    assert_eq!(good.calls.load(Ordering::SeqCst), 1);
}

#[test]
fn per_purpose_confidence_overrides_framework_then_request_takes_precedence() {
    let config = DecisionConfig {
        decision_model: "typed".into(),
        confidence_cutoff: Some(1.0),
        batch_confidence_cutoff: None,
    };
    let mut policy = config.policy().unwrap();
    policy
        .overrides
        .get_mut("browser_action")
        .unwrap()
        .confidence_cutoff = Some(0.90);
    let make = || {
        Session::new(
            Scope {
                workspace: "chat".into(),
                run: "run".into(),
                client: "host".into(),
            },
            policy.clone(),
            Models::new(),
        )
        .unwrap()
        .with_decision_backends(BTreeMap::from([(
            "typed".into(),
            Arc::new(Backend {
                calls: AtomicUsize::new(0),
                probability: 1.0,
                malformed: false,
            }) as Arc<dyn DecisionPort + Send + Sync>,
        )]))
    };
    let flag = AtomicBool::new(false);
    let c = Control {
        cancelled: &flag,
        deadline: None,
    };
    assert_eq!(
        make()
            .route("Fill City", &request(), &c)
            .unwrap()
            .actions
            .len(),
        1
    );
    assert!(make()
        .route_with_overrides(
            "Fill City",
            &request(),
            &DecisionOverrides {
                decision_model: None,
                confidence_cutoff: Some(1.0)
            },
            &c
        )
        .unwrap()
        .actions
        .is_empty());
}

#[test]
fn framework_context_configuration_preserves_other_policy_and_inherits_unspecified_cutoffs() {
    let mut base=Policy{model:"existing-advisor".into(),confidence_cutoff:Some(0.92),max_action_decisions:12,batch_confidence_cutoff:0.995,..Policy::default()};
    base.overrides.insert("browser_review".into(),Override{mode:Some(Mode::Active),..Override::default()});
    let policy=DecisionConfig::new("independent-provider").apply_to(&base).unwrap();
    assert_eq!(policy.max_action_decisions,12);assert_eq!(policy.model,"existing-advisor");
    assert_eq!(policy.batch_confidence_cutoff,0.995);assert_eq!(policy.effective(Purpose::Review).mode,Mode::Active);
    assert_eq!(policy.effective(Purpose::Action).model,"independent-provider");
    assert_eq!(policy.overrides["browser_action"].confidence_cutoff,Some(0.92));
    let strict=DecisionConfig{decision_model:"independent-provider".into(),confidence_cutoff:Some(0.97),batch_confidence_cutoff:None}.apply_to(&policy).unwrap();
    assert_eq!(strict.overrides["browser_action"].confidence_cutoff,Some(0.97));
}
