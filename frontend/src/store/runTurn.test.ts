import { beforeEach, afterEach, describe, expect, it, vi } from "vitest";
import { StreamDroppedError } from "../api/stream";
import type { AgentMode, ChatMessage, SessionRun, StreamHandlers } from "../api/types";
import { applyTranscriptToMessage } from "../lib/transcript";
import { emptyRun, emptyTransport } from "./helpers";
import { runTurn } from "./runTurn";

vi.mock("../api/stream", async () => {
  const actual = await vi.importActual<typeof import("../api/stream")>("../api/stream");
  return {
    ...actual,
    streamChat: vi.fn(),
  };
});

vi.mock("../api/client", async () => ({
  ...await vi.importActual<typeof import("../api/client")>("../api/client"), api: vi.fn(),
}));

import { streamChat } from "../api/stream";
import { ApiError, api } from "../api/client";

beforeEach(() => {
  const data = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    get length() { return data.size; }, key: (i: number) => [...data.keys()][i] ?? null,
    getItem: (key: string) => data.get(key) ?? null,
    setItem: (key: string, value: string) => { data.set(key, value); },
    removeItem: (key: string) => { data.delete(key); },
  });
});
afterEach(() => vi.unstubAllGlobals());

const PLAN_BOARD = {
  done: 1,
  total: 2,
  items: [
    { id: "one", text: "Research the product", done: true },
    { id: "two", text: "Summarize findings", done: false },
  ],
};

function harness(
  initial: Partial<SessionRun> = {},
  agentMode: AgentMode = "normal",
  extras: Record<string, unknown> = {},
) {
  let run: SessionRun = {
    ...emptyRun("sess-1", "thread-1"),
    abort: new AbortController(),
    ...initial,
  };
  const reattach = vi.fn();
  const store = {
    machineActive: Boolean(initial.machineActive),
    machineBlocked: false,
    machineDetail: "",
    machineNovncUrl: "",
    liveSurface: "computer" as const,
    liveRevealedThisTurn: false,
    surfacePinnedAt: null as number | null,
    surfaceControlling: false,
    setLiveSurface: vi.fn(),
    setLiveSplit: vi.fn(),
    pinLiveSurface: vi.fn(),
    releaseDesktopControl: vi.fn(),
    setRemoteExecution: vi.fn(),
    revealWorkspaceSurface: vi.fn(),
    showToast: vi.fn(),
    refreshSessions: vi.fn(),
    refreshArtifacts: vi.fn(),
    upsertCanvasPaths: vi.fn(),
    setAgentMode: vi.fn(),
    ...extras,
  };
  const set = vi.fn((partial: unknown) => {
    if (typeof partial === "function") {
      const next = (
        partial as (state: {
          runs: Record<string, SessionRun>;
          sessionId: string;
          sessions: [];
        }) => Record<string, unknown> & { runs?: Record<string, SessionRun> }
      )({ runs: { "sess-1": run }, sessionId: "sess-1", sessions: [] });
      if (next.runs?.["sess-1"]) run = next.runs["sess-1"];
      const { runs: _runs, sessions: _sessions, ...rest } = next;
      Object.assign(store, rest);
    } else if (partial && typeof partial === "object") {
      Object.assign(store, partial);
    }
  });
  const get = () => ({
    sessionId: "sess-1",
    runs: { "sess-1": run },
    prefs: { mainModel: "", plannerModel: "", workerModel: "", voiceReply: false },
    modelOverride: "",
    activePersonaId: "main",
    autoApprove: true,
    agentMode,
    sessionTitle: "",
    sessions: [],
    pendingApproval: null,
    ...store,
  });
  const updateRun = (_id: string, updater: (current: SessionRun) => SessionRun) => {
    run = updater(run);
  };
  const patchAssistant = (_sid: string, assistantId: string, patch: Partial<ChatMessage>) => {
    run = {
      ...run,
      messages: run.messages.map((message) =>
        message.id === assistantId ? { ...message, ...patch } : message,
      ),
    };
  };
  const patchAssistantNarrative = (_sid: string, assistantId: string, assembled: string) => {
    run = {
      ...run,
      messages: run.messages.map((message) =>
        message.id === assistantId
          ? applyTranscriptToMessage(message, { type: "narrative", assembled })
          : message,
      ),
    };
  };
  const flushStreamPatches = vi.fn();
  return {
    reattach,
    store,
    get run() {
      return run;
    },
    deps: {
      set: set as never,
      get: get as never,
      updateRun,
      patchAssistant,
      patchAssistantNarrative,
      flushStreamPatches,
      flushQueue: async () => {},
      reattach,
    },
  };
}

function assistantBlocks(run: SessionRun) {
  const assistant = [...run.messages].reverse().find((message) => message.role === "assistant");
  return assistant?.blocks || [];
}

it("shows a local pending message before team lookup and then uses the resolved coordinator", async () => {
  let release!: () => void;
  const gate = new Promise<void>(resolve => { release = resolve; });
  const ctx = harness({}, "normal", { activeTeamId: "team-1", teams: [],
    personas: [{ id: "reviewer", name: "Reviewer" }], refreshTeams: vi.fn(async () => {
      await gate;
      Object.assign(ctx.store, { teams: [{ id: "team-1", agents: ["reviewer"] }] });
    }) });
  vi.mocked(streamChat).mockClear();
  vi.mocked(streamChat).mockImplementationOnce(async (body, handlers) => {
    expect(body.recipient_id).toBe("reviewer");
    expect(ctx.run.messages.find(message => message.role === "assistant")?.speaker?.name).toBe("Reviewer");
    handlers.onStatus?.("Request received · preparing…", { phase: "received", turn_id: "owned" });
    expect(ctx.run.statusLabel).toBe("Request received · preparing…");
    return { status: "success", message: "Done" };
  });
  const task = runTurn(ctx.deps, "sess-1", "thread-1", "Check this", [], "Check this");
  expect(ctx.run.statusLabel).toBe("Sending request…");
  expect(ctx.run.messages.map(message => message.role)).toEqual(["user", "assistant"]);
  expect(vi.mocked(streamChat)).not.toHaveBeenCalled();
  release();
  await task;
  expect(ctx.run.messages.filter(message => message.role === "assistant")).toHaveLength(1);
});

it("does not dispatch after cancellation during team lookup", async () => {
  let release!: () => void;
  const gate = new Promise<void>(resolve => { release = resolve; });
  const ctx = harness({}, "normal", { activeTeamId: "team-1", teams: [], refreshTeams: () => gate });
  vi.mocked(streamChat).mockClear();
  const task = runTurn(ctx.deps, "sess-1", "thread-1", "Check this", [], "Check this");
  ctx.run.abort?.abort();
  release();
  await task;
  expect(vi.mocked(streamChat)).not.toHaveBeenCalled();
});

it("keeps item ownership and ignores repeated or foreign journal frames in the live store", async () => {
  const ctx = harness();
  vi.mocked(streamChat).mockImplementationOnce(async (_body, handlers) => {
    handlers.onStatus?.("Working", { turn_id: "owned-turn", phase: "running" });
    const item = (sequence: number, turnId: string, text: string) => ({
      kind: "stream_item", sequence, turn_id: turnId,
      payload: { item_id: "answer", channel: "text", state: "completed", text, base_offset: 0 },
    });
    handlers.onEvent?.(item(10, "owned-turn", "Current answer"));
    handlers.onEvent?.(item(100, "foreign-turn", "Other chat answer"));
    handlers.onEvent?.({ kind: "approval_required", sequence: 101, turn_id: "foreign-turn",
      payload: { approval_id: "foreign", action: "ask_human" } });
    handlers.onStatus?.("Foreign status", { turn_id: "foreign-turn", phase: "running" });
    handlers.onEvent?.(item(10, "owned-turn", "Repeated stale answer"));
    handlers.onEvent?.(item(11, "owned-turn", "Current answer."));
    const assistant = ctx.run.messages.find(message => message.role === "assistant");
    expect(assistant?.text).toBe("Current answer.");
    expect(assistant?.streamCursor).toEqual({ turnId: "owned-turn", sequence: 11 });
    expect(ctx.run.transport?.turnId).toBe("owned-turn");
    expect(ctx.run.pendingApproval).toBeFalsy();
    return { status: "success", message: "Current answer." };
  });
  await runTurn(ctx.deps, "sess-1", "thread-1", "Answer", [], "Answer");
  expect(assistantBlocks(ctx.run).filter(block => block.type === "narrative")).toHaveLength(1);
});

