// Exercise the real daemon past its polling quota without a Python host.
// KAGEHA_BROWSER_BIN=/absolute/kageha-browser node tests/polling.mjs
import assert from "node:assert/strict";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { createInterface } from "node:readline";
import { randomUUID } from "node:crypto";

const binary = process.env.KAGEHA_BROWSER_BIN;
assert(binary && path.isAbsolute(binary), "Set KAGEHA_BROWSER_BIN");
const root = fs.mkdtempSync(path.join(os.tmpdir(), "kbr-poll-"));
const state = path.join(root, "state");
const child = spawn(binary, [
  "--state-dir", state, "--instance-id", "polling-test",
  "--application-id", "polling-test",
], { stdio: ["ignore", "ignore", "pipe"] });
let stderr = "";
child.stderr.on("data", chunk => { stderr = (stderr + chunk).slice(-4096); });
let socket;
let lines;
try {
  const deadline = Date.now() + 5000;
  while (!fs.existsSync(path.join(state, "daemon.json"))) {
    assert(child.exitCode === null, stderr);
    assert(Date.now() < deadline, "Daemon startup timed out");
    await new Promise(resolve => setTimeout(resolve, 20));
  }
  const descriptor = JSON.parse(fs.readFileSync(path.join(state, "daemon.json")));
  const token = fs.readFileSync(path.join(state, "auth.token"), "utf8");
  const schema = JSON.parse(fs.readFileSync(new URL("../protocol/schema.json", import.meta.url)));
  const limit = schema["x-limits"];
  let epoch = null;
  async function call(method, params = {}) {
    const id = randomUUID().replaceAll("-", "");
    socket.write(JSON.stringify({
      protocol_version: 1, request_id: id, instance_id: descriptor.instance_id,
      application_id: descriptor.application_id, state_root: descriptor.state_root,
      workspace_id: "polling-chat", client_id: "polling-client", service_epoch: epoch,
      deadline_ms: Date.now() + 5000, expected_binding: null, authority: null, method, params,
    }) + "\n");
    const next = await lines.next();
    assert(!next.done, "Daemon closed before replying");
    const reply = JSON.parse(next.value);
    assert.equal(reply.request_id, id);
    return reply;
  }
  let count = 0;
  while (count <= limit.remembered_requests) {
    socket = net.createConnection(path.join(state, "control.sock"));
    await once(socket, "connect");
    socket.setTimeout(5000, () => socket.destroy(Error("RPC timed out")));
    const reader = createInterface({ input: socket });
    lines = reader[Symbol.asyncIterator]();
    epoch = null;
    const hello = await call("handshake", { token });
    assert.equal(hello.status, "ok");
    epoch = hello.service_epoch;
    for (let i = 1; i < limit.requests_per_connection && count <= limit.remembered_requests; i++, count++) {
      const reply = await call("health");
      assert.equal(reply.status, "ok", JSON.stringify(reply.error));
    }
    if (count > limit.remembered_requests) {
      // Reconnect to reserve capacity for the shutdown request.
      reader.close();
      socket.destroy();
      break;
    }
    reader.close();
    socket.destroy();
  }
  socket = net.createConnection(path.join(state, "control.sock"));
  await once(socket, "connect");
  socket.setTimeout(5000, () => socket.destroy(Error("RPC timed out")));
  const reader = createInterface({ input: socket });
  lines = reader[Symbol.asyncIterator]();
  epoch = null;
  epoch = (await call("handshake", { token })).service_epoch;
  assert.equal((await call("shutdown")).status, "ok");
  reader.close();
  socket.destroy();
  console.log(`Passed ${count} health checks and graceful shutdown across reconnects.`);
} finally {
  socket?.destroy();
  if (child.exitCode === null) {
    child.kill("SIGINT");
    await Promise.race([once(child, "exit"), new Promise(resolve => setTimeout(resolve, 1000))]);
    if (child.exitCode === null) child.kill("SIGKILL");
  }
  fs.rmSync(root, { recursive: true, force: true });
}
