import { useState, type ComponentType } from "react";
import type { AgentPersona } from "../../api/types";
import type { AgentTeam } from "../../api/types";
import { cn } from "../../lib/cn";
import { Icon, type IconProps } from "../../lib/icons";
import { StatusDot } from "../shared/StatusDot";
import type { DotStatus } from "../shared/StatusDot";
import { AgentAvatar } from "./AgentAvatar";
import { SidebarSection } from "./SidebarSection";

export interface AgentEntry {
  id: string;
  name: string;
  color: DotStatus;
  description?: string;
  icon?: ComponentType<IconProps>;
}

interface AgentsListProps {
  agents: AgentEntry[];
  onSelect: (agent: AgentEntry) => void;
  /** Called when the user clicks "+ New agent" — opens PersonaEditorDialog in create mode. */
  onCreateNew?: () => void;
  /** Custom personas fetched by the parent (Sidebar owns the fetch so it can
   * trigger a refresh after create/update/delete via PersonaEditorDialog). */
  personas: AgentPersona[];
  personasError?: string | null;
  onRetryPersonas?: () => void;
  /** Currently selected agent persona (Hyperagent: clicking a row enters that agent). */
  activePersonaId?: string;
  /** Opens that agent's home (Command Center bound to this persona). */
  onSelectPersona?: (persona: AgentPersona) => void;
  /** Opens PersonaEditorDialog in edit mode for the given persona. */
  onEditPersona?: (persona: AgentPersona) => void;
  /** Teams for the Teams section. */
  teams?: AgentTeam[];
  teamsError?: string | null;
  onRetryTeams?: () => void;
  /** Currently active team id (set when &team is active). */
  activeTeamId?: string | null;
  /** Called when the user clicks a team row — activates &team. */
  onSelectTeam?: (team: AgentTeam) => void;
}

const AGENT_ICONS: Record<string, ComponentType<IconProps>> = {
  "command-center": Icon.CommandCenter,
  jobs: Icon.Jobs,
  worktrees: Icon.Worktrees,
  project_brain: Icon.Brain,
  attach: Icon.Attach,
};

const AGENT_DESCRIPTIONS: Record<string, string> = {
  "command-center": "Main agent interface",
  jobs: "Background & queued runs",
  worktrees: "Git worktree branches",
  project_brain: "Project rules & context",
  attach: "File attachments",
};

const DEFAULT_AGENTS: AgentEntry[] = [
  { id: "command-center", name: "Command Center", color: "active", icon: Icon.CommandCenter, description: "Main agent interface" },
];

