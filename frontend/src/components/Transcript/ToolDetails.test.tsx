import "@testing-library/jest-dom/vitest";
import { afterEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useAppStore } from "../../store";
import { ToolCallPill } from "./ToolCallPill";
import { ShellBlock } from "./ShellBlock";
import { ToolDetails } from "./ToolDetails";
import { AgentCanvas } from "../AgentCanvas";

const original = useAppStore.getState();
afterEach(() => { cleanup(); vi.unstubAllGlobals(); useAppStore.setState(original); });

it("fetches complete tool content only when expanded", async () => {
  useAppStore.setState({ sessionId: "session-full" });
  const output = "Beginning. " + "Stored tool output. ".repeat(1000) + "Complete ending.";
  const fetcher = vi.fn().mockResolvedValue({ ok: true, json: async () => ({ arguments: { path: "file.txt" }, result: output, error: "" }) });
  vi.stubGlobal("fetch", fetcher);
  const { container } = render(<ToolCallPill card={{ id: "one", name: "read_file", attemptId: "attempt-full", status: "ok", resultPreview: "Beginning…" }} />);
  expect(fetcher).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button"));
  await waitFor(() => expect(container.textContent).toContain(output));
  expect(fetcher).toHaveBeenCalledWith("/api/sessions/session-full/tool-attempts/attempt-full", expect.any(Object));
});

it("expands a shell card to the complete command and stdout", async () => {
  useAppStore.setState({ sessionId: "shell-full" });
  const command = "printf " + "Long command argument. ".repeat(100);
  const stdout = "Complete shell line.\n".repeat(1000) + "Final shell line.";
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue({ ok: true, json: async () => ({ arguments: { command }, result: JSON.stringify({ stdout, stderr: "", exit_code: 0 }), error: "" }) }));
  const { container } = render(<ShellBlock card={{ id: "one", name: "bash", attemptId: "shell-attempt", status: "ok", argsPreview: "command=printf…", resultPreview: "short preview" }} />);
  fireEvent.click(screen.getByRole("button"));
  await waitFor(() => expect(container.textContent).toContain(stdout));
  expect(container.textContent).toContain(command);
});

it("labels a clipped preview when the full result cannot load", async () => {
  useAppStore.setState({ sessionId: "old-session" });
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue({ ok: false, status: 404, json: async () => ({ error: "Missing stored result" }) }));
  render(<ToolCallPill card={{ id: "old", name: "read_file", attemptId: "missing", status: "ok", resultPreview: "Saved fragment" }} />);
  fireEvent.click(screen.getByRole("button"));
  await screen.findByRole("alert");
  expect(screen.getByRole("region", { name: "Recorded preview" })).toHaveTextContent("Saved fragment");
});

it("distinguishes acknowledged dispatch from verification in browser receipts", async () => {
  useAppStore.setState({ sessionId: "browser-receipts" });
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue({ ok: true, json: async () => ({
    arguments: {}, result: "Partial browser result", error: "",
    browser_receipts: [
      { id: "batch", action: "batch", state: "planned", dispatch: "returned", verification: "unverified" },
      { id: "one", action: "fill", step: 0, state: "completed", dispatch: "returned", verification: "unverified" },
      { id: "two", action: "click", step: 1, state: "uncertain", dispatch: "started", verification: "unverified" },
      { id: "three", action: "click", step: 2, state: "reconciled", dispatch: "started", verification: "expected_condition" },
      { id: "four", action: "click", step: 3, state: "not_dispatched", dispatch: "not_started", verification: "unverified" },
    ],
  }) }));
  render(<ToolDetails card={{ id: "browser", name: "browser_batch", attemptId: "batch-attempt", status: "failed" }} />);
  const section = await screen.findByRole("region", { name: "Browser action receipts" });
  expect(section.textContent).toContain("batch · Plan recorded. See step receipts for input status.");
  expect(section.textContent).toContain("Step 1: fill · Dispatch acknowledged; outcome not verified");
  expect(section.textContent).toContain("Step 2: click · Outcome uncertain");
  expect(section.textContent).toContain("Step 3: click · Expected condition observed");
  expect(section.textContent).toContain("Step 4: click · Input was not dispatched");
});

