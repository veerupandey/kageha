import { act, renderHook, waitFor, cleanup } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useGeminiLiveVoice } from "./useGeminiLiveVoice";
import { useAppStore } from "../store";
import { startGeminiLiveSession, type GeminiLiveHandlers } from "../lib/geminiLiveClient";

vi.mock("../lib/geminiLiveClient", () => ({
  startGeminiLiveSession: vi.fn(),
}));

/**
 * Capture the handlers the hook hands to `startGeminiLiveSession` so the test
 * can drive `onOpen` / `onPhase` exactly as the real transport would, and
 * return a controllable fake session whose `stop` flips `active` to false.
 */
function makeFakeSession() {
  let active = true;
  const stop = vi.fn(() => {
    active = false;
  });
  return {
    get active() {
      return active;
    },
    stop,
    session_id: "voice-test",
  };
}

describe("useGeminiLiveVoice — no re-render storm", () => {
  let capturedHandlers: GeminiLiveHandlers | null;

  beforeEach(() => {
    vi.clearAllMocks();
    capturedHandlers = null;
    useAppStore.setState({ sessionId: "voice-test", showToast: vi.fn() });
    document.documentElement.style.removeProperty("--ka-live-mic-level");

    vi.mocked(startGeminiLiveSession).mockImplementation(async (_sessionId, handlers = {}) => {
      capturedHandlers = handlers;
      return makeFakeSession() as never;
    });
  });

  afterEach(() => {
    cleanup();
    document.documentElement.style.removeProperty("--ka-live-mic-level");
  });

  function rootMicLevel(): string {
    return document.documentElement.style.getPropertyValue("--ka-live-mic-level");
  }

  it("writes the mic level to :root on every onPhase but changes phase state at most once per transition (Req 7.1, 7.2)", async () => {
    // Track every distinct value of the React-managed `phase` the hook returns,
    // in render order, so we can assert phase state only advances on a genuine
    // transition even though onPhase fires on every mic chunk.
    const phaseRenders: string[] = [];
    const { result } = renderHook(() => {
      const hook = useGeminiLiveVoice();
      phaseRenders.push(hook.phase);
      return hook;
    });

    act(() => result.current.start());
    await waitFor(() => expect(capturedHandlers).not.toBeNull());

    // Transport opens; session becomes active.
    act(() => capturedHandlers!.onOpen?.());
    await waitFor(() => expect(result.current.status).toBe("active"));

    // Spy on setProperty AFTER open so we count only mic-chunk writes.
    const setPropSpy = vi.spyOn(document.documentElement.style, "setProperty");

    // Fire onPhase repeatedly with the SAME phase but changing mic levels.
    // Every call must publish the level to :root; none may change phase state.
    act(() => {
      capturedHandlers!.onPhase?.("listening", 0.1);
      capturedHandlers!.onPhase?.("listening", 0.4);
      capturedHandlers!.onPhase?.("listening", 0.9);
    });

    // Mic level flows on EACH call: :root reflects the most recent value and
    // the CSS var was written once per call.
    const listeningWrites = setPropSpy.mock.calls.filter(
      ([prop]) => prop === "--ka-live-mic-level",
    );
    expect(listeningWrites.length).toBe(3);
    expect(rootMicLevel()).toBe("0.900");

    // Phase changed at most once for this transition: idle -> listening.
    const distinctPhases = phaseRenders.filter(
      (p, i) => i === 0 || p !== phaseRenders[i - 1],
    );
    expect(distinctPhases).toEqual(["idle", "listening"]);
    expect(result.current.phase).toBe("listening");

    // A genuine transition triggers exactly one more phase-state change; repeats
    // of the new phase still stream the mic level without another change.
    act(() => {
      capturedHandlers!.onPhase?.("speaking", 0.2);
      capturedHandlers!.onPhase?.("speaking", 0.5);
    });
    await waitFor(() => expect(result.current.phase).toBe("speaking"));

    const distinctPhasesAfter = phaseRenders.filter(
      (p, i) => i === 0 || p !== phaseRenders[i - 1],
    );
    expect(distinctPhasesAfter).toEqual(["idle", "listening", "speaking"]);
    expect(rootMicLevel()).toBe("0.500");

    setPropSpy.mockRestore();
  });

  it("resets --ka-live-mic-level on :root to \"0\" on stop", async () => {
    const { result } = renderHook(() => useGeminiLiveVoice());

    act(() => result.current.start());
    await waitFor(() => expect(capturedHandlers).not.toBeNull());
    act(() => capturedHandlers!.onOpen?.());
    await waitFor(() => expect(result.current.status).toBe("active"));

    // Publish a non-zero level, then stop.
    act(() => capturedHandlers!.onPhase?.("listening", 0.75));
    expect(rootMicLevel()).toBe("0.750");

    act(() => result.current.stop());

    expect(rootMicLevel()).toBe("0");
    expect(result.current.status).toBe("idle");
    expect(result.current.phase).toBe("idle");
  });
});

