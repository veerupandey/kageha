import { cn } from "../lib/cn";
import { BLOCKED_TASK_CONTROLS, failureKindLabel, taskControlLabel } from "../lib/taskStatus";
import { useAppStore } from "../store";
import type { TaskControlState } from "../store/types";

function controlColor(control: TaskControlState): string {
  if (BLOCKED_TASK_CONTROLS.has(control)) {
    return "bg-[var(--color-danger-soft)] text-[var(--color-danger)]";
  }
  if (control === "retry" || control === "switch_tool" || control === "replan_stage" || control === "repair") {
    return "bg-[var(--color-warn-soft)] text-[var(--color-warn)]";
  }
  if (control === "stop_success") return "bg-accent/15 text-accent";
  return "bg-line/50 text-muted";
}

/**
 * Floating detail card anchored under TaskStatusChip: the adaptive loop's
 * current control decision, why it chose it, and the last classified
 * failure (if any) with the harness's own suggested remedy — so "the
 * harness blocked it" is always inspectable, not just a vague chat sentence.
 */
export function TaskStatusPanel({ onClose }: { onClose: () => void }) {
  const taskControl = useAppStore((s) => s.taskControl);
  const taskControlReason = useAppStore((s) => s.taskControlReason);
  const taskFailureKind = useAppStore((s) => s.taskFailureKind);
  const taskFailureChange = useAppStore((s) => s.taskFailureChange);
  const taskFailureCount = useAppStore((s) => s.taskFailureCount);
  const taskStageId = useAppStore((s) => s.taskStageId);
  const taskStageStatus = useAppStore((s) => s.taskStageStatus);
  const taskChronicTool = useAppStore((s) => s.taskChronicTool);
  const taskChronicCount = useAppStore((s) => s.taskChronicCount);
  const isChronic = taskChronicCount >= 2 && Boolean(taskChronicTool);

  return (
    <div
      className="absolute bottom-full left-0 z-50 mb-2 w-80 rounded-lg border border-line bg-surface shadow-lg"
      role="dialog"
      aria-label="Task status detail"
    >
      <div className="flex items-center justify-between border-b border-line px-3 py-2">
        <h3 className="text-xs font-semibold text-ink">Task status</h3>
        <button
          type="button"
          className="rounded-md px-1.5 py-0.5 text-xs text-muted hover:text-ink hover:bg-line/50"
          onClick={onClose}
        >
          ✕
        </button>
      </div>

      <div className="max-h-80 overflow-y-auto px-3 py-2.5 space-y-3">
        {isChronic && (
          <div className="rounded-md border border-[var(--color-danger)]/30 bg-[var(--color-danger-soft)] px-2.5 py-2">
            <label className="block text-[0.6rem] uppercase tracking-wide text-[var(--color-danger)] mb-1">
              Repeating across turns
            </label>
            <p className="text-xs text-ink">
              <span className="font-mono">{taskChronicTool}</span> has halted{" "}
              {taskChronicCount} turns in a row this session — the same class
              of failure, not a one-off. Consider a different approach or
              telling the agent explicitly what to change.
            </p>
          </div>
        )}
        {!taskControl ? (
          <p className="py-2 text-center text-xs text-muted">
            No control state yet this turn.
          </p>
        ) : (
          <>
            <div>
              <label className="block text-[0.6rem] uppercase tracking-wide text-muted mb-1">
                Control decision
              </label>
              <div className="flex items-center gap-2">
                <span
                  className={cn(
                    "rounded px-1.5 py-0.5 text-[0.65rem] font-medium",
                    controlColor(taskControl),
                  )}
                >
                  {taskControlLabel(taskControl)}
                </span>
                <span className="font-mono text-[0.58rem] text-faint">{taskControl}</span>
              </div>
              {taskControlReason && (
                <p className="mt-1.5 text-xs text-ink">{taskControlReason}</p>
              )}
            </div>

            {taskFailureKind && (
              <div>
                <label className="block text-[0.6rem] uppercase tracking-wide text-muted mb-1">
                  Last failure{taskFailureCount > 1 ? ` (×${taskFailureCount} this turn)` : ""}
                </label>
                <p className="text-xs text-ink">{failureKindLabel(taskFailureKind)}</p>
                {taskFailureChange && (
                  <p className="mt-1 text-xs text-muted">
                    Required change: {taskFailureChange}
                  </p>
                )}
              </div>
            )}

            {taskStageId && (
              <div>
                <label className="block text-[0.6rem] uppercase tracking-wide text-muted mb-1">
                  Current stage
                </label>
                <p className="text-xs text-ink">
                  <span className="font-mono">{taskStageId}</span>
                  {taskStageStatus ? ` — ${taskStageStatus}` : ""}
                </p>
              </div>
            )}
          </>
        )}
      </div>

      <div className="border-t border-line px-3 py-1.5">
        <p className="text-[0.58rem] text-faint">
          Reflects the agent's adaptive control loop.
        </p>
      </div>
    </div>
  );
}
