import type { SubagentGraphNode, TodoBoard } from "../api/types";
import { planOutcomeProgress } from "./planProgress";

export type PlanNodeKind = "goal" | "step" | "outcome" | "agent";
export type PlanNodeState = "done" | "active" | "blocked" | "pending" | "superseded";

export interface PlanNode {
  id: string;
  kind: PlanNodeKind;
  label: string;
  state: PlanNodeState;
  evidence: string;
  /** Heading for `evidence` in the detail panel. */
  evidenceTitle: string;
  layer: number;
  x: number;
  y: number;
}

export interface PlanEdge {
  id: string;
  from: string;
  to: string;
  /** The upstream node is finished, so work along this edge has flowed. */
  flowed: boolean;
  /** Waypoints through rows the edge skips, so it routes around nodes. */
  via: Array<{ x: number; top: number; bottom: number }>;
}

export interface PlanGraph {
  nodes: PlanNode[];
  edges: PlanEdge[];
  width: number;
  height: number;
}

export const NODE_W = 140;
export const NODE_H = 52;
const GAP_X = 10;
const GAP_Y = 34;
const PAD = 8;
const WAYPOINT_W = 16;

// ── Shared pieces ──────────────────────────────────────────────────────

type NodeInit = Pick<PlanNode, "id" | "kind" | "label" | "state" | "layer"> &
  Partial<Pick<PlanNode, "evidence" | "evidenceTitle">>;

const makeNode = (init: NodeInit): PlanNode => ({
  evidence: "",
  evidenceTitle: "Evidence",
  x: 0,
  y: 0,
  ...init,
});

/**
 * Longest-path layers (1-based) for nodes linked by `depends_on`, plus the
 * dependencies that survive: unknown ids, self-references and edges that would
 * close a cycle are dropped, so every kept edge points strictly downward.
 */
function resolveLayers(
  ids: string[],
  rawDeps: Map<string, string[] | undefined>,
): { layer: Map<string, number>; deps: Map<string, string[]> } {
  const known = new Set(ids);
  const candidates = new Map(
    ids.map((id) => [id, [...new Set(rawDeps.get(id) ?? [])].filter((d) => d !== id && known.has(d))]),
  );
  const layer = new Map<string, number>();
  const visiting = new Set<string>();
  const visit = (id: string): number => {
    const seen = layer.get(id);
    if (seen != null) return seen;
    visiting.add(id);
    let depth = 1;
    for (const dep of candidates.get(id) ?? []) {
      if (!visiting.has(dep)) depth = Math.max(depth, visit(dep) + 1);
    }
    visiting.delete(id);
    layer.set(id, depth);
    return depth;
  };
  ids.forEach(visit);
  const deps = new Map(
    ids.map((id) => [id, (candidates.get(id) ?? []).filter((d) => (layer.get(d) ?? 0) < (layer.get(id) ?? 0))]),
  );
  return { layer, deps };
}

function edgeBuilder(nodes: PlanNode[]) {
  const byId = new Map(nodes.map((n) => [n.id, n]));
  const edges: PlanEdge[] = [];
  const seen = new Set<string>();
  const link = (from: string, to: string) => {
    const id = `${from}->${to}`;
    const source = byId.get(from);
    if (!source || !byId.has(to) || seen.has(id)) return;
    seen.add(id);
    edges.push({ id, from, to, flowed: source.state === "done", via: [] });
  };
  return { edges, link };
}

function finish(nodes: PlanNode[], edges: PlanEdge[], maxCols: number): PlanGraph {
  return { nodes, edges, ...layoutGraph(nodes, edges, maxCols) };
}

// ── Plan: goal -> steps -> outcomes ────────────────────────────────────

export interface BuildOptions {
  objective: string;
  running: boolean;
  /** Outcomes after the panel's de-duplication; defaults to the board's. */
  outcomes?: NonNullable<TodoBoard["outcomes"]>;
  /** Widest row, in nodes; wider layers wrap onto extra rows. */
  maxCols?: number;
}

const BLOCKED = /^(blocked|failed|error|stuck)/i;

