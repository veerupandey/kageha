import { terminalRunState } from "../lib/runStatus";
import type { ChatMessage } from "../api/types";
import { api } from "../api/client";
import { replayStreamFrames } from "../lib/transcript";
import { captureSteeringBoundary } from "../lib/steering";
import { normalizeToolCard } from "./helpers";

interface EventFrame {
  sequence?: number;
  kind?: string;
  label?: string;
  detail?: string[];
  interesting?: boolean;
  tool_card?: Record<string, unknown>;
  payload?: Record<string, unknown>;
}

interface TurnRow {
  id?: string;
  turn_id?: string;
  status?: string;
  objective?: string;
}

const EVENT_PAGE = 500;

export function turnIdOf(turn: TurnRow): string {
  return String(turn.id || turn.turn_id || "").trim();
}

/**
 * Prefer a persisted turn_id on the assistant message. Fall back to pairing
 * remaining completed turns with unmatched assistant rows from the end.
 */
export function matchTurnsToAssistants(
  messages: ChatMessage[],
  turnIds: string[],
): Array<{ turnId: string; messageIndex: number }> {
  const used = new Set<number>();
  const pairs: Array<{ turnId: string; messageIndex: number }> = [];
  for (const turnId of turnIds) {
    const indexed = messages.findIndex(
      (message, index) =>
        !used.has(index) &&
        message.role === "assistant" &&
        !message.streaming &&
        message.turnId === turnId,
    );
    if (indexed >= 0) {
      used.add(indexed);
      pairs.push({ turnId, messageIndex: indexed });
    }
  }
  const leftoverTurns = turnIds.filter((id) => !pairs.some((pair) => pair.turnId === id));
  const leftoverAssistants: number[] = [];
  for (let i = 0; i < messages.length; i++) {
    if (
      !used.has(i) &&
      messages[i].role === "assistant" &&
      !messages[i].streaming &&
      !messages[i].turnId
    ) {
      leftoverAssistants.push(i);
    }
  }
  const n = Math.min(leftoverTurns.length, leftoverAssistants.length);
  const turnSlice = leftoverTurns.slice(leftoverTurns.length - n);
  const indexSlice = leftoverAssistants.slice(leftoverAssistants.length - n);
  for (let i = 0; i < n; i++) {
    pairs.push({ turnId: turnSlice[i], messageIndex: indexSlice[i] });
  }
  return pairs;
}

export async function fetchTurnEvents(
  sessionId: string,
  threadId: string,
  turnId: string,
  signal?: AbortSignal,
): Promise<EventFrame[]> {
  const all: EventFrame[] = [];
  let after = 0;
  for (;;) {
    if (signal?.aborted) break;
    const data = await api<{ events?: EventFrame[] }>(
      `/api/sessions/${encodeURIComponent(sessionId)}/events` +
        `?thread_id=${encodeURIComponent(threadId)}` +
        `&turn_id=${encodeURIComponent(turnId)}` +
        `&after_sequence=${after}` +
        `&limit=${EVENT_PAGE}&include_live=0`,
      { signal },
    );
    if (signal?.aborted) break;
    const events = data.events || [];
    if (!events.length) break;
    all.push(...events);
    const lastSeq = events[events.length - 1]?.sequence;
    if (typeof lastSeq !== "number" || events.length < EVENT_PAGE) break;
    if (lastSeq <= after) break;
    after = lastSeq;
  }
  return all;
}

/**
 * Rebuild Hyperagent-style transcript blocks from the durable journal
 * so a refreshed session still shows reasoning / tool pills / shell output.
 */
