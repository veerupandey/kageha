import "@testing-library/jest-dom/vitest";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { createRef, useRef, type RefObject } from "react";

import { ImmersiveVoiceOverlay } from "./ImmersiveVoiceOverlay";
import type { UseGeminiLiveVoice } from "./ImmersiveVoiceOverlay";
import type { GeminiLivePhase, GeminiLiveStatus } from "./voiceTypes";

/**
 * Unit tests for ImmersiveVoiceOverlay session lifecycle behavior:
 * session reuse-vs-start (Req 1.2, 1.3), the connecting indicator gating the
 * active view (Req 1.4, 1.6), exit keeping the session alive (Req 2.2), end
 * stopping then closing (Req 3.1, 3.2), and session-ended auto-close with the
 * unexpected-end indication (Req 3.3, 2.6).
 *
 * The overlay opens a per-session voice-events SSE stream via useVoiceTranscript,
 * so a minimal fake EventSource is installed globally in beforeEach.
 */

// --- Minimal fake EventSource -------------------------------------------------
// The overlay's useVoiceTranscript hook constructs `new EventSource(url)` and
// calls addEventListener/close. This fake satisfies that surface without doing
// anything else; captions are not exercised by these lifecycle tests.
class FakeEventSource {
  static instances: FakeEventSource[] = [];
  readonly url: string;
  closed = false;
  constructor(url: string) {
    this.url = url;
    FakeEventSource.instances.push(this);
  }
  addEventListener(): void {}
  removeEventListener(): void {}
  close(): void {
    this.closed = true;
  }
  static reset(): void {
    FakeEventSource.instances = [];
  }
}

const originalEventSource = globalThis.EventSource;

/** Build a mock voice hook object with the given status/phase. */
function makeVoice(
  status: GeminiLiveStatus,
  phase: GeminiLivePhase = "idle",
): UseGeminiLiveVoice {
  return {
    status,
    phase,
    levelTargetRef: { current: null },
    start: vi.fn(),
    stop: vi.fn(),
    toggle: vi.fn(),
  } as unknown as UseGeminiLiveVoice;
}

/**
 * Render the overlay with a real opener button (so openerRef points at a live
 * DOM element for focus restore) and a controllable onClose spy.
 */
function renderOverlay(props: {
  open: boolean;
  voice: UseGeminiLiveVoice;
  sessionId?: string | null;
  onClose?: () => void;
}) {
  const onClose = props.onClose ?? vi.fn();
  const openerRef: RefObject<HTMLElement | null> = createRef<HTMLElement>();

  function Harness({
    open,
    voice,
    sessionId,
  }: {
    open: boolean;
    voice: UseGeminiLiveVoice;
    sessionId: string | null;
  }) {
    const ref = useRef<HTMLButtonElement | null>(null);
    // Bridge the harness button into the shared openerRef.
    if (ref.current) openerRef.current = ref.current;
    return (
      <div>
        <button ref={ref} type="button">
          open immersive
        </button>
        <ImmersiveVoiceOverlay
          open={open}
          openerRef={openerRef}
          voice={voice}
          sessionId={sessionId}
          onClose={onClose}
        />
      </div>
    );
  }

  const utils = render(
    <Harness
      open={props.open}
      voice={props.voice}
      sessionId={props.sessionId === undefined ? "s1" : props.sessionId}
    />,
  );

  const rerender = (next: {
    open: boolean;
    voice: UseGeminiLiveVoice;
    sessionId?: string | null;
  }) =>
    utils.rerender(
      <Harness
        open={next.open}
        voice={next.voice}
        sessionId={next.sessionId === undefined ? "s1" : next.sessionId}
      />,
    );

  return { ...utils, rerender, onClose, openerRef };
}

beforeEach(() => {
  FakeEventSource.reset();
  const storage = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => storage.get(key) ?? null,
    setItem: (key: string, value: string) => storage.set(key, String(value)),
    removeItem: (key: string) => storage.delete(key),
    clear: () => storage.clear(),
  });
  (globalThis as { EventSource: unknown }).EventSource =
    FakeEventSource as unknown as typeof EventSource;
});

