// Actual Rust specialist settings/policy/recovery; all mutations are disposable MCP fixtures.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import http from "node:http";
import { spawn, execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { DatabaseSync } from "node:sqlite";
const root = fs.mkdtempSync("/tmp/kageha-profile-"),
  token = "disposable-profile-operator-token-long";
const persistent = process.env.KAGEHA_TEST_PROFILE_REPOSITORY === "1";
const repositoryBinary=path.resolve(process.env.CARGO_TARGET_DIR || "target", "debug/kageha-agent-profiles");
const repositoryArgs=()=>[root+"/profiles.sqlite","fixture-profile-scope"];
const repository=(...args)=>JSON.parse(execFileSync(repositoryBinary,[...repositoryArgs(),...args],{env:{PATH:root+"/empty"},encoding:"utf8"}));
const countName =
  "mcp_" +
  createHash("sha256").update("fixture:count").digest("hex").slice(0, 32);
const pause = (ms) => new Promise((r) => setTimeout(r, ms));
const counters = new Map(),
  holds = new Map(),
  errors = [],
  logs = [];
let child, base;
async function until(fn) {
  for (let end = Date.now() + 25000; Date.now() < end; ) {
    if (errors.length) throw errors[0];
    const v = await fn();
    if (v) return v;
    await pause(15);
  }
  throw Error("Condition timed out");
}
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
const plan = {
    objective: "Inspect",
    steps: [{ id: "inspect", title: "Read file", depends_on: [] }],
    outcomes: [{ id: "result", text: "Report evidence", required: true }],
  };
const model = http.createServer(async (req, res) => {
  try {
    let raw = "";
    for await (const b of req) raw += b;
    const body = JSON.parse(raw),
      intent = body.messages.find((m) => m.role === "user").content;
    if (String(body.messages[0]?.content).startsWith("You independently review ONE proposed action.")) {
      assert.equal(body.model, "specialist-fixture");
      const { request } = JSON.parse(intent);
      counters.set("fast-review", (counters.get("fast-review") || 0) + 1);
      res.end(JSON.stringify({ choices: [{ message: { role: "assistant", content: JSON.stringify({
        input_digest: request.authority.input_digest,
        verdict: "deny",
        rationale: "Fixture rejected the proposed write."
      }) } }] }));
      return;
    }
    if (intent === "planner" && body.messages.some(m => String(m.content).startsWith("Plan this task without executing it."))) {
      assert.equal(body.model, "specialist-fixture");
      assert.equal(body.tools?.length ?? 0, 0);
      res.end(JSON.stringify({ choices: [{ message: { role: "assistant", content: JSON.stringify(plan) } }] }));
      return;
    }
    const n = (counters.get(intent) || 0) + 1;
    counters.set(intent, n);
    const last = body.messages.filter((m) => m.role !== "system").at(-1);
    const has = (s) =>
      body.messages.some(
        (m) => m.role === "system" && String(m.content).includes(s),
      );
    let message;
    const tools=body.tools||[];
    const spawnTool = tools.find((t) => t.function.name === "agent_spawn");
    if (spawnTool)
      assert(
        !spawnTool.function.parameters.properties.agent_id.enum.includes(
          "blocked",
        ),
      );
    if (intent === "reader" || intent === "mutator") {
      assert(has(intent === "reader" ? "PINNED_RESEARCH" : "PINNED_MUTATOR"));
      assert.equal(body.model, intent === "reader" ? "specialist-fixture" : "fixture");
      if (n === 1)
        message = tool(
          "original",
          intent === "reader" ? "read_file" : countName,
          intent === "reader" ? { path: "notes.txt" } : {},
        );
      else if (n === 2) {
        await new Promise((r) => holds.set(intent, r));
        message = { role: "assistant", content: "Late cancelled reply" };
      } else if (n === 3)
        message = tool(
          "fresh",
          intent === "reader" ? "read_file" : countName,
          intent === "reader" ? { path: "notes.txt" } : {},
        );
      else message = { role: "assistant", content: last.content };
    } else if (intent === "quick") {
      assert.equal(body.model, "fixture");
      message = n === 1 ? tool("quick-write", countName, {}) : { role: "assistant", content: last.content };
    } else if (intent === "denied") {
      assert(has("PINNED_DENIED"));
      assert(!tools.some((t) => t.function.name === "read_file"));
      message =
        n === 1
          ? tool("excluded", "read_file", { path: "notes.txt" })
          : { role: "assistant", content: last.content };
    } else if (intent === "parent") {
      assert(has("PINNED_PARENT"));
      if (n === 1)
        message = tool("spawn", "agent_spawn", {
          agent_id: "child",
          objective: "child-work",
        });
      else if (n === 2) {
        const record = JSON.parse(last.content);
        message = tool("wait", "agent_wait_any", {
          job_ids: [record.spec.id],
          timeout_ms: 10000,
        });
      } else message = { role: "assistant", content: last.content };
    } else if (intent === "child-work") {
      assert(has("PINNED_CHILD"));
      assert(
        !tools.some((t) => t.function.name === countName),
        "Child cannot widen parent deny policy",
      );
      message =
        n === 1
          ? tool("excluded-mcp", countName, {})
          : n === 2
            ? tool("read-child", "read_file", { path: "notes.txt" })
            : { role: "assistant", content: last.content };
    } else if (intent === "planner") {
      assert(has("PINNED_PLANNER"));
      assert.equal(body.model, "fixture");
      const state = JSON.parse(
        body.messages
          .findLast((m) => String(m.content).includes("Current plan: "))
          .content.split("Current plan: ")[1],
      );
      message =
        n === 1
          ? tool("read-plan", "read_file", { path: "notes.txt" })
          : n === 2
            ? tool("progress", "task_plan_update", {
                expected_revision: state.revision,
                step_id: "inspect",
                status: "completed",
                evidence: ["read-plan"],
                outcome_ids: ["result"],
              })
            : { role: "assistant", content: "Verified planned result" };
    } else {
      assert(has("LEGACY_INSTRUCTIONS"));
      message =
        n === 1
          ? tool("read-legacy", "read_file", { path: "notes.txt" })
          : { role: "assistant", content: last.content };
    }
    res.end(JSON.stringify({ choices: [{ message }] }));
  } catch (e) {
    errors.push(e);
    res.statusCode = 500;
    res.end("{}");
  }
});
await new Promise((r) => model.listen(0, "127.0.0.1", r));
async function start() {
  const log = root + "/host-" + logs.length + ".log";
  logs.push(log);
  child = spawn(
    path.resolve(
      process.env.CARGO_TARGET_DIR || "target",
      "debug/kageha-runtime-service",
    ),
    [
      "--state",
      root + "/state",
      "--workspace",
      root + "/workspace",
      "--config",
      root + "/config.json",
      "--token-file",
      root + "/token",
      "--bind",
      "127.0.0.1:0",
    ],
    {
      env: { PATH: root + "/empty" },
      stdio: ["ignore", fs.openSync(log, "a"), fs.openSync(log, "a")],
    },
  );
  base = await until(() => {
    if (child.exitCode !== null) throw Error(fs.readFileSync(log, "utf8"));
    const m = fs
      .readFileSync(log, "utf8")
      .match(/runtime listening on (127\.0\.0\.1:\d+)\r?\n/);
    return m && "http://" + m[1];
  });
}
async function stop() {
  if (child && child.exitCode === null && child.signalCode === null) {
    child.kill("SIGTERM");
    await new Promise((r) => child.once("exit", r));
  }
}
async function api(route, body, status = 200) {
  const r = await fetch(base + route, {
    method: body ? "POST" : "GET",
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
    },
    body: body ? JSON.stringify(body) : undefined,
    signal: AbortSignal.timeout(15000),
  });
  const v = await r.json();
  assert.equal(r.status, status, JSON.stringify(v));
  return v;
}
function spec(id, agent = id, input = {}) {
  return {
    id,
    session: "session-" + id,
    turn: "turn-" + id,
    request: id,
    agent,
    parent: null,
    parent_run: null,
    depth: 0,
    objective: id,
    input: {
      messages: [{ role: "user", content: id }],
      timeout_ms: 30000,
      max_model_calls: 12,
      ...input,
    },
  };
}
async function done(id) {
  return until(async () => {
    const r = await api("/v1/runs/" + id);
    return (
      ["succeeded", "failed", "cancelled", "interrupted"].includes(r.state) && r
    );
  });
}
async function pending(turn) {
  return until(async () =>
    (await api("/v1/approvals" + (turn ? "?turn=" + turn : ""))).approvals.find(
      (r) => r.state === "pending",
    ),
  );
}
async function decide(r) {
  await api("/v1/approvals/" + r.id + "/decision", {
    expected_revision: r.revision,
    authority: r.request.authority,
    approved: true,
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
  fs.mkdirSync(root + "/workspace");
  fs.mkdirSync(root + "/empty");
  fs.writeFileSync(root + "/token", token, { mode: 0o600 });
  fs.writeFileSync(root + "/workspace/notes.txt", "scoped profile evidence");
  const profile = (id, prompt, policy, extra = {}) => ({
    id,
    name: id,
    system_prompt: prompt,
    tool_policy: policy,
    ...extra,
  });
  const config = {
    providers: [
      {
        endpoint: `http://127.0.0.1:${model.address().port}/model`,
        protocol: "openai_compat",
        model: "fixture",
      },
      {
        endpoint: `http://127.0.0.1:${model.address().port}/model`,
        protocol: "openai_compat",
        model: "specialist-fixture",
      },
    ],
    specialists: [
      {
        id: "legacy",
        instructions: "LEGACY_INSTRUCTIONS",
        allowed_tools: ["read_file"],
      },
    ],
    agent_profiles: [
      profile("research.ai", "PINNED_RESEARCH", {
        allow: ["read_*"],
        risk_class_overrides: { read_file: "high" },
      }, { model_preferences: { executor: "specialist-fixture" } }),
      profile("mutator", "PINNED_MUTATOR", {
        allow: ["mcp_*"],
        risk_class_overrides: { [countName]: "critical" },
      }),
      profile("quick", "Quick review fixture", {allow:["mcp_*"]}, {model_preferences:{fast_worker:"specialist-fixture", embedding:null}}),
      profile("denied", "PINNED_DENIED", {
        allow: ["read_*"],
        deny: ["read_file"],
      }),
      profile("parent", "PINNED_PARENT", {
        allow: ["read_*", "agent_*"],
        deny: ["mcp_*"],
        risk_class_overrides: { read_file: "high" },
      }),
      profile("child", "PINNED_CHILD", {
        allow: ["*"],
        risk_class_overrides: { read_file: "safe" },
      }),
      profile("planner", "PINNED_PLANNER", {
        allow: ["read_*", "task_plan_*"],
        risk_class_overrides: { task_plan_update: "high" },
      }, { model_preferences: { planner: "specialist-fixture" } }),
      profile(
        "blocked",
        "UNAVAILABLE",
        {},
        { model_preferences: { default: "not-silently-ignored" } },
      ),
    ],
    mcp_servers: [
      {
        id: "fixture",
        transport: "stdio",
        command: path.resolve(
          process.env.CARGO_TARGET_DIR || "target",
          "debug/examples/fixture",
        ),
        args: [],
        env_refs: {},
        header_refs: {},
        read_only_tools: [],
      },
    ],
  };
  const profileSeed = structuredClone(config.agent_profiles);
  if (persistent) {
    for (const profile of profileSeed) {
      fs.writeFileSync(root+"/profile.json",JSON.stringify(profile));
      repository("write","agent",profile.id,"0",root+"/profile.json");
    }
    config.agent_profiles=[];
    config.agent_repository={path:root+"/profiles.sqlite",scope:"fixture-profile-scope"};
  }
  fs.writeFileSync(root + "/config.json", JSON.stringify(config));
  await start();
  const catalog = await api("/v1/workers/catalog");
  assert(catalog.specialists.some((p) => p.id === "research.ai"));
  assert.deepEqual(
    catalog.profiles.find((p) => p.id === "blocked")
      .unsupported_execution_settings,
    ["model_preferences"],
  );
  const blocked = await api("/v1/runs", spec("blocked", "blocked"), 503);
  assert(blocked.message.includes("model_preferences"));
  const db = new DatabaseSync(root + "/state/runtime.sqlite", {
    readOnly: true,
  });
  assert.equal(
    db.prepare("SELECT count(*) n FROM runs WHERE id='blocked'").get().n,
    0,
  );
  db.close();
  await api("/v1/runs", spec("legacy", "legacy"));
  assert.equal(
    (await done("legacy")).completion.result.message,
    "scoped profile evidence",
  );
  await api("/v1/permissions", { session: "session-quick", profile: "auto_review" });
  await api("/v1/runs", spec("quick", "quick"));
  assert.equal((await done("quick")).state, "succeeded");
  assert.equal(counters.get("fast-review"), 1);
  assert(!effect("turn-quick", "tool:quick-write"));
  await api("/v1/runs", spec("denied", "denied"));
  assert(
    (await done("denied")).completion.result.message.includes("not exposed"),
  );
  assert(!effect("turn-denied", "tool:excluded"));
  for (const [id, agent] of [
    ["reader", "research.ai"],
    ["mutator", "mutator"],
  ]) {
    await api("/v1/permissions", {
      session: "session-" + id,
      profile: "full",
      confirm_full: true,
    });
    await api("/v1/runs", spec(id, agent));
    const approval = await pending("turn-" + id);
    assert.equal(approval.request.class, "restricted");
    await decide(approval);
    await until(() => holds.has(id));
    const receipt = JSON.parse(effect("turn-" + id, "tool:original").result);
    assert.equal(receipt._binding.class, "restricted");
    assert.equal(
      receipt._mutation_binding?.class || null,
      id === "mutator" ? "mutation" : null,
    );
    await api("/v1/runs/" + id + "/cancel", {});
    assert.equal((await done(id)).state, "cancelled");
    holds.get(id)();
    const resumed = await api("/v1/runs/" + id + "/resume", {
      command_id: "resume-once",
      confirmed: true,
    });
    if (id === "reader") {
      const review = await pending(resumed.spec.turn);
      assert.equal(review.request.class, "restricted");
      await decide(review);
      assert.equal(
        (await done(resumed.spec.id)).completion.result.message,
        "scoped profile evidence",
      );
    } else {
      assert.equal((await done(resumed.spec.id)).state, "failed");
      assert(
        !effect(resumed.spec.turn, "tool:fresh"),
        "Risk escalation must not bypass mutation replay fence",
      );
      assert.equal(
        (await api("/v1/approvals?turn=" + resumed.spec.turn)).approvals.length,
        0,
      );
    }
  }
  await api("/v1/runs", spec("parent", "parent"));
  await decide(await pending("turn-parent"));
  const childApproval = await pending();
  assert.equal(childApproval.request.action, "read_file");
  assert.equal(childApproval.request.class, "restricted");
  await decide(childApproval);
  const parent = await done("parent");
  assert.equal(parent.state, "succeeded");
  const result = JSON.parse(parent.completion.result.message);
  assert.equal(
    result.finished[0].completion.result.message,
    "scoped profile evidence",
  );
  assert(!effect(result.finished[0].spec.turn, "tool:excluded-mcp"));
  await api("/v1/permissions", {
    session: "session-planner",
    profile: "full",
    confirm_full: true,
  });
  await api(
    "/v1/runs",
    spec("planner", "planner", { planning: { required_outcomes: plan.outcomes } }),
  );
  const planApproval = await pending("turn-planner");
  assert.equal(planApproval.request.class, "plan");
  await decide(planApproval);
  const progress = await pending("turn-planner");
  assert.equal(progress.request.action, "task_plan_update");
  assert.equal(progress.request.class, "restricted");
  await decide(progress);
  assert.equal((await done("planner")).state, "succeeded");
  await stop();
  await start();
  assert.equal((await done("legacy")).state, "succeeded");
  assert.equal(
    (await api("/v1/workers/catalog")).profiles.find(
      (p) => p.id === "research.ai",
    ).effective.system_prompt,
    "PINNED_RESEARCH",
  );
  const changed = structuredClone(config);
  if (persistent) {
    assert.throws(()=>repository("export"),/Another profile writer is active/);
  }
  await stop();
  if (persistent) {
    const profile={...profileSeed[0],system_prompt:"changed after restart"};
    fs.writeFileSync(root+"/profile.json",JSON.stringify(profile));
    const revised=repository("write","agent",profile.id,"1",root+"/profile.json");
    assert.equal(revised.revisions.length,2);
  } else changed.agent_profiles[0].system_prompt = "changed after restart";
  fs.writeFileSync(root + "/config.json", JSON.stringify(changed));
  await start();
  const old = (await api("/v1/runs/reader")).spec;
  await api("/v1/runs", old, 409);
  assert.deepEqual(errors, []);
  console.log(
    JSON.stringify({
      passed: true,
      python_backend: false,
      node_backend: false,
      persistent_profiles:persistent,
      checks: [
        "shared profile settings",
        "named planner and executor routes",
        "fast-worker automatic review route",
        "legacy specialist compatibility",
        "pinned direct-run instructions",
        "wildcard allow and deny",
        "risk cannot be lowered by child or full permission mode",
        "reviewed reads remain fresh observations",
        "reviewed mutations retain replay fences",
        "planning tools inherit risk",
        "unsupported settings explicit before admission",
        "restart and changed-profile fence",
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
