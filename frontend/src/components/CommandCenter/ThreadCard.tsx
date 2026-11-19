import type { SessionSummary } from "../../api/types";
import { Icon } from "../../lib/icons";
import { getProjectColor } from "../shared/ProjectSelectorPopover";

interface ThreadCardProps {
  session: SessionSummary;
  onClick: () => void;
  onMouseEnter?: () => void;
}

function timeAgo(dateStr?: string | number | null): string {
  if (!dateStr && dateStr !== 0) return "";
  let ts: number;
  if (typeof dateStr === "number") {
    ts = dateStr > 1e12 ? dateStr : dateStr * 1000;
  } else {
    const parsed = Number(dateStr);
    if (!isNaN(parsed) && parsed > 0) {
      ts = parsed > 1e12 ? parsed : parsed * 1000;
    } else {
      ts = new Date(dateStr).getTime();
    }
  }
  if (!ts || isNaN(ts)) return "";
  const diff = Date.now() - ts;
  if (diff < 0) return "now";
  const mins = Math.floor(diff / 60000);
  if (mins < 1) return "now";
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.floor(hours / 24);
  if (days < 30) return `${days}d ago`;
  const months = Math.floor(days / 30);
  if (months < 12) return `${months}mo ago`;
  return `${Math.floor(months / 12)}y ago`;
}

export function ThreadCard({ session, onClick, onMouseEnter }: ThreadCardProps) {
  const title =
    (session.title && session.title.trim()) || session.session_id.slice(0, 8);
  const projectName = typeof session.project === "object" ? session.project?.name : session.project;
  const projectColor = typeof session.project === "object" && session.project?.color
    ? getProjectColor(session.project.color)
    : undefined;

  return (
    <button
      type="button"
      className="ka-card group relative flex w-full flex-col gap-2 rounded-xl border border-line/60 bg-surface/65 p-3.5 text-left backdrop-blur-xs transition-all duration-200 hover:-translate-y-0.5 hover:border-accent/40 hover:bg-surface/90 hover:shadow-md active:translate-y-0"
      onClick={onClick}
      onMouseEnter={onMouseEnter}
    >
      <div className="flex items-start justify-between gap-2">
        <p className="min-w-0 flex-1 truncate text-sm font-medium text-ink group-hover:text-accent transition-colors">
          {session.pinned && (
            <span className="mr-1.5 inline-flex text-accent" aria-hidden="true"><Icon.Pin size={13} /></span>
          )}
          {title}
        </p>
        <span className="shrink-0 text-[0.68rem] text-faint">
          {timeAgo(session.updated_at)}
        </span>
      </div>
      {session.objective && (
        <p className="line-clamp-2 text-xs leading-relaxed text-muted">{session.objective}</p>
      )}
      {projectName && (
        <div className="mt-0.5 flex items-center gap-1.5">
          <span
            className="inline-flex items-center gap-1.5 rounded-md px-2 py-0.5 text-[0.65rem] font-medium transition-colors"
            style={{
              backgroundColor: projectColor ? `${projectColor}18` : "color-mix(in srgb, var(--color-line) 50%, transparent)",
              color: projectColor || "var(--color-muted)",
            }}
          >
            <span
              className="h-1.5 w-1.5 rounded-full"
              style={{ backgroundColor: projectColor || "var(--color-accent)" }}
            />
            <span className="truncate max-w-[140px]">{projectName}</span>
          </span>
        </div>
      )}
    </button>
  );
}
