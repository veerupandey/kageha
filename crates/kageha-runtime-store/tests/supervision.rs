use kageha_contracts::*;
use kageha_runtime::lifecycle::*;
use kageha_runtime_store::SqliteRunStore;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
fn spec(id: &str, depth: u32) -> RunSpec {
    RunSpec {
        id: id.into(),
        session: format!("session-{id}"),
        turn: format!("turn-{id}"),
        request: id.into(),
        parent: None,
        parent_run: None,
        agent: "fixture".into(),
        objective: id.into(),
        depth,
        input: json!({"portable":true}),
    }
}
#[derive(Default)]
struct Fixture {
    executed: Mutex<Vec<String>>,
    committed: Mutex<HashMap<String, Completion>>,
    blocked: AtomicBool,
    fail_publication: AtomicBool,
}
impl RunExecutor for Fixture {
    fn execute(&self, run: &RunRecord, cancel: Arc<AtomicBool>) -> Result<Completion> {
        self.executed.lock().unwrap().push(run.spec.id.clone());
        while self.blocked.load(Ordering::SeqCst) && !cancel.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(Completion {
            state: if cancel.load(Ordering::SeqCst) {
                RunState::Cancelled
            } else {
                RunState::Succeeded
            },
            result: json!({"message":run.spec.objective}),
        })
    }
    fn inspect(&self, run: &RunRecord) -> Result<Option<Completion>> {
        Ok(self.committed.lock().unwrap().get(&run.spec.id).cloned())
    }
    fn publish(&self, _: &RunRecord) -> Result<()> {
        if self.fail_publication.load(Ordering::SeqCst) {
            Err(ServiceError::new(
                ErrorCode::Unavailable,
                "Projection unavailable",
            ))
        } else {
            Ok(())
        }
    }
}
fn wait(mut check: impl FnMut() -> bool) {
    let end = std::time::Instant::now() + Duration::from_secs(3);
    while !check() {
        assert!(std::time::Instant::now() < end, "condition timed out");
        std::thread::sleep(Duration::from_millis(3));
    }
}
fn stop(supervisor: &Supervisor) {
    supervisor.shutdown().unwrap();
    supervisor.join(Duration::from_secs(3)).unwrap();
}
#[test]
fn staged_and_cancelled_work_never_executes() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteRunStore::open(&root.path().join("runtime.sqlite")).unwrap());
    let executor = Arc::new(Fixture::default());
    let supervisor = Supervisor::new(
        store.clone(),
        executor.clone(),
        Limits {
            per_depth: 1,
            queue_per_depth: 2,
        },
    )
    .unwrap();
    supervisor.stage(&spec("staged", 0)).unwrap();
    supervisor.stage(&spec("cancelled", 0)).unwrap();
    supervisor.cancel("cancelled").unwrap();
    supervisor.start().unwrap();
    std::thread::sleep(Duration::from_millis(30));
    assert!(executor.executed.lock().unwrap().is_empty());
    supervisor.activate("staged").unwrap();
    wait(|| store.get("staged").unwrap().unwrap().state.terminal());
    assert_eq!(*executor.executed.lock().unwrap(), vec!["staged"]);
    stop(&supervisor);
}
#[test]
fn admission_is_bounded_idempotent_and_session_serial() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(&root.path().join("runtime.sqlite")).unwrap();
    let first = spec("first", 0);
    store.stage(&first, 2).unwrap();
    store.stage(&first, 2).unwrap();
    let mut second = spec("second", 0);
    second.session = first.session.clone();
    store.stage(&second, 2).unwrap();
    assert!(matches!(
        store.stage(&spec("overflow", 0), 2).unwrap_err().code,
        ErrorCode::Conflict
    ));
    store.activate("first").unwrap();
    store.activate("second").unwrap();
    let claimed = store.claim(0).unwrap().unwrap();
    assert_eq!(claimed.spec.id, "first");
    assert!(store.claim(0).unwrap().is_none());
    store
        .finish(
            "first",
            claimed.lease.as_deref().unwrap(),
            Completion {
                state: RunState::Succeeded,
                result: Value::Null,
            },
        )
        .unwrap();
    assert_eq!(store.claim(0).unwrap().unwrap().spec.id, "second");
}
#[test]
fn nested_capacity_and_scoped_cancellation_do_not_stop_independent_work() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteRunStore::open(&root.path().join("runtime.sqlite")).unwrap());
    let executor = Arc::new(Fixture::default());
    executor.blocked.store(true, Ordering::SeqCst);
    let supervisor = Supervisor::new(
        store.clone(),
        executor.clone(),
        Limits {
            per_depth: 1,
            queue_per_depth: 8,
        },
    )
    .unwrap();
    let parent = spec("parent", 0);
    supervisor.stage(&parent).unwrap();
    supervisor.activate("parent").unwrap();
    supervisor.start().unwrap();
    wait(|| executor.executed.lock().unwrap().contains(&"parent".into()));
    let mut child = spec("child", 1);
    child.parent = Some(parent.session.clone());
    child.parent_run = Some(parent.id.clone());
    supervisor.stage(&child).unwrap();
    supervisor.activate("child").unwrap();
    wait(|| executor.executed.lock().unwrap().contains(&"child".into()));
    supervisor.stage(&spec("independent", 2)).unwrap();
    supervisor.activate("independent").unwrap();
    wait(|| {
        executor
            .executed
            .lock()
            .unwrap()
            .contains(&"independent".into())
    });
    supervisor.cancel("parent").unwrap();
    wait(|| store.get("child").unwrap().unwrap().state == RunState::Cancelled);
    assert_eq!(
        store.get("independent").unwrap().unwrap().state,
        RunState::Running
    );
    stop(&supervisor);
}
#[test]
fn parent_cancellation_is_an_admission_barrier_for_existing_queued_children() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(&root.path().join("runtime.sqlite")).unwrap();
    let parent = spec("parent", 0);
    store.stage(&parent, 8).unwrap();
    store.activate("parent").unwrap();
    store.claim(0).unwrap();
    let mut child = spec("child", 1);
    child.parent = Some(parent.session.clone());
    child.parent_run = Some(parent.id.clone());
    store.stage(&child, 8).unwrap();
    store.activate("child").unwrap();
    store.cancel("parent").unwrap();
    assert!(store.claim(1).unwrap().is_none());
    assert_eq!(
        store.get("child").unwrap().unwrap().state,
        RunState::Cancelled
    );
    let mut late = child;
    late.id = "late".into();
    assert!(store.stage(&late, 8).is_err());
}
#[test]
fn restart_recovers_committed_result_and_interrupts_unknown_execution_without_replay() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("runtime.sqlite");
    let store = Arc::new(SqliteRunStore::open(&path).unwrap());
    for id in ["committed", "unknown"] {
        store.stage(&spec(id, 0), 8).unwrap();
        store.activate(id).unwrap();
        store.claim(0).unwrap();
    }
    drop(store);
    let store = Arc::new(SqliteRunStore::open(&path).unwrap());
    let executor = Arc::new(Fixture::default());
    executor.committed.lock().unwrap().insert(
        "committed".into(),
        Completion {
            state: RunState::Succeeded,
            result: json!({"message":"durable"}),
        },
    );
    let supervisor = Supervisor::new(store.clone(), executor.clone(), Limits::default()).unwrap();
    supervisor.recover().unwrap();
    assert_eq!(
        store.get("committed").unwrap().unwrap().state,
        RunState::Succeeded
    );
    assert_eq!(
        store.get("unknown").unwrap().unwrap().state,
        RunState::Interrupted
    );
    assert!(executor.executed.lock().unwrap().is_empty());
}
/// An executor whose restart decision is chosen per run id.
struct Restarting {
    inner: Fixture,
    decisions: HashMap<String, Restart>,
}
impl RunExecutor for Restarting {
    fn execute(&self, run: &RunRecord, cancel: Arc<AtomicBool>) -> Result<Completion> {
        self.inner.execute(run, cancel)
    }
    fn restart(&self, run: &RunRecord) -> Result<Restart> {
        Ok(self.decisions.get(&run.spec.id).copied().unwrap_or(Restart::Inspect))
    }
    fn inspect(&self, run: &RunRecord) -> Result<Option<Completion>> {
        self.inner.inspect(run)
    }
    fn publish(&self, run: &RunRecord) -> Result<()> {
        self.inner.publish(run)
    }
}
#[test]
fn restart_resumes_or_holds_running_work_as_the_executor_decides() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("runtime.sqlite");
    let store = Arc::new(SqliteRunStore::open(&path).unwrap());
    for id in ["resume", "hold", "inspect", "stopped"] {
        store.stage(&spec(id, 0), 8).unwrap();
        store.activate(id).unwrap();
        store.claim(0).unwrap();
    }
    store.cancel("stopped").unwrap();
    drop(store);
    let store = Arc::new(SqliteRunStore::open(&path).unwrap());
    let executor = Arc::new(Restarting {
        inner: Fixture::default(),
        decisions: HashMap::from([
            ("resume".to_owned(), Restart::Resume),
            ("hold".to_owned(), Restart::Hold),
            // A stop request wins over the executor's decision.
            ("stopped".to_owned(), Restart::Resume),
        ]),
    });
    let supervisor = Supervisor::new(store.clone(), executor.clone(), Limits::default()).unwrap();
    supervisor.recover().unwrap();
    let state = |id: &str| store.get(id).unwrap().unwrap().state;
    assert_eq!(state("resume"), RunState::Queued);
    assert!(store.get("resume").unwrap().unwrap().lease.is_none());
    assert_eq!(state("hold"), RunState::Running);
    assert_eq!(state("inspect"), RunState::Interrupted);
    assert_eq!(state("stopped"), RunState::Cancelled);
    supervisor.start().unwrap();
    wait(|| state("resume") == RunState::Succeeded);
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(state("hold"), RunState::Running, "a held run has no worker");
    supervisor.resume("hold").unwrap();
    wait(|| state("hold") == RunState::Succeeded);
    assert_eq!(*executor.inner.executed.lock().unwrap(), vec!["resume", "hold"]);
    // Only a running run can be requeued.
    assert_eq!(supervisor.resume("hold").unwrap_err().code, ErrorCode::Conflict);
    stop(&supervisor);
}
#[test]
fn steering_and_effect_receipts_survive_restart_without_replaying_unknown_input() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("runtime.sqlite");
    let store = SqliteRunStore::open(&path).unwrap();
    store.stage(&spec("run", 0), 8).unwrap();
    let message = Steering {
        id: "one".into(),
        body: "revised criteria".into(),
    };
    store.steer("run", &message).unwrap();
    store.steer("run", &message).unwrap();
    assert!(store
        .steer(
            "run",
            &Steering {
                id: "one".into(),
                body: "changed".into()
            }
        )
        .is_err());
    assert!(store
        .effect_begin("turn-run", "unknown", "hash")
        .unwrap()
        .is_none());
    assert!(store
        .effect_begin("turn-run", "done", "hash2")
        .unwrap()
        .is_none());
    store
        .effect_finish("turn-run", "done", "hash2", json!({"receipt":"confirmed"}))
        .unwrap();
    drop(store);
    let store = SqliteRunStore::open(&path).unwrap();
    assert_eq!(store.steering("run").unwrap()[0].body, "revised criteria");
    assert!(store.steering("run").unwrap().is_empty());
    assert_eq!(store.steering_history("run").unwrap().len(), 1);
    assert!(matches!(
        store
            .effect_begin("turn-run", "unknown", "hash")
            .unwrap_err()
            .code,
        ErrorCode::Uncertain
    ));
    assert_eq!(
        store
            .effect_begin("turn-run", "done", "hash2")
            .unwrap()
            .unwrap()["receipt"],
        "confirmed"
    );
}
#[test]
fn obsolete_execution_cannot_overwrite_recovered_result() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(&root.path().join("runtime.sqlite")).unwrap();
    store.stage(&spec("one", 0), 4).unwrap();
    store.activate("one").unwrap();
    let old = store.claim(0).unwrap().unwrap();
    store
        .recover(
            "one",
            Completion {
                state: RunState::Interrupted,
                result: json!({"message":"inspect"}),
            },
        )
        .unwrap();
    assert!(matches!(
        store
            .finish(
                "one",
                old.lease.as_deref().unwrap(),
                Completion {
                    state: RunState::Succeeded,
                    result: Value::Null
                }
            )
            .unwrap_err()
            .code,
        ErrorCode::StaleGeneration
    ));
    assert_eq!(
        store.get("one").unwrap().unwrap().state,
        RunState::Interrupted
    );
}
#[test]
fn projection_retry_does_not_execute_the_run_again() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteRunStore::open(&root.path().join("runtime.sqlite")).unwrap());
    let executor = Arc::new(Fixture::default());
    let supervisor = Supervisor::new(
        store.clone(),
        executor.clone(),
        Limits {
            per_depth: 1,
            queue_per_depth: 8,
        },
    )
    .unwrap();
    supervisor.stage(&spec("one", 0)).unwrap();
    supervisor.activate("one").unwrap();
    executor.fail_publication.store(true, Ordering::SeqCst);
    supervisor.start().unwrap();
    wait(|| store.get("one").unwrap().unwrap().state == RunState::Succeeded);
    assert!(!store.publications().unwrap().is_empty());
    executor.fail_publication.store(false, Ordering::SeqCst);
    supervisor.flush().unwrap();
    assert!(store.publications().unwrap().is_empty());
    assert_eq!(executor.executed.lock().unwrap().len(), 1);
    stop(&supervisor);
}
#[test]
fn journal_refuses_to_share_application_tables() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("application.sqlite");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute("CREATE TABLE native_chat_turns(id TEXT)", [])
        .unwrap();
    drop(db);
    assert!(SqliteRunStore::open(&path).is_err());
}

