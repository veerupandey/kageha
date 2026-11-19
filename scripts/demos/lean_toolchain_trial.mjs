#!/usr/bin/env node
// Opt-in live trial: two workers inventory locally installed rustc target names.
// Only target names and task-relative paths are sent to the model. Example:
// KAGEHA_TRIAL_SPACING_MS=7000 node scripts/demos/lean_toolchain_trial.mjs
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import crypto from "node:crypto";
import { execFileSync } from "node:child_process";
import { startLiveTrial } from "./lean_live_trial_host.mjs";

const args = process.argv.slice(2);
function option(name, fallback) {
  const at = args.indexOf(name);
  if (at < 0) return fallback;
  if (!args[at + 1] || args[at + 1].startsWith("--")) throw Error(`${name} needs a value`);
  return args[at + 1];
}
const binary = path.resolve(option("--binary", "target/debug/kageha-loop"));
const output = path.resolve(option("--output", "docs/audits/lean-toolchain-live-2026-11-15.json"));
const maxCalls = Number(option("--max-calls", "15"));
const maxSeconds = Number(option("--seconds", "180"));
if (!Number.isInteger(maxCalls) || maxCalls < 1 || maxCalls > 100 || !Number.isInteger(maxSeconds) || maxSeconds < 1 || maxSeconds > 600) {
  throw Error("Use bounded positive --max-calls (at most 100) and --seconds (at most 600)");
}
if (fs.existsSync(output)) throw Error("Choose a new --output path; existing reports are preserved");
const rustc = process.env.KAGEHA_TRIAL_RUSTC || path.join(os.homedir(), ".cargo/bin/rustc");
const allTargets = execFileSync(rustc, ["--print", "target-list"], { encoding: "utf8", timeout: 30000 }).trim().split("\n");
const targets = Object.fromEntries(["ios", "tvos"].map((platform) => [platform,
  allTargets.filter((target) => target.includes(`-apple-${platform}`)).sort()]));
