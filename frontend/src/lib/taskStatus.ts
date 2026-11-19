import type { TaskControlState } from "../store/types";

const KNOWN_CONTROL_STATES: ReadonlySet<string> = new Set<TaskControlState>([
  "continue",
  "retry",
  "switch_tool",
  "replan_stage",
  "replan_task",
  "delegate",
  "ask_user",
  "repair",
  "huddle",
  "advance",
  "stop_success",
]);

/** Control states that mean the loop is actively stuck / needs attention. */
export const BLOCKED_TASK_CONTROLS: ReadonlySet<TaskControlState> = new Set([
  "huddle",
  "ask_user",
  "replan_task",
]);

/** Control states that are a normal, expected mid-task correction (not "blocked"). */
export const REPAIRING_TASK_CONTROLS: ReadonlySet<TaskControlState> = new Set([
  "retry",
  "switch_tool",
  "replan_stage",
  "repair",
]);

export function taskControlFromPayload(value: unknown): TaskControlState {
  const raw = String(value || "");
  return KNOWN_CONTROL_STATES.has(raw) ? (raw as TaskControlState) : "";
}

/** Maps the backend `task_status` event payload (loop/controller.py) to store fields. */
export function taskStatusFromPayload(payload: Record<string, unknown> | undefined): {
  taskControl: TaskControlState;
  taskControlReason: string;
  taskFailureKind: string;
  taskFailureChange: string;
  taskFailureCount: number;
  taskStageId: string;
  taskStageStatus: string;
} {
  return {
    taskControl: taskControlFromPayload(payload?.control),
    taskControlReason: String(payload?.control_reason || ""),
    taskFailureKind: String(payload?.failure_kind || ""),
    taskFailureChange: String(payload?.failure_change || ""),
    taskFailureCount: Number(payload?.failure_count || 0),
    taskStageId: String(payload?.stage_id || ""),
    taskStageStatus: String(payload?.stage_status || ""),
  };
}

/** Human labels for loop/task_state.py FailureKind values — kept in one place. */
const FAILURE_KIND_LABELS: Record<string, string> = {
  invalid_args: "Invalid arguments",
  missing_dependency: "Missing dependency",
  policy_denied: "Blocked by harness policy",
  provider_error: "Model/provider error",
  bad_output: "Bad output",
  timeout: "Timed out",
  reasoning_error: "Reasoning error",
  tool_error: "Tool error",
  access_blocked: "Access blocked",
  unknown: "Unknown failure",
};

export function failureKindLabel(kind: string): string {
  return FAILURE_KIND_LABELS[kind] || (kind ? kind.replace(/_/g, " ") : "");
}

/** Human labels for loop/adaptive.py ControlDecision values. */
const CONTROL_LABELS: Record<TaskControlState, string> = {
  "": "",
  continue: "Working",
  retry: "Retrying",
  switch_tool: "Switching approach",
  replan_stage: "Replanning stage",
  replan_task: "Replanning task",
  delegate: "Delegating",
  ask_user: "Needs your input",
  repair: "Repairing",
  huddle: "Diagnosing a blocker",
  advance: "Advancing",
  stop_success: "Done",
};

export function taskControlLabel(control: TaskControlState): string {
  return CONTROL_LABELS[control] || control;
}
