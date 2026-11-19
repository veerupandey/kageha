import { describe, expect, it } from "vitest";
import type { ToolCard } from "../api/types";
import {
  applyStreamFrameToMessage,
  applyTranscriptEvent,
  applyTranscriptToMessage,
  emptyTranscript,
  formatDuration,
  isComputerObserveTool,
  isShellTool,
  messageNarrativeText,
  replayStreamFrames,
  shellCommand,
  shellSummary,
  toolChipKind,
  toolChipSummary,
  toolPillLabel,
  unwrapShellPayload,
  parseSubagentsFromCard,
} from "./transcript";

function card(partial: Partial<ToolCard> & { name: string }): ToolCard {
  return {
    id: partial.id || partial.name,
    name: partial.name,
    argsPreview: partial.argsPreview,
    status: partial.status || "running",
    durationMs: partial.durationMs ?? null,
    resultPreview: partial.resultPreview,
    outputPreview: partial.outputPreview,
    errorPreview: partial.errorPreview,
    subagentGraph: partial.subagentGraph,
  };
}

it("preserves narration and tool evidence when a terminal error arrives", () => {
  const message = replayStreamFrames({ id: "a", role: "assistant", text: "" }, [
    { kind: "progress", payload: { source_kind: "assistant_narrative", text: "Checking the page.", offset: 0 } },
    { kind: "tool_completed", card: card({ name: "browser_click", status: "ok" }) },
    { kind: "failed", payload: { error: "Controller exception", status: "error" } },
  ]);
  expect(message.failureCause).toBe("Controller exception");
  expect(message.statusLabel).toBe("Error");
  expect(message.text).toBe("Checking the page.");
  expect(message.blocks?.map(b => b.type)).toEqual(["narrative", "tool"]);
});

it("keeps the real cause separate from the saved answer on replay", () => {
  const message = replayStreamFrames({ id: "a", role: "assistant", text: "" }, [
    { kind: "failed", payload: {
      status: "error", message: "Research is saved. The task remains incomplete.",
      failure_cause: "Provider timeout after fallback attempts.",
    } },
  ]);
  expect(message.text).toContain("Research is saved");
  expect(message.failureCause).toBe("Provider timeout after fallback attempts.");
});

it.each(["invalid_output", "unverified"])("replays the saved verification reason (%s)", (status) => {
  const cause = "The Altus submission confirmation is missing.";
  const message = replayStreamFrames({ id: "a", role: "assistant", text: "" }, [
    { kind: "failed", payload: { status, message: "One submitted; four blocked.", failure_cause: cause } },
  ]);
  expect(message.text).toBe("One submitted; four blocked.");
  expect(message.failureCause).toBe(cause);
  expect(message.statusLabel).toBe("Verification unresolved");
});

it("does not invent reasoning from telemetry across a long run", () => {
  const frames = Array.from({ length: 130 }, (_, step) => [
    { kind: "thinking_started" },
    { kind: "heartbeat", label: "Still working…", detail: ["Attempt 1/3"] },
    { kind: "progress", payload: { source_kind: "reasoning", duration_ms: 22000, step }, detail: ["Duration: 22000 ms"] },
  ]).flat();
  const message = replayStreamFrames({ id: "a", role: "assistant", text: "" }, frames);
  expect(message.blocks).toEqual([]);
});

it("ignores budget telemetry during replay while advancing the cursor", () => {
  const initial = { id: "a", role: "assistant" as const, text: "", turnId: "turn" };
  const budgetFrame = {
    sequence: 2, turn_id: "turn", kind: "request_budget", label: "Working…",
    payload: { source_kind: "heartbeat", text: "Internal budget" },
    card: card({ name: "request_budget" }), interesting: true,
  };
  const budget = applyStreamFrameToMessage(initial, budgetFrame);
  expect(budget.blocks).toBeUndefined();
  expect(budget.statusLabel).toBeUndefined();
  expect(budget.streamCursor).toEqual({ turnId: "turn", sequence: 2 });

  const replayed = replayStreamFrames(initial, [
    budgetFrame,
    { sequence: 3, turn_id: "turn", kind: "tool_completed", card: card({ name: "search", status: "ok" }) },
  ]);
  expect(replayed.blocks?.map(block => block.type)).toEqual(["tool"]);
  expect(replayed.blocks?.[0]).toMatchObject({ card: { name: "search" } });
  expect(replayed.streamCursor).toEqual({ turnId: "turn", sequence: 3 });
});

it("preserves outcome checks through terminal replay without inventing malformed checks", () => {
  const outcomes = [{ id: "altus", label: "Altus application", status: "pass" },
    { id: "workday", label: "Workday application", status: "unresolved" }];
  const message = replayStreamFrames({ id: "a", role: "assistant", text: "" }, [
    { kind: "verification", payload: { outcomes: [...outcomes, null, { status: "pass" }] } },
    { kind: "failed", payload: { status: "unverified", message: "One submitted; one unknown.", failure_cause: "Missing confirmation" } },
  ]);
  expect(message.verificationOutcomes).toEqual(outcomes);
  expect(message.failureCause).toBe("Missing confirmation");
});

it("preserves the earlier outcome snapshot when a recovery fails before verification", () => {
  const outcomes = [{ id: 'done', label: 'Submitted', status: 'pass', historical: true },
    { id: 'pending', label: 'Unknown submission', status: 'unresolved', historical: true }];
  const message = replayStreamFrames({ id: 'a', role: 'assistant', text: '' }, [
    { kind: 'progress', payload: { source_kind: 'outcome_snapshot', outcomes } },
    { kind: 'failed', payload: { status: 'error', error: 'Provider unavailable' } },
  ]);
  expect(message.verificationOutcomes).toEqual(outcomes);
  expect(message.failureCause).toBe('Provider unavailable');
});

