import "@testing-library/jest-dom/vitest";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { Composer } from "./Composer";
import { SLASH_COMMANDS } from "../api/slashCatalog";
import { useAppStore } from "../store";
import { DEFAULT_PREFS } from "../store/prefs";
import type { GeminiLivePhase, GeminiLiveStatus } from "./voice/voiceTypes";

/**
 * Integration tests for the composer -> immersive overlay wiring (task 12.2).
 *
 * These exercise the REAL Composer and the REAL ImmersiveVoiceOverlay together:
 * the enter-immersive control (#btn-enter-immersive) opens the overlay wired
 * with the single shared useGeminiLiveVoice instance, the sessionId, and the
 * openerRef. Only the leaf hooks that reach the network (useGeminiLiveVoice,
 * useComposerController) and the browser EventSource are stubbed.
 *
 * Covered acceptance criteria:
 * - Enter opens the overlay reusing the active session (Req 1.1, 1.2).
 * - Enter control has a non-empty accessible name (Req 9.3).
 * - Exit returns to the composer keeping the session (Req 2.1, 2.2).
 * - No-thread activation cannot open the active view (Req 11.3, 11.4).
 */

// --- Trim the heavy composer sub-menus so the tree renders in jsdom. ---------
vi.mock("./ComposerModelMenu", () => ({ ComposerModelMenu: () => null }));
vi.mock("./ComposerSearchMenu", () => ({ ComposerSearchMenu: () => null }));
vi.mock("./ComposerMediaMenu", () => ({ ComposerMediaMenu: () => null }));
vi.mock("./ModelRoleControls", () => ({ ModelRoleControls: () => null }));
vi.mock("./AgentModeSwitcher", () => ({ AgentModeSwitcher: () => null }));
vi.mock("./ExecutionTargetChip", () => ({ ExecutionTargetChip: () => null }));
vi.mock("./TaskStatusChip", () => ({ TaskStatusChip: () => null }));

// --- useComposerController: supplies the sessionId gating the enter button. --
// The enter-immersive button is disabled when !sessionId and the overlay's
// sessionId prop comes from this controller's sessionId, so it drives both the
// enabled/no-thread cases.
let controllerSessionId: string | null = "s1";
vi.mock("./useComposerController", () => ({
  useComposerController: () => ({
    addPendingFiles: vi.fn(),
    handleMic: vi.fn(),
    pendingFiles: [],
    recording: false,
    removePendingFile: vi.fn(),
    sending: false,
    sessionId: controllerSessionId,
    stopGeneration: vi.fn(),
    submit: vi.fn(),
    transcribing: false,
  }),
}));

// --- useGeminiLiveVoice: one shared instance for the button + overlay. -------
// Returns a stable object so the same reference flows into both the composer
// live-voice button and the ImmersiveVoiceOverlay (single-session continuity).
const liveVoiceStart = vi.fn();
const liveVoiceStop = vi.fn();
const liveVoiceToggle = vi.fn();
let liveVoiceStatus: GeminiLiveStatus = "idle";
let liveVoicePhase: GeminiLivePhase = "idle";
vi.mock("./useGeminiLiveVoice", () => ({
  useGeminiLiveVoice: () => ({
    get status() {
      return liveVoiceStatus;
    },
    get phase() {
      return liveVoicePhase;
    },
    levelTargetRef: { current: null },
    start: liveVoiceStart,
    stop: liveVoiceStop,
    toggle: liveVoiceToggle,
  }),
}));

// --- Minimal fake EventSource -------------------------------------------------
// The overlay's useVoiceTranscript opens a per-session voice-events stream when
// the overlay is open. This fake satisfies the constructor/addEventListener/
// close surface without touching the network.
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

function resetStore() {
  useAppStore.setState({
    draft: "",
    agentMode: "normal",
    composerChip: { kind: null, value: null },
    slashCatalog: SLASH_COMMANDS.slice(),
    pendingFiles: [],
    sessionId: "s1",
    sending: false,
    prefs: { ...DEFAULT_PREFS },
    capabilities: {
      projectFiles: false,
      cometApi: null,
      browserApi: null,
      computerApi: null,
      machineApi: null,
      modelApi: null,
      slashCatalogApi: false,
    },
  });
}

const ENTER_LABEL = "Open immersive live voice view";
const OVERLAY_LABEL = "Immersive live voice";

beforeAll(() => {
  HTMLElement.prototype.scrollIntoView = () => {};
});

beforeEach(() => {
  FakeEventSource.reset();
  (globalThis as { EventSource: unknown }).EventSource =
    FakeEventSource as unknown as typeof EventSource;
  controllerSessionId = "s1";
  liveVoiceStatus = "idle";
  liveVoicePhase = "idle";
});

