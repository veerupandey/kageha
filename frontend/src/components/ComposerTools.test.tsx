import "@testing-library/jest-dom/vitest";
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { ComposerTools } from "./ComposerTools";
import { useAppStore } from "../store";
vi.mock("./ComposerSearchMenu", () => ({ ComposerSearchMenu: () => <button>Search choice</button> }));
vi.mock("./ComposerMediaMenu", () => ({ ComposerMediaMenu: () => <button>Media choice</button> }));
vi.mock("./AgentModeSwitcher", () => ({ AgentModeSwitcher: () => <button>Normal</button> }));
vi.mock("./ExecutionTargetChip", () => ({ ExecutionTargetChip: () => <button>This host</button> }));
vi.mock("./ModelRoleControls", () => ({ ModelRoleControls: () => null }));
vi.mock("../lib/voiceClient", () => ({ stopSpokenReply: vi.fn(), supportsLiveSpeechRecognition: () => true }));
const original = useAppStore.getState();
afterEach(() => { cleanup(); useAppStore.setState(original); });
function open() { render(<ComposerTools />); fireEvent.click(screen.getByRole("button", { name: /Tools/ })); }
it("shows normal task status instead of an empty settings row", () => {
  useAppStore.setState({ runStatus: "running", taskControl: "continue", statusLabel: "Reading files" });
  open();
  expect(screen.getByText("Working")).toBeInTheDocument();
  expect(screen.getByText("Reading files")).toBeInTheDocument();
});
it("shows recovery reason and keeps voice choices mutually exclusive", () => {
  useAppStore.setState({ taskControl: "repair", taskControlReason: "Retrying the connection" });
  open();
  expect(screen.getByText("Retrying the connection")).toBeInTheDocument();
  fireEvent.click(screen.getByRole("radio", { name: "Voice conversation" }));
  expect(useAppStore.getState().prefs.voiceReply).toBe(true);
  expect(useAppStore.getState().prefs.voiceConversation).toBe(true);
  expect(screen.getByText(/Tap the composer microphone/)).toBeInTheDocument();
  fireEvent.click(screen.getByRole("radio", { name: "Text only" }));
  expect(useAppStore.getState().prefs.voiceReply).toBe(false);
  expect(useAppStore.getState().prefs.voiceConversation).toBe(false);
});
