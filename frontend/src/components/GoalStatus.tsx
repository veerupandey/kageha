import { useEffect, useState } from "react";
import { api, ApiError } from "../api/client";

interface Goal {
  objective: string;
  state: "active" | "paused" | "blocked" | "completed" | "budget_limited";
  reason: string;
  turns: number;
}

const labels = {
  active: "Active",
  paused: "Paused",
  blocked: "Blocked",
  completed: "Completed",
  budget_limited: "Budget reached",
};

export function GoalStatus({ sessionId }: { sessionId: string }) {
  const [goal, setGoal] = useState<Goal | null>(null);
  const [pausing, setPausing] = useState(false);
  const [error, setError] = useState("");
  const endpoint = `/api/sessions/${encodeURIComponent(sessionId)}/goal`;

  useEffect(() => {
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout>;
    async function refresh() {
      try {
        const result = await api<{ goal?: Goal | null }>(endpoint, { signal: controller.signal });
        if (controller.signal.aborted) return;
        setGoal(result.goal ?? null);
        timer = setTimeout(refresh, result.goal?.state === "active" ? 2000 : 5000);
      } catch (error) {
        if (!controller.signal.aborted && !(error instanceof ApiError && error.status === 404)) {
          timer = setTimeout(refresh, 5000);
        }
      }
    }
    void refresh();
    return () => {
      controller.abort();
      clearTimeout(timer);
    };
  }, [endpoint]);

  if (!goal) return null;

  async function pause() {
    setPausing(true);
    setError("");
    try {
      const result = await api<{ goal: Goal | null }>(endpoint, {
        method: "POST",
        body: JSON.stringify({ action: "pause" }),
      });
      setGoal(result.goal);
    } catch (error) {
      setError(error instanceof Error ? error.message : "Could not pause the goal.");
    } finally {
      setPausing(false);
    }
  }

  return (
    <section aria-label="Goal" className="mb-2 rounded-md border border-line px-3 py-2 text-xs">
      <div className="flex items-center justify-between gap-2">
        <span className="font-medium text-ink">Goal · {labels[goal.state]} · {goal.turns} {goal.turns === 1 ? "turn" : "turns"}</span>
        {goal.state === "active" && <button type="button" disabled={pausing}
          className="ka-icon-btn px-2" onClick={() => void pause()}>
          {pausing ? "Pausing…" : "Pause goal"}
        </button>}
      </div>
      <p className="mt-1 line-clamp-2 break-words text-muted">{goal.objective}</p>
      {goal.reason && <p className="mt-1 break-words text-muted">{goal.reason}</p>}
      {goal.state !== "active" && goal.state !== "completed" && (
        <p className="mt-1 text-muted">Send a Goal follow-up to resume.</p>
      )}
      {error && <p role="alert" className="mt-1 text-danger">{error}</p>}
    </section>
  );
}
