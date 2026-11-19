import { useId, useState, type ReactNode } from "react";
import { Icon } from "../lib/icons";

/** Keep connection controls mounted so collapsing never interrupts sign-in. */
export function IntegrationPanel({ name, description, connected, unavailable = false, attention, onOpen, icon, children }: {
  name: string; description: string; connected?: boolean; unavailable?: boolean; attention?: string; onOpen?: () => void; icon: ReactNode; children: ReactNode;
}) {
  const [expanded, setExpanded] = useState(false);
  const id = useId();
  return <section aria-label={`${name} integration`} className="overflow-hidden rounded-xl border border-line bg-canvas/40">
    <h3>
      <button type="button" aria-label={`${name} integration`} aria-expanded={expanded} aria-controls={id} onClick={() => { if (!expanded) onOpen?.(); setExpanded(!expanded); }}
        className="flex w-full items-center gap-2.5 px-3 py-3 text-left text-ink transition-colors hover:bg-surface focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-accent sm:px-4">
        <span className="flex h-9 w-9 shrink-0 items-center justify-center rounded-xl border border-line bg-surface">{icon}</span>
        <span className="min-w-0 flex-1"><span className="block text-sm font-semibold">{name}</span><span className="block text-xs font-normal text-muted">{description}</span></span>
        <span className="inline-flex max-w-[40%] shrink-0 items-center gap-1.5 text-right text-[0.6875rem] font-medium text-muted">
          <span className={`h-1.5 w-1.5 shrink-0 rounded-full ${unavailable || attention ? "bg-amber-500" : connected ? "bg-emerald-500" : "bg-current opacity-40"}`} />
          {unavailable ? "Unavailable" : attention ?? (connected === undefined ? "Loading" : connected ? "Connected" : "Not connected")}
        </span>
        <Icon.ChevronRight size={16} className={`shrink-0 ${expanded ? "rotate-90" : ""}`} />
      </button>
    </h3>
    <div id={id} hidden={!expanded} className="space-y-4 border-t border-line p-4 sm:p-5">{children}</div>
  </section>;
}

export function GitHubMark() {
  return <svg aria-hidden="true" viewBox="0 0 24 24" width="23" height="23" fill="currentColor"><path d="M12 .75a11.25 11.25 0 0 0-3.56 21.92c.56.1.77-.24.77-.54v-2.1c-3.13.68-3.79-1.33-3.79-1.33-.51-1.3-1.25-1.65-1.25-1.65-1.02-.7.08-.69.08-.69 1.13.08 1.73 1.16 1.73 1.16 1 .1e1 2.64.71 3.28.54.1-.73.39-1.23.71-1.51-2.5-.29-5.13-1.25-5.13-5.56 0-1.23.44-2.23 1.16-3.02-.12-.29-.5-1.43.11-2.98 0 0 .95-.3 3.09 1.15a10.8 10.8 0 0 1 5.62 0c2.15-1.45 3.09-1.15 3.09-1.15.62 1.55.23 2.69.12 2.98.72.79 1.15 1.79 1.15 3.02 0 4.32-2.63 5.27-5.14 5.55.4.35.76 1.03.76 2.08v3.51c0 .3.2.65.78.54A11.25 11.25 0 0 0 12 .75Z" /></svg>;
}
