import { RunChangeCard } from "../RunChangeCard";
import { lazy, Suspense, useEffect, useRef, useState } from "react";
import { cn } from "../../lib/cn";
import { Icon } from "../../lib/icons";
import { useAppStore } from "../../store";
import type { WorkspaceSurface } from "../../store/types";
import { BrowserPanel } from "../BrowserPanel";
import { ArtifactPanel } from "./ArtifactPanel";
import type { ArtifactFilter } from "./ThreadHeader";
import { useSessionTruth } from "../../lib/sessionTruth";
import { WorkspaceTitleBar } from "./WorkspaceTitleBar";
import { ReviewPanel, useReviewPanel } from "./ReviewPanel";
import { useRequestedBrowserTarget } from "../../lib/browserWorkspace";

const AgentCanvas = lazy(() => import("../AgentCanvas").then((module) => ({ default: module.AgentCanvas })));
const ComputerStage = lazy(() => import("./ComputerStage").then((module) => ({ default: module.ComputerStage })));
const ComputerPanel = lazy(() => import("../ComputerPanel").then((module) => ({ default: module.ComputerPanel })));
const DesktopFleetPanel = lazy(() => import("../DesktopFleetPanel").then((module) => ({ default: module.DesktopFleetPanel })));


function WorkspaceSurfaceFallback() {
  return (
    <div className="flex min-h-0 flex-1 items-center justify-center text-xs text-muted" role="status">
      Loading workspace…
    </div>
  );
}

interface WorkspacePanelProps {
  artifactFilter: ArtifactFilter;
  artifactCount: number;
  onOpenLightbox: (path: string) => void;
  onCollapse?: () => void;
}

const hostSurfaces: Array<{ id: WorkspaceSurface; label: string }> = [
  { id: "live", label: "Computer" },
  { id: "monitor", label: "Activity" },
  { id: "desktops", label: "Desktops" },
  { id: "artifacts", label: "Files" },
];

const computerSurfaces: Array<{ id: WorkspaceSurface; label: string }> = [
  { id: "live", label: "Computer" },
  { id: "artifacts", label: "Files" },
  { id: "monitor", label: "Activity" },
];

const browserSurfaces: Array<{ id: WorkspaceSurface; label: string }> = [
  { id: "live", label: "Browser" },
  { id: "artifacts", label: "Files" },
  { id: "monitor", label: "Activity" },
];

const surfaceDescriptions: Record<WorkspaceSurface, string> = {
  live: "Watch the live screen — take over when you need to",
  monitor: "Task overview and activity",
  desktops: "Watch all running Docker desktops",
  artifacts: "Outputs created in this session",
  review: "Review items with the agent",
};

