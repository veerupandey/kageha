import { afterEach, describe, expect, it, vi } from "vitest";
vi.mock("./client", () => ({api:vi.fn(),ApiError:class extends Error {status=404;}}));
import { api } from "./client";
import { recoverReceipt } from "./receipts";
describe("read-only receipt recovery", () => {
  afterEach(() => {vi.resetAllMocks();vi.useRealTimers();});
  it("waits for the exact request to bind without posting", async () => {
    vi.useFakeTimers();
    vi.mocked(api).mockResolvedValueOnce({receipt:{state:"received",turn_id:""}}).mockResolvedValueOnce({receipt:{state:"accepted",turn_id:"actual"}});
    const received=vi.fn();
    const result=recoverReceipt("s","request",{stopped:()=>false,onReceived:received});
    await vi.advanceTimersByTimeAsync(1000);
    expect((await result)?.receipt.turn_id).toBe("actual");
    expect(received).toHaveBeenCalledOnce();
    expect(vi.mocked(api).mock.calls.every(([url,opts])=>url==="/api/sessions/s/requests/request" && !opts?.method)).toBe(true);
  });
  it("never polls after Stop", async () => {
    expect(await recoverReceipt("s","r",{stopped:()=>true})).toBeNull();
    expect(api).not.toHaveBeenCalled();
  });
  it("discards an in-flight receipt after Stop without firing callbacks", async () => {
    let stopped = false;
    let resolve!: (value: unknown) => void;
    vi.mocked(api).mockImplementation(() => new Promise(r => { resolve = r; }));
    const received = vi.fn();
    const result = recoverReceipt("s", "r", { stopped: () => stopped, onReceived: received });
    stopped = true;
    resolve({ receipt: { state: "received", turn_id: "" } });
    expect(await result).toBeNull();
    expect(received).not.toHaveBeenCalled();
    expect(api).toHaveBeenCalledOnce();
  });
  it("bounds delayed admission", async () => {
    vi.useFakeTimers();
    vi.mocked(api).mockResolvedValue({receipt:{state:"received",turn_id:""}});
    const result=recoverReceipt("s","r",{stopped:()=>false,waitMs:2000});
    await vi.advanceTimersByTimeAsync(2000);
    expect((await result)?.receipt.state).toBe("received");
    expect(api).toHaveBeenCalledTimes(2);
  });
});