if (!targets.ios.length || !targets.tvos.length) throw Error("rustc returned no iOS or tvOS targets");
const input = Object.fromEntries(Object.entries(targets).map(([name, lines]) => [name, lines.join("\n") + "\n"]));
const sha256 = (value) => crypto.createHash("sha256").update(value).digest("hex");
const binaryHash = sha256(fs.readFileSync(binary));
const report = {
  version: 1,
  task: "Inventory actual installed rustc iOS and tvOS target triples into an SDK matrix",
  source: "Fixture data came from selected target names in rustc --print target-list. Standard tool responses may include the temporary sandbox root.",
  target_count: { ios: targets.ios.length, tvos: targets.tvos.length },
  target_set_sha256: Object.fromEntries(Object.entries(input).map(([name, value]) => [name, sha256(value)])),
  max_model_calls: maxCalls,
  max_duration_seconds: maxSeconds,
  request_spacing_ms: Number(process.env.KAGEHA_TRIAL_SPACING_MS || 0),
  tool_restrictions: "Named coordinator can spawn, wait/status, read, and write, with shell denied. Requested reader profiles permit read_file only; actual chosen personas are reported separately.",
  overlap_basis: "Intersection of persisted ui_job_times started/ended timestamps; missing timestamps are unproven.",
  limitation: "Worker lifetimes can overlap without simultaneous tool execution. Names are diagnostic; correctness is graded from each worker's observed input and durable result. Shared input paths are not a confidentiality boundary.",
  status: "running"
};
function save() {
  fs.mkdirSync(path.dirname(output), { recursive: true });
  fs.writeFileSync(output, JSON.stringify(report, null, 2) + "\n", { mode: 0o600 });
}
function equalLists(actual, expected) {
  if (!Array.isArray(actual) || actual.some((item) => typeof item !== "string")) return false;
  const sorted = [...actual].sort();
  return sorted.length === expected.length && sorted.every((item, index) => item === expected[index]);
}
function triples(text) {
  return [...new Set(String(text || "").match(/\b[a-z0-9_]+-apple-[a-z0-9_-]+\b/g) || [])].sort();
}
function readJson(file) {
  try { return JSON.parse(fs.readFileSync(file, "utf8")); } catch { return null; }
}
function proposedSpawns(calls) {
  return calls.flatMap((call) => call.tool_calls || []).filter((call) => call.name === "agent_spawn").map((call) => {
    let input = {};
    try { input = JSON.parse(call.arguments); } catch {}
    const safe = (value) => typeof value === "string" && /^[a-z0-9_ -]{1,80}$/i.test(value) ? value : null;
    return { tool_call_id: call.id, requested_agent_id: safe(input.agent_id), requested_title: safe(input.title) };
  });
}
const prompt = `Create an SDK target matrix. Start ios_reader and tvos_reader concurrently with agent_spawn. ios_reader should read parent/inputs/ios.txt; tvos_reader should read parent/inputs/tvos.txt. Wait for both workers and use their returned target lists to write outputs/sdk_matrix.json with the exact shape {"ios":["triple",...],"tvos":["triple",...]}. Include every target exactly once. Finish after the file is written.`;
let host;
let activeSession;
let temporaryWorkspacePathObserved = false;
let agentSpawnSchemaSeen = false;
let agentIdRequired = false;
const begin = Date.now();
try {
  host = await startLiveTrial({ binary, model: "gemini-flash-lite-latest", maxCalls, seconds: maxSeconds,
    onRequest(input) {
      if (!host) return;
      for (const tool of input.tools || []) {
        if (tool.function?.name === "agent_spawn") {
          agentSpawnSchemaSeen = true;
          agentIdRequired ||= (tool.function.parameters?.required || []).includes("agent_id");
        }
      }
      const temporaryRoot = path.dirname(host.state);
      const serialized = JSON.stringify(input.messages || []);
      if (serialized.includes(temporaryRoot) || serialized.includes(fs.realpathSync(temporaryRoot))) temporaryWorkspacePathObserved = true;
    }
  });
  report.binary_sha256 = host.hash;
  report.binary_hash_matches_requested = host.hash === binaryHash;
  report.model = host.model;
  const { session, workspace } = await host.session();
  activeSession = session;
  const fixtures = path.join(workspace, "inputs");
  fs.mkdirSync(fixtures, { recursive: true, mode: 0o700 });
  for (const [platform, text] of Object.entries(input)) fs.writeFileSync(path.join(fixtures, `${platform}.txt`), text, { mode: 0o600 });
  for (const platform of ["ios", "tvos"]) {
    await host.api("/api/agents", {
      id: `${platform}_reader`, name: `${platform} target reader`,
      system_prompt: `Use read_file to read parent/inputs/${platform}.txt. Return the exact target names, one per line.`,
      tool_policy: { allow: ["read_file"], deny: ["shell", "agent_spawn"] }
    });
  }
  await host.api("/api/agents", {
    id: "sdk_coordinator", name: "SDK matrix coordinator",
    system_prompt: "Delegate the two inventories concurrently, wait for both, and write the requested matrix from their results.",
    tool_policy: { allow: ["agent_spawn", "agent_wait_any", "agent_wait_all", "agent_status", "read_file", "write_file"], deny: ["shell"] }
  });
  const submitted = Date.now();
  const answer = await host.send(session, prompt, "sdk_coordinator");
  report.elapsed_ms = Date.now() - submitted;
  report.answer_status = answer.status;
  report.answer_failure_code = answer.failure_code || null;
  const jobs = (await host.api(`/api/jobs?session_id=${encodeURIComponent(session)}`)).jobs || [];
  const db = host.db();
  let times, cards;
  try {
    times = db.prepare("SELECT job_id,started,ended FROM ui_job_times").all();
    const coordinator = db.prepare("SELECT json_extract(spec,'$.persona.id') AS id FROM ui_run_specs WHERE chat_id=? AND request_id!='job' ORDER BY rowid DESC LIMIT 1").get(session);
    report.coordinator_persona_id = coordinator?.id || null;
    cards = db.prepare("SELECT sequence,chat_id,payload FROM ui_events WHERE event='tool_card' ORDER BY sequence").all().map((row) => {
      try { return { sequence: row.sequence, chat_id: row.chat_id, ...JSON.parse(row.payload) }; } catch { return {}; }
    });
  } finally { db.close(); }
  report.workers = jobs.map((job) => {
    const record = times.find((row) => row.job_id === job.id);
    return { id: job.id, state: job.state, persona_id: job.spec?.persona_id || null,
      started_at_ms: Number.isFinite(record?.started) ? Math.round(record.started * 1000) : null,
      ended_at_ms: Number.isFinite(record?.ended) ? Math.round(record.ended * 1000) : null };
  });
  report.proposed_spawns = proposedSpawns(host.calls);
  const intervals = report.workers.filter((worker) => worker.started_at_ms !== null && worker.ended_at_ms !== null);
  const intersection = intervals.length === 2 ? Math.min(...intervals.map((worker) => worker.ended_at_ms)) - Math.max(...intervals.map((worker) => worker.started_at_ms)) : null;
  report.worker_overlap_ms = intersection === null ? null : Math.max(0, intersection);
  report.worker_overlap_proven = intersection !== null && intersection > 0 && jobs.length === 2;
  const checks = {};
  const matchedIds = [];
  for (const platform of ["ios", "tvos"]) {
    const matching = jobs.filter((job) => cards.some((card) => card.chat_id === job.session_id && card.name === "read_file"
      && card.status === "done" && String(card.result_preview || "").includes(input[platform].trim())));
    const job = matching.length === 1 ? matching[0] : null;
    if (job) matchedIds.push(job.id);
    checks[platform] = { unique_worker_read_own_input: matching.length === 1,
      worker_succeeded: job?.state === "succeeded", durable_result_exact: job ? equalLists(triples(job.result?.message), targets[platform]) : false };
  }
  report.worker_source_status = jobs.map((job) => ({
    id: job.id, persona_id: job.spec?.persona_id || null,
    read_file_done: cards.filter((card) => card.chat_id === job.session_id && card.name === "read_file" && card.status === "done").length,
    read_file_failed: cards.filter((card) => card.chat_id === job.session_id && card.name === "read_file" && card.status === "failed").length
  }));
  const waits = cards.filter((card) => card.chat_id === session && ["agent_wait_any", "agent_wait_all", "agent_status"].includes(card.name) && card.status === "done");
  const write = cards.find((card) => card.chat_id === session && card.name === "write_file" && card.status === "done" && String(card.result_preview || "").includes("sdk_matrix.json"));
  checks.distinct_workers = matchedIds.length === 2 && new Set(matchedIds).size === 2;
  checks.both_results_collected_before_write = !!write && matchedIds.length === 2 && matchedIds.every((id) => waits.some((card) => card.sequence < write.sequence && String(card.result_preview || "").includes(id)));
  const matrix = readJson(path.join(workspace, "outputs", "sdk_matrix.json"));
  checks.matrix_file_exists = !!matrix;
  checks.matrix_ios_exact = equalLists(matrix?.ios, targets.ios);
  checks.matrix_tvos_exact = equalLists(matrix?.tvos, targets.tvos);
  checks.coordinator_no_shell_dispatch = !cards.some((card) => card.chat_id === session && card.name === "shell");
  report.checks = checks;
  report.model_calls = host.calls.length;
  report.model_call_breakdown = { executor: host.calls.filter((call) => call.role === "executor").length,
    reflection: host.calls.filter((call) => call.role === "reflection").length, incomplete: host.calls.filter((call) => call.end === null).length };
  report.provider_http_429 = host.calls.filter((call) => call.status === 429).length;
  report.binary_hash_after_task = sha256(fs.readFileSync(binary));
  report.binary_hash_unchanged = report.binary_hash_after_task === binaryHash;
  const platformChecks = ["ios", "tvos"].every((name) => Object.values(checks[name]).every(Boolean));
  const completed = ["success", "completed"].includes(answer.status);
  report.verified = completed && jobs.length === 2 && report.worker_overlap_proven && report.binary_hash_matches_requested
    && report.binary_hash_unchanged && platformChecks && checks.distinct_workers && checks.both_results_collected_before_write
    && checks.matrix_ios_exact && checks.matrix_tvos_exact && checks.coordinator_no_shell_dispatch;
  report.unverified_success_status = completed && !report.verified;
  report.status = report.verified ? "verified" : "unverified";
} catch (error) {
  report.status = "infrastructure_or_execution_error";
  report.error_category = /429|throttl|rate.limit/i.test(String(error)) ? "provider_throttled" : "trial_failed";
  if (host) {
    report.model_calls = host.calls.length;
    report.provider_http_429 = host.calls.filter((call) => call.status === 429).length;
    report.proposed_spawns = proposedSpawns(host.calls);
    if (activeSession) {
      try {
        const jobs = (await host.api(`/api/jobs?session_id=${encodeURIComponent(activeSession)}`)).jobs || [];
        const db = host.db();
        try {
          const count = db.prepare("SELECT json_extract(payload,'$.status') AS status, COUNT(*) AS n FROM ui_events WHERE chat_id=? AND event='tool_card' AND json_extract(payload,'$.name')='read_file' GROUP BY status");
          report.worker_source_status = jobs.map((job) => {
            const counts = Object.fromEntries(count.all(job.session_id).map((row) => [row.status, row.n]));
            return { id: job.id, persona_id: job.spec?.persona_id || null, state: job.state,
              read_file_done: counts.done || 0, read_file_failed: counts.failed || 0 };
          });
        } finally { db.close(); }
      } catch { report.worker_source_status_unavailable = true; }
    }
  }
} finally {
  report.total_elapsed_ms = Date.now() - begin;
  report.temporary_workspace_path_observed_in_model_context = temporaryWorkspacePathObserved;
  report.agent_spawn_schema_seen = agentSpawnSchemaSeen;
  report.agent_spawn_schema_agent_id_required = agentIdRequired;
  if (host) try { await host.close(); } catch { report.cleanup_error = true; }
  save();
}
console.log(JSON.stringify({ status: report.status, verified: report.verified || false,
  overlap_proven: report.worker_overlap_proven || false, model_calls: report.model_calls ?? null, report: output }));
if (!report.verified) process.exitCode = 1;