describe("transcript reducer", () => {
  it("opens reasoning on thinking, then drops empty duration chips when a tool starts", () => {
    let state = emptyTranscript();
    state = applyTranscriptEvent(state, { type: "thinking_started" }, 1_000);
    expect(state.blocks).toHaveLength(1);
    expect(state.blocks[0]).toMatchObject({ type: "reasoning", streaming: true });

    state = applyTranscriptEvent(
      state,
      { type: "tool", card: card({ name: "write_file", id: "w1", argsPreview: '{"path":"citations.py"}' }) },
      7_000,
    );
    expect(state.blocks.map((b) => b.type)).toEqual(["tool"]);
    expect(toolPillLabel((state.blocks[0] as { card: ToolCard }).card)).toBe(
      "Writing file citations.py",
    );
  });

  it("keeps streaming thought text alongside tool pills", () => {
    let state = emptyTranscript();
    state = applyTranscriptEvent(state, { type: "thinking_started" }, 1_000);
    state = applyTranscriptEvent(
      state,
      { type: "reasoning", text: "Use capture_redirect_evidence on the machine.", partial: true },
      2_000,
    );
    state = applyTranscriptEvent(
      state,
      { type: "tool", card: card({ name: "tool_search", id: "s1", argsPreview: '{"query":"capture"}' }) },
      4_000,
    );
    expect(state.blocks.map((b) => b.type)).toEqual(["reasoning", "tool"]);
    expect(state.blocks[0]).toMatchObject({
      type: "reasoning",
      streaming: true,
      text: "Use capture_redirect_evidence on the machine.",
    });
  });

  it("does not surface run telemetry as chat status blocks", () => {
    const promptCache = applyStreamFrameToMessage(
      { id: "a", role: "assistant", text: "", blocks: [] },
      {
        kind: "model",
        label: "Prompt cache 85%",
        detail: ["cache_read=1020/1200 (85%)"],
        interesting: true,
      },
    );
    expect(promptCache.blocks || []).toEqual([]);

    const todos = applyStreamFrameToMessage(promptCache, {
      kind: "todo_board",
      label: "Todos 2/5",
      detail: ["[ ] inspect page"],
      interesting: true,
    });
    expect(todos.blocks || []).toEqual([]);
  });

  it("closes prior thought when a new loop step starts thinking", () => {
    let state = emptyTranscript();
    state = applyTranscriptEvent(state, { type: "thinking_started" }, 1_000);
    state = applyTranscriptEvent(
      state,
      { type: "reasoning", text: "First step reasoning.", partial: true },
      2_000,
    );
    state = applyTranscriptEvent(
      state,
      { type: "tool", card: card({ name: "web_fetch", id: "f1" }) },
      5_000,
    );
    state = applyTranscriptEvent(state, { type: "thinking_started" }, 10_000);
    const reasoning = state.blocks.filter((b) => b.type === "reasoning");
    expect(reasoning).toHaveLength(2);
    expect(reasoning[0]).toMatchObject({ streaming: false, text: "First step reasoning." });
    expect(reasoning[1]).toMatchObject({ streaming: true });
  });

  it("closes superseded thinking when steering interrupts generation", () => {
    let message = applyStreamFrameToMessage(
      { id: "a", role: "assistant", text: "", blocks: [] },
      { kind: "thinking_started" },
      1_000,
    );
    message = applyStreamFrameToMessage(message, {
      kind: "progress",
      payload: { source_kind: "reasoning", text: "Reviewing data engineer roles.", partial: true },
    }, 2_000);
    const interrupted = applyStreamFrameToMessage(message, {
      kind: "progress", payload: { source_kind: "model_interrupted" },
    }, 3_000);
    expect(interrupted.reasoningStartedAt).toBeUndefined();
    expect(interrupted.blocks).toMatchObject([
      { type: "reasoning", text: "Reviewing data engineer roles.", streaming: false },
    ]);
    const restarted = applyStreamFrameToMessage(interrupted, { kind: "thinking_started" }, 4_000);
    expect(restarted.reasoningStartedAt).toBe(4_000);
    expect(restarted.blocks?.at(-1)).toMatchObject({ type: "reasoning", streaming: true });
  });

  it("turns Working heartbeats into a live thinking chip before tools start", () => {
    const opened = applyStreamFrameToMessage(
      { id: "a", role: "assistant", text: "", blocks: [] },
      { kind: "running", label: "Working…" },
    );
    expect(opened.blocks).toMatchObject([{ type: "reasoning", streaming: true }]);

    const afterTool = applyStreamFrameToMessage(
      {
        id: "a",
        role: "assistant",
        text: "",
        blocks: [{ type: "tool", id: "t1", card: card({ name: "web_fetch", id: "f1" }) }],
      },
      { kind: "heartbeat", label: "Still working…" },
    );
    expect(afterTool.blocks?.map((block) => block.type)).toEqual(["tool"]);
  });

  it("keeps streamed thinking text even when the live label is Thinking…", () => {
    const opened = applyStreamFrameToMessage(
      { id: "a", role: "assistant", text: "", blocks: [] },
      { kind: "running", label: "Thinking…" },
    );
    expect(opened.blocks?.[0]).toMatchObject({ type: "reasoning", streaming: true });
    expect(opened.blocks?.[0]).not.toHaveProperty("text");

    const next = applyStreamFrameToMessage(opened, {
      kind: "reasoning",
      label: "Thinking…",
      payload: {
        source_kind: "reasoning",
        text: "I will open the Canada Post rate page first.",
        partial: true,
      },
    });
    expect(next.blocks?.[0]).toMatchObject({
      type: "reasoning",
      streaming: true,
      text: "I will open the Canada Post rate page first.",
    });
  });

    it("prefers wall-clock duration when the server reports a tiny reasoning ms value", () => {
    let state = emptyTranscript();
    state = applyTranscriptEvent(state, { type: "thinking_started" }, 0);
    state = applyTranscriptEvent(
      state,
      { type: "reasoning", text: "Need to inspect the redirect chain first.", partial: true },
      500,
    );
    state = applyTranscriptEvent(
      state,
      { type: "reasoning", text: "Need to inspect the redirect chain first.", durationMs: 1 },
      3200,
    );
    expect(state.blocks[0]).toMatchObject({
      type: "reasoning",
      streaming: false,
      durationMs: 3200,
      text: "Need to inspect the redirect chain first.",
    });
  });

  it("fills reasoning text and duration from the reasoning event", () => {
    let state = emptyTranscript();
    state = applyTranscriptEvent(state, { type: "thinking_started" }, 0);
    state = applyTranscriptEvent(
      state,
      { type: "reasoning", text: "I found a bug in citation_domain.", durationMs: 6200 },
      10_000,
    );
    expect(state.blocks[0]).toMatchObject({
      type: "reasoning",
      streaming: false,
      durationMs: 6200,
      text: "I found a bug in citation_domain.",
    });
  });

  it("replaces streamed thinking with the authoritative completed text", () => {
    let state = emptyTranscript();
    state = applyTranscriptEvent(state, { type: "thinking_started" }, 0);
    state = applyTranscriptEvent(
      state,
      { type: "reasoning", text: "First I will open the rate page", partial: true },
      1_000,
    );
    expect(state.blocks[0]).toMatchObject({
      type: "reasoning",
      streaming: true,
      text: "First I will open the rate page",
    });
    state = applyTranscriptEvent(
      state,
      { type: "reasoning", text: "First I will open the rate page and extract postage.", partial: true },
      2_000,
    );
    expect(state.blocks[0]).toMatchObject({
      type: "reasoning",
      streaming: true,
      text: "First I will open the rate page and extract postage.",
    });
    state = applyTranscriptEvent(
      state,
      { type: "reasoning", text: "First I will open", durationMs: 4100 },
      4_100,
    );
    expect(state.blocks[0]).toMatchObject({
      type: "reasoning",
      streaming: false,
      durationMs: 4100,
      text: "First I will open",
    });
  });

  it("renders shell tools as terminal blocks, not pills", () => {
    let state = emptyTranscript();
    state = applyTranscriptEvent(state, {
      type: "tool",
      card: card({
        name: "bash",
        id: "s1",
        argsPreview: '{"command":"pytest tests/test_citation_provenance.py"}',
        resultPreview: "1 passed",
      }),
    });
    expect(state.blocks[0]?.type).toBe("shell");
    expect(isShellTool("bash")).toBe(true);
  });

  it("unwraps bash JSON envelopes into stdout", () => {
    const payload = JSON.stringify({
      exit_code: 0,
      stdout: "=== chat.jsonl ===\n{\"ts\":1}\n",
      stderr: "",
    });
    expect(unwrapShellPayload(payload)).toBe('=== chat.jsonl ===\n{"ts":1}');
    expect(unwrapShellPayload(payload)).not.toContain("exit_code");
  });

  it("omits computer observe tools", () => {
    expect(isComputerObserveTool("computer_get_state")).toBe(true);
    let state = emptyTranscript();
    state = applyTranscriptEvent(state, {
      type: "tool",
      card: card({ name: "computer_get_state", id: "c1" }),
      interesting: false,
    });
    expect(state.blocks).toHaveLength(0);
  });

  it("splits narrative across tools", () => {
    let state = emptyTranscript();
    state = applyTranscriptEvent(state, { type: "narrative", assembled: "Looking at the bug." });
    state = applyTranscriptEvent(state, {
      type: "tool",
      card: card({ name: "edit_file", id: "e1", argsPreview: '{"path":"citations.py"}' }),
    });
    state = applyTranscriptEvent(
      state,
      { type: "narrative", assembled: "Looking at the bug.Now tests, pinning the real case:" },
    );
    expect(state.blocks.map((b) => b.type)).toEqual(["narrative", "tool", "narrative"]);
    expect(state.blocks[2]).toMatchObject({
      type: "narrative",
      text: "Now tests, pinning the real case:",
    });
  });

  it("appends to the last narrative when no tool landed in between", () => {
    let state = emptyTranscript();
    state = applyTranscriptEvent(state, { type: "narrative", assembled: "Hello" });
    state = applyTranscriptEvent(state, { type: "narrative", assembled: "Hello world" });
    expect(state.blocks).toHaveLength(1);
    expect(state.blocks[0]).toMatchObject({ type: "narrative", text: "Hello world" });
  });

  it("upserts a running tool card when it completes", () => {
    let state = emptyTranscript();
    state = applyTranscriptEvent(state, {
      type: "tool",
      card: card({ name: "read_file", id: "r1", status: "running", argsPreview: '{"path":"a.py"}' }),
    });
    state = applyTranscriptEvent(state, {
      type: "tool",
      card: card({
        name: "read_file",
        id: "r1",
        status: "ok",
        durationMs: 12,
        argsPreview: '{"path":"a.py"}',
        resultPreview: "ok",
      }),
    });
    expect(state.blocks).toHaveLength(1);
    expect((state.blocks[0] as { card: ToolCard }).card.status).toBe("ok");
    expect((state.blocks[0] as { card: ToolCard }).card.durationMs).toBe(12);
  });

  it("does not dump verifier repair loops as status chips", () => {
    let state = emptyTranscript();
    state = applyTranscriptEvent(state, {
      type: "status",
      label: "Checked · repair",
      kind: "verification",
    });
    expect(state.blocks).toHaveLength(0);
  });

  it("drops empty reasoning when replay has no duration", () => {
    let state = emptyTranscript();
    state = applyTranscriptEvent(state, { type: "thinking_started" }, 5_000);
    state = applyTranscriptEvent(
      state,
      { type: "tool", card: card({ name: "read_file", id: "r1", argsPreview: '{"path":"a.py"}' }) },
      5_000,
    );
    expect(state.blocks.map((b) => b.type)).toEqual(["tool"]);
  });

  it("does not keep planning telemetry in the chat transcript", () => {
    let state = emptyTranscript();
    state = applyTranscriptEvent(state, {
      type: "status",
      label: "Planning…",
      kind: "planning_started",
    });
    expect(state.blocks).toHaveLength(0);
    state = applyTranscriptEvent(state, { type: "status", label: "Thinking…", kind: "thinking_started" });
    expect(state.blocks).toHaveLength(0);
  });

  it("finish and cancel drop transient planning chips", () => {
    let state = emptyTranscript();
    state = applyTranscriptEvent(state, {
      type: "status",
      label: "Planning…",
      kind: "planning_started",
    });
    state = applyTranscriptEvent(state, { type: "status", label: "Working…" });
    state = applyTranscriptEvent(state, { type: "status", label: "Reconnecting…" });
    state = applyTranscriptEvent(state, { type: "finish", assembled: "Stopped." });
    expect(state.blocks.map((b) => b.type)).toEqual(["narrative"]);
    expect(state.blocks[0]).toMatchObject({ type: "narrative", text: "Stopped." });
  });

  it("finish drops empty reasoning and flushes remaining narrative", () => {
    let state = emptyTranscript();
    state = applyTranscriptEvent(state, { type: "thinking_started" }, 0);
    state = applyTranscriptEvent(state, { type: "finish", assembled: "Done." }, 1500);
    expect(state.blocks.map((b) => b.type)).toEqual(["narrative"]);
    expect(state.blocks[0]).toMatchObject({ type: "narrative", text: "Done." });
  });

  it("does not keep a duration-only reasoning chip", () => {
    const message = applyStreamFrameToMessage(
      { id: "a", role: "assistant", text: "" },
      { kind: "thinking_started", label: "Thinking…" },
      0,
    );
    const withReason = applyStreamFrameToMessage(
      message,
      {
        kind: "progress",
        label: "Reasoning",
        detail: ["Duration: 5766.6 ms"],
        payload: { source_kind: "reasoning", text: "", duration_ms: 5766.6 },
      },
      10_000,
    );
    expect(withReason.blocks || []).toEqual([]);
  });

  it("drops model-selected status from the transcript", () => {
    const message = applyStreamFrameToMessage(
      { id: "a", role: "assistant", text: "" },
      { kind: "model_selected", label: "Using zai/glm-5.3" },
    );
    expect(message.blocks || []).toEqual([]);
  });

  it("maps progress source_kind=reasoning onto a reasoning block", () => {
    const message = applyStreamFrameToMessage(
      { id: "a", role: "assistant", text: "" },
      { kind: "thinking_started", label: "Thinking…", payload: { step: 1 } },
      0,
    );
    const withReason = applyStreamFrameToMessage(
      message,
      {
        kind: "progress",
        label: "Reasoning",
        detail: ["I found a bug in citation_domain."],
        payload: {
          source_kind: "reasoning",
          text: "I found a bug in citation_domain.",
          duration_ms: 6200,
        },
      },
      10_000,
    );
    expect(withReason.blocks?.[0]).toMatchObject({
      type: "reasoning",
      text: "I found a bug in citation_domain.",
      durationMs: 6200,
      streaming: false,
    });
  });

  it("captures live SiliconFlow reasoning_content into a streaming thought chip", () => {
    const live = applyStreamFrameToMessage(
      { id: "a", role: "assistant", text: "" },
      {
        kind: "reasoning",
        label: "Thinking…",
        payload: {
          source_kind: "reasoning",
          text: "We need answer 17*19. Compute 17*19 = 323.",
          partial: true,
        },
      },
    );
    expect(live.blocks?.[0]).toMatchObject({
      type: "reasoning",
      streaming: true,
      text: "We need answer 17*19. Compute 17*19 = 323.",
    });
  });

  it("replays journal frames then flushes the final answer as narrative", () => {
    const replayed = replayStreamFrames(
      { id: "a", role: "assistant", text: "Fixed the scheme check." },
      [
        { kind: "thinking_started", label: "Thinking…" },
        {
          kind: "progress",
          payload: { source_kind: "reasoning", text: "Need to validate the URL scheme.", duration_ms: 4000 },
        },
        {
          kind: "tool_started",
          card: card({ name: "edit_file", id: "e1", argsPreview: '{"path":"citations.py"}' }),
        },
      ],
      "Fixed the scheme check.",
    );
    expect(replayed.blocks?.map((b) => b.type)).toEqual(["reasoning", "tool", "narrative"]);
    expect(replayed.blocks?.[2]).toMatchObject({
      type: "narrative",
      text: "Fixed the scheme check.",
    });
  });

  it("restores saved progress between tools and keeps the final answer once", () => {
    const first = "I'm opening the form.";
    const answer = "Application submitted.";
    const frames = [
      { kind: "progress", payload: { source_kind: "assistant_narrative", text: first, offset: 0 } },
      { kind: "tool_completed", card: card({ name: "browser_open", id: "b1" }) },
      { kind: "progress", payload: { source_kind: "assistant_narrative", text: answer, offset: first.length } },
    ];
    const restored = replayStreamFrames({ id: "a", role: "assistant", text: answer }, frames, answer);
    expect(restored.blocks?.map((b) => b.type)).toEqual(["narrative", "tool", "narrative"]);
    expect(restored.blocks?.[0]).toMatchObject({ text: first });
    expect(restored.blocks?.[2]).toMatchObject({ text: answer });
    expect(restored.text).toBe(answer);
  });

  it("does not duplicate durable progress already received as live deltas", () => {
    const message = applyTranscriptToMessage({ id: "a", role: "assistant", text: "" }, {
      type: "narrative", assembled: "Opening the form.",
    });
    const frame = { kind: "progress", payload: {
      source_kind: "assistant_narrative", text: "Opening the form.", offset: 0,
    } };
    expect(applyStreamFrameToMessage(message, frame)).toBe(message);
  });

  it("restores an active step and completes a partially streamed narrative", () => {
    const partial = applyTranscriptToMessage({ id: "a", role: "assistant", text: "" }, {
      type: "narrative", assembled: "Opening",
    });
    const restored = applyStreamFrameToMessage(partial, { kind: "progress", payload: {
      source_kind: "assistant_narrative", text: "Opening the form.", offset: 0,
    } });
    expect(restored.blocks?.[0]).toMatchObject({ text: "Opening the form." });
  });

  it("appends a canonical answer when a cancelled turn has only saved progress", () => {
    const restored = replayStreamFrames({ id: "a", role: "assistant", text: "Stopped." }, [
      { kind: "progress", payload: { source_kind: "assistant_narrative", text: "Opening.", offset: 0 } },
      { kind: "tool_started", card: card({ name: "browser_open", id: "b1" }) },
    ], "Stopped.");
    expect(restored.blocks?.[0]).toMatchObject({ text: "Opening." });
    expect(restored.blocks?.at(-1)).toMatchObject({ text: "Stopped." });
  });
});

