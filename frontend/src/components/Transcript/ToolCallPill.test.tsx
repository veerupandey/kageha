import "@testing-library/jest-dom/vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { ToolCallPill } from "./ToolCallPill";
afterEach(cleanup);
it("shows proposed and scheduled tool calls as preparation", () => {
  const view = render(<ToolCallPill card={{ id: "pending", name: "browser_click", status: "requested" }} />);
  expect(screen.getByText("Preparing tool")).toBeVisible();
  expect(screen.getByText("Preparing")).toBeVisible();
  expect(screen.queryByText("Using browser")).not.toBeInTheDocument();
  view.rerender(<ToolCallPill card={{ id: "pending", name: "browser_click", status: "pending" }} />);
  expect(screen.getByText("Preparing")).toBeVisible();
});
it("shows one recovery action for repeated checks of unchanged Gmail access", () => {
  const outcome = { connector: "gmail" as const, state: "disconnected", operation: "search", recovery: "manage_access" as const, revision: 3 };
  render(<><ToolCallPill card={{id:"first",name:"integration_read",status:"blocked",connectorOutcome:{...outcome,showRecovery:true}}} />
    <ToolCallPill card={{id:"repeat",name:"integration_read",status:"blocked",connectorOutcome:{...outcome,showRecovery:false}}} /></>);
  expect(screen.getAllByRole("button", {name:"Manage Gmail access"})).toHaveLength(1);
  expect(screen.getAllByText("Blocked")).toHaveLength(2);
});
it("distinguishes a browser input refused before execution from an execution failure", () => {
  const view = render(<ToolCallPill card={{ id: "held", name: "browser_fill", status: "error", resultPreview: JSON.stringify({status: "error", error_code: "browser_receipt_pending", summary: "Inspect the original click"}) }} />);
  expect(screen.getByText("Blocked").closest(".ka-tool-call")).toHaveAttribute("data-status", "blocked");
  view.rerender(<ToolCallPill card={{ id: "failed", name: "browser_fill", status: "error", resultPreview: JSON.stringify({status: "error", error_code: "execution_failed"}) }} />);
  expect(screen.getByText("Failed").closest(".ka-tool-call")).toHaveAttribute("data-status", "failed");
});

it("identifies background research without offering an unattached browser preview", () => {
  render(<ToolCallPill card={{id: "background", name: "headless_fetch", status: "completed", argsPreview: '{"urls":["https://example.test"]}'}} />);
  expect(screen.getByText("Web research")).toBeVisible();
  expect(screen.queryByRole("button", {name: /Open browser/})).not.toBeInTheDocument();
});

it("shows recovered success even when the recorded preview is clipped", () => {
  render(<ToolCallPill card={{id:"source",name:"web_fetch",status:"done",resultPreview:'{"text":"clipped',researchRecovery:{recovered:true,attemptedBackends:["http","chromium"]}}} />);
  expect(screen.getByText("Recovered").closest(".ka-tool-call")).toHaveAttribute("data-status", "complete");
  expect(screen.queryByText("Failed")).not.toBeInTheDocument();
});
