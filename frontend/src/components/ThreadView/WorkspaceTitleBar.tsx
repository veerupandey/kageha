import { useShallow } from "zustand/react/shallow";
import { Icon } from "../../lib/icons";
import { cn } from "../../lib/cn";
import { workspaceActivity } from "../../lib/workspaceActivity";
import { useAppStore } from "../../store";
import { isQuestionApproval } from "../../store/helpers";
import type { LiveSurface, WorkspaceSurface } from "../../store/types";

export interface WorkspaceTab { id: WorkspaceSurface; label: string }
const shortcuts: Partial<Record<WorkspaceSurface, string>> = { live: "⌘1", artifacts: "⌘2", monitor: "⌘3" };
const tabIcons = { live: Icon.Globe, artifacts: Icon.File, monitor: Icon.Activity, desktops: Icon.Desktop, review: Icon.Check };

export function WorkspaceTitleBar({ surface, tabs, artifactCount, machineActive, liveSurface, expanded,
  split, spotlight, onSplit, onSelect, onLiveSurface, onZoom, onCollapse }: {
  surface: WorkspaceSurface;
  tabs: WorkspaceTab[];
  artifactCount: number;
  machineActive: boolean;
  liveSurface: LiveSurface;
  expanded: boolean;
  split?: boolean;
  spotlight?: boolean;
  onSplit?: () => void;
  onSelect: (surface: WorkspaceSurface) => void;
  onLiveSurface: (surface: LiveSurface) => void;
  onZoom: () => void;
  onCollapse?: () => void;
}) {
  // Keep high-frequency transcript subscriptions out of the live viewer parent.
  const runStatus = useAppStore(s => s.runStatus);
  const waitingForAnswer = useAppStore(s => isQuestionApproval(s.pendingApproval));
  const activity = useAppStore(
    useShallow(s => workspaceActivity({
      messages: s.messages,
      runStatus: s.runStatus,
      statusLabel: s.statusLabel,
      machineShellRunning: s.machineShellRunning,
      machineShellLine: s.machineShellLine,
      machineActive,
      liveSurface,
      waitingForAnswer,
    }))
  );
  // The transcript owns the current agent status while chat is visible. Keep
  // the capsule when there is no live transcript status or chat is covered by
  // the expanded workspace, and for approval and separate terminal activity.
  const feedShowsStatus = useAppStore(s => {
    const live = s.messages.findLast(message => message.role === "user" || (message.role === "assistant" && message.streaming));
    return Boolean(live?.role === "assistant" && live.streaming &&
      (Boolean(live.statusLabel?.trim()) || Boolean(live.blocks?.some(block =>
        (block.type === "reasoning" && block.streaming) ||
        ((block.type === "tool" || block.type === "shell") && ["running", "pending", "requested"].includes(block.card.status || ""))))));
  });
  const showActivity = expanded || activity.kind === "Terminal" || runStatus !== "running" || !feedShowsStatus;

  return <header className="ka-workspace-tabs workspace-titlebar">
    <div className="workspace-surface-tabs" role="tablist" aria-label="Workspace surfaces"
      onKeyDown={event => {
        const buttons = [...event.currentTarget.querySelectorAll<HTMLButtonElement>('[role="tab"]')];
        const current = buttons.indexOf(document.activeElement as HTMLButtonElement);
        const next = event.key === "Home" ? 0 : event.key === "End" ? tabs.length - 1
          : event.key === "ArrowRight" ? (current + 1) % tabs.length
          : event.key === "ArrowLeft" ? (current - 1 + tabs.length) % tabs.length : -1;
        if (current < 0 || next < 0) return;
        event.preventDefault(); onSelect(tabs[next].id); buttons[next].focus();
      }}>
      {tabs.map(item => {
        const Glyph = item.id === "live" && item.label !== "Browser" ? Icon.Desktop : tabIcons[item.id];
        const isLiveBrowser = item.id === "live" && item.label === "Browser" && activity.running && liveSurface === "browser";
        return <button key={item.id} type="button" role="tab" aria-selected={surface === item.id} tabIndex={surface === item.id ? 0 : -1}
          title={`${item.label}${shortcuts[item.id] ? ` (${shortcuts[item.id]})` : ""}`}
          onClick={() => onSelect(item.id)}>
          <Glyph size={13} aria-hidden="true" />
          <span>{item.label}</span>
          {isLiveBrowser && <span className="workspace-live-dot" aria-label="Browser session active" />}
          {item.id === "artifacts" && artifactCount > 0 && <span className="workspace-file-count workspace-deliverable-badge" title={`${artifactCount} deliverable${artifactCount === 1 ? "" : "s"}`}>{artifactCount > 99 ? "99+" : artifactCount}</span>}
        </button>;
      })}
    </div>
    {showActivity && <button type="button" className={cn("workspace-activity", activity.running && "is-running")}
      onClick={() => onSelect("monitor")} data-running={activity.running} data-waiting={runStatus === "waiting_approval"}
      title={`Activity: ${activity.kind} · ${activity.summary}`}
      aria-label={`Activity status: ${activity.kind} ${activity.summary}`}>
      <span className="workspace-activity-dot" aria-hidden="true" />
      <strong>{activity.kind}</strong><span className="truncate">{activity.summary}</span>
    </button>}
    <div className="workspace-title-actions">
      {onSplit && (spotlight ? <button type="button" className="workspace-back-to-split"
        onClick={onSplit} title="Back to split (Esc)">
        <Icon.Back size={14} aria-hidden="true" /> Back to split
      </button> : <button type="button" className="workspace-layout-toggle" aria-pressed={split}
        aria-label="Show Browser and Files together" title="Show Browser and Files together" onClick={onSplit}>
        <Icon.Canvas size={15} aria-hidden="true" />
      </button>)}
      {surface === "live" && <div className="workspace-live-switch" role="group" aria-label="Switch live mode">
        {(["browser", "computer"] as const).map(item => {
          const Glyph = item === "browser" ? Icon.Globe : Icon.Desktop;
          const label = item === "browser" ? "Browser" : "Computer";
          return <button key={item} type="button" aria-pressed={liveSurface === item}
            aria-label={`Switch to ${label}`} title={`Switch to ${label}`} onClick={() => onLiveSurface(item)}>
            <Glyph size={13} aria-hidden="true" /><span>{label}</span>
          </button>;
        })}
      </div>}
      <button type="button" className="workspace-layout-toggle" onClick={onZoom}
        aria-label={expanded ? "Restore workspace" : "Expand workspace"}
        title={expanded ? "Restore workspace (Esc)" : "Expand workspace"}>
        <Icon.Expand size={14} aria-hidden="true" />
      </button>
      <button type="button" className="ka-workspace-close workspace-collapse" disabled={!onCollapse}
        onClick={onCollapse} aria-label="Close workspace" title="Close workspace">
        <Icon.ChevronRight size={16} aria-hidden="true" />
      </button>
    </div>
  </header>;
}
