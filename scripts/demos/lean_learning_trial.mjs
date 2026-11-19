// node scripts/demos/lean_learning_trial.mjs NEW_OUTPUT.json [number-of-pairs] [files|cli|contract]
// One independent training chat, then fresh servers restored from its learning-only snapshot.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import crypto from "node:crypto";
import { DatabaseSync } from "node:sqlite";
import { setTimeout as pause } from "node:timers/promises";
import { startLiveTrial } from "./lean_live_trial_host.mjs";
const [, , output, count = "2", scenario = "files"] = process.argv;
if (!["files", "cli", "contract"].includes(scenario)) throw Error("Unknown task variant");
const cli = scenario !== "files";
const contract = scenario === "contract";
if (!output || fs.existsSync(output) || ![1, 2, 3, 4].includes(Number(count)))
  throw Error("Supply a new output JSON path and optionally 1–4 pairs");
const work = fs.mkdtempSync(path.join(os.tmpdir(), "lean-settlement-"));
const report = {
  provider: process.env.KAGEHA_TRIAL_PROVIDER || "gemini",
  model: process.env.KAGEHA_TRIAL_MODEL || "gemini-flash-lite-latest",
  scenario,
  minimum_interval_ms: Number(process.env.KAGEHA_TRIAL_SPACING_MS || 0),
  task: "Weekly carrier settlement reconciliation",
  pairs: [],
  method: {
    training:
      "Outdated intake hint. Agent asked to retain a reusable correction if discovered. No technique or lesson wording supplied.",
    intervention:
      "Clone training learning.sqlite into two new servers per pair. Mark only the active training lessons retired in one clone. Keep all other learning history intact.",
    outcome:
      "Exactly match five independently computed fields in settlement.json, including the active source path. Count failed reads and foreground provider calls separately.",
    firstSource: "First read_file request targeting an input file. Batched reads are reported together.",
    fixture:
      "Constructed business data with paid, reversed and void entries, a stale ticket and two archived exports. Same bytes and prompt within each pair; source filename and amounts change between weeks.",
  },
};
const save = () => fs.writeFileSync(output, JSON.stringify(report, null, 2) + "\n", { mode: 0o600 });
function fixture(week) {
  const file = "drops/" + crypto.randomBytes(6).toString("hex") + ".csv";
  const a = crypto.randomInt(3000, 9000),
    b = crypto.randomInt(2000, 6000),
    r = crypto.randomInt(200, 1800);
  return {
    week,
    file,
    csv: "reference,status,cents\nA,paid," + a + "\nB,reversed," + r + "\nC,void,90000\nD,paid," + b + "\n",
    expected: { week, paid_count: 2, reversed_count: 1, net_cents: a + b - r, source: file },
  };
}
function populate(workspace, f) {
  for (const d of ["drops", "registry"]) fs.mkdirSync(path.join(workspace, d), { recursive: true });
  fs.writeFileSync(
    path.join(workspace, "registry", "active-source.json"),
    JSON.stringify({ week: f.week, current: f.file }),
  );
  fs.writeFileSync(path.join(workspace, f.file), f.csv);
  for (const name of ["archive-a", "archive-b"])
    fs.writeFileSync(path.join(workspace, "drops", name + ".csv"), "reference,status,cents\nLEGACY,paid,1\n");
}
function makeCli(workspace) {
  // Synthetic versioned tool. Each real invocation leaves an ordered trace. It
  // reads the held-out week's ledger; the agent never sees expected totals.
  fs.writeFileSync(
    path.join(workspace, "carrier-settle"),
    `#!/bin/sh
set -eu
case "$*" in
  --help|help) result=help ;;
  "${contract ? "close --basis posted --calendar freight" : "close --basis posted"}") result=success ;;
  ${contract ? '"close --basis posted") result=wrong_calendar ;;' : ""}
  *) result=rejected ;;
esac
printf '%s\\n' "$result" >> carrier-invocations.log
case "$result" in
  rejected) echo "Usage changed. Run sh carrier-settle --help for the current report command." >&2; exit 64 ;;
  wrong_calendar) echo "Export rejected: this carrier uses the freight calendar. Retry with --calendar freight." >&2; exit 65 ;;
  help) echo "To produce settlement.json: sh carrier-settle close --basis posted${contract ? " [--calendar NAME]" : ""}"; exit 0 ;;
esac
file=$(cat registry/source-path)
week=$(cat registry/week)
awk -F, -v week="$week" -v file="$file" 'BEGIN{n=0;r=0;net=0} $2=="paid"{n++;net+=$3} $2=="reversed"{r++;net-=$3} END{printf "{\\"week\\":\\"%s\\",\\"paid_count\\":%d,\\"reversed_count\\":%d,\\"net_cents\\":%d,\\"source\\":\\"%s\\"}\\n",week,n,r,net,file}' "$file" > settlement.json
cat settlement.json
`,
    { mode: 0o700 },
  );
}
function prompt(f, training) {
  if (contract)
    return (
      "Produce the weekly carrier settlement for " +
      f.week +
      ' using the existing carrier-settle utility in this workspace. Operations lists "sh carrier-settle close --basis posted" as the standard invocation. Deliver its settlement.json and report net cents. Use the installed utility for the calculation.'
    );
  if (cli)
    return (
      "Prepare the weekly carrier settlement for " +
      f.week +
      ' in this workspace. Produce settlement.json with our carrier-settle utility and tell me the net cents. The handover ticket quotes the usual invocation as "sh carrier-settle export". Use the locally installed utility as the authority; do not reimplement it.' +
      (training
        ? " This is a support investigation: reproduce the ticket command first, then recover and deliver the report."
        : "")
    );
  return (
    "Reconcile the weekly carrier settlement for " +
    f.week +
    ". The intake ticket lists drops/current.csv as the usual source, but the ticket may be out of date. Use read_file for input records so the source is traceable. Use the authoritative current export. Count paid and reversed rows, subtract reversed cents from paid cents and exclude void rows. Write settlement.json with week, paid_count, reversed_count, net_cents, and source (the input CSV path relative to the workspace). Confirm the report is ready." +
    (training
      ? " Register the report as a required artifact and attach the resulting file evidence. If you discover a reusable correction, save it for future carrier settlement requests with supporting tool evidence."
      : "")
  );
}
function getReadPaths(call) {
  return call.tool_calls
    .filter((t) => t.name === "read_file")
    .map((t) => {
      try {
        return JSON.parse(t.arguments).path;
      } catch {
        return null;
      }
    });
}
async function run(f, phase, seed, lessons = []) {
  const firstContexts = [];
  const host = await startLiveTrial({
    learningSeed: seed,
    maxCalls: 20,
    seconds: 180,
    onRequest(input, call) {
      if (call.role === "executor" && firstContexts.length === 0)
        firstContexts.push(
          (input.messages || [])
            .filter(
              (m) =>
                m.role === "system" &&
                typeof m.content === "string" &&
                m.content.includes("[Past lessons and skills"),
            )
            .map((m) => m.content)
            .join("\n"),
        );
    },
  });
  try {
    report.binary_sha256 = host.hash;
    const { session, workspace } = await host.session();
    populate(workspace, f);
    if (cli) {
      makeCli(workspace);
      fs.writeFileSync(path.join(workspace, "registry", "week"), f.week);
      fs.writeFileSync(path.join(workspace, "registry", "source-path"), f.file);
    }
    if (contract)
      await host.api("/api/agents", {
        id: "settlements",
        name: "Settlement operations",
        system_prompt: "Use the local settlement utility and report the result.",
        tool_policy: { allow: ["shell", "read_file", "lesson_save", "lesson_retire"] },
      });
    const question = prompt(f, phase === "training");
    const started = Date.now(),
      answer = await host.send(session, question, contract ? "settlements" : undefined);
    const elapsed = Date.now() - started;
    let actual = null;
    try {
      actual = JSON.parse(fs.readFileSync(path.join(workspace, "settlement.json"), "utf8"));
    } catch {}
    const db = host.db();
    const results = db
      .prepare("SELECT payload FROM ui_events WHERE chat_id=? AND event='tool_card' ORDER BY sequence")
      .all(session)
      .map((x) => JSON.parse(x.payload));
    db.close();
    const terminal = results.filter((r) => ["done", "failed"].includes(r.status));
    const executor = host.calls.filter((c) => c.role === "executor");
    const paths = new Map();
    for (const c of executor)
      for (const t of c.tool_calls) {
        if (t.name === "read_file") {
          try {
            paths.set(t.id, JSON.parse(t.arguments).path);
          } catch {}
        }
      }
    const reads = terminal
      .filter((t) => t.name === "read_file")
      .map((t) => ({ path: paths.get(t.id) ?? null, status: t.status }));
    const snapshot = {
      phase,
      expected: f.expected,
      actual,
      status: answer.status,
      verified:
        answer.status === "success" && Object.entries(f.expected).every(([k, v]) => actual?.[k] === v),
      elapsed_ms: elapsed,
      prompt_sha256: crypto.createHash("sha256").update(question).digest("hex"),
      first_read_batch: executor.map(getReadPaths).find((x) => x.length) ?? [],
      reads,
      failed_reads: reads.filter((t) => t.status === "failed").length,
      tools: terminal.map((t) => ({ name: t.name, status: t.status })),
      foreground_model_calls: executor.length,
      training_lesson_ids_in_first_model_request: lessons
        .filter((l) => firstContexts[0]?.includes(l.action))
        .map((l) => l.id),
      provider_errors: host.calls.filter((c) => c.status >= 400).map((c) => c.status),
    };
    if (cli) {
      const trace = path.join(workspace, "carrier-invocations.log");
      snapshot.cli_invocations = fs.existsSync(trace)
        ? fs.readFileSync(trace, "utf8").trim().split(/\s+/)
        : [];
      snapshot.verified &&=
        snapshot.cli_invocations.includes("success") &&
        new RegExp("(^|[^0-9])" + f.expected.net_cents + "([^0-9]|$)").test(
          String(answer.message || "").replaceAll(",", ""),
        );
      snapshot.command_attempts = executor
        .flatMap((c) => c.tool_calls)
        .filter((t) => t.name === "shell")
        .map((t) => {
          try {
            return JSON.parse(t.arguments).command;
          } catch {
            return null;
          }
        });
    }
    if (phase === "training") {
      report.training = snapshot;
      save();
      const learning = new DatabaseSync(host.learningPath);
      try {
        for (let i = 0; i < 180; i++) {
          const row = learning.prepare("SELECT state FROM learn_reflection_queue LIMIT 1").get();
          if (row && ["done", "failed"].includes(row.state)) {
            snapshot.reflection = row.state;
            break;
          }
          await pause(250);
        }
        const all = learning
          .prepare('SELECT id, status, "trigger", action, support, evidence FROM learn_lessons ORDER BY id')
          .all();
        snapshot.lessons = all.map((l) => ({
          ...l,
          support: JSON.parse(l.support),
          evidence: JSON.parse(l.evidence),
        }));
        snapshot.provider_errors = host.calls.filter((c) => c.status >= 400).map((c) => c.status);
        learning.prepare("VACUUM INTO ?").run(path.join(work, "enabled.sqlite"));
      } finally {
        learning.close();
      }
    }
    return snapshot;
  } finally {
    await host.close();
  }
}
if (cli)
  Object.assign(report.method, {
    training:
      "Only the first task explicitly requires reproducing the legacy ticket command. No request to save a lesson, and no working command supplied.",
    fixture:
      "Isolated versioned carrier utility accepts one production syntax, logs its invocations, and calculates the report from a new ledger each week. Same bytes and prompt within each pair.",
    outcome:
      "All five report fields must match an independent calculation, the CLI must have succeeded, and the assistant must give the correct net cents.",
    firstSource:
      "Compare first actual CLI invocation, number of rejected invocations, artifact correctness and foreground requests.",
  });
