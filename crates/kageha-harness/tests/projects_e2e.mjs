import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import http from "node:http";
import { spawn } from "node:child_process";
import { DatabaseSync } from "node:sqlite";
import { credentialFixture } from "../../kageha-loop/tests/credential_fixture.mjs";
import { isolatedModelFixture } from "../../kageha-loop/tests/isolated_model_fixture.mjs";
const root = fs.mkdtempSync(path.join(os.tmpdir(), "kageha-projects-"));
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

try {
  await start();
  let p = (
    await api("/api/projects", "POST", {
      name: "Bare & Fair",
      description: "Project evidence",
      color: "emerald",
      expected_revision: 0,
    })
  ).project;
  assert.equal(p.id, "bare-fair");
  assert.equal(p.revision, 1);
  const chat = (await api("/api/sessions", "POST", { project: p.id }))
    .session_id;
  assert.equal((await api("/api/sessions/" + chat)).project_id, p.id);
  assert.equal(
    (await api("/api/projects?q=evidence")).projects[0].session_count,
    1,
  );
  await api(
    "/api/projects",
    "POST",
    { name: "Bare & Fair", description: "Stale write", expected_revision: 0 },
    409,
  );
  await api("/api/chat/send", "POST", {
    session_id: chat,
    client_request_id: "answer",
    message: "Retain this evidence",
  });
  await api("/api/sessions/" + chat + "/messages/0/tag", "POST", {
    project: p.id,
    tag: "reviewed",
  });
  let state = await api("/api/sessions/" + chat);
  assert.equal(state.tagged_turns["0"].tag, "reviewed");
  assert.equal(
    (await api("/api/projects")).projects[0].tagged_message_count,
    1,
  );
  await api(
    "/api/sessions/missing/project",
    "POST",
    { project: "Must not be created" },
    404,
  );
  await api(
    "/api/sessions/" + chat + "/messages/99/tag",
    "POST",
    { project: p.id },
    404,
  );
  await api("/api/sessions/" + chat + "/project", "POST", {
    project: "Kageha",
  });
  assert.equal((await api("/api/sessions/" + chat)).project.name, "Kageha");
  await stop();
  await start();
  state = await api("/api/sessions/" + chat);
  assert.equal(state.project_id, "kageha");
  assert.equal(state.tagged_turns["0"].tag, "reviewed");
  assert(state.messages.some((m) => m.text === "Project fixture answered"));
  const sessions = await api("/api/sessions");
  assert.equal(
    sessions.sessions.find((s) => s.session_id === chat).project_id,
    "kageha",
  );
  await api("/api/sessions/" + chat + "/project", "POST", { project: null });
  assert.equal((await api("/api/sessions/" + chat)).project_id, null);
  await api("/api/sessions/" + chat + "/messages/0/tag", "POST", {
    project: "",
  });
  assert.equal(
    (await api("/api/projects?q=bare")).projects[0].tagged_message_count,
    0,
  );
  await api("/api/sessions/" + chat + "/project", "POST", {
    project: "kageha",
  });
  await api("/api/sessions/" + chat, "DELETE");
  assert.equal(
    (await api("/api/projects?q=kageha")).projects[0].session_count,
    0,
  );
  console.log(
    JSON.stringify({
      passed: true,
      python_backend: false,
      project_crud: true,
      chat_assignment: true,
      message_tags: true,
      counts: true,
      revision_conflicts: true,
      unknown_chat_isolation: true,
      restart_history_preserved: true,
    }),
  );
} finally {
  await stop();
  model.closeAllConnections();
  await new Promise((r) => model.close(r));
}