it("keeps budget telemetry out of live chat while advancing its cursor", async () => {
  const ctx = harness();
  vi.mocked(streamChat).mockImplementationOnce(async (_body, handlers) => {
    handlers.onStatus?.("Working", { turn_id: "budget-turn", phase: "running" });
    handlers.onEvent?.({ kind: "progress", label: "Reading notes", sequence: 1, turn_id: "budget-turn" });
    const before = ctx.run.messages.find(message => message.role === "assistant")!;
    expect(before.statusLabel).toBe("Reading notes");

    handlers.onEvent?.({ kind: "request_budget", sequence: 2, turn_id: "budget-turn", payload: { used_model_calls: 1 } });
    handlers.onEvent?.({
      kind: "request_budget", label: "request_budget", interesting: true,
      sequence: 3, turn_id: "budget-turn", payload: { used_model_calls: 1 },
      tool_card: { name: "request_budget", status: "running" },
    });
    handlers.onEvent?.({
      kind: "progress", label: "Budget usage", interesting: true,
      sequence: 4, turn_id: "budget-turn", payload: { source_kind: "request_budget" },
    });
    const after = ctx.run.messages.find(message => message.role === "assistant")!;
    expect(after.statusLabel).toBe("Reading notes");
    expect(ctx.run.statusLabel).toBe("Reading notes");
    expect(after.steps).toEqual(before.steps);
    expect(after.blocks).toEqual(before.blocks);
    expect(after.toolCards || []).toEqual(before.toolCards || []);
    expect(after.streamCursor).toEqual({ turnId: "budget-turn", sequence: 4 });
    expect(ctx.run.transport?.lastSequence).toBe(4);

    handlers.onEvent?.({ kind: "progress", label: "Writing answer", sequence: 5, turn_id: "budget-turn" });
    expect(ctx.run.messages.find(message => message.role === "assistant")?.statusLabel).toBe("Writing answer");
    handlers.onEvent?.({
      kind: "approval_required", sequence: 6, turn_id: "budget-turn",
      payload: { approval_id: "approval-1", action: "ask_human" },
    });
    const waiting = { status: ctx.run.status, label: ctx.run.statusLabel, approval: ctx.run.pendingApproval };
    expect(waiting.status).toBe("waiting_approval");
    handlers.onEvent?.({
      kind: "approval_resolved", sequence: 7, turn_id: "budget-turn",
      payload: { source_kind: "request_budget", approval_id: "approval-1" },
    });
    expect(ctx.run.status).toBe(waiting.status);
    expect(ctx.run.statusLabel).toBe(waiting.label);
    expect(ctx.run.pendingApproval).toEqual(waiting.approval);
    expect(ctx.run.transport?.lastSequence).toBe(7);
    return { status: "success", message: "Done" };
  });
  await runTurn(ctx.deps, "sess-1", "thread-1", "Task", [], "Task");
});

it("can deliver a chat turn when LAN HTTP does not expose crypto.randomUUID", async () => {
  const source = globalThis.crypto;
  vi.stubGlobal("crypto", { getRandomValues: source.getRandomValues.bind(source) });
  const ctx = harness();
  let requestId = "";
  vi.mocked(streamChat).mockImplementationOnce(async body => {
    requestId = body.client_request_id!;
    expect(body.message).toBe("Inspect the saved result");
    return { status: "success", message: "Inspected" };
  });
  await runTurn(ctx.deps, "sess-1", "thread-1", "Inspect the saved result", [], "Inspect the saved result");
  expect(requestId).toMatch(/^[0-9a-f-]{36}$/);
  expect(ctx.run.clientRequestId).toBe(requestId);
  expect(ctx.run.status).toBe("success");
});

it.each(["README.md", "main.py"])("does not route a leading @%s file mention to an unknown persona", async (name) => {
  const ctx = harness();
  vi.mocked(streamChat).mockImplementationOnce(async (body) => {
    expect(body.recipient_id).toBe("main");
    expect(body.message).toBe(`@${name} explain this file`);
    return {} as never;
  });
  await runTurn(ctx.deps, "sess-1", "thread-1", `@${name} explain this file`, [], "");
});

it("routes known persona mentions while retaining their speaker identity", async () => {
  const ctx = harness({}, "normal", { personas: [{ id: "reviewer", name: "Reviewer" }] });
  vi.mocked(streamChat).mockImplementationOnce(async body => {
    expect(body.recipient_id).toBe("reviewer");
    return {} as never;
  });
  await runTurn(ctx.deps, "sess-1", "thread-1", "@reviewer check this", [], "");
  expect(ctx.run.messages.find(message => message.role === "assistant")?.speaker?.name).toBe("Reviewer");
});

it("sends a selected team for a new conversation", async () => {
  const ctx = harness({}, "normal", { activeTeamId: "research", activePersonaId: "main", teams: [{ id: "research", agents: ["lead"] }], personas: [{ id: "lead", name: "Team lead" }] });
  vi.mocked(streamChat).mockImplementationOnce(async body => {
    expect(body.team_id).toBe("research");
    expect(body.recipient_id).toBe("lead");
    expect(body.message).toBe("Investigate this");
    return {} as never;
  });
  await runTurn(ctx.deps, "sess-1", "thread-1", "Investigate this", [], "");
  expect(ctx.run.messages.find(message => message.role === "assistant")?.speaker?.name).toBe("Team lead");
  vi.mocked(streamChat).mockImplementationOnce(async body => {
    expect(body.team_id).toBeUndefined();
    expect(body.recipient_id).toBe("lead");
    return {} as never;
  });
  await runTurn(ctx.deps, "sess-1", "thread-1", "Continue", [], "");
});

it("routes an explicit team mention instead of a stale selection", async () => {
  const ctx = harness({}, "normal", { activeTeamId: "old-team" });
  vi.mocked(streamChat).mockImplementationOnce(async body => {
    expect(body.team_id).toBe("new-team");
    expect(body.message).toBe("&new-team investigate");
    return {} as never;
  });
  await runTurn(ctx.deps, "sess-1", "thread-1", "&new-team investigate", [], "");
});

it("does not reuse a selected team for an existing conversation or an explicit agent", async () => {
  const prior = [{ id: "previous", role: "user" as const, text: "Earlier" }];
  const existing = harness({ messages: prior }, "normal", { activeTeamId: "research" });
  vi.mocked(streamChat).mockImplementationOnce(async body => {
    expect(body.team_id).toBeUndefined();
    return {} as never;
  });
  await runTurn(existing.deps, "sess-1", "thread-1", "Continue", [], "");
  const explicit = harness({}, "normal", { activeTeamId: "research", personas: [{ id: "reviewer", name: "Reviewer" }] });
  vi.mocked(streamChat).mockImplementationOnce(async body => {
    expect(body.team_id).toBeUndefined();
    expect(body.recipient_id).toBe("reviewer");
    return {} as never;
  });
  await runTurn(explicit.deps, "sess-1", "thread-1", "@reviewer inspect", [], "");
});

