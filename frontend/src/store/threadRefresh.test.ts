import { afterEach, beforeEach, expect, it, vi } from "vitest";
vi.mock("../api/client", async () => ({ ...await vi.importActual<typeof import("../api/client")>("../api/client"), api: vi.fn() }));
import { api } from "../api/client";
import { useAppStore } from "./index";
import { emptyRun, reconcileHistoryMessages } from "./helpers";
const original = useAppStore.getState();
beforeEach(() => {
  vi.mocked(api).mockReset();
  useAppStore.setState({ ...original, sessionId: null, runs: {}, sessions: [], tabs: [],
    refreshArtifacts: vi.fn().mockResolvedValue(undefined) });
});
afterEach(async () => { await useAppStore.getState().newChat(); useAppStore.setState(original); vi.restoreAllMocks(); });
const snapshot = (id: string) => ({ session_id: id, title: "Fresh title", status: "success", active_turn: null,
  turns: [{ id: "new-turn", status: "success" }], messages: [
    { role: "user", text: "New request", turn_id: "new-turn" },
    { role: "assistant", text: "New answer", turn_id: "new-turn", result_status: "success" },
  ] });

it("shows a parked thread synchronously and merges another tab's completed turn", async () => {
  const run = emptyRun("parked-refresh", "web-parked-refresh");
  run.messages = [{ id: "old", role: "assistant", text: "Old answer", turnId: "old-turn", blocks: [{ type: "narrative", id: "narrative", text: "Old answer" }] }];
  const file = new File(["draft"], "notes.txt");
  run.pendingFiles = [file];
  useAppStore.setState({ runs: { [run.sessionId]: run }, draft: "Unsent reply" });
  let finish!: (data: unknown) => void;
  vi.mocked(api).mockImplementation(() => new Promise(resolve => { finish = resolve; }));
  const opening = useAppStore.getState().openSession(run.sessionId);
  expect(useAppStore.getState().sessionId).toBe(run.sessionId);
  expect(useAppStore.getState().sessionLoading).toBe(false);
  expect(useAppStore.getState().sessionRefreshing).toBe(run.sessionId);
  expect(useAppStore.getState().messages[0]).toBe(run.messages[0]);
  finish(snapshot(run.sessionId));
  await opening;
  const state = useAppStore.getState();
  expect(state.messages.map(m => m.text)).toEqual(["Old answer", "New request", "New answer"]);
  expect(state.messages[0]).toBe(run.messages[0]);
  expect(state.pendingFiles).toEqual([file]);
  expect(state.draft).toBe("Unsent reply");
  expect(state.sessionTitle).toBe("Fresh title");
  expect(state.sessionRefreshing).toBeNull();
  vi.mocked(api).mockResolvedValueOnce(snapshot(run.sessionId));
  await state.openSession(run.sessionId);
  expect(useAppStore.getState().messages.map(m => m.text)).toEqual(["Old answer", "New request", "New answer"]);
});

it("keeps cached content on refresh failure and retries in place", async () => {
  const run = emptyRun("refresh-failure", "web-refresh-failure");
  run.messages = [{ id: "saved", role: "assistant", text: "Still readable" }];
  useAppStore.setState({ runs: { [run.sessionId]: run } });
  vi.mocked(api).mockRejectedValueOnce(new Error("Disconnected"));
  await useAppStore.getState().openSession(run.sessionId);
  expect(useAppStore.getState().messages[0].text).toBe("Still readable");
  expect(useAppStore.getState().sessionOpenFailure).toBeNull();
  expect(useAppStore.getState().sessionRefreshError?.message).toBe("Disconnected");
  vi.mocked(api).mockResolvedValueOnce(snapshot(run.sessionId));
  await useAppStore.getState().openSession(run.sessionId);
  expect(useAppStore.getState().sessionRefreshError).toBeNull();
  expect(useAppStore.getState().messages.at(-1)?.text).toBe("New answer");
});

