use kageha_contracts::{execution::Control, policy::*};
use kageha_policy::{input_digest, journal::Journal};
use std::{
    sync::{atomic::AtomicBool, Arc},
    time::{Duration, Instant},
};
fn request() -> ApprovalRequest {
    ApprovalRequest {
        authority: Authority {
            session: "chat".into(),
            turn: "turn".into(),
            binding: "browser.workspace".into(),
            generation: 1,
            resource: "profile/tab/destination".into(),
            input_digest: input_digest(&serde_json::json!({"file":"sha256-exact"})),
        },
        action: "upload".into(),
        class: ActionClass::Upload,
        detail: "Upload exact file to destination".into(),
    }
}
fn decision(r: &ApprovalRecord, approved: bool) -> ApprovalDecision {
    ApprovalDecision {
        authority: r.request.authority.clone(),
        expected_revision: r.revision,
        approved,
        scope: GrantScope::Once,
    }
}
#[test]
fn approval_is_exact_durable_and_consumed_once_under_concurrency() {
    let root = tempfile::tempdir().unwrap();
    let journal = Arc::new(Journal::open(&root.path().join("policy.sqlite")).unwrap());
    let r = journal.create("one", &request(), false).unwrap();
    journal.decide("one", &decision(&r, true)).unwrap();
    let results = std::thread::scope(|s| {
        let one = s.spawn(|| journal.consume("one", &r.request.authority).unwrap());
        let two = s.spawn(|| journal.consume("one", &r.request.authority).unwrap());
        [one.join().unwrap(), two.join().unwrap()]
    });
    assert_eq!(results.iter().filter(|v| **v).count(), 1);
    assert_eq!(
        journal.get("one").unwrap().unwrap().state,
        ApprovalState::Consumed
    );
    assert!(journal.decide("one", &decision(&r, true)).is_ok());
    assert!(journal.decide("one", &decision(&r, false)).is_err());
}
#[test]
fn altered_authority_or_revision_never_changes_a_decision_or_grant() {
    let root = tempfile::tempdir().unwrap();
    let j = Journal::open(&root.path().join("policy.sqlite")).unwrap();
    let r = j.create("one", &request(), false).unwrap();
    for n in 0..4 {
        let mut d = decision(&r, true);
        d.scope = GrantScope::Full;
        match n {
            0 => d.authority.generation += 1,
            1 => d.authority.resource = "different site".into(),
            2 => d.authority.input_digest = input_digest(&serde_json::json!({"file":"different"})),
            _ => d.expected_revision += 1,
        };
        assert!(j.decide("one", &d).is_err());
        assert_eq!(j.mode().unwrap(), PermissionMode::Ask);
        assert_eq!(j.get("one").unwrap().unwrap().state, ApprovalState::Pending);
    }
}
#[test]
fn restart_interrupts_unconsumed_decisions_but_preserves_outcomes() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("policy.sqlite");
    {
        let j = Journal::open(&path).unwrap();
        for id in ["pending", "approved", "denied", "consumed"] {
            let r = j.create(id, &request(), false).unwrap();
            if id != "pending" {
                j.decide(id, &decision(&r, id != "denied")).unwrap();
            }
            if id == "consumed" {
                j.consume(id, &r.request.authority).unwrap();
            }
        }
        j.set_mode(PermissionMode::Routine).unwrap();
    }
    let j = Journal::open(&path).unwrap();
    for (id, state) in [
        ("pending", ApprovalState::Interrupted),
        ("approved", ApprovalState::Interrupted),
        ("denied", ApprovalState::Denied),
        ("consumed", ApprovalState::Consumed),
    ] {
        assert_eq!(j.get(id).unwrap().unwrap().state, state);
        assert!(!j.consume(id, &request().authority).unwrap());
    }
    assert_eq!(j.mode().unwrap(), PermissionMode::Ask);
}
#[test]
fn cancelled_or_expired_approvals_reject_late_decisions() {
    let root = tempfile::tempdir().unwrap();
    let j = Journal::open(&root.path().join("policy.sqlite")).unwrap();
    let r = j.create("one", &request(), false).unwrap();
    let flag = AtomicBool::new(false);
    assert!(j
        .wait(
            "one",
            &r.request.authority,
            &Control {
                cancelled: &flag,
                deadline: Some(Instant::now() - Duration::from_millis(1))
            }
        )
        .is_err());
    assert_eq!(
        j.get("one").unwrap().unwrap().state,
        ApprovalState::Cancelled
    );
    assert!(j.decide("one", &decision(&r, true)).is_err());
}
#[test]
fn cancellation_is_scoped_to_one_turn() {
    let root = tempfile::tempdir().unwrap();
    let j = Journal::open(&root.path().join("policy.sqlite")).unwrap();
    j.create("one", &request(), false).unwrap();
    let mut other = request();
    other.authority.turn = "other".into();
    j.create("two", &other, false).unwrap();
    j.cancel_turn("chat", "turn").unwrap();
    assert_eq!(
        j.get("one").unwrap().unwrap().state,
        ApprovalState::Cancelled
    );
    assert_eq!(j.get("two").unwrap().unwrap().state, ApprovalState::Pending);
}
#[test]
fn restartable_import_preserves_ids_and_does_not_restore_old_permissions() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("policy.sqlite");
    let rows = (1..=300)
        .map(|i| {
            (
                i,
                ApprovalRecord {
                    id: format!("legacy-{i}"),
                    request: request(),
                    state: if i % 2 == 0 {
                        ApprovalState::Approved
                    } else {
                        ApprovalState::Pending
                    },
                    revision: 1,
                    decision: (i % 2 == 0).then_some(true),
                    scope: GrantScope::Once,
                    automatic: false,
                },
            )
        })
        .collect::<Vec<_>>();
    {
        let j = Journal::open(&path).unwrap();
        j.import_batch("source", PermissionMode::Full, &rows[..256])
            .unwrap();
        assert_eq!(j.import_cursor("source").unwrap(), Some(256));
        j.set_mode(PermissionMode::Ask).unwrap();
    }
    let j = Journal::open(&path).unwrap();
    j.import_batch("source", PermissionMode::Full, &rows[256..])
        .unwrap();
    j.import_batch("source", PermissionMode::Full, &rows)
        .unwrap();
    assert_eq!(j.mode().unwrap(), PermissionMode::Ask);
    assert_eq!(j.import_cursor("source").unwrap(), Some(300));
    assert_eq!(j.list(None, None, false, 0, 256).unwrap().len(), 256);
    assert_eq!(
        j.get("legacy-2").unwrap().unwrap().state,
        ApprovalState::Consumed
    );
    assert_eq!(
        j.get("legacy-1").unwrap().unwrap().state,
        ApprovalState::Interrupted
    );
}
#[test]
fn erased_sessions_cannot_be_restored_by_import_or_create() {
    let root = tempfile::tempdir().unwrap();
    let j = Journal::open(&root.path().join("policy.sqlite")).unwrap();
    let r = j.create("one", &request(), false).unwrap();
    j.erase_session("chat").unwrap();
    j.import_batch("old-source", PermissionMode::Ask, &[(1, r)])
        .unwrap();
    assert!(j.get("one").unwrap().is_none());
    assert!(j.create("two", &request(), false).is_err());
}
#[test]
fn application_databases_and_second_writers_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("policy.sqlite");
    let _one = Journal::open(&path).unwrap();
    assert!(Journal::open(&path).is_err());
    let app = root.path().join("app.sqlite");
    let db = rusqlite::Connection::open(&app).unwrap();
    db.execute("CREATE TABLE ui_approvals(id TEXT)", [])
        .unwrap();
    drop(db);
    assert!(Journal::open(&app).is_err());
}
#[test]
fn automatic_permissions_cannot_authorize_an_upload() {
    let root = tempfile::tempdir().unwrap();
    let j = Journal::open(&root.path().join("policy.sqlite")).unwrap();
    assert!(j.create("one", &request(), true).is_err());
    let mut r = request();
    r.action = "fill".into();
    r.class = ActionClass::RoutineFill;
    let audit = j.create("fill", &r, true).unwrap();
    assert_eq!(audit.state, ApprovalState::Approved);
    assert!(j.consume("fill", &r.authority).unwrap());
    assert!(!j.consume("fill", &r.authority).unwrap());
}
#[test]
fn invalid_import_rolls_back_records_and_cursor() {
    let root = tempfile::tempdir().unwrap();
    let j = Journal::open(&root.path().join("policy.sqlite")).unwrap();
    let good = ApprovalRecord {
        id: "one".into(),
        request: request(),
        state: ApprovalState::Pending,
        revision: 1,
        decision: None,
        scope: GrantScope::Once,
        automatic: false,
    };
    let mut bad = good.clone();
    bad.id = "bad".into();
    bad.request.authority.generation = 0;
    assert!(j
        .import_batch("source", PermissionMode::Full, &[(1, good), (2, bad)])
        .is_err());
    assert!(j.get("one").unwrap().is_none());
    assert!(j.import_cursor("source").unwrap().is_none());
    assert_eq!(j.mode().unwrap(), PermissionMode::Ask);
}

