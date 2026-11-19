import { afterEach, describe, expect, it, vi } from "vitest";
import { parseSseChunk, streamSessionEvents, streamChat, StreamDroppedError, STREAM_STARTUP_MS } from "./stream";
import { ApiError } from "./client";

function sseBody(chunks: string[]): ReadableStream<Uint8Array> {
  const enc = new TextEncoder();
  let i = 0;
  return new ReadableStream({
    pull(controller) {
      if (i >= chunks.length) {
        controller.close();
        return;
      }
      controller.enqueue(enc.encode(chunks[i++]));
    },
  });
}

function hangingSseBody(chunks: string[]): ReadableStream<Uint8Array> {
  const enc = new TextEncoder();
  let i = 0;
  let hangResolve: (() => void) | null = null;
  return new ReadableStream({
    pull(controller) {
      if (i >= chunks.length) {
        return new Promise<void>((resolve) => {
          hangResolve = () => {
            controller.close();
            resolve();
          };
        });
      }
      controller.enqueue(enc.encode(chunks[i++]));
    },
    cancel() {
      hangResolve?.();
    },
  });
}

function pacedSseBody(chunks: string[], gapMs: number): ReadableStream<Uint8Array> {
  const enc = new TextEncoder();
  let i = 0;
  return new ReadableStream({
    async pull(controller) {
      if (i >= chunks.length) {
        controller.close();
        return;
      }
      if (i > 0) await new Promise((resolve) => setTimeout(resolve, gapMs));
      controller.enqueue(enc.encode(chunks[i++]));
    },
  });
}

describe("parseSseChunk", () => {
  it("parses status frames", () => {
    const { frames, rest } = parseSseChunk(
      'event: status\ndata: {"label":"Starting…"}\n\n',
    );
    expect(rest).toBe("");
    expect(frames).toEqual([
      { event: "status", data: { label: "Starting…" } },
    ]);
  });

  it("parses dedicated reasoning frames", () => {
    const { frames } = parseSseChunk(
      'event: reasoning\ndata: {"kind":"reasoning","payload":{"text":"plan","partial":true}}\n\n',
    );
    expect(frames).toEqual([
      {
        event: "reasoning",
        data: { kind: "reasoning", payload: { text: "plan", partial: true } },
      },
    ]);
  });
});

