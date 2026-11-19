import "@testing-library/jest-dom/vitest";
import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { useAppStore } from "../../store";
import { WorkspaceTitleBar } from "./WorkspaceTitleBar";

const props = {
  surface: "live" as const,
  tabs: [{ id: "live" as const, label: "Browser" }, { id: "monitor" as const, label: "Activity" }],
  artifactCount: 0, machineActive: false, liveSurface: "browser" as const,
  expanded: false, onSelect: vi.fn(), onLiveSurface: vi.fn(), onZoom: vi.fn(),
};

beforeEach(() => useAppStore.setState({ runStatus: "running", statusLabel: "Waiting for model…", machineShellRunning: false, messages: [] }));
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

it("shows one agent status in chat, while retaining the capsule as a fallback", () => {
  useAppStore.setState({ messages: [{ id: "live", role: "assistant", text: "", streaming: true, statusLabel: "Waiting for model…" }] });
  const view = render(<WorkspaceTitleBar {...props} />);
  expect(screen.queryByRole("button", { name: /Activity status:/ })).not.toBeInTheDocument();

  view.rerender(<WorkspaceTitleBar {...props} expanded />);
  expect(screen.getByRole("button", { name: /Activity status: Agent Waiting for model/ })).toBeVisible();

  view.rerender(<WorkspaceTitleBar {...props} />);
  act(() => useAppStore.setState({ messages: [{ id: "live", role: "assistant", text: "", streaming: true }] }));
  expect(screen.getByRole("button", { name: /Activity status: Agent Waiting for model/ })).toBeVisible();

  act(() => useAppStore.setState({ runStatus: "waiting_approval" }));
  expect(screen.getByRole("button", { name: /Activity status: Agent Waiting for approval/ })).toBeVisible();
});

it("does not duplicate a running tool shown in the transcript, but preserves terminal status", () => {
  const card = { id: "tool", name: "browser_inspect", status: "running" };
  useAppStore.setState({ messages: [{ id: "live", role: "assistant", text: "", streaming: true,
    blocks: [{ id: "tool-block", type: "tool", card }] }] });
  render(<WorkspaceTitleBar {...props} />);
  expect(screen.queryByRole("button", { name: /Activity status:/ })).not.toBeInTheDocument();
  act(() => useAppStore.setState({ machineShellRunning: true, machineShellLine: "ls" }));
  expect(screen.getByRole("button", { name: /Activity status: Terminal/ })).toBeVisible();
});
