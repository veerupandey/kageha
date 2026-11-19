import { terminalRunState } from "./runStatus";
import { ladderStepFromEvent } from "./ladder";
import { answerRevisionSuffix, mergeTerminalNarrative, sameNarrativeSnapshot, terminalAnswerOverlap } from "./narrativeText";
import type { ChatMessage, PendingApproval, ToolCard, TranscriptBlock } from "../api/types";

export interface TranscriptState {
  blocks: TranscriptBlock[];
  narrativeCursor: number;
  seq: number;
  reasoningStartedAt?: number;
}

export type TranscriptEvent =
  | { type: "thinking_started" }
  | { type: "model_interrupted" }
  | { type: "reasoning"; text?: string; durationMs?: number; partial?: boolean }
  | { type: "tool"; card: ToolCard; interesting?: boolean }
  | { type: "narrative"; assembled: string }
  | { type: "answer_revision"; original: string; replacement: string; offset?: number }
  | { type: "peer_result"; path?: string; text: string }
  | { type: "status"; label: string; detail?: string[]; kind?: string }
  | { type: "ask_human_pending"; pending: PendingApproval }
  | {
      type: "ask_human_resolved";
      approvalId: string;
      answer?: string;
      deferred?: boolean;
    }
  | { type: "finish"; assembled?: string };

const SHELL_TOOLS = new Set([
  "bash",
  "shell",
  "run_terminal_cmd",
  "run_terminal",
  "run_command",
  "exec",
]);

const COMPUTER_OBSERVE = /^(?:computer_)?(?:get_state|wait|list_apps)$/i;

/** A refused browser input did not execute. Keep actual execution failures distinct. */
export function browserToolBlocked(card: ToolCard): boolean {
  if (!card.name.startsWith("browser_") || !["error", "failed", "denied"].includes(card.status || "")) return false;
  const codes = new Set(["browser_receipt_pending", "browser_action_blocked", "browser_selection_conflict", "browser_control_changed", "browser_access_paused"]);
  return [card.resultPreview, card.errorPreview, card.outputPreview].some(value => {
    if (!value) return false;
    try {
      const result = JSON.parse(value);
      return result?.status === "error" && codes.has(result.error_code);
    } catch {
      // Stored previews may end mid-JSON. Only accept the typed envelope at
      // the beginning, never an error code quoted within ordinary output.
      const prefix = /^\s*\{\s*"status"\s*:\s*"error"\s*,\s*"error_code"\s*:\s*"([a-z_]+)"\s*[,}]/.exec(value);
      return Boolean(prefix && codes.has(prefix[1]));
    }
  });
}

const STATUS_KINDS = new Set([
  "planning_started",
  "planned",
  "model_failover",
  "approval_required",
  "blocked",
  "plan_approval_required",
  "status",
]);

/** Live-only chips that must not survive Stop / finish (legacy persisted status rows). */
const TRANSIENT_STATUS_LABELS = /^(planning|working|reconnecting|thinking)/i;

const TOOL_VERBS: Record<string, string> = {
  write_file: "Writing file",
  edit_file: "Editing file",
  read_file: "Reading file",
  find_past_files: "Finding files from past chats",
  attach_past_file: "Attaching a file from a past chat",
  list_dir: "Listing",
  glob: "Finding files",
  grep: "Searching",
  search: "Searching",
  web_fetch: "Fetching",
  web_search: "Finding articles",
  parallel_web_search: "Searching the web",
  parallel_web_fetch: "Fetching pages",
  image_generate: "Generating image",
  headless_fetch: "Reading pages in background",
  browser: "Using browser",
  browser_navigate: "Opening page",
  browser_open: "Opening page",
  browser_click: "Clicking in browser",
  browser_click_xy: "Clicking in browser",
  browser_hover: "Hovering in browser",
  browser_drag: "Dragging in browser",
  browser_type: "Typing in browser",
  browser_snapshot: "Reading page",
  browser_highlight: "Highlighting page",
  todo_write: "Updating todos",
  todo_read: "Reading todos",
  memory_search: "Searching memory",
  memory_write: "Saving memory",
  memory_read: "Reading memory",
  tool_search: "Searching tools",
  skill_run: "Running skill",
  skill_read: "Reading skill",
  peer_delegate: "Delegating to peer",
  peer_task_status: "Checking peer task",
  peer_artifact: "Peer result",
  peer_query_knowledge: "Asking peer",
  spawn_subagent: "Subagent",
  spawn_subagents: "Subagents",
  spawn_task_graph: "Subagent graph",
  agent_spawn: "Subagent",
  agent_status: "Checking subagent",
  agent_wait: "Waiting for subagent",
  agent_wait_any: "Waiting for subagents",
  agent_wait_all: "Waiting for subagents",
  agent_cancel: "Cancelling subagent",
};

export function isSubagentSpawn(name: string): boolean {
  return /^(?:agent_spawn)$/i.test(String(name || "")) || /subagent|task_graph/i.test(String(name || ""));
}

export function emptyTranscript(): TranscriptState {
  return { blocks: [], narrativeCursor: 0, seq: 0 };
}

/** Copy the visible answer, including narration preserved before the recap. */
export function messageNarrativeText(message: ChatMessage): string {
  const text = (message.blocks || []).map((b) => b.type === "narrative" ? b.text : "").join("");
  return text || message.text;
}

export function transcriptFromMessage(message: ChatMessage): TranscriptState {
  return {
    blocks: message.blocks ? [...message.blocks] : [],
    narrativeCursor: message.transcriptCursor ?? 0,
    seq: message.transcriptSeq ?? message.blocks?.length ?? 0,
    reasoningStartedAt: message.reasoningStartedAt,
  };
}