describe("streamChat", () => {
  it("updates the accumulated draft before receiving a corrected closing message", async () => {
    const frames = [
      ["delta", { text: "The rate is 3%." }],
      ["event", { kind: "progress", payload: { source_kind: "answer_revision",
        original: "The rate is 3%.", replacement: "The rate is 4%." } }],
      ["event", { kind: "progress", payload: { source_kind: "assistant_narrative", text: "The rate is 3%.", offset: 0 } }],
      ["message", { text: "The rate is 4%.\n\nFreshness is unverified." }],
      ["done", { status: "unverified" }],
    ];
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(sseBody(
      frames.map(([event, data]) => `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`),
    ), { headers: { "Content-Type": "text/event-stream" } })));
    const seen: string[] = [];
    await streamChat({ session_id: "s", thread_id: "t", message: "Check" }, {
      onMessage: text => seen.push(text),
    });
    expect(seen).toEqual(["The rate is 4%.\n\nFreshness is unverified."]);
  });

  it("includes saved narration in later deltas without duplicating streamed text", async () => {
    const frames = [
      ["event", { kind: "progress", payload: { source_kind: "assistant_narrative", text: "First.", offset: 0 } }],
      ["tool_card", { name: "read_file" }],
      ["delta", { text: "Next." }],
      ["event", { kind: "progress", payload: { source_kind: "assistant_narrative", text: "Next.", offset: 6 } }],
      ["tool_card", { name: "write_file" }],
      ["delta", { text: "Done." }],
      ["done", {}],
    ];
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(sseBody(
      frames.map(([event, data]) => `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`),
    ), { headers: { "Content-Type": "text/event-stream" } })));
    const seen: string[] = [];
    await streamChat({ session_id: "s", thread_id: "t", message: "Task" }, {
      onDelta: (text) => seen.push(text),
    });
    expect(seen).toEqual(["First.Next.", "First.Next.Done."]);
  });

  it("recovers when response headers are withheld without submitting twice", async () => {
    vi.useFakeTimers();
    const fetch = vi.fn((_url: string, init: RequestInit) => new Promise<Response>((_resolve, reject) => {
      init.signal?.addEventListener("abort", () => reject(new DOMException("Aborted", "AbortError")));
    }));
    vi.stubGlobal("fetch", fetch);
    try {
      const pending = streamChat({ thread_id: "t", session_id: "s", message: "hi" }, {});
      const rejection = expect(pending).rejects.toMatchObject({
        name: "StreamDroppedError", turnId: "pending", sessionId: "s", threadId: "t",
      });
      await vi.advanceTimersByTimeAsync(STREAM_STARTUP_MS);
      await rejection;
      expect(fetch).toHaveBeenCalledTimes(1);
      expect((fetch.mock.calls[0][1].signal as AbortSignal).aborted).toBe(true);
    } finally {
      vi.useRealTimers();
    }
  });

  it("preserves user cancellation while waiting for response headers", async () => {
    const abort = new AbortController();
    const fetch = vi.fn((_url: string, init: RequestInit) => new Promise<Response>((_resolve, reject) => {
      init.signal?.addEventListener("abort", () => reject(new DOMException("Aborted", "AbortError")));
    }));
    vi.stubGlobal("fetch", fetch);
    const pending = streamChat({ thread_id: "t", session_id: "s", message: "hi" }, {}, { signal: abort.signal });
    const rejection = expect(pending).rejects.toMatchObject({ name: "AbortError" });
    abort.abort();
    await rejection;
    expect(fetch).toHaveBeenCalledTimes(1);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("reattaches when the body has no frames", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        new Response(sseBody([]), {
          status: 200,
          headers: { "Content-Type": "text/event-stream" },
        }),
      ),
    );
    await expect(
      streamChat({ thread_id: "t", session_id: "s", message: "hi" }, {}),
    ).rejects.toBeInstanceOf(StreamDroppedError);
  });

  it("reattaches after a premature close, even with leftover status text", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        new Response(
          sseBody([
            'event: status\ndata: {"phase":"starting","label":"Starting…"}\n\n',
            'event: delta\ndata: {"text":"I found the site."}\n\n',
          ]),
          {
            status: 200,
            headers: { "Content-Type": "text/event-stream" },
          },
        ),
      ),
    );
    await expect(
      streamChat({ thread_id: "t", session_id: "s", message: "hi" }, {}),
    ).rejects.toBeInstanceOf(StreamDroppedError);
  });

  it("returns done payload when present", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        new Response(
          sseBody([
            'event: status\ndata: {"label":"Starting…"}\n\n',
            'event: done\ndata: {"status":"success","message":"Hello"}\n\n',
          ]),
          {
            status: 200,
            headers: { "Content-Type": "text/event-stream" },
          },
        ),
      ),
    );
    const done = await streamChat({ thread_id: "t", session_id: "s", message: "hi" }, {});
    expect(done).toEqual({ status: "success", message: "Hello" });
  });

  it("replaces prior step text when a delta resets the stream", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        new Response(
          sseBody([
            'event: delta\ndata: {"text":"old"}\n\n',
            'event: delta\ndata: {"text":"new","reset":true}\n\n',
            'event: done\ndata: {"status":"success","message":"new"}\n\n',
          ]),
          { status: 200, headers: { "Content-Type": "text/event-stream" } },
        ),
      ),
    );
    const seen: string[] = [];
    await streamChat(
      { thread_id: "t", session_id: "s", message: "go" },
      { onDelta: (text) => seen.push(text) },
    );
    expect(seen).toEqual(["old", "new"]);
  });

  describe("closing message frame", () => {
    const run = async (frames: string[]) => {
      vi.stubGlobal(
        "fetch",
        vi.fn(async () =>
          new Response(sseBody(frames), { status: 200, headers: { "Content-Type": "text/event-stream" } }),
        ),
      );
      const snapshots: string[] = [];
      await streamChat(
        { thread_id: "t", session_id: "s", message: "go" },
        { onDelta: (text) => snapshots.push(text), onMessage: (text) => snapshots.push(text) },
      );
      return snapshots;
    };
    const frame = (event: string, data: object) => `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`;

    it("does not replace earlier streamed steps with the last step's shorter text", async () => {
      const full = "The complete reading, in detail.";
      const recap = "The full answer is already above.";
      const snapshots = await run([
        frame("delta", { text: full }),
        frame("delta", { text: `\n\n${recap}` }),
        // The server closes with only the final model step.
        frame("message", { text: recap, partial: false }),
        frame("done", { status: "success", message: recap }),
      ]);
      expect(snapshots.at(-1)).toBe(`${full}\n\n${recap}`);
      expect(snapshots.every((text) => text.includes(full))).toBe(true);
    });

    it("still applies a closing message that was never streamed", async () => {
      const snapshots = await run([
        frame("delta", { text: "Working on it." }),
        frame("message", { text: "Here is the answer.", partial: false }),
        frame("done", { status: "success" }),
      ]);
      expect(snapshots.at(-1)).toBe("Working on it.\n\nHere is the answer.");
    });

    it("preserves a long answer when an unstreamed stall recap closes the turn", async () => {
      const answer = "Detailed reading. ".repeat(100);
      const recap = "The reading is complete — delivered in full above.";
      const snapshots = await run([
        frame("delta", { text: answer }),
        frame("message", { text: recap }),
        frame("done", { status: "no_progress", message: recap }),
      ]);
      expect(snapshots.at(-1)).toBe(`${answer}\n\n${recap}`);
    });
  });

  it("surfaces an error frame even when followed by done", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        new Response(
          sseBody([
            'event: error\ndata: {"error":"boom"}\n\n',
            'event: done\ndata: {"status":"success","message":"Hello"}\n\n',
          ]),
          {
            status: 200,
            headers: { "Content-Type": "text/event-stream" },
          },
        ),
      ),
    );
    await expect(
      streamChat({ thread_id: "t", session_id: "s", message: "hi" }, {}),
    ).rejects.toThrow("boom");
  });

  it("identifies a resume rejection before any turn was accepted", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => new Response(sseBody([
      'event: status\ndata: {"phase":"starting","session_id":"s"}\n\n',
      'event: error\ndata: {"error":"This chat has a newer task. Refresh before resuming."}\n\n',
    ]), { status: 200, headers: { "Content-Type": "text/event-stream" } })));
    const result = streamChat({ thread_id: "t", session_id: "s", message: "Resume task", resume_turn_id: "old" }, {});
    await expect(result).rejects.toBeInstanceOf(ApiError);
    await expect(result).rejects.toMatchObject({ status: 400 });
  });

  it("throws StreamDroppedError with lastSequence after mid-stream silence", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        new Response(
          hangingSseBody([
            'event: status\ndata: {"phase":"running","label":"Working…","turn_id":"turn-9","session_id":"s1","thread_id":"t1","sequence":4}\n\n',
          ]),
          {
            status: 200,
            headers: { "Content-Type": "text/event-stream" },
          },
        ),
      ),
    );
    try {
      await streamChat(
        { thread_id: "t", session_id: "s", message: "hi" },
        {},
        { stallMs: 40 },
      );
      expect.unreachable("should have thrown");
    } catch (err) {
      expect(err).toBeInstanceOf(StreamDroppedError);
      expect((err as StreamDroppedError).turnId).toBe("turn-9");
      expect((err as StreamDroppedError).lastSequence).toBe(4);
    }
  });

  it("keeps the stream open while heartbeats continue", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        new Response(
          pacedSseBody(
            [
              'event: status\ndata: {"phase":"running","label":"Working…","turn_id":"turn-1","session_id":"s1","thread_id":"t1"}\n\n',
              'event: status\ndata: {"phase":"heartbeat","label":"Still working…","turn_id":"turn-1"}\n\n',
              'event: done\ndata: {"status":"success","message":"ok"}\n\n',
            ],
            20,
          ),
          {
            status: 200,
            headers: { "Content-Type": "text/event-stream" },
          },
        ),
      ),
    );
    const done = await streamChat(
      { thread_id: "t", session_id: "s", message: "hi" },
      {},
      { stallMs: 80 },
    );
    expect(done).toEqual({ status: "success", message: "ok" });
  });

  it("returns cancelled when the caller aborts after the first frame", async () => {
    const abort = new AbortController();
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        new Response(
          hangingSseBody([
            'event: status\ndata: {"phase":"running","label":"Working…","turn_id":"turn-1"}\n\n',
          ]),
          {
            status: 200,
            headers: { "Content-Type": "text/event-stream" },
          },
        ),
      ),
    );
    const pending = streamChat(
      { thread_id: "t", session_id: "s", message: "hi" },
      {},
      { signal: abort.signal, stallMs: 5000 },
    );
    await new Promise((resolve) => setTimeout(resolve, 10));
    abort.abort();
    await expect(pending).resolves.toMatchObject({ status: "cancelled" });
  });

  it("throws StreamDroppedError when connection drops after a turn_id", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        new Response(
          sseBody([
            'event: status\ndata: {"phase":"running","label":"Working…","turn_id":"turn-42","session_id":"s1","thread_id":"t1"}\n\n',
          ]),
          {
            status: 200,
            headers: { "Content-Type": "text/event-stream" },
          },
        ),
      ),
    );
    try {
      await streamChat(
        { thread_id: "t", session_id: "s", message: "hi" },
        {},
      );
      expect.unreachable("should have thrown");
    } catch (err) {
      expect(err).toBeInstanceOf(StreamDroppedError);
      expect((err as StreamDroppedError).turnId).toBe("turn-42");
      expect((err as StreamDroppedError).sessionId).toBe("s1");
      expect((err as StreamDroppedError).threadId).toBe("t1");
    }
  });

  it("reattaches when reader.read fails after a turn is known", async () => {
    const enc = new TextEncoder();
    let pulled = 0;
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        new Response(
          new ReadableStream({
            pull(controller) {
              if (pulled === 0) {
                pulled += 1;
                controller.enqueue(
                  enc.encode(
                    'event: status\ndata: {"phase":"running","label":"Working…","turn_id":"turn-8","session_id":"s1","thread_id":"t1","sequence":3}\n\n',
                  ),
                );
                return;
              }
              throw new Error("socket reset");
            },
          }),
          {
            status: 200,
            headers: { "Content-Type": "text/event-stream" },
          },
        ),
      ),
    );
    try {
      await streamChat({ thread_id: "t", session_id: "s", message: "hi" }, {});
      expect.unreachable("should have thrown");
    } catch (err) {
      expect(err).toBeInstanceOf(StreamDroppedError);
      expect((err as StreamDroppedError).turnId).toBe("turn-8");
      expect((err as StreamDroppedError).lastSequence).toBe(3);
    }
  });

  it("surfaces handler failures instead of converting them into drops", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        new Response(
          sseBody([
            'event: status\ndata: {"phase":"running","label":"Working…","turn_id":"turn-9"}\n\n',
          ]),
          {
            status: 200,
            headers: { "Content-Type": "text/event-stream" },
          },
        ),
      ),
    );
    await expect(
      streamChat(
        { thread_id: "t", session_id: "s", message: "hi" },
        {
          onStatus: () => {
            throw new Error("handler boom");
          },
        },
      ),
    ).rejects.toThrow("handler boom");
  });
});