export function AgentsList({
  agents,
  onSelect,
  onCreateNew,
  personas,
  personasError,
  onRetryPersonas,
  activePersonaId,
  onSelectPersona,
  onEditPersona,
  teams,
  teamsError,
  onRetryTeams,
  activeTeamId,
  onSelectTeam,
}: AgentsListProps) {
  const [agentsCollapsed, setAgentsCollapsed] = useState(() => {
    try { return localStorage.getItem("kageha.sidebar.agents") !== "0"; } catch { return true; }
  });
  const [personasCollapsed, setPersonasCollapsed] = useState(() => {
    try { return localStorage.getItem("kageha.sidebar.personas") === "1"; } catch { return false; }
  });
  const [teamsCollapsed, setTeamsCollapsed] = useState(() => {
    try { return localStorage.getItem("kageha.sidebar.teams") === "1"; } catch { return false; }
  });
  const persist = (key: string, value: boolean) => {
    try { localStorage.setItem(`kageha.sidebar.${key}`, value ? "1" : "0"); } catch { /* private storage */ }
  };

  const items = agents.length > 0 ? agents : DEFAULT_AGENTS;

  // Enrich items with icons and descriptions
  const enriched = items.map((agent) => ({
    ...agent,
    icon: agent.icon || AGENT_ICONS[agent.id] || Icon.CommandCenter,
    description: agent.description || AGENT_DESCRIPTIONS[agent.id] || "",
  }));

  return (
    <div className="space-y-2 px-3 py-2">
      <SidebarSection label="Workspace" count={enriched.length} collapsed={agentsCollapsed} onToggle={() => { const next = !agentsCollapsed; setAgentsCollapsed(next); persist("agents", next); }}>
          <ul className="space-y-0.5">
            {enriched.map((agent) => {
              const AgentIcon = agent.icon!;
              return (
                <li key={agent.id}>
                  <button
                    type="button"
                    className="ka-sidebar-item group w-full"
                    onClick={() => onSelect(agent)}
                    title={agent.description}
                  >
                    <AgentIcon size={16} />
                    <span className="min-w-0 flex-1 truncate text-left">{agent.name}</span>
                    <StatusDot status={agent.color} />
                  </button>
                </li>
              );
            })}
          </ul>

      </SidebarSection>
      <SidebarSection
        label="Agents"
        count={personas.length}
        collapsed={personasCollapsed}
        onToggle={() => { const next = !personasCollapsed; setPersonasCollapsed(next); persist("personas", next); }}
        action={
          <button
            type="button"
            className="ka-icon-btn h-6 w-6 text-muted"
            aria-label="New agent"
            title="New agent"
            onClick={() => onCreateNew?.()}
          >
            <Icon.Plus size={14} />
          </button>
        }
      >
          {personasError ? (
            <div className="space-y-1 px-1 py-1">
              <p className="text-xs text-faint">Couldn&apos;t load agents.</p>
              {onRetryPersonas && (
                <button type="button" className="text-xs font-medium text-accent hover:underline" onClick={onRetryPersonas}>
                  Retry
                </button>
              )}
            </div>
          ) : (
            <ul className="space-y-0.5">
              {personas.map((persona) => {
                const selected = activePersonaId === persona.id;
                return (
                <li key={persona.id}>
                  <div
                    className={cn(
                      "ka-sidebar-item group w-full",
                      selected && "active",
                    )}
                  >
                    <button
                      type="button"
                      className="flex min-w-0 flex-1 items-center gap-2.5 text-left"
                      onClick={() => onSelectPersona?.(persona)}
                      title={persona.description || persona.name}
                      aria-label={persona.name}
                      aria-current={selected ? "page" : undefined}
                    >
                      <AgentAvatar avatar={persona.avatar} name={persona.name} />
                      <span className="min-w-0 flex-1">
                        <span className="flex items-center gap-1.5">
                          <span className="min-w-0 truncate font-medium">{persona.name}</span>
                          {persona.builtin && (
                            <span className="shrink-0 rounded-full bg-accent/10 px-1.5 py-0.5 text-[8.5px] font-medium tracking-wide text-accent border border-accent/20" aria-hidden="true">
                              Default
                            </span>
                          )}
                        </span>
                        {persona.description ? (
                          <span className="mt-0.5 block truncate text-[0.68rem] font-normal text-faint">
                            {persona.description}
                          </span>
                        ) : null}
                      </span>
                    </button>
                    <button
                      type="button"
                      className="ka-icon-btn h-6 w-6 shrink-0 opacity-0 group-hover:opacity-100 group-focus-within:opacity-100 focus:opacity-100"
                      aria-label={`Edit ${persona.name}`}
                      title={persona.builtin ? "Configure Default knowledge" : "Configure agent"}
                      onClick={(event) => {
                        event.stopPropagation();
                        onEditPersona?.(persona);
                      }}
                    >
                      <Icon.Settings size={14} />
                    </button>
                  </div>
                </li>
                );
              })}
            </ul>
          )}
      </SidebarSection>

      {/* ── Teams section ── */}
      {((teams && teams.length > 0) || teamsError) && (
        <SidebarSection
          label="Teams"
          count={teams?.length ?? 0}
          collapsed={teamsCollapsed}
          onToggle={() => {
            const next = !teamsCollapsed;
            setTeamsCollapsed(next);
            persist("teams", next);
          }}
        >
          {teamsError ? (
            <div className="space-y-1 px-1 py-1">
              <p className="text-xs text-faint">Couldn&apos;t load teams.</p>
              {onRetryTeams && (
                <button
                  type="button"
                  className="text-xs font-medium text-accent hover:underline"
                  onClick={onRetryTeams}
                >
                  Retry
                </button>
              )}
            </div>
          ) : (
            <ul className="space-y-0.5">
              {(teams ?? []).map((team) => {
                const selected = activeTeamId === team.id;
                const modeIcon = team.mode === "parallel" ? "⇉" : team.mode === "sequence" ? "→" : "⌘";
                return (
                  <li key={team.id}>
                    <button
                      type="button"
                      className={cn(
                        "ka-sidebar-item group w-full text-left",
                        selected && "active",
                      )}
                      onClick={() => onSelectTeam?.(team)}
                      title={team.description || `&${team.id} — ${team.agents.join(", ")}`}
                      aria-current={selected ? "page" : undefined}
                    >
                      <span className="inline-flex h-5 w-5 shrink-0 items-center justify-center rounded-md bg-line/70 text-[0.65rem] font-semibold text-ink">
                        {modeIcon}
                      </span>
                      <span className="min-w-0 flex-1">
                        <span className="block truncate font-medium">{team.name}</span>
                        <span className="block truncate text-[0.65rem] text-faint">
                          &amp;{team.id} · {team.agents.map((a) => `@${a}`).join(", ")}
                        </span>
                      </span>
                    </button>
                  </li>
                );
              })}
            </ul>
          )}
        </SidebarSection>
      )}
    </div>
  );
}
