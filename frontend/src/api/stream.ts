import type { ChatStreamBody, StreamHandlers } from "./types";
import { ApiError, redirectToLoginIfNeeded } from "./client";
import { sessionTurnResponse, streamUnifiedSessionEvents } from "./sessionTransport";
import {
  answerRevisionSuffix,
  mergeTerminalNarrative,
  sameNarrativeSnapshot,
} from "../lib/narrativeText";

export const STREAM_STALL_MS = 7000;
export const STREAM_STARTUP_MS = 4000;

interface SseFrame {
  event: string;
  data: Record<string, unknown>;
}

function parseSseChunk(buffer: string): { frames: SseFrame[]; rest: string } {
  const frames: SseFrame[] = [];
  const parts = buffer.split("\n\n");
  const rest = parts.pop() ?? "";
  for (const block of parts) {
    let event = "message";
    const dataLines: string[] = [];
    for (const line of block.split("\n")) {
      if (line.startsWith("event:")) event = line.slice(6).trim();
      else if (line.startsWith("data:")) dataLines.push(line.slice(5).trim());
    }
    if (!dataLines.length) continue;
    try {
      const data = JSON.parse(dataLines.join("\n")) as Record<string, unknown>;
      frames.push({ event, data });
    } catch {
      /* ignore malformed frame */
    }
  }
  return { frames, rest };
}

export { parseSseChunk };

function frameSequence(data: Record<string, unknown>): number {
  const seq = Number(data.sequence);
  return Number.isFinite(seq) && seq > 0 ? seq : 0;
}

/**
 * Thrown when the SSE connection drops mid-stream (network blip, server
 * restart, or a half-open stall) before a `done` frame arrives. Carries the
 * `turnId` and last journal sequence so the caller can reattach without
 * replaying already-rendered events.
 */
export class StreamDroppedError extends Error {
  readonly turnId: string;
  readonly sessionId: string;
  readonly threadId: string;
  readonly lastSequence: number;
  readonly requestId?: string;
  constructor(
    turnId: string,
    sessionId: string,
    threadId: string,
    lastSequence = 0,
    requestId?: string,
  ) {
    super("Stream connection dropped — reattach required");
    this.name = "StreamDroppedError";
    this.turnId = turnId;
    this.sessionId = sessionId;
    this.threadId = threadId;
    this.lastSequence = lastSequence;
    this.requestId = requestId;
  }
}

