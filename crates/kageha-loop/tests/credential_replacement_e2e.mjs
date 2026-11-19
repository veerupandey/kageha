// Same WebUI API, existing chat, active turn, embedded -> service -> rollback. No Python.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import https from "node:https";
import { spawn, execFileSync } from "node:child_process";
const root = fs.mkdtempSync("/tmp/kageha-ui-vault-switch-");
fs.chmodSync(root, 0o700);
const app = path.resolve("target/debug/kageha-loop"),
  vaultBin = path.resolve("target/debug/kageha-credentials");
const children = [];
const token = "disposable-kageha-host-token-1234567890",
  credential = "UI-REPLACEMENT-CANARY-123456789";
let base,
  host,
  canSwitch = false,
  switched = false,
  calls = 0;
function write(name, value) {
  const file = path.join(root, name);
  fs.writeFileSync(file, value, { mode: 0o600 });
  return file;
}
function start(binary, args, env = {}) {
  const log = write("process-" + children.length + ".log", "");
  const child = spawn(binary, args, {
    env: { PATH: "", ...env },
    stdio: ["ignore", fs.openSync(log, "a"), fs.openSync(log, "a")],
  });
  children.push(child);
  return { child, log };
}
async function until(fn) {
  for (let i = 0; i < 500; i++) {
    const v = await fn();
    if (v) return v;
    await new Promise((r) => setTimeout(r, 30));
  }
  throw Error("Credential replacement fixture timed out");
}
async function api(route, body) {
  const r = await fetch(base + route, {
    method: body ? "POST" : "GET",
    headers: {
      Authorization: "Bearer " + token,
      "Content-Type": "application/json",
    },
    body: body ? JSON.stringify(body) : undefined,
  });
  const v = await r.json();
  assert.equal(r.status, 200, JSON.stringify(v));
  return v;
}
const ca = write(
  "ca.cnf",
  "[req]\ndistinguished_name=dn\nx509_extensions=ext\nprompt=no\n[dn]\nCN=Test CA\n[ext]\nbasicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign,cRLSign\n",
);
const leaf = write(
  "leaf.cnf",
  "[req]\ndistinguished_name=dn\nprompt=no\n[dn]\nCN=localhost\n[ext]\nsubjectAltName=DNS:localhost\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n",
);
const openssl = (args) =>
  execFileSync("/usr/bin/openssl", args, { stdio: "ignore" });
