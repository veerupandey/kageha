// Real Chromium through the standalone runtime's exact-approval browser adapter.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import http from "node:http";
import { spawn } from "node:child_process";
import { once } from "node:events";
const root = fs.mkdtempSync("/tmp/kageha-runtime-browser-"),
  token = "disposable-browser-runtime-auth-token";
const browserBin =
    process.env.KAGEHA_BROWSER_BIN ||
    path.resolve("target/debug/kageha-browser"),
  runtimeBin =
    process.env.KAGEHA_RUNTIME_BIN ||
    path.resolve("target/debug/kageha-runtime-service");
const children = [];
let nextCall, base;
const changed = [];
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(fn) {
  const end = Date.now() + 15000;
  while (Date.now() < end) {
    const v = await fn();
    if (v) return v;
    await sleep(20);
  }
  throw Error("Condition timed out; fixture " + root);
}
const page = http.createServer((req, res) => {
  if (req.url.startsWith("/changed?")) {
    changed.push(decodeURIComponent(req.url.slice(9)));
    res.end("ok");
    return;
  }
  res.setHeader("content-type", "text/html");
  res.end(
    "<title>Runtime browser</title><label>Name<input oninput=\"fetch('/changed?'+encodeURIComponent(this.value))\"></label>",
  );
});
const model = http.createServer(async (req, res) => {
  let raw = "";
  for await (const c of req) raw += c;
  const body = JSON.parse(raw),
    last = body.messages.at(-1);
  const message =
    last.role === "tool"
      ? { role: "assistant", content: last.content }
      : {
          role: "assistant",
          content: null,
          tool_calls: [
            {
              id: "browser-action",
              type: "function",
              function: {
                name: nextCall.name,
                arguments: JSON.stringify(nextCall.arguments),
              },
            },
          ],
        };
  res.setHeader("content-type", "application/json");
  res.end(JSON.stringify({ choices: [{ message }] }));
});
async function api(route, body) {
  const r = await fetch(base + route, {
    method: body ? "POST" : "GET",
    headers: {
      authorization: `Bearer ${token}`,
      "content-type": "application/json",
    },
    body: body ? JSON.stringify(body) : undefined,
    signal: AbortSignal.timeout(15000),
  });
  const text = await r.text();
  assert.equal(r.status, 200, text);
  return JSON.parse(text);
}
function launch(binary, args, env) {
  const log = fs.openSync(path.join(root, path.basename(binary) + ".log"), "a");
  const child = spawn(binary, args, { env, stdio: ["ignore", log, log] });
  children.push(child);
  return child;
}
let n = 0;
async function run(name, args, { session = "one", approve = true } = {}) {
  const id = "case-" + ++n;
  nextCall = { name, arguments: args };
  await api("/v1/runs", {
    id,
    session,
    turn: "turn-" + id,
    request: id,
    parent: null,
    parent_run: null,
    agent: "main",
    objective: "Browser fixture " + id,
    depth: 0,
    input: {
      messages: [{ role: "user", content: "Browser fixture " + id }],
      max_model_calls: 4,
      timeout_ms: 15000,
    },
  });
  await until(async () => {
    const records = (await api("/v1/approvals?turn=turn-" + id)).approvals;
    for (const r of records)
      await api(`/v1/approvals/${r.id}/decision`, {
        expected_revision: r.revision,
        authority: r.request.authority,
        approved: approve,
        scope: "once",
      });
    const state = await api("/v1/runs/" + id);
    return (
      ["succeeded", "failed", "interrupted", "cancelled"].includes(
        state.state,
      ) && state
    );
  });
  const result = await api("/v1/runs/" + id);
  assert.equal(result.state, "succeeded", JSON.stringify(result));
  return result.completion.result.message;
}
try {
  fs.mkdirSync(path.join(root, "workspace"));
  fs.mkdirSync(path.join(root, "empty-bin"));
  fs.writeFileSync(path.join(root, "token"), token, { mode: 0o600 });
  page.listen(0, "127.0.0.1");
  model.listen(0, "127.0.0.1");
  await Promise.all([once(page, "listening"), once(model, "listening")]);
  const browser = launch(
    browserBin,
    [
      "--state-dir",
      path.join(root, "browser"),
      "--instance-id",
      "runtime-fixture",
      "--application-id",
      "standalone-runtime",
      "--chromium",
      process.env.CHROMIUM_BIN ||
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
      "--standalone",
      "on",
    ],
    process.env,
  );
  await until(() => {
    assert.equal(browser.exitCode, null);
    return fs.existsSync(path.join(root, "browser/daemon.json"));
  });
  fs.writeFileSync(
    path.join(root, "config.json"),
    JSON.stringify({
      providers: [
        {
          endpoint: `http://127.0.0.1:${model.address().port}/v1/chat/completions`,
          model: "fixture",
          protocol: "openai_compat",
        },
      ],
      browser: { state_dir: path.join(root, "browser") },
    }),
  );
  const runtime = launch(
    runtimeBin,
    [
      "--state",
      path.join(root, "state"),
      "--workspace",
      path.join(root, "workspace"),
      "--config",
      path.join(root, "config.json"),
      "--token-file",
      path.join(root, "token"),
      "--bind",
      "127.0.0.1:0",
    ],
    { PATH: path.join(root, "empty-bin") },
  );
  base = await until(() => {
    assert.equal(
      runtime.exitCode,
      null,
      fs.readFileSync(path.join(root, "kageha-runtime-service.log"), "utf8"),
    );
    const m = fs
      .readFileSync(path.join(root, "kageha-runtime-service.log"), "utf8")
      .match(/listening on (127\.0\.0\.1:\d+)\r?\n/);
    return m && "http://" + m[1];
  });
  const catalog = await api("/v1/tools");
  assert(catalog.tools.some((t) => t.function.name === "browser_fill"));
  assert(!catalog.tools.some((t) => t.function.name === "browser_upload"));
  const opened = JSON.parse(
    await run("browser_open", {
      url: `http://127.0.0.1:${page.address().port}/`,
    }),
  );
  assert(opened.result.target_id);
  let observed = JSON.parse(await run("browser_snapshot", {})).result;
  const field = observed.controls.find((c) => c.name === "Name");
  assert(field, JSON.stringify(observed));
  const fill = JSON.parse(
    await run("browser_fill", {
      observation_id: observed.observation_id,
      target: field.ref,
      text: "Verified runtime fill",
    }),
  );
  assert(fill.request_id.startsWith("tool-"));
  await until(() => changed.includes("Verified runtime fill"));
  const receipt = JSON.parse(
    await run("browser_receipt", { request_id: fill.request_id }),
  );
  assert(JSON.stringify(receipt).includes("verified"));
  observed = JSON.parse(await run("browser_snapshot", {})).result;
  const key = JSON.parse(await run("browser_press", {
    observation_id: observed.observation_id, key: "Tab",
  }));
  assert(key.request_id.startsWith("tool-"));
  observed = JSON.parse(await run("browser_snapshot", {})).result;
  const target = observed.controls.find((c) => c.name === "Name").ref;
  await run(
    "browser_fill",
    { observation_id: observed.observation_id, target, text: "Denied input" },
    { approve: false },
  );
  assert(!changed.includes("Denied input"));
  const isolated = await run("browser_tabs", {}, { session: "other" });
  assert(
    !isolated.includes(opened.result.target_id),
    "Another session must not see the first session tab",
  );
  const original = await run("browser_tabs", {});
  assert(
    original.includes(opened.result.target_id),
    "Second session did not erase original tabs",
  );
  console.log(
    JSON.stringify({
      passed: true,
      python_backend: false,
      checks: [
        "standalone runtime browser catalog",
        "approved private navigation",
        "full observation",
        "actual Chromium fill",
        "durable receipt lookup",
        "native focused keyboard input",
        "denial prevents input",
        "cross-session tab isolation",
      ],
      fixture: root,
    }),
  );
} finally {
  for (const child of children.reverse()) {
    if (child.exitCode === null && child.signalCode === null) {
      child.kill("SIGTERM");
      await Promise.race([once(child, "exit"), sleep(2000)]);
      if (child.exitCode === null && child.signalCode === null)
        child.kill("SIGKILL");
    }
  }
  for (const server of [page, model]) {
    server.closeAllConnections();
    await new Promise((r) => server.close(r));
  }
  if (!process.env.KEEP_BROWSER_RUNTIME_FIXTURE)
    fs.rmSync(root, { recursive: true, force: true });
}
