import { describe, expect, it } from "vitest";
import {
  acknowledgeSteering,
  normalizeToolCard,
  applyLiveSessionSnapshot,
  approvalStatusLabel,
  cancelInFlightRun,
  emptyRun,
  stampPendingMachine,
  isContinueApproval,
  isPlanApproval,
  shouldIgnoreStaleQuestionApproval,
  mapHistoryMessages,
  isInternalRecoveryMessage,
  parseTodoBoardPayload,
  mergeTodoBoard,
  recoveryAfterSequence,
  sessionFileUrl,
  settleTodoBoardOnTurnEnd,
  shouldSkipLiveRecovery,
  syncFromRun,
  touchTransport,
} from "./helpers";

it("preserves verified web recovery separately from clipped tool output", () => {
  const researchRecovery = {recovered:true,attemptedBackends:["http","chromium"],reason:"research_read_failed"};
  const card = normalizeToolCard({tool_card:{id:"source",name:"web_fetch",status:"done",result_preview:JSON.stringify({text:"x".repeat(9000)}),researchRecovery}});
  expect(card?.resultPreview?.length).toBe(800);
  expect(card?.researchRecovery).toEqual(researchRecovery);
  expect(normalizeToolCard({tool_card:{name:"web_fetch",status:"failed",researchRecovery}})?.researchRecovery).toBeUndefined();
});

describe("steering acknowledgements", () => {
  it("reattaches an applied receipt without acknowledging a second identical follow-up", () => {
    const run = emptyRun("s");
    run.messages = [
      { id: "original", role: "user", text: "Again" },
      { id: "a", role: "user", text: "Again", steering: "applied", steeringId: "first" },
      { id: "b", role: "user", text: "Again", steering: "pending", steeringId: "second" },
      { id: "reply", role: "assistant", text: "Working", streaming: true, turnId: "t" },
    ];
    const restored = acknowledgeSteering(run, "Again", "first");
    expect(restored.messages[1].steeringBoundary?.tailTextLength).toBe(7);
    expect(restored.messages[2].steering).toBe("pending");
    expect(acknowledgeSteering(restored, "Again", "first")).toBe(restored);
    expect(acknowledgeSteering(restored, "Again", "second").messages[2].steering).toBe("applied");
  });

  it("matches the full attachment input and acknowledges each duplicate once", () => {
    const run = emptyRun("s");
    const input = "Correction\nAttached files:\n- inputs/detail.txt";
    run.messages = [
      { id: "one", role: "user", text: "Correction", steeringText: input, steering: "pending" },
      { id: "two", role: "user", text: "Correction", steeringText: input, steering: "pending" },
    ];
    const first = acknowledgeSteering(run, input);
    expect(first.messages.map((m) => m.steering)).toEqual(["applied", "pending"]);
    expect(acknowledgeSteering(first, input).messages.map((m) => m.steering)).toEqual(["applied", "applied"]);
    expect(acknowledgeSteering(run, "Other message")).toBe(run);
  });
});

describe("session plan state", () => {
  it("starts each run without a plan board", () => {
    expect(emptyRun("session-a").todoBoard).toBeNull();
    expect(emptyRun("session-a").personaId).toBe("main");
    expect(emptyRun("session-a", null, "brand-expert").personaId).toBe("brand-expert");
  });

  it("restores a parked session's board with the rest of its run", () => {
    const run = emptyRun("session-a");
    run.todoBoard = {
      done: 1,
      total: 2,
      items: [
        { id: "one", text: "Research the product", done: true },
        { id: "two", text: "Summarize findings", done: false },
      ],
    };

    expect(syncFromRun(run).todoBoard).toEqual(run.todoBoard);
  });

  it("keeps Machine routing scoped to the selected session", () => {
    const machineRun = emptyRun("machine-session");
    machineRun.machineActive = true;
    const hostRun = emptyRun("host-session");

    expect(syncFromRun(machineRun).machineActive).toBe(true);
    expect(syncFromRun(machineRun).liveSplit).toBe(true);
    expect(syncFromRun(hostRun).machineActive).toBe(false);
    expect(syncFromRun(hostRun).liveSplit).toBe(false);
    expect(syncFromRun(hostRun).machinePaused).toBe(false);
    expect(syncFromRun(hostRun).machineShellRunning).toBe(false);
  });

  it("carries the home-screen Machine chip onto a newly minted session", () => {
    const minted = stampPendingMachine(emptyRun("new-session"), true);
    expect(minted.machineActive).toBe(true);
    expect(syncFromRun(minted).machineActive).toBe(true);
    expect(stampPendingMachine(emptyRun("host-session"), false).machineActive).toBe(
      false,
    );
  });

  it("keeps a composer model pin scoped to the session that set it", () => {
    // Regression: modelOverride used to live only on top-level AppState, so
    // picking a model in tab A leaked into tab B's next turn on switch.
    const tabA = emptyRun("session-a", "thread-a", "main", "gemini-pro");
    const tabB = emptyRun("session-b", "thread-b", "main", "kimi-k3");

    expect(syncFromRun(tabA).modelOverride).toBe("gemini-pro");
    expect(syncFromRun(tabB).modelOverride).toBe("kimi-k3");
    // A session with no pin restores to Auto, not a leftover pin.
    expect(syncFromRun(emptyRun("session-c")).modelOverride).toBe("");
  });
});

