//! Resolve original operator intent rather than treating delegated agent instructions as user authority.
use crate::lifecycle::{RunRecord, RunStore};
use kageha_contracts::{ErrorCode, Result, ServiceError};
use serde_json::json;
pub fn user_intent(store: &dyn RunStore, run: &RunRecord) -> Result<String> {
    let mut current = run.clone();
    let mut delegated = Vec::new();
    let mut seen = std::collections::HashSet::new();
    loop {
        if !seen.insert(current.spec.id.clone()) || seen.len() > 32 {
            return Err(ServiceError::new(
                ErrorCode::Invalid,
                "Invalid review ancestry",
            ));
        }
        let Some(parent_id) = &current.spec.parent_run else {
            if current.spec.parent.is_some()
                && current.spec.input["configuration"]["delegation_source"] != "operator"
            {
                return Err(ServiceError::new(
                    ErrorCode::Unauthorized,
                    "Original user authority for delegated task is unavailable",
                ));
            }
            break;
        };
        delegated.push(current.spec.objective.clone());
        let parent = store.get(parent_id)?.ok_or_else(|| {
            ServiceError::new(ErrorCode::Invalid, "Original user request unavailable")
        })?;
        if current.spec.parent.as_ref() != Some(&parent.spec.session)
            || current.spec.depth != parent.spec.depth + 1
        {
            return Err(ServiceError::new(
                ErrorCode::Unauthorized,
                "Review ancestry scope mismatch",
            ));
        }
        current = parent;
    }
    let steering = store
        .steering_history(&current.spec.id)?
        .into_iter()
        .map(|s| s.body)
        .collect::<Vec<_>>();
    let value=json!({"user_objective":current.spec.objective,"user_steering":steering,"delegated_tasks_not_authorization":delegated}).to_string();
    if value.len() > 32768 {
        return Err(ServiceError::new(
            ErrorCode::Invalid,
            "Review context exceeds limit",
        ));
    }
    Ok(value)
}