describe("live session event stream", () => {
  afterEach(() => { vi.unstubAllGlobals(); vi.useRealTimers(); });
  it("uses GET, preserves packet deltas and closes on done", async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(sseBody([
      'event: update\ndata: {"events":[{"sequence":5}],"live":{"live_message":"Hello"}}\n\n',
      'event: update\ndata: {"events":[{"sequence":6}],"live":{"live_message_delta":" world"}}\n\n',
      'event: done\ndata: {}\n\n',
    ]), { headers: { "Content-Type": "text/event-stream" } }));
    vi.stubGlobal("fetch", fetchMock);
    const updates = vi.fn();
    await streamSessionEvents("/api/sessions/one/events/stream?after_sequence=4", updates, new AbortController().signal);
    expect(updates).toHaveBeenCalledTimes(2);
    expect(updates.mock.calls[1][0].live.live_message_delta).toBe(" world");
    expect(fetchMock.mock.calls[0][1].method).toBeUndefined();
    expect(fetchMock.mock.calls[0][1].body).toBeUndefined();
  });
  it("reports dropped streams so reattachment can fall back to polling", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(sseBody([
      'event: update\ndata: {"events":[]}\n\n',
    ]), { headers: { "Content-Type": "text/event-stream" } })));
    await expect(streamSessionEvents("/api/sessions/one/events/stream", () => {}, new AbortController().signal)).rejects.toThrow("interrupted");
  });
});

