import { useEffect, useRef, useState } from "react";
import { Icon } from "../lib/icons";

const clock = (seconds: number) => `${Math.floor(seconds / 60)}:${String(Math.floor(seconds % 60)).padStart(2, "0")}`;

export function VoiceAudioPlayer({ src, label, title }: { src: string; label: string; title: string }) {
  const audio = useRef<HTMLAudioElement>(null);
  const [playing, setPlaying] = useState(false);
  const [duration, setDuration] = useState(0);
  const [position, setPosition] = useState(0);
  const [error, setError] = useState("");
  useEffect(() => {
    const element = audio.current;
    return () => { if (element && !element.paused) element.pause(); };
  }, []);
  async function toggle() {
    if (!audio.current) return;
    if (playing) { audio.current.pause(); return; }
    try { await audio.current.play(); setError(""); }
    catch { setError("Playback could not start. Please try again."); }
  }
  return <div className="voice-player">
    <audio ref={audio} aria-label={label} src={src} preload="metadata"
      onLoadedMetadata={event => { const value = event.currentTarget.duration; setDuration(Number.isFinite(value) ? value : 0); }}
      onTimeUpdate={event => setPosition(event.currentTarget.currentTime)}
      onPlay={() => setPlaying(true)} onPause={() => setPlaying(false)} onEnded={() => setPlaying(false)}
      onError={() => { setPlaying(false); setError("This recording could not be played."); }} />
    <button type="button" className="voice-player-play" aria-label={`${playing ? "Pause" : "Play"} ${title.toLowerCase()}`} onClick={toggle}>
      {playing ? <span className="voice-pause" aria-hidden="true" /> : <Icon.Play size={16} />}
    </button>
    <div className="min-w-0 flex-1">
      <div className="mb-2 flex items-center justify-between gap-3"><span className="truncate text-xs font-medium text-ink">{title}</span><span className="shrink-0 font-mono text-[10px] tabular-nums text-muted">{clock(position)} / {clock(duration)}</span></div>
      <input type="range" aria-label={`Seek ${title.toLowerCase()}`} min={0} max={duration || 1} step={0.01} value={position}
        disabled={!duration} className="voice-player-seek" style={{ background: `linear-gradient(to right, var(--color-accent) ${duration ? position / duration * 100 : 0}%, var(--color-line-strong) ${duration ? position / duration * 100 : 0}%)` }} onChange={event => { const value = Number(event.target.value); if (audio.current) audio.current.currentTime = value; setPosition(value); }} />
      {error && <p role="alert" className="mt-2 text-xs text-red-400">{error}</p>}
    </div>
  </div>;
}