export function WorkspacePanel({ artifactFilter, artifactCount, onOpenLightbox, onCollapse }: WorkspacePanelProps) {
  const surface = useAppStore((s) => s.workspaceSurface);
  const liveSurface = useAppStore((s) => s.liveSurface);
  const selectedMachine = useAppStore((s) => s.machineActive);
  const sessionId = useAppStore((s) => s.sessionId);
  const browserRequest = useRequestedBrowserTarget(sessionId);
  const review = useReviewPanel(sessionId);
  const reviewOpen = Boolean(review.panel?.state?.view);
  const requestedPanel = review.panel?.state?.focus ?? (reviewOpen ? "checklist" : null);
  const focusRevision = review.panel?.state?.focus_revision;
  const revealSurface = useAppStore((s) => s.revealWorkspaceSurface);
  const truth = useSessionTruth(sessionId);
  const targetPending = useAppStore((s) => s.executionTargetPending);
  const machineActive = !targetPending && truth.status === "current" && truth.data?.selected_target
    ? truth.data.selected_target === "machine" : selectedMachine;
  const setSurface = useAppStore((s) => s.setWorkspaceSurface);
  const setLiveSurface = useAppStore((s) => s.setLiveSurface);
  const pinWorkspaceSurface = useAppStore((s) => s.pinWorkspaceSurface);
  const pinLiveSurface = useAppStore((s) => s.pinLiveSurface);
  const setBrowserPanelMode = useAppStore((s) => s.setBrowserPanelMode);
  const remoteExecution = useAppStore((s) => s.remoteExecution);
  const wasMachine = useRef(false);

  const [expanded, setExpanded] = useState(false);
  const [split, setSplit] = useState(false);
  const [focusedCard, setFocusedCard] = useState<"live" | "artifacts" | null>(null);
  useEffect(() => {
    setExpanded(false);
    setSplit(false);
    setFocusedCard(null);
  }, [sessionId]);
  useEffect(() => { setFocusedCard(null); }, [surface, liveSurface, browserRequest]);
  const hostBrowser = !machineActive && liveSurface === "browser";
  const canSplit = hostBrowser && (surface === "live" || surface === "artifacts");
  const splitActive = canSplit && split;
  const spotlightActive = splitActive && focusedCard !== null;
  const showSplit = splitActive && !spotlightActive;
  const displayedSurface = spotlightActive ? focusedCard : surface;
  const browserVisible = hostBrowser && (displayedSurface === "live" || showSplit);
  const filesVisible = displayedSurface === "artifacts" || showSplit;
  const immersiveLive = surface === "live" && (machineActive || liveSurface === "computer" || hostBrowser);
  const baseTabs = machineActive || liveSurface === "computer"
    ? computerSurfaces
    : hostBrowser
      ? browserSurfaces
      : hostSurfaces;
  const tabs = reviewOpen || surface === "review"
    ? [...baseTabs, { id: "review" as const, label: "Review" }]
    : baseTabs;

  useEffect(() => {
    if (requestedPanel === "checklist") revealSurface("review");
    if (requestedPanel === "files") revealSurface("artifacts");
    if (requestedPanel === "activity") revealSurface("monitor");
    if (requestedPanel === "browser" || requestedPanel === "computer") {
      revealSurface("live", { liveSurface: requestedPanel });
    }
  }, [sessionId, requestedPanel, focusRevision, revealSurface]);

  useEffect(() => {
    if (machineActive && !wasMachine.current) setLiveSurface("computer");
    if ((machineActive || liveSurface === "computer") && surface === "desktops") setSurface("live");
    wasMachine.current = machineActive;
  }, [machineActive, liveSurface, surface, setSurface, setLiveSurface]);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.defaultPrevented || event.isComposing) return;
      if (event.key === "Escape" && spotlightActive) { setFocusedCard(null); return; }
      if (event.key === "Escape" && expanded) { setExpanded(false); return; }
      if (!(event.metaKey || event.ctrlKey)) return;
      const key = event.key.toLowerCase();
      if (["1", "2", "3"].includes(key)) setFocusedCard(null);
      if (key === "1") { event.preventDefault(); pinWorkspaceSurface("live"); }
      if (key === "2") { event.preventDefault(); pinWorkspaceSurface("artifacts"); }
      if (key === "3") { event.preventDefault(); pinWorkspaceSurface("monitor"); }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [pinWorkspaceSurface, expanded, spotlightActive]);

  const selectSurface = (next: WorkspaceSurface) => {
    setFocusedCard(null);
    pinWorkspaceSurface(next);
    if (next !== "live") setBrowserPanelMode("expanded");
  };
  const spotlight = (next: "live" | "artifacts") => {
    pinWorkspaceSurface(surface);
    setFocusedCard(next);
  };

  return (
    <section
      className={cn(
        "ka-workspace-surface flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden border-l",
        "border-[var(--color-line)] bg-surface",
        expanded && "ka-workspace-surface--expanded",
      )}
      data-surface={displayedSurface}
      aria-label="Session workspace"
    >
      <div className="ka-mobile-workspace-bar">
        <button type="button" onClick={onCollapse} className="flex items-center gap-1.5">
          <Icon.Back size={14} /> Back to chat
        </button>
        <span>{tabs.find((tab) => tab.id === displayedSurface)?.label || "Workspace"}</span>
      </div>
      <WorkspaceTitleBar surface={displayedSurface} tabs={tabs} artifactCount={artifactCount}
        machineActive={machineActive} liveSurface={liveSurface} expanded={expanded}
        split={showSplit} spotlight={spotlightActive} onSplit={canSplit ? () => {
          if (spotlightActive) setFocusedCard(null);
          else setSplit(value => !value);
        } : undefined}
        onSelect={selectSurface} onLiveSurface={pinLiveSurface}
        onZoom={() => setExpanded(value => !value)}
        onCollapse={onCollapse ? () => { setExpanded(false); onCollapse(); } : undefined} />

      {surface !== "live" && surface !== "artifacts" && surface !== "review" ? (
        <div className="flex shrink-0 items-center justify-between border-b border-[var(--color-line)] px-3 py-1.5 text-[0.62rem] text-faint">
          <span>{surfaceDescriptions[surface]}</span>
          <span className="font-mono uppercase tracking-[0.12em] text-[0.56rem] text-[var(--color-faint)]">
            ⌘{surface === "monitor" ? "3" : "1"}
          </span>
        </div>
      ) : null}

      {surface === "live" && !immersiveLive && remoteExecution && remoteExecution.surface !== "worker" && (
        <div className="flex shrink-0 items-center gap-2 border-b border-sky-400/20 bg-sky-500/5 px-3 py-1.5 text-[0.62rem] text-sky-700 dark:text-sky-200">
          <span className="h-1.5 w-1.5 rounded-full bg-sky-400" />
          <span className="truncate">
            Remote {remoteExecution.surface} · {remoteExecution.peerName || remoteExecution.peerId || "peer"}
          </span>
          <span className="ml-auto shrink-0 uppercase tracking-wide opacity-70">{remoteExecution.status}</span>
        </div>
      )}

      <div className={cn("flex min-h-0 flex-1 flex-col overflow-hidden",
        (browserVisible || filesVisible) && "workspace-stage", showSplit && "workspace-split")}>
        <Suspense fallback={<WorkspaceSurfaceFallback />}>
          {machineActive ? (
            <div
              className="relative flex min-h-0 flex-1 flex-col overflow-hidden"
              style={{ display: surface === "live" ? undefined : "none" }}
            >
              <ComputerStage onCollapse={onCollapse} visible={surface === "live"} browser={liveSurface === "browser"} />
            </div>
          ) : (
            <>
              {surface === "live" && liveSurface === "computer" && (
                <ComputerPanel
                  onCollapse={onCollapse}
                  onOpenBrowser={() => pinLiveSurface("browser")}
                />
              )}
              {/* Retain the panel's state across tabs; BrowserPanel releases its
                  stream and polling requests while hidden. */}
              <div
                className={cn("relative flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden", browserVisible && "workspace-split-card")}
                style={{ display: browserVisible ? undefined : "none" }}
              >
                {showSplit && <WorkspaceCardHeader label="Browser" onSpotlight={() => spotlight("live")} />}
                <BrowserPanel onCollapse={showSplit ? undefined : onCollapse} embeddedInWorkspace visible={browserVisible} />
              </div>
            </>
          )}
        </Suspense>
        <Suspense fallback={<WorkspaceSurfaceFallback />}>
          {surface === "monitor" && <>
            {sessionId && <div className="shrink-0 px-2"><RunChangeCard key={sessionId} sessionId={sessionId} /></div>}
            <AgentCanvas alwaysShow onCollapse={onCollapse} />
          </>}
          {surface === "desktops" && !machineActive && <DesktopFleetPanel />}
        </Suspense>
        {/* Files is a core surface: keep it outside the optional activity
            module's loading boundary so it opens immediately with the chat. */}
        {(surface === "artifacts" || splitActive) && (
          <div className={cn("flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden", filesVisible && "workspace-split-card")}
            style={{ display: filesVisible ? undefined : "none" }}>
            {showSplit && <WorkspaceCardHeader label="Files" onSpotlight={() => spotlight("artifacts")} />}
            <ArtifactPanel filter={artifactFilter} onOpenLightbox={onOpenLightbox} onCollapse={showSplit ? undefined : onCollapse} />
          </div>
        )}
        {surface === "review" && <ReviewPanel {...review} />}
      </div>

      {/* Hyperagent-style floating workspace dock */}
      <div className="workspace-floating-dock" role="toolbar" aria-label="Workspace dock">
        <button
          type="button"
          className={cn("workspace-dock-pill", displayedSurface === "artifacts" && "is-active")}
          onClick={() => selectSurface("artifacts")}
          title={`Deliverables and files (${artifactCount})`}
          aria-label={`Files (${artifactCount})`}
        >
          <Icon.File size={13} aria-hidden="true" />
          <span className="workspace-dock-badge">{artifactCount > 99 ? "99+" : artifactCount}</span>
        </button>
        <button
          type="button"
          className={cn("workspace-dock-pill", displayedSurface === "live" && "is-active")}
          onClick={() => selectSurface("live")}
          title="Browser live session"
          aria-label="Browser"
        >
          <Icon.Desktop size={13} aria-hidden="true" />
          <span className="workspace-dock-badge">1</span>
        </button>
        <button
          type="button"
          className="workspace-dock-pill workspace-dock-action"
          onClick={() => setExpanded((v) => !v)}
          title={expanded ? "Restore workspace" : "Maximize workspace"}
          aria-label={expanded ? "Restore workspace" : "Maximize workspace"}
        >
          <Icon.Expand size={13} aria-hidden="true" />
        </button>
      </div>
    </section>
  );
}

function WorkspaceCardHeader({ label, onSpotlight }: { label: string; onSpotlight: () => void }) {
  return (
    <div className="workspace-card-header">
      <span>{label}</span>
      <button type="button" onClick={onSpotlight} aria-label={`Focus ${label}`} title={`Focus ${label}`}>
        <Icon.Expand size={14} aria-hidden="true" />
      </button>
    </div>
  );
}
