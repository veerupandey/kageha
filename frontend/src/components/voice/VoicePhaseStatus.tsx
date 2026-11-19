import { cn } from "../../lib/cn";
import { phaseStatusLabel } from "./voiceCore";
import type { GeminiLivePhase, GeminiLiveStatus } from "./voiceTypes";

export interface VoicePhaseStatusProps {
  status: GeminiLiveStatus;
  phase: GeminiLivePhase;
}

/**
 * Displays the current conversational phase status.
 *
 * Renders the human-readable label from `phaseStatusLabel(status, phase)`
 * (Req 6.1-6.4) and updates it whenever the phase or status changes (Req 6.5).
 * The label lives inside an `aria-live="polite"` region so screen readers
 * announce the current phase and its changes without interrupting (Req 9.4),
 * and React re-renders the region's text content on each transition (Req 9.5).
 */
export function VoicePhaseStatus({ status, phase }: VoicePhaseStatusProps) {
  const label = phaseStatusLabel(status, phase);

  return (
    <p
      className={cn(
        "ka-voice-phase-status text-center text-sm font-medium tracking-wide text-muted",
      )}
      role="status"
      aria-live="polite"
      aria-atomic="true"
      data-status={status}
      data-phase={phase}
    >
      {label}
    </p>
  );
}

export default VoicePhaseStatus;
