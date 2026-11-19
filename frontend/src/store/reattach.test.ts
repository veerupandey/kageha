import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { replayStreamFrames, applyStreamFrameToMessage } from "../lib/transcript";
import { applyLiveSessionSnapshot, emptyRun, syncFromRun } from "./helpers";

import { reattachToActiveTurn, applyLiveMessage, pendingApprovalFromFrame, terminalRunState, turnScopedFinalText } from "./reattach";

describe("Codex-style reconnect state", () => {
  it("keeps the credential label when replaying a generic approval frame", () => {
    expect(pendingApprovalFromFrame({
      kind: "approval_required", label: "Waiting for approval…",
      payload: { approval_id: "credential:1", action: "request_credential", label: "account password", secret: true },
    }, "session-1")).toMatchObject({ label: "account password", secret: true });
  });
  it("restores an approval from replayed event history", () => {
    expect(
      pendingApprovalFromFrame(
        {
          kind: "approval_required",
          label: "Waiting for approval…",
          payload: {
            approval_id: "approval-1",
            action: "bash_elevated",
            risk_class: "shell_elevated",
            detail: "Run outside the sandbox",
          },
        },
        "session-1",
      ),
    ).toEqual({
      approval_id: "approval-1",
      sessionId: "session-1",
      action: "bash_elevated",
      risk_class: "shell_elevated",
      detail: "Run outside the sandbox",
      label: "Waiting for approval…",
    });
  });

  it("does not present an interrupted turn as successful", () => {
    expect(terminalRunState("reconciliation_required")).toEqual({
      status: "interrupted",
      label: "Interrupted · review before resuming",
    });
    expect(terminalRunState("blocked").status).toBe("interrupted");
    expect(terminalRunState("complete").status).toBe("success");
  });

  it("never labels an early stop as Done", () => {
    expect(terminalRunState("no_progress")).toEqual({
      status: "interrupted",
      label: "Stopped · stalled, no progress",
    });
    expect(terminalRunState("max_steps").label).toBe("Stopped · step limit reached");
    expect(terminalRunState("budget").label).toBe("Stopped · spend limit reached");
    expect(terminalRunState("success").label).toBe("Done");
  });

  it("forces Cancelled when Stop raced finalize", () => {
    expect(terminalRunState("complete", { cancelled: true })).toEqual({
      status: "cancelled",
      label: "Cancelled",
    });
  });

  it("applies session live_message onto the in-flight assistant narrative", () => {
    const next = applyLiveMessage(
      { id: "a1", role: "assistant", text: "Hel", streaming: true },
      "Hello from the live buffer",
    );
    expect(next.text).toBe("Hello from the live buffer");
    expect(next.blocks?.some((block) => block.type === "narrative")).toBe(true);
  });

  it("finalize of turn A does not pick a later assistant's text", () => {
    const turnA = { role: "assistant", text: "Continuing the browser tour…" };
    const turnB = { role: "assistant", text: "4" };
    const sessionMessages = [turnA, turnB];
    const lastAssistant = [...sessionMessages].reverse().find((m) => m.role === "assistant");
    expect(lastAssistant?.text).toBe("4");
    expect(turnScopedFinalText(turnA.text, sessionMessages)).toBe(
      "Continuing the browser tour…",
    );
  });
});

describe("refresh with a stale live buffer", () => {
  const frames = [
    { kind: "progress", payload: { source_kind: "assistant_narrative", text: "Checking login.", offset: 0 } },
    { kind: "tool_started", card: { id: "c1", name: "bash", status: "running" as const } },
    { kind: "progress", payload: { source_kind: "assistant_narrative", text: "Now testing the proxy.", offset: "Checking login.".length } },
    { kind: "tool_started", card: { id: "c2", name: "bash", status: "running" as const } },
  ];
  function replayed() {
    return replayStreamFrames({ id: "a1", role: "assistant", text: "", streaming: true }, frames);
  }

  it("keeps text before its commands when polling returns an older buffer", () => {
    const message = replayed();
    const restored = applyLiveMessage(message, "Checking login.");
    expect(restored).toBe(message);
    expect(restored.blocks?.map(b => b.type)).toEqual(["narrative", "shell", "narrative", "shell"]);
    const next = applyStreamFrameToMessage(restored, { kind: "progress", payload: { source_kind: "assistant_narrative", text: "Verified.", offset: "Checking login.Now testing the proxy.".length } });
    expect(next.text).toBe("Checking login.Now testing the proxy.Verified.");
  });

  it("also preserves replayed order during background session recovery", () => {
    const run = emptyRun("s1", "t1");
    run.messages = [replayed()];
    const restored = applyLiveSessionSnapshot(run, { live_message: "Checking login." }, "s1");
    expect(restored.messages[0]).toBe(run.messages[0]);
  });

  it("rejects a longer divergent buffer but accepts a matching new suffix", () => {
    const message = replayed();
    expect(applyLiveMessage(message, "Older text that does not match the durable narration and is longer.")).toBe(message);
    const extended = applyLiveMessage(message, message.text + "Waiting for the result.");
    expect(extended.blocks?.map(b => b.type)).toEqual(["narrative", "shell", "narrative", "shell", "narrative"]);
    expect(extended.text).toBe(message.text + "Waiting for the result.");
  });
});


