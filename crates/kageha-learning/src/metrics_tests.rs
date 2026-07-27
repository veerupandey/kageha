use super::*;
use crate::{LearningPort, RequestSummary};

/// Wednesday 2025-11-12 12:00 UTC (ISO week 2025-W46).
const NOW: u64 = 1_762_948_800_000;
const DAY: u64 = 24 * 3600 * 1000;

fn store() -> (tempfile::TempDir, SqliteLearning) {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteLearning::open(&root.path().join("learning.sqlite")).unwrap();
    (root, store)
}

fn close(
    store: &SqliteLearning,
    request: &str,
    task_type: Option<&str>,
    status: &str,
    calls: u64,
    cost: f64,
    cached: u64,
    ended: u64,
) {
    store
        .close_request(
            request,
            RequestSummary {
                agent: "main".into(),
                chat_id: "chat".into(),
                task_type: task_type.map(str::to_owned),
                status: status.into(),
                model_calls: calls,
                cost_usd: cost,
                cached_tokens: cached,
                outcomes: Vec::new(),
                started: ended - 1000,
                ended,
            },
        )
        .unwrap();
}

#[test]
fn iso_weeks_follow_the_iso_calendar() {
    assert_eq!(iso_week(NOW), "2025-W46");
    // Thursday 1970-01-01 is in 1970-W01.
    assert_eq!(iso_week(0), "1970-W01");
    // Monday 2024-12-30 belongs to 2025-W01; Sunday 2021-01-03 to 2020-W53.
    assert_eq!(iso_week(1_735_516_800_000), "2025-W01");
    assert_eq!(iso_week(1_609_632_000_000), "2020-W53");
    // Sunday 2025-11-16 23:59:59.999 is still W46; the next millisecond is W47.
    assert_eq!(iso_week(1_763_337_599_999), "2025-W46");
    assert_eq!(iso_week(1_763_337_600_000), "2025-W47");
    assert_eq!(civil_from_days(0), (1970, 1, 1));
    assert_eq!(civil_from_days(-1), (1969, 12, 31));
    for day in (-200_000..200_000).step_by(37) {
        let (y, m, d) = civil_from_days(day);
        assert_eq!(days_from_civil(y, m, d), day);
    }
}

#[test]
fn metrics_are_grouped_by_task_type_and_week() {
    let (_root, store) = store();
    // This week: 3 pdf-reports requests, 2 verified.
    close(
        &store,
        "a",
        Some("pdf-reports"),
        "verified",
        4,
        0.10,
        100,
        NOW,
    );
    close(
        &store,
        "b",
        Some("pdf-reports"),
        "verified",
        6,
        0.20,
        300,
        NOW - DAY,
    );
    close(
        &store,
        "c",
        Some("pdf-reports"),
        "partial",
        8,
        0.30,
        200,
        NOW - 2 * DAY,
    );
    // Last week (Tuesday 2025-11-04): one failed request.
    close(
        &store,
        "d",
        Some("pdf-reports"),
        "failed",
        10,
        1.0,
        0,
        NOW - 8 * DAY,
    );
    // Another task type and a request without one.
    close(&store, "e", Some("emails"), "verified", 2, 0.0, 50, NOW);
    close(&store, "f", None, "not_required", 1, 0.0, 0, NOW);
    // Outside the default 12 weeks.
    close(
        &store,
        "g",
        Some("pdf-reports"),
        "verified",
        1,
        0.0,
        0,
        NOW - 12 * 7 * DAY,
    );

    let metrics = store.metrics_at(DEFAULT_METRICS_WEEKS, None, NOW).unwrap();
    let summary: Vec<_> = metrics
        .iter()
        .map(|m| {
            (
                m.task_type.as_deref(),
                m.week.as_str(),
                m.requests,
                m.verified,
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            (None, "2025-W46", 1, 0),
            (Some("emails"), "2025-W46", 1, 1),
            (Some("pdf-reports"), "2025-W45", 1, 0),
            (Some("pdf-reports"), "2025-W46", 3, 2),
        ]
    );
    let current = &metrics[3];
    assert_eq!(current.week_start, "2025-11-10");
    assert!((current.verified_rate - 2.0 / 3.0).abs() < 1e-12);
    assert!((current.mean_model_calls - 6.0).abs() < 1e-12);
    assert!((current.mean_cost_usd - 0.2).abs() < 1e-12);
    assert!((current.mean_cached_tokens - 200.0).abs() < 1e-12);
    assert_eq!(metrics[2].week_start, "2025-11-03");
    assert_eq!(metrics[2].verified_rate, 0.0);

    // One task type; one week only; the oldest request with a longer window.
    let only: Vec<_> = store
        .metrics_at(12, Some("emails"), NOW)
        .unwrap()
        .into_iter()
        .map(|m| m.task_type)
        .collect();
    assert_eq!(only, [Some("emails".to_owned())]);
    assert_eq!(
        store.metrics_at(1, Some("pdf-reports"), NOW).unwrap().len(),
        1
    );
    let long = store.metrics_at(13, Some("pdf-reports"), NOW).unwrap();
    assert_eq!(long.len(), 3);
    assert_eq!(long[0].week, "2025-W34");
    // `weeks` is clamped to 1..=104.
    assert_eq!(
        store.metrics_at(0, Some("pdf-reports"), NOW).unwrap().len(),
        1
    );
    assert_eq!(store.metrics_at(u32::MAX, None, NOW).unwrap().len(), 5);
}

#[test]
fn speed_cohorts_separate_agents_and_only_measure_verified_runs() {
    let (_root, store) = store();
    let run = |id: &str, agent: &str, task: Option<&str>, status: &str, elapsed: u64, ended: u64| {
        store.close_request(id, RequestSummary {
            agent: agent.into(), chat_id: "chat".into(), task_type: task.map(str::to_owned),
            status: status.into(), model_calls: 2, cost_usd: 0.0, cached_tokens: 0,
            outcomes: vec![], started: ended - elapsed, ended,
        }).unwrap();
    };
    run("a", "main", Some("review"), "verified", 10_000, NOW);
    run("b", "main", Some("review"), "verified", 30_000, NOW - DAY);
    run("c", "main", Some("review"), "failed", 1, NOW);
    run("d", "other", Some("review"), "verified", 2_000, NOW);
    run("e", "main", Some("review"), "failed", 1, NOW - 8 * DAY);
    run("f", "main", None, "verified", 1, NOW);
    run("future", "main", Some("review"), "verified", 1, NOW + DAY);
    let metrics = store.speed_metrics_at(12, Some("review"), NOW).unwrap();
    assert_eq!(metrics.len(), 3);
    let current = &metrics[1];
    assert_eq!((&current.agent[..], &current.task_type[..], &current.week[..]), ("main", "review", "2025-W46"));
    assert_eq!((current.requests, current.verified, current.elapsed_samples), (3, 2, 2));
    assert_eq!(current.median_elapsed_ms, Some(20_000.0));
    assert_eq!(current.p95_elapsed_ms, Some(30_000));
    assert!((current.verified_rate - 2.0 / 3.0).abs() < 1e-12);
    assert_eq!(current.mean_model_calls, 2.0);
    assert_eq!((metrics[0].median_elapsed_ms, metrics[0].p95_elapsed_ms), (None, None));
    assert_eq!(metrics[2].agent, "other");
    assert_eq!(metrics[2].median_elapsed_ms, Some(2_000.0));
    assert!(store.speed_metrics_at(1, Some("other-task"), NOW).unwrap().is_empty());
}
