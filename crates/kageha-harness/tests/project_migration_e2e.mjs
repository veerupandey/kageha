import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import http from "node:http";
import { spawn, spawnSync } from "node:child_process";
import { DatabaseSync } from "node:sqlite";
import { credentialFixture } from "../../kageha-loop/tests/credential_fixture.mjs";
import { isolatedModelFixture } from "../../kageha-loop/tests/isolated_model_fixture.mjs";
const root = fs.mkdtempSync(path.join(os.tmpdir(), "kageha-project-import-"));
const bin = path.resolve(
  process.env.CARGO_TARGET_DIR || "target/migration-acceptance",
  "debug/kageha-loop",
);
const state = path.join(root, "state"),
  workspace = path.join(root, "workspace"),
  empty = path.join(root, "empty");
fs.mkdirSync(workspace);
fs.mkdirSync(empty);
let child, base;
const requests = [];
const model = http.createServer(async (req, res) => {
  let text = "";
  for await (const b of req) text += b;
  const request = JSON.parse(text);
  requests.push(request);
  const intent =
    request.messages.filter((m) => m.role === "user").at(-1)?.content || "";
  const message = { role: "assistant", content: "Project fixture answered" };
  res.end(JSON.stringify({ choices: [{ message }] }));
});
await new Promise((r) => model.listen(0, "127.0.0.1", r));
const token = "shared-knowledge-fixture-access-token";
const env = {
  PATH: empty,
  HOME: root,
  ...credentialFixture(root),
  ...isolatedModelFixture(root),
  KAGEHA_SERVER_TOKEN: token,
  KAGEHA_LEGACY_SESSIONS_DIR: empty,
  KAGEHA_PROVIDER_URL: `http://127.0.0.1:${model.address().port}/v1/chat/completions`,
  KAGEHA_MODEL: "fixture",
};
async function until(check) {
  for (let end = Date.now() + 15000; Date.now() < end; ) {
    const value = await check();
    if (value) return value;
    await new Promise((r) => setTimeout(r, 25));
  }
  throw Error("Acceptance timed out");
}
async function start() {
  let log = "";
  child = spawn(bin, ["serve", workspace, state, "127.0.0.1:0"], {
    env,
    cwd: root,
    stdio: ["ignore", "pipe", "pipe"],
  });
  child.stdout.on("data", (b) => (log += b));
  child.stderr.on("data", (b) => (log += b));
  await until(() => {
    if (child.exitCode !== null) throw Error(log);
    const m = log.match(/listening on (127\.0\.0\.1:\d+)/);
    if (m) {
      base = "http://" + m[1];
      return true;
    }
  });
}
async function stop() {
  if (child && child.exitCode === null && child.signalCode === null) {
    const exited = new Promise((r) => child.once("exit", r));
    child.kill("SIGTERM");
    await exited;
  }
}
async function api(route, method = "GET", body, status = 200) {
  const response = await fetch(base + route, {
    method,
    headers: {
      Authorization: "Bearer " + token,
      "Content-Type": "application/json",
    },
    body: body === undefined ? undefined : JSON.stringify(body),
    signal: AbortSignal.timeout(20000),
  });
  const text = await response.text();
  assert.equal(response.status, status, text);
  return text ? JSON.parse(text) : null;
}

const source = path.join(root, "legacy-home"),
  chat = "historical-chat";