describe("runTurn stream handoff", () => {
  it("acknowledges steering when the controller consumes it", async () => {
    const ctx = harness();
    vi.mocked(streamChat).mockImplementationOnce(async (_body, handlers) => {
      ctx.deps.updateRun("sess-1", (run) => ({ ...run, messages: [...run.messages,
        { id: "steer", role: "user", text: "Check the screen", steering: "pending" },
      ] }));
      handlers.onEvent?.({ kind: "progress", payload: { source_kind: "user_inject", message: "Check the screen" } });
      expect(ctx.run.messages.find((m) => m.id === "steer")?.steering).toBe("applied");
      return {} as never;
    });
    await runTurn(ctx.deps, "sess-1", "thread-1", "Task", [], "Task");
  });
  it.each(["event", "tool_card"])("keeps batched narrative before a %s tool boundary", async (boundary) => {
    const ctx = harness();
    const commit = ctx.deps.patchAssistantNarrative;
    let pending: Parameters<typeof commit> | undefined;
    ctx.deps.patchAssistantNarrative = (...args) => { pending = args; };
    ctx.deps.flushStreamPatches.mockImplementation(() => {
      if (pending) {
        commit(...pending);
        pending = undefined;
      }
    });
    vi.mocked(streamChat).mockImplementationOnce(async (_body, handlers) => {
      // Simulate tokens and a tool arriving together, before the browser's
      // scheduled animation frame has committed the narrative.
      handlers.onDelta?.("Let me read the email.");
      if (boundary === "event") {
        handlers.onEvent?.({
          kind: "tool_started", sequence: 1,
          tool_card: { name: "gmail_read", status: "running", args_preview: "FS6", attempt_id: "mail-1" },
          payload: { tool: "gmail_read", attempt_id: "mail-1" },
        });
      } else {
        handlers.onToolCard?.({ name: "gmail_read", status: "running", args_preview: "FS6", attempt_id: "mail-1" });
      }
      expect(assistantBlocks(ctx.run).map((block) => block.type)).toEqual(["narrative", "tool"]);
      expect(assistantBlocks(ctx.run)[0]).toMatchObject({ text: "Let me read the email." });
      return { status: "success", message: "Let me read the email." };
    });
    await runTurn(ctx.deps, "sess-1", "thread-1", "Check FS6", [], "Check FS6");
  });

  it("keeps earlier narration before tools when the final message arrives", async () => {
    const ctx = harness();
    vi.mocked(streamChat).mockImplementationOnce(async (_body, handlers) => {
      handlers.onDelta?.("I will read the file.");
      handlers.onToolCard?.({ name: "read_file", status: "done", attempt_id: "read-1" });
      handlers.onDelta?.("I will read the file.Done.");
      handlers.onMessage?.("Done.", false);
      expect(assistantBlocks(ctx.run).map((block) => block.type)).toEqual(["narrative", "tool", "narrative"]);
      expect(assistantBlocks(ctx.run)[0]).toMatchObject({ text: "I will read the file." });
      expect(assistantBlocks(ctx.run)[2]).toMatchObject({ text: "Done." });
      return { status: "success", message: "Done." };
    });
    await runTurn(ctx.deps, "sess-1", "thread-1", "Task", [], "Task");
    expect(assistantBlocks(ctx.run).map((block) => block.type)).toEqual(["narrative", "tool", "narrative"]);
  });

  it("keeps the image picker in sync with an explicit provider instruction", async () => {
    const ctx = harness();
    vi.mocked(streamChat).mockImplementationOnce(async (_body, handlers) => {
      handlers.onEvent?.({
        kind: "capability_preference_updated",
        payload: { capability: "image_generation", provider: "nano-banana", source: "user_instruction" },
      });
      return { status: "success", message: "Provider updated" };
    });
    await runTurn(ctx.deps, "sess-1", "thread-1", "Use nano banana", [], "Use nano banana");
    expect(ctx.store).toMatchObject({ prefs: { imageGeneration: "nano-banana", mediaModel: "nano-banana" } });
  });

  it("projects Codex Runtime readiness and native tool routing into Monitor", async () => {
    const ctx = harness();
    vi.mocked(streamChat).mockImplementationOnce(async (_body, handlers) => {
      handlers.onEvent?.({
        kind: "progress",
        payload: {
          source_kind: "codex_runtime",
          native_packs: ["web search", "image generation", "vision"],
          bridge: "dynamic",
          bridge_tools: 18,
          apps: 2,
        },
      });
      handlers.onEvent?.({
        kind: "tool_started",
        payload: { tool: "web_search" },
      });
      return { status: "success", message: "ready" };
    });

    await runTurn(ctx.deps, "sess-1", "thread-1", "hello", [], "hello");

    expect(ctx.run.operatingState.nativePacks).toEqual([
      "web search",
      "image generation",
      "vision",
    ]);
    expect(ctx.run.operatingState.mcpStatus).toBe("dynamic · 18 tools · 2 apps");
    expect(ctx.run.operatingState.route).toBe("native");
    expect(ctx.run.operatingState.currentTool).toBe("web_search");
  });

  it("binds both live bubbles to the accepted turn for deletion before refresh", async () => {
    vi.mocked(streamChat).mockImplementationOnce(async (_body, handlers: StreamHandlers) => {
      handlers.onStatus?.("Working…", { turn_id: "delete-live", phase: "running", sequence: 1 });
      return { status: "success", message: "Done" };
    });
    const ctx = harness();
    await runTurn(ctx.deps, "sess-1", "thread-1", "go go", [], "go go");
    expect(ctx.run.messages.map((message) => message.turnId)).toEqual(["delete-live", "delete-live"]);
  });

  it("passes the SSE cursor to reattach and drops the obsolete abort controller", async () => {
    const streamChatMock = vi.mocked(streamChat);
    streamChatMock.mockRejectedValueOnce(
      new StreamDroppedError("turn-7", "sess-1", "thread-1", 12),
    );
    const ctx = harness();

    await runTurn(ctx.deps, "sess-1", "thread-1", "hello", [], "hello");

    expect(ctx.reattach).toHaveBeenCalledWith("sess-1", "thread-1", "turn-7", {
      assistantId: expect.any(String),
      afterSequence: 12,
    });
    expect(ctx.run.abort).toBeNull();
    expect(ctx.run.transport?.mode).toBe("polling");
    expect(ctx.run.transport?.lastSequence).toBe(12);
    expect(ctx.run.transport?.turnId).toBe("turn-7");
  });

  it("renders each browser chip as the event arrives, before done", async () => {
    const ctx = harness();
    const counts: number[] = [];
    vi.mocked(streamChat).mockImplementationOnce(async (_body, handlers: StreamHandlers) => {
      handlers.onEvent?.({
        kind: "tool_started",
        label: "Fetching bareandfair.ca",
        interesting: true,
        payload: { tool: "web_fetch", tool_name: "web_fetch" },
        tool_card: {
          name: "web_fetch",
          args_preview: 'url="https://bareandfair.ca/"',
          status: "running",
          attempt_id: "a1",
        },
      });
      counts.push(assistantBlocks(ctx.run).filter((block) => block.type === "tool").length);
      handlers.onEvent?.({
        kind: "tool_started",
        label: "Searching the web",
        interesting: true,
        payload: { tool: "web_search", tool_name: "web_search" },
        tool_card: {
          name: "web_search",
          args_preview: 'query="Canada Post India"',
          status: "running",
          attempt_id: "a2",
        },
      });
      counts.push(assistantBlocks(ctx.run).filter((block) => block.type === "tool").length);
      return { status: "success", message: "rates ready" };
    });

    await runTurn(ctx.deps, "sess-1", "thread-1", "cheapest way to send", [], "cheapest way to send");

    expect(counts).toEqual([1, 2]);
    expect(assistantBlocks(ctx.run).filter((block) => block.type === "tool")).toHaveLength(2);
  });

  it("reattaches a generic stream error onto the same bubble when a turn id is known", async () => {
    vi.mocked(streamChat).mockImplementationOnce(async (_body, handlers) => {
      handlers.onStatus?.("Working…", { turn_id: "turn-9", phase: "running", sequence: 4 });
      throw new Error("Stream failed (502)");
    });
    const ctx = harness();

    await runTurn(ctx.deps, "sess-1", "thread-1", "hello", [], "hello");

    expect(ctx.reattach).toHaveBeenCalledWith("sess-1", "thread-1", "turn-9", {
      assistantId: expect.any(String),
      afterSequence: 4,
    });
    expect(ctx.run.status).not.toBe("error");
  });

  it("clears a previous turn's journal cursor before a new turn starts", async () => {
    vi.mocked(streamChat).mockResolvedValueOnce({});
    const ctx = harness({
      transport: { mode: "sse", lastSequence: 40, lastFrameAt: 1, reconnectCount: 0, turnId: "turn-old" },
    });

    await runTurn(ctx.deps, "sess-1", "thread-1", "next task", [], "next task");

    expect(ctx.run.transport?.lastSequence).toBe(0);
    expect(ctx.run.transport?.turnId).toBe("");
  });
});

