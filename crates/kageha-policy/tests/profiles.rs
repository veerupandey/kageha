use kageha_contracts::{execution::Control, policy::*};
use kageha_policy::journal::Journal;
use std::{
    io,
    sync::atomic::{AtomicBool, Ordering},
};
fn request() -> ApprovalRequest {
    ApprovalRequest {
        authority: Authority {
            session: "chat".into(),
            turn: "turn".into(),
            binding: "browser.workspace".into(),
            generation: 1,
            resource: "https://example.com/apply".into(),
            input_digest: "a".repeat(64),
        },
        action: "browser_click".into(),
        class: ActionClass::Mutation,
        detail: "Submit application".into(),
    }
}
struct Reviewer(ReviewVerdict);
impl ReviewPort for Reviewer {
    fn review(&self, r: &ApprovalRequest, _: &Control<'_>) -> io::Result<ReviewDecision> {
        Ok(ReviewDecision {
            input_digest: r.authority.input_digest.clone(),
            verdict: self.0,
            rationale: "Reviewed specific user request".into(),
        })
    }
}
#[test]
fn chat_modes_are_independent_persistent_and_full_does_not_replay() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("policy.sqlite");
    let j = Journal::open(&path).unwrap();
    j.set_profile("chat", PermissionProfile::Full).unwrap();
    j.set_profile("other", PermissionProfile::Ask).unwrap();
    let cancel = AtomicBool::new(false);
    let c = Control {
        cancelled: &cancel,
        deadline: None,
    };
    let r = request();
    let a = j.prepare_profile(&r, None, &c).unwrap();
    assert_eq!(a.state, ApprovalState::Approved);
    assert!(j.consume(&a.id, &r.authority).unwrap());
    assert!(!j.consume(&a.id, &r.authority).unwrap());
    assert_eq!(j.profile("other").unwrap(), Some(PermissionProfile::Ask));
    drop(j);
    let j = Journal::open(&path).unwrap();
    assert_eq!(j.profile("chat").unwrap(), Some(PermissionProfile::Full));
    assert_eq!(
        j.get(&a.id).unwrap().unwrap().state,
        ApprovalState::Consumed
    );
}
#[test]
fn review_is_specific_and_uncertainty_is_not_approval() {
    for verdict in [
        ReviewVerdict::Allow,
        ReviewVerdict::Deny,
        ReviewVerdict::Ask,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let j = Journal::open(&dir.path().join("p")).unwrap();
        j.set_profile("chat", PermissionProfile::AutoReview)
            .unwrap();
        let cancel = AtomicBool::new(false);
        let r = request();
        let a = j
            .prepare_profile(
                &r,
                Some(&Reviewer(verdict)),
                &Control {
                    cancelled: &cancel,
                    deadline: None,
                },
            )
            .unwrap();
        assert_eq!(
            a.state,
            match verdict {
                ReviewVerdict::Allow => ApprovalState::Approved,
                ReviewVerdict::Deny => ApprovalState::Denied,
                ReviewVerdict::Ask => ApprovalState::Pending,
            }
        );
        let mut wrong = r.authority.clone();
        wrong.resource = "https://other.test".into();
        assert!(j.consume(&a.id, &wrong).is_err());
    }
}
#[test]
fn unavailable_reviewer_and_plan_always_require_a_human() {
    let dir = tempfile::tempdir().unwrap();
    let j = Journal::open(&dir.path().join("p")).unwrap();
    j.set_profile("chat", PermissionProfile::AutoReview)
        .unwrap();
    let cancel = AtomicBool::new(false);
    let c = Control {
        cancelled: &cancel,
        deadline: None,
    };
    assert_eq!(
        j.prepare_profile(&request(), None, &c).unwrap().state,
        ApprovalState::Pending
    );
    j.set_profile("chat", PermissionProfile::Full).unwrap();
    let mut r = request();
    r.class = ActionClass::Plan;
    assert_eq!(
        j.prepare_profile(&r, None, &c).unwrap().state,
        ApprovalState::Pending
    );
}
#[test]
fn revocation_fences_an_inflight_review() {
    struct Switch<'a>(&'a Journal);
    impl ReviewPort for Switch<'_> {
        fn review(&self, r: &ApprovalRequest, c: &Control<'_>) -> io::Result<ReviewDecision> {
            self.0.set_profile("chat", PermissionProfile::Ask).unwrap();
            Reviewer(ReviewVerdict::Allow).review(r, c)
        }
    }
    // Explicit implementation below avoids permitting a late allow after a mode change.
    let dir = tempfile::tempdir().unwrap();
    let j = Journal::open(&dir.path().join("p")).unwrap();
    j.set_profile("chat", PermissionProfile::AutoReview)
        .unwrap();
    let cancel = AtomicBool::new(false);
    let c = Control {
        cancelled: &cancel,
        deadline: None,
    };
    assert!(j
        .prepare_profile(&request(), Some(&Switch(&j)), &c)
        .is_err());
    assert!(j.list(Some("chat"), None, true, 0, 100).unwrap().is_empty());
}
#[test]
fn cancelled_and_forged_reviews_never_allow() {
    struct Bad<'a>(&'a AtomicBool, bool);
    impl ReviewPort for Bad<'_> {
        fn review(&self, r: &ApprovalRequest, c: &Control<'_>) -> io::Result<ReviewDecision> {
            if self.1 {
                self.0.store(true, Ordering::Release);
            }
            let mut d = Reviewer(ReviewVerdict::Allow).review(r, c)?;
            d.input_digest = "f".repeat(64);
            Ok(d)
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let j = Journal::open(&dir.path().join("p")).unwrap();
    j.set_profile("chat", PermissionProfile::AutoReview)
        .unwrap();
    let cancel = AtomicBool::new(false);
    let c = Control {
        cancelled: &cancel,
        deadline: None,
    };
    assert_eq!(
        j.prepare_profile(&request(), Some(&Bad(&cancel, false)), &c)
            .unwrap()
            .state,
        ApprovalState::Pending
    );
    assert!(j
        .prepare_profile(&request(), Some(&Bad(&cancel, true)), &c)
        .is_err());
}