describe("transcript labels", () => {
  it("formats durations like Hyperagent", () => {
    expect(formatDuration(6_000)).toBe("6s");
    expect(formatDuration(6200)).toBe("6.2s");
    expect(formatDuration(400)).toBe("400ms");
  });

  it("labels file tools from args preview", () => {
    expect(
      toolPillLabel(card({ name: "edit_file", argsPreview: '{"path":"src/kageha/citations.py"}' })),
    ).toBe("Editing file citations.py");
  });

  it("labels parallel web search from the first query, not the JSON blob", () => {
    expect(
      toolPillLabel(
        card({
          name: "parallel_web_search",
          argsPreview: 'queries_json=["matcha tea market size 2024 growth forecast consumer"]',
        }),
      ),
    ).toBe("Searching the web matcha tea market size 2024 growth foreca…");
  });

  it("still labels a truncated queries_json blob without a closing quote", () => {
    expect(
      toolPillLabel(
        card({
          name: "parallel_web_search",
          argsPreview: 'queries_json=["matcha tea market size 2024 growth forecast',
        }),
      ),
    ).toBe("Searching the web matcha tea market size 2024 growth foreca…");
  });

  it("hides [REDACTED] path previews on listing pills", () => {
    expect(
      toolPillLabel(card({ name: "list_dir", argsPreview: "path=[REDACTED]" })),
    ).toBe("Listing");
    expect(
      toolPillLabel(card({ name: "list_dir", argsPreview: "[REDACTED]" })),
    ).toBe("Listing");
  });

  it("keeps the full shell command, including spaces", () => {
    expect(
      shellCommand(card({ name: "bash", argsPreview: "command=echo kageha-transcript-ok" })),
    ).toBe("echo kageha-transcript-ok");
    expect(
      shellCommand(
        card({ name: "bash", argsPreview: '{"command":"pytest tests/test_citation_provenance.py"}' }),
      ),
    ).toBe("pytest tests/test_citation_provenance.py");
  });

  it("prefers a 5-10 word description for the Running command subtitle", () => {
    expect(
      shellSummary(
        card({
          name: "bash",
          argsPreview:
            'command=pytest tests/test_foo.py, description=Fix both lint errors and verify',
        }),
      ),
    ).toBe("Fix both lint errors and verify");
    expect(
      shellSummary(
        card({
          name: "bash",
          argsPreview: '{"command":"ruff check .","description":"Run pyright and build as CI does"}',
        }),
      ),
    ).toBe("Run pyright and build as CI does");
    expect(
      shellSummary(card({ name: "bash", argsPreview: "command=echo kageha-transcript-ok" })),
    ).toBe("echo kageha-transcript-ok");
  });

  it("labels browser tools as Using browser chips with a host summary", () => {
    expect(toolChipKind("browser_navigate")).toBe("Using browser");
    expect(
      toolChipSummary(
        card({
          name: "browser_navigate",
          argsPreview:
            'url="https://www.canadapost-postescanada.ca/cpc/en/tools/find-a-rate.page"',
        }),
      ),
    ).toBe("Opening page canadapost-postescanada.ca");
    expect(
      toolChipSummary(
        card({
          name: "browser_open",
          argsPreview:
            "url: https://www.canadapost-postescanada.ca/cpc/en/tools/find-a-rate.page",
        }),
      ),
    ).toBe("Opening page canadapost-postescanada.ca");
  });

  it("labels Codex-native image and MCP tools with their UI role", () => {
    expect(toolChipKind("image_generate")).toBe("Creating image");
    expect(toolPillLabel(card({ name: "image_generate", argsPreview: "a blue circle" }))).toBe(
      "Generating image a blue circle",
    );
    expect(toolChipKind("mcp.github.search_issues")).toBe("Running integration");
  });
});