describe("runTurn plan board continuity", () => {
  it("shows the current request while waiting for its authoritative plan", async () => {
    vi.mocked(streamChat).mockResolvedValueOnce({});
    const ctx = harness({ todoBoard: PLAN_BOARD }, "plan");

    await runTurn(ctx.deps, "sess-1", "thread-1", "continue the plan", [], "continue the plan", {
      agentMode: "plan",
    });

    expect(ctx.run.todoBoard).toEqual({ done: 0, total: 0, items: [], objective: "continue the plan", notice: "Preparing this request…" });
    expect(ctx.run.status).not.toBe("cancelled");
  });

  it("preserves unfinished step evidence when a reply succeeds", async () => {
    vi.mocked(streamChat).mockImplementationOnce(async (_body, handlers: StreamHandlers) => {
      handlers.onEvent?.({
        kind: "todo_board",
        payload: {
          done: 0,
          total: 1,
          items: [{ id: "act", text: "Follow-up answer", done: false }],
        },
      });
      return { status: "success", message: "ok" };
    });
    const ctx = harness({}, "normal");

    await runTurn(ctx.deps, "sess-1", "thread-1", "ping", [], "ping", {
      agentMode: "normal",
    });

    expect(ctx.run.todoBoard).toEqual({
      done: 0,
      total: 1,
      items: [{ id: "act", text: "Follow-up answer", done: false }],
    });
    expect(ctx.run.status).toBe("success");
  });

  it("does not auto-complete multi-step plan boards on turn success", async () => {
    vi.mocked(streamChat).mockImplementationOnce(async (_body, handlers: StreamHandlers) => {
      handlers.onEvent?.({ kind: "todo_board", turn_id: "new-turn", payload: { ...PLAN_BOARD, turn_id: "new-turn" } });
      return { status: "success", message: "step done" };
    });
    const ctx = harness({ todoBoard: PLAN_BOARD }, "plan");

    await runTurn(ctx.deps, "sess-1", "thread-1", "continue", [], "continue", {
      agentMode: "plan",
    });

    expect(ctx.run.todoBoard).toEqual({ ...PLAN_BOARD, turn_id: "new-turn" });
  });

  it("applies durable todo_board events to the owning session", async () => {
    vi.mocked(streamChat).mockImplementationOnce(async (_body, handlers: StreamHandlers) => {
      handlers.onEvent?.({
        kind: "todo_board",
        payload: {
          done: 2,
          total: 2,
          items: [
            { id: "one", text: "Research the product", done: true },
            { id: "two", text: "Summarize findings", done: true },
          ],
        },
      });
      return {};
    });
    const ctx = harness({ todoBoard: PLAN_BOARD }, "plan");

    await runTurn(ctx.deps, "sess-1", "thread-1", "keep going", [], "keep going", {
      agentMode: "plan",
    });

    expect(ctx.run.todoBoard).toEqual({
      done: 2,
      total: 2,
      items: [
        { id: "one", text: "Research the product", done: true },
        { id: "two", text: "Summarize findings", done: true },
      ],
    });
  });
});

describe("runTurn recovery event toasts", () => {
  it("shows a toast when a model repeats its reasoning", async () => {
    vi.mocked(streamChat).mockImplementationOnce(async (_body, handlers: StreamHandlers) => {
      handlers.onEvent?.({
        kind: "reasoning_loop",
        payload: { step: 1, action: "cancelled_model_call" },
      });
      return { status: "error", message: "Model repeated the same reasoning passage" };
    });
    const ctx = harness();

    await runTurn(ctx.deps, "sess-1", "thread-1", "do something", [], "do something", {
      agentMode: "normal",
    });

    expect(ctx.store.showToast).toHaveBeenCalledWith(
      expect.stringContaining("repeated its reasoning"),
    );
  });

  it("shows a toast when the step watchdog recovers a hung model call", async () => {
    vi.mocked(streamChat).mockImplementationOnce(async (_body, handlers: StreamHandlers) => {
      handlers.onEvent?.({
        kind: "step_watchdog",
        payload: { step: 3, deadline_s: 240.0, action: "cancelled_model_call" },
      });
      return { status: "error", message: "model call exceeded step watchdog deadline" };
    });
    const ctx = harness();

    await runTurn(ctx.deps, "sess-1", "thread-1", "do something", [], "do something", {
      agentMode: "normal",
    });

    expect(ctx.store.showToast).toHaveBeenCalledWith(
      expect.stringContaining("too long"),
    );
  });

  it("shows a toast when the browser is restarted after looking wedged", async () => {
    vi.mocked(streamChat).mockImplementationOnce(async (_body, handlers: StreamHandlers) => {
      handlers.onEvent?.({
        kind: "tool_guardrail",
        payload: {
          action: "block",
          code: "browser_hard_restart",
          message: "Browser looks wedged.",
          tool_name: "browser_open",
          count: 2,
          phase: "after_call",
          restart_outcome: "local browser process closed; will relaunch fresh",
          restart_failed: false,
        },
      });
      return { status: "success", message: "recovered and continued" };
    });
    const ctx = harness();

    await runTurn(ctx.deps, "sess-1", "thread-1", "open the page", [], "open the page", {
      agentMode: "normal",
    });

    expect(ctx.store.showToast).toHaveBeenCalledWith(
      expect.stringContaining("restarted it automatically"),
    );
  });

  it("shows a different toast when the automatic browser restart itself fails", async () => {
    // restart_failed is set once the actual restart attempt resolves (see
    // loop/controller.py) — the toast must reflect that outcome instead of
    // always showing an optimistic "restarted it" message that never
    // resolves when the restart genuinely failed.
    vi.mocked(streamChat).mockImplementationOnce(async (_body, handlers: StreamHandlers) => {
      handlers.onEvent?.({
        kind: "tool_guardrail",
        payload: {
          action: "block",
          code: "browser_hard_restart",
          message: "Browser looks wedged.",
          tool_name: "browser_open",
          count: 2,
          phase: "after_call",
          restart_outcome:
            "cannot restart machine browser — it is not owned by this session",
          restart_failed: true,
        },
      });
      return { status: "error", message: "browser restart failed" };
    });
    const ctx = harness();

    await runTurn(ctx.deps, "sess-1", "thread-1", "open the page", [], "open the page", {
      agentMode: "normal",
    });

    expect(ctx.store.showToast).toHaveBeenCalledWith(
      expect.stringContaining("the automatic restart failed"),
    );
  });

  it("shows a toast when browser recovery gives up for the turn", async () => {
    vi.mocked(streamChat).mockImplementationOnce(async (_body, handlers: StreamHandlers) => {
      handlers.onEvent?.({
        kind: "tool_guardrail",
        payload: {
          action: "halt",
          code: "browser_recovery_exhausted",
          message: "Browser restart budget exhausted this turn.",
          tool_name: "browser_open",
          count: 2,
          phase: "after_call",
        },
      });
      return { status: "error", message: "browser recovery exhausted" };
    });
    const ctx = harness();

    await runTurn(ctx.deps, "sess-1", "thread-1", "open the page", [], "open the page", {
      agentMode: "normal",
    });

    expect(ctx.store.showToast).toHaveBeenCalledWith(
      expect.stringContaining("gave up"),
    );
  });

  it("records a chronic halt and toasts once the same tool has halted 2+ turns in a row", async () => {
    vi.mocked(streamChat).mockImplementationOnce(async (_body, handlers: StreamHandlers) => {
      handlers.onEvent?.({
        kind: "tool_guardrail",
        payload: {
          action: "halt",
          code: "same_tool_failure_halt",
          message: "Stopped browser_evaluate: it failed 4 times this turn.",
          tool_name: "browser_evaluate",
          count: 2,
          phase: "after_call",
          chronic_halt_count: 3,
        },
      });
      return { status: "error", message: "guardrail halted" };
    });
    const setTaskChronicHalt = vi.fn();
    const ctx = harness({}, "normal", { setTaskChronicHalt });

    await runTurn(ctx.deps, "sess-1", "thread-1", "keep going", [], "keep going", {
      agentMode: "normal",
    });

    expect(setTaskChronicHalt).toHaveBeenCalledWith("browser_evaluate", 3);
    expect(ctx.store.showToast).toHaveBeenCalledWith(
      expect.stringContaining("browser_evaluate keeps failing the same way"),
    );
  });

  it("records but does not toast a first-time (non-chronic) guardrail halt", async () => {
    vi.mocked(streamChat).mockImplementationOnce(async (_body, handlers: StreamHandlers) => {
      handlers.onEvent?.({
        kind: "tool_guardrail",
        payload: {
          action: "halt",
          code: "same_tool_failure_halt",
          message: "Stopped bash: it failed 4 times this turn.",
          tool_name: "bash",
          count: 2,
          phase: "after_call",
          chronic_halt_count: 1,
        },
      });
      return { status: "error", message: "guardrail halted" };
    });
    const setTaskChronicHalt = vi.fn();
    const ctx = harness({}, "normal", { setTaskChronicHalt });

    await runTurn(ctx.deps, "sess-1", "thread-1", "keep going", [], "keep going", {
      agentMode: "normal",
    });

    expect(setTaskChronicHalt).toHaveBeenCalledWith("bash", 1);
    expect(ctx.store.showToast).not.toHaveBeenCalledWith(
      expect.stringContaining("keeps failing the same way"),
    );
  });

  it("does not toast for an ordinary tool_guardrail warning", async () => {
    vi.mocked(streamChat).mockImplementationOnce(async (_body, handlers: StreamHandlers) => {
      handlers.onEvent?.({
        kind: "tool_guardrail",
        payload: {
          action: "warn",
          code: "same_url_repeated_warning",
          message: "This URL was already fetched.",
          tool_name: "web_fetch",
          count: 2,
          phase: "after_call",
        },
      });
      return { status: "success", message: "ok" };
    });
    const ctx = harness();

    await runTurn(ctx.deps, "sess-1", "thread-1", "fetch it", [], "fetch it", {
      agentMode: "normal",
    });

    expect(ctx.store.showToast).not.toHaveBeenCalled();
  });
});

