// Independent workspace library + worker composed into the standalone runtime. Backend PATH is empty.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import http from "node:http";
import { spawn, execFileSync } from "node:child_process";
const root = fs.mkdtempSync("/tmp/kageha-workspace-host-"),
  token = "disposable-workspace-operator-access-token";
const pause = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(fn) {
  for (let end = Date.now() + 20000; Date.now() < end; ) {
    const value = await fn();
    if (value) return value;
    await pause(20);
  }
  throw Error("Condition timed out");
}
let child, base;
const logs = [];
const worker = path.resolve(process.env.CARGO_TARGET_DIR || "target", "debug/kageha-workspace");
const model = http.createServer(async (req, res) => {
  let raw = "";
  for await (const b of req) raw += b;
  const body = JSON.parse(raw);
  const intent = body.messages.filter((m) => m.role === "user").at(-1).content;
  let message;
  if (body.messages.at(-1).role === "tool")
    message = { role: "assistant", content: body.messages.at(-1).content };
  else {
    const file =
      intent === "text"
        ? "notes.txt"
        : intent === "outside"
          ? "../secret"
          : intent === "replace"
            ? "replace.docx"
            : "evidence.docx";
    const name = ["text", "outside"].includes(intent)
      ? "read_file"
      : "read_document";
    assert(body.tools.some((t) => t.function.name === name));
    message = {
      role: "assistant",
      content: null,
      tool_calls: [
        {
          id: "read",
          type: "function",
          function: { name, arguments: JSON.stringify({ path: file }) },
        },
      ],
    };
  }
  res.end(JSON.stringify({ choices: [{ message }] }));
});
await new Promise((r) => model.listen(0, "127.0.0.1", r));
async function start() {
  const log = root + "/host-" + logs.length + ".log";
  logs.push(log);
  child = spawn(
    path.resolve(process.env.CARGO_TARGET_DIR || "target", "debug/kageha-runtime-service"),
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
    {
      env: { PATH: root + "/empty" },
      stdio: ["ignore", fs.openSync(log, "a"), fs.openSync(log, "a")],
    },
  );
  base = await until(() => {
    if (child.exitCode !== null) throw Error(fs.readFileSync(log, "utf8"));
    const match = fs.readFileSync(log, "utf8").match(/listening on (127\.0\.0\.1:\d+)\r?\n/);
    return match && "http://" + match[1];
  });
}
async function stop() {
  if (child && child.exitCode === null && child.signalCode === null) {
    child.kill("SIGTERM");
    await new Promise((r) => child.once("exit", r));
  }
}
async function api(route, body) {
  const response = await fetch(base + route, {
    method: body ? "POST" : "GET",
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
    },
    body: body ? JSON.stringify(body) : undefined,
    signal: AbortSignal.timeout(15000),
  });
  const data = await response.json();
  assert.equal(response.status, 200, JSON.stringify(data));
  return data;
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
      timeout_ms: 30000,
    },
  });
}
async function done(id) {
  return until(async () => {
    const run = await api("/v1/runs/" + id);
    return (
      ["succeeded", "failed", "cancelled", "interrupted"].includes(run.state) &&
      run
    );
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
  await api(`/v1/approvals/${record.id}/decision`, {
    expected_revision: record.revision,
    authority: record.request.authority,
    approved: true,
    scope: "once",
  });
}
try {
  fs.mkdirSync(root + "/workspace");
  fs.mkdirSync(root + "/empty");
  fs.mkdirSync(root + "/office/word", { recursive: true });
  fs.writeFileSync(root + "/workspace/notes.txt", "Portable workspace text");
  fs.writeFileSync(root + "/secret", "OUTSIDE-SECRET");
  fs.writeFileSync(root + "/token", token, { mode: 0o600 });
  fs.writeFileSync(
    root + "/office/word/document.xml",
    '<w:document xmlns:w="urn:w"><w:p><w:r><w:t>Independent document evidence</w:t></w:r></w:p></w:document>',
  );
  execFileSync(
    "/usr/bin/zip",
    ["-q", "-r", root + "/workspace/evidence.docx", "word"],
    { cwd: root + "/office" },
  );
  fs.copyFileSync(
    root + "/workspace/evidence.docx",
    root + "/workspace/replace.docx",
  );
  const config = {
    providers: [
      {
        endpoint: `http://127.0.0.1:${model.address().port}/model`,
        protocol: "openai_compat",
        model: "fixture",
      },
    ],
    workspace_worker: worker,
    approval_tools: ["read_document"],
  };
  fs.writeFileSync(root + "/config.json", JSON.stringify(config));
  assert.equal(
    execFileSync(worker, ["read", root + "/workspace", "notes.txt"], {
      env: { PATH: root + "/empty" },
      encoding: "utf8",
    }),
    "Portable workspace text",
  );
  await start();
  await submit("text", "text");
  assert.equal(
    (await done("text")).completion.result.message,
    "Portable workspace text",
  );
  await submit("document", "document");
  const permission = await pending("document");
  assert.equal(
    permission.request.authority.binding,
    "tools.workspace.documents",
  );
  assert.equal(permission.request.authority.resource, "evidence.docx");
  await approve(permission);
  assert(
    (await done("document")).completion.result.message.includes(
      "Independent document evidence",
    ),
  );
  await submit("outside", "outside");
  const outside = (await done("outside")).completion.result.message;
  assert(!outside.includes("OUTSIDE-SECRET"));
  assert(outside.includes("escapes root"));
  await submit("replace", "replace");
  const old = await pending("replace");
  fs.writeFileSync(root + "/workspace/new.docx", "UNAPPROVED-REPLACEMENT");
  fs.renameSync(root + "/workspace/new.docx", root + "/workspace/replace.docx");
  await approve(old);
  const replaced = (await done("replace")).completion.result.message;
  assert(replaced.includes("changed"));
  assert(!replaced.includes("UNAPPROVED-REPLACEMENT"));
  await stop();
  delete config.workspace_worker;
  config.approval_tools = [];
  fs.writeFileSync(root + "/config.json", JSON.stringify(config));
  await start();
  assert(
    !(await api("/v1/tools")).tools.some(
      (t) => t.function.name === "read_document",
    ),
  );
  await submit("after-restart", "text");
  assert.equal(
    (await done("after-restart")).completion.result.message,
    "Portable workspace text",
  );
  console.log(
    JSON.stringify({
      passed: true,
      python_backend: false,
      node_backend: false,
      checks: [
        "independent library CLI",
        "native document worker",
        "real document approval",
        "outside path denied",
        "replaced file authority fenced",
        "optional worker capability reporting",
        "restart and read compatibility",
      ],
    }),
  );
} catch (error) {
  for (const log of logs) console.error(fs.readFileSync(log, "utf8"));
  throw error;
} finally {
  await stop();
  model.closeAllConnections();
  await new Promise((r) => model.close(r));
  fs.rmSync(root, { recursive: true, force: true });
}
