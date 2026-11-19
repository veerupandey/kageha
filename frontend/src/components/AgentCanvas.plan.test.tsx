import "@testing-library/jest-dom/vitest";
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useAppStore } from "../store";
import { PlanTab } from "./AgentCanvas";
import type { TodoBoard } from "../api/types";
import * as sessionTruth from "../lib/sessionTruth";

const original = useAppStore.getState();
afterEach(() => { cleanup(); vi.restoreAllMocks(); useAppStore.setState(original); });

const board: TodoBoard = {
  done: 1, total: 2, revision: 4, version: 2, turn_id: "turn", complete: false,
  current_stage_id: "inspect", verified: 0, outcome_total: 1,
  items: [
    { id: "download", text: "Download checkpoint", done: true, status: "done" },
    { id: "inspect", text: "Inspect checkpoint", done: false, status: "waiting" },
  ],
  outcomes: [{ id: "usable", text: "Checkpoint is usable", done: false }],
};

function mount(todoBoard: TodoBoard, runStatus: ReturnType<typeof useAppStore.getState>["runStatus"] = "running") {
  useAppStore.setState({ todoBoard, runStatus, sessionId: null, messages: [] });
  const view = render(<PlanTab />);
  // The graph is the default view; these tests cover the list view.
  const list = screen.queryByRole("button", { name: "list" });
  if (list) fireEvent.click(list);
  return view;
}

describe("live plan", () => {
  it("shows an unconfirmed follow-up above the last saved turn's completion", () => {
    vi.spyOn(sessionTruth, "useSessionTruth").mockReturnValue({ status: "current", checkedAt: 100,
      data: { schema_version: 1, session_id: "session", observed_at: 100, revision: 5,
        selected_target: "local", target_conflict: false,
        turn: { id: "turn", status: "success", phase: "completed", updated_at: 100,
          steps_done: 1, steps_total: 2, failed_calls: 1, actual_target: "local", actual_target_source: "receipt" } } });
    useAppStore.setState({ todoBoard: board, runStatus: "interrupted", sessionId: "session",
      statusLabel: "Delivery unconfirmed · check before retrying", messages: [] });
    render(<PlanTab />);
    expect(screen.getByRole("status")).toHaveTextContent("Delivery unconfirmed");
    expect(screen.getByText(/Last received plan/)).toBeInTheDocument();
  });

  it("marks another turn's plan unconfirmed even when the current turn is done", () => {
    vi.spyOn(sessionTruth, "useSessionTruth").mockReturnValue({ status: "current", checkedAt: 100,
      data: { schema_version: 1, session_id: "session", observed_at: 100, revision: 5,
        selected_target: "local", target_conflict: false,
        turn: { id: "new-turn", status: "success", phase: "completed", updated_at: 100,
          steps_done: 2, steps_total: 2, failed_calls: 0, actual_target: "local", actual_target_source: "receipt" } } });
    useAppStore.setState({ todoBoard: { ...board, turn_id: "old-turn", complete: true, done: 2,
      items: board.items.map(i => ({ ...i, done: true })), outcomes: [] },
      runStatus: "success", sessionId: "session", messages: [] });
    render(<PlanTab />);
    expect(screen.getByText("Last received plan · current-turn step state unconfirmed")).toBeInTheDocument();
    expect(screen.queryByText("✓ Done")).not.toBeInTheDocument();
  });
  it("does not show a Done badge for a successful response with an incomplete plan", () => {
    mount(board, "success");
    expect(screen.getByRole("status")).toHaveTextContent("Response finished · plan incomplete");
    expect(screen.queryByText("✓ Done")).not.toBeInTheDocument();
  });

  it("does not waive a pending objective hidden by display deduplication", () => {
    mount({ ...board, objective: "Explain", done: 2,
      items: board.items.map(i => ({ ...i, done: true, status: "done" })),
      outcomes: [{ id: "scope", text: "Explain", done: false },
        { id: "answer", text: "Readable answer", done: true }] }, "success");
    expect(screen.getByRole("status")).toHaveTextContent("Response finished · verification pending");
    expect(screen.queryByText("✓ Done")).not.toBeInTheDocument();
    expect(screen.getByText("Verified outcomes 1/2")).toBeInTheDocument();
  });

  it("does not infer completion against an explicit incomplete backend board", () => {
    mount({ ...board, done: 2, complete: false,
      items: board.items.map(i => ({ ...i, done: true, status: "done" })),
      outcomes: [{ id: "answer", text: "Readable answer", done: true }] }, "success");
    expect(screen.getByRole("status")).toHaveTextContent("Response finished · verification pending");
    expect(screen.queryByText("Complete")).not.toBeInTheDocument();
  });

  it("retains superseded steps in the list without false checkmarks or invalid progress", () => {
    const { container } = mount({ ...board, done: 0, total: 0, items: board.items.map(i => ({ ...i, done: false, status: "superseded" })),
      outcomes: [{ id: "old", text: "PNG requirement", done: false, status: "superseded" },
                 { id: "scope", text: "Visual explanation", done: true }] }, "success");
    expect(screen.getAllByText("superseded")).toHaveLength(2);
    expect(screen.getByText("− Superseded: PNG requirement")).toBeInTheDocument();
    expect(screen.getByText("Verified outcomes 1/1")).toBeInTheDocument();
    expect(screen.getByText("2 plan steps superseded")).toBeInTheDocument();
    expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
    expect(screen.queryByText("✓ Done")).not.toBeInTheDocument();
    expect(container.innerHTML).not.toContain("NaN");
  });
  it("shows completed steps separately from unverified outcomes", () => {
    mount(board);
    expect(screen.getByText("Download checkpoint")).toHaveClass("line-through");
    expect(screen.getByText("Inspect checkpoint")).not.toHaveClass("line-through");
    expect(screen.getByText("waiting")).toBeInTheDocument();
    expect(screen.getByText("Verified outcomes 0/1")).toBeInTheDocument();
    expect(screen.queryByText("Complete")).not.toBeInTheDocument();
  });

  it("keeps verification pending after all execution steps finish", () => {
    mount({ ...board, done: 2, items: board.items.map((item) => ({ ...item, done: true, status: "done" })) });
    expect(screen.getByText("Verifying outcomes")).toBeInTheDocument();
    expect(screen.queryByText("Complete")).not.toBeInTheDocument();
  });

  it("does not complete a cancelled single step", () => {
    mount({ ...board, done: 0, total: 1, items: [board.items[1]] }, "cancelled");
    expect(screen.getByText("Cancelled")).toBeInTheDocument();
    expect(screen.queryByText("Complete")).not.toBeInTheDocument();
  });
});

