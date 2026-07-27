use super::*;
use crate::{JournalEntry, LearningPort, NoSecrets, OutcomeStatus, SecretValues};

#[test]
fn agent_lessons_require_local_evidence_and_are_scoped_and_deduplicated() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteLearning::open(&root.path().join("learning.sqlite")).unwrap();
    let stable = store.lesson_reference("turn-a", "fixed").unwrap();
    assert_ne!(stable, store.lesson_reference("worker-b", "fixed").unwrap());
    let reopened = SqliteLearning::open(&root.path().join("learning.sqlite")).unwrap();
    assert_eq!(stable, reopened.lesson_reference("turn-a", "fixed").unwrap());
    let event = |kind, body| JournalEntry { kind, body, at: 1 };
    store.journal("one", event(JournalKind::Tool, json!({"call_id":"failed","call_ref":store.lesson_reference("turn-a", "failed").unwrap(),"status":"failed","origin_turn":lesson_origin("turn-a")}))).unwrap();
    store.journal("one", event(JournalKind::Tool, json!({"call_id":"fixed","call_ref":store.lesson_reference("turn-a", "fixed").unwrap(),"status":"done","receipt_refs":[store.lesson_reference("turn-a", "receipt:xyz").unwrap()],"origin_turn":lesson_origin("turn-a")}))).unwrap();
    store.journal("one", event(JournalKind::Repair, json!({"failed_call_id":"failed","repair_call_id":"fixed","failed_ref":store.lesson_reference("turn-a", "failed").unwrap(),"repair_ref":store.lesson_reference("turn-a", "fixed").unwrap(),"origin_turn":lesson_origin("turn-a")}))).unwrap();
    let draft = LessonDraft { task_type:"python".into(), trigger:"A package is missing".into(), action:"Install the package and retry".into(), evidence:vec![] };
    assert!(store.save_agent_lesson("main", "other", "turn-a", &draft, &["fixed".into()], None, &NoSecrets).is_err());
    assert!(store.save_agent_lesson("main", "one", "worker-b", &draft, &["fixed".into()], None, &NoSecrets).is_err());
    assert!(store.save_agent_lesson("main", "one", "turn-a", &draft, &vec!["fixed".into(); MAX_EVIDENCE + 1], None, &NoSecrets).is_err());
    let mut oversized = draft.clone(); oversized.action = "x".repeat(MAX_LESSON_CHARS * 4 + 1);
    assert!(store.save_agent_lesson("main", "one", "turn-a", &oversized, &["fixed".into()], None, &NoSecrets).is_err());
    let refs = vec!["failed".into(), "fixed".into()];
    let first = store.save_agent_lesson("main", "one", "turn-a", &draft, &refs, None, &NoSecrets).unwrap();
    assert_eq!(first.status, LessonStatus::Active);
    let again = store.save_agent_lesson("main", "one", "turn-a", &draft, &["receipt:xyz".into()], None, &NoSecrets).unwrap();
    assert_eq!(first.id, again.id);
    assert!(again.merged);
    assert!(store.retire_agent_lesson("other", first.id, "outdated", &NoSecrets).is_err());
    assert!(store.save_agent_lesson("other", "one", "turn-a", &draft, &refs, Some(first.id), &NoSecrets).is_err());
    let secret = SecretValues::new(["very-secret-value".to_string()]);
    let mut unsafe_draft = draft.clone();
    unsafe_draft.action = "very-secret-value".into();
    assert!(store.save_agent_lesson("main", "one", "turn-a", &unsafe_draft, &refs, None, &secret).is_err());
    let mut corrected = draft.clone();
    corrected.action = "Check the environment and retry with the installed interpreter".into();
    let replacement = store.save_agent_lesson("main", "one", "turn-a", &corrected, &refs, Some(first.id), &NoSecrets).unwrap();
    assert_ne!(replacement.id, first.id);
    let retry = store.save_agent_lesson("main", "one", "turn-a", &corrected, &refs, Some(first.id), &NoSecrets).unwrap();
    assert_eq!(retry.id, replacement.id);
    assert_eq!(retry.status, LessonStatus::Active);
    assert_eq!(store.lessons("main").unwrap()[0].status, LessonStatus::Retired);
    assert!(store.retire_agent_lesson("other", replacement.id, "outdated", &NoSecrets).is_err());
    assert!(store.retire_agent_lesson("main", replacement.id, "very-secret-value", &secret).is_err());
    store.retire_agent_lesson("main", replacement.id, "outdated", &NoSecrets).unwrap();
    let retired = store.lessons("main").unwrap()[1].clone();
    store.retire_agent_lesson("main", replacement.id, "retry", &NoSecrets).unwrap();
    assert_eq!(store.lessons("main").unwrap()[1], retired);
    assert!(store.save_agent_lesson("main", "one", "turn-a", &corrected, &refs, Some(first.id), &NoSecrets).is_err());
    assert_eq!(store.lessons("main").unwrap()[1].status, LessonStatus::Retired);
    let private = store.save_agent_lesson("writer", "one", "turn-a", &draft, &refs, None, &NoSecrets).unwrap();
    assert!(store.context_with("writer", "missing package", None, &[]).unwrap().lesson_ids.contains(&private.id));
    assert!(!store.context_with("other", "missing package", None, &[]).unwrap().lesson_ids.contains(&private.id));
}