/** Interrupted turns can have journal activity without a saved final answer. */
export function restoreMissingAssistantRows(
  messages: ChatMessage[], turns: TurnRow[], activeTurnId: string,
): ChatMessage[] {
  const next = messages.map((message) => ({ ...message }));
  const normalize = (text: string) => {
    // Older chat logs saved the model's continuation context instead of the
    // human's original short message. Pair it with that turn's objective.
    const continued = text.match(/^Current user message:\s*([\s\S]*?)\nEarlier request being continued:/);
    return (continued?.[1] ?? text).trim().replace(/\s+/g, " ");
  };
  let beforeUser = next.length;
  for (const turn of [...turns].reverse()) {
    const turnId = turnIdOf(turn);
    if (!turnId) continue;
    let userIndex = -1;
    for (let i = beforeUser - 1; i >= 0; i--) {
      const message = next[i];
      if (message.role === "user" && !message.steering && (
        message.turnId === turnId ||
        (!message.turnId && turn.objective && normalize(message.text) === normalize(turn.objective))
      )) {
        userIndex = i;
        break;
      }
    }
    if (userIndex < 0) continue;
    beforeUser = userIndex;
    next[userIndex].turnId = turnId;
    const end = next.findIndex((message, i) => i > userIndex && message.role === "user" && !message.steering);
    const assistantIndex = next.findIndex((message, i) =>
      i > userIndex && (end < 0 || i < end) && message.role === "assistant" &&
      (!message.turnId || message.turnId === turnId));
    if (assistantIndex >= 0) {
      next[assistantIndex].turnId = turnId;
      if (turnId !== activeTurnId && turn.status) {
        next[assistantIndex].resultStatus = turn.status;
        next[assistantIndex].statusLabel = terminalRunState(turn.status).label;
      }
    } else if (turnId !== activeTurnId && !next.some((message) => message.role === "assistant" && message.turnId === turnId)) {
      const status = String(turn.status || "");
      next.splice(end < 0 ? next.length : end, 0, {
        id: `restored-${turnId}`, role: "assistant", text: "", turnId,
        statusLabel: status === "cancelled" ? "Cancelled" :
          /^(failed|error)$/.test(status) ? "Error" : "Interrupted before a final reply",
      });
    }
  }
  return next;
}

export async function hydrateMessageTranscripts(
  sessionId: string,
  threadId: string,
  messages: ChatMessage[],
  turns: TurnRow[],
  activeTurnId: string,
  signal?: AbortSignal,
  onTurnHydrated?: (rows: ChatMessage[]) => void,
): Promise<ChatMessage[]> {
  messages = restoreMissingAssistantRows(messages, turns, activeTurnId);
  const completed = turns
    .map(turnIdOf)
    .filter((id) => id && id !== activeTurnId);
  if (!completed.length) return messages;

  const pairs = matchTurnsToAssistants(messages, completed);
  if (!pairs.length) return messages;

  const next = [...messages];
  // The reader opens at the latest turn. Keep a single history request in flight.
  pairs.sort((a, b) => b.messageIndex - a.messageIndex);
  for (const { turnId, messageIndex } of pairs) {
    if (signal?.aborted) break;
    try {
      const frames = (await fetchTurnEvents(sessionId, threadId, turnId, signal)).map((frame) => ({
        sequence: frame.sequence,
        kind: frame.kind,
        label: frame.label,
        detail: frame.detail,
        interesting: frame.interesting,
        payload: frame.payload,
        card: frame.tool_card
          ? normalizeToolCard(frame.tool_card, frame.kind || "tool_card")
          : normalizeToolCard(
              { ...(frame.payload || {}), ...frame },
              frame.kind || "tool_card",
            ),
      }));
      if (signal?.aborted) break;
      // Rebuild send positions against durable events rather than counting
      // transient thinking placeholders that may differ after refresh.
      const changed: ChatMessage[] = [];
      const consumed = new Set<number>();
      for (let i = 0; i < next.length; i++) {
        const user = next[i];
        if (user.role !== "user" || !user.steering) continue;
        if (user.turnId && user.turnId !== turnId) continue;
        const injectionIndex = frames.findIndex((f, index) => !consumed.has(index) && (f.payload?.source_kind || f.kind) === "user_inject"
          && (user.steeringId && f.payload?.message_id ? user.steeringId === f.payload.message_id : f.payload?.message === (user.steeringText || user.text)));
        const injection = frames[injectionIndex];
        if (injection) consumed.add(injectionIndex);
        if (user.turnId !== turnId && !injection) continue;
        const sequence = user.steeringBoundary?.afterSequence || injection?.sequence;
        if (!sequence) continue;
        const prefix = frames.filter((f) => (f.sequence || 0) <= sequence
          && (f.payload?.source_kind || f.kind) !== "user_inject");
        // A prefix is a new projection, not a continuation of the hydrated
        // assistant. Inheriting replay cursors/revisions would suppress its
        // older events on a second hydration and move steering to the top.
        const before = replayStreamFrames({ id: next[messageIndex].id,
          role: "assistant", text: "", turnId }, prefix);
        next[i] = { ...user, turnId, steering: injection ? "applied" : user.steering || "pending",
          steeringBoundary: { ...captureSteeringBoundary(before), afterSequence: sequence } };
        changed.push(next[i]);
      }
      next[messageIndex] = {
        ...replayStreamFrames(next[messageIndex], frames, next[messageIndex].text),
        turnId,
      };
      onTurnHydrated?.([...changed, next[messageIndex]]);
    } catch {
      /* keep plain-text history if the journal is unavailable */
    }
  }

  return next;
}