it("waits for a terminal success even if the board reports complete", () => {
  mount({ ...board, complete: true, done: 2, items: board.items.map(item => ({ ...item, done: true })) });
  expect(screen.getByText("Verifying outcomes")).toBeInTheDocument();
  expect(screen.queryByText("Complete")).not.toBeInTheDocument();
});

it("shows unresolved verification without a running step or false completion", () => {
  useAppStore.setState({ statusLabel: "Verification unresolved" });
  const { container } = mount({ ...board, complete: true }, "interrupted");
  expect(screen.getByRole("status")).toHaveTextContent("Verification unresolved");
  expect(screen.queryByText("Now")).not.toBeInTheDocument();
  expect(screen.queryByText("Complete")).not.toBeInTheDocument();
  expect(container.querySelector(".animate-pulse")).toBeNull();
});

it("does not repeat a one-step objective as progress and an outcome", () => {
  const objective = "Compare available approaches";
  mount({ ...board, objective, done: 0, total: 1,
    items: [{ id: "one", text: objective, done: false }],
    outcomes: [{ id: "result", text: objective, done: false }] });
  expect(screen.getAllByText(objective)).toHaveLength(1);
  expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
});


it("shows a follow-up step once instead of repeating it as Now and outcomes", () => {
  const text = "yes please go deep";
  mount({ ...board, objective: "Can you read my Kundali", done: 0, total: 1,
    current_stage_id: "followup", verified: 0, outcome_total: 2,
    items: [{ id: "followup", text, done: false, status: "active" }],
    outcomes: [{ id: "first", text, done: false }, { id: "second", text, done: false }] });
  expect(screen.getAllByText(text)).toHaveLength(1);
  expect(screen.queryByText("Now")).not.toBeInTheDocument();
  expect(screen.queryByText(/Verified outcomes/)).not.toBeInTheDocument();
});

