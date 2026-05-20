use kageha_contracts::{execution::Control, policy::*};
use kageha_policy::{journal::Journal, requirements::Requirements};
use std::{io, sync::atomic::AtomicBool};
fn request() -> ApprovalRequest {
    ApprovalRequest {
        authority: Authority {
            session: "chat".into(),
            turn: "turn".into(),
            binding: "browser.workspace".into(),
            generation: 1,
            resource: "https://example.test/form".into(),
            input_digest: "a".repeat(64),
        },
        action: "browser_click".into(),
        class: ActionClass::Mutation,
        detail: "Click observed sign in".into(),
    }
}
struct Review(ReviewVerdict);
impl ReviewPort for Review {
    fn review(&self, r: &ApprovalRequest, _: &Control<'_>) -> io::Result<ReviewDecision> {
        Ok(ReviewDecision {
            input_digest: r.authority.input_digest.clone(),
            verdict: self.0,
            rationale: "Specific fixture decision".into(),
        })
    }
    fn review_retry(&self, r: &ApprovalRequest, c: &Control<'_>) -> io::Result<ReviewDecision> {
        Review(ReviewVerdict::Allow).review(r, c)
    }
}
#[test]
fn requirements_cannot_be_overridden_by_full_or_legacy_modes() {
    let dir = tempfile::tempdir().unwrap();
    let rules = Requirements {
        allowed_profiles: vec![PermissionProfile::Ask, PermissionProfile::AutoReview],
        forced_profile: Some(PermissionProfile::AutoReview),
        denied_actions: vec!["remote_exec".into()],
        manual_actions: vec!["browser_upload".into()],
        ..Requirements::default()
    };
    let j = Journal::open_with_requirements(&dir.path().join("p"), rules).unwrap();
    assert_eq!(
        j.profile("chat").unwrap(),
        Some(PermissionProfile::AutoReview)
    );
    assert!(j.set_profile("chat", PermissionProfile::Full).is_err());
    assert!(j.set_mode(PermissionMode::Full).is_err());
    let cancel = AtomicBool::new(false);
    let c = Control {
        cancelled: &cancel,
        deadline: None,
    };
    let mut r = request();
    r.action = "remote_exec".into();
    assert!(is_stop(
        &j.prepare_profile(&r, Some(&Review(ReviewVerdict::Allow)), &c)
            .unwrap_err()
    ));
    r.action = "browser_upload".into();
    r.class = ActionClass::Upload;
    assert_eq!(
        j.prepare_profile(&r, Some(&Review(ReviewVerdict::Allow)), &c)
            .unwrap()
            .state,
        ApprovalState::Pending
    );
}
#[test]
fn delegated_workers_inherit_live_revocation_and_cannot_escalate() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("p");
    let j = Journal::open(&path).unwrap();
    j.set_profile("chat", PermissionProfile::Full).unwrap();
    j.delegate("chat", "worker").unwrap();
    j.delegate("worker", "grandchild").unwrap();
    assert_eq!(
        j.profile("grandchild").unwrap(),
        Some(PermissionProfile::Full)
    );
    assert!(j.set_profile("worker", PermissionProfile::Full).is_err());
    assert!(j.delegate("other", "worker").is_err());
    assert!(j.delegate("worker", "chat").is_err());
    let cancel = AtomicBool::new(false);
    let c = Control {
        cancelled: &cancel,
        deadline: None,
    };
    let mut r = request();
    r.authority.session = "grandchild".into();
    let a = j.prepare_profile(&r, None, &c).unwrap();
    j.set_profile("chat", PermissionProfile::Ask).unwrap();
    assert_eq!(
        j.get(&a.id).unwrap().unwrap().state,
        ApprovalState::Cancelled
    );
    assert_eq!(
        j.profile("grandchild").unwrap(),
        Some(PermissionProfile::Ask)
    );
    drop(j);
    let j = Journal::open(&path).unwrap();
    assert_eq!(j.owner("grandchild").unwrap(), "chat");
}
#[test]
fn repeated_denials_stop_and_survive_restarts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("p");
    let j = Journal::open(&path).unwrap();
    j.set_profile("chat", PermissionProfile::AutoReview)
        .unwrap();
    let cancel = AtomicBool::new(false);
    let c = Control {
        cancelled: &cancel,
        deadline: None,
    };
    let r = request();
    for _ in 0..2 {
        assert_eq!(
            j.prepare_profile(&r, Some(&Review(ReviewVerdict::Deny)), &c)
                .unwrap()
                .state,
            ApprovalState::Denied
        );
    }
    assert!(is_stop(
        &j.prepare_profile(&r, Some(&Review(ReviewVerdict::Deny)), &c)
            .unwrap_err()
    ));
    drop(j);
    let j = Journal::open(&path).unwrap();
    assert!(j.denial_limit("chat", "turn").unwrap());
    assert!(is_stop(
        &j.prepare_profile(&r, Some(&Review(ReviewVerdict::Allow)), &c)
            .unwrap_err()
    ));
    assert_eq!(j.denials("chat").unwrap().len(), 3);
}
#[test]
fn exact_user_retry_is_one_reviewed_attempt_and_does_not_leak_to_other_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let j = Journal::open(&dir.path().join("p")).unwrap();
    j.set_profile("chat", PermissionProfile::AutoReview)
        .unwrap();
    let cancel = AtomicBool::new(false);
    let c = Control {
        cancelled: &cancel,
        deadline: None,
    };
    let r = request();
    let denied = j
        .prepare_profile(&r, Some(&Review(ReviewVerdict::Deny)), &c)
        .unwrap();
    let mut wrong = r.authority.clone();
    wrong.generation = 2;
    assert!(j
        .request_override(&denied.id, &wrong, denied.revision)
        .is_err());
    j.request_override(&denied.id, &r.authority, denied.revision)
        .unwrap();
    assert!(j
        .request_override(&denied.id, &r.authority, denied.revision)
        .is_err());
    let mut other = r.clone();
    other.authority.resource = "https://other.test".into();
    assert_eq!(
        j.prepare_profile(&other, Some(&Review(ReviewVerdict::Ask)), &c)
            .unwrap()
            .state,
        ApprovalState::Pending
    );
    assert_eq!(
        j.prepare_profile(&r, Some(&Review(ReviewVerdict::Deny)), &c)
            .unwrap()
            .state,
        ApprovalState::Approved
    );
    assert_eq!(
        j.prepare_profile(&r, Some(&Review(ReviewVerdict::Deny)), &c)
            .unwrap()
            .state,
        ApprovalState::Denied
    );
}
#[test]
fn rolling_denials_are_bounded_and_uncertainty_resets_consecutive_count() {
    let dir = tempfile::tempdir().unwrap();
    let j = Journal::open(&dir.path().join("p")).unwrap();
    j.set_profile("chat", PermissionProfile::AutoReview)
        .unwrap();
    let cancel = AtomicBool::new(false);
    let c = Control {
        cancelled: &cancel,
        deadline: None,
    };
    let r = request();
    for _ in 0..9 {
        j.prepare_profile(&r, Some(&Review(ReviewVerdict::Deny)), &c)
            .unwrap();
        j.prepare_profile(&r, Some(&Review(ReviewVerdict::Ask)), &c)
            .unwrap();
    }
    assert!(!j.denial_limit("chat", "turn").unwrap());
    assert!(is_stop(
        &j.prepare_profile(&r, Some(&Review(ReviewVerdict::Deny)), &c)
            .unwrap_err()
    ));
}
#[test]
fn deleting_parent_erases_child_decisions_and_prevents_fallback_grants() {
    let dir = tempfile::tempdir().unwrap();
    let j = Journal::open(&dir.path().join("p")).unwrap();
    j.set_profile("chat", PermissionProfile::Ask).unwrap();
    j.delegate("chat", "worker").unwrap();
    let mut r = request();
    r.authority.session = "worker".into();
    let cancel = AtomicBool::new(false);
    let c = Control {
        cancelled: &cancel,
        deadline: None,
    };
    let a = j.prepare_profile(&r, None, &c).unwrap();
    assert_eq!(j.pending_family("chat").unwrap().len(), 1);
    j.erase_session("chat").unwrap();
    assert!(j.get(&a.id).unwrap().is_none());
    assert!(j.profile("worker").is_err());
    assert!(j.delegate("other", "worker").is_err());
}

