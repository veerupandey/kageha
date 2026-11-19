import { lazy, Suspense, useCallback, useEffect, useState } from "react";
import { ReminderNotifications } from "./ReminderNotifications";
import { JobCompletionToasts } from "./JobCompletionToasts";
import { cn } from "../lib/cn";
import { Icon } from "../lib/icons";
import { usePeerAlerts } from "../lib/usePeerAlerts";
import { isNarrowViewport } from "../lib/viewport";
import { useAppStore } from "../store";
import { CommandCenter } from "./CommandCenter/CommandCenter";
import { GeminiLiveVoiceProvider } from "./GeminiLiveVoiceContext";
import { Sidebar } from "./Sidebar/Sidebar";

const ArtifactLightbox = lazy(() =>
  import("./Lightbox/ArtifactLightbox").then((module) => ({ default: module.ArtifactLightbox })),
);
const ExtensionsPanel = lazy(() => import("./ExtensionsPanel").then((module) => ({ default: module.ExtensionsPanel })));
const JobsPanel = lazy(() => import("./Sidebar/JobsPanel").then((module) => ({ default: module.JobsPanel })));
const ProjectBrainPanel = lazy(() =>
  import("./Sidebar/ProjectBrainPanel").then((module) => ({ default: module.ProjectBrainPanel })),
);
const KnowledgePanel = lazy(() =>
  import("./Sidebar/KnowledgePanel").then((module) => ({ default: module.KnowledgePanel })),
);
const WorktreesPanel = lazy(() =>
  import("./Sidebar/WorktreesPanel").then((module) => ({ default: module.WorktreesPanel })),
);
const PeerNetwork = lazy(() => import("./PeerNetwork").then((module) => ({ default: module.PeerNetwork })));
const ThreadView = lazy(() => import("./ThreadView/ThreadView").then((module) => ({ default: module.ThreadView })));
const CommandPalette = lazy(() =>
  import("./CommandPalette/CommandPalette").then((module) => ({ default: module.CommandPalette })),
);
const ShortcutsModal = lazy(() =>
  import("./ShortcutsModal/ShortcutsModal").then((module) => ({ default: module.ShortcutsModal })),
);

type ActivePanel = null | "extensions" | "jobs" | "worktrees" | "brain" | "knowledge" | "agent_network";


function OptionalPanelFallback() {
  return (
    <div className="flex min-h-0 flex-1 items-center justify-center text-xs text-muted" role="status">
      Loading panel…
    </div>
  );
}

/**
 * AppShell — 3-panel layout with collapsible sidebars.
 * Switches between CommandCenter (home) and ThreadView (active session).
 */
