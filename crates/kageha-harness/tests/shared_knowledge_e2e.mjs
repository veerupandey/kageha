import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import http from "node:http";
import { spawn } from "node:child_process";
import { DatabaseSync } from "node:sqlite";
import { credentialFixture } from "../../kageha-loop/tests/credential_fixture.mjs";
import { isolatedModelFixture } from "../../kageha-loop/tests/isolated_model_fixture.mjs";
const root = fs.mkdtempSync(path.join(os.tmpdir(), "kageha-shared-knowledge-"));
const bin = path.resolve(
  process.env.CARGO_TARGET_DIR || "target/migration-acceptance",
  "debug/kageha-loop",
);
const state = path.join(root, "state"),
  workspace = path.join(root, "workspace"),
  empty = path.join(root, "empty");
fs.mkdirSync(workspace);
fs.mkdirSync(empty);
let records = [],
  releaseStale,
  staleSeen,
  child,
  base;
const seen = new Promise((r) => (staleSeen = r));
const model = http.createServer(async (req, res) => {
  let text = "";
  for await (const b of req) text += b;
  const request = JSON.parse(text);
  records.push(request);
  const intent =
    request.messages.filter((m) => m.role === "user").at(-1)?.content || "";
  let message = { role: "assistant", content: "Fixture completed" };
  if (request.messages.at(-1)?.role !== "tool") {
    if (intent === "stale-live") {
      staleSeen();
      await new Promise((r) => (releaseStale = r));
    }
    const delegate = intent === "delegate-notes";
    const list = intent === "list-notes";
    const read = intent === "read-notes";
    const name = delegate
      ? "agent_spawn"
      : list
        ? "knowledge_list"
        : read
          ? "read_file"
          : "knowledge_search";
    const args = delegate
      ? { agent_id: "worker", objective: "worker-query" }
      : list
        ? {}
        : read
          ? { path: "knowledge-bases/notes/sources/runbook.md" }
          : {
              query: "violet checkpoint",
              ...(intent === "filter-secret" ? { kb_id: "secret" } : {}),
            };
    message = {
      role: "assistant",
      content: null,
      tool_calls: [
        {
          id: "call-" + intent,
          type: "function",
          function: { name, arguments: JSON.stringify(args) },
        },
      ],
    };
  }
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
async function upload(id, name, text) {
  const form = new FormData();
  form.append("file", new Blob([text]), name);
  const response = await fetch(
    base + "/api/knowledge-bases/" + id + "/upload",
    {
      method: "POST",
      headers: { Authorization: "Bearer " + token },
      body: form,
    },
  );
  assert.equal(response.status, 200, await response.text());
}
async function session(persona_id = "main") {
  return (await api("/api/sessions", "POST", { persona_id })).session_id;
}
async function turn(id, intent, knowledge_bases = []) {
  return api("/api/chat/send", "POST", {
    session_id: id,
    client_request_id: intent,
    message: intent,
    knowledge_bases,
  });
}
function result(intent) {
  const request = records
    .filter((r) =>
      r.messages.some((m) => m.role === "user" && m.content === intent),
    )
    .at(-1);
  return (
    request.messages.filter((m) => m.role === "tool").at(-1)?.content || ""
  );
}
try {
  await start();
  await api(
    "/api/knowledge-bases",
    "POST",
    { id: "notes", chunking: "hierarchical" },
    201,
  );
  await api("/api/knowledge-bases", "POST", { id: "secret" }, 201);
  await api("/api/knowledge-bases", "POST", { id: "agent-private" }, 400);
  await upload(
    "notes",
    "runbook.md",
    "# Recovery\nUse the violet checkpoint.\n## Ownership\nOperations owns rollback.",
  );
  await upload(
    "secret",
    "private.txt",
    "UNATTACHED_PRIVATE_WORKSPACE_DATA violet checkpoint",
  );
  const unsupported = new FormData();
  unsupported.append("file", new Blob(["%PDF fixture"]), "unsupported.pdf");
  assert.equal(
    (
      await fetch(base + "/api/knowledge-bases/notes/upload", {
        method: "POST",
        headers: { Authorization: "Bearer " + token },
        body: unsupported,
      })
    ).status,
    415,
  );
  const list = await api("/api/knowledge-bases");
  assert.equal(list.items.length, 2);
  assert(list.items.every((b) => b.engine === "rust-lexical"));
  const source = await api("/api/knowledge-bases/notes/sources/runbook.md");
  await api(
    "/api/knowledge-bases/notes/sources/runbook.md",
    "PUT",
    { content: "wrong", expected_revision: 0 },
    409,
  );
  const chunks = await api("/api/knowledge-bases/notes/chunks?limit=1");
  assert.equal(chunks.items.length, 1);
  assert.equal(chunks.total, 2);
  assert(chunks.next_cursor);
  const page2 = await api(
    "/api/knowledge-bases/notes/chunks?limit=1&cursor=" + chunks.next_cursor,
  );
  assert.equal(page2.items[0].heading, "Recovery / Ownership");
  assert.equal(page2.next_cursor, null);
  assert(
    (await api("/api/knowledge-bases/notes/search?q=violet")).items[0]
      .sha256 === source.sha256,
  );
  assert(
    (
      await api("/api/knowledge-bases/notes/query", "POST", {
        query: "violet checkpoint",
      })
    ).answer.includes("violet"),
  );
  await api("/api/knowledge-bases/notes/sources/runbook.md", "PUT", {
    content:
      "# Recovery\nUse the violet checkpoint, updated.\n## Ownership\nOperations owns rollback.",
    expected_revision: source.revision,
  });
  await api(
    "/api/knowledge-bases/notes/chunks?limit=1&cursor=1&snapshot=" +
      encodeURIComponent(chunks.snapshot),
    "GET",
    undefined,
    409,
  );
  await api("/api/agents", "POST", {
    id: "extra",
    name: "Extra reader",
    knowledge: { inherit_workspace: true, extra_kb_ids: ["notes"] },
  });
  await api("/api/agents", "POST", {
    id: "private",
    name: "Private reader",
    knowledge: { inherit_workspace: false, extra_kb_ids: ["notes"] },
  });
  await api("/api/agents", "POST", {
    id: "worker",
    name: "Worker",
    knowledge: { inherit_workspace: true, extra_kb_ids: ["secret"] },
  });
  const main = await session();
  await turn(main, "query-notes", ["notes"]);
  assert(result("query-notes").includes("violet"));
  assert(!result("query-notes").includes("UNATTACHED_PRIVATE_WORKSPACE_DATA"));
  const count = records.length;
  await turn(main, "query-notes", ["notes"]);
  assert.equal(records.length, count);
  await api(
    "/api/chat/send",
    "POST",
    {
      session_id: main,
      client_request_id: "query-notes",
      message: "query-notes",
      knowledge_bases: ["secret"],
    },
    409,
  );
  await turn(main, "filter-secret", ["notes"]);
  assert(
    !result("filter-secret").includes("UNATTACHED_PRIVATE_WORKSPACE_DATA"),
  );
  assert(result("filter-secret").includes("outside"));
  await turn(main, "read-notes", ["notes"]);
  assert(result("read-notes").includes("violet"));
  await turn(main, "list-notes", ["notes"]);
  assert(result("list-notes").includes("notes"));
  assert(!result("list-notes").includes("secret"));
  const extra = await session("extra");
  await turn(extra, "extra-query");
  assert(result("extra-query").includes("violet"));
  const privateChat = await session("private");
  await turn(privateChat, "private-query", ["notes"]);
  assert.equal(JSON.parse(result("private-query")).hits.length, 0);
  await api(
    "/api/chat/send",
    "POST",
    {
      session_id: main,
      client_request_id: "forged-private",
      message: "forged-private",
      knowledge_bases: ["agent-private"],
    },
    400,
  );
  await turn(main, "delegate-notes", ["notes"]);
  const job = await until(() => {
    const db = new DatabaseSync(path.join(state, "native-chats.sqlite"), {
      readOnly: true,
    });
    const row = db
      .prepare("SELECT id,child_chat,state FROM ui_jobs WHERE parent_chat=?")
      .get(main);
    db.close();
    return row;
  });
  await until(async () =>
    ["succeeded", "failed"].includes(
      (await api("/api/jobs/" + job.id)).job.state,
    ),
  );
  assert(result("worker-query").includes("violet"));
  assert(!result("worker-query").includes("UNATTACHED_PRIVATE_WORKSPACE_DATA"));
  const db = new DatabaseSync(path.join(state, "native-chats.sqlite"), {
    readOnly: true,
  });
  const spec = JSON.parse(
    db
      .prepare(
        "SELECT spec FROM ui_run_specs WHERE chat_id=? AND request_id='job'",
      )
      .get(job.child_chat).spec,
  );
  db.close();
  assert.deepEqual(
    spec.knowledge_scope.shared.map((b) => b.id),
    ["notes"],
  );
  const stale = turn(main, "stale-live", ["notes"]);
  await seen;
  await api("/api/knowledge-bases/notes", "DELETE");
  await api("/api/knowledge-bases", "POST", { id: "notes" }, 201);
  await upload(
    "notes",
    "runbook.md",
    "NEW_GENERATION_MUST_NOT_LEAK violet checkpoint",
  );
  releaseStale();
  await stale;
  assert(!result("stale-live").includes("NEW_GENERATION_MUST_NOT_LEAK"));
  assert(/changed|unavailable/i.test(result("stale-live")));
  await turn(main, "new-generation", ["notes"]);
  assert(result("new-generation").includes("NEW_GENERATION_MUST_NOT_LEAK"));
  await stop();
  await start();
  assert.equal((await api("/api/knowledge-bases/notes")).generation, 2);
  assert(
    (
      await api("/api/knowledge-bases/notes/sources/runbook.md")
    ).content.includes("NEW_GENERATION"),
  );
  await api("/api/knowledge-bases/notes/sources/runbook.md", "DELETE");
  assert.equal(
    (await api("/api/knowledge-bases/notes/search?q=violet")).items.length,
    0,
  );
  console.log(
    JSON.stringify({
      passed: true,
      python_backend: false,
      shared_ui_crud: true,
      snapshot_paging: true,
      unsupported_ingest_rejected: true,
      workspace_pins: true,
      extra_kb_inheritance: true,
      private_mode: true,
      worker_scope_narrowing: true,
      immutable_retry_scope: true,
      generation_fencing: true,
      restart_and_deletions: true,
      owned_component_store: true,
      fixture: root,
    }),
  );
} finally {
  releaseStale?.();
  await stop();
  model.closeAllConnections();
  await new Promise((r) => model.close(r));
}
