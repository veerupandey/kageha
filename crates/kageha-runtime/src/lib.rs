//! Reusable turn, planning, dispatch and journal reduction kernel.
pub mod discovery;
pub mod dispatch;
pub mod fanout;
pub mod job_dependencies;
pub mod job_scheduling;
pub mod model_retry;
pub mod model_stream;
pub mod recovery;
pub mod plan;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
pub mod turn;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

type Object = Map<String, Value>;

#[derive(Deserialize)]
pub struct Event {
    pub session_id: String,
    pub turn_id: String,
    pub sequence: i64,
    pub kind: String,
    pub payload: Object,
    pub created_at: f64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    #[default]
    Accepted,
    Planning,
    Executing,
    WaitingApproval,
    AwaitingInput,
    WaitingDependencies,
    Verifying,
    Repairing,
    Blocked,
    Cancelled,
    Failed,
    Completed,
}

impl Phase {
    pub fn terminal(self) -> bool {
        matches!(
            self,
            Self::AwaitingInput | Self::Blocked | Self::Cancelled | Self::Failed | Self::Completed
        )
    }
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Snapshot {
    pub session_id: String,
    pub turn_id: String,
    pub objective: String,
    pub phase: Phase,
    pub status: String,
    pub plan_version: i64,
    pub plan: Vec<Value>,
    pub goals: Vec<Value>,
    pub current_stage: String,
    pub steps_used: i64,
    pub max_steps: i64,
    pub usd_spent: f64,
    pub max_usd: f64,
    pub artifacts: Vec<Value>,
    pub pending_action: String,
    pub open_tool_attempts: Vec<String>,
    pub last_error: String,
    pub verification_status: String,
    pub validated: bool,
    pub updated_at: f64,
    pub last_sequence: i64,
    pub metadata: Object,
    pub task_state: Object,
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64() != Some(0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}

fn string(value: &Value, default: &str) -> String {
    if !truthy(value) {
        return default.into();
    }
    value
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| value.to_string())
}

fn integer(value: &Value) -> Result<i64, String> {
    value
        .as_i64()
        .or_else(|| value.as_bool().map(i64::from))
        .or_else(|| value.as_str()?.trim().parse().ok())
        .or_else(|| {
            value
                .as_f64()
                .filter(|n| *n >= i64::MIN as f64 && *n < -(i64::MIN as f64))
                .map(|n| n as i64)
        })
        .ok_or_else(|| "invalid journal integer".into())
}

fn number(value: &Value) -> Result<f64, String> {
    value
        .as_f64()
        .or_else(|| value.as_bool().map(|v| f64::from(u8::from(v))))
        .or_else(|| value.as_str()?.trim().parse().ok())
        .filter(|n| n.is_finite())
        .ok_or_else(|| "invalid journal number".into())
}

fn update_list(target: &mut Vec<Value>, value: &Value) -> Result<(), String> {
    if truthy(value) {
        *target = value
            .as_array()
            .ok_or("journal list must be an array")?
            .clone();
    }
    Ok(())
}

/// Reduce one event. Supply a replay clock for legacy events with zero timestamps.
/// The UI activity projection in metadata.agent_progress is owned by the host.
pub fn reduce(
    snapshot: &mut Option<Snapshot>,
    event: &Event,
    replay_now: f64,
) -> Result<(), String> {
    let payload = &event.payload;
    let get = |key: &str| payload.get(key).unwrap_or(&Value::Null);
    let Some(current) = snapshot else {
        if event.kind != "accepted" {
            return Err("the first event for a turn must be accepted".into());
        }
        let objective = string(get("objective"), "").trim().to_owned();
        if objective.is_empty() {
            return Err("accepted event requires an objective".into());
        }
        let mut metadata = get("metadata").as_object().cloned().unwrap_or_default();
        metadata.remove("agent_progress");
        *snapshot = Some(Snapshot {
            session_id: event.session_id.clone(),
            turn_id: event.turn_id.clone(),
            objective,
            status: "running".into(),
            max_steps: if truthy(get("max_steps")) {
                integer(get("max_steps"))?.max(1)
            } else {
                40
            },
            max_usd: payload
                .get("max_usd")
                .map(number)
                .transpose()?
                .unwrap_or(2.0),
            verification_status: "unknown".into(),
            updated_at: event.created_at,
            last_sequence: event.sequence,
            metadata,
            ..Snapshot::default()
        });
        return Ok(());
    };
    if current.session_id != event.session_id || current.turn_id != event.turn_id {
        return Err("event identity does not match snapshot".into());
    }
    if event.sequence <= current.last_sequence {
        return Ok(());
    }
    let expected = current
        .last_sequence
        .checked_add(1)
        .ok_or("journal sequence exhausted")?;
    if event.sequence != expected {
        return Err(format!(
            "event sequence gap: expected {expected}, received {}",
            event.sequence
        ));
    }
    if current.phase.terminal() {
        let phase = serde_json::to_value(current.phase).expect("serializable phase");
        return Err(format!(
            "cannot apply {} after terminal phase {}",
            event.kind,
            phase.as_str().unwrap()
        ));
    }

    // Keep the previous snapshot intact if validation of a payload fails.
    let mut next = current.clone();
    match event.kind.as_str() {
        "waiting_dependencies" => {
            next.phase = Phase::WaitingDependencies;
            next.status = "waiting_dependencies".into();
        }
        "planning_started" => next.phase = Phase::Planning,
        "planned" => {
            next.phase = Phase::Executing;
            let version = if truthy(get("version")) {
                integer(get("version"))?
            } else {
                1
            };
            next.plan_version = next
                .plan_version
                .checked_add(1)
                .ok_or("plan version exhausted")?
                .max(version);
            next.plan.clear();
            let plan = if truthy(get("plan")) {
                get("plan")
            } else {
                get("steps")
            };
            update_list(&mut next.plan, plan)?;
            update_list(&mut next.goals, get("goals"))?;
            next.current_stage = string(get("current_stage"), "");
        }
        "thinking_started" | "model_selected" | "stream_item" | "todo_board" => {}
        "tool_started" => {
            next.phase = Phase::Executing;
            let attempt = string(get("attempt_id"), "");
            if !attempt.is_empty() && !next.open_tool_attempts.contains(&attempt) {
                next.open_tool_attempts.push(attempt);
            }
            if truthy(get("step")) {
                next.steps_used = next.steps_used.max(integer(get("step"))?);
            }
        }
        "tool_completed" => {
            let attempt = string(get("attempt_id"), "");
            next.open_tool_attempts.retain(|id| *id != attempt);
            next.phase = Phase::Executing;
            if let Some(spent) = payload.get("usd_spent") {
                next.usd_spent = next.usd_spent.max(number(spent)?);
            }
        }
        "approval_required" => {
            next.phase = Phase::WaitingApproval;
            next.pending_action = string(get("action"), "approval");
        }
        "approval_resolved" => {
            next.phase = Phase::Executing;
            next.pending_action.clear();
            next.status = "running".into();
            next.last_error = if truthy(get("approved")) {
                String::new()
            } else {
                string(get("reason"), "approval denied")
            };
        }
        "verification_started" => next.phase = Phase::Verifying,
        "verification" => {
            next.verification_status = string(get("status"), "unknown");
            next.validated = payload
                .get("validated")
                .map(truthy)
                .unwrap_or(next.verification_status == "pass");
            update_list(&mut next.goals, get("goals"))?;
            update_list(&mut next.artifacts, get("artifacts"))?;
            next.phase = if next.validated {
                Phase::Executing
            } else {
                Phase::Repairing
            };
        }
        "repair" => {
            next.phase = Phase::Repairing;
            next.pending_action = string(get("action"), "repair");
        }
        "checkpoint" => next.pending_action.clear(),
        "task_state_committed" => {
            let state = get("state")
                .as_object()
                .ok_or("task state must be an object")?;
            let expected = payload
                .get("expected_revision")
                .map(integer)
                .transpose()?
                .unwrap_or(-1);
            let actual = next
                .task_state
                .get("revision")
                .map(integer)
                .transpose()?
                .unwrap_or(0);
            let revision = state.get("revision").unwrap_or(&Value::Null);
            let next_revision = actual.checked_add(1).ok_or("task revision exhausted")?;
            let matches_revision = if revision.is_f64() {
                revision.as_f64() == Some(next_revision as f64)
            } else {
                revision
                    .as_i64()
                    .or_else(|| revision.as_bool().map(i64::from))
                    == Some(next_revision)
            };
            if expected != actual || !matches_revision {
                return Err(format!(
                    "stale task revision: expected {actual}, received {expected}"
                ));
            }
            next.plan.clear();
            next.goals.clear();
            update_list(&mut next.plan, state.get("stages").unwrap_or(&Value::Null))?;
            update_list(&mut next.goals, state.get("goals").unwrap_or(&Value::Null))?;
            next.plan_version = match state.get("version").filter(|v| truthy(v)) {
                Some(version) => integer(version)?,
                None => 1,
            };
            next.current_stage = state
                .get("current_stage_id")
                .map(|v| string(v, ""))
                .unwrap_or_default();
            next.task_state = state.clone();
        }
        "progress" => {
            if truthy(get("step")) {
                next.steps_used = next.steps_used.max(integer(get("step"))?);
            }
            if truthy(get("usd_spent")) {
                next.usd_spent = next.usd_spent.max(number(get("usd_spent"))?);
            }
            next.current_stage = string(get("current_stage"), &next.current_stage);
        }
        "awaiting_input" => {
            next.phase = Phase::AwaitingInput;
            next.status = string(get("status"), "ask_user");
            next.last_error.clear();
        }
        "blocked" => {
            next.phase = Phase::Blocked;
            next.status = string(get("status"), "blocked");
            next.last_error = string(get("reason"), "");
        }
        "cancelled" => {
            next.phase = Phase::Cancelled;
            next.status = "cancelled".into();
        }
        "completed" => {
            next.validated = payload
                .get("validated")
                .map(truthy)
                .unwrap_or(next.validated);
            let execution_complete = get("executor_finished").as_bool() == Some(true)
                && get("checks_passed").as_bool() == Some(true);
            if !next.validated && !execution_complete {
                return Err(
                    "completed event requires executor completion and applicable checks".into(),
                );
            }
            next.phase = Phase::Completed;
            next.status = "success".into();
            next.verification_status = if next.validated {
                "pass"
            } else {
                "not_required"
            }
            .into();
            update_list(&mut next.artifacts, get("artifacts"))?;
            next.open_tool_attempts.clear();
        }
        "failed" => {
            next.phase = Phase::Failed;
            next.status = string(get("status"), "error");
            next.last_error = string(get("error"), &string(get("reason"), ""));
        }
        kind => return Err(format!("unsupported event kind: {kind}")),
    }
    next.updated_at = if event.created_at == 0.0 {
        replay_now
    } else {
        event.created_at
    };
    next.last_sequence = event.sequence;
    *current = next;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(sequence: i64, kind: &str, payload: Value) -> Event {
        serde_json::from_value(json!({
            "session_id": "session",
            "turn_id": "turn",
            "sequence": sequence,
            "kind": kind,
            "payload": payload,
            "created_at": 0.0,
        }))
        .unwrap()
    }

    #[test]
    fn rejected_events_leave_the_journal_cursor_and_attempts_intact() {
        let mut snapshot = None;
        reduce(
            &mut snapshot,
            &event(1, "accepted", json!({"objective": "Verify work"})),
            10.0,
        )
        .unwrap();
        reduce(
            &mut snapshot,
            &event(2, "tool_started", json!({"attempt_id": "tool-a"})),
            11.0,
        )
        .unwrap();

        let before = serde_json::to_value(&snapshot).unwrap();
        assert!(reduce(&mut snapshot, &event(4, "checkpoint", json!({})), 12.0).is_err());
        assert!(reduce(&mut snapshot, &event(3, "completed", json!({})), 12.0).is_err());
        assert!(reduce(
            &mut snapshot,
            &event(
                3,
                "task_state_committed",
                json!({
                    "expected_revision": 1, "state": {"revision": 2}
                })
            ),
            12.0
        )
        .is_err());
        reduce(
            &mut snapshot,
            &event(2, "tool_started", json!({"attempt_id": "duplicate"})),
            12.0,
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&snapshot).unwrap(), before);

        reduce(
            &mut snapshot,
            &event(
                3,
                "completed",
                json!({
                    "executor_finished": true, "checks_passed": true
                }),
            ),
            12.0,
        )
        .unwrap();
        assert!(reduce(&mut snapshot, &event(4, "checkpoint", json!({})), 13.0).is_err());
        let completed = snapshot.unwrap();
        assert!(completed.open_tool_attempts.is_empty());
        assert_eq!(completed.verification_status, "not_required");
        assert_eq!(completed.updated_at, 12.0);
    }
}

pub mod lifecycle;

pub mod execution;
pub mod review_context;
