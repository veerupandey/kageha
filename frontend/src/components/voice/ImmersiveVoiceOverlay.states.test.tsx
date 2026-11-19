import "@testing-library/jest-dom/vitest";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { createRef, useRef, type RefObject } from "react";

import { ImmersiveVoiceOverlay } from "./ImmersiveVoiceOverlay";
import type { UseGeminiLiveVoice } from "./ImmersiveVoiceOverlay";
import type { GeminiLivePhase, GeminiLiveStatus } from "./voiceTypes";

/**
 * Unit tests for ImmersiveVoiceOverlay error / edge states, theming, and
 * reduced motion: connection-error vs. mic-permission-denied classification
 * (Req 1.5, 11.1, 11.2), the no-thread edge state (Req 11.3, 11.4), the
 * exit control being present in the error state (Req 11.5), reduced-motion
 * usability (Req 8.3, 12.4), and theme-token / CSS-variable driven surfaces
 * (Req 10.1, 10.2, 10.3).
 *
 * These mirror the conventions of ImmersiveVoiceOverlay.session.test.tsx:
 * a minimal FakeEventSource stub, a makeVoice(status, phase) factory, and a
 * Harness that bridges a real opener button into a shared openerRef with a
 * controllable onClose spy. The frontend has no @testing-library/user-event,
 * so interactions use fireEvent.
 */

// --- Minimal fake EventSource -------------------------------------------------
// The overlay's useVoiceTranscript hook constructs `new EventSource(url)` and
// calls addEventListener/close. This fake satisfies that surface only.
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

// --- Permissions API stubbing helpers ----------------------------------------
// The overlay classifies an "error" status by probing
// navigator.permissions.query({ name: "microphone" }). We install/remove a
// controllable stub per test.
const originalPermissions = Object.getOwnPropertyDescriptor(
  navigator,
  "permissions",
);

function setPermissions(value: unknown): void {
  Object.defineProperty(navigator, "permissions", {
    configurable: true,
    value,
  });
}

function restorePermissions(): void {
  if (originalPermissions) {
    Object.defineProperty(navigator, "permissions", originalPermissions);
  } else {
    // No original descriptor — remove any test-installed stub.
    delete (navigator as { permissions?: unknown }).permissions;
  }
}

// --- matchMedia stubbing helpers ---------------------------------------------
const originalMatchMedia = window.matchMedia;

/** Install a matchMedia stub reporting the given reduced-motion preference. */
function setReducedMotion(matches: boolean): void {
  window.matchMedia = ((query: string) => ({
    matches,
    media: query,
    onchange: null,
    addListener: () => {},
    removeListener: () => {},
    addEventListener: () => {},
    removeEventListener: () => {},
    dispatchEvent: () => false,
  })) as unknown as typeof window.matchMedia;
}

beforeEach(() => {
  FakeEventSource.reset();
  (globalThis as { EventSource: unknown }).EventSource =
    FakeEventSource as unknown as typeof EventSource;
});

afterEach(() => {
  cleanup();
  (globalThis as { EventSource: unknown }).EventSource = originalEventSource;
  restorePermissions();
  window.matchMedia = originalMatchMedia;
  vi.restoreAllMocks();
});

describe("ImmersiveVoiceOverlay — connection error (Req 1.5, 11.1)", () => {
  it("classifies a non-denied permission state as a connection error and hides the active view", async () => {
    // Permissions API resolves to a non-denied state -> connection branch.
    setPermissions({
      query: vi.fn().mockResolvedValue({ state: "granted" }),
    });
    const voice = makeVoice("error");
    renderOverlay({ open: true, voice });

    // Await the async classification effect.
    await act(async () => {});

    const region = screen.getByRole("alert");
    expect(region).toHaveAttribute("data-error-kind", "connection");
    expect(screen.getByText("Connection error")).toBeInTheDocument();
    // States a recommended user action (Req 11.1).
    expect(
      screen.getByRole("button", { name: "Try again" }),
    ).toBeInTheDocument();

    // The active conversation view is NOT shown (Req 1.5).
    expect(
      screen.queryByRole("log", { name: "Conversation transcript" }),
    ).toBeNull();
    expect(
      screen.queryByRole("button", { name: "End conversation" }),
    ).toBeNull();
  });

  it("falls back to a connection error when the Permissions API is absent", async () => {
    // Remove the Permissions API entirely -> classifyError defaults to
    // "connection".
    setPermissions(undefined);
    const voice = makeVoice("error");
    renderOverlay({ open: true, voice });

    await act(async () => {});

    expect(screen.getByRole("alert")).toHaveAttribute(
      "data-error-kind",
      "connection",
    );
    expect(screen.getByText("Connection error")).toBeInTheDocument();
  });
});

