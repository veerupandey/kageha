import { useEffect, useState, type KeyboardEvent } from "react";
import { cn } from "../lib/cn";
import { Icon } from "../lib/icons";
import { api } from "../api/client";

type Mode = "off" | "shadow" | "active";
interface Override { mode?: Mode; model?: string; threshold?: number; timeout_s?: number; confidence_cutoff?: number | null }
interface Policy {
  mode: Mode; browser_actions_follow_mode?: boolean; model: string; fallback_models: string[]; judge_fallback: boolean;
  threshold: number; timeout_s: number; overrides: Record<string, Override>;
  max_states?: number; max_reviews?: number; max_action_decisions?: number; max_batch_actions?: number; batch_threshold?: number; confidence_cutoff?: number | null; batch_confidence_cutoff?: number | null;
}
interface Model { id: string; label?: string; model?: string; ready: boolean; reason: string; decision_backend?: boolean; alias?: boolean }
interface Inventory {
  policy: Policy; purposes: string[]; models: Model[]; browser_action_inheritance?: boolean;
  codex: { supported: boolean; reason: string };
}

type ScoreMetric = "selected_probability" | "provider_confidence";
interface ProbeResult { ok: boolean; reason?: string; elapsed_ms: number }
const probeReasons: Record<string, string> = {
  credentials_unavailable: "The provider credentials are missing or locked.",
  provider_authentication_failed: "The provider rejected the configured credentials.",
  provider_unavailable: "The decision provider is unavailable.",
  provider_error: "The provider rejected the typed decision request.",
  rate_limited: "The provider rate-limited the test.",
  timeout: "The provider did not answer within five seconds.",
  invalid_response: "The provider returned an invalid typed decision.",
  incorrect_probe_answer: "The provider returned the wrong answer to the test question.",
  missing_confidence: "The provider did not return the selected confidence score.",
};

const PURPOSE_DETAILS: Record<string, { title: string; description: string }> = {
  intake: { title: "Understand requests", description: "Classify intent, effort, and required checks." },
  desktop_guidance: { title: "Mac desktop routing", description: "Guide native CUA observations: read visible text, group known fields, wait, or handle a blocker. Reuses unchanged accessibility state and evaluates at most three states per turn. Recovery and human handoff require at least 0.90. Never generates clicks or certifies completion." },
  browser_guidance: { title: "Browser routing", description: "Guide native browser observations using permitted controls, visible content, bounded waits, or blocker inspection. Unchanged states reuse advice. Never generates clicks or certifies completion." },
  browser_action: { title: "Browser actions", description: "Let the decision model choose a page action or fill related fields together. Uncertain steps go to the main agent." },
  browser_review: { title: "Optional browser action review", description: "Use this decision model for browser actions that already require automatic review. Off preserves your existing reviewer. This cannot enable automatic approval, override browser safeguards, or certify completion." },
  verification: { title: "Verification needed?", description: "Decide whether the executor needs one focused inspection. This does not grade the answer or trigger a separate reviewer. Active can request one check; Compare records the decision; Off skips it." },
  resume_continuity: { title: "Resume unfinished work", description: "Resolve ambiguous follow-ups against the saved task. Matching requests keep their checkpoint; browser state must be checked before acting." },
  observation_intent: { title: "Read-only completion", description: "Recognize locate and quote tasks without changing files." },
  skill_selection: { title: "Skill selection", description: "Prioritize relevant eligible skills. Explicit and pinned skills stay first." },
  tool_selection: { title: "Tool selection", description: "Prioritize relevant tools within the permitted catalog. Required tools remain available." },
  memory_relevance: { title: "Memory relevance", description: "Choose useful verified past execution notes for the current task." },
  context_retention: { title: "Context retention", description: "Remove redundant old assistant chatter under context pressure. Keep user messages and tool evidence." },
  model_routing: { title: "Automatic model routing", description: "Choose among eligible execution models. Explicit model selections take priority." },
  recovery: { title: "Recovery guidance", description: "Suggest a recovery route for the detailed reviewer to validate against observed evidence." },
  notification_triage: { title: "Notification priority", description: "Prioritize the reminder inbox. Requested reminders and delivery are preserved." },
};