#[test]
fn deletion_fences_reimport_and_effect_replay() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(&root.path().join("runtime.sqlite")).unwrap();
    let one = spec("deleted", 0);
    store.stage(&one, 4).unwrap();
    store.cancel(&one.id).unwrap();
    let record = store.get(&one.id).unwrap().unwrap();
    let effect = Effect {
        scope: one.turn.clone(),
        id: "mutation".into(),
        digest: "digest".into(),
        receipt: Some(json!({"ok":true})),
    };
    store.effect_import(&effect).unwrap();
    store
        .erase_session(&one.session, &[one.turn.clone()])
        .unwrap();
    store.import_run(&record).unwrap();
    store.effect_import(&effect).unwrap();
    assert!(store.get(&one.id).unwrap().is_none());
    assert!(store.effect_get(&one.turn, "mutation").unwrap().is_none());
    assert!(store.stage(&one, 4).is_err());
    assert!(store.effect_begin(&one.turn, "mutation", "digest").is_err());
}

#[test]
fn imported_uncertainty_never_downgrades_a_confirmed_effect() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(&root.path().join("runtime.sqlite")).unwrap();
    let mut effect = Effect {
        scope: "scope".into(),
        id: "call".into(),
        digest: "hash".into(),
        receipt: None,
    };
    store.effect_import(&effect).unwrap();
    assert!(store.effect_begin("scope", "call", "hash").is_err());
    effect.receipt = Some(json!({"ok":true}));
    store.effect_import(&effect).unwrap();
    effect.receipt = None;
    store.effect_import(&effect).unwrap();
    assert_eq!(
        store.effect_begin("scope", "call", "hash").unwrap(),
        Some(json!({"ok":true}))
    );
    effect.receipt = Some(json!({"different":true}));
    assert!(store.effect_import(&effect).is_err());
}

