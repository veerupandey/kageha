import { describe, expect, it } from "vitest";
import type { TodoBoard } from "../api/types";
import { NODE_W, buildPlanGraph, buildSubagentGraph, columnsFor } from "./planGraph";

const opts = { objective: "Ship it", running: true };
const plan = (items: TodoBoard["items"], outcomes: TodoBoard["outcomes"] = []) =>
  buildPlanGraph({ done: 0, total: items.length, items, outcomes }, opts);
const layerOf = (g: ReturnType<typeof plan>, id: string) => g.nodes.find((n) => n.id === id)!.layer;
const edgeIds = (g: ReturnType<typeof plan>) => g.edges.map((e) => e.id).sort();

describe("buildPlanGraph", () => {
  it("uses canonical outcomes even when the caller hides a pending objective", () => {
    const board: TodoBoard = { done: 1, total: 1, complete: false,
      items: [{ id: "answer", text: "Answer", done: true }],
      outcomes: [{ id: "scope", text: "Full objective", done: false },
        { id: "answer", text: "Answer", done: true }] };
    const g = buildPlanGraph(board, { ...opts, outcomes: [board.outcomes![1]] });
    expect(g.nodes.find(n => n.id === "goal")?.state).toBe("pending");
  });
  it("keeps withdrawn requirements visibly superseded without false execution checkmarks", () => {
    const g = plan([{ id: "png", text: "Render PNG", done: false, status: "superseded", outcome_ids: ["png"] }],
      [{ id: "png", text: "Unrequested PNG", done: false, status: "superseded" },
       { id: "scope", text: "Explain with visuals", done: true }]);
    expect(g.nodes.find(n => n.id === "s:png")?.state).toBe("superseded");
    expect(g.nodes.find(n => n.id === "o:png")?.state).toBe("superseded");
    expect(g.nodes.find(n => n.id === "goal")?.state).toBe("done");
    expect(g.edges.filter(e => e.from === "s:png").every(e => !e.flowed)).toBe(true);
  });

  it("does not certify the objective just because every execution step finished", () => {
    const g = plan([{ id: "read", text: "Read source", done: true }],
      [{ id: "scope", text: "Verified answer", done: false }]);
    expect(g.nodes.find(n => n.id === "goal")?.state).toBe("pending");
  });
  it("layers steps by depends_on and links outcomes from their deepest claimer", () => {
    const g = plan(
      [
        { id: "a", text: "A", done: true, status: "done" },
        { id: "b", text: "B", done: false, depends_on: ["a"] },
        { id: "c", text: "C", done: false, depends_on: ["a"], outcome_ids: ["o1"] },
        { id: "d", text: "D", done: false, depends_on: ["b", "c"], outcome_ids: ["o1"] },
      ],
      [{ id: "o1", text: "Works", done: false }],
    );
    expect(["goal", "s:a", "s:b", "s:c", "s:d", "o:o1"].map((id) => layerOf(g, id))).toEqual([0, 1, 2, 2, 3, 4]);
    expect(edgeIds(g)).toEqual(["goal->s:a", "s:a->s:b", "s:a->s:c", "s:b->s:d", "s:c->s:d", "s:d->o:o1"].sort());
    expect(g.edges.find((e) => e.id === "s:a->s:b")!.flowed).toBe(true);
    expect(g.edges.find((e) => e.id === "s:b->s:d")!.flowed).toBe(false);
  });

  it("puts an outcome in the row just below its claimer, beside later steps", () => {
    const g = plan(
      [
        { id: "a", text: "A", done: true, outcome_ids: ["early"] },
        { id: "b", text: "B", done: false, depends_on: ["a"] },
        { id: "c", text: "C", done: false, depends_on: ["b"] },
      ],
      [{ id: "early", text: "Early result", done: true }],
    );
    const outcome = g.nodes.find((n) => n.id === "o:early")!;
    expect(outcome.y).toBe(g.nodes.find((n) => n.id === "s:b")!.y);
  });

  it("hangs dependency-free steps off the goal side by side", () => {
    const g = plan([{ id: "a", text: "A", done: false }, { id: "b", text: "B", done: false }]);
    const [a, b] = ["s:a", "s:b"].map((id) => g.nodes.find((n) => n.id === id)!);
    expect(a.y).toBe(b.y);
    expect(b.x - a.x).toBeGreaterThanOrEqual(NODE_W);
    expect(g.edges.filter((e) => e.from === "goal")).toHaveLength(2);
  });

  it("attaches unclaimed outcomes to the final steps", () => {
    const g = plan(
      [{ id: "a", text: "A", done: false }, { id: "b", text: "B", done: false, depends_on: ["a"] }],
      [{ id: "o", text: "Done right", done: true, evidence: "checked" }],
    );
    expect(g.edges.filter((e) => e.to === "o:o").map((e) => e.from)).toEqual(["s:b"]);
    expect(g.nodes.find((n) => n.id === "o:o")).toMatchObject({ state: "done", evidence: "checked" });
  });

  it("drops unknown, self and cycle-closing dependencies so edges always point down", () => {
    const g = plan([
      { id: "a", text: "A", done: false, depends_on: ["a", "ghost", "b"] },
      { id: "b", text: "B", done: false, depends_on: ["a"] },
    ]);
    const pos = (id: string) => g.nodes.find((n) => n.id === id)!;
    expect(g.edges.length).toBeGreaterThan(0);
    for (const e of g.edges) expect(pos(e.from).layer).toBeLessThan(pos(e.to).layer);
  });

  it("routes an edge that skips a row beside the nodes in that row", () => {
    const g = plan([
      { id: "a", text: "A", done: false },
      { id: "b", text: "B", done: false, depends_on: ["a"] },
      { id: "c", text: "C", done: false, depends_on: ["a", "b"] },
    ]);
    const skip = g.edges.find((e) => e.id === "s:a->s:c")!;
    const b = g.nodes.find((n) => n.id === "s:b")!;
    expect(skip.via).toHaveLength(1);
    expect(skip.via[0].x < b.x || skip.via[0].x > b.x + NODE_W).toBe(true);
    for (const n of g.nodes) {
      expect(n.x).toBeGreaterThanOrEqual(0);
      expect(n.x + NODE_W).toBeLessThanOrEqual(g.width);
    }
  });

  it("marks only the current step active, and only while running", () => {
    const items = [
      { id: "a", text: "A", done: true, status: "done" },
      { id: "b", text: "B", done: false, status: "active" },
      { id: "c", text: "C", done: false, status: "blocked" },
    ];
    const states = (running: boolean) =>
      buildPlanGraph({ done: 1, total: 3, items }, { ...opts, running }).nodes.map((n) => n.state);
    expect(states(true)).toEqual(["pending", "done", "active", "blocked"]);
    expect(states(false)).toEqual(["pending", "done", "pending", "blocked"]);
  });
});

