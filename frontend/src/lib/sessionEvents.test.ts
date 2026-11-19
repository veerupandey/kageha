import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { subscribeSessionEvents } from "./sessionEvents";
import { subscribeWorkers, workerSnapshot } from "./workerConnection";
import { streamChat } from "../api/stream";
import type { DurableJob } from "../components/DurableJobs";

const cleanups: Array<() => void> = [];
function wire() {
  let controller!: ReadableStreamDefaultController<Uint8Array>;
  const cancel = vi.fn();
  const stream = new ReadableStream<Uint8Array>({
    start(c) {
      controller = c;
    },
    cancel,
  });
  return {
    response: new Response(stream, {
      headers: { "Content-Type": "text/event-stream" },
    }),
    cancel,
    frame(event: string, data: unknown) {
      controller.enqueue(
        new TextEncoder().encode(
          `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`,
        ),
      );
    },
    close() {
      controller.close();
    },
  };
}
function job(state = "running", revision = 1): DurableJob {
  return {
    id: "worker",
    label: "Reviewer", kind:"agent", created_at:1, started_at:1,can_cancel:true,attachable:true,error:{},
    state,
    revision,
    session_id: "child",
    parent_session: "parent",
    result: {},
    artifacts: [],
  };
}
async function flush() {
  await vi.advanceTimersByTimeAsync(0);
}
beforeEach(() => {
  vi.useFakeTimers();
});
afterEach(() => {
  cleanups.splice(0).forEach((stop) => stop());
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  vi.useRealTimers();
});

it("shares tools and worker lifecycle on one stream and keeps children live after parent completion", async () => {
  const events = wire();
  const fetch = vi.fn(async (url: string) =>
    url === "/api/chat/stream"
      ? new Response(
          JSON.stringify({
            event_stream: "session",
            session_id: "parent",
            thread_id: "thread",
            turn_id: "turn",
            after_sequence: 4,
            status: "accepted",
          }),
          { headers: { "Content-Type": "application/json" } },
        )
      : events.response,
  );
  vi.stubGlobal("fetch", fetch);
  const receive = vi.fn();
  const stop = subscribeWorkers("parent", vi.fn(), receive);
  cleanups.push(stop);
  await flush();
  events.frame("snapshot", {
    sequence: 5,
    session_id: "parent",
    workers: [job()],
  });
  await flush();
  const cards = vi.fn();
  const pending = streamChat(
    { session_id: "parent", thread_id: "thread", message: "work" },
    { onToolCard: cards },
  );
  await flush();
  events.frame("update", {
    events: [
      {
        sequence: 6,
        turn_id: "turn",
        kind: "tool_card",
        tool_card: { id: "spawn", name: "agent_spawn", status: "done" },
      },
      {
        sequence: 7,
        turn_id: "turn",
        kind: "done",
        payload: { status: "success", message: "Parent finished" },
      },
    ],
  });
  await flush();
  expect(await pending).toMatchObject({ status: "success" });
  expect(cards).toHaveBeenCalledTimes(1);
  expect(workerSnapshot("parent").jobs[0].state).toBe("running");
  expect(events.cancel).not.toHaveBeenCalled();
  const done = job("succeeded", 2);
  done.result = { job_reply_id: "worker:reply", message: "Child finished" };
  events.frame("update", {
    events: [{ sequence: 8, kind: "worker", job: done }],
  });
  await flush();
  expect(workerSnapshot("parent").jobs[0].state).toBe("succeeded");
  expect(receive).toHaveBeenCalledTimes(1);
  expect(
    fetch.mock.calls.filter(([url]) => String(url).includes("/live/stream")),
  ).toHaveLength(1);
  expect(
    fetch.mock.calls.some(([url]) => String(url).includes("/api/jobs")),
  ).toBe(false);
  stop();
  await flush();
  expect(events.cancel).toHaveBeenCalledTimes(1);
});

