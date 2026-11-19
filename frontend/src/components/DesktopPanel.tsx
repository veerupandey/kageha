import { useEffect, useRef, useState } from "react";
import { cn } from "../lib/cn";
import {
  desktopBootCopy,
  desktopBootSteps,
  desktopSessionNarration,
} from "../lib/desktopBoot";
import { isMachineDesktopApp } from "../lib/liveSurface";
import { useAppStore } from "../store";
import { DesktopViewer } from "./DesktopViewer";
import { StageDriver, StageIdentity, StageWindow } from "./shared/Stage";

export function DesktopPanel() {
  const sessionId = useAppStore((s) => s.sessionId);
  const runStatus = useAppStore((s) => s.runStatus);
  const machineActive = useAppStore((s) => s.machineActive);
  const machineBlocked = useAppStore((s) => s.machineBlocked);
  const machinePaused = useAppStore((s) => s.machinePaused);
  const machineDetail = useAppStore((s) => s.machineDetail);
  const machineNovncUrl = useAppStore((s) => s.machineNovncUrl);
  const machineAdopted = useAppStore((s) => s.machineAdopted);
  const machineBootPhase = useAppStore((s) => s.machineBootPhase);
  const machineBrowserReady = useAppStore((s) => s.machineBrowserReady);
  const workspaceSurface = useAppStore((s) => s.workspaceSurface);
  const desktopLeaseOwner = useAppStore((s) => s.desktopLeaseOwner);
  const takeDesktopControl = useAppStore((s) => s.takeDesktopControl);
  const releaseDesktopControl = useAppStore((s) => s.releaseDesktopControl);
  const setSurfaceControlling = useAppStore((s) => s.setSurfaceControlling);
  const pinWorkspaceSurface = useAppStore((s) => s.pinWorkspaceSurface);
  const lastAction = useAppStore((s) => {
    let frame;
    let card;
    for (let i = s.messages.length - 1; i >= 0; i--) {
      const m = s.messages[i];
      if (!frame && m.computerFrames?.length) {
        frame = m.computerFrames.findLast((f) => isMachineDesktopApp(f.app));
      }
      if (!card && m.toolCards?.length) {
        card = m.toolCards.at(-1);
      }
      if (frame && card) break;
    }
    return desktopSessionNarration({
      action: frame?.action,
      caption: frame?.caption,
      toolName: card?.name,
      toolPreview: card?.argsPreview || card?.resultPreview,
      shellLine: s.machineShellLine,
      running: s.runStatus === "running",
    });
  });
  const screenReady = Boolean(machineNovncUrl);
  const controlling = desktopLeaseOwner === "user";
  const liveVisible = workspaceSurface === "live";
  const running = runStatus === "running";
  const [painted, setPainted] = useState(false);
  const [connectionError, setConnectionError] = useState("");
  const connecting = machineActive && !machineBlocked && !painted;

  useEffect(() => {
    useAppStore.setState((state) => state.sessionId === sessionId
      ? { machineScreenConnected: painted && !machineBlocked && !machinePaused }
      : {});
    return () => {
      useAppStore.setState((state) => state.sessionId === sessionId
        ? { machineScreenConnected: false } : {});
    };
  }, [sessionId, painted, machineBlocked, machinePaused]);

  // Browser-native fullscreen for the Linux desktop. noVNC's scaleViewport
  // (see DesktopViewer/desktopBoot) auto-rescales the canvas when the host
  // element resizes, so entering fullscreen on the panel is enough — no
  // manual RFB resize needed. Escape is handled natively by the browser and
  // surfaces here through the fullscreenchange event.
  const panelRef = useRef<HTMLElement | null>(null);
  const [isFullscreen, setIsFullscreen] = useState(false);
  const fullscreenSupported =
    typeof document !== "undefined" && Boolean(document.fullscreenEnabled);

  useEffect(() => {
    const onChange = () => setIsFullscreen(document.fullscreenElement === panelRef.current);
    document.addEventListener("fullscreenchange", onChange);
    return () => document.removeEventListener("fullscreenchange", onChange);
  }, []);

  const toggleFullscreen = () => {
    const node = panelRef.current;
    if (!node) return;
    if (document.fullscreenElement) {
      void document.exitFullscreen().catch(() => {});
    } else {
      void node.requestFullscreen?.().catch(() => {});
    }
  };

  useEffect(() => {
    setPainted(false);
    setConnectionError("");
  }, [sessionId, machineBlocked]);

  useEffect(() => {
    setSurfaceControlling(controlling);
  }, [controlling, setSurfaceControlling]);

  const prevRun = useRef(runStatus);
  useEffect(() => {
    const started = prevRun.current !== "running" && runStatus === "running";
    prevRun.current = runStatus;
    if (started) void releaseDesktopControl();
  }, [runStatus, releaseDesktopControl]);

  useEffect(() => {
    if (!controlling || !sessionId) return;
    const timer = window.setInterval(() => void takeDesktopControl({ heartbeat: true }), 25_000);
    return () => window.clearInterval(timer);
  }, [controlling, sessionId, takeDesktopControl]);

  const setDriver = (who: "agent" | "you") => {
    if (who === "you") {
      pinWorkspaceSurface("live");
      void takeDesktopControl();
      return;
    }
    void releaseDesktopControl();
  };

  const bootInput = {
    phase: machineBootPhase,
    detail: connectionError || machineDetail,
    adopted: machineAdopted,
    screenReady,
    connecting,
    browserReady: machineBrowserReady,
    blocked: machineBlocked || Boolean(connectionError),
    paused: machinePaused,
  };
  const boot = desktopBootCopy(bootInput);
  const steps = desktopBootSteps(bootInput);
  const showBoot = !sessionId || !machineActive || machineBlocked || !painted;

  return (
    <section
      ref={panelRef}
      className={cn("desktop-panel absolute inset-0", isFullscreen && "is-fullscreen")}
      aria-label="Computer screen"
      data-who={controlling ? "you" : "agent"}
    >
      {sessionId && machineActive && !machineBlocked ? (
        <div className="stage-hud-wrap">
          <div className="stage-hud desktop-hud">
            <StageDriver
              controlling={controlling}
              onAgent={() => setDriver("agent")}
              onYou={() => setDriver("you")}
              label="Who controls this computer"
            />
            <StageIdentity live={!controlling && (running || Boolean(lastAction))}>
              <span className="stage-host">
                {controlling ? "You have the mouse" : running ? "Agent driving" : "Watching"}
                {lastAction ? ` · ${lastAction}` : ""}
              </span>
            </StageIdentity>
            <div className="stage-actions">
              <button
                type="button"
                className={cn("desktop-cta", controlling && "is-release")}
                onClick={() => (controlling ? void releaseDesktopControl() : void takeDesktopControl())}
              >
                {controlling ? "Release" : "Take control"}
              </button>
              {fullscreenSupported ? (
                <button
                  type="button"
                  className={cn("stage-icon-btn", isFullscreen && "is-on")}
                  onClick={toggleFullscreen}
                  aria-pressed={isFullscreen}
                  aria-label={isFullscreen ? "Exit full screen" : "Full screen desktop"}
                  title={isFullscreen ? "Exit full screen (Esc)" : "Full screen desktop"}
                >
                  <span aria-hidden="true">{isFullscreen ? "\u2715" : "\u26F6"}</span>
                </button>
              ) : null}
            </div>
          </div>
        </div>
      ) : null}

      <StageWindow overlay className="desktop-well">
        <div className="desktop-bezel" data-who={controlling ? "you" : "agent"}>
          <div className="desktop-titlebar">
            <span className="desktop-traffic" aria-hidden="true">
              <i />
              <i />
              <i />
            </span>
            <span className="desktop-title">Linux desktop</span>
            <span className={cn("desktop-live", !machineBlocked && painted && "is-on")}>
              {bootInput.blocked ? "UNAVAILABLE" : machinePaused ? "PAUSED" : painted ? running ? "LIVE" : "READY" : "CONNECTING"}
            </span>
          </div>
          <div className="desktop-screen">
            {sessionId && machineActive && !machineBlocked ? (
              <DesktopViewer
                sessionId={sessionId}
                controlling={controlling}
                visible={liveVisible}
                ready={machineActive && !machinePaused}
                onConnected={setPainted}
                onStatus={setConnectionError}
              />
            ) : null}
            {showBoot ? (
              <div className="desktop-boot" role="status">
                <div className="desktop-boot-mark" />
                <p className="stage-boot-title">{boot.title}</p>
                <p className="stage-boot-sub">{boot.sub}</p>
                {!bootInput.blocked && <ol className="desktop-boot-steps">
                  {steps.map((step) => (
                    <li key={step.id} data-state={step.state}>
                      <span />
                      {step.label}
                    </li>
                  ))}
                </ol>}
              </div>
            ) : null}
            <div className="stage-glow" data-who={controlling ? "you" : "agent"} />
            {!machineBlocked && painted && !machineBrowserReady && !controlling && !lastAction ? (
              <div
                data-testid="desktop-browser-boot"
                className="stage-chip desktop-action"
              >
                <span className="h-1.5 w-1.5 shrink-0 animate-pulse rounded-full bg-sky-400" />
                <span className="truncate text-[0.65rem]">Screen is live · browser starting</span>
              </div>
            ) : null}
            {!machineBlocked && !controlling && lastAction ? (
              <div
                data-testid="desktop-action"
                className={cn("stage-chip desktop-action", running && "is-live")}
              >
                <span className="h-1.5 w-1.5 shrink-0 animate-pulse rounded-full bg-emerald-400" />
                <span className="truncate text-[0.65rem]">{lastAction}</span>
              </div>
            ) : null}
            {!machineBlocked && machineAdopted && painted ? (
              <p className="desktop-reconnect">Reconnected to this computer</p>
            ) : null}
          </div>
        </div>
      </StageWindow>
    </section>
  );
}
