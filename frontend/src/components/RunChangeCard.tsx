import { useEffect, useState } from "react";
import { ApiError, api } from "../api/client";
import { refreshSessionTruth } from "../lib/sessionTruth";
import { useAppStore } from "../store";

interface Proposal {
  turn: string;
  kind: "plan" | "goal" | "extend";
  reason: string;
  next_action: string;
  completion_condition: string | null;
  evidence: string[];
  model_calls: number;
  successor?: string | null;
}

interface Review {
  proposal: Proposal | null;
  deadline_seconds: number | null;
}

const titles = { plan: "Switch to Plan", goal: "Start Goal", extend: "Extend budget" };
const changed = "kageha-run-change";

export function RunChangeCard({ sessionId }: { sessionId: string }) {
  const [review, setReview] = useState<Review | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const endpoint = `/api/sessions/${encodeURIComponent(sessionId)}/run-change`;

  useEffect(() => {
    const controller = new AbortController();
    let request = 0;
    async function refresh() {
      const revision = ++request;
      try {
        const response = await api<Review>(endpoint, { signal: controller.signal });
        if (!controller.signal.aborted && revision === request) setReview(response);
      } catch (error) {
        if (error instanceof ApiError && error.status === 404) clearInterval(timer);
      }
    }
    const timer = setInterval(() => void refresh(), 5000);
    window.addEventListener(changed, refresh);
    void refresh();
    return () => {
      controller.abort();
      clearInterval(timer);
      window.removeEventListener(changed, refresh);
    };
  }, [endpoint]);

  const proposal = review?.proposal;
  if (!proposal) return null;
  const deadline = review.deadline_seconds;

  async function decide(action: "accept" | "dismiss") {
    if (!proposal) return;
    setBusy(true);
    setError("");
    try {
      const decision = await api<Review>(endpoint, { method: "POST", body: JSON.stringify({ action, turn: proposal.turn }) });
      setReview(null);
      window.dispatchEvent(new Event(changed));
      if (action === "accept") {
        const store = useAppStore.getState();
        if (store.sessionId === sessionId && proposal.kind !== "extend") {
          store.setAgentMode(proposal.kind);
        }
        if (decision.proposal?.successor) store.followRunChange(sessionId, decision.proposal.successor);
        refreshSessionTruth(sessionId);
      }
    } catch (error) {
      setError(error instanceof Error ? error.message : "Could not save the decision.");
    } finally {
      setBusy(false);
    }
  }

  return (
    <section aria-label="Proposed run change" className="mb-2 space-y-1.5 rounded-md border border-line px-3 py-2 text-xs">
      <p className="font-medium text-ink">{titles[proposal.kind]}</p>
      <p className="break-words text-muted">{proposal.reason}</p>
      <p className="break-words text-muted">{proposal.next_action}</p>
      {proposal.completion_condition && <p className="break-words text-muted">Completion condition · {proposal.completion_condition}</p>}
      {proposal.evidence.length > 0 && <p className="text-muted">{proposal.evidence.length} verified tool {proposal.evidence.length === 1 ? "result" : "results"} saved</p>}
      {proposal.kind === "extend" && <p className="text-muted">
        One extra turn · up to {proposal.model_calls} model calls
        {deadline && <> · {deadline / 60} min execution deadline</>}
      </p>}
      <div className="flex gap-2">
        <button type="button" disabled={busy} className="ka-icon-btn px-2" onClick={() => void decide("accept")}>Accept</button>
        <button type="button" disabled={busy} className="ka-icon-btn px-2" onClick={() => void decide("dismiss")}>Dismiss</button>
      </div>
      {error && <p role="alert" className="text-danger">{error}</p>}
    </section>
  );
}
