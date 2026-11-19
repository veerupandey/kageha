import { useEffect, useState } from "react";
import type { PendingApproval } from "../../api/types";
import { useAppStore } from "../../store";
import { appendCaption, reduceDelegation } from "./voiceCore";
import {
  CAPTION_RETENTION_LIMIT,
  type CaptionEntry,
  type VoiceSseEvent,
} from "./voiceTypes";

/** Reconnect backoff when the SSE stream drops, mirroring usePeerAlerts. */
const SSE_RETRY_MS = 4000;

export interface UseVoiceTranscript {
  /** Chronological, length <= CAPTION_RETENTION_LIMIT. */
  captions: CaptionEntry[];
  /** call_ids currently "working" (started, not yet terminated). (Req 12.) */
  activeDelegations: ReadonlySet<string>;
  latestWork: { state: "completed" | "failed" | "detached"; text: string } | null;
  progressLabel: string | null;
}

/**
 * Subscribe to the per-session voice-events SSE stream and reduce it into a
 * bounded, chronological caption list plus a set of active delegation ids.
 *
 * Mirrors the EventSource pattern in `lib/usePeerAlerts.ts`: open/error/retry
 * handling, cleanup on unmount and on `sessionId` change. When `sessionId` is
 * null the hook is a no-op (no stream is opened). (Req 5.1-5.5, 5.7, 12.1-12.3.)
 *
 * Transcript text is sourced exclusively from this SSE stream, never from the
 * voice WebSocket. (Req 5.4.)
 */
export function useVoiceTranscript(
  sessionId: string | null,
): UseVoiceTranscript {
  const [captions, setCaptions] = useState<CaptionEntry[]>([]);
  const [activeDelegations, setActiveDelegations] = useState<Set<string>>(
    () => new Set<string>(),
  );
  const [latestWork, setLatestWork] = useState<UseVoiceTranscript["latestWork"]>(null);
  const [progressLabel, setProgressLabel] = useState<string | null>(null);

  useEffect(() => {
    // No thread/session -> nothing to subscribe to. Reset any prior state so a
    // re-opened overlay for a different (or no) session starts clean.
    setCaptions([]);
    setActiveDelegations(new Set<string>());
    setLatestWork(null);
    setProgressLabel(null);

    if (sessionId === null || typeof EventSource === "undefined") {
      return;
    }

    let cancelled = false;
    let source: EventSource | null = null;
    let retryTimer = 0;
    // Monotonic arrival counter guaranteeing chronological ordering across the
    // lifetime of this subscription, independent of React batching. (Req 5.5.)
    let seq = 0;

    const url = `/api/sessions/${encodeURIComponent(sessionId)}/voice/events`;

    const onTranscript = (raw: MessageEvent) => {
      let event: { role?: unknown; text?: unknown } | null = null;
      try {
        event = JSON.parse(raw.data);
      } catch {
        return;
      }
      if (event === null) return;
      const role = event.role;
      const text = typeof event.text === "string" ? event.text : "";
      if (role !== "user" && role !== "assistant") return;
      const nextSeq = seq++;
      setCaptions((list) =>
        appendCaption(list, { role, text }, nextSeq, CAPTION_RETENTION_LIMIT),
      );
    };

    const onDelegation = (kind: VoiceSseEvent["kind"]) => (raw: MessageEvent) => {
      let data: { call_id?: string; task?: string; result?: string; error?: string } | null = null;
      try {
        data = JSON.parse(raw.data);
      } catch {
        return;
      }
      if (!data || typeof data.call_id !== "string") return;
      const event = { kind, data } as VoiceSseEvent;
      setActiveDelegations((active) => reduceDelegation(active, event));
      if (kind === "voice_delegation_started") setLatestWork(null);
      if (kind === "voice_delegation_started") setProgressLabel("Starting work…");
      if (kind === "voice_delegation_completed") {
        setLatestWork({ state: "completed", text: String(data.result || "Done.").slice(0, 1600) });
      } else if (kind === "voice_delegation_failed") {
        setLatestWork({ state: "failed", text: String(data.error || "The task failed.").slice(0, 800) });
      } else if (kind === "voice_delegation_detached") {
        setLatestWork({ state: "detached", text: "Work continues in this thread." });
      }
      if (kind !== "voice_delegation_started") setProgressLabel(null);
    };

    const onProgress = (raw: MessageEvent) => {
      let data: { label?: string } | null = null;
      try { data = JSON.parse(raw.data); } catch { return; }
      if (typeof data?.label === "string") setProgressLabel(data.label.slice(0, 160));
    };

    const onApprovalRequired = (raw: MessageEvent) => {
      let data: PendingApproval | null = null;
      try { data = JSON.parse(raw.data) as PendingApproval; } catch { return; }
      if (!data?.approval_id) return;
      if (useAppStore.getState().sessionId !== sessionId) return;
      useAppStore.setState({ pendingApproval: { ...data, sessionId } });
    };
    const onApprovalResolved = (raw: MessageEvent) => {
      let data: { approval_id?: string } | null = null;
      try { data = JSON.parse(raw.data); } catch { return; }
      const pending = useAppStore.getState().pendingApproval;
      if (pending?.approval_id === data?.approval_id) {
        useAppStore.setState({ pendingApproval: null });
      }
    };

    const startedListener = onDelegation("voice_delegation_started");
    const completedListener = onDelegation("voice_delegation_completed");
    const failedListener = onDelegation("voice_delegation_failed");
    const cancelledListener = onDelegation("voice_delegation_cancelled");
    const detachedListener = onDelegation("voice_delegation_detached");

    const connect = () => {
      if (cancelled || typeof EventSource === "undefined") return;
      source = new EventSource(url);
      source.addEventListener("voice_transcript", onTranscript);
      source.addEventListener("voice_delegation_started", startedListener);
      source.addEventListener("voice_delegation_completed", completedListener);
      source.addEventListener("voice_delegation_failed", failedListener);
      source.addEventListener("voice_delegation_cancelled", cancelledListener);
      source.addEventListener("voice_delegation_detached", detachedListener);
      source.addEventListener("voice_progress", onProgress);
      source.addEventListener("voice_approval_required", onApprovalRequired);
      source.addEventListener("voice_approval_resolved", onApprovalResolved);
      source.addEventListener("error", () => {
        source?.close();
        source = null;
        if (!cancelled) retryTimer = window.setTimeout(connect, SSE_RETRY_MS);
      });
    };

    connect();

    return () => {
      cancelled = true;
      window.clearTimeout(retryTimer);
      source?.close();
      source = null;
    };
  }, [sessionId]);

  return { captions, activeDelegations, latestWork, progressLabel };
}
