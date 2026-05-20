//! Shared policy decisions; persistence and user interaction remain adapter ports.
pub mod classes;
pub mod specialist;
use kageha_contracts::{execution::Control, policy::*};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io;
pub struct DefaultPolicy;
impl PolicyPort for DefaultPolicy {
    fn evaluate(
        &self,
        request: &ApprovalRequest,
        mode: PermissionMode,
    ) -> io::Result<Authorization> {
        request.authority.validate()?;
        if request.action.is_empty()
            || request.action.len() > 256
            || request.action.chars().any(char::is_control)
            || request.detail.len() > 512 * 1024
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid authorization request",
            ));
        }
        Ok(match request.class {
            ActionClass::ReadOnly => Authorization::Allow,
            ActionClass::RoutineFill if mode != PermissionMode::Ask => Authorization::Allow,
            _ => Authorization::RequireApproval,
        })
    }
}
pub fn input_digest(value: &Value) -> String {
    format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
}
pub fn resolved_scope(class: ActionClass, requested: GrantScope, approved: bool) -> GrantScope {
    if !approved || matches!(class, ActionClass::Plan | ActionClass::Restricted) {
        GrantScope::Once
    } else {
        requested
    }
}
/// Common enforcement used by both hosts. It never dispatches tool input itself.
pub fn authorize(
    policy: &dyn PolicyPort,
    approvals: Option<&dyn ApprovalPort>,
    request: &ApprovalRequest,
    mode: PermissionMode,
    control: &Control<'_>,
) -> io::Result<bool> {
    control.check()?;
    let allowed = match policy.evaluate(request, mode)? {
        Authorization::Allow => true,
        Authorization::Deny => false,
        Authorization::RequireApproval => match approvals {
            Some(adapter) => adapter.authorize_once(request, control)?,
            None => false,
        },
    };
    control.check()?;
    Ok(allowed)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::Cell,
        sync::atomic::{AtomicBool, Ordering},
        time::{Duration, Instant},
    };
    fn request(class: ActionClass) -> ApprovalRequest {
        ApprovalRequest {
            authority: Authority {
                session: "session".into(),
                turn: "turn".into(),
                binding: "browser.workspace".into(),
                generation: 1,
                resource: "profile/tab/destination".into(),
                input_digest: input_digest(&serde_json::json!({"file_hash":"exact"})),
            },
            action: "operation".into(),
            class,
            detail: "Exact operation".into(),
        }
    }
    #[test]
    fn routine_and_full_modes_never_auto_approve_mutations_uploads_or_plans() {
        for mode in [
            PermissionMode::Ask,
            PermissionMode::Routine,
            PermissionMode::Full,
        ] {
            for class in [
                ActionClass::Restricted,
                ActionClass::Mutation,
                ActionClass::Upload,
                ActionClass::Plan,
            ] {
                assert_eq!(
                    DefaultPolicy.evaluate(&request(class), mode).unwrap(),
                    Authorization::RequireApproval
                );
            }
        }
    }
    #[test]
    fn read_and_fill_policy_preserves_existing_routine_grants() {
        assert_eq!(
            DefaultPolicy
                .evaluate(&request(ActionClass::ReadOnly), PermissionMode::Ask)
                .unwrap(),
            Authorization::Allow
        );
        assert_eq!(
            DefaultPolicy
                .evaluate(&request(ActionClass::RoutineFill), PermissionMode::Ask)
                .unwrap(),
            Authorization::RequireApproval
        );
        assert_eq!(
            DefaultPolicy
                .evaluate(&request(ActionClass::RoutineFill), PermissionMode::Routine)
                .unwrap(),
            Authorization::Allow
        );
    }
    #[test]
    fn authority_is_bound_to_session_turn_profile_generation_destination_and_input() {
        let original = request(ActionClass::Upload).authority;
        for n in 0..6 {
            let mut changed = original.clone();
            match n {
                0 => changed.session = "other".into(),
                1 => changed.turn = "other".into(),
                2 => changed.binding = "other-profile".into(),
                3 => changed.generation += 1,
                4 => changed.resource = "other-destination".into(),
                _ => {
                    changed.input_digest =
                        input_digest(&serde_json::json!({"file_hash":"different"}))
                }
            };
            assert!(original.verify_current(&changed).is_err());
        }
        assert!(original.verify_current(&original).is_ok());
    }
    #[test]
    fn plan_and_denied_decisions_cannot_broaden_grants() {
        assert_eq!(
            resolved_scope(ActionClass::Plan, GrantScope::Full, true),
            GrantScope::Once
        );
        assert_eq!(
            resolved_scope(ActionClass::Mutation, GrantScope::Full, false),
            GrantScope::Once
        );
    }
    struct Approval<'a> {
        calls: Cell<usize>,
        cancel: Option<&'a AtomicBool>,
    }
    impl ApprovalPort for Approval<'_> {
        fn authorize_once(&self, _: &ApprovalRequest, _: &Control<'_>) -> io::Result<bool> {
            self.calls.set(self.calls.get() + 1);
            if let Some(cancel) = self.cancel {
                cancel.store(true, Ordering::Release);
            }
            Ok(true)
        }
    }
    #[test]
    fn approval_is_required_and_an_adapter_can_be_replaced_without_consumer_changes() {
        let cancel = AtomicBool::new(false);
        let control = Control {
            cancelled: &cancel,
            deadline: None,
        };
        let req = request(ActionClass::Upload);
        assert!(!authorize(&DefaultPolicy, None, &req, PermissionMode::Full, &control).unwrap());
        for _ in 0..2 {
            let approval = Approval {
                calls: Cell::new(0),
                cancel: None,
            };
            assert!(authorize(
                &DefaultPolicy,
                Some(&approval),
                &req,
                PermissionMode::Ask,
                &control
            )
            .unwrap());
            assert_eq!(approval.calls.get(), 1);
        }
    }
    #[test]
    fn cancelled_or_expired_calls_cannot_consume_approvals() {
        let cancel = AtomicBool::new(true);
        let approval = Approval {
            calls: Cell::new(0),
            cancel: None,
        };
        assert!(authorize(
            &DefaultPolicy,
            Some(&approval),
            &request(ActionClass::Upload),
            PermissionMode::Ask,
            &Control {
                cancelled: &cancel,
                deadline: None
            }
        )
        .is_err());
        cancel.store(false, Ordering::Release);
        assert!(authorize(
            &DefaultPolicy,
            Some(&approval),
            &request(ActionClass::Upload),
            PermissionMode::Ask,
            &Control {
                cancelled: &cancel,
                deadline: Some(Instant::now() - Duration::from_millis(1))
            }
        )
        .is_err());
        assert_eq!(approval.calls.get(), 0);
    }
    #[test]
    fn cancellation_wins_over_a_late_approval() {
        let cancel = AtomicBool::new(false);
        let approval = Approval {
            calls: Cell::new(0),
            cancel: Some(&cancel),
        };
        assert!(authorize(
            &DefaultPolicy,
            Some(&approval),
            &request(ActionClass::Upload),
            PermissionMode::Ask,
            &Control {
                cancelled: &cancel,
                deadline: None
            }
        )
        .is_err());
    }
}

pub mod journal;

/// Operator-selected tools can require consent even when they are read-only.
pub struct RequireToolsPolicy {
    required: std::collections::BTreeSet<String>,
}
impl RequireToolsPolicy {
    pub fn new(tools: Vec<String>) -> io::Result<Self> {
        if tools.len() > 256
            || tools
                .iter()
                .any(|t| t.is_empty() || t.len() > 256 || t.chars().any(char::is_control))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid approval tool configuration",
            ));
        }
        Ok(Self {
            required: tools.into_iter().collect(),
        })
    }
}
impl PolicyPort for RequireToolsPolicy {
    fn evaluate(
        &self,
        request: &ApprovalRequest,
        mode: PermissionMode,
    ) -> io::Result<Authorization> {
        let base = DefaultPolicy.evaluate(request, mode)?;
        Ok(if self.required.contains(&request.action) {
            Authorization::RequireApproval
        } else {
            base
        })
    }
}

pub mod requirements;
