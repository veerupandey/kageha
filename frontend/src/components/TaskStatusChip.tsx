import { useEffect, useRef } from "react";
import { cn } from "../lib/cn";
import { BLOCKED_TASK_CONTROLS, REPAIRING_TASK_CONTROLS, taskControlLabel } from "../lib/taskStatus";
import { useAppStore } from "../store";
import { TaskStatusPanel } from "./TaskStatusPanel";

/**
 * Surfaces the adaptive loop's current control decision (loop/adaptive.py)
 * so a stuck/blocked run is visible without reading raw chat text or
 * task_state.json. In full (non-compact) mode, clicking opens an anchored
 * detail popover (TaskStatusPanel) — never destructive, a click here only
 * reveals state, it never changes agent behavior.
 */
export function TaskStatusChip({ compact = false }: { compact?: boolean } = {}) {
  const taskControl = useAppStore((s) => s.taskControl);
  const taskControlReason = useAppStore((s) => s.taskControlReason);
  const taskFailureCount = useAppStore((s) => s.taskFailureCount);
  const taskChronicTool = useAppStore((s) => s.taskChronicTool);
  const taskChronicCount = useAppStore((s) => s.taskChronicCount);
  const runStatus = useAppStore((s) => s.runStatus);
  const panelOpen = useAppStore((s) => s.taskStatusPanelOpen);
  const setPanelOpen = useAppStore((s) => s.setTaskStatusPanelOpen);
  const containerRef = useRef<HTMLDivElement>(null);
  // The mobile Tools sheet (ComposerTools) renders this compact, stacked among
  // several settings rows inside a Radix dialog — there is no safe anchor
  // point there for an absolutely-positioned popover, so compact mode is a
  // read-only status indicator only (like the other compact composer chips,
  // which have no secondary panel).
  const canOpenPanel = !compact;

  useEffect(() => {
    if (!panelOpen || !canOpenPanel) return;
    const onPointerDown = (e: MouseEvent) => {
      if (!containerRef.current?.contains(e.target as Node)) setPanelOpen(false);
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") setPanelOpen(false);
    };
    document.addEventListener("mousedown", onPointerDown);
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("mousedown", onPointerDown);
      document.removeEventListener("keydown", onKeyDown);
    };
  }, [panelOpen, canOpenPanel, setPanelOpen]);

  const setTaskChronicHalt = useAppStore((s) => s.setTaskChronicHalt);

  // Close automatically once the run settles back to a normal/idle state,
  // so a stale popover never lingers after the blocker resolves itself.
  // Also clear the chronic-tool banner then — the backend's cross-turn
  // memory (task_state.chronic_tool_halts) intentionally never resets, but
  // showing "Stuck on X" after the loop has visibly recovered would be a
  // stale/misleading display, not a useful durable warning.
  useEffect(() => {
    if (!taskControl || taskControl === "continue" || taskControl === "advance") {
      if (panelOpen) setPanelOpen(false);
      if (taskChronicTool) setTaskChronicHalt("", 0);
    }
  }, [panelOpen, taskControl, taskChronicTool, setPanelOpen, setTaskChronicHalt]);

  if (!taskControl || taskControl === "continue" || taskControl === "advance") {
    return null;
  }
  // A completed/idle run showing "stop_success" is not worth a persistent chip.
  if (taskControl === "stop_success" && runStatus !== "running") {
    return null;
  }

  const blocked = BLOCKED_TASK_CONTROLS.has(taskControl);
  const repairing = REPAIRING_TASK_CONTROLS.has(taskControl);
  // A chronic halt (same tool halted in a prior turn too — see
  // task_state.py chronic_tool_halts) is a materially different situation
  // from a first-time stumble: it means the agent is repeating the same
  // mistake across turns instead of learning from the last one. Without
  // this, both render as an identical "Repairing"/"Diagnosing" chip and the
  // user has no way to tell them apart short of reading task_state.json.
  const isChronic = taskChronicCount >= 2 && Boolean(taskChronicTool);
  const label = compact
    ? "Status"
    : isChronic
      ? `Stuck on ${taskChronicTool}`
      : taskControlLabel(taskControl);

  return (
    <div ref={containerRef} className="relative">
      <button
        type="button"
        className={cn(
          "rounded-md px-2 py-1 text-xs font-medium transition-colors",
          isChronic
            ? "bg-rose-500/20 text-rose-700 dark:text-rose-300"
            : blocked
              ? "bg-rose-500/15 text-rose-600 dark:text-rose-300"
              : repairing
                ? "bg-amber-500/15 text-amber-700 dark:text-amber-300"
                : "bg-accent-soft text-accent",
        )}
        title={
          isChronic
            ? `${taskChronicTool} has halted ${taskChronicCount} turns in a row — same class of failure repeating`
            : taskControlReason || "The agent's current recovery/control state"
        }
        aria-pressed={canOpenPanel ? panelOpen : undefined}
        aria-expanded={canOpenPanel ? panelOpen : undefined}
        onClick={canOpenPanel ? () => setPanelOpen(!panelOpen) : undefined}
      >
        <span className="inline-flex items-center gap-1.5">
          <span
            className={cn(
              "h-1.5 w-1.5 rounded-full",
              isChronic ? "bg-rose-600" : blocked ? "bg-rose-500" : repairing ? "bg-amber-500" : "bg-accent",
            )}
            aria-hidden="true"
          />
          {label}
          {isChronic && !compact ? (
            <span className="text-[0.65rem] text-faint">×{taskChronicCount}</span>
          ) : (
            taskFailureCount > 1 &&
            !compact && <span className="text-[0.65rem] text-faint">×{taskFailureCount}</span>
          )}
        </span>
      </button>
      {canOpenPanel && panelOpen && <TaskStatusPanel onClose={() => setPanelOpen(false)} />}
    </div>
  );
}