#[test]
fn explicit_evidence_scanning_has_a_hard_limit() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteLearning::open(&root.path().join("learning.sqlite")).unwrap();
    let origin = lesson_origin("turn-a");
    let call_ref = store.lesson_reference("turn-a", "late").unwrap();
    {
        let db = store.lock().unwrap();
        db.execute_batch("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<10000) INSERT INTO learn_events(request_id,seq,kind,body) SELECT 'long',x,'tool','{\"at\":1,\"body\":{}}' FROM n").unwrap();
        let body = json!({"at":1,"body":{"origin_turn":origin,"call_ref":call_ref}}).to_string();
        db.execute("INSERT INTO learn_events(request_id,seq,kind,body) VALUES('long',10001,'tool',?)", [body]).unwrap();
    }
    let draft = LessonDraft { task_type:"notes".into(),trigger:"Meeting notes".into(),action:"Use a concise style".into(),evidence:vec![] };
    let error = store.save_agent_lesson("main", "long", "turn-a", &draft, &["late".into()], None, &NoSecrets).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(store.lessons("main").unwrap().is_empty());
}

fn store() -> (tempfile::TempDir, SqliteLearning) {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteLearning::open(&root.path().join("learning.sqlite")).unwrap();
    (root, store)
}

fn summary(agent: &str, outcome: &str) -> RequestSummary {
    RequestSummary {
        agent: agent.into(),
        chat_id: "chat-1".into(),
        task_type: Some("pdf-reports".into()),
        status: "partial".into(),
        model_calls: 4,
        cost_usd: 0.0,
        cached_tokens: 0,
        outcomes: vec![OutcomeStatus {
            id: "report".into(),
            status: outcome.into(),
        }],
        started: 1_000,
        ended: 2_000,
    }
}

/// Journals `kinds` (seq 1, 2, ...) and closes the request, which queues reflection.
fn closed(
    store: &SqliteLearning,
    request: &str,
    agent: &str,
    outcome: &str,
    kinds: &[JournalKind],
) {
    for kind in kinds {
        let entry = JournalEntry {
            kind: *kind,
            body: json!({"tool":"shell"}),
            at: 1_500,
        };
        store.journal(request, entry).unwrap();
    }
    store
        .close_request(request, summary(agent, outcome))
        .unwrap();
}

fn draft(task_type: &str, trigger: &str, action: &str, evidence: &[u64]) -> LessonDraft {
    LessonDraft {
        task_type: task_type.into(),
        trigger: trigger.into(),
        action: action.into(),
        evidence: evidence.to_vec(),
    }
}

const OCR_TRIGGER: &str = "A PDF has no text layer";
const OCR_ACTION: &str = "Run OCR with ocrmypdf before extracting text";

#[test]
fn a_claim_leases_the_request_and_an_expired_lease_is_claimed_again() {
    let (_root, store) = store();
    closed(&store, "r1", "main", "verified", &[JournalKind::Tool]);
    let first = store.claim_reflection_at(10_000).unwrap().unwrap();
    assert_eq!((first.request.as_str(), first.attempt), ("r1", 1));
    let row = store.reflection_row("r1").unwrap().unwrap();
    assert_eq!(row.state, "running");
    assert_eq!(row.lease_until, Some(10_000 + REFLECTION_LEASE_MS));
    // A running row with a live lease is not claimed twice.
    assert!(store.claim_reflection_at(10_001).unwrap().is_none());

    // The worker crashed: once the lease expires another claim takes over.
    let second = store
        .claim_reflection_at(10_000 + REFLECTION_LEASE_MS)
        .unwrap()
        .unwrap();
    assert_eq!((second.request.as_str(), second.attempt), ("r1", 2));
    assert_ne!(first.lease, second.lease);
    // The first worker lost its lease and cannot finish or fail the row.
    let lessons = [draft("pdf-reports", OCR_TRIGGER, OCR_ACTION, &[1])];
    let error = store.store_lessons(&first, &lessons, None, &NoSecrets).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(!store.fail_reflection(&first, "late").unwrap());
    assert!(store.lessons("main").unwrap().is_empty());
    let report = store.store_lessons(&second, &lessons, None, &NoSecrets).unwrap();
    assert_eq!(report.stored.len(), 1);
    assert_eq!(store.reflection_row("r1").unwrap().unwrap().state, "done");
    assert!(store.claim_reflection_at(u64::MAX / 2).unwrap().is_none());
}

