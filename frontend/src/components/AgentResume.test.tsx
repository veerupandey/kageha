import "@testing-library/jest-dom/vitest";
import { afterEach, expect, it, vi } from "vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { AgentResume } from "./AgentResume";
import type { DurableJob } from "./DurableJobs";
const job: DurableJob = {
  id: "worker",
  label: "Reviewer",
  state: "interrupted",
  kind: "agent",
  session_id: "child",
  created_at: 1,
  started_at: 1,
  revision: 4,
  artifacts: [],
  error: {},
  result: {},
  can_cancel: false,
  attachable: true,
  progress: {
    turn_id: "original-turn",
    steps: 1,
    label: "Interrupted",
    updated_at: 2,
  },
  resume: { allowed: true, requires_confirmation: true },
};
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});
it("requires explicit confirmation and uses exact turn identity with stable retry ID", async () => {
  const next = { ...job, state: "queued" };
  const resumed = vi.fn();
  const fetcher = vi
    .fn()
    .mockResolvedValueOnce({
      ok: false,
      json: async () => ({
        error: "Connection changed; review current state.",
      }),
    })
    .mockResolvedValueOnce({ ok: true, json: async () => ({ job: next }) });
  vi.stubGlobal("fetch", fetcher);
  render(<AgentResume job={job} onResumed={resumed} />);
  expect(fetcher).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Resume agent" }));
  expect(fetcher).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Confirm resume" }));
  await screen.findByRole("alert");
  const body = JSON.parse(fetcher.mock.calls[0][1].body);
  expect(body).toEqual({
    client_request_id: expect.any(String),
    confirmed: true,
    expected_turn_id: "original-turn",
    expected_revision: 4,
  });
  fireEvent.click(screen.getByRole("button", { name: "Confirm resume" }));
  await waitFor(() => expect(resumed).toHaveBeenCalledWith(next));
  expect(JSON.parse(fetcher.mock.calls[1][1].body).client_request_id).toBe(
    body.client_request_id,
  );
});
it("explains uncertain receipt blockers and never offers a mutation bypass", () => {
  const fetcher = vi.fn();
  vi.stubGlobal("fetch", fetcher);
  render(
    <AgentResume
      job={{
        ...job,
        resume: {
          allowed: false,
          reason: "An earlier browser action has an unknown outcome.",
        },
      }}
    />,
  );
  expect(
    screen.getByText("An earlier browser action has an unknown outcome."),
  ).toBeVisible();
  expect(
    screen.queryByRole("button", { name: /Resume/ }),
  ).not.toBeInTheDocument();
  expect(fetcher).not.toHaveBeenCalled();
});