export function applyTranscriptToMessage(
  message: ChatMessage,
  event: TranscriptEvent,
  now = Date.now(),
): ChatMessage {
  let renderedEvent = event;
  if (event.type === "finish" && event.assembled && message.answerRevision?.replacement) {
    const replacement = message.answerRevision.replacement;
    const saved = (message.blocks || []).map((b) => b.type === "narrative" ? b.text : "").join("");
    // An explicit revision identifies even a short answer unambiguously.
    // Keep its progress prefix when the closing snapshot adds a caveat.
    if (saved.endsWith(replacement) && event.assembled.startsWith(replacement)) {
      renderedEvent = { ...event, assembled: saved.slice(0, -replacement.length) + event.assembled };
    } else if (saved.endsWith(replacement) && sameNarrativeSnapshot(replacement, event.assembled)) {
      // The revision owns this answer even when it is short or its terminal
      // projection contains a URL/path masked in the journal. Keep the mask.
      renderedEvent = { ...event, assembled: saved };
    }
  }
  const next = applyTranscriptEvent(transcriptFromMessage(message), renderedEvent, now);
  const text =
    event.type === "answer_revision"
      ? next.blocks.map((b) => b.type === "narrative" ? b.text : "").join("")
      : event.type === "narrative"
      ? event.assembled
      : event.type === "finish" && event.assembled != null
        ? event.assembled
        : message.text;
  return {
    ...message,
    text,
    blocks: next.blocks,
    ...(event.type === "finish" && message.toolCards
      ? { toolCards: message.toolCards.map(settleUnfinishedTool) } : {}),
    transcriptCursor: next.narrativeCursor,
    transcriptSeq: next.seq,
    reasoningStartedAt: next.reasoningStartedAt,
  };
}

/** Terminal transcript state is not evidence that an unfinished action succeeded. */
function settleUnfinishedTool(card: ToolCard): ToolCard {
  if (card.status && !["requested", "pending", "running"].includes(card.status)) return card;
  return {
    ...card,
    status: "interrupted",
    resultPreview: card.resultPreview || "Execution ended before completion was confirmed. Check the outcome before retrying.",
  };
}

/** A live buffer may lag durable event replay; never rewind saved narration. */
export function applyLiveNarrativeSnapshot(message: ChatMessage, live: string): ChatMessage {
  let text = String(live || "");
  const revision = message.answerRevision;
  if (revision && text.length <= (message.narrativeSourceCursor || 0)) return message;
  if (revision && text.endsWith(revision.original)) {
    text = text.slice(0, -revision.original.length) + revision.replacement;
  }
  if (!text || text === message.text) return message;
  const cursor = message.transcriptCursor ?? 0;
  const saved = (message.blocks || [])
    .filter((block) => block.type === "narrative")
    .map((block) => block.type === "narrative" ? block.text : "")
    .join("");
  if (text.length <= cursor || (saved && !text.startsWith(saved))) return message;
  return applyTranscriptToMessage(message, { type: "narrative", assembled: text });
}

export function attachAskHumanPending(
  message: ChatMessage,
  pending: PendingApproval,
): ChatMessage {
  return applyTranscriptToMessage(message, { type: "ask_human_pending", pending });
}

export function resolveAskHumanBlock(
  message: ChatMessage,
  approvalId: string,
  answer?: string,
  deferred = false,
): ChatMessage {
  return applyTranscriptToMessage(message, {
    type: "ask_human_resolved",
    approvalId,
    answer,
    deferred,
  });
}

export function applyTranscriptEvent(
  state: TranscriptState,
  event: TranscriptEvent,
  now = Date.now(),
): TranscriptState {
  switch (event.type) {
    case "thinking_started":
      return applyThinking(state, now);
    case "model_interrupted":
      return closeReasoning(state, now);
    case "reasoning":
      return applyReasoning(state, event.text, event.durationMs, now, event.partial);
    case "tool":
      return applyTool(state, event.card, event.interesting !== false, now);
    case "narrative":
      return applyNarrative(state, event.assembled);
    case "answer_revision": {
      const saved = state.blocks.map((b) => b.type === "narrative" ? b.text : "").join("");
      // Only an explicit, matching revision may replace delivered narration.
      // Replayed revisions are idempotent and cannot erase unrelated text.
      const suffix = answerRevisionSuffix(saved, event.original, event.offset);
      if (suffix === null) return state;
      let remaining = suffix;
      const blocks = [...state.blocks];
      for (let i = blocks.length - 1; i >= 0 && remaining > 0; i--) {
        const block = blocks[i];
        if (block.type !== "narrative") continue;
        const take = Math.min(remaining, block.text.length);
        blocks[i] = { ...block, text: block.text.slice(0, block.text.length - take) };
        remaining -= take;
      }
      const prefix = saved.slice(0, saved.length - suffix);
      return applyNarrative({ ...state, blocks: blocks.filter((b) => b.type !== "narrative" || b.text),
        narrativeCursor: prefix.length }, prefix + event.replacement);
    }
    case "peer_result":
      return applyPeerResult(state, event.path, event.text);
    case "status":
      return applyStatus(state, event.label, event.detail, event.kind);
    case "ask_human_pending":
      return applyAskHumanPending(state, event.pending);
    case "ask_human_resolved":
      return applyAskHumanResolved(
        state,
        event.approvalId,
        event.answer,
        event.deferred === true,
      );
    case "finish":
      return applyFinish(state, event.assembled, now);
    default:
      return state;
  }
}

export function isShellTool(name: string): boolean {
  const low = String(name || "").trim().toLowerCase();
  return SHELL_TOOLS.has(low);
}

export function isComputerObserveTool(name: string): boolean {
  const low = String(name || "").trim().toLowerCase();
  return COMPUTER_OBSERVE.test(low) || low === "computer_get_state";
}

export function isStatusKind(kind: string): boolean {
  return STATUS_KINDS.has(String(kind || "").trim());
}

export function streamSourceKind(
  kind: string,
  payload?: Record<string, unknown> | null,
): string {
  const source = String(payload?.source_kind || "").trim();
  return source || String(kind || "").trim();
}

/** Map one SSE/journal frame onto the transcript (thinking, reasoning, tool, status). */
export function applyStreamFrameToMessage(
  message: ChatMessage,
  frame: {
    sequence?: number;
    turn_id?: string;
    kind?: string;
    label?: string;
    detail?: string[];
    interesting?: boolean;
    payload?: Record<string, unknown> | null;
    card?: ToolCard | null;
  },
  now = Date.now(),
): ChatMessage {
  // Ownership applies to every journal event, including revisions and final
  // snapshots. A stale completion can corrupt an answer just like a delta.
  if (frame.turn_id && message.turnId && frame.turn_id !== message.turnId) return message;
  const turnId = frame.turn_id || message.turnId;
  const sequence = frame.sequence;
  const cursor = message.streamCursor;
  if (sequence != null && cursor && cursor.turnId === turnId && sequence <= cursor.sequence) return message;
  const next = applyOwnedStreamFrame(message, frame, now);
  return sequence != null && Number.isSafeInteger(sequence) && sequence >= 0
    ? { ...next, streamCursor: { turnId, sequence } } : next;
}

