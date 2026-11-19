// Real Docker and Chromium with disposable Kageha state and a deterministic model.
// Build the desktop image from the repository root before running this test.
import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import fs from "node:fs";
import http from "node:http";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { credentialFixture } from "./credential_fixture.mjs";

const repo = fileURLToPath(new URL("../../../", import.meta.url));
const host = process.env.KAGEHA_LOOP_BIN ?? path.join(repo, "target/debug/kageha-loop");
const browser = process.env.KAGEHA_BROWSER_BIN ?? path.join(repo, "target/debug/kageha-browser");
const chromium = process.env.KAGEHA_CHROMIUM ?? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const root = fs.mkdtempSync("/tmp/kg-docker-");
const token = randomBytes(32).toString("hex");
const browserRoot = path.join(root, "browser");
const page = "http://127.0.0.1:18083/";
const children = [];
let session, base, container, backup, fixtureFailure, uploadPath;

function docker(args, options = {}) {
  return execFileSync("docker", args, { encoding: "utf8", timeout: 15000, stdio: ["ignore", "pipe", "pipe"], ...options }).trim();
}
function start(binary, args, env, label) {
  const log = path.join(root, label + ".log");
  const fd = fs.openSync(log, "a");
  const child = spawn(binary, args, { env, cwd: repo, stdio: ["ignore", fd, fd] });
  fs.closeSync(fd);
  children.push({ child, log });
  return log;
}
async function until(label, check, timeout = 20000) {
  const end = Date.now() + timeout;
  let failure;
  while (Date.now() < end) {
    try {
      const result = await check();
      if (result) return result;
    } catch (error) {
      failure = error;
    }
    await new Promise(resolve => setTimeout(resolve, 200));
  }
  throw new Error("Timed out waiting for " + label, { cause: failure });
}
async function api(route, body, method = body === undefined ? "GET" : "POST") {
  const response = await fetch(base + route, {
    method, headers: { Authorization: "Bearer " + token, "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body), signal: AbortSignal.timeout(45000),
  });
  const data = await response.json();
  if (fixtureFailure) throw fixtureFailure;
  return { status: response.status, data };
}
async function ok(route, body, method) {
  const { status, data } = await api(route, body, method);
  assert.equal(status, 200, route + " " + JSON.stringify(data).slice(0, 700));
  return data;
}
const tool = (name, args, id = name) => ({ role: "assistant", content: null, tool_calls: [
  { id: "fixture-" + id, type: "function", function: { name, arguments: JSON.stringify(args) } },
] });
const model = http.createServer(async (request, reply) => {
  try {
    let raw = "";
    for await (const part of request) raw += part;
    const body = JSON.parse(raw);
    const user = body.messages.findLast(message => message.role === "user");
    const last = body.messages.at(-1);
    let message = { role: "assistant", content: "Ready" };
    if (body.tools?.length) {
      if (user.content === "Verify Docker shell") {
        assert(body.tools.some(item => item.function.name === "shell"));
        message = last.role === "tool" ? { role: "assistant", content: last.content }
          : tool("shell", { command: "pwd; uname -s; printf docker-proof > outputs/proof.txt; cd outputs; export KAGEHA_TEST=workstation" });
      } else if (user.content === "Open Docker Chromium") {
        message = last.role === "tool" ? { role: "assistant", content: last.content }
          : tool("browser_open", { url: page });
      } else if (user.content === "Transfer a file through Docker Chromium") {
        message = last.role === "tool" ? { role: "assistant", content: last.content }
          : tool("browser_upload", { target: "#attach", path: uploadPath });
      } else if (user.content === "Download the Chromium result") {
        if (last.role !== "tool") {
          message = tool("browser_snapshot", {});
        } else if (last.tool_call_id === "fixture-browser_snapshot") {
          const snapshot = JSON.parse(last.content);
          const link = snapshot.controls.find(item => item.name === "Download proof");
          assert(link?.ref, JSON.stringify(snapshot).slice(0, 1500));
          message = tool("browser_click", { target: link.ref, observation_id: snapshot.observation_id });
        } else {
          message = { role: "assistant", content: last.content };
        }
      } else if (user.content === "Use the Docker terminal window") {
        const index = body.messages.lastIndexOf(user);
        const results = body.messages.slice(index + 1).filter(item => item.role === "tool");
        const action = (method, args) => tool("computer_execute",
          { observation_id: JSON.parse(last.content).observation_id, method, args }, method);
        switch (results.length) {
          case 0:
            message = tool("shell", { command: "DISPLAY=:99 xterm -T 'Workstation terminal' -e sh -c 'read line; printf %s \"$line\" > /work/outputs/desktop.txt' >/tmp/kageha-xterm.log 2>&1 & sleep 1" });
            break;
          case 1:
          case 3:
            message = tool("computer_observe", {}, "observe-" + results.length);
            break;
          case 2:
            message = action("type_text", { text: "workstation desktop input" });
            break;
          case 4:
            message = action("press_key", { key: "Return" });
            break;
          default:
            message = { role: "assistant", content: last.content };
        }
      } else if (user.content === "Start and stop a Docker job") {
        const index = body.messages.lastIndexOf(user);
        const results = body.messages.slice(index + 1).filter(item => item.role === "tool");
        message = results.length === 0 ? tool("shell", { command: "sleep 60", background: true })
          : results.length === 1 ? tool("shell_job", { action: "stop", job_id: JSON.parse(last.content).job_id })
          : { role: "assistant", content: last.content };
      }
    }
    reply.setHeader("Content-Type", "application/json");
    reply.end(JSON.stringify({ choices: [{ message }] }));
  } catch (error) {
    fixtureFailure ??= error;
    reply.statusCode = 500;
    reply.end(JSON.stringify({ error: String(error) }));
  }
});

try {
  fs.mkdirSync(browserRoot, { mode: 0o700 });
  for (const name of ["home", "workspace", "legacy"]) fs.mkdirSync(path.join(root, name));
  fs.writeFileSync(path.join(root, "models.yaml"), "models: []\nroles: {}\n");
  fs.writeFileSync(path.join(root, "models.env"), "");
  await new Promise(resolve => model.listen(0, "127.0.0.1", resolve));
  start(browser, ["--state-dir", browserRoot, "--instance-id", "docker-test", "--application-id", "docker-test",
    "--chromium", chromium, "--standalone", "on"], process.env, "browser");
  await until("browser daemon", () => fs.existsSync(path.join(browserRoot, "daemon.json")));
  const log = start(host, ["serve", path.join(root, "workspace"), path.join(root, "state"), "127.0.0.1:0"], {
    PATH: process.env.PATH, HOME: path.join(root, "home"),
    DOCKER_HOST: docker(["context", "inspect", "--format", "{{.Endpoints.docker.Host}}"]),
    ...(process.env.KAGEHA_DESKTOP_IMAGE ? { KAGEHA_DESKTOP_IMAGE: process.env.KAGEHA_DESKTOP_IMAGE } : {}),
    KAGEHA_HOME: path.join(root, "home"), KAGEHA_SERVER_TOKEN: token, KAGEHA_MODEL: "fixture",
    KAGEHA_MODELS_FILE: path.join(root, "models.yaml"), KAGEHA_MODEL_ENV_FILE: path.join(root, "models.env"),
    KAGEHA_LEGACY_SESSIONS_DIR: path.join(root, "legacy"), KAGEHA_BROWSER_STATE_DIR: browserRoot,
    ...(process.env.KAGEHA_WEBUI_DIST ? { KAGEHA_WEBUI_DIST: process.env.KAGEHA_WEBUI_DIST } : {}),
    KAGEHA_PROVIDER_URL: "http://127.0.0.1:" + model.address().port + "/v1/chat/completions",
    ...credentialFixture(root),
  }, "host");
  base = await until("Kageha server", () => {
    const match = fs.readFileSync(log, "utf8").match(/listening on (127\.0\.0\.1:\d+)/);
    return match && "http://" + match[1];
  });
  session = (await ok("/api/sessions", {})).session_id;
  const chat = "/api/sessions/" + session;
  const turn = (id, message) => ok("/api/chat/send", {
    session_id: session, client_request_id: id, message, auto_approve: true, execution_target: "machine",
  });
  const result = await turn("docker-shell", "/machine Verify Docker shell");
  assert.match(result.message, /Linux/);
  const state = await ok(chat + "/state");
  assert.equal(state.selected_target, "machine");
  assert.equal(state.turn.actual_target, "machine");
  const computer = await ok(chat + "/computer");
  container = computer.containerName;
  assert.match(container, /^kageha-linux-[a-f0-9]{32}$/);
  assert.equal(docker(["exec", container, "cat", "/work/outputs/proof.txt"]), "docker-proof");
  const terminal = await ok(chat + "/machine/exec", { command: "pwd; printf '%s' \"$KAGEHA_TEST\"" });
  assert.equal(terminal.stdout, "/work/outputs\nworkstation");
  const download = await fetch(base + chat + "/machine/file/outputs/proof.txt", { headers: { Authorization: "Bearer " + token } });
  assert.equal(download.status, 200);
  assert.equal(await download.text(), "docker-proof");
  console.log("Docker shell, shared files and pinned turn state verified");

  const html = "<title>Workstation test page</title><h1>Docker Chromium</h1>"
    + "<input type='file' id='attach' aria-label='Attach proof'>"
    + "<a id='download' href='data:text/plain,browser-proof' download='browser-proof.txt'>Download proof</a>";
  const serve = "#!/bin/sh\nprintf '%s\\r\\n' 'HTTP/1.1 200 OK' 'Content-Type: text/html' 'Connection: close' ''\nprintf '%s' " + JSON.stringify(html) + "\n";
  docker(["exec", "-i", container, "sh", "-c", "cat > /tmp/machine-test-http && chmod +x /tmp/machine-test-http"],
    { input: serve, stdio: ["pipe", "pipe", "pipe"] });
  docker(["exec", "-d", container, "socat", "TCP-LISTEN:18083,reuseaddr,fork,bind=127.0.0.1", "EXEC:/tmp/machine-test-http"]);
  await turn("docker-browser", "Open Docker Chromium");
  const selected = await until("fixture tab", async () => {
    const value = await ok(chat + "/browser");
    return value.title === "Workstation test page" && value;
  });
  assert.equal(selected.selectedProfile, "machine");
  assert.equal(selected.url, page);
  assert.match(docker(["exec", container, "xdotool", "getactivewindow", "getwindowname"]), /Workstation test page/);
  console.log("Agent browser tab opened in the visible container Chromium");

  const attachment = new FormData();
  attachment.append("file", new Blob(["browser-upload-proof"]), "proof-upload.txt");
  const uploaded = await fetch(base + chat + "/upload", {
    method: "POST", headers: { Authorization: "Bearer " + token }, body: attachment,
  });
  assert.equal(uploaded.status, 200);
  uploadPath = (await uploaded.json()).path;
  const transfer = turn("docker-upload", "Transfer a file through Docker Chromium");
  let transferDone = false;
  transfer.then(() => { transferDone = true; }, () => { transferDone = true; });
  await until("upload action or completion", async () => {
    if (transferDone) return true;
    const approval = (await ok(chat)).pending_approval;
    if (approval?.action === "browser_upload") {
      await ok("/api/approvals", { approval_id: approval.approval_id, approved: true, scope: "once" });
      return true;
    }
    return false;
  });
  assert.match((await transfer).message, /proof-upload\.txt/);
  console.log("Browser attached a chat-owned file through the Docker staging mount");

  await turn("browser-download", "Download the Chromium result");
  await until("Chromium download", () => {
    const data = docker(["exec", container, "cat", "/work/outputs/browser-proof.txt"]);
    return data === "browser-proof";
  });
  console.log("Chromium download arrived in the chat workstation files");



  const preview = () => ok(chat + "/computer/preview");
  const first = await preview();
  assert.deepEqual([first.result.width, first.result.height], [1440, 900]);
  assert.equal(first.result.image.mime_type, "image/jpeg");
  assert.equal((await ok(chat + "/computer/lease", { action: "take" })).lockedBy, "user");
  const browserLease = await ok(chat + "/browser");
  assert.equal(browserLease.lockedBy, "user");
  assert.equal((await api(chat + "/computer/action", { action: "key", key: "Escape", frame_id: first.frame_id })).status, 409);
  const fresh = await preview();
  assert.equal((await ok(chat + "/computer/action", { action: "key", key: "Escape", frame_id: fresh.frame_id })).status, "ok");
  await ok(chat + "/browser/lease", { action: "return", bindingId: browserLease.bindingId,
    controlGeneration: browserLease.controlGeneration });
  assert.equal((await ok(chat + "/computer")).lockedBy, "agent");
  console.log("Full-desktop preview, shared driver and stale-frame refusal verified");

  const desktopTurn = await turn("desktop-input", "Use the Docker terminal window");
  assert.equal(docker(["exec", container, "cat", "/work/outputs/desktop.txt"]), "workstation desktop input", desktopTurn.message);
  const job = await turn("docker-job", "Start and stop a Docker job");
  assert.equal(JSON.parse(job.message).state, "killed");
  console.log("Agent typed into a Linux terminal window and cancelled a background shell job");

  if (process.env.KAGEHA_DOCKER_UI_FILE) {
    const handoff = process.env.KAGEHA_DOCKER_UI_FILE;
    fs.writeFileSync(handoff, JSON.stringify({ base, session, token }), { mode: 0o600, flag: "wx" });
    console.log("Disposable browser UI ready for manual checks");
    await until("UI check", () => fs.existsSync(handoff + ".done"), 120000);
  }


  assert.equal((await ok("/api/machine", { session_id: session, command: "/machine status" })).ok, true);
  await ok(chat + "/machine", { paused: true });
  assert.equal((await ok(chat + "/machine")).runtime.paused, true);
  await ok(chat + "/machine", { paused: false });
  const beforeReconnect = await ok(chat + "/browser");
  await ok(chat + "/computer/refresh", {});
  const reconnected = await ok(chat + "/browser");
  assert.equal(reconnected.selectedProfile, "machine");
  assert.notEqual(reconnected.bindingId, beforeReconnect.bindingId);

  docker(["stop", "--time", "1", container]);
  const stopped = await api("/api/chat/send", {
    session_id: session, client_request_id: "stopped-docker", message: "Verify Docker shell", execution_target: "machine",
  });
  assert.equal(stopped.status, 409);
  assert.equal((await ok(chat + "/state")).selected_target, "machine");
  await ok(chat + "/machine", { active: true });
  assert.equal((await ok(chat + "/computer")).containerName, container);
  assert.equal(docker(["exec", container, "cat", "/work/outputs/proof.txt"]), "docker-proof");
  console.log("Browser reconnection and stopped-container failure retained the Docker binding");

  await ok("/api/machine", { session_id: session, command: "/machine stop" });
  assert.equal((await ok(chat + "/state")).selected_target, "local");
  assert.equal((await ok(chat + "/state")).turn.actual_target, "machine");
  await ok(chat, undefined, "DELETE");
  assert.equal(docker(["ps", "-aq", "--filter", "name=^" + container + "$"]), "");
  console.log("Machine lifecycle and container removal verified");

  session = (await ok("/api/sessions", {})).session_id;
  const legacyChat = "/api/sessions/" + session;
  backup = "kageha-linux-" + randomBytes(16).toString("hex");
  const legacyWork = path.join(root, "legacy-work");
  fs.mkdirSync(legacyWork);
  docker(["run", "-d", "--name", backup, "--mount", "type=bind,source=" + legacyWork + ",target=/work",
    process.env.KAGEHA_DESKTOP_IMAGE || "kageha-desktop:machine-v3"]);
  const database = path.join(root, "state", "native-chats.sqlite");
  execFileSync("/usr/bin/sqlite3", [database,
    "INSERT INTO ui_linux_desktops(chat,name,selected) VALUES ('" + session + "','" + backup + "',1)"]);
  assert.equal((await ok(legacyChat + "/machine")).upgrade_required, true);
  assert.equal((await api(legacyChat + "/machine", { active: true })).status, 409);
  assert.equal((await ok(legacyChat + "/computer")).containerName, backup);
  const upgraded = await ok(legacyChat + "/machine", { upgrade: true });
  assert.equal(upgraded.active, true);
  container = (await ok(legacyChat + "/computer")).containerName;
  assert.notEqual(container, backup);
  assert.equal(docker(["inspect", "--format", "{{.State.Status}}", backup]), "exited");
  assert.equal((await ok(legacyChat + "/state")).selected_target, "machine");
  await ok(legacyChat, undefined, "DELETE");
  assert.equal(docker(["ps", "-aq", "--filter", "name=^" + backup + "$"]), "");
  console.log("Explicit legacy upgrade retained the original container as a stopped backup");

} catch (error) {
  for (const { log } of children) process.stderr.write(fs.readFileSync(log, "utf8").slice(-4000));
  throw error;
} finally {
  const db = path.join(root, "state", "native-chats.sqlite");
  if (!container && session && fs.existsSync(db)) {
    container = execFileSync("/usr/bin/sqlite3", ["-readonly", db, "SELECT name FROM ui_linux_desktops"], { encoding: "utf8" }).trim();
  }
  for (const { child } of children.reverse()) {
    if (child.exitCode === null && child.signalCode === null) {
      child.kill("SIGTERM");
      await new Promise(resolve => child.once("exit", resolve));
    }
  }
  if (model.listening) await new Promise(resolve => model.close(resolve));
  for (const name of [container, backup]) {
    if (/^kageha-linux-[a-f0-9]{32}$/.test(name ?? "") && docker(["ps", "-aq", "--filter", "name=^" + name + "$"])) {
      docker(["rm", "-f", name]);
    }
  }
  fs.rmSync(root, { recursive: true, force: true });
}
