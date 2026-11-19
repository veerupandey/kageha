import type { LiveSurface } from "../store/types";

/** Isolated Linux desktop frames from computer tools. */
export const MACHINE_DESKTOP_APP = "Machine Desktop";

export function isMachineDesktopApp(app: string | undefined | null): boolean {
  return app === MACHINE_DESKTOP_APP || app === "Linux Desktop";
}

/** A machine activity can reveal the full Linux display. */
export function resolveLiveSurface(
  current: LiveSurface,
  incoming: LiveSurface,
  machineActive: boolean,
): LiveSurface {
  if (incoming === "machine") {
    return "computer";
  }
  if (machineActive && incoming === "computer") {
    return current === "browser" ? "browser" : "computer";
  }
  return incoming;
}

export function novncUrlFromPayload(payload: Record<string, unknown> | undefined): string {
  if (!payload) return "";
  return String(payload.novncUrl || payload.novnc_url || "").trim();
}

export function machineLiveStateFromPayload(payload: Record<string, unknown> | undefined): {
  machineActive: boolean;
  machineBlocked: boolean;
  machineDetail: string;
  machineNovncUrl?: string;
  machineAdopted?: boolean;
  machineBootPhase?: string;
  machineBrowserReady?: boolean;
} {
  const skipped = payload?.skipped === true;
  const novnc = novncUrlFromPayload(payload);
  const novncReady = payload?.novncReady === true || payload?.novnc_ready === true;
  const phase = String(payload?.phase || "").trim();
  const hasBrowser =
    payload != null && ("browserReady" in payload || "browser_ready" in payload);
  return {
    machineActive: !skipped,
    machineBlocked: payload?.blocked === true,
    machineDetail: String((payload?.blocked === true && payload?.error) || payload?.message || payload?.error || ""),
    ...(novnc || novncReady ? { machineNovncUrl: novnc || "ready" } : {}),
    ...(payload?.adopted === true ? { machineAdopted: true } : {}),
    ...(phase ? { machineBootPhase: phase } : {}),
    ...(hasBrowser
      ? {
          machineBrowserReady:
            payload.browserReady === true || payload.browser_ready === true,
        }
      : {}),
  };
}
