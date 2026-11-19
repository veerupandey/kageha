import "@testing-library/jest-dom/vitest";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { VoiceCloningSettings } from "./VoiceCloningSettings";

const mocks = vi.hoisted(() => ({ api: vi.fn(), setPrefs: vi.fn(), prefs: { tts: "audio8", ttsVoice: "" } }));
vi.mock("../api/client", () => ({ api: mocks.api }));
vi.mock("../store", () => ({ useAppStore: (select: (state: unknown) => unknown) => select({ prefs: mocks.prefs, setPrefs: mocks.setPrefs }) }));
beforeEach(() => {
  mocks.prefs = { tts: "audio8", ttsVoice: "" };
  mocks.api.mockImplementation(async (path: string, options?: { body: string }) => {
    if (path === "/api/audio8-settings") return { enabled: false, installed: true, device: "cpu", model: "Audio8", ...(options ? JSON.parse(options.body) : {}) };
    return { profiles: [{ id: "voice1", name: "Narrator", transcript: "Hello.", duration_seconds: 3 }] };
  });
});
afterEach(() => { cleanup(); vi.clearAllMocks(); vi.unstubAllGlobals(); });

it("enables Audio8 explicitly and retains a provider-independent profile selection", async () => {
  const refresh = vi.fn();
  render(<VoiceCloningSettings onRefresh={refresh} canClone />);
  const toggle = await screen.findByRole("checkbox", { name: "Enable local Audio8 Realistic 0.6B" });
  expect(toggle).not.toBeChecked();
  fireEvent.click(toggle);
  await waitFor(() => expect(refresh).toHaveBeenCalled());
  expect(mocks.api).toHaveBeenCalledWith("/api/audio8-settings", expect.objectContaining({ body: JSON.stringify({ enabled: true, device: "cpu" }) }));
  fireEvent.change(screen.getByLabelText("Saved reference voice"), { target: { value: "voice1" } });
  expect(mocks.setPrefs).toHaveBeenCalledWith({ ttsVoice: "voice1" });
});

it("shows generation failures without switching the selected provider", async () => {
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue({ ok: false, json: async () => ({ error: "Audio8 is unavailable" }) }));
  render(<VoiceCloningSettings onRefresh={() => {}} canClone />);
  await screen.findByRole("checkbox");
  fireEvent.click(screen.getByRole("button", { name: "Preview selected voice" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("Audio8 is unavailable");
  expect(mocks.setPrefs).not.toHaveBeenCalled();
});

it("keeps preview unavailable for providers without cloning support", async () => {
  render(<VoiceCloningSettings onRefresh={() => {}} canClone={false} />);
  await screen.findByRole("checkbox");
  expect(screen.getByRole("button", { name: "Preview selected voice" })).toBeDisabled();
});