#[test]
fn scoped_decision_does_not_broaden_other_chats() {
    let dir = tempfile::tempdir().unwrap();
    let j = Journal::open(&dir.path().join("p")).unwrap();
    j.set_profile("chat", PermissionProfile::Ask).unwrap();
    let cancel = AtomicBool::new(false);
    let c = Control {
        cancelled: &cancel,
        deadline: None,
    };
    let req = request();
    let r = j.prepare_profile(&req, None, &c).unwrap();
    let resolved = j
        .decide(
            &r.id,
            &ApprovalDecision {
                expected_revision: r.revision,
                authority: req.authority,
                approved: true,
                scope: GrantScope::Full,
            },
        )
        .unwrap();
    assert_eq!(resolved.scope, GrantScope::Once);
    assert_eq!(j.mode().unwrap(), PermissionMode::Ask);
    assert_eq!(j.profile("other").unwrap(), None);
}

#[test]
fn specialist_risk_cannot_use_full_or_review_grants_and_recovery_never_replays() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("policy.sqlite");
    let j = Journal::open(&path).unwrap();
    let cancel = AtomicBool::new(false);
    let c = Control { cancelled: &cancel, deadline: None };
    let mut r=request();r.class=ActionClass::Restricted;
    for profile in [PermissionProfile::Full, PermissionProfile::AutoReview] {
        j.set_profile("chat",profile).unwrap();
        let record=j.prepare_profile(&r,Some(&Reviewer(ReviewVerdict::Allow)),&c).unwrap();
        assert_eq!(record.state,ApprovalState::Pending);
        let mut changed=r.authority.clone();changed.input_digest="b".repeat(64);
        assert!(j.decide(&record.id,&ApprovalDecision{authority:changed,expected_revision:record.revision,approved:true,scope:GrantScope::Full}).is_err());
        let decided=j.decide(&record.id,&ApprovalDecision{authority:r.authority.clone(),expected_revision:record.revision,approved:true,scope:GrantScope::Full}).unwrap();
        assert_eq!(decided.scope,GrantScope::Once);
        assert!(j.consume(&record.id,&r.authority).unwrap());
        assert!(!j.consume(&record.id,&r.authority).unwrap());
    }
    let pending=j.create("pending-risk",&r,false).unwrap();
    drop(j);
    let j=Journal::open(&path).unwrap();j.recover().unwrap();
    assert_eq!(j.get(&pending.id).unwrap().unwrap().state,ApprovalState::Interrupted);
    assert!(!j.consume(&pending.id,&r.authority).unwrap());
}