export async function streamChat(
  body: ChatStreamBody,
  handlers: StreamHandlers,
  {
    signal,
    stallMs = STREAM_STALL_MS,
  }: { signal?: AbortSignal; stallMs?: number } = {},
): Promise<Record<string, unknown>> {
  const request = {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      Accept: "text/event-stream",
      "Accept-Encoding": "identity",
    },
    body: JSON.stringify({ ...body, event_stream: "session" }),
    signal,
  } satisfies RequestInit;
  const fetchStream = async (): Promise<Response> => {
    const controller = new AbortController();
    let headerTimeout = false;
    const onAbort = () => controller.abort(signal?.reason);
    if (signal?.aborted) onAbort();
    else signal?.addEventListener("abort", onAbort, { once: true });
    // Start the watchdog before fetch: a buffering tunnel may withhold the
    // headers as well as the body while the backend is already executing.
    const timer = setTimeout(() => {
      headerTimeout = true;
      controller.abort();
    }, STREAM_STARTUP_MS);
    try {
      return await fetch("/api/chat/stream", {
        ...request,
        signal: controller.signal,
      });
    } catch (err) {
      if (headerTimeout && !signal?.aborted) {
        // Recover the accepted turn through its session. Retrying the POST
        // here could execute the user's request a second time.
        throw new StreamDroppedError(
          "pending",
          body.session_id,
          body.thread_id,
          0,
          body.client_request_id,
        );
      }
      throw err;
    } finally {
      clearTimeout(timer);
      signal?.removeEventListener("abort", onAbort);
    }
  };
  let res: Response;
  try {
    res = await fetchStream();
  } catch (firstError) {
    // A failed fetch can occur after the server accepted the POST. Recover
    // read-only instead of submitting the same user request a second time.
    if (
      firstError instanceof StreamDroppedError ||
      signal?.aborted ||
      (firstError instanceof Error && firstError.name === "AbortError")
    ) {
      throw firstError;
    }
    throw new StreamDroppedError(
      "pending",
      body.session_id,
      body.thread_id,
      0,
      body.client_request_id,
    );
  }
  const ctype = res.headers.get("content-type") || "";
  // Some proxies (Vite) may strip Content-Type while still streaming SSE.
  // Accept 200 + readable body when the type is missing or is event-stream.
  const looksLikeSse =
    ctype.includes("text/event-stream") ||
    ctype.includes("text/plain") ||
    ctype === "";
  if (!res.ok && ctype.includes("application/json") && body.client_request_id) {
    const rejection = await res.clone().json().catch(() => null);
    if (rejection?.admission === "rejected" && rejection.request_id === body.client_request_id
        && typeof rejection.error === "string") {
      void res.body?.cancel().catch(() => {});
      throw new ApiError(rejection.error, res.status, rejection);
    }
  }
  if (
    res.status >= 500 ||
    (res.ok && (!res.body || (!looksLikeSse && !ctype.includes("json"))))
  ) {
    // A proxy failure can happen after dispatch. Recover by receipt instead
    // of declaring rejection or submitting a second task.
    void res.body?.cancel().catch(() => {});
    throw new StreamDroppedError(
      "pending",
      body.session_id,
      body.thread_id,
      0,
      body.client_request_id,
    );
  }
  if (!res.ok || !res.body || (!looksLikeSse && !ctype.includes("json"))) {
    const errBody = (await res.json().catch(() => ({}))) as { error?: string };
    redirectToLoginIfNeeded(res.status, errBody);
    throw new ApiError(
      errBody.error || `Stream failed (${res.status})`,
      res.status,
      errBody,
    );
  }
  // Native admission returns identity only; all following updates use the shared session stream.
  if (ctype.includes("application/json") && !ctype.includes("event-stream")) {
    const reply = (await res.json().catch(() => ({}))) as Record<
      string,
      unknown
    >;
    if (
      reply.event_stream === "session" &&
      reply.session_id === body.session_id &&
      typeof reply.turn_id === "string" &&
      Number.isSafeInteger(reply.after_sequence)
    ) {
      res = sessionTurnResponse(reply, handlers, signal);
    } else
      throw new Error(
        String(reply.error || reply.message || `Stream failed (${res.status})`),
      );
  }

  const reader = res.body!.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  let donePayload: Record<string, unknown> | null = null;
  let lastError: string | null = null;
  let assembled = "";
  let sourceNarrativeCursor = 0;
  let answerRevisionReplacement: string | undefined;
  let sawFrame = false;
  let lastStatus = "";
  // Track the backend turn id / session / thread so we can reattach on drop.
  let turnId = "";
  let streamSessionId = "";
  let streamThreadId = "";
  let lastSequence = 0;
  let lastJournalSequence = 0;
  let lastFrameAt = 0;
  let turnStarted = false;

  // Proxy-buffering detection: if no SSE frame arrives within this window,
  // the connection is likely being buffered by a reverse proxy (Cloudspaces,
  // nginx, etc.). Cancel the reader so the loop exits and we throw
  // StreamDroppedError — the caller then falls back to polling reattach.
  let startupTimer: ReturnType<typeof setTimeout> | null = setTimeout(() => {
    startupTimer = null;
    if (!sawFrame) {
      reader.cancel().catch(() => {});
    }
  }, STREAM_STARTUP_MS);

  // Preparation can take longer than a normal heartbeat gap. Once the turn
  // starts, the server heartbeats every second and the shorter stall limit
  // can safely recover a half-open connection.
  let stallTimer: ReturnType<typeof setInterval> | null = null;
  const armStallWatchdog = () => {
    if (stallTimer || stallMs <= 0) return;
    stallTimer = setInterval(
      () => {
        const limit = turnStarted ? stallMs : Math.max(stallMs, 60000);
        if (!lastFrameAt || Date.now() - lastFrameAt < limit) return;
        reader.cancel().catch(() => {});
      },
      Math.min(1000, Math.max(50, stallMs / 2)),
    );
  };

  const handleFrame = (frame: SseFrame) => {
    const tid = String(frame.data.turn_id || "").trim();
    if (tid && turnId && tid !== turnId) return;
    const journalFrame = ["event", "reasoning", "step"].includes(frame.event);
    const seq = frameSequence(frame.data);
    if (journalFrame && seq && seq <= lastJournalSequence) return;
    if (journalFrame && seq) lastJournalSequence = seq;
    sawFrame = true;
    lastFrameAt = Date.now();
    if (
      frame.event !== "status" ||
      !["starting", "received"].includes(String(frame.data.phase || ""))
    ) {
      turnStarted = true;
    }
    if (seq > lastSequence) lastSequence = seq;
    // Clear startup timeout — stream is working. Heartbeats keep the stall
    // watchdog reset even while the model/tool is silent.
    if (startupTimer) {
      clearTimeout(startupTimer);
      startupTimer = null;
    }
    armStallWatchdog();
    // Capture routing metadata for reattach-on-drop recovery.
    if (tid) turnId = tid;
    const sid = String(frame.data.session_id || "").trim();
    if (sid) streamSessionId = sid;
    const thid = String(frame.data.thread_id || "").trim();
    if (thid) streamThreadId = thid;
    if (frame.event === "status") {
      lastStatus = String(frame.data.label || "Working…");
      handlers.onStatus?.(lastStatus, frame.data);
    } else if (
      frame.event === "event" ||
      frame.event === "reasoning" ||
      frame.event === "step"
    ) {
      // Durable narrative events also carry text from non-streaming steps.
      // Align the delta accumulator so later tokens cannot replace earlier
      // narrative blocks and move all narration below the tools.
      const payload = frame.data.payload as Record<string, unknown> | undefined;
      if (
        String(payload?.source_kind || frame.data.kind || "") ===
        "answer_revision"
      ) {
        const original = String(payload?.original || "");
        const offset =
          typeof payload?.offset === "number" ? payload.offset : undefined;
        const suffix = answerRevisionSuffix(assembled, original, offset);
        if (suffix !== null) {
          sourceNarrativeCursor = Math.max(
            sourceNarrativeCursor,
            assembled.length,
            offset === undefined ? 0 : offset + original.length,
          );
          assembled =
            assembled.slice(0, assembled.length - suffix) +
            String(payload?.replacement || "");
          answerRevisionReplacement = String(payload?.replacement || "");
        }
      }
      if (
        String(payload?.source_kind || frame.data.kind || "") ===
        "assistant_narrative"
      ) {
        const text = String(payload?.text || "");
        const offset = Number(payload?.offset || 0);
        const cursor = Math.max(sourceNarrativeCursor, assembled.length);
        if (text && cursor < offset + text.length) {
          assembled += text.slice(Math.max(0, cursor - offset));
          sourceNarrativeCursor = offset + text.length;
        }
      }
      handlers.onEvent?.(
        frame.event === "reasoning"
          ? { kind: "reasoning", ...frame.data }
          : frame.event === "step"
            ? { kind: "step", ...frame.data }
            : frame.data,
      );
    } else if (frame.event === "tool_card") {
      handlers.onToolCard?.(frame.data);
      handlers.onEvent?.({ kind: "tool_card", ...frame.data });
    } else if (
      frame.event === "computer_frame" ||
      frame.event === "artifact_thumb"
    ) {
      handlers.onComputerFrame?.(frame.data, frame.event);
      handlers.onEvent?.({ kind: frame.event, ...frame.data });
    } else if (frame.event === "delta") {
      if (frame.data.reset === true) {
        assembled = "";
        sourceNarrativeCursor = 0;
        answerRevisionReplacement = undefined;
      }
      const delta = String(frame.data.text || "");
      assembled += delta;
      sourceNarrativeCursor += delta.length;
      handlers.onDelta?.(assembled);
    } else if (frame.event === "message") {
      const full = String(frame.data.text || "");
      // An explicit revision owns its answer even when it is short or masked.
      // Compare that answer alone; preceding progress is a separate segment.
      const replacement = answerRevisionReplacement;
      const ownsRevision = replacement && assembled.endsWith(replacement);
      const merged =
        ownsRevision && sameNarrativeSnapshot(replacement, full)
          ? assembled
          : ownsRevision && full.startsWith(replacement)
            ? assembled.slice(0, -replacement.length) + full
            : mergeTerminalNarrative(assembled, full);
      if (merged !== assembled) {
        assembled = merged;
        handlers.onMessage?.(assembled, Boolean(frame.data.partial));
      }
    } else if (frame.event === "done") {
      donePayload = frame.data;
      handlers.onDone?.(frame.data);
    } else if (frame.event === "error") {
      lastError = String(frame.data.error || "stream error");
      handlers.onError?.(lastError);
    }
  };

  const dropError = () =>
    new StreamDroppedError(
      turnId || "pending",
      streamSessionId || body.session_id,
      streamThreadId || body.thread_id,
      lastSequence,
      body.client_request_id,
    );

  const onAbort = () => {
    reader.cancel().catch(() => {});
  };
  if (signal) {
    if (signal.aborted) onAbort();
    else signal.addEventListener("abort", onAbort, { once: true });
  }

  let transportDropped = false;
  try {
    while (true) {
      let chunk: ReadableStreamReadResult<Uint8Array>;
      try {
        chunk = await reader.read();
      } catch (err) {
        if (signal?.aborted) break;
        // Even a read failure before the first frame can follow admission.
        transportDropped = true;
        break;
      }
      if (chunk.done) break;
      buffer += decoder.decode(chunk.value, { stream: true });
      const parsed = parseSseChunk(buffer);
      buffer = parsed.rest;
      for (const frame of parsed.frames) handleFrame(frame);
      // A terminal frame settles the turn even if a proxy leaves HTTP open.
      if (donePayload || lastError) break;
    }
    // Flush any trailing partial bytes the TextDecoder still holds, so a
    // multi-byte char split at the final chunk boundary isn't dropped.
    if (!transportDropped && !signal?.aborted) {
      buffer += decoder.decode();
      for (const frame of parseSseChunk(buffer).frames) handleFrame(frame);
    }
  } finally {
    // Release the response body stream even if a handler throws or the
    // fetch is aborted, so it isn't left locked until GC.
    reader.cancel().catch(() => {});
    if (startupTimer) {
      clearTimeout(startupTimer);
      startupTimer = null;
    }
    if (stallTimer) {
      clearInterval(stallTimer);
      stallTimer = null;
    }
    signal?.removeEventListener("abort", onAbort);
  }

  // An explicit error frame always surfaces — don't let a later `done`
  // silently swallow it.
  if (lastError)
    throw turnId ? new Error(lastError) : new ApiError(lastError, 400);
  if (!donePayload) {
    if (signal?.aborted) {
      return { status: "cancelled", message: assembled };
    }
    // Proxy timeout, stall, or a closed socket — the backend turn is still
    // running. Never treat leftover deltas as the final answer (that left a
    // dead "Error" bubble and then dumped every tool chip on reconnect).
    throw dropError();
  }
  return donePayload;
}