#[test]
fn an_old_incarnation_cannot_publish_a_late_receipt_before_recovery() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("runtime.sqlite");
    let old = {
        let store = SqliteRunStore::open(&path).unwrap();
        store.stage(&spec("old", 0), 4).unwrap();
        store.activate("old").unwrap();
        store.claim(0).unwrap().unwrap()
    };
    let store = SqliteRunStore::open(&path).unwrap();
    assert!(store
        .finish(
            "old",
            old.lease.as_deref().unwrap(),
            Completion {
                state: RunState::Succeeded,
                result: json!({"late":true})
            }
        )
        .is_err());
    assert_eq!(store.get("old").unwrap().unwrap().state, RunState::Running);
    assert_eq!(store.late_receipts("old").unwrap().len(), 1);
}

#[test]
fn recovery_preserves_a_durable_cancellation_decision() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("runtime.sqlite");
    {
        let store = SqliteRunStore::open(&path).unwrap();
        store.stage(&spec("cancel", 0), 4).unwrap();
        store.activate("cancel").unwrap();
        store.claim(0).unwrap();
        store.cancel("cancel").unwrap();
    }
    let store = SqliteRunStore::open(&path).unwrap();
    let result = store
        .recover(
            "cancel",
            Completion {
                state: RunState::Succeeded,
                result: json!({"committed_evidence":"retained"}),
            },
        )
        .unwrap();
    assert_eq!(result.state, RunState::Cancelled);
    assert_eq!(
        result.completion.unwrap().result["committed_evidence"],
        "retained"
    );
}