function applyOwnedStreamFrame(
  message: ChatMessage,
  frame: Parameters<typeof applyStreamFrameToMessage>[1],
  now: number,
): ChatMessage {
  const payload = frame.payload && typeof frame.payload === "object" ? frame.payload : {};
  const kind = streamSourceKind(String(frame.kind || ""), payload);
  // Budget snapshots are internal telemetry, even if they carry a label or card.
  // The outer reducer still advances the journal cursor for these frames.
  if (frame.kind === "request_budget" || kind === "request_budget") return message;
  const ladderStep = ladderStepFromEvent(kind, payload);
  if (ladderStep && ladderStep !== message.ladderStep) message = { ...message, ladderStep };
  if (kind === "model") {
    return { ...message, modelUsageSteps: [...(message.modelUsageSteps || []), {
      label: frame.label || "Model usage", detail: frame.detail || [], kind: "model", interesting: false,
    }] };
  }
  if (kind === "stream_item") {
    if (Number(payload.generation || 0) < (message.streamGeneration ?? 0)) return message;
    return applyStreamItem(message, payload, now);
  }
  if (["verification", "outcome_snapshot"].includes(kind) && Array.isArray(payload.outcomes)) {
    const outcomes = payload.outcomes.filter((row): row is NonNullable<ChatMessage["verificationOutcomes"]>[number] =>
      Boolean(row && typeof row === "object" && typeof row.id === "string" && typeof row.label === "string" &&
        ["pass", "fail", "blocked", "unresolved"].includes(row.status)));
    const normalized = outcomes.map(row => ({ ...row,
      evidence: Array.isArray(row.evidence) ? row.evidence.filter(receipt => receipt && typeof receipt.id === "string" &&
        typeof receipt.label === "string" && typeof receipt.reference === "string" && typeof receipt.detail === "string" &&
        typeof receipt.timestamp === "number" && Number.isFinite(receipt.timestamp)) : undefined,
    }));
    const previous = payload.merge === true
      ? (message.verificationOutcomes || []).filter(row => !normalized.some(next => next.id === row.id))
      : [];
    return { ...message, verificationOutcomes: [...previous, ...normalized] };
  }
  if (kind === "answer_revision" && typeof payload.original === "string" && typeof payload.replacement === "string") {
    const saved = (message.blocks || []).map((b) => b.type === "narrative" ? b.text : "").join("");
    const offset = typeof payload.offset === "number" ? payload.offset : undefined;
    if (answerRevisionSuffix(saved, payload.original, offset) === null) return message;
    const revised = applyTranscriptToMessage(message, { type: "answer_revision", original: payload.original,
      replacement: payload.replacement, offset }, now);
    return { ...revised,
      narrativeSourceCursor: Math.max(message.narrativeSourceCursor || 0, message.transcriptCursor || 0,
        offset === undefined ? 0 : offset + payload.original.length),
      answerRevision: { original: payload.original, replacement: payload.replacement } };
  }
  // Reconnects consume journal frames, not the original SSE closing message.
  // Persisted terminal text includes grace answers and verification footers.
  if (["completed", "cancelled", "failed"].includes(kind) && typeof payload.message === "string" && payload.message.trim()) {
    message = applyTranscriptToMessage(message, { type: "finish", assembled: payload.message }, now);
  }
  if (kind === "failed" || kind === "error") {
    const provisional = message.activeStreamItems?.text;
    if (provisional) {
      const blocks = (message.blocks || []).filter(block => block.id !== provisional);
      const text = blocks.map(block => block.type === "narrative" ? block.text : "").join("");
      message = { ...message, blocks, text, transcriptCursor: text.length,
        narrativeSourceCursor: text.length, activeStreamItems: {} };
    }
    const resultStatus = String(payload.status || "error");
    const terminal = terminalRunState(resultStatus);
    const cause = String(payload.failure_cause || payload.error || payload.reason || (
      ["invalid_output", "unverified"].includes(resultStatus) ? "The response was delivered, but task completion could not be verified."
      : terminal.status === "error" ? payload.message || frame.detail?.[0] || "The run failed."
      : terminal.label
    ));
    return { ...applyTranscriptToMessage(message, { type: "finish", assembled: message.text }, now),
      streaming: false, resultStatus,
      statusLabel: terminal.label, failureCause: cause };
  }
  if (payload.action === "ask_human" && typeof payload.approval_id === "string" &&
      payload.approval_id.startsWith("question-")) {
    if (kind === "approval_required") {
      return attachAskHumanPending(message, {
        approval_id: payload.approval_id, action: "ask_human", risk_class: "hitl",
        question: String(payload.question || ""), options: Array.isArray(payload.options) ? payload.options.map(String) : [],
      });
    }
    if (kind === "approval_resolved") {
      return resolveAskHumanBlock(message, payload.approval_id,
        typeof payload.answer === "string" ? payload.answer : undefined, payload.deferred === true);
    }
  }
  if (kind === "assistant_progress") {
    const text = typeof payload.text === "string" ? payload.text.trim() : "";
    if (typeof payload.acknowledgment_id === "string") {
      const id = `ack-${payload.acknowledgment_id}`;
      const blocks = message.blocks || [];
      if (!text) return { ...message, blocks: blocks.filter((block) => block.id !== id) };
      const progress = { type: "progress" as const, id, text };
      return { ...message, blocks: blocks.some((block) => block.id === id)
        ? blocks.map((block) => block.id === id ? progress : block)
        : [...blocks, progress] };
    }
    const id = `progress-${frame.sequence ?? payload.step}`;
    if (!text || message.blocks?.some((block) => block.id === id)) return message;
    return { ...message, blocks: [...(message.blocks || []), { type: "progress", id, text }] };
  }
  if (kind === "assistant_narrative") {
    const text = String(payload.text || "");
    const offset = Number(payload.offset || 0);
    const cursor = Math.max(message.narrativeSourceCursor || 0, message.transcriptCursor || 0);
    // Live deltas may already have rendered some or all of this saved step.
    // Replayed frames fill the missing suffix without repeating visible text.
    if (!text || cursor >= offset + text.length) return message;
    const existing = (message.blocks || [])
      .filter((block) => block.type === "narrative")
      .map((block) => block.type === "narrative" ? block.text : "")
      .join("");
    return { ...applyTranscriptToMessage(message, {
      type: "narrative",
      assembled: existing + text.slice(Math.max(0, cursor - offset)),
    }, now), narrativeSourceCursor: offset + text.length };
  }
  if (kind === "thinking_started") {
    return applyTranscriptToMessage(message, { type: "thinking_started" }, now);
  }
  if (kind === "model_interrupted") {
    return applyTranscriptToMessage(message, { type: "model_interrupted" }, now);
  }
  if (kind === "reasoning") {
    const text = reasoningTextFromFrame(payload, frame.detail);
    const durationMs = Number(payload.duration_ms ?? payload.durationMs);
    const partial = payload.partial === true || payload.partial === "true";
    return applyTranscriptToMessage(
      message,
      {
        type: "reasoning",
        text,
        durationMs: Number.isFinite(durationMs) ? durationMs : undefined,
        partial,
      },
      now,
    );
  }
  const label = String(frame.label || "");
  // Heartbeat details are telemetry, not provider reasoning text.
  const liveText = reasoningTextFromFrame(payload);
  if (
    kind === "running" ||
    kind === "starting" ||
    kind === "heartbeat" ||
    /^(still\s+)?(working|connecting|starting|thinking)[.…]*$/i.test(label)
  ) {
    if (liveText) {
      return applyTranscriptToMessage(
        message,
        { type: "reasoning", text: liveText, partial: true },
        now,
      );
    }
    const blocks = message.blocks || [];
    if (
      kind === "heartbeat" &&
      blocks.some((block) => block.type === "tool" || block.type === "shell")
    ) {
      return message;
    }
    return applyTranscriptToMessage(message, { type: "thinking_started" }, now);
  }
  if (kind === "peer_artifact" || kind === "peer_knowledge_answer") {
    const preview = String(payload.preview || payload.result_preview || "").trim();
    const path =
      kind === "peer_knowledge_answer"
        ? `knowledge: ${String(payload.query || "").trim()}`.trim()
        : String(payload.path || "").trim();
    let next = message;
    if (frame.card) {
      next = applyTranscriptToMessage(
        next,
        {
          type: "tool",
          card: {
            ...frame.card,
            resultPreview: preview || frame.card.resultPreview,
            outputPreview: preview || frame.card.outputPreview,
          },
          interesting: frame.interesting,
        },
        now,
      );
    }
    if (preview) {
      next = applyTranscriptToMessage(next, { type: "peer_result", path, text: preview }, now);
    }
    return next;
  }
  if (frame.card) {
    return applyTranscriptToMessage(
      message,
      {
        type: "tool",
        card: frame.card,
        interesting: frame.interesting,
      },
      now,
    );
  }
  return message;
}

