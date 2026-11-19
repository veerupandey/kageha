import { useRef, useState, type ReactNode } from "react";

export interface VideoViewerProps {
  url: string;
  name: string;
  children?: ReactNode;
  onTouchStart?: (e: React.TouchEvent) => void;
  onTouchEnd?: (e: React.TouchEvent) => void;
}

export function VideoViewer({
  url,
  name: _name,
  children,
  onTouchStart,
  onTouchEnd,
}: VideoViewerProps) {
  const videoRef = useRef<HTMLVideoElement | null>(null);
  const [playbackRate, setPlaybackRate] = useState(1);
  const [isLooping, setIsLooping] = useState(false);

  const cycleRate = () => {
    const rates = [1, 1.25, 1.5, 2, 0.5];
    const nextRate = rates[(rates.indexOf(playbackRate) + 1) % rates.length] || 1;
    setPlaybackRate(nextRate);
    if (videoRef.current) videoRef.current.playbackRate = nextRate;
  };

  const toggleLoop = () => {
    const next = !isLooping;
    setIsLooping(next);
    if (videoRef.current) videoRef.current.loop = next;
  };

  return (
    <div
      className="relative flex h-full w-full min-h-0 flex-1 flex-col items-center justify-center bg-black p-4 select-none"
      onTouchStart={onTouchStart}
      onTouchEnd={onTouchEnd}
    >
      <video
        ref={videoRef}
        src={url}
        controls
        playsInline
        className="max-h-full max-w-full rounded-lg shadow-2xl"
      >
        <track kind="captions" />
      </video>

      {/* Floating Speed & Loop bar */}
      <div className="pointer-events-none absolute bottom-4 left-4 z-20 flex items-center gap-2">
        <div className="pointer-events-auto inline-flex items-center gap-1 rounded-full border border-white/15 bg-black/70 px-2.5 py-1 text-xs text-white shadow-xl backdrop-blur-md">
          <button
            type="button"
            onClick={cycleRate}
            className="rounded px-2 py-0.5 text-xs font-semibold text-white/80 hover:bg-white/20 hover:text-white transition-colors"
            title="Change video speed"
          >
            {playbackRate}x
          </button>
          <span className="h-3 w-px bg-white/20" />
          <button
            type="button"
            onClick={toggleLoop}
            className={`rounded px-2 py-0.5 text-xs font-semibold transition-colors ${
              isLooping ? "bg-accent/40 text-accent font-bold" : "text-white/70 hover:bg-white/20"
            }`}
            title="Toggle video loop"
          >
            Loop
          </button>
        </div>
      </div>

      {children}
    </div>
  );
}
