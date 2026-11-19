import { createContext, useContext, type ReactNode } from "react";
import { useAppStore } from "../../store";
import { useDurableJobs } from "../DurableJobs";
import { workerActivity, workerStatus } from "../../lib/workerPresentation";

const AgentBriefContext = createContext<ReturnType<typeof useDurableJobs>>({ jobs: [], error: "", loaded: false });

export function LiveAgentBriefs({ children, enabled = true }: { children: ReactNode; enabled?: boolean }) {
  const sessionId = useAppStore(state => state.sessionId);
  const data = useDurableJobs(enabled ? sessionId : null);
  return <AgentBriefContext.Provider value={data}>{children}</AgentBriefContext.Provider>;
}

export function AgentBriefSummary({ jobIds }: { jobIds: string[] }) {
  const { jobs, error } = useContext(AgentBriefContext);
  const tracked = jobs.filter(job => jobIds.includes(job.id));
  if (error) return <span>Live updates unavailable</span>;
  if (tracked.length !== jobIds.length) return <span>Waiting for recorded updates…</span>;
  const completed = tracked.filter(job => job.state === "succeeded").length;
  const needsAttention = tracked.filter(job => ["failed", "waiting_input", "reconciling"].includes(job.state)).length;
  return <span>{completed}/{jobIds.length} completed{needsAttention ? ` · ${needsAttention} need attention` : ""}</span>;
}

export function LiveAgentBrief({ jobId }: { jobId: string }) {
  const { jobs, error } = useContext(AgentBriefContext);
  const job = jobs.find(item => item.id === jobId);
  if (!job) return <p className="mt-2 text-xs text-muted">{error ? "Live update unavailable" : "Waiting for a recorded update…"}</p>;
  return <div aria-label={`${job.label} activity`} className="mt-2 space-y-1 border-t border-line pt-2 text-xs">
    {error && <p role="status" className="text-warn">Updates unavailable · showing last recorded activity</p>}
    <p className="font-medium text-ink">{workerStatus(job)}</p>
    <p className="break-words text-muted">{workerActivity(job)}</p>
    {job.progress?.agent_update?.next_step && !["succeeded", "failed", "cancelled", "cancelling"].includes(job.state) && <p className="break-words text-muted">Next: {job.progress.agent_update.next_step}</p>}
    {job.result?.contribution?.finding && <p className="line-clamp-3 break-words text-ink">{job.result.contribution.finding}</p>}
  </div>;
}
