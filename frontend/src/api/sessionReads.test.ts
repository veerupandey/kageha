import { afterEach, expect, it, vi } from "vitest";
vi.mock("./client", () => ({ api: vi.fn() }));
import { api } from "./client";
import { readSession } from "./sessionReads";
afterEach(() => vi.clearAllMocks());

it("shares prefetch/open reads while cancellation remains consumer scoped", async () => {
  let finish!: (value: unknown) => void;
  let sharedSignal!: AbortSignal;
  vi.mocked(api).mockImplementationOnce((_path, options) => {
    sharedSignal = options!.signal!;
    return new Promise(resolve => { finish = resolve; });
  });
  const hover = new AbortController();
  const prefetch = readSession("shared-read", hover.signal);
  const rejected = expect(prefetch).rejects.toThrow("aborted");
  const opening = readSession("shared-read");
  hover.abort();
  await rejected;
  expect(sharedSignal.aborted).toBe(false);
  finish({ session_id: "shared-read" });
  await expect(opening).resolves.toEqual({ session_id: "shared-read" });
  expect(api).toHaveBeenCalledTimes(1);
});

it("aborts obsolete requests when their last reader leaves and permits retry", async () => {
  let signal!: AbortSignal;
  vi.mocked(api).mockImplementationOnce((_path, options) => {
    signal = options!.signal!;
    return new Promise((_resolve, reject) => signal.addEventListener("abort", () => reject(new Error("aborted"))));
  }).mockResolvedValueOnce({ session_id: "retry-read" });
  const navigation = new AbortController();
  const first = readSession("retry-read", navigation.signal);
  const rejected = expect(first).rejects.toThrow("aborted");
  navigation.abort();
  await rejected;
  expect(signal.aborted).toBe(true);
  await expect(readSession("retry-read")).resolves.toEqual({ session_id: "retry-read" });
  expect(api).toHaveBeenCalledTimes(2);
});
