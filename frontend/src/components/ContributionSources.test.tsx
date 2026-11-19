import "@testing-library/jest-dom/vitest";
import { afterEach, expect, it } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { ContributionSources } from "./ContributionSources";
import type { DurableJob, ContributionSource } from "./DurableJobs";
const job: DurableJob = { id: "worker", label: "Mira", kind: "agent", state: "succeeded", session_id: "child", created_at: 1, started_at: 1, revision: 1, can_cancel: false, attachable: true, artifacts: [], error: {}, result: {} };
const renderSources = (sources: ContributionSource[]) => render(<ContributionSources job={{ ...job, result: { contribution: { finding: "Finding", evidence: [], limitations: [], decisions: [], sources } } }} />);
afterEach(cleanup);
it("opens web sources and shows their domain and supplied location", () => {
  renderSources([{ title: "Evaluation paper", locator: "Table 2, page 6", navigation: { status: "linked", kind: "web", href: "https://example.org/paper#table-2" } }]);
  expect(screen.getByRole("link")).toHaveAttribute("href", "https://example.org/paper#table-2");
  expect(screen.getByRole("link")).toHaveAttribute("rel", "noopener noreferrer");
  expect(screen.getByText("example.org · Table 2, page 6")).toBeVisible();
});
it("uses only the selected job's published file endpoint", () => {
  renderSources([{ title: "Coverage report", locator: "Night-time examples", navigation: { status: "linked", kind: "file", href: "/api/jobs/worker/artifacts/2" } }]);
  expect(screen.getByRole("link")).toHaveAttribute("href", "/api/jobs/worker/artifacts/2");
});
it.each(["javascript:alert(1)", "file:///etc/passwd", "https://user:password@example.org"])('does not turn %s into a link', href => {
  renderSources([{ title: "Unsafe target", navigation: { status: "linked", kind: "web", href } }]);
  expect(screen.queryByRole("link")).not.toBeInTheDocument();
  expect(screen.getByText("Link unavailable")).toBeVisible();
});
it("keeps unpublished references visible without guessing a destination", () => {
  renderSources([{ title: "Unpublished report", artifact_path: "/private/report.md", locator: "Page 3", navigation: { status: "unpublished", kind: "file" } }]);
  expect(screen.getByText("Unpublished report")).toBeVisible();
  expect(screen.getByText("File not published · link unavailable · Page 3")).toBeVisible();
  expect(screen.queryByRole("link")).not.toBeInTheDocument();
});
it("rejects another job's file route", () => {
  renderSources([{ title: "Other file", navigation: { status: "linked", kind: "file", href: "/api/jobs/other/artifacts/0" } }]);
  expect(screen.queryByRole("link")).not.toBeInTheDocument();
});
