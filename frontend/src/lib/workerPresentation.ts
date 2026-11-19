import type { DurableJob } from "../components/DurableJobs";

export const WORKER_TERMINAL = new Set(["succeeded", "failed", "cancelled", "interrupted"]);
export function workerArtifactUrl(job: DurableJob, path: string, index: number) {
  if (path.startsWith("workspace/") || path.startsWith("artifacts/")) {
    return `/api/sessions/${encodeURIComponent(job.session_id)}/files/${path.split("/").map(encodeURIComponent).join("/")}`;
  }
  return `/api/jobs/${encodeURIComponent(job.id)}/artifacts/${index}`;
}
export function workerName(job: DurableJob) {
  const label = (job.title || job.label || "").trim().replace(/\s+/g, " ");
  return label || "Agent";
}
export function workerActivity(job: DurableJob) {
  if (job.handoff?.status === "conflict") return "Changes conflict with the destination";
  if (job.handoff?.status === "uncertain") return "Integration outcome needs review";
  if (job.handoff?.status === "rolled_back") return "Changes rolled back; review before continuing";
  if (job.handoff?.status === "review_required") return "Patch requires human review";
  if (job.handoff?.status === "preview") return "Changes ready for integration";
  if (job.handoff?.status === "applying") return "Applying changes";
  if (job.handoff?.status === "applied") return "Changes applied";
  if (job.state === "succeeded") return job.verification?.status === "verified" ? "Finished · checks passed" : "Finished the assigned work";
  if (job.state === "failed") return job.result?.failure_detail?.trim() || "Stopped before completing the assigned work";
  if (job.state === "interrupted") return "Execution interrupted. Inspect the saved activity before continuing.";
  if (job.state === "cancelled") return "Work was cancelled";
  if (job.state === "cancelling") return "Stopping safely";
  if (job.state === "waiting_input") return "Waiting for input or permission";
  if (job.state === "waiting_dependencies") return "Waiting for related work";
  if (job.state === "queued") return "Waiting to start";
  if (job.state === "retry_wait") return "Waiting for the next attempt";
  if (job.health?.worker === "lease_expired") return "Connection interrupted. Progress is currently unavailable.";
  const progress = job.progress;
  if (progress?.current_tool?.status === "pending" || progress?.current_tool?.status === "requested") return `Preparing ${progress.current_tool.label || progress.current_tool.name.replace(/_/g, " ")}`;
  if (progress?.current_tool?.status === "running") return `Using ${progress.current_tool.label || progress.current_tool.name.replace(/_/g, " ")}`;
  const waiting: Record<string, string> = { model_waiting: "Waiting for a response", retry_wait: "Waiting before trying again", model_switching: "Trying another available model", verifying: "Checking the result", tool_failed: "An action needs attention" };
  if (progress?.activity && waiting[progress.activity]) return waiting[progress.activity];
  if (progress?.activity === "waiting_input") {
    return progress.summary || "Waiting for the next step";
  }
  return progress?.agent_update?.summary || progress?.summary || "Working on the assigned task";
}
export function workerEnvironment(job: DurableJob) {
  if (["worktree", "git", "wt"].includes(job.spec?.isolation || "")) return "Own context · isolated checkout";
  if (["browser", "desktop"].includes(job.progress?.surface || "")) return job.spec?.machine_active
    ? "Own context · shared computer" : "Own context · browser in use";
  return "Own context · tools available when needed";
}
export function workerStatus(job: DurableJob, _mainHandling = false) {
  if (job.state === "running" && job.health?.worker === "lease_expired") return "Connection interrupted";
  if (job.state === "running" && job.progress?.activity === "verifying") return "Checking";
  if (job.state === "running" && job.progress?.activity === "retry_wait") return "Retrying";
  if (job.state === "succeeded" && job.verification?.status === "verified") return "Checks passed";
  return ({ running: "Working", queued: "Queued", waiting_input: "Needs input", waiting_dependencies: "Waiting",
    retry_wait: "Retrying", reconciling: "Needs review", succeeded: "Finished", failed: "Failed",
    interrupted: "Interrupted", cancelled: "Cancelled", cancelling: "Stopping" } as Record<string, string>)[job.state] || job.state;
}