#[test]
fn shutdown_signals_all_executors_when_projection_is_unavailable() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteRunStore::open(&root.path().join("runtime.sqlite")).unwrap());
    let executor = Arc::new(Fixture::default());
    executor.blocked.store(true, Ordering::SeqCst);
    let supervisor = Supervisor::new(
        store.clone(),
        executor.clone(),
        Limits {
            per_depth: 2,
            queue_per_depth: 4,
        },
    )
    .unwrap();
    for id in ["one", "two"] {
        supervisor.stage(&spec(id, 0)).unwrap();
        supervisor.activate(id).unwrap();
    }
    supervisor.start().unwrap();
    wait(|| executor.executed.lock().unwrap().len() == 2);
    executor.fail_publication.store(true, Ordering::SeqCst);
    assert!(supervisor.shutdown().is_err());
    supervisor.join(Duration::from_secs(3)).unwrap();
    assert!(store.active().unwrap().is_empty());
    executor.fail_publication.store(false, Ordering::SeqCst);
    supervisor.flush().unwrap();
}

#[test]
fn a_newer_schema_is_rejected_without_rewriting_it() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("runtime.sqlite");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.pragma_update(None, "user_version", 99).unwrap();
    drop(db);
    assert!(SqliteRunStore::open(&path).is_err());
    let db = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        99
    );
}

#[test]
fn turns_cannot_alias_another_sessions_receipt_scope() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(&root.path().join("journal.sqlite")).unwrap();
    let first = spec("first", 0);
    store.stage(&first, 4).unwrap();
    let mut other = spec("other", 0);
    other.turn = first.turn.clone();
    assert!(store.stage(&other, 4).is_err());
    store.cancel("first").unwrap();
    store
        .erase_session(&first.session, &[first.turn.clone()])
        .unwrap();
    assert!(store.stage(&other, 4).is_err());
}

#[test]
fn event_pages_are_ordered_resumable_and_run_scoped() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(&root.path().join("journal.sqlite")).unwrap();
    for id in ["one", "two"] {
        store.stage(&spec(id, 0), 4).unwrap();
        store.activate(id).unwrap();
    }
    let first = store.events("one", 0, 1).unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].record.spec.id, "one");
    let claimed = store.claim(0).unwrap().unwrap();
    store
        .finish(
            "one",
            claimed.lease.as_deref().unwrap(),
            Completion {
                state: RunState::Succeeded,
                result: Value::Null,
            },
        )
        .unwrap();
    let rest = store.events("one", first[0].sequence, 256).unwrap();
    assert!(rest
        .iter()
        .all(|e| e.sequence > first[0].sequence && e.record.spec.id == "one"));
    assert_eq!(rest.last().unwrap().record.state, RunState::Succeeded);
    assert!(store.events("one", u64::MAX, 1).is_err());
    assert!(store.events("one", 0, 257).is_err());
}

