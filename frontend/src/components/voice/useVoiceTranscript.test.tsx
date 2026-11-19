import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { useVoiceTranscript } from "./useVoiceTranscript";
import { CAPTION_RETENTION_LIMIT } from "./voiceTypes";
import { useAppStore } from "../../store";

/**
 * Fake EventSource capturing addEventListener handlers so tests can dispatch
 * named SSE events (voice_transcript, voice_delegation_started, ...) exactly
 * as the browser would deliver them. Mirrors the real EventSource surface the
 * hook depends on: constructor(url), addEventListener(name, handler), close().
 *
 * All live (unclosed) instances are tracked so tests can assert the hook opens
 * exactly one stream and closes it on unmount / sessionId change.
 */
class FakeEventSource {
  static instances: FakeEventSource[] = [];

  readonly url: string;
  closed = false;
  private readonly listeners = new Map<string, Set<(event: MessageEvent) => void>>();

  constructor(url: string) {
    this.url = url;
    FakeEventSource.instances.push(this);
  }

  addEventListener(name: string, handler: (event: MessageEvent) => void): void {
    let set = this.listeners.get(name);
    if (!set) {
      set = new Set();
      this.listeners.set(name, set);
    }
    set.add(handler);
  }

  close(): void {
    this.closed = true;
  }

  /** Dispatch a named event with a JSON-serialized payload, like the server. */
  emit(name: string, data: unknown): void {
    const event = { data: JSON.stringify(data) } as MessageEvent;
    for (const handler of this.listeners.get(name) ?? []) {
      handler(event);
    }
  }

  /** Dispatch a raw (already-string) payload; used for malformed-JSON checks. */
  emitRaw(name: string, raw: string): void {
    const event = { data: raw } as MessageEvent;
    for (const handler of this.listeners.get(name) ?? []) {
      handler(event);
    }
  }

  static reset(): void {
    FakeEventSource.instances = [];
  }

  /** The live (not-yet-closed) instances. */
  static live(): FakeEventSource[] {
    return FakeEventSource.instances.filter((source) => !source.closed);
  }

  /** The single active stream; fails if there isn't exactly one. */
  static only(): FakeEventSource {
    const live = FakeEventSource.live();
    expect(live).toHaveLength(1);
    return live[0];
  }
}

const originalEventSource = globalThis.EventSource;

beforeEach(() => {
  FakeEventSource.reset();
  // Install the fake as the global the hook constructs.
  (globalThis as { EventSource: unknown }).EventSource =
    FakeEventSource as unknown as typeof EventSource;
});

afterEach(() => {
  cleanup();
  (globalThis as { EventSource: unknown }).EventSource = originalEventSource;
});

/** Emit a voice_transcript event with the given role/text. */
function emitTranscript(
  source: FakeEventSource,
  role: "user" | "assistant",
  text: string,
): void {
  source.emit("voice_transcript", { role, text, session_id: "s1" });
}

