import type { TruthSnapshot } from "./sessionTruth";
import { completionLabel } from "./runStatus";

const locations = { local: "This host", machine: "Linux box", remote: "Remote host" };
export function sessionStateLabel(snapshot: TruthSnapshot): string {
  if (snapshot.status !== "current") {
    return snapshot.status === "checking" ? "Syncing backend…" : snapshot.data
      ? "Updates stale · last known state" : "Backend state unavailable";
  }
  const data = snapshot.data!;
  if (data.target_conflict) return "Execution location conflict";
  if (!data.turn) return "Ready";
  const turn = data.turn;
  const status = turn.status === "stalled" ? "Stalled · recovery pending"
    : turn.status === "waiting_resource" ? `Waiting for shared ${turn.resource_wait?.resource_kind || "resource"}${turn.resource_wait?.owner_session_id ? ` · chat ${turn.resource_wait.owner_session_id.slice(0, 8)}` : ""}`
    : turn.status === "cleanup_pending" ? "Finishing execution cleanup"
    : turn.phase === "waiting_approval" ? "Approval needed"
    : turn.phase === "waiting_dependencies" ? "Waiting for background work"
    : turn.phase === "repairing" ? "Repairing" : turn.phase === "verifying" ? "Verifying"
    : completionLabel(turn.status, { stepsDone: turn.steps_done, stepsTotal: turn.steps_total,
        pendingOutcomes: turn.outcomes_pending });
  const active = ["accepted", "planning", "executing", "waiting_approval", "waiting_dependencies", "verifying", "repairing"].includes(turn.phase);
  const ageSeconds = Math.max(0, data.observed_at - turn.updated_at);
  const age = active && ageSeconds >= 60 ? ` · last task update ${Math.floor(ageSeconds / 60)}m ago` : "";
  if (active && turn.actual_target && data.selected_target && turn.actual_target !== data.selected_target) {
    return `${status} · ${locations[turn.actual_target]} · selection differs${age}`;
  }
  const location = turn.actual_target ? locations[turn.actual_target] : "location unconfirmed";
  const progress = turn.steps_total ? ` · ${turn.steps_done}/${turn.steps_total} steps` : "";
  const superseded = turn.steps_superseded ? ` · ${turn.steps_superseded} superseded` : "";
  const failures = turn.failed_calls ? ` · ${turn.failed_calls} failed calls` : "";
  return `${status} · ${location}${progress}${superseded}${failures}${age}`;
}
