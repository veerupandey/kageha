import { useCallback, useEffect, useState } from "react";
import { Icon } from "../lib/icons";
import { api } from "../api/client";
import { integrationAgents } from "../api/integrationAgents";
import { IntegrationPanel, GitHubMark } from "./IntegrationPanel";
import { LinkedInIntegration } from "./LinkedInIntegration";
import { GmailIntegration } from "./GmailIntegration";
import { GoogleCalendarIntegration, GoogleDriveIntegration } from "./GoogleCalendarIntegration";
import { SocialIntegration } from "./SocialIntegration";
import { ShopifyIntegration } from "./ShopifyIntegration";
import { RemoteSettings } from "./RemoteSettings";
import { integrationError } from "../lib/integrationError";

type Connection = { connected: boolean; login: string; repositories: string[]; agents: string[]; browser_auth_available: boolean };
type Repo = { full_name: string; private: boolean };
type Agent = { id: string; name: string };
type Device = { user_code: string; verification_uri: string; interval: number; expires_in: number };
const base = "/api/integrations/github";
const button = "inline-flex items-center justify-center gap-2 rounded-lg border border-line px-3 py-2 text-xs font-medium text-ink transition-colors hover:bg-surface focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent disabled:cursor-not-allowed disabled:opacity-50";
const primaryButton = button + " ka-github-primary min-h-10";

