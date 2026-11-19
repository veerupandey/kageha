// Disposable SSH argv fixture; the backend remains Rust-only and is exercised through real approvals.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import http from "node:http";
import { spawn } from "node:child_process";
const root = fs.mkdtempSync("/tmp/kageha-execution-host-"),
  token = "disposable-execution-operator-access-token";
const pause = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(fn) {
  for (let end = Date.now() + 20000; Date.now() < end; ) {
    let v = await fn();
    if (v) return v;
    await pause(20);
  }
  throw Error("Condition timed out");
}
let base, child;
const logs = [];
const model = http.createServer(async (req, res) => {
  let raw = "";
  for await (const b of req) raw += b;
  let body = JSON.parse(raw);
  let intent = body.messages.filter((m) => m.role === "user").at(-1).content;
  let message;
  if (body.messages.at(-1).role === "tool")
    message = { role: "assistant", content: body.messages.at(-1).content };
  else {
    let name = "remote_exec",
      args = { command: "printf x >> counter; printf executed" };
    if (intent === "upload" || intent === "changed-upload") {
      name = "remote_upload";
      args = {
        path: "source.txt",
        destination:
          intent === "upload"
            ? "uploaded/evidence.txt"
            : "uploaded/changed.txt",
      };
    }
    if (intent === "read") {
      name = "read_file";
      args = { path: "uploaded/evidence.txt" };
    }
    if (intent === "lost") args = { command: "printf x >> lost; exit 255" };
    if (intent === "crash")
      args = {
        command:
          "printf x >> crash-count; touch crash-started; while test ! -e crash-release; do sleep 0.02; done; printf completed > crash-done",
      };
    if (intent === "failed") args = { command: "exit 7" };
    if (intent === "inspect") {
      name = "remote_receipts";
      args = {};
    }
    message = {
      role: "assistant",
      content: null,
      tool_calls: [
        {
          id: "one",
          type: "function",
          function: { name, arguments: JSON.stringify(args) },
        },
      ],
    };
  }
  res.end(JSON.stringify({ choices: [{ message }] }));
});
await new Promise((r) => model.listen(0, "127.0.0.1", r));
async function api(route, body, expected = 200) {
  let r = await fetch(base + route, {
    method: body ? "POST" : "GET",
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
    },
    body: body ? JSON.stringify(body) : undefined,
    signal: AbortSignal.timeout(15000),
  });
  let text = await r.text();
  assert.equal(r.status, expected, text);
  return text ? JSON.parse(text) : null;
}
async function start() {
  let log = root + "/host-" + logs.length + ".log";
  logs.push(log);
  child = spawn(
    path.resolve(process.env.CARGO_TARGET_DIR || "target", "debug/kageha-runtime-service"),
    [
      "--state",
      root + "/state",
      "--workspace",
      root + "/local",
      "--config",
      root + "/config.json",
      "--token-file",
      root + "/token",
      "--bind",
      "127.0.0.1:0",
    ],
    {
      env: {
        PATH: root + "/empty",
        UNRELATED_MODEL_SECRET: "must-not-reach-ssh",
      },
      stdio: ["ignore", fs.openSync(log, "a"), fs.openSync(log, "a")],
    },
  );
  base = await until(() => {
    if (child.exitCode !== null) throw Error(fs.readFileSync(log, "utf8"));
    let m = fs
      .readFileSync(log, "utf8")
      .match(/listening on (127\.0\.0\.1:\d+)\r?\n/);
    return m && "http://" + m[1];
  });
}
async function stop(signal = "SIGTERM") {
  if (child && child.exitCode === null && child.signalCode === null) {
    child.kill(signal);
    await new Promise((r) => child.once("exit", r));
  }
}
async function submit(id, intent) {
  await api("/v1/runs", {
    id,
    session: "chat",
    turn: "turn-" + id,
    request: id,
    agent: "main",
    depth: 0,
    parent: null,
    parent_run: null,
    objective: intent,
    input: {
      messages: [{ role: "user", content: intent }],
      max_model_calls: 3,
      timeout_ms: 20000,
    },
  });
}
async function pending(id) {
  return until(async () =>
    (await api("/v1/approvals?turn=turn-" + id)).approvals.find(
      (r) => r.state === "pending",
    ),
  );
}
async function approve(record) {
  assert.equal(record.request.authority.binding, "execution.ssh.fixture");
  await api(`/v1/approvals/${record.id}/decision`, {
    authority: record.request.authority,
    expected_revision: record.revision,
    approved: true,
    scope: "once",
  });
}
async function done(id) {
  return until(async () => {
    let r = await api("/v1/runs/" + id);
    return (
      ["succeeded", "failed", "interrupted", "cancelled"].includes(r.state) && r
    );
  });
}
try {
  fs.mkdirSync(root + "/empty");
  fs.mkdirSync(root + "/local");
  fs.writeFileSync(root + "/local/source.txt", "approved upload bytes");
  fs.writeFileSync(root + "/token", token, { mode: 0o600 });
  fs.writeFileSync(
    root + "/ssh",
    `#!/bin/sh\n[ -z "$UNRELATED_MODEL_SECRET" ] || exit 99\nexport PATH=/usr/bin:/bin\nfor arg do script="$arg"; done\n/bin/sh -c "$script"\n`,
    { mode: 0o700 },
  );
  fs.writeFileSync(
    root + "/config.json",
    JSON.stringify({
      providers: [
        {
          endpoint: `http://127.0.0.1:${model.address().port}/model`,
          protocol: "openai_compat",
          model: "fixture",
        },
      ],
      remote: {
        id: "fixture",
        host: "fixture.example",
        user: "tester",
        workdir: root + "/remote",
        ssh_program: root + "/ssh",
      },
    }),
  );
  await start();
  await submit("execute", "execute");
  let permission = await pending("execute");
  assert(!fs.existsSync(root + "/remote/counter"));
  await approve(permission);
  assert.equal((await done("execute")).state, "succeeded");
  assert.equal(fs.readFileSync(root + "/remote/counter", "utf8"), "x");
  await submit("upload", "upload");
  permission = await pending("upload");
  await approve(permission);
  assert.equal((await done("upload")).state, "succeeded");
  assert.equal(
    fs.readFileSync(root + "/remote/uploaded/evidence.txt", "utf8"),
    "approved upload bytes",
  );
  await submit("read", "read");
  assert.equal(
    (await done("read")).completion.result.message,
    "approved upload bytes",
  );
  await submit("changed", "changed-upload");
  permission = await pending("changed");
  fs.writeFileSync(root + "/local/source.txt", "changed after review");
  await approve(permission);
  assert((await done("changed")).completion.result.message.includes("changed"));
  assert(!fs.existsSync(root + "/remote/uploaded/changed.txt"));
  await submit("failed", "failed");
  await approve(await pending("failed"));
  assert((await done("failed")).completion.result.message.includes("false"));
  await submit("lost", "lost");
  await approve(await pending("lost"));
  assert.equal((await done("lost")).state, "failed");
  assert.equal(fs.readFileSync(root + "/remote/lost", "utf8"), "x");
  await stop();
  await start();
  await submit("blocked", "execute");
  await approve(await pending("blocked"));
  assert.equal((await done("blocked")).state, "failed");
  assert.equal(fs.readFileSync(root + "/remote/counter", "utf8"), "x");
  assert.equal((await fetch(base + "/v1/execution/receipts")).status, 401);
  const receipts = (await api("/v1/execution/receipts?session=chat")).receipts;
  const uncertain = receipts.find((r) => r.state === "uncertain");
  assert(uncertain);
  await api(
    `/v1/execution/receipts/${uncertain.receipt}/acknowledge`,
    { target: "wrong", allow_continuation_without_replay: true },
    409,
  );
  await api(
    `/v1/execution/receipts/${uncertain.receipt}/acknowledge`,
    { target: uncertain.target, allow_continuation_without_replay: false },
    403,
  );
  const ack = await api(
    `/v1/execution/receipts/${uncertain.receipt}/acknowledge`,
    { target: uncertain.target, allow_continuation_without_replay: true },
  );
  assert.equal(ack.historical_outcome, "unknown");
  assert.equal(ack.replayed, false);
  await submit("continued", "execute");
  await approve(await pending("continued"));
  assert.equal((await done("continued")).state, "succeeded");
  assert.equal(fs.readFileSync(root + "/remote/counter", "utf8"), "xx");
  assert.equal(fs.readFileSync(root + "/remote/lost", "utf8"), "x");
  await submit("inspect", "inspect");
  assert(
    (await done("inspect")).completion.result.message.includes(
      "acknowledged_unknown",
    ),
  );
  await submit("crash", "crash");
  await approve(await pending("crash"));
  await until(() => fs.existsSync(root + "/remote/crash-started"));
  await stop("SIGKILL");
  fs.writeFileSync(root + "/remote/crash-release", "continue fixture");
  await until(() => fs.existsSync(root + "/remote/crash-done"));
  await start();
  assert.equal((await api("/v1/runs/crash")).state, "interrupted");
  await submit("after-crash", "execute");
  await approve(await pending("after-crash"));
  assert.equal((await done("after-crash")).state, "failed");
  assert.equal(fs.readFileSync(root + "/remote/crash-count", "utf8"), "x");
  assert.equal(fs.readFileSync(root + "/remote/counter", "utf8"), "xx");
  console.log(
    JSON.stringify({
      passed: true,
      python_backend: false,
      node_backend: false,
      checks: [
        "real remote approvals",
        "SSH argv fixture",
        "exact upload hash/destination",
        "read routing",
        "changed upload fenced",
        "nonzero status",
        "SSH 255 uncertain",
        "restart blocks fresh-ID replay",
        "operator acknowledgement retains unknown result",
        "credential environment scrubbed",
        "actual host crash between remote dispatch and receipt publication",
      ],
    }),
  );
} catch (e) {
  for (const log of logs) console.error(fs.readFileSync(log, "utf8"));
  throw e;
} finally {
  await stop();
  model.closeAllConnections();
  await new Promise((r) => model.close(r));
  fs.rmSync(root, { recursive: true, force: true });
}
