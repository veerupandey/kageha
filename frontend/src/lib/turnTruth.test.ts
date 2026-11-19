import { describe, expect, it } from "vitest";
import { serverTurnFromReplay, serverTurnFromSession, staleLocalRun, turnIndicator } from "./turnTruth";
import { loopEventDetail } from "./loopEvents";

const notRunning = { turnId: "", status: "success", running: false, observedAt: 2_000 };
const running = { turnId: "t1", status: "running", running: true, observedAt: 2_000 };

describe("turnIndicator (Requirement 18.4)", () => {
  it("never shows Running for a stale local flag once the server reports no running turn", () => {
    const stale = { status: "running", sending: true, waitingApproval: false, startedAt: 1_000, turnId: "t1" };
    expect(turnIndicator(notRunning, stale)).toBe("success");
    // A restored run without a local send: the server report is authoritative.
    expect(turnIndicator(notRunning, { status: "running", sending: true, waitingApproval: false })).toBe("success");
  });

  it("shows Running only from the server, and Connecting… for an unconfirmed local send", () => {
    const sent = { status: "running", sending: true, waitingApproval: false, startedAt: 3_000 };
    expect(turnIndicator(running, sent)).toBe("connecting");
    expect(turnIndicator({ ...notRunning, observedAt: 4_000 }, sent)).toBe("connecting");
    expect(turnIndicator({ ...running, observedAt: 4_000 }, sent)).toBe("running");
    expect(turnIndicator(running, null)).toBe("running");
  });

  it("treats waiting_for_unlock as running", () => {
    const report = serverTurnFromSession({ active_turn: { turn_id: "t1", status: "waiting_for_unlock" }, status: "waiting_for_unlock" }, 5);
    expect(report).toMatchObject({ running: true, status: "waiting_for_unlock" });
    expect(turnIndicator(report, null)).toBe("running");
    expect(turnIndicator(serverTurnFromReplay({ turn_id: "t1", status: "waiting_for_unlock", running: true }, 5), null)).toBe("running");
  });

  it("lets a local Stop settle a row the server still reports running", () => {
    expect(turnIndicator(running, { status: "cancelled", sending: false, waitingApproval: false })).toBe("cancelled");
  });
});

describe("staleLocalRun", () => {
  const busy = { status: "running", sending: true, waitingApproval: false, startedAt: 1_000, turnId: "t1" };
  it("settles an admitted run only on a newer report about the same or no turn", () => {
    expect(staleLocalRun(notRunning, busy)).toBe(true);
    expect(staleLocalRun({ ...notRunning, observedAt: 500 }, busy)).toBe(false);
    expect(staleLocalRun({ ...notRunning, turnId: "t0" }, busy)).toBe(false);
    expect(staleLocalRun(notRunning, { ...busy, turnId: undefined })).toBe(false);
    expect(staleLocalRun(running, busy)).toBe(false);
  });
});

describe("loopEventDetail", () => {
  it("adds one compact line for loop events and nothing for other kinds", () => {
    expect(loopEventDetail("completion_check", { status: "stopped", stop_reason: "no_progress", unmet: ["report"] }))
      .toEqual(["stopped · stop: no_progress · unmet: report"]);
    expect(loopEventDetail("escalation", { kind: "escalation", payload: { from: "retry", to: "replan", stale_steps: 3 } }))
      .toEqual(["retry → replan · 3 steps without progress"]);
    expect(loopEventDetail("model_fallback", { role: "executor", from: "a", to: "b", cause: "rate_limit" }))
      .toEqual(["executor · a → b · rate_limit"]);
    expect(loopEventDetail("tool_started", { name: "bash" })).toEqual([]);
  });
});

it("shows a newer backend turn as running after this tab finished an earlier turn", () => {
  const server = { turnId: "new", status: "running", running: true, observedAt: 100 };
  const run = { turnId: "old", status: "success", sending: false, waitingApproval: false };
  expect(turnIndicator(server, run)).toBe("running");
  expect(turnIndicator({ ...server, turnId: "old" }, run)).toBe("success");
});
