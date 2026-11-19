import { useEffect, useRef, useState } from "react";
import { cn } from "../../lib/cn";
import { Icon } from "../../lib/icons";
import { useAppStore } from "../../store";
import { Pill } from "../shared/Pill";
import { SessionStateIndicator } from "../SessionStateIndicator";
import { WorkspaceChip } from "../WorkspaceChip";
import { useSessionTruth } from "../../lib/sessionTruth";
import { ProjectSelectorPopover } from "../shared/ProjectSelectorPopover";

export type ArtifactFilter = "all" | "images" | "video" | "code" | "webpages" | "documents";

interface ThreadHeaderProps {
  artifactFilter: ArtifactFilter;
  onFilterChange: (filter: ArtifactFilter) => void;
  artifactCounts: Record<ArtifactFilter, number>;
  onOpenSidebar?: () => void;
}

export function ThreadHeader({
  artifactFilter,
  onFilterChange,
  artifactCounts,
  onOpenSidebar,
}: ThreadHeaderProps) {
  const sessionTitle = useAppStore((s) => s.sessionTitle);
  const sessionId = useAppStore((s) => s.sessionId);
  const prefs = useAppStore((s) => s.prefs);
  const computerApi = useAppStore((s) => s.capabilities.computerApi);
  const browserApi = useAppStore((s) => s.capabilities.browserApi);
  const selectedMachine = useAppStore((s) => s.machineActive);
  const truth = useSessionTruth(sessionId);
  const machineActive = truth.status === "current" && truth.data?.turn?.actual_target
    ? truth.data.turn.actual_target === "machine" : selectedMachine;
  const remoteActive = useAppStore((s) => s.remoteActive);
  const remoteHostName = useAppStore((s) => s.remoteHostName);
  const remoteHealth = useAppStore((s) => s.remoteHealth);
  const remoteRiskTier = useAppStore((s) => s.remoteRiskTier);
  const hasComputerActivity = useAppStore((s) => s.messages.some((message) => (message.computerFrames || []).length > 0));
  const workspaceSurface = useAppStore((s) => s.workspaceSurface);
  const liveSurface = useAppStore((s) => s.liveSurface);
  const setPrefs = useAppStore((s) => s.setPrefs);
  const setComputerPanelMode = useAppStore((s) => s.setComputerPanelMode);
  const setBrowserPanelMode = useAppStore((s) => s.setBrowserPanelMode);
  const pinLiveSurface = useAppStore((s) => s.pinLiveSurface);
  const pinWorkspaceSurface = useAppStore((s) => s.pinWorkspaceSurface);
  const renameSession = useAppStore((s) => s.renameSession);
  const showToast = useAppStore((s) => s.showToast);
  const sessions = useAppStore((s) => s.sessions);
  const sessionProject = useAppStore((s) => s.sessionProject);
  const projects = useAppStore((s) => s.projects);
  const setSessionProject = useAppStore((s) => s.setSessionProject);
  const activeSession = sessions?.find((s) => s.session_id === sessionId);
  const currentProject =
    sessionProject ||
    activeSession?.project ||
    (activeSession?.project_id
      ? projects.find((p) => p.id === activeSession.project_id)
      : null);
  const [editing, setEditing] = useState(false);
  const [titleDraft, setTitleDraft] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);

  const paneOpen = !prefs.rightPanelCollapsed;
  const surfaceActive = (surface: typeof workspaceSurface) =>
    paneOpen && workspaceSurface === surface;

  useEffect(() => {
    if (editing) {
      inputRef.current?.focus();
      inputRef.current?.select();
    }
  }, [editing]);

  const title = (sessionTitle && sessionTitle.trim()) || sessionId?.slice(0, 8) || "Thread";

  const commitTitle = async () => {
    setEditing(false);
    const next = titleDraft.trim();
    if (!sessionId || next === (sessionTitle || "")) return;
    try {
      await renameSession(next, sessionId);
    } catch (err) {
      showToast(`Rename failed: ${err instanceof Error ? err.message : err}`);
    }
  };

  const filters: ArtifactFilter[] = ["all", "images", "video", "code", "webpages", "documents"]
    .filter((f) => f === "all" || artifactCounts[f as ArtifactFilter] > 0) as ArtifactFilter[];
  const openPane = () => setPrefs({ rightPanelCollapsed: false });

  return (
    <header role="banner" className="ka-thread-header relative z-20 flex shrink-0 items-center gap-3 border-b border-[var(--color-line)] px-4 py-2.5">
      {onOpenSidebar ? (
        <button
          type="button"
          className="ka-icon-btn ka-thread-menu-btn h-8 w-8 shrink-0 text-muted lg:hidden"
          aria-label="Open menu"
          onClick={onOpenSidebar}
        >
          <Icon.Menu size={18} />
        </button>
      ) : null}
      <div className="flex min-w-0 flex-1 items-center gap-2.5">
        {editing ? (
          <input
            ref={inputRef}
            type="text"
            className="min-w-0 flex-1 rounded-md border border-[var(--color-line)] bg-[var(--color-surface)] px-2 py-1 text-sm text-ink outline-none focus:border-[var(--color-accent)]"
            value={titleDraft}
            onChange={(e) => setTitleDraft(e.target.value)}
            onBlur={() => void commitTitle()}
            onKeyDown={(e) => {
              if (e.key === "Enter") void commitTitle();
              if (e.key === "Escape") setEditing(false);
            }}
          />
        ) : (
          <button
            type="button"
            className="min-w-0 truncate text-left text-sm font-semibold text-ink hover:text-accent rounded px-1.5 py-0.5 -mx-1.5 hover:bg-surface/60 transition-colors"
            disabled={!sessionId}
            onClick={() => {
              setTitleDraft(sessionTitle || "");
              setEditing(true);
            }}
            title="Click to rename"
          >
            {title}
          </button>
        )}
        <SessionStateIndicator sessionId={sessionId} />
        <WorkspaceChip key={sessionId} sessionId={sessionId} />
        {sessionId && (
          <ProjectSelectorPopover
            currentProjectId={currentProject?.id || activeSession?.project_id || null}
            currentProject={currentProject}
            onSelectProject={(proj) => {
              void setSessionProject(sessionId, proj ? proj.id : null);
            }}
          />
        )}
        {remoteActive && (
          <span
            className={
              "inline-flex shrink-0 items-center gap-1 rounded-md px-1.5 py-0.5 text-[0.7rem] font-medium " +
              (remoteRiskTier === "production"
                ? "bg-rose-500/15 text-rose-600 dark:text-rose-300"
                : "bg-accent-soft text-accent")
            }
            title={
              "Selected remote host " +
              (remoteHostName || "(default)") +
              (remoteHealth ? " · " + remoteHealth : "") +
              (remoteRiskTier === "production" ? " · production" : "")
            }
          >
            <span
              className={
                "h-1.5 w-1.5 rounded-full " +
                (remoteHealth === "blocked"
                  ? "bg-rose-500"
                  : remoteHealth === "connected"
                    ? "bg-emerald-500"
                    : "bg-amber-500")
              }
              aria-hidden="true"
            />
            <span className="max-w-[9rem] truncate">
              {remoteHostName || "remote"}
            </span>
            {remoteRiskTier === "production" ? (
              <span className="text-[0.55rem] font-semibold uppercase tracking-wide">prod</span>
            ) : null}
          </span>
        )}
      </div>

      <button
        type="button"
        className="ka-phone-workspace-trigger"
        onClick={openPane}
        onMouseEnter={() => { void import("../AgentCanvas"); }}
        aria-label="Open workspace"
      >
        <Icon.Canvas size={18} />
        <span>Workspace</span>
      </button>
      {!paneOpen ? (
        <nav className="ka-thread-header-nav flex items-center gap-1" role="tablist" aria-label="Workspace surfaces">
          {machineActive ? (
            <Pill
              active={surfaceActive("live")}
              onClick={() => {
                openPane();
                pinLiveSurface("browser");
                pinWorkspaceSurface("live");
              }}
              className="text-xs"
            >
              <span className="mr-1.5 inline-block h-1.5 w-1.5 rounded-full bg-muted" />
              Linux computer
            </Pill>
          ) : liveSurface === "browser" ? (
            <>
              <Pill
                active={surfaceActive("live")}
                onClick={() => {
                  openPane();
                  setBrowserPanelMode("expanded");
                  pinLiveSurface("browser");
                }}
                className="text-xs"
              >
                <span className="mr-1.5 inline-block h-1.5 w-1.5 rounded-full bg-sky-500" />
                Browser
              </Pill>
              {(computerApi || hasComputerActivity) && (
                <Pill
                  active={false}
                  onClick={() => {
                    openPane();
                    setBrowserPanelMode("expanded");
                    pinLiveSurface("computer");
                    setComputerPanelMode("expanded");
                  }}
                  className="text-xs"
                >
                  Desktop
                </Pill>
              )}
            </>
          ) : (
            <>
              {(computerApi || hasComputerActivity) && (
                <Pill
                  active={paneOpen && workspaceSurface === "live" && liveSurface === "computer"}
                  onClick={() => {
                    openPane();
                    pinLiveSurface("computer");
                    setComputerPanelMode("expanded");
                  }}
                  className="text-xs"
                >
                  <span className={cn("inline-block h-1.5 w-1.5 rounded-full", hasComputerActivity ? "bg-teal-500" : "bg-faint")} />
                  Desktop
                </Pill>
              )}
              {browserApi && (
                <Pill
                  active={false}
                  onClick={() => {
                    openPane();
                    pinLiveSurface("browser");
                    setBrowserPanelMode("expanded");
                  }}
                  className="text-xs"
                >
                  <span className="mr-1.5 inline-block h-1.5 w-1.5 rounded-full bg-sky-500" />
                  Browser
                </Pill>
              )}
            </>
          )}
          <Pill
            active={surfaceActive("artifacts")}
            onClick={() => {
              openPane();
              setBrowserPanelMode("expanded");
              pinWorkspaceSurface("artifacts");
            }}
            onMouseEnter={() => {
              void import("../AgentCanvas");
            }}
            className="text-xs"
          >
            Files
          </Pill>
          <Pill
            active={surfaceActive("monitor")}
            onClick={() => {
              openPane();
              setBrowserPanelMode("expanded");
              pinWorkspaceSurface("monitor");
            }}
            onMouseEnter={() => {
              void import("../AgentCanvas");
            }}
            className="text-xs"
          >
            Activity
          </Pill>
          {workspaceSurface === "artifacts" && filters.map((f) => (
            <Pill
              key={f}
              active={artifactFilter === f}
              onClick={() => onFilterChange(f)}
              className="text-xs"
            >
              {f.charAt(0).toUpperCase() + f.slice(1)}
              {artifactCounts[f] > 0 && (
                <span className={cn("text-[0.65rem]", artifactFilter === f ? "text-accent" : "text-faint")}>
                  ({artifactCounts[f]})
                </span>
              )}
            </Pill>
          ))}
        </nav>
      ) : (
        <button
          type="button"
          className="ka-icon-btn hidden h-7 w-7 text-faint hover:text-ink lg:inline-flex items-center justify-center rounded-lg hover:bg-line/40 transition-colors"
          onClick={() => setPrefs({ rightPanelCollapsed: true })}
          title="Collapse workspace panel"
          aria-label="Collapse workspace panel"
        >
          <Icon.Canvas size={15} />
        </button>
      )}
    </header>
  );
}
