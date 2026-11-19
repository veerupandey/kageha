import { useEffect, useRef } from "react";
import { api } from "../api/client";
import {
  hasNativeSessionEvents,
  subscribeSessionEvents,
} from "../lib/sessionEvents";
import { useAppStore } from "../store";
import type { DurableJob } from "./DurableJobs";

type GlobalJob = { id: string; status: string; objective?: string };
const status = (state: string) =>
  ({ succeeded: "success", failed: "error" })[state] || state;

/** Discover global jobs once; native session changes arrive on the shared stream. */
export function JobCompletionToasts() {
  const showToast = useAppStore((s) => s.showToast);
  const sessionId = useAppStore((s) => s.sessionId);
  const seen = useRef<Record<string, string>>({});
  const primed = useRef(false);
  useEffect(() => {
    let stopped = false;
    let snapshotFloor = 0;
    const nativeIds = new Set<string>();
    const observe = (job: GlobalJob, announce: boolean) => {
      const previous = seen.current[job.id];
      if (
        announce &&
        previous &&
        previous !== job.status &&
        ["success", "error", "cancelled"].includes(job.status)
      ) {
        showToast(
          `Job ${job.status}: ${String(job.objective || job.id).slice(0, 72)}`,
        );
      }
      seen.current[job.id] = job.status;
    };
    const worker = (job: DurableJob, announce: boolean) => {
      nativeIds.add(job.id);
      observe(
        { id: job.id, status: status(job.state), objective: job.label },
        announce,
      );
    };
    const stop = sessionId
      ? subscribeSessionEvents(sessionId, {
          onSnapshot: (snapshot) => {
            snapshotFloor = Math.max(snapshotFloor, snapshot.sequence);
            for (const job of snapshot.workers) worker(job, false);
          },
          onEvent: (event) => {
            if (
              event.kind === "worker" &&
              Number(event.sequence) > snapshotFloor &&
              event.job
            )
              worker(event.job as DurableJob, true);
          },
        })
      : () => {};
    const tick = async (initial = false) => {
      if (!initial && hasNativeSessionEvents(sessionId)) return;
      try {
        const data = await api<{ jobs?: GlobalJob[] }>("/api/jobs?limit=20");
        if (stopped) return;
        for (const job of data.jobs || []) {
          if (!nativeIds.has(job.id)) observe(job, primed.current);
        }
        primed.current = true;
      } catch {
        /* Discovery failure does not interrupt the session stream. */
      }
    };
    void tick(true);
    const timer = window.setInterval(() => void tick(), 8000);
    return () => {
      stopped = true;
      stop();
      window.clearInterval(timer);
    };
  }, [showToast, sessionId]);
  return null;
}
