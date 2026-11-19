import { afterEach, beforeEach, expect, it, vi } from "vitest";
vi.mock("../api/client", async () => ({ ...await vi.importActual<typeof import("../api/client")>("../api/client"), api: vi.fn() }));
import { api } from "../api/client";
import { useAppStore } from "./index";
const original = useAppStore.getState();
beforeEach(() => {
  const data = new Map<string, string>();
  vi.stubGlobal("localStorage", { getItem: (key: string) => data.get(key) ?? null, setItem: (key: string, value: string) => data.set(key, value) });
  useAppStore.setState({ sessions: [{ session_id: "older-pinned", pinned: true }], sessionId: null });
});
afterEach(() => { useAppStore.setState(original); vi.unstubAllGlobals(); vi.clearAllMocks(); });

it("keeps known pinned chats while older pages load, then replaces deleted entries", async () => {
  let resolve!: (value: unknown) => void;
  vi.mocked(api).mockResolvedValueOnce({ sessions: [{ session_id: "recent" }], next_offset: 1 })
    .mockImplementationOnce(() => new Promise(r => { resolve = r; }));
  const refresh = useAppStore.getState().refreshSessions();
  await vi.waitFor(() => expect(api).toHaveBeenCalledTimes(2));
  expect(useAppStore.getState().sessions.some(s => s.session_id === "older-pinned")).toBe(true);
  resolve({ sessions: [{ session_id: "old-project", project_id: "project" }], next_offset: null });
  await refresh;
  expect(useAppStore.getState().sessions.map(s => s.session_id)).toEqual(["recent", "old-project"]);
});

it("does not let an older refresh overwrite a newer complete list", async () => {
  let resolve!: (value: unknown) => void;
  vi.mocked(api).mockImplementationOnce(() => new Promise(r => { resolve = r; }))
    .mockResolvedValueOnce({ sessions: [{ session_id: "fresh" }], next_offset: null });
  const older = useAppStore.getState().refreshSessions();
  await useAppStore.getState().refreshSessions();
  resolve({ sessions: [{ session_id: "stale" }], next_offset: null });
  await older;
  expect(useAppStore.getState().sessions.map(s => s.session_id)).toEqual(["fresh"]);
});

it("keeps only one pending status read per session", async () => {
  const { emptyRun } = await import("./helpers");
  const slow = emptyRun("slow", "web-slow", "main", "");
  const other = emptyRun("other", "web-other", "main", "");
  useAppStore.setState({ sessionId: "slow", threadId: "web-slow", runs: { slow, other } });
  let finish!: (value: unknown) => void;
  vi.mocked(api).mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }))
    .mockResolvedValue({ active_turn: null });
  const pending = useAppStore.getState().recoverLiveProgress("slow");
  await useAppStore.getState().recoverLiveProgress("slow");
  await useAppStore.getState().recoverLiveProgress("slow");
  expect(api).toHaveBeenCalledTimes(1);
  await useAppStore.getState().recoverLiveProgress("other");
  expect(api).toHaveBeenCalledTimes(2);
  finish({ active_turn: null });
  await pending;
  await useAppStore.getState().recoverLiveProgress("slow");
  expect(api).toHaveBeenCalledTimes(3);
});

it("coalesces background refreshes instead of aborting the current page", async () => {
  let finish!: (value: unknown) => void;
  vi.mocked(api).mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  const first = useAppStore.getState().refreshSessions({ background: true });
  await useAppStore.getState().refreshSessions({ background: true });
  expect(api).toHaveBeenCalledTimes(1);
  expect(vi.mocked(api).mock.calls[0][1]?.signal?.aborted).toBe(false);
  finish({ sessions: [{ session_id: "updated" }], next_offset: null });
  await first;
  expect(useAppStore.getState().sessions.map(s => s.session_id)).toEqual(["updated"]);
});
