// One standalone Rust run switches its logical memory from SQLite to an independent HTTP implementation.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import http from "node:http";
import { spawn } from "node:child_process";
import { DatabaseSync } from "node:sqlite";
import { createHash } from "node:crypto";
const root = fs.mkdtempSync("/tmp/kageha-runtime-memory-");
const token = "memory-runtime-disposable-operator-token";
const pause = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(fn) {
  for (let end = Date.now() + 20000; Date.now() < end; ) {
    const value = await fn();
    if (value) return value;
    await pause(20);
  }
  throw Error("Condition timed out");
}
let base,
  daemon,
  memory,
  held,
  switched = false;
const children = [],
  logs = [];
const model = http.createServer(async (req, res) => {
  try {
    let raw = "";
    for await (const chunk of req) raw += chunk;
    const body = JSON.parse(raw);
    const intent = body.messages
      .filter((m) => m.role === "user")
      .at(-1).content;
    const tools = body.messages.filter((m) => m.role === "tool");
    const reply = (message) =>
      res.end(JSON.stringify({ choices: [{ message }] }));
    const action = (id, name, args) => ({
      id,
      type: "function",
      function: { name, arguments: JSON.stringify(args) },
    });
    const send = (calls) =>
      reply({ role: "assistant", content: null, tool_calls: calls });
    if (intent === "switch memory") {
      assert(body.tools.some((t) => t.function.name === "memory_put"));
      if (!tools.length) {
        send([
          action("seed", "memory_put", { id: "note", body: "portable memory" }),
          action("seed-deleted", "memory_put", {
            id: "deleted",
            body: "private deleted note",
          }),
        ]);
        return;
      }
      if (tools.length === 2) {
        held = () =>
          send([action("read-after-switch", "memory_get", { id: "note" })]);
        return;
      }
      if (tools.length === 3) {
        const result = JSON.parse(tools.at(-1).content);
        assert.equal(result.generation, 2);
        assert.equal(result.value.body, "portable memory");
        assert(switched);
        send([
          action("update", "memory_put", {
            id: "note",
            body: "updated memory",
            expected_revision: 1,
          }),
        ]);
        return;
      }
      if (tools.length === 4) {
        send([
          action("delete", "memory_delete", {
            id: "deleted",
            expected_revision: 2,
          }),
        ]);
        return;
      }
      if (tools.length === 5) {
        send([action("search", "memory_search", { query: "updated" })]);
        return;
      }
      assert.equal(JSON.parse(tools.at(-1).content).value.length, 1);
      reply({
        role: "assistant",
        content: "Task continued across memory replacement.",
      });
      return;
    }
    if (!tools.length) {
      send([action("read", "memory_get", { id: "note" })]);
      return;
    }
    const result = JSON.parse(tools.at(-1).content);
    assert.equal(result.generation, 2);
    if (intent === "isolated") assert.equal(result.value, null);
    else assert.equal(result.value.body, "updated memory");
    reply({
      role: "assistant",
      content: intent === "isolated" ? "Isolated." : "Retained.",
    });
  } catch (error) {
    res.writeHead(500);
    res.end(JSON.stringify({ error: String(error) }));
    console.error(error);
  }
});
await new Promise((r) => model.listen(0, "127.0.0.1", r));
async function launch(binary, args, label, env = {}) {
  const log = path.join(root, label + "-" + logs.length + ".log");
  logs.push(log);
  const child = spawn(path.resolve(process.env.CARGO_TARGET_DIR || "target", "debug", binary), args, {
    env: { PATH: root + "/empty", ...env },
    stdio: ["ignore", fs.openSync(log, "a"), fs.openSync(log, "a")],
  });
  children.push(child);
  const address = await until(() => {
    if (child.exitCode !== null) throw Error(fs.readFileSync(log, "utf8"));
    return fs.readFileSync(log, "utf8").match(/listening on (\S+)/)?.[1];
  });
  return { child, base: "http://" + address };
}
async function start() {
  const launched = await launch(
    "kageha-runtime-service",
    [
      "--state",
      root + "/state",
      "--workspace",
      root + "/workspace",
      "--config",
      root + "/config.json",
      "--token-file",
      root + "/token",
      "--bind",
      "127.0.0.1:0",
    ],
    "runtime",
    { MEMORY_ACCESS_TOKEN: token },
  );
  daemon = launched.child;
  base = launched.base;
}
async function api(route, body, expected = 200) {
  const res = await fetch(base + route, {
    method: body ? "POST" : "GET",
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
    },
    body: body ? JSON.stringify(body) : undefined,
    signal: AbortSignal.timeout(20000),
  });
  const data = await res.json();
  assert.equal(res.status, expected, JSON.stringify(data));
  return data;
}
const spec = (id, intent, session = "chat") => ({
  id,
  session,
  turn: "turn-" + id,
  request: id,
  agent: "main",
  depth: 0,
  parent: null,
  parent_run: null,
  objective: intent,
  input: {
    messages: [{ role: "user", content: intent }],
    max_model_calls: 8,
    timeout_ms: 60000,
  },
});
async function approvePending() {
  for (const record of (await api("/v1/approvals")).approvals.filter(
    (r) => r.state === "pending",
  )) {
    assert.equal(record.request.authority.binding, "memory.default");
    await api(`/v1/approvals/${record.id}/decision`, {
      expected_revision: record.revision,
      authority: record.request.authority,
      scope: "once",
      approved: true,
    });
  }
}
async function done(id) {
  return until(async () => {
    await approvePending();
    const run = await api("/v1/runs/" + id);
    return (
      ["succeeded", "failed", "interrupted", "cancelled"].includes(run.state) &&
      run
    );
  });
}
async function stop(child) {
  if (child && child.exitCode === null && child.signalCode === null) {
    child.kill("SIGTERM");
    await new Promise((r) => child.once("exit", r));
  }
}
try {
  fs.mkdirSync(root + "/empty");
  fs.mkdirSync(root + "/workspace");
  fs.writeFileSync(root + "/token", token, { mode: 0o600 });
  memory = await launch(
    "kageha-memory",
    [
      "--state",
      root + "/remote.sqlite",
      "--token-file",
      root + "/token",
      "--bind",
      "127.0.0.1:0",
    ],
    "memory",
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
      memory: {
        initial: "local",
        providers: [
          { id: "local", adapter: "sqlite" },
          {
            id: "remote",
            adapter: "memory-http",
            endpoint: memory.base,
            credentials_env: "MEMORY_ACCESS_TOKEN",
          },
        ],
      },
    }),
  );
  await start();
  assert.equal((await api("/v1/bindings")).generation, 1);
  assert.equal((await fetch(base + "/v1/providers")).status, 401);
  await api("/v1/runs", spec("switch", "switch memory"));
  await until(async () => {
    await approvePending();
    return held;
  });
  const destination = (await api("/v1/providers")).remote;
  const decision = {
    destination_id: "remote",
    instance_id: destination.instance_id,
    allow_data_transfer: true,
  };
  await api(
    "/v1/replacements",
    {
      request_id: "denied",
      expected_generation: 1,
      decision: { ...decision, allow_data_transfer: false },
    },
    403,
  );
  const replacement = await api("/v1/replacements", {
    request_id: "switch-provider",
    expected_generation: 1,
    decision,
  });
  assert.equal(replacement.stage, "retired");
  assert.equal(replacement.generation, 2);
  assert.equal((await api("/v1/runs/switch")).state, "running");
  switched = true;
  held();
  const completed = await done("switch");
  assert.equal(completed.state, "succeeded", JSON.stringify(completed));
  await api("/v1/runs", spec("isolation", "isolated", "different-chat"));
  assert.equal((await done("isolation")).state, "succeeded");
  const db = new DatabaseSync(root + "/remote.sqlite", { readOnly: true });
  const records = db
    .prepare("SELECT body FROM memory_records")
    .all()
    .map((r) => JSON.parse(r.body));
  assert.equal(records.find((r) => r.id === "note").revision, 3);
  assert.equal(
    records.find((r) => r.id === "note").owner,
    "session:" + createHash("sha256").update("chat").digest("hex"),
  );
  assert.equal(records.find((r) => r.id === "deleted").tombstone, true);
  assert(
    records.every(
      (r) =>
        r.provenance?.actor ===
        "session:" + createHash("sha256").update("chat").digest("hex"),
    ),
  );
  db.close();
  await stop(daemon);
  await start();
  assert.equal((await api("/v1/bindings")).generation, 2);
  await api("/v1/runs", spec("restart", "retained"));
  assert.equal((await done("restart")).state, "succeeded");
  console.log(
    JSON.stringify({
      passed: true,
      python_backend: false,
      node_backend: false,
      checks: [
        "shared memory tools",
        "real mutation approvals",
        "one active run survives SQLite-to-HTTP replacement",
        "explicit destination decision",
        "post-cutover write/delete/search",
        "session isolation",
        "revisions and tombstones",
        "provenance",
        "restart with same binding",
      ],
    }),
  );
} catch (error) {
  for (const log of logs) console.error(fs.readFileSync(log, "utf8"));
  throw error;
} finally {
  for (const child of children) await stop(child);
  model.closeAllConnections();
  await new Promise((r) => model.close(r));
  fs.rmSync(root, { recursive: true, force: true });
}