vi.mock("../api/stream", async (original) => ({
  ...await original<typeof import("../api/stream")>(), streamSessionEvents: vi.fn(),
}));
vi.mock("../api/client", async (original) => ({
  ...await original<typeof import("../api/client")>(), api: vi.fn(),
}));

import { streamSessionEvents } from "../api/stream";
import { api } from "../api/client";
import type { AppState } from "./types";

function reconnectState() {
  const state = {
    sessionId: "speed-session", threadId: "speed-thread",
    runs: { "speed-session": emptyRun("speed-session", "speed-thread") },
    pendingApproval: null,
  } as unknown as AppState;
  const deps = {
    get: () => state,
    set: (patch: unknown) => Object.assign(state, typeof patch === "function" ? patch(state) : patch),
    updateRun: (id: string, change: (run: ReturnType<typeof emptyRun>) => ReturnType<typeof emptyRun>) => {
      state.runs[id] = change(state.runs[id]);
      Object.assign(state, syncFromRun(state.runs[id]));
    },
  };
  return { state, deps };
}

describe("fast reattachment", () => {
  afterEach(() => { vi.resetAllMocks(); vi.clearAllTimers(); vi.useRealTimers(); });
  it("clears reconnecting on a healthy snapshot even without new events", () => {
    const { state, deps } = reconnectState();
    let deliver!: Parameters<typeof streamSessionEvents>[1];
    vi.mocked(streamSessionEvents).mockImplementation(async (_url, callback, signal) => {
      deliver = callback;
      await new Promise<void>((resolve) => signal.addEventListener("abort", () => resolve(), { once: true }));
    });
    const stop = reattachToActiveTurn(deps, "speed-session", "speed-thread", "turn-speed");
    expect(state.statusLabel).toBe("Reconnecting…");
    deliver({ events: [], live: { active_turn: { turn_id: "turn-speed", phase: "planning" } } });
    expect(state.statusLabel).toBe("Planning…");
    expect(state.messages.at(-1)?.statusLabel).toBe("Planning…");
    expect(state.runs["speed-session"].transport?.lastFrameAt).toBeGreaterThan(0);
    stop();
  });
  it("keeps budget telemetry out of reconnect activity while advancing the cursor", () => {
    const { state, deps } = reconnectState();
    let deliver!: Parameters<typeof streamSessionEvents>[1];
    vi.mocked(streamSessionEvents).mockImplementation(async (_url, callback, signal) => {
      deliver = callback;
      await new Promise<void>((resolve) => signal.addEventListener("abort", () => resolve(), { once: true }));
    });
    const stop = reattachToActiveTurn(deps, "speed-session", "speed-thread", "turn-speed");
    const live = { active_turn: { turn_id: "turn-speed" } };
    deliver({ events: [{ sequence: 1, kind: "progress", label: "Searching" }], live });
    const before = state.messages.at(-1)!;
    deliver({ events: [
      { sequence: 2, kind: "request_budget", label: "Budget update", interesting: true,
        tool_card: { id: "budget", name: "request_budget", status: "running" } },
      { sequence: 3, kind: "progress", label: "Another budget update",
        payload: { source_kind: "request_budget" } },
    ], live });
    const run = state.runs["speed-session"];
    const message = run.messages.at(-1)!;
    expect(run.statusLabel).toBe("Searching");
    expect(message.statusLabel).toBe("Searching");
    expect(message.steps).toEqual(before.steps);
    expect(message.blocks).toEqual(before.blocks);
    expect(message.toolCards).toEqual(before.toolCards);
    expect(message.streamCursor).toEqual({ turnId: "turn-speed", sequence: 3 });
    expect(run.transport?.lastSequence).toBe(3);
    deliver({ events: [{ sequence: 4, kind: "progress", label: "Reviewing" }], live });
    expect(state.runs["speed-session"].statusLabel).toBe("Reviewing");
    expect(state.messages.at(-1)?.steps?.map((step) => step.label)).toEqual(["Searching", "Reviewing"]);
    stop();
  });
  it("streams narrative deltas and resolves approvals without polling", async () => {
    const { state, deps } = reconnectState();
    let deliver!: Parameters<typeof streamSessionEvents>[1];
    vi.mocked(streamSessionEvents).mockImplementation(async (_url, callback, signal) => {
      deliver = callback;
      await new Promise<void>((resolve) => signal.addEventListener("abort", () => resolve(), { once: true }));
    });
    const stop = reattachToActiveTurn(deps, "speed-session", "speed-thread", "turn-speed");
    const approval = { approval_id: "speed-approval", action: "request_secure_login" };
    deliver({ events: [{ sequence: 1, kind: "approval_required", payload: approval }], live: {
      active_turn: { turn_id: "turn-speed" }, pending_approval: approval, live_message: "Hello",
    } });
    expect(state.pendingApproval?.approval_id).toBe("speed-approval");
    deliver({ events: [{ sequence: 2, kind: "approval_resolved", payload: { approval_id: "speed-approval" } }], live: {
      active_turn: { turn_id: "turn-speed" }, pending_approval: null, live_message_delta: " world",
    } });
    expect(state.pendingApproval).toBeNull();
    expect(state.messages.at(-1)?.text).toBe("Hello world");
    expect(api).not.toHaveBeenCalled();
    stop();
  });
  it("retains an event-only approval across chat switching and later narrative frames", async () => {
    const { state, deps } = reconnectState();
    let deliver!: Parameters<typeof streamSessionEvents>[1];
    vi.mocked(streamSessionEvents).mockImplementation(async (_url, callback, signal) => {
      deliver = callback;
      await new Promise<void>((resolve) => signal.addEventListener("abort", () => resolve(), { once: true }));
    });
    const stop = reattachToActiveTurn(deps, "speed-session", "speed-thread", "turn-speed");
    const approval = { approval_id: "event-only-approval", action: "Approve scheduled job", risk_class: "plan" };
    deliver({ events: [{ sequence: 1, kind: "approval_required", payload: approval }], live: {
      active_turn: { turn_id: "turn-speed" }, live_message: "",
    } });
    expect(state.runs["speed-session"].pendingApproval?.approval_id).toBe(approval.approval_id);
    Object.assign(state, syncFromRun(state.runs["speed-session"]));
    expect(state.pendingApproval?.approval_id).toBe(approval.approval_id);
    deliver({ events: [], live: { active_turn: { turn_id: "turn-speed" }, live_message: "Waiting for your decision" } });
    expect(state.pendingApproval?.approval_id).toBe(approval.approval_id);
    stop();
  });
  it("falls back to one compact request when streaming fails", async () => {
    vi.useFakeTimers();
    const { deps } = reconnectState();
    vi.mocked(streamSessionEvents).mockRejectedValue(new Error("Proxy buffering"));
    vi.mocked(api).mockResolvedValue({ events: [], live: { active_turn: { turn_id: "turn-speed" }, live_message: "" } });
    const stop = reattachToActiveTurn(deps, "speed-session", "speed-thread", "turn-speed");
    await vi.waitFor(() => expect(api).toHaveBeenCalledTimes(1));
    expect(vi.mocked(api).mock.calls[0][0]).toContain("include_state=1");
    expect(vi.mocked(api).mock.calls[0][0]).toContain("/events?");
    stop();
  });
});

