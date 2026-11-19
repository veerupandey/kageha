import { useEffect, useId, useState } from "react";
import { api } from "../api/client";
import { usePluginRuntime } from "../lib/usePluginRuntime";
import { McpSettings } from "./McpSettings";
import { SkillsSettings } from "./SkillsSettings";
import { HooksPanel } from "./Sidebar/HooksPanel";
import { PluginsPanel } from "./Sidebar/PluginsPanel";
import { PluginPanes } from "./Sidebar/PluginPanes";

type View = "plugins" | "skills" | "connections" | "hooks";
const VIEWS: { id: View; label: string }[] = [
  { id: "plugins", label: "Plugins" }, { id: "skills", label: "Skills" },
  { id: "connections", label: "Connections" }, { id: "hooks", label: "Hooks & Mods" },
];

export function ExtensionsPanel({ onClose }: { onClose?: () => void }) {
  const id = useId();
  const [view, setView] = useState<View>("plugins");
  const [plugin, setPlugin] = useState<string>();
  const openPlugin = (name: string) => { setPlugin(name); setView("plugins"); };
  return <div className="flex h-full min-h-0 flex-col bg-surface">
    <header className="flex items-center justify-between border-b border-line px-4 py-3">
      <h2 className="font-semibold text-ink">Extensions</h2>
      {onClose && <button onClick={onClose} aria-label="Close extensions">✕</button>}
    </header>
    <div role="tablist" aria-label="Extension views" className="flex flex-wrap gap-2 border-b border-line p-3">
      {VIEWS.map((row) => <button key={row.id} role="tab" id={`${id}-tab-${row.id}`}
        tabIndex={view === row.id ? 0 : -1}
        aria-selected={view === row.id} aria-controls={`${id}-view-${row.id}`}
        className={`rounded px-3 py-2 text-sm ${view === row.id ? "bg-accent-soft text-accent" : "text-muted"}`}
        onKeyDown={(event) => {
          const index = VIEWS.findIndex((item) => item.id === view);
          const next = event.key === "ArrowRight" ? (index + 1) % VIEWS.length
            : event.key === "ArrowLeft" ? (index + VIEWS.length - 1) % VIEWS.length
            : event.key === "Home" ? 0 : event.key === "End" ? VIEWS.length - 1 : -1;
          if (next < 0) return;
          event.preventDefault(); setView(VIEWS[next].id); setPlugin(undefined);
          document.getElementById(`${id}-tab-${VIEWS[next].id}`)?.focus();
        }}
        onClick={() => { setView(row.id); setPlugin(undefined); }}>{row.label}</button>)}
    </div>
    <div role="tabpanel" id={`${id}-view-${view}`} aria-labelledby={`${id}-tab-${view}`} className="min-h-0 flex-1 overflow-auto">
      {view === "plugins" && <PluginsPanel initialPlugin={plugin} />}
      {view === "skills" && <div className="p-4"><SkillsSettings onOpenPlugin={openPlugin} /></div>}
      {view === "connections" && <div className="p-4"><McpSettings onOpenPlugin={openPlugin} /></div>}
      {view === "hooks" && <><ModsView onOpenPlugin={openPlugin} /><HooksPanel onOpenPlugin={openPlugin} /></>}
    </div>
  </div>;
}

function ModsView({ onOpenPlugin }: { onOpenPlugin: (name: string) => void }) {
  const [mods, setMods] = useState<{ mod: string; plugin: string; enabled: boolean; callbacks: string[] }[]>([]);
  const [error, setError] = useState("");
  const { sessionId, runtime, runtimeError } = usePluginRuntime();
  useEffect(() => {
    let cancelled = false;
    void api<{ mods: typeof mods }>("/api/extensions/mods").then((data) => {
      if (!cancelled) setMods(data.mods);
    }).catch((e) => { if (!cancelled) setError(String(e)); });
    return () => { cancelled = true; };
  }, []);
  return <section aria-label="Mods" className="space-y-3 border-b border-line p-4 text-sm">
    <h3 className="font-semibold">Mods</h3>
    {error && <p role="alert">{error}</p>}
    {mods.map((mod) => <div key={mod.mod} className="rounded border border-line p-3">
      <p>{mod.mod} · {mod.enabled ? "Enabled" : "Disabled"}</p>
      <p className="text-xs text-muted">{mod.callbacks.join(", ")}</p>
      <button className="text-accent underline" onClick={() => onOpenPlugin(mod.plugin)}>Plugin: {mod.plugin}</button>
    </div>)}
    {!sessionId && <p className="text-muted">Open a session to see recorded mod health and panes.</p>}
    {runtimeError && <p role="alert">{runtimeError}</p>}
    {(runtime?.workers ?? []).map((worker) => <p key={worker.mod}>Last recorded: {worker.mod} · {worker.status}{worker.last_error ? ` · ${worker.last_error}` : ""}</p>)}
    {(runtime?.mod_diagnostics ?? []).map((item, index) => <p key={`${item.at}-${index}`} className="text-[var(--color-danger)]">{item.mod} · {item.callback}: {item.message}</p>)}
    <PluginPanes panes={runtime?.panes ?? []} />
  </section>;
}
