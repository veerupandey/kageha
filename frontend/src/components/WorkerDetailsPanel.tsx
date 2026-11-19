import { useEffect, useRef, useState } from "react";
import { useAppStore } from "../store";
import { JobQuestion, useDurableJobs, type DurableJob } from "./DurableJobs";
import { WorkerCommunication } from "./WorkerCommunication";
import { AgentTree } from "./AgentTree";
import { AgentResume } from "./AgentResume";
import { agentElapsed, elapsedLabel } from "../lib/agentTree";
import { WorkerTrace } from "./WorkerTrace";
import { WORKER_TERMINAL, workerActivity, workerArtifactUrl, workerEnvironment, workerName, workerStatus } from "../lib/workerPresentation";
import { AgentContributionDetails } from "./AgentContribution";
import { AgentDisclosure } from "./AgentDisclosure";
import { WorkerHandoff } from "./WorkerHandoff";
import { Icon } from "../lib/icons";
import { cn } from "../lib/cn";

export function WorkerDetailsPanel({ initialJob, parentSession, onClose, onSelectWorker }: { initialJob: DurableJob; parentSession: string; onClose: () => void; onSelectWorker?: (job: DurableJob) => void }) {
  const { jobs, error } = useDurableJobs(parentSession);
  const job = jobs.find(item => item.id === initialJob.id) || initialJob;
  const specialists = jobs.filter(item => item.kind === "agent" && !item.spec?.continuation &&
    (item.session_id !== parentSession || (item.parent_session && item.parent_session !== parentSession)));
  const openSession = useAppStore(s => s.openSession);
  const closeRef = useRef<HTMLButtonElement>(null);
  const panelRef = useRef<HTMLElement>(null);
  const [mobile, setMobile] = useState(() => window.innerWidth < 1024);
  useEffect(() => {
    const resize = () => setMobile(window.innerWidth < 1024);
    window.addEventListener("resize", resize);
    return () => window.removeEventListener("resize", resize);
  }, []);
  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null;
    closeRef.current?.focus();
    return () => { if (previous?.isConnected) previous.focus(); };
  }, []);
  useEffect(() => {
    const close = (event: KeyboardEvent) => {
      if (event.key === "Escape") { event.preventDefault(); onClose(); }
      if (event.key === "Tab" && mobile) {
        const elements = Array.from(panelRef.current?.querySelectorAll<HTMLElement>('button:not([disabled]), a[href], input:not([disabled]), textarea:not([disabled]), summary, [tabindex="0"]') || []).filter(element => element.getClientRects().length);
        const first = elements[0], last = elements[elements.length - 1];
        if (event.shiftKey && document.activeElement === first && last) { event.preventDefault(); last.focus(); }
        else if (!event.shiftKey && document.activeElement === last && first) { event.preventDefault(); first.focus(); }
      }
    };
    window.addEventListener("keydown", close);
    return () => window.removeEventListener("keydown", close);
  }, [onClose, mobile]);
  const actionScope = `${job.id}:${job.progress?.turn_id || ""}`;
  const actionScopeRef = useRef(actionScope);
  actionScopeRef.current = actionScope;
  const [stopping, setStopping] = useState(false);
  const [stopRequested, setStopRequested] = useState(false);
  const [actionError, setActionError] = useState("");
  useEffect(() => { setStopping(false); setStopRequested(false); setActionError(""); }, [actionScope]);
  const terminal = WORKER_TERMINAL.has(job.state);
  const [now, setNow] = useState(() => Date.now() / 1000);
  useEffect(() => { if (terminal) return; const timer = setInterval(() => setNow(Date.now() / 1000), 1000); return () => clearInterval(timer); }, [terminal]);
  const parentJob = specialists.find(item => item.id === job.parent_job_id || item.session_id === job.parent_session);
  const interrupted = job.health?.worker === "lease_expired" && job.state === "running";
  const update = job.progress?.agent_update;
  const name = workerName(job);
  const disclosureScope = `${parentSession}:${job.id}`;
  const contribution = job.result.contribution || job.progress?.contribution;
  const stop = async () => {
    if (stopping || stopRequested) return;
    const scope = actionScope;
    setStopping(true); setActionError("");
    try {
      const response = await fetch(`/api/jobs/${encodeURIComponent(job.id)}/cancel`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ expected_turn_id: job.progress?.turn_id }) });
      if (!response.ok) throw new Error("Could not stop this agent. Please try again.");
      if (actionScopeRef.current === scope) setStopRequested(true);
    } catch (error) { if (actionScopeRef.current === scope) setActionError(error instanceof Error ? error.message : "Could not stop this agent."); }
    finally { if (actionScopeRef.current === scope) setStopping(false); }
  };
  return <section ref={panelRef} role={mobile ? "dialog" : "region"} aria-modal={mobile || undefined} aria-label="Agent details" className="ka-agent-details flex min-h-0 flex-1 flex-col bg-surface">
    <header className="flex shrink-0 items-center gap-3.5 border-b border-line/80 bg-surface/80 px-4 py-3.5">
      <div className="relative shrink-0">
        <span aria-hidden="true" className="flex h-10 w-10 items-center justify-center rounded-xl bg-accent/5 border border-accent/30 text-sm font-bold text-accent">
          {name.charAt(0).toUpperCase()}
        </span>
        <span
          className={cn(
            "absolute -bottom-0.5 -right-0.5 h-3 w-3 rounded-full border-2 border-surface",
            job.state === "waiting_input"
              ? "bg-amber-400"
              : job.state === "running"
                ? "bg-emerald-400"
                : job.state === "succeeded"
                  ? "bg-emerald-500"
                  : job.state === "failed"
                    ? "bg-rose-500"
                    : "bg-muted",
          )}
        />
      </div>
      <div className="min-w-0 flex-1">
        <h2 title={name} className="truncate text-base font-semibold tracking-tight text-ink">{name}</h2>
        <p className="mt-0.5 text-xs text-muted"><span className="ka-agent-status" data-state={job.state}>{workerStatus(job)}</span><span className="ml-2 tabular-nums text-faint">{elapsedLabel(agentElapsed(job, now))}</span></p>
      </div>
      <button ref={closeRef} aria-label="Close worker details" onClick={onClose} className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg border border-transparent text-muted hover:border-line hover:bg-accent/10 hover:text-ink transition-colors">
        <Icon.Close size={17} />
      </button>
    </header>
    {specialists.length > 0 && onSelectWorker && <AgentTree jobs={specialists} selectedId={job.id} onSelect={onSelectWorker} />}
    {(!terminal || actionError) && <div className="shrink-0 border-b border-line/60 bg-surface/40 px-4 py-2.5">
      <div className="flex items-center justify-between gap-3">
        <div className="min-w-0 flex-1"><WorkerCommunication key={job.id} job={stopRequested ? { ...job, state: "cancelling" } : job} variant="compose" /></div>
        {job.can_cancel && <button disabled={stopping || stopRequested || job.state === "cancelling"} onClick={() => void stop()} className="shrink-0 rounded-lg border border-line/80 bg-surface/60 px-3 py-1.5 text-xs font-medium text-muted hover:border-rose-500/35 hover:bg-rose-500/10 hover:text-rose-400 disabled:opacity-50 transition-colors">{stopping || stopRequested || job.state === "cancelling" ? "Stopping…" : "Stop agent"}</button>}
      </div>
      {actionError && <p role="alert" className="mt-2 rounded-lg border border-rose-500/30 bg-rose-500/10 p-2.5 text-xs font-medium text-rose-400">{actionError}</p>}
      {stopRequested && !terminal && <p role="status" className="mt-2 text-xs text-muted">Stop requested. Waiting for the agent to finish stopping.</p>}
    </div>}
    <div className="min-h-0 flex-1 space-y-5 overflow-y-auto overscroll-contain p-4 text-sm leading-relaxed">
      {parentJob && <button type="button" onClick={() => onSelectWorker?.(parentJob)} className="text-xs text-muted hover:text-accent">Delegated by {workerName(parentJob)}</button>}
      {job.progress?.current_tool && <div aria-label="Current tool" className="flex items-center justify-between gap-3 border-b border-line/60 pb-3 text-xs"><span className="min-w-0 truncate text-ink" title={job.progress.current_tool.name}>{job.progress.current_tool.label || job.progress.current_tool.name}</span><span className="shrink-0 text-muted">{job.progress.current_tool.status === "running" ? "In progress" : job.progress.current_tool.status}</span></div>}
      {job.state === "failed" && job.result?.failure_detail && <div role="alert" className="rounded-xl border border-rose-500/30 bg-rose-500/10 p-3 text-xs text-ink"><span className="font-semibold">Why this agent stopped: </span>{job.result.failure_detail}</div>}
      {job.handoff && <WorkerHandoff handoff={job.handoff} />}
      <AgentResume key={`${job.id}:${job.progress?.turn_id}`} job={job} onResumed={onSelectWorker} />
      {terminal && <WorkerCommunication key={`${job.id}-saved-draft`} job={job} variant="compose" />}
      {interrupted && <div role="status" className="rounded-xl border border-amber-500/30 bg-amber-500/10 p-3.5 text-ink"><p className="font-semibold text-amber-300">Live updates paused</p><p className="mt-1 text-xs text-muted">Live progress is unavailable. Your conversation is still here.</p></div>}
      {job.state === "waiting_input" && (job.question?.question || job.error.approval_id) && <section className="rounded-2xl border border-amber-500/35 bg-accent/5 p-4">
        <div className="flex items-center gap-2 mb-1">
          <span className="flex h-5 w-5 items-center justify-center rounded-md bg-amber-500/20 text-amber-500 text-xs font-bold">!</span>
          <h3 className="text-sm font-semibold text-ink">{name} needs input</h3>
        </div>
        <JobQuestion key={job.error?.approval_id || job.question?.question} job={job} />
      </section>}
      <section aria-label={terminal ? "Agent result" : "Current work"} className="space-y-3">
        <div className="flex items-center justify-between">
          <h3 className="text-xs font-semibold text-muted flex items-center gap-1.5">
            <span className={cn(
              "h-2 w-2 rounded-full",
              job.state === "succeeded" ? "bg-emerald-400" :
              job.state === "waiting_input" ? "bg-amber-400" :
              job.state === "failed" ? "bg-rose-400" :
              "bg-accent",
            )} />
            {contribution ? job.state === "succeeded" ? "Contribution" : terminal ? "Partial findings" : "Draft contribution" : terminal ? "Result" : "Current activity"}
          </h3>
        </div>
        {contribution ? <AgentContributionDetails job={job} /> : <div className="rounded-xl border border-line/70 bg-sidebar/40 p-4"><p className="whitespace-pre-wrap break-words text-sm leading-relaxed text-ink selection:bg-accent/20">{job.result?.message || update?.summary || workerActivity(job)}</p></div>}
        {!contribution && !terminal && !stopRequested && job.state !== "cancelling" && update?.next_step && <div className="rounded-xl border border-accent/25 bg-accent/[0.04] p-3 text-xs"><p className="font-semibold uppercase tracking-wider text-accent mb-1">Next</p><p className="whitespace-pre-wrap break-words leading-relaxed text-ink">{update.next_step}</p></div>}
        {job.state === "succeeded" && <p className={cn("text-xs", job.verification?.status === "verified" ? "text-success" : "text-muted")}>{job.verification?.status === "verified" ? "✓ Recorded checks passed" : "Completed · verification not recorded"}</p>}
      </section>
      {job.artifacts.length > 0 && <section className="space-y-2.5">
        <h3 className="text-xs font-semibold text-muted flex items-center gap-1.5">
          <span className="h-2 w-2 rounded-full bg-accent/60" />
          Files
        </h3>
        <div className="grid grid-cols-1 gap-2">
          {job.artifacts.map((path, index) => {
            const fileName = path.split("/").pop() || path;
            const ext = fileName.includes(".") ? fileName.split(".").pop()?.toUpperCase() : "FILE";
            return (
              <a
                key={path}
                aria-label={fileName}
                className="group flex items-center justify-between gap-3 rounded-xl border border-line/70 bg-surface/50 p-3 hover:border-accent/40 hover:bg-accent/[0.03] transition-colors"
                href={workerArtifactUrl(job, path, index)}
                target="_blank"
                rel="noreferrer"
              >
                <div className="flex items-center gap-2.5 min-w-0 flex-1">
                  <span aria-hidden="true" className="flex h-7 w-8 shrink-0 items-center justify-center rounded-md border border-line/80 bg-canvas/70 font-mono text-[11px] font-bold text-muted group-hover:border-accent/30 group-hover:text-accent transition-colors">
                    {ext?.slice(0, 4)}
                  </span>
                  <span className="break-all text-xs font-medium text-ink group-hover:text-accent transition-colors">{fileName}</span>
                </div>
                <Icon.Forward size={14} className="shrink-0 text-muted group-hover:text-accent transition-colors" />
              </a>
            );
          })}
        </div>
      </section>}
      {!contribution && !!update?.evidence?.length && <AgentDisclosure scope={disclosureScope} name="references" className="rounded-lg border border-line/60 p-3"><summary className="cursor-pointer text-xs font-semibold text-muted hover:text-ink transition-colors">Supporting references · {update.evidence.length}</summary><ul className="mt-3 list-disc space-y-1.5 pl-4 text-xs text-muted leading-relaxed">{update.evidence.map((reference, index) => <li key={index} className="break-words">{reference}</li>)}</ul></AgentDisclosure>}
      {job.progress?.turn_id && <section aria-label="Agent tool activity" className="space-y-3"><h3 className="text-xs font-semibold text-muted">Tool activity <span className="ml-1 font-normal text-faint">{job.progress.steps}</span></h3><WorkerTrace key={job.id} inline jobId={job.id} sessionId={job.session_id} turnId={job.progress.turn_id} parentSessionId={job.parent_session || parentSession} /></section>}
      <WorkerCommunication key={`${job.id}-history`} job={job} variant="history" />
      <AgentDisclosure scope={disclosureScope} name="assignment" className="rounded-xl border border-line/60 bg-surface/30 p-3"><summary className="cursor-pointer text-xs font-semibold text-muted hover:text-ink transition-colors">Assignment & checks</summary>
        <div className="mt-3 space-y-3 text-xs text-muted pt-2 border-t border-line/50">
          {job.objective && <section><h4 className="mb-1 text-xs font-semibold text-ink/80">Assignment</h4><p className="whitespace-pre-wrap break-words leading-relaxed text-ink/90">{job.objective}</p></section>}
          {job.spec?.contract && <>
            <section><h4 className="mb-1 text-xs font-semibold text-ink/80">Expected result</h4><p className="whitespace-pre-wrap break-words leading-relaxed text-ink/90">{job.spec.contract.expected_output}</p></section>
            <section><h4 className="mb-1 text-xs font-semibold text-ink/80">Acceptance criteria</h4><ul className="list-disc space-y-1 pl-4 text-ink/90">{job.spec.contract.acceptance_criteria.map((criterion, index) => <li key={index}>{criterion}</li>)}</ul></section>
            {!!job.spec.contract.constraints.length && <section><h4 className="mb-1 text-xs font-semibold text-ink/80">Constraints</h4><ul className="list-disc space-y-1 pl-4 text-ink/90">{job.spec.contract.constraints.map((constraint, index) => <li key={index}>{constraint}</li>)}</ul></section>}
          </>}
          {job.verification?.status === "verified" && <p className="text-emerald-400 font-medium">Recorded verification checks passed. This does not guarantee that every claim is correct.</p>}
        </div>
      </AgentDisclosure>
      <AgentDisclosure scope={disclosureScope} name="activity" className="rounded-xl border border-line/60 bg-surface/30 p-3"><summary className="cursor-pointer text-xs font-semibold text-muted hover:text-ink transition-colors">Activity & technical details</summary>
        <div className="mt-3 space-y-2.5 text-xs text-muted pt-2 border-t border-line/50">
          <p className="font-medium text-ink/80">{workerEnvironment(job)}</p>
          {job.progress && <p className="break-words">{job.progress.steps} tool {job.progress.steps === 1 ? "call" : "calls"} recorded</p>}
          {job.progress?.retry_at && !interrupted && !terminal && <p>Retry scheduled for {new Date(job.progress.retry_at * 1000).toLocaleTimeString()}</p>}
          {job.progress?.last_action_at && <p>Last tool step · <time dateTime={new Date(job.progress.last_action_at * 1000).toISOString()}>{new Date(job.progress.last_action_at * 1000).toLocaleTimeString()}</time></p>}
          {(job.error?.message || job.progress?.detail) && <p className="whitespace-pre-wrap break-words rounded-lg border border-rose-500/25 bg-rose-500/10 p-2 text-rose-400">{job.error?.message || job.progress?.detail}</p>}
          {!!job.activity?.length && <ol className="space-y-1.5 rounded-lg border border-line/50 bg-canvas/40 p-2.5">{job.activity.map(item => <li key={item.id} className="flex items-start justify-between gap-3"><span>{item.failed ? "A step needs attention" : item.label}</span><time className="shrink-0 text-muted/60" dateTime={new Date(item.created_at * 1000).toISOString()}>{new Date(item.created_at * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}</time></li>)}</ol>}
          {job.attachable && <button className="min-h-8 text-xs font-semibold text-accent hover:underline inline-flex items-center gap-1" onClick={() => { onClose(); void openSession(job.session_id); }}>Open execution history →</button>}
        </div>
      </AgentDisclosure>
      {error && <p role="alert" className="text-xs font-medium text-rose-400">{error}</p>}
    </div>
  </section>;
}
