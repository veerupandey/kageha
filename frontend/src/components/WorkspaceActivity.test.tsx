import "@testing-library/jest-dom/vitest";
import { act, cleanup, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { api } from "../api/client";
import { applyStreamFrameToMessage } from "../lib/transcript";
import { ladderStepFromEvent } from "../lib/ladder";
import { LadderChip, WorkspaceFiles, WorkspacePlanCard } from "./WorkspaceActivity";
import { WORKSPACE_POLL_MS, workspaceFileUrl } from "../lib/workspaceFiles";

vi.mock("../api/client", () => ({ api: vi.fn() }));
afterEach(() => { cleanup(); vi.useRealTimers(); vi.mocked(api).mockReset(); });

const planUrl = "/api/sessions/chat-1/workspace/plan";
const treeUrl = "/api/sessions/chat-1/workspace/tree";

describe("ladder step", () => {
  it("is hidden while making progress and labelled with a tooltip otherwise", () => {
    const { container, rerender } = render(<LadderChip step="normal" />);
    expect(container).toBeEmptyDOMElement();
    rerender(<LadderChip />);
    expect(container).toBeEmptyDOMElement();
    rerender(<LadderChip step="not_a_step" />);
    expect(container).toBeEmptyDOMElement();
    for (const [step, label] of [["repair", "Repair"], ["change_method", "Change method"], ["stronger_model", "Stronger model"],
      ["replan", "Replan"], ["conclude", "Conclude"]] as const) {
      rerender(<LadderChip step={step} />);
      const chip = screen.getByText(label);
      expect(chip).toHaveAttribute("data-ladder", step);
      expect(chip.getAttribute("title")).toBeTruthy();
    }
  });

  it("follows the latest escalation and completion_check events of the turn", () => {
    expect(ladderStepFromEvent("escalation", { kind: "escalation", from: "normal", to: "repair", stale_steps: 3 })).toBe("repair");
    expect(ladderStepFromEvent("escalation", { payload: { to: "replan" } })).toBe("replan");
    expect(ladderStepFromEvent("completion_check", { status: "continue", ladder: "conclude" })).toBe("conclude");
    expect(ladderStepFromEvent("completion_check", { status: "continue" })).toBeUndefined();
    expect(ladderStepFromEvent("tool_result", { ladder: "repair" })).toBeUndefined();
    let message = applyStreamFrameToMessage({ id: "a", role: "assistant", text: "" },
      { sequence: 1, kind: "escalation", payload: { kind: "escalation", from: "normal", to: "change_method" } });
    expect(message.ladderStep).toBe("change_method");
    message = applyStreamFrameToMessage(message, { sequence: 2, kind: "completion_check", payload: { status: "continue", ladder: "normal" } });
    expect(message.ladderStep).toBe("normal");
    // A replayed older frame does not move it back.
    message = applyStreamFrameToMessage(message, { sequence: 1, kind: "escalation", payload: { to: "change_method" } });
    expect(message.ladderStep).toBe("normal");
  });
});

describe("plan card", () => {
  it("renders todo.md as markdown and refreshes while the turn runs", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    let plan = { exists: true, content: "- [x] draft\n- [ ] **verify**\n", sha256: "a" };
    vi.mocked(api).mockImplementation(async () => plan);
    render(<WorkspacePlanCard sessionId="chat-1" running refreshKey="k" />);
    const card = await screen.findByRole("region", { name: "Plan (todo.md)" });
    expect(api).toHaveBeenCalledWith(planUrl);
    expect(within(card).getByText("verify").tagName).toBe("STRONG");
    plan = { exists: true, content: "- [ ] send the brief\n", sha256: "b" };
    await act(async () => { vi.advanceTimersByTime(WORKSPACE_POLL_MS); });
    await waitFor(() => expect(within(card).getByText("send the brief")).toBeInTheDocument());
  });

  it("refetches on a refresh key change, does not poll when idle, and hides an empty plan", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.mocked(api).mockResolvedValue({ exists: true, content: "  \n", sha256: "e" });
    const { container, rerender } = render(<WorkspacePlanCard sessionId="chat-1" running={false} refreshKey="1" />);
    await waitFor(() => expect(api).toHaveBeenCalledTimes(1));
    await act(async () => { vi.advanceTimersByTime(WORKSPACE_POLL_MS * 2); });
    expect(api).toHaveBeenCalledTimes(1);
    expect(container).toBeEmptyDOMElement();
    rerender(<WorkspacePlanCard sessionId="chat-1" running={false} refreshKey="2" />);
    await waitFor(() => expect(api).toHaveBeenCalledTimes(2));
    rerender(<WorkspacePlanCard sessionId={null} running={false} refreshKey="2" />);
    expect(api).toHaveBeenCalledTimes(2);
  });
});

describe("workspace files", () => {
  const files = [
    { path: "todo.md", size: 20, sha256: "1", artifact_path: "workspace/todo.md" },
    { path: "outputs/brief notes.md", size: 2048, sha256: "2", artifact_path: "workspace/outputs/brief notes.md" },
    { path: "outputs/data/table.csv", size: 5, sha256: "3", artifact_path: "workspace/outputs/data/table.csv" },
  ];

  it("lists the tree with open and download links", async () => {
    vi.mocked(api).mockResolvedValue({ exists: true, files, truncated: false });
    render(<WorkspaceFiles sessionId="chat-1" running={false} refreshKey="k" />);
    const tree = await screen.findByRole("list", { name: "Workspace files" });
    expect(api).toHaveBeenCalledWith(treeUrl);
    expect(within(tree).getByText("outputs")).toBeInTheDocument();
    expect(within(tree).getByText("data")).toBeInTheDocument();
    expect(within(tree).getByText("2.0 KB")).toBeInTheDocument();
    const open = screen.getByRole("link", { name: "Open outputs/brief notes.md" });
    expect(open).toHaveAttribute("href", "/api/sessions/chat-1/workspace/file?path=outputs%2Fbrief%20notes.md");
    expect(open).toHaveAttribute("target", "_blank");
    expect(open.getAttribute("rel")).toContain("noopener");
    const download = screen.getByRole("link", { name: "Download outputs/data/table.csv" });
    expect(download).toHaveAttribute("href", "/api/sessions/chat-1/workspace/file?path=outputs%2Fdata%2Ftable.csv&download=1");
    expect(download).toHaveAttribute("download", "table.csv");
    expect(workspaceFileUrl("a b", "x.md", true)).toBe("/api/sessions/a%20b/workspace/file?path=x.md&download=1");
  });

  it("refreshes when a tool call finishes and hides a workspace that does not exist", async () => {
    vi.mocked(api).mockResolvedValueOnce({ exists: true, files: files.slice(0, 1) })
      .mockResolvedValueOnce({ exists: true, files });
    const { rerender } = render(<WorkspaceFiles sessionId="chat-1" running={false} refreshKey="running:" />);
    await screen.findByRole("link", { name: "Open todo.md" });
    rerender(<WorkspaceFiles sessionId="chat-1" running={false} refreshKey="running:call-1" />);
    await screen.findByRole("link", { name: "Open outputs/data/table.csv" });
    cleanup();
    vi.mocked(api).mockResolvedValue({ exists: false, files: [] });
    const { container } = render(<WorkspaceFiles sessionId="chat-1" running={false} refreshKey="x" />);
    await waitFor(() => expect(api).toHaveBeenCalledTimes(3));
    expect(container).toBeEmptyDOMElement();
  });
});
