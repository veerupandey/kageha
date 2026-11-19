// Real Rust service, approval journal and shell/HTTP hooks. Backend PATH is empty.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import http from "node:http";
import { spawn, execFileSync } from "node:child_process";
import { DatabaseSync } from "node:sqlite";
const root = fs.mkdtempSync("/tmp/kageha-lifecycle-"),
  workspace = root + "/workspace",
  token = "disposable-hook-operator-token";
const pause = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(fn) {
  for (let end = Date.now() + 25000; Date.now() < end; ) {
    const v = await fn();
    if (v) return v;
    await pause(20);
  }
  throw Error("Condition timed out");
}
let child,
  base,
  modelCalls = 0,
  httpCalls = 0,
  httpMode = "ok";
const logs = [],
  modelErrors = [];
const model = http.createServer(async (req, res) => {
  try {
    let raw = "";
    for await (const b of req) raw += b;
    const body = JSON.parse(raw);
    modelCalls++;
    assert(
      !body.tools.some((t) => t.function.name === "project_hook"),
      "Hooks are private host operations",
    );
    const intent = body.messages
      .filter((m) => m.role === "user")
      .at(-1).content;
    const message =
      body.messages.at(-1).role === "tool"
        ? { role: "assistant", content: body.messages.at(-1).content }
        : {
            role: "assistant",
            content: null,
            tool_calls: [
              {
                id: "read-one",
                type: "function",
                function: {
                  name: intent === "forged" ? "project_hook" : "read_file",
                  arguments: JSON.stringify(
                    intent === "forged" ? {} : { path: "notes.txt" },
                  ),
                },
              },
            ],
          };
    res.end(JSON.stringify({ choices: [{ message }] }));
  } catch (e) {
    modelErrors.push(e);
    res.statusCode = 500;
    res.end("{}");
  }
});
const webhook = http.createServer(async (req, res) => {
  let raw = "";
  for await (const b of req) raw += b;
  const body = JSON.parse(raw);
  assert.equal(body.tool_name, "read_file");
  httpCalls++;
  if (httpMode === "hold") return;
  res.statusCode = httpMode === "fail" ? 500 : 200;
  res.end(JSON.stringify({ extra_context: "webhook evidence" }));
});
await new Promise((r) => model.listen(0, "127.0.0.1", r));
await new Promise((r) => webhook.listen(0, "127.0.0.1", r));
const endpoint = `http://127.0.0.1:${webhook.address().port}/hook`;
async function start() {
  const log = root + "/host-" + logs.length + ".log";
  logs.push(log);
  child = spawn(
    path.resolve(
      process.env.CARGO_TARGET_DIR || "target",
      "debug/kageha-runtime-service",
    ),
    [
      "--state",
      root + "/state",
      "--workspace",
      workspace,
      "--config",
      root + "/config.json",
      "--token-file",
      root + "/token",
      "--bind",
      "127.0.0.1:0",
    ],
    {
      env: { PATH: root + "/empty" },
      stdio: ["ignore", fs.openSync(log, "a"), fs.openSync(log, "a")],
    },
  );
  base = await until(() => {
    if (child.exitCode !== null) throw Error(fs.readFileSync(log, "utf8"));
    const m = fs
      .readFileSync(log, "utf8")
      .match(/runtime listening on (127\.0\.0\.1:\d+)/);
    return m && "http://" + m[1];
  });
}
async function stop(signal = "SIGTERM") {
  if (child && child.exitCode === null && child.signalCode === null) {
    child.kill(signal);
    await new Promise((r) => child.once("exit", r));
  }
}
async function api(route, body, status = 200) {
  const res = await fetch(base + route, {
    method: body ? "POST" : "GET",
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
    },
    body: body ? JSON.stringify(body) : undefined,
    signal: AbortSignal.timeout(15000),
  });
  const data = await res.json();
  assert.equal(res.status, status, JSON.stringify(data));
  return data;
}
function spec(id, extra = {}) {
  return {
    id,
    session: "session-" + id,
    turn: "turn-" + id,
    request: id,
    agent: "main",
    depth: 0,
    parent: null,
    parent_run: null,
    objective: id,
    input: {
      messages: [{ role: "user", content: id }],
      max_model_calls: 4,
      timeout_ms: 30000,
      ...extra,
    },
  };
}
async function submit(id, extra = {}) {
  await api("/v1/runs", spec(id, extra));
}
async function done(id) {
  return until(async () => {
    const r = await api("/v1/runs/" + id);
    return (
      ["succeeded", "failed", "cancelled", "interrupted"].includes(r.state) && r
    );
  });
}
async function pending(id) {
  return until(async () => {
    const rows = await api("/v1/approvals?turn=turn-" + id);
    return rows.approvals.find((r) => r.state === "pending");
  });
}
async function decide(r, approved = true) {
  await api("/v1/approvals/" + r.id + "/decision", {
    expected_revision: r.revision,
    authority: r.request.authority,
    approved,
    scope: "once",
  });
}
function configure(hooks) {
  fs.writeFileSync(
    workspace + "/.kageha/hooks.json",
    JSON.stringify({ hooks }),
  );
}
const command = `printf '%s\\n' "$KAGEHA_HOOK_EVENT" >> trace; printf '{"extra_context":"shell hook evidence"}'`;
try {
  fs.mkdirSync(workspace + "/.kageha", { recursive: true });
  fs.mkdirSync(root + "/empty");
  fs.writeFileSync(root + "/token", token, { mode: 0o600 });
  fs.writeFileSync(workspace + "/notes.txt", "retained file evidence");
  execFileSync("/usr/bin/git", ["init", "-q", workspace]);
  execFileSync("/usr/bin/git", ["-C", workspace, "add", "."]);
  execFileSync("/usr/bin/git", [
    "-C",
    workspace,
    "-c",
    "user.name=Fixture",
    "-c",
    "user.email=fixture@example.invalid",
    "commit",
    "-qm",
    "fixture",
  ]);
  fs.writeFileSync(
    root + "/config.json",
    JSON.stringify({
      providers: [
        {
          endpoint: `http://127.0.0.1:${model.address().port}/model`,
          model: "fixture",
          protocol: "openai_compat",
        },
      ],
    }),
  );
  configure(
    ["sessionStart", "preToolUse", "postToolUse"].map((event) => ({
      event,
      command,
    })),
  );
  await start();
  assert((await api("/v1/info")).capabilities.includes("hooks.receipts"));
  await submit("normal");
  for (let n = 0; n < 3; n++) {
    const r = await pending("normal");
    assert.equal(r.request.action, "project_hook");
    const exact = JSON.parse(
      r.request.detail.split("Exact execution context: ")[1],
    );
    assert.equal(exact.hook.command, command);
    assert.equal(exact.workspace, fs.realpathSync(workspace));
    assert.equal(r.request.authority.binding, "hooks.workspace");
    if (n === 0) {
      assert.equal(modelCalls, 0);
      assert(!fs.existsSync(workspace + "/trace"));
    }
    await decide(r);
  }
  const normal = await done("normal");
  assert.equal(normal.state, "succeeded");
  assert(normal.completion.result.message.includes("retained file evidence"));
  assert(normal.completion.result.message.includes("shell hook evidence"));
  assert.equal(
    fs.readFileSync(workspace + "/trace", "utf8"),
    "sessionStart\npreToolUse\npostToolUse\n",
  );
  const normalSpec = spec("normal");
  await api("/v1/runs", normalSpec);
  assert.equal(
    fs.readFileSync(workspace + "/trace", "utf8"),
    "sessionStart\npreToolUse\npostToolUse\n",
  );
  assert.equal(
    (await api("/v1/runs/normal/hooks/receipts")).receipts.length,
    3,
  );
  configure([{ event: "preToolUse", deny: "excluded by project rule" }]);
  await submit("denied");
  assert.equal((await done("denied")).state, "failed");
  let db = new DatabaseSync(root + "/state/runtime.sqlite", { readOnly: true });
  assert.equal(
    db
      .prepare(
        "SELECT count(*) n FROM effects WHERE scope='turn-denied' AND id='tool:read-one'",
      )
      .get().n,
    0,
  );
  db.close();
  configure([{ event: "preToolUse", command }]);
  await submit("changed");
  let r = await pending("changed");
  configure([]);
  await decide(r);
  assert.equal((await done("changed")).state, "failed");
  assert.equal(
    (await api("/v1/runs/changed/hooks/receipts")).receipts.length,
    0,
  );
  configure([{ event: "preToolUse", command }]);
  await submit("declined");
  r = await pending("declined");
  await decide(r, false);
  assert.equal((await done("declined")).state, "failed");
  assert.equal(
    (await api("/v1/runs/declined/hooks/receipts")).receipts.length,
    0,
  );
  configure([{ event: "preToolUse", command }]);
  await submit("root-change");
  r = await pending("root-change");
  fs.renameSync(workspace, workspace + "-held");
  fs.mkdirSync(workspace + "/.kageha", { recursive: true });
  fs.copyFileSync(
    workspace + "-held/.kageha/hooks.json",
    workspace + "/.kageha/hooks.json",
  );
  fs.copyFileSync(workspace + "-held/notes.txt", workspace + "/notes.txt");
  await decide(r);
  assert.equal((await done("root-change")).state, "failed");
  assert(!fs.existsSync(workspace + "/trace"));
  assert.equal(
    (await api("/v1/runs/root-change/hooks/receipts")).receipts.length,
    0,
  );
  fs.rmSync(workspace, { recursive: true });
  fs.renameSync(workspace + "-held", workspace);
  configure([{ event: "preToolUse", http: endpoint }]);
  await submit("http");
  r = await pending("http");
  assert.equal(httpCalls, 0);
  assert(r.request.detail.includes(endpoint));
  assert(r.request.detail.includes("payload_sha256"));
  await decide(r);
  assert.equal((await done("http")).state, "succeeded");
  assert.equal(httpCalls, 1);
  configure([{ event: "sessionStart", command: "pwd" }]);
  await api("/v1/runs", {
    ...spec("isolated", {
      worker_workspace: { isolation: "worktree", keep_worktree: true },
    }),
    agent: "worker",
  });
  await decide(await pending("isolated"));
  const isolated = await done("isolated");
  assert.equal(isolated.state, "succeeded");
  const snapshot = await api("/v1/runs/isolated/workspace");
  assert.notEqual(snapshot.root, workspace);
  assert(snapshot.root);
  assert.equal(
    fs.readFileSync(workspace + "/trace", "utf8"),
    "sessionStart\npreToolUse\npostToolUse\n",
  );
  configure([{ event: "postToolUse", http: endpoint }]);
  httpMode = "fail";
  await submit("postfail");
  await decide(await pending("postfail"));
  assert.equal((await done("postfail")).state, "failed");
  db = new DatabaseSync(root + "/state/runtime.sqlite", { readOnly: true });
  const fact = db
    .prepare(
      "SELECT result FROM effects WHERE scope='turn-postfail' AND id='tool:read-one'",
    )
    .get();
  assert(fact);
  assert(JSON.parse(fact.result).content.includes("retained file evidence"));
  db.close();
  let unknown = (await api("/v1/runs/postfail/hooks/receipts")).receipts.find(
    (r) => r.state === "uncertain",
  );
  assert(unknown);
  await api(
    "/v1/runs/normal/hooks/acknowledge",
    { receipt: unknown.receipt, target: unknown.target, confirmed: true },
    403,
  );
  await api(
    "/v1/runs/postfail/hooks/acknowledge",
    { receipt: unknown.receipt, target: unknown.target, confirmed: false },
    409,
  );
  await api("/v1/runs/postfail/hooks/acknowledge", {
    receipt: unknown.receipt,
    target: unknown.target,
    confirmed: true,
  });
  configure([
    {
      event: "preToolUse",
      command:
        "printf started > cancel-marker; /bin/sleep 20; printf finished >> cancel-marker",
    },
  ]);
  await submit("cancel");
  await decide(await pending("cancel"));
  await until(() => fs.existsSync(workspace + "/cancel-marker"));
  await api("/v1/runs/cancel/cancel", {});
  assert.equal((await done("cancel")).state, "cancelled");
  assert.equal(
    fs.readFileSync(workspace + "/cancel-marker", "utf8"),
    "started",
  );
  unknown = (await api("/v1/runs/cancel/hooks/receipts")).receipts.find(
    (r) => r.state === "uncertain",
  );
  assert(unknown);
  await api("/v1/runs/cancel/hooks/acknowledge", {
    receipt: unknown.receipt,
    target: unknown.target,
    confirmed: true,
  });
  configure([
    {
      event: "preToolUse",
      command:
        "printf started > timeout-marker; /bin/sleep 20; printf finished >> timeout-marker",
      timeout_s: 0.1,
    },
  ]);
  await submit("timeout");
  await decide(await pending("timeout"));
  assert.equal((await done("timeout")).state, "failed");
  assert.equal(
    fs.readFileSync(workspace + "/timeout-marker", "utf8"),
    "started",
  );
  unknown = (await api("/v1/runs/timeout/hooks/receipts")).receipts.find(
    (r) => r.state === "uncertain",
  );
  assert(unknown);
  await api("/v1/runs/timeout/hooks/acknowledge", {
    receipt: unknown.receipt,
    target: unknown.target,
    confirmed: true,
  });
  configure([]);
  await submit("forged");
  assert(
    (await done("forged")).completion.result.message.includes("not exposed"),
  );
  configure([{ event: "preToolUse", http: endpoint }]);
  httpMode = "hold";
  await submit("crash");
  const beforeCrash = httpCalls;
  await decide(await pending("crash"));
  await until(() => httpCalls > beforeCrash);
  await stop("SIGKILL");
  await start();
  assert.equal((await done("crash")).state, "interrupted");
  const count = httpCalls;
  await api("/v1/runs", spec("crash"));
  await pause(150);
  assert.equal(
    httpCalls,
    count,
    "Unknown HTTP hook must not replay after a real service crash",
  );
  unknown = (await api("/v1/runs/crash/hooks/receipts")).receipts.find(
    (r) => r.state === "uncertain",
  );
  assert(unknown);
  await api("/v1/runs/crash/hooks/acknowledge", {
    receipt: unknown.receipt,
    target: unknown.target,
    confirmed: true,
  });
  assert.deepEqual(modelErrors, []);
  console.log(
    JSON.stringify({
      passed: true,
      python_backend: false,
      node_backend: false,
      checks: [
        "shared shell lifecycle",
        "exact approval",
        "static and operator denial",
        "configuration change fence",
        "execution root replacement fence",
        "HTTP hook",
        "isolated execution root",
        "confirmed base result survives post-hook failure",
        "cancellation and deadlines",
        "private hook port",
        "run-scoped receipt acknowledgement",
        "actual crash without replay",
      ],
      fixture: root,
    }),
  );
} catch (e) {
  for (const log of logs) console.error(fs.readFileSync(log, "utf8"));
  throw e;
} finally {
  await stop();
  model.closeAllConnections();
  webhook.closeAllConnections();
  await Promise.all([
    new Promise((r) => model.close(r)),
    new Promise((r) => webhook.close(r)),
  ]);
  fs.rmSync(root, { recursive: true, force: true });
}
