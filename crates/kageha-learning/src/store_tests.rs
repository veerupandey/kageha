use super::*;
use std::{collections::BTreeSet, os::unix::fs::symlink, sync::Arc};

fn entry(kind: JournalKind, body: Value) -> JournalEntry {
    JournalEntry {
        kind,
        body,
        at: 1_700_000_000_000,
    }
}

fn summary(status: &str) -> RequestSummary {
    RequestSummary {
        agent: "main".into(),
        chat_id: "chat-1".into(),
        task_type: Some("pdf-reports".into()),
        status: status.into(),
        model_calls: 7,
        cost_usd: 0.25,
        cached_tokens: 1200,
        outcomes: vec![OutcomeStatus {
            id: "report".into(),
            status: "verified".into(),
        }],
        started: 1_000,
        ended: 9_000,
    }
}

#[test]
fn open_creates_private_directory_and_file() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("home/learning.sqlite");
    SqliteLearning::open(&path).unwrap();
    let dir = fs::metadata(path.parent().unwrap())
        .unwrap()
        .permissions()
        .mode();
    let file = fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(dir & 0o777, 0o700);
    assert_eq!(file & 0o777, 0o600);

    // An existing loose file is tightened; a symlinked file is refused.
    let loose = root.path().join("home/loose.sqlite");
    fs::write(&loose, b"").unwrap();
    fs::set_permissions(&loose, fs::Permissions::from_mode(0o644)).unwrap();
    SqliteLearning::open(&loose).unwrap();
    assert_eq!(
        fs::metadata(&loose).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let link = root.path().join("home/link.sqlite");
    symlink(&path, &link).unwrap();
    assert!(SqliteLearning::open(&link).is_err());
}

#[test]
fn open_rejects_shared_directory_and_unknown_schema_version() {
    let root = tempfile::tempdir().unwrap();
    let shared = root.path().join("shared");
    fs::create_dir(&shared).unwrap();
    fs::set_permissions(&shared, fs::Permissions::from_mode(0o777)).unwrap();
    let err = SqliteLearning::open(&shared.join("learning.sqlite"))
        .err()
        .unwrap();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);

    let path = root.path().join("home/learning.sqlite");
    SqliteLearning::open(&path).unwrap();
    let db = Connection::open(&path).unwrap();
    let version: i64 = db
        .query_row("SELECT version FROM learn_schema", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    let tables: BTreeSet<String> = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name LIKE 'learn_%'")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    for table in [
        "learn_schema",
        "learn_runs",
        "learn_events",
        "learn_failures",
        "learn_lessons",
        "learn_lesson_uses",
        "learn_skill_uses",
        "learn_reflection_queue",
        "learn_event_drops",
    ] {
        assert!(tables.contains(table), "{table}");
    }
    db.execute("UPDATE learn_schema SET version=2", []).unwrap();
    drop(db);
    let err = SqliteLearning::open(&path).err().unwrap();
    assert_eq!(err.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn concurrent_writers_get_increasing_gapless_seq_per_request() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("home/learning.sqlite");
    let shared = Arc::new(SqliteLearning::open(&path).unwrap());
    let mut threads = Vec::new();
    for t in 0..8u64 {
        // Half the writers share one connection, half open their own, so seq
        // assignment is exercised both in-process and across connections.
        let store = if t % 2 == 0 {
            shared.clone()
        } else {
            Arc::new(SqliteLearning::open(&path).unwrap())
        };
        threads.push(std::thread::spawn(move || {
            for i in 0..25u64 {
                let request = if i % 2 == 0 { "req-a" } else { "req-b" };
                store
                    .journal(
                        request,
                        entry(JournalKind::Tool, json!({"thread": t, "i": i})),
                    )
                    .unwrap();
            }
        }));
    }
    for thread in threads {
        thread.join().unwrap();
    }
    for (request, expected) in [("req-a", 8 * 13), ("req-b", 8 * 12)] {
        let events = shared.events(request, 0, 1000).unwrap();
        let seqs: Vec<u64> = events.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, (1..=expected).collect::<Vec<_>>(), "{request}");
        // Each writer's own entries keep their order.
        for t in 0..8u64 {
            let mine: Vec<u64> = events
                .iter()
                .filter(|e| e.body["thread"] == t)
                .map(|e| e.body["i"].as_u64().unwrap())
                .collect();
            assert!(mine.windows(2).all(|w| w[0] < w[1]));
        }
    }
}

