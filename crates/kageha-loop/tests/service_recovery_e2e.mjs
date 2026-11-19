import { credentialFixture } from "./credential_fixture.mjs";
import { isolatedModelFixture } from "./isolated_model_fixture.mjs";
// Kill the actual Rust harness after a staging import commits but before its response arrives.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import http from "node:http";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
const repo = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "../../..",
);
const root = fs.mkdtempSync("/tmp/kageha-recovery-e2e-");
const token = "replacement-recovery-test-operator-token";
const children = [];
let base,
  memoryBase,
  firstImport = true;
let imported, releaseImport;
const importReached = new Promise((r) => (imported = r)),
  hold = new Promise((r) => (releaseImport = r));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(check) {
  for (let i = 0; i < 300; i++) {
    const value = await check();
    if (value) return value;
    await sleep(50);
  }
  throw Error("Timed out");
}
async function api(route, body) {
  const res = await fetch(base + route, {
    method: body ? "POST" : "GET",
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
    },
    body: body ? JSON.stringify(body) : undefined,
  });
  const value = await res.json();
  assert.equal(res.status, 200, JSON.stringify(value));
  return value;
}
function start(binary, args, environment, name) {
  const log = path.join(root, name + ".log");
  const child = spawn(binary, args, {
    env: environment,
    stdio: ["ignore", fs.openSync(log, "a"), fs.openSync(log, "a")],
  });
  children.push(child);
  return { child, log };
}
async function address(process, pattern) {
  return until(() => {
    if (process.child.exitCode !== null)
      throw Error(fs.readFileSync(process.log, "utf8"));
    return fs.readFileSync(process.log, "utf8").match(pattern)?.[1];
  });
}
const model = http.createServer(async (req, res) => {
  try {
    let bytes = "";
    for await (const part of req) bytes += part;
    const input = JSON.parse(bytes);
    const last = input.messages.at(-1);
    let message;
    if (last.role === "tool") {
      const result = JSON.parse(last.content);
      if (
        input.messages
          .filter((m) => m.role === "user")
          .at(-1)
          .content.includes("recall")
      ) {
        assert.equal(result.generation, 2);
        assert.equal(
          result.value.body,
          "preserved through interrupted migration",
        );
        message = { role: "assistant", content: "Recovered memory is intact." };
      } else {
        assert.equal(result.generation, 1);
        message = { role: "assistant", content: "Saved before migration." };
      }
    } else {
      const recall = input.messages
        .filter((m) => m.role === "user")
        .at(-1)
        .content.includes("recall");
      message = {
        role: "assistant",
        content: null,
        tool_calls: [
          {
            id: recall ? "recall-after-crash" : "save-before-crash",
            type: "function",
            function: {
              name: recall ? "memory_get" : "memory_put",
              arguments: JSON.stringify(
                recall
                  ? { id: "durable" }
                  : {
                      id: "durable",
                      body: "preserved through interrupted migration",
                    },
              ),
            },
          },
        ],
      };
    }
    res.setHeader("Content-Type", "application/json");
    res.end(JSON.stringify({ choices: [{ message }] }));
  } catch (error) {
    res.statusCode = 500;
    res.end(JSON.stringify({ error: { message: error.stack } }));
  }
});
const proxy = http.createServer(async (req, res) => {
  try {
    let bytes = "";
    for await (const part of req) bytes += part;
    const upstream = await fetch(memoryBase + req.url, {
      method: req.method,
      headers: {
        Authorization: `Bearer ${token}`,
        "Content-Type": "application/json",
        ...(req.headers["x-kageha-writer"]
          ? { "x-kageha-writer": req.headers["x-kageha-writer"] }
          : {}),
      },
      body: req.method === "GET" ? undefined : bytes,
    });
    const body = await upstream.text();
    if (
      firstImport &&
      req.url === "/v1/calls" &&
      bytes &&
      JSON.parse(bytes).operation === "import"
    ) {
      assert.equal(upstream.status, 200, body);
      firstImport = false;
      imported();
      await hold;
    }
    if (!res.destroyed) {
      res.writeHead(upstream.status, { "Content-Type": "application/json" });
      res.end(body);
    }
  } catch (error) {
    if (!res.destroyed) {
      res.statusCode = 502;
      res.end(JSON.stringify({ message: error.message }));
    }
  }
});
try {
  fs.mkdirSync(path.join(root, "workspace"));
  fs.mkdirSync(path.join(root, "bin"));
  fs.writeFileSync(path.join(root, "token"), token, { mode: 0o600 });
  await new Promise((r) => model.listen(0, "127.0.0.1", r));
  await new Promise((r) => proxy.listen(0, "127.0.0.1", r));
  const env = {
    PATH: path.join(root, "bin"),
    ...credentialFixture(root),
    ...isolatedModelFixture(root),
    KAGEHA_SERVER_TOKEN: token,
    KAGEHA_PROVIDER_URL: `http://127.0.0.1:${model.address().port}/v1/chat/completions`,
    KAGEHA_MODEL: "fixture",
    KAGEHA_MEMORY_RECOVERY_TOKEN: token,
  };
  const memory = start(
    process.env.KAGEHA_MEMORY_BIN ||
      path.resolve(process.env.CARGO_TARGET_DIR || path.join(repo, "target"), "debug/kageha-memory"),
    [
      "--state",
      path.join(root, "remote.sqlite"),
      "--bind",
      "127.0.0.1:0",
      "--token-file",
      path.join(root, "token"),
    ],
    env,
    "memory",
  );
  memoryBase = "http://" + (await address(memory, /memory listening on (\S+)/));
  const launch = async (name) => {
    const processInfo = start(
      process.env.KAGEHA_LOOP_BIN ||
        path.resolve(process.env.CARGO_TARGET_DIR || path.join(repo, "target"), "debug/kageha-loop"),
      [
        "serve",
        path.join(root, "workspace"),
        path.join(root, "state"),
        "127.0.0.1:0",
      ],
      env,
      name,
    );
    base = "http://" + (await address(processInfo, /listening on (\S+)/));
    return processInfo;
  };
  const harness = await launch("harness-before");
  const chat = (await api("/api/sessions", {})).session_id;
  const saved = await api("/api/chat/send", {
    session_id: chat,
    client_request_id: "before",
    message: "save this durable memory",
  });
  assert(JSON.stringify(saved).includes("Saved before migration"));
  const provider = await api("/api/services/providers", {
    id: "remote",
    configuration: {
      adapter: "memory-http",
      endpoint: `http://127.0.0.1:${proxy.address().port}`,
      credentials_ref: "env:KAGEHA_MEMORY_RECOVERY_TOKEN",
    },
  });
  const transferring = api("/api/services/replacements", {
    request_id: "interrupted",
    expected_generation: 1,
    decision: {
      destination_id: "remote",
      instance_id: provider.descriptor.instance_id,
      allow_data_transfer: true,
    },
  }).catch((error) => ({ disconnected: error.message }));
  await Promise.race([
    importReached,
    sleep(10000).then(() => {
      throw Error("Import was not reached");
    }),
  ]);
  assert.equal(
    (await api("/api/services/replacements/interrupted")).replacement.stage,
    "transferring",
  );
  const exited = new Promise((r) => harness.child.once("exit", r));
  harness.child.kill("SIGKILL");
  await exited;
  releaseImport();
  assert((await transferring).disconnected);
  await launch("harness-after");
  const recovered = (await api("/api/services/replacements/interrupted"))
    .replacement;
  assert.equal(recovered.stage, "retired", JSON.stringify(recovered));
  assert.equal((await api("/api/services")).bindings[0].generation, 2);
  const result = await api("/api/chat/send", {
    session_id: chat,
    client_request_id: "after",
    message: "recall the saved memory after recovery",
  });
  assert(
    JSON.stringify(result).includes("Recovered memory is intact"),
    JSON.stringify(result),
  );
  assert.equal(
    (await api("/api/services/replacements/interrupted/resume", {})).replacement
      .stage,
    "retired",
  );
  const exportResponse = await fetch(memoryBase + "/v1/calls", {
    method: "POST",
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ operation: "snapshot" }),
  });
  const snapshot = await exportResponse.json();
  assert.equal(snapshot.records.length, 1);
  assert.equal(snapshot.watermark, 1);
  console.log(
    JSON.stringify({
      passed: true,
      chat,
      checkpoint: "transferring",
      staging_import_response: "lost",
      generation: 2,
      application_writes: 1,
      python_backend: false,
      evidence: root,
    }),
  );
} finally {
  releaseImport();
  for (const child of children) child.kill("SIGTERM");
  model.closeAllConnections();
  proxy.closeAllConnections();
  await Promise.all([
    new Promise((r) => model.close(r)),
    new Promise((r) => proxy.close(r)),
  ]);
}
