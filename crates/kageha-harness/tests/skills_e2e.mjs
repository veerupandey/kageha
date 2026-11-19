import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import http from "node:http";
import { spawn } from "node:child_process";
import { DatabaseSync } from "node:sqlite";
import { credentialFixture } from "../../kageha-loop/tests/credential_fixture.mjs";
import { isolatedModelFixture } from "../../kageha-loop/tests/isolated_model_fixture.mjs";
const root = fs.mkdtempSync(path.join(os.tmpdir(), "kageha-skills-"));
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
  const message =
    request.messages.at(-1)?.role === "tool"
      ? { role: "assistant", content: "Fixture done" }
      : {
          role: "assistant",
          content: null,
          tool_calls: [
            {
              id: "read-" + intent,
              type: "function",
              function: {
                name: "read_file",
                arguments: JSON.stringify({
                  path: "skills/source_reader/references/source.json",
                }),
              },
            },
          ],
        };
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
const manifest =
  "---\nname: source_reader\ndescription: Read fixture references\nallowed-tools: read_file\nmetadata:\n  required_resources: [references/source.json]\n---\nUse the violet reference";
const pkg = path.join(root, "source_reader");
fs.mkdirSync(path.join(pkg, "references"), { recursive: true });
fs.writeFileSync(path.join(pkg, "SKILL.md"), manifest);
fs.writeFileSync(
  path.join(pkg, "references/source.json"),
  JSON.stringify({ value: "VIOLET_COPIED_RESOURCE" }),
);
const rows = () => api("/api/extensions/skills");
async function action(name, action, extra = {}, status = 200) {
  return api(
    "/api/knowledge/skill",
    "POST",
    { name, action, ...extra },
    status,
  );
}
async function run(intent, persona_id = "main") {
  const id = (await api("/api/sessions", "POST", { persona_id })).session_id;
  await api("/api/chat/send", "POST", {
    session_id: id,
    client_request_id: intent,
    message: intent,
  });
  const request = requests
    .filter((r) =>
      r.messages.some((m) => m.role === "user" && m.content === intent),
    )
    .at(-1);
  return {
    text:
      request.messages.filter((m) => m.role === "tool").at(-1)?.content || "",
    prompt: JSON.stringify(request.messages.filter((m) => m.role === "system")),
  };
}
try {
  await start();
  await api("/api/skills/install", "POST", { source: pkg });
  fs.renameSync(pkg, pkg + "-hidden");
  let record = (await rows()).skills.find((s) => s.name === "source_reader");
  assert.equal(record.revision, 1);
  assert.equal(record.peer_shareable, false);
  await action("source_reader", "pin", { expected_revision: 1 });
  let result = await run("copied");
  assert(result.text.includes("VIOLET_COPIED_RESOURCE"));
  assert(result.prompt.includes("Use the violet reference"));
  await action("source_reader", "save", {
    content: manifest.replace("violet reference", "updated violet reference"),
    expected_revision: 2,
  });
  await action(
    "source_reader",
    "save",
    { content: manifest, expected_revision: 2 },
    409,
  );
  result = await run("edited");
  assert(result.text.includes("VIOLET_COPIED_RESOURCE"));
  assert(result.prompt.includes("updated violet reference"));
  await api("/api/agents", "POST", {
    id: "restricted",
    name: "Restricted",
    skills_restricted_to_pinned: true,
    pinned_skills: [],
  });
  result = await run("restricted", "restricted");
  assert(!result.text.includes("VIOLET_COPIED_RESOURCE"));
  assert(!result.prompt.includes("updated violet reference"));
  await action("source_reader", "archive");
  assert.equal((await rows()).skills[0].archived, true);
  result = await run("archived");
  assert(!result.text.includes("VIOLET_COPIED_RESOURCE"));
  await stop();
  await start();
  await action("source_reader", "restore");
  result = await run("restart");
  assert(result.text.includes("VIOLET_COPIED_RESOURCE"));
  await action("source_reader", "delete");
  await stop();
  // Existing application records are preserved and adopted once, including unknown metadata.
  const db = new DatabaseSync(path.join(state, "native-chats.sqlite"));
  const legacy = {
    name: "legacy_reader",
    content: "Retained legacy instructions",
    pinned: true,
    archived: false,
    custom: { original: true },
  };
  db.prepare("INSERT INTO ui_documents VALUES ('skill',?,?,1,0)").run(
    "legacy_reader",
    JSON.stringify(legacy),
  );
  db.close();
  await start();
  assert(
    (await rows()).skills.some((s) => s.name === "legacy_reader" && s.pinned),
  );
  assert(!(await rows()).skills.some((s) => s.name === "source_reader"));
  await action("legacy_reader", "save", {
    content: "Edited legacy instructions",
  });
  await action("legacy_reader", "delete");
  await stop();
  await start();
  assert.equal((await rows()).skills.length, 0);
  const owned = new DatabaseSync(
    path.join(state, "components/skills/registry.sqlite"),
    { readOnly: true },
  );
  assert.equal(
    JSON.parse(
      owned.prepare("SELECT body FROM skill_import_snapshots").get().body,
    ).custom.original,
    true,
  );
  owned.close();
  console.log(
    JSON.stringify({
      passed: true,
      python_backend: false,
      resource_copy: true,
      resource_preserving_edits: true,
      revision_conflicts: true,
      restricted_agent_scope: true,
      lifecycle_restart: true,
      legacy_adoption: true,
      tombstones: true,
    }),
  );
} finally {
  await stop();
  model.closeAllConnections();
  await new Promise((r) => model.close(r));
}
