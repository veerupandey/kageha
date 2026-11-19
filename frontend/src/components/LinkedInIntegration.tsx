import { IntegrationPanel } from "./IntegrationPanel";
import { useEffect, useState } from "react";
import { api } from "../api/client";
import { integrationAgents } from "../api/integrationAgents";
import { Icon } from "../lib/icons";
import { LinkedInOAuthSetup } from "./LinkedInOAuthSetup";
import { integrationError } from "../lib/integrationError";

type Connection = { read_only?: boolean; connected: boolean; configured: boolean; name: string; email: string; agents: string[]; write_agents: string[]; can_publish: boolean; needs_reconnect: boolean; pending: boolean; authorization_error: string; page_url?: string };
type Action = { id: string; agent: string; text: string; status: string; error?: string; post_id?: string };
type Agent = { id: string; name: string };
const base = "/api/integrations/linkedin";
const button = "inline-flex items-center justify-center gap-2 rounded-lg border border-line px-3 py-2 text-xs font-medium text-ink transition-colors hover:bg-surface focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent disabled:cursor-not-allowed disabled:opacity-50";

export function LinkedInIntegration() {
  const [connection, setConnection] = useState<Connection | null>(null);
  const [agents, setAgents] = useState<Agent[]>([]);
  const [posting, setPosting] = useState<string[]>([]);
  const [actions, setActions] = useState<Action[]>([]);
  const [assigned, setAssigned] = useState<string[]>([]);
  const [pageUrl, setPageUrl] = useState("");
  const [busy, setBusy] = useState(false);
  const [waiting, setWaiting] = useState(false);
  const [authorizationUrl, setAuthorizationUrl] = useState("");
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const [reload, setReload] = useState(0);
  function apply(row: Connection) { setConnection(row); setAssigned(row.agents); setPosting(row.write_agents); setPageUrl(row.page_url || ""); }
  async function refreshActions() { setActions((await api<{ actions: Action[] }>(base + "/actions")).actions); }
  async function run(action: () => Promise<void>) {
    setBusy(true); setError(""); setMessage("");
    try { await action(); } catch (e) { setError(e instanceof Error ? e.message : String(e)); }
    finally { setBusy(false); }
  }
  useEffect(() => {
    let cancelled = false;
    setError("");
    void integrationAgents().then(data => {
      if (!cancelled) setAgents(data.agents);
    }).catch(e => { if (!cancelled) setError(integrationError(e)); });
    void api<Connection>(base).then(row => {
      if (!cancelled) { apply(row); setWaiting(row.pending); if (row.authorization_error) setError(row.authorization_error); if (!row.read_only) void refreshActions().catch(e => setError(integrationError(e))); }
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
        setWaiting(false); setAuthorizationUrl(""); apply(row); if (!row.read_only) await refreshActions();
        if (row.authorization_error) setError(row.authorization_error);
        else if (row.connected) setMessage("LinkedIn connected. Select agents and save access below.");
        else setError("LinkedIn sign-in expired or was cancelled. Please try again.");
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

  return <IntegrationPanel name="LinkedIn" description={connection?.read_only ? "Read profile information" : "Profile and reviewed text posts"} connected={connection?.connected} attention={waiting ? "Awaiting sign-in" : connection?.needs_reconnect ? "Reconnect required" : undefined} unavailable={!connection && !!error} icon={<svg aria-hidden="true" width="23" height="23" viewBox="0 0 24 24" fill="currentColor"><path d="M20.5 2h-17C2.67 2 2 2.67 2 3.5v17c0 .83.67 1.5 1.5 1.5h17c.83 0 1.5-.67 1.5-1.5v-17c0-.83-.67-1.5-1.5-1.5ZM8 19H5V9h3v10ZM6.5 7.7a1.7 1.7 0 1 1 0-3.4 1.7 1.7 0 0 1 0 3.4ZM19 19h-3v-5.2c0-1.2-.3-2-1.4-2s-1.6.8-1.6 2V19h-3V9h2.9v1.4c.4-.7 1.2-1.7 2.8-1.7 2.1 0 3.3 1.3 3.3 4V19Z" /></svg>}>
    <p className="text-xs leading-relaxed text-muted">Connect your personal LinkedIn profile. {connection?.read_only ? "Selected agents can read your basic profile." : "Selected agents can read your basic profile and prepare text posts for your review."}</p>
    {error && <p role="alert" className="rounded-xl border border-red-400/20 bg-red-400/5 px-4 py-3 text-sm text-red-400">{error}</p>}
    {error && !connection && <button className={button} onClick={() => setReload(value => value + 1)}>Retry LinkedIn connection status</button>}
    {message && <p role="status" className="rounded-xl border border-line bg-accent-soft px-4 py-3 text-sm text-ink">{message}</p>}
    {connection && !connection.configured && <div className="rounded-xl border border-line bg-surface p-4 space-y-2">
      <p className="text-sm font-medium text-ink">LinkedIn sign-in needs setup</p>
      <p className="text-xs leading-relaxed text-muted">Enter your LinkedIn Client ID, Client Secret, and callback URL below, then connect your account.</p>
    </div>}
    {connection && <details className="rounded-xl border border-line bg-surface p-4" open={!connection.configured || undefined}>
      <summary className="cursor-pointer text-sm font-medium text-ink">LinkedIn OAuth setup</summary>
      <LinkedInOAuthSetup onSaved={async () => { const row = await api<Connection>(base); apply(row); if (!row.read_only) await refreshActions(); setWaiting(false); setAuthorizationUrl(""); }} />
    </details>}
    {connection?.needs_reconnect && <p role="status" className="text-sm text-muted">LinkedIn access expired or was revoked. Reconnect to continue.</p>}
    {connection?.connected && <p className="text-sm text-ink">Connected as <strong className="break-all">{connection.name || connection.email}</strong> · {connection.agents.length} agents assigned</p>}
    {connection?.connected && !connection.read_only && !connection.can_publish && <p className="text-xs text-muted">Profile access is connected. To prepare posts, enable posting permission in OAuth setup and reconnect.</p>}
    {connection?.configured && !waiting && <button className={button + " ka-github-primary min-h-10 w-full"} disabled={busy} onClick={connect}>{connection.connected ? "Reconnect with LinkedIn" : "Connect with LinkedIn"}</button>}
    {waiting && <div role="status" className="rounded-xl border border-line bg-surface p-4 space-y-3">
      <p className="text-sm font-medium text-ink">Finish connecting on LinkedIn</p>
      <p className="text-xs text-muted">Choose your account and approve the requested permissions. This panel updates automatically.</p>
      {authorizationUrl && <a href={authorizationUrl} target="_blank" rel="noreferrer" className={button}>Open LinkedIn sign-in <Icon.External size={14} /></a>}
      <button disabled={busy} className={button} onClick={() => void run(async () => { apply(await api<Connection>(base + "/cancel", { method: "POST" })); setWaiting(false); setAuthorizationUrl(""); })}>Cancel sign-in</button>
    </div>}
    {(connection?.connected || connection?.needs_reconnect) && <>
      {!connection.read_only && <div className="space-y-2 rounded-xl border border-line p-4">
        <label className="block text-sm font-medium text-ink" htmlFor="linkedin-page-url">Existing LinkedIn page</label>
        <input id="linkedin-page-url" type="url" value={pageUrl} onChange={e => setPageUrl(e.target.value)} placeholder="https://www.linkedin.com/company/your-page/" className="w-full rounded-lg border border-line bg-surface px-3 py-2 text-sm text-ink" />
        <p className="text-xs text-muted">Agents use your signed-in browser to read feed and alerts, manage this page, and publish text or media. Your browser account needs page admin access.</p>
        <button className={button} disabled={busy || waiting || !connection.connected} onClick={() => void run(async () => { apply(await api<Connection>(base + "/page", { method: "PUT", body: JSON.stringify({ page_url: pageUrl }) })); setMessage("LinkedIn page saved."); })}>Save LinkedIn page</button>
        {connection.page_url && <a href={connection.page_url} target="_blank" rel="noreferrer" className={button}>Open page admin <Icon.External size={14} /></a>}
      </div>}
      <div className="flex flex-wrap gap-2">
        <button className={button} disabled={busy || waiting || !connection.connected} onClick={() => void run(async () => { apply(await api<Connection>(base + "/check", { method: "POST" })); setMessage("LinkedIn connection is working."); })}>Check LinkedIn connection</button>
        <button className={button} disabled={busy} onClick={() => void run(async () => { apply(await api<Connection>(base, { method: "DELETE" })); setWaiting(false); setAuthorizationUrl(""); setActions([]); setMessage("LinkedIn disconnected from Kageha. You can also remove app access in your LinkedIn account."); })}>Disconnect LinkedIn</button>
      </div>
      <fieldset disabled={busy || waiting || !connection.connected} className="space-y-3 border-t border-line pt-4">
        <legend className="text-sm font-medium text-ink">Agent access</legend>
        <p className="text-xs text-muted">{connection.read_only ? "Choose which agents can read this profile." : "Select profile access and, optionally, permission to prepare posts. You approve publishing below."}</p>
        {agents.map(agent => <div key={agent.id} className="flex flex-wrap gap-4">
          <label className="flex items-center gap-2 text-sm text-ink"><input type="checkbox" checked={assigned.includes(agent.id)} onChange={() => { setAssigned(old => old.includes(agent.id) ? old.filter(id => id !== agent.id) : [...old, agent.id]); setPosting(old => old.filter(id => id !== agent.id)); }} />{agent.name}</label>
          {connection.can_publish && <label className="flex items-center gap-2 text-xs text-muted"><input type="checkbox" disabled={!assigned.includes(agent.id)} checked={posting.includes(agent.id)} onChange={() => setPosting(old => old.includes(agent.id) ? old.filter(id => id !== agent.id) : [...old, agent.id])} />Allow {agent.name} to prepare posts</label>}
        </div>)}
        <button className={button} onClick={() => void run(async () => { apply(await api<Connection>(base + "/access", { method: "PUT", body: JSON.stringify({ agents: assigned, write_agents: connection.can_publish ? posting : [] }) })); setMessage("LinkedIn access saved. Ask an assigned agent to read your profile or prepare a post."); })}>Save LinkedIn access</button>
      </fieldset>
      {!connection.read_only && <section className="space-y-3 border-t border-line pt-4">
        <div className="flex items-center justify-between"><h4 className="text-sm font-medium text-ink">Review text posts</h4><button className={button} disabled={busy} onClick={() => void run(refreshActions)}>Refresh drafts</button></div>
        <p className="text-xs text-muted">Approved posts are published publicly on your connected personal profile.</p>
        {!actions.length && <p className="text-xs text-muted">No prepared posts yet.</p>}
        {actions.map(action => <article key={action.id} className="space-y-2 rounded-xl border border-line p-3">
          <p className="text-xs text-muted">{action.agent} · {action.status}</p>
          <p className="whitespace-pre-wrap break-words text-sm text-ink">{action.text}</p>
          {action.error && <p className="text-xs text-red-400">{action.error}</p>}
          {action.post_id && <a className="text-xs underline text-muted" href={"https://www.linkedin.com/feed/update/" + encodeURIComponent(action.post_id)} target="_blank" rel="noreferrer">View published post</a>}
          <div className="flex gap-2">
            {action.status === "review" && <button className={button} disabled={busy || waiting || !connection.connected || !connection.can_publish} onClick={() => void run(async () => { try { await api(base + "/actions/" + action.id, { method: "POST" }); setMessage("LinkedIn post published."); } finally { await refreshActions(); } })}>Approve and publish</button>}
            {!["running", "uncertain"].includes(action.status) && <button className={button} disabled={busy} onClick={() => void run(async () => { await api(base + "/actions/" + action.id, { method: "DELETE" }); await refreshActions(); })}>Remove post</button>}
            {["running", "uncertain"].includes(action.status) && <p className="text-xs text-muted">Check your LinkedIn profile before preparing another post; this result is uncertain.</p>}
          </div>
        </article>)}
      </section>}
    </>}
  </IntegrationPanel>;
}
