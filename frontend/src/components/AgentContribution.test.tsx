import "@testing-library/jest-dom/vitest";
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { AgentContributionCard, AgentContributionDetails } from "./AgentContribution";
import type { DurableJob } from "./DurableJobs";
const job: DurableJob = { id: "research", label: "Mira", kind: "agent", state: "succeeded", session_id: "child", created_at: 1, started_at: 1, revision: 1, can_cancel: false, attachable: true, artifacts: ["/private/report.md"], error: {}, result: { contribution: { finding: "Night-time coverage is incomplete.", evidence: ["Coverage table"], limitations: ["Only the supplied sample was reviewed."], decisions: ["Choose whether to collect more examples."] } } };
afterEach(cleanup);
it("shows the finding and files and opens the selected contribution", () => {
  const inspect = vi.fn();
  render(<AgentContributionCard job={job} onInspect={inspect} />);
  expect(screen.getByText("Night-time coverage is incomplete.")).toBeVisible();
  expect(screen.getByText("1 decision to consider")).toBeVisible();
  expect(screen.getByRole("link", { name: "report.md" })).toHaveAttribute("href", "/api/jobs/research/artifacts/0");
  fireEvent.click(screen.getByRole("button", { name: "View Mira’s contribution" }));
  expect(inspect).toHaveBeenCalledWith(job);
  expect(screen.queryByText("Checks passed")).not.toBeInTheDocument();
});
it("keeps limitations and decisions visible without inventing evidence", () => {
  render(<AgentContributionDetails job={job} />);
  expect(screen.getByText("Only the supplied sample was reviewed.")).toBeVisible();
  expect(screen.getByText("Choose whether to collect more examples.")).toBeVisible();
  expect(screen.getAllByRole("listitem")).toHaveLength(3);
});
it("labels findings from failed work as partial", () => {
  render(<AgentContributionCard job={{ ...job, state: "failed" }} />);
  expect(screen.getByText("Partial findings · the assignment did not complete.")).toBeVisible();
  expect(screen.queryByText("Checks passed")).not.toBeInTheDocument();
});
it("supports older results without parsing their prose", () => {
  render(<AgentContributionCard job={{ ...job, result: { message: "Verified! No gaps!" } }} />);
  expect(screen.getByText("Verified! No gaps!")).toBeVisible();
  expect(screen.queryByText(/limitation/)).not.toBeInTheDocument();
  expect(screen.queryByText("Checks passed")).not.toBeInTheDocument();
});

it("can be minimized to a compact row and expanded again in chat", () => {
  render(<AgentContributionCard job={job} />);
  expect(screen.getByText("Night-time coverage is incomplete.")).toBeVisible();

  fireEvent.click(screen.getByRole("button", { name: "Minimize Mira" }));
  expect(screen.queryByText("Night-time coverage is incomplete.")).not.toBeInTheDocument();
  expect(screen.getByRole("button", { name: "Expand Mira" })).toBeInTheDocument();

  fireEvent.click(screen.getByRole("button", { name: "Expand Mira" }));
  expect(screen.getByText("Night-time coverage is incomplete.")).toBeVisible();
});

it("supports controlled minimized prop and onMinimize callback", () => {
  const onMinimize = vi.fn();
  const { rerender } = render(<AgentContributionCard job={job} minimized={false} onMinimize={onMinimize} />);
  fireEvent.click(screen.getByRole("button", { name: "Minimize Mira" }));
  expect(onMinimize).toHaveBeenCalledWith(true);

  rerender(<AgentContributionCard job={job} minimized={true} onMinimize={onMinimize} />);
  expect(screen.queryByText("Night-time coverage is incomplete.")).not.toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: "Expand Mira" }));
  expect(onMinimize).toHaveBeenCalledWith(false);
});

it("supports opening inline guidance form and submitting notes to specialist", async () => {
  const fetchMock = vi.fn(async () => ({ ok: true }));
  vi.stubGlobal("fetch", fetchMock);
  render(<AgentContributionCard job={job} />);

  const guideBtn = screen.getByRole("button", { name: "Guide" });
  expect(guideBtn).toBeInTheDocument();
  fireEvent.click(guideBtn);

  const input = screen.getByPlaceholderText(/Guide Mira/);
  expect(input).toBeInTheDocument();
  fireEvent.change(input, { target: { value: "Focus on edge cases" } });

  const sendBtn = screen.getByRole("button", { name: "Send" });
  fireEvent.click(sendBtn);

  await waitFor(() =>
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/jobs/research/message",
      expect.objectContaining({
        body: expect.stringContaining("Focus on edge cases"),
      })
    )
  );
  expect(await screen.findByText("Guidance sent to specialist")).toBeInTheDocument();
});