it("does not display failed verification as successful completion", () => {
  expect(terminalRunState("invalid_output")).toEqual({ status: "interrupted", label: "Verification unresolved" });
});

it.each(["invalid_output", "unverified"])("keeps an answer separate from its replayed failure notice (%s)", (status) => {
  const text = "Here is the complete research answer with all its details.";
  const replayed = applyStreamFrameToMessage({ id: "answer", role: "assistant", text }, {
    kind: "failed", payload: { status, message: text },
  });
  expect(replayed.text).toBe(text);
  expect(replayed.resultStatus).toBe(status);
  expect(replayed.statusLabel).toBe("Verification unresolved");
  expect(replayed.failureCause).not.toContain(text);
  expect(replayed.failureCause).toContain("completion could not be verified");
});

it("does not replace a pinned overview when replay receives browser activity", () => {
  const { state, deps } = reconnectState();
  state.surfacePinnedAt = Date.now();
  state.workspaceSurface = "monitor";
  state.liveSurface = "browser";
  state.setLiveSurface = vi.fn();
  state.setLiveSplit = vi.fn();
  let deliver!: Parameters<typeof streamSessionEvents>[1];
  vi.mocked(streamSessionEvents).mockImplementation(async (_url, callback, signal) => {
    deliver = callback;
    await new Promise<void>(resolve => signal.addEventListener("abort", () => resolve(), { once: true }));
  });
  const stop = reattachToActiveTurn(deps, "speed-session", "speed-thread", "turn-speed");
  deliver({ events: [{ sequence: 1, kind: "tool_started", label: "Browser opening page",
    payload: { tool_name: "browser_open" } }] });
  expect(state.setLiveSurface).not.toHaveBeenCalled();
  expect(state.workspaceSurface).toBe("monitor");
  stop();
});

