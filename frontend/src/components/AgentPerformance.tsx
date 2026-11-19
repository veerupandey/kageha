import { useEffect, useMemo, useState, type ReactNode } from "react";
import { api } from "../api/client";
import { Icon } from "../lib/icons";

interface SpeedRow {
  agent: string;
  task_type: string;
  week: string;
  requests: number;
  verified: number;
  elapsed_samples: number;
  median_elapsed_ms: number | null;
  p95_elapsed_ms: number | null;
  mean_model_calls: number;
}
interface PerformanceResponse { enabled: boolean; speed_metrics: SpeedRow[] }

function duration(ms: number | null): string {
  if (ms == null || !Number.isFinite(ms) || ms < 0) return "—";
  const seconds = Math.round(ms / 1000);
  if (seconds < 60) return `${seconds}s`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ${seconds % 60}s`;
  return `${Math.floor(seconds / 3600)}h ${Math.floor((seconds % 3600) / 60)}m`;
}

function cohortKey(row: SpeedRow): string {
  return JSON.stringify([row.agent, row.task_type]);
}

function hasTiming(row: SpeedRow): boolean {
  return row.elapsed_samples > 0 && row.median_elapsed_ms != null && Number.isFinite(row.median_elapsed_ms) && row.median_elapsed_ms >= 0;
}

function Metric({ label, children }: { label: string; children: ReactNode }) {
  return <div className="min-w-0">
    <dt className="text-[11px] font-medium text-muted">{label}</dt>
    <dd className="mt-1 font-medium tabular-nums text-ink">{children}</dd>
  </div>;
}

export function AgentPerformance() {
  const [data, setData] = useState<PerformanceResponse>();
  const [error, setError] = useState("");
  const [attempt, setAttempt] = useState(0);
  const [selected, setSelected] = useState("");

  useEffect(() => {
    let active = true;
    setError("");
    void api<PerformanceResponse>("/api/learning/metrics?weeks=12").then(result => {
      if (!result || !Array.isArray(result.speed_metrics)) throw new Error("Performance data is unavailable.");
      if (active) setData(result);
    }).catch(cause => { if (active) setError(cause instanceof Error ? cause.message : String(cause)); });
    return () => { active = false; };
  }, [attempt]);

  const rows = useMemo(() => [...(data?.speed_metrics || [])].sort((a, b) =>
    b.week.localeCompare(a.week) || a.agent.localeCompare(b.agent) || a.task_type.localeCompare(b.task_type)), [data]);
  const cohorts = useMemo(() => {
    const groups = new Map<string, { row: SpeedRow; timedWeeks: Set<string> }>();
    for (const row of rows) {
      const key = cohortKey(row);
      let group = groups.get(key);
      if (!group) {
        group = { row, timedWeeks: new Set() };
        groups.set(key, group);
      }
      if (hasTiming(row)) group.timedWeeks.add(row.week);
    }
    return [...groups.entries()]
      .sort(([, a], [, b]) => b.timedWeeks.size - a.timedWeeks.size || b.row.week.localeCompare(a.row.week)
        || a.row.agent.localeCompare(b.row.agent) || a.row.task_type.localeCompare(b.row.task_type))
      .map(([key, group]) => [key, group.row] as const);
  }, [rows]);
  const chosen = cohorts.some(([key]) => key === selected) ? selected : cohorts[0]?.[0] || "";
  const observations = rows.filter(row => cohortKey(row) === chosen);
  const timed = observations.filter(hasTiming).slice().reverse();
  const maxMedian = Math.max(0, ...timed.map(row => row.median_elapsed_ms || 0));
  const requestCount = rows.reduce((sum, row) => sum + row.requests, 0);
  const verifiedCount = rows.reduce((sum, row) => sum + row.verified, 0);
  const timedCount = rows.reduce((sum, row) => sum + row.elapsed_samples, 0);

  return <section aria-label="Agent performance" className="space-y-5 text-sm">
    <div className="relative overflow-hidden rounded-2xl border border-line bg-surface px-5 py-5 sm:px-6 sm:py-6">
      <div aria-hidden="true" className="pointer-events-none absolute -right-10 -top-16 h-40 w-40 rounded-full bg-accent/10 blur-3xl" />
      <div className="relative flex items-start gap-3">
        <span className="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl border border-accent/20 bg-accent-soft text-accent"><Icon.Activity size={20} aria-hidden="true" /></span>
        <div className="min-w-0">
          <p className="text-xs font-medium text-accent">Last 12 weeks</p>
          <h3 className="mt-1 text-base font-semibold tracking-tight text-ink sm:text-lg">A closer look at your agents’ work</h3>
          <p className="mt-1.5 max-w-2xl text-xs leading-relaxed text-muted">Explore verified task durations by agent and task type. Each observation is a weekly snapshot.</p>
        </div>
      </div>
      {data?.enabled && rows.length > 0 && <dl className="relative mt-5 grid grid-cols-3 gap-4 border-t border-line/70 pt-4">
        <Metric label="Categorized requests">{requestCount}</Metric>
        <Metric label="Verified">{verifiedCount}</Metric>
        <Metric label="With timing">{timedCount}</Metric>
      </dl>}
    </div>

    {error && <div role="alert" className="rounded-xl border border-warn/30 bg-warn-soft px-4 py-3 text-xs text-warn">
      <p>{error}</p><button type="button" className="mt-2 font-medium underline underline-offset-2 focus-visible:outline-2 focus-visible:outline-accent" onClick={() => { setData(undefined); setAttempt(value => value + 1); }}>Try again</button>
    </div>}
    {!data && !error && <p role="status" className="px-1 text-xs text-muted">Loading performance…</p>}
    {data && !data.enabled && <div className="rounded-xl border border-line bg-surface/50 px-5 py-5 text-xs text-muted"><p className="font-medium text-ink">Measurements unavailable</p><p className="mt-1.5 leading-relaxed">Learning measurements are unavailable.</p></div>}
    {data?.enabled && rows.length === 0 && <div className="rounded-xl border border-dashed border-line-strong bg-surface/40 px-5 py-7 text-center">
      <Icon.BarChart size={20} className="mx-auto text-faint" aria-hidden="true" />
      <p className="mt-3 text-sm font-medium text-ink">Your history starts here</p>
      <p className="mx-auto mt-1.5 max-w-sm text-xs leading-relaxed text-muted">No timed, categorized requests yet. Results appear as requests are recorded.</p>
    </div>}

    {data?.enabled && rows.length > 0 && <>
      <div className="rounded-2xl border border-line bg-surface/60 p-4 sm:p-5">
        <div className="flex flex-col gap-3 sm:flex-row sm:items-end sm:justify-between">
          <div><h4 className="font-semibold text-ink">Weekly median</h4><p className="mt-1 text-xs text-muted">Compare one agent and task type at a time.</p></div>
          <label className="min-w-0 text-xs text-muted sm:w-60">Agent and task type
            <select aria-label="Agent and task type" value={chosen} onChange={event => setSelected(event.target.value)} className="mt-1.5 block w-full rounded-lg border border-line-strong bg-canvas px-3 py-2 text-xs text-ink focus-visible:outline-2 focus-visible:outline-accent">
              {cohorts.map(([key, row]) => <option key={key} value={key}>{row.agent} · {row.task_type}</option>)}
            </select>
          </label>
        </div>
        {timed.length > 0 ? <>
          <ol aria-label="Weekly median durations" className="mt-5 space-y-3">
            {timed.map(row => <li key={row.week} className="grid grid-cols-[4.5rem_minmax(0,1fr)_4.25rem] items-center gap-2 text-xs sm:grid-cols-[5.5rem_minmax(0,1fr)_5rem] sm:gap-3">
              <span className="tabular-nums text-muted">{row.week}</span>
              <span className="h-2 overflow-hidden rounded-full bg-line/70" aria-hidden="true"><span className="block h-full rounded-full bg-accent" style={{ width: `${maxMedian > 0 ? (row.median_elapsed_ms || 0) / maxMedian * 100 : 0}%` }} /></span>
              <span className="text-right font-medium tabular-nums text-ink">{duration(row.median_elapsed_ms)}</span>
              <span className="sr-only"> from {row.elapsed_samples} timed {row.elapsed_samples === 1 ? "request" : "requests"}</span>
            </li>)}
          </ol>
          <p className="mt-4 text-[11px] leading-relaxed text-faint">Bars are relative to the longest median in this group. Only verified requests with timing are shown.</p>
        </> : <p className="mt-5 rounded-lg bg-canvas/70 px-4 py-4 text-xs text-muted">No verified timings are available for this group yet.</p>}
      </div>

      <div>
        <div className="mb-3 flex items-baseline justify-between gap-2"><h4 className="font-semibold text-ink">Weekly observations</h4><span className="text-xs text-muted">{observations.length} {observations.length === 1 ? "week" : "weeks"}</span></div>
        <ol aria-label="Weekly agent performance by task type" className="overflow-hidden rounded-xl border border-line bg-surface/50">
          {observations.map(row => <li key={row.week} className="border-b border-line/70 px-4 py-4 last:border-b-0 sm:px-5">
            <div className="flex flex-wrap items-center justify-between gap-x-3 gap-y-1"><p className="text-sm font-semibold text-ink">{row.week}</p><p className="text-xs text-muted">{row.agent} <span aria-hidden="true">·</span> {row.task_type}</p></div>
            <dl className="mt-3 grid grid-cols-2 gap-x-4 gap-y-3 text-xs sm:grid-cols-4">
              <Metric label="Verified">{row.verified}/{row.requests}</Metric>
              <Metric label="Median">{duration(row.median_elapsed_ms)} <span className="font-normal text-muted">({row.elapsed_samples} timed)</span></Metric>
              <Metric label="95th percentile">{duration(row.p95_elapsed_ms)}</Metric>
              <Metric label="Avg. model calls">{row.mean_model_calls.toFixed(1)}</Metric>
            </dl>
          </li>)}
        </ol>
      </div>
      <div className="flex gap-3 rounded-xl border border-line bg-canvas/50 px-4 py-3 text-xs leading-relaxed text-muted">
        <Icon.Alert size={15} className="mt-0.5 shrink-0 text-faint" aria-hidden="true" />
        <p>Durations include time waiting for people and time offline. Different tasks and models can make weeks incomparable, so a change is not proof of a speedup. Durations use verified requests only; model calls are averaged across all requests. Cost is not available.</p>
      </div>
    </>}
  </section>;
}
