import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Icon } from "../../lib/icons";
import { cn } from "../../lib/cn";
import { useAppStore } from "../../store";
import { ConversationPanel } from "./ConversationPanel";
import { WorkspacePanel } from "./WorkspacePanel";
import { WorkerDetailsPanel } from "../WorkerDetailsPanel";
import { useSelectedAgent } from "../../lib/useSelectedAgent";
import type { ArtifactFilter } from "./ThreadHeader";
import { ThreadHeader } from "./ThreadHeader";

interface ThreadViewProps {
  onOpenLightbox: (path: string) => void;
  onOpenSidebar?: () => void;
  rightPanelCollapsed?: boolean;
  onToggleRightPanel?: () => void;
}

export function ThreadView({ onOpenLightbox, onOpenSidebar, rightPanelCollapsed, onToggleRightPanel }: ThreadViewProps) {
  const sessionId = useAppStore(s => s.sessionId);
  const [selectedWorker, setSelectedWorker] = useSelectedAgent(sessionId);
  const closeWorker = useCallback(() => {
    setSelectedWorker(null);
    if (window.innerWidth < 1024 && !rightPanelCollapsed) onToggleRightPanel?.();
  }, [onToggleRightPanel, rightPanelCollapsed, setSelectedWorker]);
  const panelOpen = Boolean(selectedWorker) || !rightPanelCollapsed;
  const closePanel = selectedWorker ? closeWorker : onToggleRightPanel;
  const canvasItems = useAppStore((s) => s.canvasItems);
  const pinWorkspaceSurface = useAppStore((s) => s.pinWorkspaceSurface);
  const machineActive = useAppStore((s) => s.machineActive);
  const liveSurface = useAppStore((s) => s.liveSurface);
  const [artifactFilter, setArtifactFilter] = useState<ArtifactFilter>("all");
  const layoutRef = useRef<HTMLDivElement>(null);
  const cinematic = !selectedWorker && (machineActive || liveSurface === "computer");
  const minPanel = cinematic ? 560 : 360;
  const restoredPanelWidth = useRef(false);
  const startedOnPhone = useRef(window.innerWidth < 1024);
  const [rightWidth, setRightWidth] = useState(() => {
    try {
      const saved = Number(localStorage.getItem("kageha.rightPanelWidth"));
      if (Number.isFinite(saved) && saved > 0) {
        restoredPanelWidth.current = true;
        return saved;
      }
      return Math.round(window.innerWidth * 0.44);
    } catch {
      return Math.round(window.innerWidth * 0.44);
    }
  });
  const [resizing, setResizing] = useState(false);

  useEffect(() => {
    if (!cinematic) return;
    setRightWidth((current) => (current < 640 ? 720 : current));
  }, [cinematic]);

  useEffect(() => {
    if (!resizing) return;
    const onMove = (event: PointerEvent) => {
      const bounds = layoutRef.current?.getBoundingClientRect();
      if (!bounds) return;
      const max = Math.max(minPanel, bounds.width - 400);
      setRightWidth(Math.min(max, Math.max(minPanel, bounds.right - event.clientX)));
    };
    const onUp = () => setResizing(false);
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp, { once: true });
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
    return () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
    };
  }, [resizing, minPanel]);

  useEffect(() => {
    try {
      localStorage.setItem(
        "kageha.rightPanelWidth",
        String(Math.round(rightWidth)),
      );
    } catch {
      /* restricted/private storage — ignore */
    }
  }, [rightWidth]);

  useEffect(() => {
    let previousWidth = 0;
    const clampWidth = () => {
      const bounds = layoutRef.current?.getBoundingClientRect();
      if (!bounds || bounds.width <= 0 || window.innerWidth < 1024) return;
      const max = Math.max(minPanel, bounds.width - 400);
      const ratio = previousWidth > 0 ? bounds.width / previousWidth : 1;
      const firstDesktop = !previousWidth && startedOnPhone.current && !restoredPanelWidth.current;
      previousWidth = bounds.width;
      setRightWidth((current) => Math.min(max, Math.max(minPanel, firstDesktop ? bounds.width * 0.44 : current * ratio)));
    };
    clampWidth();
    const observer = new ResizeObserver(clampWidth);
    if (layoutRef.current) observer.observe(layoutRef.current);
    window.addEventListener("resize", clampWidth);
    return () => { observer.disconnect(); window.removeEventListener("resize", clampWidth); };
  }, [minPanel]);

  // Count artifacts by type
  const artifactCounts = useMemo(() => {
    const counts: Record<ArtifactFilter, number> = {
      all: canvasItems.length,
      images: 0,
      video: 0,
      webpages: 0,
      documents: 0,
      code: 0,
    };
    for (const item of canvasItems) {
      if (item.kind === "image") counts.images++;
      else if (item.kind === "video") counts.video++;
      else if (item.kind === "markdown" || item.kind === "webpage") counts.webpages++;
      else if (item.kind === "code") counts.code++;
      else if (item.kind === "pdf" || item.kind === "document" || item.kind === "text" || item.kind === "spreadsheet" || item.kind === "presentation") counts.documents++;
    }
    return counts;
  }, [canvasItems]);

  const handleFilterChange = useCallback((filter: ArtifactFilter) => {
    setArtifactFilter(filter);
    pinWorkspaceSurface("artifacts");
  }, [pinWorkspaceSurface]);

  return (
    <main
      className={cn(
        "ka-thread-view flex min-h-0 min-w-0 flex-1 flex-col",
        panelOpen && "ka-workspace-open",
      )}
      id="stage"
    >
      {panelOpen && closePanel ? (
        <button
          type="button"
          className="ka-workspace-backdrop fixed inset-0 z-[35] bg-black/60 backdrop-blur-xs transition-opacity duration-200 lg:hidden"
          aria-label="Close workspace"
          onClick={closePanel}
        />
      ) : null}

      <div ref={layoutRef} className="flex min-h-0 flex-1 overflow-hidden">
        {/* Conversation panel (left) — expands to full width when right panel is collapsed */}
        <div className="flex min-h-0 min-w-0 flex-1 flex-col">
          <ThreadHeader
            artifactFilter={artifactFilter}
            onFilterChange={handleFilterChange}
            artifactCounts={artifactCounts}
            onOpenSidebar={onOpenSidebar}
          />
          <ConversationPanel onInspectWorker={setSelectedWorker} />
        </div>

        {/* Right panel: collapsed rail or full panel */}
        {!panelOpen ? (
          <aside className="hidden lg:flex flex-col items-center w-11 border-l border-line/60 bg-surface/50 backdrop-blur-xs py-3 gap-2" aria-label="Collapsed workspace">
            <button
              type="button"
              className="ka-icon-btn h-8 w-8 rounded-lg text-faint hover:text-accent hover:bg-accent-soft transition-all"
              aria-label="Expand right panel"
              title="Expand workspace (⌘\)"
              onClick={onToggleRightPanel}
              onMouseEnter={() => {
                void import("../AgentCanvas");
              }}
            >
              <Icon.Canvas size={17} />
            </button>
            {canvasItems.length > 0 && (
              <span className="rounded-full bg-accent/15 border border-accent/20 px-1.5 py-0.5 text-[0.58rem] text-accent font-bold tabular-nums">
                {canvasItems.length}
              </span>
            )}
          </aside>
        ) : (
          <>
          <div
            className="group relative hidden w-2 shrink-0 cursor-col-resize bg-line/30 transition-colors hover:bg-accent/40 focus-visible:bg-accent/40 focus-visible:outline-2 focus-visible:outline-accent lg:block"
            role="separator"
            aria-label="Resize chat and workspace panels"
            aria-orientation="vertical"
            title="Drag to resize, or use the left and right arrow keys"
            onPointerDown={(event) => {
              event.preventDefault();
              event.currentTarget.setPointerCapture?.(event.pointerId);
              setResizing(true);
            }}
            onKeyDown={(event) => {
              if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
              event.preventDefault();
              const available = layoutRef.current?.getBoundingClientRect().width || 1600;
              const max = Math.max(minPanel, Math.min(1200, available - 400));
              setRightWidth((value) => event.key === "ArrowLeft"
                ? Math.min(value + 24, max) : Math.max(value - 24, minPanel));
            }}
            tabIndex={0}
            aria-valuemin={minPanel}
            aria-valuenow={Math.round(rightWidth)}
            aria-valuemax={1200}
          >
            <span className="absolute left-1/2 top-1/2 h-8 w-0.5 -translate-x-1/2 -translate-y-1/2 rounded-full bg-line-strong/80 group-hover:bg-accent group-focus-visible:bg-accent transition-colors" />
          </div>
          <div
            className={cn(
              "ka-canvas-panel hidden min-h-0 shrink-0 flex-col overflow-hidden lg:flex",
              panelOpen && "ka-canvas-panel--open",
            )}
            style={{ width: `${selectedWorker ? Math.min(rightWidth, 480) : rightWidth}px`, maxWidth: "calc(100% - 400px)" }}
          >
            {selectedWorker && sessionId ? (
              <WorkerDetailsPanel
                key={selectedWorker.id}
                initialJob={selectedWorker}
                parentSession={sessionId}
                onClose={closeWorker}
                onSelectWorker={setSelectedWorker}
              />
            ) : <WorkspacePanel
              artifactFilter={artifactFilter}
              artifactCount={canvasItems.length}
              onOpenLightbox={onOpenLightbox}
              onCollapse={onToggleRightPanel}
            />}
          </div>
          </>
        )}
      </div>
    </main>
  );
}
