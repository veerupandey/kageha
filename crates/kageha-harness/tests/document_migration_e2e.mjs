import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import http from "node:http";
import { spawn, spawnSync } from "node:child_process";
import { DatabaseSync } from "node:sqlite";
import { credentialFixture } from "../../kageha-loop/tests/credential_fixture.mjs";
import { isolatedModelFixture } from "../../kageha-loop/tests/isolated_model_fixture.mjs";
const root = fs.mkdtempSync(
  path.join(os.tmpdir(), "kageha-document-migration-"),
);
const bin = path.resolve(
  process.env.CARGO_TARGET_DIR || "target/migration-acceptance",
  "debug",
);
const launcher = path.join(bin, "kageha"),
  source = path.join(root, "sessions"),
  agents = path.join(root, "agents"),
  state = path.join(root, "state"),
  chat = "old-chat";
const agent = "research.ai-" + "a".repeat(60),
  snapshot = "20261106T080000.000000Z-update-abcd.yaml";
fs.mkdirSync(path.join(source, chat), { recursive: true });
fs.mkdirSync(path.join(agents, ".history", agent), { recursive: true });
fs.mkdirSync(path.join(root, "workspace"));
fs.mkdirSync(path.join(root, "empty"));
fs.writeFileSync(
  path.join(source, chat, "chat.jsonl"),
  JSON.stringify({
    role: "user",
    text: "Historical objective",
    turn_id: "old-turn",
  }) + "\n",
);
const task = {
  version: 1,
  revision: 7,
  objective: "Finish retained task",
  turn_id: "old-turn",
  constraints: ["Do not submit anything"],
  current_stage_id: "active",
  stages: [
    {
      id: "done",
      description: "Earlier research",
      status: "done",
      depends_on: [],
      outcome_ids: ["report"],
      evidence_refs: ["old-call"],
    },
    {
      id: "active",
      description: "Remaining review",
      status: "active",
      depends_on: ["done"],
      outcome_ids: ["review"],
    },
  ],
  goals: [
    { id: "report", description: "Retain the report", passes: true },
    { id: "review", description: "Review the result", passes: false },
  ],
  budget: { usd_spent: 1.25, max_usd: 2 },
  browser_blocker: { id: "stale-binding-123" },
  answered_questions: { standing_fact: "Previously supplied fact" },
};
fs.writeFileSync(
  path.join(source, chat, "task_state.json"),
  JSON.stringify(task),
);
const definition = {
  schema_version: 1,
  id: agent,
  name: "Migrated Researcher",
  system_prompt: "Current imported instructions",
  pinned_skills: ["research"],
  tool_policy: {
    allow: ["read_file"],
    deny: ["bash"],
    risk_class_overrides: { read_file: "high" },
  },
  model_preferences: { planner: "saved-planner" },
  knowledge: { inherit_workspace: false, extra_kb_ids: [] },
  created_at: "2026-11-01T00:00:00Z",
  updated_at: "2026-11-07T00:00:00Z",
};
fs.writeFileSync(
  path.join(agents, agent + ".yaml"),
  JSON.stringify(definition),
);
fs.writeFileSync(
  path.join(agents, ".history", agent, snapshot),
  JSON.stringify({
    ...definition,
    system_prompt: "Previous imported instructions",
  }),
);
fs.writeFileSync(
  path.join(agents, ".default-knowledge.yaml"),
  "inherit_workspace: false\nextra_kb_ids: []\n",
);
let requests = [];
const model = http.createServer(async (req, res) => {
  let text = "";
  for await (const part of req) text += part;
  requests.push(JSON.parse(text));
  res.end(
    JSON.stringify({
      choices: [
        {
          message: { role: "assistant", content: "Current context received." },
        },
      ],
    }),
  );
});
await new Promise((r) => model.listen(0, "127.0.0.1", r));
const token = "disposable-document-migration-access-token";
const env = {
  PATH: path.join(root, "empty"),
  HOME: root,
  ...credentialFixture(root),
  ...isolatedModelFixture(root),
  KAGEHA_SERVER_TOKEN: token,
  KAGEHA_LEGACY_SESSIONS_DIR: path.join(root, "empty"),
  KAGEHA_PROVIDER_URL: `http://127.0.0.1:${model.address().port}/v1/chat/completions`,
  KAGEHA_MODEL: "fixture",
};
function migrate(flag, from, success = true) {
  const result = spawnSync(
    launcher,
    ["migrate", flag, from, "--state-dir", state],
    { env, cwd: root, encoding: "utf8", timeout: 15000 },
  );
  if (success) {
    assert.equal(result.status, 0, result.stderr);
    return JSON.parse(result.stdout).report;
  }
  assert.notEqual(result.status, 0);
  return result;
}
let child, base;
async function start() {
  let log = "";
  child = spawn(
    path.join(bin, "kageha-loop"),
    ["serve", path.join(root, "workspace"), state, "127.0.0.1:0"],
    { env, cwd: root, stdio: ["ignore", "pipe", "pipe"] },
  );
  child.stdout.on("data", (b) => {
    log += b;
  });
  child.stderr.on("data", (b) => {
    log += b;
  });
  for (const end = Date.now() + 15000; Date.now() < end; ) {
    if (child.exitCode !== null) throw Error(log);
    const m = log.match(/listening on (127\.0\.0\.1:\d+)/);
    if (m) {
      base = `http://${m[1]}`;
      return;
    }
    await new Promise((r) => setTimeout(r, 25));
  }
  throw Error(log || "Startup timed out");
}
async function stop() {
  if (child && child.exitCode === null && child.signalCode === null) {
    const end = new Promise((r) => child.once("exit", r));
    child.kill("SIGTERM");
    await end;
  }
}
async function api(route, method = "GET", body, expected = 200) {
  const response = await fetch(base + route, {
    method,
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
    },
    body: body ? JSON.stringify(body) : undefined,
    signal: AbortSignal.timeout(15000),
  });
  const text = await response.text();
  assert.equal(response.status, expected, text);
  return text ? JSON.parse(text) : null;
}
try {
  migrate("--legacy-sessions", source);
  assert.equal(migrate("--legacy-agents", agents).imported, 2);
  assert.equal(migrate("--legacy-task-plans", source).imported, 1);
  assert.equal(migrate("--legacy-agents", agents).already_present, 2);
  assert.equal(migrate("--legacy-task-plans", source).already_present, 1);
  await start();
  let migrated = (await api(`/api/agents/${agent}`)).persona;
  assert.equal(migrated.id, agent);
  assert.deepEqual(migrated.tool_policy, definition.tool_policy);
  assert.deepEqual(migrated.model_preferences, definition.model_preferences);
  const history = (await api(`/api/agents/${agent}/history`)).history;
  const old = history.find((r) => r.id === snapshot);
  assert(old);
  assert.equal(old.timestamp, Date.UTC(2026, 10, 6, 8) / 1000);
  assert.equal(
    (await api("/api/agents/main")).persona.knowledge.inherit_workspace,
    false,
  );
  const session = await api("/api/sessions", "POST", { persona_id: agent });
  assert.equal(
    (await api(`/api/sessions/${session.session_id}`)).persona_id,
    agent,
  );
  const plan = await api(`/api/sessions/${chat}/design`);
  assert.equal(plan.revision, 7);
  assert.equal(plan.approved, false);
  assert.equal(plan.todo_board.complete, false);
  assert.equal(plan.todo_board.verified, 0);
  assert.equal(plan.todo_board.outcomes.length, 2);
  assert(plan.todo_board.outcomes.every((o) => !o.done));
  assert(plan.files["plan.md"].includes("Retain the report"));
  await api(
    `/api/sessions/${chat}/design`,
    "PUT",
    {
      plan: {
        objective: "Try now",
        steps: [{ id: "one", title: "Only one thing", depends_on: [] }],
      },
    },
    409,
  );
  await api(`/api/agents/${agent}`, "PUT", {
    system_prompt: "User edited instructions",
  });
  await api("/api/chat/send", "POST", {
    session_id: chat,
    client_request_id: "continue-import",
    message: "Continue this task without submitting anything",
  });
  assert.equal(requests.length, 1);
  const messages = JSON.stringify(requests[0].messages);
  assert(messages.includes("Historical task context"));
  assert(messages.includes("Retain the report"));
  assert(messages.includes("historical_budget"));
  assert(messages.includes("Previously supplied fact"));
  assert(!messages.includes("stale-binding-123"));
  await stop();
  migrate("--legacy-agents", agents);
  migrate("--legacy-task-plans", source);
  const db = new DatabaseSync(path.join(state, "native-chats.sqlite"), {
    readOnly: true,
  });
  assert.equal(db.prepare("SELECT COUNT(*) AS n FROM ui_jobs").get().n, 0);
  db.close();
  await start();
  assert.equal(
    (await api(`/api/agents/${agent}`)).persona.system_prompt,
    "User edited instructions",
  );
  await api(`/api/agents/${agent}`, "DELETE");
  await stop();
  migrate("--legacy-agents", agents);
  await start();
  await api(`/api/agents/${agent}`, "GET", undefined, 404);
  const restored = await api(`/api/agents/${agent}/rollback`, "POST", {
    snapshot_id: snapshot,
  });
  assert.equal(restored.agent.system_prompt, "Previous imported instructions");
  await stop();
  fs.writeFileSync(
    path.join(agents, agent + ".yaml"),
    JSON.stringify({ ...definition, name: "Changed source" }),
  );
  migrate("--legacy-agents", agents, false);
  console.log(
    JSON.stringify({
      passed: true,
      python_backend: false,
      node_backend: false,
      agent_identity_and_revisions: true,
      default_knowledge_overlay: true,
      plans_preserve_required_outcomes: true,
      historical_authority_not_reused: true,
      new_turn_receives_context: true,
      edits_and_deletions_preserved: true,
      no_worker_replay: true,
      fixture: root,
    }),
  );
} finally {
  await stop();
  model.closeAllConnections();
  await new Promise((r) => model.close(r));
}
