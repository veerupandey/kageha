// Standalone Rust composition with real stdio and HTTP MCP adapters. Node is only the test driver/model fixture.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import http from "node:http";
import { spawn } from "node:child_process";
import { DatabaseSync } from "node:sqlite";
import { createHash } from "node:crypto";
const root = fs.mkdtempSync("/tmp/kageha-mcp-host-"),
  token = "disposable-mcp-host-fixture-authentication";
const binary = path.resolve(process.env.CARGO_TARGET_DIR || "target", "debug/kageha-runtime-service"),
  fixture = path.resolve(process.env.CARGO_TARGET_DIR || "target", "debug/examples/fixture");
let child,
  base,
  initializations = 0,
  remoteCalls = 0;
const logs = [];
const sessions = new Map();
const pause = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(fn) {
  for (let end = Date.now() + 15000; Date.now() < end; ) {
    let v = await fn();
    if (v) return v;
    await pause(20);
  }
  throw Error("Condition timed out");
}
const id = (server, name) =>
  "mcp_" +
  createHash("sha256").update(`${server}:${name}`).digest("hex").slice(0, 32);
const tools = {
  stdio: id("local", "count"),
  http: id("remote", "echo"),
  crash: id("local", "crash"),
  receipts: "mcp_receipts",
  file: "read_file",
};
const server = http.createServer(async (req, res) => {
  let bytes = "";
  for await (const b of req) bytes += b;
  if (req.method === "DELETE") {
    res.end("{}");
    return;
  }
  const body = JSON.parse(bytes);
  if (req.url === "/mcp") {
    if (body.method === "initialize") {
      const session = "fixture-" + ++initializations;
      sessions.set(session, 0);
      res.setHeader("Mcp-Session-Id", session);
      res.end(
        JSON.stringify({
          jsonrpc: "2.0",
          id: body.id,
          result: {
            protocolVersion: "2024-11-05",
            capabilities: { tools: {} },
          },
        }),
      );
      return;
    }
    const session = req.headers["mcp-session-id"];
    assert(sessions.has(session));
    if (!body.id) {
      res.writeHead(202);
      res.end();
      return;
    }
    const result =
      body.method === "tools/list"
        ? { tools: [{ name: "echo", inputSchema: { type: "object" } }] }
        : {
            content: [
              { type: "text", text: String(sessions.get(session) + 1) },
            ],
          };
    if (body.method === "tools/call") {
      remoteCalls++;
      sessions.set(session, sessions.get(session) + 1);
    }
    // Split CRLF SSE event and keep stream alive: client must return before EOF.
    res.setHeader("Content-Type", "text/event-stream");
    const frame =
      "data: " +
      JSON.stringify({ jsonrpc: "2.0", id: body.id, result }) +
      "\r\n\r\n";
    res.write(frame.slice(0, 15));
    setTimeout(() => res.write(frame.slice(15)), 5);
    res.on("close", () => res.end());
    return;
  }
  const intent = body.messages.filter((m) => m.role === "user").at(-1).content;
  let message;
  if (body.messages.at(-1).role === "tool")
    message = { role: "assistant", content: body.messages.at(-1).content };
  else if (intent === "parallel") {
    message = {
      role: "assistant",
      content: null,
      tool_calls: ["http", "stdio", "file"].map((kind) => ({
        id: "parallel-" + kind,
        type: "function",
        function: {
          name: tools[kind],
          arguments: JSON.stringify(
            kind === "file" ? { path: "notes.txt" } : {},
          ),
        },
      })),
    };
  } else {
    const name = tools[intent];
    assert(body.tools.some((t) => t.function.name === name));
    message = {
      role: "assistant",
      content: null,
      tool_calls: [
        {
          id: "call-" + intent,
          type: "function",
          function: {
            name,
            arguments: JSON.stringify(
              intent === "file" ? { path: "notes.txt" } : {},
            ),
          },
        },
      ],
    };
  }
  res.end(JSON.stringify({ choices: [{ message }] }));
});
await new Promise((r) => server.listen(0, "127.0.0.1", r));
const endpoint = `http://127.0.0.1:${server.address().port}`;
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
  return JSON.parse(text);
}
async function start() {
  const log = path.join(root, "host-" + logs.length + ".log");
  logs.push(log);
  child = spawn(
    binary,
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
    let m = fs.readFileSync(log, "utf8").match(/listening on ([^\s]+)/);
    return m && "http://" + m[1];
  });
}
async function stop() {
  if (child && child.exitCode === null && child.signalCode === null) {
    child.kill("SIGTERM");
    await new Promise((r) => child.once("exit", r));
  }
}
async function run(n, intent, session = "chat", approve = false) {
  const remoteBefore = remoteCalls;
  await api("/v1/runs", {
    id: n,
    session,
    turn: "turn-" + n,
    request: n,
    agent: "main",
    depth: 0,
    parent: null,
    parent_run: null,
    objective: intent,
    input: {
      messages: [{ role: "user", content: intent }],
      max_model_calls: 3,
      timeout_ms: 10000,
    },
  });
  if (approve) {
    const record = await until(
      async () => (await api("/v1/approvals?turn=turn-" + n)).approvals[0],
    );
    assert.equal(record.request.authority.binding, "mcp.remote");
    assert.equal(remoteCalls, remoteBefore);
    if (intent === "parallel") {
      // The first call is waiting for approval; an independent server still completes.
      await until(() => {
        const db = new DatabaseSync(root + "/state/mcp.sqlite", {
          readOnly: true,
        });
        try {
          return db
            .prepare("SELECT result FROM receipts WHERE turn=? AND id=?")
            .get("turn-" + n, "parallel-stdio")?.result;
        } finally {
          db.close();
        }
      });
      assert.equal(
        remoteCalls,
        remoteBefore,
        "approval still prevents remote input",
      );
    }
    await api(`/v1/approvals/${record.id}/decision`, {
      expected_revision: record.revision,
      authority: record.request.authority,
      approved: true,
      scope: "once",
    });
  }
  return until(async () => {
    let r = await api("/v1/runs/" + n);
    return (
      ["succeeded", "failed", "cancelled", "interrupted"].includes(r.state) && r
    );
  });
}
try {
  fs.mkdirSync(root + "/workspace");
  fs.mkdirSync(root + "/empty");
  fs.writeFileSync(root + "/workspace/notes.txt", "workspace still works");
  fs.writeFileSync(root + "/token", token, { mode: 0o600 });
  fs.writeFileSync(
    root + "/config.json",
    JSON.stringify({
      providers: [
        {
          endpoint: endpoint + "/model",
          model: "fixture",
          protocol: "openai_compat",
        },
      ],
      mcp_servers: [
        {
          id: "local",
          transport: "stdio",
          command: fixture,
          ...(process.platform === "darwin" ? {sandbox:{read_roots:[],write_roots:[],network:false}} : {}),
          read_only_tools: ["count", "crash"],
        },
        { id: "remote", transport: "streamable_http", url: endpoint + "/mcp" },
      ],
    }),
  );
  await start();
  assert(
    (await api("/v1/tools")).tools.some((t) => t.function.name === tools.http),
  );
  assert.equal((await run("one", "stdio")).state, "succeeded");
  const second = await run("two", "stdio");
  assert.equal(second.state, "succeeded");
  assert.equal(
    JSON.parse(second.completion.result.message).content[0].text,
    "2",
  ); // persistent stdio process across turns
  assert.equal((await run("http", "http", "chat", true)).state, "succeeded");
  assert.equal(remoteCalls, 1);
  const parallel = await run("fanout", "parallel", "fanout-chat", true);
  assert.equal(parallel.completion.result.message, "workspace still works");
  assert.equal(remoteCalls, 2);
  await run("lost", "crash");
  await run("blocked", "stdio");
  let blocked = await api("/v1/runs/blocked");
  assert(JSON.stringify(blocked.completion).includes("uncertain"));
  assert.equal((await run("other", "stdio", "other-chat")).state, "succeeded");
  assert.equal((await run("file", "file")).state, "succeeded");
  await stop();
  await start();
  await run("still-blocked", "stdio");
  assert(
    JSON.stringify((await api("/v1/runs/still-blocked")).completion).includes(
      "uncertain",
    ),
  );
  assert.equal((await run("inspect", "receipts")).state, "succeeded");
  assert.equal(
    (await api("/v1/sessions/chat/tools/release", {})).receipts_retained,
    true,
  );
  console.log(
    "Standalone MCP: approval, persistent stdio, SSE, isolation, durable uncertainty, restart, inspection and unrelated tools pass.",
  );
} catch (e) {
  for (const log of logs) console.error(fs.readFileSync(log, "utf8"));
  throw e;
} finally {
  await stop();
  server.closeAllConnections();
  await new Promise((r) => server.close(r));
  fs.rmSync(root, { recursive: true, force: true });
}
