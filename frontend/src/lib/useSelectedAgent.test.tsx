// Presentation/restore cases retain legacy transport coverage; native streaming is exercised in sessionEvents.test.ts.
vi.mock("./workerConnection", async () => import("./legacyWorkerConnection"));
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { useSelectedAgent } from "./useSelectedAgent";
import { agentKey } from "./agentContinuity";
import type { DurableJob } from "../components/DurableJobs";
import { useDurableJobs } from "../components/DurableJobs";
const job: DurableJob = { id: "worker", label: "Mira", kind: "agent", state: "running", session_id: "child", parent_session: "parent", created_at: 1, started_at: 1, revision: 1, can_cancel: true, attachable: true, artifacts: [], error: {}, result: {} };
beforeEach(() => {
  const data = new Map<string, string>();
  vi.stubGlobal("localStorage", { getItem: (key: string) => data.get(key) ?? null, setItem: (key: string, value: string) => data.set(key, value) });
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); vi.useRealTimers(); });
it("restores the chosen specialist from fresh server data after remount", async () => {
  vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, json: async () => ({ jobs: [{ ...job, state: "succeeded" }] }) })));
  const first = renderHook(() => useSelectedAgent("parent"));
  act(() => first.result.current[1](job));
  expect(first.result.current[0]?.id).toBe("worker");
  first.unmount();
  const second = renderHook(() => useSelectedAgent("parent"));
  await waitFor(() => expect(second.result.current[0]?.state).toBe("succeeded"));
  act(() => second.result.current[1](null));
  second.unmount();
  const third = renderHook(() => useSelectedAgent("parent"));
  expect(third.result.current[0]).toBeNull();
});
it("never shows a previous chat's selected specialist while switching", async () => {
  vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, json: async () => ({ jobs: [] }) })));
  const hook = renderHook(({ chat }) => useSelectedAgent(chat), { initialProps: { chat: "parent" } });
  act(() => hook.result.current[1](job));
  hook.rerender({ chat: "other" });
  expect(hook.result.current[0]).toBeNull();
  hook.rerender({ chat: "parent" });
  await waitFor(() => expect(hook.result.current[0]?.id).toBe("worker"));
});
it("does not reopen a deleted or unrelated specialist", async () => {
  localStorage.setItem(agentKey("parent", "selected-agent"), JSON.stringify("missing"));
  vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, json: async () => ({ jobs: [job] }) })));
  const hook = renderHook(() => useSelectedAgent("parent"));
  await waitFor(() => expect(localStorage.getItem(agentKey("parent", "selected-agent"))).toBe("null"));
  expect(hook.result.current[0]).toBeNull();
});


it("keeps the saved view through a transient error and retries restoration", async () => {
  vi.useFakeTimers();
  localStorage.setItem(agentKey("parent", "selected-agent"), JSON.stringify(job.id));
  vi.stubGlobal("fetch", vi.fn().mockResolvedValueOnce({ ok: false }).mockResolvedValueOnce({ ok: true, json: async () => ({ jobs: [job] }) }));
  let hook!: ReturnType<typeof renderHook<ReturnType<typeof useSelectedAgent>, unknown>>;
  await act(async () => { hook = renderHook(() => useSelectedAgent("parent")); });
  expect(hook.result.current[0]).toBeNull();
  expect(localStorage.getItem(agentKey("parent", "selected-agent"))).toBe(JSON.stringify(job.id));
  await act(async () => { await vi.advanceTimersByTimeAsync(2100); });
  expect(hook.result.current[0]?.id).toBe(job.id);
});

it("does not erase the saved specialist when restoration receives incomplete JSON", async () => {
  vi.useFakeTimers();
  localStorage.setItem(agentKey("parent", "selected-agent"), JSON.stringify(job.id));
  vi.stubGlobal("fetch", vi.fn().mockResolvedValueOnce({ ok: true, json: async () => ({}) })
    .mockResolvedValue({ ok: true, json: async () => ({ jobs: [job] }) }));
  let hook!: ReturnType<typeof renderHook<ReturnType<typeof useSelectedAgent>, unknown>>;
  await act(async () => { hook = renderHook(() => useSelectedAgent("parent")); });
  expect(localStorage.getItem(agentKey("parent", "selected-agent"))).toBe(JSON.stringify(job.id));
  await act(async () => { await vi.advanceTimersByTimeAsync(2000); });
  expect(hook.result.current[0]?.id).toBe(job.id);
});

it("restoration and monitoring share one request and follow the same fresh state", async () => {
  localStorage.setItem(agentKey("parent", "selected-agent"), JSON.stringify(job.id));
  const fetch = vi.fn(async () => ({ ok: true, json: async () => ({ jobs: [job] }) }));
  vi.stubGlobal("fetch", fetch);
  const hook = renderHook(() => ({ selected: useSelectedAgent("parent"), monitor: useDurableJobs("parent") }));
  await waitFor(() => expect(hook.result.current.selected[0]?.id).toBe(job.id));
  expect(fetch).toHaveBeenCalledTimes(1);
  expect(hook.result.current.monitor.jobs[0]).toBe(hook.result.current.selected[0]);
});

it("keeps parent session identity in an agent link and ignores links from another session", async () => {
 window.history.replaceState({}, "", "/");
 vi.stubGlobal("fetch",vi.fn(async()=>({ok:true,json:async()=>({jobs:[job]})})));
 const hook=renderHook(({chat})=>useSelectedAgent(chat),{initialProps:{chat:"parent"}});
 act(()=>hook.result.current[1](job));
 expect(new URLSearchParams(window.location.search).get("session")).toBe("parent");
 expect(new URLSearchParams(window.location.search).get("agent")).toBe(job.id);
 hook.rerender({chat:"other"});
 expect(hook.result.current[0]).toBeNull();
 expect(localStorage.getItem(agentKey("other","selected-agent"))).toBeNull();
});
