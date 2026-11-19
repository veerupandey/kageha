import { useEffect, useRef, useState } from "react";
import { Icon } from "../../../lib/icons";

export interface AudioViewerProps {
  url: string;
  name: string;
}

function formatTime(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return "0:00";
  const m = Math.floor(seconds / 60);
  const s = Math.floor(seconds % 60);
  return `${m}:${s < 10 ? "0" : ""}${s}`;
}

export function AudioViewer({ url, name }: AudioViewerProps) {
  const audioRef = useRef<HTMLAudioElement | null>(null);
  const [isPlaying, setIsPlaying] = useState(false);
  const [currentTime, setCurrentTime] = useState(0);
  const [duration, setDuration] = useState(0);
  const [volume, setVolume] = useState(1);
  const [isMuted, setIsMuted] = useState(false);
  const [rate, setRate] = useState(1);
  const [playError, setPlayError] = useState("");

  useEffect(() => {
    setIsPlaying(false);
    setCurrentTime(0);
    setDuration(0);
    setPlayError("");
  }, [url]);

  useEffect(() => {
    const audio = audioRef.current;
    if (!audio) return;
    audio.volume = volume;
    audio.muted = isMuted;
    audio.playbackRate = rate;
  }, [url, volume, isMuted, rate]);

  const togglePlay = () => {
    const audio = audioRef.current;
    if (!audio) return;
    setPlayError("");
    if (!audio.paused) {
      audio.pause();
    } else {
      void audio.play().catch(() => {
        if (audioRef.current !== audio) return;
        setIsPlaying(false);
        setPlayError("Could not play this recording. Retry or download the file.");
      });
    }
  };

  const handleSeek = (e: React.ChangeEvent<HTMLInputElement>) => {
    const nextTime = Number(e.target.value);
    setCurrentTime(nextTime);
    if (audioRef.current) {
      audioRef.current.currentTime = nextTime;
    }
  };

  const handleVolumeChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const nextVol = Number(e.target.value);
    setVolume(nextVol);
    setIsMuted(nextVol === 0);
  };

  const toggleMute = () => {
    setIsMuted(value => !value);
  };

  const cycleRate = () => {
    const rates = [1, 1.25, 1.5, 2, 0.75];
    const nextRate = rates[(rates.indexOf(rate) + 1) % rates.length] || 1;
    setRate(nextRate);
  };

  return (
    <div className="flex h-full w-full min-h-0 items-center justify-center bg-[#090b10] p-6 text-white">
      <audio key={url} ref={audioRef} src={url} preload="metadata"
        onPlay={() => { setIsPlaying(true); setPlayError(""); }}
        onPause={() => setIsPlaying(false)}
        onEnded={() => setIsPlaying(false)}
        onTimeUpdate={event => setCurrentTime(event.currentTarget.currentTime)}
        onLoadedMetadata={event => setDuration(event.currentTarget.duration)}
        onError={() => { setIsPlaying(false); setPlayError("Could not load this recording. Try downloading the file."); }}
      />

      <div className="w-full max-w-lg rounded-3xl border border-white/10 bg-[#161a23] p-8 shadow-2xl backdrop-blur-xl">
        {/* Animated Waveform visualizer bars */}
        <div className="mb-8 flex h-24 items-center justify-center gap-1.5 overflow-hidden px-4">
          {Array.from({ length: 32 }).map((_, i) => {
            const height = isPlaying
              ? Math.max(15, Math.sin((i * 18 + currentTime * 8)) * 40 + 50)
              : Math.max(10, ((i * 13) % 45) + 10);
            return (
              <span
                key={i}
                className="w-1.5 rounded-full bg-gradient-to-t from-accent/60 to-accent transition-all duration-100"
                style={{ height: `${height}%` }}
              />
            );
          })}
        </div>

        {/* Title */}
        <div className="mb-6 text-center">
          <p className="truncate text-base font-semibold text-white" title={name}>
            {name}
          </p>
          <p className="mt-1 text-xs text-white/50">Audio Recording · {formatTime(duration)}</p>
          {playError && <p role="alert" className="mt-2 text-xs text-red-300">{playError}</p>}
        </div>

        {/* Scrubber */}
        <div className="mb-4">
          <input
            type="range"
            min={0}
            max={duration || 100}
            value={currentTime}
            onChange={handleSeek}
            className="h-1.5 w-full cursor-pointer appearance-none rounded-lg bg-white/20 accent-accent"
          />
          <div className="mt-2 flex justify-between text-xs text-white/60">
            <span>{formatTime(currentTime)}</span>
            <span>{formatTime(duration)}</span>
          </div>
        </div>

        {/* Controls row */}
        <div className="flex items-center justify-between pt-2">
          <button
            type="button"
            onClick={cycleRate}
            className="rounded-lg border border-white/10 bg-white/5 px-2.5 py-1 text-xs font-semibold text-white/80 hover:bg-white/15 transition-colors"
            title="Playback speed"
          >
            {rate}x
          </button>

          <button
            type="button"
            onClick={togglePlay}
            className="flex h-14 w-14 items-center justify-center rounded-full bg-accent text-white shadow-lg transition-transform hover:scale-105 active:scale-95"
            aria-label={isPlaying ? "Pause" : "Play"}
          >
            {isPlaying ? (
              <span className="flex gap-1">
                <span className="h-5 w-1.5 rounded-full bg-white" />
                <span className="h-5 w-1.5 rounded-full bg-white" />
              </span>
            ) : (
              <Icon.Play size={24} className="ml-0.5 fill-white" />
            )}
          </button>

          <div className="flex items-center gap-2">
            <button
              type="button"
              onClick={toggleMute}
              className="text-white/70 hover:text-white transition-colors"
              title={isMuted ? "Unmute" : "Mute"}
            >
              {isMuted || volume === 0 ? <Icon.Mute size={18} /> : <Icon.Volume size={18} />}
            </button>
            <input
              type="range"
              min={0}
              max={1}
              step={0.05}
              value={isMuted ? 0 : volume}
              onChange={handleVolumeChange}
              className="h-1 w-16 cursor-pointer appearance-none rounded-lg bg-white/20 accent-white"
            />
          </div>
        </div>
      </div>
    </div>
  );
}
