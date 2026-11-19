import type { StreamHandlers } from "./types";
import {
  subscribeSessionEvents,
  type SessionFrame,
} from "../lib/sessionEvents";

export function sessionTurnResponse(
  handshake: Record<string, unknown>,
  handlers: StreamHandlers,
  signal?: AbortSignal,
): Response {
  let stop: () => void = () => {};
  let heartbeat: ReturnType<typeof setInterval> | undefined;
  let closed = false;
  let abort: () => void = () => {};
  const cleanup = () => {
    clearInterval(heartbeat);
    stop();
    signal?.removeEventListener("abort", abort);
  };
  const encoder = new TextEncoder();
  const session = String(handshake.session_id),
    turn = String(handshake.turn_id);
  const data = (value: Record<string, unknown>) => ({
    ...value,
    session_id: session,
    thread_id: handshake.thread_id,
    turn_id: turn,
  });
  const stream = new ReadableStream<Uint8Array>({
    start(controller) {
      const write = (event: string, value: Record<string, unknown>) => {
        if (!closed)
          controller.enqueue(
            encoder.encode(
              `event: ${event}\ndata: ${JSON.stringify(data(value))}\n\n`,
            ),
          );
      };
      abort = () => {
        if (closed) return;
        closed = true;
        cleanup();
        controller.close();
      };
      if (signal?.aborted) {
        abort();
        return;
      }
      signal?.addEventListener("abort", abort, { once: true });
      // The admission response is authoritative even if the journal stream
      // has not yet delivered its first status frame.
      write("status", { label: "Request received · preparing…", phase: "received", request_id: handshake.request_id });
      write("heartbeat", {});
      stop = subscribeSessionEvents(
        session,
        {
          onEvent: (frame: SessionFrame) => {
            if (
              frame.turn_id !== turn ||
              frame.kind === "worker" ||
              frame.kind === "worker_activity"
            )
              return;
            const payload =
              frame.payload && typeof frame.payload === "object"
                ? (frame.payload as Record<string, unknown>)
                : {};
            if (frame.kind === "tool_card")
              write("tool_card", {
                ...(frame.tool_card as Record<string, unknown>),
                sequence: frame.sequence,
              });
            else if (frame.kind === "done") {
              write("done", { ...payload, sequence: frame.sequence });
              abort();
            } else if (frame.kind === "status")
              write("status", {
                ...payload,
                label: frame.label || payload.label,
                sequence: frame.sequence,
              });
            else
              write(
                [
                  "reasoning",
                  "step",
                  "delta",
                  "computer_frame",
                  "artifact_thumb",
                ].includes(String(frame.kind))
                  ? String(frame.kind)
                  : "event",
                frame,
              );
          },
          onError: (message) => {
            if (message && !closed)
              handlers.onStatus?.("Reconnecting…", data({ message }));
          },
          onUnsupported: () => {
            if (!closed) {
              closed = true;
              cleanup();
              controller.error(new Error("Session event stream unavailable"));
            }
          },
        },
        { afterSequence: Number(handshake.after_sequence), turnId: turn },
      );
      if (closed) cleanup();
      else heartbeat = setInterval(() => write("heartbeat", {}), 1000);
    },
    cancel() {
      closed = true;
      cleanup();
    },
  });
  return new Response(stream, {
    headers: { "Content-Type": "text/event-stream" },
  });
}

/** Existing reattach consumers join the same transport as tools and workers. */
export async function streamUnifiedSessionEvents(
  url: string,
  onUpdate: (packet: Record<string, unknown>) => void,
  signal: AbortSignal,
  legacyStream: (
    url: string,
    onUpdate: (packet: Record<string, unknown>) => void,
    signal: AbortSignal,
  ) => Promise<void>,
): Promise<void> {
  const parsed = new URL(url, window.location.origin);
  const match = parsed.pathname.match(
    /^\/api\/sessions\/([^/]+)\/events\/stream$/,
  );
  const turn = parsed.searchParams.get("turn_id");
  if (!match || !turn) return legacyStream(url, onUpdate, signal);
  const session = decodeURIComponent(match[1]);
  const audience = parsed.searchParams.get("parent_session_id") || session;
  const jobId = parsed.searchParams.get("job_id");
  const after = Number(parsed.searchParams.get("after") || parsed.searchParams.get("after_sequence") || 0);
  return new Promise<void>((resolve, reject) => {
    let stop: () => void = () => {};
    let closed = false,
      ready = false,
      legacy = false;
    let pending: SessionFrame[] = [];
    let live: Record<string, unknown> = {
      active_turn: { turn_id: turn, status: "running" },
    };
    const cleanup = () => {
      stop();
      signal.removeEventListener("abort", abort);
    };
    const finish = (error?: unknown) => {
      if (closed) return;
      closed = true;
      cleanup();
      if (error) reject(error);
      else resolve();
    };
    const abort = () => finish();
    const deliver = (frame: SessionFrame) => {
      if (
        closed ||
        frame.turn_id !== turn ||
        frame.kind === "worker" ||
        frame.kind === "worker_activity"
      )
        return;
      if (!ready) {
        pending.push(frame);
        return;
      }
      if (frame.kind === "done") {
        const payload = frame.payload as Record<string, unknown> | undefined;
        live = {
          ...live,
          active_turn: null,
          requested_turn: {
            turn_id: turn,
            status: payload?.status || "completed",
            phase: "completed",
          },
        };
      }
      onUpdate({ events: [frame], live });
      if (frame.kind === "done") finish();
    };
    if (signal.aborted) {
      finish();
      return;
    }
    signal.addEventListener("abort", abort, { once: true });
    stop = subscribeSessionEvents(
      audience,
      {
        onEvent: (frame) => {
          if (audience === session) {
            deliver(frame);
            return;
          }
          if (
            frame.kind !== "worker_activity" ||
            frame.job_id !== jobId ||
            frame.source_session_id !== session
          )
            return;
          const activity = frame.activity as SessionFrame | undefined;
          if (activity)
            deliver({
              ...activity,
              sequence: frame.sequence,
              turn_id: String(frame.source_turn_id || activity.turn_id || turn),
            });
        },
        onUnsupported: () => {
          if (closed || legacy) return;
          legacy = true;
          stop();
          void legacyStream(url, onUpdate, signal).then(
            () => finish(),
            (error) => finish(error),
          );
        },
      },
      { afterSequence: after, turnId: audience === session ? turn : "" },
    );
    if (closed) {
      cleanup();
      return;
    }
    // One read-only state snapshot restores approval and narrative metadata;
    // subsequent changes come from the shared stream, never a polling loop.
    void (async () => {
      if (legacy) return;
      const response = await fetch(
        `/api/sessions/${encodeURIComponent(session)}/events?turn_id=${encodeURIComponent(turn)}&after_sequence=${after}&include_state=1`,
        { signal, headers: { Accept: "application/json" } },
      );
      if (!response.ok) throw new Error("Recorded session state unavailable");
      const packet = (await response.json()) as Record<string, unknown>;
      if (closed || legacy) return;
      live = (packet.live as Record<string, unknown>) || live;
      onUpdate(packet);
      ready = true;
      const events = [...pending].sort(
        (a, b) => Number(a.sequence) - Number(b.sequence),
      );
      pending = [];
      for (const event of events) deliver(event);
      if (
        !closed &&
        Array.isArray(packet.events) &&
        packet.events.some(
          (frame: SessionFrame) =>
            frame.turn_id === turn && frame.kind === "done",
        )
      )
        finish();
    })().catch((error) => {
      if (!legacy) finish(error);
    });
  });
}
