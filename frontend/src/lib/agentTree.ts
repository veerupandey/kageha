import type { DurableJob } from "../components/DurableJobs";
import { WORKER_TERMINAL } from "./workerPresentation";

export interface AgentNode {
  job: DurableJob;
  parentId: string | null;
  depth: number;
  children: string[];
}
/** Derive hierarchy from stable identities. Missing parents and corrupt cycles stay inspectable. */
export function agentOutline(jobs: DurableJob[]): AgentNode[] {
  const nodes = new Map(
    jobs.map((job) => [
      job.id,
      {
        job,
        parentId: null as string | null,
        depth: 0,
        children: [] as string[],
      },
    ]),
  );
  const sessions = new Map(jobs.map((job) => [job.session_id, job.id]));
  for (const node of nodes.values()) {
    const proposed =
      node.job.parent_job_id || sessions.get(node.job.parent_session || "");
    if (proposed && proposed !== node.job.id && nodes.has(proposed))
      node.parentId = proposed;
  }
  for (const node of nodes.values()) {
    const seen = new Set([node.job.id]);
    let parent = node.parentId;
    while (parent) {
      if (seen.has(parent)) {
        node.parentId = null;
        break;
      }
      seen.add(parent);
      parent = nodes.get(parent)?.parentId || null;
    }
  }
  for (const node of nodes.values())
    if (node.parentId) nodes.get(node.parentId)?.children.push(node.job.id);
  const ordered: AgentNode[] = [];
  const append = (node: AgentNode, depth: number) => {
    node.depth = depth;
    ordered.push(node);
    for (const id of node.children) append(nodes.get(id)!, depth + 1);
  };
  for (const node of nodes.values()) if (!node.parentId) append(node, 0);
  return ordered;
}
export function agentElapsed(job: DurableJob, now: number): number | null {
  if (
    WORKER_TERMINAL.has(job.state) &&
    Number.isFinite(job.progress?.elapsed_s)
  )
    return Math.max(0, job.progress!.elapsed_s!);
  if (!job.started_at)
    return Number.isFinite(job.progress?.elapsed_s)
      ? Math.max(0, job.progress!.elapsed_s!)
      : null;
  const end =
    job.ended_at ||
    (WORKER_TERMINAL.has(job.state) ? job.progress?.updated_at : now);
  return end && Number.isFinite(end) ? Math.max(0, end - job.started_at) : null;
}
export function elapsedLabel(seconds: number | null) {
  if (seconds === null) return "Not started";
  const n = Math.floor(seconds);
  return n >= 3600
    ? `${Math.floor(n / 3600)}h ${Math.floor((n % 3600) / 60)}m`
    : n >= 60
      ? `${Math.floor(n / 60)}m ${n % 60}s`
      : `${n}s`;
}
