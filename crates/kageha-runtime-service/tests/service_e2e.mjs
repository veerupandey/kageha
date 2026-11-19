import { credentialFixture } from "../../kageha-loop/tests/credential_fixture.mjs";
import { isolatedModelFixture } from "../../kageha-loop/tests/isolated_model_fixture.mjs";
// The same real model/tool flow runs embedded through WebUI and in an independent service.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import http from "node:http";
import { spawn } from "node:child_process";
const runtime =
  process.env.KAGEHA_RUNTIME_BIN ||
  path.resolve("target/debug/kageha-runtime-service");
const app =
  process.env.KAGEHA_LOOP_BIN || path.resolve("target/debug/kageha-loop");
const root = fs.mkdtempSync("/tmp/kageha-execution-ports-");
const token = "runtime-fixture-disposable-access-token";
const pause = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(fn) {
  for (let end = Date.now() + 15000; Date.now() < end; ) {
    const result = await fn();
    if (result) return result;
    await pause(20);
  }
  throw Error("Condition timed out");
}
const requests = [],
  held = new Map(),
  children = [],
  logs = [];
let runtimeBase,
  appBase,
  daemon,
  fallbackCalls = 0;
const model = http.createServer(async (req, res) => {
  let text = "";
  for await (const chunk of req) text += chunk;
  const body = JSON.parse(text);
  if (
    req.url === "/unavailable" ||
    req.url === "/unavailable/chat/completions"
  ) {
    fallbackCalls++;
    // Exercise bounded transient retries before fallback without relying on the
    // production default backoff to fit inside a fixture's polling deadline.
    res.writeHead(503, { "retry-after": "1" });
    res.end("{}");
    return;
  }
  requests.push(body);
  const intent = body.messages.filter((m) => m.role === "user").at(-1)?.content;
  const reply = (message) => {
    if (!res.destroyed) res.end(JSON.stringify({ choices: [{ message }] }));
  };
  if (intent.startsWith("hold ")) {
    held.set(intent, reply);
    req.on("close", () => {});
    return;
  }
  if (body.messages.at(-1)?.role === "tool") {
    assert.equal(body.messages.at(-1).content, "Portable workspace proof.");
    reply({ role: "assistant", content: "Read proof." });
    return;
  }
  if (intent === "read the proof") {
    assert(body.tools.some((t) => t.function.name === "read_file"));
    reply({
      role: "assistant",
      content: null,
      tool_calls: [
        {
          id: "read-one",
          type: "function",
          function: {
            name: "read_file",
            arguments: JSON.stringify({ path: "notes.txt" }),
          },
        },
      ],
    });
    return;
  }
  reply({
    role: "assistant",
    content:
      intent === "revised instructions" ? "Steering applied." : "Continued.",
  });
});
await new Promise((r) => model.listen(0, "127.0.0.1", r));
const endpoint = `http://127.0.0.1:${model.address().port}/v1/chat/completions`;
async function start(kind) {
  const log = path.join(root, `${kind}-${logs.length}.log`);
  logs.push(log);
  const binary = kind === "runtime" ? runtime : app;
  const args =
    kind === "runtime"
      ? [
          "--state",
          path.join(root, "runtime"),
          "--workspace",
          path.join(root, "workspace"),
          "--config",
          path.join(root, "config.json"),
          "--token-file",
          path.join(root, "token"),
          "--bind",
          "127.0.0.1:0",
        ]
      : [
          "serve",
          path.join(root, "workspace"),
          path.join(root, "webui"),
          "127.0.0.1:0",
        ];
  const child = spawn(binary, args, {
    env: {
      PATH: path.join(root, "empty-bin"),
      ...credentialFixture(root),
      ...isolatedModelFixture(root),
      KAGEHA_SERVER_TOKEN: token,
      KAGEHA_PROVIDER_URL: endpoint,
      KAGEHA_MODEL: "fixture",
    },
    stdio: ["ignore", fs.openSync(log, "a"), fs.openSync(log, "a")],
  });
  children.push(child);
  const base = await until(() => {
    if (child.exitCode !== null) throw Error(fs.readFileSync(log, "utf8"));
    const match = fs
      .readFileSync(log, "utf8")
      .match(/listening on (127\.0\.0\.1:\d+)\r?\n/);
    return match && "http://" + match[1];
  });
  if (kind === "runtime") {
    daemon = child;
    runtimeBase = base;
  } else appBase = base;
}
async function api(base, route, body, expected = 200, extra = {}) {
  const res = await fetch(base + route, {
    method: body ? "POST" : "GET",
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
      ...extra,
    },
    body: body ? JSON.stringify(body) : undefined,
    signal: AbortSignal.timeout(15000),
  });
  const bytes = await res.text();
  assert.equal(res.status, expected, bytes);
  return bytes ? JSON.parse(bytes) : null;
}
const spec = (id, message) => ({
  id,
  session: "portable-chat",
  turn: "turn-" + id,
  request: id,
  parent: null,
  parent_run: null,
  agent: "main",
  objective: message,
  depth: 0,
  input: {
    messages: [{ role: "user", content: message }],
    max_model_calls: 8,
    timeout_ms: 120000,
  },
});
async function done(id) {
  return until(async () => {
    const run = await api(runtimeBase, "/v1/runs/" + id);
    return (
      ["succeeded", "cancelled", "failed", "interrupted"].includes(run.state) &&
      run
    );
  });
}
async function stop(child, signal = "SIGKILL") {
  if (child && child.exitCode === null && child.signalCode === null) {
    child.kill(signal);
    await new Promise((r) => child.once("exit", r));
  }
}
try {
  fs.mkdirSync(path.join(root, "workspace"));
  fs.mkdirSync(path.join(root, "empty-bin"));
  fs.writeFileSync(
    path.join(root, "workspace/notes.txt"),
    "Portable workspace proof.",
  );
  fs.writeFileSync(path.join(root, "token"), token, { mode: 0o600 });
  fs.writeFileSync(
    path.join(root, "config.json"),
    JSON.stringify({
      providers: [
        {
          endpoint: `http://127.0.0.1:${model.address().port}/unavailable`,
          model: "unavailable",
          protocol: "openai_compat",
        },
        { endpoint, model: "fixture", protocol: "openai_compat" },
      ],
    }),
  );
  if (process.env.KAGEHA_TEST_MODEL_CATALOG === "1") {
    const catalog = path.join(root, "models.yaml");
    fs.writeFileSync(
      catalog,
      JSON.stringify({
        providers: {
          unavailable: {
            base_url: `http://127.0.0.1:${model.address().port}/unavailable`,
            protocol: "openai_compat",
          },
          fixture: { base_url: endpoint, protocol: "openai_compat" },
        },
        models: [
          { id: "first", provider: "unavailable", model: "unavailable" },
          { id: "second", provider: "fixture", model: "fixture" },
        ],
      }),
    );
    fs.writeFileSync(
      path.join(root, "config.json"),
      JSON.stringify({
        catalog_files: [catalog],
        catalog_models: ["first", "second"],
      }),
    );
  }
  await start("runtime");
  await start("webui");
  assert.equal((await fetch(runtimeBase + "/v1/info")).status, 401);
  const info = await api(runtimeBase, "/v1/info");
  assert(info.capabilities.includes("events.resume"));
  assert.equal(info.live_replacement, false);
  const standalone = spec("standalone", "read the proof");
  await api(runtimeBase, "/v1/runs", standalone);
  const result = await done(standalone.id);
  assert.equal(result.state, "succeeded");
  assert.equal(result.completion.result.message, "Read proof.");
  assert(fallbackCalls >= 2, "Configured fallback supplies complete replies");
  const count = requests.length,
    beforeRetryFallbacks = fallbackCalls;
  await api(runtimeBase, "/v1/runs", standalone);
  assert.equal(requests.length, count, "Same request cannot execute twice");
  assert.equal(fallbackCalls, beforeRetryFallbacks);
  await api(
    runtimeBase,
    "/v1/runs",
    { ...standalone, id: "aliased-run", session: "other-chat" },
    409,
  );
  await api(
    runtimeBase,
    "/v1/runs",
    { ...standalone, objective: "changed input" },
    409,
  );
  await api(
    runtimeBase,
    "/v1/runs",
    {
      ...spec("override", "read the proof"),
      input: { ...standalone.input, workspace: "/tmp" },
    },
    400,
  );
  const chat = (await api(appBase, "/api/sessions", {})).session_id;
  const answer = await api(appBase, "/api/chat/send", {
    session_id: chat,
    client_request_id: "embedded",
    message: "read the proof",
  });
  assert.equal(answer.status, "success");
  assert.equal(answer.message, result.completion.result.message);
  // Durable SSE must replay ordered records and honor Last-Event-ID without duplicates.
  const events = await fetch(runtimeBase + "/v1/runs/standalone/events", {
    headers: { Authorization: `Bearer ${token}` },
    signal: AbortSignal.timeout(5000),
  }).then((r) => r.text());
  const frames = events
    .split("\n\n")
    .filter((s) => s.includes("data:"))
    .map((s) =>
      JSON.parse(
        s
          .split("\n")
          .find((l) => l.startsWith("data:"))
          .slice(5),
      ),
    );
  assert(frames.length >= 2);
  assert.equal(frames.at(-1).record.state, "succeeded");
  assert(
    frames.every((f, i) => i === 0 || f.sequence > frames[i - 1].sequence),
  );
  const cursor = frames[0].sequence;
  const resumed = await fetch(runtimeBase + "/v1/runs/standalone/events", {
    headers: {
      Authorization: `Bearer ${token}`,
      "Last-Event-ID": String(cursor),
    },
    signal: AbortSignal.timeout(5000),
  }).then((r) => r.text());
  assert(!resumed.includes(`id: ${cursor}\n`));
  assert(resumed.includes(`id: ${frames.at(-1).sequence}\n`));
  const steered = spec("steering", "hold steering");
  await api(runtimeBase, "/v1/runs", steered);
  await until(() => held.has("hold steering"));
  const message = { id: "direction", body: "revised instructions" };
  await api(runtimeBase, "/v1/runs/steering/steer", message);
  await api(runtimeBase, "/v1/runs/steering/steer", message);
  held.get("hold steering")({
    role: "assistant",
    content: null,
    tool_calls: [
      { id: "obsolete", function: { name: "read_file", arguments: "{}" } },
    ],
  });
  assert.equal(
    (await done("steering")).completion.result.message,
    "Steering applied.",
  );
  assert(
    !requests.some((r) =>
      r.messages.some((m) => m.tool_call_id === "obsolete"),
    ),
  );
  await api(runtimeBase, "/v1/runs", spec("cancel", "hold cancel"));
  await until(() => held.has("hold cancel"));
  await api(runtimeBase, "/v1/runs/cancel/cancel", {});
  assert.equal((await done("cancel")).state, "cancelled");
  const deadline = { ...spec("deadline", "hold timeout") };
  deadline.input.timeout_ms = 100;
  await api(runtimeBase, "/v1/runs", deadline);
  const expired = await done("deadline");
  assert.equal(expired.state, "failed");
  assert.equal(expired.completion.result.error_code, "deadline");
  // Kill after admission to a model call. Restart must not replay an unknown execution.
  await api(runtimeBase, "/v1/runs", spec("crash", "hold crash"));
  await until(() => held.has("hold crash"));
  const beforeCrash = requests.length;
  await stop(daemon);
  await start("runtime");
  assert.equal((await done("crash")).state, "interrupted");
  assert.equal(requests.length, beforeCrash);
  assert.equal(
    (await api(runtimeBase, "/v1/runs/standalone")).completion.result.message,
    "Read proof.",
  );
  await api(runtimeBase, "/v1/runs", spec("after", "read the proof"));
  assert.equal((await done("after")).state, "succeeded");
  // Existing WebUI conversation still works with its own independent state.
  await api(appBase, "/api/chat/send", {
    session_id: chat,
    client_request_id: "after",
    message: "continue",
  });
  assert(
    (await api(appBase, `/api/sessions/${chat}`)).messages.some(
      (m) => m.text === "Read proof.",
    ),
  );
  await api(runtimeBase, "/v1/runs", spec("drain", "hold drain"));
  await until(() => held.has("hold drain"));
  await stop(daemon, "SIGTERM");
  assert.equal(daemon.exitCode, 0);
  await start("runtime");
  assert.equal((await done("drain")).state, "cancelled");
  await stop(daemon, "SIGTERM");
  console.log(
    JSON.stringify({
      passed: true,
      python_backend: false,
      node_backend: false,
      curl_model_processes: 0,
      embedded_and_service: "same model/tool engine and outcome",
      checks: [
        "provider fallback",
        "authentication",
        "idempotency",
        "conflicting retry",
        "workspace binding isolation",
        "ordered resumable SSE",
        "in-flight steering",
        "cancellation",
        "deadline",
        "crash recovery without replay",
        "existing WebUI history",
        "graceful drain",
      ],
      fixture: root,
    }),
  );
} catch (error) {
  console.error(error);
  for (const log of logs) console.error(fs.readFileSync(log, "utf8"));
  process.exitCode = 1;
} finally {
  for (const child of children) await stop(child);
  model.closeAllConnections();
  await new Promise((r) => model.close(r));
}
