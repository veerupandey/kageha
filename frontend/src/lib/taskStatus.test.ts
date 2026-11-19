import { describe, expect, it } from "vitest";
import {
  BLOCKED_TASK_CONTROLS,
  REPAIRING_TASK_CONTROLS,
  failureKindLabel,
  taskControlFromPayload,
  taskControlLabel,
  taskStatusFromPayload,
} from "./taskStatus";

describe("taskStatusFromPayload", () => {
  it("maps a backend task_status event (loop/controller.py) into store fields", () => {
    expect(
      taskStatusFromPayload({
        control: "huddle",
        control_reason: "tool_error/access_blocked failed twice — sign in or provide access",
        failure_kind: "access_blocked",
        failure_change: "Use an available logged-in browser; otherwise ask the user to sign in",
        failure_count: 2,
        stage_id: "s2",
        stage_status: "blocked",
      }),
    ).toEqual({
      taskControl: "huddle",
      taskControlReason: "tool_error/access_blocked failed twice — sign in or provide access",
      taskFailureKind: "access_blocked",
      taskFailureChange: "Use an available logged-in browser; otherwise ask the user to sign in",
      taskFailureCount: 2,
      taskStageId: "s2",
      taskStageStatus: "blocked",
    });
  });

  it("defaults to empty/zero fields for an undefined or empty payload", () => {
    expect(taskStatusFromPayload(undefined)).toEqual({
      taskControl: "",
      taskControlReason: "",
      taskFailureKind: "",
      taskFailureChange: "",
      taskFailureCount: 0,
      taskStageId: "",
      taskStageStatus: "",
    });
  });

  it("drops an unrecognized control value rather than passing it through untyped", () => {
    expect(taskControlFromPayload("some_future_decision")).toBe("");
    expect(taskControlFromPayload("repair")).toBe("repair");
  });
});

describe("task control/failure labels", () => {
  it("labels every known FailureKind (loop/task_state.py) distinctly from 'unknown'", () => {
    expect(failureKindLabel("access_blocked")).toBe("Access blocked");
    expect(failureKindLabel("missing_dependency")).toBe("Missing dependency");
    expect(failureKindLabel("policy_denied")).toBe("Blocked by harness policy");
    expect(failureKindLabel("")).toBe("");
  });

  it("labels every known ControlDecision (loop/adaptive.py)", () => {
    expect(taskControlLabel("huddle")).toBe("Diagnosing a blocker");
    expect(taskControlLabel("ask_user")).toBe("Needs your input");
    expect(taskControlLabel("stop_success")).toBe("Done");
  });

  it("classifies blocked vs repairing control states without overlap", () => {
    for (const state of BLOCKED_TASK_CONTROLS) {
      expect(REPAIRING_TASK_CONTROLS.has(state)).toBe(false);
    }
  });
});
