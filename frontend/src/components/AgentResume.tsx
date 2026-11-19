import { useState } from "react";
import type { DurableJob } from "./DurableJobs";
import { createClientId } from "../lib/clientId";

/** Continuation is an explicit new turn; a transport reconnect never submits it. */
export function AgentResume({
  job,
  onResumed,
}: {
  job: DurableJob;
  onResumed?: (job: DurableJob) => void;
}) {
  const [confirming, setConfirming] = useState(false);
  const [sending, setSending] = useState(false);
  const [requestId, setRequestId] = useState<string | null>(null);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  if (!["failed", "cancelled", "interrupted"].includes(job.state)) return null;
  const resume = async () => {
    if (sending || !job.resume?.allowed) return;
    const id = requestId || createClientId();
    setRequestId(id);
    setSending(true);
    setError("");
    try {
      const response = await fetch(
        `/api/jobs/${encodeURIComponent(job.id)}/resume`,
        {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            client_request_id: id,
            confirmed: true,
            expected_turn_id: job.progress?.turn_id,
            expected_revision: job.revision,
          }),
        },
      );
      const packet = await response.json();
      if (!response.ok)
        throw new Error(
          packet.error ||
            "Could not continue this agent. Review its current state before retrying.",
        );
      setConfirming(false);
      setNotice("Continuation queued. Waiting for the agent to start.");
      if (packet.job) onResumed?.(packet.job);
    } catch (error) {
      setError(
        error instanceof Error
          ? error.message
          : "Could not continue this agent.",
      );
    } finally {
      setSending(false);
    }
  };
  return (
    <section
      aria-label="Continue interrupted work"
      className="space-y-2 rounded-lg border border-line/70 bg-canvas/40 p-3 text-xs"
    >
      {job.resume?.allowed ? (
        <>
          <h3 className="font-medium text-ink">Continue this agent</h3>
          <p className="leading-relaxed text-muted">
            Continue from its saved conversation in a new turn. The interrupted
            turn stays in history; uncertain actions will not be repeated.
          </p>
          {!confirming && !notice && (
            <button
              type="button"
              onClick={() => setConfirming(true)}
              className="min-h-9 rounded-md border border-accent/30 px-3 font-medium text-accent hover:bg-accent/10"
            >
              Resume agent
            </button>
          )}
          {confirming && (
            <div className="flex flex-wrap items-center gap-2">
              <button
                type="button"
                disabled={sending}
                onClick={() => void resume()}
                className="min-h-9 rounded-md bg-accent px-3 font-medium text-surface disabled:opacity-50"
              >
                {sending ? "Continuing…" : "Confirm resume"}
              </button>
              <button
                type="button"
                disabled={sending}
                onClick={() => setConfirming(false)}
                className="min-h-9 rounded-md px-3 text-muted"
              >
                Cancel
              </button>
            </div>
          )}
        </>
      ) : (
        <>
          <h3 className="font-medium text-ink">Review before continuing</h3>
          <p className="leading-relaxed text-muted">
            {job.resume?.reason ||
              "Inspect the saved activity. Continuation is unavailable for this run."}
          </p>
        </>
      )}
      {notice && (
        <p role="status" className="text-accent">
          {notice}
        </p>
      )}
      {error && (
        <p role="alert" className="text-danger">
          {error}
        </p>
      )}
    </section>
  );
}