it("checks status without history and loads details only when a saved turn changes", async () => {
  const id = "lightweight-recovery";
  const run = emptyRun(id, `web-${id}`);
  run.messages = [{ id: "saved", role: "assistant", text: "Old answer", turnId: "old-turn", resultStatus: "success" }];
  useAppStore.setState({ sessionId: id, threadId: run.threadId, runs: { [id]: run } });
  vi.mocked(api).mockResolvedValueOnce({ session_id: id, status: "success", active_turn: null, messages: [], turns: [{ id: "old-turn", status: "success" }] });
  await useAppStore.getState().recoverLiveProgress(id, { lightweight: true });
  expect(api).toHaveBeenCalledTimes(1);
  expect(vi.mocked(api).mock.calls[0][0]).toContain("?include_history=0");
  vi.mocked(api).mockResolvedValueOnce({ ...snapshot(id), messages: [] }).mockResolvedValueOnce(snapshot(id));
  await useAppStore.getState().recoverLiveProgress(id, { lightweight: true });
  expect(api).toHaveBeenCalledTimes(3);
  expect(useAppStore.getState().messages.at(-1)?.text).toBe("New answer");
});

it("preserves the order of copied conversation history during a status-only refresh", async () => {
  const id = "fork-refresh";
  const run = emptyRun(id, `web-${id}`);
  run.messages = [
    { id: "question", role: "user", text: "Original question" },
    { id: "answer", role: "assistant", text: "Original answer" },
  ];
  useAppStore.setState({ sessionId: id, threadId: run.threadId, runs: { [id]: run }, messages: run.messages });
  vi.mocked(api).mockResolvedValueOnce({ session_id: id, status: "idle", active_turn: null, messages: [], turns: [] });
  await useAppStore.getState().recoverLiveProgress(id, { lightweight: true });
  expect(useAppStore.getState().messages).toBe(run.messages);
});

it("matches repeated steering by receipt and preserves hydrated activity and live text", () => {
  const run = emptyRun("receipts");
  run.messages = [
    { id: "u", role: "user", text: "Work", turnId: "turn" },
    { id: "one", role: "user", text: "Again", turnId: "turn", steering: "applied", steeringId: "one" },
    { id: "two", role: "user", text: "Again", turnId: "turn", steering: "pending", steeringId: "two" },
    { id: "a", role: "assistant", text: "Live text", turnId: "turn", streaming: true, toolCards: [{ id: "tool", name: "read_file" }] },
  ];
  const rows = [
    { role: "user", text: "Work", turn_id: "turn" },
    { role: "user", text: "Again", turn_id: "turn", steering: "applied" as const, message_id: "one" },
    { role: "user", text: "Again", turn_id: "turn", steering: "applied" as const, message_id: "two" },
    { role: "assistant", text: "Older text", turn_id: "turn" },
  ];
  const next = reconcileHistoryMessages(run, rows);
  expect(next.messages.map(m => m.id)).toEqual(["u", "one", "two", "a"]);
  expect(next.messages[2].steering).toBe("applied");
  expect(next.messages[3]).toBe(run.messages[3]);
  expect(reconcileHistoryMessages(next, rows)).toBe(next);
});

it("clears an approval resolved elsewhere when its live event was missed", async () => {
  const { applyLiveSessionSnapshot } = await import("./helpers");
  const run = emptyRun("resolved");
  Object.assign(run, { sending: true, waitingApproval: true, status: "waiting_approval", pendingApproval: { approval_id: "approval", action: "approve_plan" } });
  const next = applyLiveSessionSnapshot(run, { pending_approval: null }, "resolved");
  expect(next.waitingApproval).toBe(false);
  expect(next.pendingApproval).toBeNull();
  expect(next.status).toBe("running");
});

it("does not dismiss a live approval that arrived after revalidation began", async () => {
  const run = emptyRun("approval-race", "web-approval-race");
  run.messages = [{ id: "saved", role: "assistant", text: "Earlier reply" }];
  useAppStore.setState({ runs: { [run.sessionId]: run } });
  let finish!: (value: unknown) => void;
  vi.mocked(api).mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  const opening = useAppStore.getState().openSession(run.sessionId);
  const pending = { approval_id: "new-approval", action: "approve_plan" };
  useAppStore.setState(state => ({ runs: { ...state.runs, [run.sessionId]: {
    ...state.runs[run.sessionId], pendingApproval: pending, waitingApproval: true, status: "waiting_approval",
  } } }));
  finish({ ...snapshot(run.sessionId), pending_approval: null });
  await opening;
  expect(useAppStore.getState().pendingApproval?.approval_id).toBe("new-approval");
});
