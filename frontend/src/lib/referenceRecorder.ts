/** Microphone capture for reference voices; normalize browser codecs to PCM WAV. */
export function pcmWav(channels: Float32Array[], sampleRate: number): Blob {
  const frames = Math.min(channels[0]?.length || 0, Math.floor(sampleRate * 29.5));
  if (!channels.length || frames < sampleRate) throw new Error("Record at least one second of speech.");
  const bytes = new ArrayBuffer(44 + frames * 2);
  const view = new DataView(bytes);
  const text = (offset: number, value: string) => [...value].forEach((c, i) => view.setUint8(offset + i, c.charCodeAt(0)));
  text(0, "RIFF"); view.setUint32(4, 36 + frames * 2, true); text(8, "WAVE"); text(12, "fmt ");
  view.setUint32(16, 16, true); view.setUint16(20, 1, true); view.setUint16(22, 1, true);
  view.setUint32(24, sampleRate, true); view.setUint32(28, sampleRate * 2, true);
  view.setUint16(32, 2, true); view.setUint16(34, 16, true); text(36, "data"); view.setUint32(40, frames * 2, true);
  let peak = 0;
  for (let i = 0; i < frames; i++) {
    const sample = channels.reduce((sum, channel) => sum + channel[i], 0) / channels.length;
    if (!Number.isFinite(sample)) throw new Error("Recording contains invalid audio. Please record again.");
    peak = Math.max(peak, Math.abs(sample));
    const clipped = Math.max(-1, Math.min(1, sample));
    view.setInt16(44 + i * 2, Math.round(clipped * (clipped < 0 ? 32768 : 32767)), true);
  }
  if (peak < 0.001) throw new Error("The recording is silent. Check your microphone and record again.");
  return new Blob([bytes], { type: "audio/wav" });
}

export async function startReferenceRecording(signal: AbortSignal): Promise<{ stop: () => Promise<Blob> }> {
  if (!navigator.mediaDevices?.getUserMedia || typeof MediaRecorder === "undefined") {
    throw new Error("Microphone recording requires a supported browser on localhost or HTTPS.");
  }
  const stream = await navigator.mediaDevices.getUserMedia({ audio: { channelCount: 1, echoCancellation: true } });
  const release = () => stream.getTracks().forEach(track => track.stop());
  if (signal.aborted) { release(); throw new DOMException("Recording cancelled", "AbortError"); }
  let recorder: MediaRecorder;
  try {
    const mimeType = ["audio/webm;codecs=opus", "audio/mp4", "audio/webm"].find(type => MediaRecorder.isTypeSupported(type));
    recorder = new MediaRecorder(stream, mimeType ? { mimeType } : undefined);
  } catch (err) { release(); throw err; }
  const chunks: BlobPart[] = [];
  let resolve!: (blob: Blob) => void;
  let reject!: (err: Error) => void;
  const result = new Promise<Blob>((done, fail) => { resolve = done; reject = fail; });
  // An error can arrive before Stop is pressed.
  void result.catch(() => {});
  const cancel = () => {
    recorder.onstop = null;
    if (recorder.state !== "inactive") recorder.stop();
    release(); reject(new DOMException("Recording cancelled", "AbortError"));
    signal.removeEventListener("abort", cancel);
  };
  signal.addEventListener("abort", cancel, { once: true });
  recorder.ondataavailable = event => { if (event.data.size) chunks.push(event.data); };
  recorder.onerror = () => {
    release(); signal.removeEventListener("abort", cancel);
    reject(new Error("Microphone recording failed. Please try again."));
  };
  recorder.onstop = async () => {
    release(); signal.removeEventListener("abort", cancel);
    let context: AudioContext | undefined;
    try {
      const blob = new Blob(chunks, { type: recorder.mimeType });
      context = new AudioContext();
      const audio = await context.decodeAudioData(await blob.arrayBuffer());
      resolve(pcmWav(Array.from({ length: audio.numberOfChannels }, (_, i) => audio.getChannelData(i)), audio.sampleRate));
    } catch (err) { reject(err instanceof Error ? err : new Error("Could not decode recording. Please try another browser.")); }
    finally { await context?.close().catch(() => {}); }
  };
  try { recorder.start(); } catch (err) { signal.removeEventListener("abort", cancel); release(); throw err; }
  return { stop: () => { if (recorder.state === "recording") recorder.stop(); return result; } };
}
