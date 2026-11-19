// Paired, sequential real-model benchmark. All browser/session state is disposable.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { spawn } from 'node:child_process';
import assert from 'node:assert/strict';

const host = process.env.KAGEHA_LOOP_BIN;
const browser = process.env.KAGEHA_BROWSER_BIN;
assert(host && browser, 'Set frozen host and browser binary paths');
const qualify = process.argv.includes('--qualify');
const typed = !process.argv.includes('--advice-only');
const scenarios = qualify ? ['ordinary-search', 'ordinary-chat']
  : process.argv.includes('--ordinary-search') ? ['ordinary-search']
  : process.argv.includes('--ordinary-chat') ? ['ordinary-chat'] : ['workflow'];
const pairs = Number(process.env.KAGEHA_BENCH_PAIRS || (qualify ? 10 : 3));
assert(Number.isInteger(pairs) && pairs >= 1 && pairs <= 10);
assert(!qualify || (pairs === 10 && typed), 'Qualification requires ten pairs and a typed provider');
const model = process.env.KAGEHA_TEST_DECISION_MODEL
  || (qualify ? 'typesafe/jev-1.14.0' : typed ? '@cf/cloudflare/clef-flash' : 'advice-fixture');
const executor = process.env.KAGEHA_TEST_MODEL || 'gpt-5.4';
const env = { ...process.env, KAGEHA_TEST_DECISION_MODEL: model };
const root = fs.mkdtempSync('/tmp/kageha-browser-decision-bench-');
fs.chmodSync(root, 0o700);
const digest = file => crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
const frozenHost = path.join(root, 'host');
const frozenBrowser = path.join(root, 'browser');
for (const [original, copy] of [[host, frozenHost], [browser, frozenBrowser]]) {
  fs.copyFileSync(original, copy);
  fs.chmodSync(copy, 0o700);
}
Object.assign(env, { KAGEHA_LOOP_BIN: frozenHost, KAGEHA_BROWSER_BIN: frozenBrowser });
const report = {
  version: 2, root, pairs, qualification: qualify, scenarios, executor, decision_model: model,
  reasoning_effort: env.KAGEHA_TEST_REASONING || 'provider_default',
  decision_threshold: Number(env.KAGEHA_TEST_DECISION_THRESHOLD || 0.90),
  batch_threshold: Number(env.KAGEHA_TEST_BATCH_THRESHOLD || 0.99),
  confidence_cutoff: env.KAGEHA_TEST_CONFIDENCE_CUTOFF === undefined ? null : Number(env.KAGEHA_TEST_CONFIDENCE_CUTOFF),
  decision_timeout_s: 3, decision_first: typed, typed_decisions: typed, decision_state_budget: typed ? 64 : 3,
  host_sha256: digest(frozenHost), browser_sha256: digest(frozenBrowser),
  fixture_sha256: digest(new URL('./workflow_site.mjs', import.meta.url)),
  harness_sha256: digest(new URL('./workflow_e2e.mjs', import.meta.url)), runs: [],
};
const persist = () => fs.writeFileSync(path.join(root, 'report.json'), JSON.stringify(report, null, 2), { mode: 0o600 });
const median = values => {
  if (!values.length) return null;
  const sorted = values.slice().sort((a, b) => a - b);
  const middle = Math.floor(sorted.length / 2);
  return sorted.length % 2 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2;
};
function loggedJson(file) {
  return fs.readFileSync(file, 'utf8').split('\n').flatMap(line => {
    try { return [JSON.parse(line)]; } catch { return []; }
  });
}
async function run(args, log) {
  const fd = fs.openSync(log, 'w', 0o600);
  const child = spawn(process.execPath, ['crates/kageha-loop/tests/workflow_e2e.mjs', ...args],
    { env, stdio: ['ignore', fd, fd] });
  fs.closeSync(fd);
  return await new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('exit', (code, signal) => resolve({ code, signal }));
  });
}

if (qualify) {
  const log = path.join(root, 'provider-probe.log');
  const exit = await run(['--real', '--real-decisions', '--typed-decisions', '--decisions=active', '--probe-only', '--require-main-credentials'], log);
  const entries = loggedJson(log);
  const probe = entries.find(row => row.provider_probe)?.provider_probe;
  const error = entries.find(row => row.preflight_error);
  report.provider_probe = probe || { ok: false, reason: error?.preflight_error || 'benchmark_preflight_failed', exit };
  if (exit.code !== 0 || probe?.ok !== true) {
    report.qualified = false;
    report.finished_at = new Date().toISOString();
    persist();
    console.log(JSON.stringify({ benchmark_finished: true, root, qualified: false, provider_probe: report.provider_probe }));
    process.exit(1);
  }
}

