import { GmailAccessAction, gmailToolLabel } from "./GmailAccessAction";
/**
 * AgentCanvas — Monitoring panel.
 *
 * Four tabs:
 * - Timeline: Live tool calls with expand/collapse, I/O, duration, status
 * - Plan: Current task plan and todo state
 * - Subagents: Delegated work (when present)
 * - Stats: Cost, tokens, steps, elapsed time
 */

import { lazy, memo, Suspense, useEffect, useMemo, useRef, useState } from "react";
import { DurableJobs, useDurableJobs } from "./DurableJobs";
import { LadderChip, WorkspaceFiles, WorkspacePlanCard } from "./WorkspaceActivity";
import { cn } from "../lib/cn";
import { Icon } from "../lib/icons";
import { useAppStore } from "../store";
import type { ActivityStep, AgentOperatingState, ToolCard } from "../api/types";
import { initMermaidBlocks } from "../lib/codeEnhancer";
import { renderMarkdownHtml } from "../lib/markdownHtml";
import { activityLineText, collectTraceSteps, friendlyPlanStepText } from "../lib/activityUi";
import { isSubagentSpawn, toolPillLabel } from "../lib/transcript";
import { collectMessageToolCards } from "../lib/messageArtifacts";
import { ToolDetails } from "./Transcript/ToolDetails";
import { ToolCallPill } from "./Transcript/ToolCallPill";
import { useSessionTruth } from "../lib/sessionTruth";
import { sessionStateLabel } from "../lib/sessionStateLabel";
import { completionLabel, isDeliveryUnconfirmed } from "../lib/runStatus";
import { planOutcomeProgress } from "../lib/planProgress";

const PlanGraph = lazy(() => import("./PlanGraph").then((m) => ({ default: m.PlanGraph })));


// ── Types ──────────────────────────────────────────────────────────────

type CanvasTab = "timeline" | "plan" | "subagents" | "stats";
type CanvasTabIcon = "Activity" | "Jobs" | "Network" | "BarChart";

interface SessionStats {
  steps: number;
  toolCalls: number;
  toolSuccesses: number;
  toolFailures: number;
  totalTokensIn: number;
  totalTokensOut: number;
  cachedTokens: number;
  totalCostUsd: number;
  elapsedMs: number;
  models: string[];
}

// ── Helpers ────────────────────────────────────────────────────────────

function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  if (ms < 60000) return `${(ms / 1000).toFixed(1)}s`;
  return `${(ms / 60000).toFixed(1)}m`;
}

function formatCost(usd: number): string {
  if (usd < 0.001) return "<$0.001";
  if (usd < 0.01) return `$${usd.toFixed(4)}`;
  return `$${usd.toFixed(3)}`;
}

const completedTool = (status?: string) => ["done", "success", "completed", "ok"].includes(status ?? "");
const failedTool = (status?: string) => ["error", "failed", "denied"].includes(status ?? "");
const runningTool = (status?: string) => ["running", "pending", "requested"].includes(status ?? "");

function toolStatusIcon(status?: string): string {
  if (completedTool(status)) return "✓";
  if (failedTool(status)) return "✗";
  if (runningTool(status)) return "⟳";
  return "·";
}

function toolStatusColor(status?: string): string {
  if (completedTool(status)) return "text-green-500";
  if (failedTool(status)) return "text-red-500";
  if (runningTool(status)) return "text-accent";
  return "text-muted";
}

const OperatingState = memo(function OperatingState({ state, fallbackMode }: { state: AgentOperatingState; fallbackMode: string }) {
  const mode = state.mode || fallbackMode;
  const routeLabel = state.route === "mcp" ? "MCP" : state.route === "skill" ? "Skill" : "Native";
  const routeClass = state.route === "mcp"
    ? "border-violet-400/30 bg-violet-400/10 text-violet-200"
    : state.route === "skill"
      ? "border-sky-400/30 bg-sky-400/10 text-sky-200"
      : "border-accent/30 bg-accent/10 text-accent";

  return (
    <section className="border-b border-line bg-canvas/45 px-3 py-2.5" aria-label="Agent operating state">
      <div className="flex items-center gap-2">
        <span className="text-[0.62rem] font-semibold uppercase tracking-[0.14em] text-muted">Operating state</span>
        <span className="ml-auto rounded-full border border-accent/30 bg-accent/10 px-1.5 py-0.5 text-[0.6rem] font-medium capitalize text-accent">{mode}</span>
        {state.effort && <span className="rounded-full border border-line px-1.5 py-0.5 text-[0.6rem] capitalize text-muted">{state.effort}</span>}
      </div>
      <div className="mt-2 flex flex-wrap gap-1.5">
        {state.currentTool && (
          <span className={cn("max-w-full truncate rounded-md border px-1.5 py-0.5 text-[0.62rem]", routeClass)} title={state.currentTool}>
            {routeLabel} · {state.currentTool}
          </span>
        )}
        {state.maxTokens && <span className="rounded-md border border-line px-1.5 py-0.5 text-[0.62rem] text-muted">{state.maxTokens.toLocaleString()} token cap</span>}
        {state.loopMode && <span className="rounded-md border border-line px-1.5 py-0.5 text-[0.62rem] text-muted">{state.loopMode}</span>}
        {state.mcpStatus && <span className="rounded-md border border-line px-1.5 py-0.5 text-[0.62rem] text-muted">MCP · {state.mcpStatus}</span>}
      </div>
      {(state.nativePacks.length > 0 || state.activeSkills.length > 0) && (
        <div className="mt-2 flex min-w-0 items-center gap-1.5 overflow-hidden text-[0.6rem] text-faint">
          <span className="shrink-0 uppercase tracking-wide text-muted">Loaded</span>
          <span className="truncate">{[...state.nativePacks, ...state.activeSkills.map((skill) => `skill:${skill}`)].join(" · ")}</span>
        </div>
      )}
    </section>
  );
});

