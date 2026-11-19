import test from "node:test";
import assert from "node:assert/strict";
import http from "node:http";
import { AgentClient, AgentError } from "./client.mjs";
const token = "test-agent-operator-token-for-client";
const record = { spec: { id: "run", objective: "héllo" }, state: "running" };
const frame = (sequence = 4, id = sequence, state = "running") =>
  `id: ${id}\r\nevent: run\r\ndata: ${JSON.stringify({ sequence, record: { ...record, state } })}\r\n\r\n`;
async function host(handler, action) {
  const server = http.createServer(handler);
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  try {
    await action(`http://127.0.0.1:${server.address().port}`);
  } finally {
    server.closeAllConnections();
    await new Promise((r) => server.close(r));
  }
}
async function collect(client) {
  const result = [];
  for await (const e of client.events("run")) result.push(e);
  return result;
}
test("endpoint and secret validation", async () => {
  for (const endpoint of [
    "http://remote.example",
    "https://user:pass@example.com",
    "https://example.com?q=1",
    "https://example.com#secret",
  ]) {
    assert.throws(() => new AgentClient(endpoint, token));
  }
  assert.throws(() => new AgentClient("http://localhost", "short"));
  const client = new AgentClient("http://localhost", token);
  assert(!JSON.stringify(client).includes(token));
  assert(!JSON.stringify(client).includes("localhost"));
  assert.throws(() => client.status(".."));
  await assert.rejects(() => client.events("run", { after: -1 }).next());
});
test("fragmented UTF-8, CRLF, comments, multiline and terminal event", async () => {
  await host(
    (req, res) => {
      res.writeHead(200, { "Content-Type": "text/event-stream" });
      const data = JSON.stringify({ sequence: 4, record }, null, 2)
        .split("\n")
        .map((v) => `data: ${v}\r\n`)
        .join("");
      const bytes = Buffer.from(
        `: keepalive\r\n\r\nid: 4\r\nevent: run\r\n${data}\r\n${frame(7, 7, "succeeded")}`,
      );
      const cut = bytes.indexOf(Buffer.from("é")) + 1;
      res.write(bytes.subarray(0, cut));
      setTimeout(() => res.end(bytes.subarray(cut)), 5);
    },
    async (url) => {
      const events = await collect(new AgentClient(url, token));
      assert.deepEqual(
        events.map((e) => e.sequence),
        [4, 7],
      );
      assert.equal(events[0].record.spec.objective, "héllo");
    },
  );
});
test("invalid, truncated and out-of-order events fail closed", async () => {
  for (const body of [
    "event: error\ndata: failed\n\n",
    frame(4, 5),
    frame() + frame(),
    frame().slice(0, -4),
    frame(Number.MAX_SAFE_INTEGER + 1),
  ])
    await host(
      (req, res) => {
        res.writeHead(200, { "Content-Type": "text/event-stream" });
        res.end(body);
      },
      async (url) => {
        await assert.rejects(
          () => collect(new AgentClient(url, token)),
          AgentError,
        );
      },
    );
});
test("plain authentication errors and redirects retain status without forwarding", async () => {
  for (const status of [401, 302]) {
    let requests = 0;
    await host(
      (req, res) => {
        requests++;
        res.writeHead(status, { Location: "/secret-destination" });
        res.end("plain error");
      },
      async (url) => {
        await assert.rejects(
          () => new AgentClient(url, token).info(),
          (e) => e.status === status,
        );
        assert.equal(requests, 1);
      },
    );
  }
});
test("abort and idle timeout close event connections", async () => {
  await host(
    (req, res) => {
      res.writeHead(200, { "Content-Type": "text/event-stream" });
      res.write(": keepalive\n\n");
    },
    async (url) => {
      const controller = new AbortController();
      const events = new AgentClient(url, token).events("run", {
        signal: controller.signal,
      });
      const next = events.next();
      setTimeout(() => controller.abort(), 10);
      await assert.rejects(() => next);
      await assert.rejects(
        () =>
          new AgentClient(url, token, { timeoutMs: 50 }).events("run").next(),
        AgentError,
      );
    },
  );
});
