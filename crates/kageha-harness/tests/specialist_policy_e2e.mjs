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
  path.join(os.tmpdir(), "kageha-specialist-policy-"),
);
const bin = path.resolve(
  process.env.CARGO_TARGET_DIR || "target/migration-acceptance",
  "debug",
);
const launcher = path.join(bin, "kageha"),
  agents = path.join(root, "agents"),
  state = path.join(root, "state");
const agent = "research.ai-" + "a".repeat(60),
  snapshot = "20261106T080000.000000Z-update-abcd.yaml";
fs.mkdirSync(path.join(agents, ".history", agent), { recursive: true });
fs.mkdirSync(path.join(root, "workspace"));
fs.mkdirSync(path.join(root, "empty"));
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

  const request = JSON.parse(text);
  requests.push(request);
  const intent =
    request.messages.filter((m) => m.role === "user").at(-1)?.content || "";
  const tool = request.messages.at(-1);
  let message = { role: "assistant", content: "Finished fixture turn" };
  if (tool.role !== "tool") {
    const delegate = intent === "delegate-risk";
    message = {
      role: "assistant",
      content: null,
      tool_calls: [
        {
          id: "call-" + intent,
          type: "function",
          function: {
            name: delegate ? "agent_spawn" : "read_file",
            arguments: JSON.stringify(
              delegate
                ? { agent_id: "risk-child", objective: "child-risk" }
                : { path: "evidence.txt" },
            ),
          },
        },
      ],
    };
  }
  res.end(JSON.stringify({ choices: [{ message }] }));
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
const pause = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(check) {
  for (let end = Date.now() + 15000; Date.now() < end; ) {
    const v = await check();
    if (v) return v;
    await pause(25);
  }
  throw Error("Acceptance timed out");
}
async function pending(id) {
  return until(async () => (await api("/api/sessions/" + id)).pending_approval);
}
async function send(id, message) {
  return api("/api/chat/send", "POST", {
    session_id: id,
    client_request_id: message,
    message,
  });
}
const evidence = "DISPOSABLE_RISK_APPROVAL_EVIDENCE";
fs.writeFileSync(path.join(root, "workspace", "evidence.txt"), evidence);
// Parent can delegate, but read risk and deny rules must constrain its child.
definition.tool_policy.allow = ["read_file", "agent_spawn"];
definition.tool_policy.risk_class_overrides = { read_file: "high" };
fs.writeFileSync(
  path.join(agents, agent + ".yaml"),
  JSON.stringify(definition),
);
try {
  migrate("--legacy-agents", agents);
  await start();
  await api("/api/agents", "POST", {
    id: "risk-child",
    name: "Child",
    tool_policy: { allow: ["*"], risk_class_overrides: { read_file: "safe" } },
  });
  await api(
    "/api/agents",
    "POST",
    {
      id: "invalid",
      name: "Invalid",
      tool_policy: { risk_class_overrides: { read_file: true } },
    },
    400,
  );
  const id = (await api("/api/sessions", "POST", { persona_id: agent }))
    .session_id;
  await api("/api/permissions", "POST", {
    session_id: id,
    profile: "full",
    confirm_full: true,
  });
  let turn = send(id, "risk-approved");
  let p = await pending(id);
  assert(p.detail.includes("Specialist policy"));
  assert.equal(p.risk_class, "specialist");
  assert.equal(p.action, "read_file");
  assert(!JSON.stringify(requests).includes(evidence));
  const approved = await api("/api/approvals", "POST", {
    approval_id: p.approval_id,
    approved: true,
    scope: "full",
  });
  assert.equal(approved.scope, "once");
  await turn;
  assert(JSON.stringify(requests).includes(evidence));
  // A previous grant and Full mode cannot approve the next exact invocation.
  turn = send(id, "risk-denied");
  p = await pending(id);
  await api("/api/approvals", "POST", {
    approval_id: p.approval_id,
    approved: false,
  });
  await turn;
  const denied = requests
    .filter((r) =>
      r.messages.some((m) => m.role === "user" && m.content === "risk-denied"),
    )
    .at(-1);
  assert(
    !denied.messages
      .filter((m) => m.role === "tool")
      .at(-1)
      ?.content.includes(evidence),
  );
  turn = send(id, "risk-cancelled");
  p = await pending(id);
  await api("/api/chat/cancel", "POST", { session_id: id });
  await turn;
  const expired = await api("/api/approvals", "POST", {
    approval_id: p.approval_id,
    approved: true,
  });
  assert(expired.expired);
  const deniedAgent = {
    id: "deny-reader",
    name: "Denied reader",
    tool_policy: {
      deny: ["read_file"],
      risk_class_overrides: { read_file: "high" },
    },
  };
  await api("/api/agents", "POST", deniedAgent);
  const deniedChat = (
    await api("/api/sessions", "POST", { persona_id: deniedAgent.id })
  ).session_id;
  await send(deniedChat, "risk-forged-denied");
  assert(!(await api("/api/sessions/" + deniedChat)).pending_approval);
  const forged = requests
    .filter((r) =>
      r.messages.some(
        (m) => m.role === "user" && m.content === "risk-forged-denied",
      ),
    )
    .at(-1);
  assert(!forged.tools.some((t) => t.function.name === "read_file"));
  assert(
    !forged.messages
      .filter((m) => m.role === "tool")
      .at(-1)
      ?.content.includes(evidence),
  );
  // Agent-created worker inherits risk even when its own override requests safe.
  await send(id, "delegate-risk");
  p = await pending(id);
  assert.equal(p.action, "read_file");
  assert(p.profile_scoped);
  assert(p.detail.includes("Specialist policy"));
  const db = new DatabaseSync(path.join(state, "native-chats.sqlite"), {
    readOnly: true,
  });
  const childId = db
    .prepare("SELECT child_chat FROM ui_jobs WHERE parent_chat=?")
    .get(id).child_chat;
  const saved = JSON.parse(
    db
      .prepare(
        "SELECT spec FROM ui_run_specs WHERE chat_id=? AND request_id='job'",
      )
      .get(childId).spec,
  );
  assert.equal(
    saved.persona._inherited_tool_policies[0].risk_class_overrides.read_file,
    "high",
  );
  db.close();
  // Kill while the child awaits approval. A late decision after recovery cannot dispatch input.
  child.kill("SIGKILL");
  await new Promise((r) => child.once("exit", r));
  await start();
  const late = await api("/api/approvals", "POST", {
    approval_id: p.approval_id,
    approved: true,
  });
  assert(late.expired);
  await pause(150);
  const policy = new DatabaseSync(
    path.join(state, "components", "policy.sqlite"),
    { readOnly: true },
  );
  const row = JSON.parse(
    policy.prepare("SELECT body FROM approvals WHERE id=?").get(p.approval_id)
      .body,
  );
  assert.equal(row.state, "interrupted");
  policy.close();
  console.log(
    JSON.stringify({
      passed: true,
      python_backend: false,
      imported_risk_enforced: true,
      full_access_not_bypass: true,
      one_use: true,
      denial_blocks: true,
      cancellation_blocks: true,
      forged_denied_tool_blocks: true,
      delegation_inherits: true,
      restart_no_replay: true,
      fixture: root,
    }),
  );
} finally {
  await stop();
  model.closeAllConnections();
  await new Promise((r) => model.close(r));
}
