use super::*;
use crate::audited_session_tests::{Fixture, CHAT};
use crate::model_router::TestModels;
use kageha_learning::{lessons::REFLECTION_LEASE_MS, JournalEntry, LearningPort, LessonStatus, OutcomeStatus};
use kageha_runtime::test_support::{text_reply, ScriptedModel};

struct Setup {
    fixture: Fixture,
    _directory: tempfile::TempDir,
    store: Arc<SqliteLearning>,
}

fn setup() -> Setup {
    let fixture = Fixture::new();
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteLearning::open(&directory.path().join("learning.sqlite")).unwrap());
    Setup { fixture, _directory: directory, store }
}

/// Makes `replies` the planner's replies, through the `RoleProviders` test seam.
fn planner(setup: &Setup, replies: &[&str]) -> Arc<ScriptedModel> {
    let model = Arc::new(ScriptedModel::new(replies.iter().map(|text| text_reply(*text))));
    let roles = RoleProviders { fakes: TestModels { planner: Some(model.clone()), reviewer: None }, ..Default::default() };
    *setup.fixture.db.reflection.test_roles.lock().unwrap() = Some(roles);
    model
}

/// Journals entries of `kinds` (seq 1, 2, ...) and closes the request with `outcome`.
fn closed(store: &SqliteLearning, request: &str, outcome: &str, kinds: &[JournalKind]) {
    for (n, kind) in kinds.iter().enumerate() {
        let body = match kind {
            JournalKind::Failure => json!({"call_id":format!("c{n}"),"tool":"shell","operation":"shell python3","error_code":"nonzero_exit","exit_code":1,"excerpt":"ModuleNotFoundError: No module named 'pdfplumber'"}),
            JournalKind::Repair => json!({"tool":"shell","operation":"shell python3","failed_call_id":"c0","repair_call_id":format!("c{n}"),"error_code":"nonzero_exit","between":["c1"],"between_count":1}),
            _ => json!({"call_id":format!("c{n}"),"tool":"shell","status":"done","effect":"workspace","excerpt":"ok"}),
        };
        store.journal(request, JournalEntry { kind: *kind, body, at: 1_000 + n as u64 }).unwrap();
    }
    store.close_request(request, RequestSummary {
        agent: "main".into(), chat_id: CHAT.into(), task_type: Some("pdf-reports".into()), status: "partial".into(),
        model_calls: 6, cost_usd: 0.0, cached_tokens: 0,
        outcomes: vec![OutcomeStatus { id: "report".into(), status: outcome.into() }], started: 1_000, ended: 61_000,
    }).unwrap();
}

fn lesson(task_type: &str, trigger: &str, action: &str, evidence: &[u64]) -> Value {
    json!({"task_type":task_type,"trigger":trigger,"action":action,"evidence":evidence})
}

fn reply(lessons: &[Value]) -> String {
    json!({"lessons":lessons}).to_string()
}

fn state(store: &SqliteLearning, request: &str) -> (String, Option<String>) {
    let row = store.reflection_row(request).unwrap().unwrap();
    (row.state, row.reason)
}

const TRIGGER: &str = "A Python script fails with ModuleNotFoundError";
const ACTION: &str = "Install the missing package with uv pip install, then rerun the script";

#[test]
fn routine_answers_and_failures_before_tool_work_skip_reflection() {
    let setup = setup();
    setup.store.close_request("routine", RequestSummary {
        agent: "main".into(), chat_id: CHAT.into(), task_type: None, status: "verified".into(),
        model_calls: 1, cost_usd: 0.0, cached_tokens: 0, outcomes: vec![], started: 1, ended: 2,
    }).unwrap();
    setup.store.close_request("transport-failure", RequestSummary {
        agent: "main".into(), chat_id: CHAT.into(), task_type: None, status: "failed".into(),
        model_calls: 4, cost_usd: 0.0, cached_tokens: 0, outcomes: vec![], started: 1, ended: 2,
    }).unwrap();
    let model = planner(&setup, &[]);
    assert_eq!(reflect_pending(&setup.fixture.db, &setup.store, &VaultSecrets::new(&setup.fixture.db)), 2);
    assert!(model.requests().is_empty());
    assert!(setup.fixture.events("learning_usage").is_empty());
    assert_eq!(state(&setup.store, "routine"), ("done".into(), None));
}