#[test]
fn wait_any_uses_owned_runtime_records_and_returns_first_terminal_child() {
    use kageha_contracts::execution::Control;
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteRunStore::open(&root.path().join("runtime.sqlite")).unwrap());
    let supervisor = Supervisor::new(
        store.clone(),
        Arc::new(Fixture::default()),
        Limits::default(),
    )
    .unwrap();
    let parent = spec("wait-parent", 0);
    supervisor.stage(&parent).unwrap();
    supervisor.activate(&parent.id).unwrap();
    store.claim(0).unwrap().unwrap();
    for id in ["slow-child", "fast-child"] {
        let mut child = spec(id, 1);
        child.parent = Some(parent.session.clone());
        child.parent_run = Some(parent.id.clone());
        supervisor.stage(&child).unwrap();
        supervisor.activate(id).unwrap();
    }
    let ids = vec!["slow-child".to_owned(), "fast-child".to_owned()];
    let flag = AtomicBool::new(false);
    let ctl = Control {
        cancelled: &flag,
        deadline: None,
    };
    assert!(supervisor
        .wait_children(&parent.session, &ids, &ctl, Duration::ZERO)
        .unwrap()
        .is_empty());
    assert!(supervisor
        .wait_children("another-session", &ids, &ctl, Duration::ZERO)
        .is_err());
    assert!(supervisor
        .wait_children(
            &parent.session,
            &[ids[0].clone(), ids[0].clone()],
            &ctl,
            Duration::ZERO
        )
        .is_err());
    assert!(supervisor
        .wait_children(&parent.session, &ids, &ctl, Duration::from_secs(31))
        .is_err());
    std::thread::scope(|s| {
        let waiter = s.spawn(|| {
            supervisor
                .wait_children(&parent.session, &ids, &ctl, Duration::from_secs(3))
                .unwrap()
        });
        supervisor.cancel("fast-child").unwrap();
        let done = waiter.join().unwrap();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].spec.id, "fast-child");
    });
    // A later parent turn can still inspect its earlier child without restarting it.
    assert_eq!(
        supervisor
            .wait_children(&parent.session, &ids, &ctl, Duration::ZERO)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store.get("slow-child").unwrap().unwrap().state,
        RunState::Queued
    );
    flag.store(true, Ordering::SeqCst);
    assert_eq!(
        supervisor
            .wait_children(&parent.session, &ids, &ctl, Duration::ZERO)
            .unwrap_err()
            .code,
        ErrorCode::Cancelled
    );
    flag.store(false, Ordering::SeqCst);
    let expired = Control {
        cancelled: &flag,
        deadline: Some(std::time::Instant::now()),
    };
    assert_eq!(
        supervisor
            .wait_children(&parent.session, &ids, &expired, Duration::from_secs(3))
            .unwrap_err()
            .code,
        ErrorCode::Deadline
    );
}

#[test]
fn wait_all_waits_for_every_owned_child_and_returns_partial_on_timeout() {
    use kageha_contracts::execution::Control;
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteRunStore::open(&root.path().join("runtime.sqlite")).unwrap());
    let supervisor = Supervisor::new(store.clone(), Arc::new(Fixture::default()), Limits::default()).unwrap();
    let parent = spec("all-parent", 0);
    supervisor.stage(&parent).unwrap();
    supervisor.activate(&parent.id).unwrap();
    store.claim(0).unwrap().unwrap();
    for id in ["all-first", "all-second"] {
        let mut child = spec(id, 1);
        child.parent = Some(parent.session.clone());
        child.parent_run = Some(parent.id.clone());
        supervisor.stage(&child).unwrap();
        supervisor.activate(id).unwrap();
    }
    let ids = vec!["all-first".to_owned(), "all-second".to_owned()];
    let cancelled = AtomicBool::new(false);
    let ctl = Control { cancelled: &cancelled, deadline: None };
    supervisor.cancel("all-first").unwrap();
    let partial = supervisor.wait_all_children(&parent.session, &ids, &ctl, Duration::from_millis(20)).unwrap();
    assert_eq!(partial.len(), 1);
    assert_eq!(partial[0].spec.id, "all-first");
    assert!(supervisor.wait_all_children("wrong-owner", &ids, &ctl, Duration::ZERO).is_err());
    assert!(supervisor.wait_all_children(&parent.session, &[ids[0].clone(), ids[0].clone()], &ctl, Duration::ZERO).is_err());
    assert!(supervisor.wait_all_children(&parent.session, &ids, &ctl, Duration::from_secs(31)).is_err());
    std::thread::scope(|scope| {
        let (send, recv) = std::sync::mpsc::channel();
        let supervisor_ref = &supervisor;
        let parent_ref = &parent.session;
        let ids_ref = &ids;
        let ctl_ref = &ctl;
        scope.spawn(move || { send.send(supervisor_ref.wait_all_children(parent_ref, ids_ref, ctl_ref, Duration::from_secs(3)).unwrap()).unwrap(); });
        assert!(recv.recv_timeout(Duration::from_millis(50)).is_err(), "the first terminal child must not end a wait for all");
        supervisor.cancel("all-second").unwrap();
        assert_eq!(recv.recv_timeout(Duration::from_secs(2)).unwrap().len(), 2);
    });
    cancelled.store(true, Ordering::SeqCst);
    assert_eq!(supervisor.wait_all_children(&parent.session, &ids, &ctl, Duration::ZERO).unwrap_err().code, ErrorCode::Cancelled);
}