it("uses the fresh opening snapshot before the event stream connects", () => {
  const { state, deps } = reconnectState();
  vi.mocked(streamSessionEvents).mockImplementation(async (_url, _callback, signal) => {
    await new Promise<void>((resolve) => signal.addEventListener("abort", () => resolve(), { once: true }));
  });
  const stop = reattachToActiveTurn(deps, "speed-session", "speed-thread", "turn-speed", {
    initialSnapshot: { active_turn: { turn_id: "turn-speed", phase: "executing" }, live_message: "Reading the current window" },
  });
  expect(state.statusLabel).toBe("Working…");
  expect(state.messages.at(-1)?.text).toBe("Reading the current window");
  expect(state.messages.at(-1)?.streaming).toBe(true);
  stop();
});

it("settles its own terminal status when the session has moved to a different turn", async () => {
  const { state, deps } = reconnectState();
  state.refreshArtifacts = vi.fn().mockResolvedValue(undefined);
  let deliver!: Parameters<typeof streamSessionEvents>[1];
  vi.mocked(streamSessionEvents).mockImplementation(async (_url, callback, signal) => {
    deliver = callback;
    await new Promise<void>(resolve => signal.addEventListener("abort", () => resolve(), {once:true}));
  });
  const stop=reattachToActiveTurn(deps,"speed-session","speed-thread","turn-speed");
  deliver({events:[],live:{requested_turn:{turn_id:"turn-speed",status:"cancelled",phase:"cancelled"},active_turn:{turn_id:"new-turn"},status:"complete"}});
  await Promise.resolve();
  expect(state.runStatus).toBe("cancelled");
  expect(state.messages.at(-1)?.streaming).toBe(false);
  stop();
});

it("ignores late updates after a replacement turn owns the run", () => {
  const { state, deps } = reconnectState();
  let deliver!: Parameters<typeof streamSessionEvents>[1];
  vi.mocked(streamSessionEvents).mockImplementation(async (_url, callback, signal) => {
    deliver=callback;
    await new Promise<void>(resolve=>signal.addEventListener("abort",()=>resolve(),{once:true}));
  });
  const stop=reattachToActiveTurn(deps,"speed-session","speed-thread","turn-speed");
  state.runs["speed-session"].transport = {...state.runs["speed-session"].transport!,turnId:"replacement"};
  state.runs["speed-session"].statusLabel="New work";
  deliver({events:[{sequence:999,kind:"progress",label:"Old work"}],live:{active_turn:{turn_id:"turn-speed"}}});
  expect(state.runs["speed-session"].statusLabel).toBe("New work");
  expect(state.runs["speed-session"].transport?.turnId).toBe("replacement");
  stop();
});