describe("runTurn live approval", () => {
  it("does not let heartbeats hide a waiting plan approval", async () => {
    const ctx = harness();
    const afterHeartbeat = {
      waitingApproval: false,
      status: "",
      statusLabel: "",
      approvalId: undefined as string | undefined,
    };
    vi.mocked(streamChat).mockImplementationOnce(async (_body, handlers: StreamHandlers) => {
      handlers.onEvent?.({
        kind: "approval_required",
        label: "Waiting for approval…",
        payload: {
          approval_id: "appr-1",
          action: "approve_plan",
          risk_class: "plan",
        },
      });
      handlers.onStatus?.("Still working…", {
        phase: "heartbeat",
        label: "Still working…",
      });
      afterHeartbeat.waitingApproval = ctx.run.waitingApproval;
      afterHeartbeat.status = ctx.run.status;
      afterHeartbeat.statusLabel = ctx.run.statusLabel;
      afterHeartbeat.approvalId = ctx.run.pendingApproval?.approval_id;
      return { status: "awaiting_plan_approval" };
    });
    await runTurn(ctx.deps, "sess-1", "thread-1", "build a listing", [], "build a listing");
    expect(afterHeartbeat.waitingApproval).toBe(true);
    expect(afterHeartbeat.status).toBe("waiting_approval");
    expect(afterHeartbeat.statusLabel).toBe("Awaiting Build");
    expect(afterHeartbeat.approvalId).toBe("appr-1");
  });

  it("keeps a live tool label when SSE heartbeats say Still working…", async () => {
    const ctx = harness();
    let afterHeartbeat = "";
    vi.mocked(streamChat).mockImplementationOnce(async (_body, handlers: StreamHandlers) => {
      handlers.onEvent?.({
        kind: "tool_started",
        label: "Running bash…",
        interesting: true,
        payload: { tool: "bash", name: "bash", attempt_id: "att-1" },
        tool_card: { name: "bash", status: "running", attempt_id: "att-1" },
      });
      handlers.onStatus?.("Still working…", {
        phase: "heartbeat",
        label: "Still working…",
      });
      afterHeartbeat = ctx.run.statusLabel;
      return { status: "ok" };
    });
    await runTurn(ctx.deps, "sess-1", "thread-1", "fix lint", [], "fix lint");
    expect(afterHeartbeat).toBe("Running bash…");
  });
});

