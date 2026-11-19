import { orbStateFor } from "./voiceCore";
import type { GeminiLivePhase, GeminiLiveStatus } from "./voiceTypes";
import "./voiceOrb.css";

/**
 * Resolve the default reduced-motion preference from the browser.
 * Guarded for non-DOM environments (SSR / tests without a window).
 */
function prefersReducedMotion(): boolean {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
    return false;
  }
  return window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

export interface VoiceOrbProps {
  status: GeminiLiveStatus;
  phase: GeminiLivePhase;
  /**
   * Optional override for tests; defaults to
   * `matchMedia('(prefers-reduced-motion: reduce)')`.
   */
  reducedMotion?: boolean;
}

/**
 * Presentational orb. It derives a single discrete `data-orb-state` from
 * `orbStateFor(status, phase)` (precedence error > connecting > phase) and
 * leaves *all* continuous, amplitude-driven animation to CSS, which reads the
 * `--ka-live-mic-level` custom property published on `:root` by the voice hook.
 *
 * The mic level is intentionally never read in JS here — reading it per frame
 * would defeat the no-re-render-storm guarantee (Req 7.1, 7.3). The orb only
 * re-renders when the coarse `status`/`phase` props change.
 */
export function VoiceOrb({ status, phase, reducedMotion }: VoiceOrbProps) {
  const orbState = orbStateFor(status, phase);
  const reduceMotion = reducedMotion ?? prefersReducedMotion();

  return (
    <div
      className="ka-voice-orb-stage"
      data-orb-state={orbState}
      data-reduced-motion={reduceMotion ? "true" : "false"}
      aria-hidden="true"
    >
      <span className="ka-voice-orb-glow" />
      <span className="ka-voice-orb" />
    </div>
  );
}

export default VoiceOrb;