#[test]
fn a_reflection_that_keeps_expiring_is_failed_after_the_attempt_limit() {
    let (_root, store) = store();
    closed(&store, "r1", "main", "verified", &[JournalKind::Tool]);
    let mut now = 0;
    for attempt in 1..=MAX_REFLECTION_ATTEMPTS {
        let claim = store.claim_reflection_at(now).unwrap().unwrap();
        assert_eq!(claim.attempt, attempt);
        now += REFLECTION_LEASE_MS;
    }
    assert!(store.claim_reflection_at(now).unwrap().is_none());
    let row = store.reflection_row("r1").unwrap().unwrap();
    assert_eq!(row.state, "failed");
    assert!(row
        .reason
        .unwrap()
        .contains("did not finish after 3 attempts"));
    // Closing the request again (a resumed request) queues it with fresh attempts.
    store
        .close_request("r1", summary("main", "verified"))
        .unwrap();
    assert_eq!(store.claim_reflection_at(now).unwrap().unwrap().attempt, 1);
}

#[test]
fn a_failed_reflection_keeps_its_reason() {
    let (_root, store) = store();
    closed(&store, "r1", "main", "verified", &[JournalKind::Tool]);
    let claim = store.claim_reflection().unwrap().unwrap();
    assert!(store
        .fail_reflection(&claim, "planner unavailable:\n 503")
        .unwrap());
    let row = store.reflection_row("r1").unwrap().unwrap();
    assert_eq!(
        (row.state.as_str(), row.reason.as_deref(), row.lease_until),
        ("failed", Some("planner unavailable: 503"), None)
    );
    assert!(store.claim_reflection().unwrap().is_none());
}

#[test]
fn verified_runs_store_active_lessons_and_unverified_runs_store_candidates() {
    let (_root, store) = store();
    closed(
        &store,
        "verified",
        "main",
        "verified",
        &[JournalKind::Tool, JournalKind::Gate],
    );
    let claim = store.claim_reflection().unwrap().unwrap();
    let report = store
        .store_lessons(
            &claim,
            &[draft("pdf-reports", OCR_TRIGGER, OCR_ACTION, &[2, 1, 2])],
            None,
            &NoSecrets,
        )
        .unwrap();
    assert!(report.verified);
    assert_eq!(report.similarity, SimilarityMethod::Jaccard);
    assert_eq!(report.stored[0].status, LessonStatus::Active);
    let lesson = &store.lessons("main").unwrap()[0];
    assert_eq!(
        (
            lesson.status,
            lesson.support.clone(),
            lesson.evidence.clone()
        ),
        (
            LessonStatus::Active,
            vec!["verified".to_owned()],
            BTreeMap::from([("verified".to_owned(), vec![1, 2])])
        )
    );
    assert_eq!(lesson.task_type.as_deref(), Some("pdf-reports"));

    // Lessons are scoped to the request's agent.
    closed(
        &store,
        "unverified",
        "researcher",
        "failed",
        &[JournalKind::Failure],
    );
    let claim = store.claim_reflection().unwrap().unwrap();
    let report = store
        .store_lessons(
            &claim,
            &[draft(
                "web-research",
                "A site blocks the fetch",
                "Use the archived copy",
                &[1],
            )],
            None,
            &NoSecrets,
        )
        .unwrap();
    assert!(!report.verified);
    assert_eq!(report.stored[0].status, LessonStatus::Candidate);
    assert_eq!(store.lessons("researcher").unwrap().len(), 1);
    assert_eq!(store.lessons("main").unwrap().len(), 1);

    // A repair entry verifies the run without a verified outcome.
    closed(
        &store,
        "repaired",
        "main",
        "failed",
        &[JournalKind::Failure, JournalKind::Repair],
    );
    let claim = store.claim_reflection().unwrap().unwrap();
    let report = store
        .store_lessons(
            &claim,
            &[draft(
                "python-setup",
                "pdfplumber is missing",
                "Install it with uv pip install pdfplumber",
                &[1, 2],
            )],
            None,
            &NoSecrets,
        )
        .unwrap();
    assert_eq!(report.stored[0].status, LessonStatus::Active);
}

