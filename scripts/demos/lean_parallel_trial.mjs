#!/usr/bin/env node
// Opt-in live, synthetic-only two-worker business trial. The helper launches a
// private isolated server and a loopback provider adapter. The benchmark uses
// actual job lifecycle times and checks produced files independently.
import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { startLiveTrial } from "./lean_live_trial_host.mjs";

const args = process.argv.slice(2);
function option(name, fallback) {
  const i = args.indexOf(name);
  if (i < 0) return fallback;
  if (!args[i + 1] || args[i + 1].startsWith("--")) throw Error(`${name} needs a value`);
  return args[i + 1];
}
const binary = path.resolve(option("--binary", "target/debug/kageha-loop"));
const model = option("--model", undefined);
const light = args.includes("--light");
const output = path.resolve(
  option(
    "--output",
    light
      ? "output/benchmarks/lean-parallel-light-live-2026-11-15.json"
      : "output/benchmarks/lean-parallel-live-2026-11-15.json",
  ),
);
if (fs.existsSync(output)) throw Error("Output already exists; choose a new --output path");

// These are synthetic business figures, not customer data. Workers receive
// separate input filenames and write into separate nested workspaces. Shared
// workers can technically read other parent files, so this is not a test of
// access-control confidentiality.
const input = {
  north: "item,quantity,unit_price\nA,2,11\nB,3,17\nC,4,7\n",
  south: "item,quantity,unit_price\nD,5,13\nE,2,19\nF,3,9\n",
};
function calculate(csv) {
  return csv
    .trim()
    .split("\n")
    .slice(1)
    .reduce(
      (acc, line) => {
        const [, quantity, price] = line.split(",");
        acc.units += Number(quantity);
        acc.revenue += Number(quantity) * Number(price);
        return acc;
      },
      { units: 0, revenue: 0 },
    );
}
const expected = { north: calculate(input.north), south: calculate(input.south) };
expected.combined_revenue = expected.north.revenue + expected.south.revenue;
expected.leader = expected.north.revenue > expected.south.revenue ? "north" : "south";
const digest = (value) => crypto.createHash("sha256").update(value).digest("hex");
const expectedBinaryHash = digest(fs.readFileSync(binary));
const report = {
  version: 1,
  task: "Two independent synthetic sales analyses and a checked management summary",
  expected,
  fixture_sha256: Object.fromEntries(Object.entries(input).map(([name, value]) => [name, digest(value)])),
  fixture_is_synthetic: true,
  methodology: light
    ? "Fresh isolated state; task requests two read-only workers with separate synthetic inputs and a parent-written JSON artifact. Shared inputs are not an access-control confidentiality boundary. Observed tool and file evidence cannot establish the model's internal reasoning."
    : "Fresh isolated state; task requests two shared workers with distinct input paths and nested outputs, plus parent source reads and integration of both worker summaries. Shared input paths are not an access-control confidentiality boundary. Evidence of reads does not establish the order of those reads or the model's internal reasoning.",
  overlap_basis:
    "Durable worker started_at/ended_at interval intersection. Missing or incomplete timestamps are unproven, never assumed concurrent.",
  actual_tool_start_timing: "Unavailable: tool cards record admission, not scheduler start.",
  status: "running",
};
function save() {
  fs.mkdirSync(path.dirname(output), { recursive: true });
  fs.writeFileSync(output, JSON.stringify(report, null, 2) + "\n", { mode: 0o600 });
}
function readJson(file) {
  try {
    return JSON.parse(fs.readFileSync(file, "utf8"));
  } catch {
    return null;
  }
}
function numericSummary(value, expectedValue) {
  return value && value.units === expectedValue.units && value.revenue === expectedValue.revenue;
}
const fullPrompt = `Prepare a management summary from two separate synthetic sales datasets. Use exactly two independent workers concurrently, one per dataset. Launch two agent_spawn calls together with agent_id main and isolation shared.
Worker North: read parent/inputs/north.csv, calculate total quantity as units and sum(quantity * unit_price) as revenue, then use write_file to write its own outputs/summary.json containing only {"units":integer,"revenue":integer}. Return the file path and numbers.
Worker South: do the same using parent/inputs/south.csv and its own outputs/summary.json. Each worker must use its own workspace; do not let either edit the other's files.
While they run, independently read inputs/north.csv and inputs/south.csv yourself, calculate the figures, and check them against each worker's result. Wait for both, then read agents/<job_id>/outputs/summary.json for each worker. Integrate the checked figures into your own outputs/management.json with exact shape {"north":{"units":integer,"revenue":integer},"south":{"units":integer,"revenue":integer},"combined_revenue":integer,"leader":"north or south"}. The leader has the greater revenue. Report the results and the final file path. Use read_file and write_file; do not use shell or external services. Do not claim completion if any required result is missing.`;
const lightPrompt = `Create a management summary for two sales datasets. Start two workers in parallel with agent_spawn: north_worker must analyze parent/inputs/north.csv, and south_worker must analyze parent/inputs/south.csv. Each returns a JSON object with integer units and revenue. Wait for both results, then write outputs/management.json with {"north":{"units":integer,"revenue":integer},"south":{"units":integer,"revenue":integer},"combined_revenue":integer,"leader":"north or south"}. Revenue is the sum of quantity times unit_price; the leader has higher revenue. Report the summary only after both workers finish.`;
let host;
const started = Date.now();
try {
  host = await startLiveTrial({ binary, model, maxCalls: 32, seconds: 240 });
  report.binary_sha256 = host.hash;
  report.binary_hash_matches_requested = host.hash === expectedBinaryHash;
  report.model = host.model;
  report.provider = host.provider;
  report.minimum_interval_ms = Number(process.env.KAGEHA_TRIAL_SPACING_MS || 0);
  const { session, workspace } = await host.session();
  if (light) {
    for (const name of ["north", "south"]) {
      await host.api("/api/agents", {
        id: `${name}_worker`,
        name: `${name} sales analyst`,
        system_prompt: `Read parent/inputs/${name}.csv with read_file. Return only JSON {"units":integer,"revenue":integer}. Units are the sum of quantities; revenue is the sum of quantity times unit_price.`,
        tool_policy: { allow: ["read_file"], deny: ["shell", "agent_spawn"] },
      });
    }
  }
  const fixtures = path.join(workspace, "inputs");
  fs.mkdirSync(fixtures, { recursive: true, mode: 0o700 });
  for (const [name, text] of Object.entries(input))
    fs.writeFileSync(path.join(fixtures, `${name}.csv`), text, { mode: 0o600 });
  const submitted = Date.now();
  const answer = await host.send(session, light ? lightPrompt : fullPrompt);
  report.task_elapsed_ms = Date.now() - submitted;
  report.answer_status = answer.status;
  report.answer_failure_code = answer.failure_code || null;
  const jobs = (await host.api(`/api/jobs?session_id=${encodeURIComponent(session)}`)).jobs || [];
  const db = host.db();
  let cards, times;
  try {
    times = db.prepare("SELECT job_id,started,ended FROM ui_job_times").all();
    cards = db
      .prepare("SELECT sequence,chat_id,payload FROM ui_events WHERE event='tool_card' ORDER BY sequence")
      .all()
      .map((row) => {
        try {
          return { sequence: row.sequence, chat_id: row.chat_id, ...JSON.parse(row.payload) };
        } catch {
          return {};
        }
      });
  } finally {
    db.close();
  }
  report.workers = jobs.map((job) => ({
    id: job.id,
    state: job.state,
    persona_id: job.spec?.persona_id || null,
    started_at_ms: Number.isFinite(times.find((row) => row.job_id === job.id)?.started)
      ? Math.round(times.find((row) => row.job_id === job.id).started * 1000)
      : null,
    ended_at_ms: Number.isFinite(times.find((row) => row.job_id === job.id)?.ended)
      ? Math.round(times.find((row) => row.job_id === job.id).ended * 1000)
      : null,
    objective_category: /north/i.test(job.objective || "")
      ? "north"
      : /south/i.test(job.objective || "")
        ? "south"
        : "unknown",
  }));
  const matching = (name) =>
    jobs.filter((job) =>
      light ? job.spec?.persona_id === `${name}_worker` : new RegExp(name, "i").test(job.objective || ""),
    );
  const workerFiles = {};
  for (const name of ["north", "south"]) {
    const [job] = matching(name);
    const file = job ? path.join(workspace, "agents", job.id, "outputs", "summary.json") : "";
    const value = file ? readJson(file) : null;
    workerFiles[name] = {
      unique_job: matching(name).length === 1,
      succeeded: job?.state === "succeeded",
      exists: !!value,
      correct: numericSummary(value, expected[name]),
      sha256: value ? digest(fs.readFileSync(file)) : null,
    };
  }
  const result = readJson(path.join(workspace, "outputs", "management.json"));
  const resultCorrect =
    !!result &&
    numericSummary(result.north, expected.north) &&
    numericSummary(result.south, expected.south) &&
    Number(result.combined_revenue) === expected.combined_revenue &&
    result.leader === expected.leader;
  const reads = cards.filter(
    (card) => card.chat_id === session && card.name === "read_file" && card.status === "done",
  );
  const sourceReads = Object.fromEntries(
    Object.entries(input).map(([name, text]) => [
      name,
      reads.some((card) => String(card.result_preview || "").includes(text.trim())),
    ]),
  );
  const workerReads = Object.fromEntries(
    ["north", "south"].map((name) => [
      name,
      reads.some((card) => {
        try {
          return numericSummary(JSON.parse(String(card.result_preview)), expected[name]);
        } catch {
          return false;
        }
      }),
    ]),
  );
  const intervals = report.workers.filter(
    (worker) => worker.started_at_ms !== null && worker.ended_at_ms !== null,
  );
  const overlap =
    intervals.length === 2
      ? Math.min(...intervals.map((w) => w.ended_at_ms)) - Math.max(...intervals.map((w) => w.started_at_ms))
      : null;
  report.overlap_ms = overlap === null ? null : Math.max(0, overlap);
  report.overlap_proven = overlap !== null && overlap > 0 && jobs.length === 2;
  report.checks = {
    exactly_two_workers: jobs.length === 2,
    worker_files: workerFiles,
    parent_source_reads: sourceReads,
    parent_worker_output_reads: workerReads,
    parent_file_exists: !!result,
    parent_file_correct: resultCorrect,
  };
  report.model_calls = host.calls.length;
  report.model_call_breakdown = {
    executor: host.calls.filter((call) => call.role === "executor").length,
    reflection: host.calls.filter((call) => call.role === "reflection").length,
    incomplete: host.calls.filter((call) => call.end === null).length,
  };
  report.proposed_tool_calls = {};
  for (const call of host.calls)
    for (const tool of call.tool_calls) {
      report.proposed_tool_calls[tool.name] = (report.proposed_tool_calls[tool.name] || 0) + 1;
    }
  report.proposed_tool_calls_note = "Model-proposed calls, not proof of tool dispatch.";
  report.provider_http_429 = host.calls.filter((call) => call.status === 429).length;
  report.binary_hash_after_task = digest(fs.readFileSync(binary));
  report.binary_hash_unchanged = report.binary_hash_after_task === expectedBinaryHash;
  let evidenceSatisfied;
  if (light) {
    const workerResults = {};
    for (const name of ["north", "south"]) {
      const [job] = matching(name);
      const text = String(job?.result?.message || "");
      let value = null;
      try {
        value = JSON.parse(text);
      } catch {
        try {
          value = JSON.parse(text.slice(text.indexOf("{"), text.lastIndexOf("}") + 1));
        } catch {}
      }
      workerResults[name] = {
        unique_job: matching(name).length === 1,
        succeeded: job?.state === "succeeded",
        correct: !!numericSummary(value, expected[name]),
        own_source_read: cards.some(
          (card) =>
            card.chat_id === job?.session_id &&
            card.name === "read_file" &&
            card.status === "done" &&
            String(card.result_preview || "").includes(input[name].trim()),
        ),
      };
    }
    const waits = cards.filter(
      (card) =>
        card.chat_id === session &&
        ["agent_wait_any", "agent_wait_all", "agent_status"].includes(card.name) &&
        card.status === "done",
    );
    const outputWrite = cards.find(
      (card) =>
        card.chat_id === session &&
        card.name === "write_file" &&
        card.status === "done" &&
        String(card.result_preview || "").includes("management.json"),
    );
    const workerResultsCollectedBeforeWrite =
      jobs.length === 2 &&
      !!outputWrite &&
      jobs.every((job) =>
        waits.some(
          (card) =>
            card.sequence < outputWrite.sequence && String(card.result_preview || "").includes(job.id),
        ),
      );
    report.checks = {
      exactly_two_workers: jobs.length === 2,
      worker_results: workerResults,
      worker_results_collected_before_write: workerResultsCollectedBeforeWrite,
      parent_file_exists: !!result,
      parent_file_correct: resultCorrect,
    };
    evidenceSatisfied =
      Object.values(workerResults).every(
        (value) => value.unique_job && value.succeeded && value.correct && value.own_source_read,
      ) && workerResultsCollectedBeforeWrite;
  } else {
    evidenceSatisfied =
      Object.values(workerFiles).every((value) => value.unique_job && value.succeeded && value.correct) &&
      Object.values(sourceReads).every(Boolean) &&
      Object.values(workerReads).every(Boolean);
  }
  const taskSatisfied =
    ["success", "completed"].includes(answer.status) &&
    jobs.length === 2 &&
    evidenceSatisfied &&
    resultCorrect;
  const verified =
    taskSatisfied &&
    report.overlap_proven &&
    report.binary_hash_matches_requested &&
    report.binary_hash_unchanged;
  report.verified = verified;
  report.unverified_success_status = ["success", "completed"].includes(answer.status) && !verified;
  const answerText = String(answer.message || "");
  report.false_completion =
    !taskSatisfied &&
    /\b(done|completed|successfully)\b/i.test(answerText) &&
    !/\b(incomplete|unable|could not|cannot|failed)\b/i.test(answerText);
  report.status = verified ? "verified" : "unverified";
} catch (error) {
  report.status = "infrastructure_or_execution_error";
  // Record only a small category, never raw provider bodies or prompt content.
  report.error_category = /429|throttl|rate.limit/i.test(String(error))
    ? "provider_throttled"
    : "trial_failed";
  if (host) report.model_calls = host.calls.length;
} finally {
  report.total_elapsed_ms = Date.now() - started;
  if (host) {
    try {
      await host.close();
    } catch {
      report.cleanup_error = true;
    }
  }
  save();
}
console.log(
  JSON.stringify({
    status: report.status,
    verified: report.verified || false,
    overlap_proven: report.overlap_proven || false,
    model_calls: report.model_calls ?? null,
    report: output,
  }),
);
if (!report.verified) process.exitCode = 1;