describe("useGeminiLiveVoice — start() session id freshness", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    document.documentElement.style.removeProperty("--ka-live-mic-level");
    vi.mocked(startGeminiLiveSession).mockImplementation(async () => {
      return makeFakeSession() as never;
    });
  });

  afterEach(() => {
    cleanup();
    document.documentElement.style.removeProperty("--ka-live-mic-level");
  });

  it("uses an explicit sessionIdOverride even when the store's sessionId hasn't propagated to this render yet", async () => {
    // Regression: a caller that just minted a session via ensureSession()
    // has the new id immediately, but the store write does not retroactively
    // refresh an already-created start() closure — this hook's own
    // subscribed `sessionId` can still read as null/stale at call time.
    // start() must accept and use an explicit override instead of only
    // trusting its own (possibly stale) subscription.
    const showToast = vi.fn();
    useAppStore.setState({ sessionId: null, showToast });
    const { result } = renderHook(() => useGeminiLiveVoice());

    act(() => result.current.start("freshly-minted-session"));

    await waitFor(() => expect(startGeminiLiveSession).toHaveBeenCalledTimes(1));
    expect(startGeminiLiveSession).toHaveBeenCalledWith(
      "freshly-minted-session",
      expect.anything(),
    );
    // No "start a thread" toast — the override made the session usable.
    expect(showToast).not.toHaveBeenCalledWith("Start a thread to use live voice.");
  });

  it("falls back to the live store sessionId when no override is given", async () => {
    useAppStore.setState({ sessionId: "store-session", showToast: vi.fn() });
    const { result } = renderHook(() => useGeminiLiveVoice());

    act(() => result.current.start());

    await waitFor(() => expect(startGeminiLiveSession).toHaveBeenCalledTimes(1));
    expect(startGeminiLiveSession).toHaveBeenCalledWith("store-session", expect.anything());
  });

  it("shows the thread-required toast when there is neither an override nor a store sessionId", async () => {
    const showToast = vi.fn();
    useAppStore.setState({ sessionId: null, showToast });
    const { result } = renderHook(() => useGeminiLiveVoice());

    act(() => result.current.start());

    await waitFor(() =>
      expect(showToast).toHaveBeenCalledWith("Start a thread to use live voice."),
    );
    expect(startGeminiLiveSession).not.toHaveBeenCalled();
  });
});

describe("useGeminiLiveVoice — startup races", () => {
  afterEach(cleanup);

  it("deduplicates pending starts and ignores a session that resolves after stop", async () => {
    vi.clearAllMocks();
    useAppStore.setState({ sessionId: "test", showToast: vi.fn() });
    let resolve!: (session: ReturnType<typeof makeFakeSession>) => void;
    let handlers!: GeminiLiveHandlers;
    vi.mocked(startGeminiLiveSession).mockImplementation((_id, callbacks = {}) => {
      handlers = callbacks;
      return new Promise(done => { resolve = done; });
    });
    const { result } = renderHook(useGeminiLiveVoice);
    act(() => { result.current.start(); result.current.start(); });
    expect(startGeminiLiveSession).toHaveBeenCalledTimes(1);
    act(() => result.current.stop());
    expect(handlers.signal?.aborted).toBe(true);
    const lateSession = makeFakeSession();
    await act(async () => { handlers.onOpen?.(); resolve(lateSession); });
    expect(lateSession.stop).toHaveBeenCalledOnce();
    expect(result.current.status).toBe("idle");
  });

  it("keeps the failure visible when an error is followed by socket close", async () => {
    vi.clearAllMocks();
    useAppStore.setState({ sessionId: "test", showToast: vi.fn() });
    let handlers!: GeminiLiveHandlers;
    vi.mocked(startGeminiLiveSession).mockImplementation(async (_id, callbacks = {}) => {
      handlers = callbacks;
      return makeFakeSession();
    });
    const { result } = renderHook(useGeminiLiveVoice);
    await act(async () => result.current.start());
    act(() => { handlers.onError?.("Provider unavailable"); handlers.onClose?.(); });
    expect(result.current.status).toBe("error");
    expect(result.current.error).toBe("Provider unavailable");
  });
});
