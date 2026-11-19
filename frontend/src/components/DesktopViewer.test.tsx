import "@testing-library/jest-dom/vitest";
import { act, cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ApiError, api } from "../api/client";
import { DesktopViewer } from "./DesktopViewer";

const connections = vi.hoisted(() => [] as Array<{
  emit: (name: string) => void;
  disconnect: ReturnType<typeof vi.fn>;
}>);
vi.mock("../api/client", async () => ({
  ...await vi.importActual<typeof import("../api/client")>("../api/client"), api: vi.fn(),
}));
vi.mock("@novnc/novnc", () => ({ default: class {
  listeners: Record<string, (e: Event) => void> = {};
  disconnect = vi.fn(() => this.emit("disconnect"));
  constructor() { connections.push(this); }
  emit(name: string) { this.listeners[name]?.(new Event(name)); }
  addEventListener(name: string, listener: (e: Event) => void) { this.listeners[name] = listener; }
} }));

beforeEach(() => {
  vi.useFakeTimers();
  connections.length = 0;
  vi.mocked(api).mockReset().mockResolvedValue({ novncReady: true, password: "test" });
});
afterEach(() => { cleanup(); vi.useRealTimers(); });

describe("DesktopViewer recovery", () => {
  it("keeps connecting through an initial activation race and then paints", async () => {
    vi.mocked(api).mockRejectedValueOnce(new ApiError("task machine is not active", 409));
    const status = vi.fn(), connected = vi.fn();
    render(<DesktopViewer sessionId="chat" ready controlling={false} onStatus={status} onConnected={connected} />);
    await act(async () => {});
    expect(status).toHaveBeenLastCalledWith("");
    expect(connections).toHaveLength(0);
    await act(async () => { await vi.advanceTimersByTimeAsync(800); });
    expect(connections).toHaveLength(1);
    act(() => connections[0].emit("connect"));
    expect(connected).toHaveBeenLastCalledWith(true);
  });

  it("shows persistent inactivity after the bounded activation grace period", async () => {
    vi.mocked(api).mockRejectedValue(new ApiError("task machine is not active", 409));
    const status = vi.fn();
    render(<DesktopViewer sessionId="chat" ready controlling={false} onStatus={status} />);
    await act(async () => { await vi.advanceTimersByTimeAsync(25_000); });
    expect(status).toHaveBeenLastCalledWith("task machine is not active");
  });

  it("does not hide blocked-computer errors during activation", async () => {
    vi.mocked(api).mockRejectedValue(new ApiError("Docker startup failed", 409));
    const status = vi.fn();
    render(<DesktopViewer sessionId="chat" ready controlling={false} onStatus={status} />);
    await act(async () => {});
    expect(status).toHaveBeenLastCalledWith("Docker startup failed");
  });
  it("times out a stalled handshake, ignores obsolete events, and reconnects", async () => {
    const status = vi.fn(), connected = vi.fn();
    render(<DesktopViewer sessionId="chat" ready controlling={false} onStatus={status} onConnected={connected} />);
    await act(async () => {});
    expect(connections).toHaveLength(1);
    await act(async () => { await vi.advanceTimersByTimeAsync(20_000); });
    expect(status).toHaveBeenLastCalledWith(expect.stringContaining("timed out"));
    expect(connected).toHaveBeenLastCalledWith(false);
    await act(async () => { await vi.advanceTimersByTimeAsync(800); });
    expect(api).toHaveBeenCalledTimes(2);
    expect(connections).toHaveLength(2);
    act(() => connections[0].emit("connect"));
    expect(connected).toHaveBeenLastCalledWith(false);
    act(() => connections[1].emit("connect"));
    expect(connected).toHaveBeenLastCalledWith(true);
    expect(status).toHaveBeenLastCalledWith("");
  });

  it("aborts hung metadata requests and suppresses late results", async () => {
    let resolve!: (value: unknown) => void;
    vi.mocked(api).mockImplementationOnce(() => new Promise(r => { resolve = r; }));
    const status = vi.fn();
    render(<DesktopViewer sessionId="chat" ready controlling={false} onStatus={status} />);
    const signal = vi.mocked(api).mock.calls[0][1]?.signal;
    await act(async () => { await vi.advanceTimersByTimeAsync(20_000); });
    expect(signal?.aborted).toBe(true);
    await act(async () => resolve({ novncReady: true }));
    expect(connections).toHaveLength(0);
    expect(status).toHaveBeenLastCalledWith(expect.stringContaining("timed out"));
  });

  it("clears readiness after a dropped screen and cancels retries on unmount", async () => {
    const connected = vi.fn();
    const view = render(<DesktopViewer sessionId="chat" ready controlling={false} onConnected={connected} />);
    await act(async () => {});
    act(() => connections[0].emit("connect"));
    act(() => connections[0].emit("disconnect"));
    expect(connected).toHaveBeenLastCalledWith(false);
    view.unmount();
    await act(async () => { await vi.advanceTimersByTimeAsync(60_000); });
    expect(api).toHaveBeenCalledTimes(1);
  });

  it("does not connect while paused", async () => {
    render(<DesktopViewer sessionId="chat" ready={false} controlling={false} />);
    await act(async () => { await vi.advanceTimersByTimeAsync(30_000); });
    expect(api).not.toHaveBeenCalled();
  });
});
