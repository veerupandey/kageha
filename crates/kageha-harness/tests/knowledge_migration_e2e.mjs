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
  path.join(os.tmpdir(), "kageha-knowledge-migration-"),
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
    allow: ["read_file", "knowledge_search", "knowledge_list"],
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
  const message =
    request.messages.at(-1)?.role === "tool"
      ? {
          role: "assistant",
          content: "Retrieved and cited the private runbook.",
        }
      : {
          role: "assistant",
          content: null,
          tool_calls: [
            {
              id: "search-call",
              type: "function",
              function: {
                name: "knowledge_search",
                arguments: JSON.stringify({
                  query: "rollback violet checkpoint",
                }),
              },
            },
          ],
        };
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
const knowledge = path.join(root, "knowledge");
function kb(id, filename, text, chunks) {
  const directory = path.join(knowledge, "agent-" + id);
  fs.mkdirSync(path.join(directory, "raw"), { recursive: true });
  fs.writeFileSync(
    path.join(directory, "manifest.yaml"),
    JSON.stringify({
      id: "agent-" + id,
      scope: "persona",
      persona_id: id,
      engine: "zvec",
      sources: [filename],
    }),
  );
  fs.writeFileSync(path.join(directory, "raw", filename), text);
  if (chunks)
    fs.writeFileSync(
      path.join(directory, "chunks.jsonl"),
      chunks.map(JSON.stringify).join("\n"),
    );
  return directory;
}
const original =
  "Rollback requires the violet checkpoint. Deployment ownership stays with the operator.";
const own = kb(agent, "runbook.md", original, [
  {
    id: "source-chunk-17",
    source: "/old-machine/raw/runbook.md",
    text: original,
    heading: "Verified recovery",
    parent_id: "recovery-parent",
  },
  {
    id: "orphan-chunk",
    source: "deleted.txt",
    text: "REMOVED_CONTENT should never return",
  },
]);
fs.writeFileSync(
  path.join(own, "raw", "deleted.txt"),
  "REMOVED_CONTENT should never return",
);
const other = "another.agent";
fs.writeFileSync(
  path.join(agents, other + ".yaml"),
  JSON.stringify({ id: other, name: "Other agent" }),
);
kb(
  other,
  "secret.txt",
  "FOREIGN_PRIVATE_TOKEN violet checkpoint for another agent",
);
try {
  migrate("--legacy-agent-knowledge", knowledge, false);
  migrate("--legacy-agents", agents);
  assert.equal(migrate("--legacy-agent-knowledge", knowledge).imported, 2);
  // Simulate a lost acknowledgment after the host transaction committed.
  const journal = new DatabaseSync(
    path.join(state, "document-import-agent-knowledge-v1", "migration.sqlite"),
  );
  journal.exec("UPDATE history_import_v1 SET state='prepared'");
  journal.close();
  assert.equal(migrate("--legacy-agent-knowledge", knowledge).imported, 2);
  assert.equal(
    migrate("--legacy-agent-knowledge", knowledge).already_present,
    2,
  );
  await start();
  let list = await api("/api/agents/" + agent + "/knowledge");
  assert.equal(list.sources.length, 1);
  assert.equal(list.files[0].filename, "runbook.md");
  assert.equal(list.retrieval.semantic_search, false);
  const route = "/api/agents/" + agent + "/knowledge/search";
  let found = await api(route, "POST", { query: "rollback violet checkpoint" });
  assert.equal(found.hits[0].chunk_id, "source-chunk-17");
  assert.equal(found.hits[0].heading, "Verified recovery");
  assert.equal(found.hits[0].sha256, list.files[0].sha256);
  assert(found.hits[0].source.startsWith("knowledge/inputs/"));
  assert(!JSON.stringify(found).includes("FOREIGN_PRIVATE_TOKEN"));
  assert.equal(
    (await api(route, "POST", { query: "REMOVED_CONTENT" })).hits.length,
    0,
  );
  await api(route, "POST", { query: "violet", kb_id: "agent-" + other }, 400);
  await api(route, "POST", { query: "violet", search_mode: "semantic" }, 400);
  const session = (await api("/api/sessions", "POST", { persona_id: agent }))
    .session_id;
  await api("/api/chat/send", "POST", {
    session_id: session,
    client_request_id: "knowledge-turn",
    message: "What checkpoint does rollback require?",
  });
  assert(
    requests.some((r) =>
      r.messages.some(
        (m) => m.role === "tool" && m.content.includes("source-chunk-17"),
      ),
    ),
  );
  assert(!JSON.stringify(requests).includes("FOREIGN_PRIVATE_TOKEN"));
  // Original files remain available after the legacy tree is unavailable.
  fs.renameSync(knowledge, knowledge + "-hidden");
  found = await api(route, "POST", { query: "rollback" });
  assert.equal(found.hits[0].chunk_id, "source-chunk-17");
  fs.renameSync(knowledge + "-hidden", knowledge);
  // A new native upload participates in the same reusable retrieval path.
  const multipart = new FormData();
  multipart.append(
    "file",
    new Blob(["Quarterly budget is eight credits."]),
    "budget.txt",
  );
  const upload = await fetch(
    base + "/api/agents/" + agent + "/knowledge/upload",
    {
      method: "POST",
      headers: { Authorization: "Bearer " + token },
      body: multipart,
    },
  );
  assert.equal(upload.status, 200, await upload.text());
  found = await api(route, "POST", { query: "budget credits" });
  assert(found.hits.some((h) => h.text.includes("eight credits")));
  await stop();
  assert.equal(
    migrate("--legacy-agent-knowledge", knowledge).already_present,
    2,
  );
  await start();
  assert.equal(
    (await api("/api/agents/" + agent + "/knowledge")).sources.length,
    2,
  );
  found = await api(route, "POST", { query: "budget credits" });
  assert(found.hits.some((h) => h.text.includes("eight credits")));
  // Owned byte drift invalidates retrieval even when the stored chunk still matches.
  const db = new DatabaseSync(path.join(state, "native-chats.sqlite"), {
    readOnly: true,
  });
  const sourceFile = db
    .prepare("SELECT source FROM ui_legacy_files WHERE path=?")
    .get(list.files[0].path).source;
  db.close();
  const retained = fs.readFileSync(sourceFile);
  fs.writeFileSync(sourceFile, "tampered");
  await api(route, "POST", { query: "rollback" }, 400);
  fs.writeFileSync(sourceFile, retained);
  await stop();
  fs.writeFileSync(
    path.join(own, "raw", "runbook.md"),
    "Changed historical source",
  );
  migrate("--legacy-agent-knowledge", knowledge, false);
  fs.writeFileSync(path.join(own, "raw", "runbook.md"), original);
  await start();
  await api("/api/agents/" + agent, "DELETE");
  await stop();
  migrate("--legacy-agents", agents);
  migrate("--legacy-agent-knowledge", knowledge);
  await start();
  await api("/api/agents/" + agent, "GET", undefined, 404);
  await api(route, "POST", { query: "rollback" }, 404);
  console.log(
    JSON.stringify({
      passed: true,
      python_backend: false,
      private_knowledge_import: true,
      cited_retrieval: true,
      agent_isolation: true,
      orphan_chunks_excluded: true,
      source_independent: true,
      native_upload_search: true,
      repeat_and_recovery: true,
      tamper_rejected: true,
      deleted_agent_not_resurrected: true,
      semantic_capability_honest: true,
      fixture: root,
    }),
  );
} finally {
  await stop();
  model.closeAllConnections();
  await new Promise((r) => model.close(r));
}
