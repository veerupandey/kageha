import { useState, useMemo, useRef, useEffect, type ReactNode } from "react";
import * as Popover from "@radix-ui/react-popover";
import type { Project } from "../../api/types";
import { useAppStore } from "../../store";
import { Icon } from "../../lib/icons";
import { cn } from "../../lib/cn";

export interface ProjectSelectorPopoverProps {
  currentProjectId?: string | null;
  currentProject?: Project | null;
  onSelectProject: (project: Project | null) => void;
  trigger?: ReactNode;
  align?: "start" | "center" | "end";
  side?: "top" | "right" | "bottom" | "left";
  contentZIndex?: number;
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
}

const PROJECT_COLORS: Record<string, string> = {
  blue: "#3b82f6",
  emerald: "#10b981",
  purple: "#a855f7",
  amber: "#f59e0b",
  rose: "#f43f5e",
  cyan: "#06b6d4",
  indigo: "#6366f1",
};

export function getProjectColor(color?: string): string {
  if (!color) return "#3b82f6";
  return PROJECT_COLORS[color] || color;
}

export function ProjectSelectorPopover({
  currentProjectId,
  currentProject: currentProjectProp,
  onSelectProject,
  trigger,
  align = "start",
  side = "bottom",
  contentZIndex = 50,
  open: controlledOpen,
  onOpenChange: controlledOnOpenChange,
}: ProjectSelectorPopoverProps) {
  const [internalOpen, setInternalOpen] = useState(false);
  const isOpen = controlledOpen !== undefined ? controlledOpen : internalOpen;
  const setIsOpen = controlledOnOpenChange || setInternalOpen;

  const rawProjects = useAppStore((s) => s.projects);
  const projects = useMemo(() => (Array.isArray(rawProjects) ? rawProjects : []), [rawProjects]);
  const loadProjects = useAppStore((s) => s.loadProjects);
  const createProject = useAppStore((s) => s.createProject);

  const [query, setQuery] = useState("");
  const [creating, setCreating] = useState(false);
  const [selectedIndex, setSelectedIndex] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (projects.length === 0 && typeof loadProjects === "function") {
      void loadProjects();
    }
  }, [projects.length, loadProjects]);

  useEffect(() => {
    if (isOpen) {
      if (typeof loadProjects === "function") void loadProjects();
      setQuery("");
      setSelectedIndex(0);
      setTimeout(() => inputRef.current?.focus(), 50);
    }
  }, [isOpen, loadProjects]);

  const filteredProjects = useMemo(() => {
    if (!query.trim()) return projects;
    const q = query.toLowerCase().trim();
    return projects.filter(
      (p) =>
        p.name.toLowerCase().includes(q) ||
        p.id.toLowerCase().includes(q) ||
        (p.description && p.description.toLowerCase().includes(q)) ||
        (p.tags && p.tags.some((t) => t.toLowerCase().includes(q)))
    );
  }, [projects, query]);

  const currentProject = useMemo(() => {
    if (currentProjectProp !== undefined && currentProjectProp !== null) {
      return currentProjectProp;
    }
    const targetId = currentProjectId;
    return projects.find((p) => p.id === targetId) || null;
  }, [projects, currentProjectId, currentProjectProp]);

  const effectiveProjectId = currentProjectId || currentProject?.id || null;

  const hasExactMatch = useMemo(() => {
    const q = query.trim().toLowerCase();
    return projects.some((p) => p.name.toLowerCase() === q || p.id.toLowerCase() === q);
  }, [projects, query]);

  const handleSelect = (project: Project | null) => {
    onSelectProject(project);
    setIsOpen(false);
  };

  const handleCreateNew = async () => {
    const trimmed = query.trim();
    if (!trimmed || creating) return;
    try {
      setCreating(true);
      const newProj = await createProject(trimmed);
      handleSelect(newProj);
    } catch (err) {
      console.error("Failed to create project", err);
    } finally {
      setCreating(false);
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setSelectedIndex((prev) => Math.min(prev + 1, filteredProjects.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setSelectedIndex((prev) => Math.max(prev - 1, 0));
    } else if (e.key === "Enter") {
      e.preventDefault();
      if (filteredProjects[selectedIndex]) {
        handleSelect(filteredProjects[selectedIndex]);
      } else if (query.trim() && !hasExactMatch) {
        void handleCreateNew();
      }
    } else if (e.key === "Escape") {
      setIsOpen(false);
    }
  };

  return (
    <Popover.Root open={isOpen} onOpenChange={setIsOpen}>
      <Popover.Trigger asChild>
        {trigger ? (
          trigger
        ) : (
          <button
            type="button"
            className={cn(
              "inline-flex items-center gap-1.5 rounded-full px-2.5 py-1 text-xs font-medium transition-colors border",
              currentProject
                ? "bg-[var(--color-surface)] border-[var(--color-line)] text-ink hover:border-accent"
                : "bg-surface/50 border-dashed border-[var(--color-line)] text-faint hover:text-ink hover:border-accent"
            )}
            title={currentProject ? `Project: ${currentProject.name}` : "Assign to project"}
          >
            {currentProject ? (
              <>
                <span
                  className="h-2 w-2 rounded-full shrink-0"
                  style={{ backgroundColor: getProjectColor(currentProject.color) }}
                />
                <span className="max-w-[120px] truncate">{currentProject.name}</span>
              </>
            ) : (
              <>
                <Icon.Projects size={12} className="shrink-0 text-faint" />
                <span>+ Project</span>
              </>
            )}
          </button>
        )}
      </Popover.Trigger>

      <Popover.Portal>
        <Popover.Content
          align={align}
          side={side}
          sideOffset={6}
          style={{ zIndex: contentZIndex }}
          className="z-50 w-72 rounded-xl border border-[var(--color-line)] bg-[var(--color-surface)] p-2 shadow-xl animate-in fade-in-0 zoom-in-95"
        >
          {/* Search Input */}
          <div className="relative mb-2">
            <Icon.Search
              size={14}
              className="absolute left-2.5 top-1/2 -translate-y-1/2 text-faint pointer-events-none"
            />
            <input
              ref={inputRef}
              type="text"
              placeholder="Search or create project..."
              className="w-full rounded-lg border border-[var(--color-line)] bg-surface/50 pl-8 pr-3 py-1.5 text-xs text-ink placeholder:text-faint outline-none focus:border-accent focus:bg-[var(--color-surface)] transition-all"
              value={query}
              onChange={(e) => {
                setQuery(e.target.value);
                setSelectedIndex(0);
              }}
              onKeyDown={handleKeyDown}
            />
          </div>

          {/* Project List */}
          <div className="max-h-56 overflow-y-auto space-y-0.5 pr-0.5">
            {filteredProjects.map((p, idx) => {
              const isSelected = p.id === effectiveProjectId;
              const isHighlighted = idx === selectedIndex;
              return (
                <button
                  key={p.id}
                  type="button"
                  onClick={() => handleSelect(p)}
                  className={cn(
                    "w-full flex items-center justify-between rounded-lg px-2 py-1.5 text-xs transition-colors text-left",
                    isHighlighted ? "bg-accent/15 text-accent" : "hover:bg-line/40 text-ink",
                    isSelected && !isHighlighted && "bg-surface font-semibold"
                  )}
                >
                  <div className="flex items-center gap-2 min-w-0">
                    <span
                      className="h-2.5 w-2.5 rounded-full shrink-0"
                      style={{ backgroundColor: getProjectColor(p.color) }}
                    />
                    <span className="truncate">{p.name}</span>
                  </div>
                  <div className="flex items-center gap-1.5 shrink-0">
                    {p.session_count !== undefined && p.session_count > 0 && (
                      <span className="text-[10px] text-faint">{p.session_count} chats</span>
                    )}
                    {isSelected && <Icon.Check size={13} className="text-accent" />}
                  </div>
                </button>
              );
            })}

            {filteredProjects.length === 0 && !query.trim() && (
              <div className="py-4 text-center text-xs text-faint">No projects yet</div>
            )}

            {/* Quick Create Option */}
            {query.trim() && !hasExactMatch && (
              <button
                type="button"
                onClick={handleCreateNew}
                disabled={creating}
                className="w-full mt-1 flex items-center gap-2 rounded-lg border border-dashed border-accent/40 bg-accent/5 px-2 py-1.5 text-xs text-accent hover:bg-accent/10 transition-colors text-left"
              >
                <Icon.Plus size={14} className="shrink-0" />
                <span className="truncate">
                  {creating ? "Creating..." : `Create project "${query.trim()}"`}
                </span>
              </button>
            )}
          </div>

          {/* Unassign / Remove project button if one is currently assigned */}
          {effectiveProjectId && (
            <div className="mt-2 pt-2 border-t border-[var(--color-line)]">
              <button
                type="button"
                onClick={() => handleSelect(null)}
                className="w-full flex items-center justify-center gap-1.5 rounded-lg py-1 text-xs text-muted hover:text-danger hover:bg-danger/10 transition-colors"
              >
                <span>Remove from project</span>
              </button>
            </div>
          )}
        </Popover.Content>
      </Popover.Portal>
    </Popover.Root>
  );
}
