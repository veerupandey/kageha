import "@testing-library/jest-dom/vitest";
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { ParallelControls } from "./ParallelControls";

const api = vi.hoisted(() => vi.fn());
vi.mock("../api/client", () => ({ api }));

afterEach(() => {
  cleanup();
  api.mockReset();
});

it("shows capacity and saves settings through the parallelism API", async () => {
  const status = {
    enabled: false, mode: "auto", max_agents: 4, effective_agents: 3,
    active_agents: 1, available_slots: 2, cpu_count: 4,
    memory_available_bytes: 5 * 1024 ** 3,
    jobs: { running: 1, queued: 2, waiting: 0 },
  };
  api.mockResolvedValue(status);
  render(<ParallelControls />);
  expect(await screen.findByText(/1 running · 2 queued/)).toBeInTheDocument();
  fireEvent.click(screen.getByLabelText("Enable durable agents"));
  await waitFor(() => expect(api).toHaveBeenCalledWith(
    "/api/parallelism",
    expect.objectContaining({ method: "POST", body: '{"enabled":true}' }),
  ));
  fireEvent.change(screen.getByLabelText("Max"), { target: { value: "8" } });
  await waitFor(() => expect(api).toHaveBeenCalledWith(
    "/api/parallelism",
    expect.objectContaining({ method: "POST", body: '{"max_agents":8}' }),
  ));
});
