import "@testing-library/jest-dom/vitest";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { Composer } from "./Composer";
import { SLASH_COMMANDS } from "../api/slashCatalog";
import { useAppStore } from "../store";
import { DEFAULT_PREFS } from "../store/prefs";

vi.mock("./ComposerModelMenu", () => ({
  ComposerModelMenu: () => null,
}));
vi.mock("./ComposerSearchMenu", () => ({
  ComposerSearchMenu: () => null,
}));
vi.mock("./ModelRoleControls", () => ({
  ModelRoleControls: () => null,
}));
vi.mock("./AgentModeSwitcher", () => ({
  AgentModeSwitcher: () => null,
}));
vi.mock("./ExecutionTargetChip", () => ({
  ExecutionTargetChip: () => null,
}));
const composerSubmit = vi.hoisted(() => vi.fn());
vi.mock("./useComposerController", () => ({
  useComposerController: () => ({
    addPendingFiles: vi.fn(),
    handleMic: vi.fn(),
    pendingFiles: [],
    recording: false,
    removePendingFile: vi.fn(),
    sending: useAppStore.getState().sending,
    sessionId: "s1",
    stopGeneration: vi.fn(),
    submit: composerSubmit,
    transcribing: false,
  }),
}));

const liveVoiceToggle = vi.fn();
let liveVoiceStatus: "idle" | "connecting" | "active" | "error" = "idle";
vi.mock("./useGeminiLiveVoice", () => ({
  useGeminiLiveVoice: () => ({
    status: liveVoiceStatus,
    start: vi.fn(),
    stop: vi.fn(),
    toggle: liveVoiceToggle,
  }),
}));

function resetStore() {
  composerSubmit.mockClear();
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

function typeDraft(value: string) {
  const input = screen.getByLabelText("Message");
  fireEvent.change(input, {
    target: { value, selectionStart: value.length, selectionEnd: value.length },
  });
  fireEvent.select(input);
}

describe("Composer slash commands", () => {
  beforeAll(() => {
    HTMLElement.prototype.scrollIntoView = () => {};
  });

  afterEach(() => {
    cleanup();
    resetStore();
  });

  it("offers Queue and Steer while running, including keyboard shortcuts", () => {
    resetStore();
    useAppStore.setState({ sending: true, draft: "Change the format" });
    render(<Composer />);
    fireEvent.click(screen.getByRole("button", { name: /^Queue$/ }));
    expect(composerSubmit).toHaveBeenLastCalledWith(undefined, { allowWhileSending: true });
    fireEvent.click(screen.getByRole("button", { name: /^Steer$/ }));
    expect(composerSubmit).toHaveBeenLastCalledWith(undefined, { delivery: "steer" });
    const input = screen.getByPlaceholderText(/Ask anything/);
    fireEvent.keyDown(input, { key: "Enter" });
    expect(composerSubmit).toHaveBeenLastCalledWith(undefined, undefined);
    fireEvent.keyDown(input, { key: "Enter", ctrlKey: true });
    expect(composerSubmit).toHaveBeenLastCalledWith(undefined, { delivery: "steer" });
  });

  it("lists commands when the user types /", () => {
    resetStore();
    render(<Composer />);
    typeDraft("/");
    const picker = screen.getByRole("listbox", { name: "Slash commands" });
    expect(within(picker).getByText("Modes")).toBeInTheDocument();
    expect(within(picker).getByText("Plan")).toBeInTheDocument();
    expect(within(picker).getByText("/plan")).toBeInTheDocument();
  });

  it("ranks Plan first for /p and turns it into a chip", () => {
    resetStore();
    render(<Composer />);
    typeDraft("/p");
    const picker = screen.getByRole("listbox", { name: "Slash commands" });
    const options = within(picker).getAllByRole("option");
    expect(options[0]).toHaveTextContent("/plan");
    expect(options[0]).toHaveTextContent("Plan");
    expect(options.some((el) => el.textContent?.includes("/permissions"))).toBe(
      true,
    );
    fireEvent.mouseDown(options[0]);
    expect(screen.getByTestId("composer-chip")).toHaveTextContent("/plan");
    expect(useAppStore.getState().agentMode).toBe("plan");
    expect(useAppStore.getState().draft).toBe("");
  });
});

describe("Composer live voice", () => {
  beforeAll(() => {
    HTMLElement.prototype.scrollIntoView = () => {};
  });

  afterEach(() => {
    cleanup();
    resetStore();
    liveVoiceToggle.mockClear();
    liveVoiceStatus = "idle";
  });

  it("renders a distinct live-voice control alongside the push-to-talk mic", () => {
    resetStore();
    render(<Composer />);
    const mic = screen.getByRole("button", { name: "Voice input" });
    const live = screen.getByRole("button", { name: "Start live voice" });
    expect(mic).toBeInTheDocument();
    expect(live).toBeInTheDocument();
    expect(mic).not.toBe(live);
  });

  it("toggles the live session on click and reflects active state", () => {
    resetStore();
    render(<Composer />);
    fireEvent.click(screen.getByRole("button", { name: "Start live voice" }));
    expect(liveVoiceToggle).toHaveBeenCalledTimes(1);
  });

  it("shows a stop label and pressed state once the session is active", () => {
    liveVoiceStatus = "active";
    resetStore();
    render(<Composer />);
    const live = screen.getByRole("button", { name: "Stop live voice" });
    expect(live).toHaveAttribute("aria-pressed", "true");
  });

  it("disables the live-voice button while recording with the push-to-talk mic", async () => {
    const useComposerControllerMock = await import("./useComposerController");
    vi.spyOn(useComposerControllerMock, "useComposerController").mockReturnValue({
      addPendingFiles: vi.fn(),
      handleMic: vi.fn(),
      pendingFiles: [],
      recording: true,
      removePendingFile: vi.fn(),
      sending: false,
      sessionId: "s1",
      stopGeneration: vi.fn(),
      submit: vi.fn(),
      transcribing: false,
    } as unknown as ReturnType<typeof useComposerControllerMock.useComposerController>);
    resetStore();
    render(<Composer />);
    expect(screen.getByRole("button", { name: "Start live voice" })).toBeDisabled();
  });
});