#[test]
fn late_allow_is_fenced_after_another_review_stops_the_turn() {
    struct Slow<'a>(&'a std::sync::Barrier, &'a std::sync::Barrier);
    impl ReviewPort for Slow<'_> {
        fn review(&self, r: &ApprovalRequest, c: &Control<'_>) -> io::Result<ReviewDecision> {
            self.0.wait();
            self.1.wait();
            Review(ReviewVerdict::Allow).review(r, c)
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let j = Journal::open_with_requirements(
        &dir.path().join("p"),
        Requirements {
            consecutive_denials: 1,
            ..Default::default()
        },
    )
    .unwrap();
    j.set_profile("chat", PermissionProfile::AutoReview)
        .unwrap();
    let started = std::sync::Barrier::new(2);
    let resume = std::sync::Barrier::new(2);
    let cancel = AtomicBool::new(false);
    let c = Control {
        cancelled: &cancel,
        deadline: None,
    };
    let r = request();
    std::thread::scope(|scope| {
        let job = scope.spawn(|| j.prepare_profile(&r, Some(&Slow(&started, &resume)), &c));
        started.wait();
        assert!(is_stop(
            &j.prepare_profile(&r, Some(&Review(ReviewVerdict::Deny)), &c)
                .unwrap_err()
        ));
        resume.wait();
        assert!(is_stop(&job.join().unwrap().unwrap_err()));
    });
    assert!(j
        .list(Some("chat"), None, false, 0, 100)
        .unwrap()
        .iter()
        .all(|(_, r)| r.state != ApprovalState::Approved));
}

#[test]
fn duplicate_profiles_and_loosened_denial_limits_are_rejected(){
 let mut rules=Requirements{allowed_profiles:vec![PermissionProfile::Ask,PermissionProfile::Ask,PermissionProfile::Ask],..Default::default()};assert!(rules.validate().is_err());rules.allowed_profiles=vec![PermissionProfile::Ask];rules.consecutive_denials=4;assert!(rules.validate().is_err());
}
