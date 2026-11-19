import { useEffect, useState } from "react";
import { api } from "../api/client";

interface Policy { enabled: boolean; max_duration_seconds: number; max_model_calls: number }
function valid(value: unknown): value is Policy {
  if (!value || typeof value !== "object") return false;
  const policy = value as Policy;
  return typeof policy.enabled === "boolean"
    && Number.isInteger(policy.max_duration_seconds) && policy.max_duration_seconds >= 1 && policy.max_duration_seconds <= 604800
    && Number.isInteger(policy.max_model_calls) && policy.max_model_calls >= 1 && policy.max_model_calls <= 100000;
}

export function PersistenceSettings() {
  const [policy, setPolicy] = useState<Policy>();
  const [dirty, setDirty] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  useEffect(() => {
    let active = true;
    void api<{ policy: Policy }>("/api/settings/persistence").then((result) => {
      if (!valid(result?.policy)) throw new Error("Could not load persistence settings.");
      if (active) setPolicy(result.policy);
    }).catch((cause) => { if (active) setError(cause instanceof Error ? cause.message : String(cause)); });
    return () => { active = false; };
  }, []);
  const update = (change: Partial<Policy>) => {
    if (policy) { setPolicy({ ...policy, ...change }); setDirty(true); setMessage(""); }
  };
  const save = async () => {
    if (!policy || !valid(policy)) return;
    setBusy(true); setError(""); setMessage("");
    try {
      const result = await api<{ policy: Policy }>("/api/settings/persistence", { method: "POST", body: JSON.stringify(policy) });
      if (!valid(result?.policy)) throw new Error("Could not save persistence settings.");
      setPolicy(result.policy); setDirty(false); setMessage("Saved. Applies to new requests.");
    } catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)); }
    finally { setBusy(false); }
  };
  const control = "mt-1 block w-full rounded-lg border border-line bg-canvas px-3 py-2 text-sm text-ink";
  return <section aria-label="Persistence settings" className="space-y-4 rounded-2xl border border-line bg-canvas/35 p-4 text-sm">
    <div><h3 className="font-semibold text-ink">Keep working through recoverable failures</h3>
      <p className="mt-2 text-xs leading-relaxed text-muted">Let the agent try another approach and continue from saved progress while there is evidence of useful work. Existing task modes and approvals still apply.</p></div>
    {error && <p role="alert" className="text-xs text-warn">{error}</p>}
    {!policy && !error && <p role="status" className="text-xs text-muted">Loading persistence settings…</p>}
    {policy && <fieldset disabled={busy} className="space-y-4">
      <label className="flex items-center gap-3"><input type="checkbox" className="accent-accent" checked={policy.enabled} onChange={(event) => update({ enabled: event.target.checked })} />Continue automatically within these limits</label>
      <div className="grid gap-4 sm:grid-cols-2">
        <label className="text-xs text-muted">Active work limit (minutes)<input aria-label="Persistence time limit in minutes" className={control} type="number" min="1" max="10080" step="1" disabled={!policy.enabled} value={policy.max_duration_seconds / 60} onChange={(event) => update({ max_duration_seconds: Number(event.target.value) * 60 })} /></label>
        <label className="text-xs text-muted">Model call limit<input aria-label="Persistence model call limit" className={control} type="number" min="1" max="100000" step="1" disabled={!policy.enabled} value={policy.max_model_calls} onChange={(event) => update({ max_model_calls: Number(event.target.value) })} /></label>
      </div>
      <p className="text-xs leading-relaxed text-muted">Up to 7 days and 100,000 model calls per request (default 24 hours and 5,000), shared with its workers and retries. Only active work counts; waiting for your input and time offline are excluded. Saved progress keeps the same limits after a restart. Changes apply to new requests.</p>
      <button type="button" disabled={!dirty || !valid(policy) || busy} onClick={() => void save()} className="rounded-lg bg-accent px-3 py-2 text-xs font-medium text-black disabled:opacity-50">{busy ? "Saving…" : "Save persistence settings"}</button>
    </fieldset>}
    {message && <p role="status" className="text-xs text-muted">{message}</p>}
  </section>;
}
