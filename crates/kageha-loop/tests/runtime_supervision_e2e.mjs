import { credentialFixture } from './credential_fixture.mjs';
// Kill the actual harness between its adapter commit and runtime receipt publication.
// The backend gets no Python or Node executables in PATH.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import http from "node:http";
import { spawn, execFileSync } from "node:child_process";
import { DatabaseSync } from "node:sqlite";
const binary =
  process.env.KAGEHA_LOOP_BIN || path.resolve("target/debug/kageha-loop");
const root = fs.mkdtempSync("/tmp/kageha-supervision-");
const token = "disposable-supervision-test-token";
const pause = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(fn) {
  for (let end = Date.now() + 15000; Date.now() < end; ) {
    const v = await fn();
    if (v) return v;
    await pause(20);
  }
  throw Error("Condition timed out");
}
let child,
  base,
  release,
  requests = [],
  logs = [];
const fixture = http.createServer(async (req, res) => {
  let data = "";
  for await (const b of req) data += b;
  const value = JSON.parse(data);
  requests.push(value);
  const intent = value.messages
    .filter((m) => m.role === "user")
    .at(-1)?.content;
  const reply = (content) =>
    res.end(
      JSON.stringify({
        choices: [{ message: { role: "assistant", content } }],
      }),
    );
  if (
    intent === "hold for crash" ||
    intent === "hold for steering" ||
    intent === "hold for cancel"
  ) {
    release = () => {
      if (intent === "hold for steering")
        res.end(
          JSON.stringify({
            choices: [
              {
                message: {
                  role: "assistant",
                  content: null,
                  tool_calls: [
                    {
                      id: "obsolete-input",
                      type: "function",
                      function: {
                        name: "read_file",
                        arguments: JSON.stringify({ path: "obsolete.txt" }),
                      },
                    },
                  ],
                },
              },
            ],
          }),
        );
      else reply("Original result");
    };
    return;
  }
  reply(
    intent === "Use revised criteria" ? "Revised result" : "Continued result",
  );
});
await new Promise((r) => fixture.listen(0, "127.0.0.1", r));
async function start() {
  const log = path.join(root, `server-${logs.length}.log`);
  logs.push(log);
  child = spawn(
    binary,
    [
      "serve",
      path.join(root, "workspace"),
      path.join(root, "state"),
      "127.0.0.1:0",
    ],
    {
      env: {
        PATH: path.join(root, "bin"),
        ...credentialFixture(root),
        KAGEHA_SERVER_TOKEN: token,
        KAGEHA_MODEL: "fixture",
        KAGEHA_PROVIDER_URL: `http://127.0.0.1:${fixture.address().port}/chat/completions`,
      },
      stdio: ["ignore", fs.openSync(log, "a"), fs.openSync(log, "a")],
    },
  );
  base = await until(() => {
    if (child.exitCode !== null) throw Error(fs.readFileSync(log, "utf8"));
    const match = fs.readFileSync(log, "utf8").match(/listening on ([^\s]+)/);
    return match && "http://" + match[1];
  });
}
async function api(route, body, method) {
  const res = await fetch(base + route, {
    method: method || (body ? "POST" : "GET"),
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
    },
    body: body ? JSON.stringify(body) : undefined,
  });
  const data = await res.json();
  assert.equal(res.status, 200, JSON.stringify(data));
  return data;
}
async function stop() {
  if (child && child.exitCode === null) {
    child.kill("SIGKILL");
    await new Promise((r) => child.once("exit", r));
  }
}
let journal, native;
try {
  fs.mkdirSync(path.join(root, "workspace"));
  fs.mkdirSync(path.join(root, "bin"));
  fs.symlinkSync(
    execFileSync("/usr/bin/which", ["curl"], { encoding: "utf8" }).trim(),
    path.join(root, "bin/curl"),
  );
  await start();
  const session = (await api("/api/sessions", {})).session_id;
  const send = (id, message) =>
    api("/api/chat/send", {
      session_id: session,
      client_request_id: id,
      message,
    });
  let response = send("steer", "hold for steering");
  await until(() => release);
  const active = await api(`/api/sessions/${session}`);
  const steering = {
    session_id: session,
    turn_id: active.active_turn.turn_id,
    message_id: "direction-one",
    message: "Use revised criteria",
  };
  await api("/api/chat/steer", steering);
  await api("/api/chat/steer", steering); // same key must not apply twice
  const reply = release;
  release = null;
  reply();
  const steered = await response;
  assert.equal(steered.status, "success", JSON.stringify(steered));
  let stored = await api(`/api/sessions/${session}`);
  assert.equal(stored.messages.at(-1).text, "Revised result");
  assert.equal(
    stored.messages.filter(
      (m) => m.steering && m.text === "Use revised criteria",
    ).length,
    1,
  );
  assert.equal(
    requests.at(-1).messages.filter((m) => m.content === "Use revised criteria")
      .length,
    1,
  );
  assert(
    !requests.some((r) => r.messages.some((m) => m.role === "tool")),
    "Obsolete tool request must not execute",
  );
  await send("followup", "Continue with that direction");
  assert(
    requests.at(-1).messages.some((m) => m.content === "Use revised criteria"),
    "Steering survives the turn boundary",
  );
  response = send("cancel", "hold for cancel");
  await until(() => release);
  const cancelTurn = (await api(`/api/sessions/${session}`)).active_turn
    .turn_id;
  assert.equal(
    (
      await api("/api/chat/cancel", {
        session_id: session,
        turn_id: cancelTurn,
      })
    ).cancelled,
    true,
  );
  await response;
  assert.equal(
    (await api(`/api/sessions/${session}`)).turns.at(-1).status,
    "cancelled",
  );
  release();
  release = null;
  response = send("crash", "hold for crash").catch(() => null);
  await until(() => release);
  journal = new DatabaseSync(
    path.join(root, "state/components/runtime.sqlite"),
  );
  native = new DatabaseSync(path.join(root, "state/native-chats.sqlite"));
  journal.exec("BEGIN IMMEDIATE");
  const crashReply = release;
  release = null;
  crashReply();
  await until(() =>
    native
      .prepare(
        "SELECT 1 FROM ui_runtime_results r JOIN native_chat_turns t ON r.turn=(SELECT json_extract(spec,'$.turn') FROM ui_runtime_runs WHERE json_extract(spec,'$.session')=t.chat_id AND json_extract(spec,'$.request')=t.request_id) WHERE t.request_id='crash'",
      )
      .get(),
  );
  assert.equal(
    journal
      .prepare(
        "SELECT state FROM runs WHERE json_extract(body,'$.spec.request')='crash'",
      )
      .get().state,
    "running",
  );
  await stop();
  journal.exec("ROLLBACK");
  journal.close();
  journal = null;
  native.close();
  native = null;
  await response;
  const before = requests.length;
  await start();
  stored = await api(`/api/sessions/${session}`);
  assert.equal(stored.turns.at(-1).status, "success", JSON.stringify(stored));
  assert.equal(stored.messages.at(-1).text, "Original result");
  assert.equal(
    requests.length,
    before,
    "Recovered committed result must not call the model again",
  );
  await send("after-restart", "Continue after restart");
  assert(requests.at(-1).messages.some((m) => m.content === "Original result"));
  // Duplicate accepted request must return the existing result after restart.
  const count = requests.length;
  await send("crash", "hold for crash");
  assert.equal(requests.length, count);
  // An admitted run with no committed outcome is interrupted, never replayed.
  response = send("unknown", "hold for crash").catch(() => null);
  await until(() => release);
  const unknownRelease = release;
  release = null;
  await stop();
  await response;
  unknownRelease();
  const unknownCalls = requests.length;
  await start();
  stored = await api(`/api/sessions/${session}`);
  assert.equal(stored.turns.at(-1).status, "interrupted");
  assert.equal(stored.active, false);
  assert.equal(requests.length, unknownCalls);
  await send("resumed", "Continue after interruption");
  console.log(
    JSON.stringify({
      passed: true,
      python_backend: false,
      steering: "in-flight and retained",
      cancellation: "durable",
      crash: "saved result before receipt publication",
      replayed_model_calls: 0,
      unknown_execution: "interrupted without replay",
      fixture: root,
    }),
  );
} catch (error) {
  console.error(error);
  for (const log of logs) console.error(fs.readFileSync(log, "utf8"));
  process.exitCode = 1;
} finally {
  if (journal) {
    try {
      journal.exec("ROLLBACK");
    } catch {}
    journal.close();
  }
  native?.close();
  await stop();
  fixture.closeAllConnections();
  await new Promise((r) => fixture.close(r));
}
