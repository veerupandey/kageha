#!/usr/bin/env node
// Opt-in real-model benchmark. Each trial uses a fresh isolated server and state.
import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { fatalFailure, fixtureHash, observedFastest, runTrialSequence, schedule, summarize, summarizeExperience, summarizeWaitModes, waitComparisonSchedule } from "./agent_speed_benchmark_lib.mjs";

const args = process.argv.slice(2);
const option = (name, fallback) => {
  const index = args.indexOf(name);
  if (index < 0) return fallback;
  if (!args[index + 1] || args[index + 1].startsWith("--")) throw Error(`${name} needs a value`);
  return args[index + 1];
};
if (args.includes("--help")) {
  console.log("Usage: node scripts/demos/agent_speed_benchmark.mjs --output NEW_DIRECTORY [--rounds 3] [--fixture EXISTING_FIXTURE_ROOT] [--binary PATH] [--settings-db PATH] [--model-id ID] [--unlock-vault] [--wait-all | --compare-wait]\nThe default runs 3 × rounds real-model requests; --compare-wait runs 2 × rounds at width four, alternating wait-any and wait-all. Provider charges may apply. --unlock-vault requests an interactive Keychain authorization. KAGEHA_PROVIDER_URL/KAGEHA_MODEL/KAGEHA_API_KEY may configure the provider. Existing output is never overwritten.");
  process.exit(0);
}
const outputArg = option("--output", null);
if (!outputArg) throw Error("--output NEW_DIRECTORY is required");
const rounds = Number(option("--rounds", "3"));
const paired = args.includes("--compare-wait");
if (paired && args.includes("--wait-all")) throw Error("Choose --compare-wait or --wait-all, not both");
const specs = paired ? waitComparisonSchedule(rounds) : schedule(rounds).map((width) => ({width, wait_mode: args.includes("--wait-all") ? "all" : "any"}));
const output = path.resolve(outputArg);
if (fs.existsSync(output)) throw Error("Output already exists; choose a new directory");
const source = option("--fixture", null);
const fixture = path.join(output, "fixture");
fs.mkdirSync(path.join(fixture, "fixtures"), {recursive: true, mode: 0o700});
const tokens = new Set();
for (let i = 1; i <= 4; i++) {
  const relative = `fixtures/task-${i}.txt`;
  // Deterministic built-in content supports comparisons across benchmark runs.
  const content = source ? fs.readFileSync(path.join(path.resolve(source), relative))
    : Buffer.from(`KAGEHA_FIXTURE_${i}_9ac2f0e7b3d1\n`, "utf8");
  const token = content.toString("utf8").trim();
  if (!content.length || content.length > 4096 || token.length < 8 || token.includes("\ufffd") || /[\r\n]/.test(token) ||
    [...tokens].some((previous) => token.includes(previous) || previous.includes(token))) {
    throw Error(`${relative} must contain a distinct single-line UTF-8 token of at least 8 characters and at most 4096 bytes; no token may contain another`);
  }
  tokens.add(token);
  fs.writeFileSync(path.join(fixture, relative), content, {mode: 0o600});
}
const hash = fixtureHash(fixture);
const runner = fileURLToPath(new URL("./agent_repo_review.mjs", import.meta.url));
const passthrough = ["--binary", "--settings-db", "--model-id"].flatMap((key) => {
  const value = option(key, null);
  return value ? [key, path.isAbsolute(value) || key === "--model-id" ? value : path.resolve(value)] : [];
});
if (args.includes("--unlock-vault")) passthrough.push("--unlock-vault");
let model = null;
let providerFingerprint = null;
let activeChild;
let interrupted = false;
function interrupt() {
  if (interrupted) return;
  interrupted = true;
  activeChild?.kill("SIGTERM");
}
process.once("SIGINT", interrupt);
process.once("SIGTERM", interrupt);
const binary = path.resolve(option("--binary", "target/debug/kageha-loop"));
if (!fs.existsSync(binary)) throw Error("Build first: cargo build -p kageha-loop, or provide --binary");
const binaryHash = crypto.createHash("sha256").update(fs.readFileSync(binary)).digest("hex");
const report = {version: 1, started_at: new Date().toISOString(), fixture_sha256: hash, binary_sha256: binaryHash, rounds, wait_mode: paired ? "comparison" : args.includes("--wait-all") ? "all" : "any", trials: [], results: [], observed_fastest: observedFastest([]),
  limitation: "Elapsed time includes the parent and all children from chat submission to response, not server setup. The headless milestones measure server acceptance and event delivery, not the browser's immediate acknowledgment or actual tool start. P50/p95 use only verified runs; model findings are not evaluated. Provider load and caching can vary; compare only matching fixture hashes, models, workload and runtime configuration. Costs are unavailable when not measured."};