/**
 * Lay a task board out as a layered DAG. Only edges present in the data are
 * drawn: steps without `depends_on` hang off the goal, and an outcome links from
 * the deepest step(s) claiming it (earlier claimers are implied by the chain). An
 * outcome no step claims hangs off the final steps so it is never orphaned.
 */
export function buildPlanGraph(board: TodoBoard, opts: BuildOptions): PlanGraph {
  const items = board.items;
  const outcomes = opts.outcomes ?? board.outcomes ?? [];
  const outcomeProgress = planOutcomeProgress(board);
  const { layer, deps } = resolveLayers(
    items.map((i) => i.id),
    new Map(items.map((i) => [i.id, i.depends_on])),
  );
  const layerOf = (id: string) => layer.get(id) ?? 1;
  const lastStepLayer = Math.max(0, ...items.map((i) => layerOf(i.id)));

  const claimers = new Map<string, string[]>();
  for (const item of items) {
    for (const oid of item.outcome_ids ?? []) {
      if (outcomes.some((o) => o.id === oid)) claimers.set(oid, [...(claimers.get(oid) ?? []), item.id]);
    }
  }
  const deepest = (oid: string) => {
    const ids = claimers.get(oid) ?? [];
    const max = Math.max(0, ...ids.map(layerOf));
    return ids.filter((id) => layerOf(id) === max);
  };

  const stepState = (item: TodoBoard["items"][number]): PlanNodeState => {
    if (item.status === "superseded") return "superseded";
    if (item.done) return "done";
    if (item.status && BLOCKED.test(item.status)) return "blocked";
    const current = item.status === "active" || (!item.status && item.id === board.current_stage_id);
    return opts.running && current ? "active" : "pending";
  };

  const nodes: PlanNode[] = [
    makeNode({
      id: "goal",
      kind: "goal",
      label: opts.objective,
      state: ((board.outcomes?.length ? outcomeProgress.verified > 0 && outcomeProgress.pending === 0
        : items.length > 0 && items.every(i => i.done)) && board.complete !== false) ? "done" : "pending",
      layer: 0,
    }),
    ...items.map((item) =>
      makeNode({
        id: `s:${item.id}`,
        kind: "step",
        label: item.text,
        state: stepState(item),
        evidence: item.evidence ?? "",
        layer: layerOf(item.id),
      }),
    ),
    ...outcomes.map((outcome) =>
      makeNode({
        id: `o:${outcome.id}`,
        kind: "outcome",
        label: outcome.text,
        state: outcome.status === "superseded" ? "superseded" : outcome.done ? "done" : "pending",
        evidence: outcome.evidence ?? "",
        // One row below its deepest claimer keeps edges short; unclaimed go last.
        layer: claimers.has(outcome.id)
          ? Math.max(...deepest(outcome.id).map(layerOf)) + 1
          : lastStepLayer + 1,
      }),
    ),
  ];

  const { edges, link } = edgeBuilder(nodes);
  const hasDependents = new Set<string>();
  for (const item of items) {
    const parents = deps.get(item.id) ?? [];
    if (!parents.length) link("goal", `s:${item.id}`);
    for (const parent of parents) {
      link(`s:${parent}`, `s:${item.id}`);
      hasDependents.add(parent);
    }
  }
  const leaves = items.filter((i) => !hasDependents.has(i.id));
  for (const outcome of outcomes) {
    const from = claimers.has(outcome.id) ? deepest(outcome.id) : leaves.map((i) => i.id);
    if (from.length) from.forEach((id) => link(`s:${id}`, `o:${outcome.id}`));
    else link("goal", `o:${outcome.id}`);
  }
  return finish(nodes, edges, opts.maxCols ?? Infinity);
}

// ── Subagents: coordinator -> workers ──────────────────────────────────

export type SubagentRun = "running" | "done" | "failed";

const NODE_FAILED = new Set(["failed", "blocked", "uncertain", "stale", "cancelled"]);

