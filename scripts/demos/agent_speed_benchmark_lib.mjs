import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";

/** Track only observed parent-turn events; a scheduled call is not execution. */
export function experienceTimings(events, turnId, serverAcceptanceMs, completionMs) {
  const valid = (n) => Number.isFinite(n) && n >= 0 ? Math.round(n) : null;
  const first = (predicate) => {
    const times = events.filter(({frame, elapsed_ms}) => frame?.turn_id === turnId && valid(elapsed_ms) !== null && predicate(frame))
      .map(({elapsed_ms}) => elapsed_ms);
    return times.length ? valid(Math.min(...times)) : null;
  };
  return {
    local_acknowledgment_ms: null,
    server_acceptance_ms: valid(serverAcceptanceMs),
    first_model_commentary_ms: first((frame) => frame.kind === "assistant_progress" && frame.payload?.origin === "model" &&
      typeof frame.payload?.text === "string" && frame.payload.text.trim() !== ""),
    first_confirmed_tool_result_ms: first((frame) => frame.kind === "tool_card" &&
      frame.tool_card?.status === "done" && frame.tool_card.cached !== true),
    completion_ms: valid(completionMs),
    note: "Headless observations include transport and event-stream delivery. Local UI acknowledgment is not measured. A successful non-cached tool result is evidence of activity, not its start time; failures and scheduled calls are excluded.",
  };
}

/** Diagnostic windows are wall times, not CPU time or a provider-only measure. */
export function analyzeTimings(jobs, modelEvents, taskElapsedMs, modelStarts = [], parentSessionId = null) {
  function adapter(sessionId) {
    const events = (Array.isArray(modelEvents) ? modelEvents : []).filter((event) => event.chat_id === sessionId);
    const durations = events.map((event) => event.elapsed_ms).filter((ms) => Number.isFinite(ms) && ms >= 0);
    const starts = modelStarts.filter((event) => event.chat_id === sessionId);
    const expected = starts.length || null;
    const steps = starts.map((event) => event.step);
    // Any retry, repeated step, absent or malformed event makes the accounting
    // ambiguous; do not turn a partial duration into a misleading remainder.
    const complete = expected !== null && steps.every(Number.isInteger) && new Set(steps).size === steps.length &&
      events.length === steps.length && durations.length === events.length &&
      steps.every((step) => events.filter((event) => event.step === step).length === 1);
    return {model_adapter_calls: durations.length, expected_model_steps: expected,
      model_adapter_ms: complete ? Math.round(durations.reduce((a, b) => a + b, 0)) : null};
  }
  const workers = (Array.isArray(jobs) ? jobs : []).map((job) => {
    const start = job.started_at, end = job.ended_at;
    const valid = Number.isFinite(start) && Number.isFinite(end) && end >= start;
    // A missing or partial timing stream must never be represented as zero.
    const total = valid ? Math.round((end - start) * 1000) : null;
    const metrics = adapter(job.session_id);
    const model = metrics.model_adapter_ms;
    return {agent: job.spec?.persona_id ?? null, session_id: job.session_id ?? null,
      started_at: valid ? start : null, ended_at: valid ? end : null, elapsed_ms: total,
      ...metrics,
      other_ms: total !== null && model !== null && model <= total ? Math.round(total - model) : null};
  });
  const timed = workers.filter((worker) => worker.started_at !== null);
  const complete = workers.length > 0 && timed.length === workers.length;
  const span = complete ? Math.round((Math.max(...timed.map((w) => w.ended_at)) - Math.min(...timed.map((w) => w.started_at))) * 1000) : null;
  const spread = complete ? Math.round((Math.max(...timed.map((w) => w.started_at)) - Math.min(...timed.map((w) => w.started_at))) * 1000) : null;
  const parent = parentSessionId ? adapter(parentSessionId) : null;
  return {workers, parent, worker_span_ms: span, worker_start_spread_ms: spread,
    outside_worker_span_ms: span !== null && Number.isFinite(taskElapsedMs) && taskElapsedMs >= span ? Math.round(taskElapsedMs - span) : null,
    note: "Model adapter time includes provider transport, retries, backoff and local adapter work; it is not provider-only time. Other time is an unclassified remainder. Durations use monotonic adapter clocks and wall-clock worker timestamps; clock adjustments can make the remainder unavailable."};
}

export function schedule(rounds) {
  if (!Number.isInteger(rounds) || rounds < 1 || rounds > 100) throw Error("Trials must be an integer from 1 to 100");
  const widths = [1, 2, 4];
  return Array.from({length: rounds}, (_, round) => widths.map((_, i) => widths[(i + round) % widths.length])).flat();
}

export function waitComparisonSchedule(rounds) {
  if (!Number.isInteger(rounds) || rounds < 1 || rounds > 100) throw Error("Trials must be an integer from 1 to 100");
  return Array.from({length: rounds}, (_, round) => (round % 2 ? ["all", "any"] : ["any", "all"])
    .map((wait_mode) => ({width: 4, wait_mode}))).flat();
}