describe("peer result transcript", () => {
  it("shows imported peer preview as its own block and keeps it after finish", () => {
    const message = applyStreamFrameToMessage(
      { id: "a", role: "assistant", text: "" },
      {
        kind: "progress",
        label: "Remote artifact received",
        payload: {
          source_kind: "peer_artifact",
          path: "artifacts/agent-handoff.txt",
          preview: "artifacts/agent-handoff.txt:\nchat handoff ok",
        },
        card: card({
          name: "peer_artifact",
          status: "completed",
          resultPreview: "artifacts/agent-handoff.txt:\nchat handoff ok",
        }),
      },
    );
    expect(message.blocks?.some((block) => block.type === "peer_result")).toBe(true);
    const result = message.blocks?.find((block) => block.type === "peer_result");
    expect(result).toMatchObject({
      type: "peer_result",
      path: "artifacts/agent-handoff.txt",
      text: "artifacts/agent-handoff.txt:\nchat handoff ok",
    });
    const finished = applyTranscriptEvent(
      {
        blocks: message.blocks || [],
        narrativeCursor: 0,
        seq: message.transcriptSeq || 0,
      },
      { type: "finish", assembled: "Task completed." },
    );
    expect(finished.blocks.some((block) => block.type === "peer_result")).toBe(true);
    expect(finished.blocks.some((block) => block.type === "narrative")).toBe(true);
  });

  it("shows a peer knowledge answer as its own block labeled with the query", () => {
    const message = applyStreamFrameToMessage(
      { id: "b", role: "assistant", text: "" },
      {
        kind: "progress",
        label: "Peer knowledge answer received",
        payload: {
          source_kind: "peer_knowledge_answer",
          query: "how did you fix the flaky retry test",
          lessons: [
            { task: "fix flaky retry test", summary: "Added jitter to the retry backoff." },
          ],
          preview:
            "Lessons from peer-1 on 'how did you fix the flaky retry test':\n1. fix flaky retry test\n   Added jitter to the retry backoff.",
        },
      },
    );
    const result = message.blocks?.find((block) => block.type === "peer_result");
    expect(result).toMatchObject({
      type: "peer_result",
      path: "knowledge: how did you fix the flaky retry test",
      text: "Lessons from peer-1 on 'how did you fix the flaky retry test':\n1. fix flaky retry test\n   Added jitter to the retry backoff.",
    });
  });

  it("adds and resolves ask_human transcript blocks", () => {
    const pending = {
      approval_id: "ap_1",
      action: "ask_human",
      question: "Which color?",
      options: ["Blue", "Green"],
      risk_class: "hitl",
    };
    let message = applyTranscriptToMessage(
      { id: "a", role: "assistant", text: "", blocks: [] },
      { type: "ask_human_pending", pending },
    );
    expect(message.blocks).toMatchObject([
      {
        type: "ask_human",
        approvalId: "ap_1",
        question: "Which color?",
        options: ["Blue", "Green"],
        status: "pending",
      },
    ]);

    message = applyTranscriptToMessage(message, {
      type: "ask_human_resolved",
      approvalId: "ap_1",
      answer: "Blue",
    });
    expect(message.blocks?.[0]).toMatchObject({
      type: "ask_human",
      status: "answered",
      answer: "Blue",
    });

    message = applyTranscriptToMessage(
      { id: "b", role: "assistant", text: "", blocks: message.blocks || [] },
      {
        type: "ask_human_resolved",
        approvalId: "ap_1",
        deferred: true,
      },
    );
    expect(message.blocks?.[0]).toMatchObject({
      type: "ask_human",
      status: "deferred",
      answer: "Let the agent decide",
    });
  });

  it("parses and formats subagents launched via spawn_subagents", () => {
    const rawArgs =
      'tasks_json=[{"id": "structure", "task": "Research account structure"}, {"id": "retargeting", "task": "Research retargeting"}]';
    const subCard = card({
      name: "spawn_subagents",
      argsPreview: rawArgs,
      status: "running",
    });

    expect(toolChipKind("spawn_subagents")).toBe("Running subagents");
    expect(toolPillLabel(subCard)).toBe("Subagents 2 tasks (structure, retargeting)");
    expect(toolChipSummary(subCard)).toBe("Subagents 2 tasks (structure, retargeting)");

    const items = parseSubagentsFromCard(subCard);
    expect(items).toHaveLength(2);
    expect(items[0]).toMatchObject({
      id: "structure",
      task: "Research account structure",
      status: "running",
    });
    expect(items[1]).toMatchObject({
      id: "retargeting",
      task: "Research retargeting",
      status: "running",
    });

    const completedCard = card({
      name: "spawn_subagents",
      argsPreview: rawArgs,
      status: "success",
      resultPreview: JSON.stringify({
        ok: true,
        completed: 2,
        total: 2,
        results: [
          { label: "structure", ok: true, result: "Summary of structure" },
          { label: "retargeting", ok: false, error: "Retargeting failed" },
        ],
      }),
    });

    const completedItems = parseSubagentsFromCard(completedCard);
    expect(completedItems[0]).toMatchObject({
      id: "structure",
      status: "completed",
      result: "Summary of structure",
    });
    expect(completedItems[1]).toMatchObject({
      id: "retargeting",
      status: "failed",
      error: "Retargeting failed",
    });
  });
  it("builds subagents from the structured graph when the args preview is clipped", () => {
    const items = parseSubagentsFromCard(card({
      name: "spawn_task_graph",
      argsPreview: 'nodes_json=[{"id": "research", "task": "Find primary sour…',
      status: "ok",
      subagentGraph: [
        { id: "research", task: "Find sources", status: "done", result: "3 sources" },
        { id: "draft", task: "Write", depends_on: ["research"], status: "blocked", error: "upstream" },
      ],
    }));
    expect(items).toHaveLength(2);
    expect(items[0]).toMatchObject({ status: "completed", result: "3 sources" });
    expect(items[1]).toMatchObject({ status: "failed", error: "upstream" });
  });
});

