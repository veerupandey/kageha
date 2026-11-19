import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import http from "node:http";
import { spawn, spawnSync } from "node:child_process";
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

const source = path.join(root, "legacy-knowledge"),
  kb = path.join(source, "runbooks");
fs.mkdirSync(path.join(kb, "raw"), { recursive: true });
fs.writeFileSync(
  path.join(kb, "manifest.yaml"),
  JSON.stringify({
    id: "runbooks",
    engine: "zvec",
    chunking: "hierarchical",
    sources: ["manual.pdf", "notes.md"],
  }),
);
fs.writeFileSync(path.join(kb, "raw", "manual.pdf"), "%PDF retained original");
fs.writeFileSync(path.join(kb, "raw", "notes.md"), "Legacy text source");
fs.writeFileSync(
  path.join(kb, "chunks.jsonl"),
  [
    {
      id: "pdf-17",
      source: "/old/machine/manual.pdf",
      text: "The violet checkpoint is the retained PDF rollback procedure",
      heading: "Recovery",
      parent_id: "pdf-parent",
    },
    {
      id: "md-18",
      source: "notes.md",
      text: "The violet checkpoint has written operator approval",
      heading: "Notes",
    },
    {
      id: "orphan",
      source: "removed.txt",
      text: "REMOVED_KNOWLEDGE_MUST_NOT_RETURN",
    },
  ]
    .map(JSON.stringify)
    .join("\n"),
);
fs.mkdirSync(path.join(source, "agent-private", "raw"), { recursive: true });
fs.writeFileSync(
  path.join(source, "agent-private", "manifest.yaml"),
  JSON.stringify({
    id: "agent-private",
    scope: "persona",
    persona_id: "private",
    sources: [],
  }),
);
function migrate(success = true) {
  const r = spawnSync(
    path.join(path.dirname(bin), "kageha"),
    [
      "migrate",
      "--legacy-workspace-knowledge",
      source,
      "--workspace",
      workspace,
      "--state-dir",
      state,
    ],
    { env, cwd: root, encoding: "utf8", timeout: 15000 },
  );
  if (success) {
    assert.equal(r.status, 0, r.stderr);
    return JSON.parse(r.stdout).report;
  }
  assert.notEqual(r.status, 0);
}
try {
  assert.equal(migrate().imported, 1);
  assert.equal(migrate().already_present, 1);
  const journalName = fs
    .readdirSync(state)
    .find((n) => n.startsWith("document-import-workspace-knowledge-v1-"));
  assert(journalName);
  const journal = new DatabaseSync(
    path.join(state, journalName, "migration.sqlite"),
  );
  journal.exec("UPDATE history_import_v1 SET state='prepared'");
  journal.close();
  assert.equal(migrate().imported, 1);
  await start();
  let list = await api("/api/knowledge-bases");
  assert.deepEqual(
    list.items.map((b) => b.id),
    ["runbooks"],
  );
  assert.equal(list.items[0].stats.chunks, 2);
  let hits = (await api("/api/knowledge-bases/runbooks/search?q=violet")).items;
  assert(hits.some((h) => h.id === "pdf-17"));
  assert(!JSON.stringify(hits).includes("REMOVED_KNOWLEDGE"));
  let chunks = (await api("/api/knowledge-bases/runbooks/chunks")).items;
  assert(chunks.some((c) => c.id === "pdf-17" && c.parent_id === "pdf-parent"));
  const id = await session();
  await turn(id, "migrated-search", ["runbooks"]);
  assert(result("migrated-search").includes("pdf-17"));
  await api("/api/knowledge-bases/runbooks/sources/notes.md", "PUT", {
    content: "New native operator edit",
  });
  await api("/api/knowledge-bases/runbooks/sources/manual.pdf", "DELETE");
  await upload("runbooks", "new.txt", "Native added source");
  await stop();
  assert.equal(migrate().already_present, 1);
  await start();
  assert(
    (
      await api("/api/knowledge-bases/runbooks/sources/notes.md")
    ).content.includes("New native"),
  );
  await api(
    "/api/knowledge-bases/runbooks/sources/manual.pdf",
    "GET",
    undefined,
    404,
  );
  assert(
    (await api("/api/knowledge-bases/runbooks")).sources.includes("new.txt"),
  );
  fs.renameSync(source, source + "-hidden");
  assert(
    (await api("/api/knowledge-bases/runbooks/search?q=native")).items.length >
      0,
  );
  fs.renameSync(source + "-hidden", source);
  await stop();
  fs.writeFileSync(path.join(kb, "raw", "notes.md"), "Changed original source");
  migrate(false);
  fs.writeFileSync(path.join(kb, "raw", "notes.md"), "Legacy text source");
  await start();
  await api("/api/knowledge-bases/runbooks", "DELETE");
  await stop();
  migrate();
  await start();
  await api("/api/knowledge-bases/runbooks", "GET", undefined, 404);
  await stop();
  const db = new DatabaseSync(
    path.join(state, "components", "knowledge", "knowledge.sqlite"),
  );
  db.exec(
    "UPDATE source_history SET body=X'626164' WHERE kb='runbooks' AND name='manual.pdf'",
  );
  db.close();
  migrate(false);
  console.log(
    JSON.stringify({
      passed: true,
      python_backend: false,
      workspace_knowledge_import: true,
      original_chunk_ids: true,
      binary_extraction_retained: true,
      lost_ack_recovery: true,
      native_edits_preserved: true,
      deleted_base_not_resurrected: true,
      private_stores_not_shared: true,
      source_independent: true,
      changed_source_and_tamper_rejected: true,
      fixture: root,
    }),
  );
} finally {
  releaseStale?.();
  await stop();
  model.closeAllConnections();
  await new Promise((r) => model.close(r));
}