it("never replays a chat POST after an ambiguous network failure", async () => {
  const fetcher = vi.fn().mockRejectedValue(new TypeError("Failed to fetch"));
  vi.stubGlobal("fetch", fetcher);
  await expect(streamChat({ thread_id: "t", session_id: "s", message: "Top 3 only" }, {}))
    .rejects.toMatchObject({ name: "StreamDroppedError", turnId: "pending", sessionId: "s" });
  expect(fetcher).toHaveBeenCalledTimes(1);
});

it("shows a definitive model admission rejection even when it uses HTTP 503", async () => {
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(Response.json({
    error: "Saved credentials are locked", admission: "rejected", request_id: "r",
  }, { status: 503 })));
  await expect(streamChat({ session_id: "s", thread_id: "t", message: "hello", client_request_id: "r" }, {}))
    .rejects.toMatchObject({ name: "ApiError", message: "Saved credentials are locked", status: 503 });
  expect(fetch).toHaveBeenCalledTimes(1);
});

it("recovers ambiguous proxy failure using the exact receipt without retrying POST", async () => {
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response("gateway", {status:502,headers:{"content-type":"text/html"}})));
  await expect(streamChat({session_id:"s",thread_id:"t",message:"hello",client_request_id:"r"},{}))
    .rejects.toMatchObject({turnId:"pending",requestId:"r"});
  expect(fetch).toHaveBeenCalledTimes(1);
});