#[test]
fn a_verified_run_stores_active_lessons_with_valid_evidence() {
    let setup = setup();
    closed(&setup.store, "r1", "verified", &[JournalKind::Failure, JournalKind::Tool, JournalKind::Tool, JournalKind::Repair]);
    let text = format!("```json\n{}\n```", reply(&[
        lesson("pdf-reports", TRIGGER, ACTION, &[1, 4]),
        // Evidence that is not in the journal drops the lesson.
        lesson("pdf-reports", "The report needs charts", "Use matplotlib", &[1, 99]),
    ]));
    let model = planner(&setup, &[&text]);
    assert_eq!(reflect_pending(&setup.fixture.db, &setup.store, &VaultSecrets::new(&setup.fixture.db)), 1);
    assert_eq!(state(&setup.store, "r1"), ("done".into(), None));
    let lessons = setup.store.lessons("main").unwrap();
    assert_eq!(lessons.len(), 1);
    let stored = &lessons[0];
    assert_eq!((stored.status, stored.trigger.as_str(), stored.action.as_str()), (LessonStatus::Active, TRIGGER, ACTION));
    assert_eq!(stored.support, ["r1"]);
    assert_eq!(stored.evidence, BTreeMap::from([("r1".to_owned(), vec![1, 4])]));

    // One planner call with the prompt and the compact journal, no tools.
    let usage = setup.fixture.events("learning_usage");
    assert_eq!(usage.len(), 1);
    assert_eq!(usage[0]["payload"]["model_calls"], 1);
    assert_eq!(usage[0]["payload"]["purpose"], "reflection");
    assert!(usage[0]["payload"]["cost_usd"].is_null());
    let requests = model.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!((request.role, request.tools.len(), request.messages[0]["content"].as_str()), (Role::Planner, 0, Some(REFLECTION_PROMPT)));
    let content = request.messages[1]["content"].as_str().unwrap();
    assert!(content.len() <= JOURNAL_BYTES);
    let journal: Value = serde_json::from_str(content).unwrap();
    assert_eq!((&journal["final_status"], &journal["task_type"], &journal["duration_seconds"]), (&json!("partial"), &json!("pdf-reports"), &json!(60)));
    assert_eq!(journal["outcomes"], json!([{"id":"report","status":"verified"}]));
    let seqs: Vec<u64> = journal["entries"].as_array().unwrap().iter().map(|entry| entry["seq"].as_u64().unwrap()).collect();
    assert_eq!(seqs, [1, 2, 3, 4]);
    assert_eq!(journal["entries"][0]["error_code"], "nonzero_exit");
    assert_eq!(journal["entry_counts"]["tool"], 2);
    assert_eq!(journal["tool_calls"]["shell"], json!({"done":2,"failed":0}));
}

#[test]
fn an_unverified_run_stores_candidates_and_a_second_request_activates_a_similar_lesson() {
    let setup = setup();
    closed(&setup.store, "r1", "failed", &[JournalKind::Failure]);
    planner(&setup, &[&reply(&[lesson("pdf-reports", TRIGGER, ACTION, &[1])]),
        &reply(&[lesson("pdf-reports", "a python script fails with ModuleNotFoundError.", "Install the missing package with uv pip install then rerun the script", &[2])])]);
    reflect_pending(&setup.fixture.db, &setup.store, &VaultSecrets::new(&setup.fixture.db));
    assert_eq!(setup.store.lessons("main").unwrap()[0].status, LessonStatus::Candidate);
    closed(&setup.store, "r2", "failed", &[JournalKind::Tool, JournalKind::Failure]);
    reflect_pending(&setup.fixture.db, &setup.store, &VaultSecrets::new(&setup.fixture.db));
    let lessons = setup.store.lessons("main").unwrap();
    assert_eq!(lessons.len(), 1, "{lessons:#?}");
    assert_eq!((lessons[0].status, lessons[0].support.clone()), (LessonStatus::Active, vec!["r1".to_owned(), "r2".to_owned()]));
}

#[test]
fn more_than_five_lessons_are_truncated() {
    let setup = setup();
    closed(&setup.store, "r1", "verified", &[JournalKind::Tool]);
    let many: Vec<Value> = (0..7).map(|n| lesson(&format!("type-{n}"), &format!("trigger {n}"), &format!("action {n}"), &[1])).collect();
    planner(&setup, &[&reply(&many)]);
    reflect_pending(&setup.fixture.db, &setup.store, &VaultSecrets::new(&setup.fixture.db));
    let kept: Vec<String> = setup.store.lessons("main").unwrap().into_iter().map(|lesson| lesson.task_type.unwrap()).collect();
    assert_eq!(kept, ["type-0", "type-1", "type-2", "type-3", "type-4"]);
}

