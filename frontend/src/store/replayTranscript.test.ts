import { describe, expect, it, vi } from "vitest";
import type { ChatMessage } from "../api/types";
import { hydrateMessageTranscripts, matchTurnsToAssistants, restoreMissingAssistantRows, turnIdOf } from "./replayTranscript";
import { orderSteeringMessages } from "../lib/steering";

function assistant(id: string, text: string, turnId?: string): ChatMessage {
  return { id, role: "assistant", text, turnId };
}

it("restores native browser failures with their recorded details and target", async () => {
  const failure = JSON.stringify({status: "error", error_code: "execution_failed", summary: "Control not found"});
  const fetcher = vi.spyOn(globalThis, "fetch").mockResolvedValue(new Response(JSON.stringify({ events: [
    { sequence: 1, kind: "tool_card", tool_card: { id: "inspect", name: "browser_inspect", status: "pending" } },
    { sequence: 2, kind: "tool_card", tool_card: { id: "inspect", name: "browser_inspect", status: "failed",
      result_preview: failure, attempt_id: "native-2", browserTarget: {browserId: "browser", tabId: "tab"} } },
  ] }), {headers: {"Content-Type": "application/json"}}));
  try {
    const messages: ChatMessage[] = [{id: "u", role: "user", text: "Inspect", turnId: "t"}, assistant("a", "Could not inspect.", "t")];
    const restored = await hydrateMessageTranscripts("s", "thread", messages, [{id: "t", objective: "Inspect", status: "success"}], "");
    expect(restored[1].blocks?.filter(block => block.type === "tool")).toEqual([
      expect.objectContaining({type: "tool", card: expect.objectContaining({id: "inspect", status: "failed", resultPreview: failure,
        attemptId: "native-2", browserTarget: {browserId: "browser", tabId: "tab"}})}),
    ]);
    expect(restored[1].text).toBe("Could not inspect.");
  } finally { fetcher.mockRestore(); }
});

it("restores a progress update and its tool activity without changing the final answer", async () => {
  const fetcher = vi.spyOn(globalThis, "fetch").mockResolvedValue(new Response(JSON.stringify({ events: [
    { sequence: 1, kind: "progress", payload: { source_kind: "assistant_progress", text: "I’ll check the listing.", step: 1 } },
    { sequence: 2, kind: "tool_started", tool_card: { id: "open", name: "browser_open", status: "running" } },
    { sequence: 3, kind: "tool_completed", tool_card: { id: "open", name: "browser_open", status: "done" } },
  ] }), { headers: { "Content-Type": "application/json" } }));
  try {
    const messages: ChatMessage[] = [{ id: "u", role: "user", text: "Review", turnId: "t" }, assistant("a", "The listing is open.", "t")];
    const restored = await hydrateMessageTranscripts("s", "thread", messages, [{ id: "t", objective: "Review", status: "success" }], "");
    expect(restored[1].text).toBe("The listing is open.");
    expect(restored[1].blocks?.map(block => block.type)).toEqual(["progress", "tool", "narrative"]);
    expect(restored[1].blocks?.[0]).toMatchObject({ type: "progress", text: "I’ll check the listing." });
    expect(restored[1].blocks?.[1]).toMatchObject({ type: "tool", card: { id: "open", status: "done" } });
  } finally { fetcher.mockRestore(); }
});

it("restores a correction at its sent position despite transient block counts", async () => {
  const fetcher = vi.spyOn(globalThis, "fetch").mockImplementation(async () => new Response(JSON.stringify({ events: [
    { sequence: 1, kind: "progress", payload: { source_kind: "assistant_narrative", text: "Before", offset: 0 } },
    { sequence: 2, kind: "progress", payload: { source_kind: "user_inject", message: "Change direction" } },
    { sequence: 3, kind: "tool_started", tool_card: { id: "tool", name: "read_file", status: "running" } },
    { sequence: 4, kind: "progress", payload: { source_kind: "assistant_narrative", text: "After", offset: 6 } },
  ] }), { status: 200, headers: { "Content-Type": "application/json" } }));
  try {
    const messages = await hydrateMessageTranscripts("s", "thread", [
      { id: "u", role: "user", text: "Task", turnId: "t" },
      { id: "steer", role: "user", text: "Change direction", turnId: "t", steering: "pending",
        steeringBoundary: { blockCount: 99, tailTextLength: 0, afterSequence: 1 } },
      assistant("a", "BeforeAfter", "t"),
    ], [{ id: "t", objective: "Task", status: "success" }], "");
    const ordered = orderSteeringMessages(messages);
    expect(ordered.map((m) => m.text)).toEqual(["Task", "Before", "Change direction", "After"]);
    expect(messages.find((m) => m.id === "steer")?.steering).toBe("applied");
    expect(ordered[3].blocks?.some((b) => b.type === "tool")).toBe(true);
    const again = await hydrateMessageTranscripts("s", "thread", messages,
      [{ id: "t", objective: "Task", status: "success" }], "");
    expect(orderSteeringMessages(again).map(message => message.text))
      .toEqual(["Task", "Before", "Change direction", "After"]);
  } finally { fetcher.mockRestore(); }
});