describe("runTurn isolated desktop Live panel", () => {
  it("sends an explicit host choice even when the cached run still says machine", async () => {
    vi.mocked(streamChat).mockResolvedValueOnce({});
    const ctx = harness({ machineActive: true }, "normal", { machineActive: false });
    await runTurn(ctx.deps, "sess-1", "thread-1", "Use this Mac to render the video", [], "Use this Mac");
    expect(streamChat).toHaveBeenLastCalledWith(expect.objectContaining({
      machine_active: false, execution_target: "local",
    }), expect.anything(), expect.anything());
  });

  it("pins the remote host with the request instead of inheriting a stale machine", async () => {
    vi.mocked(streamChat).mockResolvedValueOnce({});
    const ctx = harness({ machineActive: true }, "normal", {
      machineActive: false, remoteActive: true, remoteHostId: "gpu-host",
    });
    await runTurn(ctx.deps, "sess-1", "thread-1", "render the video", [], "render the video");
    expect(streamChat).toHaveBeenLastCalledWith(expect.objectContaining({
      machine_active: false, execution_target: "remote", remote_host_id: "gpu-host",
    }), expect.anything(), expect.anything());
  });

  it("activates the correct session before sending a machine turn for planning", async () => {
    let activate!: () => void;
    vi.mocked(api).mockImplementationOnce(() => new Promise(resolve => { activate = () => resolve({ ok: true }); }));
    const callsBefore = vi.mocked(streamChat).mock.calls.length;
    vi.mocked(streamChat).mockResolvedValueOnce({});
    const ctx = harness({ machineActive: true });
    const pending = runTurn(ctx.deps, "sess-1", "thread-1", "hello", [], "hello");
    expect(api).toHaveBeenLastCalledWith("/api/sessions/sess-1/machine", expect.objectContaining({
      method: "POST", body: JSON.stringify({ active: true }),
    }));
    expect(vi.mocked(streamChat).mock.calls.length).toBe(callsBefore);
    // An unrelated selected-tab update must not change this turn's routing.
    ctx.store.machineActive = false;
    activate();
    await pending;
    expect(streamChat).toHaveBeenLastCalledWith(expect.objectContaining({ machine_active: true }), expect.anything(), expect.anything());
  });

  it("does not start host execution if requested machine activation fails", async () => {
    vi.mocked(api).mockRejectedValueOnce(new ApiError("Computer binding conflicts", 409));
    const before = vi.mocked(streamChat).mock.calls.length;
    const ctx = harness({ machineActive: true });
    await runTurn(ctx.deps, "sess-1", "thread-1", "hello", [], "hello");
    expect(vi.mocked(streamChat).mock.calls.length).toBe(before);
  });
  it("opens Live immediately for a natural-language desktop ask", async () => {
    vi.mocked(streamChat).mockResolvedValueOnce({});
    const ctx = harness();

    await runTurn(
      ctx.deps,
      "sess-1",
      "thread-1",
      "Start a Linux desktop and control it here",
      [],
      "Start a Linux desktop and control it here",
    );

    expect(ctx.store.machineActive).toBe(true);
    expect(ctx.store.pinLiveSurface).toHaveBeenCalledWith("computer");
    expect(ctx.run.machineActive).toBe(true);
  });

  it("stores machine readiness without replacing a pinned Monitor", async () => {
    vi.mocked(streamChat).mockImplementationOnce(async (_body, handlers: StreamHandlers) => {
      handlers.onEvent?.({
        kind: "machine_ready",
        payload: {
          novncUrl: "http://127.0.0.1:54759/vnc.html?autoconnect=1",
          message: "Linux computer ready",
        },
      });
      return {};
    });
    const ctx = harness();
    ctx.store.surfacePinnedAt = Date.now();
    ctx.store.liveSurface = "computer";

    await runTurn(ctx.deps, "sess-1", "thread-1", "hello", [], "hello");

    expect(ctx.store.machineActive).toBe(true);
    expect(ctx.store.machineNovncUrl).toContain("vnc.html");
    expect(ctx.store.setLiveSurface).not.toHaveBeenCalled();
    expect(ctx.run.machineActive).toBe(true);
  });

  it("routes Machine Desktop frames to Live instead of macOS Computer", async () => {
    vi.mocked(streamChat).mockImplementationOnce(async (_body, handlers: StreamHandlers) => {
      const frame = {
        app: "Machine Desktop",
        url: "/session/shot.png",
        action: "screenshot",
      };
      handlers.onComputerFrame?.(frame, "computer_frame");
      handlers.onEvent?.({ kind: "computer_frame", ...frame });
      return {};
    });
    const ctx = harness();

    await runTurn(ctx.deps, "sess-1", "thread-1", "click the dock", [], "click the dock");

    expect(ctx.store.machineActive).toBe(true);
    expect(ctx.run.machineActive).toBe(true);
    expect(ctx.store.setLiveSurface).toHaveBeenCalledWith("computer");
  });

  it("does not open macOS Computer on a bare computer_* card", async () => {
    vi.mocked(streamChat).mockImplementationOnce(async (_body, handlers: StreamHandlers) => {
      handlers.onToolCard?.({ name: "computer_click", status: "running" });
      return {};
    });
    const ctx = harness();

    await runTurn(ctx.deps, "sess-1", "thread-1", "click the dock", [], "click the dock");

    expect(ctx.store.setLiveSurface).not.toHaveBeenCalledWith("computer");
    expect(ctx.store.machineActive).toBe(false);
  });

  it("leaves Trace pinned after the desktop is already up", async () => {
    const ctx = harness();
    vi.mocked(streamChat).mockImplementationOnce(async (_body, handlers: StreamHandlers) => {
      handlers.onEvent?.({
        kind: "machine_ready",
        payload: {
          novncUrl: "http://127.0.0.1:54759/vnc.html?autoconnect=1",
          message: "Linux computer ready",
        },
      });
      ctx.store.surfacePinnedAt = Date.now();
      ctx.store.liveRevealedThisTurn = true;
      ctx.store.setLiveSurface.mockClear();
      handlers.onToolCard?.({ name: "computer_click", status: "running" });
      return {};
    });

    await runTurn(ctx.deps, "sess-1", "thread-1", "hello", [], "hello");

    expect(ctx.store.machineActive).toBe(true);
    expect(ctx.store.setLiveSurface).not.toHaveBeenCalled();
  });
});

