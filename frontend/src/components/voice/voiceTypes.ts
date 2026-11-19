import type { GeminiLivePhase } from "../../lib/geminiLiveClient";
import type { GeminiLiveStatus } from "../useGeminiLiveVoice";

export type { GeminiLivePhase } from "../../lib/geminiLiveClient";
export type { GeminiLiveStatus } from "../useGeminiLiveVoice";

/**
 * A voice event as delivered on the per-session voice-events SSE stream.
 * `kind` is the server event name; `data` mirrors what gemini_live.py emits.
 */
export type VoiceSseEvent =
  | {
      kind: "voice_transcript";
      data: { role: "user" | "assistant"; text: string; session_id: string };
    }
  | {
      kind: "voice_delegation_started";
      data: { call_id: string; task: string; session_id: string };
    }
  | {
      kind: "voice_delegation_completed";
      data: { call_id: string; task: string; result: string; session_id: string };
    }
  | {
      kind: "voice_delegation_failed";
      data: { call_id: string; task: string; error: string; session_id: string };
    }
  | {
      kind: "voice_delegation_cancelled";
      data: { call_id: string; task: string; session_id: string };
    }
  | {
      kind: "voice_delegation_detached";
      data: { call_id: string; task: string; session_id: string };
    }
  | { kind: "voice_interrupted"; data: { session_id: string } }
  | { kind: "voice_turn_complete"; data: { session_id: string } };

/** A rendered caption entry retained in overlay state. */
export interface CaptionEntry {
  /** Stable key (monotonic counter or crypto.randomUUID). */
  id: string;
  role: "user" | "assistant";
  /** Trimmed, guaranteed non-empty. */
  text: string;
  /** Monotonic arrival order for the chronological guarantee. */
  seq: number;
}

/** The discrete visual state the orb renders, derived from status + phase. */
export type OrbVisualState =
  | "error"
  | "connecting"
  | "idle"
  | "listening"
  | "thinking"
  | "speaking";

/** Maximum number of caption entries retained in overlay state. */
export const CAPTION_RETENTION_LIMIT = 50;

// Re-export the aliased types so downstream imports can pull everything from
// this module without also referencing the transport files directly.
export type VoicePhase = GeminiLivePhase;
export type VoiceStatus = GeminiLiveStatus;
