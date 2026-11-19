import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { refreshWorkers, subscribeWorkers, workerSnapshot } from "./legacyWorkerConnection";

const cleanups: (() => void)[] = [];
beforeEach(() => {
  vi.useFakeTimers();
  vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, json: async () => ({ jobs: [{ id: "a", state: "running", result: {}, artifacts: [] }] }) })));
});
afterEach(() => { cleanups.splice(0).forEach(fn => fn()); vi.unstubAllGlobals(); vi.restoreAllMocks(); vi.useRealTimers(); });
async function flush() { await vi.advanceTimersByTimeAsync(0); }
function subscribe(session = "parent", receive = vi.fn()) {
  const stop = subscribeWorkers(session, vi.fn(), receive);
  cleanups.push(stop);
  return stop;
}

it("shares one request and timer across observers and releases the last observer", async () => {
  const a = subscribe(), b = subscribe(), c = subscribe();
  await flush();
  expect(fetch).toHaveBeenCalledTimes(1);
  expect(fetch).toHaveBeenCalledWith(expect.stringContaining("&view=ui"), expect.anything());
  await vi.advanceTimersByTimeAsync(2000);
  expect(fetch).toHaveBeenCalledTimes(2);
  a(); b();
  await vi.advanceTimersByTimeAsync(2000);
  expect(fetch).toHaveBeenCalledTimes(3);
  c();
  await vi.advanceTimersByTimeAsync(10_000);
  expect(fetch).toHaveBeenCalledTimes(3);
  expect(workerSnapshot("parent").jobs).toEqual([]);
});

it("delivers a background reply once per connection, regardless of observer count", async () => {
  vi.mocked(fetch).mockResolvedValue({ ok: true, json: async () => ({ jobs: [{ id: "a", state: "succeeded", artifacts: [], result: { job_reply_id: "reply1", message: "Done" } }] }) } as Response);
  const receive = vi.fn();
  subscribe("parent", receive); subscribe("parent", receive);
  await flush();
  await vi.advanceTimersByTimeAsync(10_000);
  expect(receive).toHaveBeenCalledTimes(1);
  expect(fetch).toHaveBeenCalledTimes(2);
});

it("isolates session snapshots and ignores a late response after switching chats", async () => {
  let resolve!: (value: Response) => void;
  vi.mocked(fetch).mockImplementationOnce(() => new Promise<Response>(r => { resolve = r; }));
  const stop = subscribe("old"); stop(); subscribe("new");
  resolve({ ok: true, json: async () => ({ jobs: [{ id: "private-old", state: "running" }] }) } as Response);
  await flush();
  expect(workerSnapshot("old").jobs).toEqual([]);
  expect(workerSnapshot("new").jobs[0].id).toBe("a");
});

it("bounds a hung request, backs off failures, and refreshes on reconnect", async () => {
  vi.spyOn(AbortSignal, "timeout").mockImplementation(ms => {
    const controller = new AbortController();
    setTimeout(() => controller.abort(new DOMException("Timed out", "TimeoutError")), ms);
    return controller.signal;
  });
  const hungRequest = (_url: unknown, options?: RequestInit) => new Promise<Response>((_resolve, reject) => {
    options?.signal?.addEventListener("abort", () => reject(new DOMException("Timed out", "TimeoutError")));
  });
  vi.mocked(fetch).mockImplementationOnce(hungRequest).mockImplementationOnce(hungRequest);
  subscribe();
  await vi.advanceTimersByTimeAsync(15_000);
  expect(fetch).toHaveBeenCalledTimes(2);
  expect(workerSnapshot("parent").error).toBe("");
  await vi.advanceTimersByTimeAsync(15_000);
  expect(workerSnapshot("parent").error).toBeTruthy();
  window.dispatchEvent(new Event("online"));
  await flush();
  expect(workerSnapshot("parent").error).toBe("");
  expect(fetch).toHaveBeenCalledTimes(3);
});

