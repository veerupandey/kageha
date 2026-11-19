import "@testing-library/jest-dom/vitest";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
} from "@testing-library/react";
import { AgentTree } from "./AgentTree";
import type { DurableJob } from "./DurableJobs";
import { agentElapsed, agentOutline } from "../lib/agentTree";
const job = (id: string, extra: Partial<DurableJob> = {}): DurableJob => ({
  id,
  label: id,
  kind: "agent",
  state: "running",
  session_id: id + "-session",
  created_at: 10,
  started_at: 10,
  revision: 1,
  can_cancel: true,
  attachable: true,
  artifacts: [],
  error: {},
  result: {},
  ...extra,
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});
describe("agent tree", () => {
  it("renders parent relationships, selection, approval and current state", () => {
    const select = vi.fn();
    render(
      <AgentTree
        jobs={[
          job("Parent"),
          job("Child", {
            parent_job_id: "Parent",
            state: "waiting_input",
            error: { approval_id: "exact-approval" },
          }),
          job("Sibling"),
        ]}
        selectedId="Child"
        onSelect={select}
      />,
    );
    const child = screen.getByRole("treeitem", { name: /Child/ });
    expect(child).toHaveAttribute("aria-level", "2");
    expect(child).toHaveAttribute("aria-selected", "true");
    expect(child).toHaveTextContent("Approval");
    expect(child).toHaveTextContent("Needs input");
    fireEvent.click(screen.getByRole("treeitem", { name: /Sibling/ }));
    expect(select).toHaveBeenCalledWith(
      expect.objectContaining({ id: "Sibling" }),
    );
  });
  it("supports keyboard navigation and collapse without changing selection or executing tools", () => {
    const select = vi.fn();
    const jobs = [
      job("Parent"),
      job("Child", { parent_job_id: "Parent" }),
      job("Sibling"),
    ];
    const { rerender } = render(
      <AgentTree jobs={jobs} selectedId="Parent" onSelect={select} />,
    );
    const parent = screen.getByRole("treeitem", { name: /Parent/ });
    act(() => parent.focus());
    fireEvent.keyDown(parent, { key: "ArrowRight" });
    expect(screen.getByRole("treeitem", { name: /Child/ })).toHaveFocus();
    fireEvent.keyDown(document.activeElement!, { key: "ArrowDown" });
    expect(screen.getByRole("treeitem", { name: /Sibling/ })).toHaveFocus();
    act(() => parent.focus());
    fireEvent.keyDown(parent, { key: "ArrowLeft" });
    expect(
      screen.queryByRole("treeitem", { name: /Child/ }),
    ).not.toBeInTheDocument();
    rerender(
      <AgentTree
        jobs={jobs.map((job) => ({ ...job, revision: 2 }))}
        selectedId="Parent"
        onSelect={select}
      />,
    );
    expect(
      screen.queryByRole("treeitem", { name: /Child/ }),
    ).not.toBeInTheDocument();
    expect(select).not.toHaveBeenCalled();
  });
  it("handles broken parent references and cycles without losing workers", () => {
    const outline = agentOutline([
      job("a", { parent_job_id: "b" }),
      job("b", { parent_job_id: "a" }),
      job("orphan", { parent_job_id: "missing" }),
    ]);
    expect(outline).toHaveLength(3);
    expect(new Set(outline.map((node) => node.job.id)).size).toBe(3);
  });
  it("updates elapsed time for active agents and freezes completed durations", async () => {
    vi.useFakeTimers();
    vi.setSystemTime(20000);
    render(
      <AgentTree
        jobs={[
          job("active"),
          job("done", { state: "succeeded", ended_at: 15 }),
        ]}
        onSelect={vi.fn()}
      />,
    );
    expect(screen.getByRole("treeitem", { name: /active/ })).toHaveTextContent(
      "10s",
    );
    expect(screen.getByRole("treeitem", { name: /done/ })).toHaveTextContent(
      "5s",
    );
    await act(async () => {
      await vi.advanceTimersByTimeAsync(2000);
    });
    expect(screen.getByRole("treeitem", { name: /active/ })).toHaveTextContent(
      "12s",
    );
    expect(
      agentElapsed(job("done", { state: "interrupted", ended_at: 15 }), 22),
    ).toBe(5);
  });
});