const save = () => fs.writeFileSync(path.join(output, "summary.json"), JSON.stringify(report, null, 2) + "\n", {mode: 0o600});
save();
const trials = await runTrialSequence(specs, async ({width, wait_mode}, index) => {
  const trialDir = path.join(output, `trial-${String(index + 1).padStart(3, "0")}-width-${width}${paired ? `-wait-${wait_mode}` : ""}`);
  fs.mkdirSync(trialDir, {mode: 0o700});
  const log = fs.openSync(path.join(trialDir, "trial.log"), "w", 0o600);
  console.log(`Trial ${index + 1}/${specs.length}: width ${width}${paired ? `, wait ${wait_mode}` : ""}`);
  const status = await new Promise((resolve) => {
    const waitFlag = wait_mode === "all" ? ["--wait-all"] : paired ? ["--verify-wait-any"] : [];
    const child = spawn(process.execPath, [runner, "--workspace", fixture, "--output", trialDir, "--benchmark-width", String(width), ...waitFlag, ...passthrough],
      {stdio: ["ignore", log, log], env: process.env});
    activeChild = child;
    if (interrupted) child.kill("SIGTERM");
    child.once("error", (error) => resolve({error: error.message}));
    child.once("exit", (code, signal) => resolve({code, signal}));
  });
  activeChild = undefined;
  fs.closeSync(log);
  let trialReport;
  try { trialReport = JSON.parse(fs.readFileSync(path.join(trialDir, "report.json"), "utf8")); } catch { /* retained failure */ }
  // Logs can contain provider payloads; classify them but never copy raw text
  // into the summary or console output.
  const logText = fs.readFileSync(path.join(trialDir, "trial.log"), "utf8");
  const fatal = fatalFailure(trialReport, logText);
  const currentHash = fixtureHash(fixture);
  const sameModel = !model || !trialReport?.provider_model || model === trialReport.provider_model;
  const sameProvider = !providerFingerprint || !trialReport?.provider_fingerprint || providerFingerprint === trialReport.provider_fingerprint;
  if (!model && trialReport?.provider_model) model = trialReport.provider_model;
  if (!providerFingerprint && trialReport?.provider_fingerprint) providerFingerprint = trialReport.provider_fingerprint;
  const verified = status.code === 0 && trialReport?.validation?.passed === true &&
    trialReport.benchmark_width === width && trialReport.peak_concurrent_workers === width &&
    (!(paired || wait_mode === "all") || trialReport.wait_mode === wait_mode) &&
    Number.isFinite(trialReport.task_elapsed_ms) && currentHash === hash && sameModel && sameProvider && Boolean(trialReport?.provider_fingerprint);
  const stopReason = interrupted ? "Benchmark interrupted." : fatal || (currentHash !== hash || !sameModel || !sameProvider
    ? "Fixture, model or provider changed; stopped to avoid misleading comparisons." : null);
  return {index: index + 1, width, wait_mode, verified, elapsed_ms: trialReport?.task_elapsed_ms ?? null,
    cost_usd: trialReport?.cost_usd ?? null, model: trialReport?.provider_model ?? null,
    timing_diagnostics: trialReport?.timing_diagnostics ?? null,
    experience_timings: trialReport?.experience_timings ?? null,
    fixture_sha256: currentHash, exit_code: status.code ?? null, error: fatal || trialReport?.error || status.error || (status.signal ? `signal ${status.signal}` : status.code !== 0 ? `runner exited ${status.code}` : null),
    stop_reason: stopReason, report: path.relative(output, path.join(trialDir, "report.json"))};
}, (completed, trial) => {
  report.trials = completed;
  report.model = model;
  report.provider_fingerprint = providerFingerprint;
  report.results = paired ? summarizeWaitModes(completed) : summarize(completed);
  report.experience_results = paired ? ["any", "all"].map((wait_mode) => ({wait_mode,
    ...summarizeExperience(completed.filter((row) => row.wait_mode === wait_mode)).find((row) => row.width === 4)})) : summarizeExperience(completed);
  report.observed_fastest = paired ? {available: false, reason: "Wait-mode comparison is descriptive; review each mode's verified and failed trials."} : observedFastest(report.results);
  if (trial.stop_reason) report.stop_reason = trial.stop_reason;
  save();
  if (trial.stop_reason) console.error(trial.stop_reason);
});
console.log(JSON.stringify({fixture_sha256: hash, model, results: report.results, experience_results: report.experience_results, observed_fastest: report.observed_fastest, stop_reason: report.stop_reason ?? null, summary: path.join(output, "summary.json")}, null, 2));
if (trials.length !== specs.length || trials.some((trial) => !trial.verified)) process.exitCode = 1;
