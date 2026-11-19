import "@testing-library/jest-dom/vitest";
import { afterEach, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { VoiceAudioPlayer } from "./VoiceAudioPlayer";
afterEach(() => { cleanup(); vi.restoreAllMocks(); });

it("plays, pauses and seeks using the actual audio element state", async () => {
  const play = vi.spyOn(HTMLMediaElement.prototype, "play").mockImplementation(function (this: HTMLMediaElement) { this.dispatchEvent(new Event("play")); return Promise.resolve(); });
  const pause = vi.spyOn(HTMLMediaElement.prototype, "pause").mockImplementation(function (this: HTMLMediaElement) { this.dispatchEvent(new Event("pause")); });
  render(<VoiceAudioPlayer src="blob:voice" label="Voice sample" title="Your recording" />);
  const audio = screen.getByLabelText("Voice sample") as HTMLAudioElement;
  Object.defineProperty(audio, "duration", { configurable: true, value: 12 });
  fireEvent.loadedMetadata(audio);
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Play your recording" })); });
  expect(play).toHaveBeenCalledOnce();
  fireEvent.click(screen.getByRole("button", { name: "Pause your recording" }));
  expect(pause).toHaveBeenCalledOnce();
  fireEvent.change(screen.getByRole("slider", { name: "Seek your recording" }), { target: { value: "6" } });
  expect(audio.currentTime).toBe(6);
  expect(screen.getByText("0:06 / 0:12")).toBeInTheDocument();
});

it("shows playback errors", () => {
  render(<VoiceAudioPlayer src="blob:voice" label="Voice sample" title="Your recording" />);
  fireEvent.error(screen.getByLabelText("Voice sample"));
  expect(screen.getByRole("alert")).toHaveTextContent("could not be played");
});
