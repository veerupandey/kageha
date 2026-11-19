import { useCallback, useEffect, useState } from "react";
import { Download } from "lucide-react";
import { api } from "../api/client";
import { integrationAgents } from "../api/integrationAgents";
import { integrationError } from "../lib/integrationError";
import { IntegrationPanel } from "./IntegrationPanel";

const root = "/api/integrations/";
function Instagram({ size = 23 }: { size?: number }) { return <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" aria-hidden="true"><rect x="3" y="3" width="18" height="18" rx="5" /><circle cx="12" cy="12" r="4" /><circle cx="17.5" cy="6.5" r="1" fill="currentColor" /></svg>; }
function Facebook({ size = 23 }: { size?: number }) { return <svg width={size} height={size} viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path d="M14 22v-9h3l.5-4H14V7c0-1 .3-2 2-2h2V1.5c-.5-.1-1.7-.2-3-.2-3 0-5 1.8-5 5.2V9H7v4h3v9z" /></svg>; }
const button = "rounded-lg border border-line px-3 py-2 text-xs font-medium text-ink hover:bg-surface disabled:opacity-50 focus-visible:ring-2 focus-visible:ring-accent";
const field = "mt-1 w-full rounded-lg border border-line bg-canvas p-2 text-sm text-ink";
type Account = { key: string; username: string; platform: string; provider: string; capabilities: string[]; agents: string[]; write_agents: string[]; needs_reconnect: boolean };
type Agent = { id: string; name: string };
type Setup = { configured: boolean; client_id: string; config_id?: string; secret_configured: boolean; webhook_configured: boolean; redirect_uri: string; api_version: string; features?: string[] };
type Login = { pending: boolean; error?: string; authorization_error?: string; candidates?: Account[]; selection_id?: string };
type Action = { id: string; kind: string; status: string; agent: string; payload: Record<string, unknown>; error?: string };

export function SocialAppSetup({ provider, onSaved }: { provider: "meta" | "instagram"; onSaved: () => Promise<void> }) {
  const [setup, setSetup] = useState<Setup | null>(null);
  const [secret, setSecret] = useState("");
  const [verify, setVerify] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  useEffect(() => { void api<Setup>(root + provider + "/setup").then(row => setSetup({ ...row, redirect_uri: row.redirect_uri || window.location.origin + root + provider + "/callback", api_version: row.api_version || "v25.0" })).catch(e => setError(integrationError(e))); }, [provider]);
  return <details className="rounded-xl border border-line p-3">
    <summary className="cursor-pointer text-sm text-ink">Administrator setup · {provider === "meta" ? "Facebook Login for Business" : "Direct Instagram login"}</summary>
    {error && <p role="alert" className="text-sm text-red-400">{error}</p>}
    {setup && <form className="mt-3 space-y-3" onSubmit={e => { e.preventDefault(); setBusy(true); setError(""); void api<Setup>(root + provider + "/setup", { method: "PUT", body: JSON.stringify({ ...setup, client_secret: secret, verify_token: verify }) }).then(async row => { setSetup(row); setSecret(""); setVerify(""); await onSaved(); }).catch(e => setError(integrationError(e))).finally(() => setBusy(false)); }}>
      <p className="text-xs text-muted">Configure once for this deployment. Users then connect their own accounts. Changing the App ID removes connections made through that app.</p>
      <fieldset disabled={busy} className="space-y-3">
        {([ ["client_id", provider === "meta" ? "Meta App ID" : "Instagram App ID"], ...(provider === "meta" ? [["config_id", "Business login configuration ID"]] : []), ["redirect_uri", "Exact HTTPS redirect URI"], ["api_version", "Graph API version"] ] as [keyof Setup, string][]).map(([key, label]) => <label key={key} className="block text-xs text-muted">{label}<input required className={field} value={String(setup[key] || "")} onChange={e => setSetup({ ...setup, [key]: e.target.value })} />{key === "redirect_uri" && <button type="button" className={button + " mt-1"} onClick={() => setSetup({ ...setup, redirect_uri: window.location.origin + root + provider + "/callback" })}>Use this installation URL</button>}</label>)}
        <label className="block text-xs text-muted">App Secret<input className={field} type="password" autoComplete="new-password" value={secret} onChange={e => setSecret(e.target.value)} placeholder={setup.secret_configured ? "Saved — leave blank to keep" : "Paste matching app secret"} /></label>
        {provider === "instagram" ? ["publish", "comments", "messages"].map(feature => <label key={feature} className="flex gap-2 text-xs text-ink"><input type="checkbox" checked={setup.features?.includes(feature) || false} onChange={e => setSetup({ ...setup, features: e.target.checked ? [...(setup.features || ["read"]), feature] : setup.features?.filter(f => f !== feature) })} />Request {feature} permission</label>) : <p className="text-xs text-muted">Configure Page and Instagram permissions in Meta’s saved business login configuration. The connection only enables features actually granted by Meta.</p>}
        <label className="block text-xs text-muted">Webhook verify token<input className={field} type="password" autoComplete="new-password" value={verify} onChange={e => setVerify(e.target.value)} placeholder={setup.webhook_configured ? "Saved — leave blank to keep" : "Optional, at least 16 characters"} /></label>
        {["webhook", "deauthorize", "delete-data"].map(path => <p key={path} className="break-all text-xs text-muted">{path}: {setup.redirect_uri.replace(/\/callback$/, "/" + path)}</p>)}
        <button type="submit" className={button + " ka-github-primary"}>Save {provider === "meta" ? "Meta" : "Instagram"} setup</button>
      </fieldset>
    </form>}
  </details>;
}

function AccountPanel({ account, agents, refresh }: { account: Account; agents: Agent[]; refresh: () => Promise<void> }) {
  const [readers, setReaders] = useState(account.agents);
  const [writers, setWriters] = useState(account.write_agents);
  const [actions, setActions] = useState<Action[]>([]);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const [error, setError] = useState("");
  const path = root + "social/accounts/" + encodeURIComponent(account.key);
  const load = useCallback(async () => { setActions((await api<{ actions: Action[] }>(path + "/actions")).actions); }, [path]);
  useEffect(() => { setReaders(account.agents); setWriters(account.write_agents); }, [account]);
  async function run(fn: () => Promise<void>) { setBusy(true); setError(""); setMessage(""); try { await fn(); } catch (e) { setError(integrationError(e)); } finally { setBusy(false); } }
  return <details className="rounded-xl border border-line bg-surface p-4" onToggle={e => { if (e.currentTarget.open) void run(load); }}>
    <summary className="cursor-pointer text-sm font-medium text-ink">{account.platform === "facebook" ? "Facebook Page" : "Instagram"} · {account.username}{account.needs_reconnect ? " · Reconnect required" : ""}</summary>
    <div className="mt-3 space-y-3">
      <p className="text-xs text-muted">Available: {account.capabilities.join(", ") || "No permissions granted"} · Connected through {account.provider === "facebook" ? "Facebook" : "Instagram"}</p>
      {error && <p role="alert" className="text-sm text-red-400">{error}</p>}
      {message && <p role="status" className="text-sm text-ink">{message}</p>}
      <fieldset disabled={busy} className="space-y-3">
        <legend className="text-sm text-ink">Agent permissions</legend>
        {agents.map(agent => <div key={agent.id} className="space-y-1">
          <label className="flex gap-2 text-xs text-ink"><input type="checkbox" checked={readers.includes(agent.id)} onChange={e => { setReaders(e.target.checked ? [...readers, agent.id] : readers.filter(id => id !== agent.id)); setWriters(writers.filter(id => id !== agent.id)); }} />{agent.name} — read this account</label>
          <label className="flex gap-2 text-xs text-muted"><input type="checkbox" disabled={!readers.includes(agent.id)} checked={writers.includes(agent.id)} onChange={e => setWriters(e.target.checked ? [...writers, agent.id] : writers.filter(id => id !== agent.id))} />Prepare actions for review</label>
        </div>)}
        <div className="flex flex-wrap gap-2">
          <button className={button} onClick={() => void run(async () => { await api(path + "/access", { method: "PUT", body: JSON.stringify({ agents: readers, write_agents: writers }) }); await refresh(); setMessage("Account permissions saved."); })}>Save access</button>
          <button className={button} onClick={() => void run(async () => { await api(path + "/check", { method: "POST" }); setMessage("Connection is working."); })}>Check connection</button>
          <button className={button} onClick={() => void run(async () => { await api(path + "/subscribe", { method: "POST" }); setMessage("Webhook subscription enabled."); })}>Enable webhooks</button>
          <button className={button} onClick={() => void run(async () => { await api(path, { method: "DELETE" }); await refresh(); })}>Disconnect account</button>
        </div>
      </fieldset>
      <div className="space-y-3 border-t border-line pt-3">
        <div className="flex justify-between"><h4 className="text-sm font-medium text-ink">Review queue</h4><button disabled={busy} className={button} onClick={() => void run(load)}>Refresh actions</button></div>
        {!actions.length && <p className="text-xs text-muted">Ask an assigned agent to prepare content for this account. Nothing is published until you approve it here.</p>}
        {actions.map(action => <article key={action.id} className="space-y-2 rounded-lg border border-line bg-canvas p-3">
          <p className="text-sm text-ink">{action.kind.replaceAll("_", " ")} · {action.status}</p>
          <p className="text-xs text-muted">Prepared by {action.agent} for {account.username}</p>
          {Object.entries(action.payload).map(([key, value]) => <div key={key} className="break-words text-sm text-ink"><span className="text-xs text-muted">{key.replaceAll("_", " ")}: </span>{typeof value === "string" ? value : JSON.stringify(value)}</div>)}
          {action.error && <p className="text-xs text-red-400">{action.error}</p>}
          {["running", "uncertain"].includes(action.status) ? <p className="text-xs text-muted">Check the account directly. The result is uncertain and replay is blocked.</p> : <div className="flex gap-2">
            {action.status !== "done" && <button disabled={busy} className={button + " ka-github-primary"} onClick={() => void run(async () => { await api(path + "/actions/" + action.id, { method: "POST" }); await load(); })}>{action.status === "processing" ? "Check media and publish" : "Approve and execute"}</button>}
            <button disabled={busy} className={button} onClick={() => void run(async () => { await api(path + "/actions/" + action.id, { method: "DELETE" }); await load(); })}>{action.status === "review" ? "Discard" : "Remove record"}</button>
          </div>}
        </article>)}
      </div>
    </div>
  </details>;
}

export function SocialIntegration() {
  const [accounts, setAccounts] = useState<Account[]>([]);
  const [agents, setAgents] = useState<Agent[]>([]);
  const [configured, setConfigured] = useState<Record<string, boolean>>({});
  const [login, setLogin] = useState<Login>({ pending: false });
  const [provider, setProvider] = useState<"meta" | "instagram">("meta");
  const [selected, setSelected] = useState<string[]>([]);
  const [authUrl, setAuthUrl] = useState("");
  const [busy, setBusy] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState("");
  const [draft, setDraft] = useState("");
  const refresh = useCallback(async () => {
    const [rows, people, meta, ig] = await Promise.all([api<{ accounts: Account[] }>(root + "social"), integrationAgents(), api<Login & { configured: boolean }>(root + "meta"), api<Login & { configured: boolean }>(root + "instagram")]);
    if (!Array.isArray(rows.accounts)) throw new Error("Facebook and Instagram are unavailable on this host.");
    setAccounts(rows.accounts); setAgents(people.agents); setConfigured({ meta: meta.configured, instagram: ig.configured }); setLoaded(true);
    if (meta.pending || meta.candidates?.length) { setProvider("meta"); setLogin(meta); }
    else if (ig.pending) { setProvider("instagram"); setLogin(ig); }
  }, []);
  useEffect(() => { void refresh().catch(e => setError(integrationError(e))); }, [refresh]);
  useEffect(() => {
    if (!login.pending) return;
    let disposed = false;
    const timer = setInterval(() => { void api<Login>(root + provider).then(async row => {
      if (disposed) return;
      setLogin(row);
      if (!row.pending) { setAuthUrl(""); setError(row.error || row.authorization_error || ""); await refresh(); }
    }).catch(e => { if (!disposed) { setError(integrationError(e)); setLogin({ pending: false }); } }); }, 2500);
    return () => { disposed = true; clearInterval(timer); };
  }, [login.pending, provider, refresh]);
  async function run(fn: () => Promise<void>) { setBusy(true); setError(""); try { await fn(); } catch (e) { setError(integrationError(e)); } finally { setBusy(false); } }
  function connect(next: "meta" | "instagram") {
    const popup = window.open("about:blank", "_blank");
    if (popup) popup.opener = null;
    void run(async () => {
      try { const data = await api<{ authorization_url: string }>(root + next + "/connect", { method: "POST" }); setProvider(next); setSelected([]); setLogin({ pending: true }); setAuthUrl(data.authorization_url); if (popup) popup.location.href = data.authorization_url; }
      catch (e) { popup?.close(); throw e; }
    });
  }
  return <IntegrationPanel name="Facebook & Instagram" description="Pages, professional accounts, and personal content" connected={loaded ? accounts.length > 0 : undefined} unavailable={!loaded && !!error} icon={<Instagram size={23} />}>
    <p className="text-xs text-muted">Connect Facebook Pages and linked Instagram accounts, or connect Instagram Business/Creator directly. Select account access separately for each agent.</p>
    {error && <p role="alert" className="text-sm text-red-400">{error}</p>}
    <div className="flex flex-wrap gap-2">
      <button className={button + " ka-github-primary inline-flex items-center gap-2"} disabled={busy || login.pending || !configured.meta} onClick={() => connect("meta")}><Facebook size={15} />Connect Facebook & Instagram</button>
      <button className={button + " inline-flex items-center gap-2"} disabled={busy || login.pending || !configured.instagram} onClick={() => connect("instagram")}><Instagram size={15} />Connect Instagram directly</button>
      <button className={button} disabled={busy} onClick={() => void run(refresh)}>Refresh connections</button>
    </div>
    {loaded && !configured.meta && <p className="text-xs text-muted">Facebook connection needs administrator setup below.</p>}
    {login.pending && <div className="space-y-2 text-sm text-ink"><p>Finish sign-in in the provider window.</p>{authUrl && <a className={button} href={authUrl} target="_blank" rel="noreferrer">Open sign-in</a>}<button className={button} onClick={() => void run(async () => { await api(root + provider + "/cancel", { method: "POST" }); setLogin({ pending: false }); setAuthUrl(""); })}>Cancel sign-in</button></div>}
    {!!login.candidates?.length && <fieldset disabled={busy} className="space-y-3 rounded-xl border border-line p-3"><legend className="text-sm text-ink">Choose accounts to connect</legend>
      {login.candidates.map(a => <label key={a.key} className="flex gap-2 text-sm text-ink"><input type="checkbox" checked={selected.includes(a.key)} onChange={e => setSelected(e.target.checked ? [...selected, a.key] : selected.filter(k => k !== a.key))} />{a.username} · {a.platform === "facebook" ? "Facebook Page" : "Instagram"}</label>)}
      <button disabled={!selected.length} className={button + " ka-github-primary"} onClick={() => void run(async () => { await api(root + "meta/select", { method: "POST", body: JSON.stringify({ accounts: selected, selection_id: login.selection_id }) }); setLogin({ pending: false }); setSelected([]); await refresh(); })}>Connect selected accounts</button>
      <button className={button} onClick={() => void run(async () => { await api(root + "meta/cancel", { method: "POST" }); setLogin({ pending: false }); })}>Discard selection</button>
    </fieldset>}
    {accounts.map(a => <AccountPanel key={a.key} account={a} agents={agents} refresh={refresh} />)}
    <details className="rounded-xl border border-line p-3"><summary className="cursor-pointer text-sm text-ink">Personal Instagram or Facebook profile</summary><p className="my-3 text-xs text-muted">Use an agent to prepare captions and media, then publish manually. Personal profiles are not connected publishing accounts. Instagram API access requires a Business or Creator account.</p><label className="text-xs text-muted">Caption draft<textarea className={field} rows={4} value={draft} onChange={e => setDraft(e.target.value)} /></label><button disabled={!draft.trim()} className={button + " mt-2 inline-flex items-center gap-2"} onClick={() => { const url = URL.createObjectURL(new Blob([draft], { type: "text/plain;charset=utf-8" })); const link = document.createElement("a"); link.href = url; link.download = "social-caption.txt"; link.click(); setTimeout(() => URL.revokeObjectURL(url), 1000); }}><Download size={14} />Download caption</button></details>
    <SocialAppSetup provider="meta" onSaved={refresh} />
    <SocialAppSetup provider="instagram" onSaved={refresh} />
  </IntegrationPanel>;
}
