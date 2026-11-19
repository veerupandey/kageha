import { useEffect, useRef, useState } from "react";
import { Icon } from "../lib/icons";
import { VoiceAudioPlayer } from "./VoiceAudioPlayer";
import { startReferenceRecording } from "../lib/referenceRecorder";

export const REFERENCE_SCRIPT = "Hello, this is my voice. I enjoy learning new things and turning ideas into something useful. Today, I am recording a short sample so Kageha can speak with a voice that sounds like me.";

export function VoiceReferenceRecorder({ disabled, onReady, onBusy }: {
  disabled: boolean;
  onReady: (file: File | null, transcript: string) => void;
  onBusy: (busy: boolean) => void;
}) {
  const [script, setScript] = useState(REFERENCE_SCRIPT);
  const [phase, setPhase] = useState<"idle" | "permission" | "recording" | "processing" | "review">("idle");
  const [seconds, setSeconds] = useState(0);
  const [url, setUrl] = useState("");
  const [error, setError] = useState("");
  const controller = useRef<AbortController | null>(null);
  const capture = useRef<Awaited<ReturnType<typeof startReferenceRecording>> | null>(null);
  const alive = useRef(true);
  const active = phase === "permission" || phase === "recording" || phase === "processing";
  const button = "voice-secondary";

  useEffect(() => { alive.current = true; return () => { alive.current = false; controller.current?.abort(); }; }, []);
  useEffect(() => () => { if (url) URL.revokeObjectURL(url); }, [url]);
  useEffect(() => {
    if (phase !== "recording") return;
    const start = Date.now();
    const timer = window.setInterval(() => setSeconds(Math.floor((Date.now() - start) / 1000)), 250);
    const limit = window.setTimeout(() => { void stop(); }, 25000);
    return () => { window.clearInterval(timer); window.clearTimeout(limit); };
  }, [phase]);

  async function start() {
    setError(""); setUrl(""); setSeconds(0); onReady(null, ""); onBusy(true); setPhase("permission");
    const abort = new AbortController(); controller.current = abort;
    try {
      const recording = await startReferenceRecording(abort.signal);
      if (!alive.current || abort.signal.aborted) return;
      capture.current = recording; setPhase("recording");
    } catch (err) {
      if (!alive.current || abort.signal.aborted) return;
      setError(err instanceof DOMException && err.name === "NotAllowedError"
        ? "Microphone access was denied. Allow it in your browser settings, then try again."
        : err instanceof Error ? err.message : "Could not start the microphone.");
      onBusy(false); setPhase("idle");
    }
  }
  async function stop() {
    const recording = capture.current;
    const abort = controller.current;
    if (!recording) return;
    capture.current = null; setPhase("processing");
    try {
      const blob = await recording.stop();
      if (!alive.current || abort?.signal.aborted) return;
      setUrl(URL.createObjectURL(blob));
      onReady(new File([blob], "my-voice.wav", { type: "audio/wav" }), script.trim());
      setPhase("review");
    } catch (err) {
      if (!alive.current || abort?.signal.aborted) return;
      setError(err instanceof Error ? err.message : "Could not finish recording."); setPhase("idle");
    } finally { if (alive.current && controller.current === abort) onBusy(false); }
  }
  function cancel() {
    controller.current?.abort(); capture.current = null;
    onBusy(false); setPhase("idle"); setSeconds(0);
  }

  return <div className="space-y-4">
    <div className="flex flex-wrap gap-x-4 gap-y-2 text-[10px] text-muted">
      <span className="flex items-center gap-1.5"><Icon.Volume size={12} />Quiet surroundings</span>
      <span className="flex items-center gap-1.5"><Icon.Mic size={12} />Your natural speaking voice</span>
      <span className="flex items-center gap-1.5"><Icon.Check size={12} />15–25 seconds</span>
    </div>
    <div className="voice-script-card">
      <div className="flex items-center justify-between px-5 pt-4">
        <label htmlFor="voice-reference-script" className="text-[10px] font-semibold uppercase tracking-widest text-muted">Read this aloud</label>
        <span className="text-[10px] text-faint">Sample passage</span>
      </div>
      <textarea id="voice-reference-script" aria-label="Read this aloud" value={script} readOnly={active || phase === "review"} maxLength={1000}
        onChange={event => setScript(event.target.value)} rows={5} className="voice-script" />
      <div className="voice-script-footer"><span>Read at a relaxed, conversational pace.</span><span className="shrink-0">{script.trim().split(/\s+/).filter(Boolean).length} words</span></div>
    </div>
    <div className="space-y-2">
      <div className="voice-record-deck" data-recording={phase === "recording"}>
        <div className="flex items-center gap-3">
          <div className="voice-record-icon">{phase === "recording" ? <span className="voice-record-dot" data-live="true" /> : phase === "review" ? <Icon.Check size={18} /> : <Icon.Mic size={18} />}</div>
          <div><p className="text-xs font-medium text-ink">{phase === "recording" ? "Recording your voice" : phase === "review" ? "Recording complete" : phase === "processing" ? "Preparing your audio" : phase === "permission" ? "Waiting for microphone" : "Ready when you are"}</p><p className="mt-1 text-[10px] text-muted">{phase === "recording" ? "Read the passage, then press Stop." : phase === "review" ? "Listen back before saving." : "Microphone access starts when you record."}</p></div>
        </div>
        <div className="flex items-center gap-2">
          {phase === "recording" && <span className="mr-1 font-mono text-xs tabular-nums text-muted">0:{String(seconds).padStart(2, "0")} <span className="text-faint">/ 0:25</span></span>}
          {!active && <button className="voice-record-button" disabled={disabled || !script.trim()} onClick={start}>{phase === "review" ? <Icon.Reset size={14} /> : <span className="voice-record-dot" />}{phase === "review" ? "Record again" : "Start recording"}</button>}
          {phase === "recording" && <button className="voice-record-button" data-stop="true" onClick={stop}><span className="h-2.5 w-2.5 rounded-[2px] bg-current" aria-hidden="true" />Stop recording</button>}
          {active && <button className={button} aria-label="Cancel recording" title="Cancel recording" onClick={cancel}><Icon.Close size={14} /></button>}
        </div>
      </div>
      {phase === "recording" && <div className="voice-record-progress" role="progressbar" aria-label="Recording time" aria-valuenow={seconds} aria-valuemin={0} aria-valuemax={25}><span style={{ width: `${seconds / 25 * 100}%` }} /></div>}
    </div>
    {phase === "permission" && <p role="status" className="text-xs text-muted">Allow microphone access in your browser to begin.</p>}
    {phase === "recording" && <p role="status" className="sr-only">Recording · {seconds}s / 25s — read the entire passage, then press Stop.</p>}
    {phase === "processing" && <p role="status" className="text-xs text-muted">Preparing your recording…</p>}
    {url && <><VoiceAudioPlayer key={url} src={url} label="Your reference recording" title="Your recording" /><p className="text-[11px] leading-relaxed text-muted">Check the transcript matches exactly what you said. If you missed a word, correct it below or record again.</p></>}
    {!active && phase !== "review" && <p className="text-[10px] leading-relaxed text-faint">Prefer your own passage? Edit the text above before recording. Recording stops automatically at 25 seconds.</p>}
    {error && <p role="alert" className="rounded-lg border border-red-400/20 bg-red-400/5 px-3 py-2 text-xs text-red-400">{error}</p>}
  </div>;
}