#[test]
fn invalid_lessons_are_dropped_and_only_five_are_kept() {
    let (_root, store) = store();
    closed(
        &store,
        "r1",
        "main",
        "verified",
        &[JournalKind::Tool, JournalKind::Failure],
    );
    let claim = store.claim_reflection().unwrap().unwrap();
    let long = "x".repeat(MAX_LESSON_CHARS);
    let mut drafts = vec![
        draft("pdf-reports", OCR_TRIGGER, OCR_ACTION, &[3]),
        draft("PDF Reports", OCR_TRIGGER, OCR_ACTION, &[1]),
        draft("pdf-reports", "", OCR_ACTION, &[1]),
        draft("pdf-reports", &long, "a", &[1]),
        draft("pdf-reports", OCR_TRIGGER, OCR_ACTION, &[]),
    ];
    drafts.extend((0..7).map(|n| {
        draft(
            &format!("type-{n}"),
            &format!("trigger {n}"),
            &format!("action {n}"),
            &[1, 2],
        )
    }));
    let report = store.store_lessons(&claim, &drafts, None, &NoSecrets).unwrap();
    let reasons: Vec<(usize, &str)> = report
        .dropped
        .iter()
        .map(|drop| (drop.index, drop.reason.as_str()))
        .collect();
    assert_eq!(
        reasons,
        [
            (0, "unknown_evidence:3"),
            (1, "invalid_task_type"),
            (2, "empty_text"),
            (3, "invalid_text"),
            (4, "invalid_evidence"),
            (10, "over_limit"),
            (11, "over_limit"),
        ]
    );
    assert_eq!(report.stored.len(), MAX_LESSONS);
    let stored = store.lessons("main").unwrap();
    assert_eq!(
        stored
            .iter()
            .map(|l| l.task_type.clone().unwrap())
            .collect::<Vec<_>>(),
        ["type-0", "type-1", "type-2", "type-3", "type-4"]
    );
    // Trigger and action together stay under the limit.
    let just_under = draft("pdf-reports", &"y".repeat(MAX_LESSON_CHARS - 2), "z", &[1]);
    assert!(validate_lesson(&just_under, &BTreeSet::from([1])).is_ok());
}

#[test]
fn a_similar_lesson_merges_and_a_candidate_becomes_active_with_two_requests() {
    let (_root, store) = store();
    closed(&store, "r1", "main", "failed", &[JournalKind::Failure]);
    let claim = store.claim_reflection().unwrap().unwrap();
    let first = store
        .store_lessons(
            &claim,
            &[draft("pdf-reports", OCR_TRIGGER, OCR_ACTION, &[1])],
            None,
            &NoSecrets,
        )
        .unwrap();
    assert_eq!(first.stored[0].status, LessonStatus::Candidate);

    // The same request reflected again (closed after a resume) adds no support.
    store
        .close_request("r1", summary("main", "failed"))
        .unwrap();
    let claim = store.claim_reflection().unwrap().unwrap();
    let again = store
        .store_lessons(
            &claim,
            &[draft("pdf-reports", OCR_TRIGGER, OCR_ACTION, &[1])],
            None,
            &NoSecrets,
        )
        .unwrap();
    assert_eq!(
        (again.stored[0].merged, again.stored[0].status),
        (true, LessonStatus::Candidate)
    );

    // A second unverified request with nearly the same wording activates it; a
    // different lesson and a different task type are added separately.
    closed(
        &store,
        "r2",
        "main",
        "failed",
        &[JournalKind::Failure, JournalKind::Tool],
    );
    let claim = store.claim_reflection().unwrap().unwrap();
    let report = store
        .store_lessons(
            &claim,
            &[
                draft(
                    "pdf-reports",
                    "a PDF has no text layer.",
                    "Run OCR with ocrmypdf, before extracting text",
                    &[2],
                ),
                draft(
                    "pdf-reports",
                    "The PDF is encrypted",
                    "Ask the user for the password",
                    &[1],
                ),
                draft("other-type", OCR_TRIGGER, OCR_ACTION, &[1]),
            ],
            None,
            &NoSecrets,
        )
        .unwrap();
    assert_eq!(
        report
            .stored
            .iter()
            .map(|s| (s.id, s.merged, s.status))
            .collect::<Vec<_>>(),
        [
            (first.stored[0].id, true, LessonStatus::Active),
            (first.stored[0].id + 1, false, LessonStatus::Candidate),
            (first.stored[0].id + 2, false, LessonStatus::Candidate),
        ]
    );
    let merged = &store.lessons("main").unwrap()[0];
    assert_eq!(merged.support, ["r1", "r2"]);
    assert_eq!(merged.trigger, OCR_TRIGGER);
    assert_eq!(
        merged.evidence,
        BTreeMap::from([("r1".to_owned(), vec![1]), ("r2".to_owned(), vec![2])])
    );
    // Another agent's similar lesson is its own.
    closed(
        &store,
        "r3",
        "researcher",
        "failed",
        &[JournalKind::Failure],
    );
    let claim = store.claim_reflection().unwrap().unwrap();
    let report = store
        .store_lessons(
            &claim,
            &[draft("pdf-reports", OCR_TRIGGER, OCR_ACTION, &[1])],
            None,
            &NoSecrets,
        )
        .unwrap();
    assert!(!report.stored[0].merged);
}