#[test]
fn failed_or_interrupted_ancestor_prevents_queued_child_dispatch() {
    for state in [RunState::Failed, RunState::Interrupted] {
        let root = tempfile::tempdir().unwrap();
        let store = SqliteRunStore::open(&root.path().join("runs.sqlite")).unwrap();
        let parent = spec("parent", 0);
        store.stage(&parent, 8).unwrap();
        store.activate("parent").unwrap();
        let claimed = store.claim(0).unwrap().unwrap();
        let mut child = spec("child", 1);
        child.parent = Some(parent.session);
        child.parent_run = Some(parent.id);
        store.stage(&child, 8).unwrap();
        store.activate("child").unwrap();
        store
            .finish(
                "parent",
                claimed.lease.as_deref().unwrap(),
                Completion {
                    state,
                    result: Value::Null,
                },
            )
            .unwrap();
        assert!(
            store.claim(1).unwrap().is_none(),
            "failed ancestor must prevent dispatch"
        );
        assert_eq!(
            store.get("child").unwrap().unwrap().state,
            RunState::Cancelled
        );
    }
}

#[test]
fn failed_parent_cancels_already_running_children() {
    struct Failing {
        child_started: AtomicBool,
    }
    impl RunExecutor for Failing {
        fn execute(&self, run: &RunRecord, cancel: Arc<AtomicBool>) -> Result<Completion> {
            if run.spec.depth == 0 {
                let end = std::time::Instant::now() + Duration::from_secs(2);
                while !self.child_started.load(Ordering::SeqCst) && std::time::Instant::now() < end
                {
                    std::thread::sleep(Duration::from_millis(1));
                }
                return Err(ServiceError::new(ErrorCode::Unavailable, "parent failed"));
            }
            self.child_started.store(true, Ordering::SeqCst);
            while !cancel.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(Completion {
                state: RunState::Cancelled,
                result: Value::Null,
            })
        }
        fn inspect(&self, _: &RunRecord) -> Result<Option<Completion>> {
            Ok(None)
        }
        fn publish(&self, _: &RunRecord) -> Result<()> {
            Ok(())
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteRunStore::open(&dir.path().join("runs.sqlite")).unwrap());
    let supervisor = Supervisor::new(
        store.clone(),
        Arc::new(Failing {
            child_started: AtomicBool::new(false),
        }),
        Limits::default(),
    )
    .unwrap();
    let parent = spec("parent", 0);
    supervisor.stage(&parent).unwrap();
    supervisor.activate("parent").unwrap();
    let mut child = spec("child", 1);
    child.parent = Some(parent.session);
    child.parent_run = Some(parent.id);
    supervisor.stage(&child).unwrap();
    supervisor.activate("child").unwrap();
    supervisor.start().unwrap();
    wait(|| store.get("parent").unwrap().unwrap().state == RunState::Failed);
    let until = std::time::Instant::now() + Duration::from_millis(300);
    while std::time::Instant::now() < until
        && !store.get("child").unwrap().unwrap().state.terminal()
    {
        std::thread::sleep(Duration::from_millis(2));
    }
    let state = store.get("child").unwrap().unwrap().state;
    stop(&supervisor);
    assert_eq!(
        state,
        RunState::Cancelled,
        "running orphan must be cancelled"
    );
}

#[test]
fn completion_is_retried_after_transient_journal_write_failure_without_reexecution() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("runs.sqlite");
    let store = Arc::new(SqliteRunStore::open(&path).unwrap());
    let sql = rusqlite::Connection::open(&path).unwrap();
    sql.execute_batch("CREATE TRIGGER fail_completion BEFORE UPDATE ON runs WHEN NEW.state='succeeded' BEGIN SELECT RAISE(FAIL,'injected finish failure'); END;").unwrap();
    let executor = Arc::new(Fixture::default());
    let supervisor = Supervisor::new(store.clone(), executor.clone(), Limits::default()).unwrap();
    supervisor.stage(&spec("receipt", 0)).unwrap();
    supervisor.activate("receipt").unwrap();
    supervisor.start().unwrap();
    wait(|| !executor.executed.lock().unwrap().is_empty());
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(
        store.get("receipt").unwrap().unwrap().state,
        RunState::Running
    );
    sql.execute_batch("DROP TRIGGER fail_completion").unwrap();
    let until = std::time::Instant::now() + Duration::from_secs(1);
    while std::time::Instant::now() < until
        && !store.get("receipt").unwrap().unwrap().state.terminal()
    {
        std::thread::sleep(Duration::from_millis(2));
    }
    let state = store.get("receipt").unwrap().unwrap().state;
    stop(&supervisor);
    assert_eq!(
        state,
        RunState::Succeeded,
        "completion must be retained until the journal recovers"
    );
    assert_eq!(executor.executed.lock().unwrap().len(), 1);
}

#[test]
fn drained_supervisor_never_reports_restart_success() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteRunStore::open(&dir.path().join("runs.sqlite")).unwrap());
    let supervisor =
        Supervisor::new(store, Arc::new(Fixture::default()), Limits::default()).unwrap();
    supervisor.shutdown().unwrap();
    assert_eq!(supervisor.start().unwrap_err().code, ErrorCode::Unavailable);
}

