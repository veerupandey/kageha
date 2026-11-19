import { useCallback, useEffect, useState } from "react";
import { api } from "../../api/client";
import { PluginCatalog } from "./PluginCatalog";
import { PluginAppBridge, type PluginApp } from "./PluginAppBridge";

type Hook = { event?: string; matcher?: string; matcher_type?: string; command?: string; http?: string; timeout_s?: number; status_message?: string | null; resources?: Record<string, string> };

function HookDetails({ hook }: { hook: Hook }) {
  return <>
    {hook.matcher && hook.matcher_type === "regex" && <p>Matcher type: regular expression</p>}
    {hook.status_message && <p>Status: {hook.status_message}</p>}
    {hook.command && <p>Command: <code className="break-all whitespace-pre-wrap">{hook.command}</code></p>}
    {hook.http && <p>Endpoint: <code className="break-all">{hook.http}</code></p>}
    {Object.entries(hook.resources || {}).map(([path, contents]) => <details key={path} className="text-xs"><summary className="cursor-pointer">Review hook resource: {path}</summary><pre className="max-h-72 overflow-auto whitespace-pre-wrap break-all">{contents}</pre></details>)}
  </>;
}

interface Plugin {
  name: string; status: string; preview_id?: string; owner?: string; source?: string; commit?: string; package_sha256?: string;
  skills?: { name: string; sha256?: string }[];
  mcp?: { name: string; transport: string; configuration_status?: string; enabled?: boolean | null; recorded_connected?: boolean | null }[];
  apps?: PluginApp[];
  agents?: { id: string; name: string; description?: string }[];
  commands?: { name: string; description?: string }[];
  hooks?: Hook[];
  hooks_enabled?: boolean;
  unsupported?: string[];
}
interface Preview {
  preview_id: string; name: string; source: string; commit: string; package_sha256: string;
  skills: { name: string; sha256: string }[];
  mcp: { name: string; transport: string; endpoint?: string; command?: string; args?: string[]; enabled: boolean; oauth?: { client_id?: string; callback_url?: string; client_secret_required?: boolean } }[];
  apps?: (PluginApp & { setup_required?: boolean })[];
  agents?: { id: string; name: string; description?: string }[];
  commands?: { name: string; description?: string }[];
  hooks?: Hook[];
  unsupported: string[]; installable: boolean;
}

function mcpStatus(server: NonNullable<Plugin["mcp"]>[number]): string {
  if (server.configuration_status === "removed") return "configuration removed";
  if (server.configuration_status === "replaced") return "configuration replaced";
  if (!server.enabled) return "disabled · setup required";
  if (server.recorded_connected === true) return "enabled · last recorded connection succeeded";
  if (server.recorded_connected === false) return "enabled · last recorded connection unavailable";
  return "enabled · connection not checked";
}

