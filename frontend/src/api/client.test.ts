import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { api, ApiTimeoutError } from "./client";
const deadlines: AbortController[] = [];
const timeout = () => new DOMException("signal timed out", "TimeoutError");
const waitForAbort = (_path: unknown, options?: RequestInit) => new Promise<Response>((_resolve, reject) => {
  options?.signal?.addEventListener("abort", () => reject(options.signal?.reason), { once: true });
});
beforeEach(() => {
  deadlines.length = 0;
  vi.spyOn(AbortSignal, "timeout").mockImplementation(() => {
    const controller = new AbortController();
    deadlines.push(controller);
    return controller.signal;
  });
});
afterEach(() => { vi.restoreAllMocks(); vi.unstubAllGlobals(); });
it("recovers a timed-out read with one fresh deadline", async () => {
  const fetch = vi.fn().mockImplementationOnce(waitForAbort)
    .mockResolvedValueOnce({ ok: true, json: async () => ({ status: "running" }) });
  vi.stubGlobal("fetch", fetch);
  const result = api("/api/sessions/chat/state");
  deadlines[0].abort(timeout());
  await expect(result).resolves.toEqual({ status: "running" });
  expect(fetch).toHaveBeenCalledTimes(2);
  expect(deadlines).toHaveLength(2);
});
it("bounds retries and replaces raw timeout text with a chat-list explanation", async () => {
  const fetch = vi.fn(waitForAbort);
  vi.stubGlobal("fetch", fetch);
  const result = api("/api/sessions?limit=50");
  const rejection = expect(result).rejects.toThrow("Refreshing the chat list took too long");
  deadlines[0].abort(timeout());
  await vi.waitFor(() => expect(fetch).toHaveBeenCalledTimes(2));
  deadlines[1].abort(timeout());
  await rejection;
  expect(fetch).toHaveBeenCalledTimes(2);
});
it("does not repeat a submission when its caller deadline expires", async () => {
  const fetch = vi.fn(waitForAbort);
  vi.stubGlobal("fetch", fetch);
  const controller = new AbortController();
  const result = api("/api/chat/send", { method: "POST", signal: controller.signal, body: "{}" });
  const rejection = expect(result).rejects.toBeInstanceOf(ApiTimeoutError);
  controller.abort(timeout());
  await rejection;
  expect(fetch).toHaveBeenCalledTimes(1);
  expect(deadlines).toHaveLength(0);
});
it("identifies specialist refresh timeouts without changing task status", async () => {
  vi.stubGlobal("fetch", vi.fn(waitForAbort));
  const result = api("/api/jobs?session_id=chat&view=ui");
  const rejection = expect(result).rejects.toThrow("Refreshing specialist updates took too long. This does not confirm that your task failed.");
  deadlines[0].abort(timeout());
  await vi.waitFor(() => expect(fetch).toHaveBeenCalledTimes(2));
  deadlines[1].abort(timeout());
  await rejection;
});
it("respects caller cancellation without retrying a read", async () => {
  const fetch = vi.fn(waitForAbort);
  vi.stubGlobal("fetch", fetch);
  const controller = new AbortController();
  const result = api("/api/sessions/chat", { signal: controller.signal });
  const reason = new DOMException("View changed", "AbortError");
  const rejection = expect(result).rejects.toBe(reason);
  controller.abort(reason);
  await rejection;
  expect(fetch).toHaveBeenCalledTimes(1);
});
it("does not extend an explicit read deadline", async () => {
  const fetch = vi.fn(waitForAbort);
  vi.stubGlobal("fetch", fetch);
  const controller = new AbortController();
  const result = api("/api/sessions/chat", { signal: controller.signal });
  const rejection = expect(result).rejects.toBeInstanceOf(ApiTimeoutError);
  controller.abort(timeout());
  await rejection;
  expect(fetch).toHaveBeenCalledTimes(1);
});
it("recovers when the response headers arrive but the JSON body stalls", async () => {
  let signal!: AbortSignal;
  const body = vi.fn(() => new Promise((_resolve, reject) => {
    signal.addEventListener("abort", () => reject(signal.reason), { once: true });
  }));
  const fetch = vi.fn().mockImplementationOnce((_path, options) => {
    signal = options.signal;
    return Promise.resolve({ ok: true, json: body });
  }).mockResolvedValueOnce({ ok: true, json: async () => ({ status: "running" }) });
  vi.stubGlobal("fetch", fetch);
  const result = api("/api/sessions/chat");
  await vi.waitFor(() => expect(body).toHaveBeenCalledTimes(1));
  deadlines[0].abort(timeout());
  await expect(result).resolves.toEqual({ status: "running" });
  expect(fetch).toHaveBeenCalledTimes(2);
});