it("shows a labelled preview when the journal has no detail identifier", () => {
  const fetcher = vi.fn();
  vi.stubGlobal("fetch", fetcher);
  render(<ToolDetails card={{ id: "legacy", name: "read_file", resultPreview: "Saved fragment" }} />);
  expect(screen.getByRole("region", { name: "Recorded preview" })).toHaveTextContent("Saved fragment");
  expect(fetcher).not.toHaveBeenCalled();
});

it("puts research protocol errors behind technical details", () => {
  render(<ToolDetails card={{ id: "research", name: "web_fetch", status: "failed", resultPreview: "cdp_invalid_reply" }} />);
  expect(screen.getByText("Couldn’t read this source")).toBeVisible();
  const technical = screen.getByText("Technical details").closest("details");
  expect(technical).not.toHaveAttribute("open");
  expect(technical).toHaveTextContent("cdp_invalid_reply");
});

it("explains exhausted reading methods while keeping protocol details collapsed", () => {
  const summary = "Both public-page reading methods failed. Continue with another source.";
  render(<ToolDetails card={{ id: "exhausted", name: "web_fetch", status: "failed", resultPreview: `Tool failed: ${JSON.stringify({error_code:"research_recovery_exhausted",summary,attempted_backends:["http","chromium"]})}` }} />);
  expect(screen.getByText(summary)).toBeVisible();
  expect(screen.getByText("Tried: HTTP reading, browser extraction.")).toBeVisible();
  expect(screen.getByText("Technical details").closest("details")).not.toHaveAttribute("open");
});

it("shows successful recovery and the limits of retained source text", () => {
  render(<ToolDetails card={{ id: "recovered", name: "web_fetch", status: "done", resultPreview: JSON.stringify({recovery:{recovered:true},source_id:"source-one",source_truncated:true,text:"Read source"}) }} />);
  expect(screen.getByText("Read successfully using browser extraction")).toBeVisible();
  expect(screen.getByText(/without downloading again/)).toBeVisible();
  expect(screen.getByText(/does not establish absence/)).toBeVisible();
  expect(screen.queryByText("Couldn’t read this source")).not.toBeInTheDocument();
});

it("ignores a stale response after the session changes", async () => {
  let resolveOld!: (value: unknown) => void;
  const pending = new Promise((resolve) => { resolveOld = resolve; });
  const fetcher = vi.fn().mockReturnValueOnce(pending).mockResolvedValueOnce({
    ok: true, json: async () => ({ arguments: {}, result: "Current session output", error: "" }),
  });
  vi.stubGlobal("fetch", fetcher);
  useAppStore.setState({ sessionId: "old" });
  render(<ToolDetails card={{ id: "one", name: "read_file", attemptId: "attempt", status: "ok" }} />);
  act(() => useAppStore.setState({ sessionId: "current" }));
  await screen.findByText("Current session output");
  await act(async () => resolveOld({ ok: true, json: async () => ({ arguments: {}, result: "Stale session output", error: "" }) }));
  expect(screen.queryByText("Stale session output")).not.toBeInTheDocument();
  expect(fetcher.mock.calls[0][1].signal.aborted).toBe(true);
});

it("reloads a running attempt when it completes and preserves both result and error", async () => {
  useAppStore.setState({ sessionId: "active" });
  const fetcher = vi.fn()
    .mockResolvedValueOnce({ ok: true, json: async () => ({ arguments: {}, result: "", error: "" }) })
    .mockResolvedValueOnce({ ok: true, json: async () => ({ arguments: {}, result: "Partial tool output", error: "Complete failure explanation" }) });
  vi.stubGlobal("fetch", fetcher);
  const card = { id: "one", name: "read_file", attemptId: "attempt", status: "running" };
  const { rerender } = render(<ToolDetails card={card} />);
  await waitFor(() => expect(screen.queryByText("Loading full output…")).not.toBeInTheDocument());
  rerender(<ToolDetails card={{ ...card, status: "failed" }} />);
  await screen.findByText("Complete failure explanation");
  expect(screen.getByText("Partial tool output")).toBeVisible();
  expect(fetcher).toHaveBeenCalledTimes(2);
});