describe("runTurn model + effort", () => {
  it("sends a pinned reasoning effort with the chat body", async () => {
    vi.mocked(streamChat).mockResolvedValueOnce({});
    const ctx = harness({}, "normal", {
      modelOverride: "gemini-pro",
      prefs: {
        mainModel: "gemini-pro",
        plannerModel: "",
        workerModel: "",
        voiceReply: false,
        reasoningEffort: "high",
      },
    });

    await runTurn(ctx.deps, "sess-1", "thread-1", "hello", [], "hello");

    const body = vi.mocked(streamChat).mock.calls.at(-1)?.[0];
    expect(body).toEqual(
      expect.objectContaining({
        model: "gemini-pro",
        effort: "high",
        model_roles: { planner: "", executor: "gemini-pro" },
      }),
    );
  });

  it("does not let Settings worker clobber the composer model", async () => {
    vi.mocked(streamChat).mockResolvedValueOnce({});
    const ctx = harness({}, "normal", {
      modelOverride: "kimi-k3",
      prefs: {
        mainModel: "kimi-k3",
        plannerModel: "glm-5.3",
        workerModel: "glm-5.3",
        voiceReply: false,
      },
    });

    await runTurn(ctx.deps, "sess-1", "thread-1", "hello", [], "hello");

    expect(vi.mocked(streamChat).mock.calls.at(-1)?.[0]).toEqual(
      expect.objectContaining({
        model: "kimi-k3",
        model_roles: { planner: "glm-5.3", executor: "kimi-k3" },
      }),
    );
  });

  it("uses the live pin even when it matches the catalog default", async () => {
    vi.mocked(streamChat).mockResolvedValueOnce({});
    const ctx = harness({}, "normal", {
      modelOverride: "glm-5.3",
      defaultModel: "glm-5.3",
      prefs: {
        mainModel: "glm-5.3",
        plannerModel: "",
        workerModel: "glm-5.3",
        voiceReply: false,
      },
    });

    await runTurn(ctx.deps, "sess-1", "thread-1", "hello", [], "hello");

    expect(vi.mocked(streamChat).mock.calls.at(-1)?.[0]).toEqual(
      expect.objectContaining({
        model: "glm-5.3",
        model_roles: { planner: "", executor: "glm-5.3" },
      }),
    );
  });

  it("omits effort when the picker is Auto", async () => {
    vi.mocked(streamChat).mockResolvedValueOnce({});
    const ctx = harness();
    await runTurn(ctx.deps, "sess-1", "thread-1", "hello", [], "hello");
    expect(vi.mocked(streamChat).mock.calls.at(-1)?.[0].effort).toBeUndefined();
  });

  it.each(["error", "failed", "invalid_output", "unverified"])("preserves streamed narration when a turn ends with %s", async (status) => {
    vi.mocked(streamChat).mockImplementationOnce(async (_body, handlers) => {
      handlers.onDelta?.("I read the document and found three missing fields.");
      return { status, message: "Verification failed before completion." };
    });
    const ctx = harness();
    await runTurn(ctx.deps, "sess-1", "thread-1", "Review document", [], "Review document");
    const assistant = [...ctx.run.messages].reverse().find((m) => m.role === "assistant");
    expect(assistant?.text).toContain("three missing fields");
    expect(assistant?.text).toContain("Verification failed before completion.");
    expect(assistant?.statusLabel).toBe(["invalid_output", "unverified"].includes(status) ? "Verification unresolved" : "Error");
    expect(assistant?.failureCause).toBe(["invalid_output", "unverified"].includes(status)
      ? "The response was delivered, but task completion could not be verified."
      : "Verification failed before completion.");
  });

  it("marks a model-unavailable failure as error with a surfaced cause", async () => {
    const cause =
      "No model was available to handle this request. " +
      "Open Settings → Models and connect at least one provider.";
    vi.mocked(streamChat).mockResolvedValueOnce({ status: "error", message: cause });
    const ctx = harness();

    await runTurn(ctx.deps, "sess-1", "thread-1", "make a brochure", [], "make a brochure");

    const assistant = [...ctx.run.messages]
      .reverse()
      .find((m) => m.role === "assistant");
    expect(assistant?.statusLabel).toBe("Error");
    expect(assistant?.failureCause).toContain("connect at least one provider");
    // Run must be terminal-error, never silently "success".
    expect(ctx.run.status).toBe("error");
    expect(ctx.store).toMatchObject({ runStatus: "error" });
  });

  it("keeps streamed outcome checks after the final reply arrives", async () => {
    const outcomes = [{ id: "altus", label: "Altus application", status: "pass" },
      { id: "workday", label: "Workday application", status: "unresolved" }];
    vi.mocked(streamChat).mockImplementationOnce(async (_body, handlers) => {
      handlers.onEvent?.({ kind: "verification", payload: { outcomes } });
      return { status: "unverified", message: "One confirmed; one unknown.", failure_cause: "Missing confirmation" };
    });
    const ctx = harness();
    await runTurn(ctx.deps, "sess-1", "thread-1", "Check applications", [], "Check applications");
    const assistant = [...ctx.run.messages].reverse().find(m => m.role === "assistant");
    expect(assistant?.verificationOutcomes).toEqual(outcomes);
  });

  it("uses the actual terminal cause instead of repeating the answer or an earlier error", async () => {
    const cause = "The model provider timed out after fallback attempts.";
    vi.mocked(streamChat).mockImplementationOnce(async (_body, handlers) => {
      handlers.onEvent?.({ kind: "error", payload: { error: "Earlier browser failure" } });
      return { status: "error", message: "Research is saved. The task remains incomplete.", failure_cause: cause };
    });
    const ctx = harness();
    await runTurn(ctx.deps, "sess-1", "thread-1", "Find jobs", [], "Find jobs");
    const assistant = ctx.run.messages.at(-1);
    expect(assistant?.failureCause).toBe(cause);
    expect(assistant?.text).toContain("Research is saved");
    expect(ctx.run.status).toBe("error");
  });

  it("keeps the failure detail of an unrecognized failure from the done event", async () => {
    const cause = "The agent stopped before completing this turn. Inspect its recorded activity.";
    vi.mocked(streamChat).mockImplementationOnce(async () => ({ status: "error", message: cause, failure_cause: cause,
      failure_code: "execution_failed", failure_detail: "Image attachments exceed 4 MiB per model turn" }));
    const ctx = harness();
    await runTurn(ctx.deps, "sess-1", "thread-1", "", [], "");
    expect(ctx.run.messages.at(-1)?.failureDetail).toBe("Image attachments exceed 4 MiB per model turn");
    vi.mocked(streamChat).mockImplementationOnce(async () => ({ status: "success", message: "Done.", failure_detail: null }));
    const next = harness();
    await runTurn(next.deps, "sess-1", "thread-1", "Again", [], "Again");
    expect(next.run.messages.at(-1)?.failureDetail).toBeUndefined();
  });

  it("replaces the draft with inspection corrections instead of showing two answers", async () => {
    vi.mocked(streamChat).mockImplementationOnce(async (_body, handlers) => {
      handlers.onDelta?.("The rate is 3%.");
      handlers.onEvent?.({ kind: "answer_revision", payload: {
        original: "The rate is 3%.", replacement: "The rate is 4%.",
      } });
      return { status: "success", message: "The rate is 4%." };
    });
    const ctx = harness();
    await runTurn(ctx.deps, "sess-1", "thread-1", "Check the rate", [], "Check the rate");
    const assistant = ctx.run.messages.find((m) => m.role === "assistant");
    const visible = assistant?.blocks?.filter((b) => b.type === "narrative")
      .map((b) => b.type === "narrative" ? b.text : "").join("");
    expect(visible).toBe("The rate is 4%.");
  });

  it("keeps the detailed answer visible when a stalled turn ends with a short recap", async () => {
    const answer = "Detailed reading. ".repeat(100);
    const recap = "The reading is complete — delivered in full above.";
    vi.mocked(streamChat).mockImplementationOnce(async (_body, handlers) => {
      handlers.onDelta?.(answer);
      return { status: "no_progress", message: recap };
    });
    const ctx = harness();
    await runTurn(ctx.deps, "sess-1", "thread-1", "Read", [], "Read");
    const assistant = ctx.run.messages.find((m) => m.role === "assistant");
    const visible = assistant?.blocks?.filter((b) => b.type === "narrative")
      .map((b) => b.type === "narrative" ? b.text : "").join("");
    expect(visible).toContain(answer);
    expect(visible).toContain(recap);
    expect(ctx.run.status).toBe("interrupted");
  });

  it("shows a stalled turn as Stopped, never as Done", async () => {
    vi.mocked(streamChat).mockResolvedValueOnce({
      status: "no_progress",
      message: "I wasn't able to finish the form — the run stalled.",
    });
    const ctx = harness();

    await runTurn(ctx.deps, "sess-1", "thread-1", "apply", [], "apply");

    const assistant = [...ctx.run.messages]
      .reverse()
      .find((m) => m.role === "assistant");
    expect(assistant?.statusLabel).toBe("Stopped · stalled, no progress");
    expect(assistant?.failureCause).toBeUndefined();
    expect(ctx.run.status).toBe("interrupted");
    expect(ctx.store).toMatchObject({
      runStatus: "interrupted",
      statusLabel: "Stopped · stalled, no progress",
    });
  });

  it("does not attach a failure cause to a successful turn", async () => {
    vi.mocked(streamChat).mockResolvedValueOnce({ status: "success", message: "All done." });
    const ctx = harness();

    await runTurn(ctx.deps, "sess-1", "thread-1", "hello", [], "hello");

    const assistant = [...ctx.run.messages]
      .reverse()
      .find((m) => m.role === "assistant");
    expect(assistant?.failureCause).toBeUndefined();
    expect(ctx.run.status).toBe("success");
  });

  it("does not treat an ask_user clarification as a failure", async () => {
    vi.mocked(streamChat).mockResolvedValueOnce({
      status: "ask_user",
      message: "Which color palette do you prefer?",
    });
    const ctx = harness();

    await runTurn(ctx.deps, "sess-1", "thread-1", "design it", [], "design it");

    const assistant = [...ctx.run.messages]
      .reverse()
      .find((m) => m.role === "assistant");
    expect(assistant?.failureCause).toBeUndefined();
    expect(ctx.run.status).not.toBe("error");
  });
});

it("resumes a saved task by reference while preserving the composer's draft", async () => {
  vi.mocked(streamChat).mockResolvedValueOnce({ status: "success" });
  const ctx = harness({}, "normal", { draft: "My next question" });
  await runTurn(ctx.deps, "sess-1", "thread-1", "Resume task", [], "Resume task", { resumeTurnId: "saved-turn" });
  expect(vi.mocked(streamChat).mock.calls.at(-1)?.[0]).toEqual(expect.objectContaining({
    session_id: "sess-1", resume_turn_id: "saved-turn", message: "Resume task",
  }));
  expect((ctx.store as Record<string, unknown>).draft).toBe("My next question");
});

it("sends resume: true only for the Resume task button, never for typed text", async () => {
  vi.mocked(streamChat).mockResolvedValue({ status: "success" });
  const ctx = harness();
  await runTurn(ctx.deps, "sess-1", "thread-1", "Resume task", [], "Resume task", { resume: true, resumeTurnId: "saved-turn" });
  expect(vi.mocked(streamChat).mock.calls.at(-1)?.[0]).toEqual(expect.objectContaining({
    session_id: "sess-1", resume: true, resume_turn_id: "saved-turn",
  }));
  await runTurn(ctx.deps, "sess-1", "thread-1", "Resume task", [], "Resume task");
  const typed = vi.mocked(streamChat).mock.calls.at(-1)?.[0];
  expect(typed).toEqual(expect.objectContaining({ message: "Resume task" }));
  expect(typed).not.toHaveProperty("resume");
  expect(typed?.resume_turn_id).toBeUndefined();
});
it("shows the server's refusal of a resume without a budget and keeps the saved reply", async () => {
  const openSession = vi.fn().mockResolvedValue(undefined);
  const showToast = vi.fn();
  const original: ChatMessage = { id: "saved", role: "assistant", text: "Saved progress", turnId: "old" };
  const ctx = harness({ messages: [original], status: "interrupted", statusLabel: "Task paused" }, "normal", { openSession, showToast });
  const refusal = "This request has used all 40 of its model calls, so it cannot continue. Start a new request instead.";
  vi.mocked(streamChat).mockRejectedValueOnce(new ApiError(refusal, 409));
  await runTurn(ctx.deps, "sess-1", "thread-1", "Resume task", [], "Resume task", { resume: true, resumeTurnId: "old" });
  expect(showToast).toHaveBeenCalledWith(refusal);
  expect(ctx.run.messages).toEqual([original]);
  expect(ctx.run.status).toBe("interrupted");
});
it("refreshes a rejected resume without creating a failed agent reply or duplicate banners", async () => {
  const openSession = vi.fn().mockResolvedValue(undefined);
  const original: ChatMessage = { id: "saved", role: "assistant", text: "Saved progress", turnId: "old" };
  const ctx = harness({ messages: [original], status: "interrupted", statusLabel: "Task paused" }, "normal", { openSession });
  vi.mocked(streamChat).mockRejectedValueOnce(new ApiError("This chat has a newer task. Refresh before resuming.", 400));
  await runTurn(ctx.deps, "sess-1", "thread-1", "Resume task", [], "Resume task", { resumeTurnId: "old" });
  expect(ctx.run.messages).toEqual([original]);
  expect(ctx.run.status).toBe("interrupted");
  expect((ctx.store as Record<string, unknown>).error).toBeNull();
  expect(openSession).toHaveBeenCalledWith("sess-1");
});