/** Apply one ordered executor item. Offsets make retries/replay idempotent. */
function applyStreamItem(message: ChatMessage, payload: Record<string, unknown>, now: number): ChatMessage {
  const channel = payload.channel;
  if (channel !== "text" && channel !== "reasoning") return message;
  const generation = Number(payload.generation || 0);
  if (!Number.isFinite(generation) || generation < (message.streamGeneration ?? 0)) return message;
  const itemId = String(payload.item_id || "");
  if (!itemId) return message;
  const id = `stream-${itemId}`;
  const base = Number(payload.base_offset || 0);
  const offset = Number(payload.offset || 0);
  if (!Number.isSafeInteger(base) || base < 0 || !Number.isSafeInteger(offset) || offset < 0) return message;
  const newGeneration = generation > (message.streamGeneration ?? 0);
  const superseded = new Set(newGeneration ? Object.values(message.activeStreamItems || {}) : []);
  const blocks = (message.blocks || []).filter(block => !superseded.has(block.id));
  const activeStreamItems: NonNullable<ChatMessage["activeStreamItems"]> = newGeneration ? {} : { ...message.activeStreamItems };
  const index = blocks.findIndex(block => block.id === id);
  if (payload.state === "discarded") {
    if (index < 0) return message;
    blocks.splice(index, 1);
  } else {
    const existing = index < 0 ? "" : (blocks[index].type === "narrative" || blocks[index].type === "reasoning")
      ? blocks[index].text || "" : "";
    const chunk = String(payload.text || "");
    if (payload.state !== "completed" && offset > existing.length) return message;
    const text = payload.state === "completed" ? chunk : existing + chunk.slice(Math.max(0, existing.length - offset));
    const block: TranscriptBlock = channel === "text"
      ? { type: "narrative", id, text }
      : { type: "reasoning", id, text, streaming: payload.state !== "completed" };
    // The empty activity placeholder is replaced by the first real thought.
    if (channel === "reasoning" && index < 0) {
      const empty = blocks.findIndex(b => b.type === "reasoning" && b.streaming && !b.text);
      if (empty >= 0) blocks.splice(empty, 1);
    }
    const target = blocks.findIndex(b => b.id === id);
    if (target >= 0) blocks[target] = block;
    else blocks.push(block);
  }
  const text = blocks.map(block => block.type === "narrative" ? block.text : "").join("");
  const item = blocks.find(b => b.id === id);
  if (payload.state === "delta") activeStreamItems[channel] = id;
  else delete activeStreamItems[channel];
  const cursor = channel === "text" || newGeneration ? base + (item?.type === "narrative" ? item.text.length : 0)
    : message.transcriptCursor;
  return { ...message, blocks, text, streamGeneration: generation, activeStreamItems,
    transcriptCursor: cursor, narrativeSourceCursor: channel === "text" || newGeneration ? cursor : message.narrativeSourceCursor,
    reasoningStartedAt: channel === "reasoning" && payload.state === "delta" ? message.reasoningStartedAt ?? now : undefined };
}

export function replayStreamFrames(
  message: ChatMessage,
  frames: Array<Parameters<typeof applyStreamFrameToMessage>[1]>,
  finalText?: string,
): ChatMessage {
  let next: ChatMessage = { ...message, blocks: message.blocks ? [...message.blocks] : [] };
  for (const frame of frames) {
    next = applyStreamFrameToMessage(next, frame);
  }
  const answer = finalText?.trim() ? finalText : next.text;
  const finished = applyTranscriptToMessage(next, { type: "finish", assembled: answer });
  // Keep the canonical answer for persistence; blocks retain delivered narration.
  return { ...finished, text: answer };
}

export function formatDuration(ms: number): string {
  if (!Number.isFinite(ms) || ms < 0) return "";
  if (ms < 1000) return `${Math.round(ms)}ms`;
  if (ms < 60_000) {
    const sec = ms / 1000;
    return Number.isInteger(sec) || sec >= 10 ? `${Math.round(sec)}s` : `${sec.toFixed(1)}s`;
  }
  return `${(ms / 60_000).toFixed(1)}m`;
}

