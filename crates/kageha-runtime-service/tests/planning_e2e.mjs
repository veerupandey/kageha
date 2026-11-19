// Real standalone Rust host and owned plan journal. Node is only the model/operator fixture.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import http from "node:http";
import { spawn } from "node:child_process";
import { DatabaseSync } from "node:sqlite";
import { AgentClient } from "../../../sdk/typescript/agent.mjs";
const root = fs.mkdtempSync("/tmp/kageha-planning-"),
  workspace = root + "/workspace",
  token = "disposable-planning-operator-token";
const pause = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(fn) {
  for (let end = Date.now() + 25000; Date.now() < end; ) {
    const v = await fn();
    if (v) return v;
    await pause(15);
  }
  throw Error("Condition timed out");
}
const calls = new Map(),
  errors = [],
  holds = new Map(),
  logs = [];
let child, base;
const document = {
  objective: "Deliver evidence",
  steps: [{ id: "inspect", title: "Inspect retained file", depends_on: [] }],
  outcomes: [
    { id: "result", text: "Deliver evidence-backed result", required: true },
  ],
};
const final = (content) => ({ role: "assistant", content });
const tool = (id, name, args) => ({
  role: "assistant",
  content: null,
  tool_calls: [
    {
      id,
      type: "function",
      function: { name, arguments: JSON.stringify(args) },
    },
  ],
});
const model = http.createServer(async (req, res) => {
  try {
    let raw = "";
    for await (const b of req) raw += b;
    const body = JSON.parse(raw),
      intent = body.messages.filter((m) => m.role === "user").at(0).content;
    if (!body.tools?.length) {
      assert(
        body.messages.some((m) => String(m.content).includes("Plan this task")),
      );
      calls.set(intent + ":planner", (calls.get(intent + ":planner") || 0) + 1);
      res.end(
        JSON.stringify({
          choices: [
            {
              message: final(
                JSON.stringify({
                  ...document,
                  outcomes: [],
                  steps: [
                    {
                      ...document.steps[0],
                      status: "completed",
                      evidence: ["invented"],
                    },
                  ],
                }),
              ),
            },
          ],
        }),
      );
      return;
    }
    const count = (calls.get(intent) || 0) + 1;
    calls.set(intent, count);
    assert(
      !body.tools.some((t) => t.function.name === "approve_plan"),
      "Approval is host-only",
    );
    const context = body.messages.findLast(
      (m) =>
        m.role === "system" && String(m.content).includes("Current plan: "),
    );
    assert(context);
    const current = JSON.parse(context.content.split("Current plan: ")[1]);
    assert(current.approved);
    const update = (
      id,
      step = "inspect",
      outcomes = ["result"],
      evidence = ["read-good"],
    ) =>
      tool(id, "task_plan_update", {
        expected_revision: current.revision,
        step_id: step,
        status: "completed",
        evidence,
        outcome_ids: outcomes,
      });
    let message;
    if (["normal", "generated", "edited"].includes(intent)) {
      message =
        count === 1
          ? final("Unverified completion claim")
          : count === 2
            ? update("invented", "inspect", ["result"], ["made-up"])
            : count === 3
              ? tool("read-good", "read_file", { path: "notes.txt" })
              : count === 4
                ? update("progress")
                : final("Verified completion");
    } else if (intent === "dependencies") {
      message =
        count === 1
          ? tool("read-good", "read_file", { path: "notes.txt" })
          : count === 2
            ? update("too-early", "deliver")
            : count === 3
              ? update("inspect", "inspect", [])
              : count === 4
                ? update("deliver", "deliver")
                : final("Dependencies verified");
    } else if (intent === "cross-evidence") {
      message = count === 1 ? update("foreign") : final("False completion");
    } else if (intent === "self-evidence") {
      message =
        count === 1
          ? tool("self", "task_plan_read", {})
          : count === 2
            ? update("fake-evidence", "inspect", ["result"], ["self"])
            : final("False completion");
    } else if (intent === "outcome-pending") {
      message =
        count === 1
          ? tool("read-good", "read_file", { path: "notes.txt" })
          : count === 2
            ? update("step-only", "inspect", [])
            : final("False completion");
    } else if (intent === "resume") {
      if (count === 1)
        message = tool("read-good", "read_file", { path: "notes.txt" });
      else if (count === 2)
        message = tool("started", "task_plan_update", {
          expected_revision: current.revision,
          step_id: "inspect",
          status: "in_progress",
        });
      else if (count === 3) {
        await new Promise((r) => holds.set("resume", r));
        message = final("Late cancelled result");
      } else if (count === 4) message = update("resumed-progress");
      else message = final("Resumed with evidence");
    } else if (intent === "crash") {
      if (count === 1)
        message = tool("read-good", "read_file", { path: "notes.txt" });
      else if (count === 2) {
        message = update("commit-before-receipt");
      } else throw Error("Crash recovery must not replay model or tool input");
    } else message = final("No action");
    res.end(JSON.stringify({ choices: [{ message }] }));
  } catch (e) {
    errors.push(e);
    res.statusCode = 500;
    res.end("{}");
  }
});
await new Promise((r) => model.listen(0, "127.0.0.1", r));
async function start(fault = false) {
  const log = root + "/host-" + logs.length + ".log";
  logs.push(log);
  const binary = path.resolve(
    process.env.CARGO_TARGET_DIR || "target",
    fault
      ? "debug/examples/planning_fault_fixture"
      : "debug/kageha-runtime-service",
  );
  const args = fault
    ? [root, `http://127.0.0.1:${model.address().port}/model`]
    : [
        "--state",
        root + "/state",
        "--workspace",
        workspace,
        "--config",
        root + "/config.json",
        "--token-file",
        root + "/token",
        "--bind",
        "127.0.0.1:0",
      ];
  child = spawn(binary, args, {
    env: { PATH: root + "/empty" },
    stdio: ["ignore", fs.openSync(log, "a"), fs.openSync(log, "a")],
  });
  base = await until(() => {
    if (child.exitCode !== null) throw Error(fs.readFileSync(log, "utf8"));
    const m = fs
      .readFileSync(log, "utf8")
      .match(/runtime listening on (127\.0\.0\.1:\d+)/);
    return m && "http://" + m[1];
  });
}
async function stop(signal = "SIGTERM") {
  if (child && child.exitCode === null && child.signalCode === null) {
    child.kill(signal);
    await new Promise((r) => child.once("exit", r));
  }
}
async function api(route, body, status = 200) {
  const res = await fetch(base + route, {
    method: body ? "POST" : "GET",
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
    },
    body: body ? JSON.stringify(body) : undefined,
    signal: AbortSignal.timeout(15000),
  });
  const data = await res.json();
  assert.equal(res.status, status, JSON.stringify(data));
  return data;
}
function spec(id, options = {}) {
  const planning = { document, ...options.planning };
  return {
    id,
    session: "session-" + id,
    turn: "turn-" + id,
    request: id,
    agent: options.agent || "main",
    depth: 0,
    parent: null,
    parent_run: null,
    objective: id,
    input: {
      messages: [{ role: "user", content: id }],
      planning,
      max_model_calls: 12,
      timeout_ms: 30000,
      ...options.input,
    },
  };
}
async function submit(id, options = {}) {
  await new AgentClient(base, token).run(spec(id, options));
}
async function done(id) {
  return until(async () => {
    const r = await new AgentClient(base, token).status(id);
    return (
      ["succeeded", "failed", "cancelled", "interrupted"].includes(r.state) && r
    );
  });
}
async function pending(id) {
  return until(async () => {
    const rows = await api("/v1/approvals?turn=turn-" + id);
    return rows.approvals.find((r) => r.state === "pending");
  });
}
async function decide(r, approved = true) {
  await api("/v1/approvals/" + r.id + "/decision", {
    expected_revision: r.revision,
    authority: r.request.authority,
    approved,
    scope: "once",
  });
}
function effect(turn, id) {
  const db = new DatabaseSync(root + "/state/runtime.sqlite", {
    readOnly: true,
  });
  try {
    return db
      .prepare("SELECT * FROM effects WHERE scope=? AND id=?")
      .get(turn, id);
  } finally {
    db.close();
  }
}
try {
  fs.mkdirSync(workspace);
  fs.mkdirSync(root + "/empty");
  fs.writeFileSync(root + "/token", token, { mode: 0o600 });
  fs.writeFileSync(workspace + "/notes.txt", "retained evidence");
  fs.writeFileSync(
    root + "/config.json",
    JSON.stringify({
      providers: [
        {
          endpoint: `http://127.0.0.1:${model.address().port}/model`,
          model: "fixture",
          protocol: "openai_compat",
        },
      ],
      specialists: [
        {
          id: "restricted",
          instructions: "Read only",
          allowed_tools: ["read_file"],
        },
      ],
    }),
  );
  await start();
  assert((await api("/v1/info")).capabilities.includes("planning.evidence"));
  await submit("normal");
  let approval = await pending("normal");
  assert.equal(approval.request.action, "approve_plan");
  assert.equal(approval.request.class, "plan");
  assert(approval.request.detail.includes("Deliver evidence-backed result"));
  assert(approval.request.detail.includes("Exact plan document:"));
  assert.equal(
    calls.get("normal"),
    undefined,
    "No task model/tool before approval",
  );
  assert.equal((await api("/v1/runs/normal/plan")).plan.approved, false);
  await decide(approval);
  assert.equal((await done("normal")).state, "succeeded");
  assert((await api("/v1/runs/normal/plan")).complete);
  assert(JSON.parse(effect("turn-normal", "tool:invented").result).is_error);
  assert.equal(calls.get("normal"), 5);
  await api("/v1/runs", spec("normal"));
  assert.equal(calls.get("normal"), 5);
  await api(
    "/v1/runs/normal/plan",
    { command_id: "locked", expected_revision: 3, document },
    409,
  );
  await submit("deny");
  approval = await pending("deny");
  await decide(approval, false);
  assert.equal((await done("deny")).state, "failed");
  assert.equal(calls.get("deny"), undefined);
  assert(!effect("turn-deny", "tool:read-good"));
  await submit("edited");
  const old = await pending("edited");
  const original = (await api("/v1/runs/edited/plan")).plan;
  await api(
    "/v1/runs/edited/plan",
    {
      command_id: "drop",
      expected_revision: original.revision,
      document: { ...document, outcomes: [] },
    },
    409,
  );
  const changed = {
    ...document,
    steps: [{ ...document.steps[0], title: "Revised inspection" }],
  };
  const edit = {
    command_id: "edit-once",
    expected_revision: original.revision,
    document: changed,
  };
  const edited = await api("/v1/runs/edited/plan", edit);
  assert.equal(edited.plan.revision, 2);
  assert.equal((await api("/v1/runs/edited/plan", edit)).plan.revision, 2);
  await api("/v1/runs/edited/plan", { ...edit, command_id: "conflict" }, 409);
  await decide(old);
  approval = await pending("edited");
  assert.notEqual(approval.id, old.id);
  assert(approval.request.detail.includes("Revised inspection"));
  await decide(approval);
  assert.equal((await done("edited")).state, "succeeded");
  await submit("generated", {
    input: { planning: { required_outcomes: document.outcomes } },
  });
  approval = await pending("generated");
  const generated = (await api("/v1/runs/generated/plan")).plan;
  assert.equal(generated.steps[0].status, "pending");
  assert.equal(generated.outcomes[0].id, "result");
  assert.equal(generated.outcomes[0].done, false);
  assert.equal(calls.get("generated:planner"), 1);
  await decide(approval);
  assert.equal((await done("generated")).state, "succeeded");
  await submit("budget", {
    input: {
      planning: { required_outcomes: document.outcomes },
      max_model_calls: 1,
    },
  });
  await decide(await pending("budget"));
  assert.equal((await done("budget")).state, "failed");
  assert.equal(calls.get("budget:planner"), 1);
  assert.equal(calls.get("budget"), undefined);
  const depDoc = {
    ...document,
    steps: [
      ...document.steps,
      { id: "deliver", title: "Deliver", depends_on: ["inspect"] },
    ],
  };
  await submit("dependencies", { planning: { document: depDoc } });
  await decide(await pending("dependencies"));
  assert.equal((await done("dependencies")).state, "succeeded");
  assert(
    JSON.parse(effect("turn-dependencies", "tool:too-early").result).is_error,
  );
  for (const id of ["cross-evidence", "self-evidence", "outcome-pending"]) {
    await submit(id);
    await decide(await pending(id));
    assert.equal((await done(id)).state, "failed");
    assert.equal((await api("/v1/runs/" + id + "/plan")).complete, false);
  }
  assert(
    JSON.parse(effect("turn-cross-evidence", "tool:foreign").result).is_error,
  );
  assert(
    JSON.parse(effect("turn-self-evidence", "tool:fake-evidence").result)
      .is_error,
  );
  await submit("restricted", { agent: "restricted" });
  await decide(await pending("restricted"));
  assert.equal((await done("restricted")).state, "failed");
  assert.equal((await api("/v1/runs/restricted/plan")).complete, false);
  await submit("cancel");
  approval = await pending("cancel");
  await api("/v1/runs/cancel/cancel", {});
  assert.equal((await done("cancel")).state, "cancelled");
  await api(
    "/v1/approvals/" + approval.id + "/decision",
    {
      expected_revision: approval.revision,
      authority: approval.request.authority,
      approved: true,
      scope: "once",
    },
    409,
  );
  await submit("resume");
  await decide(await pending("resume"));
  await until(() => holds.has("resume"));
  await api("/v1/runs/resume/cancel", {});
  assert.equal((await done("resume")).state, "cancelled");
  holds.get("resume")();
  const resumed = await api("/v1/runs/resume/resume", {
    command_id: "resume-once",
    confirmed: true,
  });
  const resumedDone = await done(resumed.spec.id);
  assert.equal(resumedDone.state, "succeeded", JSON.stringify(resumedDone));
  assert((await api("/v1/runs/" + resumed.spec.id + "/plan")).complete);
  assert.equal(
    (await api("/v1/runs/" + resumed.spec.id + "/plan")).plan_key.turn,
    "turn-resume",
  );
  await stop();
  await start(true);
  await submit("crash");
  await decide(await pending("crash"));
  await until(() =>
    fs.existsSync(root + "/state/plan-committed-before-receipt"),
  );
  assert((await api("/v1/runs/crash/plan")).complete);
  assert.equal(effect("turn-crash", "tool:commit-before-receipt").result, null);
  await stop("SIGKILL");
  await start();
  assert.equal((await done("crash")).state, "interrupted");
  assert.equal(calls.get("crash"), 2);
  assert((await api("/v1/runs/crash/plan")).complete);
  await api("/v1/runs", spec("crash"));
  assert.equal(calls.get("crash"), 2);
  await api(
    "/v1/runs/crash/resume",
    { command_id: "must-not-replay", confirmed: true },
    409,
  );
  assert((await api("/v1/runs/normal/plan")).complete);
  assert.deepEqual(errors, []);
  console.log(
    JSON.stringify({
      passed: true,
      python_backend: false,
      node_backend: false,
      checks: [
        "exact plan review",
        "public Agent SDK planning input",
        "required outcomes immutable",
        "drafts cannot self-approve or complete",
        "revision conflicts and exact retry",
        "stale approval requires fresh decision",
        "generated planner requirements",
        "planner calls consume the run budget",
        "successful evidence and dependencies",
        "foreign and planning evidence denied",
        "incomplete final claims repaired or stopped",
        "specialist allowlist",
        "cancellation and stale decision",
        "same-plan continuation with confirmed receipts",
        "actual crash between plan commit and receipt publication",
        "restart preserves plans without replay",
      ],
      fixture: root,
    }),
  );
} catch (e) {
  for (const log of logs) console.error(fs.readFileSync(log, "utf8"));
  throw e;
} finally {
  for (const release of holds.values()) release();
  await stop();
  model.closeAllConnections();
  await new Promise((r) => model.close(r));
  fs.rmSync(root, { recursive: true, force: true });
}
