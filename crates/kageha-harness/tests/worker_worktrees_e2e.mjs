import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import http from "node:http";
import { spawn, execFileSync } from "node:child_process";
import { DatabaseSync } from "node:sqlite";
import { credentialFixture } from "../../kageha-loop/tests/credential_fixture.mjs";
import { isolatedModelFixture } from "../../kageha-loop/tests/isolated_model_fixture.mjs";
const root = fs.mkdtempSync(path.join(os.tmpdir(), "kageha-worker-worktrees-"));
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
const held = new Map();
const model = http.createServer(async (req, res) => {
  let text = "";
  for await (const b of req) text += b;
  const request = JSON.parse(text);
  requests.push(request);
  const intent =
    request.messages.filter((m) => m.role === "user").at(-1)?.content || "";
  const reply = (message) =>
    res.end(JSON.stringify({ choices: [{ message }] }));
  if (intent.startsWith("held-")) {
    held.set(intent, () =>
      reply({ role: "assistant", content: "Held worker completed" }),
    );
    return;
  }
  if (
    intent === "delegate-isolated" &&
    request.messages.at(-1)?.role !== "tool"
  ) {
    reply({
      role: "assistant",
      content: null,
      tool_calls: [
        {
          id: "delegate-worker",
          type: "function",
          function: {
            name: "agent_spawn",
            arguments: JSON.stringify({
              agent_id: "worker",
              objective: "read-worker-delegated",
              isolation: "worktree",
              keep_worktree: false,
            }),
          },
        },
      ],
    });
    return;
  }
  if (
    (intent === "nested-inherit" || intent === "nested-shared") &&
    request.messages.at(-1)?.role !== "tool"
  ) {
    reply({
      role: "assistant",
      content: null,
      tool_calls: [
        {
          id: "nested-worker",
          type: "function",
          function: {
            name: "agent_spawn",
            arguments: JSON.stringify({
              agent_id: "worker",
              objective: "read-worker-nested",
              ...(intent === "nested-shared" ? { isolation: "shared" } : {}),
            }),
          },
        },
      ],
    });
    return;
  }
  if (
    intent === "worker-knowledge" &&
    request.messages.at(-1)?.role !== "tool"
  ) {
    reply({
      role: "assistant",
      content: null,
      tool_calls: [
        {
          id: "knowledge",
          type: "function",
          function: { name: "knowledge_list", arguments: "{}" },
        },
      ],
    });
    return;
  }
  if (request.messages.at(-1)?.role === "tool") {
    reply({ role: "assistant", content: request.messages.at(-1).content });
    return;
  }
  if (intent.startsWith("read-worker")) {
    if (intent === "read-worker-hook") {
      const context = request.messages
        .filter((m) => m.role === "system")
        .map((m) => m.content)
        .join("\n");
      assert(context.includes("HOOK_ROOT="));
      assert(context.includes("/worktrees/specialist-"));
      assert(!context.includes(`HOOK_ROOT=${workspace}\n`));
    }
    const checkouts = fs
      .readdirSync(path.join(workspace, ".kageha/worktrees"))
      .filter((n) => n.startsWith("specialist-"));
    if (intent === "read-worker-dirty") {
      const target = checkouts
        .map((n) => path.join(workspace, ".kageha/worktrees", n))
        .find((p) => !seen.has(p));
      assert(target);
      fs.writeFileSync(
        path.join(target, "ignored.log"),
        "Retained worker output",
      );
    }
    for (const n of checkouts)
      seen.add(path.join(workspace, ".kageha/worktrees", n));
    reply({
      role: "assistant",
      content: null,
      tool_calls: [
        {
          id: "read-proof",
          type: "function",
          function: {
            name: "read_file",
            arguments: JSON.stringify({
              path:
                intent === "read-worker-outside"
                  ? path.join(workspace, "evidence.txt")
                  : "evidence.txt",
            }),
          },
        },
      ],
    });
    return;
  }
  reply({ role: "assistant", content: "Isolated worker completed" });
});
const seen = new Set();
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
async function start(serveRoot = workspace, serveState = state) {
  let log = "";
  child = spawn(bin, ["serve", serveRoot, serveState, "127.0.0.1:0"], {
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
  "Committed isolated proof",
);
git("add", "evidence.txt");
git("commit", "-m", "fixture");
// The main checkout's dirty edit must never leak into an isolated read.
fs.writeFileSync(
  path.join(workspace, "evidence.txt"),
  "PRIVATE SOURCE DIRTY EDIT",
);
fs.writeFileSync(path.join(workspace, ".git/info/exclude"), "ignored.log\n");
const terminal = (j) =>
  ["succeeded", "failed", "cancelled", "interrupted"].includes(j.state);
let wrapperPid;
try {
  await start();
  await api("/api/agents", "POST", {
    id: "worker",
    name: "Isolated specialist",
  });
  const parent = (await api("/api/sessions", "POST", {})).session_id;
  const submit = (id, objective, extra = {}) =>
    api("/api/jobs", "POST", {
      session_id: parent,
      agent_id: "worker",
      objective,
      client_request_id: id,
      isolation: "worktree",
      ...extra,
    });
  async function finished(job) {
    return until(async () => {
      const row = (await api(`/api/jobs/${job.id}`)).job;
      return terminal(row) ? row : null;
    });
  }
  let kept = await finished(await submit("kept", "read-worker-kept"));
  assert.equal(kept.state, "succeeded", JSON.stringify(kept));
  assert.equal(kept.spec.isolation, "worktree");
  assert.equal(kept.spec.keep_worktree, true);
  assert.equal(kept.result.message, "Committed isolated proof");
  const keptPath = kept.workspace.operation.handle.path;
  assert(fs.existsSync(keptPath));
  const workspaceState = (await api(`/api/sessions/${kept.session_id}/state`))
    .workspace;
  assert.equal(workspaceState.mode, "worktree");
  assert.equal(workspaceState.root, keptPath);
  assert.equal(
    fs.readFileSync(path.join(workspace, "evidence.txt"), "utf8"),
    "PRIVATE SOURCE DIRTY EDIT",
  );
  const repeat = await submit("kept", "read-worker-kept");
  assert.equal(repeat.id, kept.id);
  assert.equal(repeat.workspace.operation.handle.path, keptPath);
  await api(
    "/api/jobs",
    "POST",
    {
      session_id: parent,
      agent_id: "worker",
      objective: "read-worker-kept",
      client_request_id: "kept",
      isolation: "shared",
    },
    409,
  );
  const clean = await finished(
    await submit("clean", "read-worker-clean", { keep_worktree: false }),
  );
  assert.equal(clean.state, "succeeded", JSON.stringify(clean));
  assert.equal(clean.workspace.cleanup.state, "completed");
  assert(!fs.existsSync(clean.workspace.operation.handle.path));
  const dirty = await finished(
    await submit("dirty", "read-worker-dirty", { keep_worktree: false }),
  );
  assert.equal(dirty.state, "succeeded", JSON.stringify(dirty));
  assert.equal(
    fs.readFileSync(
      path.join(dirty.workspace.operation.handle.path, "ignored.log"),
      "utf8",
    ),
    "Retained worker output",
  );
  assert.equal(dirty.result.workspace.retained, true);
  const outside = await finished(
    await submit("outside-read", "read-worker-outside"),
  );
  assert(!outside.result.message.includes("PRIVATE SOURCE DIRTY EDIT"));
  assert(
    outside.result.message.includes("inside the workspace"),
    outside.result.message,
  );
  await api("/api/knowledge-bases", "POST", { id: "notes" }, 201);
  await api("/api/agents/worker", "PUT", {
    knowledge: { inherit_workspace: true, extra_kb_ids: ["notes"] },
  });
  const knowledge = await finished(
    await submit("knowledge", "worker-knowledge"),
  );
  assert.equal(JSON.parse(knowledge.result.message).shared[0].id, "notes");
  await api("/api/chat/send", "POST", {
    session_id: parent,
    client_request_id: "delegate-isolated",
    message: "delegate-isolated",
  });
  const delegated = await until(async () => {
    const jobs = (await api(`/api/jobs?session_id=${parent}`)).jobs;
    const j = jobs.find((j) => j.objective === "read-worker-delegated");
    return j && terminal(j) ? j : null;
  });
  assert.equal(delegated.result.message, "Committed isolated proof");
  assert.equal(delegated.workspace.cleanup.state, "completed");
  const nested = await finished(await submit("nested", "nested-inherit"));
  const nestedChild = await until(async () => {
    const jobs = (await api(`/api/jobs?session_id=${nested.session_id}`)).jobs;
    return jobs.find(
      (j) => j.parent_session === nested.session_id && terminal(j),
    );
  });
  assert.equal(nestedChild.workspace.mode, "worktree");
  assert.notEqual(
    nestedChild.workspace.operation.handle.path,
    nested.workspace.operation.handle.path,
  );
  assert.equal(nestedChild.result.message, "Committed isolated proof");
  const blocked = await finished(
    await submit("nested-shared", "nested-shared"),
  );
  assert(blocked.result.message.includes("cannot delegate into the shared"));
  assert.equal(
    (await api(`/api/jobs?session_id=${blocked.session_id}`)).jobs.filter(
      (j) => j.parent_session === blocked.session_id,
    ).length,
    0,
  );
  const parallel = await Promise.all([
    submit("parallel-one", "held-parallel-one"),
    submit("parallel-two", "held-parallel-two"),
  ]);
  await until(
    () => held.has("held-parallel-one") && held.has("held-parallel-two"),
  );
  const parallelRows = await Promise.all(
    parallel.map(async (j) => (await api(`/api/jobs/${j.id}`)).job),
  );
  assert.notEqual(
    parallelRows[0].workspace.operation.handle.path,
    parallelRows[1].workspace.operation.handle.path,
  );
  held.get("held-parallel-one")();
  held.get("held-parallel-two")();
  held.delete("held-parallel-one");
  held.delete("held-parallel-two");
  for (const j of parallel)
    assert.equal((await finished(j)).state, "succeeded");
  const heldJob = await submit("held", "held-worker", { keep_worktree: false });
  await until(() => held.has("held-worker"));
  const activeHeld = (await api(`/api/jobs/${heldJob.id}`)).job;
  const activeRemoval = await api(
    "/api/worktrees/cleanup",
    "POST",
    {
      request_id: "active-cleanup",
      creation_id: activeHeld.workspace.operation.id,
    },
    409,
  );
  assert(activeRemoval.error.includes("active specialist"));
  assert(fs.existsSync(activeHeld.workspace.operation.handle.path));

  await api(`/api/jobs/${heldJob.id}/cancel`, "POST", {});
  const cancelled = await finished(heldJob);
  assert.equal(cancelled.state, "cancelled");
  assert(fs.existsSync(cancelled.workspace.operation.handle.path));
  held.get("held-worker")();
  held.delete("held-worker");
  assert.equal(cancelled.can_resume, true, JSON.stringify(cancelled));
  await api(`/api/jobs/${heldJob.id}/resume`, "POST", {
    client_request_id: "resume-held",
    confirmed: true,
  });
  const resumed = await finished(heldJob);
  assert.equal(resumed.state, "succeeded", JSON.stringify(resumed));
  assert.equal(
    resumed.workspace.operation.id,
    cancelled.workspace.operation.id,
  );
  assert.equal(
    resumed.workspace.operation.handle.path,
    cancelled.workspace.operation.handle.path,
  );
  assert.equal(resumed.workspace.cleanup.state, "completed");
  const removedJob = await submit("removed-stopped", "held-removed");
  await until(() => held.has("held-removed"));
  await api(`/api/jobs/${removedJob.id}/cancel`, "POST", {});
  const stopped = await finished(removedJob);
  held.get("held-removed")();
  held.delete("held-removed");
  assert.equal(stopped.can_resume, true);
  await api("/api/worktrees/cleanup", "POST", {
    request_id: "removed-stopped-cleanup",
    creation_id: stopped.workspace.operation.id,
  });
  const missing = (await api(`/api/jobs/${removedJob.id}`)).job;
  assert.equal(missing.can_resume, false);
  assert.equal(missing.workspace.ready_for_input, false);
  await api(
    `/api/jobs/${removedJob.id}/resume`,
    "POST",
    { client_request_id: "must-not-recreate", confirmed: true },
    409,
  );
  await api("/api/permissions", "POST", {
    session_id: parent,
    profile: "full",
    confirm_full: true,
  });
  const hookFile = path.join(workspace, ".kageha/hooks.json");
  fs.writeFileSync(
    hookFile,
    JSON.stringify([
      {
        event: "sessionStart",
        command: "printf 'HOOK_ROOT=%s\\n' \"$KAGEHA_PROJECT_ROOT\"",
      },
    ]),
  );
  const hooked = await finished(
    await submit("hooked", "read-worker-hook", { keep_worktree: false }),
  );
  assert.equal(hooked.state, "succeeded", JSON.stringify(hooked));
  assert.equal(hooked.workspace.cleanup.state, "completed");
  fs.unlinkSync(hookFile);
  await api(
    "/api/jobs",
    "POST",
    {
      session_id: parent,
      agent_id: "worker",
      objective: "bad",
      client_request_id: "bad",
      isolation: "bad",
    },
    400,
  );
  // Non-Git isolation fails admission before any worker model call; no shared fallback.
  fs.renameSync(
    path.join(workspace, ".git"),
    path.join(workspace, ".git-hidden"),
  );
  await api(
    "/api/jobs",
    "POST",
    {
      session_id: parent,
      agent_id: "worker",
      objective: "not-git",
      client_request_id: "not-git",
      isolation: "worktree",
    },
    409,
  );
  fs.renameSync(
    path.join(workspace, ".git-hidden"),
    path.join(workspace, ".git"),
  );
  assert(
    !requests.some((r) =>
      r.messages.some((m) => m.role === "user" && m.content === "not-git"),
    ),
  );
  await stop();
  const marker = path.join(root, "checkout-finished"),
    pidFile = path.join(root, "wrapper-pid"),
    wrapper = path.join(root, "git-wrapper");
  fs.writeFileSync(
    wrapper,
    `#!/bin/sh\n/usr/bin/git "$@"\ncode=$?\ncase " $* " in *" worktree add "*) if [ "$code" = 0 ]; then echo $$ > '${pidFile}'; : > '${marker}'; /bin/sleep 30; fi;; esac\nexit "$code"\n`,
    { mode: 0o700 },
  );
  env.KAGEHA_GIT_BIN = wrapper;
  await start();
  const crashed = await submit("crash", "never-started-worker");
  await until(() => fs.existsSync(marker));
  wrapperPid = Number(fs.readFileSync(pidFile, "utf8"));
  const exited = new Promise((r) => child.once("exit", r));
  child.kill("SIGKILL");
  await exited;
  try {
    process.kill(-wrapperPid, "SIGKILL");
  } catch {}
  wrapperPid = null;
  env.KAGEHA_GIT_BIN = "/usr/bin/git";
  await start();
  const recovered = await finished(crashed);
  assert.equal(recovered.state, "interrupted", JSON.stringify(recovered));
  const before = git("worktree", "list", "--porcelain");
  const same = await submit("crash", "never-started-worker");
  assert.equal(same.id, crashed.id);
  assert.equal(git("worktree", "list", "--porcelain"), before);
  assert(
    !requests.some((r) =>
      r.messages.some(
        (m) => m.role === "user" && m.content === "never-started-worker",
      ),
    ),
  );
  const op = (await api("/api/worktrees")).operations.find(
    (o) => o.id === `worker-${crashed.id}`,
  );
  assert(op);
  await api(`/api/worktrees/operations/${op.id}`, "POST", {});
  assert.equal((await api(`/api/jobs/${crashed.id}`)).job.can_resume, false);
  await stop();
  await start();
  assert(fs.existsSync(keptPath));
  assert.equal(
    (await api(`/api/jobs/${kept.id}`)).job.result.message,
    "Committed isolated proof",
  );
  await stop();
  const narrow = path.join(workspace, "narrow-scope");
  fs.mkdirSync(narrow);
  await start(narrow, path.join(root, "narrow-state"));
  await api("/api/agents", "POST", { id: "worker", name: "Scoped worker" });
  const narrowParent = (await api("/api/sessions", "POST", {})).session_id;
  const requestsBefore = requests.length;
  const denied = await api(
    "/api/jobs",
    "POST",
    {
      session_id: narrowParent,
      agent_id: "worker",
      objective: "must not expand",
      client_request_id: "narrow",
      isolation: "worktree",
    },
    409,
  );
  assert(denied.error.includes("subdirectory scope"));
  assert.equal(requests.length, requestsBefore);
  assert.equal((await api("/api/jobs")).jobs.length, 0);
  console.log(
    JSON.stringify({
      passed: true,
      python_backend: false,
      specialist_worktree_isolation: true,
      subdirectory_scope_denied: true,
      accurate_session_workspace_state: true,
      agent_tool_isolation: true,
      nested_isolation_inherited: true,
      parallel_worker_checkouts: true,
      shared_delegation_blocked: true,
      same_checkout_resume: true,
      source_knowledge_namespace_preserved: true,
      hook_checkout_context: true,
      outside_source_read_denied: true,
      source_dirty_edit_excluded: true,
      default_retention: true,
      successful_clean_settlement: true,
      changed_output_retained: true,
      cancelled_worker_retained: true,
      active_worker_cleanup_blocked: true,
      removed_checkout_resume_blocked: true,
      idempotent_workspace_policy: true,
      non_git_no_fallback: true,
      crash_before_model_no_replay: true,
      restart_preserves_worker_paths: true,
    }),
  );
} finally {
  if (wrapperPid) {
    try {
      process.kill(-wrapperPid, "SIGKILL");
    } catch {}
  }
  for (const release of held.values()) release();
  await stop();
  model.closeAllConnections();
  await new Promise((r) => model.close(r));
}