function TransportDiagnostics({
  transport,
}: {
  transport: { mode: string; lastSequence: number; lastFrameAt: number; reconnectCount: number };
}) {
  const ageMs = transport.lastFrameAt ? Date.now() - transport.lastFrameAt : 0;
  const age =
    ageMs <= 0
      ? "—"
      : ageMs < 1000
        ? `${ageMs}ms`
        : `${Math.round(ageMs / 1000)}s`;
  return (
    <section className="border-b border-line bg-canvas/30 px-3 py-2" aria-label="Transport diagnostics">
      <p className="text-[0.62rem] font-semibold uppercase tracking-[0.14em] text-muted">Transport</p>
      <div className="mt-1.5 flex flex-wrap gap-1.5 text-[0.62rem] text-faint">
        <span className="rounded-md border border-line px-1.5 py-0.5 font-mono">{transport.mode}</span>
        <span className="rounded-md border border-line px-1.5 py-0.5">seq {transport.lastSequence || 0}</span>
        <span className="rounded-md border border-line px-1.5 py-0.5">last frame {age}</span>
        <span className="rounded-md border border-line px-1.5 py-0.5">reconnects {transport.reconnectCount || 0}</span>
      </div>
    </section>
  );
}


// ── Trace feed ─────────────────────────────────────────────────────────

const TraceFeed = memo(function TraceFeed({
  steps,
  className,
}: {
  steps: ActivityStep[];
  className?: string;
}) {
  if (!steps.length) return null;
  return (
    <div className={cn("border-b border-line px-3 py-2", className)}>
      <p className="mb-1.5 text-[0.65rem] uppercase tracking-wide text-muted">
        Trace ({steps.length})
      </p>
      <div className="space-y-1">
        {steps.map((step, i) => (
          <details
            key={`${i}-${step.kind || ""}-${step.label}`}
            className="rounded-lg border border-line/60 bg-canvas px-2 py-1"
          >
            <summary className="cursor-pointer list-none text-xs text-ink [&::-webkit-details-marker]:hidden">
              <span className="mr-1 text-accent">·</span>
              {activityLineText(step)}
            </summary>
            {step.detail?.length ? (
              <ul className="mt-1 space-y-0.5 border-t border-line/40 pt-1 font-mono text-[0.68rem] leading-relaxed text-faint">
                {step.detail.map((line, j) => (
                  <li key={j} className="whitespace-pre-wrap">
                    {line}
                  </li>
                ))}
              </ul>
            ) : null}
          </details>
        ))}
      </div>
    </div>
  );
});

// ── Timeline Tab ───────────────────────────────────────────────────────

const ToolCallRow = memo(function ToolCallRow({ card, recovered }: { card: ToolCard; recovered?: boolean }) {
  const [expanded, setExpanded] = useState(false);
  const icon = toolStatusIcon(card.status);
  const wasRecovered = Boolean(card.errorPreview && recovered);
  const color = wasRecovered ? "text-amber-400" : toolStatusColor(card.status);

  return (
    <div className="border-b border-line/50 last:border-b-0">
      <button
        type="button"
        className="flex w-full items-center gap-2 px-3 py-2 text-left hover:bg-line/30 transition-colors"
        aria-expanded={expanded}
        onClick={() => setExpanded(!expanded)}
      >
        <span className={cn("text-sm font-mono", color)}>{icon}</span>
        <span className="min-w-0 flex-1 truncate text-xs font-medium text-ink">
          {gmailToolLabel(card.connectorOutcome) || toolPillLabel(card)}
        </span>
        {card.durationMs != null && (
          <span className="shrink-0 text-[0.65rem] tabular-nums text-muted">
            {formatDuration(card.durationMs)}
          </span>
        )}
        <span className="shrink-0 text-[0.6rem] text-faint">
          {expanded ? "▾" : "▸"}
        </span>
      </button>
      {card.connectorOutcome?.recovery && card.connectorOutcome.showRecovery !== false && <GmailAccessAction outcome={card.connectorOutcome} />}
      {expanded && (
        <div className="px-3 pb-2 space-y-1">
          <ToolDetails card={card} />
          {card.artifactRefs && card.artifactRefs.length > 0 && (
            <div className="flex flex-wrap gap-1 pt-1">
              {card.artifactRefs.map((ref) => (
                <span key={ref} className="rounded bg-accent-soft px-1.5 py-0.5 text-[0.6rem] text-accent">
                  📎 {ref.split("/").pop()}
                </span>
              ))}
            </div>
          )}
          {(card.sideEffect || card.policyGrant || card.attemptId) && (
            <div className="flex flex-wrap gap-x-3 gap-y-1 pt-1 text-[0.6rem] text-faint">
              {card.sideEffect && <span>Side effect: {card.sideEffect}</span>}
              {card.policyGrant && <span>Policy: {card.policyGrant}</span>}
              {card.attemptId && <span>Attempt: {card.attemptId}</span>}
            </div>
          )}
        </div>
      )}
    </div>
  );
});