it("reconnects read-only from the event cursor and does not roll workers backward across snapshot replay", async () => {
  const first = wire(),
    second = wire();
  const fetch = vi
    .fn()
    .mockResolvedValueOnce(first.response)
    .mockResolvedValueOnce(second.response);
  vi.stubGlobal("fetch", fetch);
  const seen: unknown[] = [];
  cleanups.push(
    subscribeSessionEvents(
      "parent",
      { onEvent: (event) => seen.push(event) },
      { afterSequence: 0 },
    ),
  );
  cleanups.push(subscribeWorkers("parent", vi.fn(), vi.fn()));
  await flush();
  first.frame("snapshot", {
    sequence: 4,
    session_id: "parent",
    workers: [job()],
  });
  first.frame("update", {
    events: [{ sequence: 5, kind: "worker", job: job("succeeded", 3) }],
  });
  await flush();
  first.close();
  await flush();
  await vi.advanceTimersByTimeAsync(1000);
  expect(fetch.mock.calls[1][0]).toContain("after_sequence=5");
  second.frame("snapshot", {
    sequence: 10,
    session_id: "parent",
    workers: [job("succeeded", 3)],
  });
  second.frame("update", {
    events: [
      { sequence: 5, kind: "worker", job: job("running", 1) },
      {
        sequence: 6,
        kind: "tool_card",
        turn_id: "old-turn",
        tool_card: { id: "read", status: "done" },
      },
      { sequence: 7, kind: "worker", job: job("running", 2) },
    ],
  });
  await flush();
  expect(seen).toHaveLength(3);
  expect(workerSnapshot("parent").jobs[0]).toMatchObject({
    state: "succeeded",
    revision: 3,
  });
  expect(fetch).toHaveBeenCalledTimes(2);
});

it("deduplicates reconnect notifications and ignores a late response from an aborted connection", async () => {
  let late!: (response: Response) => void;
  const current = wire();
  const fetch = vi
    .fn()
    .mockImplementationOnce(
      () =>
        new Promise<Response>((resolve) => {
          late = resolve;
        }),
    )
    .mockResolvedValueOnce(current.response);
  vi.stubGlobal("fetch", fetch);
  const seen = vi.fn();
  cleanups.push(subscribeSessionEvents("parent", { onEvent: seen }));
  await flush();
  vi.spyOn(navigator, "onLine", "get").mockReturnValue(false);
  window.dispatchEvent(new Event("offline"));
  vi.spyOn(navigator, "onLine", "get").mockReturnValue(true);
  window.dispatchEvent(new Event("online"));
  window.dispatchEvent(new Event("focus"));
  await flush();
  current.frame("snapshot", { sequence: 1, session_id: "parent", workers: [] });
  current.frame("update", {
    events: [
      {
        sequence: 2,
        kind: "status",
        turn_id: "turn",
        payload: { label: "new" },
      },
    ],
  });
  await flush();
  const obsolete = wire();
  late(obsolete.response);
  await flush();
  expect(seen).toHaveBeenCalledTimes(1);
  expect(fetch).toHaveBeenCalledTimes(2);
});

it("uses legacy polling only when the native stream explicitly reports unsupported", async () => {
  const fetch = vi.fn(async (url: string) =>
    String(url).includes("/live/stream")
      ? new Response("{}", {
          status: 404,
          headers: { "Content-Type": "application/json" },
        })
      : new Response(JSON.stringify({ jobs: [job()] }), {
          headers: { "Content-Type": "application/json" },
        }),
  );
  vi.stubGlobal("fetch", fetch);
  cleanups.push(subscribeWorkers("parent", vi.fn(), vi.fn()));
  await flush();
  expect(workerSnapshot("parent").jobs[0].id).toBe("worker");
  expect(fetch.mock.calls.map(([url]) => url)).toEqual([
    expect.stringContaining("/live/stream"),
    expect.stringContaining("/api/jobs"),
  ]);
  await vi.advanceTimersByTimeAsync(2000);
  expect(fetch).toHaveBeenCalledTimes(3);
});

