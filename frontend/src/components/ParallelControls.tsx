import { useCallback, useEffect, useState } from "react";
import { api } from "../api/client";

interface ParallelStatus {
  enabled: boolean;
  mode: "auto" | "fixed";
  max_agents: number;
  effective_agents: number;
  active_agents: number;
  available_slots: number;
  cpu_count: number;
  memory_available_bytes: number | null;
  jobs: { running: number; queued: number; waiting: number };
}

export function ParallelControls() {
  const [status, setStatus] = useState<ParallelStatus | null>(null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");

  const refresh = useCallback(async () => {
    try {
      setStatus(await api<ParallelStatus>("/api/parallelism"));
      setError("");
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Could not load parallel work settings");
    }
  }, []);

  useEffect(() => {
    void refresh();
    const timer = setInterval(() => void refresh(), 5000);
    return () => clearInterval(timer);
  }, [refresh]);

  const update = async (change: Partial<Pick<ParallelStatus, "enabled" | "mode" | "max_agents">>) => {
    setSaving(true);
    try {
      setStatus(await api<ParallelStatus>("/api/parallelism", {
        method: "POST",
        body: JSON.stringify(change),
      }));
      setError("");
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Could not update parallel work settings");
    } finally {
      setSaving(false);
    }
  };

  return (
    <section className="space-y-2 border-b border-line px-4 py-3" aria-label="Parallel agents">
      <div className="flex items-center justify-between gap-3">
        <div>
          <h3 className="text-xs font-semibold text-ink">Parallel agents</h3>
          <p className="text-[0.65rem] text-faint">Queue agents in the background and resume when they finish.</p>
        </div>
        <label className="flex items-center gap-2 text-xs text-ink">
          <input
            type="checkbox"
            aria-label="Enable durable agents"
            checked={status?.enabled ?? false}
            disabled={!status || saving}
            onChange={(event) => void update({ enabled: event.target.checked })}
          />
          Enabled
        </label>
      </div>
      {status && (
        <>
          <p className="text-[0.7rem] text-muted">
            {status.jobs.running} running · {status.jobs.queued} queued · {status.jobs.waiting} waiting
            {status.enabled ? ` · ${status.available_slots} slots open` : ""}
          </p>
          <div className="flex items-center gap-2 text-xs text-ink">
            <label htmlFor="parallel-mode">Mode</label>
            <select
              id="parallel-mode"
              className="rounded border border-line bg-canvas px-2 py-1"
              value={status.mode}
              disabled={saving}
              onChange={(event) => void update({ mode: event.target.value as ParallelStatus["mode"] })}
            >
              <option value="auto">Auto</option>
              <option value="fixed">Fixed</option>
            </select>
            <label htmlFor="parallel-max-agents">Max</label>
            <select
              id="parallel-max-agents"
              className="rounded border border-line bg-canvas px-2 py-1"
              value={status.max_agents}
              disabled={saving}
              onChange={(event) => void update({ max_agents: Number(event.target.value) })}
            >
              {Array.from({ length: 8 }, (_, index) => index + 1).map((count) => (
                <option key={count} value={count}>{count}</option>
              ))}
            </select>
          </div>
          <p className="text-[0.65rem] text-faint">
            Effective limit {status.effective_agents} · {status.cpu_count} CPUs
            {status.memory_available_bytes !== null
              ? ` · ${(status.memory_available_bytes / 1024 ** 3).toFixed(1)} GiB available`
              : ""}
          </p>
          {status.mode === "auto" && (
            <p className="text-[0.65rem] text-faint">Auto adjusts new starts to CPU and memory headroom, up to Max.</p>
          )}
        </>
      )}
      {error && <p role="alert" className="text-xs text-[var(--color-danger)]">{error}</p>}
    </section>
  );
}