function TimelineTab() {
  const messages = useAppStore((s) => s.messages);
  const runStatus = useAppStore((s) => s.runStatus);
  const agentMode = useAppStore((s) => s.agentMode);
  const sessionId = useAppStore((s) => s.sessionId);
  const operatingState = useAppStore((s) => sessionId ? s.runs[sessionId]?.operatingState : undefined);
  const transport = useAppStore((s) => sessionId ? s.runs[sessionId]?.transport : undefined);
  const recovered = runStatus !== "running" && runStatus !== "error";

  // Collect tool cards from live cards and journal-replayed blocks.
  const allToolCards = useMemo(() => {
    const cards: ToolCard[] = [];
    const seen = new Set<string>();
    for (const m of messages) {
      if (m.role !== "assistant") continue;
      for (const card of collectMessageToolCards(m)) {
        if (card.id && seen.has(card.id)) continue;
        if (card.id) seen.add(card.id);
        cards.push(card);
      }
    }
    return cards;
  }, [messages]);

  // Full run trace: prompt cache, todos, verification, plan milestones.
  const traceSteps = useMemo(() => collectTraceSteps(messages), [messages]);

  return (
    <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain">
      <OperatingState
        state={operatingState || { activeSkills: [], nativePacks: [] }}
        fallbackMode={agentMode}
      />
      {transport ? <TransportDiagnostics transport={transport} /> : null}
      {!allToolCards.length && !traceSteps.length && (
        <div className="flex min-h-[15rem] flex-col items-center justify-center px-6 text-center">
          <span className="flex h-10 w-10 items-center justify-center rounded-xl border border-line bg-surface text-accent shadow-sm">
            <Icon.Activity size={18} />
          </span>
          <p className="mt-3 text-sm font-medium text-ink">Ready to observe</p>
          <p className="mt-1 max-w-[16rem] text-xs leading-relaxed text-muted">
            Tool calls, milestones, and execution details will appear here when the agent starts working.
          </p>
        </div>
      )}
      <TraceFeed steps={traceSteps} />
      {/* Tool calls */}
      <div>
        <p className="px-3 py-2 text-[0.65rem] uppercase tracking-wide text-muted">
          Tool Calls ({allToolCards.length})
        </p>
        {allToolCards.map((card) => (
          <ToolCallRow key={card.id} card={card} recovered={recovered} />
        ))}
      </div>
    </div>
  );
}

function SubagentsTab() {
  const messages = useAppStore((s) => s.messages);
  const cards = useMemo(() => {
    const out: ToolCard[] = [];
    for (const message of messages) {
      if (message.role !== "assistant" || !message.toolCards) continue;
      out.push(...message.toolCards.filter((card) =>
        isSubagentSpawn(card.name),
      ));
    }
    return out;
  }, [messages]);

  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <div className="border-b border-line/70 bg-gradient-to-b from-accent-soft/40 to-transparent px-4 py-3.5 space-y-1">
        <p className="text-xs font-semibold text-ink flex items-center gap-1.5">
          <Icon.Jobs size={13} className="text-accent" />
          <span>Specialist Delegations</span>
        </p>
        <p className="text-[11px] leading-relaxed text-muted">
          Each specialist works in its own context. Their updates and findings stay synchronized with this task.
        </p>
      </div>
      {cards.length ? (
        <div className="p-3 space-y-2">
          <p className="px-1 text-[10px] font-semibold uppercase tracking-wider text-muted">
            Delegations ({cards.length})
          </p>
          {cards.map((card) => (
            <div key={card.id} className="rounded-xl border border-line/60 bg-surface/70 hover:bg-surface hover:border-accent/40 p-2.5 transition-all shadow-2xs">
              <ToolCallPill card={card} />
            </div>
          ))}
        </div>
      ) : (
        <div className="flex flex-1 flex-col items-center justify-center px-6 py-14 text-center">
          <span className="flex h-12 w-12 items-center justify-center rounded-2xl bg-accent/10 border border-accent/20 text-accent mb-3 shadow-xs">
            <Icon.Jobs size={20} />
          </span>
          <p className="text-sm font-semibold text-ink">No subagents yet</p>
          <p className="mt-1 text-xs leading-relaxed text-muted max-w-[220px]">
            The coordinator will display delegated research, implementation, and review specialists here.
          </p>
        </div>
      )}
    </div>
  );
}


// ── Plan Tab ───────────────────────────────────────────────────────────