describe("width-aware wrapping", () => {
  const fan = (n: number) =>
    Array.from({ length: n }, (_, i) => ({ id: `w${i}`, task: `Worker ${i}` }));

  it("wraps a wide layer into evenly sized rows that fit the panel", () => {
    const g = buildSubagentGraph(fan(4), { label: "Subagents", run: "running", maxCols: 3 });
    const rows = new Map<number, number>();
    for (const n of g.nodes.filter((x) => x.kind === "agent")) rows.set(n.y, (rows.get(n.y) ?? 0) + 1);
    expect([...rows.values()].sort()).toEqual([2, 2]); // 2 + 2, not 3 + 1
    expect(g.width).toBeLessThan(buildSubagentGraph(fan(4), { label: "S", run: "running" }).width);
  });

  it("leaves a layer that already fits untouched and sizes columns from the width", () => {
    const g = buildSubagentGraph(fan(2), { label: "Subagents", run: "running", maxCols: 3 });
    expect(new Set(g.nodes.filter((n) => n.kind === "agent").map((n) => n.y)).size).toBe(1);
    expect(columnsFor(460)).toBe(3);
    expect(columnsFor(100)).toBe(2); // never a single column
  });
});

describe("buildSubagentGraph", () => {
  const finished = [
    { id: "research", task: "Find sources", status: "done", result: "3 sources" },
    { id: "draft", task: "Write draft", depends_on: ["research"], status: "failed", error: "timeout" },
    { id: "review", task: "Review draft", name: "Reviewer", depends_on: ["draft"], status: "blocked" },
  ];

  it("chains nodes by depends_on and maps scheduler statuses", () => {
    const g = buildSubagentGraph(finished, { label: "Task graph", run: "done" });
    expect(g.nodes.map((n) => [n.id, n.layer, n.state])).toEqual([
      ["goal", 0, "pending"],
      ["a:research", 1, "done"],
      ["a:draft", 2, "blocked"],
      ["a:review", 3, "blocked"],
    ]);
    expect(g.nodes.find((n) => n.id === "a:draft")).toMatchObject({ evidence: "timeout", evidenceTitle: "Error" });
    expect(g.nodes.find((n) => n.id === "a:review")!.label).toBe("Reviewer · Review draft");
  });

  it("fans independent workers out from the coordinator", () => {
    const g = buildSubagentGraph([{ id: "a", task: "A" }, { id: "b", task: "B" }], { label: "Subagents", run: "running" });
    expect(g.nodes.filter((n) => n.kind === "agent").map((n) => n.state)).toEqual(["active", "active"]);
    expect(g.edges.filter((e) => e.from === "goal")).toHaveLength(2);
  });

  it("infers waiting dependents while running, and stays neutral when finished without status", () => {
    const nodes = [{ id: "a", task: "A" }, { id: "b", task: "B", depends_on: ["a"] }];
    const states = (run: "running" | "done" | "failed") =>
      buildSubagentGraph(nodes, { label: "Task graph", run }).nodes.filter((n) => n.kind === "agent").map((n) => n.state);
    expect(states("running")).toEqual(["active", "pending"]);
    expect(states("done")).toEqual(["pending", "pending"]);
    expect(states("failed")).toEqual(["blocked", "blocked"]);
  });
});
