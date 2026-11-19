/**
 * The Escalation_Ladder step of a turn (design section 7), as the Activity
 * panel shows it. The runtime reports it in `escalation` events (`to`) and in
 * each `completion_check` (`ladder`); the latest one wins.
 */
export type LadderStep = "normal" | "repair" | "change_method" | "stronger_model" | "replan" | "conclude";

export const LADDER_STEPS: Record<LadderStep, { label: string; tooltip: string }> = {
  normal: { label: "Normal", tooltip: "Making progress." },
  repair: { label: "Repair", tooltip: "No progress for 3 steps: fixing the last failure." },
  change_method: { label: "Change method", tooltip: "Trying a different source, a script or another tool." },
  stronger_model: { label: "Stronger model", tooltip: "The planner model takes the next step." },
  replan: { label: "Replan", tooltip: "Rewriting todo.md from the evidence so far." },
  conclude: { label: "Conclude", tooltip: "Delivering verified work and reporting blockers for the rest." },
};

const isStep = (value: unknown): value is LadderStep =>
  typeof value === "string" && Object.prototype.hasOwnProperty.call(LADDER_STEPS, value);

/** The ladder step a loop event reports, or `undefined` for other events. */
export function ladderStepFromEvent(kind: string, payload: Record<string, unknown> | null | undefined): LadderStep | undefined {
  if (!payload || (kind !== "escalation" && kind !== "completion_check")) return undefined;
  // The stored event may wrap the runtime frame in its own `payload`.
  const nested = payload.payload && typeof payload.payload === "object" && !Array.isArray(payload.payload)
    ? payload.payload as Record<string, unknown> : {};
  const value = kind === "escalation" ? payload.to ?? nested.to : payload.ladder ?? nested.ladder;
  return isStep(value) ? value : undefined;
}
