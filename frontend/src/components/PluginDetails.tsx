import { useEffect, useState } from "react";
import { PluginConnectionCard } from "./PluginConnectionCard";
import { api } from "../api/client";

interface Details {
  name: string; version: string; enabled: boolean; description: string;
  format?: string; warnings?: string[];
  skills: { name: string; description: string }[];
  servers: { name: string; transport: string; enabled: boolean }[];
  connections?: { id: string }[];
  hooks: { event: string; matcher: string }[];
  mods: { mod: string; callbacks: string[] }[];
}

export function PluginDetails({ name, onClose, onConnectionChange }: { name: string; onClose: () => void; onConnectionChange?: () => void }) {
  const [details, setDetails] = useState<Details | null>(null);
  const [error, setError] = useState("");
  useEffect(() => {
    let cancelled = false;
    setDetails(null); setError("");
    void api<Details>(`/api/plugins/${encodeURIComponent(name)}`).then((data) => {
      if (!cancelled) setDetails(data);
    }).catch((e) => { if (!cancelled) setError(String(e)); });
    return () => { cancelled = true; };
  }, [name]);
  return <section aria-label={`Plugin details: ${name}`} className="space-y-3 rounded border border-line p-3 text-sm">
    <div className="flex justify-between gap-3"><h3 className="font-semibold">{name}</h3><button onClick={onClose}>Close details</button></div>
    {error && <p role="alert">{error}</p>}
    {!details && !error && <p role="status">Loading package contents…</p>}
    {details && <>
      <p>{details.version} · {details.enabled ? "Enabled" : "Disabled"}</p>
      <p className="text-muted">{details.description}</p>
      {details.warnings?.map((warning) => <p key={warning} role="status" className="text-amber-500">{warning}</p>)}
      {details.connections?.map(connection => <PluginConnectionCard key={connection.id} id={connection.id} onStateChange={status => { if (status === "ready") setDetails(old => old ? { ...old, enabled: true } : old); onConnectionChange?.(); }} />)}
      <h4 className="font-medium">Skills ({details.skills.length})</h4>
      {details.skills.map((skill) => <p key={skill.name}>{skill.name} <span className="text-muted">{skill.description}</span></p>)}
      {!details.connections?.length && <><h4 className="font-medium">MCP connections ({details.servers.length})</h4>
      {details.servers.map((server) => <p key={server.name}>{server.name} · {server.transport} · {server.enabled ? "Enabled" : "Disabled"}</p>)}</>}
      <h4 className="font-medium">Hooks ({details.hooks.length})</h4>
      {details.hooks.map((hook, index) => <p key={`${hook.event}-${index}`}>{hook.event}{hook.matcher ? ` · ${hook.matcher}` : ""}</p>)}
      <h4 className="font-medium">Mods ({details.mods.length})</h4>
      {details.mods.map((mod) => <p key={mod.mod}>{mod.mod} · {mod.callbacks.join(", ")}</p>)}
    </>}
  </section>;
}
