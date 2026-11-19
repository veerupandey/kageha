import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { WorkspaceChip } from "./WorkspaceChip";
import { api } from "../api/client";
import { refreshSessionTruth } from "../lib/sessionTruth";

const state = vi.hoisted(() => ({ status: "current", data: { selected_target: "local", turn: null as null | { phase: string },
  workspace: { mode: "private", root: "/sessions/chat", source_root: "", revision: "one", legacy: false } } }));
vi.mock("../api/client", () => ({ api: vi.fn() }));
vi.mock("../lib/sessionTruth", () => ({ useSessionTruth: () => state, refreshSessionTruth: vi.fn() }));
beforeEach(() => { state.status = "current"; state.data.turn = null; state.data.selected_target = "local"; state.data.workspace.mode = "private"; vi.mocked(api).mockResolvedValue({}); });
afterEach(() => { cleanup(); vi.clearAllMocks(); });

it("shows the existing session folder and makes shared project attachment explicit", async () => {
  render(<WorkspaceChip sessionId="chat" />);
  expect(screen.getByRole("button", { name: "Chat files" }).title).toBe("/sessions/chat");
  fireEvent.click(screen.getByRole("button", { name: "Chat files" }));
  fireEvent.click(screen.getByLabelText("Use a shared project folder"));
  fireEvent.change(screen.getByLabelText("Project folder"), { target: { value: "/project" } });
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Use workspace" })));
  expect(api).toHaveBeenCalledWith("/api/sessions/chat/workspace", expect.objectContaining({ method: "POST", body: JSON.stringify({ mode: "shared", project_root: "/project" }) }));
  expect(refreshSessionTruth).toHaveBeenCalledWith("chat");
});

it("disables workspace changes during work, on stale contact, and for remote work", () => {
  state.data.turn = { phase: "executing" };
  const view = render(<WorkspaceChip sessionId="chat" />);
  expect(screen.getByRole("button").hasAttribute("disabled")).toBe(true);
  state.data.turn = null; state.status = "stale"; view.rerender(<WorkspaceChip sessionId="chat" />);
  expect(screen.getByRole("button").hasAttribute("disabled")).toBe(true);
  state.status = "current"; state.data.selected_target = "remote"; view.rerender(<WorkspaceChip sessionId="chat" />);
  expect(screen.getByRole("button").textContent).toBe("Remote workspace");
  expect(screen.getByRole("button").hasAttribute("disabled")).toBe(true);
});

it("retains the dialog and error if the backend rejects a raced active run", async () => {
  vi.mocked(api).mockRejectedValue(new Error("Stop the current run first"));
  render(<WorkspaceChip sessionId="chat" />);
  fireEvent.click(screen.getByRole("button", { name: "Chat files" }));
  fireEvent.click(screen.getByRole("button", { name: "Use workspace" }));
  await waitFor(() => expect(screen.getByRole("alert").textContent).toContain("Stop the current run"));
  expect(screen.getByRole("dialog")).toBeTruthy();
  expect(refreshSessionTruth).not.toHaveBeenCalled();
});

it("explains that isolated checkouts preserve uncommitted files in their original folder", () => {
  render(<WorkspaceChip sessionId="chat" />);
  fireEvent.click(screen.getByRole("button", { name: "Chat files" }));
  fireEvent.click(screen.getByLabelText("Create an isolated Git checkout"));
  expect(screen.getByText(/uncommitted files are not copied/)).toBeTruthy();
  expect(screen.getByRole("button", { name: "Use workspace" }).hasAttribute("disabled")).toBe(true);
});