#[test]
fn duplicates_in_one_reply_merge_and_retired_lessons_stay_retired() {
    let (_root, store) = store();
    closed(&store, "r1", "main", "verified", &[JournalKind::Tool]);
    let claim = store.claim_reflection().unwrap().unwrap();
    let report = store
        .store_lessons(
            &claim,
            &[
                draft("pdf-reports", OCR_TRIGGER, OCR_ACTION, &[1]),
                draft("pdf-reports", OCR_TRIGGER, OCR_ACTION, &[1]),
            ],
            None,
            &NoSecrets,
        )
        .unwrap();
    assert_eq!(report.stored[0].id, report.stored[1].id);
    assert!(report.stored[1].merged);
    assert_eq!(store.lessons("main").unwrap().len(), 1);

    store
        .lock()
        .unwrap()
        .execute("UPDATE learn_lessons SET status='retired',retired_at=5", [])
        .unwrap();
    closed(&store, "r2", "main", "verified", &[JournalKind::Tool]);
    let claim = store.claim_reflection().unwrap().unwrap();
    let report = store
        .store_lessons(
            &claim,
            &[draft("pdf-reports", OCR_TRIGGER, OCR_ACTION, &[1])],
            None,
            &NoSecrets,
        )
        .unwrap();
    assert_eq!(report.stored[0].status, LessonStatus::Retired);
    assert_eq!(store.lessons("main").unwrap()[0].support, ["r1", "r2"]);
}

/// Embeds a text as a fixed vector by its first word.
struct FirstWord;
impl TextEmbedder for FirstWord {
    fn embed(&self, texts: &[&str]) -> io::Result<Vec<Vec<f64>>> {
        Ok(texts
            .iter()
            .map(|text| match text.split_whitespace().next() {
                Some("Scanned") | Some("A") => vec![1.0, 0.05],
                _ => vec![0.0, 1.0],
            })
            .collect())
    }
}
struct Broken;
impl TextEmbedder for Broken {
    fn embed(&self, _: &[&str]) -> io::Result<Vec<Vec<f64>>> {
        Err(io::Error::other("embedding route down"))
    }
}

#[test]
fn embeddings_merge_by_cosine_and_a_failed_embedding_falls_back_to_jaccard() {
    let (_root, store) = store();
    closed(&store, "r1", "main", "failed", &[JournalKind::Failure]);
    let claim = store.claim_reflection().unwrap().unwrap();
    store
        .store_lessons(
            &claim,
            &[draft("pdf-reports", OCR_TRIGGER, OCR_ACTION, &[1])],
            Some(&FirstWord),
            &NoSecrets,
        )
        .unwrap();
    // Different words, close embedding: merged.
    closed(&store, "r2", "main", "failed", &[JournalKind::Failure]);
    let claim = store.claim_reflection().unwrap().unwrap();
    let report = store
        .store_lessons(
            &claim,
            &[draft(
                "pdf-reports",
                "Scanned documents",
                "OCR them first",
                &[1],
            )],
            Some(&FirstWord),
            &NoSecrets,
        )
        .unwrap();
    assert_eq!(report.similarity, SimilarityMethod::Cosine);
    assert_eq!(
        (report.stored[0].merged, report.stored[0].status),
        (true, LessonStatus::Active)
    );
    // The embedding route fails: Jaccard decides, and these texts differ.
    closed(&store, "r3", "main", "failed", &[JournalKind::Failure]);
    let claim = store.claim_reflection().unwrap().unwrap();
    let report = store
        .store_lessons(
            &claim,
            &[draft(
                "pdf-reports",
                "Scanned documents",
                "OCR them first",
                &[1],
            )],
            Some(&Broken),
            &NoSecrets,
        )
        .unwrap();
    assert_eq!(report.similarity, SimilarityMethod::Jaccard);
    assert!(!report.stored[0].merged);
}

