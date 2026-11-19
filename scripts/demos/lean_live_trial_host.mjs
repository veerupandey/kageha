// Isolated WebUI server and live-provider transport for synthetic task evaluations.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import http from "node:http";
import crypto from "node:crypto";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { DatabaseSync } from "node:sqlite";
import { setTimeout as pause } from "node:timers/promises";

let nextRequestAt = 0;

export async function startLiveTrial({
  binary = path.resolve("target/debug/kageha-loop"),
  model = process.env.KAGEHA_TRIAL_MODEL || "gemini-flash-lite-latest",
  maxCalls = 24,
  seconds = 180,
  learningSeed,
  onRequest,
} = {}) {
  const provider = process.env.KAGEHA_TRIAL_PROVIDER || "gemini";
  const routes = {
    gemini: ["https://generativelanguage.googleapis.com/v1beta/openai/chat/completions", "GOOGLE_API_KEY"],
    poe: ["https://api.poe.com/v1/chat/completions", "POE_API_KEY"],
  };
  if (!Object.hasOwn(routes, provider)) throw Error("Unknown live trial provider");
  const [endpoint, keyName] = routes[provider];
  const key = process.env[keyName];
  if (!key) throw Error("Set " + keyName + " to run live trials");
  const hash = crypto.createHash("sha256").update(fs.readFileSync(binary)).digest("hex");
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "kageha-live-work-"));
  const project = path.join(dir, "project"),
    state = path.join(dir, "state"),
    home = path.join(state, "home");
  const learningPath = path.join(home, "learning.sqlite");
  for (const folder of [project, state, home, path.join(state, "empty-legacy")])
    fs.mkdirSync(folder, { mode: 0o700 });
  if (learningSeed) fs.copyFileSync(learningSeed, learningPath);
  const calls = [];
  const adapter = http.createServer(async (req, res) => {
    const call = { start: Date.now(), end: null, status: null, role: "executor", tool_calls: [] };
    calls.push(call);
    try {
      let raw = "";
      for await (const chunk of req) raw += chunk;
      const input = JSON.parse(raw);
      const first = String(input.messages?.[0]?.content || "");
      if (first.startsWith("You review the journal of a request")) call.role = "reflection";
      onRequest?.(input, call);
      const stream = input.stream;
      input.stream = false;
      delete input.stream_options;
      const spacing = Number(process.env.KAGEHA_TRIAL_SPACING_MS || 0);
      if (spacing > 0) {
        const slot = Math.max(Date.now(), nextRequestAt);
        nextRequestAt = slot + spacing;
        await pause(Math.max(0, slot - Date.now()));
      }
      const upstream = await fetch(endpoint, {
        method: "POST",
        headers: { "content-type": "application/json", authorization: "Bearer " + key },
        body: JSON.stringify(input),
        signal: AbortSignal.timeout(60000),
      });
      call.status = upstream.status;
      const text = await upstream.text();
      if (!upstream.ok) {
        res.writeHead(upstream.status, {
          "content-type": "application/json",
          ...(upstream.headers.get("retry-after")
            ? { "retry-after": upstream.headers.get("retry-after") }
            : {}),
        });
        res.end(JSON.stringify({ error: { message: "Live benchmark provider HTTP " + upstream.status } }));
        return;
      }
      const output = JSON.parse(text),
        choice = output.choices?.[0];
      if (!choice?.message) throw Error("Missing provider choice");
      call.tool_calls = (choice.message.tool_calls || []).map((c) => ({
        name: c.function.name,
        id: c.id,
        arguments: c.function.arguments,
      }));
      if (!stream) {
        res.writeHead(200, { "content-type": "application/json" });
        res.end(text);
        return;
      }
      const delta = { ...choice.message };
      if (delta.tool_calls) delta.tool_calls = delta.tool_calls.map((c, index) => ({ ...c, index }));
      res.writeHead(200, { "content-type": "text/event-stream" });
      res.end(
        "data: " +
          JSON.stringify({
            choices: [{ index: 0, delta, finish_reason: choice.finish_reason }],
            usage: output.usage,
          }) +
          "\n\ndata: [DONE]\n\n",
      );
    } catch {
      call.status = 502;
      if (!res.headersSent) res.writeHead(502, { "content-type": "application/json" });
      res.end('{"error":"Trial transport failure"}');
    } finally {
      call.end = Date.now();
    }
  });
  let child, exit, base;
  const token = crypto.randomBytes(32).toString("hex");
  async function close() {
    if (child && child.exitCode === null && child.signalCode === null) {
      child.kill("SIGTERM");
      await Promise.race([exit, pause(2000, null, { ref: false })]);
      if (child.exitCode === null && child.signalCode === null) {
        child.kill("SIGKILL");
        await exit;
      }
    }
    adapter.closeAllConnections();
    if (adapter.listening) await new Promise((r) => adapter.close(r));
    fs.rmSync(dir, { recursive: true, force: true });
  }
  async function api(route, body) {
    const response = await fetch(base + route, {
      method: body === undefined ? "GET" : "POST",
      headers: { authorization: "Bearer " + token, "content-type": "application/json" },
      body: body === undefined ? undefined : JSON.stringify(body),
      signal: AbortSignal.timeout((seconds + 50) * 1000),
    });
    if (!response.ok) throw Error("Trial endpoint " + route + " returned HTTP " + response.status);
    return response.json();
  }
  try {
    await new Promise((resolve) => adapter.listen(0, "127.0.0.1", resolve));
    const log = path.join(dir, "server.log"),
      fd = fs.openSync(log, "w", 0o600);
    try {
      child = spawn(binary, ["serve", project, state, "127.0.0.1:0"], {
        stdio: ["ignore", fd, fd],
        env: {
          PATH: process.env.PATH,
          HOME: dir,
          TMPDIR: os.tmpdir(),
          KAGEHA_HOME: home,
          KAGEHA_PROVIDER_URL: "http://127.0.0.1:" + adapter.address().port + "/v1",
          KAGEHA_MODEL: model,
          KAGEHA_API_KEY: "local-adapter",
          KAGEHA_SERVER_TOKEN: token,
          KAGEHA_LEGACY_SESSIONS_DIR: path.join(state, "empty-legacy"),
        },
      });
    } finally {
      fs.closeSync(fd);
    }
    exit = once(child, "exit").catch(() => {});
    for (let end = Date.now() + 120000; Date.now() < end && child.exitCode === null && !child.signalCode; ) {
      const addr = fs.readFileSync(log, "utf8").match(/listening on ([^\s]+)/)?.[1];
      if (addr) {
        base = "http://" + addr;
        break;
      }
      await pause(80);
    }
    if (!base) throw Error("Isolated server startup failed");
    await api("/api/settings/persistence", {
      enabled: true,
      max_duration_seconds: seconds,
      max_model_calls: maxCalls,
    });
    return {
      hash,
      model,
      provider,
      state,
      learningPath,
      calls,
      close,
      api,
      db: () => new DatabaseSync(path.join(state, "native-chats.sqlite"), { readOnly: true }),
      session: async () => {
        const session = (await api("/api/sessions", {})).session_id;
        const workspace = path.join(state, "sessions", session, "workspace");
        fs.mkdirSync(workspace, { recursive: true, mode: 0o700 });
        return { session, workspace };
      },
      send: (session, message, recipient) =>
        api("/api/chat/send", {
          session_id: session,
          ...(recipient ? { recipient_id: recipient } : {}),
          client_request_id: "live-" + crypto.randomUUID(),
          message,
          max_steps: maxCalls,
        }),
    };
  } catch (error) {
    await close();
    throw error;
  }
}