it("rebuilds steering prefixes independently of completed item replay cursors", async () => {
  const events = [
    { sequence: 1, kind: "stream_item", payload: { item_id: "before", channel: "text", state: "completed", text: "Before", base_offset: 0 } },
    { sequence: 2, kind: "progress", payload: { source_kind: "user_inject", message: "Change direction" } },
    { sequence: 3, kind: "stream_item", payload: { item_id: "after", channel: "text", state: "completed", text: "After", base_offset: 6 } },
  ];
  const fetcher = vi.spyOn(globalThis, "fetch").mockImplementation(async () => new Response(
    JSON.stringify({ events }), { headers: { "Content-Type": "application/json" } },
  ));
  try {
    const messages: ChatMessage[] = [
      { id: "u", role: "user", text: "Task", turnId: "t" },
      { id: "steer", role: "user", text: "Change direction", turnId: "t", steering: "pending",
        steeringBoundary: { blockCount: 99, tailTextLength: 0, afterSequence: 1 } },
      assistant("a", "BeforeAfter", "t"),
    ];
    const first = await hydrateMessageTranscripts("s", "thread", messages, [{ id: "t", objective: "Task" }], "");
    expect(first.at(-1)?.streamCursor?.sequence).toBe(3);
    const second = await hydrateMessageTranscripts("s", "thread", first, [{ id: "t", objective: "Task" }], "");
    expect(orderSteeringMessages(second).map(message => message.text))
      .toEqual(["Task", "Before", "Change direction", "After"]);
  } finally { fetcher.mockRestore(); }
});

it("pairs failed continuation turns with expanded legacy user messages", () => {
  const restored = restoreMissingAssistantRows([
    { id: "u", role: "user", text: "Current user message: Go ahead\nEarlier request being continued: What’s next?\nUse the earlier request and session state to determine the remaining work." },
  ], [{ id: "t", objective: "Go ahead", status: "error" }], "");
  expect(restored).toHaveLength(2);
  expect(restored[1]).toMatchObject({ role: "assistant", turnId: "t", statusLabel: "Error" });
});

it("restores a failed turn's cause without dropping earlier narration after reload", async () => {
  const fetcher = vi.spyOn(globalThis, "fetch").mockResolvedValue(new Response(JSON.stringify({ events: [
    { sequence: 1, kind: "progress", payload: { source_kind: "assistant_narrative", text: "Checking the page.", offset: 0 } },
    { sequence: 2, kind: "failed", payload: { error: "Controller exception", status: "error" } },
  ] }), { status: 200, headers: { "Content-Type": "application/json" } }));
  try {
    const messages = await hydrateMessageTranscripts("s", "thread", [
      { id: "u", role: "user", text: "check" },
    ], [{ id: "t", objective: "check", status: "error" }], "");
    expect(messages[1]).toMatchObject({ text: "Checking the page.", failureCause: "Controller exception", statusLabel: "Error" });
    expect(messages[1].blocks).toContainEqual(expect.objectContaining({ type: "narrative", text: "Checking the page." }));
  } finally { fetcher.mockRestore(); }
});

describe("turn-bound transcript hydration", () => {
  it("matches assistant messages by persisted turn_id instead of position", () => {
    const messages: ChatMessage[] = [
      { id: "u1", role: "user", text: "first" },
      assistant("a1", "old answer", "turn-old"),
      { id: "u2", role: "user", text: "second" },
      assistant("a2", "new answer", "turn-new"),
    ];
    expect(matchTurnsToAssistants(messages, ["turn-new", "turn-old"])).toEqual([
      { turnId: "turn-new", messageIndex: 3 },
      { turnId: "turn-old", messageIndex: 1 },
    ]);
  });

  it("falls back to trailing positional pairing when turn ids are missing", () => {
    const messages: ChatMessage[] = [
      { id: "u1", role: "user", text: "first" },
      assistant("a1", "one"),
      { id: "u2", role: "user", text: "second" },
      assistant("a2", "two"),
    ];
    expect(matchTurnsToAssistants(messages, ["t1", "t2"])).toEqual([
      { turnId: "t1", messageIndex: 1 },
      { turnId: "t2", messageIndex: 3 },
    ]);
  });

  it("reads turn_id from either id or turn_id", () => {
    expect(turnIdOf({ id: "abc" })).toBe("abc");
    expect(turnIdOf({ turn_id: "def" })).toBe("def");
  });
});


it("restores narration for all visible turns, including turns older than eight", async () => {
  const messages = Array.from({ length: 10 }, (_, i) => assistant(`a${i}`, "Done.", `t${i}`));
  const fetcher = vi.spyOn(globalThis, "fetch").mockImplementation(async () => new Response(JSON.stringify({
    events: [
      { kind: "progress", payload: { source_kind: "assistant_narrative", text: "Opening the browser.", offset: 0 } },
      { kind: "tool_started", tool_card: { id: "b", name: "browser_open", status: "running" } },
    ],
  }), { status: 200, headers: { "Content-Type": "application/json" } }));
  try {
    const restored = await hydrateMessageTranscripts("s", "thread", messages,
      messages.map((m) => ({ turn_id: m.turnId })), "");
    expect(fetcher).toHaveBeenCalledTimes(10);
    for (const message of restored) {
      expect(message.blocks?.[0]).toMatchObject({ type: "narrative", text: "Opening the browser." });
      expect(message.blocks?.at(-1)).toMatchObject({ type: "narrative", text: "Done." });
    }
  } finally {
    fetcher.mockRestore();
  }
});

