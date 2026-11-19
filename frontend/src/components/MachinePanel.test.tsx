import "@testing-library/jest-dom/vitest";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, render, waitFor } from "@testing-library/react";
import { api } from "../api/client";
import { useAppStore } from "../store";
import { MachinePanel } from "./MachinePanel";

vi.mock("../api/client", () => ({ api: vi.fn() }));

describe("MachinePanel status polling", () => {
  beforeEach(() => {
    vi.mocked(api).mockReset();
    useAppStore.setState({
      sessionId: "machine-1", machineActive: true, machineBlocked: false,
      machineDetail: "", machineNovncUrl: "stale", runStatus: "running",
    });
  });

  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    useAppStore.setState({ sessionId: null, machineActive: false, machineBlocked: false, machineDetail: "", machineNovncUrl: "" });
  });

  it("surfaces a persisted startup failure and clears the stale screen URL", async () => {
    vi.mocked(api).mockResolvedValue({ blocked: true, error: "Docker daemon unavailable", runtime: null });
    render(<MachinePanel compact />);

    await waitFor(() => expect(useAppStore.getState().machineBlocked).toBe(true));
    expect(useAppStore.getState().machineDetail).toBe("Docker daemon unavailable");
    expect(useAppStore.getState().machineNovncUrl).toBe("");
  });

  it.each([false, true])("clears the blocked state when the machine recovers (saved blocked: %s)", async (blocked) => {
    useAppStore.setState({ machineBlocked: true, machineDetail: "old failure" });
    vi.mocked(api).mockResolvedValue({ blocked, runtime: { status: "running", novncReady: true } });
    render(<MachinePanel compact />);

    await waitFor(() => expect(useAppStore.getState().machineBlocked).toBe(false));
    expect(useAppStore.getState().machineDetail).toBe("");
    expect(useAppStore.getState().machineNovncUrl).toBe("ready");
  });

  it("keeps at most one status request in flight", async () => {
    vi.useFakeTimers();
    vi.mocked(api).mockImplementation(() => new Promise(() => {}));
    render(<MachinePanel compact />);
    await act(async () => { await vi.advanceTimersByTimeAsync(5000); });
    expect(api).toHaveBeenCalledTimes(1);
  });

  it("shows status request failures and recovers on the next successful poll", async () => {
    vi.mocked(api).mockRejectedValueOnce(new Error("existing machine belongs to a different session or workspace"));
    vi.mocked(api).mockResolvedValue({ runtime: { status: "running", novncReady: true } });
    render(<MachinePanel compact />);
    await waitFor(() => expect(useAppStore.getState().machineBlocked).toBe(true));
    expect(useAppStore.getState().machineDetail).toContain("different session or workspace");
    expect(useAppStore.getState().machineNovncUrl).toBe("");
    await waitFor(() => expect(useAppStore.getState().machineBlocked).toBe(false));
    expect(useAppStore.getState().machineNovncUrl).toBe("ready");
  });
});
