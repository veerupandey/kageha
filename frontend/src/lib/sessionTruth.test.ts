import { afterEach, expect, it, vi } from "vitest";
vi.mock("../api/client", () => ({ api: vi.fn() }));
import { api } from "../api/client";
import { SessionTruthConnection, validateSessionTruth, type SessionTruth } from "./sessionTruth";
import { sessionStateLabel } from "./sessionStateLabel";
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { createElement, StrictMode } from "react";
import { useSessionTruth, refreshSessionTruth } from "./sessionTruth";

const data = (revision = 1): SessionTruth => ({ schema_version: 1, session_id: "fixture", observed_at: 100,
  revision, selected_target: "local", target_conflict: false, turn: { id: "turn", status: "running",
    phase: "executing", updated_at: 99, steps_done: 6, steps_total: 6, failed_calls: 2,
    actual_target: "machine", actual_target_source: "tool_receipt" } });
afterEach(() => { cleanup(); vi.clearAllMocks(); vi.useRealTimers(); });

it("keeps explicit refresh connected after StrictMode unsubscribe/resubscribe", async () => {
  vi.mocked(api).mockResolvedValue(data());
  const hook = renderHook(() => useSessionTruth("fixture"), {
    wrapper: ({ children }) => createElement(StrictMode, null, children),
  });
  await waitFor(() => expect(hook.result.current.status).toBe("current"));
  const before = vi.mocked(api).mock.calls.length;
  await act(async () => refreshSessionTruth("fixture"));
  await waitFor(() => expect(vi.mocked(api).mock.calls.length).toBeGreaterThan(before));
  await waitFor(() => expect(hook.result.current.status).toBe("current"));
});

it("does not claim completion while terminal execution cleanup is pending", () => {
  const snapshot = data();
  snapshot.turn!.status = "cleanup_pending";
  snapshot.turn!.phase = "repairing";
  snapshot.turn!.actual_target = "local";
  expect(sessionStateLabel({ data: snapshot, status: "current", checkedAt: Date.now() }))
    .toContain("Finishing execution cleanup");
});

it.each(["success", "complete", "completed"])("labels %s with open steps as a finished response, not Done", status => {
  const snapshot = data();
  Object.assign(snapshot.turn!, { status, phase: "completed", steps_done: 0, steps_total: 4 });
  const label = sessionStateLabel({ data: snapshot, status: "current", checkedAt: 100 });
  expect(label).toContain("Response finished · plan incomplete");
  expect(label).toContain("0/4 steps");
  expect(label).not.toContain("Done");
});

it("keeps completed execution separate from unresolved outcomes", () => {
  const snapshot = data();
  Object.assign(snapshot.turn!, { status: "success", phase: "completed", outcomes_pending: 1 });
  expect(sessionStateLabel({ data: snapshot, status: "current", checkedAt: 100 }))
    .toContain("Response finished · verification pending");
});

it("reports superseded work separately without claiming it was executed", () => {
  const snapshot = data();
  Object.assign(snapshot.turn!, { status: "success", phase: "completed", steps_done: 0,
    steps_total: 0, steps_superseded: 4, outcomes_pending: 0 });
  const label = sessionStateLabel({ data: snapshot, status: "current", checkedAt: 100 });
  expect(label).toContain("Done");
  expect(label).toContain("4 superseded");
  expect(label).not.toContain("0/4 steps");
});

it("shows unresolved verification as unresolved rather than generic interruption", () => {
  const snapshot = data();
  Object.assign(snapshot.turn!, { status: "unverified", phase: "completed" });
  expect(sessionStateLabel({ data: snapshot, status: "current", checkedAt: 100 }))
    .toContain("Verification unresolved");
});

it.each([-1, NaN, 0.5])("rejects malformed completion counts: %s", count => {
  for (const field of ["steps_superseded", "outcomes_pending"] as const) {
    const snapshot = data();
    snapshot.turn![field] = count;
    expect(() => validateSessionTruth(snapshot, "fixture")).toThrow("Incomplete backend state");
  }
});

it("reports an expired tool as stalled even while backend contact is current", () => {
  const snapshot = data();
  snapshot.turn!.status = "stalled";
  snapshot.turn!.phase = "repairing";
  expect(sessionStateLabel({ data: snapshot, status: "current", checkedAt: Date.now() }))
    .toContain("Stalled · recovery pending");
});