export function AppShell() {
  const loadModels = useAppStore((s) => s.loadModels);
  useEffect(() => {
    const refreshModels = () => {
      if (!document.hidden) void loadModels();
    };
    const timer = window.setInterval(refreshModels, 60_000);
    window.addEventListener("focus", refreshModels);
    return () => {
      window.clearInterval(timer);
      window.removeEventListener("focus", refreshModels);
    };
  }, [loadModels]);
  useEffect(() => {
    const viewport = window.visualViewport;
    if (!viewport) return;
    const update = () => {
      // Pinch zoom must keep the page zoomable; only follow keyboard/browser resizing.
      if (viewport.scale !== 1 || viewport.height <= 0) return;
      document.documentElement.style.setProperty("--ka-visual-height", `${viewport.height}px`);
      document.documentElement.style.setProperty("--ka-visual-offset-top", `${viewport.offsetTop}px`);
    };
    update();
    viewport.addEventListener("resize", update);
    viewport.addEventListener("scroll", update);
    window.addEventListener("pageshow", update);
    document.addEventListener("visibilitychange", update);
    return () => {
      viewport.removeEventListener("resize", update);
      viewport.removeEventListener("scroll", update);
      window.removeEventListener("pageshow", update);
      document.removeEventListener("visibilitychange", update);
      document.documentElement.style.removeProperty("--ka-visual-height");
      document.documentElement.style.removeProperty("--ka-visual-offset-top");
    };
  }, []);
  const sessionId = useAppStore((s) => s.sessionId);
  const sessions = useAppStore((s) => s.sessions);
  const openSession = useAppStore((s) => s.openSession);
  const sessionLoading = useAppStore((s) => s.sessionLoading);
  const sessionOpenFailure = useAppStore((s) => s.sessionOpenFailure);
  const sessionRefreshing = useAppStore((s) => s.sessionRefreshing);
  const sessionRefreshError = useAppStore((s) => s.sessionRefreshError);
  const canvasItems = useAppStore((s) => s.canvasItems);
  const canvasExpanded = useAppStore((s) => s.canvasExpanded);
  const canvasSelectedPath = useAppStore((s) => s.canvasSelectedPath);
  const setCanvasExpanded = useAppStore((s) => s.setCanvasExpanded);
  const newChat = useAppStore((s) => s.newChat);
  const sidebarCollapsed = useAppStore((s) => s.prefs.sidebarCollapsed);
  const rightPanelCollapsed = useAppStore((s) => s.prefs.rightPanelCollapsed);
  const setPrefs = useAppStore((s) => s.setPrefs);
  const [sidebarOpen, setSidebarOpen] = useState(false);
  const [activePanel, setActivePanel] = useState<ActivePanel>(null);
  const [lightboxPath, setLightboxPath] = useState<string | null>(null);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [shortcutsOpen, setShortcutsOpen] = useState(false);
  const peerAlertNavRequestId = useAppStore((s) => s.peerAlertNavRequestId);

  usePeerAlerts();

  // A toast's "Review" action bumps this counter (App.tsx renders <Toasts />
  // as AppShell's sibling, so it can't call setActivePanel directly).
  useEffect(() => {
    if (peerAlertNavRequestId > 0) setActivePanel("agent_network");
  }, [peerAlertNavRequestId]);

  // Clicking a native OS notification (peerNotifications.ts) fires this.
  useEffect(() => {
    const onOpen = () => setActivePanel("agent_network");
    window.addEventListener("kageha:open-agent-network", onOpen);
    return () => window.removeEventListener("kageha:open-agent-network", onOpen);
  }, []);

  // Each session on phone starts with workspace hidden so chat uses the full viewport.
  useEffect(() => {
    if (!sessionId || !isNarrowViewport()) return;
    setPrefs({ rightPanelCollapsed: true });
  }, [sessionId, setPrefs]);

  // Keyboard shortcuts
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      const meta = e.metaKey || e.ctrlKey;
      // Cmd+N: new thread
      if (meta && e.key === "n" && !e.shiftKey) {
        e.preventDefault();
        void newChat();
        return;
      }
      // Cmd+K: toggle command palette
      if (meta && e.key === "k") {
        e.preventDefault();
        setPaletteOpen((prev) => !prev);
        return;
      }
      // Cmd+B: toggle sidebar
      if (meta && e.key === "b") {
        e.preventDefault();
        setPrefs({ sidebarCollapsed: !sidebarCollapsed });
        return;
      }
      // Cmd+\\: toggle right panel
      if (meta && e.key === "\\") {
        e.preventDefault();
        setPrefs({ rightPanelCollapsed: !rightPanelCollapsed });
        return;
      }
      // Alt+Up / Alt+Down: switch between recent threads
      if (e.altKey && (e.key === "ArrowDown" || e.key === "ArrowUp")) {
        const visibleSessions = sessions.filter((s) => !s.archived);
        if (visibleSessions.length > 0) {
          e.preventDefault();
          const curIndex = visibleSessions.findIndex((s) => s.session_id === sessionId);
          const nextIndex = e.key === "ArrowDown"
            ? (curIndex + 1) % visibleSessions.length
            : (curIndex - 1 + visibleSessions.length) % visibleSessions.length;
          void openSession(visibleSessions[nextIndex].session_id);
          return;
        }
      }
      // Cmd+Shift+S: toggle subagents / jobs panel
      if (meta && e.shiftKey && e.key.toLowerCase() === "s") {
        e.preventDefault();
        setActivePanel((prev) => (prev === "jobs" ? null : "jobs"));
        return;
      }
      // ?: toggle keyboard shortcuts cheatsheet
      if (e.key === "?" && !meta) {
        const tag = (e.target as HTMLElement)?.tagName?.toLowerCase();
        const isEditable = (e.target as HTMLElement)?.isContentEditable;
        if (tag !== "input" && tag !== "textarea" && !isEditable) {
          e.preventDefault();
          setShortcutsOpen((prev) => !prev);
          return;
        }
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [newChat, openSession, rightPanelCollapsed, sessionId, sessions, setPrefs, sidebarCollapsed]);

  const handleOpenLightbox = useCallback((path: string) => {
    setLightboxPath(path);
    setCanvasExpanded(true);
  }, [setCanvasExpanded]);

  const handleCloseLightbox = useCallback(() => {
    setLightboxPath(null);
    setCanvasExpanded(false);
  }, [setCanvasExpanded]);

  // Message artifact cards update the shared canvas state. Bridge that state
  // into this layout so generated files open in the panel or lightbox.
  useEffect(() => {
    if (!canvasSelectedPath) return;
    // Restored and newly discovered artifacts may select a preview in the
    // background. On phone, only an explicit user action should reveal it.
    if (!isNarrowViewport()) setPrefs({ rightPanelCollapsed: false });
    if (canvasExpanded) setLightboxPath(canvasSelectedPath);
  }, [canvasSelectedPath, canvasExpanded, setPrefs]);

  const handleNavigateLightbox = useCallback(
    (direction: "prev" | "next") => {
      if (!lightboxPath || canvasItems.length === 0) return;
      const idx = canvasItems.findIndex((i) => i.path === lightboxPath);
      if (idx === -1) return;
      const next =
        direction === "next"
          ? (idx + 1) % canvasItems.length
          : (idx - 1 + canvasItems.length) % canvasItems.length;
      setLightboxPath(canvasItems[next].path);
    },
    [lightboxPath, canvasItems],
  );

  const handleAgentSelect = useCallback((agentId: string) => {
    if (agentId === "jobs") setActivePanel("jobs");
    else if (agentId === "extensions") setActivePanel("extensions");
    else if (agentId === "worktrees") setActivePanel("worktrees");
    else if (agentId === "project_brain") setActivePanel("brain");
    else if (agentId === "knowledge") setActivePanel("knowledge");
    else if (agentId === "agent_network") setActivePanel("agent_network");
    else setActivePanel(null);
  }, []);

  const isHome = !sessionId && !activePanel;

  return (
    <GeminiLiveVoiceProvider>
      <JobCompletionToasts />
      <ReminderNotifications />
      <div className="app-shell flex min-h-0 flex-1 overflow-hidden bg-canvas text-ink" data-ui="kageha">
        {/* Sidebar */}
        <Sidebar
          open={sidebarOpen}
          collapsed={sidebarCollapsed}
          onClose={() => setSidebarOpen(false)}
          onToggleCollapse={() => setPrefs({ sidebarCollapsed: !sidebarCollapsed })}
          onAgentSelect={handleAgentSelect}
        />

        {/* Mobile sidebar backdrop */}
        <div
          className={cn(
            "fixed inset-0 z-40 bg-black/60 backdrop-blur-xs transition-opacity duration-200 lg:hidden",
            sidebarOpen ? "opacity-100 pointer-events-auto" : "opacity-0 pointer-events-none",
          )}
          onClick={() => setSidebarOpen(false)}
          aria-hidden={!sidebarOpen}
        />

        {/* Main content */}
        <div className="flex min-h-0 min-w-0 flex-1 flex-col">
          {/* Mobile header: home + resource panels (thread view uses ThreadHeader menu) */}
          {(!sessionId || activePanel) && (
          <div className="ka-mobile-header flex h-11 shrink-0 items-center justify-between border-b border-[var(--color-line)] bg-canvas/80 backdrop-blur-md px-3 lg:hidden">
            <div className="flex items-center gap-2">
              <button
                type="button"
                className="ka-icon-btn h-8 w-8 text-muted hover:text-ink"
                aria-label="Open menu"
                onClick={() => setSidebarOpen(true)}
              >
                <Icon.Menu size={18} />
              </button>
              <span className="text-sm font-semibold tracking-tight text-ink">Kageha</span>
            </div>
            {!isHome && (
              <button
                type="button"
                className="inline-flex items-center gap-1 px-2 py-1 rounded-md text-xs font-medium text-accent hover:bg-accent-soft transition-colors"
                onClick={() => {
                  setActivePanel(null);
                  newChat();
                }}
              >
                <Icon.NewThread size={13} />
                <span>New</span>
              </button>
            )}
          </div>
          )}

          {/* Content area */}
          <Suspense fallback={<OptionalPanelFallback />}>
            {activePanel === "jobs" ? (
              <JobsPanel onClose={() => setActivePanel(null)} />
            ) : activePanel === "extensions" ? (
              <ExtensionsPanel onClose={() => setActivePanel(null)} />
            ) : activePanel === "worktrees" ? (
              <WorktreesPanel onClose={() => setActivePanel(null)} />
            ) : activePanel === "brain" ? (
              <ProjectBrainPanel onClose={() => setActivePanel(null)} />
            ) : activePanel === "knowledge" ? (
              <KnowledgePanel onClose={() => setActivePanel(null)} />
            ) : activePanel === "agent_network" ? (
              <PeerNetwork onClose={() => setActivePanel(null)} />
            ) : sessionOpenFailure ? (
              <div className="flex min-h-0 flex-1 flex-col items-center justify-center gap-3 p-6 text-sm text-muted" role="alert">
                <p>Could not open this thread</p>
                <p>{sessionOpenFailure.message}</p>
                <p>This loading error does not establish whether the background task failed.</p>
                <button type="button" className="rounded-md border border-line px-3 py-2 text-ink"
                  onClick={() => void openSession(sessionOpenFailure.sessionId).catch(() => {})}>Retry opening thread</button>
              </div>
            ) : sessionLoading ? (
              <div className="flex min-h-0 flex-1 items-center justify-center text-sm text-muted" role="status">
                Opening thread…
              </div>
            ) : isHome ? (
              <CommandCenter />
            ) : (
              <>
              {sessionRefreshing === sessionId && <div className="px-4 py-1 text-xs text-muted" role="status">Refreshing thread…</div>}
              {sessionRefreshError?.sessionId === sessionId && <div className="flex items-center gap-3 px-4 py-1 text-xs text-muted" role="alert">
                <span>Could not refresh this thread. {sessionRefreshError.message}</span>
                <button type="button" className="underline" onClick={() => void openSession(sessionId!).catch(() => {})}>Retry refresh</button>
              </div>}
              <ThreadView
                onOpenSidebar={() => setSidebarOpen(true)}
                onOpenLightbox={handleOpenLightbox}
                rightPanelCollapsed={rightPanelCollapsed}
                onToggleRightPanel={() => setPrefs({ rightPanelCollapsed: !rightPanelCollapsed })}
              />
              </>
            )}
          </Suspense>
        </div>
      </div>

      {/* Lightbox overlay */}
      {lightboxPath ? (
        <Suspense fallback={null}>
          <ArtifactLightbox
            itemPath={lightboxPath}
            onClose={handleCloseLightbox}
            onNavigate={handleNavigateLightbox}
          />
        </Suspense>
      ) : null}

      {/* Global Command Palette */}
      {paletteOpen ? (
        <Suspense fallback={null}>
          <CommandPalette
            open={paletteOpen}
            onClose={() => setPaletteOpen(false)}
            onOpenShortcuts={() => setShortcutsOpen(true)}
          />
        </Suspense>
      ) : null}

      {/* Keyboard Shortcuts Cheatsheet */}
      {shortcutsOpen ? (
        <Suspense fallback={null}>
          <ShortcutsModal open={shortcutsOpen} onClose={() => setShortcutsOpen(false)} />
        </Suspense>
      ) : null}
    </GeminiLiveVoiceProvider>
  );
}