export function PluginsPanel({ onClose, initialPlugin }: { onClose?: () => void; initialPlugin?: string }) {
  const [plugins, setPlugins] = useState<Plugin[]>([]);
  const [showCatalog, setShowCatalog] = useState(false);
  const [name, setName] = useState(initialPlugin ?? "");
  const [preview, setPreview] = useState<Preview | null>(null);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const [error, setError] = useState("");
  const refresh = useCallback(async () => {
    const data = await api<{ plugins: Plugin[] }>("/api/plugins");
    setPlugins(data.plugins);
  }, []);
  useEffect(() => { void refresh().catch((e) => setError(e instanceof Error ? e.message : String(e))); }, [refresh]);
  useEffect(() => { if (initialPlugin) setName(initialPlugin); }, [initialPlugin]);
  const review = async (pluginName: string) => {
    setBusy(true); setError(""); setMessage(""); setPreview(null);
    try {
      const result = await api<Preview>("/api/plugins/preview", { method: "POST", body: JSON.stringify({ name: pluginName }) });
      setName(pluginName); setPreview(result);
    } catch (e) { setError(e instanceof Error ? e.message : String(e)); }
    finally { setBusy(false); }
  };
  const reviewIncomplete = async (plugin: Plugin) => {
    if (!plugin.preview_id || plugin.owner !== "operator") return;
    setBusy(true); setError(""); setMessage(""); setPreview(null);
    try {
      const result = await api<Preview>(`/api/plugins/preview?id=${encodeURIComponent(plugin.preview_id)}`);
      setName(plugin.name); setPreview(result);
      setMessage("This installation was interrupted. Review the original package before resuming.");
    } catch (e) { setError(e instanceof Error ? e.message : String(e)); }
    finally { setBusy(false); }
  };
  const install = async () => {
    if (!preview || !preview.installable) return;
    setBusy(true); setError(""); setMessage("");
    try {
      await api("/api/plugins/install", { method: "POST", body: JSON.stringify({ preview_id: preview.preview_id, approved_summary: preview }) });
      setMessage(`${preview.name} installed. Review its component status below.`);
      setPreview(null);
      try { await refresh(); }
      catch { setError("The package was installed, but the plugin list could not be refreshed."); }
    } catch (e) { setError(e instanceof Error ? e.message : String(e)); }
    finally { setBusy(false); }
  };
  const setHooks = async (plugin: Plugin, enabled: boolean) => {
    setBusy(true); setError(""); setMessage("");
    try {
      await api("/api/plugins/hooks", { method: "POST", body: JSON.stringify({ name: plugin.name, enabled }) });
      await refresh(); setMessage(`${plugin.name} hooks ${enabled ? "enabled" : "disabled"}.`);
    } catch (e) { setError(e instanceof Error ? e.message : String(e)); }
    finally { setBusy(false); }
  };
  return <div className="flex h-full flex-col bg-surface">
    <div className="flex items-center justify-between border-b border-line px-4 py-3">
      <h2 className="text-sm font-semibold text-ink">Plugins</h2>
      {onClose && <button onClick={onClose}>Close plugins</button>}
    </div>
    <div className="space-y-4 overflow-auto p-4">
      <p className="text-sm text-muted">Review the package and its supported components before installing. A configured connection may require separate setup.</p>
      <button type="button" aria-expanded={showCatalog} onClick={() => setShowCatalog((value) => !value)} className="text-sm text-accent">{showCatalog ? "Hide plugin catalog" : "Browse plugin catalog"}</button>
      {showCatalog && <PluginCatalog installed={plugins.filter((plugin) => plugin.status === "installed").map((plugin) => plugin.name)} installing={plugins.filter((plugin) => plugin.status === "installing").map((plugin) => plugin.name)} busy={busy} onPreview={(pluginName) => void review(pluginName)} />}
      <form className="flex gap-2" onSubmit={(e) => { e.preventDefault(); void review(name.trim()); }}>
        <input aria-label="Plugin name" placeholder="Plugin name" value={name} onChange={(e) => setName(e.target.value)} className="min-w-0 flex-1 rounded border border-line bg-canvas px-3 py-2 text-sm" />
        <button type="submit" disabled={busy || !name.trim()} className="rounded bg-accent px-3 text-sm text-white disabled:opacity-50">Review plugin</button>
      </form>
      {error && <p role="alert">{error}</p>}
      {message && <p role="status">{message}</p>}
      {preview && <section aria-label={`Review ${preview.name}`} className="space-y-2 rounded border border-line p-3 text-sm">
        <h3 className="font-semibold">Review {preview.name}</h3>
        <p>Source: {preview.source}</p>
        <p>Commit: <code>{preview.commit}</code></p>
        <p>Package SHA-256: <code>{preview.package_sha256}</code></p>
        <h4 className="font-medium">Skills ({preview.skills.length})</h4>
        {preview.skills.map((skill) => <p key={skill.name}>{skill.name}</p>)}
        {(preview.apps?.length ?? 0) > 0 && <><h4 className="font-medium">App connections</h4>{preview.apps?.map((app) => <p key={app.name}>{app.integration ? `${app.name} · ${app.integration} · ${(app.capabilities || []).join(", ")} · account setup required` : `${app.name} · compatible connection required`}</p>)}</>}
        {(preview.agents?.length ?? 0) > 0 && <><h4 className="font-medium">Specialists</h4>{preview.agents?.map((agent) => <p key={agent.id}>{agent.name} · {agent.id}</p>)}</>}
        {(preview.commands?.length ?? 0) > 0 && <><h4 className="font-medium">Commands</h4>{preview.commands?.map((command) => <p key={command.name}>{command.name} · {command.description}</p>)}</>}
        {(preview.hooks?.length ?? 0) > 0 && <><h4 className="font-medium">Hooks · disabled until enabled</h4>{preview.hooks?.map((hook, index) => <div key={index}><p>{hook.event}{hook.matcher ? ` · ${hook.matcher}` : ""}</p><HookDetails hook={hook} /></div>)}</>}
        <h4 className="font-medium">MCP connections ({preview.mcp.length})</h4>
        {preview.mcp.map((server) => <div key={server.name} className="space-y-1">
          <p>{server.name} · {server.transport}{server.endpoint ? ` · ${server.endpoint}` : ""} · disabled, requires setup</p>
          {server.oauth && <p>{server.oauth.client_id ? <>OAuth client: <code className="break-all">{server.oauth.client_id}</code></> : "OAuth client registration required"} · callback: <code className="break-all">{server.oauth.callback_url}</code> · user authorization required</p>}
          {server.transport === "stdio" && <div>
            <p>Command: <code className="break-all">{server.command}</code></p>
            <p>Arguments:</p>
            {server.args?.length ? <ol aria-label={`Arguments for ${server.name}`} className="list-decimal space-y-1 pl-6">{server.args.map((arg, index) => <li key={index}><code className="break-all whitespace-pre-wrap">{arg}</code></li>)}</ol> : <p>None</p>}
            <p className="text-muted">This command is not run during installation.</p>
          </div>}
        </div>)}
        {preview.unsupported.length > 0 && <><h4 className="font-medium">Unsupported components</h4>{preview.unsupported.map((item, index) => <p key={index} className="text-amber-500">{item}</p>)}</>}
        {!preview.installable && <p role="status">This package has no installable components.</p>}
        <div className="flex gap-3"><button type="button" disabled={busy || !preview.installable} onClick={() => void install()} className="rounded bg-accent px-3 py-1 text-white disabled:opacity-50">Install reviewed package</button><button type="button" disabled={busy} onClick={() => setPreview(null)}>Cancel</button></div>
      </section>}
      <section aria-label="Installed plugins" className="space-y-3">
        <h3 className="text-sm font-semibold">Installed plugins</h3>
        {!plugins.length && <p className="text-sm text-muted">No plugins installed.</p>}
        {plugins.map((plugin) => <article key={plugin.name} role="region" aria-label={`Plugin details: ${plugin.name}`} className="space-y-1 rounded border border-line p-3 text-sm">
          <p className="font-medium">{plugin.name} · {plugin.status === "installing" ? "installation interrupted or in progress" : plugin.status}</p>
          {plugin.status === "installing" && plugin.preview_id && plugin.owner === "operator" && <button type="button" disabled={busy} onClick={() => void reviewIncomplete(plugin)} className="text-accent">Review and resume</button>}
          {plugin.status === "installing" && plugin.owner !== "operator" && <p className="text-muted">Return to the chat that started this installation to continue it.</p>}
          {plugin.commit && <p className="text-xs text-muted">Commit: {plugin.commit}</p>}
          {(plugin.skills ?? []).map((skill) => <p key={skill.name}>Skill: {skill.name}</p>)}
          {(plugin.apps ?? []).map((app) => app.integration ? <p key={app.name}>App: {app.name} · {app.integration} · {(app.capabilities || []).join(", ")} (check account access in Integrations)</p> : plugin.package_sha256 && app.connector_id ? <PluginAppBridge key={app.name} plugin={plugin.name} packageHash={plugin.package_sha256} app={app} onChanged={refresh} /> : <p key={app.name}>App: {app.name} · compatible connection required</p>)}
          {(plugin.agents ?? []).map((agent) => <p key={agent.id}>Specialist: {agent.name}</p>)}
          {(plugin.commands ?? []).map((command) => <p key={command.name}>Command: {command.name}</p>)}
          {(plugin.hooks ?? []).map((hook, index) => <div key={index}><p>Hook: {hook.event}{hook.matcher ? ` · ${hook.matcher}` : ""} · {plugin.hooks_enabled ? "enabled" : "disabled"}</p><HookDetails hook={hook} /></div>)}
          {plugin.status === "installed" && (plugin.hooks?.length ?? 0) > 0 && <button type="button" disabled={busy} className="text-accent underline" onClick={() => void setHooks(plugin, !plugin.hooks_enabled)}>{plugin.hooks_enabled ? "Disable" : "Enable"} {plugin.name} hooks</button>}
          {(plugin.mcp ?? []).map((server) => <p key={server.name}>MCP: {server.name} · {mcpStatus(server)}</p>)}
          {(plugin.unsupported ?? []).map((item, index) => <p key={index} className="text-amber-500">Unsupported: {item}</p>)}
        </article>)}
      </section>
    </div>
  </div>;
}