fs.mkdirSync(path.join(source, "sessions", chat), { recursive: true });
const project = {
  id: "historical-id",
  name: "Original Name",
  description: "Historical evidence",
  color: "emerald",
  created_at: 12,
  updated_at: 34,
  tags: ["original"],
  unknown: { preserve: true },
};
fs.writeFileSync(
  path.join(source, "projects.json"),
  JSON.stringify({ [project.id]: project }),
);
fs.writeFileSync(
  path.join(source, "sessions", chat, "chat.jsonl"),
  [
    JSON.stringify({ role: "user", text: "Historic request", turn_id: "old" }),
    JSON.stringify({
      role: "assistant",
      text: "Historic answer",
      turn_id: "old",
    }),
  ].join("\n") + "\n",
);
fs.writeFileSync(
  path.join(source, "sessions", chat, "meta.json"),
  JSON.stringify({ title: "Historical chat", created_at: 12, updated_at: 34 }),
);
fs.writeFileSync(
  path.join(source, "sessions", chat, "session.json"),
  JSON.stringify({
    project_id: project.id,
    project,
    tagged_turns: {
      0: {
        session_id: chat,
        turn_index: 0,
        project_id: project.id,
        project_name: project.name,
        tag: "original evidence",
      },
    },
  }),
);
const original = fs.readFileSync(path.join(source, "projects.json"), "utf8");
function migrate(domain = "projects", success = true) {
  const from = domain === "sessions" ? path.join(source, "sessions") : source;
  const result = spawnSync(
    path.join(path.dirname(bin), "kageha"),
    ["migrate", "--legacy-" + domain, from, "--state-dir", state],
    { env, cwd: root, encoding: "utf8", timeout: 15000 },
  );
  if (success) {
    assert.equal(result.status, 0, result.stderr);
    return JSON.parse(result.stdout).report;
  }
  assert.notEqual(result.status, 0);
  return result.stderr;
}
function prepare() {
  const db = new DatabaseSync(
    path.join(state, "document-import-projects-v1/migration.sqlite"),
  );
  db.exec("UPDATE history_import_v1 SET state='prepared'");
  db.close();
}
try {
  assert(migrate("projects", false).includes("Import chat history"));
  assert.equal(migrate("sessions").imported, 1);
  assert.equal(migrate().imported, 1);
  assert.equal(migrate().already_present, 1);
  prepare();
  assert.equal(migrate().imported, 1);
  await start();
  let history = await api("/api/sessions/" + chat);
  assert.equal(history.project_id, project.id);
  assert.equal(history.project.created_at, 12);
  assert.equal(history.tagged_turns["0"].tag, "original evidence");
  assert(history.messages.some((m) => m.text === "Historic answer"));
  assert.equal(
    (await api("/api/projects")).projects[0].tagged_message_count,
    1,
  );
  await api("/api/sessions/" + chat + "/project", "POST", {
    project: "Native Choice",
  });
  await api("/api/sessions/" + chat + "/messages/0/tag", "POST", {
    project: "",
  });
  await stop();
  prepare();
  migrate();
  await start();
  history = await api("/api/sessions/" + chat);
  assert.equal(history.project_id, "native-choice");
  assert.equal(Object.keys(history.tagged_turns).length, 0);
  await api("/api/sessions/" + chat, "DELETE");
  await stop();
  prepare();
  migrate();
  await start();
  await api("/api/sessions/" + chat, "GET", undefined, 404);
  await stop();
  assert.equal(
    fs.readFileSync(path.join(source, "projects.json"), "utf8"),
    original,
  );
  fs.writeFileSync(
    path.join(source, "projects.json"),
    JSON.stringify({ [project.id]: { ...project, name: "Changed source" } }),
  );
  assert(migrate("projects", false).toLowerCase().includes("changed"));
  fs.writeFileSync(path.join(source, "projects.json"), original);
  const storePath = path.join(state, "components/workspace/projects.sqlite");
  const db = new DatabaseSync(storePath);
  const snapshot = JSON.parse(
    db.prepare("SELECT snapshot FROM project_imports").get().snapshot,
  );
  assert.equal(snapshot.original.catalog[project.id].unknown.preserve, true);
  db.exec(
    "UPDATE project_history SET body='{}' WHERE id='historical-id' AND revision=1",
  );
  db.close();
  assert(migrate("projects", false).includes("history changed"));
  fs.renameSync(source, source + "-hidden");
  await start();
  assert(
    (await api("/api/projects")).projects.some((p) => p.id === project.id),
  );
  await api("/api/sessions/" + chat, "GET", undefined, 404);
  console.log(
    JSON.stringify({
      passed: true,
      python_backend: false,
      historical_project_import: true,
      project_ids_and_tags: true,
      history_preserved: true,
      lost_ack_recovery: true,
      native_edits_preserved: true,
      deleted_chat_not_resurrected: true,
      source_independent: true,
      source_changes_and_tampering_rejected: true,
    }),
  );
} finally {
  await stop();
  model.closeAllConnections();
  await new Promise((r) => model.close(r));
}