#[test]
fn wait_any_returns_all_already_terminal_owned_children() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteRunStore::open(&dir.path().join("runs.sqlite")).unwrap());
    let supervisor = Supervisor::new(
        store.clone(),
        Arc::new(Fixture::default()),
        Limits::default(),
    )
    .unwrap();
    let parent = spec("parent", 0);
    supervisor.stage(&parent).unwrap();
    supervisor.activate("parent").unwrap();
    let mut ids = Vec::new();
    for id in ["a", "b"] {
        let mut child = spec(id, 1);
        child.parent = Some(parent.session.clone());
        child.parent_run = Some(parent.id.clone());
        supervisor.stage(&child).unwrap();
        supervisor.cancel(id).unwrap();
        ids.push(id.into());
    }
    let flag = AtomicBool::new(false);
    let completed = supervisor
        .wait_children(
            &parent.session,
            &ids,
            &kageha_contracts::execution::Control {
                cancelled: &flag,
                deadline: None,
            },
            Duration::ZERO,
        )
        .unwrap();
    assert_eq!(
        completed
            .iter()
            .map(|r| r.spec.id.as_str())
            .collect::<Vec<_>>(),
        vec!["a", "b"]
    );
}

#[test]
fn failed_parent_cancels_grandchild_through_completed_intermediate() {
    struct Failing {
        child_started: AtomicBool,
    }
    impl RunExecutor for Failing {
        fn execute(&self, run: &RunRecord, cancel: Arc<AtomicBool>) -> Result<Completion> {
            if run.spec.depth == 0 {
                let end = std::time::Instant::now() + Duration::from_secs(2);
                while !self.child_started.load(Ordering::SeqCst) && std::time::Instant::now() < end
                {
                    std::thread::sleep(Duration::from_millis(1));
                }
                return Err(ServiceError::new(ErrorCode::Unavailable, "parent failed"));
            }
            if run.spec.id == "middle" {
                return Ok(Completion {
                    state: RunState::Succeeded,
                    result: Value::Null,
                });
            }
            self.child_started.store(true, Ordering::SeqCst);
            while !cancel.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(Completion {
                state: RunState::Cancelled,
                result: Value::Null,
            })
        }
        fn inspect(&self, _: &RunRecord) -> Result<Option<Completion>> {
            Ok(None)
        }
        fn publish(&self, _: &RunRecord) -> Result<()> {
            Ok(())
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteRunStore::open(&dir.path().join("runs.sqlite")).unwrap());
    let supervisor = Supervisor::new(
        store.clone(),
        Arc::new(Failing {
            child_started: AtomicBool::new(false),
        }),
        Limits::default(),
    )
    .unwrap();
    let parent = spec("parent", 0);
    supervisor.stage(&parent).unwrap();
    supervisor.activate("parent").unwrap();
    let mut middle = spec("middle", 1);
    middle.parent = Some(parent.session.clone());
    middle.parent_run = Some(parent.id.clone());
    supervisor.stage(&middle).unwrap();
    supervisor.activate("middle").unwrap();
    let mut child = spec("child", 2);
    child.parent = Some(middle.session);
    child.parent_run = Some(middle.id);
    supervisor.stage(&child).unwrap();
    supervisor.activate("child").unwrap();
    supervisor.start().unwrap();
    wait(|| store.get("parent").unwrap().unwrap().state == RunState::Failed);
    let until = std::time::Instant::now() + Duration::from_millis(300);
    while std::time::Instant::now() < until
        && !store.get("child").unwrap().unwrap().state.terminal()
    {
        std::thread::sleep(Duration::from_millis(2));
    }
    let state = store.get("child").unwrap().unwrap().state;
    stop(&supervisor);
    assert_eq!(
        state,
        RunState::Cancelled,
        "running orphan must be cancelled"
    );
}

#[test]
fn finished_parent_cannot_be_erased_while_its_descendants_are_active() {
    let dir = tempfile::tempdir().unwrap();
    let store = SqliteRunStore::open(&dir.path().join("runs.sqlite")).unwrap();
    let parent = spec("parent", 0);
    store.stage(&parent, 8).unwrap();
    store.activate("parent").unwrap();
    let run = store.claim(0).unwrap().unwrap();
    let mut child = spec("child", 1);
    child.parent = Some(parent.session.clone());
    child.parent_run = Some(parent.id.clone());
    store.stage(&child, 8).unwrap();
    store.activate("child").unwrap();
    store
        .finish(
            "parent",
            run.lease.as_deref().unwrap(),
            Completion {
                state: RunState::Succeeded,
                result: Value::Null,
            },
        )
        .unwrap();
    assert!(
        store.erase_session(&parent.session, &[]).is_err(),
        "erasure must retain ancestry for active child"
    );
    assert_eq!(store.claim(1).unwrap().unwrap().spec.id, "child");
}

#[test]
fn orphaned_or_cyclic_queue_head_is_quarantined_without_blocking_other_work() {
    for missing in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteRunStore::open(&dir.path().join("runs.sqlite")).unwrap();
        let mut orphan = spec("orphan", 1);
        orphan.parent = Some("old-parent-session".into());
        orphan.parent_run = Some(if missing { "missing" } else { "orphan" }.into());
        store
            .import_run(&RunRecord {
                spec: orphan,
                state: RunState::Staged,
                revision: 1,
                lease: None,
                completion: None,
                cancel_requested: false,
            })
            .unwrap();
        store.activate("orphan").unwrap();
        store.stage(&spec("independent", 1), 8).unwrap();
        store.activate("independent").unwrap();
        assert_eq!(store.claim(1).unwrap().unwrap().spec.id, "independent");
        let orphan = store.get("orphan").unwrap().unwrap();
        assert_eq!(orphan.state, RunState::Cancelled);
        assert_eq!(orphan.completion.unwrap().result["not_dispatched"], true);
    }
}

