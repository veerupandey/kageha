import "@testing-library/jest-dom/vitest";
import { afterEach, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { BrowserDecisionRoute } from "./BrowserDecisionRoute";
afterEach(cleanup);

it("shows provider confidence without claiming proposals were executed", async () => {
  render(<BrowserDecisionRoute route={{ turn_id: "t", status: "routed", model: "jev", score_metric: "provider_confidence", cutoff: .9, batch_cutoff: .99, minimum_score: .95 }} />);
  fireEvent.click(screen.getByRole("button", { name: "Browser execution route" }));
  expect(await screen.findByText(/decision model proposed a browser action/)).toBeVisible();
  expect(screen.getByText(/Provider confidence cutoff: 90%/)).toHaveTextContent("Batch: 99%");
  expect(screen.getByText("Decision model: jev")).toBeVisible();
});

it("explains normal-agent fallback without a browser failure banner", async () => {
  render(<BrowserDecisionRoute route={{ turn_id: "t", status: "caller_values_required", model: "clef" }} />);
  expect(screen.getByRole("button", { name: "Browser execution route" })).toHaveTextContent("Regular agent");
  fireEvent.click(screen.getByRole("button", { name: "Browser execution route" }));
  expect(await screen.findByText(/without an extra decision call/)).toBeVisible();
  expect(screen.queryByRole("alert")).not.toBeInTheDocument();
});

it("removes old run metadata when the new chat has no route", () => {
  const { rerender } = render(<BrowserDecisionRoute route={{ turn_id: "t", status: "routed", model: "jev" }} />);
  rerender(<BrowserDecisionRoute route={null} />);
  expect(screen.queryByRole("button")).not.toBeInTheDocument();
});

it("keeps verified actions visible after a later provider failure", async () => {
  render(<BrowserDecisionRoute route={{ turn_id: "t", status: "fallback", model: "jev",
    reason: "invalid_response", elapsed_ms: 613, verified_actions: 2 }} />);
  fireEvent.click(screen.getByRole("button", { name: "Browser execution route" }));
  expect(await screen.findByText("The provider returned an invalid typed response.")).toBeVisible();
  expect(screen.getByText("Verified decision actions this turn: 2")).toBeVisible();
});
