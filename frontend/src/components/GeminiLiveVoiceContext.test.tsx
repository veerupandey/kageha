import "@testing-library/jest-dom/vitest";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render, waitFor } from "@testing-library/react";
import {
  GeminiLiveVoiceProvider,
  useSharedGeminiLiveVoice,
} from "./GeminiLiveVoiceContext";
import { useAppStore } from "../store";
import { startGeminiLiveSession, type GeminiLiveHandlers } from "../lib/geminiLiveClient";

vi.mock("../lib/geminiLiveClient", () => ({
  startGeminiLiveSession: vi.fn(),
}));

/**
 * These tests use the REAL `useGeminiLiveVoice` hook (only the transport
 * `startGeminiLiveSession` is mocked) so the provider's core guarantee is
 * actually exercised: one hook instance, with real component-local
 * `useState`/`useRef`, survives a sibling swap instead of being torn down
 * and recreated.
 *
 * This is the exact scenario that broke in production: clicking "Live" on
 * the home screen mints a session; the moment the store's `sessionId`
 * becomes non-null, `AppShell` swaps `CommandCenter` for `ThreadView` —
 * unmounting whatever was rendering the home composer. If the live-voice
 * hook lived *inside* that unmounted subtree (as it did before this
 * `GeminiLiveVoiceProvider`), the pending `start(mintedSessionId)` call
 * landed on an already-destroyed hook instance and silently did nothing —
 * no toast, no "Connecting…", no error. Lifting the hook to a provider that
 * sits ABOVE the CommandCenter/ThreadView swap keeps the same instance (and
 * therefore the in-flight `start()` call) alive across the transition.
 */

function makeFakeSession() {
  let active = true;
  return {
    get active() {
      return active;
    },
    stop: vi.fn(() => {
      active = false;
    }),
  };
}

function Probe({ onProbe }: { onProbe: (voice: ReturnType<typeof useSharedGeminiLiveVoice>) => void }) {
  const voice = useSharedGeminiLiveVoice();
  onProbe(voice);
  return <span data-testid="status">{voice.status}</span>;
}

/** Mirrors AppShell's `isHome ? <CommandCenter/> : <ThreadView/>` switch: two
 * mutually-exclusive siblings, both inside ONE shared provider. */
function HomeOrThreadSwitch({
  onHome,
  onThread,
}: {
  onHome: boolean;
  onThread: (voice: ReturnType<typeof useSharedGeminiLiveVoice>) => void;
}) {
  return (
    <GeminiLiveVoiceProvider>
      {onHome ? <Probe onProbe={onThread} /> : <Probe onProbe={onThread} />}
    </GeminiLiveVoiceProvider>
  );
}

let capturedHandlers: GeminiLiveHandlers | null;

beforeEach(() => {
  vi.clearAllMocks();
  capturedHandlers = null;
  useAppStore.setState({ sessionId: null, showToast: vi.fn() });
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

describe("GeminiLiveVoiceProvider — survives a CommandCenter <-> ThreadView-style swap", () => {
  it("a start() call in flight during the sibling swap still connects, because both siblings share one hook instance", async () => {
    let latestVoice!: ReturnType<typeof useSharedGeminiLiveVoice>;
    const { rerender } = render(
      <HomeOrThreadSwitch onHome={true} onThread={(v) => (latestVoice = v)} />,
    );

    // Kick off start() while still on the "home" sibling — before a session
    // id exists, mirroring the click firing before ensureSession() resolves.
    act(() => {
      latestVoice.start("remount-session");
    });

    // The "home" sibling unmounts; the "thread" sibling mounts in its place —
    // both inside the SAME provider, so this is not a fresh hook instance.
    rerender(<HomeOrThreadSwitch onHome={false} onThread={(v) => (latestVoice = v)} />);

    // The in-flight start() must still land: startGeminiLiveSession is
    // called with the id that was passed before the swap, and the surviving
    // instance transitions through connecting -> active.
    await waitFor(() => expect(startGeminiLiveSession).toHaveBeenCalledTimes(1));
    expect(startGeminiLiveSession).toHaveBeenCalledWith("remount-session", expect.anything());

    act(() => capturedHandlers?.onOpen?.());
    await waitFor(() => expect(latestVoice.status).toBe("active"));
  });

  it("baseline sanity: two INDEPENDENT (unprovided) hook instances would not share state", async () => {
    // Contrast case proving the provider is doing real work: two separate
    // GeminiLiveVoiceProvider trees (as if each sibling owned its own, the
    // pre-fix structure) do NOT share a session — starting one has no effect
    // on the other's status.
    function Independent({ probeA, probeB }: {
      probeA: (v: ReturnType<typeof useSharedGeminiLiveVoice>) => void;
      probeB: (v: ReturnType<typeof useSharedGeminiLiveVoice>) => void;
    }) {
      return (
        <>
          <GeminiLiveVoiceProvider>
            <Probe onProbe={probeA} />
          </GeminiLiveVoiceProvider>
          <GeminiLiveVoiceProvider>
            <Probe onProbe={probeB} />
          </GeminiLiveVoiceProvider>
        </>
      );
    }

    let voiceA!: ReturnType<typeof useSharedGeminiLiveVoice>;
    let voiceB!: ReturnType<typeof useSharedGeminiLiveVoice>;
    render(<Independent probeA={(v) => (voiceA = v)} probeB={(v) => (voiceB = v)} />);

    act(() => voiceA.start("session-a"));
    await waitFor(() => expect(startGeminiLiveSession).toHaveBeenCalledTimes(1));
    act(() => capturedHandlers?.onOpen?.());

    await waitFor(() => expect(voiceA.status).toBe("active"));
    // B never started its own session — proving each provider tree owns an
    // independent instance, which is exactly why a SINGLE shared provider
    // (as AppShell now uses) is required across the home/thread swap.
    expect(voiceB.status).toBe("idle");
  });
});