describe("transport diagnostics", () => {
  it("records polling handoff without dropping the previous cursor", () => {
    const run = emptyRun("session-a");
    const next = touchTransport(run, { mode: "polling", lastSequence: 8, reconnectCount: 1 }, 1_000);
    expect(next.transport).toMatchObject({
      mode: "polling",
      lastSequence: 8,
      lastFrameAt: 1_000,
      reconnectCount: 1,
    });
  });

  it("resets the journal cursor when the owning turn changes", () => {
    const run = touchTransport(emptyRun("session-a"), {
      lastSequence: 40,
      turnId: "turn-old",
    });
    const next = touchTransport(run, { turnId: "turn-new", mode: "sse" });
    expect(next.transport).toMatchObject({ turnId: "turn-new", lastSequence: 0 });
  });
});

describe("recoveryAfterSequence", () => {
  it("resumes the same turn from its stored cursor", () => {
    expect(
      recoveryAfterSequence({ mode: "polling", lastSequence: 12, lastFrameAt: 1, reconnectCount: 1, turnId: "turn-7" }, "turn-7"),
    ).toBe(12);
  });

  it("starts a newly active turn from sequence zero", () => {
    expect(
      recoveryAfterSequence({ mode: "sse", lastSequence: 40, lastFrameAt: 1, reconnectCount: 0, turnId: "turn-old" }, "turn-new"),
    ).toBeUndefined();
  });

  it("does not reuse a parked session cursor for a different turn", () => {
    expect(
      recoveryAfterSequence({ mode: "polling", lastSequence: 9, lastFrameAt: 1, reconnectCount: 2, turnId: "parked-1" }, "parked-2"),
    ).toBeUndefined();
  });

  it("does not revive a cancelled run's cursor without a matching turn", () => {
    const run = emptyRun("session-a");
    run.status = "cancelled";
    run.transport = { mode: "sse", lastSequence: 18, lastFrameAt: 1, reconnectCount: 0, turnId: "turn-1" };
    expect(shouldSkipLiveRecovery(run)).toBe(true);
    expect(recoveryAfterSequence(run.transport, "turn-2")).toBeUndefined();
    expect(recoveryAfterSequence(run.transport, "turn-1")).toBe(18);
  });
});

describe("session file urls", () => {
  it("encodes path segments without flattening slashes", () => {
    expect(sessionFileUrl("abc", "artifacts/browser_live.jpg")).toBe(
      "/api/sessions/abc/files/artifacts/browser_live.jpg",
    );
  });
});

describe("cancelInFlightRun", () => {
  it("freezes the streaming assistant as Cancelled and drops planning chips", () => {
    const run = emptyRun("session-a");
    run.messages = [
      { id: "u1", role: "user", text: "tour" },
      {
        id: "a1",
        role: "assistant",
        text: "Opening the page",
        streaming: true,
        statusLabel: "Planning…",
        blocks: [{ type: "status", id: "s1", label: "Planning…" }],
      },
    ];
    const next = cancelInFlightRun(run);
    expect(next.status).toBe("cancelled");
    expect(next.sending).toBe(false);
    expect(next.messages[1].streaming).toBe(false);
    expect(next.messages[1].statusLabel).toBe("Cancelled");
    expect(next.messages[1].text).toBe("Opening the page");
    expect(
      next.messages[1].blocks?.some(
        (block) => block.type === "status" && block.label === "Planning…",
      ),
    ).toBe(false);
  });
});

