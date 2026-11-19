/**
 * Gemini Live voice bridge — browser mic/speaker <-> our WebSocket proxy
 * (`/api/sessions/{id}/voice/live`) <-> Gemini Live <-> backend agent.
 *
 * The browser never talks to Gemini directly (see gemini_live.py for why:
 * tool-call delegation must terminate in our own process). This module only
 * owns local audio capture/playback and the WebSocket framing:
 *   - mic PCM (16-bit, 16kHz, little-endian) goes out as binary WS frames
 *   - a JSON `{"type":"end"}` binary frame (see gemini_live.py's UTF-8-decode
 *     discriminator) signals a clean stop
 *   - incoming binary WS frames are PCM (24kHz) to play back immediately
 *   - incoming *text* WS frames are small JSON control messages — currently
 *     just `{"type":"interrupted"}`, sent the instant Gemini reports
 *     barge-in, so playback of already-queued (not-yet-started) audio can be
 *     cut immediately instead of finishing stale content
 *
 * No provider protocol details (base64, tool calls, function schemas) live
 * here — that's entirely server-side. This keeps the browser bundle honest
 * about what it can see: raw audio in, raw audio out, a couple of control
 * signals, nothing else.
 */

const INPUT_SAMPLE_RATE = 16_000;
const OUTPUT_SAMPLE_RATE = 24_000;

export type LiveVoiceProvider = "gemini" | "openai";
export function getLiveVoiceProvider(): LiveVoiceProvider {
  try {
    return localStorage.getItem("kageha.liveVoiceProvider") === "openai" ? "openai" : "gemini";
  } catch {
    return "gemini";
  }
}
export function setLiveVoiceProvider(provider: LiveVoiceProvider): void {
  try { localStorage.setItem("kageha.liveVoiceProvider", provider); } catch { /* storage unavailable */ }
}

/**
 * Conversational phase, derived from real signals (mic amplitude, playback
 * activity, server turn-lifecycle frames) — never a fake/timed animation.
 *   - listening: mic input above the speech threshold
 *   - thinking: reserved for explicit server processing signals
 *   - speaking: reply audio is actively scheduled/playing
 *   - idle: connected, but none of the above (silence, no one has spoken yet)
 */
export type GeminiLivePhase = "idle" | "listening" | "thinking" | "speaking";

export interface GeminiLiveSession {
  stop: () => void;
  readonly active: boolean;
  setMuted?: (muted: boolean) => void;
  startVisual?: (source: "camera" | "screen") => Promise<void>;
  stopVisual?: () => void;
}

export interface GeminiLiveHandlers {
  signal?: AbortSignal;
  onOpen?: () => void;
  onClose?: () => void;
  onError?: (message: string) => void;
  onAvatarUrl?: (url: string | null) => void;
  onVisualStop?: () => void;
  /** Fires on every mic chunk and playback state change — drive UI
   * animation from this, not from a fixed timer. */
  onPhase?: (phase: GeminiLivePhase, micLevel: number) => void;
}

// RMS above this is "the user is speaking" for animation purposes only —
// generous enough to ignore room noise/echo-cancellation residue picked up
// by a typical laptop mic, not a real voice-activity-detection threshold
// (Gemini's own VAD governs actual turn-taking; this only drives visuals).
const MIC_SPEECH_RMS_THRESHOLD = 0.02;

export function floatTo16BitPCM(input: Float32Array): ArrayBuffer {
  const buffer = new ArrayBuffer(input.length * 2);
  const view = new DataView(buffer);
  for (let i = 0; i < input.length; i++) {
    const clamped = Math.max(-1, Math.min(1, input[i]));
    view.setInt16(i * 2, clamped < 0 ? clamped * 0x8000 : clamped * 0x7fff, true);
  }
  return buffer;
}

/** Root-mean-square amplitude of a chunk, 0 (silence) to ~1 (full scale). */
export function rmsLevel(chunk: Float32Array): number {
  if (chunk.length === 0) return 0;
  let sumSquares = 0;
  for (let i = 0; i < chunk.length; i++) {
    sumSquares += chunk[i] * chunk[i];
  }
  return Math.sqrt(sumSquares / chunk.length);
}

