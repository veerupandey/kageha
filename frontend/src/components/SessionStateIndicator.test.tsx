import { cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { useAppStore } from "../store";
import { emptyRun } from "../store/helpers";
import { SessionStateIndicator } from "./SessionStateIndicator";

vi.mock("../lib/sessionTruth", () => ({
  refreshSessionTruth: vi.fn(),
  useSessionTruth: () => ({ status: "current", checkedAt: 1, data: {
    selected_target: "machine", target_conflict: false, turn: null,
  } }),
}));
const original = useAppStore.getState();
beforeEach(() => useAppStore.setState({ sessionId: "fixture", machineActive: false, remoteActive: true,
  remoteHostId: "stale-host", executionTargetPending: false, runs: { fixture: emptyRun("fixture") } }));
afterEach(() => { cleanup(); useAppStore.setState(original); });

it("reconciles top-level controls and the cached run with confirmed backend selection", () => {
  render(<SessionStateIndicator sessionId="fixture" />);
  const state = useAppStore.getState();
  expect(state.machineActive).toBe(true);
  expect(state.remoteActive).toBe(false);
  expect(state.remoteHostId).toBe("");
  expect(state.runs.fixture.machineActive).toBe(true);
});

it("does not overwrite an in-flight user selection or another chat", () => {
  useAppStore.setState({ executionTargetPending: true });
  const view = render(<SessionStateIndicator sessionId="fixture" />);
  expect(useAppStore.getState().machineActive).toBe(false);
  view.unmount();
  useAppStore.setState({ executionTargetPending: false, sessionId: "another-chat" });
  render(<SessionStateIndicator sessionId="fixture" />);
  expect(useAppStore.getState().machineActive).toBe(false);
});