it("uses stored tool content in the activity timeline and keeps the entire trace", async () => {
  const output = "Full timeline output. ".repeat(100) + "Timeline ending.";
  vi.stubGlobal("fetch", vi.fn(async (path: string) => ({ ok: true, json: async () =>
    path.includes("tool-attempts") ? { arguments: {}, result: output, error: "" } : { jobs: [] },
  })));
  useAppStore.setState({ sessionId: "timeline", messages: [{ id: "message", role: "assistant", text: "",
    steps: Array.from({ length: 30 }, (_, i) => ({ label: `Trace entry ${i}`, kind: "todo_board", interesting: true })),
    toolCards: [{ id: "one", name: "read_file", attemptId: "attempt", status: "ok", resultPreview: "Saved fragment" }],
  }] });
  const { container } = render(<AgentCanvas alwaysShow />);
  fireEvent.click(screen.getByRole("tab", { name: "Timeline" }));
  expect(screen.getByText("Trace entry 0")).toBeInTheDocument();
  expect(screen.getByText("Trace entry 29")).toBeInTheDocument();
  fireEvent.click(screen.getByRole("button", { name: /Reading file/ }));
  await waitFor(() => expect(container.textContent).toContain(output));
  expect(screen.queryByText("Saved fragment")).not.toBeInTheDocument();
});


it("shows saved execution and child timings with distinct scope", async () => {
  useAppStore.setState({ sessionId: "timings" });
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue({ ok: true, json: async () => ({
    arguments: {}, result: "script output", error: "",
    browser_timing: { wall_ms: 940, dropped_spans: 1, spans: [
      { stage: "script_process", duration_ms: 845.4, status: "returned" },
      { stage: "verification", duration_ms: 12, status: "interrupted" },
    ] }, script_timing: { cdp_attach_ms: 23, child_total_ms: 295 },
  }) }));
  render(<ToolDetails card={{ id: "one", name: "browser_script", attemptId: "script", status: "ok" }} />);
  fireEvent.click(await screen.findByText("Browser execution timing · 940 ms"));
  expect(screen.getByText("845.4 ms")).toBeVisible();
  expect(screen.getByText("Check expected condition (interrupted)")).toBeVisible();
  expect(screen.getByText("Inside script process")).toBeVisible();
  expect(screen.getByText("Attach to browser")).toBeVisible();
  expect(screen.getByText(/exclude model time/)).toBeVisible();
});

it("refreshes an expanded uncertain receipt after read-only reconciliation", async () => {
  vi.useFakeTimers();
  try {
    useAppStore.setState({ sessionId: "reconcile" });
    let reads = 0;
    vi.stubGlobal("fetch", vi.fn().mockImplementation(async () => ({ ok: true, json: async () => ({
      arguments: {}, result: "output", error: "", browser_receipts: [{ id: "receipt", action: "click", step: null,
        state: reads++ === 0 ? "uncertain" : "reconciled", dispatch: "started", verification: reads === 1 ? "unverified" : "expected_condition" }],
    }) })));
    await act(async () => { render(<ToolDetails card={{ id: "one", name: "browser_click", attemptId: "click", status: "failed" }} />); });
    expect(screen.getByText(/Outcome uncertain/)).toBeInTheDocument();
    await act(async () => { await vi.advanceTimersByTimeAsync(2000); });
    expect(screen.getByText(/Expected condition observed/)).toBeInTheDocument();
    await act(async () => { await vi.advanceTimersByTimeAsync(4000); });
    expect(reads).toBe(2);
  } finally { vi.useRealTimers(); }
});

it("shows the recorded failure preview when native tool details have no attempt ID", () => {
  render(<ToolDetails card={{ id: "native", name: "browser_click", status: "error", resultPreview: "No click was sent." }} />);
  expect(screen.getByRole("region", { name: "Recorded failure preview" })).toHaveTextContent("No click was sent.");
});

it("explains an empty native memory search", () => {
  render(<ToolDetails card={{ id: "native-memory", name: "memory_search", status: "done",
    resultPreview: JSON.stringify({ value: [] }) }} />);
  expect(screen.getByText("No memories matched.")).toBeVisible();
});
