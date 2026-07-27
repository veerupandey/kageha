use kageha_contracts::execution::*;
use kageha_workflow::*;
use serde_json::json;
use std::{
    collections::BTreeMap,
    io,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Mutex,
    },
};
fn spec() -> Spec {
    Spec {
        version: 1,
        id: "run".into(),
        namespace: "chat".into(),
        resource: "draft-tab".into(),
        origin: "https://example.test".into(),
        goals: BTreeMap::from([
            ("review".into(), "Draft at Review without submission".into()),
            ("resume".into(), "Original resume attached".into()),
        ]),
        limits: Limits::default(),
    }
}
fn evidence(goals: &[&str]) -> Observation {
    Observation {
        resource: "draft-tab".into(),
        origin: "https://example.test".into(),
        progress_token: goals.join(","),
        satisfied: goals.iter().map(|s| s.to_string()).collect(),
        pending: vec![],
        blocker: None,
    }
}
#[test]
fn a_final_claim_is_not_evidence_and_all_goals_are_required() {
    let s = spec();
    for goals in [vec![], vec!["review"], vec!["resume"]] {
        let (_, d) = evaluate(&s, None, &evidence(&goals), Checkpoint::CandidateFinal).unwrap();
        assert!(matches!(d, Decision::Continue { .. }));
    }
    let (_, d) = evaluate(
        &s,
        None,
        &evidence(&["review", "resume"]),
        Checkpoint::BeforeModel,
    )
    .unwrap();
    assert!(matches!(d, Decision::Verified { .. }));
}
#[test]
fn pending_receipts_prevent_false_completion() {
    let mut o = evidence(&["review", "resume"]);
    o.pending.push("original-upload".into());
    let (state, d) = evaluate(&spec(), None, &o, Checkpoint::CandidateFinal).unwrap();
    assert_eq!(state.phase, Phase::Repairing);
    assert!(
        matches!(d,Decision::Continue{instruction}if instruction.contains("original-upload")&&instruction.contains("without replaying"))
    );
}
#[test]
fn resource_and_origin_mismatches_never_authorize_input() {
    for (resource, origin) in [
        ("other-tab", "https://example.test"),
        ("draft-tab", "https://evil.test"),
    ] {
        let mut o = evidence(&["review", "resume"]);
        o.resource = resource.into();
        o.origin = origin.into();
        assert!(matches!(
            evaluate(&spec(), None, &o, Checkpoint::BeforeMutation)
                .unwrap()
                .1,
            Decision::Stop {
                phase: Phase::Blocked,
                ..
            }
        ));
    }
}
#[test]
fn completed_runs_are_revalidated_instead_of_trusting_cached_success() {
    let (s, _) = evaluate(
        &spec(),
        None,
        &evidence(&["review", "resume"]),
        Checkpoint::BeforeModel,
    )
    .unwrap();
    assert_eq!(s.phase, Phase::Verified);
    assert!(matches!(
        evaluate(&spec(), Some(&s), &evidence(&[]), Checkpoint::BeforeModel)
            .unwrap()
            .1,
        Decision::Proceed { .. }
    ));
}
#[test]
fn unknown_goals_and_changed_contracts_are_rejected() {
    let (s, _) = evaluate(&spec(), None, &evidence(&[]), Checkpoint::BeforeModel).unwrap();
    let mut changed = spec();
    changed.goals.clear();
    assert!(evaluate(&changed, Some(&s), &evidence(&[]), Checkpoint::BeforeModel).is_err());
    assert!(evaluate(
        &spec(),
        None,
        &evidence(&["model-invented"]),
        Checkpoint::BeforeModel
    )
    .is_err());
}
#[test]
fn no_progress_stops_truthfully_and_is_bounded() {
    let s = spec();
    let mut state = None;
    for attempt in 1..=s.limits.no_progress + 1 {
        let (next, d) = evaluate(
            &s,
            state.as_ref(),
            &evidence(&[]),
            Checkpoint::CandidateFinal,
        )
        .unwrap();
        state = Some(next);
        if attempt >= s.limits.no_progress + 1 {
            assert!(matches!(
                d,
                Decision::Stop {
                    phase: Phase::Blocked,
                    ..
                }
            ));
        }
    }
}
#[test]
fn known_partial_progress_does_not_consume_the_no_progress_budget() {
    let s = spec();
    let mut state = None;
    for i in 0..12 {
        let mut o = evidence(&[]);
        o.progress_token = format!("verified-field-{i}");
        let (next, _) = evaluate(&s, state.as_ref(), &o, Checkpoint::BeforeModel).unwrap();
        let (next, d) = evaluate(&s, Some(&next), &o, Checkpoint::CandidateFinal).unwrap();
        assert!(matches!(d, Decision::Continue { .. }));
        state = Some(next);
    }
}
#[test]
fn real_external_input_and_permissions_have_explicit_non_success_states() {
    let mut o = evidence(&[]);
    o.blocker = Some(Blocker {
        question: "User must return browser control".into(),
        awaiting_input: true,
    });
    let (state, d) = evaluate(&spec(), None, &o, Checkpoint::BeforeModel).unwrap();
    assert_eq!(state.phase, Phase::AwaitingInput);
    assert!(matches!(
        d,
        Decision::Stop {
            phase: Phase::AwaitingInput,
            ..
        }
    ));
}
#[test]
fn an_always_changing_page_is_still_bounded_by_the_total_budget() {
    let mut s = spec();
    s.limits.continuations = 2;
    let mut state = None;
    for i in 0..3 {
        let mut o = evidence(&[]);
        o.progress_token = i.to_string();
        let (next, d) = evaluate(&s, state.as_ref(), &o, Checkpoint::CandidateFinal).unwrap();
        state = Some(next);
        if i == 2 {
            assert!(matches!(d, Decision::Stop { .. }));
        }
    }
}
#[derive(Default)]
struct MemoryStore(Mutex<Option<State>>);
impl Store for MemoryStore {
    fn load(&self, _: &str, _: &str) -> io::Result<Option<State>> {
        Ok(self.0.lock().unwrap().clone())
    }
    fn save(&self, previous: Option<u64>, state: &State) -> io::Result<()> {
        let mut v = self.0.lock().unwrap();
        if v.as_ref().map(|s| s.revision) != previous {
            return Err(io::Error::other("revision conflict"));
        }
        *v = Some(state.clone());
        Ok(())
    }
}
struct Adapter {
    store: MemoryStore,
    done: AtomicBool,
}
impl Port for Adapter {
    fn checkpoint(&self, at: Checkpoint, c: &Control<'_>) -> io::Result<Decision> {
        c.check()?;
        checkpoint(
            &self.store,
            &spec(),
            &evidence(if self.done.load(Ordering::Acquire) {
                &["review", "resume"]
            } else {
                &[]
            }),
            at,
        )
    }
}
struct PrematureModel {
    calls: AtomicUsize,
}
impl ModelPort for PrematureModel {
    fn complete(&self, r: ModelRequest, _: &Control<'_>) -> io::Result<AssistantMessage> {
        assert!(r.messages.last().unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("WORKFLOW CONTROLLER"));
        let n = self.calls.fetch_add(1, Ordering::AcqRel);
        Ok(if n < 2 {
            AssistantMessage {
                continuation: None,
                content: json!("I'm done, or please continue next turn"),
                tool_calls: vec![],
            }
        } else {
            AssistantMessage {
                continuation: None,
                content: json!(null),
                tool_calls: vec![ToolCall {
                    id: "finish".into(),
                    name: "complete_form".into(),
                    arguments: json!({}),
                }],
            }
        })
    }
}
#[test]
fn interchangeable_model_and_observer_ports_reject_early_finals_in_one_request() {
    let adapter = Adapter {
        store: MemoryStore::default(),
        done: AtomicBool::new(false),
    };
    let model = PrematureModel {
        calls: AtomicUsize::new(0),
    };
    let guarded = GuardedModel {
        model: &model,
        workflow: &adapter,
    };
    let cancel = AtomicBool::new(false);
    let c = Control {
        cancelled: &cancel,
        deadline: None,
    };
    let r = || ModelRequest {
        messages: vec![json!({"role":"user","content":"Finish the draft and stop at Review"})],
        tools: vec![],
        role: Default::default(),
    };
    assert_eq!(
        guarded.complete(r(), &c).unwrap().tool_calls[0].name,
        "complete_form"
    );
    assert_eq!(model.calls.load(Ordering::Acquire), 3);
    assert_eq!(
        adapter
            .store
            .0
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .continuations,
        2
    );
    adapter.done.store(true, Ordering::Release);
    let answer = guarded.complete(r(), &c).unwrap();
    assert!(answer.tool_calls.is_empty());
    assert!(answer
        .content
        .as_str()
        .unwrap()
        .starts_with("Workflow verified"));
    assert_eq!(model.calls.load(Ordering::Acquire), 3);
}
#[test]
fn cancellation_prevents_more_model_calls() {
    let adapter = Adapter {
        store: MemoryStore::default(),
        done: AtomicBool::new(false),
    };
    let model = PrematureModel {
        calls: AtomicUsize::new(0),
    };
    let cancel = AtomicBool::new(true);
    assert_eq!(
        GuardedModel {
            model: &model,
            workflow: &adapter
        }
        .complete(
            ModelRequest {
                messages: vec![],
                tools: vec![],
                role: Default::default(),
            },
            &Control {
                cancelled: &cancel,
                deadline: None
            }
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::Interrupted
    );
    assert_eq!(model.calls.load(Ordering::Acquire), 0);
}
#[cfg(feature = "sqlite")]
#[test]
fn sqlite_restart_namespace_isolation_and_revision_fencing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workflows.sqlite");
    let store = sqlite::SqliteStore::open(&path).unwrap();
    let (s, _) = evaluate(&spec(), None, &evidence(&[]), Checkpoint::CandidateFinal).unwrap();
    store.save(None, &s).unwrap();
    assert!(store.save(None, &s).is_err());
    drop(store);
    let store = sqlite::SqliteStore::open(&path).unwrap();
    let old = store.load("chat", "run").unwrap().unwrap();
    assert_eq!(old.continuations, 1);
    assert!(store.load("other-chat", "run").unwrap().is_none());
    let (next, _) = evaluate(
        &spec(),
        Some(&old),
        &evidence(&["review"]),
        Checkpoint::CandidateFinal,
    )
    .unwrap();
    store.save(Some(old.revision), &next).unwrap();
    assert!(store.save(Some(old.revision), &next).is_err());
}
#[test]
fn repeated_tool_calls_cannot_evade_the_no_progress_budget() {
    let mut s = spec();
    s.limits.stalled_model_calls = 3;
    let mut o = evidence(&[]);
    o.progress_token = "initial".into();
    let (mut state, _) = evaluate(&s, None, &o, Checkpoint::BeforeModel).unwrap();
    for _ in 0..2 {
        let (next, d) = evaluate(&s, Some(&state), &o, Checkpoint::BeforeModel).unwrap();
        assert!(matches!(d, Decision::Proceed { .. }));
        state = next;
    }
    let (_, d) = evaluate(&s, Some(&state), &o, Checkpoint::BeforeModel).unwrap();
    assert!(matches!(
        d,
        Decision::Stop {
            phase: Phase::Blocked,
            ..
        }
    ));
    let (next, d) = evaluate(
        &s,
        Some(&state),
        &evidence(&["review"]),
        Checkpoint::BeforeModel,
    )
    .unwrap();
    assert!(matches!(d, Decision::Proceed { .. }));
    assert_eq!(next.stalled_model_calls, 0);
}