describe("applyLiveSessionSnapshot", () => {
  it("surfaces a plan approval the same way a refresh would", () => {
    const run = emptyRun("session-a", "thread-a");
    run.sending = true;
    run.status = "running";
    run.statusLabel = "Working…";
    run.messages = [
      { id: "u1", role: "user", text: "Build a listing" },
      { id: "a1", role: "assistant", text: "", streaming: true, statusLabel: "Working…" },
    ];
    const next = applyLiveSessionSnapshot(
      run,
      {
        pending_approval: {
          approval_id: "appr-1",
          action: "approve_plan",
          risk_class: "plan",
          detail: "Plan ready",
        },
        todo_board: {
          done: 0,
          total: 2,
          items: [
            { id: "one", text: "Research", done: false },
            { id: "two", text: "Write copy", done: false },
          ],
        },
        live_message: "Plan ready — awaiting Build/Approve.",
      },
      "session-a",
    );
    expect(next.waitingApproval).toBe(true);
    expect(next.status).toBe("waiting_approval");
    expect(next.statusLabel).toBe("Awaiting Build");
    expect(next.pendingApproval?.approval_id).toBe("appr-1");
    expect(next.todoBoard?.total).toBe(2);
    expect(next.messages[1]?.text).toBe("Plan ready — awaiting Build/Approve.");
    expect(next.messages[1]?.approval?.approval_id).toBe("appr-1");
    expect(syncFromRun(next).runStatus).toBe("waiting_approval");
    expect(syncFromRun(next).pendingApproval?.approval_id).toBe("appr-1");
    expect(isPlanApproval(next.pendingApproval)).toBe(true);
    expect(applyLiveSessionSnapshot(next, {
      pending_approval: {
        approval_id: "appr-1",
        action: "approve_plan",
        risk_class: "plan",
        detail: "Plan ready",
      },
      todo_board: {
        done: 0,
        total: 2,
        items: [
          { id: "one", text: "Research", done: false },
          { id: "two", text: "Write copy", done: false },
        ],
      },
      live_message: "Plan ready — awaiting Build/Approve.",
    }, "session-a")).toBe(next);
  });

  it("still shows a plan approval while a turn is running", () => {
    const run = emptyRun("session-a", "thread-a");
    run.sending = true;
    run.status = "running";
    run.waitingApproval = false;
    const next = applyLiveSessionSnapshot(
      run,
      {
        pending_approval: {
          approval_id: "appr-plan",
          action: "approve_plan",
          risk_class: "plan",
          detail: "Plan ready",
        },
      },
      "session-a",
    );
    expect(next.waitingApproval).toBe(true);
    expect(next.pendingApproval?.approval_id).toBe("appr-plan");
    expect(isPlanApproval(next.pendingApproval)).toBe(true);
  });

  it("still shows an unanswered ask_human while idle", () => {
    const run = emptyRun("session-a", "thread-a");
    run.status = "waiting_approval";
    run.waitingApproval = true;
    run.sending = false;
    const pending = {
      approval_id: "question:turn-1",
      action: "ask_human" as const,
      risk_class: "hitl",
      detail: "Forest look or keep?",
    };
    run.pendingApproval = pending;
    expect(shouldIgnoreStaleQuestionApproval(run, pending)).toBe(false);
    const next = applyLiveSessionSnapshot(
      run,
      { pending_approval: pending },
      "session-a",
    );
    expect(next.waitingApproval).toBe(true);
    expect(next.pendingApproval?.approval_id).toBe("question:turn-1");
  });

  it("does not restore a stale ask_human banner after the user already answered", () => {
    const run = emptyRun("session-a", "thread-a");
    run.sending = true;
    run.status = "running";
    run.statusLabel = "Planning…";
    run.waitingApproval = false;
    run.pendingApproval = null;
    run.messages = [
      { id: "u1", role: "user", text: "Regenerate in forest look" },
      { id: "a1", role: "assistant", text: "", streaming: true, statusLabel: "Planning…" },
    ];
    const next = applyLiveSessionSnapshot(
      run,
      {
        pending_approval: {
          approval_id: "question:old-turn",
          action: "ask_human",
          risk_class: "hitl",
          detail: "Regenerate that scene with the forest look, or keep it?",
        },
      },
      "session-a",
    );
    expect(next.waitingApproval).toBeFalsy();
    expect(next.pendingApproval).toBeNull();
    expect(next.status).toBe("running");
  });

  it("does not restore a credential approval after the credential was submitted", async () => {
    const { suppressCredentialApproval } = await import("./helpers");
    const run = emptyRun("session-a", "thread-a");
    suppressCredentialApproval("session-a", "credential:turn-1");
    const next = applyLiveSessionSnapshot(
      run,
      {
        pending_approval: {
          approval_id: "credential:turn-1",
          action: "request_credential",
          risk_class: "hitl",
          secret: true,
          label: "instagram password",
        },
      },
      "session-a",
    );
    expect(next.waitingApproval).toBe(false);
    expect(next.pendingApproval).toBeNull();
  });

  it("does not clear an in-flight Working state when the snapshot has no gate yet", () => {
    const run = emptyRun("session-a");
    run.status = "running";
    run.statusLabel = "Working…";
    const next = applyLiveSessionSnapshot(run, {}, "session-a");
    expect(next).toBe(run);
  });
});