it("does not overlap requests when focus and online events arrive together", async () => {
  let resolve!: (value: Response) => void;
  vi.mocked(fetch).mockImplementationOnce(() => new Promise<Response>(r => { resolve = r; }));
  subscribe();
  window.dispatchEvent(new Event("focus")); window.dispatchEvent(new Event("online"));
  expect(fetch).toHaveBeenCalledTimes(1);
  resolve({ ok: true, json: async () => ({ jobs: [] }) } as Response);
  await flush();
  expect(workerSnapshot("parent").error).toBe("");
});

it("backs off repeated failures while preserving the last snapshot", async () => {
  subscribe(); await flush();
  vi.mocked(fetch).mockRejectedValue(new Error("Offline"));
  await vi.advanceTimersByTimeAsync(2000);
  expect(workerSnapshot("parent").jobs[0].id).toBe("a");
  await vi.advanceTimersByTimeAsync(2000);
  expect(fetch).toHaveBeenCalledTimes(3);
  await vi.advanceTimersByTimeAsync(3999);
  expect(fetch).toHaveBeenCalledTimes(3);
  await vi.advanceTimersByTimeAsync(1);
  expect(fetch).toHaveBeenCalledTimes(4);
});

it("refreshes new submissions immediately and deduplicates replayed announcements", async () => {
  vi.mocked(fetch).mockResolvedValue({ ok: true, json: async () => ({ jobs: [] }) } as Response);
  subscribe(); await flush();
  refreshWorkers("parent", ["new-job"]); await flush();
  expect(fetch).toHaveBeenCalledTimes(2);
  refreshWorkers("parent", ["new-job"]); await flush();
  expect(fetch).toHaveBeenCalledTimes(2);
});

it("preserves recorded work when a proxy returns a malformed successful response", async () => {
  subscribe(); await flush();
  vi.mocked(fetch).mockResolvedValue({ ok: true, json: async () => ({}) } as Response);
  await vi.advanceTimersByTimeAsync(2000);
  expect(workerSnapshot("parent").jobs[0]?.id).toBe("a");
  expect(workerSnapshot("parent").error).toBeTruthy();
});

it("one rejected reply delivery does not hide other replies or mark the connection offline", async () => {
  vi.mocked(fetch).mockResolvedValue({ ok: true, json: async () => ({ jobs: ["one", "two"].map(id => ({
    id, state: "succeeded", artifacts: [], result: { job_reply_id: id, message: id },
  })) }) } as Response);
  const receive = vi.fn().mockImplementationOnce(() => { throw new Error("Local receiver failed"); });
  subscribe("parent", receive); await flush();
  expect(receive).toHaveBeenCalledTimes(2);
  expect(workerSnapshot("parent").error).toBe("");
  await vi.advanceTimersByTimeAsync(10_000);
  expect(receive).toHaveBeenCalledTimes(3);
  expect(receive.mock.calls[2][1]).toBe("one");
});

it("coalesces announcements received during a request into one follow-up read", async () => {
  let resolve!: (value: Response) => void;
  vi.mocked(fetch).mockImplementationOnce(() => new Promise<Response>(r => { resolve = r; }));
  subscribe();
  refreshWorkers("parent", ["one"]); refreshWorkers("parent", ["two"]);
  resolve({ ok: true, json: async () => ({ jobs: [] }) } as Response);
  await vi.advanceTimersByTimeAsync(1);
  expect(fetch).toHaveBeenCalledTimes(2);
});

it("slows polling in hidden tabs and refreshes immediately on return", async () => {
  vi.spyOn(document, "hidden", "get").mockReturnValue(true);
  subscribe(); await flush();
  await vi.advanceTimersByTimeAsync(29_999);
  expect(fetch).toHaveBeenCalledTimes(1);
  await vi.advanceTimersByTimeAsync(1);
  expect(fetch).toHaveBeenCalledTimes(2);
  vi.spyOn(document, "hidden", "get").mockReturnValue(false);
  document.dispatchEvent(new Event("visibilitychange")); await flush();
  expect(fetch).toHaveBeenCalledTimes(3);
});

it("waits offline without polling and resumes on the online event", async () => {
  vi.spyOn(navigator, "onLine", "get").mockReturnValue(false);
  subscribe(); await flush();
  await vi.advanceTimersByTimeAsync(60_000);
  expect(fetch).not.toHaveBeenCalled();
  vi.spyOn(navigator, "onLine", "get").mockReturnValue(true);
  window.dispatchEvent(new Event("online")); await flush();
  expect(fetch).toHaveBeenCalledTimes(1);
});