it("preserves verification evidence even when its text matches a step", () => {
  mount({ ...board, outcomes: [{ id: "verified", text: "Inspect checkpoint", done: true, evidence: "Checksum matched" }] });
  expect(screen.getByText("Verified outcomes 1/1")).toBeInTheDocument();
  expect(screen.getByText("Checksum matched")).toBeInTheDocument();
});

describe("plan graph view", () => {
  const dag: TodoBoard = {
    ...board,
    items: [
      { id: "download", text: "Download checkpoint", done: true, status: "done", outcome_ids: ["usable"], evidence: "200 OK" },
      { id: "inspect", text: "Inspect checkpoint", done: false, status: "active", depends_on: ["download"], outcome_ids: ["usable"] },
    ],
  };

  it("defaults to a graph and shows evidence when a node is selected", async () => {
    useAppStore.setState({ todoBoard: dag, runStatus: "running", sessionId: null, messages: [] });
    render(<PlanTab />);
    const node = await screen.findByRole("button", { name: /Step: Download checkpoint, done, has details/ });
    expect(screen.getByRole("button", { name: /Outcome: Checkpoint is usable, waiting/ })).toBeInTheDocument();
    expect(screen.queryByText("Verified outcomes 0/1")).not.toBeInTheDocument();
    fireEvent.click(node);
    expect(screen.getByTestId("plan-graph-detail")).toHaveTextContent("200 OK");
    fireEvent.click(node);
    expect(screen.queryByTestId("plan-graph-detail")).not.toBeInTheDocument();
  });

  it("switches back to the list view", () => {
    useAppStore.setState({ todoBoard: dag, runStatus: "running", sessionId: null, messages: [] });
    render(<PlanTab />);
    fireEvent.click(screen.getByRole("button", { name: "list" }));
    expect(screen.getByText("Verified outcomes 0/1")).toBeInTheDocument();
    expect(screen.queryByRole("group", { name: "Plan graph" })).not.toBeInTheDocument();
  });
});

it("renders subagent nodes with their results", async () => {
  const { SubagentGraph } = await import("./PlanGraph");
  render(<SubagentGraph label="Task graph" run="done" nodes={[
    { id: "a", task: "Collect data", status: "done", result: "42 rows" },
    { id: "b", task: "Summarise", depends_on: ["a"], status: "failed", error: "model timeout" },
  ]} />);
  fireEvent.click(screen.getByRole("button", { name: /Subagent: b · Summarise, blocked, has details/ }));
  expect(screen.getByTestId("plan-graph-detail")).toHaveTextContent("Errormodel timeout");
  expect(screen.getByTestId("plan-graph-detail")).toHaveTextContent("After: a");
  fireEvent.click(screen.getByRole("button", { name: /Subagent: a · Collect data, done/ }));
  expect(screen.getByTestId("plan-graph-detail")).toHaveTextContent("Result42 rows");
});

it("hides legacy plan details while a new request awaits its turn identity", async () => {
  vi.spyOn(globalThis, "fetch").mockResolvedValue(new Response("# Previous request plan"));
  vi.spyOn(sessionTruth, "useSessionTruth").mockReturnValue({ status: "checking", data: null, checkedAt: 0 });
  useAppStore.setState({ sessionId: "session", runStatus: "running", messages: [],
    todoBoard: { done: 0, total: 0, items: [], objective: "New request", notice: "Preparing this request…" } });
  await act(async () => { render(<PlanTab />); });
  expect(screen.queryByRole("button", { name: /Plan Details/ })).not.toBeInTheDocument();
  expect(screen.getByText("New request")).toBeInTheDocument();
});