export function toolPillLabel(card: ToolCard): string {
  const name = String(card.name || "").trim();
  const verb = TOOL_VERBS[name] || humanizeTool(name);
  const target = previewTarget(card.argsPreview, name);
  if (!target) return verb;
  if (isShellTool(name)) return target;
  return `${verb} ${target}`;
}

export function toolChipKind(name: string): string {
  const n = String(name || "").toLowerCase();
  if (n === "headless_fetch" || /^(?:parallel_)?web_(?:search|fetch)$/.test(n)) return "Using web research";
  if (n === "image_generate" || n.includes("image_generation")) return "Creating image";
  if (isShellTool(n)) return "Running command";
  if (n.startsWith("mcp_") || n.startsWith("mcp.") || n.includes("__")) return "Running integration";
  if (n.includes("browser") || n.startsWith("web_") || n.includes("fetch")) return "Using browser";
  if (n.startsWith("computer_")) return "Using computer";
  if (isSubagentSpawn(n)) return "Running subagents";
  if (n === "agent_wait" || n === "agent_wait_any" || n === "agent_wait_all") return "Waiting for subagents";
  if (n === "agent_status") return "Checking subagent";
  if (n === "agent_cancel") return "Cancelling subagent";
  return "Running tool";
}

export function toolChipSummary(card: ToolCard): string {
  const name = String(card.name || "").trim();
  if (isShellTool(name)) return shellSummary(card);
  if (
    name.toLowerCase().startsWith("mcp_") ||
    name.toLowerCase().startsWith("mcp.") ||
    name.includes("__")
  ) return name;
  return clipLine(toolPillLabel(card), 56);
}

export function shellCommand(card: ToolCard): string {
  return extractShellCommand(card.argsPreview) || "";
}

export function shellSummary(card: ToolCard): string {
  const fromArgs = extractKeyedValue(card.argsPreview, ["description"]);
  if (fromArgs) return clipLine(fromArgs, 72);
  return clipLine(shellCommand(card), 64);
}

/** bash returns a JSON envelope; show stdout/stderr in the transcript. */
export function unwrapShellPayload(raw: string): string {
  const trimmed = String(raw || "").trim();
  if (!trimmed.startsWith("{")) return trimmed;
  try {
    const obj = JSON.parse(trimmed) as Record<string, unknown>;
    if (obj && typeof obj === "object" && ("stdout" in obj || "stderr" in obj || "exit_code" in obj)) {
      const stdout = String(obj.stdout ?? "").trim();
      const stderr = String(obj.stderr ?? "").trim();
      const parts = [stdout, stderr].filter(Boolean);
      const code = obj.exit_code;
      if (code != null && Number(code) !== 0) parts.push(`exit_code: ${code}`);
      return parts.join("\n").trim() || trimmed;
    }
  } catch {
    // Plain text and malformed JSON remain visible verbatim.
  }
  return trimmed;
}

function nextId(state: TranscriptState, prefix: string): { id: string; seq: number } {
  const seq = state.seq + 1;
  return { id: `${prefix}-${seq}`, seq };
}

function closeReasoning(state: TranscriptState, now: number): TranscriptState {
  const idx = lastIndex(state.blocks, (b) => b.type === "reasoning");
  if (idx < 0) return { ...state, reasoningStartedAt: undefined };
  const block = state.blocks[idx];
  if (block.type !== "reasoning") return state;
  if (!block.streaming && block.durationMs != null) {
    return { ...state, reasoningStartedAt: undefined };
  }
  const durationMs = resolveReasoningDuration(
    block.durationMs,
    state.reasoningStartedAt,
    now,
  );
  const blocks = [...state.blocks];
  const closed = { ...block, streaming: false, durationMs };
  if (!closed.text) {
    blocks.splice(idx, 1);
    return { ...state, blocks, reasoningStartedAt: undefined };
  }
  blocks[idx] = closed;
  return { ...state, blocks, reasoningStartedAt: undefined };
}

function stripEmptyStreamingReasoning(state: TranscriptState): TranscriptState {
  const idx = lastIndex(
    state.blocks,
    (b) => b.type === "reasoning" && b.streaming === true,
  );
  if (idx < 0) return state;
  const block = state.blocks[idx];
  if (block.type !== "reasoning" || block.text?.trim()) return state;
  const blocks = [...state.blocks];
  blocks.splice(idx, 1);
  return { ...state, blocks, reasoningStartedAt: undefined };
}

function applyThinking(state: TranscriptState, now: number): TranscriptState {
  const next = closeReasoning(state, now);
  if (next.blocks.some((block) => block.type === "reasoning" && block.streaming)) {
    return next;
  }
  const { id, seq } = nextId(next, "reason");
  return {
    ...next,
    seq,
    reasoningStartedAt: now,
    blocks: [...next.blocks, { type: "reasoning", id, streaming: true }],
  };
}

function applyReasoning(
  state: TranscriptState,
  text: string | undefined,
  durationMs: number | undefined,
  now: number,
  partial = false,
): TranscriptState {
  let next = state;
  const idx = lastIndex(next.blocks, (b) => b.type === "reasoning");
  if (idx < 0) {
    next = applyThinking(next, now);
  }
  const reasonIdx = lastIndex(next.blocks, (b) => b.type === "reasoning");
  if (reasonIdx < 0) return next;
  const block = next.blocks[reasonIdx];
  if (block.type !== "reasoning") return next;
  const hasDuration = durationMs != null && Number.isFinite(durationMs);
  const stillStreaming = Boolean(partial) && !hasDuration;
  const incoming = (text || "").trim();
  const nextText = text == null ? block.text : incoming;
  if (!nextText) {
    if (stillStreaming) return next;
    const blocks = [...next.blocks];
    blocks.splice(reasonIdx, 1);
    return { ...next, blocks, reasoningStartedAt: undefined };
  }
  const duration = stillStreaming
    ? block.durationMs
    : resolveReasoningDuration(
        hasDuration ? durationMs : block.durationMs,
        next.reasoningStartedAt,
        now,
      );
  const blocks = [...next.blocks];
  blocks[reasonIdx] = {
    ...block,
    text: nextText,
    durationMs: duration,
    streaming: stillStreaming,
  };
  return {
    ...next,
    blocks,
    reasoningStartedAt: stillStreaming ? next.reasoningStartedAt : undefined,
  };
}

