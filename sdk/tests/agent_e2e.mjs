// Three public SDKs, one real standalone Rust runtime. No WebUI or Python backend.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import http from "node:http";
import { spawn } from "node:child_process";
import { AgentClient, AgentError } from "../typescript/client.mjs";
const root = fs.mkdtempSync("/tmp/kageha-agent-sdk-");
const token = "disposable-agent-sdk-operator-token";
const tokenFile = path.join(root, "token");
fs.writeFileSync(tokenFile, token, { mode: 0o600 });
fs.mkdirSync(path.join(root, "workspace"));
fs.mkdirSync(path.join(root, "empty-bin"));
fs.writeFileSync(path.join(root, "workspace/notes.txt"), "SDK proof héllo.");
const children = [],
  held = new Map(),
  counts = new Map();
let child,
  base,
  logIndex = 0;
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
async function until(fn) {
  for (const end = Date.now() + 20000; Date.now() < end; ) {
    const value = await fn();
    if (value) return value;
    await sleep(25);
  }
  throw Error("SDK acceptance timed out");
}
const model = http.createServer(async (req, res) => {
  let raw = "";
  for await (const b of req) raw += b;
  const body = JSON.parse(raw);
  const intent = body.messages.filter((m) => m.role === "user").at(-1).content;
  counts.set(intent, (counts.get(intent) || 0) + 1);
  const reply = (message) => {
    if (!res.destroyed) res.end(JSON.stringify({ choices: [{ message }] }));
  };
  if (intent.startsWith("hold ")) {
    held.set(intent, reply);
    return;
  }
  if (body.messages.at(-1).role === "tool") {
    reply({
      role: "assistant",
      content:
        body.messages.at(-1).content === "SDK proof héllo."
          ? "Verified SDK proof."
          : "Permission denied.",
    });
    return;
  }
  if (intent.startsWith("read ")) {
    reply({
      role: "assistant",
      content: null,
      tool_calls: [
        {
          id: "read-proof",
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
  if (intent.startsWith("continue "))
    assert(
      body.messages.some(
        (m) => m.role === "assistant" && m.content === "Verified SDK proof.",
      ),
    );
  reply({
    role: "assistant",
    content: intent.startsWith("revised ")
      ? "Steering applied."
      : "Conversation continued.",
  });
});
await new Promise((r) => model.listen(0, "127.0.0.1", r));
fs.writeFileSync(
  path.join(root, "config.json"),
  JSON.stringify({
    providers: [
      {
        endpoint: `http://127.0.0.1:${model.address().port}/v1/chat/completions`,
        model: "sdk-fixture",
        protocol: "openai_compat",
      },
    ],
    approval_tools: ["read_file"],
  }),
);
async function start() {
  const log = path.join(root, `runtime-${++logIndex}.log`);
  child = spawn(
    process.env.KAGEHA_RUNTIME_BIN ||
      path.resolve("target/debug/kageha-runtime-service"),
    [
      "--state",
      path.join(root, "state"),
      "--workspace",
      path.join(root, "workspace"),
      "--config",
      path.join(root, "config.json"),
      "--token-file",
      tokenFile,
      "--bind",
      "127.0.0.1:0",
    ],
    {
      env: { PATH: path.join(root, "empty-bin") },
      stdio: ["ignore", fs.openSync(log, "a"), fs.openSync(log, "a")],
    },
  );
  children.push(child);
  base = await until(() => {
    if (child.exitCode !== null) throw Error(fs.readFileSync(log, "utf8"));
    const match = fs
      .readFileSync(log, "utf8")
      .match(/runtime listening on (127\.0\.0\.1:\d+)/);
    return match && `http://${match[1]}`;
  });
}
async function kill() {
  if (child.exitCode === null) {
    const exit = new Promise((r) => child.once("exit", r));
    child.kill("SIGKILL");
    await exit;
  }
}
function external(binary, args, body) {
  return new Promise((resolve, reject) => {
    const process = spawn(binary, args, {
      env: {
        ...globalThis.process.env,
        PYTHONPATH: path.resolve("sdk/python"),
      },
      stdio: ["pipe", "pipe", "pipe"],
    });
    let out = "",
      err = "";
    const timer = setTimeout(() => {
      process.kill("SIGKILL");
      reject(Error("SDK driver timed out"));
    }, 25000);
    process.stdout.on("data", (b) => (out += b));
    process.stderr.on("data", (b) => (err += b));
    process.once("error", reject);
    process.once("exit", (code) => {
      clearTimeout(timer);
      if (code !== 0) return reject(Error(err || out));
      try {
        resolve(JSON.parse(out));
      } catch (e) {
        reject(Error(out + err));
      }
    });
    process.stdin.end(JSON.stringify(body));
  });
}
function transport(language, credentialFile = tokenFile) {
  return async (method, value = {}) => {
    if (language !== "typescript")
      return external(
        language === "rust"
          ? process.env.KAGEHA_AGENT_CLIENT_BIN ||
              path.resolve("target/debug/examples/client")
          : "python3",
        language === "rust"
          ? [base, credentialFile, method]
          : ["sdk/python/examples/agent_driver.py", base, credentialFile, method],
        value,
      );
    const client = new AgentClient(
      base,
      fs.readFileSync(credentialFile, "utf8").trimEnd(),
    );
    try {
      let result;
      switch (method) {
        case "info":
          result = await client.info();
          break;
        case "run":
          result = await client.run(value);
          break;
        case "status":
          result = await client.status(value.run);
          break;
        case "cancel":
          result = await client.cancel(value.run);
          break;
        case "steer":
          result = await client.steer(value.run, value.message);
          break;
        case "approvals":
          result = await client.approvals(value);
          break;
        case "approval":
          result = await client.approval(value.id);
          break;
        case "decide":
          result = await client.decide(value.id, value.decision);
          break;
        case "events":
          result = [];
          for await (const event of client.events(value.run, {
            after: value.after ?? 0,
          })) {
            result.push(event);
            if (result.length >= (value.limit ?? 1024)) break;
          }
          break;
        default:
          throw Error("Unknown SDK test method");
      }
      return { value: result };
    } catch (e) {
      if (!(e instanceof AgentError)) throw e;
      return {
        error: {
          status: e.status,
          code: e.code,
          message: e.message,
          retryable: e.retryable,
        },
      };
    }
  };
}
function spec(
  id,
  intent,
  session = id,
  messages = [{ role: "user", content: intent }],
) {
  return {
    id,
    session,
    turn: `${id}-turn`,
    request: `${id}-request`,
    agent: "main",
    objective: intent,
    parent: null,
    parent_run: null,
    depth: 0,
    input: { messages, max_model_calls: 8, timeout_ms: 60000 },
  };
}
const results = [];
try {
  await start();
  for (const language of (
    process.env.AGENT_SDK_LANGUAGES ?? "typescript,rust,python"
  ).split(",")) {
    assert(["typescript", "rust", "python"].includes(language));
    const call = transport(language);
    const badTokenFile = path.join(root, "bad-token");
    fs.writeFileSync(badTokenFile, "wrong-disposable-operator-token");
    assert.equal(
      (await transport(language, badTokenFile)("info")).error.status,
      401,
    );
    const ok = async (method, value) => {
      const reply = await call(method, value);
      assert(!reply.error, JSON.stringify(reply));
      return reply.value;
    };
    const done = (run) =>
      until(async () => {
        const record = await ok("status", { run });
        return (
          ["succeeded", "failed", "cancelled", "interrupted"].includes(
            record.state,
          ) && record
        );
      });
    assert((await ok("info")).capabilities.includes("events.resume"));
    const run = spec(
      `${language}-read`,
      `read ${language}`,
      `${language}-chat`,
    );
    await ok("run", run);
    const approval = await until(async () => {
      const page = await ok("approvals", { session: run.session });
      return page.approvals[0];
    });
    assert.equal((await ok("approval", { id: approval.id })).state, "pending");
    assert.deepEqual(
      (await ok("approvals", { session: "unrelated" })).approvals,
      [],
    );
    const independent = spec(language + "-independent", "hello " + language);
    await ok("run", independent);
    assert.equal((await done(independent.id)).state, "succeeded");
    assert.equal((await ok("approval", { id: approval.id })).state, "pending");
    const decision = {
      authority: approval.request.authority,
      expected_revision: approval.revision,
      approved: true,
      scope: "once",
    };
    assert.equal(
      (
        await call("decide", {
          id: approval.id,
          decision: { ...decision, expected_revision: approval.revision + 1 },
        })
      ).error.status,
      409,
    );
    assert(
      (
        await call("decide", {
          id: approval.id,
          decision: {
            ...decision,
            authority: { ...decision.authority, resource: "wrong-resource" },
          },
        })
      ).error,
    );
    await ok("decide", { id: approval.id, decision });
    assert.equal(
      (await done(run.id)).completion.result.message,
      "Verified SDK proof.",
    );
    const first = await ok("events", { run: run.id, limit: 1 });
    assert.equal(first.length, 1);
    const resumed = await ok("events", {
      run: run.id,
      after: first[0].sequence,
    });
    assert(
      resumed.every(
        (v, i) =>
          v.sequence > (i ? resumed[i - 1].sequence : first[0].sequence),
      ),
    );
    assert.equal(resumed.at(-1).record.state, "succeeded");
    assert.deepEqual(
      await ok("events", { run: run.id, after: resumed.at(-1).sequence }),
      [],
    );
    const prior = counts.get(`read ${language}`);
    assert.equal((await ok("run", run)).state, "succeeded");
    assert.equal(counts.get(`read ${language}`), prior);
    assert.equal(
      (await call("run", { ...run, objective: "conflicting" })).error.status,
      409,
    );
    const denial = spec(`${language}-deny`, `read denied-${language}`);
    await ok("run", denial);
    const denied = await until(
      async () =>
        (await ok("approvals", { session: denial.session })).approvals[0],
    );
    await ok("decide", {
      id: denied.id,
      decision: {
        authority: denied.request.authority,
        expected_revision: denied.revision,
        approved: false,
        scope: "once",
      },
    });
    assert.equal(
      (await done(denial.id)).completion.result.message,
      "Permission denied.",
    );
    const steering = spec(`${language}-steer`, `hold ${language}-steer`);
    await ok("run", steering);
    await until(() => held.has(steering.objective));
    await ok("steer", {
      run: steering.id,
      message: { id: `${language}-direction`, body: `revised ${language}` },
    });
    held.get(steering.objective)({ role: "assistant", content: "obsolete" });
    assert.equal(
      (await done(steering.id)).completion.result.message,
      "Steering applied.",
    );
    const cancelling = spec(`${language}-cancel`, `hold ${language}-cancel`);
    await ok("run", cancelling);
    await until(() => held.has(cancelling.objective));
    await ok("cancel", { run: cancelling.id });
    assert.equal((await done(cancelling.id)).state, "cancelled");
    held.get(cancelling.objective)({
      role: "assistant",
      content: "late obsolete",
    });
    assert.equal(
      (await ok("events", { run: cancelling.id })).at(-1).record.state,
      "cancelled",
    );
    const crashing = spec(`${language}-crash`, `hold ${language}-crash`);
    await ok("run", crashing);
    await until(() => held.has(crashing.objective));
    await kill();
    await start();
    assert.equal(
      (await ok("status", { run: crashing.id })).state,
      "interrupted",
    );
    assert.equal(counts.get(crashing.objective), 1);
    assert.equal(
      (await ok("events", { run: crashing.id })).at(-1).record.state,
      "interrupted",
    );
    assert.equal((await ok("status", { run: run.id })).state, "succeeded");
    const continuing = spec(
      `${language}-continue`,
      `continue ${language}`,
      run.session,
      [
        { role: "user", content: run.objective },
        { role: "assistant", content: "Verified SDK proof." },
        { role: "user", content: `continue ${language}` },
      ],
    );
    await ok("run", continuing);
    assert.equal(
      (await done(continuing.id)).completion.result.message,
      "Conversation continued.",
    );
    results.push({ language, passed: true });
  }
  console.log(
    JSON.stringify({
      passed: true,
      clients: results,
      python_backend: false,
      node_backend: false,
      webui: false,
      checks: [
        "run/status and exact retry",
        "conflicting id rejected",
        "approval list/status/allow/deny and stale authority/revision",
        "session isolation",
        "unrelated work continues while approval waits",
        "event close/resume/terminal ordering",
        "in-flight steering",
        "cancellation and late reply",
        "actual service crash without replay",
        "multi-turn conversation after restart",
      ],
    }),
  );
} catch (e) {
  for (const file of fs.readdirSync(root).filter((v) => v.endsWith(".log")))
    console.error(fs.readFileSync(path.join(root, file), "utf8"));
  throw e;
} finally {
  await kill();
  for (const reply of held.values())
    reply({ role: "assistant", content: "cleanup" });
  model.closeAllConnections();
  await new Promise((r) => model.close(r));
  if (!process.env.KEEP_AGENT_SDK_FIXTURE)
    fs.rmSync(root, { recursive: true, force: true });
}
