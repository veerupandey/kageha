import { useAppStore } from "../../store";
import { withViewTransition } from "../../lib/viewTransition";
import { AgentAvatar } from "../Sidebar/AgentAvatar";
import { HeroInput } from "./HeroInput";
import { QuickActions } from "./QuickActions";
import { ThreadCard } from "./ThreadCard";

export function CommandCenter() {
  const sessions = useAppStore((s) => s.sessions);
  const openSession = useAppStore((s) => s.openSession);
  const prefetchSession = useAppStore((s) => s.prefetchSession);
  const personas = useAppStore((s) => s.personas);
  const activePersonaId = useAppStore((s) => s.activePersonaId);
  const activePersona = personas.find((persona) => persona.id === activePersonaId);
  const customAgent = activePersona && !activePersona.builtin ? activePersona : null;

  const recentSessions = sessions
    .filter((session) => !session.archived)
    .filter((session) =>
      customAgent ? session.persona_id === customAgent.id : true,
    )
    .slice(0, 6);

  return (
    <div className="ka-command-center flex min-h-0 flex-1 flex-col items-center justify-center px-4 py-12">
      <div className="flex w-full max-w-[700px] flex-col items-center gap-8">
        <div className="flex flex-col items-center gap-3 text-center">
          {customAgent ? (
            <AgentAvatar
              avatar={customAgent.avatar}
              name={customAgent.name}
              size="lg"
            />
          ) : null}
          <div className="ka-command-center-kicker">
            {customAgent ? "Agent" : "Kageha · Workspace"}
          </div>
          <h1 className="ka-command-center-title text-4xl font-bold tracking-tight text-ink">
            {customAgent ? customAgent.name : "Where should we begin?"}
          </h1>
          {!customAgent && <p className="ka-home-description">Bring an idea. We’ll find the next step together.</p>}
          {customAgent ? (
            <p className="max-w-md text-sm leading-relaxed text-muted">
              {customAgent.description?.trim() ||
                "New threads started here use this agent's instructions, skills, and model preferences."}
            </p>
          ) : null}
        </div>

        <HeroInput />

        <QuickActions />

        {recentSessions.length > 0 ? (
          <div className="w-full pt-4">
            <div className="mb-3 flex items-center justify-between">
              <p className="text-sm font-medium text-muted">
                {customAgent ? "Threads with this agent" : "Recent threads"}
              </p>
            </div>
            <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
              {recentSessions.map((session) => (
                <ThreadCard
                  key={session.session_id}
                  session={session}
                  onClick={() => withViewTransition(() => void openSession(session.session_id))}
                  onMouseEnter={() => prefetchSession(session.session_id)}
                />
              ))}
            </div>
          </div>
        ) : customAgent ? (
          <p className="text-sm text-faint">No threads with this agent yet.</p>
        ) : null}
      </div>
    </div>
  );
}