it("ignores a poll response that arrives after Stop", async () => {
  const {state,deps}=reconnectState();
  let resolve!: (value:unknown)=>void;
  vi.mocked(streamSessionEvents).mockRejectedValueOnce(new Error("disconnect"));
  vi.mocked(api).mockImplementationOnce(()=>new Promise(done=>{resolve=done;}));
  const stop=reattachToActiveTurn(deps,"speed-session","speed-thread","turn-speed");
  await vi.waitFor(()=>expect(resolve).toBeTypeOf("function"));
  stop({cancelled:true});
  const before=JSON.stringify(state.runs["speed-session"]);
  resolve({events:[{sequence:300,kind:"progress",label:"Late mutation"}],live:{active_turn:{turn_id:"turn-speed"},live_message:"late text"}});
  await Promise.resolve();await Promise.resolve();
  expect(JSON.stringify(state.runs["speed-session"])).toBe(before);
});

it("continues read-only polling when SSE ends but the turn is still active", async () => {
  const {state,deps}=reconnectState();
  vi.mocked(streamSessionEvents).mockResolvedValueOnce(undefined);
  vi.mocked(api).mockResolvedValue({events:[],live:{active_turn:{turn_id:"turn-speed"}},active_turn:{turn_id:"turn-speed"}});
  const stop=reattachToActiveTurn(deps,"speed-session","speed-thread","turn-speed");
  await vi.waitFor(()=>expect(vi.mocked(api).mock.calls.some(([path])=>path.includes("/events?"))).toBe(true));
  expect(state.sending).toBe(true);
  expect(state.messages.at(-1)?.streaming).toBe(true);
  stop();
});

it("ignores final status read that resolves after a consumer is replaced", async () => {
  const {state,deps}=reconnectState();
  let resolve!: (value:unknown)=>void;
  vi.mocked(streamSessionEvents).mockResolvedValueOnce(undefined);
  vi.mocked(api).mockImplementationOnce(()=>new Promise(done=>{resolve=done;}));
  const stop=reattachToActiveTurn(deps,"speed-session","speed-thread","turn-speed");
  await vi.waitFor(()=>expect(resolve).toBeTypeOf("function"));
  stop();
  state.runs["speed-session"].statusLabel="Replacement running";
  resolve({status:"complete"});
  await Promise.resolve();await Promise.resolve();
  expect(state.runs["speed-session"].statusLabel).toBe("Replacement running");
});

it("notifies queue settlement exactly once on confirmed completion", async () => {
  const {state,deps}=reconnectState();
  state.refreshArtifacts=vi.fn().mockResolvedValue(undefined);
  const settled=vi.fn();
  let deliver!: Parameters<typeof streamSessionEvents>[1];
  vi.mocked(streamSessionEvents).mockImplementation(async (_url,callback,signal)=>{
    deliver=callback;
    await new Promise<void>(resolve=>signal.addEventListener("abort",()=>resolve(),{once:true}));
  });
  const stop=reattachToActiveTurn({...deps,onSettled:settled},"speed-session","speed-thread","turn-speed");
  const packet={events:[],live:{requested_turn:{turn_id:"turn-speed",status:"complete",phase:"complete"},active_turn:null}};
  deliver(packet);deliver(packet);
  await Promise.resolve();
  expect(settled).toHaveBeenCalledExactlyOnceWith("speed-session","success");
  stop();
});

it("background peer monitoring cannot overwrite a newer turn's controls", async () => {
  const {state,deps}=reconnectState();
  state.runs["speed-session"].statusLabel="New work";
  state.runs["speed-session"].transport={...state.runs["speed-session"].transport!,turnId:"new-turn"};
  vi.mocked(api).mockResolvedValue({events:[{sequence:1,kind:"approval_required",payload:{approval_id:"old-approval",action:"ask_human"}}],live:{active_turn:{turn_id:"new-turn"},pending_approval:{approval_id:"old-approval",action:"ask_human"}}});
  const stop=reattachToActiveTurn(deps,"speed-session","speed-thread","old-turn",{backgroundPeerMonitor:true});
  await vi.waitFor(()=>expect(api).toHaveBeenCalled());
  await Promise.resolve();
  expect(state.runs["speed-session"].statusLabel).toBe("New work");
  expect(state.runs["speed-session"].pendingApproval).toBeFalsy();
  expect(state.runs["speed-session"].transport?.turnId).toBe("new-turn");
  stop();
});

