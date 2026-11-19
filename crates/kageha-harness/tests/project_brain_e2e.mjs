import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import http from "node:http";
import { spawn } from "node:child_process";
import { DatabaseSync } from "node:sqlite";
import { credentialFixture } from "../../kageha-loop/tests/credential_fixture.mjs";
import { isolatedModelFixture } from "../../kageha-loop/tests/isolated_model_fixture.mjs";
const root = fs.mkdtempSync(path.join(os.tmpdir(), "kageha-project-context-"));
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
let release, held;
const holding = new Promise((r) => (held = r));
const model = http.createServer(async (req, res) => {
  let text = "";
  for await (const b of req) text += b;
  const request = JSON.parse(text);
  requests.push(request);
  const intent =
    request.messages.filter((m) => m.role === "user").at(-1)?.content || "";
  let message = {
    role: "assistant",
    content: "Project context fixture answered",
  };
  if (intent === "held-turn" && request.messages.at(-1)?.role !== "tool") {
    held();
    await new Promise((r) => (release = r));
    message = {
      role: "assistant",
      content: null,
      tool_calls: [
        {
          id: "delegate-one",
          type: "function",
          function: {
            name: "agent_spawn",
            arguments: JSON.stringify({
              agent_id: "worker",
              objective: "child-fixture",
            }),
          },
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

fs.mkdirSync(path.join(workspace, ".kageha/rules"), { recursive: true });
fs.mkdirSync(path.join(workspace, ".kageha/commands"), { recursive: true });
fs.writeFileSync(path.join(workspace, "AGENTS.md"), "ORIGINAL_PROJECT_CONTEXT");
fs.writeFileSync(
  path.join(workspace, "CLAUDE.md"),
  "SECONDARY_SHOULD_NOT_DUPLICATE",
);
fs.writeFileSync(
  path.join(workspace, ".kageha/rules/rust.md"),
  "---\nglobs: ['*.rs']\n---\nRETAINED_RUST_RULE",
);
fs.writeFileSync(
  path.join(workspace, ".kageha/commands/check.md"),
  "EXPLICIT_RECIPE_BODY",
);
async function send(chat, id, message, status = 200) {
  return api(
    "/api/chat/send",
    "POST",
    { session_id: chat, client_request_id: id, message },
    status,
  );
}
const prompt = (r) =>
  JSON.stringify(r.messages.filter((m) => m.role === "system"));
try {
  await start();
  const inspection=await api("/api/project");assert.equal(inspection.brain.root_file,"AGENTS.md");assert(inspection.brain.rendered.includes("RETAINED_RUST_RULE"));
  const chat = (await api("/api/sessions", "POST", {})).session_id;
  await send(chat, "recipe", "/project:check evidence");
  const recipe = requests.find((r) =>
    r.messages.some(
      (m) => m.role === "user" && m.content === "/project:check evidence",
    ),
  );
  assert(prompt(recipe).includes("ORIGINAL_PROJECT_CONTEXT"));
  assert(prompt(recipe).includes("RETAINED_RUST_RULE"));
  assert(prompt(recipe).includes("EXPLICIT_RECIPE_BODY"));
  assert(!prompt(recipe).includes("SECONDARY_SHOULD_NOT_DUPLICATE"));
  await api("/api/agents", "POST", { id: "worker", name: "Context worker" });
  const other = (await api("/api/sessions", "POST", {})).session_id;
  const turn = send(other, "held", "held-turn");
  await holding;
  fs.writeFileSync(path.join(workspace, "AGENTS.md"), "NEW_PROJECT_CONTEXT");
  release();
  await turn;
  await until(() =>
    requests.some((r) =>
      r.messages.some(
        (m) => m.role === "user" && m.content === "child-fixture",
      ),
    ),
  );
  const child = requests.find((r) =>
    r.messages.some((m) => m.role === "user" && m.content === "child-fixture"),
  );
  assert(prompt(child).includes("ORIGINAL_PROJECT_CONTEXT"));
  assert(!prompt(child).includes("NEW_PROJECT_CONTEXT"));
  const count = requests.length;
  await send(chat, "recipe", "/project:check evidence");
  assert.equal(requests.length, count);
  const fresh = (await api("/api/sessions", "POST", {})).session_id;
  await send(fresh, "fresh", "fresh-context");
  assert(
    prompt(
      requests.find((r) =>
        r.messages.some(
          (m) => m.role === "user" && m.content === "fresh-context",
        ),
      ),
    ).includes("NEW_PROJECT_CONTEXT"),
  );
  await send(fresh, "missing", "/project:missing", 400);
  fs.unlinkSync(path.join(workspace, "AGENTS.md"));
  fs.symlinkSync("/etc/passwd", path.join(workspace, "AGENTS.md"));
  await send(fresh, "outside", "outside-context", 400);
  await stop();
  console.log(
    JSON.stringify({
      passed: true,
      python_backend: false,
      project_instruction_precedence: true,
      bounded_rule_context: true,
      explicit_recipe: true,
      immutable_retry_context: true,
      worker_inherits_original_context: true,
      new_turn_gets_updated_context: true,
      outside_instructions_denied: true,
    }),
  );
} finally {
  release?.();
  await stop();
  model.closeAllConnections();
  await new Promise((r) => model.close(r));
}