it("backs off a broken stream without silently falling back to a polling loop", async () => {
  const fetch = vi.fn(
    async (_url: string) =>
      new Response("{}", {
        status: 503,
        headers: { "Content-Type": "application/json" },
      }),
  );
  vi.stubGlobal("fetch", fetch);
  cleanups.push(subscribeWorkers("parent", vi.fn(), vi.fn()));
  await flush();
  expect(workerSnapshot("parent").error).toBeTruthy();
  await vi.advanceTimersByTimeAsync(1000);
  expect(fetch).toHaveBeenCalledTimes(2);
  expect(
    fetch.mock.calls.every(([url]) => String(url).includes("/live/stream")),
  ).toBe(true);
});

it("keeps chat sessions isolated when the last subscriber is replaced", async () => {
  const old = wire(),
    next = wire();
  vi.stubGlobal(
    "fetch",
    vi
      .fn()
      .mockResolvedValueOnce(old.response)
      .mockResolvedValueOnce(next.response),
  );
  const stopped = subscribeWorkers("old", vi.fn(), vi.fn());
  await flush();
  stopped();
  const stop = subscribeWorkers("parent", vi.fn(), vi.fn());
  cleanups.push(stop);
  await flush();
  stopped();
  next.frame("snapshot", {
    sequence: 3,
    session_id: "parent",
    workers: [job()],
  });
  await flush();
  expect(workerSnapshot("old").jobs).toEqual([]);
  expect(workerSnapshot("parent").jobs[0].id).toBe("worker");
});

it("replays a completed parent from shared cache without opening a second stream", async () => {
  const events = wire();
  const fetch = vi.fn(async (url: string) =>
    url === "/api/chat/stream"
      ? new Response(
          JSON.stringify({
            event_stream: "session",
            session_id: "parent",
            thread_id: "thread",
            turn_id: "turn",
            after_sequence: 1,
          }),
          { headers: { "Content-Type": "application/json" } },
        )
      : events.response,
  );
  vi.stubGlobal("fetch", fetch);
  cleanups.push(subscribeWorkers("parent", vi.fn(), vi.fn()));
  await flush();
  events.frame("snapshot", {
    sequence: 8,
    session_id: "parent",
    workers: [job()],
  });
  events.frame("update", {
    events: [
      {
        sequence: 2,
        turn_id: "turn",
        kind: "tool_card",
        tool_card: { id: "read", status: "done" },
      },
      {
        sequence: 3,
        turn_id: "turn",
        kind: "done",
        payload: { status: "success", message: "cached" },
      },
    ],
  });
  await flush();
  const cards = vi.fn();
  const done = await streamChat(
    { session_id: "parent", thread_id: "thread", message: "work" },
    { onToolCard: cards },
  );
  expect(done.message).toBe("cached");
  expect(cards).toHaveBeenCalledTimes(1);
  expect(
    fetch.mock.calls.filter(([url]) => String(url).includes("/live/stream")),
  ).toHaveLength(1);
  expect(events.cancel).not.toHaveBeenCalled();
});