it("ignores duplicate or older journal frames after advancing the reconnect cursor", () => {
  const {state,deps}=reconnectState();
  let deliver!: Parameters<typeof streamSessionEvents>[1];
  vi.mocked(streamSessionEvents).mockImplementation(async (_url,callback,signal)=>{
    deliver=callback;
    await new Promise<void>(resolve=>signal.addEventListener("abort",()=>resolve(),{once:true}));
  });
  const stop=reattachToActiveTurn(deps,"speed-session","speed-thread","turn-speed");
  deliver({events:[{sequence:10,kind:"progress",label:"Current work"}],live:{active_turn:{turn_id:"turn-speed"}}});
  deliver({events:[{sequence:9,kind:"approval_required",label:"Old approval",payload:{approval_id:"old",action:"ask_human"}},{sequence:10,kind:"progress",label:"Duplicate work"}],live:{active_turn:{turn_id:"turn-speed"}}});
  expect(state.runs["speed-session"].pendingApproval).toBeFalsy();
  expect(state.runs["speed-session"].statusLabel).toBe("Current work");
  stop();
});

it("rejects foreign frames before advancing the reconnect cursor and retains item ownership", () => {
  const { state, deps } = reconnectState();
  let deliver!: Parameters<typeof streamSessionEvents>[1];
  vi.mocked(streamSessionEvents).mockImplementation(async (_url, callback, signal) => {
    deliver = callback;
    await new Promise<void>(resolve => signal.addEventListener("abort", () => resolve(), { once: true }));
  });
  const stop = reattachToActiveTurn(deps, "speed-session", "speed-thread", "turn-speed");
  const item = (sequence: number, turnId: string, text: string) => ({
    kind: "stream_item", sequence, turn_id: turnId,
    payload: { item_id: "answer", channel: "text", state: "completed", text, base_offset: 0 },
  });
  deliver({ events: [item(10, "turn-speed", "Current"), item(100, "foreign", "Other chat"),
    { kind: "approval_required", sequence: 101, turn_id: "foreign", payload: { approval_id: "foreign", action: "ask_human" } },
    item(11, "turn-speed", "Current answer")], live: { active_turn: { turn_id: "turn-speed" } } });
  const run = state.runs["speed-session"];
  expect(run.messages.at(-1)?.text).toBe("Current answer");
  expect(run.messages.at(-1)?.streamCursor).toEqual({ turnId: "turn-speed", sequence: 11 });
  expect(run.transport?.lastSequence).toBe(11);
  expect(run.pendingApproval).toBeFalsy();
  stop();
});