function applyTool(
  state: TranscriptState,
  card: ToolCard,
  interesting: boolean,
  _now: number,
): TranscriptState {
  if (isComputerObserveTool(card.name)) return state;
  if (!interesting && /^computer_/i.test(card.name)) return state;

  const next = stripEmptyStreamingReasoning(state);
  const shell = isShellTool(card.name);
  const existing = next.blocks.findIndex(
    (b) => (b.type === "tool" || b.type === "shell") && b.card.id === card.id,
  );
  if (existing >= 0) {
    const prev = next.blocks[existing];
    if (prev.type !== "tool" && prev.type !== "shell") return next;
    const merged = { ...prev.card, ...card };
    const blocks = [...next.blocks];
    blocks[existing] = shell
      ? { type: "shell", id: prev.id, card: merged }
      : { type: "tool", id: prev.id, card: merged };
    return { ...next, blocks };
  }
  const { id, seq } = nextId(next, shell ? "shell" : "tool");
  const block: TranscriptBlock = shell
    ? { type: "shell", id, card }
    : { type: "tool", id, card };
  return { ...next, seq, blocks: [...next.blocks, block] };
}

function applyPeerResult(state: TranscriptState, path: string | undefined, text: string): TranscriptState {
  const body = String(text || "").trim();
  if (!body) return state;
  const already = state.blocks.some(
    (block) => block.type === "peer_result" && block.text === body && (block.path || "") === (path || ""),
  );
  if (already) return state;
  const { id, seq } = nextId(state, "peer");
  return {
    ...state,
    seq,
    blocks: [...state.blocks, { type: "peer_result", id, path, text: body }],
  };
}

function applyNarrative(state: TranscriptState, assembled: string): TranscriptState {
  const text = assembled ?? "";
  if (text.length < state.narrativeCursor) {
    return replaceNarratives(state, text);
  }
  const chunk = text.slice(state.narrativeCursor);
  if (!chunk) return { ...state, narrativeCursor: text.length };
  const last = state.blocks[state.blocks.length - 1];
  if (last?.type === "narrative") {
    const blocks = [...state.blocks];
    blocks[blocks.length - 1] = { ...last, text: last.text + chunk };
    return { ...state, blocks, narrativeCursor: text.length };
  }
  const { id, seq } = nextId(state, "say");
  return {
    ...state,
    seq,
    narrativeCursor: text.length,
    blocks: [...state.blocks, { type: "narrative", id, text: chunk }],
  };
}

function replaceNarratives(state: TranscriptState, text: string): TranscriptState {
  const without = state.blocks.filter((b) => b.type !== "narrative");
  if (!text) {
    return { ...state, blocks: without, narrativeCursor: 0 };
  }
  const { id, seq } = nextId({ ...state, blocks: without }, "say");
  return {
    ...state,
    seq,
    narrativeCursor: text.length,
    blocks: [...without, { type: "narrative", id, text }],
  };
}

function askHumanFieldsFromPending(pending: PendingApproval): {
  approvalId: string;
  question: string;
  options: string[];
} {
  const rawDetail = Array.isArray(pending.detail)
    ? pending.detail.join("\n")
    : String(pending.detail || "");
  const question =
    String(pending.question || "").trim() ||
    rawDetail.split(/\n\s*\[(?:Y|N|\d+)\]/i)[0]?.trim() ||
    rawDetail.trim() ||
    "The agent has a question";
  let options = Array.isArray(pending.options)
    ? pending.options.map((item) => String(item).trim()).filter(Boolean).slice(0, 8)
    : [];
  if (!options.length) {
    const yes = String(pending.yes_label || "").trim();
    const no = String(pending.no_label || "").trim();
    if (yes || no) options = [yes || "Yes", no || "No"];
  }
  return {
    approvalId: String(pending.approval_id || "").trim(),
    question,
    options,
  };
}

function applyAskHumanPending(
  state: TranscriptState,
  pending: PendingApproval,
): TranscriptState {
  const { approvalId, question, options } = askHumanFieldsFromPending(pending);
  if (!approvalId) return state;
  const existing = state.blocks.findIndex(
    (block) => block.type === "ask_human" && block.approvalId === approvalId,
  );
  if (existing >= 0) {
    const blocks = [...state.blocks];
    const prev = blocks[existing];
    if (prev.type !== "ask_human") return state;
    blocks[existing] = {
      ...prev,
      question,
      options,
      status: "pending",
      answer: undefined,
    };
    return { ...state, blocks };
  }
  const { id, seq } = nextId(state, "ask");
  return {
    ...state,
    seq,
    blocks: [
      ...state.blocks,
      {
        type: "ask_human",
        id,
        approvalId,
        question,
        options,
        status: "pending",
      },
    ],
  };
}

function applyAskHumanResolved(
  state: TranscriptState,
  approvalId: string,
  answer: string | undefined,
  deferred: boolean,
): TranscriptState {
  const id = String(approvalId || "").trim();
  if (!id) return state;
  const idx = state.blocks.findIndex(
    (block) => block.type === "ask_human" && block.approvalId === id,
  );
  if (idx < 0) return state;
  const prev = state.blocks[idx];
  if (prev.type !== "ask_human") return state;
  const blocks = [...state.blocks];
  blocks[idx] = {
    ...prev,
    status: deferred ? "deferred" : "answered",
    answer: deferred
      ? "Let the agent decide"
      : String(answer || "").trim() || prev.answer,
  };
  return { ...state, blocks };
}

function applyStatus(
  state: TranscriptState,
  _label: string,
  _detail: string[] | undefined,
  _kind: string | undefined,
): TranscriptState {
  // Run telemetry (prompt cache, todos, verification, planning) lives in the
  // Plan tab and canvas sidebar — not inline in the conversation transcript.
  return state;
}

