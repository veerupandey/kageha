import { useState } from "react";
import { useAppStore } from "../../store";
import { Icon } from "../../lib/icons";
import { cn } from "../../lib/cn";
import { SidebarSection } from "./SidebarSection";
import { ProjectSelectorPopover, getProjectColor } from "../shared/ProjectSelectorPopover";
import type { SessionSummary } from "../../api/types";
import { activeThreads } from "./threadPaging";

export function ProjectsList({ sessions }: { sessions: SessionSummary[] }) {
  const [collapsed, setCollapsed] = useState(() => {
    try {
      return localStorage.getItem("kageha.sidebar.projects") === "1";
    } catch {
      return false;
    }
  });

  const projects = useAppStore((s) => s.projects);
  const selectedProjectId = useAppStore((s) => s.selectedProjectId);
  const setSelectedProject = useAppStore((s) => s.setSelectedProject);
  const visibleThreads = activeThreads(sessions);

  const toggle = () => {
    const next = !collapsed;
    setCollapsed(next);
    try {
      localStorage.setItem("kageha.sidebar.projects", next ? "1" : "0");
    } catch {
      /* private storage */
    }
  };

  return (
    <div className="px-3 py-1">
      <SidebarSection
        label="Projects"
        count={projects.length}
        collapsed={collapsed}
        onToggle={toggle}
        action={
          <ProjectSelectorPopover
            onSelectProject={(proj) => {
              if (proj) setSelectedProject(proj.id);
            }}
            trigger={
              <button
                type="button"
                className="ka-icon-btn h-5 w-5 text-faint hover:text-ink"
                title="Create or assign project"
              >
                <Icon.Plus size={13} />
              </button>
            }
          />
        }
      >
        <ul className="space-y-0.5 mt-1">
          {/* All Threads row */}
          <li>
            <button
              type="button"
              onClick={() => setSelectedProject(null)}
              className={cn(
                "w-full flex items-center justify-between rounded-md px-2 py-1.5 text-xs transition-colors text-left",
                selectedProjectId === null
                  ? "bg-accent/15 text-accent font-medium"
                  : "text-muted hover:bg-line/40 hover:text-ink"
              )}
            >
              <div className="flex items-center gap-2 min-w-0">
                <Icon.Projects size={13} className="shrink-0 text-faint" />
                <span className="truncate">All Threads</span>
              </div>
            </button>
          </li>

          {/* Project Items */}
          {projects.map((p) => {
            const isSelected = selectedProjectId === p.id;
            const count = visibleThreads.filter(
              (s) => s.project_id === p.id || s.project?.id === p.id
            ).length;
            return (
              <li key={p.id}>
                <button
                  type="button"
                  onClick={() => setSelectedProject(isSelected ? null : p.id)}
                  className={cn(
                    "w-full flex items-center justify-between rounded-md px-2 py-1.5 text-xs transition-colors text-left group",
                    isSelected
                      ? "bg-accent/15 text-accent font-medium"
                      : "text-muted hover:bg-line/40 hover:text-ink"
                  )}
                  title={p.description || p.name}
                >
                  <div className="flex items-center gap-2 min-w-0">
                    <span
                      className="h-2 w-2 rounded-full shrink-0"
                      style={{ backgroundColor: getProjectColor(p.color) }}
                    />
                    <span className="truncate">{p.name}</span>
                  </div>
                  {count > 0 && (
                    <span className="text-[10px] tabular-nums text-faint group-hover:text-muted shrink-0">
                      {count}
                    </span>
                  )}
                </button>
              </li>
            );
          })}
        </ul>
      </SidebarSection>
    </div>
  );
}