describe("shouldSkipLiveRecovery", () => {
  it("blocks recovery after a user-requested Stop even when abort is already gone", () => {
    const run = emptyRun("session-a");
    run.status = "cancelled";
    run.sending = false;
    run.abort = null;
    expect(shouldSkipLiveRecovery(run)).toBe(true);
  });

  it("skips while the live SSE consumer is still attached", () => {
    const run = emptyRun("session-a");
    run.status = "running";
    run.sending = true;
    run.abort = new AbortController();
    expect(shouldSkipLiveRecovery(run)).toBe(true);
    run.abort.abort();
    expect(shouldSkipLiveRecovery(run)).toBe(false);
  });

  it("allows recovery for a dropped in-flight run that was not cancelled", () => {
    const run = emptyRun("session-a");
    run.status = "running";
    run.sending = true;
    run.abort = null;
    expect(shouldSkipLiveRecovery(run)).toBe(false);
    expect(shouldSkipLiveRecovery(undefined)).toBe(false);
  });
});

describe("parseTodoBoardPayload", () => {
  it("keeps replacement notice, version, and row evidence", () => {
    expect(
      parseTodoBoardPayload({
        done: 1,
        total: 2,
        notice: "Live plan updated — previous list archived.",
        version: 2,
        items: [
          { id: "g1", text: "Lock characters", done: true, evidence: "artifacts/chars.md" },
          { id: "g2", text: "Write chapter 1", done: false },
        ],
      }),
    ).toEqual({
      done: 1,
      total: 2,
      notice: "Live plan updated — previous list archived.",
      version: 2,
      items: [
        { id: "g1", text: "Lock characters", done: true, evidence: "artifacts/chars.md" },
        { id: "g2", text: "Write chapter 1", done: false },
      ],
    });
  });
});

describe("settleTodoBoardOnTurnEnd", () => {
  it("does not invent step completion from reply success", () => {
    const board = {
      done: 0,
      total: 1,
      items: [{ id: "act", text: "Follow-up", done: false }],
    };
    expect(
      settleTodoBoardOnTurnEnd(board, { status: "success", mode: "normal" }),
    ).toBe(board);
  });

  it("leaves multi-step plan boards alone", () => {
    const board = {
      done: 1,
      total: 2,
      items: [
        { id: "one", text: "Research", done: true },
        { id: "two", text: "Write", done: false },
      ],
    };
    expect(
      settleTodoBoardOnTurnEnd(board, { status: "success", mode: "plan" }),
    ).toEqual(board);
  });

  it("does not settle while awaiting approval", () => {
    const board = {
      done: 0,
      total: 1,
      items: [{ id: "act", text: "Follow-up", done: false }],
    };
    expect(
      settleTodoBoardOnTurnEnd(board, {
        status: "success",
        mode: "normal",
        awaiting: true,
      }),
    ).toEqual(board);
  });
});