it("uses the approval phase even when the runtime status remains running", () => {
  const snapshot = data();
  snapshot.turn!.status = "running";
  snapshot.turn!.phase = "waiting_approval";
  expect(sessionStateLabel({ data: snapshot, status: "current", checkedAt: Date.now() }))
    .toContain("Approval needed");
});

it("identifies a real resource wait instead of reporting generic work", () => {
  const snapshot = data();
  snapshot.turn!.status = "waiting_resource";
  snapshot.turn!.resource_wait = { resource_kind: "file", owner_session_id: "blocking-chat", started_at: 90 };
  expect(sessionStateLabel({ data: snapshot, status: "current", checkedAt: Date.now() }))
    .toContain("Waiting for shared file · chat blocking");
});

it("reports a location mismatch and does not equate completed steps with task completion", () => {
  const label = sessionStateLabel({ status: "current", data: data(), checkedAt: 100 });
  expect(label).toContain("Working");
  expect(label).toContain("Linux box");
  expect(label).toContain("selection differs");
  expect(label).not.toContain("Done");
});

it("marks failures stale and keeps the last confirmed snapshot", async () => {
  vi.mocked(api).mockResolvedValueOnce(data()).mockRejectedValueOnce(new Error("offline"));
  const connection = new SessionTruthConnection("fixture");
  const stop = connection.subscribe(() => {});
  await vi.waitFor(() => expect(connection.snapshot.status).toBe("current"));
  await connection.load();
  expect(connection.snapshot.status).toBe("stale");
  expect(connection.snapshot.data?.revision).toBe(1);
  expect(sessionStateLabel(connection.snapshot)).toContain("stale");
  stop();
});

it("rejects wrong-session, incomplete and older responses", async () => {
  vi.mocked(api).mockResolvedValueOnce(data(5)).mockResolvedValueOnce(data(4))
    .mockResolvedValueOnce({ ...data(6), session_id: "another-chat" }).mockResolvedValueOnce({});
  const connection = new SessionTruthConnection("fixture");
  const stop = connection.subscribe(() => {});
  await vi.waitFor(() => expect(connection.snapshot.status).toBe("current"));
  for (let i = 0; i < 3; i++) {
    await connection.load();
    expect(connection.snapshot.status).toBe("stale");
    expect(connection.snapshot.data?.revision).toBe(5);
  }
  stop();
});

it("ignores a late response after the last subscriber leaves", async () => {
  let resolve!: (value: SessionTruth) => void;
  vi.mocked(api).mockImplementationOnce(() => new Promise(r => { resolve = r; }));
  const connection = new SessionTruthConnection("fixture");
  const stop = connection.subscribe(() => {});
  stop();
  resolve(data());
  await Promise.resolve(); await Promise.resolve();
  expect(connection.snapshot.data).toBeNull();
});

it("expires confirmation without treating an open connection as progress", async () => {
  vi.useFakeTimers();
  vi.mocked(api).mockResolvedValueOnce(data()).mockImplementation(() => new Promise(() => {}));
  const connection = new SessionTruthConnection("fixture");
  const stop = connection.subscribe(() => {});
  await vi.advanceTimersByTimeAsync(1);
  expect(connection.snapshot.status).toBe("current");
  await vi.advanceTimersByTimeAsync(12_001);
  expect(connection.snapshot.status).toBe("stale");
  stop();
});

it("shares one in-flight request across subscribers and marks offline data stale", async () => {
  vi.mocked(api).mockResolvedValue(data());
  const connection = new SessionTruthConnection("fixture");
  const first = connection.subscribe(() => {});
  const second = connection.subscribe(() => {});
  await vi.waitFor(() => expect(connection.snapshot.status).toBe("current"));
  expect(api).toHaveBeenCalledTimes(1);
  window.dispatchEvent(new Event("offline"));
  expect(connection.snapshot.status).toBe("stale");
  first(); second();
});

it("reports fresh backend contact separately from old task progress", () => {
  const snapshot = data();
  snapshot.turn!.actual_target = "local";
  snapshot.turn!.phase = "waiting_dependencies";
  snapshot.turn!.status = "waiting_dependencies";
  expect(sessionStateLabel({ data: snapshot, status: "current", checkedAt: Date.now() }))
    .toContain("Waiting for background work");
});