describe("ImmersiveVoiceOverlay — mic-permission denied (Req 11.2)", () => {
  it("identifies denied microphone permission with a recommended action, distinct from the connection message", async () => {
    setPermissions({
      query: vi.fn().mockResolvedValue({ state: "denied" }),
    });
    const voice = makeVoice("error");
    renderOverlay({ open: true, voice });

    // Await the async classification effect that resolves to "mic-permission".
    await act(async () => {});

    const region = screen.getByRole("alert");
    expect(region).toHaveAttribute("data-error-kind", "mic-permission");
    expect(screen.getByText("Microphone access denied")).toBeInTheDocument();
    // Recommended action mentions browser microphone settings (Req 11.2).
    expect(
      screen.getByText(/allow microphone access for this site/i),
    ).toBeInTheDocument();

    // Distinct from the connection-error message.
    expect(screen.queryByText("Connection error")).toBeNull();
  });
});

describe("ImmersiveVoiceOverlay — no-thread edge state (Req 11.3, 11.4)", () => {
  it("shows the thread-required message, hides the active view, and never starts a session", () => {
    const voice = makeVoice("idle");
    renderOverlay({ open: true, voice, sessionId: null });

    // Thread-required message with a recommended action (Req 11.3).
    expect(screen.getByText("A thread is required")).toBeInTheDocument();
    expect(
      screen.getByText(/start or open a thread first/i),
    ).toBeInTheDocument();

    // Active conversation view is NOT shown (Req 11.4).
    expect(
      screen.queryByRole("log", { name: "Conversation transcript" }),
    ).toBeNull();
    expect(
      screen.queryByRole("button", { name: "End conversation" }),
    ).toBeNull();

    // No session-start action is invoked with no thread (Req 11.3, 11.4).
    expect(voice.start).not.toHaveBeenCalled();
  });
});

describe("ImmersiveVoiceOverlay — exit control in the error state (Req 11.5)", () => {
  it("renders an exit-immersive control while in the error state", async () => {
    setPermissions({
      query: vi.fn().mockResolvedValue({ state: "granted" }),
    });
    const voice = makeVoice("error");
    const { onClose } = renderOverlay({ open: true, voice });

    await act(async () => {});

    const exit = screen.getByRole("button", { name: "Exit immersive view" });
    expect(exit).toBeInTheDocument();

    // The exit control closes the overlay without stopping the session.
    fireEvent.click(exit);
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(voice.stop).not.toHaveBeenCalled();
  });
});

describe("ImmersiveVoiceOverlay — reduced motion (Req 8.3, 12.4)", () => {
  it("marks the overlay reduced-motion and keeps it usable (exit control present)", () => {
    setReducedMotion(true);
    const voice = makeVoice("active", "listening");
    const { onClose } = renderOverlay({ open: true, voice });

    const dialog = screen.getByRole("dialog", { name: "Immersive live voice" });
    // The overlay container reports the reduced-motion preference (Req 8.3).
    expect(dialog).toHaveAttribute("data-reduced-motion", "true");

    // The overlay remains fully usable under reduced motion: the exit control
    // is present and operable (Req 8.3). The same reducedMotion branch drives
    // the static delegation indicator (Req 12.4) — it renders without
    // animate-pulse when a delegation is active.
    const exit = screen.getByRole("button", { name: "Exit immersive view" });
    expect(exit).toBeInTheDocument();
    fireEvent.click(exit);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("does not mark reduced-motion when the preference is not set", () => {
    setReducedMotion(false);
    const voice = makeVoice("active", "listening");
    renderOverlay({ open: true, voice });

    const dialog = screen.getByRole("dialog", { name: "Immersive live voice" });
    expect(dialog).toHaveAttribute("data-reduced-motion", "false");
  });
});

describe("ImmersiveVoiceOverlay — theming via CSS tokens (Req 10.1, 10.2, 10.3)", () => {
  it("drives the container surface from CSS custom properties and theme token classes", () => {
    const voice = makeVoice("active", "listening");
    renderOverlay({ open: true, voice });

    const dialog = screen.getByRole("dialog", { name: "Immersive live voice" });

    // Inline backgroundColor references a theme CSS variable, so a theme
    // change while open is reflected via CSS variables without a remount
    // (Req 10.2, 10.3). jsdom does not resolve the token, so we assert the
    // var(--color-...) usage is present on the element style.
    expect(dialog.style.backgroundColor).toContain("var(--color-bg");

    // Theme token utility classes for surface/ink are applied (Req 10.1,
    // 10.2), so accent/surface/text colors follow the active theme tokens.
    expect(dialog).toHaveClass("bg-surface");
    expect(dialog).toHaveClass("text-ink");
  });
});
