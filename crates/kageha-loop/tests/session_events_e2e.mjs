import { credentialFixture } from './credential_fixture.mjs';
// Session feed carries parent tools and durable workers; reconnect never starts work.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import http from "node:http";
import { spawn } from "node:child_process";
const binary =
  process.env.KAGEHA_LOOP_BIN || path.resolve("target/debug/kageha-loop");
const root = fs.mkdtempSync("/tmp/kageha-session-feed-");
const token = "session-feed-disposable-authentication-token";
const pause = (ms) => new Promise((r) => setTimeout(r, ms));
let backend,
  base,
  releaseChild,
  releaseStream,
  childHolding = false,
  modelCalls = 0;
const logs = [];
const feeds = [];
async function until(fn) {
  for (let end = Date.now() + 15000; Date.now() < end; ) {
    const value = await fn();
    if (value) return value;
    await pause(15);
  }
  throw Error("Condition timed out");
}
const model = http.createServer(async (req, res) => {
  let text = "";
  for await (const chunk of req) text += chunk;
  const body = JSON.parse(text);
  modelCalls++;
  const intent = body.messages.filter((m) => m.role === "user").at(-1)?.content;
  const reply = (message) =>
    res.end(JSON.stringify({ choices: [{ message }] }));
  if (intent === "stream incremental fixture") {
    assert.equal(body.stream, true);
    res.writeHead(200, {"Content-Type":"text/event-stream"});
    res.write('data: {"choices":[{"delta":{"content":"First "}}]}\n\n');
    await new Promise(resolve => { releaseStream = resolve; });
    res.end('data: {"choices":[{"delta":{"content":"second."},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n');
    return;
  }
  if (intent === "parent request" || intent === "child held") {
    if (body.messages.at(-1)?.role !== "tool") {
      reply({
        role: "assistant",
        content: null,
        tool_calls: [
          {
            id: "read",
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
    assert.equal(body.messages.at(-1).content, "session stream proof");
    if (intent === "child held") {
      childHolding = true;
      await new Promise((r) => {
        releaseChild = r;
      });
    }
  }
  reply({
    role: "assistant",
    content:
      intent === "child held"
        ? "Child finished."
        : intent === "parent request"
          ? "Parent finished."
          : "Reconnected worker finished.",
  });
});
await new Promise((r) => model.listen(0, "127.0.0.1", r));
async function start() {
  const log = path.join(root, "backend-" + logs.length + ".log");
  logs.push(log);
  const fd = fs.openSync(log, "a");
  backend = spawn(
    binary,
    ["serve", root + "/workspace", root + "/state", "127.0.0.1:0"],
    {
      env: {
        PATH: "",
        HOME: root,
        ...credentialFixture(root),
        KAGEHA_SERVER_TOKEN: token,
        KAGEHA_PROVIDER_URL: `http://127.0.0.1:${model.address().port}/v1/chat/completions`,
        KAGEHA_MODEL: "fixture",
        KAGEHA_LEGACY_SESSIONS_DIR: root + "/empty",
        ...(process.env.KAGEHA_RUNTIME_SERVICE_URL ? { KAGEHA_RUNTIME_SERVICE_URL:process.env.KAGEHA_RUNTIME_SERVICE_URL, KAGEHA_RUNTIME_SERVICE_TOKEN_FILE:process.env.KAGEHA_RUNTIME_SERVICE_TOKEN_FILE } : {}),
      },
      stdio: ["ignore", fd, fd],
    },
  );
  fs.closeSync(fd);
  base = await until(() => {
    if (backend.exitCode !== null) throw Error(fs.readFileSync(log, "utf8"));
    const m = fs.readFileSync(log, "utf8").match(/listening on ([^\s]+)/);
    return m && "http://" + m[1];
  });
}
async function stop() {
  if (backend?.exitCode === null) {
    backend.kill("SIGTERM");
    await Promise.race([
      new Promise((r) => backend.once("exit", r)),
      pause(1500),
    ]);
    if (backend.exitCode === null) backend.kill("SIGKILL");
  }
}
async function api(url, body) {
  const r = await fetch(base + url, {
    method: body ? "POST" : "GET",
    headers: {
      Authorization: "Bearer " + token,
      "Content-Type": "application/json",
    },
    body: body ? JSON.stringify(body) : undefined,
    signal: AbortSignal.timeout(15000),
  });
  const text = await r.text();
  assert.equal(r.status, 200, text);
  return JSON.parse(text);
}
async function feed(chat, after = 0) {
  const abort = new AbortController();
  const response = await fetch(
    base + `/api/sessions/${chat}/live/stream?after_sequence=${after}`,
    { headers: { Authorization: "Bearer " + token }, signal: abort.signal },
  );
  assert.equal(response.status, 200);
  const data = { frames: [], closed: false, abort, last: after };
  feeds.push(data);
  const reader = response.body.getReader();
  data.pump = (async () => {
    let buffer = "";
    const decoder = new TextDecoder();
    try {
      for (;;) {
        const { value, done } = await reader.read();
        if (done) break;
        buffer += decoder.decode(value, { stream: true });
        let at;
        while ((at = buffer.indexOf("\n\n")) >= 0) {
          const chunk = buffer.slice(0, at);
          buffer = buffer.slice(at + 2);
          const event = chunk
            .split("\n")
            .find((v) => v.startsWith("event:"))
            ?.slice(6)
            .trim();
          const json = chunk
            .split("\n")
            .filter((v) => v.startsWith("data:"))
            .map((v) => v.slice(5).trim())
            .join("\n");
          if (!json) continue;
          const payload = JSON.parse(json);
          data.frames.push({ event, data: payload });
          if (event === "update")
            for (const frame of payload.events || [])
              data.last = Math.max(data.last, frame.sequence);
        }
      }
    } catch (e) {
      if (!abort.signal.aborted) throw e;
    } finally {
      data.closed = true;
      reader.releaseLock();
    }
  })();
  return data;
}
const events = (data) =>
  data.frames.filter((f) => f.event === "update").flatMap((f) => f.data.events);
try {
  fs.mkdirSync(root + "/workspace");
  fs.mkdirSync(root + "/empty");
  fs.writeFileSync(root + "/workspace/notes.txt", "session stream proof");
  await start();
  const chat = (await api("/api/sessions", {})).session_id;
  const other = (await api("/api/sessions", {})).session_id;
  const live = await feed(chat);
  const first = await until(() =>
    live.frames.find((f) => f.event === "snapshot"),
  );
  assert.equal(first.data.session_id, chat);
  assert.deepEqual(first.data.workers, []);
  assert.equal(first.data.live.session_id, chat);
  const streamChat = (await api("/api/sessions", {})).session_id;
  const streaming = await feed(streamChat);
  const streamed = await api("/api/chat/stream", {
    session_id:streamChat, client_request_id:"incremental", message:"stream incremental fixture", event_stream:"session",
  });
  const firstChunk = await until(() => events(streaming).find(event =>
    event.kind === "stream_item" && event.payload?.text === "First "));
  assert.equal(firstChunk.turn_id, streamed.turn_id);
  streaming.abort.abort();
  await streaming.pump;
  const reconnected = await feed(streamChat, firstChunk.sequence);
  releaseStream();
  await until(() => events(reconnected).find(event => event.kind === "stream_item" && event.payload?.text === "second."));
  await until(() => events(reconnected).find(event => event.kind === "done"));
  assert((await api(`/api/sessions/${streamChat}`)).messages.some(message => message.text === "First second."));
  reconnected.abort.abort();
  await reconnected.pump;
  const accepted = await api("/api/chat/stream", {
    session_id: chat,
    client_request_id: "parent",
    message: "parent request",
    event_stream: "session",
  });
  assert.equal(accepted.event_stream, "session");
  assert.equal(accepted.status, "accepted");
  assert.equal(accepted.after_sequence, 0);
  const worker = await api("/api/jobs", {
    session_id: chat,
    agent_id: "main",
    objective: "child held",
    client_request_id: "worker",
  });
  await until(() => childHolding);
  await until(() =>
    events(live).some(
      (e) => e.kind === "tool_card" && e.tool_card.status === "done",
    ),
  );
  await until(() =>
    events(live).some(
      (e) => e.kind === "done" && e.turn_id === accepted.turn_id,
    ),
  );
  await until(() =>
    events(live).some(
      (e) =>
        e.kind === "worker_activity" &&
        e.job_id === worker.id &&
        e.activity.kind === "tool_card" &&
        e.activity.tool_card.status === "done",
    ),
  );
  await until(() =>
    events(live).some(
      (e) =>
        e.kind === "worker" &&
        e.job.id === worker.id &&
        e.job.state === "running",
    ),
  );
  assert.equal(
    live.closed,
    false,
    "parent done must not close the session feed",
  );
  releaseChild();
  await until(() =>
    events(live).some(
      (e) =>
        e.kind === "worker" &&
        e.job.id === worker.id &&
        e.job.state === "succeeded",
    ),
  );
  assert.equal((await api("/api/jobs/" + worker.id)).job.progress.steps, 1, "worker progress counts recorded tool calls");
  const sequence = events(live).map((e) => e.sequence);
  assert(sequence.every((n, i) => i === 0 || n > sequence[i - 1]));
  const last = live.last;
  live.abort.abort();
  await live.pump;
  const next = await api("/api/jobs", {
    session_id: chat,
    agent_id: "main",
    objective: "reconnected child",
    client_request_id: "worker-2",
  });
  await until(
    async () => (await api("/api/jobs/" + next.id)).job.state === "succeeded",
  );
  const calls = modelCalls;
  const restored = await feed(chat, last);
  const snap = await until(() =>
    restored.frames.find((f) => f.event === "snapshot"),
  );
  assert.equal(snap.data.workers.length, 2);
  await until(() =>
    events(restored).some(
      (e) =>
        e.kind === "worker" &&
        e.job.id === next.id &&
        e.job.state === "succeeded",
    ),
  );
  assert(events(restored).every((e) => e.sequence > last));
  assert.equal(modelCalls, calls, "replay must never resubmit model work");
  const isolated = await feed(other);
  const isolatedSnap = await until(() =>
    isolated.frames.find((f) => f.event === "snapshot"),
  );
  assert.deepEqual(isolatedSnap.data.workers, []);
  assert.equal(isolatedSnap.data.sequence, 0);
  isolated.abort.abort();
  await isolated.pump;
  restored.abort.abort();
  await restored.pump;
  await stop();
  await start();
  const persisted = await api("/api/sessions/" + chat);
  assert(
    persisted.messages.some((m) =>
      JSON.stringify(m).includes("Parent finished."),
    ),
  );
  const restarted = await feed(chat, last);
  await until(() =>
    restarted.frames.some(
      (f) =>
        f.event === "snapshot" &&
        f.data.workers.every((j) => j.state === "succeeded"),
    ),
  );
  assert.equal(modelCalls, calls, "restart subscription must be read-only");
  restarted.abort.abort();
  await restarted.pump;
  const unauthorized = await fetch(base + `/api/sessions/${chat}/live/stream`);
  assert.equal(unauthorized.status, 401);
  const missing = await fetch(base + "/api/sessions/missing/live/stream", {
    headers: { Authorization: "Bearer " + token },
  });
  assert.equal(missing.status, 404);
  console.log(
    JSON.stringify({
      passed: true,
      python_backend: false,
      checks: [
        "session handshake",
        "parent tool and worker events in one ordered feed",
        "child activity identities",
        "parent done retains stream",
        "worker completion without polling",
        "cursor replay without resubmission",
        "session isolation",
        "history and worker restart preservation",
        "authentication",
      ],
      root,
    }),
  );
} catch (error) {
  console.error(error);
  for (const log of logs) console.error(fs.readFileSync(log, "utf8"));
  process.exitCode = 1;
} finally {
  for (const f of feeds) f.abort.abort();
  releaseChild?.();
  await stop();
  model.closeAllConnections();
  await new Promise((r) => model.close(r));
  if (!process.argv.includes("--keep"))
    fs.rmSync(root, {
      recursive: true,
      force: true,
      maxRetries: 3,
      retryDelay: 100,
    });
}