#[test]
fn permanent_descendant_contract_error_fences_admission_instead_of_retrying_forever() {
    struct Fails;
    impl RunExecutor for Fails {
        fn execute(&self,_:&RunRecord,_:Arc<AtomicBool>)->Result<Completion>{Err(ServiceError::new(ErrorCode::Unavailable,"parent failed"))}
        fn inspect(&self,_:&RunRecord)->Result<Option<Completion>>{Ok(None)}
        fn publish(&self,_:&RunRecord)->Result<()>{Ok(())}
    }
    let dir=tempfile::tempdir().unwrap();let store=Arc::new(SqliteRunStore::open(&dir.path().join("runs.sqlite")).unwrap());let supervisor=Supervisor::new(store.clone(),Arc::new(Fails),Limits::default()).unwrap();
    let mut corrupt=spec("cyclic",1);corrupt.parent=Some("old-session".into());corrupt.parent_run=Some("cyclic".into());
    store.import_run(&RunRecord{spec:corrupt,state:RunState::Staged,revision:1,lease:None,completion:None,cancel_requested:false}).unwrap();
    supervisor.stage(&spec("parent",0)).unwrap();supervisor.activate("parent").unwrap();supervisor.start().unwrap();wait(||store.get("parent").unwrap().unwrap().state==RunState::Failed);
    let until=std::time::Instant::now()+Duration::from_millis(200);let mut fenced=false;
    while std::time::Instant::now()<until {if supervisor.stage(&spec("probe",0)).is_err(){fenced=true;break;}std::thread::sleep(Duration::from_millis(2));}
    stop(&supervisor);assert!(fenced,"permanent ancestry conflict must be surfaced as stopped admission");
}

#[test]
fn an_exclusive_owner_question_lends_capacity_until_its_run_can_resume() {
    use kageha_runtime::{execution::{Control, ToolCall, ToolPort, ToolReply}, fanout::execute_parallel};

    #[derive(Default)]
    struct Waiting {
        question_shown: AtomicBool,
        answered: AtomicBool,
        returning: AtomicBool,
        resumed: AtomicBool,
        next_started: AtomicBool,
        next_finished: AtomicBool,
    }
    impl ToolPort for Waiting {
        fn discover(&self) -> std::io::Result<Vec<Value>> {
            Ok(Vec::new())
        }
        fn execute(&self, _: &ToolCall, control: &Control<'_>) -> std::io::Result<ToolReply> {
            while_waiting(|| {
                self.question_shown.store(true, Ordering::SeqCst);
                while !self.answered.load(Ordering::SeqCst) {
                    control.check()?;
                    std::thread::sleep(Duration::from_millis(2));
                }
                self.returning.store(true, Ordering::SeqCst);
                Ok(())
            })?;
            self.resumed.store(true, Ordering::SeqCst);
            Ok(ToolReply::success("The original question was answered".into()))
        }
    }
    impl RunExecutor for Waiting {
        fn execute(&self, run: &RunRecord, cancel: Arc<AtomicBool>) -> Result<Completion> {
            if run.spec.id == "original" {
                let call = ToolCall {id: "question".into(), name: "ask_human".into(), arguments: json!({})};
                execute_parallel(self, &[call], &Control {cancelled: &cancel, deadline: None}, 4)
                    .map_err(|e| ServiceError::new(ErrorCode::Unavailable, e.to_string()))?;
            } else {
                self.next_started.store(true, Ordering::SeqCst);
                while !self.next_finished.load(Ordering::SeqCst) && !cancel.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
            Ok(Completion {state: RunState::Succeeded, result: Value::Null})
        }
        fn inspect(&self, _: &RunRecord) -> Result<Option<Completion>> { Ok(None) }
        fn publish(&self, _: &RunRecord) -> Result<()> { Ok(()) }
    }
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteRunStore::open(&root.path().join("runtime.sqlite")).unwrap());
    let executor = Arc::new(Waiting::default());
    let supervisor = Supervisor::new(
        store.clone(),
        executor.clone(),
        Limits {per_depth: 1, queue_per_depth: 8},
    ).unwrap();
    supervisor.stage(&spec("original", 0)).unwrap();
    supervisor.activate("original").unwrap();
    supervisor.start().unwrap();
    wait(|| executor.question_shown.load(Ordering::SeqCst));
    supervisor.stage(&spec("unrelated", 0)).unwrap();
    supervisor.activate("unrelated").unwrap();
    wait(|| executor.next_started.load(Ordering::SeqCst));
    assert_eq!(store.get("original").unwrap().unwrap().state, RunState::Running);
    executor.answered.store(true, Ordering::SeqCst);
    wait(|| executor.returning.load(Ordering::SeqCst));
    assert!(!executor.resumed.load(Ordering::SeqCst), "resume must wait for the one execution slot");
    executor.next_finished.store(true, Ordering::SeqCst);
    wait(|| store.get("original").unwrap().unwrap().state == RunState::Succeeded);
    assert!(executor.resumed.load(Ordering::SeqCst));
    assert_eq!(store.get("unrelated").unwrap().unwrap().state, RunState::Succeeded);
    stop(&supervisor);
}
