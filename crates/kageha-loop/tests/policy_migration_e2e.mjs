import { credentialFixture } from "./credential_fixture.mjs";
import { isolatedModelFixture } from "./isolated_model_fixture.mjs";
// Preserve legacy approval IDs, migrate mode once, and recover a pending WebUI decision.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import http from "node:http";
import { spawn } from "node:child_process";
import { DatabaseSync } from "node:sqlite";
const binary =
  process.env.KAGEHA_LOOP_BIN || path.resolve("target/debug/kageha-loop");
const root = fs.mkdtempSync("/tmp/kageha-ui-policy-");
const token = "ui-policy-fixture-authentication-token";
const pause = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(fn) {
  for (let end = Date.now() + 15000; Date.now() < end; ) {
    const value = await fn();
    if (value) return value;
    await pause(25);
  }
  throw Error("Condition timed out");
}
let child,
  base,
  calls = 0;
const children = [],
  logs = [];
const model = http.createServer(async (req, res) => {
  let bytes = "";
  for await (const part of req) bytes += part;
  JSON.parse(bytes);
  calls++;
  res.end(
    JSON.stringify({
      choices: [
        {
          message: {
            role: "assistant",
            content: JSON.stringify({
              objective: "Review fixture",
              steps: [
                {
                  id: "read",
                  title: "Read notes",
                  depends_on: [],
                  acceptance_criteria: ["Read verified"],
                },
              ],
            }),
          },
        },
      ],
    }),
  );
});
await new Promise((r) => model.listen(0, "127.0.0.1", r));
async function start() {
  const log = path.join(root, `server-${logs.length}.log`);
  logs.push(log);
  child = spawn(
    binary,
    [
      "serve",
      path.join(root, "workspace"),
      path.join(root, "state"),
      "127.0.0.1:0",
    ],
    {
      env: {
        PATH: path.join(root, "empty"),
        ...credentialFixture(root),
        ...isolatedModelFixture(root),
        KAGEHA_SERVER_TOKEN: token,
        KAGEHA_PROVIDER_URL: `http://127.0.0.1:${model.address().port}/model`,
        KAGEHA_MODEL: "fixture",
      },
      stdio: ["ignore", fs.openSync(log, "a"), fs.openSync(log, "a")],
    },
  );
  children.push(child);
  base = await until(() => {
    if (child.exitCode !== null) throw Error(fs.readFileSync(log, "utf8"));
    const m = fs.readFileSync(log, "utf8").match(/listening on (\S+)/);
    return m && "http://" + m[1];
  });
}
async function api(route, body) {
  const response = await fetch(base + route, {
    method: body ? "POST" : "GET",
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
    },
    body: body ? JSON.stringify(body) : undefined,
    signal: AbortSignal.timeout(15000),
  });
  const data = await response.json();
  assert.equal(response.status, 200, JSON.stringify(data));
  return data;
}
async function stop(c) {
  if (c && c.exitCode === null && c.signalCode === null) {
    c.kill("SIGKILL");
    await new Promise((r) => c.once("exit", r));
  }
}
try {
  fs.mkdirSync(path.join(root, "workspace"));
  fs.mkdirSync(path.join(root, "state"));
  fs.mkdirSync(path.join(root, "empty"));
  let native = new DatabaseSync(path.join(root, "state/native-chats.sqlite"));
  native.exec(
    "CREATE TABLE ui_approvals(id TEXT PRIMARY KEY,chat_id TEXT NOT NULL,turn_id TEXT NOT NULL,action TEXT NOT NULL,detail TEXT NOT NULL,context TEXT NOT NULL,decision TEXT NOT NULL);CREATE TABLE ui_permissions(id INTEGER PRIMARY KEY CHECK(id=1),scope TEXT NOT NULL);INSERT INTO ui_permissions VALUES(1,'full');INSERT INTO ui_approvals VALUES ('old-approved','old-chat','old-turn','browser_upload','Old exact upload','{}','approved'),('old-waiting','old-chat','old-turn','browser_click','Old click','{}','pending');",
  );
  native.close();
  await start();
  assert.equal((await api("/api/permissions")).scope, "full");
  assert.equal(
    (await api("/api/permissions?session_id=old-chat")).legacy_routine,
    true,
  );
  let policy = new DatabaseSync(
    path.join(root, "state/components/policy.sqlite"),
  );
  const record = (id) =>
    JSON.parse(
      policy.prepare("SELECT body FROM approvals WHERE id=?").get(id).body,
    );
  assert.equal(record("old-approved").state, "consumed");
  assert.equal(record("old-waiting").state, "interrupted");
  policy.close();
  await api("/api/permissions", { mode: "ask" });
  const chat = (await api("/api/sessions", {})).session_id;
  await api("/api/permissions", { session_id: chat, profile: "ask" });
  assert.equal(
    (await api(`/api/permissions?session_id=${chat}`)).legacy_routine,
    false,
  );
  let pending = api("/api/chat/send", {
    session_id: chat,
    client_request_id: "first",
    message: "Review fixture",
    agent_mode: "plan",
  }).catch(() => null);
  const approval = await until(
    async () => (await api(`/api/sessions/${chat}`)).pending_approval,
  );
  assert.equal(approval.action, "approve_plan");
  assert.equal(approval.profile_scoped, true);
  assert.equal(calls, 1);
  await stop(child);
  await pending;
  await start();
  assert.equal(
    (await api("/api/permissions")).scope,
    "ask",
    "Stale source full permission must not be reimported",
  );
  const restored = await api(`/api/sessions/${chat}`);
  assert.equal(restored.pending_approval, null);
  assert.equal(restored.status, "interrupted");
  assert.equal(calls, 1, "Restart cannot replay the planner or a held action");
  const late = await api("/api/approvals", {
    approval_id: approval.approval_id,
    approved: true,
  });
  assert.equal(late.approved, false);
  assert.equal(late.expired, true);
  policy = new DatabaseSync(path.join(root, "state/components/policy.sqlite"));
  assert.equal(record(approval.approval_id).state, "interrupted");
  policy.close();
  native = new DatabaseSync(path.join(root, "state/native-chats.sqlite"));
  assert.equal(
    native.prepare("SELECT count(*) AS n FROM ui_approvals").get().n,
    2,
    "Runtime approval writes belong only to the component",
  );
  assert.equal(
    native
      .prepare("SELECT decision FROM ui_approvals WHERE id='old-waiting'")
      .get().decision,
    "pending",
    "Migration preserves source data",
  );
  native.close();
  pending = api("/api/chat/send", {
    session_id: chat,
    client_request_id: "second",
    message: "Review again",
    agent_mode: "plan",
  });
  const next = await until(
    async () => (await api(`/api/sessions/${chat}`)).pending_approval,
  );
  await api("/api/approvals", {
    approval_id: next.approval_id,
    approved: false,
  });
  await pending;
  assert.equal(calls, 2);
  assert.equal((await api(`/api/sessions/${chat}`)).pending_approval, null);
  console.log(
    JSON.stringify({
      passed: true,
      python_backend: false,
      existing_chats: true,
      checks: [
        "legacy IDs and source preserved",
        "approved source inputs not reusable",
        "permissions imported once",
        "WebUI pending approval",
        "restart interruption",
        "late decision rejected",
        "new decisions only in policy component",
        "existing API denial",
      ],
      fixture: root,
    }),
  );
} catch (error) {
  console.error(error);
  for (const log of logs) console.error(fs.readFileSync(log, "utf8"));
  process.exitCode = 1;
} finally {
  for (const c of children) await stop(c);
  model.closeAllConnections();
  await new Promise((r) => model.close(r));
}
