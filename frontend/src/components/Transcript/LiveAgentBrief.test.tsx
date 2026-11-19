import "@testing-library/jest-dom/vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { AgentBriefSummary, LiveAgentBrief, LiveAgentBriefs } from "./LiveAgentBrief";
const job = vi.hoisted(() => ({ id: "worker", label: "Core architect", state: "running", progress: { activity: "tool_completed", agent_update: { summary: "Compared the supplied architecture diagrams", next_step: "Check the routing evidence" } } }));
const connection = vi.hoisted(() => ({ error: "" }));
vi.mock("../../store", () => ({ useAppStore: (selector: (state: unknown) => unknown) => selector({ sessionId: "parent" }) }));
vi.mock("../DurableJobs", () => ({ useDurableJobs: () => ({ jobs: [job], error: connection.error }) }));
afterEach(() => { cleanup(); connection.error = ""; });
it("counts worker completion from live jobs rather than submission status", () => {
  render(<LiveAgentBriefs><AgentBriefSummary jobIds={["worker"]} /></LiveAgentBriefs>);
  expect(screen.getByText("0/1 completed")).toBeVisible();
});
it("shows only recorded brief work for the requested durable agent", () => {
  render(<LiveAgentBriefs><LiveAgentBrief jobId="worker" /></LiveAgentBriefs>);
  expect(screen.getByText("Compared the supplied architecture diagrams")).toBeVisible();
  expect(screen.getByText("Next: Check the routing evidence")).toBeVisible();
});
it("does not substitute another worker's update when no matching identity exists", () => {
  render(<LiveAgentBriefs><LiveAgentBrief jobId="other" /></LiveAgentBriefs>);
  expect(screen.queryByText("Compared the supplied architecture diagrams")).not.toBeInTheDocument();
  expect(screen.getByText("Waiting for a recorded update…")).toBeVisible();
});

it("identifies retained worker progress as stale when live updates fail", () => {
  connection.error = "Offline";
  render(<LiveAgentBriefs><LiveAgentBrief jobId="worker" /></LiveAgentBriefs>);
  expect(screen.getByText("Compared the supplied architecture diagrams")).toBeVisible();
  expect(screen.getByText("Updates unavailable · showing last recorded activity")).toBeVisible();
});
