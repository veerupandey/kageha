import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import http from "node:http";
import { spawn, execFileSync } from "node:child_process";
import { DatabaseSync } from "node:sqlite";
import { credentialFixture } from "../../kageha-loop/tests/credential_fixture.mjs";
import { isolatedModelFixture } from "../../kageha-loop/tests/isolated_model_fixture.mjs";
const root = fs.mkdtempSync(path.join(os.tmpdir(), "kageha-worktrees-"));
const bin = path.resolve(
  process.env.CARGO_TARGET_DIR || "target/migration-acceptance",
  "debug/kageha-loop",
);
const state = path.join(root, "state"),
  workspace = path.join(root, "workspace"),
  empty = path.join(root, "empty");
fs.mkdirSync(workspace);
fs.mkdirSync(empty);
let child, base;
const requests = [];
const model = http.createServer(async (req, res) => {
  let text = "";
  for await (const b of req) text += b;
  const request = JSON.parse(text);
  requests.push(request);
  const intent =
    request.messages.filter((m) => m.role === "user").at(-1)?.content || "";
  const message = { role: "assistant", content: "Project fixture answered" };
  res.end(JSON.stringify({ choices: [{ message }] }));
});
await new Promise((r) => model.listen(0, "127.0.0.1", r));
const token = "shared-knowledge-fixture-access-token";
const env = {
  PATH: empty,
  KAGEHA_GIT_BIN: "/usr/bin/git",
  HOME: root,
  ...credentialFixture(root),
  ...isolatedModelFixture(root),
  KAGEHA_SERVER_TOKEN: token,
  KAGEHA_LEGACY_SESSIONS_DIR: empty,
  KAGEHA_PROVIDER_URL: `http://127.0.0.1:${model.address().port}/v1/chat/completions`,
  KAGEHA_MODEL: "fixture",
};
async function until(check) {
  for (let end = Date.now() + 15000; Date.now() < end; ) {
    const value = await check();
    if (value) return value;
    await new Promise((r) => setTimeout(r, 25));
  }
  throw Error("Acceptance timed out");
}
async function start() {
  let log = "";
  child = spawn(bin, ["serve", workspace, state, "127.0.0.1:0"], {
    env,
    cwd: root,
    stdio: ["ignore", "pipe", "pipe"],
  });
  child.stdout.on("data", (b) => (log += b));
  child.stderr.on("data", (b) => (log += b));
  await until(() => {
    if (child.exitCode !== null) throw Error(log);
    const m = log.match(/listening on (127\.0\.0\.1:\d+)/);
    if (m) {
      base = "http://" + m[1];
      return true;
    }
  });
}
async function stop() {
  if (child && child.exitCode === null && child.signalCode === null) {
    const exited = new Promise((r) => child.once("exit", r));
    child.kill("SIGTERM");
    await exited;
  }
}
async function api(route, method = "GET", body, status = 200) {
  const response = await fetch(base + route, {
    method,
    headers: {
      Authorization: "Bearer " + token,
      "Content-Type": "application/json",
    },
    body: body === undefined ? undefined : JSON.stringify(body),
    signal: AbortSignal.timeout(20000),
  });
  const text = await response.text();
  assert.equal(response.status, status, text);
  return text ? JSON.parse(text) : null;
}

