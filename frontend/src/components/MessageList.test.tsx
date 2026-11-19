import "@testing-library/jest-dom/vitest";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, within } from "@testing-library/react";
import type { ChatMessage, SessionSummary } from "../api/types";
import { useAppStore } from "../store";
import { MessageList } from "./MessageList";
import { publishBrowserControls } from "../lib/browserWorkspace";

/**
 * Regression tests for the two mobile scroll bugs reported against the chat
 * thread view:
 *   1. Initial load did not land at the latest message and there was no
 *      working way to reach it — see MessageList.tsx's initial-scroll effect.
 *   2. While a response streamed in, scrolling up got silently overridden —
 *      the view kept snapping back to the bottom, hiding new content below
 *      the fold — see the onScroll handler's `programmaticUntil` guard.
 */

let resizeCallbacks: Array<() => void> = [];

class ResizeObserverStub {
  callback: () => void;
  constructor(cb: () => void) {
    this.callback = cb;
    resizeCallbacks.push(cb);
  }
  observe() {}
  unobserve() {}
  disconnect() {
    resizeCallbacks = resizeCallbacks.filter((cb) => cb !== this.callback);
  }
}

function baseMessage(overrides: Partial<ChatMessage> = {}): ChatMessage {
  return {
    id: "m1",
    role: "assistant",
    text: "hello",
    ...overrides,
  };
}

function mountConversationDom() {
  document.body.innerHTML = "";
  const conversation = document.createElement("section");
  conversation.id = "conversation";
  const nav = document.createElement("div");
  nav.id = "conversation-navigation";
  document.body.appendChild(conversation);
  document.body.appendChild(nav);
  return conversation;
}

function stubScrollMetrics(
  el: HTMLElement,
  metrics: { scrollHeight: number; clientHeight: number; scrollTop?: number },
) {
  Object.defineProperty(el, "scrollHeight", {
    configurable: true,
    value: metrics.scrollHeight,
  });
  Object.defineProperty(el, "clientHeight", {
    configurable: true,
    value: metrics.clientHeight,
  });
  if (metrics.scrollTop !== undefined) {
    Object.defineProperty(el, "scrollTop", {
      configurable: true,
      writable: true,
      value: metrics.scrollTop,
    });
  }
  if (!el.scrollTo) {
    // jsdom doesn't implement scrollTo — the "Go to top" button calls it.
    (el as any).scrollTo = (opts?: { top?: number }) => {
      if (opts && typeof opts.top === "number") {
        Object.defineProperty(el, "scrollTop", {
          configurable: true,
          writable: true,
          value: opts.top,
        });
      }
    };
  }
}