afterEach(() => {
  cleanup();
  resetStore();
  (globalThis as { EventSource: unknown }).EventSource = originalEventSource;
  liveVoiceStart.mockClear();
  liveVoiceStop.mockClear();
  liveVoiceToggle.mockClear();
});

describe("Composer immersive — enter control accessible name (Req 9.3)", () => {
  it("exposes a non-empty accessible name on the enter-immersive control", () => {
    resetStore();
    render(<Composer />);
    const enter = screen.getByRole("button", { name: ENTER_LABEL });
    expect(enter).toBeInTheDocument();
    expect(enter).toHaveAttribute("id", "btn-enter-immersive");
    expect((enter.getAttribute("aria-label") ?? "").trim().length).toBeGreaterThan(0);
  });
});

describe("Composer immersive — enter opens the overlay reusing the session (Req 1.1, 1.2)", () => {
  it("does not render the overlay until the enter control is activated", () => {
    liveVoiceStatus = "active";
    liveVoicePhase = "listening";
    resetStore();
    render(<Composer />);
    expect(screen.queryByRole("dialog", { name: OVERLAY_LABEL })).toBeNull();
  });

  it("opens the immersive overlay when the enter control is clicked (Req 1.1)", () => {
    // An already-active session so the overlay shows the active view on open.
    liveVoiceStatus = "active";
    liveVoicePhase = "listening";
    resetStore();
    render(<Composer />);

    fireEvent.click(screen.getByRole("button", { name: ENTER_LABEL }));

    const overlay = screen.getByRole("dialog", { name: OVERLAY_LABEL });
    expect(overlay).toBeInTheDocument();
    // Active view rendered: transcript log + exit/end controls.
    expect(
      screen.getByRole("log", { name: "Conversation transcript" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Exit immersive view" }),
    ).toBeInTheDocument();
  });

  it("reuses the active session without starting a new one (Req 1.2)", () => {
    liveVoiceStatus = "active";
    liveVoicePhase = "listening";
    resetStore();
    render(<Composer />);

    fireEvent.click(screen.getByRole("button", { name: ENTER_LABEL }));

    // Opening onto an active session must not invoke start().
    expect(liveVoiceStart).not.toHaveBeenCalled();
  });

  it("marks the enter control expanded once the overlay is open", () => {
    liveVoiceStatus = "active";
    liveVoicePhase = "listening";
    resetStore();
    render(<Composer />);

    const enter = screen.getByRole("button", { name: ENTER_LABEL });
    expect(enter).toHaveAttribute("aria-expanded", "false");
    fireEvent.click(enter);
    expect(enter).toHaveAttribute("aria-expanded", "true");
  });
});

describe("Composer immersive — exit returns to composer keeping the session (Req 2.1, 2.2)", () => {
  it("closes the overlay (composer visible again) without stopping the session", () => {
    liveVoiceStatus = "active";
    liveVoicePhase = "listening";
    resetStore();
    render(<Composer />);

    fireEvent.click(screen.getByRole("button", { name: ENTER_LABEL }));
    expect(screen.getByRole("dialog", { name: OVERLAY_LABEL })).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Exit immersive view" }));

    // Overlay closed -> composer's enter control visible again, no dialog.
    expect(screen.queryByRole("dialog", { name: OVERLAY_LABEL })).toBeNull();
    expect(screen.getByRole("button", { name: ENTER_LABEL })).toBeInTheDocument();
    // Exit must NOT stop the underlying session (Req 2.2).
    expect(liveVoiceStop).not.toHaveBeenCalled();
  });
});

describe("Composer immersive — no-thread activation (Req 11.3, 11.4)", () => {
  it("disables the enter control when there is no session/thread", () => {
    controllerSessionId = null;
    resetStore();
    useAppStore.setState({ sessionId: null });
    render(<Composer />);

    // With no thread the enter control is disabled, so the active view can
    // never open (Req 11.3, 11.4).
    expect(screen.getByRole("button", { name: ENTER_LABEL })).toBeDisabled();
  });

  it("does not open an active conversation view while disabled with no thread", () => {
    controllerSessionId = null;
    liveVoiceStatus = "active";
    liveVoicePhase = "listening";
    resetStore();
    useAppStore.setState({ sessionId: null });
    render(<Composer />);

    // Attempting to click the disabled control is a no-op; no overlay opens.
    fireEvent.click(screen.getByRole("button", { name: ENTER_LABEL }));
    expect(screen.queryByRole("dialog", { name: OVERLAY_LABEL })).toBeNull();
    expect(
      screen.queryByRole("log", { name: "Conversation transcript" }),
    ).toBeNull();
  });
});