/**
 * Coordinator -> subagents, layered by `depends_on` (spawn_task_graph) or fanned
 * out in parallel (spawn_subagents). `run` is the whole call's state and only
 * matters for nodes the scheduler has not reported: while running, nodes with no
 * dependencies are shown as active and the rest as waiting (inferred, not
 * observed); once finished without a per-node status the outcome is unknown, so
 * the node stays neutral rather than claiming success.
 */
export function buildSubagentGraph(
  jobs: SubagentGraphNode[],
  opts: { label: string; run: SubagentRun; maxCols?: number },
): PlanGraph {
  const { layer, deps } = resolveLayers(
    jobs.map((j) => j.id),
    new Map(jobs.map((j) => [j.id, j.depends_on])),
  );
  const stateOf = (job: SubagentGraphNode): PlanNodeState => {
    if (job.status) {
      if (job.status === "done") return "done";
      if (job.status === "running") return "active";
      return NODE_FAILED.has(job.status) ? "blocked" : "pending";
    }
    if (opts.run === "running") return (deps.get(job.id) ?? []).length ? "pending" : "active";
    return opts.run === "failed" ? "blocked" : "pending";
  };

  const nodes: PlanNode[] = [
    makeNode({
      id: "goal",
      kind: "goal",
      label: opts.label,
      state: jobs.length > 0 && jobs.every((j) => j.status === "done") ? "done" : "pending",
      layer: 0,
    }),
    ...jobs.map((job) =>
      makeNode({
        id: `a:${job.id}`,
        kind: "agent",
        label: [job.name || job.id, job.task].filter(Boolean).join(" · "),
        state: stateOf(job),
        evidence: job.error || job.result || "",
        evidenceTitle: job.error ? "Error" : "Result",
        layer: layer.get(job.id) ?? 1,
      }),
    ),
  ];
  const { edges, link } = edgeBuilder(nodes);
  for (const job of jobs) {
    const parents = deps.get(job.id) ?? [];
    if (!parents.length) link("goal", `a:${job.id}`);
    parents.forEach((parent) => link(`a:${parent}`, `a:${job.id}`));
  }
  return finish(nodes, edges, opts.maxCols ?? Infinity);
}

// ── Layout ─────────────────────────────────────────────────────────────

interface Slot {
  id: string;
  /** Visual row. */
  row: number;
  w: number;
  x: number;
  y: number;
  node?: PlanNode;
}

const SPACING = NODE_W + GAP_X;

/**
 * Decide each node's visual row. Logical layers become one or more rows: a layer
 * wider than `maxCols` wraps into evenly sized rows (4 nodes at 3 columns become
 * 2 + 2, not 3 + 1). Nodes are ordered by the mean position of their parents to
 * limit crossings.
 */
function assignRows(nodes: PlanNode[], edges: PlanEdge[], maxCols: number): Map<string, number> {
  const parents = new Map<string, string[]>();
  for (const e of edges) parents.set(e.to, [...(parents.get(e.to) ?? []), e.from]);
  const layers: PlanNode[][] = [];
  for (const n of nodes) (layers[n.layer] ??= []).push(n);

  const rowOf = new Map<string, number>();
  const centre = new Map<string, number>();
  let row = 0;
  for (const layer of layers) {
    if (!layer?.length) continue;
    const score = (n: PlanNode) => {
      const ps = parents.get(n.id) ?? [];
      return ps.length ? ps.reduce((sum, p) => sum + (centre.get(p) ?? 0), 0) / ps.length : 0;
    };
    const ordered = [...layer].sort((a, b) => score(a) - score(b)); // stable
    const rows = Math.max(1, Math.ceil(ordered.length / Math.max(1, maxCols)));
    const size = Math.ceil(ordered.length / rows);
    for (let r = 0; r < rows; r++, row++) {
      const chunk = ordered.slice(r * size, (r + 1) * size);
      chunk.forEach((n, i) => {
        rowOf.set(n.id, row);
        centre.set(n.id, (i - (chunk.length - 1) / 2) * SPACING);
      });
    }
  }
  return rowOf;
}

