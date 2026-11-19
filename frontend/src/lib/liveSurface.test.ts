import { describe, expect, it } from "vitest";
import {
  isMachineDesktopApp,
  machineLiveStateFromPayload,
  resolveLiveSurface,
} from "./liveSurface";

describe("resolveLiveSurface", () => {
  it("keeps Path B on Screen instead of stealing to macOS", () => {
    expect(resolveLiveSurface("browser", "computer", true)).toBe("browser");
    expect(resolveLiveSurface("machine", "computer", true)).toBe("computer");
    expect(resolveLiveSurface("computer", "computer", true)).toBe("computer");
  });

  it("still opens macOS when the box is off", () => {
    expect(resolveLiveSurface("browser", "computer", false)).toBe("computer");
  });

  it("keeps Screen visible when bash reveals Terminal", () => {
    expect(resolveLiveSurface("browser", "machine", true)).toBe("computer");
  });

  it("maps isolated-desktop start onto Screen even before the flag flips", () => {
    expect(resolveLiveSurface("computer", "machine", false)).toBe("computer");
  });
});

describe("machine live payload", () => {
  it("shows the real startup error instead of generic blocked copy", () => {
    expect(machineLiveStateFromPayload({ blocked: true,
      message: "Task machine could not start.", error: "Docker did not respond within 12s." })).toMatchObject({
      machineBlocked: true, machineDetail: "Docker did not respond within 12s.",
    });
  });
  it("captures noVNC so the Live viewer can connect immediately", () => {
    expect(machineLiveStateFromPayload({
      novncUrl: "http://127.0.0.1:9/vnc.html?autoconnect=1",
      message: "Linux computer ready",
    })).toEqual({
      machineActive: true,
      machineBlocked: false,
      machineDetail: "Linux computer ready",
      machineNovncUrl: "http://127.0.0.1:9/vnc.html?autoconnect=1",
    });
  });

  it("treats novncReady without a host URL as screen-up", () => {
    expect(machineLiveStateFromPayload({
      novncReady: true,
      adopted: true,
      phase: "screen",
      message: "Reconnected to this computer.",
    })).toEqual({
      machineActive: true,
      machineBlocked: false,
      machineDetail: "Reconnected to this computer.",
      machineNovncUrl: "ready",
      machineAdopted: true,
      machineBootPhase: "screen",
    });
  });

  it("captures browserReady so the HUD can show the gap", () => {
    expect(machineLiveStateFromPayload({
      novncReady: true,
      phase: "screen",
      browserReady: false,
      message: "Screen is live — browser is still starting.",
    })).toMatchObject({
      machineNovncUrl: "ready",
      machineBootPhase: "screen",
      machineBrowserReady: false,
    });
    expect(machineLiveStateFromPayload({
      novncReady: true,
      phase: "browser",
      browserReady: true,
    }).machineBrowserReady).toBe(true);
  });

  it("does not claim Live when the start was skipped", () => {
    expect(machineLiveStateFromPayload({ skipped: true, message: "disabled" })).toEqual({
      machineActive: false,
      machineBlocked: false,
      machineDetail: "disabled",
    });
  });

  it("recognizes Machine Desktop frames", () => {
    expect(isMachineDesktopApp("Machine Desktop")).toBe(true);
    expect(isMachineDesktopApp("Finder")).toBe(false);
  });
});
