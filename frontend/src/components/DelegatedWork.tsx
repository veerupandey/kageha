import { useState, useEffect } from "react";
import { agentOutline, agentElapsed, elapsedLabel } from "../lib/agentTree";
import { agentKey, useAgentValue } from "../lib/agentContinuity";
import { useAppStore } from "../store";
import { JobQuestion, useDurableJobs, type DurableJob } from "./DurableJobs";
import { AgentContributionCard } from "./AgentContribution";
import { WorkerHandoff } from "./WorkerHandoff";
import { Icon } from "../lib/icons";
import { cn } from "../lib/cn";
import { WORKER_TERMINAL, workerActivity, workerName, workerStatus } from "../lib/workerPresentation";

export function DelegatedWork({ onInspectWorker }: { onInspectWorker?: (job: DurableJob) => void }) {
  const sessionId = useAppStore(s => s.sessionId);
  const openSession = useAppStore(s => s.openSession);
  const runStatus = useAppStore(s => s.runStatus);
  const pendingApproval = useAppStore(s => s.pendingApproval);
  const { jobs, error } = useDurableJobs(sessionId);
  const [expanded, setExpanded] = useAgentValue(agentKey(sessionId || "", "dock-expanded"), false);
  const [showHistory, setShowHistory] = useAgentValue(agentKey(sessionId || "", "dock-history"), false);
  const [minimizedJobs, setMinimizedJobs] = useAgentValue<Record<string, boolean>>(agentKey(sessionId || "", "dock-minimized-jobs"), {});
  const [dockMinimized, setDockMinimized] = useAgentValue(agentKey(sessionId || "", "dock-minimized"), false);
  const [dismissed, setDismissed] = useAgentValue(agentKey(sessionId || "", "dock-dismissed"), false);

  const setJobMinimized = (id: string, min: boolean) => {
    setMinimizedJobs(prev => ({ ...(prev || {}), [id]: min }));
  };
  const setAllMinimized = (min: boolean) => {
    const updated: Record<string, boolean> = {};
    for (const j of visible) {
      updated[j.id] = min;
    }
    setMinimizedJobs(updated);
  };
  const workers = jobs.filter(job => job.kind === "agent" && !job.spec?.continuation &&
    (job.session_id !== sessionId || (job.parent_session && job.parent_session !== sessionId)));
  const active = workers.filter(job => !WORKER_TERMINAL.has(job.state));
  const past = workers.filter(job => WORKER_TERMINAL.has(job.state));
  const ownWorker = workers.find(job => job.session_id === sessionId);
  const coordinator = jobs.find(job => job.spec?.continuation && job.session_id === sessionId);
  const mainHandling = coordinator && !WORKER_TERMINAL.has(coordinator.state)
    ? ["running", "queued", "retry_wait", "waiting_dependencies"].includes(coordinator.state)
    : runStatus === "running";
  const workerQuestion = active.find(job => job.state === "waiting_input" && job.question?.question);
  const questionJob = !pendingApproval && !mainHandling
    ? coordinator?.state === "waiting_input" ? coordinator : workerQuestion : undefined;
  const featured = workerQuestion || active[0] || workers[0];
  // Keep failed or interrupted workers visible while their siblings continue.
  const handoffNeedsReview = (job: DurableJob) => ["preview", "review_required", "conflict", "uncertain", "rolled_back"].includes(job.handoff?.status || "");
  const unresolved = past.filter(job => ["failed", "interrupted"].includes(job.state) || handoffNeedsReview(job));
  const visible = showHistory ? [...active, ...past] : active.length ? [...active, ...unresolved] : past.slice(0, 3);
  const [filter, setFilter] = useState<"all" | "active" | "attention" | "completed">("all");
  const activeList = workers.filter(j => !WORKER_TERMINAL.has(j.state));
  const attentionList = workers.filter(j => ["failed", "interrupted", "waiting_input", "reconciling"].includes(j.state) || handoffNeedsReview(j));
  const completedList = workers.filter(j => j.state === "succeeded");
  const outline = agentOutline(workers);
  const filteredIds = new Set((filter === "all" ? visible : workers).filter(j => {
    if (filter === "active") return !WORKER_TERMINAL.has(j.state);
    if (filter === "attention") return ["failed", "interrupted", "waiting_input", "reconciling"].includes(j.state) || handoffNeedsReview(j);
    if (filter === "completed") return j.state === "succeeded";
    return true;
  }).map(job => job.id));
  const filteredVisible = outline.filter(node => filteredIds.has(node.job.id));
  const [now, setNow] = useState(() => Date.now() / 1000);
  useEffect(() => { if (!active.length) return; const timer = setInterval(() => setNow(Date.now() / 1000), 1000); return () => clearInterval(timer); }, [active.length]);
  const problem = workers.find(job => (!coordinator?.root_id || job.root_id === coordinator.root_id)
    && (["failed", "interrupted", "waiting_input", "reconciling"].includes(job.state) || handoffNeedsReview(job)));
  const needsReview = !active.length && !mainHandling && Boolean(problem);
  const isFinished = !active.length && !needsReview && !questionJob && past.length > 0;

  useEffect(() => {
    if (dismissed && (active.length > 0 || needsReview || Boolean(questionJob))) {
      setDismissed(false);
    }
  }, [active.length, needsReview, questionJob, dismissed, setDismissed]);

  useEffect(() => {
    if (dockMinimized && questionJob) {
      setDockMinimized(false);
    }
  }, [questionJob, dockMinimized, setDockMinimized]);

  let coordination = "";
  if (coordinator?.state === "running") coordination = problem ? `Kageha is reviewing ${workerName(problem)}’s update.` : "Kageha is bringing the findings together.";
  else if (coordinator?.state === "queued") coordination = "Kageha will continue with the latest findings.";
  else if (coordinator?.state === "waiting_dependencies") coordination = problem ? "Kageha will review the question before asking you." : "";
  else if (coordinator?.state === "failed" && runStatus !== "running") coordination = "Kageha stopped. Send a follow-up to continue.";
  else if (questionJob) coordination = "Your input is needed to continue this task.";

  if (!workers.length) return error ? (
    <section aria-label="Specialist updates" className="shrink-0 border-t border-line/70 bg-surface/90 px-4 py-2">
      <p role="alert" className="mx-auto max-w-3xl text-sm text-warn">{error}</p>
    </section>
  ) : null;
  if (isFinished && dismissed) return null;

  const isCompact = (isFinished || dockMinimized) && !expanded;

  return (
    <section
      aria-label="Delegated work"
      className={cn(
        "ka-worker-dock shrink-0 border-t border-line/70 bg-surface/90 transition-all duration-200",
        isCompact ? "max-h-14 px-3 py-1.5 sm:px-4" : "max-h-[48dvh] overflow-y-auto px-3 py-2 sm:px-4"
      )}
    >
      <div className="mx-auto w-full max-w-3xl">
        {ownWorker?.parent_session && (
          <button
            className="mb-2 inline-flex min-h-10 items-center gap-1 text-sm text-accent"
            onClick={() => void openSession(ownWorker.parent_session!)}
          >
            <Icon.Back size={14} /> Back to main conversation
          </button>
        )}

        <div className="flex items-center justify-between gap-2">
          <button
            aria-label={expanded ? "Hide agents" : "Show agents"}
            aria-expanded={expanded}
            aria-controls="delegated-agents"
            className={cn(
              "flex min-w-0 flex-1 items-center rounded-lg text-left hover:bg-accent/5 transition-colors cursor-pointer",
              isCompact ? "min-h-7 gap-2.5 px-1 py-0.5" : "min-h-12 gap-3 px-1 py-1"
            )}
            onClick={() => {
              if (dockMinimized) {
                setDockMinimized(false);
              } else {
                setExpanded(value => !value);
              }
            }}
          >
            <span aria-hidden="true" className="flex shrink-0 -space-x-1.5 sm:-space-x-2">
              {(active.length ? active : past).slice(0, 3).map(job => {
                const isRunning = job.state === "running";
                const isWaiting = job.state === "waiting_input" || job.state === "reconciling";
                const isFailed = job.state === "failed";
                const isSucceeded = job.state === "succeeded";
                return (
                  <span key={job.id} className="relative inline-flex">
                    <span
                      className={cn(
                        "flex items-center justify-center rounded-full border-2 border-surface bg-accent/15 font-semibold text-accent",
                        isCompact ? "h-6 w-6 text-[11px]" : "h-8 w-8 text-xs"
                      )}
                    >
                      {workerName(job).charAt(0).toUpperCase()}
                    </span>
                    <span
                      className={cn(
                        "absolute -bottom-0.5 -right-0.5 rounded-full ring-2 ring-surface",
                        isCompact ? "h-2 w-2" : "h-2.5 w-2.5",
                        isRunning && "bg-emerald-400",
                        isWaiting && "bg-amber-400",
                        isFailed && "bg-rose-500",
                        isSucceeded && "bg-emerald-500",
                        !isRunning && !isWaiting && !isFailed && !isSucceeded && "bg-muted"
                      )}
                      title={`${workerName(job)}: ${job.state}`}
                    />
                  </span>
                );
              })}
            </span>

            <span className="min-w-0 flex-1">
              <span
                className={cn(
                  "block font-medium leading-tight",
                  isCompact ? "text-xs" : "text-sm",
                  needsReview ? "text-warn" : "text-ink"
                )}
              >
                {needsReview
                  ? "Review specialist update"
                  : active.length
                    ? `${active.length} ${active.length === 1 ? "specialist" : "specialists"} helping`
                    : "Specialist contributions"}
              </span>

              {active.length > 1 && !workerQuestion ? (
                <span
                  className={cn(
                    "block truncate text-muted leading-tight",
                    isCompact ? "text-[0.7rem] text-muted/80 max-w-[280px] sm:max-w-md" : "mt-0.5 text-xs"
                  )}
                >
                  {active.slice(0, 3).map(job => `${workerName(job)} · ${workerStatus(job, mainHandling)}`).join("  •  ")}
                  {active.length > 3 ? `  •  +${active.length - 3} more` : ""}
                </span>
              ) : featured && (active.length > 0 || needsReview) && (
                <span
                  className={cn(
                    "block truncate text-muted leading-tight",
                    isCompact ? "text-[0.7rem] text-muted/80 max-w-[280px] sm:max-w-md" : "mt-0.5 text-xs"
                  )}
                >
                  {workerName(needsReview && problem ? problem : featured)} · {workerActivity(needsReview && problem ? problem : featured)}
                </span>
              )}

              {!active.length && !needsReview && (
                <span
                  className={cn(
                    "block text-muted truncate leading-tight",
                    isCompact ? "text-[0.7rem] text-muted/80" : "mt-0.5 text-xs"
                  )}
                >
                  {past.length} {past.length === 1 ? "contribution" : "contributions"} · View results and activity
                </span>
              )}
            </span>

            <Icon.Forward
              size={isCompact ? 13 : 15}
              className={cn("shrink-0 text-muted transition-transform motion-reduce:transition-none", expanded && "rotate-90")}
            />
          </button>

          <div className="flex items-center gap-1 shrink-0">
            <button
              type="button"
              className={cn(
                "ka-icon-btn shrink-0 rounded-lg text-muted hover:text-ink hover:bg-line/40 transition-colors flex items-center justify-center cursor-pointer",
                isCompact ? "h-6 w-6" : "h-7 w-7"
              )}
              onClick={(e) => {
                e.stopPropagation();
                if (dockMinimized) {
                  setDockMinimized(false);
                } else {
                  setDockMinimized(true);
                  setExpanded(false);
                }
              }}
              aria-label={dockMinimized ? "Expand agent dock" : "Minimize agent dock"}
              title={dockMinimized ? "Expand agent dock" : "Minimize agent dock"}
            >
              {dockMinimized ? <Icon.Maximize size={isCompact ? 12 : 13} /> : <Icon.Minimize size={isCompact ? 12 : 13} />}
            </button>

            {isFinished && (
              <button
                type="button"
                className={cn(
                  "ka-icon-btn shrink-0 rounded-lg text-faint hover:text-ink hover:bg-line/40 transition-colors flex items-center justify-center cursor-pointer",
                  isCompact ? "h-6 w-6" : "h-7 w-7"
                )}
                onClick={(e) => {
                  e.stopPropagation();
                  setDismissed(true);
                }}
                aria-label="Dismiss specialist contributions"
                title="Dismiss (contributions remain in Activity panel)"
              >
                <Icon.Close size={isCompact ? 12 : 13} />
              </button>
            )}
          </div>
        </div>

        {!expanded && !dockMinimized && active.length > 0 && (
          <div className="mt-1 border-t border-line/50 pt-1">
            <ul aria-label="Agent progress" className="space-y-0.5">
              {[...active, ...unresolved].map(job => (
                <li key={job.id}>
                  <button type="button" onClick={() => onInspectWorker ? onInspectWorker(job) : setExpanded(true)}
                    aria-label={`Inspect ${workerName(job)}: ${workerStatus(job)}`}
                    className="flex w-full min-w-0 items-start gap-2 rounded-md px-1 py-1 text-left text-xs hover:bg-accent/5">
                    <span className="min-w-0 flex-1">
                      <span className="block truncate font-medium text-ink">{workerName(job)} <span className="font-normal text-muted">· {workerStatus(job)}</span></span>
                      <span className="block truncate text-muted" title={workerActivity(job)}>{workerActivity(job)}</span>
                    </span>
                    <span className="shrink-0 tabular-nums text-faint">{elapsedLabel(agentElapsed(job, now))}</span>
                  </button>
                </li>
              ))}
            </ul>
            {active.length > 1 && (
              <div className="flex justify-end pt-1">
                <button
                  type="button"
                  onClick={() => {
                    setDockMinimized(true);
                    setExpanded(false);
                  }}
                  className="inline-flex items-center gap-1 text-[11px] text-muted hover:text-ink px-2 py-0.5 rounded hover:bg-accent/5 transition-colors cursor-pointer"
                  title="Minimize agent dock to compact bar"
                >
                  <Icon.Minimize size={10} />
                  <span>Minimize</span>
                </button>
              </div>
            )}
          </div>
        )}

        {!dockMinimized && coordination && <p className="mt-1 px-1 text-xs text-muted">{coordination}</p>}
        {questionJob && (
          <div className="mt-2 max-h-[40vh] overflow-y-auto rounded-lg border border-warn/30 bg-warn-soft/30 p-2.5 sm:p-3" aria-label="Task needs your input">
            <p className="text-sm font-medium text-ink">
              {questionJob.spec?.continuation ? "Kageha needs your input" : `${workerName(questionJob)} needs your input`}
            </p>
            <JobQuestion key={`${questionJob.id}:${questionJob.error.approval_id || questionJob.question?.question}`} job={questionJob} />
          </div>
        )}
        {error && <p role="alert" className="mt-2 text-sm text-warn">{error}</p>}

        {expanded && (
          <div id="delegated-agents" className="ka-agent-list mt-2.5 max-h-[36vh] space-y-2 overflow-y-auto overscroll-contain pr-1">
            {workers.length > 1 && (
              <div className="flex flex-col gap-1.5 px-1 pb-1.5 border-b border-line/40">
                <div className="flex items-center justify-between text-xs text-muted">
                  <span className="font-medium text-ink/80">{active.length} active · {past.filter(job => job.state === "succeeded").length} finished</span>
                  <div className="flex items-center gap-2" hidden={!visible.some(job => job.result.contribution || job.progress?.contribution)}>
                    <button
                      type="button"
                      className="text-xs text-accent hover:underline flex items-center gap-1 cursor-pointer"
                      onClick={() => setAllMinimized(true)}
                      title="Minimize all subagents to compact rows"
                    >
                      <Icon.Minimize size={11} />
                      <span>Minimize all</span>
                    </button>
                    <span className="text-line-strong">·</span>
                    <button
                      type="button"
                      className="text-xs text-accent hover:underline flex items-center gap-1 cursor-pointer"
                      onClick={() => setAllMinimized(false)}
                      title="Expand all subagent cards"
                    >
                      <Icon.Maximize size={11} />
                      <span>Expand all</span>
                    </button>
                  </div>
                </div>

                {workers.length > 2 && (
                  <div className="flex flex-wrap items-center gap-1.5 pt-1">
                    <div className="inline-flex p-0.5 rounded-full bg-line/25 border border-line/40 gap-0.5">
                      <button
                        type="button"
                        aria-pressed={filter === "all"}
                          onClick={() => setFilter("all")}
                        className={cn(
                          "rounded-full px-2.5 py-0.5 text-[11px] font-medium transition-colors cursor-pointer select-none",
                          filter === "all"
                            ? "bg-surface text-ink border border-line/50 font-semibold"
                            : "text-muted hover:text-ink"
                        )}
                      >
                        All ({visible.length})
                      </button>
                      {activeList.length > 0 && (
                        <button
                          type="button"
                          aria-pressed={filter === "active"}
                          onClick={() => setFilter("active")}
                          className={cn(
                            "inline-flex items-center gap-1 rounded-full px-2.5 py-0.5 text-[11px] font-medium transition-colors cursor-pointer select-none",
                            filter === "active"
                              ? "bg-surface text-emerald-400 border border-line/50 font-semibold"
                              : "text-muted hover:text-ink"
                          )}
                        >
                          <span className="h-1.5 w-1.5 rounded-full bg-emerald-400" />
                          <span>Active ({activeList.length})</span>
                        </button>
                      )}
                      {attentionList.length > 0 && (
                        <button
                          type="button"
                          aria-pressed={filter === "attention"}
                          onClick={() => setFilter("attention")}
                          className={cn(
                            "inline-flex items-center gap-1 rounded-full px-2.5 py-0.5 text-[11px] font-medium transition-colors cursor-pointer select-none",
                            filter === "attention"
                              ? "bg-surface text-amber-400 border border-line/50 font-semibold"
                              : "text-muted hover:text-ink"
                          )}
                        >
                          <span className="h-1.5 w-1.5 rounded-full bg-amber-400" />
                          <span>Attention ({attentionList.length})</span>
                        </button>
                      )}
                      {completedList.length > 0 && (
                        <button
                          type="button"
                          aria-pressed={filter === "completed"}
                          onClick={() => setFilter("completed")}
                          className={cn(
                            "inline-flex items-center gap-1 rounded-full px-2.5 py-0.5 text-[11px] font-medium transition-colors cursor-pointer select-none",
                            filter === "completed"
                              ? "bg-surface text-emerald-400 border border-line/50 font-semibold"
                              : "text-muted hover:text-ink"
                          )}
                        >
                          <span className="h-1.5 w-1.5 rounded-full bg-emerald-400" />
                          <span>Completed ({completedList.length})</span>
                        </button>
                      )}
                    </div>
                  </div>
                )}
              </div>
            )}
            {filteredVisible.length === 0 && <p className="px-3 py-4 text-xs text-muted">No agents in this view.</p>}
            {filteredVisible.map(({ job, depth, parentId }) => <div key={job.id} className={depth ? "ka-agent-branch" : undefined} style={{ marginInlineStart: `${Math.min(depth, 6) * 12}px` }}>
              {depth > 0 && <p className="mb-1 truncate px-1 text-[10px] text-faint">Delegated by {workerName(workers.find(item => item.id === parentId) || job)}</p>}
              {WORKER_TERMINAL.has(job.state) ? (
              <AgentContributionCard
                key={job.id}
                job={job}
                onInspect={onInspectWorker}
                minimized={Boolean(minimizedJobs?.[job.id])}
                onMinimize={(min) => setJobMinimized(job.id, min)}
              />
            ) : (
              <button
                key={job.id}
                onClick={() => onInspectWorker?.(job)}
                disabled={!onInspectWorker}
                aria-label={`View ${workerName(job)} details`}
                className="flex w-full items-center gap-3 ka-agent-row rounded-lg px-3 py-3 text-left hover:bg-surface hover:border-accent/40 disabled:opacity-60 border border-line/70 bg-accent/5 transition-colors cursor-pointer group"
              >
                <div className="relative shrink-0">
                  <span className="flex h-8 w-8 items-center justify-center rounded-xl bg-accent/5 border border-accent/20 text-xs font-bold text-accent" aria-hidden="true">
                    {workerName(job).charAt(0).toUpperCase()}
                  </span>
                  <span
                    className={cn(
                      "absolute -bottom-0.5 -right-0.5 h-2.5 w-2.5 rounded-full ring-2 ring-surface",
                      job.state === "running" && "bg-emerald-400",
                      job.state === "waiting_input" && "bg-amber-400",
                      job.state === "failed" && "bg-rose-500",
                      job.state === "succeeded" && "bg-emerald-500",
                      !["running", "waiting_input", "failed", "succeeded"].includes(job.state) && "bg-muted",
                    )}
                  />
                </div>
                <span className="min-w-0 flex-1">
                  <span className="flex flex-wrap items-center justify-between gap-2">
                    <strong className="max-w-full truncate text-xs font-semibold text-ink group-hover:text-accent transition-colors">{workerName(job)}</strong>
                    <span data-state={job.state} className={cn(
                      "inline-flex items-center gap-1 ka-agent-status rounded-md px-2 py-0.5 text-[11px] font-medium border",
                      ["failed", "interrupted", "waiting_input", "reconciling"].includes(job.state)
                        ? "bg-amber-500/10 text-amber-400 border-amber-500/25"
                        : "bg-surface/80 text-muted border-line/60"
                    )}>
                      {workerStatus(job, mainHandling)}
                    </span>
                  </span>
                  <span className="mt-0.5 block truncate text-xs text-muted leading-tight">{workerActivity(job)}</span>
                  <span className="mt-1 flex items-center gap-2 text-[10px] tabular-nums text-faint"><span>{elapsedLabel(agentElapsed(job, now))}</span>{job.progress && <span>{job.progress.steps} tool {job.progress.steps === 1 ? "call" : "calls"}</span>}{job.error.approval_id && <span className="text-warn">Approval needed</span>}</span>
                  {job.progress?.agent_update?.next_step && (
                    <span className="mt-1 inline-flex items-center gap-1 rounded bg-accent/10 px-1.5 py-0.5 text-[11px] text-accent font-medium max-w-full truncate">
                      <span>Next:</span>
                      <span className="truncate">{job.progress.agent_update.next_step}</span>
                    </span>
                  )}
                </span>
                <Icon.Forward size={13} className="shrink-0 text-muted group-hover:text-accent transition-colors" />
              </button>
            )}
              {job.handoff && <div className="mt-1"><WorkerHandoff handoff={job.handoff} /></div>}
            </div>)}
            {past.length > (active.length ? 0 : 3) && (
              <button
                className="min-h-9 px-3 text-xs text-muted hover:text-ink underline"
                onClick={() => setShowHistory(value => !value)}
              >
                {showHistory ? "Show fewer contributions" : `View all contributions (${past.length})`}
              </button>
            )}
          </div>
        )}
      </div>
    </section>
  );
}
