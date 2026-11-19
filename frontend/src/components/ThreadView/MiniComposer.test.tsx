import "@testing-library/jest-dom/vitest";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { SLASH_COMMANDS } from "../../api/slashCatalog";
import { MiniComposer } from "./MiniComposer";
import { GeminiLiveVoiceProvider } from "../GeminiLiveVoiceContext";
import { useAppStore } from "../../store";
import { DEFAULT_PREFS } from "../../store/prefs";
import type { ModelInfo } from "../../api/types";
import { parseSearchSetup } from "../../lib/searchSetup";
import type { GeminiLivePhase, GeminiLiveStatus } from "../voice/voiceTypes";

/**
 * `MiniComposer` reads the live-voice hook via `useSharedGeminiLiveVoice()`,
 * which requires a `GeminiLiveVoiceProvider` ancestor (the hook instance is
 * lifted to `AppShell` in production so it survives the CommandCenter <->
 * ThreadView remount). Wrap every render in these tests the same way.
 */
function renderMiniComposer(props?: { home?: boolean }) {
  return render(
    <GeminiLiveVoiceProvider>
      <MiniComposer {...props} />
    </GeminiLiveVoiceProvider>,
  );
}

// --- Minimal fake EventSource -------------------------------------------------
// ImmersiveVoiceOverlay's useVoiceTranscript opens one whenever mounted with a
// non-null sessionId. This fake satisfies that surface without touching the
// network.
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