export function downsampleBuffer(input: Float32Array, inputRate: number, outputRate: number): Float32Array {
  if (outputRate === inputRate) return input;
  const ratio = inputRate / outputRate;
  const outLength = Math.round(input.length / ratio);
  const out = new Float32Array(outLength);
  let offsetIn = 0;
  for (let i = 0; i < outLength; i++) {
    const nextOffsetIn = Math.round((i + 1) * ratio);
    let sum = 0;
    let count = 0;
    for (let j = offsetIn; j < nextOffsetIn && j < input.length; j++) {
      sum += input[j];
      count++;
    }
    out[i] = count > 0 ? sum / count : 0;
    offsetIn = nextOffsetIn;
  }
  return out;
}

/** PCM16LE playback queue — plays chunks back-to-back with no gaps/overlaps. */
export class PcmPlayer {
  private ctx: AudioContext;
  private nextStartTime = 0;
  // Every scheduled (not necessarily yet-playing) source, so clearQueue() can
  // actually stop them — resetting nextStartTime alone does nothing to nodes
  // already given a future start() time; they play regardless.
  private scheduled = new Set<AudioBufferSourceNode>();
  /** Called whenever hasActivePlayback() may have changed (source finished
   * naturally, or was force-stopped) — the phase-tracking animation needs
   * to know the instant "speaking" ends, not just when it starts. */
  onPlaybackChange: (() => void) | null = null;

  constructor() {
    this.ctx = new AudioContext({ sampleRate: OUTPUT_SAMPLE_RATE });
    this.nextStartTime = this.ctx.currentTime;
  }

  async resume(): Promise<void> {
    await this.ctx.resume();
  }

  enqueue(pcm: ArrayBuffer): void {
    const view = new DataView(pcm);
    const sampleCount = pcm.byteLength / 2;
    const float = new Float32Array(sampleCount);
    for (let i = 0; i < sampleCount; i++) {
      float[i] = view.getInt16(i * 2, true) / 0x8000;
    }
    const buffer = this.ctx.createBuffer(1, sampleCount, OUTPUT_SAMPLE_RATE);
    buffer.copyToChannel(float, 0);
    const source = this.ctx.createBufferSource();
    source.buffer = buffer;
    source.connect(this.ctx.destination);
    source.onended = () => {
      this.scheduled.delete(source);
      this.onPlaybackChange?.();
    };
    const startAt = Math.max(this.nextStartTime, this.ctx.currentTime);
    source.start(startAt);
    this.scheduled.add(source);
    this.nextStartTime = startAt + buffer.duration;
  }

  /** True while any scheduled chunk is still playing/pending — the real
   * signal for "the model is currently speaking" (vs. guessing from a
   * fixed-duration timer that would drift from actual audio length). */
  hasActivePlayback(): boolean {
    return this.scheduled.size > 0;
  }

  /** Stop and discard all scheduled playback (playing or not-yet-started) —
   * used on model interruption (barge-in). */
  clearQueue(): void {
    const hadScheduled = this.scheduled.size > 0;
    for (const source of this.scheduled) {
      // .stop() synchronously fires onended (which would otherwise try to
      // mutate `this.scheduled` while this loop is iterating it) — clear the
      // set up front and skip the redundant per-source onPlaybackChange spam
      // below with one call after the loop instead.
      source.onended = null;
      try {
        source.stop();
      } catch {
        /* already stopped/ended */
      }
    }
    this.scheduled.clear();
    this.nextStartTime = this.ctx.currentTime;
    if (hadScheduled) this.onPlaybackChange?.();
  }

  async close(): Promise<void> {
    this.clearQueue();
    try {
      await this.ctx.close();
    } catch {
      /* already closed */
    }
  }
}

const WORKLET_MODULE_URL = "/gemini-live-worklet.js";
const WORKLET_PROCESSOR_NAME = "gemini-live-capture";

type CaptureNode = AudioWorkletNode | ScriptProcessorNode;

/**
 * Prefer AudioWorkletNode (runs on the dedicated audio thread — no glitching
 * under main-thread/UI load, and it's the non-deprecated API). Falls back to
 * ScriptProcessorNode only if the worklet module can't be loaded (e.g. an
 * older Safari, or the static file failing to fetch) so mic capture still
 * works rather than hard-failing the whole session over it.
 */
