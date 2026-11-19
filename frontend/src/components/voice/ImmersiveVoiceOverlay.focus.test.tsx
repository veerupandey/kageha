import "@testing-library/jest-dom/vitest";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  act,
  cleanup,
  fireEvent,
  render,
  waitFor,
} from "@testing-library/react";
import { createRef, useRef, type RefObject } from "react";

import { ImmersiveVoiceOverlay } from "./ImmersiveVoiceOverlay";
import type { UseGeminiLiveVoice } from "./ImmersiveVoiceOverlay";
import { getFocusableElements } from "../../lib/focusTrap";
import type { GeminiLivePhase, GeminiLiveStatus } from "./voiceTypes";

/**
 * Unit tests for ImmersiveVoiceOverlay keyboard / focus accessibility:
 * initial focus moves inside the overlay on open (Req 9.1), Tab / Shift+Tab
 * wrap within the overlay's focusable controls (Req 9.2), Escape closes the
 * overlay (Req 2.3, 9.6), and focus is restored to the opener on close
 * (Req 2.4, 9.7) or falls back to the chat view when the opener is absent
 * (Req 2.5).
 *
 * Mirrors the conventions of ImmersiveVoiceOverlay.session.test.tsx: a minimal
 * fake EventSource stub (the overlay opens a per-session SSE stream), a
 * makeVoice(status, phase) helper for the mocked hook, and a Harness that
 * bridges a real opener button into openerRef. Uses fireEvent (the frontend
 * has no @testing-library/user-event).
 */

// --- Minimal fake EventSource -------------------------------------------------
// The overlay's useVoiceTranscript hook constructs `new EventSource(url)` and
// calls addEventListener/close. This fake satisfies that surface without doing
// anything else; captions are not exercised by these focus tests.
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
 *
 * `withOpener` controls whether the harness renders the opener button; when it
 * is false, a `<main>` element stands in for the underlying chat view so the
 * opener-absent focus fallback (Req 2.5) can be exercised.
 */
