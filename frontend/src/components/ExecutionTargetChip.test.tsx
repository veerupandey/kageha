import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { ExecutionTargetChip } from "./ExecutionTargetChip";

const state = vi.hoisted(() => ({
  machineActive: false, machineBlocked: false, remoteActive: false,
  remoteHostId: "", remoteHostName: "", remoteHealth: "", remoteRiskTier: "",
  sending: false, sessionId: "fixture", runs: {} as Record<string, { status: string }>,
  setMachineActive: vi.fn(), setRemoteActive: vi.fn(), openSettings: vi.fn(),
}));

vi.mock("../store", () => ({ useAppStore: (select: (value: typeof state) => unknown) => select(state) }));
vi.mock("../lib/sessionTruth", () => ({ useSessionTruth: () => ({ status: "current", data: {
  selected_target: state.machineActive ? "machine" : state.remoteActive ? "remote" : "local", target_conflict: false,
} }) }));
afterEach(cleanup);
beforeEach(() => {
  state.machineActive = false;
  state.remoteActive = false;
  state.sending = false;
  state.runs = {};
});

it("shows the selected host in the compact settings control", () => {
  render(<ExecutionTargetChip compact />);
  expect(screen.getByRole("button", { name: "Execution target" }).textContent).toContain("This host");
});

it("shows the Linux location when that machine is selected", () => {
  state.machineActive = true;
  render(<ExecutionTargetChip compact />);
  expect(screen.getByRole("button", { name: "Execution target" }).textContent).toContain("Fresh Linux box");
});

it("prevents changing location while an attached run is executing", () => {
  state.runs.fixture = { status: "running" };
  render(<ExecutionTargetChip />);
  const button = screen.getByRole("button", { name: "Execution target" }) as HTMLButtonElement;
  expect(button.disabled).toBe(true);
  expect(button.title).toContain("Stop the current run");
});