it("reconnects immediately when offline interrupts a hanging request", async () => {
  let resolveOld!: (value: Response) => void;
  vi.mocked(fetch).mockImplementationOnce(() => new Promise<Response>(resolve => { resolveOld = resolve; }));
  subscribe();
  const oldSignal = vi.mocked(fetch).mock.calls[0][1]?.signal;
  vi.spyOn(navigator, "onLine", "get").mockReturnValue(false);
  window.dispatchEvent(new Event("offline"));
  expect(oldSignal?.aborted).toBe(true);
  vi.spyOn(navigator, "onLine", "get").mockReturnValue(true);
  window.dispatchEvent(new Event("online")); await flush();
  expect(fetch).toHaveBeenCalledTimes(2);
  expect(workerSnapshot("parent").jobs[0]?.id).toBe("a");
  resolveOld({ ok: true, json: async () => ({ jobs: [{ id: "stale", state: "running" }] }) } as Response);
  await flush();
  expect(workerSnapshot("parent").jobs[0]?.id).toBe("a");
  await vi.advanceTimersByTimeAsync(2000);
  expect(fetch).toHaveBeenCalledTimes(3);
});

it("new submissions do not bypass backoff when the backend request fails", async () => {
  let reject!: (reason: Error) => void;
  vi.mocked(fetch).mockImplementationOnce(() => new Promise<Response>((_resolve, fail) => { reject = fail; }));
  subscribe(); refreshWorkers("parent", ["new-job"]);
  reject(new Error("Backend unavailable"));
  await vi.advanceTimersByTimeAsync(1);
  expect(fetch).toHaveBeenCalledTimes(1);
  await vi.advanceTimersByTimeAsync(1999);
  expect(fetch).toHaveBeenCalledTimes(2);
});

it("old cleanup cannot tear down a replacement connection for the same chat", async () => {
  const stop = subscribe(); await flush(); stop();
  subscribe(); await flush(); stop();
  expect(workerSnapshot("parent").jobs[0]?.id).toBe("a");
  await vi.advanceTimersByTimeAsync(2000);
  expect(fetch).toHaveBeenCalledTimes(3);
});

it("announcements arriving during retry delay do not restart failed reads early", async () => {
  vi.mocked(fetch).mockRejectedValue(new Error("Backend unavailable"));
  subscribe(); await flush();
  refreshWorkers("parent", ["one"]); refreshWorkers("parent", ["two"]);
  await vi.advanceTimersByTimeAsync(1999);
  expect(fetch).toHaveBeenCalledTimes(1);
  await vi.advanceTimersByTimeAsync(1);
  expect(fetch).toHaveBeenCalledTimes(2);
});

it("does not deliver more replies after the last observer closes during delivery", async () => {
  vi.mocked(fetch).mockResolvedValue({ ok: true, json: async () => ({ jobs: ["one", "two"].map(id => ({
    id, state: "succeeded", artifacts: [], result: { job_reply_id: id, message: id },
  })) }) } as Response);
  let stop!: () => void;
  const receive = vi.fn(() => stop());
  stop = subscribe("parent", receive);
  await flush();
  expect(receive).toHaveBeenCalledTimes(1);
  expect(workerSnapshot("parent").jobs).toEqual([]);
});

it("an obsolete response cannot overwrite a completed job after reconnect", async () => {
  let resolveOld!: (value: Response) => void;
  vi.mocked(fetch).mockImplementationOnce(() => new Promise<Response>(resolve => { resolveOld = resolve; }));
  subscribe();
  window.dispatchEvent(new Event("offline"));
  vi.mocked(fetch).mockResolvedValue({ ok: true, json: async () => ({ jobs: [{ id: "a", state: "succeeded", revision: 3, result: {} }] }) } as Response);
  window.dispatchEvent(new Event("online")); await flush();
  resolveOld({ ok: true, json: async () => ({ jobs: [{ id: "a", state: "running", revision: 2 }] }) } as Response);
  await flush();
  expect(workerSnapshot("parent").jobs[0]).toMatchObject({ state: "succeeded", revision: 3 });
});
