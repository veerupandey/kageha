import type { RunStatus } from "../api/types";

const EARLY_STOP_LABELS: Record<string, string> = {
  no_progress: "Stopped · stalled, no progress",
  max_steps: "Stopped · step limit reached",
  budget: "Stopped · spend limit reached",
  hitl: "Stopped · needs your approval",
};
export function earlyStopLabel(rawStatus: string): string | null {
  return EARLY_STOP_LABELS[String(rawStatus || "").toLowerCase()] ?? null;
}
export function isEarlyStopLabel(label: string | undefined | null): boolean {
  return String(label || "").startsWith("Stopped · ");
}
export function isDeliveryUnconfirmed(resultStatus?: string, label?: string | null): boolean {
  return resultStatus === "delivery_unconfirmed" || /^(Delivery unconfirmed|Request received ·|Request ended ·)/.test(label || "");
}

/** A finished response does not certify execution or its required outcomes. */
export function completionLabel(rawStatus: string, progress: {
  stepsDone: number; stepsTotal: number; pendingOutcomes?: number;
}): string {
  const state = terminalRunState(rawStatus);
  if (state.status !== "success") return state.label;
  if (progress.stepsDone < progress.stepsTotal) return "Response finished · plan incomplete";
  if (progress.pendingOutcomes) return "Response finished · verification pending";
  return state.label;
}

/** One projection for streaming, replay, session restoration and task chrome. */
export function terminalRunState(rawStatus: string, opts: { cancelled?: boolean } = {}): { status: RunStatus; label: string } {
  const value = String(rawStatus || "").toLowerCase();
  if (opts.cancelled || value === "cancelled") return { status: "cancelled", label: "Cancelled" };
  if (value === "delivery_unconfirmed") return { status: "interrupted", label: "Delivery unconfirmed" };
  if (["invalid_output", "unverified"].includes(value)) return { status: "interrupted", label: "Verification unresolved" };
  if (value === "error" || value === "failed") return { status: "error", label: "Error" };
  const stopped = earlyStopLabel(value);
  if (stopped) return { status: "interrupted", label: stopped };
  if (value === "blocked") return { status: "interrupted", label: "Blocked" };
  if (value === "recovery_incompatible") return { status: "interrupted", label: "Blocked · saved request needs a compatible version" };
  if (value === "recovery_exhausted") return { status: "interrupted", label: "Blocked · repeated execution crashes" };
  if (["interrupted", "reconciliation_required"].includes(value)) return { status: "interrupted", label: "Interrupted · review before resuming" };
  if (["awaiting_clarify", "ask_user", "waiting_input", "awaiting_input"].includes(value)) return { status: "waiting_approval", label: "Needs your input" };
  if (value === "waiting_dependencies") return { status: "running", label: "Waiting for background work" };
  if (value === "stalled") return { status: "running", label: "Stalled · recovery pending" };
  if (value === "waiting_resource") return { status: "running", label: "Waiting for shared resource" };
  if (value === "repairing") return { status: "running", label: "Repairing" };
  // Held after a server restart; continues automatically once credentials are unlocked.
  if (value === "waiting_for_unlock") return { status: "running", label: "Waiting for credential unlock" };
  if (value === "awaiting_plan_approval") return { status: "waiting_approval", label: "Awaiting Build" };
  if (value === "waiting_approval") return { status: "waiting_approval", label: "Approval needed" };
  if (["success", "complete", "completed"].includes(value)) return { status: "success", label: "Done" };
  if (["running", "accepted", "planning", "executing", "verifying"].includes(value)) return { status: "running", label: "Working" };
  if (value === "idle") return { status: "idle", label: "Ready" };
  return { status: "interrupted", label: "Completion unconfirmed" };
}