it("preserves specialist names and durable identities independently from dependency IDs", async () => {
  const { parseSubagentsFromCard } = await import("./transcript");
  const items = parseSubagentsFromCard({ id: "spawn", name: "spawn_subagents", status: "success", subagentGraph: [{ id: "a", name: "Core architect", job_id: "worker-123", task: "Review architecture", status: "queued" }] });
  expect(items[0]).toMatchObject({ id: "a", name: "Core architect", job_id: "worker-123", status: "pending" });
});

it("preserves a single specialist name from complete legacy arguments", () => {
  const items = parseSubagentsFromCard({ id: "single", name: "spawn_subagent", status: "running",
    argsPreview: JSON.stringify({ name: "Reviewer", task: "Check sources" }) });
  expect(items[0]).toMatchObject({ name: "Reviewer", task: "Check sources" });
});

it("tracks an accepted agent_spawn by its job ID without declaring it complete", () => {
  const items = parseSubagentsFromCard({ id: "spawn", name: "agent_spawn", status: "done",
    subagentGraph: [{ id: "job-123", job_id: "job-123", name: "reviewer", task: "Review the code", status: "queued" }] });
  expect(items).toEqual([{ id: "job-123", job_id: "job-123", name: "reviewer", task: "Review the code", status: "pending" }]);
  expect(toolChipKind("agent_spawn")).toBe("Running subagents");
  expect(toolChipKind("agent_wait_any")).toBe("Waiting for subagents");
  expect(toolChipKind("agent_wait_all")).toBe("Waiting for subagents");
});

