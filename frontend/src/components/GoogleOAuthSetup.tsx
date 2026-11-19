import { useEffect, useState } from "react";
import { api } from "../api/client";

type Setup = { client_id: string; secret_configured: boolean; redirect_uri: string; source: string; configured: boolean };
const field = "w-full rounded-lg border border-line bg-canvas p-2.5 text-sm text-ink outline-none focus:border-accent focus:ring-2 focus:ring-accent/15";
export function GoogleOAuthSetup({ onSaved, integration = "gmail" }: { onSaved: () => Promise<void>; integration?: "gmail" | "google-calendar" | "google-drive" }) {
  const serviceName = integration === "gmail" ? "Gmail" : integration === "google-drive" ? "Google Drive" : "Google Calendar";
  const callbackPath = `/api/integrations/${integration}/callback`;
  const setupPath = `/api/integrations/${integration}/setup`;
  const [config, setConfig] = useState<Setup | null>(null);
  const [client, setClient] = useState("");
  const [secret, setSecret] = useState("");
  const [redirect, setRedirect] = useState(window.location.origin + callbackPath);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [note, setNote] = useState("");
  useEffect(() => {
    let cancelled = false;
    void api<Setup>(setupPath).then(row => {
      if (!cancelled) { setConfig(row); setClient(row.client_id); if (row.redirect_uri) setRedirect(row.redirect_uri); }
    }).catch(e => { if (!cancelled) setError(String(e)); });
    return () => { cancelled = true; };
  }, [setupPath]);

  async function importFile(file: File) {
    setError(""); setNote("");
    try {
      if (file.size > 65536) throw new Error("Choose the small OAuth client JSON downloaded from Google Cloud.");
      const data = JSON.parse(await file.text());
      if (!data.web || typeof data.web.client_id !== "string" || typeof data.web.client_secret !== "string") throw new Error("Download credentials for a Web application OAuth client, not a Desktop client.");
      setClient(data.web.client_id); setSecret(data.web.client_secret);
      const uris: string[] = Array.isArray(data.web.redirect_uris) ? data.web.redirect_uris.filter((uri: unknown) => typeof uri === "string" && uri.endsWith(callbackPath)) : [];
      setRedirect(uris.find(uri => uri === window.location.origin + callbackPath) || uris[0] || window.location.origin + callbackPath);
      setNote("Configuration imported. Check the callback URL, then save.");
    } catch (e) { setError(e instanceof SyntaxError ? "This file is not valid Google OAuth JSON." : String(e)); }
  }

  return <form className="mt-3 space-y-3" onSubmit={e => { e.preventDefault(); setBusy(true); setError(""); setNote(""); void (async () => {
    try {
      const row = await api<Setup>(setupPath, { method: "PUT", body: JSON.stringify({ client_id: client, client_secret: secret, redirect_uri: redirect }) });
      setConfig(row); setSecret(""); await onSaved(); setNote("Google setup saved. You can connect now; no restart needed.");
    } catch (e) { setError(String(e)); } finally { setBusy(false); }
  })(); }}>
    <p className="text-xs leading-relaxed text-muted">Create a Web application OAuth client in <a href="https://console.cloud.google.com/auth/clients" target="_blank" rel="noreferrer" className="underline">Google Cloud</a>. Enable {serviceName} API and add your account as a test user while the app is in Testing.</p>
    {error && <p role="alert" className="text-sm text-red-400">{error}</p>}
    {note && <p role="status" className="text-sm text-muted">{note}</p>}
    <fieldset disabled={busy || !config} className="space-y-3">
      <label className="block text-xs text-muted">Import Google OAuth JSON (optional)
        <input type="file" accept=".json,application/json" className="mt-2 block w-full text-xs" onChange={e => { const file = e.target.files?.[0]; if (file) void importFile(file); e.target.value = ""; }} />
      </label>
      <label className="block text-xs text-muted" htmlFor={`${integration}-google-client-id`}>Google Client ID</label>
      <input id={`${integration}-google-client-id`} required className={field} value={client} onChange={e => setClient(e.target.value)} placeholder="…apps.googleusercontent.com" autoComplete="off" spellCheck={false} />
      <label className="block text-xs text-muted" htmlFor={`${integration}-google-client-secret`}>Google Client Secret</label>
      <input id={`${integration}-google-client-secret`} type="password" className={field} value={secret} onChange={e => setSecret(e.target.value)} placeholder={config?.secret_configured ? "Saved — leave blank to keep" : "Paste your client secret"} autoComplete="new-password" spellCheck={false} />
      <label className="block text-xs text-muted" htmlFor={`${integration}-google-redirect-uri`}>Authorized redirect URI</label>
      <input id={`${integration}-google-redirect-uri`} required type="url" className={field} value={redirect} onChange={e => setRedirect(e.target.value)} spellCheck={false} />
      <p className="text-xs leading-relaxed text-muted">Copy this exact URL into the OAuth client’s Authorized redirect URIs in Google Cloud. Use the public HTTPS address if you access Kageha remotely.</p>
      {config?.client_id && client !== config.client_id && <p className="text-xs text-muted">Changing the Client ID disconnects the current {serviceName} account and clears its agent assignments.</p>}
      <p className="text-xs text-muted">Saved values override environment configuration and are encrypted on this server. The saved secret is never displayed.</p>
      <button type="submit" className="ka-github-primary rounded-lg border px-3 py-2 text-xs font-medium disabled:opacity-50">{busy ? "Saving…" : "Save Google setup"}</button>
    </fieldset>
  </form>;
}