#[test]
fn unordered_import_is_rejected_before_writing_or_advancing_cursor() {
    let root = tempfile::tempdir().unwrap();
    let j = Journal::open(&root.path().join("policy.sqlite")).unwrap();
    let r = ApprovalRecord {
        id: "one".into(),
        request: request(),
        state: ApprovalState::Pending,
        revision: 1,
        decision: None,
        scope: GrantScope::Once,
        automatic: false,
    };
    assert!(j
        .import_batch("source", PermissionMode::Full, &[(2, r.clone()), (1, r)])
        .is_err());
    assert!(j.import_cursor("source").unwrap().is_none());
    assert_eq!(j.mode().unwrap(), PermissionMode::Ask);
    assert!(j.get("one").unwrap().is_none());
}
#[test]
fn explicitly_selected_full_mode_survives_restart() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("policy.sqlite");
    {
        let j = Journal::open(&path).unwrap();
        j.set_mode(PermissionMode::Full).unwrap();
    }
    let j = Journal::open(&path).unwrap();
    assert_eq!(j.mode().unwrap(), PermissionMode::Full);
}

#[test]
fn supported_long_plan_and_destination_metadata_remain_reviewable() {
    let root = tempfile::tempdir().unwrap();
    let j = Journal::open(&root.path().join("policy.sqlite")).unwrap();
    let mut r = request();
    r.class = ActionClass::Plan;
    r.action = "approve_plan".into();
    r.detail = "review".repeat(22000);
    r.authority.resource = format!("https://example.test/?{}", "q".repeat(8000));
    let saved = j.create("long-plan", &r, false).unwrap();
    assert_eq!(saved.request, r);
}