describe("useVoiceTranscript", () => {
  it("surfaces Live approval through the shared thread approval state", () => {
    useAppStore.setState({ sessionId: "s1", pendingApproval: null });
    const { unmount } = renderHook(() => useVoiceTranscript("s1"));
    const source = FakeEventSource.only();
    act(() => source.emit("voice_approval_required", {
      approval_id: "approve-1", action: "send_email",
      detail: "Send the drafted email", risk_class: "messaging", session_id: "s1",
    }));
    expect(useAppStore.getState().pendingApproval?.approval_id).toBe("approve-1");
    act(() => source.emit("voice_approval_resolved", { approval_id: "approve-1" }));
    expect(useAppStore.getState().pendingApproval).toBeNull();
    unmount();
  });

  it("shows real progress and a completed work preview", () => {
    const { result } = renderHook(() => useVoiceTranscript("s1"));
    const source = FakeEventSource.only();
    act(() => {
      source.emit("voice_delegation_started", { call_id: "c1", task: "inspect repo" });
      source.emit("voice_progress", { label: "Running tests…" });
    });
    expect(result.current.progressLabel).toBe("Running tests…");
    act(() => source.emit("voice_delegation_completed", {
      call_id: "c1", task: "inspect repo", result: "Tests passed.",
    }));
    expect(result.current.latestWork).toEqual({ state: "completed", text: "Tests passed." });
    expect(result.current.progressLabel).toBeNull();
  });

  it("appends captions in arrival order (Req 5.5)", () => {
    const { result } = renderHook(() => useVoiceTranscript("s1"));
    const source = FakeEventSource.only();

    act(() => {
      emitTranscript(source, "user", "hello");
      emitTranscript(source, "assistant", "hi there");
      emitTranscript(source, "user", "how are you");
    });

    expect(result.current.captions.map((c) => c.text)).toEqual([
      "hello",
      "hi there",
      "how are you",
    ]);
    expect(result.current.captions.map((c) => c.role)).toEqual([
      "user",
      "assistant",
      "user",
    ]);
    // seq is strictly increasing, guaranteeing chronological order.
    const seqs = result.current.captions.map((c) => c.seq);
    expect(seqs).toEqual([...seqs].sort((a, b) => a - b));
    expect(new Set(seqs).size).toBe(seqs.length);
  });

  it("skips blank and whitespace-only transcript text (Req 5.3)", () => {
    const { result } = renderHook(() => useVoiceTranscript("s1"));
    const source = FakeEventSource.only();

    act(() => {
      emitTranscript(source, "user", "");
      emitTranscript(source, "assistant", "   ");
      emitTranscript(source, "user", "\n\t  \u00a0");
      emitTranscript(source, "assistant", "  kept  ");
    });

    // Only the non-blank entry survives, and its text is trimmed.
    expect(result.current.captions).toHaveLength(1);
    expect(result.current.captions[0].text).toBe("kept");
    expect(result.current.captions[0].role).toBe("assistant");
  });

  it("ignores transcript events with an unknown role", () => {
    const { result } = renderHook(() => useVoiceTranscript("s1"));
    const source = FakeEventSource.only();

    act(() => {
      source.emit("voice_transcript", { role: "system", text: "nope", session_id: "s1" });
      emitTranscript(source, "user", "yes");
    });

    expect(result.current.captions.map((c) => c.text)).toEqual(["yes"]);
  });

  it("ignores transcript events with malformed JSON payloads", () => {
    const { result } = renderHook(() => useVoiceTranscript("s1"));
    const source = FakeEventSource.only();

    act(() => {
      source.emitRaw("voice_transcript", "{ not valid json");
      emitTranscript(source, "user", "ok");
    });

    expect(result.current.captions.map((c) => c.text)).toEqual(["ok"]);
  });

  it("honors the retention bound, keeping only the most recent (Req 5.7)", () => {
    const { result } = renderHook(() => useVoiceTranscript("s1"));
    const source = FakeEventSource.only();

    const total = CAPTION_RETENTION_LIMIT + 10;
    act(() => {
      for (let i = 0; i < total; i += 1) {
        emitTranscript(source, "user", `msg-${i}`);
      }
    });

    expect(result.current.captions).toHaveLength(CAPTION_RETENTION_LIMIT);
    // The oldest (msg-0 .. msg-9) were dropped; the most recent are retained,
    // still in chronological order.
    expect(result.current.captions[0].text).toBe("msg-10");
    expect(result.current.captions[CAPTION_RETENTION_LIMIT - 1].text).toBe(
      `msg-${total - 1}`,
    );
  });

  it("sources captions from the SSE stream with no voice-WS dependency (Req 5.4)", () => {
    const { result } = renderHook(() => useVoiceTranscript("s1"));
    const source = FakeEventSource.only();

    // The only channel that produces captions is the EventSource stream. The
    // hook opens exactly one SSE connection to the per-session voice endpoint.
    expect(source.url).toBe("/api/sessions/s1/voice/events");
    expect(result.current.captions).toEqual([]);

    act(() => emitTranscript(source, "assistant", "from sse"));

    expect(result.current.captions.map((c) => c.text)).toEqual(["from sse"]);
  });

  it("adds a delegation on started and removes it on completed (Req 12.1, 12.2)", () => {
    const { result } = renderHook(() => useVoiceTranscript("s1"));
    const source = FakeEventSource.only();

    act(() => {
      source.emit("voice_delegation_started", {
        call_id: "call-1",
        task: "research",
        session_id: "s1",
      });
    });
    expect([...result.current.activeDelegations]).toEqual(["call-1"]);

    act(() => {
      source.emit("voice_delegation_completed", {
        call_id: "call-1",
        task: "research",
        result: "done",
        session_id: "s1",
      });
    });
    expect(result.current.activeDelegations.size).toBe(0);
  });

  it("removes a delegation on failed and on cancelled (Req 12.2, 12.3)", () => {
    const { result } = renderHook(() => useVoiceTranscript("s1"));
    const source = FakeEventSource.only();

    act(() => {
      source.emit("voice_delegation_started", {
        call_id: "a",
        task: "t",
        session_id: "s1",
      });
      source.emit("voice_delegation_started", {
        call_id: "b",
        task: "t",
        session_id: "s1",
      });
    });
    expect(new Set(result.current.activeDelegations)).toEqual(new Set(["a", "b"]));

    act(() => {
      source.emit("voice_delegation_failed", {
        call_id: "a",
        task: "t",
        error: "boom",
        session_id: "s1",
      });
    });
    expect([...result.current.activeDelegations]).toEqual(["b"]);

    act(() => {
      source.emit("voice_delegation_cancelled", {
        call_id: "b",
        task: "t",
        session_id: "s1",
      });
    });
    expect(result.current.activeDelegations.size).toBe(0);
  });

  it("treats a terminal event for an unknown delegation as a no-op (Req 12.3)", () => {
    const { result } = renderHook(() => useVoiceTranscript("s1"));
    const source = FakeEventSource.only();

    act(() => {
      source.emit("voice_delegation_completed", {
        call_id: "ghost",
        task: "t",
        result: "r",
        session_id: "s1",
      });
    });

    expect(result.current.activeDelegations.size).toBe(0);
  });

  it("closes the EventSource on unmount", () => {
    const { unmount } = renderHook(() => useVoiceTranscript("s1"));
    const source = FakeEventSource.only();
    expect(source.closed).toBe(false);

    unmount();

    expect(source.closed).toBe(true);
    expect(FakeEventSource.live()).toHaveLength(0);
  });

  it("closes the old EventSource and opens a new one on sessionId change", () => {
    const { rerender } = renderHook(
      ({ sessionId }: { sessionId: string | null }) => useVoiceTranscript(sessionId),
      { initialProps: { sessionId: "s1" as string | null } },
    );
    const first = FakeEventSource.only();
    expect(first.url).toBe("/api/sessions/s1/voice/events");

    rerender({ sessionId: "s2" });

    // The prior stream is closed; a single new stream targets the new session.
    expect(first.closed).toBe(true);
    const second = FakeEventSource.only();
    expect(second).not.toBe(first);
    expect(second.url).toBe("/api/sessions/s2/voice/events");
  });

  it("resets captions and delegations when sessionId changes", () => {
    const { result, rerender } = renderHook(
      ({ sessionId }: { sessionId: string | null }) => useVoiceTranscript(sessionId),
      { initialProps: { sessionId: "s1" as string | null } },
    );
    const first = FakeEventSource.only();

    act(() => {
      emitTranscript(first, "user", "old session");
      first.emit("voice_delegation_started", {
        call_id: "c",
        task: "t",
        session_id: "s1",
      });
    });
    expect(result.current.captions).toHaveLength(1);
    expect(result.current.activeDelegations.size).toBe(1);

    rerender({ sessionId: "s2" });

    // State is cleared for the new session.
    expect(result.current.captions).toEqual([]);
    expect(result.current.activeDelegations.size).toBe(0);
  });

  it("opens no EventSource when sessionId is null", () => {
    const { result } = renderHook(() => useVoiceTranscript(null));

    expect(FakeEventSource.instances).toHaveLength(0);
    expect(result.current.captions).toEqual([]);
    expect(result.current.activeDelegations.size).toBe(0);
  });

  it("opens a stream once sessionId becomes non-null, and closes it when it returns to null", () => {
    const { rerender } = renderHook(
      ({ sessionId }: { sessionId: string | null }) => useVoiceTranscript(sessionId),
      { initialProps: { sessionId: null as string | null } },
    );
    expect(FakeEventSource.instances).toHaveLength(0);

    rerender({ sessionId: "s9" });
    const source = FakeEventSource.only();
    expect(source.url).toBe("/api/sessions/s9/voice/events");

    rerender({ sessionId: null });
    expect(source.closed).toBe(true);
    expect(FakeEventSource.live()).toHaveLength(0);
  });
});