console.log(JSON.stringify({ benchmark_started: true, root, pairs, executor, decision_model: model, scenarios }));
persist();
for (const scenario of scenarios) {
  for (let pair = 1; pair <= pairs; pair++) {
    for (const mode of pair % 2 ? ['off', 'active'] : ['active', 'off']) {
      const log = path.join(root, `${scenario}-pair-${pair}-${mode}.log`);
      const row = { scenario, pair, mode, started_at: new Date().toISOString(), log };
      report.runs.push(row);
      persist();
      row.exit = await run(['--real', `--decisions=${mode}`,
        ...(scenario === 'workflow' ? [] : [`--${scenario}`]),
        ...(mode === 'active' ? ['--real-decisions'] : []),
        ...(typed ? ['--decision-first', '--typed-decisions'] : []),
      ], log);
      const summary = loggedJson(log).findLast(row => row.passed === true || row.failed === true);
      row.passed = row.exit.code === 0 && summary?.passed === true;
      row.fixture_root = summary?.root;
      if (row.fixture_root) {
        const file = path.join(row.fixture_root, row.passed ? 'result.json' : 'benchmark-failure.json');
        if (fs.existsSync(file)) {
          const result = JSON.parse(fs.readFileSync(file, 'utf8'));
          row.metrics = result.benchmark || result;
        }
      }
      persist();
      console.log(JSON.stringify({ scenario, pair, mode, passed: row.passed,
        elapsed_ms: row.metrics?.elapsed_ms, executor_calls: row.metrics?.executor_calls,
        decision_verified: row.metrics?.decision_verified, fallback_reasons: row.metrics?.fallback_reasons }));
    }
  }
}

function summarize(runs) {
  const successful = runs.filter(run => run.passed);
  const metrics = successful.map(run => run.metrics);
  const calls = runs.flatMap(run => run.metrics?.provider_calls || []);
  return {
    runs: runs.length, successes: successful.length,
    median_elapsed_ms: median(metrics.map(value => value.elapsed_ms)),
    median_executor_calls: median(metrics.map(value => value.executor_calls)),
    decision_calls: metrics.reduce((total, value) => total + (value.decision_calls || 0), 0),
    decision_verified: metrics.reduce((total, value) => total + (value.decision_verified || 0), 0),
    wrong_or_duplicate_actions: metrics.reduce((total, value) => total + (value.wrong_or_duplicate_actions || 0), 0),
    fallback_reasons: Object.fromEntries(Object.entries(Object.groupBy(
      metrics.flatMap(value => value.fallback_reasons || []), reason => reason,
    )).map(([reason, occurrences]) => [reason, occurrences.length])),
    provider_usage: Object.fromEntries(Object.entries(Object.groupBy(calls, call => call.role)).map(([role, entries]) => [role, {
      observed_input_tokens: entries.reduce((total, call) => total + (call.usage?.prompt_tokens ?? call.usage?.input_tokens ?? 0), 0),
      observed_output_tokens: entries.reduce((total, call) => total + (call.usage?.completion_tokens ?? call.usage?.output_tokens ?? 0), 0),
      calls_without_usage: entries.filter(call => !call.usage).length,
    }])),
  };
}
report.summary = Object.fromEntries(scenarios.map(scenario => {
  const runs = report.runs.filter(run => run.scenario === scenario);
  const off = summarize(runs.filter(run => run.mode === 'off'));
  const active = summarize(runs.filter(run => run.mode === 'active'));
  const pair_differences_ms = Array.from({ length: pairs }, (_, index) => {
    const pair = runs.filter(run => run.pair === index + 1);
    if (pair.length !== 2 || pair.some(run => !run.passed)) return null;
    return pair.find(run => run.mode === 'active').metrics.elapsed_ms - pair.find(run => run.mode === 'off').metrics.elapsed_ms;
  });
  const gate = {
    verified_actions: active.decision_verified > 0,
    success_rate: off.successes > 0 && active.successes >= off.successes,
    correct_actions: active.successes === pairs && active.wrong_or_duplicate_actions === 0,
    fewer_main_model_calls: active.median_executor_calls !== null && off.median_executor_calls !== null
      && active.median_executor_calls < off.median_executor_calls,
    faster_median: active.median_elapsed_ms !== null && off.median_elapsed_ms > 0
      && active.median_elapsed_ms <= off.median_elapsed_ms * 0.90,
  };
  return [scenario, { off, active, pair_differences_ms, gate, passed: Object.values(gate).every(Boolean) }];
}));
report.qualified = qualify && Object.values(report.summary).every(scenario => scenario.passed);
report.finished_at = new Date().toISOString();
persist();
console.log(JSON.stringify({ benchmark_finished: true, root, qualified: report.qualified, summary: report.summary }));
if (qualify && !report.qualified) process.exitCode = 1;
