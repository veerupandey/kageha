import { cn } from "../../lib/cn";
import { Icon } from "../../lib/icons";

export interface VoiceControlsProps {
  /** Close the overlay while keeping the underlying session active. (Req 2.) */
  onExit: () => void;
  /** Stop the live voice session, then close the overlay. (Req 3.) */
  onEnd: () => void;
  /** Whether to render the end-conversation control. */
  showEndControl: boolean;
  muted?: boolean;
  onToggleMute?: () => void;
}

/**
 * Overlay controls for the immersive voice experience.
 *
 * Renders an exit-immersive control and, when `showEndControl` is true, an
 * end-conversation control. Each control carries a non-empty ARIA label
 * exposed as its accessible name so keyboard and screen-reader users can
 * operate the overlay. (Req 2.1, 3.1, 9.3.)
 */
export function VoiceControls({ onExit, onEnd, showEndControl, muted, onToggleMute }: VoiceControlsProps) {
  return (
    <div className="ka-voice-controls flex items-center justify-center gap-3">
      <button
        type="button"
        className={cn(
          "inline-flex h-11 w-11 items-center justify-center rounded-full border border-line",
          "bg-surface/80 text-muted transition-colors hover:bg-line/50 hover:text-ink",
          "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent",
        )}
        aria-label="Exit immersive view"
        title="Exit immersive view (Esc)"
        onClick={onExit}
      >
        <Icon.Minimize size={18} />
      </button>
      {onToggleMute ? (
        <button type="button" onClick={onToggleMute} aria-label={muted ? "Unmute microphone" : "Mute microphone"}
          aria-pressed={muted} className="inline-flex h-11 items-center gap-2 rounded-full border border-line px-4 text-sm hover:bg-line/50 focus-visible:ring-2 focus-visible:ring-accent">
          <Icon.Mic size={18} />{muted ? "Unmute" : "Mute"}
        </button>
      ) : null}
      {showEndControl ? (
        <button
          type="button"
          className={cn(
            "inline-flex h-11 items-center justify-center gap-1.5 rounded-full px-4 text-sm font-medium",
            "bg-danger/15 text-danger transition-colors hover:bg-danger/25",
            "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-danger",
          )}
          aria-label="End conversation"
          title="End the voice conversation"
          onClick={onEnd}
        >
          <Icon.Close size={16} />
          <span>End</span>
        </button>
      ) : null}
    </div>
  );
}

export default VoiceControls;
