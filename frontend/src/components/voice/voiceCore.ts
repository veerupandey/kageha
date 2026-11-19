import type { GeminiLivePhase } from "../../lib/geminiLiveClient";
import type { GeminiLiveStatus } from "../useGeminiLiveVoice";
import {
  CAPTION_RETENTION_LIMIT,
  type CaptionEntry,
  type OrbVisualState,
  type VoiceSseEvent,
} from "./voiceTypes";

/**
 * Pure, total functions that form the testable core of the immersive voice UI.
 * None of these read the DOM, React state, or the network; they operate purely
 * on their arguments so they can be unit- and property-tested in isolation.
 */

/**
 * Clamp any input to the range [0, 1].
 * Missing / NaN / non-finite / non-numeric inputs map to 0. (Req 4.2.)
 */
export function clamp01(value: unknown): number {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return 0;
  }
  if (value <= 0) return 0;
  if (value >= 1) return 1;
  return value;
}

/**
 * Map (status, phase) -> orb visual state.
 * Precedence: error > connecting > phase. Every status x phase pair maps to
 * exactly one state. (Req 4.3, 4.4, 4.5, 4.7, 4.8.)
 */
export function orbStateFor(
  status: GeminiLiveStatus,
  phase: GeminiLivePhase,
): OrbVisualState {
  if (status === "error") return "error";
  if (status === "connecting") return "connecting";
  switch (phase) {
    case "listening":
      return "listening";
    case "thinking":
      return "thinking";
    case "speaking":
      return "speaking";
    case "idle":
    default:
      return "idle";
  }
}

/**
 * Append a transcript event to a caption list.
 * - Drops empty / whitespace-only text (returns the list unchanged). (Req 5.3.)
 * - Trims text; the resulting entry is guaranteed non-empty. (Req 5.1, 5.2.)
 * - Preserves chronological order (append at the end). (Req 5.5.)
 * - Bounds the list to the most recent `limit` entries. (Req 5.7.)
 */
export function appendCaption(
  list: CaptionEntry[],
  event: { role: "user" | "assistant"; text: string },
  nextSeq: number,
  limit: number = CAPTION_RETENTION_LIMIT,
): CaptionEntry[] {
  const text = event.text.trim();
  if (text.length === 0) {
    return list;
  }
  const entry: CaptionEntry = {
    id: String(nextSeq),
    role: event.role,
    text,
    seq: nextSeq,
  };
  const appended = [...list, entry];
  if (limit >= 1 && appended.length > limit) {
    return appended.slice(appended.length - limit);
  }
  return appended;
}

/**
 * Human-readable phase status label. Total function. (Req 6.1-6.4.)
 * connecting -> "Connecting"; active + listening/thinking/speaking ->
 * "Listening"/"Thinking"/"Speaking". Always returns a non-empty string.
 */
export function phaseStatusLabel(
  status: GeminiLiveStatus,
  phase: GeminiLivePhase,
): string {
  if (status === "connecting") return "Connecting";
  if (status === "error") return "Error";
  if (status === "active") {
    switch (phase) {
      case "listening":
        return "Listening";
      case "thinking":
        return "Thinking";
      case "speaking":
        return "Speaking";
      case "idle":
      default:
        return "Listening";
    }
  }
  // status === "idle"
  return "Idle";
}

/**
 * Reduce a delegation lifecycle event into the active-delegation set.
 * started -> add call_id; completed/failed/cancelled -> delete (no-op if
 * absent). Returns a new Set; the input is not mutated. (Req 12.1-12.3.)
 */
export function reduceDelegation(
  active: Set<string>,
  event: VoiceSseEvent,
): Set<string> {
  switch (event.kind) {
    case "voice_delegation_started": {
      const next = new Set(active);
      next.add(event.data.call_id);
      return next;
    }
    case "voice_delegation_completed":
    case "voice_delegation_failed":
    case "voice_delegation_cancelled":
    case "voice_delegation_detached": {
      if (!active.has(event.data.call_id)) {
        return active;
      }
      const next = new Set(active);
      next.delete(event.data.call_id);
      return next;
    }
    default:
      return active;
  }
}
