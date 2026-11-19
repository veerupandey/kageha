import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { startGeminiLiveSession } from "./geminiLiveClient";

class Socket {
  static OPEN = 1;
  static CONNECTING = 0;
  static instances: Socket[] = [];
  readyState = 0;
  onopen: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: ((event: { code: number }) => void) | null = null;
  send = vi.fn();
  close = vi.fn(() => { this.readyState = 3; });
  constructor() { Socket.instances.push(this); }
  message(type: string) { this.onmessage?.({ data: JSON.stringify({ type }) }); }
}
const track = { stop: vi.fn(), enabled: true };
const stream = { getTracks: () => [track], getAudioTracks: () => [track] };
const node = { connect: vi.fn(), disconnect: vi.fn(), onaudioprocess: null as null | ((event: unknown) => void) };
const closeAudio = vi.fn(async () => undefined);
const resumeAudio = vi.fn(async () => undefined);
class Context {
  sampleRate = 48000;
  currentTime = 0;
  destination = {};
  resume = resumeAudio;
  close = closeAudio;
  createMediaStreamSource() { return node; }
  createScriptProcessor() { return node; }
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.clearAllMocks();
  Socket.instances = [];
  track.enabled = true;
  vi.stubGlobal("isSecureContext", true);
  vi.stubGlobal("AudioContext", Context);
  vi.stubGlobal("WebSocket", Socket);
  Object.defineProperty(navigator, "mediaDevices", { configurable: true, value: { getUserMedia: vi.fn(async () => stream) } });
});
afterEach(() => { vi.useRealTimers(); vi.unstubAllGlobals(); });

describe("live voice startup and teardown", () => {
  it("waits for microphone capture before resuming audio contexts", async () => {
    let resolveMic!: (value: MediaStream) => void;
    vi.mocked(navigator.mediaDevices.getUserMedia).mockImplementation(() => new Promise(resolve => { resolveMic = resolve; }) as Promise<MediaStream>);
    const pending = startGeminiLiveSession("test");
    expect(resumeAudio).not.toHaveBeenCalled();
    resolveMic(stream as unknown as MediaStream);
    const session = await pending;
    expect(resumeAudio).toHaveBeenCalledTimes(2);
    session.stop();
  });

  it("waits for provider readiness, handles the first frame, and clears the deadline", async () => {
    const onOpen = vi.fn();
    const onError = vi.fn();
    const session = await startGeminiLiveSession("test", { onOpen, onError });
    const ws = Socket.instances[0];
    expect(ws.onmessage).toBeTypeOf("function");
    ws.readyState = Socket.OPEN;
    ws.onopen?.();
    expect(onOpen).not.toHaveBeenCalled();
    ws.message("ready");
    expect(onOpen).toHaveBeenCalledOnce();
    await vi.advanceTimersByTimeAsync(16000);
    expect(onError).not.toHaveBeenCalled();
    expect(resumeAudio).toHaveBeenCalledTimes(2);
    session.stop();
    expect(track.stop).toHaveBeenCalledOnce();
    expect(closeAudio).toHaveBeenCalledTimes(2);
  });

  it("times out a socket that opens but never receives provider readiness", async () => {
    const onError = vi.fn();
    const session = await startGeminiLiveSession("test", { onError });
    Socket.instances[0].readyState = Socket.OPEN;
    await vi.advanceTimersByTimeAsync(15000);
    expect(onError).toHaveBeenCalledWith(expect.stringContaining("timed out"));
    expect(session.active).toBe(false);
    expect(track.stop).toHaveBeenCalledOnce();
    expect(Socket.instances[0].close).toHaveBeenCalledOnce();
  });

  it("releases a microphone that resolves after cancellation", async () => {
    let resolveMic!: (value: MediaStream) => void;
    vi.mocked(navigator.mediaDevices.getUserMedia).mockImplementation(() => new Promise(resolve => { resolveMic = resolve; }) as Promise<MediaStream>);
    const controller = new AbortController();
    const result = startGeminiLiveSession("test", { signal: controller.signal });
    const rejected = expect(result).rejects.toMatchObject({ name: "AbortError" });
    controller.abort();
    await rejected;
    resolveMic(stream as unknown as MediaStream);
    await Promise.resolve();
    expect(track.stop).toHaveBeenCalledOnce();
    expect(Socket.instances).toHaveLength(0);
  });

  it("times out an unanswered permission request and releases late media", async () => {
    let resolveMic!: (value: MediaStream) => void;
    vi.mocked(navigator.mediaDevices.getUserMedia).mockImplementation(() => new Promise(resolve => { resolveMic = resolve; }) as Promise<MediaStream>);
    const result = startGeminiLiveSession("test");
    const rejected = expect(result).rejects.toThrow("timed out");
    await vi.advanceTimersByTimeAsync(15000);
    await rejected;
    resolveMic(stream as unknown as MediaStream);
    await Promise.resolve();
    expect(track.stop).toHaveBeenCalledOnce();
  });

  it("cleans up on disconnect without replacing an error with idle", async () => {
    const onError = vi.fn();
    const onClose = vi.fn();
    await startGeminiLiveSession("test", { onError, onClose });
    Socket.instances[0].onclose?.({ code: 1006 });
    expect(onError).toHaveBeenCalledOnce();
    expect(onClose).not.toHaveBeenCalled();
    expect(track.stop).toHaveBeenCalledOnce();
  });

  it("mutes the track and marks the input stream ended", async () => {
    const session = await startGeminiLiveSession("test");
    const ws = Socket.instances[0];
    ws.readyState = Socket.OPEN;
    ws.message("ready");
    session.setMuted?.(true);
    expect(track.enabled).toBe(false);
    expect(ws.send).toHaveBeenCalledWith(JSON.stringify({ type: "audio_stream_end" }));
    session.setMuted?.(false);
    expect(track.enabled).toBe(true);
    session.stop();
  });
});