export function PlanTab() {
  const storedBoard = useAppStore((s) => s.todoBoard);
  const messages = useAppStore((s) => s.messages);
  const runStatus = useAppStore((s) => s.runStatus);
  const sessionId = useAppStore((s) => s.sessionId);
  const truth = useSessionTruth(sessionId);
  const staleBoard = Boolean(sessionId && storedBoard?.turn_id && truth.status === "current" &&
    storedBoard.turn_id !== truth.data?.turn?.id);
  const todoBoard = staleBoard ? null : storedBoard;
  const sessionTitle = useAppStore((s) => s.sessionTitle);
  const statusLabel = useAppStore((s) => s.statusLabel);
  const pendingApproval = useAppStore((s) => s.pendingApproval);
  const [planMd, setPlanMd] = useState("");
  const [planLoading, setPlanLoading] = useState(false);
  const [planExpanded, setPlanExpanded] = useState(false);
  const planMarkdownRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (planMarkdownRef.current) initMermaidBlocks(planMarkdownRef.current);
  }, [planMd]);
  const [stepsExpanded, setStepsExpanded] = useState(true);
  const [planView, setPlanView] = useState<"graph" | "list">("graph");

  // Fetch plan.md content from the session
  useEffect(() => {
    if (!sessionId) { setPlanMd(""); return; }
    let cancelled = false;
    setPlanLoading(true);
    void fetch(`/api/sessions/${encodeURIComponent(sessionId)}/files/plan.md`)
      .then((r) => {
        if (!r.ok) throw new Error("not found");
        return r.text();
      })
      .then((text) => { if (!cancelled) setPlanMd(text); })
      .catch(() => { if (!cancelled) setPlanMd(""); })
      .finally(() => { if (!cancelled) setPlanLoading(false); });
    return () => { cancelled = true; };
  }, [sessionId, todoBoard?.version, todoBoard?.total, runStatus]);

  // Extract plan summary from activity steps
  const planInfo = useMemo(() => {
    for (const m of [...messages].reverse()) {
      if (m.role !== "assistant") continue;
      for (const step of [...(m.steps || [])].reverse()) {
        if (step.label?.toLowerCase().includes("plan ready") && step.detail?.length) {
          return { label: step.label, details: step.detail };
        }
      }
    }
    return null;
  }, [messages]);

  const hasBoard = todoBoard && todoBoard.items.length > 0;
  const isEmpty = !storedBoard && !planInfo && !planMd && !sessionTitle && !messages.length;

  if (isEmpty) {
    return (
      <div className="flex flex-1 flex-col items-center justify-center px-4 py-8 text-center">
        <div className="flex h-12 w-12 items-center justify-center rounded-2xl bg-line/30 mb-3">
          <span className="text-xl text-faint">☑</span>
        </div>
        <p className="text-sm font-medium text-muted">Your task at a glance</p>
        <p className="mt-1.5 text-xs text-faint leading-relaxed max-w-[220px]">
          Your goal, progress and any decisions needed will appear here.
        </p>
      </div>
    );
  }

  const objective = todoBoard?.objective?.trim() || [...messages].reverse().find(m => m.role === "user")?.text || sessionTitle?.trim() || "Current task";
  const normalizePlanText = (text: string) => text.replace(/\s+/g, " ").trim().toLowerCase();
  const objectiveKey = normalizePlanText(objective);
  const items = todoBoard?.items || [];
  const outcomes = (todoBoard?.outcomes || []).filter((outcome, index, all) => {
    const key = normalizePlanText(outcome.text);
    // Keep real verification and evidence, but omit repeated placeholder goals.
    if (!outcome.done && !outcome.evidence &&
        (key === objectiveKey || items.some((item) => normalizePlanText(item.text) === key))) return false;
    return all.findIndex((other) => normalizePlanText(other.text) === key &&
      other.done === outcome.done && other.evidence === outcome.evidence) === index;
  });
  const hasExecutionSteps = hasBoard && (items.length > 1 ||
    items.some((item) => normalizePlanText(item.text) !== objectiveKey || item.evidence));
  const showGraph = Boolean(hasExecutionSteps) && planView === "graph";
  const outcomeProgress = planOutcomeProgress(todoBoard ?? { done: 0, total: 0, items: [] });
  const finishedLabel = completionLabel(runStatus, { stepsDone: todoBoard?.done ?? 0,
    stepsTotal: todoBoard?.total ?? 0, pendingOutcomes: Math.max(outcomeProgress.pending,
      sessionId && truth.status === "current" ? truth.data?.turn?.outcomes_pending ?? 0 : 0) });
  const currentBoard = !sessionId || (truth.status === "current" && todoBoard?.turn_id === truth.data?.turn?.id);
  const progress = hasBoard && todoBoard!.total > 0 ? Math.max(0, Math.min(100, (todoBoard!.done / todoBoard!.total) * 100)) : 0;
  const allDone = (!hasBoard || currentBoard) && finishedLabel === "Done" && (sessionId
    ? truth.status === "current" && truth.data?.turn?.status === "success"
    : runStatus === "success");
  const latestAssistant = messages.findLast((message) => message.role === "assistant");
  // The ladder step and the workspace views follow the live turn: they refresh
  // when one of its tool calls finishes, and poll while it runs.
  const liveTurn = runStatus === "running" || runStatus === "waiting_approval";
  const workspaceRefreshKey = `${runStatus}:${(latestAssistant?.toolCards || [])
    .filter((card) => card.status && card.status !== "running" && card.status !== "pending")
    .map((card) => card.id).join(",")}`;
  const deliveryPending = runStatus === "interrupted" &&
    isDeliveryUnconfirmed(latestAssistant?.resultStatus, statusLabel || latestAssistant?.statusLabel);
  const progressLabel = deliveryPending ? "Delivery unconfirmed"
    : sessionId && !(truth.status === "current" && !truth.data?.turn && runStatus === "error")
    ? sessionStateLabel(truth) : runStatus === "cancelled" ? "Cancelled"
    : runStatus === "error" ? "Failed"
    : runStatus === "interrupted" ? (statusLabel === "Verification unresolved" ? statusLabel : "Paused")
    : runStatus === "waiting_approval" ? "Needs your input"
    : allDone ? "Complete"
    : runStatus === "success" ? finishedLabel
    : runStatus === "running" && hasBoard && todoBoard!.done === todoBoard!.total ? "Verifying outcomes"
    : runStatus === "running" ? "Working" : "Ready";
  const attention = runStatus === "waiting_approval"
    ? pendingApproval?.question || pendingApproval?.label || pendingApproval?.action || "A response is needed to continue."
    : runStatus === "interrupted" || runStatus === "error"
      ? latestAssistant?.resume?.reason || latestAssistant?.failureCause || statusLabel || "Execution has stopped. Your saved work is retained."
      : undefined;

  return (
    <div className="min-h-0 flex-1 overflow-y-auto">
      <section className="border-b border-line bg-canvas/40 px-4 py-4" aria-label="Task overview">
        <div className="mb-3 flex items-center justify-between gap-3">
          <p className="text-[0.65rem] font-semibold uppercase tracking-[0.14em] text-muted">Current task</p>
          <div className="flex shrink-0 items-center gap-1.5">
            {liveTurn && <LadderChip step={latestAssistant?.ladderStep} />}
            <span role="status" className={cn("inline-flex items-center gap-1.5 rounded-full border px-2 py-1 text-[0.65rem] font-medium",
              runStatus === "error" ? "border-danger/25 bg-danger-soft text-danger" :
              runStatus === "interrupted" || runStatus === "waiting_approval" ? "border-warn/25 bg-warn-soft text-warn" :
              allDone ? "border-accent/25 bg-accent-soft text-accent" : "border-line text-muted")}>
              <span className={cn("h-1.5 w-1.5 rounded-full bg-current", runStatus === "running" && "animate-pulse")} />
              {progressLabel}
            </span>
          </div>
        </div>
        <p className="break-words text-[0.85rem] font-medium leading-relaxed text-ink">{(() => {
          const jsonBoundary = objective.search(/(\[\s*\{"|\{\s*"error"|\{\s*"id"|New messages:\s*\[)/);
          return jsonBoundary > 0 ? objective.slice(0, jsonBoundary).trim() : objective;
        })()}</p>
        {(() => {
          const jsonBoundary = objective.search(/(\[\s*\{"|\{\s*"error"|\{\s*"id"|New messages:\s*\[)/);
          if (jsonBoundary <= 0) return null;
          const technicalJson = objective.slice(jsonBoundary).trim();
          return (
            <details className="mt-2.5 rounded-lg border border-line/60 bg-surface/60 p-2 text-xs">
              <summary className="cursor-pointer text-[11px] font-medium text-accent hover:underline inline-flex items-center gap-1">
                <Icon.Terminal size={11} />
                <span>Runtime orchestration state</span>
              </summary>
              <pre className="mt-1.5 max-h-36 overflow-y-auto rounded border border-line/40 bg-canvas/80 p-2 text-[10px] text-muted font-mono whitespace-pre-wrap break-words leading-relaxed">
                {technicalJson}
              </pre>
            </details>
          );
        })()}
        {attention && <div className="mt-3 rounded-lg border border-line bg-surface px-3 py-2.5">
          <p className="text-[0.62rem] font-semibold text-muted">{runStatus === "waiting_approval" ? "Needed from you" : "Needs attention"}</p>
          <p className="mt-1 whitespace-pre-wrap break-words text-xs leading-relaxed text-muted">{attention}</p>
        </div>}
        {(deliveryPending && hasExecutionSteps || staleBoard || (hasExecutionSteps && !currentBoard)) &&
          <p className="mt-3 text-xs text-warn">Last received plan · current-turn step state unconfirmed</p>}
        {hasExecutionSteps && todoBoard!.total > 0 && <div className="mt-4">
          <div className="mb-1.5 flex justify-between text-xs text-muted">
            <span className="font-medium text-ink/80">Steps completed</span>
            <span className="tabular-nums font-mono text-xs font-semibold text-accent">{todoBoard!.done} / {todoBoard!.total}</span>
          </div>
          <div className="h-2 overflow-hidden rounded-full bg-line/60 border border-line/40 p-px" aria-label="Execution progress" role="progressbar"
            aria-valuenow={todoBoard!.done} aria-valuemin={0} aria-valuemax={todoBoard!.total}>
            <div className="h-full rounded-full bg-gradient-to-r from-accent/90 to-accent shadow-[0_0_8px_var(--color-accent)] transition-[width] duration-500 ease-out" style={{ width: `${progress}%` }} />
          </div>
        </div>}
        {hasExecutionSteps && items.some(item => item.status === "superseded") && (
          <p className="mt-2 text-xs text-muted">{items.filter(item => item.status === "superseded").length} plan steps superseded</p>
        )}
      </section>

      <div className="p-3 space-y-2">
        {hasExecutionSteps && (
          <div className="flex items-center justify-between gap-2">
            <p className="text-[0.65rem] font-semibold uppercase tracking-[0.14em] text-muted">Plan</p>
            <div role="group" aria-label="Plan view" className="inline-flex rounded-lg border border-line p-0.5 text-[0.65rem]">
              {(["graph", "list"] as const).map((view) => (
                <button
                  key={view}
                  type="button"
                  aria-pressed={planView === view}
                  onClick={() => setPlanView(view)}
                  className={cn(
                    "rounded-md px-2 py-0.5 capitalize transition-colors",
                    planView === view ? "bg-line/60 font-medium text-ink" : "text-muted hover:text-ink",
                  )}
                >
                  {view}
                </button>
              ))}
            </div>
          </div>
        )}
        {showGraph && (
          <Suspense fallback={null}>
            <PlanGraph board={todoBoard!} objective={objective} outcomes={outcomes} running={runStatus === "running"} />
          </Suspense>
        )}
        {hasBoard && todoBoard!.notice && (
          <div className="rounded-xl border border-accent/30 bg-accent/5 px-3 py-2">
            <p className="text-[0.72rem] leading-snug text-ink">{todoBoard!.notice}</p>
            {todoBoard!.version != null && (
              <p className="mt-0.5 text-[0.62rem] text-faint">v{todoBoard!.version}</p>
            )}
          </div>
        )}
        {hasBoard && outcomes.length > 0 && !showGraph && (
          <div className="rounded-xl border border-line bg-canvas px-3 py-2.5">
            <p className="text-[0.7rem] font-semibold text-ink mb-2">Verified outcomes {outcomeProgress.verified}/{outcomeProgress.total}</p>
            {outcomes.map((outcome) => (
              <div key={outcome.id} className="mb-2 text-[0.72rem] leading-snug">
                <p className={cn(outcome.done ? "text-muted" : "text-ink")}>{outcome.status === "superseded" ? "− Superseded: " : outcome.done ? "✓ " : "○ "}{outcome.text}</p>
                {outcome.evidence && <details className="mt-1 text-[0.65rem] text-muted">
                  <summary className="cursor-pointer text-faint hover:text-ink">View evidence</summary>
                  <p className="mt-1.5 whitespace-pre-wrap break-words leading-relaxed">{outcome.evidence}</p>
                </details>}
              </div>
            ))}
          </div>
        )}
        {/* Steps — vertical stepper */}
        {hasExecutionSteps && !showGraph && (
          <div className="rounded-xl border border-line bg-canvas overflow-hidden">
            <button
              type="button"
              className="flex w-full items-center gap-2 px-3 py-2.5 text-left hover:bg-line/20 transition-colors"
              onClick={() => setStepsExpanded((v) => !v)}
            >
              <svg
                width="10" height="10" viewBox="0 0 10 10"
                className={cn("shrink-0 text-muted transition-transform duration-200", stepsExpanded ? "rotate-0" : "-rotate-90")}
              >
                <path d="M2 3.5L5 6.5L8 3.5" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round"/>
              </svg>
              <span className="text-[0.7rem] font-semibold text-ink flex-1">Steps</span>
              {allDone && todoBoard!.total > 0 && <span className="text-[0.6rem] text-accent font-medium">✓ Done</span>}
            </button>
            <div className={cn(
              "grid transition-all duration-300 ease-out",
              stepsExpanded ? "grid-rows-[1fr]" : "grid-rows-[0fr]",
            )}>
              <div className="overflow-hidden">
                <div className="px-3 pb-2.5">
                  {todoBoard!.items.map((item, i) => {
                    const isLast = i === todoBoard!.items.length - 1;
                    const isCurrent = runStatus === "running" && !item.done && (item.status === "active" || (!item.status && item.id === todoBoard!.current_stage_id));
                    return (
                      <div key={item.id} className="flex gap-3 relative">
                        {/* Vertical connector line */}
                        {!isLast && (
                          <div className={cn(
                            "absolute left-[9px] top-[22px] bottom-0 w-[2px] rounded-full",
                            item.done ? "bg-accent/40" : "bg-line",
                          )} />
                        )}
                        {/* Step indicator */}
                        <div className="relative z-10 mt-[5px] shrink-0">
                          {item.status === "superseded" ? (
                            <div className="flex h-[20px] w-[20px] items-center justify-center rounded-full border border-dashed border-line text-muted">−</div>
                          ) : item.done ? (
                            <div className="flex h-[20px] w-[20px] items-center justify-center rounded-full bg-accent text-white">
                              <svg width="10" height="10" viewBox="0 0 12 12" fill="none">
                                <path d="M2 6.5L4.5 9L10 3" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"/>
                              </svg>
                            </div>
                          ) : isCurrent ? (
                            <div className="flex h-[20px] w-[20px] items-center justify-center rounded-full border-2 border-accent bg-accent/10">
                              <div className="h-2 w-2 rounded-full bg-accent animate-pulse" />
                            </div>
                          ) : (
                            <div className="flex h-[20px] w-[20px] items-center justify-center rounded-full border-2 border-line bg-surface">
                              <span className="text-[0.5rem] font-bold text-faint">{i + 1}</span>
                            </div>
                          )}
                        </div>
                        {/* Step text */}
                        <div className={cn(
                          "flex-1 pb-3 pt-[3px]",
                          item.done ? "opacity-60" : isCurrent ? "opacity-100" : "opacity-50",
                        )}>
                          <p className={cn(
                            "text-[0.78rem] leading-snug",
                            item.done ? "text-muted line-through decoration-muted/40" : "text-ink",
                            isCurrent && "font-medium",
                          )}>
                            {friendlyPlanStepText(item.text)}
                          </p>
                          {item.status && item.status !== "active" && item.status !== "done" && item.status !== "pending" && (
                            <p className="mt-0.5 text-[0.65rem] text-muted capitalize">{item.status}</p>
                          )}
                          {item.evidence && (
                            <details className="mt-1 text-[0.65rem] text-muted">
                              <summary className="cursor-pointer text-faint hover:text-ink">View step evidence</summary>
                              <p className="mt-1.5 whitespace-pre-wrap break-words leading-relaxed">{item.evidence}</p>
                            </details>
                          )}
                        </div>
                      </div>
                    );
                  })}
                </div>
              </div>
            </div>
          </div>
        )}

        {/* Plan.md — collapsible rendered markdown */}
        {currentBoard && !staleBoard && (planMd || planLoading) && (
          <div className="rounded-xl border border-line bg-canvas overflow-hidden">
            <button
              type="button"
              className="flex w-full items-center gap-2 px-3 py-2.5 text-left hover:bg-line/20 transition-colors"
              onClick={() => setPlanExpanded((v) => !v)}
            >
              <svg
                width="10" height="10" viewBox="0 0 10 10"
                className={cn("shrink-0 text-muted transition-transform duration-200", planExpanded ? "rotate-0" : "-rotate-90")}
              >
                <path d="M2 3.5L5 6.5L8 3.5" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round"/>
              </svg>
              <span className="text-[0.7rem] font-semibold text-ink flex-1">Plan Details</span>
              <span className="text-[0.55rem] text-faint font-mono">plan.md</span>
            </button>
            <div className={cn(
              "grid transition-all duration-300 ease-out",
              planExpanded ? "grid-rows-[1fr]" : "grid-rows-[0fr]",
            )}>
              <div className="overflow-hidden">
                {planLoading && !planMd ? (
                  <p className="px-3 py-2 text-xs text-muted">Loading…</p>
                ) : planMd ? (
                  <div
                    ref={planMarkdownRef}
                    className="markdown px-3 py-3 text-[0.78rem] border-t border-line/40 overflow-auto max-h-[50vh]"
                    dangerouslySetInnerHTML={{
                      __html: renderMarkdownHtml(planMd),
                    }}
                  />
                ) : null}
              </div>
            </div>
          </div>
        )}

        {/* Task_Workspace: the agent's live todo.md and its files. */}
        <WorkspacePlanCard sessionId={sessionId} running={runStatus === "running"} refreshKey={workspaceRefreshKey} />
        <WorkspaceFiles sessionId={sessionId} running={runStatus === "running"} refreshKey={workspaceRefreshKey} />

        {!hasBoard && !hasExecutionSteps && !planMd && !planLoading && (
          <div className="rounded-xl border border-line bg-canvas p-3.5 space-y-3">
            <div className="flex items-center justify-between">
              <span className="text-[0.72rem] font-semibold text-ink">Session Overview</span>
              <span className="text-[0.62rem] text-faint tabular-nums">
                {messages.length} {messages.length === 1 ? "turn" : "turns"}
              </span>
            </div>
            <p className="text-[0.72rem] leading-relaxed text-muted">
              Live steps, execution plans, and verified outcomes will automatically populate here as the agent works.
            </p>
            {messages.some((m) => (m.toolCards || []).length > 0) && (
              <div className="border-t border-line/60 pt-2.5">
                <p className="text-[0.65rem] font-semibold uppercase tracking-wider text-faint mb-1.5">
                  Recent Tools
                </p>
                <div className="flex flex-wrap gap-1">
                  {Array.from(
                    new Set(
                      messages
                        .flatMap((m) => m.toolCards || [])
                        .map((c) => c.name)
                        .filter(Boolean),
                    ),
                  )
                    .slice(0, 6)
                    .map((toolName) => (
                      <span
                        key={toolName}
                        className="rounded-md border border-line bg-surface px-1.5 py-0.5 text-[0.65rem] font-mono text-muted"
                      >
                        {toolName}
                      </span>
                    ))}
                </div>
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}


// ── Stats Tab ──────────────────────────────────────────────────────────

function StatsTab() {
  const statusLabel = useAppStore((s) => s.statusLabel);
  const messages = useAppStore((s) => s.messages);
  const runStatus = useAppStore((s) => s.runStatus);

  const stats = useMemo<SessionStats>(() => {
    let toolCalls = 0;
    let toolSuccesses = 0;
    let toolFailures = 0;
    let totalTokensIn = 0;
    let totalTokensOut = 0;
    let cachedTokens = 0;
    let totalCostUsd = 0;
    let steps = 0;
    const models = new Set<string>();

    for (const m of messages) {
      if (m.role !== "assistant") continue;
      if (m.toolCards) {
        for (const card of m.toolCards) {
          toolCalls++;
          if (completedTool(card.status)) toolSuccesses++;
          else if (failedTool(card.status)) toolFailures++;
        }
      }
      steps += m.steps?.length || 0;
      const usageSteps = m.modelUsageSteps?.length ? m.modelUsageSteps : m.steps;
      if (usageSteps) {
        for (const step of usageSteps) {
          const blob = `${step.label || ""} ${(step.detail || []).join(" ")}`;
          const tokMatch = blob.match(/tokens?[=:]?\s*(\d+)\+(\d+)/i);
          if (tokMatch) {
            totalTokensIn += parseInt(tokMatch[1], 10);
            totalTokensOut += parseInt(tokMatch[2], 10);
          }
          const cacheMatch = blob.match(/cache_read=(\d+)\/(\d+)/i);
          if (cacheMatch) {
            cachedTokens += parseInt(cacheMatch[1], 10);
          }
          const costMatch = blob.match(/usd~?([\d.]+)/i);
          if (costMatch) {
            totalCostUsd = Math.max(totalCostUsd, parseFloat(costMatch[1]));
          }
          const modelMatch = blob.match(/model[=:]\s*([a-z0-9._-]+)/i);
          if (modelMatch) models.add(modelMatch[1]);
        }
      }
    }

    return {
      steps,
      toolCalls,
      toolSuccesses,
      toolFailures,
      totalTokensIn,
      totalTokensOut,
      cachedTokens,
      totalCostUsd,
      elapsedMs: 0, // Could be calculated from first/last message timestamps
      models: Array.from(models),
    };
  }, [messages]);

  return (
    <div className="min-h-0 flex-1 overflow-y-auto p-3 space-y-4">
      {/* Status */}
      <div className="rounded-lg border border-line bg-canvas p-3">
        <p className="text-[0.65rem] uppercase tracking-wide text-muted mb-2">Status</p>
        <div className="flex items-center gap-2">
          <span className={cn(
            "h-2.5 w-2.5 rounded-full",
            runStatus === "running" ? "bg-accent animate-pulse" :
            runStatus === "success" ? "bg-green-500" :
            runStatus === "error" ? "bg-red-500" : "bg-line-strong"
          )} />
          <span className="text-sm font-medium text-ink">{statusLabel || runStatus}</span>
        </div>
      </div>

      {/* Tool Calls */}
      <div className="rounded-lg border border-line bg-canvas p-3">
        <p className="text-[0.65rem] uppercase tracking-wide text-muted mb-2">Tool Calls</p>
        <div className="grid grid-cols-3 gap-2 text-center">
          <div>
            <p className="text-lg font-bold text-ink">{stats.toolCalls}</p>
            <p className="text-[0.6rem] text-muted">Total</p>
          </div>
          <div>
            <p className="text-lg font-bold text-green-500">{stats.toolSuccesses}</p>
            <p className="text-[0.6rem] text-muted">Success</p>
          </div>
          <div>
            <p className="text-lg font-bold text-red-500">{stats.toolFailures}</p>
            <p className="text-[0.6rem] text-muted">Failed</p>
          </div>
        </div>
      </div>

      {/* Tokens & Cost */}
      <div className="rounded-lg border border-line bg-canvas p-3">
        <p className="text-[0.65rem] uppercase tracking-wide text-muted mb-2">Usage</p>
        <div className="space-y-1.5">
            <div className="flex justify-between text-xs">
              <span className="text-muted">Tokens In</span>
              <span className="font-mono text-ink">{stats.totalTokensIn > 0 ? stats.totalTokensIn.toLocaleString() : "Unavailable"}</span>
            </div>
            <div className="flex justify-between text-xs">
              <span className="text-muted">Tokens Out</span>
              <span className="font-mono text-ink">{stats.totalTokensOut > 0 ? stats.totalTokensOut.toLocaleString() : "Unavailable"}</span>
            </div>
            <div className="flex justify-between text-xs">
              <span className="text-muted">Prompt cache</span>
              <span className="font-mono text-ink">
                {stats.totalTokensIn > 0 ? `${stats.cachedTokens.toLocaleString()} (${Math.round((stats.cachedTokens / stats.totalTokensIn) * 100)}%)` : "Unavailable"}
              </span>
            </div>
          {stats.totalCostUsd > 0 && (
            <div className="flex justify-between text-xs">
              <span className="text-muted">Cost</span>
              <span className="font-mono font-medium text-ink">{formatCost(stats.totalCostUsd)}</span>
            </div>
          )}
          <div className="flex justify-between text-xs">
            <span className="text-muted">Steps</span>
            <span className="font-mono text-ink">{stats.steps}</span>
          </div>
        </div>
      </div>

      {/* Models Used */}
      {stats.models.length > 0 && (
        <div className="rounded-lg border border-line bg-canvas p-3">
          <p className="text-[0.65rem] uppercase tracking-wide text-muted mb-2">Models</p>
          <div className="flex flex-wrap gap-1">
            {stats.models.map((model) => (
              <span key={model} className="rounded bg-accent-soft px-1.5 py-0.5 text-[0.65rem] font-mono text-accent">
                {model}
              </span>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}


// ── Main Canvas Component ──────────────────────────────────────────────

const TAB_CONFIG: { id: CanvasTab; label: string; icon: CanvasTabIcon }[] = [
  { id: "timeline", label: "Timeline", icon: "Activity" },
  { id: "plan", label: "Overview", icon: "Jobs" },
  { id: "subagents", label: "Subagents", icon: "Network" },
  { id: "stats", label: "Stats", icon: "BarChart" },
];

/** Monitoring canvas with Timeline, Artifacts, and Stats tabs. */
export function AgentCanvas({ alwaysShow, onCollapse }: { alwaysShow?: boolean; onCollapse?: () => void } = {}) {
  const canvasOpen = useAppStore((s) => s.canvasOpen);
  const setCanvasOpen = useAppStore((s) => s.setCanvasOpen);
  const [activeTab, setActiveTab] = useState<CanvasTab>("plan");

  // Keep the Subagents surface contextual: it only appears after the
  // coordinator has actually delegated work to a subagent.
  const durableSessionId = useAppStore(s => s.sessionId);
  const durable = useDurableJobs(durableSessionId);
  const hasSubagentActivity = useAppStore((s) =>
    s.messages.some((message) =>
      message.role === "assistant" &&
      message.toolCards?.some((card) => isSubagentSpawn(card.name)),
    ),
  );
  const visibleTabs = useMemo(
    () => TAB_CONFIG.filter((tab) => tab.id !== "subagents" || hasSubagentActivity || durable.jobs.length > 0),
    [hasSubagentActivity, durable.jobs.length],
  );

  useEffect(() => {
    if (!hasSubagentActivity && durable.jobs.length === 0 && activeTab === "subagents") {
      setActiveTab("plan");
    }
  }, [activeTab, hasSubagentActivity, durable.jobs.length]);

  if (!alwaysShow && !canvasOpen) return null;

  return (
    <aside
      className="flex w-full min-w-0 flex-1 flex-col overflow-hidden border-l border-line bg-surface md:min-w-[22rem]"
      id="agent-canvas"
      aria-label="Agent monitoring canvas"
    >
      {/* Header */}
      <header className="flex h-12 shrink-0 items-center gap-1.5 border-b border-line px-2.5 bg-surface/80 backdrop-blur-md">
        {/* Tabs */}
        <div className="flex min-w-0 flex-1 p-0.5 bg-line/25 rounded-xl border border-line/40 backdrop-blur-md gap-0.5" role="tablist" aria-label="Agent details">
          {visibleTabs.map((tab) => {
            const TabIcon = Icon[tab.icon];
            const isSelected = activeTab === tab.id;
            return (
              <button
                key={tab.id}
                type="button"
                role="tab"
                id={`agent-tab-${tab.id}`}
                aria-selected={isSelected}
                aria-controls={`agent-panel-${tab.id}`}
                title={`Show ${tab.label.toLowerCase()}`}
                className={cn(
                  "relative flex min-w-0 flex-1 items-center justify-center rounded-lg px-2 py-1 text-xs font-medium transition-all duration-200 cursor-pointer select-none",
                  isSelected
                    ? "bg-surface text-ink shadow-xs border border-line/50 font-semibold"
                    : "text-muted hover:text-ink hover:bg-surface/40"
                )}
                onClick={() => setActiveTab(tab.id)}
              >
                <TabIcon size={13} className={cn("mr-1.5 shrink-0 transition-colors", isSelected ? "text-accent" : "text-muted")} />
                <span className="truncate">{tab.label}</span>
                {tab.id === "subagents" && durable.jobs.length > 0 && (
                  <span className="ml-1.5 flex h-4 w-4 items-center justify-center rounded-full bg-accent/15 text-[9px] font-bold text-accent tabular-nums">
                    {durable.jobs.length}
                  </span>
                )}
              </button>
            );
          })}
        </div>
        {/* Collapse / Close */}
        {onCollapse && (
          <button
            type="button"
            className="flex h-7 w-7 items-center justify-center rounded-lg text-xs text-muted hover:bg-line/40 hover:text-ink transition-colors cursor-pointer"
            aria-label="Collapse panel"
            title="Collapse panel"
            onClick={onCollapse}
          >
            <Icon.Collapse size={13} />
          </button>
        )}
        {!alwaysShow && (
          <button
            type="button"
            className="flex h-7 w-7 items-center justify-center rounded-lg text-xs text-muted hover:bg-line/40 hover:text-ink transition-colors cursor-pointer"
            aria-label="Close canvas"
            onClick={() => setCanvasOpen(false)}
          >
            <Icon.Close size={13} />
          </button>
        )}
      </header>

      {/* Tab Content */}
      <div
        className="flex min-h-0 flex-1 flex-col overflow-hidden"
        role="tabpanel"
        id={`agent-panel-${activeTab}`}
        aria-labelledby={`agent-tab-${activeTab}`}
      >
        {activeTab === "timeline" && <TimelineTab />}
        {activeTab === "plan" && <PlanTab />}
        {activeTab === "subagents" && (durable.jobs.length ? <div className="min-h-0 flex-1 overflow-y-auto"><DurableJobs jobs={durable.jobs} error={durable.error} /></div> : <SubagentsTab />)}
        {activeTab === "stats" && <StatsTab />}
      </div>

      {/* TodoBoard lives exclusively in the Plan tab now */}
    </aside>
  );
}