#[test]
fn a_lesson_holding_a_vault_value_is_dropped_and_an_unreadable_vault_stores_none() {
    struct Locked;
    impl SecretSource for Locked {
        fn values(&self) -> io::Result<Vec<zeroize::Zeroizing<String>>> {
            Err(io::Error::other("The credential vault could not be read: locked"))
        }
    }
    const VALUE: &str = "portal-passphrase-41";
    let setup = setup();
    closed(&setup.store, "r1", "verified", &[JournalKind::Tool]);
    planner(&setup, &[&reply(&[
        lesson("portal-login", "The portal asks for a login", &format!("Sign in with {VALUE}"), &[1]),
        lesson("pdf-reports", TRIGGER, ACTION, &[1]),
    ])]);
    reflect_pending(&setup.fixture.db, &setup.store, &kageha_learning::SecretValues::new([VALUE]));
    assert_eq!(state(&setup.store, "r1"), ("done".into(), None));
    let lessons = setup.store.lessons("main").unwrap();
    assert_eq!(lessons.len(), 1);
    assert_eq!(lessons[0].action, ACTION);
    assert!(!format!("{lessons:?}").contains(VALUE));

    // Without the vault nothing is stored, and the failure reason holds no value.
    closed(&setup.store, "r2", "verified", &[JournalKind::Tool]);
    planner(&setup, &[&reply(&[lesson("other-task", "A different trigger here", "A different action here", &[1])])]);
    reflect_pending(&setup.fixture.db, &setup.store, &Locked);
    let (state_name, reason) = state(&setup.store, "r2");
    assert_eq!(state_name, "failed");
    assert!(reason.as_deref().is_some_and(|reason| reason.contains("secret scan could not read the credential vault")), "{reason:?}");
    assert_eq!(setup.store.lessons("main").unwrap().len(), 1);
}

#[test]
fn a_model_error_or_unparseable_reply_marks_the_queue_failed() {
    let setup = setup();
    closed(&setup.store, "r1", "verified", &[JournalKind::Tool]);
    // No scripted reply: the model call fails.
    planner(&setup, &[]);
    assert_eq!(reflect_pending(&setup.fixture.db, &setup.store, &VaultSecrets::new(&setup.fixture.db)), 1);
    let (state_name, reason) = state(&setup.store, "r1");
    assert_eq!(state_name, "failed");
    assert!(reason.as_deref().is_some_and(|reason| reason.contains("planner model test-planner failed")), "{reason:?}");
    closed(&setup.store, "r2", "verified", &[JournalKind::Tool]);
    planner(&setup, &["I learned nothing useful."]);
    reflect_pending(&setup.fixture.db, &setup.store, &VaultSecrets::new(&setup.fixture.db));
    assert_eq!(state(&setup.store, "r2"), ("failed".into(), Some("The planner reply was not a JSON object".into())));
    assert!(setup.store.lessons("main").unwrap().is_empty());
    // An empty list is a finished reflection.
    closed(&setup.store, "r3", "verified", &[JournalKind::Tool]);
    planner(&setup, &["{\"lessons\": []}"]);
    reflect_pending(&setup.fixture.db, &setup.store, &VaultSecrets::new(&setup.fixture.db));
    assert_eq!(state(&setup.store, "r3").0, "done");
}

#[test]
fn a_stuck_running_row_is_claimed_again_after_its_lease() {
    let setup = setup();
    closed(&setup.store, "r1", "verified", &[JournalKind::Tool]);
    // A worker claimed the request long ago and died without finishing.
    let stuck = setup.store.claim_reflection_at(1_000).unwrap().unwrap();
    assert_eq!(setup.store.reflection_row("r1").unwrap().unwrap().lease_until, Some(1_000 + REFLECTION_LEASE_MS));
    planner(&setup, &[&reply(&[lesson("pdf-reports", TRIGGER, ACTION, &[1])])]);
    assert_eq!(reflect_pending(&setup.fixture.db, &setup.store, &VaultSecrets::new(&setup.fixture.db)), 1);
    let row = setup.store.reflection_row("r1").unwrap().unwrap();
    assert_eq!((row.state.as_str(), row.attempts), ("done", 2));
    assert_eq!(setup.store.lessons("main").unwrap().len(), 1);
    // The dead worker's late result is refused and changes nothing.
    assert!(!setup.store.fail_reflection(&stuck, "late").unwrap());
    assert_eq!(state(&setup.store, "r1").0, "done");
}