#[test]
fn close_request_upserts_run_and_queues_reflection() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteLearning::open(&root.path().join("home/learning.sqlite")).unwrap();
    store.close_request("req-1", summary("partial")).unwrap();
    assert_eq!(store.run("req-1").unwrap().unwrap().status, "partial");
    assert_eq!(
        store.reflection_state("req-1").unwrap().unwrap().0,
        "queued"
    );

    // Closing again replaces the row and re-queues finished reflection.
    {
        let db = store.lock().unwrap();
        db.execute("UPDATE learn_reflection_queue SET state='done'", [])
            .unwrap();
    }
    let mut updated = summary("verified");
    updated.model_calls = 9;
    store.close_request("req-1", updated.clone()).unwrap();
    assert_eq!(store.run("req-1").unwrap().unwrap(), updated);
    assert_eq!(
        store.reflection_state("req-1").unwrap().unwrap().0,
        "queued"
    );
    let db = store.lock().unwrap();
    let rows: i64 = db
        .query_row("SELECT COUNT(*) FROM learn_runs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 1);
    // A running reflection is left alone.
    db.execute("UPDATE learn_reflection_queue SET state='running'", [])
        .unwrap();
    drop(db);
    store.close_request("req-1", summary("verified")).unwrap();
    assert_eq!(
        store.reflection_state("req-1").unwrap().unwrap().0,
        "running"
    );

    let mut bad = summary("verified");
    bad.task_type = Some("Not A Slug".into());
    assert!(store.close_request("req-2", bad).is_err());
    let mut bad = summary("verified");
    bad.cost_usd = f64::NAN;
    assert!(store.close_request("req-2", bad).is_err());
    assert!(store.run("req-2").unwrap().is_none());
}

#[test]
fn reopen_preserves_journal_and_runs() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("home/learning.sqlite");
    let store = SqliteLearning::open(&path).unwrap();
    store
        .journal(
            "req-1",
            entry(JournalKind::Failure, json!({"code": "missing_dependency"})),
        )
        .unwrap();
    store
        .journal(
            "req-1",
            entry(
                JournalKind::Repair,
                json!({"cmd": "uv pip install pdfplumber"}),
            ),
        )
        .unwrap();
    store.close_request("req-1", summary("verified")).unwrap();
    drop(store);

    let store = SqliteLearning::open(&path).unwrap();
    let events = store.events("req-1", 0, 10).unwrap();
    assert_eq!(
        events,
        vec![
            StoredEvent {
                seq: 1,
                kind: JournalKind::Failure,
                body: json!({"code": "missing_dependency"}),
                at: 1_700_000_000_000
            },
            StoredEvent {
                seq: 2,
                kind: JournalKind::Repair,
                body: json!({"cmd": "uv pip install pdfplumber"}),
                at: 1_700_000_000_000
            },
        ]
    );
    assert_eq!(store.events("req-1", 1, 10).unwrap().len(), 1);
    assert_eq!(store.run("req-1").unwrap().unwrap(), summary("verified"));
    store
        .journal("req-1", entry(JournalKind::Gate, json!({})))
        .unwrap();
    assert_eq!(store.events("req-1", 2, 10).unwrap()[0].seq, 3);
}

#[test]
fn oversize_body_is_truncated_with_marker() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteLearning::open(&root.path().join("home/learning.sqlite")).unwrap();
    // Quotes double when escaped inside the marker, which must still fit.
    let big = "\"é".repeat(40_000);
    store
        .journal(
            "req-1",
            entry(JournalKind::ShellCommand, json!({"stdout": big})),
        )
        .unwrap();
    let small = json!({"stdout": "ok"});
    store
        .journal("req-1", entry(JournalKind::ShellCommand, small.clone()))
        .unwrap();
    let events = store.events("req-1", 0, 10).unwrap();
    let marker = &events[0].body;
    assert_eq!(marker["truncated"], true);
    assert_eq!(
        marker["bytes"].as_u64().unwrap() as usize,
        serde_json::to_string(&json!({"stdout": big}))
            .unwrap()
            .len()
    );
    assert!(marker["head"]
        .as_str()
        .unwrap()
        .starts_with("{\"stdout\":\"\\\"é"));
    assert!(serde_json::to_string(marker).unwrap().len() <= MAX_EVENT_BODY_BYTES);
    assert_eq!(events[1].body, small);
}

#[test]
fn entries_past_the_request_limit_are_dropped_and_counted() {
    let root = tempfile::tempdir().unwrap();
    let mut store = SqliteLearning::open(&root.path().join("home/learning.sqlite")).unwrap();
    store.event_limit = 3;
    for i in 0..5 {
        store
            .journal("req-1", entry(JournalKind::Tool, json!({"i": i})))
            .unwrap();
    }
    store
        .journal("req-2", entry(JournalKind::Tool, json!({})))
        .unwrap();
    assert_eq!(store.events("req-1", 0, 10).unwrap().len(), 3);
    assert_eq!(store.dropped_events("req-1").unwrap(), 2);
    assert_eq!(store.dropped_events("req-2").unwrap(), 0);
    assert!(store
        .journal("", entry(JournalKind::Tool, json!({})))
        .is_err());
}

#[test]
fn unimplemented_lookups_return_empty_results() {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteLearning::open(&root.path().join("home/learning.sqlite")).unwrap();
    let signature = FailureSignature {
        tool: "shell".into(),
        error_code: "missing_dependency".into(),
        message: "no module named <id>".into(),
        os: "macos".into(),
        arch: "aarch64".into(),
    };
    assert_eq!(signature.digest().len(), 64);
    assert!(store.known_fix(&signature).unwrap().is_none());
    assert_eq!(
        store.context_for("main", "make a report", None).unwrap(),
        LearnedContext::default()
    );
}
