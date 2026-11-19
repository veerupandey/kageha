// Disposable installed composition; no checkout resource, Python or Node on backend PATH.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import os from "node:os";
import { spawn, spawnSync } from "node:child_process";
import { credentialFixture } from "../../kageha-loop/tests/credential_fixture.mjs";
import { isolatedModelFixture } from "../../kageha-loop/tests/isolated_model_fixture.mjs";

const root = fs.mkdtempSync(path.join(os.tmpdir(), "kageha-installed-"));
const sourceBin = path.resolve(
  process.env.CARGO_TARGET_DIR || "target/migration-acceptance",
  "debug",
);
const bin = path.join(root, "bin");
const resources = path.join(root, "share/kageha");
fs.mkdirSync(bin, { recursive: true });
fs.mkdirSync(path.join(resources, "webui/assets"), { recursive: true });
fs.mkdirSync(path.join(root, "workspace"));
fs.mkdirSync(path.join(root, "empty"));
for (const name of ["kageha", "kageha-loop"])
  fs.copyFileSync(path.join(sourceBin, name), path.join(bin, name));
fs.writeFileSync(
  path.join(resources, "webui/index.html"),
  "<title>Installed Rust fixture</title>",
);
fs.writeFileSync(
  path.join(resources, "webui/assets/app.js"),
  "/* fixture asset */",
);
fs.writeFileSync(path.join(resources, "models.yaml"), "providers: {}\n");
fs.writeFileSync(path.join(root, "private"), "not a resource");
fs.symlinkSync(
  path.join(root, "private"),
  path.join(resources, "webui/assets/escape"),
);
const env = {
  PATH: path.join(root, "empty"),
  HOME: root,
  ...credentialFixture(root),
  ...isolatedModelFixture(root),
  KAGEHA_LEGACY_SESSIONS_DIR: path.join(root, "empty"),
};
const doctor = spawnSync(path.join(bin, "kageha"), ["doctor"], {
  cwd: root,
  env,
  encoding: "utf8",
});
assert.equal(doctor.status, 0, doctor.stderr);
assert.equal(JSON.parse(doctor.stdout).ready, true);
const invalid = spawnSync(
  path.join(bin, "kageha"),
  ["doctor", "--resources", path.join(root, "absent")],
  { cwd: root, env, encoding: "utf8" },
);
assert.notEqual(invalid.status, 0);
assert.equal(JSON.parse(invalid.stdout).ready, false);
let child, address, token;
async function start() {
  let log = "";
  child = spawn(
    path.join(bin, "kageha"),
    [
      "up",
      "--workspace",
      path.join(root, "workspace"),
      "--state-dir",
      path.join(root, "state"),
      "--bind",
      "127.0.0.1:0",
    ],
    { cwd: root, env, stdio: ["ignore", "pipe", "pipe"] },
  );
  child.stdout.on("data", (b) => {
    log += b;
  });
  child.stderr.on("data", (b) => {
    log += b;
  });
  for (const end = Date.now() + 15000; Date.now() < end; ) {
    if (child.exitCode !== null) throw Error(log);
    const match = log.match(/listening on (127\.0\.0\.1:\d+)/);
    if (match) {
      address = `http://${match[1]}`;
      token = fs.readFileSync(path.join(root, "state/server.token"), "utf8");
      return;
    }
    await new Promise((r) => setTimeout(r, 25));
  }
  throw Error("Installed host startup timed out: " + log);
}
async function stop() {
  if (child && child.exitCode === null && child.signalCode === null) {
    const done = new Promise((r) => child.once("exit", r));
    child.kill("SIGTERM");
    await done;
  }
}
async function api(route, body) {
  const res = await fetch(address + route, {
    method: body ? "POST" : "GET",
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
    },
    body: body ? JSON.stringify(body) : undefined,
    signal: AbortSignal.timeout(10000),
  });
  assert.equal(res.status, 200);
  return res.json();
}
try {
  await start();
  const initialToken = token;
  assert.equal(
    fs.statSync(path.join(root, "state/server.token")).mode & 0o777,
    0o600,
  );
  const login = await fetch(address, { redirect: "manual" });
  assert.equal(login.status, 303);
  const page = await fetch(address, {
    headers: { Cookie: `kageha_native_session=${token}` },
  });
  assert.equal(page.status, 200);
  assert((await page.text()).includes("Installed Rust fixture"));
  assert.equal((await fetch(address + "/assets/app.js")).status, 200);
  assert.equal((await fetch(address + "/assets/escape")).status, 404);
  const chat = (await api("/api/sessions", {})).session_id;
  assert(chat);
  await stop();
  await start();
  assert.equal(token, initialToken);
  assert.equal((await api(`/api/sessions/${chat}`)).session_id, chat);
  console.log(
    JSON.stringify({
      passed: true,
      python_backend: false,
      node_backend: false,
      relocated_installation: true,
      static_webui: true,
      restart_preserves_chat: true,
      resource_escape_denied: true,
    }),
  );
} finally {
  await stop();
}