function radioKeys(event: KeyboardEvent<HTMLDivElement>) {
  if (!["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Home", "End"].includes(event.key)) return;
  const radios = Array.from(event.currentTarget.querySelectorAll<HTMLButtonElement>('button[role="radio"]:not(:disabled)'));
  if (!radios.length) return;
  const current = radios.indexOf(event.target as HTMLButtonElement);
  const index = event.key === "Home" ? 0 : event.key === "End" ? radios.length - 1
    : (current + (event.key === "ArrowLeft" || event.key === "ArrowUp" ? -1 : 1) + radios.length) % radios.length;
  event.preventDefault(); radios[index].focus(); radios[index].click();
}

export function DecisionSettings() {
  const [data, setData] = useState<Inventory>();
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);
  const [saved, setSaved] = useState("");
  const [probe, setProbe] = useState<{ key: string; result?: ProbeResult }>();
  useEffect(() => {
    let active = true;
    void api<Inventory>("/api/models/decisions").then((value) => {
      if (!value?.policy || !Array.isArray(value.models) || !Array.isArray(value.purposes)) throw new Error("Invalid decision settings response");
      if (active) { setData(value); setSaved(JSON.stringify(value.policy)); }
    }).catch((e) => { if (active) setError(String(e)); });
    return () => { active = false; };
  }, []);
  const update = (patch: Partial<Policy>) => {
    if (data) setData({ ...data, policy: { ...data.policy, ...patch } });
    setMessage("");
  };
  const save = async () => {
    if (!data) return;
    setBusy(true); setError(""); setMessage("");
    try {
      const result = await api<{ policy: Policy; message: string }>("/api/models/decisions", {
        method: "POST", body: JSON.stringify(data.policy),
      });
      if (!result?.policy) throw new Error("Invalid save response");
      setData({ ...data, policy: result.policy }); setSaved(JSON.stringify(result.policy)); setMessage("Settings saved. Applies to new runs.");
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  };
  const invalidLimits = !!data && [data.policy, ...Object.values(data.policy.overrides)].some((limits) =>
    (limits.confidence_cutoff != null && (!Number.isFinite(limits.confidence_cutoff) || limits.confidence_cutoff < 0 || limits.confidence_cutoff > 1)) ||
    (limits.threshold !== undefined && (!Number.isFinite(limits.threshold) || limits.threshold < 0.5 || limits.threshold > 1)) ||
    (limits.timeout_s !== undefined && (!Number.isFinite(limits.timeout_s) || limits.timeout_s < 0.1 || limits.timeout_s > 15))) || !!data && [[data.policy.max_states,1,32],[data.policy.max_action_decisions,1,256],[data.policy.max_batch_actions,1,16]].some(([value,min,max])=>value!==undefined&&(!Number.isInteger(value)||value<min!||value>max!));
  const invalidBatchThreshold = data?.policy.batch_threshold !== undefined && (!Number.isFinite(data.policy.batch_threshold) || data.policy.batch_threshold < 0.5 || data.policy.batch_threshold > 1);
  const invalidBatchConfidence = data?.policy.batch_confidence_cutoff != null && (!Number.isFinite(data.policy.batch_confidence_cutoff) || data.policy.batch_confidence_cutoff < 0 || data.policy.batch_confidence_cutoff > 1);
  const action = data?.policy.overrides.browser_action;
  const followsMode = data?.policy.browser_actions_follow_mode === true;
  const actionMode = action?.mode ?? (followsMode ? data?.policy.mode : "off") ?? "off";
  const actionModel = data?.models.find(m => m.id === (action?.model || data.policy.model));
  const actionScore: ScoreMetric = (action?.confidence_cutoff ?? data?.policy.confidence_cutoff) == null
    ? "selected_probability" : "provider_confidence";
  const probeKey = actionModel ? `${actionModel.id}:${actionScore}` : "";
  const currentProbe = probe?.key === probeKey ? probe : undefined;
  const invalidActionModel = actionMode !== "off" && (!actionModel?.decision_backend || !actionModel.ready);
  async function testDecisionModel() {
    if (!actionModel?.ready || currentProbe && !currentProbe.result) return;
    setProbe({ key: probeKey });
    try {
      const result = await api<ProbeResult>("/api/models/decisions/test", {
        method: "POST", body: JSON.stringify({ model_id: actionModel.id, score_metric: actionScore }),
      });
      setProbe(current => current?.key === probeKey ? { key: probeKey, result } : current);
    } catch {
      setProbe(current => current?.key === probeKey
        ? { key: probeKey, result: { ok: false, reason: "provider_unavailable", elapsed_ms: 0 } } : current);
    }
  }
  const dirty = !!data && JSON.stringify(data.policy) !== saved;
  const models = (data?.models ?? []).filter(model => model.decision_backend);
  const catalog = models.filter(m => m.decision_backend && (!m.alias || m.id === data?.policy.model));
  const label = (id: string) => models.find(m => m.id === id)?.label || id;
  const control = "w-full min-w-0 rounded-xl border border-line bg-surface px-3 py-2.5 text-sm text-ink outline-none transition focus:border-accent focus:ring-2 focus:ring-accent/20 disabled:opacity-50";
  const options = models.map((m) => <option key={m.id} value={m.id} disabled={!m.ready}>{label(m.id)}{m.ready ? "" : " · Setup required"}</option>);
  const modes: { id: Mode; name: string; subtitle: string; detail: string }[] = [
    { id: "off", name: "Off", subtitle: "Use the existing agent", detail: "Tasks that inherit this mode use the regular agent." },
    { id: "shadow", name: "Compare", subtitle: "Shadow mode", detail: "Runs alongside existing behavior. Results are logged for comparison and do not control actions. Additional model calls incur usage." },
    { id: "active", name: "Use decisions", subtitle: "Active mode", detail: "Eligible browser actions can run directly. Uncertain steps go to the regular agent." },
  ];
  function taskControls(purpose: string) {
    if (!data) return null;
    const override = data.policy.overrides[purpose] ?? {};
    const setOverride = (key: keyof Override, value: string) => {
      const next = { ...override };
      if (value) Object.assign(next, { [key]: key === "threshold" || key === "timeout_s" || key === "confidence_cutoff" ? Number(value) : value }); else delete next[key];
      const overrides = { ...data.policy.overrides };
      if (Object.keys(next).length) overrides[purpose] = next; else delete overrides[purpose];
      update({ overrides });
    };
    const confidenceMode=purpose === "browser_action" && (override.confidence_cutoff != null || data.policy.confidence_cutoff != null);
    const limits = <>
      <div className="grid gap-2 sm:grid-cols-2">
        {purpose !== "browser_review" && <label className="space-y-1 text-xs text-muted">Minimum probability
          <input className={control} aria-label={`${purpose} probability threshold`} disabled={confidenceMode} type="number" min="0.5" max="1" step="0.01" placeholder={`Inherit (${data.policy.threshold})`} value={override.threshold ?? ""} onChange={(e) => setOverride("threshold", e.target.value)} />
        </label>}
        <label className="space-y-1 text-xs text-muted">Time budget (seconds)
          <input className={control} aria-label={`${purpose} timeout`} type="number" min="0.1" max="15" step="0.1" placeholder={`Inherit (${data.policy.timeout_s})`} value={override.timeout_s ?? ""} onChange={(e) => setOverride("timeout_s", e.target.value)} />
        </label>
      </div>
      {purpose === "browser_action" && <label className="block space-y-1 text-xs text-muted">Provider confidence cutoff
        <input className={control} aria-label="Browser action confidence cutoff" type="number" min="0" max="1" step="0.001" placeholder={data.policy.confidence_cutoff == null ? "Empty uses legacy probability cutoff" : `Inherit (${data.policy.confidence_cutoff})`} value={override.confidence_cutoff ?? ""} onChange={(e)=>setOverride("confidence_cutoff",e.target.value)} />
        <span className="block text-[0.68rem] leading-relaxed">Optional. Leave empty to use selected probability. If the provider omits confidence, the regular agent takes over.</span>
      </label>}
      {purpose === "browser_action" && <label className="block space-y-1 text-xs text-muted">Action decisions per run
        <input className={control} aria-label="Browser action decision budget" type="number" min="1" max="256" step="1" value={data.policy.max_action_decisions ?? 64} onChange={(e) => update({ max_action_decisions: Number(e.target.value) })} />
      </label>}
      {purpose === "browser_action" && <><label className="block space-y-1 text-xs text-muted">Maximum actions per batch
        <input className={control} aria-label="Browser decision batch size" type="number" min="1" max="16" step="1" value={data.policy.max_batch_actions ?? 8} onChange={(e)=>update({max_batch_actions:Number(e.target.value)})} />
      </label>
      <label className="block space-y-1.5"><span className="text-xs font-medium text-ink">{confidenceMode ? "Minimum batch confidence" : "Minimum batch probability"}</span><input className={control} aria-label={confidenceMode ? "Browser batch confidence cutoff" : "Browser batch probability threshold"} type="number" min={confidenceMode ? "0" : "0.5"} max="1" step="0.001" value={(confidenceMode ? data.policy.batch_confidence_cutoff : data.policy.batch_threshold) ?? 0.99} onChange={(e)=>update(confidenceMode ? {batch_confidence_cutoff:Number(e.target.value)} : {batch_threshold:Number(e.target.value)})} /><span className="block text-[0.68rem] leading-relaxed text-muted">Every selected field, eligibility answer and batch check must pass. This is not the probability that the whole batch succeeds.</span></label></>}
      {purpose === "browser_guidance" && <label className="block space-y-1 text-xs text-muted">Distinct browser states per run
        <input className={control} aria-label="Browser decision state budget" type="number" min="1" max="32" step="1" value={data.policy.max_states ?? 3} onChange={(e) => update({ max_states: Number(e.target.value) })} />
      </label>}
    </>;
    return <div key={purpose} className="space-y-2">
      <div><p className="text-xs font-medium text-ink">{PURPOSE_DETAILS[purpose]?.title ?? purpose}</p><p className="mt-1 text-[0.68rem] text-muted">{PURPOSE_DETAILS[purpose]?.description ?? "Use the configured decision model for this task."}</p></div>
      <div className="grid gap-2 sm:grid-cols-2">
        <label className="text-xs text-muted">Mode<select className={control} aria-label={`${purpose} decision mode`} value={override.mode ?? (purpose === "browser_review" || (purpose === "browser_action" && !followsMode) ? "off" : "")} onChange={(e) => setOverride("mode", e.target.value)}>{purpose !== "browser_review" && (purpose !== "browser_action" || followsMode) && <option value="">
          {followsMode && (purpose === "browser_guidance" || purpose === "recovery") ? "Default (Off)" : "Inherit mode"}
        </option>}<option value="off">Off</option><option value="shadow">Compare (Shadow)</option><option value="active">Use decisions (Active)</option></select></label>
        <label className="text-xs text-muted">Model<select className={control} aria-label={`${purpose} decision model`} value={override.model ?? ""} onChange={(e) => setOverride("model", e.target.value)}><option value="">Inherit model</option>{purpose === "browser_action" ? models.filter(m=>(!m.alias || m.id === override.model || m.id === data.policy.model)).map(m=><option key={m.id} value={m.id} disabled={!m.ready}>{label(m.id)}</option>) : options}</select></label>
      </div>
      {purpose === "browser_action" && <details className="pt-1 text-xs"><summary className="cursor-pointer text-muted">Confidence and action limits</summary><div className="mt-3 space-y-3">{limits}</div></details>}
      {purpose !== "browser_action" && limits}
    </div>;
  }
  return <section aria-label="Decision model settings" className="overflow-hidden rounded-2xl border border-line bg-canvas/35 text-sm">
    {error && <p role="alert" className="mx-5 mt-4 rounded-xl border border-warn/30 bg-warn/5 p-3 text-xs text-warn">{error}</p>}
    {!data && !error && <p role="status" className="animate-pulse px-6 py-8 text-muted">Loading decision settings…</p>}
    {data && <>
      <fieldset disabled={busy} className="min-w-0 space-y-6 p-5 sm:p-6">
        <div>
          <div className="mb-3 flex items-center justify-between"><h4 className="text-xs font-semibold text-ink">Default mode</h4><span className="text-[0.65rem] text-muted">For new runs</span></div>
          <div role="radiogroup" aria-label="Decision mode" onKeyDown={radioKeys} className="grid gap-2 sm:grid-cols-3">
            {modes.map((mode) => <button key={mode.id} type="button" role="radio" aria-label={`${mode.name} — ${mode.subtitle}`} aria-checked={data.policy.mode === mode.id}
              className={cn("min-w-0 rounded-xl border p-3 text-left transition-all duration-150 focus-visible:outline-2 focus-visible:outline-accent", data.policy.mode === mode.id ? "border-accent bg-accent-soft shadow-xs ring-1 ring-accent/30" : "border-line/70 bg-surface/60 hover:border-line hover:bg-surface")}
              onClick={() => update({ mode: mode.id,
                ...(data.browser_action_inheritance && mode.id !== "off" ? { browser_actions_follow_mode: true } : {}),
              })}>
              <span className="flex items-center justify-between gap-2"><span className="text-sm font-medium text-ink">{mode.name}</span>{data.policy.mode === mode.id && <Icon.Check size={14} className="shrink-0 text-accent" />}</span>
              <span className="mt-1 block text-[0.68rem] text-muted">{mode.subtitle}</span>
            </button>)}
          </div>
          <p className="mt-3 rounded-xl bg-surface/60 px-3 py-3 text-xs leading-relaxed text-muted">{modes.find((m) => m.id === data.policy.mode)?.detail}</p>
        </div>
        <div>
          <h4 className="mb-2 text-xs font-semibold text-ink">Default decision model</h4>
          <div role="radiogroup" aria-label="Default decision model" onKeyDown={radioKeys} className="divide-y divide-line overflow-hidden rounded-xl border border-line">
            {catalog.map(model => {
              const selected = data.policy.model === model.id;
              return <button key={model.id} type="button" role="radio" aria-label={`${label(model.id)} ${model.ready ? "Configured" : "Setup required"}`} aria-checked={selected} disabled={!model.ready}
                onClick={() => update({ model: model.id, fallback_models: data.policy.fallback_models.filter(id => id !== model.id) })}
                className={cn("flex w-full items-center gap-3 px-3 py-2.5 text-left focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-accent disabled:cursor-not-allowed", selected ? "bg-accent-soft" : "bg-surface/40 enabled:hover:bg-surface")}>
                <span aria-hidden className={cn("size-3.5 shrink-0 rounded-full border", selected ? "border-4 border-accent" : "border-muted/50")} />
                <span className="min-w-0 flex-1"><span className="block font-medium text-ink">{label(model.id)}</span>
                  <span className="block break-all text-xs text-muted">{model.model || model.id}</span>
                  {!model.ready && <span className="mt-1 block text-xs text-muted">{model.reason}</span>}
                </span>
                <span className="shrink-0 text-xs text-muted">{model.ready ? "Configured" : "Setup required"}</span>
              </button>;
            })}
          </div>
        </div>
        {data.purposes.includes("browser_action") && <div className="space-y-3 rounded-xl border border-line bg-surface/30 p-4">
          {taskControls("browser_action")}
          <p className="text-xs text-muted">Effective browser mode · {actionMode === "active" ? "Direct execution" : actionMode === "shadow" ? "Compare" : "Off"}</p>
          {data.browser_action_inheritance && !followsMode && data.policy.mode !== "off" && action?.mode === undefined &&
            <button type="button" className="text-xs font-medium text-accent underline"
              onClick={() => update({ browser_actions_follow_mode: true })}>Use the default mode for browser actions</button>}
          {data.browser_action_inheritance && <div className="space-y-2">
            <button type="button" className="rounded-lg border border-line px-3 py-1.5 text-xs disabled:opacity-50"
              disabled={!actionModel?.ready || Boolean(currentProbe && !currentProbe.result)}
              onClick={() => void testDecisionModel()}>{currentProbe && !currentProbe.result ? "Testing…" : "Test decision model"}</button>
            {currentProbe?.result && <p role={currentProbe.result.ok ? "status" : "alert"} className="text-xs text-muted">
              {currentProbe.result.ok ? `Typed decision succeeded in ${currentProbe.result.elapsed_ms} ms.`
                : probeReasons[currentProbe.result.reason ?? ""] ?? "The decision model test failed."}
            </p>}
          </div>}
        </div>}
        <details className="group rounded-xl border border-line bg-surface/30">
          <summary className="cursor-pointer px-4 py-3 text-xs font-medium text-ink">Customize by task <span className="ml-1 font-normal text-muted">· Optional</span></summary>
          <div className="space-y-4 border-t border-line p-4">
            <p className="text-xs leading-relaxed text-muted">Override the default mode, model or limits for an individual task.</p>
            {data.purposes.filter(purpose => purpose !== "browser_action").map(taskControls)}
          </div>
        </details>
        <details className="rounded-xl border border-line bg-surface/30">
          <summary className="cursor-pointer px-4 py-3 text-xs font-medium text-ink">Reliability & limits <span className="ml-1 font-normal text-muted">· Advanced</span></summary>
          <div className="space-y-4 border-t border-line p-4">
            <label className="block space-y-1.5"><span className="text-xs font-medium text-ink">Backup decision model</span><select className={control} aria-label="Decision fallback order" value={data.policy.fallback_models.join(",")} onChange={(e) => update({ fallback_models: e.target.value ? e.target.value.split(",") : [] })}><option value="">No backup model</option>{models.filter((m) => m.id !== data.policy.model).map((m) => <option key={m.id} value={m.id} disabled={!m.ready}>{label(m.id)}</option>)}</select></label>
            <label className="flex items-start gap-3 rounded-xl bg-surface/60 p-3"><input className="mt-0.5 accent-accent" type="checkbox" checked={data.policy.judge_fallback} onChange={(e) => update({ judge_fallback: e.target.checked })} /><span><span className="block text-xs font-medium text-ink">Fall back to the existing judge</span><span className="mt-1 block text-[0.68rem] leading-relaxed text-muted">Applies to intent classification. The inspection gate never invokes a fallback reviewer. Rankings preserve existing behavior if unavailable.</span></span></label>
            <div className="grid gap-4 sm:grid-cols-2">
              <label className="block space-y-1.5"><span className="text-xs font-medium text-ink">Minimum answer probability</span><input className={control} aria-label="Decision probability threshold" type="number" min="0.5" max="1" step="0.01" required value={data.policy.threshold} onChange={(e) => update({ threshold: Number(e.target.value) })} /><span className="block text-[0.68rem] leading-relaxed text-muted">0.90 requires a strong model preference. It does not guarantee 90% accuracy.</span></label>
              <label className="block space-y-1.5"><span className="text-xs font-medium text-ink">Time budget (seconds)</span><input className={control} aria-label="Decision timeout" type="number" min="0.1" max="15" step="0.1" required value={data.policy.timeout_s} onChange={(e) => update({ timeout_s: Number(e.target.value) })} /><span className="block text-[0.68rem] leading-relaxed text-muted">Shared across the primary and backup decision models.</span></label>
            </div>
          </div>
        </details>
      </fieldset>
      <div className="flex flex-wrap items-center justify-between gap-3 border-t border-line bg-surface/40 px-5 py-4 sm:px-6">
        <p role="status" aria-live="polite" className="text-xs text-muted">{invalidActionModel ? "Choose a connected typed model for browser actions." : message || (dirty ? "Unsaved changes" : "All changes saved")}</p>
        <div className="flex gap-2">
          {dirty && <button type="button" disabled={busy} className="rounded-xl border border-line px-3 py-2 text-xs text-muted hover:text-ink" onClick={() => { setData({ ...data, policy: JSON.parse(saved) as Policy }); setError(""); setMessage(""); }}>Discard</button>}
          <button type="button" disabled={busy || !dirty || invalidLimits || invalidBatchThreshold || invalidBatchConfidence || invalidActionModel} onClick={() => void save()} className="rounded-xl bg-accent px-4 py-2 text-xs font-semibold text-black transition hover:brightness-110 disabled:cursor-not-allowed disabled:opacity-40">{busy ? "Saving…" : "Save decision settings"}</button>
        </div>
      </div>
    </>}
  </section>;
}