async function createCaptureNode(
  ctx: AudioContext,
  onChunk: (chunk: Float32Array) => void,
): Promise<CaptureNode> {
  if (typeof AudioWorkletNode !== "undefined" && ctx.audioWorklet) {
    try {
      await ctx.audioWorklet.addModule(WORKLET_MODULE_URL);
      const node = new AudioWorkletNode(ctx, WORKLET_PROCESSOR_NAME, {
        numberOfInputs: 1,
        numberOfOutputs: 1,
        channelCount: 1,
      });
      node.port.onmessage = (event: MessageEvent<Float32Array>) => {
        onChunk(event.data);
      };
      return node;
    } catch {
      // Fall through to the ScriptProcessorNode fallback below.
    }
  }
  // Deprecated, main-thread API — only reached when AudioWorklet is
  // unavailable or the worklet module failed to load.
  const processor = ctx.createScriptProcessor(4096, 1, 1);
  processor.onaudioprocess = (event) => {
    onChunk(event.inputBuffer.getChannelData(0));
  };
  return processor;
}

export async function startGeminiLiveSession(
  sessionId: string,
  handlers: GeminiLiveHandlers = {},
): Promise<GeminiLiveSession> {
  if (!sessionId) throw new Error("Live voice requires an active session");
  if (!window.isSecureContext) {
    // getUserMedia is only available in secure contexts. On a plain-http LAN
    // origin (e.g. http://10.0.0.40:5173) the call silently never resolves and
    // no permission prompt appears — surface that instead of hanging.
    throw new Error(
      "Live voice needs a secure context. Open the app on http://localhost or over HTTPS.",
    );
  }
  if (!navigator.mediaDevices?.getUserMedia) {
    throw new Error("Microphone access is not available in this browser");
  }

  let active = true;
  let ready = false;
  let muted = false;
  let stream: MediaStream | undefined;
  let ws: WebSocket | undefined;
  let captureCtx: AudioContext | undefined;
  let source: MediaStreamAudioSourceNode | undefined;
  let captureNode: CaptureNode | undefined;
  let player: PcmPlayer | undefined;
  let timer: number | undefined;
  let visualStream: MediaStream | undefined;
  let visualTimer: number | undefined;
  let visualVideo: HTMLVideoElement | undefined;
  let avatarMedia: MediaSource | undefined;
  let avatarBuffer: SourceBuffer | undefined;
  let avatarUrl: string | undefined;
  const avatarQueue: Uint8Array[] = [];
  const appendAvatar = () => {
    if (!avatarBuffer || avatarBuffer.updating || !avatarQueue.length) return;
    try {
      avatarBuffer.appendBuffer(avatarQueue.shift()! as BufferSource);
    } catch {
      fail("The live avatar stream could not be played in this browser");
    }
  };
  const receiveAvatar = (base64: string, mimeType: string) => {
    if (!avatarMedia) {
      if (typeof MediaSource === "undefined") {
        fail("This browser cannot play the live avatar stream");
        return;
      }
      avatarMedia = new MediaSource();
      avatarUrl = URL.createObjectURL(avatarMedia);
      avatarMedia.addEventListener("sourceopen", () => {
        if (!avatarMedia || !active) return;
        const candidates = [mimeType, 'video/mp4; codecs="avc1.42E01E, mp4a.40.2"', "video/mp4"];
        const supported = candidates.find((candidate) => MediaSource.isTypeSupported(candidate));
        if (!supported) { fail("This browser cannot decode the live avatar stream"); return; }
        try {
          avatarBuffer = avatarMedia.addSourceBuffer(supported);
        } catch {
          fail("This browser could not open the live avatar stream");
          return;
        }
        avatarBuffer.addEventListener("updateend", appendAvatar);
        appendAvatar();
      }, { once: true });
      handlers.onAvatarUrl?.(avatarUrl);
    }
    const bytes = Uint8Array.from(atob(base64), (char) => char.charCodeAt(0));
    if (avatarQueue.length > 120) avatarQueue.shift();
    avatarQueue.push(bytes);
    appendAvatar();
  };
  let rejectStartup: (error: Error) => void = () => undefined;
  const signal = handlers.signal;

  const stop = () => {
    if (!active) return;
    active = false;
    window.clearTimeout(timer);
    window.clearInterval(visualTimer);
    visualStream?.getTracks().forEach((track) => track.stop());
    if (visualVideo) visualVideo.srcObject = null;
    avatarQueue.length = 0;
    avatarBuffer = undefined;
    avatarMedia = undefined;
    handlers.onAvatarUrl?.(null);
    if (avatarUrl) URL.revokeObjectURL(avatarUrl);
    avatarUrl = undefined;
    signal?.removeEventListener("abort", abort);
    rejectStartup(new DOMException("Voice startup cancelled", "AbortError"));
    if (ws) {
      ws.onopen = ws.onmessage = ws.onerror = ws.onclose = null;
      if (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING) ws.close();
    }
    captureNode?.disconnect();
    source?.disconnect();
    stream?.getTracks().forEach((track) => track.stop());
    if (captureCtx) void captureCtx.close().catch(() => undefined);
    if (player) {
      player.onPlaybackChange = null;
      void player.close();
    }
  };
  const abort = () => stop();
  const fail = (message: string) => {
    if (!active) return;
    rejectStartup(new Error(message));
    stop();
    handlers.onError?.(message);
  };
  const startupFailure = new Promise<never>((_resolve, reject) => { rejectStartup = reject; });
  // Attach a handler before starting any operation that can synchronously fail.
  void startupFailure.catch(() => undefined);
  signal?.addEventListener("abort", abort, { once: true });
  let startupStage: "microphone" | "audio" | "connection" = "microphone";
  const startupTimeoutMessage = () => {
    if (startupStage === "microphone") return "Microphone access timed out. Allow microphone access for this site, then retry.";
    if (startupStage === "audio") return "Browser audio did not start. Check microphone permission and your audio device, then retry.";
    return "Live voice connection timed out. Check the voice server, then retry.";
  };
  timer = window.setTimeout(() => fail(startupTimeoutMessage()), 15_000);
  if (signal?.aborted) stop();

  const setup = async () => {
    if (!active) throw new DOMException("Voice startup cancelled", "AbortError");
    const acquired = await navigator.mediaDevices.getUserMedia({
      audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true },
    });
    if (!active) {
      acquired.getTracks().forEach((track) => track.stop());
      return;
    }
    stream = acquired;
    startupStage = "audio";
    // Browsers can keep resume() pending until capture is active. Waiting for
    // it before getUserMedia caused Live to sit at Connecting and time out.
    captureCtx = new AudioContext();
    player = new PcmPlayer();
    await Promise.all([captureCtx.resume(), player.resume()]);
    if (!active) return;
    source = captureCtx.createMediaStreamSource(stream);
    let micLevel = 0;
    const reportPhase = () => {
      if (!active) return;
      handlers.onPhase?.(player!.hasActivePlayback() ? "speaking" : micLevel >= MIC_SPEECH_RMS_THRESHOLD ? "listening" : "idle", micLevel);
    };
    player.onPlaybackChange = reportPhase;
    const node = await createCaptureNode(captureCtx, (chunk) => {
      if (!active) return;
      micLevel = muted ? 0 : rmsLevel(chunk);
      reportPhase();
      if (!ready || ws?.readyState !== WebSocket.OPEN || muted) return;
      ws.send(floatTo16BitPCM(downsampleBuffer(chunk, captureCtx!.sampleRate, INPUT_SAMPLE_RATE)));
    });
    if (!active) { node.disconnect(); return; }
    captureNode = node;
    source.connect(node);
    node.connect(captureCtx.destination);
    startupStage = "connection";

    // Create the socket only after audio initialization; install every handler
    // synchronously so a fast handshake cannot be lost while loading a worklet.
    const protocol = window.location.protocol === "https:" ? "wss" : "ws";
    ws = new WebSocket(`${protocol}://${window.location.host}/api/sessions/${encodeURIComponent(sessionId)}/voice/live?provider=${getLiveVoiceProvider()}`);
    ws.binaryType = "arraybuffer";
    ws.onmessage = (event) => {
      if (!active) return;
      if (event.data instanceof ArrayBuffer) {
        if (ready && event.data.byteLength > 0 && event.data.byteLength % 2 === 0) player!.enqueue(event.data);
        reportPhase();
        return;
      }
      let control: { type?: string; message?: string; data?: string; mimeType?: string };
      try { control = JSON.parse(event.data); } catch { return; }
      if (control?.type === "avatar_chunk" && control.data) {
        receiveAvatar(control.data, control.mimeType || "video/mp4");
        return;
      }
      if (control?.type === "ready" && !ready) {
        ready = true;
        window.clearTimeout(timer);
        handlers.onOpen?.();
      } else if (control?.type === "error") {
        fail(control.message || "Live voice could not connect. Please retry.");
      } else if (control?.type === "interrupted") {
        player!.clearQueue();
        reportPhase();
      } else if (control?.type === "turn_complete") {
        // Turn complete means the reply ended, not that the model is thinking.
        reportPhase();
      }
    };
    ws.onerror = () => fail("Live voice connection failed. Check the server and Gemini API configuration, then retry.");
    ws.onclose = (event) => {
      if (!active) return;
      if (!ready || event.code !== 1000) {
        fail("Live voice disconnected. Please retry.");
      } else {
        stop();
        handlers.onClose?.();
      }
    };
  };
  try {
    await Promise.race([setup(), startupFailure]);
  } catch (error) {
    stop();
    if (error instanceof DOMException && error.name === "NotAllowedError") {
      throw new Error("Microphone access was denied. Allow it for this site, then retry.");
    }
    if (error instanceof DOMException && error.name === "NotFoundError") {
      throw new Error("No microphone was found. Connect or enable one, then retry.");
    }
    throw error;
  }
  return {
    stop,
    async startVisual(source) {
      if (!active || !ready) throw new Error("Start the live conversation first");
      window.clearInterval(visualTimer);
      visualStream?.getTracks().forEach((track) => track.stop());
      if (visualVideo) visualVideo.srcObject = null;
      visualStream = undefined;
      const acquired = source === "camera"
        ? await navigator.mediaDevices.getUserMedia({ video: true })
        : await navigator.mediaDevices.getDisplayMedia({ video: true });
      if (!active) { acquired.getTracks().forEach((track) => track.stop()); return; }
      visualStream = acquired;
      const video = document.createElement("video");
      video.muted = true;
      video.playsInline = true;
      video.srcObject = acquired;
      visualVideo = video;
      await video.play();
      const canvas = document.createElement("canvas");
      const sendFrame = () => {
        if (!active || ws?.readyState !== WebSocket.OPEN || video.readyState < 2) return;
        const scale = Math.min(1, 768 / Math.max(video.videoWidth, video.videoHeight));
        canvas.width = Math.max(1, Math.round(video.videoWidth * scale));
        canvas.height = Math.max(1, Math.round(video.videoHeight * scale));
        canvas.getContext("2d")?.drawImage(video, 0, 0, canvas.width, canvas.height);
        ws.send(JSON.stringify({ type: "video_frame", data: canvas.toDataURL("image/jpeg", 0.7).split(",")[1] }));
      };
      sendFrame();
      visualTimer = window.setInterval(sendFrame, 1000);
      acquired.getVideoTracks()[0]?.addEventListener("ended", () => {
        window.clearInterval(visualTimer);
        if (visualStream === acquired) {
          visualStream = undefined;
          handlers.onVisualStop?.();
        }
      }, { once: true });
    },
    stopVisual() {
      window.clearInterval(visualTimer);
      visualStream?.getTracks().forEach((track) => track.stop());
      visualStream = undefined;
      if (visualVideo) visualVideo.srcObject = null;
      visualVideo = undefined;
    },
    setMuted(value) {
      muted = value;
      stream?.getAudioTracks().forEach((track) => { track.enabled = !value; });
      if (value && ready && ws?.readyState === WebSocket.OPEN) ws.send(JSON.stringify({ type: "audio_stream_end" }));
      if (value) handlers.onPhase?.(player?.hasActivePlayback() ? "speaking" : "idle", 0);
    },
    get active() { return active; },
  };
}