export function IntegrationsSettings() {
  const [connection, setConnection] = useState<Connection | null>(null);
  const [token, setToken] = useState("");
  const [repos, setRepos] = useState<Repo[]>([]);
  const [agents, setAgents] = useState<Agent[]>([]);
  const [selected, setSelected] = useState<string[]>([]);
  const [assigned, setAssigned] = useState<string[]>([]);
  const [page, setPage] = useState(1);
  const [more, setMore] = useState(false);
  const [search, setSearch] = useState("");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const [error, setError] = useState("");
  const [device, setDevice] = useState<Device | null>(null);
  const [editingToken, setEditingToken] = useState(false);
  const [showToken, setShowToken] = useState(false);
  const [reload, setReload] = useState(0);
  const [githubOpened, setGitHubOpened] = useState(false);
  const [repositoryRevision, setRepositoryRevision] = useState(0);
  const [repositoryStatus, setRepositoryStatus] = useState<"idle" | "loading" | "ready" | "error">("idle");

  function apply(row: Connection) {
    setConnection(row); setSelected(row.repositories); setAssigned(row.agents);
  }
  const loadRepos = useCallback(async (next = 1, signal?: AbortSignal) => {
    const data = await api<{ repositories: Repo[]; has_more: boolean }>(`${base}/repositories?page=${next}`, signal ? { signal } : undefined);
    if (signal?.aborted) return;
    setRepos(old => next === 1 ? data.repositories : [...old, ...data.repositories.filter(r => !old.some(x => x.full_name === r.full_name))]);
    setPage(next); setMore(data.has_more);
    setRepositoryStatus("ready");
  }, []);
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
      if (cancelled) return;
      apply(row);
    }).catch(e => { if (!cancelled) setError(integrationError(e)); });
    return () => { cancelled = true; };
  }, [reload]);

  useEffect(() => {
    if (!githubOpened || !connection?.connected) return;
    const controller = new AbortController();
    setRepositoryStatus("loading");
    setRepos([]);
    setMore(false);
    void loadRepos(1, controller.signal).catch(() => {
      if (!controller.signal.aborted) setRepositoryStatus("error");
    });
    return () => controller.abort();
  }, [githubOpened, connection?.connected, loadRepos, repositoryRevision]);

  useEffect(() => {
    if (!device) return;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout>;
    const expires = Date.now() + device.expires_in * 1000;
    async function poll() {
      try {
        if (Date.now() >= expires) throw new Error("GitHub sign-in expired. Start again.");
        const result = await api<Connection & { pending: boolean; interval: number }>(base + "/device/poll", { method: "POST" });
        if (cancelled) return;
        if (result.pending) timer = setTimeout(poll, result.interval * 1000);
        else { apply(result); setRepositoryRevision(value => value + 1); setDevice(null); setEditingToken(false); setMessage("GitHub connected. Select repositories and agents below."); }
      } catch (e) { if (!cancelled) { setError(String(e)); setDevice(null); } }
    }
    timer = setTimeout(poll, device.interval * 1000);
    return () => { cancelled = true; clearTimeout(timer); };
  }, [device]);

  const toggle = (list: string[], value: string) => list.includes(value) ? list.filter(x => x !== value) : [...list, value];
  return <div className="space-y-4">
    {error && <p role="alert" className="rounded-xl border border-red-400/20 bg-red-400/5 px-4 py-3 text-sm text-red-400">{error}</p>}
    {error && !connection && <button className={button} onClick={() => setReload(value => value + 1)}>Retry GitHub connection status</button>}
    {message && <p role="status" className="rounded-xl border border-line bg-accent-soft px-4 py-3 text-sm text-ink">{message}</p>}
    <h2 className="text-sm font-semibold text-ink">Accounts</h2>
    <IntegrationPanel name="GitHub" description="Code, issues, and pull requests" connected={connection?.connected} unavailable={!connection && !!error} icon={<GitHubMark />} onOpen={() => setGitHubOpened(true)}>
      <p className="text-xs text-muted">Read repository files, follow discussions, and inspect pull request changes. Access is limited to the repositories and agents you select.</p>
      {connection?.connected && <div className="space-y-3">
        <p className="text-sm text-ink">Connected as <strong>{connection.login}</strong> · Read only</p>
        <p className="text-xs text-muted">{connection.repositories.length} repositories · {connection.agents.length} agents assigned</p>
        <div className="flex flex-wrap gap-2">
          <button className={button} disabled={busy || !!device || repositoryStatus === "loading"} onClick={() => void run(async () => { apply(await api<Connection>(base + "/check", { method: "POST" })); await loadRepos(); setMessage("GitHub connection is working."); })}>Check connection</button>
          <button className={button} disabled={busy || !!device} onClick={() => { setEditingToken(!editingToken); setShowToken(false); setToken(""); }}>{editingToken ? "Cancel reconnect" : "Reconnect"}</button>
          <button className={button} disabled={busy} onClick={() => void run(async () => { setDevice(null); apply(await api<Connection>(base, { method: "DELETE" })); setRepos([]); setMore(false); setToken(""); setMessage("Disconnected from Kageha. You can also revoke access in GitHub settings."); })}>Disconnect</button>
        </div>
      </div>}
      {connection && (!connection.connected || editingToken) && <div className="space-y-3">
        {connection.browser_auth_available && <div className="space-y-2 pt-1">
          <button className={primaryButton + " w-full"} disabled={busy || !!device} onClick={() => void run(async () => { setShowToken(false); setToken(""); setDevice(await api<Device>(base + "/device", { method: "POST" })); })}>{device ? "Waiting for GitHub…" : "Connect with GitHub"}</button>
          {!device && <p className="text-center text-xs text-muted">Sign in on GitHub and approve access to your account.</p>}
        </div>}
        {device && <div role="status" className="rounded-xl border border-line bg-surface p-4 text-sm space-y-3">
          <p className="font-medium text-ink">Finish connecting on GitHub</p>
          <p className="text-xs text-muted">Open GitHub and enter this one-time code.</p>
          <div className="rounded-lg border border-line bg-canvas px-4 py-3 text-center"><strong className="select-all font-mono text-xl tracking-widest text-ink">{device.user_code}</strong></div>
          <a className={primaryButton + " w-full"} href={device.verification_uri} target="_blank" rel="noreferrer">Open GitHub authorization <Icon.External size={14} /></a>
          <p className="text-xs text-muted">This panel updates automatically after you authorize.</p>
          <button className={button} onClick={() => { setDevice(null); setMessage(""); }}>Cancel sign-in</button>
        </div>}
        {connection.browser_auth_available && <div className="border-t border-line pt-3">
          <button type="button" className="inline-flex items-center gap-1.5 rounded text-xs text-muted hover:text-ink focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent disabled:opacity-50" aria-expanded={showToken} aria-controls="github-token-form" disabled={busy || !!device} onClick={() => { setShowToken(!showToken); setToken(""); }}><Icon.ChevronRight size={14} className={showToken ? "rotate-90 transition-transform" : "transition-transform"} />Connect another way</button>
        </div>}
        {(!connection.browser_auth_available || showToken) && <form id="github-token-form" className="rounded-xl border border-line bg-canvas/50 p-4 space-y-3" onSubmit={e => { e.preventDefault(); void run(async () => {
          const row = await api<Connection>(base + "/connect", { method: "POST", body: JSON.stringify({ token }) });
          setToken(""); setShowToken(false); setDevice(null); setEditingToken(false); apply(row); setRepositoryRevision(value => value + 1); setMessage("Connected. Select repositories and agents, then save access.");
        }); }}>
          <label className="block text-xs text-muted" htmlFor="github-token">Fine-grained personal access token</label>
          <input id="github-token" type="password" autoComplete="new-password" spellCheck={false} disabled={busy || !!device} value={token} onChange={e => setToken(e.target.value)} placeholder="github_pat_…" className="w-full rounded-lg border border-line bg-canvas p-2.5 text-sm text-ink outline-none focus:border-accent focus:ring-2 focus:ring-accent/15" />
          <p className="text-xs leading-relaxed text-muted">Choose your repositories and grant read access to Contents, Issues, and Pull requests. Your token is stored securely on this server.</p>
          <a className="text-xs underline text-muted" href="https://github.com/settings/personal-access-tokens/new" target="_blank" rel="noreferrer">Create a token on GitHub</a>
          <div><button type="submit" className={button} disabled={busy || !!device || !token.trim()}>Connect token</button></div>
        </form>}
      </div>}
    {connection?.connected && <section className="rounded-2xl border border-line p-5 space-y-4">
      <div><h3 className="text-sm font-semibold text-ink">Manage access</h3><p className="text-xs text-muted">Selected agents can read all selected repositories. Nothing is accessible until you save an assignment.</p></div>
      <fieldset disabled={busy || !!device} className="space-y-3">
        <legend className="text-sm font-medium text-ink">Repositories</legend>
        <input aria-label="Search repositories" value={search} onChange={e => setSearch(e.target.value)} placeholder="Filter loaded repositories…" className="w-full rounded-lg border border-line bg-canvas p-2 text-sm text-ink" />
        <div className="max-h-48 overflow-y-auto space-y-2">
          {[...new Set([...selected, ...repos.map(r => r.full_name)])].filter(name => name.toLowerCase().includes(search.toLowerCase())).map(name => <label key={name} className="flex items-center gap-2 text-sm text-ink"><input type="checkbox" checked={selected.includes(name)} onChange={() => setSelected(toggle(selected, name))} />{name}{repos.find(r => r.full_name === name)?.private && <span className="text-xs text-muted">Private</span>}</label>)}
          {repositoryStatus === "loading" && <p role="status" className="text-xs text-muted">Loading repositories…</p>}
          {repositoryStatus === "error" && <button type="button" className={button} onClick={() => void run(() => loadRepos())}>Retry loading repositories</button>}
          {repositoryStatus === "ready" && !repos.length && !selected.length && <p className="text-xs text-muted">No repositories found. Check your GitHub repository permissions.</p>}
        </div>
        {more && <button className={button} onClick={() => void run(() => loadRepos(page + 1))}>Load more repositories</button>}
        <p className="text-sm font-medium text-ink">Agents</p>
        {agents.map(agent => <label key={agent.id} className="flex items-center gap-2 text-sm text-ink"><input type="checkbox" checked={assigned.includes(agent.id)} onChange={() => setAssigned(toggle(assigned, agent.id))} />{agent.name}</label>)}
        <button className={button + " bg-accent-soft"} onClick={() => void run(async () => { apply(await api<Connection>(base + "/access", { method: "PUT", body: JSON.stringify({ repositories: selected, agents: assigned }) })); setMessage("Access saved. Ask an assigned agent to list its GitHub repositories."); })}>Save access</button>
      </fieldset>
    </section>}
    </IntegrationPanel>
    <GmailIntegration />
    <GoogleCalendarIntegration />
    <GoogleDriveIntegration />
    <LinkedInIntegration />
    <SocialIntegration />
    <ShopifyIntegration />
    <div className="border-t border-line pt-4">
      <RemoteSettings />
    </div>
  </div>;
}