function applyFinish(
  state: TranscriptState,
  assembled: string | undefined,
  now: number,
): TranscriptState {
  let next = closeReasoning(state, now);
  next = {
    ...next,
    blocks: next.blocks
      .filter(
        (b) =>
          b.type !== "status" || !TRANSIENT_STATUS_LABELS.test(b.label),
      )
      .map((b) => b.type === "reasoning" ? { ...b, streaming: false }
        : b.type === "tool" || b.type === "shell" ? { ...b, card: settleUnfinishedTool(b.card) } : b),
  };
  if (assembled != null) {
    const saved = next.blocks.map((b) => b.type === "narrative" ? b.text : "").join("");
    const overlap = terminalAnswerOverlap(saved, assembled);
    if (overlap) {
      // Replace only the shared final body, retaining earlier narration and
      // tool/reasoning blocks in their original order. applyNarrative appends;
      // it cannot insert a terminal artifact header into already streamed text.
      let remaining = overlap;
      const blocks = [...next.blocks];
      for (let i = blocks.length - 1; i >= 0 && remaining > 0; i--) {
        const block = blocks[i];
        if (block.type !== "narrative") continue;
        const take = Math.min(remaining, block.text.length);
        blocks[i] = { ...block, text: block.text.slice(0, block.text.length - take) };
        remaining -= take;
      }
      next = { ...next, blocks: blocks.filter((b) => b.type !== "narrative" || b.text),
        narrativeCursor: saved.length - overlap };
      return applyNarrative(next, mergeTerminalNarrative(saved, assembled));
    }
    const merged = mergeTerminalNarrative(saved, assembled,
      next.blocks.at(-1)?.type === "narrative" ? "\n\n" : "");
    if (merged !== saved) next = applyNarrative(next, merged);
  }
  return next;
}

function lastIndex<T>(items: T[], pred: (item: T) => boolean): number {
  for (let i = items.length - 1; i >= 0; i--) {
    if (pred(items[i])) return i;
  }
  return -1;
}

function humanizeTool(name: string): string {
  const pretty = String(name || "tool").replace(/_/g, " ").trim();
  if (!pretty) return "Running tool";
  return pretty.charAt(0).toUpperCase() + pretty.slice(1);
}

function resolveReasoningDuration(
  durationMs: number | undefined,
  startedAt: number | undefined,
  now: number,
): number | undefined {
  const wall =
    startedAt != null && Number.isFinite(startedAt)
      ? Math.max(0, now - startedAt)
      : undefined;
  if (durationMs == null || !Number.isFinite(durationMs)) return wall;
  if (wall != null && durationMs < 500 && wall >= 1500) return wall;
  return durationMs;
}

function reasoningTextFromFrame(
  payload: Record<string, unknown>,
  detail?: string[],
): string {
  const fromPayload = String(payload.text || "").trim();
  if (fromPayload) return fromPayload;
  const first = String(detail?.[0] || "").trim();
  if (!first || /^(duration|step)\b/i.test(first)) return "";
  return first;
}

function extractKeyedValue(args: string | undefined, keys: string[]): string {
  const raw = String(args || "").trim();
  if (!raw) return "";
  const fromJson = targetFromJson(raw, keys);
  if (fromJson) return fromJson;
  return keyedArg(raw, keys);
}

function extractShellCommand(args: string | undefined): string {
  const raw = String(args || "").trim();
  if (!raw) return "";
  const fromKeyed = extractKeyedValue(raw, ["command", "cmd"]);
  if (fromKeyed) return fromKeyed;
  if (/^(command|cmd|description)\s*[:=]/i.test(raw) || raw.startsWith("{")) return "";
  return raw.replace(/^["']|["']$/g, "");
}

function hostnameFromArgs(raw: string): string {
  const match = String(raw || "").match(/https?:\/\/[^\s"'<>]+/i);
  if (!match) return "";
  try {
    return new URL(match[0].replace(/[.,;)\]]+$/, "")).hostname.replace(/^www\./, "");
  } catch {
    return "";
  }
}

function previewTarget(args: string | undefined, name: string): string {
  const raw = String(args || "").trim();
  if (!raw) return "";
  if (/\[REDACTED\]/i.test(raw)) return "";
  if (isShellTool(name)) return extractShellCommand(raw);
  if (isSubagentSpawn(name)) {
    const sub = previewSubagentTarget(raw);
    if (sub) return sub;
  }
  if (/browse|navigate|open|fetch/i.test(name)) {
    const host = hostnameFromArgs(raw);
    if (host) return host;
  }
  const blob = firstQuotedBlob(raw);
  if (blob && /search|fetch|browse/i.test(name)) return clipLine(blob, 42);
  const fromJson = targetFromJson(raw);
  if (fromJson) return basename(fromJson);
  const keyed = keyedArg(raw, [
    "path",
    "file",
    "filename",
    "query",
    "url",
    "pattern",
    "glob",
    "name",
    "script",
  ]);
  if (keyed) return basename(keyed);
  const file = raw.match(/([\w./-]+\.[A-Za-z0-9]{1,8})/);
  if (file?.[1]) return basename(file[1]);
  return clipLine(raw, 64);
}

function keyedArg(raw: string, keys: string[]): string {
  for (const key of keys) {
    const match = raw.match(
      new RegExp(`(?:^|,\\s*)${key}\\s*[:=]\\s*(?:"([^"]*)"|'([^']*)'|([^,]+))`, "i"),
    );
    const value = (match?.[1] || match?.[2] || match?.[3] || "").trim();
    if (value) return value;
  }
  return "";
}

function firstQuotedBlob(raw: string): string {
  const match = String(raw || "").match(/"([^"]{6,})/);
  return match?.[1]?.replace(/[.…]+$/, "").trim() || "";
}

function clipLine(line: string, max: number): string {
  const trimmed = String(line || "").trim();
  if (trimmed.length <= max) return trimmed;
  return `${trimmed.slice(0, Math.max(1, max - 1))}…`;
}

function targetFromJson(raw: string, prefer?: string[]): string | null {
  if (!raw.startsWith("{") && !raw.startsWith("[")) return null;
  try {
    const obj = JSON.parse(raw) as Record<string, unknown>;
    const keys = prefer?.length
      ? prefer
      : ["path", "file", "filename", "command", "cmd", "query", "url", "pattern", "glob", "name"];
    for (const key of keys) {
      if (obj[key] != null && obj[key] !== "") return String(obj[key]);
    }
    return null;
  } catch {
    return null;
  }
}

function basename(path: string): string {
  const trimmed = String(path || "").replace(/\\/g, "/").replace(/\/+$/, "");
  if (!trimmed) return path;
  const parts = trimmed.split("/");
  return parts[parts.length - 1] || trimmed;
}

export interface SubagentInfo {
  id: string;
  job_id?: string;
  name?: string;
  task: string;
  status: "pending" | "running" | "completed" | "failed";
  result?: string;
  error?: string;
}

function previewSubagentTarget(raw: string): string {
  const items = parseSubagentJobsFromArgs(raw);
  if (!items.length) return "";
  if (items.length === 1) return items[0].name || items[0].id || clipLine(items[0].task, 32);
  const ids = items.map((i) => i.name || i.id).filter(Boolean);
  if (ids.length) {
    return `${items.length} tasks (${ids.join(", ")})`;
  }
  return `${items.length} tasks`;
}