// --- useGeminiLiveVoice mock ---------------------------------------------------
// A controllable fake so the live-voice pill's start/stop/connecting states
// can be driven directly, without a real Gemini Live socket.
const liveVoiceStart = vi.fn();
const liveVoiceStop = vi.fn();
const liveVoiceToggle = vi.fn(() => {
  if (liveVoiceStatus === "active" || liveVoiceStatus === "connecting") {
    liveVoiceStop();
  } else {
    liveVoiceStart();
  }
});
let liveVoiceStatus: GeminiLiveStatus = "idle";
let liveVoicePhase: GeminiLivePhase = "idle";
vi.mock("../useGeminiLiveVoice", () => ({
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

const CATALOG: ModelInfo[] = [
  { id: "glm-5.3", family: "GLM", label: "GLM 5.3", latest: true, ready: true },
];

function resetStore() {
  useAppStore.setState({
    draft: "",
    agentMode: "normal",
    composerChip: { kind: null, value: null },
    slashCatalog: SLASH_COMMANDS.slice(),
    personas: [],
    teams: [],
    activePersonaId: "main",
    activeTeamId: null,
    sessionId: "s1",
    sending: false,
    preparingSends: [],
    pendingFiles: [],
    runs: {},
    modelCatalog: CATALOG,
    models: ["glm-5.3"],
    defaultModel: "glm-5.3",
    modelOverride: "glm-5.3",
    searchSetup: parseSearchSetup({
      backend: "zai",
      configured: "auto",
      label: "Z.AI Web Search Prime",
      options: [{ id: "auto", label: "Auto", ready: true }],
    }),
    loadSearch: async () => true,
    prefs: { ...DEFAULT_PREFS, mainModel: "glm-5.3" },
    addPendingFiles: (files) => {
      if (!files) return;
      useAppStore.setState((s) => ({
        pendingFiles: [...s.pendingFiles, ...Array.from(files)],
      }));
    },
    removePendingFile: (index) =>
      useAppStore.setState((s) => ({
        pendingFiles: s.pendingFiles.filter((_, i) => i !== index),
      })),
  });
}

it("shows and cancels local preparation before server admission", () => {
  resetStore();
  const originalCancel = useAppStore.getState().cancelPreparingSend;
  const cancel = vi.fn();
  useAppStore.setState({ preparingSends: [{ id: "p", sessionId: "s1", text: "Draft" }], cancelPreparingSend: cancel });
  renderMiniComposer();
  expect(screen.getByRole("status")).toHaveTextContent("Preparing request…");
  fireEvent.click(screen.getByRole("button", { name: "Cancel request preparation" }));
  expect(cancel).toHaveBeenCalledOnce();
  act(() => useAppStore.setState({ cancelPreparingSend: originalCancel, preparingSends: [] }));
});

beforeAll(() => {
  URL.createObjectURL = vi.fn(() => "blob:preview");
  URL.revokeObjectURL = vi.fn();
  class FakePointerEvent extends Event {
    button: number;
    ctrlKey: boolean;
    pointerType: string;
    constructor(type: string, props: Record<string, unknown> = {}) {
      super(type, props);
      this.button = Number(props.button || 0);
      this.ctrlKey = Boolean(props.ctrlKey);
      this.pointerType = String(props.pointerType || "mouse");
    }
  }
  window.PointerEvent = FakePointerEvent as unknown as typeof PointerEvent;
  HTMLElement.prototype.hasPointerCapture = () => false;
  HTMLElement.prototype.releasePointerCapture = () => {};
  HTMLElement.prototype.setPointerCapture = () => {};
  HTMLElement.prototype.scrollIntoView = () => {};
});

beforeEach(() => {
  FakeEventSource.reset();
  (globalThis as { EventSource: unknown }).EventSource =
    FakeEventSource as unknown as typeof EventSource;
  liveVoiceStatus = "idle";
  liveVoicePhase = "idle";
  liveVoiceStart.mockClear();
  liveVoiceStop.mockClear();
  liveVoiceToggle.mockClear();
});

afterEach(() => {
  cleanup();
  resetStore();
  (globalThis as { EventSource: unknown }).EventSource = originalEventSource;
});

describe("MiniComposer follow-up", () => {
  it("offers Queue, Steer and Send now on the conversation composer", () => {
    const previous = { sendMessage: useAppStore.getState().sendMessage, steerMessage: useAppStore.getState().steerMessage, sendNowMessage: useAppStore.getState().sendNowMessage };
    const send = vi.fn();
    const steer = vi.fn();
    const now = vi.fn();
    useAppStore.setState({ sending: true, draft: "Change the format", sendMessage: send, steerMessage: steer, sendNowMessage: now });
    try {
      renderMiniComposer();
      fireEvent.click(screen.getByRole("button", { name: /^Queue$/ }));
      expect(send).toHaveBeenCalledWith("Change the format");
      fireEvent.click(screen.getByRole("button", { name: /^Steer$/ }));
      expect(steer).toHaveBeenCalledWith("Change the format");
      fireEvent.click(screen.getByRole("button", { name: /^Send now$/ }));
      expect(now).toHaveBeenCalledWith("Change the format");
      const input = screen.getByRole("textbox", { name: "Message Kageha" });
      fireEvent.keyDown(input, { key: "Enter", ctrlKey: true });
      expect(steer).toHaveBeenCalledTimes(2);
      fireEvent.keyDown(input, { key: "Enter" });
      expect(send).toHaveBeenCalledTimes(2);
    } finally {
      act(() => useAppStore.setState(previous));
    }
  });

  it("uses the shared draft populated by welcome suggestions", () => {
    resetStore();
    useAppStore.getState().setDraft("Help me build an app that ");
    renderMiniComposer();
    const input = screen.getByRole("textbox", { name: "Message Kageha" });
    expect(input).toHaveValue("Help me build an app that ");
    fireEvent.change(input, { target: { value: "My next idea" } });
    expect(useAppStore.getState().draft).toBe("My next idea");
  });

  it("shows the model picker on follow-up input", () => {
    resetStore();
    renderMiniComposer();
    expect(screen.getByRole("button", { name: "Model and reasoning effort" })).toHaveTextContent(
      "GLM 5.3",
    );
    fireEvent.click(screen.getByRole("button", { name: "Tools" }));
    expect(screen.getByRole("button", { name: "Web search and vision hop" })).toHaveTextContent(
      "Z.AI search",
    );
  });

  it("pastes an image into a thumbnail chip", () => {
    resetStore();
    renderMiniComposer();
    const image = new File([new Uint8Array([1, 2, 3, 4])], "shot.png", { type: "image/png" });
    const data = {
      items: [{ kind: "file", type: "image/png", getAsFile: () => image }],
      files: [image],
      types: ["Files"],
      getData: () => "",
    };
    fireEvent.paste(screen.getByPlaceholderText(/Add a follow-up/i), {
      clipboardData: data,
    });
    expect(screen.getByTestId("pending-attachments")).toBeInTheDocument();
    expect(screen.getByAltText("shot.png")).toBeInTheDocument();
    expect(useAppStore.getState().pendingFiles).toHaveLength(1);
  });

  it("thumbnails a screenshot whose File blob has no name or type", () => {
    resetStore();
    renderMiniComposer();
    const blob = new File([new Uint8Array([1, 2, 3, 4])], "", { type: "" });
    fireEvent.paste(screen.getByPlaceholderText(/Add a follow-up/i), {
      clipboardData: {
        items: [{ kind: "file", type: "image/png", getAsFile: () => blob }],
        files: [],
        types: ["Files"],
        getData: () => "",
      },
    });
    const pending = useAppStore.getState().pendingFiles;
    expect(pending).toHaveLength(1);
    expect(pending[0].type).toBe("image/png");
    expect(screen.getByTestId("pending-attachments").querySelector("img")).toBeTruthy();
  });
});

describe("MiniComposer live voice", () => {
  it("auto-creates a session before starting live voice from the home screen (no thread yet)", async () => {
    resetStore();
    const ensureSession = vi.fn().mockResolvedValue({ sessionId: "new-s1", threadId: "web-new-s1" });
    useAppStore.setState({ sessionId: null, ensureSession });

    renderMiniComposer({ home: true });

    const live = screen.getByRole("button", { name: "Start live voice" });
    // Never permanently disabled just because there is no session yet — the
    // click itself mints one, mirroring Gemini/ChatGPT's "just start talking".
    expect(live).not.toBeDisabled();

    fireEvent.click(live);

    await waitFor(() => expect(ensureSession).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(liveVoiceStart).toHaveBeenCalledTimes(1));
    // Regression: start() must be called WITH the just-minted session id, not
    // with no argument. useGeminiLiveVoice's own `sessionId` subscription is
    // still stale at this point (the store write from ensureSession() hasn't
    // necessarily re-rendered this component's closure yet), so calling
    // start() with no override previously fell through to the stale/null
    // value and showed "Start a thread to use live voice" even though a
    // session had just been created moments earlier.
    expect(liveVoiceStart).toHaveBeenCalledWith("new-s1");
    // toggle() is not used for the mint-then-start path — start() is called
    // directly once the session exists, so it isn't double-invoked via toggle.
    expect(liveVoiceToggle).not.toHaveBeenCalled();
  });

  it("does not mint a second session when one already exists", () => {
    resetStore();
    const ensureSession = vi.fn().mockResolvedValue({ sessionId: "s1", threadId: "web-s1" });
    useAppStore.setState({ sessionId: "s1", ensureSession });

    renderMiniComposer();
    fireEvent.click(screen.getByRole("button", { name: "Start live voice" }));

    expect(ensureSession).not.toHaveBeenCalled();
    expect(liveVoiceToggle).toHaveBeenCalledTimes(1);
  });

  it("provides a way to stop while the session is active", () => {
    resetStore();
    liveVoiceStatus = "active";
    liveVoicePhase = "listening";
    renderMiniComposer();

    const stopButton = screen.getByRole("button", { name: "Stop live voice" });
    expect(stopButton).not.toBeDisabled();
    expect(stopButton).toHaveTextContent("Stop");

    fireEvent.click(stopButton);
    expect(liveVoiceToggle).toHaveBeenCalledTimes(1);
    expect(liveVoiceStop).toHaveBeenCalledTimes(1);
  });

  it("provides a way to cancel while still connecting, instead of trapping the user", () => {
    resetStore();
    liveVoiceStatus = "connecting";
    renderMiniComposer();

    const button = screen.getByRole("button", { name: "Stop live voice" });
    // Previously disabled during "connecting" with no way to cancel a stuck
    // connection attempt; it must remain clickable.
    expect(button).not.toBeDisabled();

    fireEvent.click(button);
    expect(liveVoiceToggle).toHaveBeenCalledTimes(1);
    expect(liveVoiceStop).toHaveBeenCalledTimes(1);
  });

  it("stops the session from the immersive overlay's End control", () => {
    resetStore();
    liveVoiceStatus = "active";
    liveVoicePhase = "listening";
    renderMiniComposer();

    fireEvent.click(screen.getByRole("button", { name: "Open immersive live voice view" }));
    const end = screen.getByRole("button", { name: "End conversation" });

    fireEvent.click(end);
    expect(liveVoiceStop).toHaveBeenCalledTimes(1);
  });

  it("keeps the live-voice button enabled if minting the session fails", async () => {
    resetStore();
    const ensureSession = vi.fn().mockRejectedValue(new Error("network down"));
    useAppStore.setState({ sessionId: null, ensureSession });

    renderMiniComposer({ home: true });
    const live = screen.getByRole("button", { name: "Start live voice" });

    await act(async () => {
      fireEvent.click(live);
      await Promise.resolve().catch(() => {});
    });

    await waitFor(() => expect(ensureSession).toHaveBeenCalledTimes(1));
    // start() must never be called when the session failed to mint.
    expect(liveVoiceStart).not.toHaveBeenCalled();
    // The button recovers to a clickable state rather than being stuck.
    await waitFor(() => expect(screen.getByRole("button", { name: "Start live voice" })).not.toBeDisabled());
  });

});


describe("MiniComposer autocomplete", () => {
  function type(value: string) {
    const input = screen.getByRole("textbox", { name: "Message Kageha" });
    fireEvent.change(input, { target: { value, selectionStart: value.length, selectionEnd: value.length } });
    return input;
  }

  it.each([false, true])("shows the entire slash catalog on home=%s and selects with Tab", (home) => {
    resetStore();
    renderMiniComposer({ home });
    const input = type("/");
    expect(screen.getByRole("listbox", { name: "Slash commands" })).toBeInTheDocument();
    expect(screen.getAllByRole("option")).toHaveLength(SLASH_COMMANDS.length);
    type("/pl");
    fireEvent.keyDown(input, { key: "Tab" });
    expect(useAppStore.getState().agentMode).toBe("plan");
    expect(useAppStore.getState().draft).toBe("");
    expect(screen.getByTestId("composer-selection")).toHaveTextContent("/plan");
  });

  it("completes live skills and multi-word commands", () => {
    resetStore();
    useAppStore.setState({ slashCatalog: [...SLASH_COMMANDS, { id: "skill-my_skill", label: "/my_skill", kind: "skill", description: "Custom skill" }] });
    renderMiniComposer();
    const input = type("/my_s");
    fireEvent.keyDown(input, { key: "Enter" });
    expect(useAppStore.getState().draft).toBe("/my_skill ");
    type("/permissions a");
    expect(screen.getAllByRole("option")).toHaveLength(2);
  });

  it("filters agent names and selects without sending", () => {
    resetStore();
    useAppStore.setState({ personas: [{ schema_version: 1, id: "writer", name: "Writer", description: "Writes content" }, { schema_version: 1, id: "researcher", name: "Researcher", description: "Finds sources" }] });
    renderMiniComposer();
    const input = type("@rese");
    expect(screen.getAllByRole("option")).toHaveLength(1);
    fireEvent.keyDown(input, { key: "Enter" });
    expect(useAppStore.getState().activePersonaId).toBe("researcher");
    expect(useAppStore.getState().draft).toBe("@researcher ");
    expect(screen.getByTestId("composer-selection")).toHaveTextContent("Researcher");
  });

  it.each(["@", "&"])("finds teams with %s and inserts native team routing", (sigil) => {
    resetStore();
    useAppStore.setState({ teams: [{ id: "founders", name: "Founders", agents: ["writer"], mode: "coordinate" }] });
    renderMiniComposer();
    const input = type(`${sigil}found`);
    fireEvent.keyDown(input, { key: "Tab" });
    expect(useAppStore.getState().draft).toBe("&founders ");
    expect(useAppStore.getState().activeTeamId).toBe("founders");
    expect(useAppStore.getState().activePersonaId).toBe("main");
    expect(screen.getByTestId("composer-selection")).toHaveTextContent("Founders");
  });

  it("navigates suggestions and dismisses them without deleting the draft", () => {
    resetStore();
    useAppStore.setState({ personas: [{ schema_version: 1, id: "writer", name: "Writer" }, { schema_version: 1, id: "researcher", name: "Researcher" }] });
    renderMiniComposer();
    const input = type("@");
    fireEvent.keyDown(input, { key: "ArrowDown" });
    expect(screen.getAllByRole("option")[1]).toHaveAttribute("aria-selected", "true");
    fireEvent.keyDown(input, { key: "Escape" });
    expect(screen.queryByRole("listbox", { name: "Agents and teams" })).not.toBeInTheDocument();
    expect(useAppStore.getState().draft).toBe("@");
    type("@rese");
    fireEvent.mouseDown(screen.getByRole("option"));
    expect(useAppStore.getState().activePersonaId).toBe("researcher");
  });
});