afterEach(() => {
  cleanup();
  (globalThis as { EventSource: unknown }).EventSource = originalEventSource;
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("ImmersiveVoiceOverlay — session reuse vs. start (Req 1.2, 1.3)", () => {
  it("opens no caption stream while closed and releases it on every close", () => {
    const voice = makeVoice("idle");
    const view = renderOverlay({ open: false, voice });
    expect(FakeEventSource.instances).toHaveLength(0);
    for (let attempt = 0; attempt < 3; attempt++) {
      view.rerender({ open: true, voice });
      expect(FakeEventSource.instances.filter(source => !source.closed)).toHaveLength(1);
      view.rerender({ open: false, voice });
      expect(FakeEventSource.instances.filter(source => !source.closed)).toHaveLength(0);
    }
  });
  it("reuses an existing 'active' session without calling start() (Req 1.2)", () => {
    const voice = makeVoice("active", "listening");
    renderOverlay({ open: true, voice });

    expect(voice.start).not.toHaveBeenCalled();
  });

  it("reuses an existing 'connecting' session without calling start() (Req 1.2)", () => {
    const voice = makeVoice("connecting");
    renderOverlay({ open: true, voice });

    expect(voice.start).not.toHaveBeenCalled();
  });

  it("starts a session when 'idle' at open (Req 1.3)", () => {
    const voice = makeVoice("idle");
    renderOverlay({ open: true, voice });

    expect(voice.start).toHaveBeenCalledTimes(1);
  });

  it("starts a session when 'error' at open (Req 1.3)", async () => {
    const voice = makeVoice("error");
    renderOverlay({ open: true, voice });

    expect(voice.start).toHaveBeenCalledTimes(1);
    // Flush the async error-classification effect so its state update is
    // wrapped in act(...) and does not warn.
    await act(async () => {});
  });

  it("does not start when the overlay is closed", () => {
    const voice = makeVoice("idle");
    renderOverlay({ open: false, voice });

    expect(voice.start).not.toHaveBeenCalled();
  });
});

describe("ImmersiveVoiceOverlay — connecting indicator then active view (Req 1.4, 1.6)", () => {
  it("shows a connecting indicator and hides the active view while 'connecting' (Req 1.4)", () => {
    const voice = makeVoice("connecting");
    renderOverlay({ open: true, voice });

    // Connecting indicator present.
    expect(screen.getByText("Connecting to live voice…")).toBeInTheDocument();
    // Startup can be cancelled, while captions remain hidden.
    expect(
      screen.queryByRole("button", { name: "End conversation" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("log", { name: "Conversation transcript" }),
    ).toBeNull();
  });

  it("shows the active view (captions + end control) while 'active' (Req 1.6)", () => {
    const voice = makeVoice("active", "listening");
    renderOverlay({ open: true, voice });

    // Active view: captions log and the End control are present.
    expect(
      screen.getByRole("log", { name: "Conversation transcript" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "End conversation" }),
    ).toBeInTheDocument();
    // The connecting indicator is gone.
    expect(screen.queryByText("Connecting to live voice…")).toBeNull();
  });
});

describe("ImmersiveVoiceOverlay — exit keeps the session (Req 2.2)", () => {
  it("exit control calls onClose but never stop()", () => {
    const voice = makeVoice("active", "listening");
    const { onClose } = renderOverlay({ open: true, voice });

    fireEvent.click(
      screen.getByRole("button", { name: "Exit immersive view" }),
    );

    expect(onClose).toHaveBeenCalledTimes(1);
    expect(voice.stop).not.toHaveBeenCalled();
  });
});

describe("ImmersiveVoiceOverlay — end stops and closes (Req 3.1, 3.2)", () => {
  it("end control calls stop() then onClose()", () => {
    const voice = makeVoice("active", "speaking");
    const { onClose } = renderOverlay({ open: true, voice });

    const end = screen.getByRole("button", { name: "End conversation" });
    fireEvent.click(end);

    expect(voice.stop).toHaveBeenCalledTimes(1);
    expect(onClose).toHaveBeenCalledTimes(1);
    // stop() is invoked before onClose() (Req 3.1 -> 3.2).
    const stopOrder = (voice.stop as ReturnType<typeof vi.fn>).mock
      .invocationCallOrder[0];
    const closeOrder = (onClose as ReturnType<typeof vi.fn>).mock
      .invocationCallOrder[0];
    expect(stopOrder).toBeLessThan(closeOrder);
  });
});

describe("ImmersiveVoiceOverlay — session-ended auto-close (Req 3.3, 2.6)", () => {
  it("auto-closes when a live session transitions to 'idle' unexpectedly, surfacing the ended indication (Req 3.3, 2.6)", () => {
    // Open onto an active session (so hadSessionAtOpen is true).
    const activeVoice = makeVoice("active", "listening");
    const { rerender, onClose } = renderOverlay({
      open: true,
      voice: activeVoice,
    });

    expect(onClose).not.toHaveBeenCalled();

    // The underlying session ends -> status becomes idle. This was not an
    // end-control action, so it is an unexpected end.
    const endedVoice = makeVoice("idle");
    rerender({ open: true, voice: endedVoice });

    // Auto-close fired (Req 3.3).
    expect(onClose).toHaveBeenCalledTimes(1);
    // The "voice session has ended" indication is surfaced (Req 2.6).
    expect(screen.getByText("The voice session has ended.")).toBeInTheDocument();
  });

  it("does not auto-close on the brief idle window during entry (before connect)", () => {
    // Opening onto an idle session triggers start(); the session has not been
    // observed as live yet, so a lingering idle must NOT be treated as ended.
    const idleVoice = makeVoice("idle");
    const { onClose } = renderOverlay({ open: true, voice: idleVoice });

    expect(idleVoice.start).toHaveBeenCalledTimes(1);
    expect(onClose).not.toHaveBeenCalled();
  });
});

describe("live voice provider selection", () => {
  it("saves the provider and reconnects the current session", () => {
    localStorage.removeItem("kageha.liveVoiceProvider");
    const voice = makeVoice("active", "listening");
    renderOverlay({ open: true, voice, sessionId: "s-provider" });
    fireEvent.change(screen.getByRole("combobox", { name: "Voice provider" }), {
      target: { value: "openai" },
    });
    expect(localStorage.getItem("kageha.liveVoiceProvider")).toBe("openai");
    expect(voice.stop).toHaveBeenCalledTimes(1);
    expect(voice.start).toHaveBeenCalledWith("s-provider");
    localStorage.removeItem("kageha.liveVoiceProvider");
  });
});