export function summarizeWaitModes(trials) {
  return ["any", "all"].map((wait_mode) => ({wait_mode,
    ...summarize(trials.filter((trial) => trial.wait_mode === wait_mode)).find((row) => row.width === 4)}));
}

// Nearest-rank percentile; report sample counts because small samples are noisy.
export function percentile(values, fraction) {
  if (!values.length) return null;
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.max(0, Math.ceil(fraction * sorted.length) - 1)];
}

/** Each milestone has its own sample count; absent commentary is not zero. */
export function summarizeExperience(trials) {
  const keys = ["server_acceptance_ms", "first_model_commentary_ms", "first_confirmed_tool_result_ms", "completion_ms"];
  return [1, 2, 4].map((width) => {
    const rows = trials.filter((trial) => trial.width === width && trial.verified);
    const metrics = Object.fromEntries(keys.map((key) => {
      const values = rows.map((row) => row.experience_timings?.[key])
        .filter((value) => typeof value === "number" && Number.isFinite(value) && value >= 0);
      return [key, {samples: values.length, missing: rows.length - values.length,
        p50_ms: percentile(values, 0.5), p95_ms: percentile(values, 0.95)}];
    }));
    return {width, verified: rows.length, metrics};
  });
}

export function fixtureHash(root) {
  const hash = crypto.createHash("sha256");
  for (let i = 1; i <= 4; i++) {
    const name = `fixtures/task-${i}.txt`;
    const bytes = fs.readFileSync(path.join(root, name));
    hash.update(`${name}\0${bytes.length}\0`);
    hash.update(bytes);
  }
  return hash.digest("hex");
}

export function summarize(trials) {
  return [1, 2, 4].map((width) => {
    const rows = trials.filter((trial) => trial.width === width);
    const valid = rows.filter((trial) => trial.verified && Number.isFinite(trial.elapsed_ms) && trial.elapsed_ms >= 0);
    const times = valid.map((trial) => trial.elapsed_ms);
    const costs = valid.map((trial) => trial.cost_usd);
    return {width, attempts: rows.length, verified: valid.length, failed: rows.length - valid.length,
      p50_ms: percentile(times, 0.5), p95_ms: percentile(times, 0.95),
      mean_cost_usd: costs.length && costs.every((cost) => typeof cost === "number" && Number.isFinite(cost) && cost >= 0)
        ? costs.reduce((sum, cost) => sum + cost, 0) / costs.length : null};
  });
}

/** Describe these samples only; never change runtime routing from this result. */
export function observedFastest(results) {
  if (results.length !== 3 || [1, 2, 4].some((width) => !results.some((row) => row.width === width)) ||
    results.some((row) => row.verified < 3 || row.failed !== 0 ||
    row.verified !== row.attempts || !Number.isFinite(row.p50_ms))) {
    return {available: false, reason: "Requires at least three verified trials and no failed trials for each width."};
  }
  const median = Math.min(...results.map((row) => row.p50_ms));
  const widths = results.filter((row) => row.p50_ms === median).map((row) => row.width);
  return {available: true, widths, p50_ms: median,
    note: "Describes observed medians for this fixture only; it is not a statistically established speedup or an automatic routing recommendation."};
}

/** Return a safe, actionable description without copying provider error payloads. */
export function fatalFailure(report, log = "") {
  const workerDetails = Array.isArray(report?.jobs) ? report.jobs.flatMap((job) =>
    [job?.result?.failure_detail, job?.result?.failure_cause, job?.result?.message]) : [];
  const details = [report?.parent?.failure_detail, report?.parent?.failure_cause, report?.error, ...workerDetails, log]
    .filter((value) => typeof value === "string").join("\n");
  if (/Saved credentials are locked|Keychain.*(?:locked|status -25293)|Keychain authorization (?:failed|unavailable)/i.test(details)) {
    return "Saved credentials are locked; rerun with --unlock-vault and authorize the Keychain prompt.";
  }
  const modelHttp = details.match(/\bModel returned HTTP\s+(401|403|404)\b/i);
  if (modelHttp) {
    if (modelHttp[1] === "404") return "Model endpoint or model was not found (HTTP 404); check provider URL and model configuration.";
    return `Model provider rejected authentication or access (HTTP ${modelHttp[1]}); check the configured provider credentials and access.`;
  }
  if (/Model is not configured|Use an HTTPS model endpoint|Build first: cargo build -p kageha-loop|no such table: ui_documents/i.test(details)) {
    return "Benchmark configuration is incomplete; check the model, HTTPS provider, settings database and runtime binary.";
  }
  return null;
}

/** Execute in order, reporting every attempted trial and stopping on a fatal setup error. */
export async function runTrialSequence(widths, runTrial, onTrial = () => {}) {
  const trials = [];
  for (let index = 0; index < widths.length; index++) {
    const trial = await runTrial(widths[index], index);
    trials.push(trial);
    await onTrial(trials, trial);
    if (trial.stop_reason) break;
  }
  return trials;
}
