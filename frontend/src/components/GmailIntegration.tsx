import { IntegrationPanel } from "./IntegrationPanel";
import { useEffect, useState } from "react";
import { api } from "../api/client";
import { integrationAgents } from "../api/integrationAgents";
import { Icon } from "../lib/icons";
import { GoogleOAuthSetup } from "./GoogleOAuthSetup";
import { integrationError } from "../lib/integrationError";
import { useAppStore } from "../store";
import { gmailAccessChanged } from "./GmailAccessAction";

type Connection = { connected: boolean; needs_verification?: boolean; configured: boolean; email: string; agents: string[]; pending: boolean; authorization_error: string; redirect_uri: string };
type Agent = { id: string; name: string };
const base = "/api/integrations/gmail";
const button = "inline-flex items-center justify-center gap-2 rounded-lg border border-line px-3 py-2 text-xs font-medium text-ink transition-colors hover:bg-surface focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent disabled:cursor-not-allowed disabled:opacity-50";

export function GmailIntegration() {
  const agentId = useAppStore(s => s.activePersonaId);
  const chatId = useAppStore(s => s.sessionId);
  const focus = useAppStore(s => s.searchKeyFocus);
  const clearFocus = useAppStore(s => s.clearSearchKeyFocus);
  const [connection, setConnection] = useState<Connection | null>(null);
  const [agents, setAgents] = useState<Agent[]>([]);
  const [assigned, setAssigned] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [waiting, setWaiting] = useState(false);
  const [authorizationUrl, setAuthorizationUrl] = useState("");
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const [reload, setReload] = useState(0);
  useEffect(() => {
    if (focus !== "gmail-access") return;
    const section = document.getElementById("gmail-access-settings");
    section?.querySelector<HTMLButtonElement>('button[aria-expanded="false"]')?.click();
    section?.scrollIntoView?.({block: "nearest"});
    section?.focus();
    clearFocus();
  }, [focus, clearFocus]);
  function apply(row: Connection) {
    const assignments = Array.isArray(row.agents) ? row.agents : [];
    setConnection({ ...row, agents: assignments });
    setAssigned(assignments);
  }
  async function run(action: () => Promise<void>) {
    setBusy(true); setError(""); setMessage("");
    try { await action(); } catch (e) { setError(e instanceof Error ? e.message : String(e)); }
    finally { setBusy(false); }
  }
  useEffect(() => {
    let cancelled = false;
    setError("");
    void integrationAgents().then(data => {
      if (!cancelled) setAgents(Array.isArray(data.agents) ? data.agents : []);
    }).catch(e => { if (!cancelled) setError(integrationError(e)); });
    void api<Connection>(base).then(row => {
      if (!cancelled) { apply(row); setWaiting(row.pending); }
    }).catch(e => { if (!cancelled) setError(integrationError(e)); });
    return () => { cancelled = true; };
  }, [reload]);
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
        else if (row.connected) setMessage("Gmail connected. Select agents and save access below.");
        else setError("Google sign-in expired or was cancelled. Please try again.");
      } catch (e) { if (!cancelled) { setWaiting(false); setError(String(e)); } }
    }
    timer = setTimeout(poll, 2500);
    return () => { cancelled = true; clearTimeout(timer); };
  }, [waiting]);

  function connect() {
    // Open synchronously to avoid popup blockers after the API request resolves.
    const popup = window.open("about:blank", "_blank");
    if (popup) popup.opener = null;
    void run(async () => {
      try {
        const data = await api<{ authorization_url: string }>(base + "/connect", { method: "POST" });
        setAuthorizationUrl(data.authorization_url); setWaiting(true);
        if (popup) popup.location.href = data.authorization_url;
      } catch (e) { popup?.close(); throw e; }
    });
  }

  return <div id="gmail-access-settings" tabIndex={-1}><IntegrationPanel name="Gmail" description="Email search and conversations" connected={connection?.connected} attention={waiting ? "Awaiting sign-in" : connection?.needs_verification ? "Needs verification" : connection?.connected && !connection.agents.length ? "Connected · no agents assigned" : connection && !connection.connected ? "Disconnected" : undefined} unavailable={!connection && !!error} icon={<Icon.Mail size={23} />}>
    <p className="text-xs leading-relaxed text-muted">Find emails, read conversations, and summarize your inbox. The default agent has access automatically. Select additional agents below. Read only.</p>
    {error && <p role="alert" className="rounded-xl border border-red-400/20 bg-red-400/5 px-4 py-3 text-sm text-red-400">{error}</p>}
    {error && !connection && <button className={button} onClick={() => setReload(value => value + 1)}>Retry Gmail connection status</button>}
    {message && <p role="status" className="rounded-xl border border-line bg-accent-soft px-4 py-3 text-sm text-ink">{message}</p>}
    {connection && !connection.configured && <div className="rounded-xl border border-line bg-surface p-4 space-y-2">
      <p className="text-sm font-medium text-ink">Google sign-in needs setup</p>
      <p className="text-xs leading-relaxed text-muted">The server administrator needs to configure Google OAuth before you can connect.</p>
      <details className="text-xs text-muted"><summary className="cursor-pointer">Setup instructions</summary>
        <ol className="mt-3 list-decimal space-y-2 pl-4">
          <li>Enable Gmail API in <a className="underline" href="https://console.cloud.google.com/apis/library/gmail.googleapis.com" target="_blank" rel="noreferrer">Google Cloud</a> and configure the Google Auth consent screen.</li>
          <li>Create a Web application OAuth client. Add your Kageha URL followed by <code className="break-all">/api/integrations/gmail/callback</code> as an authorized redirect URI.</li>
          <li>Enter your Client ID, Client Secret, and redirect URI in Google OAuth setup below.</li>
          <li>For testing, add your Google account to the app’s test users.</li>
        </ol>
      </details>
    </div>}
    {connection && <details className="rounded-xl border border-line bg-surface p-4" open={!connection.configured || undefined}>
      <summary className="cursor-pointer text-sm font-medium text-ink">Google OAuth setup</summary>
      <GoogleOAuthSetup onSaved={async () => { apply(await api<Connection>(base)); setWaiting(false); setAuthorizationUrl(""); }} />
    </details>}
    {connection?.connected && <p className="text-sm text-ink">Connected as <strong className="break-all">{connection.email}</strong> · {connection.agents.length} agents assigned</p>}
    {connection?.configured && !waiting && <button className={button + " ka-github-primary min-h-10 w-full"} disabled={busy} onClick={connect}>{connection.connected ? "Reconnect with Google" : "Connect with Google"}</button>}
    {waiting && <div role="status" className="rounded-xl border border-line bg-surface p-4 space-y-3">
      <p className="text-sm font-medium text-ink">Finish connecting on Google</p>
      <p className="text-xs text-muted">Choose your account and approve Gmail access. This panel updates automatically.</p>
      {authorizationUrl && <a href={authorizationUrl} target="_blank" rel="noreferrer" className={button}>Open Google sign-in <Icon.External size={14} /></a>}
      <button disabled={busy} className={button} onClick={() => void run(async () => { apply(await api<Connection>(base + "/cancel", { method: "POST" })); setWaiting(false); setAuthorizationUrl(""); })}>Cancel sign-in</button>
    </div>}
    {connection?.connected && <>
      <div className="flex flex-wrap gap-2">
        <button className={button} disabled={busy || waiting} onClick={() => void run(async () => { apply(await api<Connection>(base + "/check", { method: "POST" })); setMessage("Gmail connection is working."); })}>Check Gmail connection</button>
        <button className={button} disabled={busy} onClick={() => void run(async () => { apply(await api<Connection>(base, { method: "DELETE" })); setWaiting(false); setAuthorizationUrl(""); setMessage("Gmail disconnected from Kageha. You can also remove app access in your Google account."); })}>Disconnect Gmail</button>
      </div>
      <fieldset disabled={busy || waiting} className="space-y-3 border-t border-line pt-4">
        <legend className="text-sm font-medium text-ink">Agent access</legend>
        <p className="text-xs text-muted">Selected agents can search and read the connected mailbox.</p>
        {chatId && <p className="text-xs text-muted">Current chat agent: {agents.find(agent => agent.id === agentId)?.name || agentId} · {connection.agents.includes(agentId) ? "Can read Gmail" : "Gmail access needed"}</p>}
        {agents.map(agent => <label key={agent.id} className="flex items-center gap-2 text-sm text-ink"><input type="checkbox" checked={assigned.includes(agent.id)} disabled={agent.id === "main"} onChange={() => setAssigned(old => old.includes(agent.id) ? old.filter(id => id !== agent.id) : [...old, agent.id])} />{agent.name}</label>)}
        <button className={button} onClick={() => void run(async () => {
          await api<Connection>(base + "/access", { method: "PUT", body: JSON.stringify({ agents: assigned, expected_agents: connection.agents }) });
          const saved = await api<Connection>(base);
          apply(saved);
          if (!saved.connected || saved.agents.length !== assigned.length || assigned.some(id => !saved.agents.includes(id)))
            throw new Error("Gmail access could not be verified. Review the current assignments.");
          window.dispatchEvent(new Event(gmailAccessChanged));
          setMessage("Gmail access saved and verified.");
        })}>Save Gmail access</button>
      </fieldset>
    </>}
  </IntegrationPanel></div>;
}