describe("mapHistoryMessages", () => {
  it("keeps agent-only recovery context out of the conversation", () => {
    const recovery = "Continue the interrupted request: book a doctor\n\nClarification question: Virtual or in-person?\nUser's answer: Virtual";
    expect(isInternalRecoveryMessage(recovery)).toBe(true);
    expect(mapHistoryMessages([
      { role: "user", text: recovery },
      { role: "assistant", text: "Continuing with a virtual appointment." },
    ])).toHaveLength(1);
  });

  it("hydrates user-bubble attachments from chat.jsonl rows", () => {
    const messages = mapHistoryMessages([
      {
        role: "user",
        text: "swap the tin",
        attachments: ["inputs/ad.jpg", "inputs/tin.jpg"],
      },
      { role: "assistant", text: "Done" },
    ]);
    expect(messages[0].attachments).toEqual(["inputs/ad.jpg", "inputs/tin.jpg"]);
    expect(messages[1].attachments).toBeUndefined();
  });

  it("hydrates assistant-bubble deliverables from chat.jsonl rows", () => {
    const messages = mapHistoryMessages([
      { role: "user", text: "go chapter 2" },
      {
        role: "assistant",
        text: "Chapter 2",
        attachments: ["artifacts/ch2_s5.jpg", "artifacts/ch2_s6.jpg"],
      },
    ]);
    expect(messages[1].attachments).toEqual([
      "artifacts/ch2_s5.jpg",
      "artifacts/ch2_s6.jpg",
    ]);
  });
});

describe("continue approval", () => {
  it("labels budget pauses as Continue?", () => {
    const pending = {
      approval_id: "budget:1",
      action: "continue_after_budget",
      risk_class: "loop",
    };
    expect(isContinueApproval(pending)).toBe(true);
    expect(approvalStatusLabel(pending)).toBe("Continue?");
    expect(isContinueApproval({ approval_id: "q", action: "ask_human" })).toBe(false);
  });
});

it("adds an offline reminder on refresh without replacing live messages or duplicating it", () => {
  const run = emptyRun("origin", "thread");
  run.messages = [{ id: "existing", role: "assistant", text: "Original answer" }];
  const snapshot = { messages: [{ role: "assistant", text: "Interview tomorrow", job_reply_id: "job:reminder:done" }] };
  const refreshed = applyLiveSessionSnapshot(run, snapshot, "origin");
  expect(refreshed.messages.map((row) => row.text)).toEqual(["Original answer", "Interview tomorrow"]);
  expect(applyLiveSessionSnapshot(refreshed, snapshot, "origin").messages).toHaveLength(2);
});


describe("canonical plan revisions", () => {
  const board = {
    done: 1, total: 1, revision: 4, version: 2, turn_id: "turn", complete: false,
    current_stage_id: "", verified: 0, outcome_total: 1,
    items: [{ id: "work", text: "Work", done: true, status: "done" }],
    outcomes: [{ id: "result", text: "Result verified", done: false, evidence: "" }],
  };

  it("preserves execution and verification independently", () => {
    expect(parseTodoBoardPayload(board)).toEqual(board);
  });

  it("preserves superseded outcome state through replay and live parsing", () => {
    const retired = { ...board, done: 0, total: 0,
      items: [{ id: "png", text: "Render PNG", done: false, status: "superseded" }],
      outcomes: [{ id: "png", text: "Unrequested PNG", done: false, evidence: "report", status: "superseded" }] };
    expect(parseTodoBoardPayload(retired)).toEqual(retired);
  });

  it("ignores stale events and revisionless snapshots", () => {
    expect(mergeTodoBoard(board, { ...board, revision: 3, done: 0 })).toBe(board);
    expect(mergeTodoBoard(board, { ...board, revision: undefined, done: 0 })).toBe(board);
    expect(mergeTodoBoard(board, { ...board, revision: 5, complete: true })?.complete).toBe(true);
  });

  it("does not turn execution or cancellation into verified outcomes", () => {
    for (const status of ["success", "cancelled", "error"]) {
      expect(settleTodoBoardOnTurnEnd(board, { status, mode: "goal" })).toBe(board);
    }
  });

  it("ignores malformed outcomes without inventing completion", () => {
    expect(parseTodoBoardPayload({ ...board, outcomes: [null, "invalid", board.outcomes[0]] })?.outcomes).toEqual(board.outcomes);
  });
});