/**
 * Assign x/y to every node. An edge that skips rows gets a waypoint in each
 * skipped row, which reserves space so it routes around the nodes there. Rows are
 * re-ordered with the waypoints included, then centred.
 */
export function layoutGraph(
  nodes: PlanNode[],
  edges: PlanEdge[],
  maxCols = Infinity,
): { width: number; height: number } {
  const rowOf = assignRows(nodes, edges, maxCols);
  const slots = new Map<string, Slot>(
    nodes.map((n) => [n.id, { id: n.id, row: rowOf.get(n.id) ?? 0, w: NODE_W, x: 0, y: 0, node: n }]),
  );
  const parents = new Map<string, string[]>();
  const chains = new Map<string, Slot[]>();
  for (const edge of edges) {
    const from = slots.get(edge.from);
    const to = slots.get(edge.to);
    if (!from || !to) continue;
    let prev = edge.from;
    const chain: Slot[] = [];
    for (let row = from.row + 1; row < to.row; row++) {
      const slot: Slot = { id: `~${edge.id}~${row}`, row, w: WAYPOINT_W, x: 0, y: 0 };
      slots.set(slot.id, slot);
      chain.push(slot);
      parents.set(slot.id, [prev]);
      prev = slot.id;
    }
    parents.set(edge.to, [...(parents.get(edge.to) ?? []), prev]);
    chains.set(edge.id, chain);
  }

  const rows: Slot[][] = [];
  for (const slot of slots.values()) (rows[slot.row] ??= []).push(slot);
  const column = new Map<string, number>();
  rows.forEach((row, index) => {
    if (!row) return;
    if (index > 0) {
      const score = (slot: Slot) => {
        const ps = parents.get(slot.id) ?? [];
        return ps.length ? ps.reduce((sum, p) => sum + (column.get(p) ?? 0), 0) / ps.length : 0;
      };
      row.sort((a, b) => score(a) - score(b)); // stable: ties keep plan order
    }
    row.forEach((slot, i) => column.set(slot.id, i));
  });

  const filled = rows.filter((row): row is Slot[] => Boolean(row?.length));
  const rowWidth = (row: Slot[]) => row.reduce((sum, s) => sum + s.w, 0) + (row.length - 1) * GAP_X;
  const width = Math.max(NODE_W, ...filled.map(rowWidth)) + PAD * 2;
  filled.forEach((row, index) => {
    let x = (width - rowWidth(row)) / 2;
    const y = PAD + index * (NODE_H + GAP_Y);
    for (const slot of row) {
      slot.x = x;
      slot.y = y;
      if (slot.node) Object.assign(slot.node, { x, y });
      x += slot.w + GAP_X;
    }
  });
  for (const edge of edges) {
    edge.via = (chains.get(edge.id) ?? []).map((s) => ({ x: s.x + s.w / 2, top: s.y, bottom: s.y + NODE_H }));
  }
  const height = filled.length ? filled.length * NODE_H + (filled.length - 1) * GAP_Y + PAD * 2 : PAD * 2;
  return { width, height };
}

/**
 * How many nodes fit side by side in `available` pixels. Never fewer than two: a
 * single column sends every long edge around the outside of the graph, which
 * reads worse than a slight shrink.
 */
export function columnsFor(available: number): number {
  return Math.max(2, Math.floor((available - PAD * 2 + GAP_X) / SPACING));
}

/** Smooth path from one node's bottom-centre to another's top-centre, through any waypoints. */
export function edgePath(from: PlanNode, to: PlanNode, via: PlanEdge["via"] = []): string {
  let cx = from.x + NODE_W / 2;
  let cy = from.y + NODE_H;
  let d = `M ${cx} ${cy}`;
  const curve = (tx: number, ty: number) => {
    const mid = (cy + ty) / 2;
    d += ` C ${cx} ${mid}, ${tx} ${mid}, ${tx} ${ty}`;
    [cx, cy] = [tx, ty];
  };
  for (const v of via) {
    curve(v.x, v.top);
    d += ` L ${v.x} ${v.bottom}`;
    cy = v.bottom;
  }
  curve(to.x + NODE_W / 2, to.y);
  return d;
}
