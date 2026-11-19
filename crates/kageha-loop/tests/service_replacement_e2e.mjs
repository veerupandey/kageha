// One active Kageha turn switches from embedded SQLite to the standalone Rust HTTP service.
import { credentialFixture } from "./credential_fixture.mjs";
import { isolatedModelFixture } from "./isolated_model_fixture.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import http from "node:http";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { MemoryClient } from "../../../sdk/typescript/client.mjs";
const repo = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "../../..",
);
const root = fs.mkdtempSync("/tmp/kageha-live-replacement-");
const auth = "operator-token-with-at-least-thirty-two-bytes";
const children = [];
let base,
  memoryEndpoint,
  switched = false;
const pause = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(fn) {
  for (let n = 0; n < 300; n++) {
    const result = await fn();
    if (result) return result;
    await pause(50);
  }
  throw Error("Timed out");
}
async function api(route, body) {
  const response = await fetch(base + route, {
    method: body ? "POST" : "GET",
    headers: {
      Authorization: `Bearer ${auth}`,
      "Content-Type": "application/json",
    },
    body: body ? JSON.stringify(body) : undefined,
  });
  const value = await response.json();
  assert.equal(response.status, 200, JSON.stringify(value));
  return value;
}
const fixture = http.createServer(async (req, res) => {
  try {
    let raw = "";
    for await (const chunk of req) raw += chunk;
    const input = JSON.parse(raw);
    const tools = input.messages.filter((m) => m.role === "tool");
    let message;
    if (!tools.length) {
      assert(input.tools.some((t) => t.function.name === "memory_put"));
      message = {
        role: "assistant",
        content: null,
        tool_calls: [
          {
            id: "memory-save",
            type: "function",
            function: {
              name: "memory_put",
              arguments: JSON.stringify({
                id: "portable",
                body: "the running task survived replacement",
              }),
            },
          },
        ],
      };
    } else if (tools.length === 1) {
      const before = JSON.parse(tools[0].content);
      assert.equal(before.generation, 1);
      const job = await api("/api/services/replacements", {
        request_id: "e2e-switch",
        expected_generation: 1,
        decision: {
          destination_id: "remote",
          instance_id: memoryEndpoint.instance_id,
          allow_data_transfer: true,
        },
      });
      assert.equal(job.replacement.stage, "retired", JSON.stringify(job));
      switched = true;
      message = {
        role: "assistant",
        content: null,
        tool_calls: [
          {
            id: "memory-recall",
            type: "function",
            function: {
              name: "memory_get",
              arguments: JSON.stringify({ id: "portable" }),
            },
          },
        ],
      };
    } else {
      const after = JSON.parse(tools.at(-1).content);
      assert.equal(after.generation, 2);
      assert.equal(after.value.body, "the running task survived replacement");
      message = {
        role: "assistant",
        content: "Live memory replacement passed in the same turn.",
      };
    }
    res.setHeader("Content-Type", "application/json");
    res.end(JSON.stringify({ choices: [{ message }] }));
  } catch (e) {
    res.statusCode = 500;
    res.end(JSON.stringify({ error: { message: e.stack } }));
  }
});
function launch(binary, args, env, name) {
  const log = path.join(root, name + ".log");
  const child = spawn(binary, args, {
    env,
    stdio: ["ignore", fs.openSync(log, "a"), fs.openSync(log, "a")],
  });
  children.push(child);
  return { child, log };
}
try {
  fs.mkdirSync(path.join(root, "workspace"));
  fs.mkdirSync(path.join(root, "bin"));
  fs.writeFileSync(path.join(root, "memory.token"), auth, { mode: 0o600 });
  await new Promise((r) => fixture.listen(0, "127.0.0.1", r));
  const environment = {
    PATH: path.join(root, "bin"),
    ...credentialFixture(root),
    ...isolatedModelFixture(root),
    KAGEHA_SERVER_TOKEN: auth,
    KAGEHA_PROVIDER_URL: `http://127.0.0.1:${fixture.address().port}/v1/chat/completions`,
    KAGEHA_MODEL: "fixture",
    KAGEHA_MEMORY_TEST_TOKEN: auth,
  };
  const memory = launch(
    process.env.KAGEHA_MEMORY_BIN ||
      path.join(repo, "target/debug/kageha-memory"),
    [
      "--state",
      path.join(root, "remote.sqlite"),
      "--bind",
      "127.0.0.1:0",
      "--token-file",
      path.join(root, "memory.token"),
    ],
    environment,
    "memory",
  );
  const address = await until(() => {
    if (memory.child.exitCode !== null)
      throw Error(fs.readFileSync(memory.log, "utf8"));
    return fs
      .readFileSync(memory.log, "utf8")
      .match(/memory listening on (\S+)/)?.[1];
  });
  const endpoint = `http://${address}`;
  const sdk = new MemoryClient(endpoint, auth);
  memoryEndpoint = await sdk.info();
  const server = launch(
    process.env.KAGEHA_LOOP_BIN || path.join(repo, "target/debug/kageha-loop"),
    [
      "serve",
      path.join(root, "workspace"),
      path.join(root, "state"),
      "127.0.0.1:0",
    ],
    environment,
    "harness",
  );
  base = await until(() => {
    if (server.child.exitCode !== null)
      throw Error(fs.readFileSync(server.log, "utf8"));
    const address = fs
      .readFileSync(server.log, "utf8")
      .match(/listening on (\S+)/)?.[1];
    return address && `http://${address}`;
  });
  await api("/api/services/providers", {
    id: "remote",
    configuration: {
      adapter: "memory-http",
      endpoint,
      credentials_ref: "env:KAGEHA_MEMORY_TEST_TOKEN",
    },
  });
  const chat = (await api("/api/sessions", {})).session_id;
  const result = await api("/api/chat/send", {
    session_id: chat,
    client_request_id: "live-memory",
    message: "Save and recall my memory while its implementation changes.",
  });
  assert(switched);
  assert(JSON.stringify(result).includes("Live memory replacement passed"));
  const events = (await api("/api/services/events")).events;
  assert(
    events.some(([cursor, job]) => cursor > 0 && job.stage === "checkpointed"),
  );
  const records = await sdk.call({
    operation: "search",
    access: {
      namespace: (await sdk.request("/v1/calls", { operation: "snapshot" }))
        .records[0].namespace,
      actor: "main",
    },
    query: "survived",
    limit: 10,
  });
  assert.equal(records.length, 1);
  const access = { namespace: "typescript-sdk", actor: "client" };
  await assert.rejects(
    sdk.call({
      operation: "put",
      request: {
        request_id: "stale-direct-client",
        access,
        id: "sdk-note",
        kind: "note",
        body: "must not bypass the active controller",
        expected_revision: 0,
      },
    }),
    (error) => error.code === "stale_generation",
  );
  assert.equal(
    await sdk.call({ operation: "get", access, id: "sdk-note" }),
    null,
  );
  const stopped = new Promise((resolve) => server.child.once("exit", resolve));
  server.child.kill("SIGKILL");
  await stopped;
  const restarted = launch(
    process.env.KAGEHA_LOOP_BIN || path.join(repo, "target/debug/kageha-loop"),
    [
      "serve",
      path.join(root, "workspace"),
      path.join(root, "state"),
      "127.0.0.1:0",
    ],
    environment,
    "harness-restarted",
  );
  base = await until(() => {
    if (restarted.child.exitCode !== null)
      throw Error(fs.readFileSync(restarted.log, "utf8"));
    const address = fs
      .readFileSync(restarted.log, "utf8")
      .match(/listening on (\S+)/)?.[1];
    return address && `http://${address}`;
  });
  const restored = await api("/api/services");
  assert.equal(restored.bindings[0].generation, 2);
  assert.equal(restored.bindings[0].provider_id, "remote");
  assert.deepEqual(restored.connection_errors, {});
  assert(
    JSON.stringify(await api(`/api/sessions/${chat}`)).includes(
      "Live memory replacement passed",
    ),
  );
  console.log(
    JSON.stringify({
      passed: true,
      chat,
      executions: "one uninterrupted turn",
      generation: 2,
      python_backend: false,
      restart_persistence: true,
      evidence: root,
    }),
  );
} finally {
  for (const child of children) child.kill("SIGTERM");
  await new Promise((r) => fixture.close(r));
}