it("recovers a response body failure before the first frame", async () => {
  const body=new ReadableStream<Uint8Array>({start(controller){controller.error(new Error("socket lost"));}});
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(body,{headers:{"content-type":"text/event-stream"}})));
  await expect(streamChat({session_id:"s",thread_id:"t",message:"hello",client_request_id:"r"},{}))
    .rejects.toMatchObject({turnId:"pending",requestId:"r"});
  expect(fetch).toHaveBeenCalledTimes(1);
});

it("settles on done without waiting for the proxy to close its socket", async () => {
  const cancel=vi.fn();
  const body=new ReadableStream<Uint8Array>({start(controller){controller.enqueue(new TextEncoder().encode('event: done\ndata: {"status":"success","message":"done"}\n\n'));},cancel});
  vi.stubGlobal("fetch",vi.fn().mockResolvedValue(new Response(body,{headers:{"content-type":"text/event-stream"}})));
  await expect(streamChat({session_id:"s",thread_id:"t",message:"hello"},{})).resolves.toMatchObject({status:"success"});
  expect(cancel).toHaveBeenCalledOnce();
});

it("deduplicates sequenced journal frames before accumulating text and rejects foreign turns", async () => {
  const frame = (event: string, data: object) => `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`;
  const narrative = (sequence: number, turnId: string, text: string, offset: number) => frame("event", {
    kind: "progress", sequence, turn_id: turnId,
    payload: { source_kind: "assistant_narrative", text, offset },
  });
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(sseBody([
    frame("status", { turn_id: "owned", phase: "running" }),
    narrative(1, "owned", "First", 0),
    narrative(1, "owned", " stale", 5),
    narrative(100, "foreign", " other chat", 5),
    narrative(2, "owned", " second", 5),
    frame("message", { text: "First second" }),
    frame("done", { status: "success", message: "First second" }),
  ]), { headers: { "Content-Type": "text/event-stream" } })));
  const onEvent = vi.fn();
  const onMessage = vi.fn();
  await streamChat({ session_id: "s", thread_id: "t", message: "Task" }, { onEvent, onMessage });
  expect(onEvent).toHaveBeenCalledTimes(2);
  // Narration was already accumulated; its identical terminal snapshot adds nothing.
  expect(onMessage).not.toHaveBeenCalled();
  vi.unstubAllGlobals();
});

