import { useRef, useState } from "react";
import { api } from "../api/client";

type Server = { name: string; revision?: number; oauth?: { client_id?: string; callback_url?: string } | null };
const field = "w-full rounded-lg border border-line bg-canvas px-3 py-2 text-xs text-ink";

export function McpMtlsCredential({ server, onSaved }: { server: Server; onSaved: () => Promise<void> }) {
  const [client, setClient] = useState(server.oauth?.client_id || "");
  const [callback, setCallback] = useState(server.oauth?.callback_url || "http://127.0.0.1:12799/callback");
  const [method, setMethod] = useState<"tls_client_auth" | "self_signed_tls_client_auth">("tls_client_auth");
  const [identity, setIdentity] = useState("");
  const fileVersion = useRef(0);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  async function save() {
    if (server.revision == null) { setError("Reload the connection before saving credentials."); return; }
    setBusy(true); setError(""); setMessage("");
    try {
      await api("/api/mcp/oauth/mtls-credential", { method: "POST", body: JSON.stringify({ name: server.name, expected_revision: server.revision, client_id: client.trim(), callback_url: callback.trim(), auth_method: method, identity_pem: identity }) });
      ++fileVersion.current; setIdentity(""); await onSaved(); setMessage("Certificate saved. Reconnect to authorize the updated connection.");
    } catch (e) { setError(e instanceof Error ? e.message : String(e)); }
    finally { setBusy(false); }
  }
  return <details className="mt-2 rounded-lg border border-line p-3 text-xs">
    <summary className="cursor-pointer font-medium">Mutual TLS authentication</summary>
    <div className="mt-2 space-y-2">
      <p className="text-muted">Supply the client certificate chain and its private key as combined PEM. Updating the identity disconnects this server until you reconnect.</p>
      {error && <p role="alert">{error}</p>}{message && <p role="status">{message}</p>}
      <label className="block">Authentication method<select className={field} aria-label={`mTLS method for ${server.name}`} value={method} onChange={e => setMethod(e.target.value as typeof method)}><option value="tls_client_auth">CA-issued client certificate</option><option value="self_signed_tls_client_auth">Self-signed client certificate</option></select></label>
      <label className="block">OAuth client ID<input className={field} aria-label={`mTLS client ID for ${server.name}`} value={client} onChange={e => setClient(e.target.value)} /></label>
      <label className="block">Callback URL<input className={field} aria-label={`mTLS callback for ${server.name}`} type="url" value={callback} onChange={e => setCallback(e.target.value)} /></label>
      <label className="block">Client identity PEM file<input className={field} aria-label={`mTLS identity for ${server.name}`} type="file" accept=".pem,application/x-pem-file,text/plain" onChange={e => { const file=e.target.files?.[0], version=++fileVersion.current; setIdentity(""); setError(""); if (file) { if (file.size>49152) setError("Choose an identity file no larger than 48 KiB."); else void file.text().then(text => { if (fileVersion.current === version) setIdentity(text); }).catch(() => { if (fileVersion.current === version) setError("Could not read the identity file."); }); } e.target.value=""; }} /></label>
      {identity && <p className="text-muted">Identity file ready to save. <button type="button" onClick={() => { ++fileVersion.current; setIdentity(""); }} className="underline">Clear</button></p>}
      <button type="button" className="text-accent underline disabled:opacity-50" disabled={busy || !client.trim() || !callback.trim() || !identity} onClick={() => void save()}>Save mTLS credentials</button>
    </div>
  </details>;
}