if (contract)
  Object.assign(report.method, {
    training:
      "Natural account rule. Standard documented command omits a required calendar; the installed utility explains the correction on rejection. Same prompt template in training and test. No instruction to fail, save a lesson or consult memory.",
    agent:
      "Named settlement operations persona, with shell, read_file and lesson tools available. Same persona and portfolio in every arm.",
  });
try {
  report.training = await run(fixture("2026-W40"), "training");
  save();
  if (report.training.provider_errors.some((code) => [401, 402, 403].includes(code)))
    throw Error("Provider rejected training access or quota");
  const lessons = report.training.lessons.filter((l) => l.status === "active");
  if (!lessons.length) throw Error("No active training lesson for the paired intervention");
  fs.copyFileSync(path.join(work, "enabled.sqlite"), path.join(work, "withheld.sqlite"));
  const ablated = new DatabaseSync(path.join(work, "withheld.sqlite"));
  try {
    for (const l of lessons)
      ablated.prepare("UPDATE learn_lessons SET status='retired' WHERE id=?").run(l.id);
  } finally {
    ablated.close();
  }
  for (let i = 0; i < Number(count); i++) {
    const f = fixture("2026-W" + (41 + i));
    const pair = { week: f.week };
    report.pairs.push(pair);
    for (const arm of i % 2 === 0 ? ["withheld", "enabled"] : ["enabled", "withheld"]) {
      pair[arm] = await run(f, arm, path.join(work, arm + ".sqlite"), lessons);
      save();
      if (pair[arm].provider_errors.includes(429) && !pair[arm].verified)
        throw Error("Provider throttled before verified completion");
    }
  }
} catch (error) {
  report.interruption = String(error.message);
  process.exitCode = 1;
} finally {
  save();
  fs.rmSync(work, { recursive: true, force: true });
}
