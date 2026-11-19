import { useEffect, useState } from "react";
import { api } from "../api/client";

type Setup = { client_id: string; secret_configured: boolean; redirect_uri: string; source: string; configured: boolean; publish: boolean; api_version: string };
const field = "w-full rounded-lg border border-line bg-canvas p-2.5 text-sm text-ink outline-none focus:border-accent focus:ring-2 focus:ring-accent/15";
const callbackPath = "/api/integrations/linkedin/callback";

export function LinkedInOAuthSetup({ onSaved }: { onSaved: () => Promise<void> }) {
  const [config, setConfig] = useState<Setup | null>(null);
  const [client, setClient] = useState("");
  const [secret, setSecret] = useState("");
  const [redirect, setRedirect] = useState(window.location.origin + callbackPath);
  const [publish, setPublish] = useState(false);
  const [version, setVersion] = useState("202609");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [note, setNote] = useState("");
  useEffect(() => {
    let cancelled = false;
    void api<Setup>("/api/integrations/linkedin/setup").then(row => {
      if (!cancelled) { setConfig(row); setClient(row.client_id); setPublish(row.publish); setVersion(row.api_version); if (row.redirect_uri) setRedirect(row.redirect_uri); }
    }).catch(e => { if (!cancelled) setError(String(e)); });
    return () => { cancelled = true; };
  }, []);

  return <form className="mt-3 space-y-3" onSubmit={e => { e.preventDefault(); setBusy(true); setError(""); setNote(""); void (async () => {
    try {
      const row = await api<Setup>("/api/integrations/linkedin/setup", { method: "PUT", body: JSON.stringify({ client_id: client, client_secret: secret, redirect_uri: redirect, publish, api_version: version }) });
      setConfig(row); setSecret(""); await onSaved(); setNote("LinkedIn setup saved. You can connect now; no restart needed.");
    } catch (e) { setError(String(e)); } finally { setBusy(false); }
  })(); }}>
    <p className="text-xs leading-relaxed text-muted">Create an app in the <a href="https://www.linkedin.com/developers/apps" target="_blank" rel="noreferrer" className="underline">LinkedIn Developer Portal</a>. Enable Sign In with LinkedIn using OpenID Connect under Products. Enable Share on LinkedIn if you want to publish posts.</p>
    {error && <p role="alert" className="text-sm text-red-400">{error}</p>}
    {note && <p role="status" className="text-sm text-muted">{note}</p>}
    <fieldset disabled={busy || !config} className="space-y-3">
      <label className="block text-xs text-muted" htmlFor="linkedin-client-id">LinkedIn Client ID</label>
      <input id="linkedin-client-id" required className={field} value={client} onChange={e => setClient(e.target.value)} placeholder="Your LinkedIn app Client ID" autoComplete="off" spellCheck={false} />
      <label className="block text-xs text-muted" htmlFor="linkedin-client-secret">LinkedIn Client Secret</label>
      <input id="linkedin-client-secret" type="password" className={field} value={secret} onChange={e => setSecret(e.target.value)} placeholder={config?.secret_configured ? "Saved — leave blank to keep" : "Paste your client secret"} autoComplete="new-password" spellCheck={false} />
      <label className="block text-xs text-muted" htmlFor="linkedin-redirect-uri">Authorized redirect URI</label>
      <input id="linkedin-redirect-uri" required type="url" className={field} value={redirect} onChange={e => setRedirect(e.target.value)} spellCheck={false} />
      <p className="text-xs leading-relaxed text-muted">Copy this exact URL into the LinkedIn app’s Auth tab under Authorized redirect URLs. Use your public HTTPS address, or an HTTP localhost callback for this machine.</p>
      <label className="flex items-center gap-2 text-xs text-ink"><input type="checkbox" checked={publish} onChange={e => setPublish(e.target.checked)} />Request permission to publish text posts</label>
      <p className="text-xs text-muted">Enable Share on LinkedIn in your app before requesting posting permission. Reconnect after changing this permission.</p>
      <label className="block text-xs text-muted" htmlFor="linkedin-api-version">LinkedIn API version</label>
      <input id="linkedin-api-version" required pattern="20[0-9]{2}(0[1-9]|1[0-2])" className={field} value={version} onChange={e => setVersion(e.target.value)} />
      {config?.client_id && client !== config.client_id && <p className="text-xs text-muted">Changing the Client ID disconnects the current LinkedIn account and clears its agent assignments.</p>}
      <p className="text-xs text-muted">Saved values override environment configuration and are encrypted on this server. The saved secret is never displayed.</p>
      <button type="submit" className="ka-github-primary rounded-lg border px-3 py-2 text-xs font-medium disabled:opacity-50">{busy ? "Saving…" : "Save LinkedIn setup"}</button>
    </fieldset>
  </form>;
}
