import { useEffect, useRef, useState } from "react";
import { useAppStore } from "../../store";
import { Icon } from "../../lib/icons";
import { MachinePanel } from "../MachinePanel";
import { ComputerPanel } from "../ComputerPanel";
import { BrowserPanel } from "../BrowserPanel";

/**
 * Path B cockpit: screen, terminal, and box files stay visible together.
 */
export function ComputerStage({ onCollapse, visible, browser }: { onCollapse?: () => void; visible: boolean; browser: boolean }) {
  const [drawerPx, setDrawerPx] = useState(248);
  const [dragging, setDragging] = useState(false);
  const stageRef = useRef<HTMLDivElement>(null);
  const dragStart = useRef<{ y: number; h: number } | null>(null);
  const pinWorkspaceSurface = useAppStore((s) => s.pinWorkspaceSurface);
  const machineScreenConnected = useAppStore((s) => s.machineScreenConnected);
  const machinePaused = useAppStore((s) => s.machinePaused);
  const machineBlocked = useAppStore((s) => s.machineBlocked);
  const live = !machineBlocked && !machinePaused && machineScreenConnected;

  useEffect(() => {
    if (!dragging) return;
    const onMove = (event: PointerEvent) => {
      const origin = dragStart.current;
      const bounds = stageRef.current?.getBoundingClientRect();
      if (!origin || !bounds) return;
      const max = Math.round(bounds.height * 0.55);
      const next = origin.h + (origin.y - event.clientY);
      setDrawerPx(Math.min(max, Math.max(168, next)));
    };
    const onUp = () => {
      dragStart.current = null;
      setDragging(false);
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
    document.body.style.cursor = "row-resize";
    document.body.style.userSelect = "none";
    return () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
    };
  }, [dragging]);

  return (
    <div ref={stageRef} className="desktop-stage relative flex min-h-0 flex-1 flex-col overflow-hidden">
      <div className="desktop-stage-bar">
        <Icon.Desktop size={13} className="text-muted" />
        <span className="text-[0.7rem] font-medium tracking-tight text-ink">Computer</span>
        <span className={live ? "desktop-rec is-on" : "desktop-rec"}>
          {machineBlocked ? "UNAVAILABLE" : machinePaused ? "PAUSED" : browser ? "CHROMIUM" : live ? "LIVE" : "CONNECTING"}
        </span>
        <span className="hidden text-[0.62rem] text-faint sm:inline">Screen · terminal · files</span>
        <div className="ml-auto flex items-center gap-0.5">
          <button
            type="button"
            className="desktop-stage-link"
            onClick={() => pinWorkspaceSurface("artifacts")}
          >
            Artifacts
          </button>
          <button
            type="button"
            className="desktop-stage-link"
            onClick={() => pinWorkspaceSurface("monitor")}
          >
            Trace
          </button>
          {onCollapse ? (
            <button
              type="button"
              className="ka-icon-btn ml-0.5 h-7 w-7 text-muted hover:text-ink"
              onClick={onCollapse}
              aria-label="Collapse workspace"
              title="Collapse workspace"
            >
              ›
            </button>
          ) : null}
        </div>
      </div>

      <div className="relative flex min-h-0 flex-1 flex-col">
        {visible && (browser
          ? <BrowserPanel embeddedInWorkspace visible />
          : <ComputerPanel machine />)}
      </div>

      <div
        className="stage-term-grip z-10 shrink-0"
        role="separator"
        aria-label="Resize terminal"
        aria-orientation="horizontal"
        onPointerDown={(event) => {
          event.preventDefault();
          dragStart.current = { y: event.clientY, h: drawerPx };
          setDragging(true);
        }}
      >
        <span />
      </div>

      <div className="desktop-dock flex shrink-0 flex-col overflow-hidden" style={{ height: drawerPx }}>
        <MachinePanel compact filesOpen />
      </div>
    </div>
  );
}
