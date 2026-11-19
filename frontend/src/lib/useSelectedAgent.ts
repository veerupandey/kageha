import { useCallback, useEffect, useState } from "react";
import type { DurableJob } from "../components/DurableJobs";
import { useDurableJobs } from "../components/DurableJobs";
import { agentKey, useAgentValue } from "./agentContinuity";
import { parseUrlState, updateUrl } from "./urlSync";

export function useSelectedAgent(sessionId: string | null) {
  const [savedId, saveId] = useAgentValue<string | null>(agentKey(sessionId || "", "selected-agent"), null);
  const [selection, setSelection] = useState<{ session: string; job: DurableJob } | null>(null);
  const { jobs, error, loaded } = useDurableJobs(savedId ? sessionId : null);
  const liveJob = jobs.find(item => item.id === savedId && item.kind === "agent" && !item.spec?.continuation);
  const selected = liveJob || (selection?.session === sessionId && selection.job.id === savedId ? selection.job : null);
  const select = useCallback((job: DurableJob | null) => {
    saveId(job?.id || null);
    setSelection(job && sessionId ? { session: sessionId, job } : null);
    updateUrl({ sessionId, agentId: job?.id || null }, { replace: true });
  }, [sessionId, saveId]);
  useEffect(() => {
    const url = parseUrlState();
    const urlAgent = url.agentId;
    if (urlAgent && url.sessionId === sessionId && !savedId && sessionId) {
      saveId(urlAgent);
    }
  }, [sessionId, savedId, saveId]);
  useEffect(() => {
    // Only a successful scoped snapshot can establish that an agent is gone.
    // Restoration shares the bounded connection and retries with other panels.
    if (sessionId && savedId && !selected && loaded && !error) {
      saveId(null);
      updateUrl({ agentId: null }, { replace: true });
    }
  }, [sessionId, savedId, selected, loaded, error, saveId]);
  return [selected, select] as const;
}