#[test]
fn similarity_measures() {
    assert_eq!(jaccard("Run OCR first", "run ocr, FIRST!"), 1.0);
    assert_eq!(jaccard("", ""), 0.0);
    assert!((jaccard("a b c d", "a b c e") - 0.6).abs() < 1e-9);
    assert!((cosine(&[1.0, 0.0], &[2.0, 0.0]) - 1.0).abs() < 1e-9);
    assert_eq!(cosine(&[1.0], &[1.0, 0.0]), 0.0);
    assert_eq!(
        sanitize_lesson(draft("a", "b", "c", &[1]), &[]),
        Ok(draft("a", "b", "c", &[1]))
    );
}

/// A source whose vault cannot be read.
struct Locked;
impl SecretSource for Locked {
    fn values(&self) -> io::Result<Vec<Zeroizing<String>>> {
        Err(io::Error::new(io::ErrorKind::PermissionDenied, "vault locked"))
    }
}

#[test]
fn lessons_holding_a_vault_value_or_a_secret_pattern_are_dropped() {
    const VALUE: &str = "correct-horse-battery";
    let (_root, store) = store();
    closed(&store, "r1", "main", "verified", &[JournalKind::Tool]);
    let claim = store.claim_reflection().unwrap().unwrap();
    let drafts = [
        draft("portal-login", "The portal asks for a login", &format!("Sign in with {VALUE}"), &[1]),
        draft("api-calls", "The API returns 401", "Send password=hunter2 in the form", &[1]),
        draft("pdf-reports", OCR_TRIGGER, OCR_ACTION, &[1]),
    ];
    let report = store
        .store_lessons(&claim, &drafts, None, &SecretValues::new([VALUE]))
        .unwrap();
    let reasons: Vec<(usize, &str)> = report.dropped.iter().map(|d| (d.index, d.reason.as_str())).collect();
    assert_eq!(reasons, [(0, "secret:vault_value"), (1, "secret:password")]);
    assert_eq!(report.stored.len(), 1);
    let lessons = store.lessons("main").unwrap();
    assert_eq!(lessons.len(), 1);
    assert_eq!(lessons[0].action, OCR_ACTION);
    // The reasons and the stored rows never hold the value.
    assert!(!format!("{report:?}{lessons:?}").contains(VALUE));
}

#[test]
fn an_unreadable_vault_stores_no_lessons() {
    let (_root, store) = store();
    closed(&store, "r1", "main", "verified", &[JournalKind::Tool]);
    let claim = store.claim_reflection().unwrap().unwrap();
    let error = store
        .store_lessons(&claim, &[draft("pdf-reports", OCR_TRIGGER, OCR_ACTION, &[1])], None, &Locked)
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert!(store.lessons("main").unwrap().is_empty());
    assert_eq!(store.reflection_row("r1").unwrap().unwrap().state, "running");
}

#[test]
fn a_store_without_the_lease_columns_is_migrated() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("learning.sqlite");
    drop(SqliteLearning::open(&path).unwrap());
    {
        let db = Connection::open(&path).unwrap();
        db.execute_batch(
            "DROP TABLE learn_reflection_queue;
             CREATE TABLE learn_reflection_queue(request_id TEXT PRIMARY KEY,queued_at INTEGER NOT NULL,state TEXT NOT NULL);
             INSERT INTO learn_reflection_queue VALUES('old',1,'queued');",
        )
        .unwrap();
    }
    let store = SqliteLearning::open(&path).unwrap();
    let row = store.reflection_row("old").unwrap().unwrap();
    assert_eq!((row.state.as_str(), row.attempts), ("queued", 0));
    assert_eq!(store.claim_reflection().unwrap().unwrap().attempt, 1);
}