openssl([
  "req",
  "-x509",
  "-newkey",
  "rsa:2048",
  "-nodes",
  "-days",
  "1",
  "-keyout",
  root + "/ca.key",
  "-out",
  root + "/ca.pem",
  "-config",
  ca,
]);
openssl([
  "req",
  "-newkey",
  "rsa:2048",
  "-nodes",
  "-keyout",
  root + "/leaf.key",
  "-out",
  root + "/leaf.csr",
  "-config",
  leaf,
]);
openssl([
  "x509",
  "-req",
  "-in",
  root + "/leaf.csr",
  "-CA",
  root + "/ca.pem",
  "-CAkey",
  root + "/ca.key",
  "-CAcreateserial",
  "-days",
  "1",
  "-out",
  root + "/leaf.pem",
  "-extfile",
  leaf,
  "-extensions",
  "ext",
]);
const model = https.createServer(
  {
    key: fs.readFileSync(root + "/leaf.key"),
    cert: fs.readFileSync(root + "/leaf.pem"),
  },
  async (req, res) => {
    try {
      assert.equal(req.headers.authorization, "Bearer " + credential);
      let body = "";
      for await (const b of req) body += b;
      const request = JSON.parse(body);
      calls++;
      if (canSwitch && !switched) {
        const decision = {
          id: "explicit-disposable-service",
          destination: "remote",
        };
        await api("/api/credentials/replacements", {
          action: "request",
          destination: "remote",
          operation_id: "active-ui-switch",
          decision,
        });
        const status = await api("/api/credentials/replacements", {
          action: "resume",
          destination: "remote",
          operation_id: "active-ui-switch",
        });
        assert.equal(status.stage, "verified", JSON.stringify(status));
        switched = true;
      }
      const message = request.messages.some((m) => m.role === "tool")
        ? {
            role: "assistant",
            content: "Same chat continued after credential switch.",
          }
        : {
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
          };
      res.setHeader("Content-Type", "application/json");
      res.end(JSON.stringify({ choices: [{ message }] }));
    } catch (e) {
      res.writeHead(500);
      res.end("{}");
      throw e;
    }
  },
);
await new Promise((r) => model.listen(0, "127.0.0.1", r));
const endpoint = `https://localhost:${model.address().port}/v1`;
const key = write("master.key", crypto.randomBytes(32));
fs.mkdirSync(root + "/workspace");
fs.writeFileSync(root + "/workspace/notes.txt", "Disposable fixture");
async function boot(backends) {
  host = start(
    app,
    ["serve", root + "/workspace", root + "/state", "127.0.0.1:0"],
    {
      KAGEHA_SERVER_TOKEN: token,
      KAGEHA_CREDENTIAL_KEY_FILE: key,
      KAGEHA_CA_BUNDLE: root + "/ca.pem",
      KAGEHA_PROVIDER_URL: endpoint + "/chat/completions",
      KAGEHA_MODEL: "initial",
      ...(backends ? { KAGEHA_CREDENTIAL_BACKENDS: backends } : {}),
    },
  );
  base = await until(() => {
    if (host.child.exitCode !== null)
      throw Error(fs.readFileSync(host.log, "utf8"));
    const m = fs.readFileSync(host.log, "utf8").match(/listening on ([^\s]+)/);
    return m && "http://" + m[1];
  });
}
async function stop(child) {
  if (child.exitCode === null && child.signalCode === null) {
    child.kill("SIGTERM");
    await new Promise((r) => {
      const timer = setTimeout(() => child.kill("SIGKILL"), 3000);
      child.once("exit", () => {
        clearTimeout(timer);
        r();
      });
    });
  }
}
try {
  await boot();
  await api("/api/provider-setup/connect", {
    provider_name: "fixture",
    base_url: endpoint,
    api_key: credential,
    models: ["fixture"],
  });
  await api("/api/provider-setup/roles", { default: "fixture/fixture" });
  const chat = (await api("/api/sessions", {})).session_id;
  await stop(host.child);
  const owner = execFileSync(
    "/usr/bin/sqlite3",
    [
      root + "/state/native-chats.sqlite",
      "SELECT owner FROM ui_credential_owner",
    ],
    { encoding: "utf8" },
  ).trim();
  const socket = root + "/remote.sock";
  const destinations = ["embedded", "remote"];
  const clientNames = [
    "models",
    "oauth",
    "mcp",
    "media",
    "channels",
    "migration",
    "replacement",
  ];
  const permissions = ["metadata", "resolve", "write", "revoke"];
  const grants = clientNames
    .filter((n) => !["migration", "replacement"].includes(n))
    .map((n) => ({
      principal: n,
      namespace: owner + "/" + n,
      ids: ["*"],
      purposes: ["compat"],
      destinations: ["local"],
      permissions: n === "oauth" ? [...permissions,"export"] : permissions,
    }));
  grants.push({
    principal: "migration",
    namespace: owner + "/oauth",
    ids: ["*"],
    purposes: ["migration"],
    destinations: ["local"],
    permissions: ["export"],
  });
  for (const namespace of [
    "system",
    ...["models", "oauth", "mcp", "media", "channels"].map(
      (n) => owner + "/" + n,
    ),
  ])
    grants.push({
      principal: "replacement",
      namespace,
      ids: ["*"],
      purposes: ["replacement"],
      destinations,
      permissions: ["admin", "export", "write"],
    });
  const workloads = {},
    clients = [];
  for (const name of clientNames) {
    const value = crypto.randomBytes(32).toString("hex"),
      tokenFile = write(name + ".token", value);
    workloads[name] = { transport: "unix", socket, token_file: tokenFile };
    clients.push({
      principal: name,
      authentication_generation: 1,
      management: name === "replacement",
      uid: process.getuid(),
      local_token_sha256: crypto
        .createHash("sha256")
        .update(value)
        .digest("hex"),
    });
  }
  const configuration = write(
    "remote.json",
    JSON.stringify({
      vault: root + "/remote.sqlite",
      key_id: "remote",
      key_files: { remote: write("remote.key", crypto.randomBytes(32)) },
      socket,
      grants,
      clients,
    }),
  );
  const remote = start(vaultBin, ["serve", "--config", configuration]);
  await until(() => {
    if (remote.child.exitCode !== null)
      throw Error(fs.readFileSync(remote.log, "utf8"));
    return fs.existsSync(socket);
  });
  const backends = write(
    "backends.json",
    JSON.stringify([{ id: "remote", device_id: "fixture-service", workloads }]),
  );
  await boot(backends);
  assert.equal((await api("/api/sessions/" + chat)).session_id, chat);
  canSwitch = true;
  await api("/api/chat/send", {
    session_id: chat,
    client_request_id: "active",
    message: "Read notes.txt",
  });
  assert(switched);
  assert(calls >= 2);
  assert.equal((await api("/api/credentials/replacements")).backend, "remote");
  const state = await api("/api/sessions/" + chat);
  assert(JSON.stringify(state).includes("Same chat continued"));
  assert(!JSON.stringify(state).includes(credential));
  const rollback = await api("/api/credentials/replacements", {
    action: "rollback",
    destination: "embedded",
    previous_operation_id: "active-ui-switch",
    operation_id: "ui-rollback",
    decision: { id: "explicit-disposable-return", destination: "embedded" },
  });
  assert.equal(rollback.stage, "verified", JSON.stringify(rollback));
  await api("/api/chat/send", {
    session_id: chat,
    client_request_id: "after-rollback",
    message: "Read notes.txt",
  });
  assert(calls >= 4);
  for (const child of children) {
    const text = fs.readFileSync(
      root + "/process-" + children.indexOf(child) + ".log",
      "utf8",
    );
    assert(!text.includes(credential));
  }
  console.log(
    JSON.stringify({
      passed: true,
      same_webui: true,
      same_chat: true,
      active_turn_switch: true,
      rollback: true,
      python_backend: false,
      model_calls: calls,
    }),
  );
} finally {
  for (const child of children.reverse()) await stop(child);
  model.closeAllConnections();
  await new Promise((r) => model.close(r));
  if (process.env.KEEP_CREDENTIAL_FIXTURE) console.error(root);
  else fs.rmSync(root, { recursive: true, force: true });
}