it("restores cancelled turns with only user messages from the durable journal", async () => {
  const fetcher = vi.spyOn(globalThis, "fetch").mockImplementation(async () => new Response(JSON.stringify({
    events: [
      { kind: "progress", payload: { source_kind: "assistant_narrative", text: "Checking integrations.", offset: 0 } },
      { kind: "tool_completed", tool_card: { name: "mcp_list_servers", status: "error", error_preview: "Authentication failed" } },
    ],
  }), { status: 200, headers: { "Content-Type": "application/json" } }));
  try {
    const restored = await hydrateMessageTranscripts("s", "thread", [
      { id: "u1", role: "user", text: "check" },
      { id: "u2", role: "user", text: "retry" },
    ], [
      { id: "t1", objective: "check", status: "cancelled" },
      { id: "t2", objective: "retry", status: "cancelled" },
    ], "");
    expect(restored.map((m) => m.role)).toEqual(["user", "assistant", "user", "assistant"]);
    for (const m of restored.filter((m) => m.role === "assistant")) {
      expect(m.statusLabel).toBe("Cancelled");
      expect(m.blocks).toContainEqual(expect.objectContaining({ type: "narrative", text: "Checking integrations." }));
      expect(m.blocks).toContainEqual(expect.objectContaining({ type: "tool", card: expect.objectContaining({ name: "mcp_list_servers", status: "error" }) }));
    }
    const again = await hydrateMessageTranscripts("s", "thread", restored, [
      { id: "t1", objective: "check", status: "cancelled" },
      { id: "t2", objective: "retry", status: "cancelled" },
    ], "");
    expect(again).toHaveLength(4);
    expect(fetcher).toHaveBeenCalledTimes(4);
  } finally { fetcher.mockRestore(); }
});


it("keeps replies attached to their own turns around an interrupted turn", () => {
  const messages: ChatMessage[] = [
    { id: "u1", role: "user", text: "one" }, assistant("a1", "answer one"),
    { id: "u2", role: "user", text: "two" },
    { id: "u3", role: "user", text: "three" }, assistant("a3", "answer three"),
  ];
  const restored = restoreMissingAssistantRows(messages, [
    { id: "t1", objective: "one", status: "success" },
    { id: "t2", objective: "two", status: "cancelled" },
    { id: "t3", objective: "three", status: "success" },
  ], "");
  expect(restored.filter((m) => m.role === "assistant").map((m) => [m.turnId, m.text])).toEqual([
    ["t1", "answer one"], ["t2", ""], ["t3", "answer three"],
  ]);
  expect(messages).toHaveLength(5);
  expect(messages[1].turnId).toBeUndefined();
});

it("leaves the active turn to streaming reattachment", () => {
  const messages: ChatMessage[] = [{ id: "u", role: "user", text: "go" }];
  expect(restoreMissingAssistantRows(messages, [{ id: "active", objective: "go", status: "running" }], "active")).toHaveLength(1);
  expect(matchTurnsToAssistants([assistant("a", "partial", "active")], ["old"])).toEqual([]);
});

it("matches repeated requests to separate turns and ignores unrelated history", () => {
  const restored = restoreMissingAssistantRows([
    { id: "u1", role: "user", text: "go" }, { id: "u2", role: "user", text: "go" },
  ], [{ id: "old", objective: "unrelated" }, { id: "t1", objective: "go" }, { id: "t2", objective: "go" }], "");
  expect(restored.filter((m) => m.role === "assistant").map((m) => m.turnId)).toEqual(["t1", "t2"]);
});

it("cancels obsolete replay requests and leaves connection capacity for live work", async () => {
  const controller = new AbortController();
  let replaySignal: AbortSignal | null | undefined;
  const fetcher = vi.spyOn(globalThis, "fetch").mockImplementation((_url, options) => {
    replaySignal = options?.signal;
    return new Promise((_resolve, reject) => options?.signal?.addEventListener("abort", () => reject(new DOMException("Cancelled replay", "AbortError"))));
  });
  try {
    const pending = hydrateMessageTranscripts("s", "thread", [
      assistant("a", "first", "t1"), assistant("b", "second", "t2"), assistant("c", "third", "t3"),
    ], [{ id: "t1" }, { id: "t2" }, { id: "t3" }], "", controller.signal);
    expect(fetcher).toHaveBeenCalledTimes(1);
    controller.abort();
    await pending;
    expect(replaySignal?.aborted).toBe(true);
    expect(fetcher).toHaveBeenCalledTimes(1);
  } finally { fetcher.mockRestore(); }
});