function renderOverlay(props: {
  open: boolean;
  voice: UseGeminiLiveVoice;
  sessionId?: string | null;
  onClose?: () => void;
  withOpener?: boolean;
}) {
  const onClose = props.onClose ?? vi.fn();
  const withOpener = props.withOpener ?? true;
  const openerRef: RefObject<HTMLElement | null> = createRef<HTMLElement>();

  function Harness({
    open,
    voice,
    sessionId,
    showOpener,
  }: {
    open: boolean;
    voice: UseGeminiLiveVoice;
    sessionId: string | null;
    showOpener: boolean;
  }) {
    const ref = useRef<HTMLButtonElement | null>(null);
    // Bridge the harness button into the shared openerRef.
    if (ref.current) openerRef.current = ref.current;
    return (
      <div>
        {showOpener ? (
          <button ref={ref} type="button">
            open immersive
          </button>
        ) : (
          // Stand-in for the underlying chat view; the overlay's fallback
          // selector is `[data-chat-view]` then `main` (Req 2.5).
          <main>chat view</main>
        )}
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
      showOpener={withOpener}
    />,
  );

  const rerender = (next: {
    open: boolean;
    voice: UseGeminiLiveVoice;
    sessionId?: string | null;
    withOpener?: boolean;
  }) =>
    utils.rerender(
      <Harness
        open={next.open}
        voice={next.voice}
        sessionId={next.sessionId === undefined ? "s1" : next.sessionId}
        showOpener={next.withOpener ?? withOpener}
      />,
    );

  return { ...utils, rerender, onClose, openerRef };
}

/** Locate the overlay dialog container (portaled to document.body). */
function getOverlay(): HTMLElement {
  const dialog = document.querySelector<HTMLElement>('[role="dialog"]');
  if (!dialog) throw new Error("overlay dialog not found");
  return dialog;
}

beforeEach(() => {
  FakeEventSource.reset();
  (globalThis as { EventSource: unknown }).EventSource =
    FakeEventSource as unknown as typeof EventSource;
});

afterEach(() => {
  cleanup();
  (globalThis as { EventSource: unknown }).EventSource = originalEventSource;
  vi.restoreAllMocks();
});

describe("ImmersiveVoiceOverlay — initial focus on open (Req 9.1)", () => {
  it("moves focus to a focusable control within the overlay on open", async () => {
    const voice = makeVoice("active", "listening");
    renderOverlay({ open: true, voice });

    const overlay = getOverlay();

    // useFocusTrap focuses the initial element on the next animation frame.
    await waitFor(() => {
      expect(overlay.contains(document.activeElement)).toBe(true);
    });

    // Focus must be on an actual focusable control, not the container itself.
    expect(document.activeElement).not.toBe(document.body);
    const focusables = getFocusableElements(overlay);
    expect(focusables.length).toBeGreaterThan(0);
    expect(focusables).toContain(document.activeElement as HTMLElement);
  });
});

describe("ImmersiveVoiceOverlay — Tab / Shift+Tab wrap within the overlay (Req 9.2)", () => {
  it("Tab from the last focusable wraps to the first, keeping focus inside", async () => {
    const voice = makeVoice("active", "listening");
    renderOverlay({ open: true, voice });

    const overlay = getOverlay();
    await waitFor(() => {
      expect(overlay.contains(document.activeElement)).toBe(true);
    });

    const focusables = getFocusableElements(overlay);
    expect(focusables.length).toBeGreaterThan(1);
    const first = focusables[0];
    const last = focusables[focusables.length - 1];

    // Move focus to the last control, then Tab forward.
    act(() => {
      last.focus();
    });
    expect(document.activeElement).toBe(last);

    fireEvent.keyDown(last, { key: "Tab" });

    // Focus wrapped to the first control and remained inside the overlay.
    expect(document.activeElement).toBe(first);
    expect(overlay.contains(document.activeElement)).toBe(true);
  });

  it("Shift+Tab from the first focusable wraps to the last, keeping focus inside", async () => {
    const voice = makeVoice("active", "listening");
    renderOverlay({ open: true, voice });

    const overlay = getOverlay();
    await waitFor(() => {
      expect(overlay.contains(document.activeElement)).toBe(true);
    });

    const focusables = getFocusableElements(overlay);
    expect(focusables.length).toBeGreaterThan(1);
    const first = focusables[0];
    const last = focusables[focusables.length - 1];

    act(() => {
      first.focus();
    });
    expect(document.activeElement).toBe(first);

    fireEvent.keyDown(first, { key: "Tab", shiftKey: true });

    // Focus wrapped to the last control and remained inside the overlay.
    expect(document.activeElement).toBe(last);
    expect(overlay.contains(document.activeElement)).toBe(true);
  });
});

describe("ImmersiveVoiceOverlay — Escape closes the overlay (Req 2.3, 9.6)", () => {
  it("pressing Escape while open calls onClose", async () => {
    const voice = makeVoice("active", "listening");
    const { onClose } = renderOverlay({ open: true, voice });

    const overlay = getOverlay();
    await waitFor(() => {
      expect(overlay.contains(document.activeElement)).toBe(true);
    });

    fireEvent.keyDown(document, { key: "Escape" });

    expect(onClose).toHaveBeenCalledTimes(1);
  });
});

describe("ImmersiveVoiceOverlay — focus restore on close (Req 2.4, 2.5, 9.7)", () => {
  it("restores focus to the opener when it is still present (Req 2.4, 9.7)", async () => {
    const voice = makeVoice("active", "listening");
    const { rerender, openerRef } = renderOverlay({ open: true, voice });

    const overlay = getOverlay();
    await waitFor(() => {
      expect(overlay.contains(document.activeElement)).toBe(true);
    });

    // Close the overlay.
    rerender({ open: false, voice });

    const opener = openerRef.current;
    expect(opener).not.toBeNull();
    expect(document.activeElement).toBe(opener);
  });

  it("falls back to the chat view when the opener is absent (Req 2.5)", async () => {
    const voice = makeVoice("active", "listening");
    // Render without an opener button; a <main> stands in for the chat view.
    const { rerender } = renderOverlay({
      open: true,
      voice,
      withOpener: false,
    });

    const overlay = getOverlay();
    await waitFor(() => {
      expect(overlay.contains(document.activeElement)).toBe(true);
    });

    // Close the overlay; the opener never existed so focus should fall back to
    // the underlying chat view (`main`).
    rerender({ open: false, voice, withOpener: false });

    const main = document.querySelector<HTMLElement>("main");
    expect(main).not.toBeNull();
    expect(document.activeElement).toBe(main);
    // The overlay marks the fallback focusable via tabindex when needed.
    expect(main).toHaveAttribute("tabindex", "-1");
  });
});
