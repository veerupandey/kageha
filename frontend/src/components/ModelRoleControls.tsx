import { useAppStore } from "../store";
import { Icon } from "../lib/icons";
import { AgentAvatar } from "./Sidebar/AgentAvatar";

interface ModelRoleControlsProps {
  compact?: boolean;
}

export function ModelRoleControls({ compact = false }: ModelRoleControlsProps) {
  const personas = useAppStore((s) => s.personas);
  const activePersonaId = useAppStore((s) => s.activePersonaId);
  const setActivePersonaId = useAppStore((s) => s.setActivePersonaId);
  const sessionId = useAppStore((s) => s.sessionId);
  const openPersonaEditor = useAppStore((s) => s.openPersonaEditor);
  const personaLocked = Boolean(sessionId);
  const activePersona = personas.find((persona) => persona.id === activePersonaId);
  const selectClass = compact
    ? "max-w-[10rem] rounded-md border border-line bg-transparent px-1.5 py-1 text-xs text-muted outline-none"
    : "min-w-[8rem] rounded-md border border-line bg-transparent px-2 py-1 text-xs text-muted outline-none";

  return (
    <div className="flex flex-wrap items-center gap-1.5" aria-label="Agent selection">
      <label className="inline-flex items-center gap-1 text-xs text-faint">
        Agent
        {personaLocked ? (
          <span
            className="inline-flex max-w-[10rem] items-center gap-1 rounded-md border border-line px-1.5 py-1 text-xs text-muted"
            title="This thread is bound to this agent"
          >
            {activePersona ? (
              <AgentAvatar avatar={activePersona.avatar} name={activePersona.name} />
            ) : null}
            <span className="truncate">{activePersona?.name || "Default"}</span>
          </span>
        ) : (
          <select
            id="persona-select"
            className={selectClass}
            title="Select the agent for this session"
            value={activePersonaId}
            onChange={(event) => setActivePersonaId(event.target.value)}
          >
            {personas.map((persona) => (
              <option key={persona.id} value={persona.id}>
                {persona.avatar ? `${persona.avatar} ` : ""}
                {persona.name}
              </option>
            ))}
          </select>
        )}
        <button
          type="button"
          className="inline-flex h-6 w-6 items-center justify-center rounded-md text-faint hover:bg-line/60 hover:text-ink"
          aria-label={
            activePersona && !activePersona.builtin
              ? `Edit ${activePersona.name}`
              : "New agent"
          }
          title={
            activePersona && !activePersona.builtin
              ? `Configure ${activePersona.name}`
              : "Create a custom agent"
          }
          onClick={() => {
            if (activePersona && !activePersona.builtin) {
              openPersonaEditor({ mode: "edit", personaId: activePersona.id });
            } else {
              openPersonaEditor({ mode: "create" });
            }
          }}
        >
          <Icon.Settings size={13} />
        </button>
      </label>
    </div>
  );
}
