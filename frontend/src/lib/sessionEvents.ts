import { ApiError, redirectToLoginIfNeeded } from "../api/client";
import type { DurableJob } from "../components/DurableJobs";

export type SessionFrame = Record<string, unknown> & {
  sequence?: number;
  turn_id?: string;
  kind?: string;
};
export type SessionSnapshot = {
  sequence: number;
  session_id: string;
  workers: DurableJob[];
};
type Receiver = {
  onEvent?: (event: SessionFrame) => void;
  onSnapshot?: (snapshot: SessionSnapshot) => void;
  onError?: (message: string) => void;
  onUnsupported?: () => void;
};
type Subscriber = {
  receiver: Receiver;
  after: number;
  ready: boolean;
  pending: SessionFrame[];
  pendingThrough: number;
  turn: string;
  stopped: boolean;
  history?: AbortController;
  retry?: ReturnType<typeof setTimeout>;
};
const connections = new Map<string, SessionConnection>();
const online = () => navigator.onLine !== false;
const seq = (event: SessionFrame) =>
  Number.isSafeInteger(event.sequence) && Number(event.sequence) > 0
    ? Number(event.sequence)
    : 0;
const MAX_BUFFER = 16 * 1024 * 1024;

/** One read-only, resumable transport shared by conversation and worker views. */
class SessionConnection {
  private subscribers = new Set<Subscriber>();
  private cursor: number;
  private floor: number;
  private cache: SessionFrame[] = [];
  private cacheBytes = 0;
  private snapshot?: SessionSnapshot;
  get nativeAvailable() {
    return !this.unsupported && Boolean(this.snapshot);
  }
  private request?: AbortController;
  private reader?: ReadableStreamDefaultReader<Uint8Array>;
  private timer?: ReturnType<typeof setTimeout>;
  private failures = 0;
  private unsupported = false;
  readonly session: string;
  constructor(session: string, after: number) {
    this.session = session;
    this.cursor = after;
    this.floor = after;
    window.addEventListener("online", this.wake);
    window.addEventListener("offline", this.offline);
    window.addEventListener("focus", this.wake);
    document.addEventListener("visibilitychange", this.visible);
  }
  private notifyError(message: string) {
    for (const subscriber of this.subscribers) {
      try {
        subscriber.receiver.onError?.(message);
      } catch {
        /* isolate local projections */
      }
    }
  }
  private offline = () => {
    clearTimeout(this.timer);
    this.timer = undefined;
    this.request?.abort();
    this.request = undefined;
    void this.reader?.cancel().catch(() => {});
    this.reader = undefined;
    this.notifyError("You’re offline. Updates will resume when connected.");
  };
  private visible = () => { if (!document.hidden) this.wake(); };
  private wake = () => {
    if (this.request || this.unsupported || !online() || !this.subscribers.size)
      return;
    clearTimeout(this.timer);
    this.timer = undefined;
    void this.connect();
  };
  private deliver(subscriber: Subscriber, event: SessionFrame) {
    if (
      subscriber.stopped ||
      seq(event) <= subscriber.after ||
      (subscriber.turn && event.turn_id !== subscriber.turn)
    )
      return;
    if (!subscriber.ready) {
      subscriber.pendingThrough = Math.max(
        subscriber.pendingThrough,
        seq(event),
      );
      if (subscriber.pending.length < 2000) subscriber.pending.push(event);
      // History hydration reads the journal, so a capped pending queue does not
      // lose the source record. Restart hydration from its original cursor.
      return;
    }
    try {
      subscriber.receiver.onEvent?.(event);
      subscriber.after = seq(event);
    } catch {
      subscriber.receiver.onError?.("A local update could not be displayed.");
    }
  }
  private event(event: SessionFrame) {
    const sequence = seq(event);
    if (!sequence || sequence <= this.cursor) return;
    this.cursor = sequence;
    this.cache.push(event);
    this.cacheBytes += JSON.stringify(event).length;
    while (this.cache.length > 2000 || this.cacheBytes > 8 * 1024 * 1024) {
      const removed = this.cache.shift()!;
      this.cacheBytes -= JSON.stringify(removed).length;
      this.floor = seq(removed);
    }
    for (const subscriber of this.subscribers) this.deliver(subscriber, event);
  }
  private frame(event: string, data: Record<string, unknown>) {
    if (event === "snapshot") {
      if (
        data.session_id !== this.session ||
        !Number.isSafeInteger(data.sequence) ||
        !Array.isArray(data.workers)
      ) {
        throw new Error("Session snapshot was incomplete");
      }
      this.snapshot = data as SessionSnapshot;
      for (const subscriber of this.subscribers) {
        try {
          subscriber.receiver.onSnapshot?.(this.snapshot);
        } catch {
          /* isolate local projections */
        }
      }
      // The snapshot is newer than replay, but replay may contain a late chat
      // subscriber's first tool events. Only event records advance the cursor.
    } else if (event === "update") {
      if (!Array.isArray(data.events))
        throw new Error("Session update was incomplete");
      for (const item of data.events) {
        if (!item || typeof item !== "object" || !seq(item))
          throw new Error("Session event has no durable sequence");
        this.event(item as SessionFrame);
      }
    }
  }
  private async connect() {
    if (this.request || !this.subscribers.size || !online() || this.unsupported)
      return;
    const request = new AbortController();
    this.request = request;
    let reader: ReadableStreamDefaultReader<Uint8Array> | undefined;
    let lastChunk = Date.now();
    const watchdog = setInterval(() => {
      if (Date.now() - lastChunk > 15000 && !request.signal.aborted) {
        this.failures++;
        this.notifyError("Live session connection stalled; reconnecting…");
        request.abort();
        void reader?.cancel().catch(() => {});
        if (this.request === request) {
          this.request = undefined;
          this.timer = setTimeout(
            this.wake,
            Math.min(30000, 1000 * 2 ** Math.min(this.failures - 1, 5)),
          );
        }
      }
    }, 1000);
    try {
      const response = await fetch(
        `/api/sessions/${encodeURIComponent(this.session)}/live/stream?after_sequence=${this.cursor}`,
        {
          signal: request.signal,
          headers: { Accept: "text/event-stream" },
        },
      );
      if (request.signal.aborted) return;
      if (response.status === 404 || response.status === 501) {
        this.unsupported = true;
        void response.body?.cancel().catch(() => {});
        for (const subscriber of this.subscribers)
          subscriber.receiver.onUnsupported?.();
        return;
      }
      if (
        !response.ok ||
        !response.body ||
        !response.headers.get("content-type")?.includes("text/event-stream")
      ) {
        const data = await response.json().catch(() => ({}));
        redirectToLoginIfNeeded(response.status, data);
        throw new ApiError(
          data.error || "Live session stream unavailable",
          response.status,
        );
      }
      reader = response.body.getReader();
      this.reader = reader;
      const decoder = new TextDecoder();
      let buffer = "";
      while (!request.signal.aborted) {
        const next = await reader.read();
        if (next.done)
          throw new Error("Live session stream interrupted; reconnecting…");
        if (request.signal.aborted) return;
        lastChunk = Date.now();
        buffer = (
          buffer + decoder.decode(next.value, { stream: true })
        ).replace(/\r\n/g, "\n");
        if (buffer.length > MAX_BUFFER)
          throw new Error("Live session event exceeded its size limit");
        let end: number;
        while ((end = buffer.indexOf("\n\n")) !== -1) {
          const block = buffer.slice(0, end);
          buffer = buffer.slice(end + 2);
          let event = "message";
          const data: string[] = [];
          for (const line of block.split("\n")) {
            if (line.startsWith("event:")) event = line.slice(6).trim();
            else if (line.startsWith("data:"))
              data.push(line.slice(5).trimStart());
          }
          if (!data.length) continue; // Comment heartbeats still refresh watchdog.
          this.frame(event, JSON.parse(data.join("\n")));
          this.failures = 0;
          this.notifyError("");
        }
      }
    } catch (error) {
      if (!request.signal.aborted) {
        this.failures++;
        this.notifyError(
          error instanceof Error ? error.message : "Live session unavailable",
        );
      }
    } finally {
      clearInterval(watchdog);
      void reader?.cancel().catch(() => {});
      if (this.reader === reader) this.reader = undefined;
      const current = this.request === request;
      if (current) this.request = undefined;
      if (current && this.subscribers.size && online() && !this.unsupported) {
        const delay = Math.min(
          30000,
          1000 * 2 ** Math.min(Math.max(0, this.failures - 1), 5),
        );
        this.timer = setTimeout(() => {
          this.timer = undefined;
          this.wake();
        }, delay);
      }
    }
  }
  private async backfill(subscriber: Subscriber, turn: string) {
    const history = new AbortController();
    subscriber.history = history;
    try {
      while (!subscriber.stopped) {
        const after = subscriber.after;
        const response = await fetch(
          `/api/sessions/${encodeURIComponent(this.session)}/events?turn_id=${encodeURIComponent(turn)}&after_sequence=${after}&limit=500&include_live=0`,
          {
            signal: history.signal,
            headers: { Accept: "application/json" },
          },
        );
        if (!response.ok) throw new Error("Recorded turn updates unavailable");
        const data = await response.json();
        if (!Array.isArray(data.events))
          throw new Error("Recorded turn update was incomplete");
        const events = data.events as SessionFrame[];
        if (subscriber.stopped) return;
        // Journal pages are ordered. Publish them without retaining an unbounded
        // transcript while live arrivals wait in a small catch-up buffer.
        subscriber.ready = true;
        for (const event of events) this.deliver(subscriber, event);
        subscriber.ready = false;
        const next = Number(data.next_after);
        if (Number.isSafeInteger(next) && next > subscriber.after) subscriber.after = next;
        const more = data.has_more === true || (data.has_more === undefined && events.length >= 500);
        if (!more && subscriber.after >= subscriber.pendingThrough) break;
        if (subscriber.after === after)
          throw new Error("Recorded turn replay made no progress");
      }
      if (subscriber.stopped) return;
      subscriber.ready = true;
      const events = subscriber.pending.sort((a, b) => seq(a) - seq(b));
      subscriber.pending = [];
      subscriber.pendingThrough = 0;
      for (const event of events) this.deliver(subscriber, event);
    } catch (error) {
      if (!subscriber.stopped) {
        subscriber.receiver.onError?.(
          error instanceof Error
            ? error.message
            : "Recorded updates unavailable",
        );
        subscriber.retry = setTimeout(
          () => void this.backfill(subscriber, turn),
          1000,
        );
      }
    }
  }
  subscribe(receiver: Receiver, after?: number, turn = "") {
    const subscriber: Subscriber = {
      receiver,
      after: after ?? this.cursor,
      ready: true,
      pending: [],
      pendingThrough: 0,
      turn,
      stopped: false,
    };
    this.subscribers.add(subscriber);
    if (this.snapshot) receiver.onSnapshot?.(this.snapshot);
    if (this.unsupported) receiver.onUnsupported?.();
    if (after !== undefined) {
      if (after < this.floor && turn) {
        subscriber.ready = false;
        void this.backfill(subscriber, turn);
      } else for (const event of this.cache) this.deliver(subscriber, event);
    }
    this.wake();
    let stopped = false;
    return () => {
      if (stopped) return;
      stopped = true;
      subscriber.stopped = true;
      subscriber.history?.abort();
      clearTimeout(subscriber.retry);
      this.subscribers.delete(subscriber);
      if (this.subscribers.size) return;
      this.request?.abort();
      void this.reader?.cancel().catch(() => {});
      clearTimeout(this.timer);
      window.removeEventListener("online", this.wake);
      window.removeEventListener("offline", this.offline);
      window.removeEventListener("focus", this.wake);
      document.removeEventListener("visibilitychange", this.visible);
      if (connections.get(this.session) === this)
        connections.delete(this.session);
    };
  }
}

export function subscribeSessionEvents(
  session: string,
  receiver: Receiver,
  options: { afterSequence?: number; turnId?: string } = {},
) {
  let connection = connections.get(session);
  if (!connection) {
    connection = new SessionConnection(session, options.afterSequence ?? 0);
    connections.set(session, connection);
  }
  return connection.subscribe(receiver, options.afterSequence, options.turnId);
}

/** Successful native discovery remains authoritative during reconnects. */
export function hasNativeSessionEvents(
  session: string | null | undefined,
): boolean {
  return Boolean(session && connections.get(session)?.nativeAvailable);
}