it("does not mistake a successful spawn call for a finished worker when metadata is unavailable", () => {
  const argsPreview = JSON.stringify({ agent_id: "reviewer", objective: "Review the code" });
  expect(parseSubagentsFromCard({ id: "spawn", name: "agent_spawn", status: "done", argsPreview })[0])
    .toMatchObject({ name: "reviewer", task: "Review the code", status: "pending" });
  expect(parseSubagentsFromCard({ id: "spawn", name: "agent_spawn", status: "failed", argsPreview,
    subagentGraph: [{ id: "spawn", task: "Review the code", status: "running" }] })[0].status).toBe("failed");
  expect(parseSubagentsFromCard({ id: "spawn", name: "agent_spawn", status: "done", argsPreview,
    subagentGraph: [{ id: "spawn", task: "Review the code", status: "running" }] })[0].status).toBe("pending");
});

describe("executor stream items", () => {
  const start = () => ({ id: "live", role: "assistant" as const, text: "", streaming: true });
  const frame = (text: string, offset = 0, state = "delta", itemId = "attempt-1", generation = 0) => ({
    kind: "stream_item", payload: { item_id: itemId, channel: "text", state, text, offset, base_offset: 0, generation },
  });
  it("renders partial output and deduplicates replay offsets including UTF-16", () => {
    let message = applyStreamFrameToMessage(start(), frame("Opening 📄"));
    message = applyStreamFrameToMessage(message, frame("Opening 📄"));
    message = applyStreamFrameToMessage(message, frame(" now", "Opening 📄".length));
    expect(message.text).toBe("Opening 📄 now");
    message = applyStreamFrameToMessage(message, frame("Opening 📄 now", 0, "completed"));
    message = applyStreamFrameToMessage(message, { kind: "progress", payload: { source_kind: "assistant_narrative", text: "Opening 📄 now", offset: 0 } });
    expect(message.text).toBe("Opening 📄 now");
    expect(message.blocks?.filter(b => b.type === "narrative")).toHaveLength(1);
  });
  it("discards a failed attempt before rendering fallback output", () => {
    let message = applyStreamFrameToMessage(start(), frame("Wrong partial"));
    message = applyStreamFrameToMessage(message, frame("", 0, "discarded"));
    message = applyStreamFrameToMessage(message, frame("Correct", 0, "delta", "attempt-2", 1));
    message = applyStreamFrameToMessage(message, frame(" late", 13, "delta", "attempt-1", 0));
    expect(message.text).toBe("Correct");
  });
  it("streams thought separately and replaces the activity placeholder", () => {
    let message = applyStreamFrameToMessage(start(), { kind: "thinking_started" });
    message = applyStreamFrameToMessage(message, { kind: "stream_item", payload: { ...frame("Inspecting").payload, channel: "reasoning" } });
    expect(message.text).toBe("");
    expect(message.blocks).toMatchObject([{ type: "reasoning", text: "Inspecting", streaming: true }]);
  });
});