export function parseSubagentJobsFromArgs(args: string): Array<{ id: string; task: string; name?: string }> {
  const raw = String(args || "").trim();
  if (!raw) return [];
  let parsedRaw: any = null;
  try {
    if (raw.startsWith("{") || raw.startsWith("[")) {
      parsedRaw = JSON.parse(raw);
    }
  } catch {}

  if (!parsedRaw) {
    const match = raw.match(/(?:tasks_json|nodes_json|tasks|nodes)\s*[:=]\s*(.+)$/s);
    if (match) {
      const candidate = match[1].trim();
      try {
        parsedRaw = JSON.parse(candidate);
      } catch {
        try {
          const unquoted = candidate.replace(/^["']|["']$/g, "").replace(/\\"/g, '"').replace(/\\n/g, "\n");
          parsedRaw = JSON.parse(unquoted);
        } catch {}
      }
    }
  }

  let jobList: any[] = [];
  if (Array.isArray(parsedRaw)) {
    jobList = parsedRaw;
  } else if (parsedRaw && typeof parsedRaw === "object") {
    if (Array.isArray(parsedRaw.tasks)) jobList = parsedRaw.tasks;
    else if (Array.isArray(parsedRaw.nodes)) jobList = parsedRaw.nodes;
    else if (typeof parsedRaw.tasks_json === "string") {
      try { jobList = JSON.parse(parsedRaw.tasks_json); } catch {}
    } else if (typeof parsedRaw.nodes_json === "string") {
      try { jobList = JSON.parse(parsedRaw.nodes_json); } catch {}
    } else if (parsedRaw.objective) {
      jobList = [{ ...parsedRaw, id: parsedRaw.id || parsedRaw.agent_id || "subagent", name: parsedRaw.name || parsedRaw.agent_id }];
    } else if (parsedRaw.task) {
      jobList = [{ ...parsedRaw, id: parsedRaw.id || parsedRaw.label || "subagent" }];
    } else if (parsedRaw.assignment?.task) {
      jobList = [{ ...parsedRaw.assignment, name: parsedRaw.name || parsedRaw.assignment.name }];
    }
  }

  if (!jobList.length && raw) {
    const pairRegex = /["'](?:id|label)["']\s*:\s*["']([^"']+)["']\s*,\s*["'](?:task|prompt)["']\s*:\s*["']((?:[^"\\]|\\.)*)["']/g;
    let m;
    while ((m = pairRegex.exec(raw)) !== null) {
      jobList.push({ id: m[1], task: m[2].replace(/\\n/g, " ").replace(/\\"/g, '"') });
    }
  }

  const result: Array<{ id: string; task: string; name?: string }> = [];
  for (let i = 0; i < jobList.length; i++) {
    const item = jobList[i];
    if (typeof item === "string") {
      result.push({ id: `t${i + 1}`, task: item });
    } else if (item && typeof item === "object") {
      result.push({
        id: String(item.id || item.label || `t${i + 1}`),
        ...(item.name ? { name: String(item.name) } : {}),
        task: String(item.task || item.prompt || item.objective || ""),
      });
    }
  }
  return result;
}

const GRAPH_STATUS: Record<string, SubagentInfo["status"]> = {
  done: "completed",
  running: "running",
  pending: "pending",
  ready: "pending",
  queued: "pending",
  succeeded: "completed",
  completed: "completed",
  waiting_input: "running",
  reconciling: "running",
  cancelling: "running",
  cancelled: "failed",
  interrupted: "failed",
  failed: "failed",
};

function subagentsFromGraph(card: ToolCard): SubagentInfo[] {
  const running = !card.status || card.status === "running" || card.status === "pending" || card.status === "requested";
  const callFailed = card.status === "error" || card.status === "failed" || card.status === "denied";
  return (card.subagentGraph || []).map(({ id, job_id, name, task, status, result, error }) => ({
    id,
    ...(name ? { name } : {}),
    ...(job_id ? { job_id } : {}),
    task,
    status: callFailed ? "failed" : card.name === "agent_spawn" && !job_id && !running ? "pending" : status
      ? GRAPH_STATUS[status] ?? "failed"
      : running ? "running" : callFailed ? "failed" : "completed",
    ...(result ? { result } : {}),
    ...(error ? { error } : {}),
  }));
}

export function parseSubagentsFromCard(card: ToolCard): SubagentInfo[] {
  const name = String(card.name || "").toLowerCase();
  if (!isSubagentSpawn(name)) return [];
  // The server ships structured nodes: the one-line args preview is too short
  // to carry a real graph, and result previews are clipped mid-JSON.
  if (card.subagentGraph?.length) return subagentsFromGraph(card);
  const jobs = parseSubagentJobsFromArgs(card.argsPreview || "");
  const running = !card.status || card.status === "running" || card.status === "pending" || card.status === "requested";
  const overallFailed = card.status === "error" || card.status === "failed" || card.status === "denied";

  const items: SubagentInfo[] = jobs.map((j) => ({
    id: j.id,
    ...(j.name ? { name: j.name } : {}),
    task: j.task,
    status: overallFailed ? "failed" : running ? "running" : name === "agent_spawn" ? "pending" : "completed",
  }));

  const resultRaw = String(card.resultPreview || card.outputPreview || "").trim();
  if (resultRaw) {
    try {
      const resObj = JSON.parse(resultRaw);
      if (name === "agent_spawn" && resObj && typeof resObj.id === "string" && typeof resObj.objective === "string") {
        const status = GRAPH_STATUS[String(resObj.state || "")] || "pending";
        const entry: SubagentInfo = { id: resObj.id, job_id: resObj.id, task: resObj.objective, status,
          ...(resObj.spec?.persona_id ? { name: String(resObj.spec.persona_id) } : {}) };
        if (items.length) items[0] = entry;
        else items.push(entry);
      }
      const resList = Array.isArray(resObj.results)
        ? resObj.results
        : Array.isArray(resObj)
          ? resObj
          : [];
      for (const r of resList) {
        if (!r || typeof r !== "object") continue;
        const targetId = String(r.label || r.id || "");
        const match = items.find((it) => it.id === targetId);
        if (match) {
          match.status = r.ok === false || r.error ? "failed" : "completed";
          match.result = r.result || r.summary || (r.ok ? "Done" : undefined);
          match.error = r.error;
        }
      }
    } catch {}
  }
  return items;
}
