/**
 * Task DAGs for the monitor panel: the plan (goal -> steps -> outcomes) and a
 * subagent call (coordinator -> workers). Nodes are buttons, so the graph is
 * keyboard reachable; selecting one shows its full text, dependencies and any
 * evidence below the canvas.
 */

import { useCallback, useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { SubagentGraphNode, TodoBoard } from "../api/types";
import { cn } from "../lib/cn";
import { friendlyPlanStepText } from "../lib/activityUi";
import { Icon } from "../lib/icons";
import {
  NODE_H, NODE_W, buildPlanGraph, buildSubagentGraph, columnsFor, edgePath,
  type PlanGraph as PlanGraphData, type PlanNode, type PlanNodeState, type SubagentRun,
} from "../lib/planGraph";

const MIN_SCALE = 0.85;

const KIND_LABEL = { goal: "Goal", step: "Step", outcome: "Outcome", agent: "Subagent" } as const;
const STATE_LABEL: Record<PlanNodeState, string> = {
  done: "Done",
  active: "In progress",
  blocked: "Blocked",
  pending: "Waiting",
  superseded: "Superseded",
};

/** Tone per state: node surface, caption colour, and the legend/chip dot. */
const TONE: Record<PlanNodeState, { node: string; caption: string; dot: string }> = {
  superseded: {
    node: "border-line/50 border-dashed bg-surface/40 text-muted",
    caption: "text-muted",
    dot: "bg-muted/30",
  },
  done: {
    node: "border-emerald-500/40 bg-emerald-500/[0.08] text-ink hover:border-emerald-500/60 shadow-xs",
    caption: "text-emerald-400 font-semibold",
    dot: "bg-emerald-500 shadow-[0_0_6px_rgba(16,185,129,0.5)]",
  },
  active: {
    node: "border-accent bg-accent/[0.12] text-ink shadow-[0_0_12px_color-mix(in_srgb,var(--color-accent)_25%,transparent)] ring-1 ring-accent/40",
    caption: "text-accent font-semibold",
    dot: "bg-accent animate-pulse shadow-[0_0_8px_var(--color-accent)]",
  },
  blocked: {
    node: "border-rose-500/40 bg-rose-500/[0.08] text-ink hover:border-rose-500/60 shadow-xs",
    caption: "text-rose-400 font-semibold",
    dot: "bg-rose-500 shadow-[0_0_6px_rgba(244,63,94,0.5)]",
  },
  pending: {
    node: "border-line/70 bg-surface/70 text-ink/70 hover:border-line-strong hover:bg-surface/90 shadow-2xs backdrop-blur-xs",
    caption: "text-muted font-medium",
    dot: "bg-muted/50",
  },
};

function StateGlyph({ state }: { state: PlanNodeState }) {
  const base = "flex h-4 w-4 shrink-0 items-center justify-center rounded-full";
  if (state === "superseded") {
    return <span className={cn(base, "border border-line text-muted")} aria-hidden="true">−</span>;
  }
  if (state === "done") {
    return (
      <span className={cn(base, "bg-accent text-white shadow-xs")} aria-hidden="true">
        <Icon.Check size={10} strokeWidth={2.5} />
      </span>
    );
  }
  if (state === "active") {
    return (
      <span className={cn(base, "border-2 border-accent bg-accent/15")} aria-hidden="true">
        <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-accent" />
      </span>
    );
  }
  if (state === "blocked") {
    return (
      <span className={cn(base, "bg-danger text-white shadow-xs")} aria-hidden="true">
        <Icon.Alert size={10} strokeWidth={2.5} />
      </span>
    );
  }
  return <span className={cn(base, "border-2 border-line/80 bg-canvas")} aria-hidden="true" />;
}

export function PlanGraph({
  board,
  objective,
  outcomes,
  running,
}: {
  board: TodoBoard;
  objective: string;
  outcomes: NonNullable<TodoBoard["outcomes"]>;
  running: boolean;
}) {
  const build = useCallback(
    (maxCols: number) => buildPlanGraph(board, { objective, outcomes, running, maxCols }),
    [board, objective, outcomes, running],
  );
  return <DagCanvas build={build} ariaLabel="Plan graph" />;
}

/** Coordinator -> subagents for one spawn_subagents / spawn_task_graph call. */
export function SubagentGraph({
  nodes,
  label,
  run,
}: {
  nodes: SubagentGraphNode[];
  label: string;
  run: SubagentRun;
}) {
  const build = useCallback(
    (maxCols: number) => buildSubagentGraph(nodes, { label, run, maxCols }),
    [nodes, label, run],
  );
  return <DagCanvas build={build} ariaLabel={`${label} graph`} />;
}

function DagCanvas({ build, ariaLabel }: { build: (maxCols: number) => PlanGraphData; ariaLabel: string }) {
  // useId() contains colons, which are awkward inside url(#…).
  const markerId = useId().replace(/:/g, "");
  const [selectedId, setSelectedId] = useState<string | null>(null);

  // Wrap wide layers onto extra rows to fit the panel rather than shrinking text.
  const holderRef = useRef<HTMLDivElement>(null);
  const [available, setAvailable] = useState(0);
  useLayoutEffect(() => {
    const el = holderRef.current;
    if (!el) return;
    const measure = () => setAvailable(el.clientWidth);
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    return () => observer.disconnect();
  }, []);
  const graph = useMemo(
    () => build(available > 0 ? columnsFor(available) : Infinity),
    [build, available],
  );
  // A narrow panel may still be a little too tight for two columns: shrink up
  // to a limit (text stays legible), then fall back to scrolling.
  const scale = available > 0 ? Math.max(MIN_SCALE, Math.min(1, available / graph.width)) : 1;
  const nodeById = useMemo(() => new Map(graph.nodes.map((n) => [n.id, n])), [graph]);
  const selected = selectedId ? nodeById.get(selectedId) ?? null : null;

  const labelFor = (n: PlanNode) => (n.kind === "step" ? friendlyPlanStepText(n.label) : n.label);
  const shortLabel = (n: PlanNode) => {
    const head = n.kind === "agent" ? n.label.split(" · ")[0] : labelFor(n);
    return head.length > 36 ? `${head.slice(0, 35).trimEnd()}…` : head;
  };
  const presentStates = (Object.keys(STATE_LABEL) as PlanNodeState[]).filter((state) =>
    graph.nodes.some((n) => n.kind !== "goal" && n.state === state),
  );
  const waitsFor = selected
    ? graph.edges
        .filter((e) => e.to === selected.id && e.from !== "goal")
        .map((e) => nodeById.get(e.from))
        .filter((n): n is PlanNode => Boolean(n))
    : [];

  return (
    <div className="rounded-xl border border-line bg-canvas px-2.5 py-3" aria-label={ariaLabel} role="group">
      <div ref={holderRef} className="overflow-x-auto overflow-y-hidden">
        <div className="mx-auto" style={{ width: graph.width * scale, height: graph.height * scale }}>
        <div
          className="relative origin-top-left"
          style={{ width: graph.width, height: graph.height, transform: `scale(${scale})` }}
        >
            <svg className="pointer-events-none absolute inset-0" width={graph.width} height={graph.height} aria-hidden="true">
              <defs>
                {(["flow", "idle"] as const).map((kind) => (
                  <marker key={kind} id={`${markerId}-${kind}`} viewBox="0 0 8 8" refX="7" refY="4" markerWidth="6" markerHeight="6" orient="auto">
                    <path d="M0 0L8 4L0 8z" className={kind === "flow" ? "fill-accent/80" : "fill-muted/60"} />
                  </marker>
                ))}
              </defs>
              {graph.edges.map((edge) => {
                const from = nodeById.get(edge.from);
                const to = nodeById.get(edge.to);
                if (!from || !to) return null;
                const touched = selectedId === edge.from || selectedId === edge.to;
                return (
                  <path
                    key={edge.id}
                    d={edgePath(from, to, edge.via)}
                    fill="none"
                    strokeWidth={touched ? 2 : 1.5}
                    strokeLinecap="round"
                    strokeDasharray={edge.flowed ? undefined : "4 4"}
                    className={cn(
                      edge.flowed ? "stroke-accent/75" : "stroke-muted/45",
                      touched && (edge.flowed ? "stroke-accent" : "stroke-muted"),
                    )}
                    markerEnd={`url(#${markerId}-${edge.flowed ? "flow" : "idle"})`}
                  />
                );
              })}
            </svg>
            {graph.nodes.map((node) => {
              const text = labelFor(node);
              const tone = TONE[node.state];
              return (
                <button
                  key={node.id}
                  type="button"
                  aria-pressed={selectedId === node.id}
                  aria-label={`${KIND_LABEL[node.kind]}: ${text}, ${STATE_LABEL[node.state].toLowerCase()}${node.evidence ? ", has details" : ""}`}
                  title={text}
                  onClick={() => setSelectedId((cur) => (cur === node.id ? null : node.id))}
                  style={{ left: node.x, top: node.y, width: NODE_W, height: NODE_H }}
                  className={cn(
                    "absolute flex items-center gap-2 rounded-lg border px-2.5 text-left shadow-xs transition-all duration-150 hover:-translate-y-0.5 hover:shadow-sm focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent",
                    tone.node,
                    node.kind === "outcome" && node.state === "pending" && "border-dashed",
                    selectedId === node.id && "ring-2 ring-accent shadow-md",
                  )}
                >
                  <StateGlyph state={node.state} />
                  <span className="min-w-0 flex-1">
                    <span className={cn("block text-[0.5rem] font-semibold uppercase leading-none tracking-[0.12em]", tone.caption)}>
                      {KIND_LABEL[node.kind]}
                    </span>
                    <span
                      className={cn(
                        "mt-1 line-clamp-2 break-words text-[0.68rem] leading-[1.25]",
                        node.state === "pending" ? "text-muted" : "text-ink",
                      )}
                    >
                      {text}
                    </span>
                  </span>
                  {node.evidence ? (
                    <span
                      className="absolute -right-1 -top-1 flex h-3.5 w-3.5 items-center justify-center rounded-full bg-accent text-[0.45rem] text-white ring-2 ring-canvas"
                      aria-hidden="true"
                    >
                      ◆
                    </span>
                  ) : null}
                </button>
              );
            })}
        </div>
        </div>
      </div>

      {presentStates.length > 0 && (
        <ul className="mt-3 flex flex-wrap items-center gap-x-3.5 gap-y-1.5 px-1 text-[0.65rem] font-medium text-muted" aria-hidden="true">
          {presentStates.map((state) => (
            <li key={state} className="flex items-center gap-1.5">
              <span className={cn("h-2 w-2 rounded-full", TONE[state].dot)} />
              <span className="text-muted/90">{STATE_LABEL[state]}</span>
            </li>
          ))}
        </ul>
      )}

      {selected ? (
        <div className="mt-2.5 rounded-lg border border-line bg-surface px-3 py-2.5" data-testid="plan-graph-detail">
          <p className="flex items-center gap-1.5 text-[0.6rem] font-semibold uppercase tracking-[0.12em] text-muted">
            <span className={cn("h-1.5 w-1.5 rounded-full", TONE[selected.state].dot)} />
            {KIND_LABEL[selected.kind]} · {STATE_LABEL[selected.state]}
          </p>
          <p className="mt-1.5 whitespace-pre-wrap break-words text-[0.75rem] leading-snug text-ink">{labelFor(selected)}</p>
          {waitsFor.length > 0 && (
            <p className="mt-2 text-[0.65rem] leading-snug text-muted">
              <span className="font-semibold">After:</span>{" "}
              {waitsFor.map(shortLabel).join(", ")}
            </p>
          )}
          {selected.evidence ? (
            <>
              <p className="mt-2 text-[0.6rem] font-semibold uppercase tracking-[0.12em] text-muted">{selected.evidenceTitle}</p>
              <p className="mt-1 whitespace-pre-wrap break-words text-[0.68rem] leading-relaxed text-muted">{selected.evidence}</p>
            </>
          ) : (
            <p className="mt-2 text-[0.65rem] text-faint">
              {selected.kind === "agent" ? "No result reported yet." : "No evidence recorded yet."}
            </p>
          )}
        </div>
      ) : null}
    </div>
  );
}