/** Read-only reattachment: never submits the user's task again. */
async function streamLegacySessionEvents(
  url: string,
  onUpdate: (packet: Record<string, unknown>) => void,
  signal: AbortSignal,
): Promise<void> {
  const controller = new AbortController();
  const abort = () => controller.abort(signal.reason);
  if (signal.aborted) abort();
  else signal.addEventListener("abort", abort, { once: true });
  let lastFrameAt = 0;
  const startup = setTimeout(() => controller.abort(), STREAM_STARTUP_MS);
  const watchdog = setInterval(() => {
    if (lastFrameAt && Date.now() - lastFrameAt >= STREAM_STALL_MS)
      controller.abort();
  }, 1000);
  let reader: ReadableStreamDefaultReader<Uint8Array> | undefined;
  try {
    const res = await fetch(url, {
      signal: controller.signal,
      headers: { Accept: "text/event-stream" },
    });
    if (
      !res.ok ||
      !res.body ||
      !res.headers.get("content-type")?.includes("text/event-stream")
    ) {
      throw new Error("Live session stream unavailable");
    }
    reader = res.body.getReader();
    const decoder = new TextDecoder();
    let buffer = "";
    while (true) {
      const next = await reader.read();
      if (next.done) throw new Error("Live session stream interrupted");
      buffer += decoder.decode(next.value, { stream: true });
      const parsed = parseSseChunk(buffer);
      buffer = parsed.rest;
      for (const frame of parsed.frames) {
        clearTimeout(startup);
        lastFrameAt = Date.now();
        if (frame.event === "update") onUpdate(frame.data);
        if (frame.event === "done") return;
      }
    }
  } finally {
    clearTimeout(startup);
    clearInterval(watchdog);
    signal.removeEventListener("abort", abort);
    await reader?.cancel().catch(() => {});
    reader?.releaseLock();
  }
}

/** Read-only reattachment shares native updates, with legacy transport compatibility. */
export function streamSessionEvents(url:string,onUpdate:(packet:Record<string,unknown>)=>void,signal:AbortSignal):Promise<void> {
  return streamUnifiedSessionEvents(url,onUpdate,signal,streamLegacySessionEvents);
}
