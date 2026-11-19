import { useEffect, useState } from "react";
import { api } from "../api/client";

type Mode = "normal" | "last" | "never";
interface Rule { mode?: Mode; allow_fallback?: boolean }
interface Policy { enabled: boolean; providers: Record<string, Rule>; models: Record<string, Rule> }
interface Inventory { policy: Policy; providers: string[]; models: { id: string; provider: string }[] }

function validPolicy(value: unknown): value is Policy {
  if (!value || typeof value !== "object") return false;
  const policy = value as Policy;
  return typeof policy.enabled === "boolean"
    && !!policy.providers && typeof policy.providers === "object" && !Array.isArray(policy.providers)
    && !!policy.models && typeof policy.models === "object" && !Array.isArray(policy.models);
}

function validInventory(value: Inventory): boolean {
  return !!value && validPolicy(value.policy)
    && Array.isArray(value.providers) && value.providers.every((provider) => typeof provider === "string")
    && Array.isArray(value.models) && value.models.every((model) => !!model && typeof model.id === "string" && typeof model.provider === "string");
}

export function FallbackSettings() {
  const [data, setData] = useState<Inventory>();
  const [scope, setScope] = useState<"providers" | "models">("providers");
  const [identity, setIdentity] = useState("");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const [error, setError] = useState("");
  const [dirty, setDirty] = useState(false);
  useEffect(() => {
    let active = true;
    void api<Inventory>("/api/models/fallback")
      .then((result) => {
        if (!active) return;
        if (!validInventory(result)) throw new Error("Could not load fallback settings: invalid response.");
        setData(result); setIdentity(result.providers[0] ?? "");
      })
      .catch((e) => { if (active) setError(String(e)); });
    return () => { active = false; };
  }, []);
  const updateRule = (key: keyof Rule, value: Mode | boolean | undefined) => {
    if (!data || !identity) return;
    const next = { ...data.policy[scope][identity] };
    if (value === undefined) delete next[key];
    else Object.assign(next, { [key]: value });
    setData({ ...data, policy: { ...data.policy, [scope]: { ...data.policy[scope], [identity]: next } } });
    setDirty(true); setMessage("");
  };
  const save = async () => {
    if (!data) return;
    setBusy(true); setError(""); setMessage("");
    try {
      const result = await api<{ policy: Policy; message: string }>("/api/models/fallback", {
        method: "POST", body: JSON.stringify(data.policy),
      });
      if (!result || !validPolicy(result.policy)) throw new Error("Could not save fallback settings: invalid response.");
      setData({ ...data, policy: result.policy }); setMessage(result.message); setDirty(false);
    } catch (e) { setError(e instanceof Error ? e.message : String(e)); }
    finally { setBusy(false); }
  };
  const selected = data?.policy[scope][identity] ?? {};
  const provider = scope === "models" ? data?.models.find((model) => model.id === identity)?.provider : undefined;
  const inherited = provider ? data?.policy.providers[provider] : undefined;
  const effectiveMode = selected.mode ?? inherited?.mode ?? "normal";
  const effectiveOutgoing = selected.allow_fallback ?? inherited?.allow_fallback ?? true;
  return <section aria-label="Model fallback settings" className="space-y-3 rounded-2xl border border-line bg-canvas/35 p-4 text-sm">
    <h3 className="font-semibold text-ink">Fallback behavior</h3>
    <p className="text-xs text-muted">Normal models follow the configured order. Last resort models come after eligible normal models. Never excludes a model from automatic routes; you can still select it directly. Model rules override provider rules. Retry and time limits still apply.</p>
    {error && <p role="alert" className="text-xs text-warn">{error}</p>}
    {!data && !error && <p role="status" className="text-xs text-muted">Loading fallback settings…</p>}
    {data && <fieldset disabled={busy} className="space-y-3">
      <label className="flex items-center gap-2"><input type="checkbox" checked={data.policy.enabled} onChange={(e) => {
        setData({ ...data, policy: { ...data.policy, enabled: e.target.checked } }); setDirty(true); setMessage("");
      }} />Allow automatic model fallback</label>
      <div className="flex flex-wrap gap-2">
        <select aria-label="Fallback rule scope" value={scope} className="rounded border border-line bg-canvas p-2" onChange={(e) => {
          const value = e.target.value as typeof scope; setScope(value);
          setIdentity(value === "providers" ? data.providers[0] ?? "" : data.models[0]?.id ?? "");
        }}><option value="providers">Provider</option><option value="models">Model</option></select>
        <select aria-label="Fallback rule target" value={identity} className="min-w-0 flex-1 rounded border border-line bg-canvas p-2" onChange={(e) => setIdentity(e.target.value)}>
          {(scope === "providers" ? data.providers : data.models.map((model) => model.id)).map((id) => <option key={id} value={id}>{id}</option>)}
        </select>
      </div>
      {identity && <>
        <label className="block">Use in automatic routes
          <select aria-label="Automatic fallback participation" value={selected.mode ?? "inherit"} className="ml-2 rounded border border-line bg-canvas p-2" onChange={(e) => updateRule("mode", e.target.value === "inherit" ? undefined : e.target.value as Mode)}>
            <option value="inherit">Inherit ({inherited?.mode ?? "normal"})</option>
            <option value="normal">Normal</option><option value="last">Last resort</option><option value="never">Never</option>
          </select>
        </label>
        <label className="block">If this selection fails
          <select aria-label="Fallback after selected model fails" value={selected.allow_fallback === undefined ? "inherit" : String(selected.allow_fallback)} className="ml-2 rounded border border-line bg-canvas p-2" onChange={(e) => updateRule("allow_fallback", e.target.value === "inherit" ? undefined : e.target.value === "true")}>
            <option value="inherit">Inherit ({inherited?.allow_fallback === false ? "stop" : "allow fallback"})</option>
            <option value="true">Allow fallback</option><option value="false">Stop and show error</option>
          </select>
        </label>
        <p className="text-xs text-muted">Effective: {effectiveMode} · {!data.policy.enabled || !effectiveOutgoing ? "stop on failure" : "allow fallback"}</p>
      </>}
      <button type="button" disabled={!dirty} onClick={() => void save()} className="rounded bg-accent px-3 py-2 text-xs font-medium text-black disabled:opacity-50">{busy ? "Saving…" : "Save fallback settings"}</button>
    </fieldset>}
    {message && <p role="status" className="text-xs text-muted">{message}</p>}
  </section>;
}
