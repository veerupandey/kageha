import { afterEach, expect, it, vi } from "vitest";
import { pcmWav, startReferenceRecording } from "./referenceRecorder";

afterEach(() => vi.unstubAllGlobals());

async function bytes(blob: Blob): Promise<ArrayBuffer> {
  return new Promise((resolve, reject) => { const reader = new FileReader(); reader.onload = () => resolve(reader.result as ArrayBuffer); reader.onerror = reject; reader.readAsArrayBuffer(blob); });
}

it("writes mono 16-bit PCM WAV accepted by the voice profile API", async () => {
  const samples = new Float32Array(16000).fill(0.25);
  const blob = pcmWav([samples, samples], 16000);
  const buffer = await bytes(blob), view = new DataView(buffer);
  expect(String.fromCharCode(...new Uint8Array(buffer, 0, 4))).toBe("RIFF");
  expect(view.getUint16(22, true)).toBe(1);
  expect(view.getUint32(24, true)).toBe(16000);
  expect(view.getUint16(34, true)).toBe(16);
  expect(view.getUint32(40, true)).toBe(32000);
  expect(view.getInt16(44, true)).toBe(8192);
});

it("rejects silent and too-short recordings and limits duration below 30 seconds", () => {
  expect(() => pcmWav([new Float32Array(16000)], 16000)).toThrow("silent");
  expect(() => pcmWav([new Float32Array(100)], 16000)).toThrow("at least one second");
  const blob = pcmWav([new Float32Array(16000 * 32).fill(0.2)], 16000);
  expect(blob.size).toBe(44 + 16000 * 29.5 * 2);
});

it("releases a microphone granted after cancellation", async () => {
  let grant!: (stream: MediaStream) => void;
  const stop = vi.fn();
  vi.stubGlobal("navigator", { mediaDevices: { getUserMedia: () => new Promise<MediaStream>(resolve => { grant = resolve; }) } });
  vi.stubGlobal("MediaRecorder", class {});
  const controller = new AbortController();
  const result = startReferenceRecording(controller.signal);
  controller.abort();
  grant({ getTracks: () => [{ stop }] } as unknown as MediaStream);
  await expect(result).rejects.toMatchObject({ name: "AbortError" });
  expect(stop).toHaveBeenCalledOnce();
});

it("releases the microphone if the recorder cannot be constructed", async () => {
  const stop = vi.fn();
  vi.stubGlobal("navigator", { mediaDevices: { getUserMedia: async () => ({ getTracks: () => [{ stop }] }) } });
  vi.stubGlobal("MediaRecorder", class { static isTypeSupported() { return true; } constructor() { throw new Error("Unsupported recorder"); } });
  await expect(startReferenceRecording(new AbortController().signal)).rejects.toThrow("Unsupported recorder");
  expect(stop).toHaveBeenCalledOnce();
});
