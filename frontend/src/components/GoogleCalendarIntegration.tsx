import { useEffect, useState } from "react";
import { api } from "../api/client";
import { integrationAgents } from "../api/integrationAgents";
import { IntegrationPanel } from "./IntegrationPanel";
import { GoogleOAuthSetup } from "./GoogleOAuthSetup";

type Connection = { connected: boolean; configured: boolean; pending: boolean; needs_verification?: boolean; authorization_error?: string; email?: string; agents: string[] };
type Agent = { id: string; name: string };
const button = "inline-flex items-center justify-center gap-2 rounded-lg border border-line px-3 py-2 text-xs font-medium text-ink disabled:opacity-50";

export function GoogleCalendarIntegration() { return <GoogleReadIntegration integration="google-calendar" />; }
export function GoogleDriveIntegration() { return <GoogleReadIntegration integration="google-drive" />; }

/** Native read-only Google authorization and agent assignments. */
function GoogleReadIntegration({ integration }: { integration: "google-calendar" | "google-drive" }) {
  const drive = integration === "google-drive";
  const service = drive ? "Google Drive" : "Google Calendar";
  const base = `/api/integrations/${integration}`;
  const [connection, setConnection] = useState<Connection | null>(null);
  const [agents, setAgents] = useState<Agent[]>([]);
  const [assigned, setAssigned] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [waiting, setWaiting] = useState(false);
  const [authorizationUrl, setAuthorizationUrl] = useState("");
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const [opened, setOpened] = useState(false);
  function apply(row: Connection) {
    const selected = Array.isArray(row.agents) ? row.agents : [];
    setConnection({ ...row, agents: selected });
    setAssigned(selected);
  }
  async function run(action: () => Promise<void>) {
    setBusy(true); setError(""); setMessage("");
    try { await action(); } catch (e) { setError(e instanceof Error ? e.message : String(e)); }
    finally { setBusy(false); }
  }
  useEffect(() => {
    if (!opened) return;
    let cancelled = false;
    void api<Connection>(base).then(row => { if (!cancelled) { apply(row); setWaiting(row.pending); } })
      .catch(e => { if (!cancelled) setError(String(e)); });
    void integrationAgents().then(data => { if (!cancelled) setAgents(data.agents); })
      .catch(e => { if (!cancelled) setError(String(e)); });
    return () => { cancelled = true; };
  }, [opened, base]);
  useEffect(() => {
    if (!waiting) return;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout>;
    async function poll() {
      try {
        const row = await api<Connection>(base);
        if (cancelled) return;
        if (row.pending) { timer = setTimeout(poll, 2500); return; }
        setWaiting(false); setAuthorizationUrl(""); apply(row);
        if (row.authorization_error) setError(row.authorization_error);
        else if (row.connected) setMessage(`${service} connected.`);
        else setError("Google sign-in expired or was cancelled. Please try again.");
      } catch (e) { if (!cancelled) { setWaiting(false); setError(String(e)); } }
    }
    timer = setTimeout(poll, 2500);
    return () => { cancelled = true; clearTimeout(timer); };
  }, [waiting, base, service]);
  function connect() {
    const popup = window.open("about:blank", "_blank");
    if (popup) popup.opener = null;
    void run(async () => {
      try {
        const result = await api<{ authorization_url: string }>(base + "/connect", { method: "POST" });
        setAuthorizationUrl(result.authorization_url); setWaiting(true);
        if (popup) popup.location.href = result.authorization_url;
      } catch (e) { popup?.close(); throw e; }
    });
  }
  return <IntegrationPanel name={service} description={drive ? "Read file metadata" : "Read calendars and events"} connected={connection?.connected} onOpen={() => setOpened(true)}
    icon={drive ? <svg aria-hidden="true" width="23" height="23" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7"><path d="M8 3h7l7 12-3 5H5l-3-5zM8 3l7 12M2 15h13M15 15l-3 5"/></svg> : <svg aria-hidden="true" width="23" height="23" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.7"><rect x="3" y="5" width="18" height="16" rx="2"/><path d="M7 2v6M17 2v6M3 10h18"/></svg>}
    attention={waiting ? "Awaiting sign-in" : connection?.needs_verification ? "Needs verification" : undefined} unavailable={!connection && !!error}>
    <p className="text-xs text-muted">{drive ? "Read file lists and metadata. This connection does not download file contents or change files." : "Read calendar lists and primary-calendar events. This connection does not create or change events."}</p>
    {error && <p role="alert" className="text-sm text-red-400">{error}</p>}
    {message && <p role="status" className="text-sm">{message}</p>}
    {connection && <details className="rounded-xl border border-line bg-surface p-4" open={!connection.configured || undefined}>
      <summary className="cursor-pointer text-sm font-medium">Google OAuth setup</summary>
      <GoogleOAuthSetup integration={integration} onSaved={async () => { apply(await api<Connection>(base)); setWaiting(false); }} />
    </details>}
    {connection?.connected && <p className="text-sm">Connected{connection.email ? ` as ${connection.email}` : ""}.</p>}
    {connection?.configured && !waiting && <button className={button} disabled={busy} onClick={connect}>{connection.connected ? `Reconnect ${service}` : `Connect ${service}`}</button>}
    {waiting && <div role="status" className="space-y-2"><p>Finish connecting on Google.</p>
      {authorizationUrl && <a href={authorizationUrl} target="_blank" rel="noreferrer" className={button}>Open Google sign-in</a>}
      <button className={button} disabled={busy} onClick={() => void run(async () => { apply(await api<Connection>(base + "/cancel", { method: "POST" })); setWaiting(false); setAuthorizationUrl(""); })}>Cancel sign-in</button>
    </div>}
    {connection?.connected && <>
      <button className={button} disabled={busy || waiting} onClick={() => void run(async () => { apply(await api<Connection>(base + "/check", { method: "POST" })); setMessage(`${service} connection is working.`); })}>Check connection</button>
      <button className={button} disabled={busy} onClick={() => void run(async () => { apply(await api<Connection>(base, { method: "DELETE" })); setMessage(`${service} disconnected.`); })}>Disconnect</button>
      <fieldset disabled={busy || waiting} className="space-y-2 border-t border-line pt-4"><legend className="text-sm font-medium">Agent access</legend>
        {agents.map(agent => <label key={agent.id} className="flex items-center gap-2 text-sm"><input type="checkbox" checked={assigned.includes(agent.id)} disabled={agent.id === "main"} onChange={() => setAssigned(old => old.includes(agent.id) ? old.filter(id => id !== agent.id) : [...old, agent.id])} />{agent.name}</label>)}
        <button className={button} onClick={() => void run(async () => { const row = await api<Connection>(base + "/access", { method: "PUT", body: JSON.stringify({ agents: assigned, expected_agents: connection.agents }) }); apply(row); setMessage(`${service} access saved.`); })}>Save {drive ? "Drive" : "calendar"} access</button>
      </fieldset>
    </>}
  </IntegrationPanel>;
}
