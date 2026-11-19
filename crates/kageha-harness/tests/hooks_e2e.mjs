import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import http from "node:http";
import { spawn } from "node:child_process";
import { DatabaseSync } from "node:sqlite";
import { credentialFixture } from "../../kageha-loop/tests/credential_fixture.mjs";
import { isolatedModelFixture } from "../../kageha-loop/tests/isolated_model_fixture.mjs";
const root = fs.mkdtempSync(path.join(os.tmpdir(), "kageha-hooks-"));
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
  if (intent === "hold-config" && request.messages.at(-1)?.role !== "tool") {
    held();
    await new Promise((r) => (release = r));
  }
  const message =
    request.messages.at(-1)?.role === "tool"
      ? { role: "assistant", content: "Hook fixture answered" }
      : {
          role: "assistant",
          content: null,
          tool_calls: [
            {
              id: "read-one",
              type: "function",
              function: {
                name: "read_file",
                arguments: JSON.stringify({ path: "evidence.txt" }),
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

fs.mkdirSync(path.join(workspace, ".kageha"));
fs.writeFileSync(
  path.join(workspace, "evidence.txt"),
  "ISOLATED_TOOL_EVIDENCE",
);
const config = path.join(workspace, ".kageha/hooks.json"),
  counts = path.join(workspace, "hook-counts");
const cmd = (word) =>
  `printf '${word}\\n' >> hook-counts; printf '{"extra_context":"${word}_CTX"}'`;
const hooks = [
  { event: "sessionStart", command: cmd("START") },
  { event: "preToolUse", matcher: "read_file", command: cmd("PRE") },
  { event: "postToolUse", matcher: "read_file", command: cmd("POST") },
];
function configure(value) {
  fs.writeFileSync(config, JSON.stringify(value));
}
async function session(profile = "full") {
  const id = (await api("/api/sessions", "POST", {})).session_id;
  await api("/api/permissions", "POST", {
    session_id: id,
    profile,
    ...(profile === "full" ? { confirm_full: true } : {}),
  });
  return id;
}
async function send(id, intent) {
  return api("/api/chat/send", "POST", {
    session_id: id,
    client_request_id: intent,
    message: intent,
  });
}
const relevant = (intent) =>
  requests.filter((r) =>
    r.messages.some((m) => m.role === "user" && m.content === intent),
  );
let httpCalls = 0;
const endpoint = http.createServer(async (req, res) => {
  let text = "";
  for await (const b of req) text += b;
  const payload = JSON.parse(text);
  assert(["preToolUse", "postToolUse"].includes(payload.event));
  assert.equal(payload.tool_name, "read_file");
  assert.equal(payload.arguments.path, "evidence.txt");
  if (payload.event === "postToolUse")
    assert(payload.result_preview.includes("ISOLATED_TOOL_EVIDENCE"));
  assert(!req.headers.authorization);
  httpCalls++;
  res.statusCode = req.url === "/fail" ? 503 : 200;
  res.end(JSON.stringify({ extra_context: "HTTP_CTX" }));
});
await new Promise((r) => endpoint.listen(0, "127.0.0.1", r));
const url = `http://127.0.0.1:${endpoint.address().port}`;
try {
  configure(hooks);
  await start();
  const id = await session();
  await send(id, "initial");
  assert.equal(fs.readFileSync(counts, "utf8"), "START\nPRE\nPOST\n");
  assert(JSON.stringify(relevant("initial")).includes("START_CTX"));
  assert(JSON.stringify(relevant("initial")).includes("PRE_CTX"));
  assert(JSON.stringify(relevant("initial")).includes("POST_CTX"));
  const prior = requests.length;
  await send(id, "initial");
  assert.equal(requests.length, prior);
  assert.equal(fs.readFileSync(counts, "utf8"), "START\nPRE\nPOST\n");
  const heldTurn = send(id, "hold-config");
  await holding;
  configure([
    { event: "preToolUse", matcher: "read_file", deny: "Configured exclusion" },
  ]);
  release();
  await heldTurn;
  assert(
    JSON.stringify(relevant("hold-config")).includes("ISOLATED_TOOL_EVIDENCE"),
  );
  await send(id, "denied");
  assert(
    !JSON.stringify(relevant("denied")).includes("ISOLATED_TOOL_EVIDENCE"),
  );
  assert.equal(relevant("denied").length, 1);
  configure([
    {
      event: "preToolUse",
      matcher: "read_file",
      command:
        'if [ -n "$KAGEHA_SERVER_TOKEN" ] || [ -n "$KAGEHA_API_KEY" ]; then exit 2; fi; printf \'{"extra_context":"ENV_CLEAN"}\'',
      timeout_s: 0.2,
    },
  ]);
  const ask = await session("ask");
  let turn = send(ask, "manual");
  const pending = await until(async () => {
    const p = (await api("/api/sessions/" + ask)).pending_approval;
    return p?.action === "project_hook" && p;
  });
  await new Promise((r) => setTimeout(r, 350));
  await api("/api/approvals", "POST", {
    approval_id: pending.approval_id,
    approved: true,
    scope: "once",
  });
  await turn;
  assert(JSON.stringify(relevant("manual")).includes("ENV_CLEAN"));
  configure([{ event: "preToolUse", matcher: "read_file", http: url + "/ok" }]);
  await send(id, "http");
  assert.equal(httpCalls, 1);
  assert(JSON.stringify(relevant("http")).includes("HTTP_CTX"));
  configure([
    { event: "preToolUse", matcher: "read_file", http: url + "/fail" },
  ]);
  await send(id, "uncertain");
  assert.equal(httpCalls, 2);
  assert.equal(relevant("uncertain").length, 1);
  await send(id, "blocked-new-call");
  assert.equal(httpCalls, 2);
  assert.equal(relevant("blocked-new-call").length, 1);
  let receipts = (await api("/api/sessions/" + id + "/hooks/receipts"))
    .receipts;
  const unknown = receipts.find((r) => r.state === "uncertain");
  assert(unknown);
  await api(
    "/api/sessions/" + ask + "/hooks/receipts",
    "POST",
    {
      action: "acknowledge_unknown",
      receipt: unknown.receipt,
      target: unknown.target,
    },
    404,
  );
  await api("/api/sessions/" + id + "/hooks/receipts", "POST", {
    action: "acknowledge_unknown",
    receipt: unknown.receipt,
    target: unknown.target,
  });
  assert.equal(httpCalls, 2);
  configure([
    { event: "postToolUse", matcher: "read_file", http: url + "/fail" },
  ]);
  await send(id, "post-uncertain");
  assert.equal(httpCalls, 3);
  assert.equal(relevant("post-uncertain").length, 1);
  const stopped = await api("/api/sessions/" + id);
  const saved = stopped.messages.at(-1).text;
  assert(saved.includes("actions completed"));
  assert(saved.includes("ISOLATED_TOOL_EVIDENCE"));
  assert(saved.includes("do not repeat"));
  configure([{ event: "afterFileEdit", command: "exit 2" }]);
  const inspection = await api("/api/project");
  assert.equal(inspection.hooks[0].active, false);
  configure([]);
  await send(id, "unrelated");
  assert(
    JSON.stringify(relevant("unrelated")).includes("ISOLATED_TOOL_EVIDENCE"),
  );
  await stop();
  await start();
  assert(
    (await api("/api/sessions/" + id + "/hooks/receipts")).receipts.some(
      (r) => r.state === "acknowledged_unknown",
    ),
  );
  await api("/api/sessions/" + id, "DELETE");
  await api("/api/sessions/" + id + "/hooks/receipts", "GET", undefined, 404);
  console.log(
    JSON.stringify({
      passed: true,
      python_backend: false,
      static_deny: true,
      command_and_http: true,
      lifecycle_start: true,
      exact_manual_approval: true,
      execution_timeout_excludes_approval_wait: true,
      environment_scrubbed: true,
      immutable_hook_config: true,
      retry_no_replay: true,
      confirmed_tool_facts_survive_post_hook_failure: true,
      inspection_capabilities_honest: true,
      uncertain_hook_fencing: true,
      receipt_isolation: true,
      unknown_acknowledgment_not_success: true,
      restart_and_erasure: true,
    }),
  );
} finally {
  release?.();
  await stop();
  model.closeAllConnections();
  await new Promise((r) => model.close(r));
  endpoint.closeAllConnections();
  await new Promise((r) => endpoint.close(r));
}
