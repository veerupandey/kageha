import { useEffect, useMemo, useState } from "react";
import { cn } from "../../lib/cn";
import { Icon } from "../../lib/icons";
import { isNarrowViewport } from "../../lib/viewport";
import { withViewTransition } from "../../lib/viewTransition";
import { useAppStore } from "../../store";
import type { AgentEntry } from "./AgentsList";
import { AgentsList } from "./AgentsList";
import { PersonaEditorDialog } from "./PersonaEditorDialog";
import { ProjectsList } from "./ProjectsList";
import { RecentThreadsList } from "./RecentThreadsList";
import { ResourcesNav } from "./ResourcesNav";
import { SidebarFooter } from "./SidebarFooter";
import { SidebarHeader } from "./SidebarHeader";
import { SidebarSearch } from "./SidebarSearch";

interface SidebarProps {
  open: boolean;
  collapsed: boolean;
  onClose?: () => void;
  onToggleCollapse?: () => void;
  onAgentSelect?: (agentId: string) => void;
}

export function Sidebar({ open, collapsed, onClose, onToggleCollapse, onAgentSelect }: SidebarProps) {
  const sessions = useAppStore((s) => s.sessions);
  const sessionId = useAppStore((s) => s.sessionId);
  const openSession = useAppStore((s) => s.openSession);
  const newChat = useAppStore((s) => s.newChat);
  const meta = useAppStore((s) => s.meta);
  const personas = useAppStore((s) => s.personas);
  const personasError = useAppStore((s) => s.personasError);
  const refreshPersonas = useAppStore((s) => s.refreshPersonas);
  const activePersonaId = useAppStore((s) => s.activePersonaId);
  const setActivePersonaId = useAppStore((s) => s.setActivePersonaId);
  const activeTeamId = useAppStore((s) => s.activeTeamId);
  const setActiveTeamId = useAppStore((s) => s.setActiveTeamId);
  const teams = useAppStore((s) => s.teams);
  const teamsError = useAppStore((s) => s.teamsError);
  const refreshTeams = useAppStore((s) => s.refreshTeams);
  const personaEditor = useAppStore((s) => s.personaEditor);
  const openPersonaEditor = useAppStore((s) => s.openPersonaEditor);
  const closePersonaEditor = useAppStore((s) => s.closePersonaEditor);
  const [query, setQuery] = useState("");
  const [width, setWidth] = useState(() => {
    try {
      return Number(localStorage.getItem("kageha.sidebarWidth")) || 232;
    } catch {
      return 232;
    }
  });
  const [resizing, setResizing] = useState(false);

  useEffect(() => {
    if (!resizing) return;
    const onMove = (event: PointerEvent) => setWidth(Math.min(420, Math.max(220, event.clientX)));
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
  }, [resizing]);

  useEffect(() => {
    try {
      localStorage.setItem("kageha.sidebarWidth", String(Math.round(width)));
    } catch {
      /* restricted/private storage — ignore */
    }
  }, [width]);

  const agents: AgentEntry[] = useMemo(() => {
    const list: AgentEntry[] = [
      { id: "command-center", name: "Command Center", color: "active" },
    ];
    // Derive agents from meta features/skills if available
    if (meta?.features) {
      for (const [key, enabled] of Object.entries(meta.features)) {
        // Knowledge has its own navigation group; Extensions lives in Settings.
        if (enabled && key !== "sandbox" && key !== "knowledge" && key !== "extensions") {
          list.push({
            id: key,
            name: key.replace(/_/g, " ").replace(/\b\w/g, (c) => c.toUpperCase()),
            color: "idle",
          });
        }
      }
    }
    return list;
  }, [meta]);

  const selectedProjectId = useAppStore((s) => s.selectedProjectId);

  const searchedSessions = useMemo(() => {
    if (!query.trim()) return sessions;
    const q = query.toLowerCase();
    return sessions.filter(
      (s) =>
        (s.title || "").toLowerCase().includes(q) ||
        s.session_id.toLowerCase().includes(q) ||
        (s.project?.name || "").toLowerCase().includes(q),
    );
  }, [sessions, query]);

  const filteredSessions = useMemo(() => selectedProjectId
    ? searchedSessions.filter(
      (s) => s.project_id === selectedProjectId || s.project?.id === selectedProjectId
    )
    : searchedSessions, [searchedSessions, selectedProjectId]);

  // Desktop collapsed rail — mobile always uses the slide-over drawer below.
  const collapsedRail = collapsed ? (
    <aside
      className="ka-sidebar-collapsed hidden lg:flex flex-col items-center w-12 border-r border-[var(--color-line)] bg-surface py-3 gap-2"
      aria-label="Collapsed sidebar"
    >
      <button
        type="button"
        className="ka-icon-btn h-8 w-8"
        aria-label="Expand sidebar"
        title="Expand sidebar"
        onClick={onToggleCollapse}
      >
        <Icon.Expand size={18} />
      </button>
      <button
        type="button"
        className="inline-flex h-8 w-8 items-center justify-center rounded-md text-accent hover:bg-accent-soft transition-colors"
        aria-label="New thread"
        title="New thread"
        onClick={() => {
          withViewTransition(() => {
            newChat().catch(() => {});
          });
        }}
      >
        <Icon.NewThread size={18} />
      </button>
      <div className="w-6 border-t border-[var(--color-line)] my-1" />
      {agents.slice(0, 6).map((agent) => {
        const RailIcon = agent.id === "command-center" ? Icon.CommandCenter
          : agent.id === "jobs" ? Icon.Jobs
          : agent.id === "worktrees" ? Icon.Worktrees
          : agent.id === "project_brain" ? Icon.Brain
          : agent.id === "attach" ? Icon.Attach
          : agent.id === "knowledge" ? Icon.Memories
          : Icon.CommandCenter;
        return (
          <button
            key={agent.id}
            type="button"
            className="inline-flex h-8 w-8 items-center justify-center rounded-md text-muted hover:bg-line/50 hover:text-ink transition-colors"
            title={agent.name}
            onClick={() => {
              if (agent.id === "command-center") {
                // Go home
              }
            }}
          >
            <RailIcon size={18} />
          </button>
        );
      })}
    </aside>
  ) : null;

  return (
    <>
    <aside
      className={cn(
        "ka-sidebar ka-sidebar-shell fixed inset-y-0 left-0 z-50 flex w-[250px] shrink-0 flex-col overflow-hidden transition-transform lg:static lg:z-0 lg:translate-x-0",
        collapsed && "lg:hidden",
        open ? "translate-x-0" : "-translate-x-full lg:translate-x-0",
      )}
      style={{ width: `${width}px` }}
      aria-label="Main navigation"
    >
      <div
        className="absolute bottom-0 right-0 top-0 z-20 hidden w-1.5 cursor-col-resize hover:bg-accent/40 lg:block"
        role="separator"
        aria-label="Resize navigation sidebar"
        aria-orientation="vertical"
        onPointerDown={(event) => { event.preventDefault(); setResizing(true); }}
      />
      <SidebarHeader
        onImported={() => {
          onAgentSelect?.("command-center");
          if (isNarrowViewport()) onClose?.();
        }}
        onNewThread={() => {
          withViewTransition(() => {
            return newChat().then(() => onClose?.()).catch(() => {});
          });
        }}
        onCollapse={() => {
          if (isNarrowViewport()) onClose?.();
          else onToggleCollapse?.();
        }}
      />
      <SidebarSearch query={query} onChange={setQuery} />

      {/* Navigation stays visible; threads get the flexible reading space. */}
      <div className="ka-sidebar-nav shrink-0 overflow-y-auto">
        <AgentsList
          agents={agents}
          onSelect={(agent) => {
            if (agent.id === "command-center") {
              // Go home — deselect session
            }
            onAgentSelect?.(agent.id);
            onClose?.();
          }}
          onCreateNew={() => openPersonaEditor({ mode: "create" })}
          personas={personas}
          personasError={personasError}
          onRetryPersonas={() => void refreshPersonas()}
          activePersonaId={activePersonaId}
          onSelectPersona={(persona) => {
            setActivePersonaId(persona.id);
            setActiveTeamId(null);
            if (sessionId) void newChat();
            onAgentSelect?.("command-center");
            onClose?.();
          }}
          onEditPersona={(persona) => openPersonaEditor({ mode: "edit", personaId: persona.id })}
          teams={teams}
          teamsError={teamsError}
          onRetryTeams={() => void refreshTeams()}
          activeTeamId={activeTeamId}
          onSelectTeam={(team) => {
            setActiveTeamId(team.id);
            setActivePersonaId(team.agents[0] || "main");
            if (sessionId) void newChat();
            onAgentSelect?.("command-center");
            onClose?.();
          }}
        />
        <div className="mx-3 my-0.5 border-t border-line/30" />
        <ResourcesNav onNavigate={onAgentSelect} knowledgeEnabled={meta?.features?.knowledge !== false} />
        <div className="mx-3 my-0.5 border-t border-line/30" />
        <ProjectsList sessions={searchedSessions} />
      </div>
      <div className="mx-3 my-0.5 border-t border-line/40" />
      <div className="ka-sidebar-threads min-h-0 flex-1 overflow-y-auto">
        <RecentThreadsList
          sessions={filteredSessions}
          activeSessionId={sessionId}
          onForked={() => {
            onAgentSelect?.("command-center");
            onClose?.();
          }}
          onOpen={(id) => {
            // Reveal the conversation immediately. Opening a session also
            // restores optional remote/machine state, which can be slow.
            onAgentSelect?.("command-center");
            onClose?.();
            withViewTransition(() => {
              void openSession(id).catch(() => {});
            });
          }}
        />
      </div>

      <SidebarFooter />

      {personaEditor && (
        <PersonaEditorDialog
          mode={personaEditor.mode}
          persona={
            personaEditor.mode === "edit"
              ? personas.find((persona) => persona.id === personaEditor.personaId) || null
              : null
          }
          onClose={closePersonaEditor}
          onSaved={(agent) => {
            if (personaEditor.mode === "create" && agent?.id) {
              openPersonaEditor({ mode: "edit", personaId: agent.id });
              return;
            }
            closePersonaEditor();
          }}
        />
      )}
    </aside>
    {collapsedRail}
    </>
  );
}
