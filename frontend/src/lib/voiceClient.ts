/** Browser mic → STT and text → TTS helpers for the WebUI. */

let activeSpeech: { cancel: () => void } | null = null;

export function stopSpokenReply(): void {
  activeSpeech?.cancel();
  activeSpeech = null;
}

/** Flatten assistant markdown into speakable plain text. */
export function plainSpeakable(text: string): string {
  return String(text || "")
    .replace(/```[\s\S]*?```/g, " ")
    .replace(/`([^`]+)`/g, "$1")
    .replace(/!\[[^\]]*]\([^)]*\)/g, " ")
    .replace(/\[([^\]]*)]\([^)]*\)/g, "$1")
    .replace(/^#{1,6}\s+/gm, "")
    .replace(/(\*\*|__)(.*?)\1/g, "$2")
    .replace(/(\*|_)(.*?)\1/g, "$2")
    .replace(/^>\s?/gm, "")
    .replace(/\s+/g, " ")
    .trim();
}

export async function speakText(
  sessionId: string,
  text: string,
): Promise<void> {
  const spoken = plainSpeakable(text);
  if (!sessionId || !spoken) return;
  stopSpokenReply();
  const controller = new AbortController();
  let audio: HTMLAudioElement | null = null;
  let url = "";
  let cancelPlayback: (() => void) | null = null;
  const speech = { cancel: () => {
    controller.abort();
    cancelPlayback?.();
    audio?.pause();
  } };
  activeSpeech = speech;
  try {
    const res = await fetch(`/api/sessions/${encodeURIComponent(sessionId)}/tts`, {
      method: "POST", headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ text: spoken.slice(0, 1200) }), signal: controller.signal,
    });
    if (!res.ok) {
      const data = (await res.json().catch(() => ({}))) as { error?: string };
      throw new Error(data.error || `TTS failed (${res.status})`);
    }
    const blob = await res.blob();
    if (controller.signal.aborted) throw new DOMException("Speech cancelled", "AbortError");
    url = URL.createObjectURL(blob);
    const player = new Audio(url);
    audio = player;
    await new Promise<void>((resolve, reject) => {
      cancelPlayback = () => reject(new DOMException("Speech cancelled", "AbortError"));
      player.onended = () => resolve();
      player.onerror = () => reject(new Error("Spoken reply could not be played"));
      void player.play().catch(reject);
    });
  } finally {
    if (audio) { audio.onended = null; audio.onerror = null; audio.src = ""; }
    if (url) URL.revokeObjectURL(url);
    if (activeSpeech === speech) activeSpeech = null;
  }
}

export async function transcribeBlob(
  sessionId: string,
  blob: Blob,
  filename = "voice.webm",
): Promise<string> {
  const form = new FormData();
  form.append("file", blob, filename);
  const res = await fetch(
    `/api/sessions/${encodeURIComponent(sessionId)}/stt`,
    { method: "POST", body: form },
  );
  const data = (await res.json().catch(() => ({}))) as {
    text?: string;
    error?: string;
  };
  if (!res.ok) {
    throw new Error(data.error || `STT failed (${res.status})`);
  }
  return String(data.text || "").trim();
}

interface LiveSpeechResult {
  isFinal: boolean;
  [index: number]: { transcript: string };
}

interface LiveSpeechEvent {
  resultIndex: number;
  results: {
    length: number;
    [index: number]: LiveSpeechResult;
  };
}

interface LiveSpeechRecognition {
  continuous: boolean;
  interimResults: boolean;
  lang: string;
  onerror: ((event: { error?: string }) => void) | null;
  onend: (() => void) | null;
  onresult: ((event: LiveSpeechEvent) => void) | null;
  start: () => void;
  stop: () => void;
}

type LiveSpeechRecognitionConstructor = new () => LiveSpeechRecognition;

function liveSpeechConstructor(): LiveSpeechRecognitionConstructor | null {
  if (typeof window === "undefined") return null;
  const browserWindow = window as Window & {
    SpeechRecognition?: LiveSpeechRecognitionConstructor;
    webkitSpeechRecognition?: LiveSpeechRecognitionConstructor;
  };
  return browserWindow.SpeechRecognition || browserWindow.webkitSpeechRecognition || null;
}

export function supportsLiveSpeechRecognition(): boolean {
  return liveSpeechConstructor() !== null;
}

/** Start browser speech recognition and report both final and interim text. */
export function startLiveSpeechRecognition(
  onUpdate: (finalText: string, interimText: string) => void,
  onEnded?: () => void,
): { stop: () => Promise<string> } {
  const Constructor = liveSpeechConstructor();
  if (!Constructor) throw new Error("Live speech recognition is not available in this browser");

  const recognition = new Constructor();
  recognition.continuous = true;
  recognition.interimResults = true;
  recognition.lang = document.documentElement.lang || "en-US";

  let finalText = "";
  let stopped = false;
  let settled = false;
  let resolveStop: ((text: string) => void) | null = null;
  let failure: Error | null = null;
  let rejectStop: ((error: Error) => void) | null = null;

  const finish = () => {
    if (!settled) {
      settled = true;
      resolveStop?.(finalText.trim());
      if (!stopped) onEnded?.();
    }
  };
  recognition.onresult = (event) => {
    let interimText = "";
    for (let index = event.resultIndex; index < event.results.length; index += 1) {
      const result = event.results[index];
      const transcript = result[0]?.transcript || "";
      if (result.isFinal) finalText += `${transcript} `;
      else interimText += transcript;
    }
    onUpdate(finalText.trim(), interimText.trim());
  };
  recognition.onerror = (event) => {
    if (stopped && (event.error === "aborted" || event.error === "no-speech")) {
      finish();
      return;
    }
    if (!settled) {
      settled = true;
      failure = new Error(event.error ? `Speech recognition: ${event.error}` : "Speech recognition failed");
      rejectStop?.(failure);
      if (!stopped) onEnded?.();
    }
  };
  recognition.onend = finish;
  recognition.start();

  return {
    stop: () => {
      if (settled) return failure ? Promise.reject(failure) : Promise.resolve(finalText.trim());
      stopped = true;
      return new Promise<string>((resolve, reject) => {
        resolveStop = resolve;
        rejectStop = reject;
        recognition.stop();
      });
    },
  };
}

/** Record until stop() is called. Returns audio blob (webm/ogg). */
export async function startMicRecording(): Promise<{
  stop: () => Promise<Blob>;
}> {
  if (!navigator.mediaDevices?.getUserMedia) {
    throw new Error("Microphone not available in this browser");
  }
  const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
  const mime = MediaRecorder.isTypeSupported("audio/webm;codecs=opus")
    ? "audio/webm;codecs=opus"
    : MediaRecorder.isTypeSupported("audio/webm")
      ? "audio/webm"
      : "";
  const recorder = mime
    ? new MediaRecorder(stream, { mimeType: mime })
    : new MediaRecorder(stream);
  const chunks: BlobPart[] = [];
  recorder.ondataavailable = (e) => {
    if (e.data.size > 0) chunks.push(e.data);
  };
  recorder.start();

  return {
    stop: () =>
      new Promise((resolve, reject) => {
        recorder.onerror = () => {
          stream.getTracks().forEach((t) => t.stop());
          reject(new Error("Recording failed"));
        };
        recorder.onstop = () => {
          stream.getTracks().forEach((t) => t.stop());
          const type = recorder.mimeType || "audio/webm";
          resolve(new Blob(chunks, { type }));
        };
        if (recorder.state === "recording") recorder.stop();
        else {
          stream.getTracks().forEach((t) => t.stop());
          resolve(new Blob(chunks, { type: recorder.mimeType || "audio/webm" }));
        }
      }),
  };
}
