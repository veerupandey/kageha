import { useCallback, useEffect, useState } from "react";
import { ShoppingBag } from "lucide-react";
import { api } from "../api/client";
import { integrationAgents } from "../api/integrationAgents";
import { integrationError } from "../lib/integrationError";
import { IntegrationPanel } from "./IntegrationPanel";

const base = "/api/integrations/shopify";
const button = "rounded-lg border border-line px-3 py-2 text-xs font-medium text-ink hover:bg-surface disabled:opacity-50 focus-visible:ring-2 focus-visible:ring-accent";
const field = "mt-1 w-full rounded-lg border border-line bg-canvas p-2 text-sm text-ink";
type Store = { shop: string; name: string; agents: string[]; write_agents: string[]; scopes: string[] };
type Status = { configured: boolean; pending: boolean; error: string; stores: Store[] };
type Setup = { configured: boolean; client_id: string; redirect_uri: string; secret_configured: boolean; scopes: string[]; api_version: string; mode: string };
type Agent = { id: string; name: string };
type Edit = { id: string; title: string; agent: string; status: string; before: Record<string, unknown>; changes: Record<string, unknown>; error?: string };

function StorePanel({ store, agents, refresh }: { store: Store; agents: Agent[]; refresh: () => Promise<void> }) {
  const [readers, setReaders] = useState(store.agents);
  const [writers, setWriters] = useState(store.write_agents);
  const [edits, setEdits] = useState<Edit[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [note, setNote] = useState("");
  const path = base + "/stores/" + store.shop;
  const load = useCallback(async () => { setEdits((await api<{ actions: Edit[] }>(path + "/actions")).actions); }, [path]);
  useEffect(() => { setReaders(store.agents); setWriters(store.write_agents); }, [store]);
  async function run(fn: () => Promise<void>) { setBusy(true); setError(""); setNote(""); try { await fn(); } catch (e) { setError(integrationError(e)); } finally { setBusy(false); } }
  return <details className="rounded-xl border border-line bg-surface p-4" onToggle={e => { if (e.currentTarget.open) void run(load); }}>
    <summary className="cursor-pointer text-sm font-medium text-ink">{store.name} · {store.shop}</summary>
    <div className="mt-3 space-y-3">
      <p className="text-xs text-muted">Granted permissions: {store.scopes.join(", ")}</p>
      {error && <p role="alert" className="text-sm text-red-400">{error}</p>}
      {note && <p role="status" className="text-sm text-ink">{note}</p>}
      <fieldset disabled={busy} className="space-y-3"><legend className="text-sm text-ink">Agent access to this store</legend>
        {agents.map(agent => <div key={agent.id} className="space-y-2">
          <label className="flex gap-2 text-xs text-ink"><input type="checkbox" checked={readers.includes(agent.id)} onChange={e => { setReaders(e.target.checked ? [...readers, agent.id] : readers.filter(id => id !== agent.id)); setWriters(writers.filter(id => id !== agent.id)); }} />{agent.name} — read store</label>
          <label className="flex gap-2 text-xs text-muted"><input type="checkbox" disabled={!readers.includes(agent.id) || !store.scopes.includes("write_products")} checked={writers.includes(agent.id)} onChange={e => setWriters(e.target.checked ? [...writers, agent.id] : writers.filter(id => id !== agent.id))} />Prepare product edits for review</label>
        </div>)}
        <div className="flex flex-wrap gap-2">
          <button className={button} onClick={() => void run(async () => { await api(path + "/access", { method: "PUT", body: JSON.stringify({ agents: readers, write_agents: writers }) }); await refresh(); setNote("Shopify agent access saved."); })}>Save store access</button>
          <button className={button} onClick={() => void run(async () => { await api(path + "/check", { method: "POST" }); setNote("Shopify connection is working."); })}>Check store connection</button>
          <button className={button} onClick={() => void run(async () => { await api(path, { method: "DELETE" }); await refresh(); })}>Disconnect store</button>
        </div>
      </fieldset>
      <div className="flex justify-between border-t border-line pt-3"><h4 className="text-sm text-ink">Product edit review</h4><button disabled={busy} className={button} onClick={() => void run(load)}>Refresh edits</button></div>
      {!edits.length && <p className="text-xs text-muted">Ask an assigned agent to prepare title, description, or tag changes. Review the original and proposed values here.</p>}
      {edits.map(edit => <article key={edit.id} className="space-y-2 rounded-lg border border-line bg-canvas p-3">
        <p className="text-sm font-medium text-ink">{edit.title} · {edit.status}</p><p className="text-xs text-muted">Prepared by {edit.agent}</p>
        {Object.entries(edit.changes).map(([name, value]) => <div key={name} className="space-y-1 break-words text-sm text-ink"><p className="font-medium">{name === "descriptionHtml" ? "Description (HTML source)" : name}</p><p className="text-muted">Before: {Array.isArray(edit.before[name]) ? JSON.stringify(edit.before[name]) : String(edit.before[name])}</p><p>After: {Array.isArray(value) ? JSON.stringify(value) : String(value)}</p></div>)}
        {edit.error && <p className="text-xs text-red-400">{edit.error}</p>}
        {["running", "uncertain"].includes(edit.status) ? <p className="text-xs text-muted">Result uncertain. Check Shopify directly before preparing another edit.</p> : <div className="flex gap-2">
          {edit.status !== "done" && <button disabled={busy} className={button + " ka-github-primary"} onClick={() => void run(async () => { await api(path + "/actions/" + edit.id, { method: "POST" }); await load(); })}>Approve product edit</button>}
          <button disabled={busy} className={button} onClick={() => void run(async () => { await api(path + "/actions/" + edit.id, { method: "DELETE" }); await load(); })}>{edit.status === "review" ? "Discard edit" : "Remove edit record"}</button>
        </div>}
      </article>)}
    </div>
  </details>;
}

export function ShopifyIntegration() {
  const [status, setStatus] = useState<Status | null>(null);
  const [setup, setSetup] = useState<Setup | null>(null);
  const [agents, setAgents] = useState<Agent[]>([]);
  const [shop, setShop] = useState("");
  const [secret, setSecret] = useState("");
  const [error, setError] = useState("");
  const [note, setNote] = useState("");
  const [authUrl, setAuthUrl] = useState("");
  const [busy, setBusy] = useState(false);
  const refresh = useCallback(async () => {
    const [row, people] = await Promise.all([api<Status>(base), integrationAgents()]);
    if (!Array.isArray(row.stores)) throw new Error("Shopify is unavailable on this host.");
    setStatus(row); setAgents(people.agents);
  }, []);
  const load = useCallback(async () => {
    await refresh();
    const row = await api<Setup>(base + "/setup");
    setSetup({ ...row, mode: row.mode || "oauth", redirect_uri: row.redirect_uri || window.location.origin + base + "/callback", api_version: row.api_version || "2026-07" });
  }, [refresh]);
  useEffect(() => { void load().catch(e => setError(integrationError(e))); }, [load]);
  useEffect(() => {
    if (!status?.pending) return;
    let disposed = false;
    const timer = setInterval(() => { void api<Status>(base).then(row => { if (disposed) return; if (!Array.isArray(row.stores)) throw new Error("Shopify is unavailable on this host."); setStatus(row); if (!row.pending) { setAuthUrl(""); setError(row.error); if (!row.error) setNote("Shopify connected. Select agents for the store below."); } }).catch(e => { if (!disposed) { setError(integrationError(e)); setStatus(old => old ? { ...old, pending: false } : old); } }); }, 2500);
    return () => { disposed = true; clearInterval(timer); };
  }, [status?.pending]);
  async function run(fn: () => Promise<void>) { setBusy(true); setError(""); setNote(""); try { await fn(); } catch (e) { setError(integrationError(e)); } finally { setBusy(false); } }
  function connect() {
    const popup = setup?.mode === "oauth" ? window.open("about:blank", "_blank") : null;
    if (popup) popup.opener = null;
    void run(async () => { try {
      const result = await api<{ authorization_url?: string }>(base + "/connect", { method: "POST", body: JSON.stringify({ shop }) });
      if (result.authorization_url) { setAuthUrl(result.authorization_url); setStatus(old => old ? { ...old, pending: true } : old); if (popup) popup.location.href = result.authorization_url; }
      else { popup?.close(); await refresh(); setNote("Store connected. Select agent access below."); }
    } catch (e) { popup?.close(); throw e; } });
  }
  return <IntegrationPanel name="Shopify" description="Products, inventory items, and order summaries" connected={status ? status.stores.length > 0 : undefined} unavailable={!status && !!error} icon={<ShoppingBag size={23} />}>
    <p className="text-xs text-muted">Connect stores and choose agent access. Product edits require your review. Customer contact details, payments, and refunds are not exposed by this integration.</p>
    {error && <p role="alert" className="text-sm text-red-400">{error}</p>}
    {note && <p role="status" className="text-sm text-ink">{note}</p>}
    <label className="block text-xs text-muted">Store domain<input className={field} placeholder="your-store.myshopify.com" value={shop} onChange={e => setShop(e.target.value)} /></label>
    <div className="flex flex-wrap gap-2"><button disabled={busy || status?.pending || !status?.configured || !shop.trim()} className={button + " ka-github-primary"} onClick={connect}>Connect Shopify store</button><button disabled={busy} className={button} onClick={() => void run(load)}>Refresh Shopify connections</button></div>
    {status && !status.configured && <p className="text-xs text-muted">Shopify connection needs administrator setup below.</p>}
    {status?.pending && <div className="space-y-2 text-sm text-ink"><p>Finish authorization in Shopify.</p>{authUrl && <a className={button} href={authUrl} target="_blank" rel="noreferrer">Open Shopify sign-in</a>}<button className={button} onClick={() => void run(async () => { setStatus(await api<Status>(base + "/cancel", { method: "POST" })); setAuthUrl(""); })}>Cancel Shopify sign-in</button></div>}
    {status?.stores.map(store => <StorePanel key={store.shop} store={store} agents={agents} refresh={refresh} />)}
    <details className="rounded-xl border border-line p-3"><summary className="cursor-pointer text-sm text-ink">Administrator setup · Shopify app</summary>
      {setup && <form className="mt-3 space-y-3" onSubmit={e => { e.preventDefault(); void run(async () => { await api(base + "/setup", { method: "PUT", body: JSON.stringify({ ...setup, client_secret: secret }) }); setSecret(""); await load(); setNote("Shopify setup saved. Connect or reconnect stores to apply permissions."); }); }}>
        <p className="text-xs text-muted">Create an app in Shopify’s Dev Dashboard and open <strong className="text-ink">Versions → Create version</strong>. The URL fields are on the version screen, not the app’s general Settings page. Register the exact redirect URI below. Changing the Client ID removes existing store connections.</p>
        <p className="rounded-lg border border-line bg-canvas p-3 text-xs text-muted">In Shopify Dev Dashboard, set <strong className="text-ink">Application URL</strong> to the same host as this redirect URI. Current host: <strong className="break-all text-ink">{setup.redirect_uri ? new URL(setup.redirect_uri).host : "your public host"}</strong>. The Application URL and redirect URI cannot use different hosts. For production, use a stable HTTPS domain instead of a temporary preview hostname.</p>
        <fieldset disabled={busy || status?.pending} className="space-y-3">
          <label className="block text-xs text-muted">Connection type<select className={field} value={setup.mode} onChange={e => setSetup({ ...setup, mode: e.target.value })}><option value="oauth">Merchant authorization (OAuth)</option><option value="client_credentials">Stores in our own Shopify organization</option></select></label>
          <p className="text-xs text-muted">Own-organization mode requires the app to be installed on that store and owned by the same Shopify organization. Other merchants must use OAuth.</p>
          {([ ["client_id", "Shopify Client ID"], ["redirect_uri", "Shopify redirect URI"], ["api_version", "Shopify API version"] ] as const).map(([key, label]) => <label key={key} className="block text-xs text-muted">{label}<input required className={field} value={setup[key]} onChange={e => setSetup({ ...setup, [key]: e.target.value })} />{key === "redirect_uri" && <button type="button" className={button + " mt-1"} onClick={() => setSetup({ ...setup, redirect_uri: window.location.origin + base + "/callback" })}>Use this installation URL</button>}</label>)}
          <label className="block text-xs text-muted">Shopify Client Secret<input className={field} type="password" autoComplete="new-password" value={secret} onChange={e => setSecret(e.target.value)} placeholder={setup.secret_configured ? "Saved — leave blank to keep" : "Paste client secret"} /></label>
          {([ ["read_inventory", "Read inventory items"], ["read_orders", "Read order summaries (requires Shopify data access)"], ["write_products", "Allow reviewed product edits"] ] as const).map(([scope, label]) => <label key={scope} className="flex gap-2 text-xs text-ink"><input type="checkbox" checked={setup.scopes.includes(scope)} onChange={e => setSetup({ ...setup, scopes: e.target.checked ? [...setup.scopes, scope] : setup.scopes.filter(s => s !== scope) })} />{label}</label>)}
          <p className="break-all text-xs text-muted">Webhook URL: {setup.redirect_uri.replace(/\/callback$/, "/webhook")}</p>
          <p className="text-xs text-muted">Register app/uninstalled, app/scopes_update, and required privacy webhooks in the app configuration. Tokens renew on use. Disconnecting here removes local credentials; uninstall the app in Shopify to revoke access there.</p>
          <button className={button + " ka-github-primary"} type="submit">Save Shopify setup</button>
        </fieldset>
      </form>}
    </details>
  </IntegrationPanel>;
}