describe("normalizeToolCard subagent graph", () => {
  it("keeps structured nodes from the server tool card and drops malformed ones", () => {
    const card = normalizeToolCard({
      tool_card: {
        name: "spawn_task_graph",
        args_preview: "nodes_json=[{…",
        status: "ok",
        subagent_graph: [
          { id: "a", task: "First", status: "done", result: "ok" },
          { id: "b", task: "Second", depends_on: ["a", 3], error: "boom" },
          null,
          { task: "no id" },
        ],
      },
    });
    expect(card?.subagentGraph).toEqual([
      { id: "a", task: "First", status: "done", result: "ok" },
      { id: "b", task: "Second", depends_on: ["a"], error: "boom" },
    ]);
  });

  it("omits the field for ordinary tools", () => {
    expect(normalizeToolCard({ tool_card: { name: "bash", status: "ok" } })).not.toHaveProperty("subagentGraph");
  });
});

it("Stop settles unfinished tool blocks and legacy cards without overwriting completed siblings", () => {
  const run = emptyRun("stopped");
  const running = { id: "running", name: "browser_script", status: "running" };
  const done = { id: "done", name: "browser_snapshot", status: "completed", resultPreview: "Page" };
  run.messages = [{ id: "answer", role: "assistant", text: "Working", streaming: true,
    toolCards: [running, done],
    blocks: [{ type: "tool", id: "tool-running", card: running }, { type: "tool", id: "tool-done", card: done }],
  }];
  const message = cancelInFlightRun(run).messages[0];
  expect(message.toolCards?.[0].status).toBe("interrupted");
  expect(message.toolCards?.[0].resultPreview).toContain("Check the outcome");
  expect(message.toolCards?.[1]).toBe(done);
  const block = message.blocks?.find(b => b.type === "tool" && b.card.id === "running");
  expect(block?.type === "tool" && block.card.status).toBe("interrupted");
  expect(run.messages[0].toolCards?.[0].status).toBe("running");
});


it("does not append a saved foreground question after the user's answer", () => {
  const run = emptyRun("origin", "thread");
  run.messages = [
    { id: "question", role: "assistant", text: "Which languages?", turnId: "question-turn" },
    { id: "answer", role: "user", text: "1", turnId: "answer-turn" },
    { id: "working", role: "assistant", text: "", streaming: true, turnId: "answer-turn" },
  ];
  const snapshot = { messages: [
    { role: "assistant", text: "Which languages?", turn_id: "question-turn", job_reply_id: "foreground:question-turn" },
    { role: "user", text: "1", turn_id: "answer-turn" },
  ] };
  expect(applyLiveSessionSnapshot(run, snapshot, "origin")).toBe(run);
});

it("restores a missing foreground question before its answer and only once", () => {
  const run = emptyRun("origin", "thread");
  run.messages = [
    { id: "answer", role: "user", text: "1", turnId: "answer-turn" },
    { id: "working", role: "assistant", text: "", streaming: true, turnId: "answer-turn" },
  ];
  const snapshot = { messages: [
    { role: "assistant", text: "Which languages?", turn_id: "question-turn", job_reply_id: "foreground:question-turn" },
    { role: "user", text: "1", turn_id: "answer-turn" },
  ] };
  const next = applyLiveSessionSnapshot(run, snapshot, "origin");
  expect(next.messages.map((message) => message.id)).toEqual(["foreground:question-turn", "answer", "working"]);
  expect(applyLiveSessionSnapshot(next, snapshot, "origin")).toBe(next);
});

it("accepts new-turn planning despite equal version and fences stale snapshots", () => {
  const old = { done: 1, total: 1, items: [{ id: "old", text: "Advice", done: true }], turn_id: "old-turn", revision: 16, version: 2 };
  const next = { done: 0, total: 0, items: [], objective: "Edit resume", turn_id: "new-turn", version: 2 };
  expect(mergeTodoBoard(old, next, "new-turn")).toEqual(next);
  expect(mergeTodoBoard(next, old, "new-turn")).toEqual(next);
  const canonical = { ...next, revision: 1 };
  expect(mergeTodoBoard(next, canonical, "new-turn")).toEqual(canonical);
  expect(mergeTodoBoard(canonical, next, "new-turn")).toEqual(canonical);
});