function git(...args) {
  return execFileSync(
    "/usr/bin/git",
    [
      "-c",
      "user.name=Fixture",
      "-c",
      "user.email=fixture@example.invalid",
      "-c",
      "commit.gpgsign=false",
      ...args,
    ],
    {
      cwd: workspace,
      env: {
        PATH: "/usr/bin:/bin",
        GIT_CONFIG_NOSYSTEM: "1",
        GIT_CONFIG_GLOBAL: "/dev/null",
      },
      encoding: "utf8",
    },
  );
}
git("init", "-b", "main");
fs.writeFileSync(
  path.join(workspace, "evidence.txt"),
  "Retained checkout evidence",
);
git("add", "evidence.txt");
git("commit", "-m", "fixture");
let wrapperPid;
try {
  await start();
  assert.equal((await api("/api/meta")).features.worktrees, true);
  const inspection = await api("/api/project");
  assert.equal(inspection.git, true);
  assert.equal(inspection.worktrees.length, 1);
  await api(
    "/api/project?project_root=" + encodeURIComponent(root),
    "GET",
    undefined,
    403,
  );
  let list = await api("/api/worktrees");
  assert.equal(list.worktrees.length, 1);
  const first = await api("/api/worktrees", "POST", {
    request_id: "create-one",
    label: "agent",
    base: "HEAD",
  });
  assert.equal(first.operation.state, "completed");
  assert.equal(
    fs.readFileSync(path.join(first.path, "evidence.txt"), "utf8"),
    "Retained checkout evidence",
  );
  const duplicate = await api("/api/worktrees", "POST", {
    request_id: "create-one",
    label: "agent",
    base: "HEAD",
  });
  assert.equal(duplicate.path, first.path);
  assert.equal((await api("/api/worktrees")).worktrees.length, 2);
  await api(
    "/api/worktrees",
    "POST",
    { request_id: "create-one", label: "changed", base: "HEAD" },
    409,
  );
  await api(
    "/api/worktrees",
    "POST",
    { request_id: "outside", project_root: root, label: "agent", base: "HEAD" },
    403,
  );
  await api(
    "/api/worktrees",
    "POST",
    { request_id: "bad-ref", label: "agent", base: "--help" },
    409,
  );
  await stop();
  // A real host death after Git commits checkout but before publication.
  const marker = path.join(root, "git-finished"),
    pidFile = path.join(root, "wrapper-pid"),
    wrapper = path.join(root, "git-wrapper");
  fs.writeFileSync(
    wrapper,
    `#!/bin/sh\n/usr/bin/git "$@"\ncode=$?\ncase " $* " in *" worktree add "*) if [ "$code" = 0 ]; then echo $$ > '${pidFile}'; : > '${marker}'; /bin/sleep 30; fi;; esac\nexit "$code"\n`,
    { mode: 0o700 },
  );
  env.KAGEHA_GIT_BIN = wrapper;
  await start();
  const interrupted = api("/api/worktrees", "POST", {
    request_id: "crash-one",
    label: "crash",
    base: "HEAD",
  }).catch(() => null);
  await until(() => fs.existsSync(marker));
  wrapperPid = Number(fs.readFileSync(pidFile, "utf8"));
  let exited = new Promise((r) => child.once("exit", r));
  child.kill("SIGKILL");
  await exited;
  try {
    process.kill(-wrapperPid, "SIGKILL");
  } catch {}
  wrapperPid = null;
  await interrupted;
  env.KAGEHA_GIT_BIN = "/usr/bin/git";
  await start();
  list = await api("/api/worktrees");
  assert.equal(list.worktrees.length, 3);
  assert.equal(list.operations[0].id, "crash-one");
  assert.equal(list.operations[0].state, "dispatched");
  const prior = await api(
    "/api/worktrees",
    "POST",
    { request_id: "crash-one", label: "crash", base: "HEAD" },
    409,
  );
  assert.equal(prior.operation.state, "dispatched");
  assert.equal((await api("/api/worktrees")).worktrees.length, 3);
  await api(
    "/api/worktrees",
    "POST",
    { request_id: "blocked-after-crash", label: "new", base: "HEAD" },
    409,
  );
  const repaired = await api("/api/worktrees/operations/crash-one", "POST", {});
  assert.equal(repaired.operation.state, "completed");
  assert.equal((await api("/api/worktrees")).worktrees.length, 3);
  const after = await api("/api/worktrees", "POST", {
    request_id: "after-repair",
    label: "after",
    base: "HEAD",
  });
  assert.equal(after.operation.state, "completed");
  assert.equal((await api("/api/worktrees")).worktrees.length, 4);
  // Managed cleanup retains local work, including ignored output.
  const current = (await api("/api/worktrees")).worktrees.find(
    (w) => w.path === after.path,
  );
  assert.equal(current.creation_id, "after-repair");
  await api(
    "/api/worktrees/cleanup",
    "POST",
    { request_id: "bad-cleanup", creation_id: "unknown" },
    409,
  );
  fs.writeFileSync(
    path.join(after.path, "evidence.txt"),
    "Preserved edited work",
  );
  await api(
    "/api/worktrees/cleanup",
    "POST",
    { request_id: "cleanup-after", creation_id: "after-repair" },
    409,
  );
  assert.equal(
    fs.readFileSync(path.join(after.path, "evidence.txt"), "utf8"),
    "Preserved edited work",
  );
  fs.writeFileSync(
    path.join(after.path, "evidence.txt"),
    "Retained checkout evidence",
  );
  fs.writeFileSync(path.join(workspace, ".git/info/exclude"), "ignored.log\n");
  fs.writeFileSync(
    path.join(after.path, "ignored.log"),
    "Preserved ignored work",
  );
  await api(
    "/api/worktrees/cleanup",
    "POST",
    { request_id: "cleanup-after", creation_id: "after-repair" },
    409,
  );
  assert.equal(
    fs.readFileSync(path.join(after.path, "ignored.log"), "utf8"),
    "Preserved ignored work",
  );
  fs.unlinkSync(path.join(after.path, "ignored.log"));
  await stop();
  async function crashCleanupAt(pattern) {
    fs.rmSync(marker, { force: true });
    fs.writeFileSync(
      wrapper,
      `#!/bin/sh\n/usr/bin/git "$@"\ncode=$?\ncase " $* " in *" ${pattern} "*) if [ "$code" = 0 ]; then echo $$ > '${pidFile}'; : > '${marker}'; /bin/sleep 30; fi;; esac\nexit "$code"\n`,
      { mode: 0o700 },
    );
    env.KAGEHA_GIT_BIN = wrapper;
    await start();
    const interrupted = api("/api/worktrees/cleanup", "POST", {
      request_id: "cleanup-after",
      creation_id: "after-repair",
    }).catch(() => null);
    await until(() => fs.existsSync(marker));
    wrapperPid = Number(fs.readFileSync(pidFile, "utf8"));
    const exited = new Promise((r) => child.once("exit", r));
    child.kill("SIGKILL");
    await exited;
    try {
      process.kill(-wrapperPid, "SIGKILL");
    } catch {}
    wrapperPid = null;
    await interrupted;
    env.KAGEHA_GIT_BIN = "/usr/bin/git";
    await start();
  }
  await crashCleanupAt("worktree remove");
  assert.equal(fs.existsSync(after.path), false);
  assert.equal(
    (await api("/api/worktrees")).cleanup_operations[0].state,
    "removing",
  );
  const heldCleanup = await api(
    "/api/worktrees/cleanup",
    "POST",
    { request_id: "cleanup-after", creation_id: "after-repair" },
    409,
  );
  assert.equal(heldCleanup.cleanup.state, "removing");
  await api(
    "/api/worktrees",
    "POST",
    { request_id: "during-cleanup", label: "wait", base: "HEAD" },
    409,
  );
  assert.equal(
    (await api("/api/worktrees/cleanup/cleanup-after", "POST", {})).cleanup
      .state,
    "worktree_removed",
  );
  assert(git("show-ref", "--verify", `refs/heads/${after.branch}`));
  await stop();
  await crashCleanupAt("update-ref -d");
  assert.equal(
    (await api("/api/worktrees")).cleanup_operations[0].state,
    "deleting_branch",
  );
  assert.equal(
    (await api("/api/worktrees/cleanup/cleanup-after", "POST", {})).cleanup
      .state,
    "completed",
  );
  assert.equal(
    (
      await api("/api/worktrees/cleanup", "POST", {
        request_id: "cleanup-after",
        creation_id: "after-repair",
      })
    ).cleanup.state,
    "completed",
  );
  await api(
    "/api/worktrees/cleanup",
    "POST",
    { request_id: "cleanup-after", creation_id: "create-one" },
    409,
  );
  assert.equal((await api("/api/worktrees")).cleanup_operations.length, 0);
  const fresh = await api("/api/worktrees", "POST", {
    request_id: "after-cleanup",
    label: "new",
    base: "HEAD",
  });
  assert.equal(fresh.operation.state, "completed");
  console.log(
    JSON.stringify({
      passed: true,
      python_backend: false,
      existing_worktree_api: true,
      idempotent_create: true,
      scope_isolation: true,
      real_host_crash: true,
      no_mutation_replay: true,
      observation_recovery: true,
      independent_git_library: true,
      managed_cleanup: true,
      ignored_work_retained: true,
      real_cleanup_crash_each_stage: true,
      cleanup_no_replay: true,
    }),
  );
} finally {
  if (wrapperPid) {
    try {
      process.kill(-wrapperPid, "SIGKILL");
    } catch {}
  }
  await stop();
  model.closeAllConnections();
  await new Promise((r) => model.close(r));
}