it("does not certify a completion frame without a run status", async () => {
  vi.mocked(streamChat).mockResolvedValueOnce({ message: "A response exists." });
  const ctx = harness();
  await runTurn(ctx.deps, "sess-1", "thread-1", "Check the task", [], "Check the task");
  expect(ctx.run.status).toBe("interrupted");
  expect(ctx.run.statusLabel).toBe("Completion unconfirmed");
});

it("preserves an unacknowledged reply instead of reopening old history or replaying the request", async () => {
  vi.mocked(streamChat).mockRejectedValueOnce(new StreamDroppedError("pending", "sess-1", "thread-1"));
  vi.mocked(api).mockResolvedValueOnce({ active_turn: null, status: "ask_user" });
  const openSession = vi.fn();
  const ctx = harness({}, "normal", { openSession });
  await runTurn(ctx.deps, "sess-1", "thread-1", "Top 3 only", [], "Top 3 only");
  expect(ctx.run.status).toBe("interrupted");
  expect(ctx.run.sending).toBe(false);
  expect(ctx.run.messages.find(m => m.role === "user")?.text).toBe("Top 3 only");
  expect(ctx.run.messages.find(m => m.role === "assistant")?.streaming).toBe(false);
  expect(ctx.run.statusLabel).toContain("Delivery unconfirmed");
  expect(ctx.run.messages.find(m => m.role === "assistant")?.resultStatus).toBe("delivery_unconfirmed");
  expect(openSession).not.toHaveBeenCalled();
  expect(ctx.reattach).not.toHaveBeenCalled();
});

it("does not treat the previous active turn as acknowledgement of a new reply", async () => {
  vi.mocked(streamChat).mockRejectedValueOnce(new StreamDroppedError("pending", "sess-1", "thread-1"));
  vi.mocked(api).mockResolvedValueOnce({ active_turn: { turn_id: "old-turn" } });
  const ctx = harness({ transport: { ...emptyTransport(), turnId: "old-turn" } });
  await runTurn(ctx.deps, "sess-1", "thread-1", "New answer", [], "New answer");
  expect(ctx.run.status).toBe("interrupted");
  expect(ctx.reattach).not.toHaveBeenCalled();
});

it("reattaches a confirmed new turn after a headerless connection drop", async () => {
  vi.mocked(streamChat).mockRejectedValueOnce(new StreamDroppedError("pending", "sess-1", "thread-1"));
  vi.mocked(api).mockResolvedValueOnce({ active_turn: { turn_id: "new-turn" } });
  const ctx = harness();
  await runTurn(ctx.deps, "sess-1", "thread-1", "Answer", [], "Answer");
  expect(ctx.reattach).toHaveBeenCalledWith("sess-1", "thread-1", "new-turn", expect.anything());
  expect(ctx.run.messages.filter(m => m.role === "user")).toHaveLength(1);
});

it("ends reconnecting when the read-only acceptance check times out", async () => {
  vi.mocked(streamChat).mockRejectedValueOnce(new StreamDroppedError("pending", "sess-1", "thread-1"));
  vi.mocked(api).mockRejectedValueOnce(new DOMException("Timed out", "TimeoutError"));
  const ctx = harness();
  await runTurn(ctx.deps, "sess-1", "thread-1", "Answer", [], "Answer");
  expect(ctx.run.status).toBe("interrupted");
  expect(ctx.run.sending).toBe(false);
  expect(ctx.reattach).not.toHaveBeenCalled();
});

it("does not revive Stop after a pending acceptance check finishes", async () => {
  vi.mocked(streamChat).mockRejectedValueOnce(new StreamDroppedError("pending", "sess-1", "thread-1"));
  let resolve!: (value: unknown) => void;
  vi.mocked(api).mockImplementationOnce(() => new Promise(r => { resolve = r; }));
  const ctx = harness();
  const pending = runTurn(ctx.deps, "sess-1", "thread-1", "Answer", [], "Answer");
  await Promise.resolve();
  await Promise.resolve();
  ctx.deps.updateRun("sess-1", r => ({ ...r, status: "cancelled", sending: false }));
  resolve({ active_turn: { turn_id: "late-turn" } });
  await pending;
  expect(ctx.run.status).toBe("cancelled");
  expect(ctx.reattach).not.toHaveBeenCalled();
});

it("does not drain queued work after an unsuccessful turn", async () => {
  const ctx=harness();
  ctx.deps.flushQueue=vi.fn().mockResolvedValue(undefined);
  vi.mocked(streamChat).mockResolvedValueOnce({status:"error",message:"provider failed"});
  await runTurn(ctx.deps,"sess-1","thread-1","hello",[],"hello");
  expect(ctx.deps.flushQueue).not.toHaveBeenCalled();
});

it("retains the reply when an unbound admission ends without execution", async () => {
  const {pendingDeliveries}=await import("./delivery");
  const ctx=harness();
  let requestId="";
  vi.mocked(streamChat).mockImplementationOnce(async body=>{
    requestId=body.client_request_id!;
    throw new StreamDroppedError("pending","sess-1","thread-1",0,requestId);
  });
  vi.mocked(api).mockResolvedValue({receipt:{state:"finished",turn_id:"",result_status:"interrupted"}});
  await runTurn(ctx.deps,"sess-1","thread-1","keep this reply",[],"keep this reply");
  expect(pendingDeliveries("sess-1").find(item=>item.id===requestId)?.text).toBe("keep this reply");
});

it("recovers a saved rejection after losing the admission response", async () => {
  const ctx = harness();
  vi.mocked(streamChat).mockImplementationOnce(async body => {
    throw new StreamDroppedError("pending", "sess-1", "thread-1", 0, body.client_request_id);
  });
  vi.mocked(api).mockResolvedValueOnce({
    receipt: { state: "rejected", turn_id: "", message: "Model credentials are unavailable" },
  });
  await runTurn(ctx.deps, "sess-1", "thread-1", "Keep this reply", [], "Keep this reply");
  expect(ctx.run.status).toBe("error");
  expect(ctx.run.messages.find(message => message.role === "user")?.text).toBe("Keep this reply");
  expect(ctx.run.messages.find(message => message.role === "assistant")?.failureCause).toBe("Model credentials are unavailable");
  expect(ctx.reattach).not.toHaveBeenCalled();
});

it("warns when browser recovery persistence is unavailable", async () => {
  vi.stubGlobal("localStorage",{get length(){return 0;},key:()=>null,getItem:()=>null,setItem(){throw new Error("quota");},removeItem(){}});
  const ctx=harness();
  vi.mocked(streamChat).mockResolvedValueOnce({status:"success",message:"ok"});
  await runTurn(ctx.deps,"sess-1","thread-1","hello",[],"hello");
  expect(ctx.store.showToast).toHaveBeenCalledWith("Browser recovery storage is unavailable. Keep this tab open until delivery is confirmed.");
});
