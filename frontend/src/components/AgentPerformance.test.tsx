import "@testing-library/jest-dom/vitest";
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { AgentPerformance } from "./AgentPerformance";

const mocks = vi.hoisted(() => ({ api: vi.fn() }));
vi.mock("../api/client", () => ({ api: mocks.api }));
afterEach(() => { cleanup(); vi.clearAllMocks(); });

it("shows weekly timing and verification together without claiming a speedup", async () => {
  mocks.api.mockResolvedValue({ enabled: true, speed_metrics: [{ agent: "reviewer", task_type: "review", week: "2026-W45", requests: 5, verified: 3, elapsed_samples: 2, median_elapsed_ms: 75000, p95_elapsed_ms: 120000, mean_model_calls: 2.5 }] });
  render(<AgentPerformance />);
  expect(await screen.findByRole("list", { name: "Weekly agent performance by task type" })).toBeInTheDocument();
  expect(screen.getByText("3/5")).toBeInTheDocument();
  expect(screen.getByText("(2 timed)")).toBeInTheDocument();
  expect(within(screen.getByRole("list", { name: "Weekly agent performance by task type" })).getByText(/1m 15s/)).toBeInTheDocument();
  expect(screen.getByText(/not proof of a speedup/)).toBeInTheDocument();
});

it("compares only weeks for the selected agent and task type", async () => {
  const row = (agent: string, task_type: string, week: string, median: number | null) => ({ agent, task_type, week, requests: 2, verified: 1, elapsed_samples: median == null ? 0 : 1, median_elapsed_ms: median, p95_elapsed_ms: median, mean_model_calls: 1 });
  mocks.api.mockResolvedValue({ enabled: true, speed_metrics: [row("reviewer", "review", "2026-W45", 30000), row("reviewer", "review", "2026-W43", 60000), row("writer", "draft", "2026-W44", null)] });
  render(<AgentPerformance />);
  const selector = await screen.findByRole("combobox", { name: "Agent and task type" });
  const chart = screen.getByRole("list", { name: "Weekly median durations" });
  expect(within(chart).getByText("2026-W43")).toBeInTheDocument();
  expect(within(chart).queryByText("2026-W44")).not.toBeInTheDocument();
  fireEvent.change(selector, { target: { value: JSON.stringify(["writer", "draft"]) } });
  expect(screen.queryByRole("list", { name: "Weekly median durations" })).not.toBeInTheDocument();
  expect(screen.getByText("No verified timings are available for this group yet.")).toBeInTheDocument();
  expect(within(screen.getByRole("list", { name: "Weekly agent performance by task type" })).getByText("2026-W44")).toBeInTheDocument();
});

it("defaults to the cohort with the most timed weeks and preserves a manual selection", async () => {
  const row = (agent: string, week: string, median: number | null) => ({ agent, task_type: "review", week, requests: 1, verified: 1, elapsed_samples: median == null ? 0 : 1, median_elapsed_ms: median, p95_elapsed_ms: median, mean_model_calls: 1 });
  mocks.api.mockResolvedValue({ enabled: true, speed_metrics: [
    row("alpha", "2026-W46", 12000),
    row("beta", "2026-W45", 30000), row("beta", "2026-W43", 60000),
    row("zeta", "2026-W45", 18000), row("zeta", "2026-W43", 20000),
    row("gamma", "2026-W44", 14000), row("gamma", "2026-W42", 20000),
    row("delta", "2026-W46", null), row("delta", "2026-W45", null),
  ] });
  render(<AgentPerformance />);
  const selector = await screen.findByRole("combobox", { name: "Agent and task type" });
  expect(selector).toHaveValue(JSON.stringify(["beta", "review"]));
  expect(within(screen.getByRole("list", { name: "Weekly median durations" })).getByText("2026-W43")).toBeInTheDocument();
  fireEvent.change(selector, { target: { value: JSON.stringify(["alpha", "review"]) } });
  expect(selector).toHaveValue(JSON.stringify(["alpha", "review"]));
  expect(within(screen.getByRole("list", { name: "Weekly median durations" })).getByText("2026-W46")).toBeInTheDocument();
});

it("offers a retry when loading fails", async () => {
  mocks.api.mockRejectedValueOnce(new Error("Temporarily unavailable")).mockResolvedValueOnce({ enabled: true, speed_metrics: [] });
  render(<AgentPerformance />);
  expect(await screen.findByRole("alert")).toHaveTextContent("Temporarily unavailable");
  fireEvent.click(screen.getByRole("button", { name: "Try again" }));
  expect(await screen.findByText("Your history starts here")).toBeInTheDocument();
});

it("explains when measurements are unavailable", async () => {
  mocks.api.mockResolvedValue({ enabled: false, speed_metrics: [] });
  render(<AgentPerformance />);
  expect(await screen.findByText("Learning measurements are unavailable.")).toBeInTheDocument();
});