#[test]
fn closing_a_request_wakes_the_background_worker() {
    let setup = setup();
    let db = &setup.fixture.db;
    let directory = tempfile::tempdir().unwrap();
    crate::ui_learning::install(db, &directory.path().join("learning.sqlite")).unwrap();
    let store = crate::ui_learning::store(db).unwrap();
    planner(&setup, &[&reply(&[lesson("pdf-reports", TRIGGER, ACTION, &[1])])]);
    install(db, store.clone()).unwrap();
    crate::ui_learning::observe(db, &setup.fixture.turn, &json!({"kind":"tools","calls":[
        {"id":"read","name":"read_file","arguments":{"path":"workspace/missing.txt"}}]}));
    crate::ui_learning::observe(db, &setup.fixture.turn, &json!({"kind":"tool_results","results":[
        {"tool_call_id":"read","is_error":true,"content":"File not found"}]}));
    crate::ui_learning::finish_turn(db, CHAT, &setup.fixture.turn, 0, "main", "partial", 0);
    let key = crate::ui_task_completion::scope(db, &setup.fixture.turn).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while store.reflection_row(&key).unwrap().is_none_or(|row| row.state != "done") {
        assert!(Instant::now() < deadline, "reflection did not finish: {:?}", store.reflection_row(&key).unwrap());
        std::thread::sleep(Duration::from_millis(20));
    }
    let lessons = store.lessons("main").unwrap();
    assert_eq!(lessons.len(), 1);
    assert_eq!(lessons[0].status, LessonStatus::Candidate);
}

#[test]
fn the_compact_journal_stays_under_its_limit_and_keeps_repairs_first() {
    let summary = RequestSummary {
        agent: "main".into(), chat_id: CHAT.into(), task_type: None, status: "verified".into(), model_calls: 900,
        cost_usd: 0.0, cached_tokens: 0, outcomes: Vec::new(), started: 0, ended: 1,
    };
    let big = "x".repeat(4000);
    let mut events = (1..=3000u64).map(|seq| {
        let kind = match seq % 100 { 0 => JournalKind::Repair, 1..=9 => JournalKind::Failure, _ => JournalKind::Tool };
        Ok(StoredEvent { seq, kind, body: json!({"tool":"shell","status":"done","excerpt":big,"operation":big}), at: seq })
    });
    let journal = compact_journal(&summary, &mut events).unwrap();
    assert!(journal.to_string().len() <= JOURNAL_BYTES);
    let entries = journal["entries"].as_array().unwrap();
    let kinds = |kind: &str| entries.iter().filter(|entry| entry["kind"] == kind).count();
    // 30 repairs exist and all fit; the summaries cut each long text.
    assert_eq!(kinds("repair"), 30);
    assert!(entries.windows(2).all(|pair| pair[0]["seq"].as_u64() < pair[1]["seq"].as_u64()));
    assert_eq!(journal["omitted_entries"].as_u64().unwrap(), 3000 - entries.len() as u64);
    assert_eq!(journal["entry_counts"]["tool"], 2700);
    assert!(entries.iter().all(|entry| entry["operation"].as_str().is_none_or(|text| text.chars().count() <= TEXT_CHARS + 1)));
}

#[test]
fn reflection_uses_the_planner_chain_else_the_executor_chain() {
    let provider = |model: &str| Provider { url: "https://models.example/v1".into(), key: String::new(), model: model.into(),
        protocol: "openai_compat".into(), source: None, context_window: None };
    let without = model_for(Ok(vec![provider("executor-model")]), &RoleProviders::default()).unwrap();
    assert_eq!((without.role, without.name.as_str()), ("executor", "executor-model"));
    let roles = RoleProviders { planner: vec![provider("planner-model")], ..Default::default() };
    let with = model_for(Ok(vec![provider("executor-model")]), &roles).unwrap();
    assert_eq!((with.role, with.name.as_str()), ("planner", "planner-model"));
    let none = model_for(Err((400, "Configure a ready model".into())), &RoleProviders::default()).err().unwrap();
    assert!(none.to_string().contains("Configure a ready model"));
}

#[test]
fn lessons_parse_leniently_and_bad_fields_are_dropped_by_validation() {
    let drafts = parse_lessons("Here: {\"lessons\":[{\"task_type\":\"a\",\"trigger\":\"t\",\"action\":\"x\",\"evidence\":[\"3\",2,\"n\"]},{\"trigger\":7}]}").unwrap();
    assert_eq!(drafts[0], LessonDraft { task_type: "a".into(), trigger: "t".into(), action: "x".into(), evidence: vec![3, 2, 0] });
    assert_eq!(drafts[1], LessonDraft::default());
    assert!(parse_lessons("{\"lesson\":[]}").is_err());
}