it("recovery supersedes unfinished stream items while preserving committed narrative", () => {
  let message = applyStreamFrameToMessage({ id: "recover", role: "assistant", text: "" }, {
    kind: "stream_item", payload: { item_id: "abandoned", channel: "text", state: "delta", text: "Partial old attempt", offset: 0, base_offset: 0, generation: 0 },
  });
  message = applyStreamFrameToMessage(message, {
    kind: "stream_item", payload: { item_id: "new-thought", channel: "reasoning", state: "delta", text: "Recovering", offset: 0, base_offset: 0, generation: 1 },
  });
  expect(message.text).toBe("");
  expect(message.narrativeSourceCursor).toBe(0);
  message = applyStreamFrameToMessage(message, {
    kind: "stream_item", payload: { item_id: "recovered", channel: "text", state: "completed", text: "Recovered answer", offset: 0, base_offset: 0, generation: 1 },
  });
  expect(message.text).toBe("Recovered answer");
});

it("does not resurrect a discarded provider attempt when an older frame arrives late", () => {
  const event = (sequence: number, itemId: string, state: string, text: string) => ({
    kind: "stream_item", sequence, turn_id: "turn-a",
    payload: { item_id: itemId, channel: "text", state, text, offset: 0, base_offset: 0, generation: 0 },
  });
  let message = applyStreamFrameToMessage({ id: "a", role: "assistant", turnId: "turn-a", text: "" }, event(10, "old", "delta", "Wrong"));
  message = applyStreamFrameToMessage(message, event(11, "old", "discarded", ""));
  message = applyStreamFrameToMessage(message, event(12, "new", "delta", "Correct"));
  message = applyStreamFrameToMessage(message, event(10, "old", "delta", "Wrong"));
  expect(message.text).toBe("Correct");
});

it("rejects stream items from another turn even at a newer journal sequence", () => {
  const frame = { kind: "stream_item", sequence: 50, turn_id: "other-turn",
    payload: { item_id: "other", channel: "text", state: "delta", text: "Other task", offset: 0, base_offset: 0, generation: 0 } };
  const message = { id: "a", role: "assistant" as const, turnId: "current-turn", text: "Current task" };
  expect(applyStreamFrameToMessage(message, frame)).toEqual(message);
});

it.each([
  { kind: "assistant_narrative", payload: { text: "Wrong task", offset: 0 } },
  { kind: "answer_revision", payload: { original: "Current task", replacement: "Wrong task" } },
  { kind: "completed", payload: { message: "Wrong task" } },
  { kind: "tool_completed", card: { id: "wrong", name: "read_file", status: "ok" } },
])("rejects foreign-turn $kind frames before changing the transcript", (event) => {
  const message = { id: "a", role: "assistant" as const, turnId: "current", text: "Current task" };
  expect(applyStreamFrameToMessage(message, { ...event, turn_id: "other", sequence: 100 })).toEqual(message);
});

it("does not replay a stale revision after a newer completed item", () => {
  let message = applyStreamFrameToMessage({ id: "a", role: "assistant", turnId: "t", text: "" }, {
    turn_id: "t", sequence: 10, kind: "stream_item", payload: {
      item_id: "answer", channel: "text", state: "completed", text: "Current answer", base_offset: 0,
    },
  });
  const previous = message;
  message = applyStreamFrameToMessage(message, { turn_id: "t", sequence: 9, kind: "answer_revision",
    payload: { original: "Current answer", replacement: "Old correction" } });
  expect(message).toEqual(previous);
});

it.each(["New name", "[Posting](https://[REDACTED]/)"])(
  "settles a short explicitly owned revision once: %s", (replacement) => {
    const progress = "Checking the saved posting.";
    let message = applyStreamFrameToMessage({ id: "a", role: "assistant", turnId: "t", text: "" }, {
      turn_id: "t", sequence: 1, kind: "assistant_narrative", payload: { text: progress + "Old name", offset: 0 },
    });
    message = applyStreamFrameToMessage(message, { turn_id: "t", sequence: 2, kind: "answer_revision",
      payload: { original: "Old name", replacement, offset: progress.length } });
    const final = replacement.replace("[REDACTED]", "example.com/jobs/123");
    message = replayStreamFrames(message, [], final);
    expect(messageNarrativeText(message)).toBe(progress + replacement);
    expect(messageNarrativeText(replayStreamFrames(message, [], final))).toBe(progress + replacement);
  },
);

it("recognizes only typed browser refusal envelopes, including bounded previews", async () => {
  const { browserToolBlocked } = await import("./transcript");
  const base = { id: "held", name: "browser_fill", status: "error" };
  expect(browserToolBlocked({...base, resultPreview: '{"status": "error", "error_code": "browser_receipt_pending", "summary": "truncat'})).toBe(true);
  expect(browserToolBlocked({...base, resultPreview: 'Example: {"status":"error","error_code":"browser_receipt_pending"}'})).toBe(false);
  expect(browserToolBlocked({...base, status: "completed", resultPreview: '{"status":"error","error_code":"browser_receipt_pending"}'})).toBe(false);
  expect(browserToolBlocked({...base, name: "bash", resultPreview: '{"status":"error","error_code":"browser_receipt_pending"}'})).toBe(false);
  expect(browserToolBlocked({...base, resultPreview: '{"status":"error","error_code":"execution_failed"}'})).toBe(false);
});