describe("replay on reconnect (design section 16)", () => {
  beforeEach(() => { vi.resetAllMocks(); });
  afterEach(() => { vi.resetAllMocks(); vi.clearAllTimers(); vi.useRealTimers(); });
  const waitForAbort = async (_url: string, _callback: unknown, signal: AbortSignal) => {
    await new Promise<void>(resolve => signal.addEventListener("abort", () => resolve(), { once: true }));
  };
  const stepLabels = (state: AppState) => (state.runs["speed-session"].messages.at(-1)?.steps || []).map(step => step.label);

  it("pages through missed events from the last sequence, applies each once, then resumes streaming", async () => {
    vi.useFakeTimers();
    const { state, deps } = reconnectState();
    state.runs["speed-session"].messages = [{ id: "a1", role: "assistant", text: "", streaming: true }];
    const running = { turn_id: "turn-speed", status: "running", native_status: "running", running: true };
    vi.mocked(api)
      .mockResolvedValueOnce({ events: [
        { sequence: 5, kind: "progress", label: "Already seen" },
        { sequence: 6, kind: "progress", label: "Missed one" },
      ], next_after: 6, has_more: true, turn: running })
      .mockResolvedValueOnce({ events: [
        { sequence: 6, kind: "progress", label: "Missed one" },
        { sequence: 7, kind: "escalation", label: "Changed approach", payload: { from: "retry", to: "replan" } },
      ], next_after: 7, has_more: false, turn: running });
    vi.mocked(streamSessionEvents).mockImplementation(waitForAbort);
    const onServerTurn = vi.fn();
    const stop = reattachToActiveTurn({ ...deps, onServerTurn }, "speed-session", "speed-thread", "turn-speed",
      { assistantId: "a1", afterSequence: 5 });
    await vi.waitFor(() => expect(api).toHaveBeenCalledTimes(2));
    expect(vi.mocked(api).mock.calls[0][0]).toContain("after=5");
    expect(vi.mocked(api).mock.calls[1][0]).toContain("after=6");
    expect(stepLabels(state)).toEqual(["Missed one", "Changed approach"]);
    expect(state.runs["speed-session"].messages.at(-1)?.steps?.at(-1)?.detail).toEqual(["retry → replan"]);
    expect(onServerTurn).toHaveBeenLastCalledWith("speed-session", expect.objectContaining({ running: true }));
    expect(streamSessionEvents).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1000);
    expect(streamSessionEvents).toHaveBeenCalledTimes(1);
    expect(vi.mocked(streamSessionEvents).mock.calls[0][0]).toContain("after=7");
    expect(state.sending).toBe(true);
    stop();
  });

  it("replays from the last streamed sequence after the stream drops, then reconnects with backoff", async () => {
    vi.useFakeTimers();
    const { state, deps } = reconnectState();
    vi.mocked(streamSessionEvents)
      .mockImplementationOnce(async (_url, callback) => {
        callback({ events: [{ sequence: 3, kind: "progress", label: "Live step" }], live: { active_turn: { turn_id: "turn-speed" } } });
        throw new Error("Live session stream interrupted");
      })
      .mockImplementation(waitForAbort);
    vi.mocked(api).mockResolvedValue({ events: [
      { sequence: 3, kind: "progress", label: "Live step" },
      { sequence: 4, kind: "progress", label: "Missed while dropped" },
    ], next_after: 4, has_more: false, turn: { turn_id: "turn-speed", status: "running", running: true } });
    const stop = reattachToActiveTurn(deps, "speed-session", "speed-thread", "turn-speed");
    await vi.waitFor(() => expect(api).toHaveBeenCalledTimes(1));
    expect(vi.mocked(api).mock.calls[0][0]).toContain("/events?");
    expect(vi.mocked(api).mock.calls[0][0]).toContain("after=3");
    await vi.waitFor(() => expect(stepLabels(state)).toEqual(["Live step", "Missed while dropped"]));
    // The stream reconnects after the first backoff step (1 s), not at once.
    expect(streamSessionEvents).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1000);
    expect(streamSessionEvents).toHaveBeenCalledTimes(2);
    expect(vi.mocked(streamSessionEvents).mock.calls[1][0]).toContain("after=4");
    stop();
  });

  it("finalizes from replayed events when the turn already ended, without reconnecting", async () => {
    vi.useFakeTimers();
    const { state, deps } = reconnectState();
    state.refreshArtifacts = vi.fn().mockResolvedValue(undefined);
    vi.mocked(streamSessionEvents).mockRejectedValueOnce(new Error("Live session stream interrupted"));
    vi.mocked(api).mockResolvedValue({ events: [
      { sequence: 2, kind: "assistant_narrative", payload: { source_kind: "assistant_narrative", text: "All done.", offset: 0 } },
      { sequence: 3, kind: "done", payload: { status: "success" } },
    ], next_after: 3, has_more: false, turn: { turn_id: "turn-speed", status: "success", native_status: "completed", running: false } });
    const onServerTurn = vi.fn();
    const stop = reattachToActiveTurn({ ...deps, onServerTurn }, "speed-session", "speed-thread", "turn-speed");
    await vi.waitFor(() => expect(state.runStatus).toBe("success"));
    expect(state.sending).toBe(false);
    expect(state.messages.at(-1)).toMatchObject({ text: "All done.", streaming: false });
    expect(onServerTurn).toHaveBeenCalledWith("speed-session", expect.objectContaining({ running: false }));
    await vi.advanceTimersByTimeAsync(60_000);
    expect(streamSessionEvents).toHaveBeenCalledTimes(1);
    expect(api).toHaveBeenCalledTimes(1);
    stop();
  });

  it("shows the unlock wait for a turn held by a locked credential vault", () => {
    const { state, deps } = reconnectState();
    vi.mocked(streamSessionEvents).mockImplementation(waitForAbort);
    const stop = reattachToActiveTurn(deps, "speed-session", "speed-thread", "turn-speed", {
      initialSnapshot: { active_turn: { turn_id: "turn-speed", status: "waiting_for_unlock", phase: "waiting_for_unlock" } },
    });
    expect(state.runStatus).toBe("running");
    expect(state.statusLabel).toBe("Waiting for credential unlock");
    stop();
  });
});