describe("MessageList mobile scroll behavior", () => {
  beforeEach(() => {
    resizeCallbacks = [];
    vi.stubGlobal("ResizeObserver", ResizeObserverStub);
    useAppStore.setState({
      sessionId: "sess-1",
      sessionLoading: false,
      runStatus: "idle",
      sending: false,
      pendingApproval: null,
      meta: null,
    } as Partial<ReturnType<typeof useAppStore.getState>>);
  });

  afterEach(() => {
    publishBrowserControls(null);
    cleanup();
    document.body.innerHTML = "";
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it("mounts recent history first, expands earlier rows, and resets for another thread", () => {
    const rows: ChatMessage[] = Array.from({ length: 60 }, (_, index) => ({ id: `row-${index}`, role: "user", text: `History row ${index}` }));
    const conversation = mountConversationDom();
    const view = render(<MessageList messages={rows} />, { container: conversation });
    expect(conversation.querySelectorAll("article")).toHaveLength(20);
    expect(view.queryByText("History row 0")).toBeNull();
    fireEvent.click(view.getByRole("button", { name: "Load earlier messages (40)" }));
    expect(conversation.querySelectorAll("article")).toHaveLength(40);
    fireEvent.click(view.getByRole("button", { name: "Load earlier messages (20)" }));
    expect(conversation.querySelectorAll("article")).toHaveLength(60);
    expect(view.getByText("History row 0")).toBeInTheDocument();
    act(() => useAppStore.setState({ sessionId: "another-thread" }));
    expect(conversation.querySelectorAll("article")).toHaveLength(20);
  });

  it("shows a receipt after admission until the first visible progress", () => {
    const pending = baseMessage({ text: "", streaming: true });
    const view = render(<MessageList messages={[pending]} />, { container: mountConversationDom() });
    expect(view.queryByText("Message received.")).not.toBeInTheDocument();

    const accepted = { ...pending, turnId: "turn" };
    view.rerender(<MessageList messages={[accepted]} />);
    expect(view.getByText("Message received.")).toBeVisible();

    view.rerender(<MessageList messages={[{ ...accepted, blocks: [
      { type: "progress", id: "ack-turn", text: "I'll check the file." },
    ] }]} />);
    expect(view.queryByText("Message received.")).not.toBeInTheDocument();
  });

  it("offers a compact browser link only when the live browser is out of view", () => {
    const conversation = mountConversationDom();
    useAppStore.setState({
      workspaceSurface: "live", liveSurface: "browser",
      prefs: { ...useAppStore.getState().prefs, rightPanelCollapsed: false },
    });
    publishBrowserControls({ sessionId: "sess-1", connected: true, controlling: false, busy: false, error: "", transfer: vi.fn() });
    const view = render(<MessageList messages={[baseMessage({ streaming: true })]} />, { container: conversation });
    expect(view.queryByRole("button", { name: "Open live browser" })).not.toBeInTheDocument();

    act(() => useAppStore.setState({ workspaceSurface: "artifacts" }));
    fireEvent.click(view.getByRole("button", { name: "Open live browser" }));
    expect(useAppStore.getState().workspaceSurface).toBe("live");
    expect(useAppStore.getState().liveSurface).toBe("browser");
    expect(view.queryByRole("button", { name: "Open live browser" })).not.toBeInTheDocument();

    act(() => {
      publishBrowserControls(null);
      useAppStore.setState({ prefs: { ...useAppStore.getState().prefs, rightPanelCollapsed: true } });
    });
    fireEvent.click(view.getByRole("button", { name: "Open browser" }));
    expect(useAppStore.getState().prefs.rightPanelCollapsed).toBe(false);
  });

  it.each([{}, { "0": { turn_index: 0 } }, "invalid", null])("opens chats with legacy or malformed cached tag metadata: %j", (tags) => {
    const conversation = mountConversationDom();
    useAppStore.setState({ sessions: [{ session_id: "sess-1", tagged_turns: tags as unknown as SessionSummary["tagged_turns"] }] });
    const { container } = render(<MessageList messages={[baseMessage()]} />, { container: conversation });
    expect(container).toHaveTextContent("hello");
  });

  it("keeps saved authorship when another agent is selected", () => {
    const conversation = mountConversationDom();
    const messages = [baseMessage({ speaker: { id: "researcher", name: "Researcher", avatar: "R" } })];
    const { container, rerender } = render(<MessageList messages={messages} />, { container: conversation });
    expect(container.querySelector(".ka-message-header")).toHaveTextContent("Researcher");
    act(() => useAppStore.setState({ activePersonaId: "engineer" }));
    rerender(<MessageList messages={messages} />);
    expect(container.querySelector(".ka-message-header")).toHaveTextContent("Researcher");
  });

  it("offers resume after a provider failure and shows its actual cause", () => {
    const conversation = mountConversationDom();
    const { container, getByRole } = render(<MessageList messages={[
      baseMessage({ text: "Research is saved; the task remains incomplete.",
        resultStatus: "error", statusLabel: "Error", failureCause: "Provider timeout after fallback attempts." }),
    ]} />, { container: conversation });
    expect(container).toHaveTextContent("Run failed");
    expect(container).toHaveTextContent("Provider timeout after fallback attempts.");
    expect(getByRole("button", { name: "Resume task" })).toBeEnabled();
  });

  it("shows the failure detail of an unrecognized failure under the run-failed card", () => {
    const conversation = mountConversationDom();
    const cause = "The agent stopped before completing this turn. Inspect its recorded activity.";
    const { container, getByTestId, rerender } = render(<MessageList messages={[
      baseMessage({ text: cause, resultStatus: "error", statusLabel: "Error", failureCause: cause,
        failureDetail: "Image attachments exceed 4 MiB per model turn" }),
    ]} />, { container: conversation });
    expect(container).toHaveTextContent("Run failed");
    expect(getByTestId("failure-detail")).toHaveTextContent("Details: Image attachments exceed 4 MiB per model turn");
    rerender(<MessageList messages={[
      baseMessage({ text: cause, resultStatus: "error", statusLabel: "Error", failureCause: cause }),
    ]} />);
    expect(container.querySelector("[data-testid='failure-detail']")).toBeNull();
  });

  it.each([undefined, "delivery_unconfirmed"])("shows unconfirmed delivery as a warning, including old messages (%s)", (resultStatus) => {
    const conversation = mountConversationDom();
    const { container } = render(<MessageList messages={[
      baseMessage({ text: "", resultStatus, statusLabel: "Delivery unconfirmed · check before retrying",
        failureCause: "The connection ended before a new run was confirmed. Your reply is preserved." }),
    ]} />, { container: conversation });
    expect(container).toHaveTextContent("Delivery unconfirmed");
    expect(container).not.toHaveTextContent("Run failed");
    expect(container.querySelector('[role="alert"]')).toBeNull();
    expect(within(container).getByRole("button", { name: "Check delivery" })).toBeEnabled();
    expect(within(container).queryByRole("button", { name: "Resume task" })).toBeNull();
  });

  it("puts the authoritative incomplete status before an unsupported completion claim", () => {
    const conversation = mountConversationDom();
    const { container } = render(<MessageList messages={[
      baseMessage({ text: "Every application was completed.", resultStatus: "unverified",
        failureCause: "One application has no receipt." }),
    ]} />, { container: conversation });
    const notice = within(container).getByTestId("unverified-status");
    const claim = within(container).getByText("Every application was completed.");
    expect(notice).toHaveTextContent("Task incomplete");
    expect(notice.compareDocumentPosition(claim) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it("labels a failed additional inspection as unresolved verification", () => {
    const conversation = mountConversationDom();
    const { container } = render(<MessageList messages={[
      baseMessage({ text: "The requested answer is saved.", resultStatus: "unverified",
        statusLabel: "Verification unresolved", failureCause: "Additional verification could not finish: provider timeout." }),
    ]} />, { container: conversation });
    expect(container).toHaveTextContent("The requested answer is saved.");
    expect(container).toHaveTextContent("Some results need checking");
    expect(container).toHaveTextContent("provider timeout");
    expect(container).not.toHaveTextContent("Run failed");
    expect(container.querySelector('[role="alert"]')).toBeNull();
  });

  it("does not repeat an old answer as the verification warning", () => {
    const conversation = mountConversationDom();
    const answer = "One application submitted. Four blocked by ATS failures.";
    const { container } = render(<MessageList messages={[
      baseMessage({ text: answer, resultStatus: "unverified", failureCause: answer }),
    ]} />, { container: conversation });
    const warning = container.querySelector('[role="status"]');
    expect(warning).toHaveTextContent("some outcomes remain unverified");
    expect(warning).not.toHaveTextContent(answer);
    expect(container).toHaveTextContent(answer);
  });

  it("shows confirmed and unresolved checks with an explicit continuation action", () => {
    const conversation = mountConversationDom();
    const cause = "The remaining outcomes could not be confirmed.";
    const saved = `One application submitted.\n\n---\n_${cause}_`;
    const message = baseMessage({ text: saved, resultStatus: "unverified", failureCause: cause,
      blocks: [{ type: "narrative", id: "final", text: saved }],
      verificationOutcomes: [
        { id: "submitted", label: "Altus application", status: "pass" },
        { id: "remaining", label: "Workday application", status: "unresolved" },
        { id: "blocked", label: "Cohere application", status: "blocked" },
      ] });
    const { container, getByRole, getAllByText } = render(<MessageList messages={[message]} />, { container: conversation });
    expect(container).toHaveTextContent("Checks: 1 confirmed · 2 need attention");
    expect(getByRole("button", { name: "Continue unfinished work" })).toBeEnabled();
    expect(getAllByText(cause)).toHaveLength(1);
    expect(container.querySelector("details")).toHaveTextContent("Needs checking:Workday application");
    expect(container.querySelector("details")).toHaveTextContent("Blocked:Cohere application");
    expect(message.text).toBe(saved);
  });

  it("targets a single recheck and only unfinished outcomes for continuation", () => {
    const resume = vi.fn().mockResolvedValue(undefined);
    const original = useAppStore.getState().resumeLastTask;
    useAppStore.setState({ resumeLastTask: resume });
    const conversation = mountConversationDom();
    try {
      const { getByRole } = render(<MessageList messages={[baseMessage({ turnId: 'saved', resultStatus: 'unverified',
        failureCause: 'Missing confirmation', verificationOutcomes: [
          { id: 'done', label: 'Altus', status: 'pass' },
          { id: 'pending', label: 'Workday', status: 'unresolved' },
        ] })]} />, { container: conversation });
      fireEvent.click(getByRole('button', { name: 'Check Workday again' }));
      expect(resume).toHaveBeenLastCalledWith({ mode: 'recheck', outcomeIds: ['pending'], turnId: 'saved' });
      fireEvent.click(getByRole('button', { name: 'Continue unfinished work' }));
      expect(resume).toHaveBeenLastCalledWith({ mode: 'continue', outcomeIds: ['pending'], turnId: 'saved' });
    } finally { act(() => useAppStore.setState({ resumeLastTask: original })); }
  });

  it("keeps earlier outcomes and evidence available after a successful scoped check", () => {
    const conversation = mountConversationDom();
    const { container, getByRole } = render(<MessageList messages={[baseMessage({ resultStatus: 'success',
      verificationOutcomes: [{ id: 'done', label: 'Altus', status: 'pass', historical: true,
        evidence: [{ id: 'receipt', label: 'browser observation', reference: 'https://ats.example/candidate',
          detail: 'Application received', timestamp: 1793500000 }] },
      { id: 'pending', label: 'Workday', status: 'unresolved' }],
    })]} />, { container: conversation });
    expect(container).toHaveTextContent('Saved outcome checks');
    expect(container).toHaveTextContent('(earlier check)');
    expect(container).toHaveTextContent('Application received');
    expect(container).not.toHaveTextContent('Run failed');
    expect(container.querySelector('[role="alert"]')).toBeNull();
    expect(container.querySelector('a[href="https://ats.example/candidate"]')).not.toBeNull();
    expect(getByRole('button', { name: 'Continue unfinished work' })).toBeEnabled();
  });

  it.each([false, true])("shows a short note for an old verification footer (transcript: %s)", (transcript) => {
    const conversation = mountConversationDom();
    const saved = "Here is the reading.\n\n---\n_Required checks failed — 1 required criterion(s) unmet: Famous-person comparison: identify documented charts.: The answer admits the research is incomplete. (fix: Address the specific assignment criterion using fresh evidence.)_";
    const message = baseMessage({ text: saved, resultStatus: "unverified",
      failureCause: saved.split("\n_")[1].slice(0, -1),
      ...(transcript ? { blocks: [{ type: "narrative" as const, id: "n", text: saved }] } : {}) });
    const { container } = render(<MessageList messages={[message]} />, { container: conversation });
    expect(container).toHaveTextContent("Here is the reading.");
    expect(container).toHaveTextContent("I couldn't verify this part of the request: Famous-person comparison.");
    expect(container).not.toHaveTextContent("Required checks failed");
    expect(container).not.toHaveTextContent("fresh evidence");
    expect(message.text).toBe(saved);
  });

  it("offers a fresh allowance for an exhausted task budget without model settings", () => {
    useAppStore.setState({ meta: { resume_allowance_usd: 10 } });
    const conversation = mountConversationDom();
    const { container, queryByRole } = render(<MessageList messages={[
      baseMessage({ resultStatus: "error", statusLabel: "Error",
        failureCause: "This task's spending allocation is exhausted. Changing providers will not restore the allocation. Use kageha runtime budget root --limit-usd AMOUNT." }),
    ]} />, { container: conversation });
    expect(container).toHaveTextContent("--limit-usd AMOUNT");
    expect(queryByRole("button", { name: "Resume task" })).toBeEnabled();
    expect(queryByRole("button", { name: /Open Settings/ })).toBeNull();
  });

  it("disables the Resume task button while its resume request is submitted", () => {
    useAppStore.setState({ sessionId: "chat-1", sending: false, resumingSessionId: "chat-1" });
    const conversation = mountConversationDom();
    const { getByRole, queryByRole } = render(<MessageList messages={[
      baseMessage({ resultStatus: "interrupted", statusLabel: "Task paused", resume: { allowed: true, reason: null } }),
    ]} />, { container: conversation });
    expect(getByRole("button", { name: "Resuming…" })).toBeDisabled();
    expect(queryByRole("button", { name: "Resume task" })).toBeNull();
    act(() => useAppStore.setState({ resumingSessionId: null }));
    expect(getByRole("button", { name: "Resume task" })).toBeEnabled();
  });
  it("shows the server's reason when a checkpoint cannot be resumed", () => {
    useAppStore.setState({ sessionId: "chat-1", sending: false, resumingSessionId: null });
    const conversation = mountConversationDom();
    const reason = "This request has used all 40 of its model calls, so it cannot continue. Start a new request instead.";
    const { container, queryByRole } = render(<MessageList messages={[
      baseMessage({ resultStatus: "interrupted", statusLabel: "Task paused", resume: { allowed: false, reason } }),
    ]} />, { container: conversation });
    expect(container).toHaveTextContent(reason);
    expect(queryByRole("button", { name: "Resume task" })).toBeNull();
  });
  it("keeps local storage diagnostics out of the primary recovery action", () => {
    const conversation = mountConversationDom();
    const cause = "gemini is pinned and failed with no fallback: no such column: spent_baseline. Unpin it or pick another model.";
    const { container, getByRole, queryByRole } = render(<MessageList messages={[
      baseMessage({ text: `I couldn't complete that request because the run failed. Cause: ${cause}`,
        resultStatus: "error", statusLabel: "Error", failureCause: cause }),
    ]} />, { container: conversation });
    expect(container).toHaveTextContent("Your progress is saved. Resume to continue.");
    expect(getByRole("button", { name: "Resume task" })).toBeEnabled();
    expect(queryByRole("button", { name: /Open Settings/ })).toBeNull();
    const details = container.querySelector("details");
    expect(details).not.toHaveAttribute("open");
    expect(details).toHaveTextContent("spent_baseline");
  });

  it("keeps earlier output above a consumed steer and corrected output below it", () => {
    const conversation = mountConversationDom();
    const { container } = render(<MessageList messages={[
      baseMessage({ id: "request", role: "user", text: "Find jobs", turnId: "turn-1" }),
      baseMessage({ id: "answer", text: "First shortlistCorrected shortlist", turnId: "turn-1",
        blocks: [{ type: "narrative", id: "n", text: "First shortlistCorrected shortlist" }] }),
      baseMessage({ id: "steer", role: "user", text: "No data engineer job", steering: "applied", turnId: "turn-1",
        steeringBoundary: { blockCount: 1, tailTextLength: "First shortlist".length } }),
    ]} />, { container: conversation });
    const rows = container.querySelectorAll("article");
    expect(rows[1]).toHaveTextContent("First shortlist");
    expect(rows[2]).toHaveTextContent("No data engineer job");
    expect(rows[2]).toHaveTextContent("Update received");
    expect(rows[3]).toHaveTextContent("Corrected shortlist");
    expect(container).not.toHaveTextContent("continuing with your update");
  });

  it("does not claim unapplied steering is still waiting after the turn ends", () => {
    const conversation = mountConversationDom();
    const { container } = render(<MessageList messages={[
      baseMessage({ role: "user", steering: "pending", text: "Correction", turnId: "finished" }),
    ]} />, { container: conversation });
    expect(container).toHaveTextContent("Update was not applied");
    expect(container).not.toHaveTextContent("Update waiting");
  });

  it("shows pending steering as waiting only for its active turn", () => {
    useAppStore.setState({ sending: true });
    const conversation = mountConversationDom();
    const { container } = render(<MessageList messages={[
      baseMessage({ id: "old", role: "user", steering: "pending", text: "Old correction", turnId: "old" }),
      baseMessage({ id: "active", streaming: true, text: "Working", turnId: "active" }),
      baseMessage({ id: "steer", role: "user", steering: "pending", text: "Current correction", turnId: "active" }),
    ]} />, { container: conversation });
    const rows = container.querySelectorAll("article");
    expect(rows[0]).toHaveTextContent("Update was not applied");
    expect(rows[2]).toHaveTextContent("Current correction");
    expect(rows[2]).toHaveTextContent("Update waiting to be applied");
  });

  it("keeps re-snapping to the true bottom while content is still settling after load", async () => {
    const conversation = mountConversationDom();
    // Start short: as if mermaid/markdown/images haven't finished growing
    // #messages yet on the first paint.
    stubScrollMetrics(conversation, { scrollHeight: 400, clientHeight: 400, scrollTop: 0 });

    render(<MessageList messages={[baseMessage()]} />);

    // First settle tick (queued via requestAnimationFrame).
    await act(async () => {
      await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
    });
    expect(conversation.scrollTop).toBe(400);

    // Content keeps growing on later frames (late image/diagram layout) —
    // the settle loop must keep re-snapping instead of stopping after one
    // frame, so the final resting scrollTop reflects the true bottom.
    stubScrollMetrics(conversation, { scrollHeight: 900, clientHeight: 400 });
    await act(async () => {
      await new Promise((r) =>
        requestAnimationFrame(() => requestAnimationFrame(() => requestAnimationFrame(r))),
      );
    });
    expect(conversation.scrollTop).toBe(900);
  });

  it("lets a genuine scroll-up detach from the bottom even while a response is streaming", async () => {
    const conversation = mountConversationDom();
    stubScrollMetrics(conversation, { scrollHeight: 1000, clientHeight: 400, scrollTop: 600 });

    const { rerender } = render(
      <MessageList messages={[baseMessage({ id: "m1", text: "start", streaming: true })]} />,
    );
    await act(async () => {
      await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
    });

    // Streaming grows the content — this is what re-arms the programmatic
    // guard window on every chunk in the real bug.
    stubScrollMetrics(conversation, { scrollHeight: 1200, clientHeight: 400 });
    rerender(
      <MessageList
        messages={[baseMessage({ id: "m1", text: "start more tokens streaming in", streaming: true })]}
      />,
    );
    await act(async () => {
      await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
    });
    // Still following the bottom while the user hasn't touched the scroll.
    expect(conversation.scrollTop).toBe(1200);

    // User deliberately scrolls up, well past mobile rubber-band jitter,
    // immediately after an autoscroll (i.e. squarely inside the
    // programmatic guard window) — this must register as real intent. A
    // real scroll gesture is touchstart at the OLD position followed by a
    // scroll event landing at the NEW (scrolled-up) position — exactly like
    // a real swipe, and the only sequence the fix should trust as
    // deliberate (see gestureStartScrollTop in MessageList.tsx).
    act(() => {
      conversation.dispatchEvent(new Event("touchstart"));
    });
    stubScrollMetrics(conversation, {
      scrollHeight: 1200,
      clientHeight: 400,
      scrollTop: 400,
    });
    act(() => {
      conversation.dispatchEvent(new Event("scroll"));
    });

    // More streamed content arrives. If the scroll-up wasn't honored, the
    // view would snap back to the bottom here, hiding the new content
    // instead of leaving it below the user's current read position. Only
    // scrollHeight/clientHeight simulate the external content growth —
    // scrollTop is intentionally left alone so the component's own logic
    // (not the test) decides where it ends up.
    stubScrollMetrics(conversation, { scrollHeight: 1400, clientHeight: 400 });
    rerender(
      <MessageList
        messages={[
          baseMessage({
            id: "m1",
            text: "start more tokens streaming in and even more after that",
            streaming: true,
          }),
        ]}
      />,
    );
    await act(async () => {
      await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
    });

    expect(conversation.scrollTop).toBe(400);
  });

  it("still re-latches to the bottom when the user is already at (or returns to) the bottom", async () => {
    const conversation = mountConversationDom();
    stubScrollMetrics(conversation, { scrollHeight: 1000, clientHeight: 400, scrollTop: 600 });
    render(<MessageList messages={[baseMessage({ streaming: true })]} />);
    await act(async () => {
      await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
    });

    // A tiny jitter (mobile rubber-banding) right after an autoscroll must
    // NOT be treated as a deliberate scroll-up.
    stubScrollMetrics(conversation, { scrollHeight: 1000, clientHeight: 400, scrollTop: 598 });
    act(() => {
      conversation.dispatchEvent(new Event("scroll"));
    });

    stubScrollMetrics(conversation, { scrollHeight: 1300, clientHeight: 400 });
    act(() => {
      resizeCallbacks.forEach((cb) => cb());
    });

    expect(conversation.scrollTop).toBe(1300);
  });

  it("recovers delayed rollback after jumping during thinking without another token", async () => {
    const conversation = mountConversationDom();
    stubScrollMetrics(conversation, { scrollHeight: 2000, clientHeight: 400, scrollTop: 1600 });
    useAppStore.setState({ sending: true, runStatus: "running" });
    render(<MessageList messages={[baseMessage({ streaming: true })]} />);
    await act(async () => {
      for (let i = 0; i < 8; i++) await new Promise(requestAnimationFrame);
    });
    act(() => {
      conversation.dispatchEvent(new WheelEvent("wheel", { deltaY: -100 }));
      conversation.scrollTop = 400;
      conversation.dispatchEvent(new Event("scroll"));
    });
    const jump = Array.from(document.querySelectorAll("button")).find(
      (button) => button.textContent?.includes("Jump to latest"),
    );
    expect(jump).toBeTruthy();
    act(() => jump!.click());
    await act(async () => {
      for (let i = 0; i < 8; i++) await new Promise(requestAnimationFrame);
    });
    // No resize/new token; the settle loop and programmatic guard have ended.
    vi.spyOn(Date, "now").mockReturnValue(Date.now() + 2000);
    act(() => {
      conversation.scrollTop = 0;
      conversation.dispatchEvent(new Event("scroll"));
    });
    expect(conversation.scrollTop).toBe(2000);
    expect(document.body.textContent).not.toContain("Jump to latest");
    act(() => {
      conversation.dispatchEvent(new WheelEvent("wheel", { deltaY: -100 }));
      conversation.scrollTop = 400;
      conversation.dispatchEvent(new Event("scroll"));
    });
    expect(conversation.scrollTop).toBe(400);
    expect(document.body.textContent).toContain("Jump to latest");
  });

  it("does not snap back after Jump to latest just because a layout-driven scroll event fires (no real gesture)", async () => {
    // Reported bug: tapping "Jump to latest" worked, then the view
    // "suddenly" landed back in the middle of the chat on its own. Root
    // cause was inferring user intent from the scrollTop delta alone, which
    // a non-gesture layout shift (shrinking scrollHeight, keyboard resize,
    // etc.) can mimic with zero actual user interaction.
    const conversation = mountConversationDom();
    stubScrollMetrics(conversation, { scrollHeight: 1000, clientHeight: 400, scrollTop: 300 });
    render(<MessageList messages={[baseMessage()]} />);
    // Let the initial-load settle loop finish (it polls for a few stable
    // frames) before simulating user interaction, matching a real page
    // that has already finished landing on load.
    await act(async () => {
      for (let i = 0; i < 8; i++) {
        await new Promise((r) => requestAnimationFrame(r));
      }
    });

    // User was scrolled up (not at the bottom) — jump button state is
    // internal (showJump), so simulate the effect of having scrolled up via
    // a real gesture + scroll event, exactly like the earlier detach test:
    // touchstart at the old (bottom) position, then a scroll event landing
    // at the new, scrolled-up position.
    act(() => {
      conversation.dispatchEvent(new Event("touchstart"));
    });
    stubScrollMetrics(conversation, { scrollHeight: 1000, clientHeight: 400, scrollTop: 300 });
    act(() => {
      conversation.dispatchEvent(new Event("scroll"));
    });

    // Tap "Jump to latest" — this is a click on a separate portaled button,
    // not a gesture on the scroller itself, so it must not be mistaken for
    // (or accidentally require) a user-gesture scroll-up.
    stubScrollMetrics(conversation, { scrollHeight: 1000, clientHeight: 400 });
    const jumpBtn = Array.from(document.querySelectorAll(".ka-chat-jump")).find(
      (b) => b.textContent?.includes("Jump to latest"),
    ) as HTMLButtonElement | undefined;
    expect(jumpBtn).toBeTruthy();
    act(() => {
      jumpBtn!.click();
    });
    // jumpToLatest now settles across animation frames (so late-growing
    // content can't strand the view mid-thread); await the first snap.
    await act(async () => {
      await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
    });
    expect(conversation.scrollTop).toBe(1000);

    // Immediately after, a purely layout-driven scroll event fires (e.g. the
    // browser clamping scrollTop after a reflow) with NO preceding touch/
    // wheel/pointer gesture — this must NOT be able to detach stickToBottom
    // and pull the view back up.
    stubScrollMetrics(conversation, { scrollHeight: 1000, clientHeight: 400, scrollTop: 550 });
    act(() => {
      conversation.dispatchEvent(new Event("scroll"));
    });

    // A subsequent resize (more content arriving) must still re-snap to the
    // true bottom, proving stickToBottom was never falsely detached.
    stubScrollMetrics(conversation, { scrollHeight: 1500, clientHeight: 400 });
    act(() => {
      resizeCallbacks.forEach((cb) => cb());
    });
    expect(conversation.scrollTop).toBe(1500);
  });

  it("keeps following the latest reply on its own when the user never actually scrolls, even while tapping things", async () => {
    // Reported bug: with no interaction with "Jump to latest" at all, just
    // asking a question, the reply stopped following to the bottom on its
    // own. Root cause was treating touchstart/pointerdown ALONE as "the
    // user scrolled up" — but a tap fires touchstart on almost any
    // interaction in the chat area (expanding "Thinking…", tapping a link),
    // not just an actual scroll gesture. A tap that never moves the scroll
    // position must not detach stickToBottom.
    const conversation = mountConversationDom();
    stubScrollMetrics(conversation, { scrollHeight: 1000, clientHeight: 400, scrollTop: 1000 });
    render(
      <MessageList messages={[baseMessage({ id: "m1", text: "start", streaming: true })]} />,
    );
    await act(async () => {
      for (let i = 0; i < 8; i++) await new Promise((r) => requestAnimationFrame(r));
    });

    // User taps to expand the "Thinking…" block (or any other tap) — this
    // fires touchstart/pointerdown on the scroller. A moment later, while
    // still within the tap's gesture window, a layout reflow (the block
    // expanding) transiently pushes scrollTop away from the bottom before
    // the next auto-snap catches up — with zero directional swipe from the
    // user. Under the old (gesture-alone) check this looked identical to a
    // deliberate scroll-up and permanently killed auto-follow.
    act(() => {
      conversation.dispatchEvent(new Event("touchstart"));
      conversation.dispatchEvent(new Event("pointerdown"));
    });
    stubScrollMetrics(conversation, { scrollHeight: 1600, clientHeight: 400, scrollTop: 1000 });
    act(() => {
      conversation.dispatchEvent(new Event("scroll"));
    });

    // The reply keeps streaming and growing — this must still auto-follow
    // to the true bottom, exactly as if the tap never happened.
    stubScrollMetrics(conversation, { scrollHeight: 1400, clientHeight: 400 });
    act(() => {
      resizeCallbacks.forEach((cb) => cb());
    });
    expect(conversation.scrollTop).toBe(1400);

    stubScrollMetrics(conversation, { scrollHeight: 1800, clientHeight: 400 });
    act(() => {
      resizeCallbacks.forEach((cb) => cb());
    });
    expect(conversation.scrollTop).toBe(1800);
  });

  it("re-snaps to the true bottom when content keeps growing after Jump to latest", async () => {
    // Reported bug: tapping "Jump to latest" lands at the bottom for a moment
    // and then drifts back to the middle. Root cause: a single snap on tap,
    // while content below the fold (images, diagrams, a freshly rendered
    // failure banner) keeps growing #messages over the next several frames —
    // leaving the view short of the true bottom. jumpToLatest must settle
    // across frames like initial load, re-snapping until height stabilizes.
    const conversation = mountConversationDom();
    stubScrollMetrics(conversation, { scrollHeight: 1000, clientHeight: 400, scrollTop: 300 });
    render(<MessageList messages={[baseMessage()]} />);
    await act(async () => {
      for (let i = 0; i < 8; i++) await new Promise((r) => requestAnimationFrame(r));
    });

    // Simulate the user having scrolled up via a real gesture.
    act(() => {
      conversation.dispatchEvent(new Event("touchstart"));
    });
    stubScrollMetrics(conversation, { scrollHeight: 1000, clientHeight: 400, scrollTop: 300 });
    act(() => {
      conversation.dispatchEvent(new Event("scroll"));
    });

    const jumpBtn = Array.from(document.querySelectorAll(".ka-chat-jump")).find(
      (b) => b.textContent?.includes("Jump to latest"),
    ) as HTMLButtonElement | undefined;
    expect(jumpBtn).toBeTruthy();

    // Tap jump: snaps to the current bottom (1000)...
    stubScrollMetrics(conversation, { scrollHeight: 1000, clientHeight: 400 });
    act(() => {
      jumpBtn!.click();
    });
    await act(async () => {
      await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
    });
    expect(conversation.scrollTop).toBe(1000);

    // ...then content grows on later frames (late image/diagram/banner
    // layout). The settle loop must keep re-snapping to the new true bottom
    // instead of leaving the view stranded at 1000 (the middle).
    stubScrollMetrics(conversation, { scrollHeight: 1800, clientHeight: 400 });
    await act(async () => {
      for (let i = 0; i < 6; i++) await new Promise((r) => requestAnimationFrame(r));
    });
    expect(conversation.scrollTop).toBe(1800);
  });

  it("re-settles to the bottom after iOS restores an old scroll position on bfcache resume", async () => {
    // Reported bug (Chrome on iPhone, which is WebKit under the hood): the
    // chat always opens scrolled up, requiring a manual scroll-down every
    // time. Root cause is iOS's own scroll restoration firing on
    // pageshow(persisted) / tab-resume AFTER our initial-scroll effect
    // already landed at the true bottom, silently clobbering it back to an
    // old position. Only a listener on pageshow/visibilitychange can catch
    // and correct this, since it happens outside of any React render.
    const conversation = mountConversationDom();
    stubScrollMetrics(conversation, { scrollHeight: 1000, clientHeight: 400, scrollTop: 0 });
    render(<MessageList messages={[baseMessage()]} />);
    await act(async () => {
      await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
    });
    expect(conversation.scrollTop).toBe(1000);

    // iOS silently restores the old scrollTop outside of React, then fires
    // pageshow with persisted=true (a true bfcache restore).
    stubScrollMetrics(conversation, { scrollHeight: 1000, clientHeight: 400, scrollTop: 20 });
    await act(async () => {
      window.dispatchEvent(
        Object.assign(new Event("pageshow"), { persisted: true }),
      );
      await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
    });
    expect(conversation.scrollTop).toBe(1000);

    // Same clobber can happen on a plain tab-resume (visibilitychange),
    // e.g. the tab was backgrounded and iOS reloaded it without a full
    // bfcache restore.
    stubScrollMetrics(conversation, { scrollHeight: 1000, clientHeight: 400, scrollTop: 20 });
    Object.defineProperty(document, "visibilityState", {
      configurable: true,
      get: () => "visible",
    });
    await act(async () => {
      document.dispatchEvent(new Event("visibilitychange"));
      await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
    });
    expect(conversation.scrollTop).toBe(1000);
  });

  it("does not fight a genuine scroll-up just because pageshow/visibilitychange fires", async () => {
    // The bfcache/resume re-settle must only re-assert the bottom while the
    // user is actually stuck there (stickToBottom) — it must never yank the
    // view back down if the user had deliberately scrolled up before
    // backgrounding the tab.
    const conversation = mountConversationDom();
    stubScrollMetrics(conversation, { scrollHeight: 1000, clientHeight: 400, scrollTop: 0 });
    render(<MessageList messages={[baseMessage()]} />);
    await act(async () => {
      await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
    });

    act(() => {
      conversation.dispatchEvent(new Event("touchstart"));
    });
    stubScrollMetrics(conversation, { scrollHeight: 1000, clientHeight: 400, scrollTop: 100 });
    act(() => {
      conversation.dispatchEvent(new Event("scroll"));
    });

    stubScrollMetrics(conversation, { scrollHeight: 1000, clientHeight: 400, scrollTop: 100 });
    await act(async () => {
      window.dispatchEvent(
        Object.assign(new Event("pageshow"), { persisted: true }),
      );
      await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
    });
    expect(conversation.scrollTop).toBe(100);
  });

  it("does not get stranded at the top when a run's activity collapses right after the user manually scrolled to the bottom", async () => {
    // Reported bug: after manually scrolling down to the latest message
    // (e.g. following a tool run), finishing the run collapses its
    // activity/transcript (a "Thinking…" block, tool call cards) — a large
    // shrink of #messages. Browsers clamp an out-of-range scrollTop toward
    // 0 as part of that reflow, which our own onScroll handler can then
    // misread as a deliberate scroll-up (it's not inside the programmatic
    // guard window, since we didn't cause it) and permanently detach
    // stickToBottom, stranding the view at the top with no auto-recovery.
    const conversation = mountConversationDom();
    stubScrollMetrics(conversation, { scrollHeight: 5000, clientHeight: 400, scrollTop: 2000 });
    render(
      <MessageList messages={[baseMessage({ id: "m1", text: "running", streaming: true })]} />,
    );
    await act(async () => {
      await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
    });

    // User deliberately scrolls all the way down to the latest message
    // (real gesture: touchstart at the old position, then scroll landing at
    // the true bottom).
    act(() => {
      conversation.dispatchEvent(new Event("touchstart"));
    });
    stubScrollMetrics(conversation, { scrollHeight: 5000, clientHeight: 400, scrollTop: 4600 });
    act(() => {
      conversation.dispatchEvent(new Event("scroll"));
    });
    expect(conversation.scrollTop).toBe(4600);

    // The run finishes; its activity collapses, shrinking #messages a lot.
    // The BROWSER (not our code) clamps scrollTop toward the new, much
    // smaller max as part of that reflow and dispatches its own "scroll"
    // event for that clamp BEFORE our ResizeObserver callback runs (a
    // realistic ordering — native scroll clamping happens synchronously
    // during layout, while ResizeObserver callbacks are batched to run
    // after). That clamp-driven scroll event has no preceding user gesture
    // and lands outside the programmatic guard window, so onScroll must not
    // treat it as a deliberate scroll-up and detach stickToBottom before the
    // ResizeObserver callback gets a chance to correct it.
    stubScrollMetrics(conversation, { scrollHeight: 600, clientHeight: 400, scrollTop: 0 });
    act(() => {
      conversation.dispatchEvent(new Event("scroll"));
    });
    act(() => {
      resizeCallbacks.forEach((cb) => cb());
    });

    // The view must recover to the true (new, smaller) bottom on its own —
    // the user was following along, not reading history, so a content
    // collapse must not leave them stuck at the top.
    expect(conversation.scrollTop).toBe(600);
    expect(document.querySelector(".ka-chat-jump")?.textContent).not.toContain(
      "Jump to latest",
    );
  });

  it("a small phone swipe cancels an already queued stream frame and stays put through momentum", async () => {
    const conversation = mountConversationDom();
    stubScrollMetrics(conversation, { scrollHeight: 1000, clientHeight: 400, scrollTop: 600 });
    const { rerender } = render(<MessageList messages={[baseMessage({ streaming: true })]} />);
    await act(async () => {
      for (let i = 0; i < 8; i++) await new Promise(requestAnimationFrame);
    });
    // Use the real bottom position (jsdom does not clamp scrollTop natively).
    conversation.scrollTop = 600;
    rerender(<MessageList messages={[baseMessage({ text: "another chunk", streaming: true })]} />);
    act(() => {
      conversation.dispatchEvent(Object.assign(new Event("touchstart"), {
        touches: [{ clientX: 100, clientY: 100 }],
      }));
      conversation.dispatchEvent(Object.assign(new Event("touchmove"), {
        touches: [{ clientX: 102, clientY: 130 }],
      }));
      // A resize can fire before the native scroll event for that swipe.
      resizeCallbacks.forEach(cb => cb());
    });
    expect(conversation.scrollTop).toBe(600);
    act(() => {
      conversation.scrollTop = 570;
      conversation.dispatchEvent(new Event("scroll"));
      conversation.dispatchEvent(new Event("touchend"));
    });
    await act(async () => { await new Promise(requestAnimationFrame); });
    expect(conversation.scrollTop).toBe(570);
    // Streaming, keyboard resize, and final markdown must all respect history.
    stubScrollMetrics(conversation, { scrollHeight: 1400, clientHeight: 200 });
    rerender(<MessageList messages={[baseMessage({ text: "finished answer", streaming: false })]} />);
    act(() => { resizeCallbacks.forEach(cb => cb()); });
    await act(async () => { await new Promise(requestAnimationFrame); });
    expect(conversation.scrollTop).toBe(570);
    expect(document.querySelector(".ka-chat-jump")?.textContent).toContain("Jump to latest");
  });

  it("a tap followed by delayed content growth does not disable auto-follow", async () => {
    const conversation = mountConversationDom();
    stubScrollMetrics(conversation, { scrollHeight: 1000, clientHeight: 400, scrollTop: 600 });
    render(<MessageList messages={[baseMessage({ streaming: true })]} />);
    await act(async () => {
      for (let i = 0; i < 8; i++) await new Promise(requestAnimationFrame);
    });
    conversation.scrollTop = 600;
    vi.spyOn(Date, "now").mockReturnValue(Date.now() + 1000);
    act(() => { conversation.dispatchEvent(new Event("touchstart")); });
    stubScrollMetrics(conversation, { scrollHeight: 1500, clientHeight: 400 });
    act(() => {
      conversation.dispatchEvent(new Event("scroll"));
      resizeCallbacks.forEach(cb => cb());
    });
    expect(conversation.scrollTop).toBe(1500);
  });

  it("resumes streaming follow when manually scrolling back to the actual bottom", async () => {
    const conversation = mountConversationDom();
    stubScrollMetrics(conversation, { scrollHeight: 1000, clientHeight: 400, scrollTop: 600 });
    render(<MessageList messages={[baseMessage({ streaming: true })]} />);
    await act(async () => {
      for (let i = 0; i < 8; i++) await new Promise(requestAnimationFrame);
    });
    conversation.scrollTop = 600;
    act(() => {
      conversation.dispatchEvent(new WheelEvent("wheel", { deltaY: -60 }));
      conversation.scrollTop = 540;
      conversation.dispatchEvent(new Event("scroll"));
    });
    act(() => {
      conversation.dispatchEvent(new WheelEvent("wheel", { deltaY: 60 }));
      conversation.scrollTop = 600;
      conversation.dispatchEvent(new Event("scroll"));
    });
    stubScrollMetrics(conversation, { scrollHeight: 1300, clientHeight: 250 });
    act(() => { resizeCallbacks.forEach(cb => cb()); });
    expect(conversation.scrollTop).toBe(1300);
  });

  it("horizontal swipes across code do not stop vertical auto-follow", async () => {
    const conversation = mountConversationDom();
    stubScrollMetrics(conversation, { scrollHeight: 1000, clientHeight: 400, scrollTop: 600 });
    render(<MessageList messages={[baseMessage({ streaming: true })]} />);
    await act(async () => { await new Promise(requestAnimationFrame); });
    act(() => {
      conversation.dispatchEvent(Object.assign(new Event("touchstart"), {
        touches: [{ clientX: 100, clientY: 100 }],
      }));
      conversation.dispatchEvent(Object.assign(new Event("touchmove"), {
        touches: [{ clientX: 180, clientY: 120 }],
      }));
    });
    stubScrollMetrics(conversation, { scrollHeight: 1200, clientHeight: 400 });
    act(() => { resizeCallbacks.forEach(cb => cb()); });
    expect(conversation.scrollTop).toBe(1200);
  });

  it("Jump to latest immediately snaps to bottom and clears active trackpad/wheel gestures so trailing scroll events cannot detach", async () => {
    const conversation = mountConversationDom();
    stubScrollMetrics(conversation, { scrollHeight: 2000, clientHeight: 400, scrollTop: 1600 });
    render(<MessageList messages={[baseMessage({ text: "initial message" })]} />);
    await act(async () => {
      for (let i = 0; i < 8; i++) {
        await new Promise((r) => requestAnimationFrame(r));
      }
    });

    // Simulate trackpad wheel up to an earlier message (Campaign 4)
    act(() => {
      conversation.dispatchEvent(Object.assign(new Event("wheel"), { deltaY: -50, deltaX: 0 }));
    });
    stubScrollMetrics(conversation, { scrollHeight: 2000, clientHeight: 400, scrollTop: 600 });
    act(() => {
      conversation.dispatchEvent(new Event("scroll"));
    });

    // Jump button is visible
    const jumpBtn = Array.from(document.querySelectorAll(".ka-chat-jump")).find(
      (b) => b.textContent?.includes("Jump to latest"),
    ) as HTMLButtonElement | undefined;
    expect(jumpBtn).toBeTruthy();

    // Click Jump to latest
    act(() => {
      jumpBtn!.click();
    });

    // Immediately snaps to the bottom synchronously
    expect(conversation.scrollTop).toBe(2000);

    // Trailing scroll event arrives shortly after while old gesture window would have been active
    stubScrollMetrics(conversation, { scrollHeight: 2000, clientHeight: 400, scrollTop: 1600 });
    act(() => {
      conversation.dispatchEvent(new Event("scroll"));
    });

    // Content grows (new tokens arriving) — must continue sticking to the bottom, not snapped back
    stubScrollMetrics(conversation, { scrollHeight: 2500, clientHeight: 400 });
    act(() => {
      resizeCallbacks.forEach((cb) => cb());
    });
    expect(conversation.scrollTop).toBe(2500);
  });

});

