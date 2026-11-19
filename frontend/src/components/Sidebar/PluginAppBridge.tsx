import { useState } from "react";
import { api } from "../../api/client";

export type PluginApp = { name: string; connector_id?: string; integration?: string | null; capabilities?: string[]; bridge?: { server?: string; tools?: string[]; status?: string } };
type Server = { name: string; enabled?: boolean; revision?: number; tools?: { name: string; description?: string }[] };

/** An operator explicitly selects an existing server and the tools it provides for an app. */
export function PluginAppBridge({ plugin, packageHash, app, onChanged }: { plugin: string; packageHash: string; app: PluginApp; onChanged: () => Promise<void> }) {
  const [open, setOpen] = useState(false);
  const [servers, setServers] = useState<Server[]>([]);
  const [serverName, setServerName] = useState("");
  const [selected, setSelected] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const server = servers.find(row => row.name === serverName);
  async function show() {
    setBusy(true); setError("");
    try { const data = await api<{ servers: Server[] }>("/api/mcp"); setServers(data.servers || []); setOpen(true); }
    catch (e) { setError(e instanceof Error ? e.message : String(e)); }
    finally { setBusy(false); }
  }
  async function save() {
    if (!server || server.revision == null || !app.connector_id || !selected.length) return;
    setBusy(true); setError("");
    try {
      await api("/api/plugins/app-bridge", { method: "POST", body: JSON.stringify({ plugin, app: app.name, connector_id: app.connector_id, package_sha256: packageHash, server: server.name, expected_server_revision: server.revision, tools: selected }) });
      await onChanged(); setOpen(false);
    } catch (e) { setError(e instanceof Error ? e.message : String(e)); }
    finally { setBusy(false); }
  }
  async function remove() {
    setBusy(true); setError("");
    try { await api("/api/plugins/app-bridge", { method: "DELETE", body: JSON.stringify({ plugin, app: app.name, connector_id: app.connector_id }) }); await onChanged(); setOpen(false); }
    catch (e) { setError(e instanceof Error ? e.message : String(e)); }
    finally { setBusy(false); }
  }
  return <div className="space-y-1 border-t border-line pt-2">
    <p>App: {app.name} · {app.bridge?.server ? `MCP ${app.bridge.server} · ${app.bridge.status}` : "compatible connection required"}</p>
    {app.bridge?.tools?.length ? <p className="text-xs text-muted">Selected tools: {app.bridge.tools.join(", ")}</p> : null}
    {error && <p role="alert">{error}</p>}
    {!open && <button type="button" disabled={busy} className="text-accent underline" onClick={() => void show()}>Configure {app.name} connection</button>}
    {open && <div className="space-y-2">
      <p className="text-xs text-muted">Choose a compatible MCP connection and the operations to associate with this app. This does not change existing access to the server.</p>
      <label className="block">Connection <select aria-label={`Connection for ${app.name}`} value={serverName} onChange={e => { setServerName(e.target.value); setSelected([]); }} className="ml-2 bg-canvas"><option value="">Select connection</option>{servers.map(row => <option key={row.name} value={row.name}>{row.name}{row.enabled ? "" : " (disabled)"}</option>)}</select></label>
      {server && <fieldset><legend>Associated tools</legend>{(server.tools || []).map(tool => <label key={tool.name} className="block"><input type="checkbox" checked={selected.includes(tool.name)} onChange={() => setSelected(old => old.includes(tool.name) ? old.filter(name => name !== tool.name) : [...old, tool.name])} /> {tool.name}{tool.description ? ` — ${tool.description}` : ""}</label>)}</fieldset>}
      <button type="button" disabled={busy || !server || server.revision == null || !selected.length} className="mr-3 text-accent underline" onClick={() => void save()}>Save {app.name} connection</button>
      {app.bridge?.server && <button type="button" disabled={busy} className="mr-3 text-accent underline" onClick={() => void remove()}>Remove {app.name} connection</button>}
      <button type="button" disabled={busy} onClick={() => setOpen(false)}>Cancel</button>
    </div>}
  </div>;
}
