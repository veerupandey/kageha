import { useState } from "react";
import { api } from "../api/client";

type Server = { name: string; revision?: number; oauth?: { client_id?: string; callback_url?: string } | null };
const field = "w-full rounded-lg border border-line bg-canvas px-3 py-2 text-xs text-ink";

/** Configure an operator-owned signing key or secret without displaying saved credentials. */
export function McpAssertionCredential({ server, onSaved }: { server: Server; onSaved: () => Promise<void> }) {
  const [method, setMethod] = useState<"client_secret_jwt" | "private_key_jwt">("client_secret_jwt");
  const [algorithm, setAlgorithm] = useState<"HS256" | "EdDSA" | "RS256" | "ES256">("HS256");
  const [issuer, setIssuer] = useState("");
  const [client, setClient] = useState(server.oauth?.client_id || "");
  const [callback, setCallback] = useState(server.oauth?.callback_url || "http://127.0.0.1:12799/callback");
  const [credential, setCredential] = useState("");
  const [keyId, setKeyId] = useState("");
  const [audience, setAudience] = useState<"token_endpoint" | "issuer">("token_endpoint");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const [error, setError] = useState("");
  async function save() {
    if (server.revision == null) { setError("Reload the connection before saving credentials."); return; }
    setBusy(true); setError(""); setMessage("");
    try {
      await api("/api/mcp/oauth/assertion-credential", { method: "POST", body: JSON.stringify({
        name: server.name, expected_revision: server.revision, method, algorithm, issuer: issuer.trim(), assertion_audience: audience,
        client_id: client.trim(), callback_url: callback.trim(), credential, ...(keyId.trim() ? { key_id: keyId.trim() } : {}),
      }) });
      setCredential(""); await onSaved(); setMessage("Credential saved. Reconnect to authorize the updated connection.");
    } catch (e) { setError(e instanceof Error ? e.message : String(e)); }
    finally { setBusy(false); }
  }
  return <details className="mt-2 rounded-lg border border-line p-3 text-xs">
    <summary className="cursor-pointer font-medium">JWT client authentication</summary>
    <div className="mt-2 space-y-2">
      <p className="text-muted">Use an existing OAuth client registered with your provider. Updating its credential disconnects this server until you reconnect.</p>
      {error && <p role="alert">{error}</p>}{message && <p role="status">{message}</p>}
      <label className="block">Authentication method<select className={field} aria-label={`JWT method for ${server.name}`} value={method} onChange={e => {const next=e.target.value as typeof method;setMethod(next);setAlgorithm(next === "client_secret_jwt" ? "HS256" : "EdDSA");setCredential("");}}><option value="client_secret_jwt">Client secret JWT</option><option value="private_key_jwt">Private key JWT</option></select></label>
      <label className="block">Signing algorithm<select className={field} aria-label={`JWT algorithm for ${server.name}`} value={algorithm} onChange={e => setAlgorithm(e.target.value as typeof algorithm)}>{method === "client_secret_jwt" ? <option value="HS256">HS256</option> : <><option value="EdDSA">EdDSA (Ed25519)</option><option value="RS256">RS256</option><option value="ES256">ES256 (P-256)</option></>}</select></label>
      <label className="block">Authorization issuer<input className={field} aria-label={`OAuth issuer for ${server.name}`} type="url" value={issuer} onChange={e => setIssuer(e.target.value)} placeholder="https://auth.example.com" /></label>
      <label className="block">Assertion audience<select className={field} aria-label={`JWT audience for ${server.name}`} value={audience} onChange={e => setAudience(e.target.value as typeof audience)}><option value="token_endpoint">Token endpoint</option><option value="issuer">Authorization issuer</option></select></label>
      <label className="block">OAuth client ID<input className={field} aria-label={`OAuth client ID for ${server.name}`} value={client} onChange={e => setClient(e.target.value)} /></label>
      <label className="block">Callback URL<input className={field} aria-label={`OAuth callback for ${server.name}`} type="url" value={callback} onChange={e => setCallback(e.target.value)} /></label>
      <label className="block">{method === "client_secret_jwt" ? "Client secret" : "Private key (PKCS#8 DER, base64url)"}<input className={field} aria-label={`JWT credential for ${server.name}`} type="password" autoComplete="new-password" value={credential} onChange={e => setCredential(e.target.value)} /></label>
      <p className="text-muted">{method === "client_secret_jwt" ? "HS256 requires a secret of at least 32 bytes." : "Register the matching public key with the provider before connecting."}</p>
      <label className="block">Key ID (optional)<input className={field} aria-label={`JWT key ID for ${server.name}`} value={keyId} onChange={e => setKeyId(e.target.value)} /></label>
      <button type="button" className="text-accent underline disabled:opacity-50" disabled={busy || !issuer.trim() || !client.trim() || !callback.trim() || !credential} onClick={() => void save()}>Save JWT credentials</button>
    </div>
  </details>;
}
