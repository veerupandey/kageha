import { useEffect, useRef, useState } from "react";
import { api } from "../api/client";

interface Field { key: string; title: string; description: string; type: string; sensitive?: boolean; required?: boolean; configured?: boolean; value?: unknown; multiple?: boolean; options?: string[] }
interface Connection { id: string; status: string; message: string; fields: Field[]; oauth: boolean; requires_client: boolean; client_configured: boolean; callback_uri: string; authorization_url?: string; tools?: string[]; account?: string; adapter?: string; write_access?: boolean; activation_required?: boolean; verification_note?: string; access_note?: string }
const labels: Record<string, string> = { disabled: "Disabled", ready: "Ready", connecting: "Connecting", needs_setup: "Setup required", needs_connection: "Connect account", administrator_setup_required: "Administrator setup required", reconnect_required: "Reconnect required", disconnected: "Disconnected" };

/** Same lifecycle in Settings and a paused agent task. Never sends secrets to chat. */
export function PluginConnectionCard({ id, purpose, onReady, onCancel, onStateChange }: { id: string; purpose?: string; onReady?: () => void; onCancel?: () => void; onStateChange?: (status: string) => void }) {
  const [connection, setConnection] = useState<Connection | null>(null);
  const [values, setValues] = useState<Record<string, unknown>>({});
  const [clientId, setClientId] = useState("");
  const [clientSecret, setClientSecret] = useState("");
  const [writeAccess, setWriteAccess] = useState<boolean | null>(null);
  const [executionApproved, setExecutionApproved] = useState(false);
  const [adminOpen, setAdminOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const readyCallback = useRef(onReady);
  readyCallback.current = onReady;
  const notified = useRef(false);
  const stateCallback = useRef(onStateChange);
  stateCallback.current = onStateChange;
  const previousStatus = useRef("");
  const actionPending = useRef(false);
  const revision = useRef(0);
  const base = `/api/plugin-connections/${encodeURIComponent(id)}`;
  useEffect(() => {
    let cancelled = false;
    notified.current = false;
    previousStatus.current = "";
    const refresh = async () => {
      try {
        if (actionPending.current) return;
        const started = revision.current;
        const next = await api<Connection>(base);
        if (cancelled || actionPending.current || started !== revision.current) return;
        setConnection(next);
        if (previousStatus.current !== next.status) { previousStatus.current = next.status; stateCallback.current?.(next.status); }
        if (next.status === "ready" && !notified.current) { notified.current = true; readyCallback.current?.(); }
      } catch (e) { if (!cancelled) setError(e instanceof Error ? e.message : String(e)); }
    };
    void refresh();
    const timer = window.setInterval(() => { void refresh(); }, 2000);
    return () => { cancelled = true; window.clearInterval(timer); };
  }, [base]);
  const act = async (action: string, setup = false) => {
    if (actionPending.current) return;
    actionPending.current = true; revision.current += 1;
    setBusy(true); setError("");
    try {
      if (setup && (Object.keys(values).length || clientId || writeAccess !== null || executionApproved)) {
        await api(base + "/configure", { method: "POST", body: JSON.stringify({ execution_approved: executionApproved, values, client_id: clientId, client_secret: clientSecret, write_access: writeAccess ?? connection?.write_access ?? false }) });
        setValues({}); setClientId(""); setClientSecret(""); setWriteAccess(null);
      }
      const next = await api<Connection>(base + "/" + action, { method: "POST" });
      setConnection(next);
      if (previousStatus.current !== next.status) { previousStatus.current = next.status; stateCallback.current?.(next.status); }
      if (action === "cancel") onCancel?.();
    } catch (e) { setError(e instanceof Error ? e.message : String(e)); }
    finally { actionPending.current = false; setBusy(false); }
  };
  return <section aria-label={`Connection ${id}`} className="space-y-3 rounded-xl border border-line bg-surface p-4 text-sm">
    <div className="flex flex-wrap items-center justify-between gap-2"><strong>{id.split(":").at(-1)?.replaceAll("-", " ")}</strong><span role="status">{connection ? labels[connection.status] || connection.status : "Checking connection…"}</span></div>
    {purpose && <p>{purpose}</p>}
    {error && <p role="alert" className="text-[var(--color-danger)]">{error}</p>}
    {connection && <>
      {connection.adapter === "Kageha Calendar API" && <p className="text-muted">Connect through Kageha’s Calendar API integration. View events and availability; scheduling requires additional permission.</p>}
      {connection.message && <p>{connection.message}</p>}
      {connection.account && <p>Connected account: {connection.account}</p>}
      {connection.status !== "ready" && (connection.fields || []).map(field => <label key={field.key} className="block space-y-1">
        <span>{field.title}{field.required ? " *" : ""}</span>
        {field.type === "boolean" ? <input type="checkbox" checked={Boolean(values[field.key] ?? field.value)} onChange={e => setValues(v => ({ ...v, [field.key]: e.target.checked }))} /> : field.options ? <select multiple={field.multiple} value={field.multiple ? (values[field.key] ?? field.value ?? []) as string[] : String(values[field.key] ?? field.value ?? "")} onChange={e => setValues(v => ({ ...v, [field.key]: field.multiple ? Array.from(e.target.selectedOptions, option => option.value) : e.target.value }))}><option value="">Choose…</option>{field.options.map(option => <option key={option}>{option}</option>)}</select> : <input className="w-full rounded border border-line bg-canvas px-3 py-2" type={field.sensitive ? "password" : field.type === "number" ? "number" : "text"} autoComplete="off" value={String(values[field.key] ?? (field.sensitive ? "" : field.value ?? ""))} placeholder={field.configured ? "Saved securely" : field.multiple ? "Comma-separated values" : ""} onChange={e => setValues(v => ({ ...v, [field.key]: field.type === "number" ? Number(e.target.value) : field.multiple ? e.target.value.split(",").map(s => s.trim()) : e.target.value }))} />}
        <span className="block text-xs text-muted">{field.description}</span>
      </label>)}
      {connection.requires_client && !connection.client_configured && <p className="text-muted">This server needs an OAuth application registered for Kageha. An administrator must configure it before you can sign in.</p>}
      {connection.oauth && connection.status !== "ready" && <>
        <button className="min-h-11 underline" onClick={() => setAdminOpen(v => !v)} aria-expanded={adminOpen}>Application setup</button>
        {adminOpen && <div className="space-y-2"><p className="text-xs">Register this exact callback URL with the provider: <code className="break-all">{connection.callback_uri}</code></p><label className="block">Client ID<input className="block w-full rounded border border-line bg-canvas p-2" value={clientId} onChange={e => setClientId(e.target.value)} autoComplete="off" /></label><label className="block">Client secret<input className="block w-full rounded border border-line bg-canvas p-2" type="password" value={clientSecret} onChange={e => setClientSecret(e.target.value)} autoComplete="off" /></label></div>}
      </>}
      {connection.adapter === "Kageha Calendar API" && <label className="flex items-center gap-2"><input type="checkbox" checked={writeAccess ?? connection.write_access ?? false} onChange={e => setWriteAccess(e.target.checked)} />Allow scheduling and changing events</label>}
      {connection.authorization_url && connection.status === "connecting" && <a className="inline-flex min-h-11 items-center rounded bg-accent px-4 text-white" href={connection.authorization_url} target="_blank" rel="noopener noreferrer">{connection.adapter === "Kageha Calendar API" ? "Continue with Google" : "Continue sign-in"}</a>}
      {connection.activation_required && <label className="flex items-start gap-2"><input type="checkbox" checked={executionApproved} onChange={e => setExecutionApproved(e.target.checked)} /><span>I trust this plugin and approve enabling its tools, skills, hooks, and code. Local servers and hooks can run commands on the Kageha host.</span></label>}
      <div className="flex flex-wrap gap-3">
        {connection.status !== "connecting" && (connection.status !== "ready" || writeAccess !== null) && <button className="min-h-11 rounded bg-accent px-4 text-white disabled:opacity-50" disabled={busy || (connection.activation_required && !executionApproved) || (connection.requires_client && !connection.client_configured && !clientId)} onClick={() => void act("connect", true)}>{busy ? "Checking…" : connection.status === "disabled" ? "Enable and verify" : connection.oauth ? "Connect account" : "Set up and verify"}</button>}
        {connection.status === "connecting" && <button className="min-h-11" disabled={busy} onClick={() => void act("cancel")}>Cancel connection</button>}
        {connection.status === "ready" && <button className="min-h-11" disabled={busy} onClick={() => void act("disconnect")}>Disconnect</button>}
        {onCancel && connection.status !== "connecting" && <button className="min-h-11" disabled={busy} onClick={onCancel}>Not now</button>}
      </div>
      {connection.status === "ready" && <p className="text-xs text-muted">{connection.tools?.length || 0} tools available. {connection.verification_note} {connection.access_note}</p>}
    </>}
  </section>;
}
