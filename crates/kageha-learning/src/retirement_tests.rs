use super::*;
use crate::lessons::LessonStatus;
use crate::{LearningPort, RequestSummary};
use serde_json::json;

const DAY: u64 = 24 * 3600 * 1000;
const NOW: u64 = 400 * DAY;
const TASK: &str = "pdf-reports";
const PROMPT: &str = "Extract the tables from the scanned PDF report";

fn store() -> (tempfile::TempDir, SqliteLearning) {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteLearning::open(&root.path().join("learning.sqlite")).unwrap();
    (root, store)
}

fn lesson(store: &SqliteLearning, task_type: &str) -> i64 {
    let db = store.lock().unwrap();
    db.execute(
        "INSERT INTO learn_lessons(agent,task_type,\"trigger\",action,status,support,shown,created) VALUES('main',?,?,?,'active',?,0,?)",
        params![
            task_type,
            "A scanned PDF report has no text layer",
            "run OCR with `ocrmypdf` before extracting tables",
            json!(["r0"]).to_string(),
            (NOW - DAY) as i64
        ],
    )
    .unwrap();
    db.last_insert_rowid()
}

/// Closes `request` with `status`, as the Host does.
fn close(store: &SqliteLearning, request: &str, task_type: Option<&str>, status: &str, ended: u64) {
    store
        .close_request(
            request,
            RequestSummary {
                agent: "main".into(),
                chat_id: "chat".into(),
                task_type: task_type.map(str::to_owned),
                status: status.into(),
                model_calls: 3,
                cost_usd: 0.0,
                cached_tokens: 0,
                outcomes: Vec::new(),
                started: ended - 1000,
                ended,
            },
        )
        .unwrap();
}

/// `count` requests that showed `lesson`, the first `verified` of them verified. Returns
/// the last request id.
fn shown(store: &SqliteLearning, lesson: i64, count: usize, verified: usize) -> String {
    let mut last = String::new();
    for n in 0..count {
        last = format!("shown-{lesson}-{n}");
        store.record_lesson_uses(&last, &[lesson]).unwrap();
        let ok = n < verified;
        close(
            store,
            &last,
            Some(TASK),
            if ok { "verified" } else { "partial" },
            NOW - DAY,
        );
        store.decide_lesson_uses(&last, ok).unwrap();
    }
    last
}

/// `count` requests of `task_type` that did not show the lesson, the first `verified` of
/// them verified, ending at `ended`.
fn others(
    store: &SqliteLearning,
    prefix: &str,
    task_type: Option<&str>,
    count: usize,
    verified: usize,
    ended: u64,
) {
    for n in 0..count {
        let status = if n < verified { "verified" } else { "failed" };
        close(store, &format!("{prefix}-{n}"), task_type, status, ended);
    }
}

fn status(store: &SqliteLearning, id: i64) -> (String, Option<i64>) {
    store
        .lock()
        .unwrap()
        .query_row(
            "SELECT status,retired_at FROM learn_lessons WHERE id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
}

#[test]
fn should_retire_needs_both_minimums_and_a_strictly_lower_rate() {
    assert!(should_retire(10, 3, 10, 8));
    assert!(!should_retire(9, 0, 10, 10), "fewer than 10 shown uses");
    assert!(
        !should_retire(10, 0, 9, 9),
        "fewer than 10 comparison requests"
    );
    assert!(!should_retire(10, 5, 20, 10), "equal rates");
    assert!(!should_retire(10, 8, 10, 3), "a higher rate");
    // 6/11 < 11/20: exact comparison, no rounding.
    assert!(should_retire(11, 6, 20, 11));
    assert!(!should_retire(u64::MAX, u64::MAX, u64::MAX, u64::MAX));
}

#[test]
fn a_lesson_that_does_worse_than_ten_comparison_requests_is_retired_and_leaves_the_context() {
    let (_root, store) = store();
    let id = lesson(&store, TASK);
    assert_eq!(
        store
            .context_at("main", PROMPT, Some(TASK), &[], NOW)
            .unwrap()
            .lesson_ids,
        [id]
    );
    let last = shown(&store, id, 10, 3);
    others(&store, "other", Some(TASK), 10, 8, NOW - 2 * DAY);
    assert_eq!(
        store.retirement_counts(id, NOW).unwrap(),
        Some(RetirementCounts {
            shown_uses: 10,
            lesson_verified: 3,
            comparison_total: 10,
            comparison_verified: 8
        })
    );
    assert_eq!(store.retire_underperforming_at(&last, NOW).unwrap(), [id]);
    assert_eq!(status(&store, id), ("retired".into(), Some(NOW as i64)));
    // Retired lessons are never shown again, and a second check changes nothing.
    let context = store
        .context_at("main", PROMPT, Some(TASK), &[], NOW)
        .unwrap();
    assert!(
        context.lesson_ids.is_empty() && context.text.is_empty(),
        "{context:?}"
    );
    assert!(store
        .retire_underperforming_at(&last, NOW + 1)
        .unwrap()
        .is_empty());
    assert_eq!(status(&store, id), ("retired".into(), Some(NOW as i64)));
    assert_eq!(
        store.lessons("main").unwrap()[0].status,
        LessonStatus::Retired
    );
}

#[test]
fn fewer_than_ten_comparison_requests_keep_the_lesson() {
    let (_root, store) = store();
    let id = lesson(&store, TASK);
    let last = shown(&store, id, 10, 0);
    others(&store, "other", Some(TASK), 9, 9, NOW - 2 * DAY);
    // Requests of another task type, or older than 90 days, are not comparisons.
    others(&store, "elsewhere", Some("emails"), 10, 10, NOW - 2 * DAY);
    others(&store, "old", Some(TASK), 10, 10, NOW - 91 * DAY);
    assert_eq!(
        store
            .retirement_counts(id, NOW)
            .unwrap()
            .unwrap()
            .comparison_total,
        9
    );
    assert!(store
        .retire_underperforming_at(&last, NOW)
        .unwrap()
        .is_empty());
    assert_eq!(status(&store, id), ("active".into(), None));
}

#[test]
fn an_equal_rate_keeps_the_lesson() {
    let (_root, store) = store();
    let id = lesson(&store, TASK);
    let last = shown(&store, id, 10, 5);
    others(&store, "other", Some(TASK), 10, 5, NOW - 2 * DAY);
    assert!(store
        .retire_underperforming_at(&last, NOW)
        .unwrap()
        .is_empty());
    assert_eq!(status(&store, id), ("active".into(), None));
}

#[test]
fn undecided_uses_do_not_count_and_only_the_closing_requests_lessons_are_checked() {
    let (_root, store) = store();
    let id = lesson(&store, TASK);
    let unrelated = lesson(&store, TASK);
    shown(&store, unrelated, 10, 0);
    let last = shown(&store, id, 9, 0);
    // A tenth use whose request has not closed yet.
    store.record_lesson_uses("open", &[id]).unwrap();
    others(&store, "other", Some(TASK), 10, 10, NOW - 2 * DAY);
    assert_eq!(
        store
            .retirement_counts(id, NOW)
            .unwrap()
            .unwrap()
            .shown_uses,
        9
    );
    assert!(store
        .retire_underperforming_at(&last, NOW)
        .unwrap()
        .is_empty());
    // `unrelated` qualifies but was not shown in `last`.
    assert_eq!(status(&store, unrelated).0, "active");
    assert_eq!(status(&store, id).0, "active");
}
