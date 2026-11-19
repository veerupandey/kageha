import "@testing-library/jest-dom/vitest";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { REFERENCE_SCRIPT, VoiceReferenceRecorder } from "./VoiceReferenceRecorder";
const mocks = vi.hoisted(() => ({ start: vi.fn() }));
vi.mock("../lib/referenceRecorder", () => ({ startReferenceRecording: mocks.start }));
beforeEach(() => {
  vi.stubGlobal("URL", { createObjectURL: vi.fn(() => "blob:test"), revokeObjectURL: vi.fn() });
});
afterEach(() => { cleanup(); vi.clearAllMocks(); vi.unstubAllGlobals(); vi.useRealTimers(); });

it("displays the full script and prepares a WAV with its exact transcript for saving", async () => {
  const blob = new Blob(["WAV"], { type: "audio/wav" });
  mocks.start.mockResolvedValue({ stop: vi.fn().mockResolvedValue(blob) });
  const ready = vi.fn(), busy = vi.fn();
  render(<VoiceReferenceRecorder disabled={false} onReady={ready} onBusy={busy} />);
  expect(screen.getByLabelText("Read this aloud")).toHaveValue(REFERENCE_SCRIPT);
  fireEvent.click(screen.getByRole("button", { name: "Start recording" }));
  fireEvent.click(await screen.findByRole("button", { name: "Stop recording" }));
  await screen.findByLabelText("Your reference recording");
  expect(ready).toHaveBeenLastCalledWith(expect.any(File), REFERENCE_SCRIPT);
  expect(ready.mock.calls.at(-1)?.[0].type).toBe("audio/wav");
  expect(busy).toHaveBeenLastCalledWith(false);
  expect(screen.getByRole("button", { name: "Record again" })).toBeEnabled();
});

it("allows a custom script and locks it while recording", async () => {
  mocks.start.mockResolvedValue({ stop: vi.fn().mockResolvedValue(new Blob(["wav"])) });
  const ready = vi.fn();
  render(<VoiceReferenceRecorder disabled={false} onReady={ready} onBusy={() => {}} />);
  fireEvent.change(screen.getByLabelText("Read this aloud"), { target: { value: "My own passage." } });
  fireEvent.click(screen.getByRole("button", { name: "Start recording" }));
  await screen.findByRole("button", { name: "Stop recording" });
  expect(screen.getByLabelText("Read this aloud")).toHaveAttribute("readonly");
  fireEvent.click(screen.getByRole("button", { name: "Stop recording" }));
  await waitFor(() => expect(ready).toHaveBeenLastCalledWith(expect.any(File), "My own passage."));
});

it("explains denied microphone permission and allows retry", async () => {
  mocks.start.mockRejectedValue(new DOMException("Denied", "NotAllowedError"));
  render(<VoiceReferenceRecorder disabled={false} onReady={() => {}} onBusy={() => {}} />);
  fireEvent.click(screen.getByRole("button", { name: "Start recording" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("Microphone access was denied");
  expect(screen.getByRole("button", { name: "Start recording" })).toBeEnabled();
});

it("aborts microphone capture when the settings section unmounts", async () => {
  let signal: AbortSignal | undefined;
  mocks.start.mockImplementation(async (s: AbortSignal) => { signal = s; return { stop: vi.fn() }; });
  const view = render(<VoiceReferenceRecorder disabled={false} onReady={() => {}} onBusy={() => {}} />);
  fireEvent.click(screen.getByRole("button", { name: "Start recording" }));
  await screen.findByRole("button", { name: "Stop recording" });
  view.unmount(); expect(signal?.aborted).toBe(true);
});

it("stops automatically at 25 seconds", async () => {
  const stop = vi.fn().mockResolvedValue(new Blob(["wav"]));
  mocks.start.mockResolvedValue({ stop });
  render(<VoiceReferenceRecorder disabled={false} onReady={() => {}} onBusy={() => {}} />);
  fireEvent.click(screen.getByRole("button", { name: "Start recording" }));
  await screen.findByRole("button", { name: "Stop recording" });
  // Switch to fake timers before restarting so the recorder's effect owns them.
  fireEvent.click(screen.getByRole("button", { name: "Cancel recording" }));
  vi.useFakeTimers();
  const { act } = await import("@testing-library/react");
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Start recording" })); });
  await act(async () => { await vi.advanceTimersByTimeAsync(25000); });
  expect(stop).toHaveBeenCalledOnce();
});