it("uses an explicit revision to reconcile a short masked answer after progress", async () => {
  const frame = (event: string, data: object) => `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`;
  const progress = "Checking the sources. ";
  const draft = "See [Posting]([REDACTED]).";
  const replacement = "**Next:** [Posting]([REDACTED]).";
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(sseBody([
    frame("event", { kind: "assistant_narrative", payload: { text: progress, offset: 0 } }),
    frame("delta", { text: draft }),
    frame("event", { kind: "answer_revision", payload: {
      original: draft, replacement, offset: progress.length,
    } }),
    frame("message", { text: replacement.replace("[REDACTED]", "https://example.com/job") }),
    frame("done", { status: "success", message: replacement.replace("[REDACTED]", "https://example.com/job") }),
  ]), { headers: { "Content-Type": "text/event-stream" } })));
  const onMessage = vi.fn();
  const onDelta = vi.fn();
  await streamChat({ session_id: "s", thread_id: "t", message: "Next?" }, { onMessage, onDelta });
  expect(onDelta).toHaveBeenLastCalledWith(progress + draft);
  // The revision event already replaces the owned answer in the store.
  expect(onMessage).not.toHaveBeenCalled();
  vi.unstubAllGlobals();
});

it("joins child traces to the parent's shared transport and retains child identity", async () => {
  vi.useFakeTimers();
  const {subscribeWorkers}=await import("../lib/workerConnection");
  let controller!:ReadableStreamDefaultController<Uint8Array>;
  const stream=new ReadableStream<Uint8Array>({start(value){controller=value;}});
  const fetch=vi.fn(async(url:string)=>String(url).includes("/live/stream")
    ?new Response(stream,{headers:{"Content-Type":"text/event-stream"}})
    :new Response(JSON.stringify({events:[],live:{active_turn:{turn_id:"child-turn"}}}),{headers:{"Content-Type":"application/json"}}));
  vi.stubGlobal("fetch",fetch);
  const stop=subscribeWorkers("parent",vi.fn(),vi.fn());const abort=new AbortController();
  try {
    await vi.advanceTimersByTimeAsync(0);
    controller.enqueue(new TextEncoder().encode('event: snapshot\ndata: {"sequence":1,"session_id":"parent","workers":[]}\n\n'));
    const updates=vi.fn();let settled=false;
    const pending=streamSessionEvents("/api/sessions/child/events/stream?turn_id=child-turn&after_sequence=0&parent_session_id=parent&job_id=job",updates,abort.signal).then(()=>{settled=true;});
    await vi.advanceTimersByTimeAsync(0);
    const emit=(events:unknown[])=>controller.enqueue(new TextEncoder().encode(`event: update\ndata: ${JSON.stringify({events})}\n\n`));
    emit([{sequence:2,kind:"worker_activity",job_id:"other",source_session_id:"child",source_turn_id:"child-turn",activity:{kind:"tool_card",tool_card:{id:"wrong"}}},
      {sequence:3,kind:"worker_activity",job_id:"job",source_session_id:"child",source_turn_id:"child-turn",activity:{kind:"tool_card",tool_card:{id:"right",status:"done"}}},
      {sequence:4,kind:"done",turn_id:"parent-turn",payload:{status:"success"}}]);
    await vi.advanceTimersByTimeAsync(0);
    expect(settled).toBe(false);
    const frames=updates.mock.calls.flatMap(([packet])=>packet.events||[]);
    expect(frames).toHaveLength(1);expect(frames[0]).toMatchObject({turn_id:"child-turn",tool_card:{id:"right"}});
    emit([{sequence:5,kind:"worker_activity",job_id:"job",source_session_id:"child",source_turn_id:"child-turn",activity:{kind:"done",payload:{status:"success"}}}]);
    await vi.advanceTimersByTimeAsync(0);await pending;
    expect(fetch.mock.calls.filter(([url])=>String(url).includes("/live/stream"))).toHaveLength(1);
  } finally {abort.abort();stop();vi.useRealTimers();}
});