it("hydrates a late parent observer from journal pages after its cache was evicted", async () => {
  const events = wire();
  const old = [
    {
      sequence: 1,
      turn_id: "turn",
      kind: "tool_card",
      tool_card: { id: "old-read", status: "done" },
    },
    {
      sequence: 2,
      turn_id: "turn",
      kind: "done",
      payload: { status: "success", message: "history" },
    },
  ];
  const fetch = vi.fn(async (url: string) =>
    url === "/api/chat/stream"
      ? new Response(
          JSON.stringify({
            event_stream: "session",
            session_id: "parent",
            thread_id: "thread",
            turn_id: "turn",
            after_sequence: 0,
          }),
          { headers: { "Content-Type": "application/json" } },
        )
      : String(url).includes("/events?")
        ? new Response(JSON.stringify({ events: old }), {
            headers: { "Content-Type": "application/json" },
          })
        : events.response,
  );
  vi.stubGlobal("fetch", fetch);
  cleanups.push(subscribeWorkers("parent", vi.fn(), vi.fn()));
  await flush();
  events.frame("snapshot", {
    sequence: 2010,
    session_id: "parent",
    workers: [],
  });
  events.frame("update", {
    events: [
      ...old,
      ...Array.from({ length: 2002 }, (_, i) => ({
        sequence: i + 3,
        turn_id: "newer",
        kind: "status",
        payload: { label: "progress" },
      })),
    ],
  });
  await flush();
  const cards = vi.fn();
  const pending = streamChat(
    { session_id: "parent", thread_id: "thread", message: "work" },
    { onToolCard: cards },
  );
  await flush();
  expect((await pending).message).toBe("history");
  expect(cards).toHaveBeenCalledTimes(1);
  expect(
    fetch.mock.calls.some(([url]) =>
      String(url).includes("/events?turn_id=turn&after_sequence=0"),
    ),
  ).toBe(true);
  expect(
    fetch.mock.calls.filter(([url]) => String(url).includes("/live/stream")),
  ).toHaveLength(1);
});

it("releases a stalled header request and reconnects without waiting for the old promise", async () => {
  let late!: (response: Response) => void;
  const current = wire();
  const fetch = vi
    .fn()
    .mockImplementationOnce(
      () =>
        new Promise<Response>((resolve) => {
          late = resolve;
        }),
    )
    .mockResolvedValueOnce(current.response);
  vi.stubGlobal("fetch", fetch);
  cleanups.push(subscribeWorkers("parent", vi.fn(), vi.fn()));
  await flush();
  await vi.advanceTimersByTimeAsync(17000);
  expect(fetch).toHaveBeenCalledTimes(2);
  expect(fetch.mock.calls[0][1].signal.aborted).toBe(true);
  current.frame("snapshot", {
    sequence: 3,
    session_id: "parent",
    workers: [job("succeeded", 2)],
  });
  await flush();
  late(new Response("{}", { status: 404 }));
  await flush();
  expect(workerSnapshot("parent").jobs[0].state).toBe("succeeded");
  expect(fetch).toHaveBeenCalledTimes(2);
});

it("backfills every 500-event page for a late subscriber without duplicating live arrivals", async () => {
  const events = wire();
  const fetch = vi.fn(async (url: string) => {
    if (url.includes("/live/stream")) return events.response;
    const query = new URL(url, "http://test").searchParams;
    expect(query.get("include_live")).toBe("0");
    const after = Number(query.get("after_sequence"));
    const page = Array.from({ length: 500 }, (_, i) => ({ sequence: after + i + 1, turn_id: "old", kind: "progress" }));
    return new Response(JSON.stringify({ events: page, next_after: after + 500, has_more: after < 1000 }));
  });
  vi.stubGlobal("fetch", fetch);
  cleanups.push(subscribeSessionEvents("paged-backfill", {}, { afterSequence: 2000 }));
  await flush();
  const seen: number[] = [];
  cleanups.push(subscribeSessionEvents("paged-backfill", { onEvent: frame => seen.push(Number(frame.sequence)) }, { afterSequence: 0, turnId: "old" }));
  await flush();
  expect(seen).toHaveLength(1500);
  expect(seen[0]).toBe(1);
  expect(seen.at(-1)).toBe(1500);
  events.frame("update", { events: [{ sequence: 2001, turn_id: "old", kind: "done" }] });
  await flush();
  expect(seen.at(-1)).toBe(2001);
  expect(new Set(seen).size).toBe(1501);
  expect(fetch.mock.calls.filter(([url]) => !url.includes("/live/stream"))).toHaveLength(3);
});