it("retains model usage separately without duplicating replayed or foreign-turn frames", () => {
  const frame = {turn_id: "usage-turn", sequence: 5, kind: "model", label: "model=test tokens=120+30 cache_read=80/120", detail: ["usage"]};
  const initial = {id: "usage", role: "assistant" as const, turnId: "usage-turn", text: ""};
  const message = applyStreamFrameToMessage(initial, frame);
  expect(message.modelUsageSteps).toEqual([{label: frame.label, detail: ["usage"], kind: "model", interesting: false}]);
  expect(message.steps).toBeUndefined();
  expect(applyStreamFrameToMessage(message, frame)).toBe(message);
  expect(applyStreamFrameToMessage(message, {...frame, sequence: 6, turn_id: "another-turn"})).toBe(message);
});

it("replays assistant progress in order without changing the final answer", () => {
  const progress = {
    sequence: 1, turn_id: "turn", kind: "assistant_progress",
    payload: { text: "I’m checking the listing first.", step: 1 },
  };
  const message = replayStreamFrames({ id: "a", role: "assistant", text: "" }, [
    progress,
    progress,
    { sequence: 2, turn_id: "turn", kind: "tool_card", card: card({ name: "browser_open" }) },
  ], "I found one listing.");
  expect(message.blocks?.map(block => block.type)).toEqual(["progress", "tool", "narrative"]);
  expect(messageNarrativeText(message)).toBe("I found one listing.");
});

it("keeps live progress and tool results ordered across replay without adding them to the answer", () => {
  const initial = { id: "a", role: "assistant" as const, text: "", turnId: "owned", streaming: true };
  const progress = { sequence: 1, turn_id: "owned", kind: "assistant_progress", payload: { text: "I’ll check the listing.", step: 1 } };
  const start = { sequence: 2, turn_id: "owned", kind: "tool_card", card: card({ id: "open", name: "browser_open" }) };
  const finish = { sequence: 3, turn_id: "owned", kind: "tool_card", card: card({ id: "open", name: "browser_open", status: "done" }) };
  let live = applyStreamFrameToMessage(initial, progress);
  live = applyStreamFrameToMessage(live, start);
  // Reattachment may replay the last observed frame, or deliver a foreign turn.
  live = applyStreamFrameToMessage(live, start);
  live = applyStreamFrameToMessage(live, { ...finish, turn_id: "other" });
  live = applyStreamFrameToMessage(live, finish);
  const replay = replayStreamFrames(initial, [progress, start, start, finish], "The listing is open.");
  expect(live.blocks?.map(block => block.type)).toEqual(["progress", "tool"]);
  expect(live.blocks?.[1]).toMatchObject({ type: "tool", card: { id: "open", status: "done" } });
  expect(replay.blocks?.map(block => block.type)).toEqual(["progress", "tool", "narrative"]);
  expect(replay.blocks?.slice(0, 2)).toEqual(live.blocks);
  expect(messageNarrativeText(replay)).toBe("The listing is open.");
});

it("restores the selected answer on a native inline question", () => {
  const id = "question-fixture";
  const result = replayStreamFrames({ id: "a", role: "assistant", text: "" }, [
    { sequence: 1, kind: "approval_required", payload: { action: "ask_human", approval_id: id,
      question: "Where should I search?", options: ["Canada", "US"] } },
    { sequence: 2, kind: "approval_resolved", payload: { action: "ask_human", approval_id: id, answer: "Canada" } },
  ], "I’ll search in Canada.");
  expect(result.blocks?.[0]).toMatchObject({ type: "ask_human", status: "answered", answer: "Canada" });
});

it("updates one acknowledgment across interleaved tool events and removes an interrupted sentence", () => {
  const initial = { id: "assistant", role: "assistant" as const, text: "", turnId: "owned" };
  const ack = (sequence: number, text: string) => ({ sequence, turn_id: "owned", kind: "assistant_progress",
    payload: { acknowledgment_id: "owned", text } });
  const tool = { sequence: 2, turn_id: "owned", kind: "tool_card", card: card({ id: "read", name: "read_file", status: "done" }) };
  const frames = [ack(1, "I’ll check"), tool, ack(3, "I’ll check the file.")];
  const live = frames.reduce((message, frame) => applyStreamFrameToMessage(message, frame), initial as Parameters<typeof applyStreamFrameToMessage>[0]);
  expect(live.blocks?.map(b => b.type)).toEqual(["progress", "tool"]);
  expect(live.blocks?.[0]).toMatchObject({ text: "I’ll check the file." });
  expect(applyStreamFrameToMessage(live, ack(4, "")).blocks?.map(b => b.type)).toEqual(["tool"]);
  const replay = replayStreamFrames(initial, frames, "The file says violet.");
  expect(replay.blocks?.slice(0, 2)).toEqual(live.blocks);
  expect(messageNarrativeText(replay)).toBe("The file says violet.");
});


it("preserves independent confirmation evidence and unrelated pending outcomes", () => {
  const evidence = { id: "proof", label: "Independent site confirmation", reference: "receipt:proof", detail: "Application submitted", timestamp: 1795000000 };
  const message = replayStreamFrames({ id: "a", role: "assistant", text: "" }, [
    { kind: "outcome_snapshot", payload: { outcomes: [{ id: "application-original", label: "Submission awaiting verification", status: "unresolved" }, { id: "other", label: "Another application", status: "unresolved" }] } },
    { kind: "outcome_snapshot", payload: { merge: true, outcomes: [{ id: "application-original", label: "Confirmed by site", status: "pass", evidence: [evidence] }] } },
  ]);
  expect(message.verificationOutcomes).toEqual([{ id: "other", label: "Another application", status: "unresolved", evidence: undefined }, { id: "application-original", label: "Confirmed by site", status: "pass", evidence: [evidence] }]);
});
