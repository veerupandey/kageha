import assert from "node:assert/strict";
import { DatabaseSync } from "node:sqlite";
import { createHash } from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import http from "node:http";
import { spawn, execFileSync } from "node:child_process";
const root = fs.mkdtempSync("/tmp/kageha-standalone-workers-"),
  source = path.join(root, "source"),
  state = path.join(root, "state"),
  empty = path.join(root, "empty");
fs.mkdirSync(source);
fs.mkdirSync(empty);
const bin = path.resolve(
  process.env.CARGO_TARGET_DIR || "target/migration-acceptance",
  "debug/kageha-runtime-service",
);
const documentWorker = path.resolve(
  process.env.CARGO_TARGET_DIR || "target/migration-acceptance",
  "debug/kageha-workspace",
);
const token = "standalone-worker-disposable-fixture-token";
fs.writeFileSync(path.join(root, "token"), token, { mode: 0o600 });
const modelErrors = [];
const requests = [],
  held = new Map();
let child,
  base,
  logs = "",
  foreignId,
  wrapperPid;
const pause = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(fn) {
  for (let end = Date.now() + 20000; Date.now() < end; ) {
    if (modelErrors.length) throw modelErrors[0];
    const v = await fn();
    if (v) return v;
    await pause(20);
  }
  throw Error("Acceptance timed out: " + logs);
}
const model = http.createServer(async (req, res) => {
  try {
    let text = "";
    for await (const b of req) text += b;
    const request = JSON.parse(text);
    requests.push(request);
    const intent =
      request.messages.filter((m) => m.role === "user").at(-1)?.content || "";
    const tools = request.messages.filter((m) => m.role === "tool");
    const reply = (message) => {
      if (!res.destroyed) res.end(JSON.stringify({ choices: [{ message }] }));
    };
    const call = (name, args, id = "call-one") =>
      reply({
        role: "assistant",
        content: null,
        tool_calls: [
          {
            id,
            type: "function",
            function: { name, arguments: JSON.stringify(args) },
          },
        ],
      });
    if (intent.startsWith("hold")) {
      held.set(intent, () =>
        reply({ role: "assistant", content: "Held worker continued" }),
      );
      return;
    }
    if (intent === "parallel") {
      if (!tools.length) {
        reply({
          role: "assistant",
          content: null,
          tool_calls: ["one", "two"].map((id) => ({
            id,
            type: "function",
            function: {
              name: "agent_spawn",
              arguments: JSON.stringify({
                agent_id: "worker",
                objective: "isolated child",
                isolation: "worktree",
              }),
            },
          })),
        });
        return;
      }
      reply({
        role: "assistant",
        content: JSON.stringify(
          tools.map((t) => JSON.parse(t.content).spec.id),
        ),
      });
      return;
    }
    if (intent === "foreign") {
      if (!tools.length) {
        call("agent_status", { job_id: foreignId });
        return;
      }
      reply({ role: "assistant", content: tools.at(-1).content });
      return;
    }
    if (intent === "uncertain") {
      const tool = request.tools.find(
        (t) =>
          t.function.name ===
          "mcp_" +
            createHash("sha256")
              .update("fixture:crash")
              .digest("hex")
              .slice(0, 32),
      );
      assert(tool);
      call(tool.function.name, {});
      return;
    }
    if (intent === "repeat-completed") {
      const old = request.messages.flatMap((m) => m.tool_calls || [])[0];
      call("read_file", { path: "proof.txt" }, old.id);
      return;
    }
    if (intent === "mutate-once") {
      if (!tools.length) {
        call(
          "mcp_" +
            createHash("sha256")
              .update("fixture:count")
              .digest("hex")
              .slice(0, 32),
          {},
          "original-mutation",
        );
        return;
      }
      held.set(intent, () => reply({ role: "assistant", content: "done" }));
      return;
    }
    if (intent === "continue-after-read") {
      if (!tools.length) {
        call("read_file", { path: "proof.txt" }, "confirmed-read");
        return;
      }
      held.set(intent, () => reply({ role: "assistant", content: "done" }));
      return;
    }
    if (intent.startsWith("Continue the existing assignment")) {
      const oldIntent = request.messages.find(
        (m) => m.role === "user",
      )?.content;
      if (oldIntent === "mutate-once") {
        call(
          "mcp_" +
            createHash("sha256")
              .update("fixture:count")
              .digest("hex")
              .slice(0, 32),
          {},
          "fresh-mutation-id",
        );
        return;
      }
      if (oldIntent === "continue-after-read") {
        const old = request.messages.flatMap((m) => m.tool_calls || [])[0];
        call("read_file", { path: "proof.txt" }, old.id);
        return;
      }
    }
    if (intent === "parent" || intent === "parent-wait-all") {
      if (!tools.length) {
        call(
          "agent_spawn",
          {
            agent_id: "worker",
            objective: "isolated child",
            isolation: "worktree",
            keep_worktree: false,
          },
          "spawn-child",
        );
        return;
      }
      const child = JSON.parse(tools[0].content);
      if (tools.length === 1) {
        call(
          intent === "parent-wait-all" ? "agent_wait_all" : "agent_wait_any",
          { job_ids: [child.spec.id], timeout_ms: 10000 },
          "wait-child",
        );
        return;
      }
      const finished = JSON.parse(tools.at(-1).content).finished;
      if (intent === "parent-wait-all") {
        const result = JSON.parse(tools.at(-1).content);
        assert.equal(result.timed_out, false);
        assert.deepEqual(result.pending_job_ids, []);
      }
      assert.equal(
        finished[0].completion.result.message,
        "Committed source proof",
      );
      reply({
        role: "assistant",
        content: "Parent and isolated worker finished",
      });
      return;
    }
    if (intent === "nested") {
      if (!tools.length) {
        call(
          "agent_spawn",
          { agent_id: "worker", objective: "isolated child" },
          "nested-child",
        );
        return;
      }
      reply({ role: "assistant", content: tools.at(-1).content });
      return;
    }
    if (intent === "blocked nesting") {
      if (!tools.length) {
        call(
          "agent_spawn",
          {
            agent_id: "worker",
            objective: "must not escape",
            isolation: "shared",
          },
          "shared-child",
        );
        return;
      }
      reply({ role: "assistant", content: tools.at(-1).content });
      return;
    }
    if (intent === "forged tool") {
      assert(!request.tools.some((t) => t.function.name === "read_file"));
      call("read_file", { path: "proof.txt" }, "forged-read");
      return;
    }
    if (intent === "blocked-tool-final" && tools.length) {
      reply({ role: "assistant", content: tools.at(-1).content });
      return;
    }
    if (tools.length) {
      reply({ role: "assistant", content: tools.at(-1).content });
      return;
    }
    if (intent === "hold-after-read") {
      /* handled above only without completed tools below */
    }
    if (intent === "document") {
      call("read_document", { path: "proof.txt" });
      return;
    }
    if (intent === "outside") {
      call("read_file", { path: path.join(source, "proof.txt") });
      return;
    }
    if (intent === "dirty child") {
      const prompt = request.messages
        .filter((m) => m.role === "system")
        .map((m) => m.content)
        .join("\n");
      const target = prompt.match(
        /isolated Git checkout at (.+)\. Workspace file tools/,
      )[1];
      fs.writeFileSync(
        path.join(target, "ignored.log"),
        "Retained ignored worker output",
      );
    }

    call("read_file", { path: "proof.txt" });
  } catch (error) {
    modelErrors.push(error);
    if (!res.destroyed) {
      res.writeHead(500);
      res.end("{}");
    }
  }
});
const seen = new Set();
await new Promise((r) => model.listen(0, "127.0.0.1", r));
const config = {
  mcp_servers: [
    {
      id: "fixture",
      transport: "stdio",
      command: path.resolve(
        process.env.CARGO_TARGET_DIR || "target/migration-acceptance",
        "debug/examples/fixture",
      ),
    },
  ],
  providers: [
    {
      endpoint: `http://127.0.0.1:${model.address().port}/v1/chat/completions`,
      model: "fixture",
      protocol: "openai_compat",
    },
  ],
  workspace_worker: documentWorker,
  specialists: [
    { id: "worker", instructions: "Use the configured worker tools." },
    {
      id: "restricted",
      instructions: "No workspace reads",
      allowed_tools: ["agent_spawn", "agent_status"],
    },
  ],
};
fs.writeFileSync(path.join(root, "config.json"), JSON.stringify(config));
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
      cwd: source,
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
fs.writeFileSync(path.join(source, "proof.txt"), "Committed source proof");
git("add", "proof.txt");
git("commit", "-m", "fixture");
fs.writeFileSync(path.join(source, "proof.txt"), "PRIVATE DIRTY SOURCE");
fs.writeFileSync(path.join(source, ".git/info/exclude"), "ignored.log\n");
async function start(workspace = source) {
  logs = "";
  child = spawn(
    bin,
    [
      "--state",
      state,
      "--workspace",
      workspace,
      "--config",
      path.join(root, "config.json"),
      "--token-file",
      path.join(root, "token"),
      "--bind",
      "127.0.0.1:0",
    ],
    { env: { PATH: empty }, stdio: ["ignore", "pipe", "pipe"] },
  );
  child.stdout.on("data", (b) => (logs += b));
  child.stderr.on("data", (b) => (logs += b));
  await until(() => {
    if (child.exitCode !== null) throw Error(logs);
    const match = logs.match(/listening on (127\.0\.0\.1:\d+)/);
    if (match) {
      base = "http://" + match[1];
      return true;
    }
  });
}
async function stop(signal = "SIGTERM") {
  if (child && child.exitCode === null && child.signalCode === null) {
    const exited = new Promise((r) => child.once("exit", r));
    child.kill(signal);
    await exited;
  }
}
async function api(route, body, status = 200) {
  const response = await fetch(base + route, {
    method: body === undefined ? "GET" : "POST",
    headers: {
      Authorization: "Bearer " + token,
      "Content-Type": "application/json",
    },
    body: body === undefined ? undefined : JSON.stringify(body),
    signal: AbortSignal.timeout(25000),
  });
  const text = await response.text();
  assert.equal(response.status, status, text);
  return text ? JSON.parse(text) : null;
}
function spec(id, intent, extra = {}) {
  return {
    id,
    session: `chat-${id}`,
    turn: `turn-${id}`,
    request: id,
    parent: null,
    parent_run: null,
    agent: "main",
    objective: intent,
    depth: 0,
    input: {
      messages: [{ role: "user", content: intent }],
      max_model_calls: 6,
      timeout_ms: 60000,
      ...extra,
    },
  };
}
async function submit(run) {
  await api("/v1/permissions", {
    session: run.session,
    profile: "full",
    confirm_full: true,
  });
  return api("/v1/runs", run);
}
async function finished(id) {
  return until(async () => {
    const row = await api(`/v1/runs/${id}`);
    return ["succeeded", "failed", "cancelled", "interrupted"].includes(
      row.state,
    )
      ? row
      : null;
  });
}
try {
  await start();
  assert((await api("/v1/info")).capabilities.includes("workers.spawn"));
  assert.equal((await api("/v1/workers/catalog")).specialists.length, 2);
  const parent = await submit(spec("parent", "parent"));
  assert.equal(
    (await finished(parent.spec.id)).completion.result.message,
    "Parent and isolated worker finished",
  );
  const parentAll = await submit(spec("parent-all", "parent-wait-all"));
  assert.equal((await finished(parentAll.spec.id)).completion.result.message, "Parent and isolated worker finished");
  const spawned = JSON.parse(
    requests
      .findLast((r) =>
        r.messages.some((m) => m.role === "user" && m.content === "parent"),
      )
      .messages.find((m) => m.role === "tool").content,
  );
  const childWorkspace = await until(async () => {
    const v = await api(`/v1/runs/${spawned.spec.id}/workspace`);
    return v.cleanup?.state === "completed" ? v : null;
  });
  assert.equal(childWorkspace.retained, false);
  assert(!fs.existsSync(childWorkspace.root));
  foreignId = spawned.spec.id;
  const foreign = await submit(spec("foreign", "foreign"));
  assert(
    (await finished(foreign.spec.id)).completion.result.message.includes(
      "outside this parent scope",
    ),
  );
  const parallel = await submit(spec("parallel", "parallel"));
  const ids = JSON.parse(
    (await finished(parallel.spec.id)).completion.result.message,
  );
  for (const id of ids) assert.equal((await finished(id)).state, "succeeded");
  assert.notEqual(
    (await api(`/v1/runs/${ids[0]}/workspace`)).root,
    (await api(`/v1/runs/${ids[1]}/workspace`)).root,
  );
  const restrictedSpec = spec("restricted", "forged tool");
  restrictedSpec.agent = "restricted";
  await submit(restrictedSpec);
  assert.equal((await finished("restricted")).state, "failed");
  const isolated = { isolation: "worktree", keep_worktree: true };
  const direct = await submit(
    spec("direct", "isolated child", { worker_workspace: isolated }),
  );
  assert.equal(
    (await finished("direct")).completion.result.message,
    "Committed source proof",
  );
  const workspace = await api("/v1/runs/direct/workspace");
  assert(workspace.ready_for_input);
  assert(fs.existsSync(workspace.root));
  const doc = await submit(
    spec("doc", "document", { worker_workspace: isolated }),
  );
  assert(
    (await finished(doc.spec.id)).completion.result.message.includes(
      "Committed source proof",
    ),
  );
  const outside = await submit(
    spec("outside", "outside", { worker_workspace: isolated }),
  );
  const denied = await finished(outside.spec.id);
  assert(!JSON.stringify(denied).includes("PRIVATE DIRTY SOURCE"));
  const dirty = await submit(
    spec("dirty", "dirty child", {
      worker_workspace: { ...isolated, keep_worktree: false },
    }),
  );
  await finished(dirty.spec.id);
  const dirtyWorkspace = await api("/v1/runs/dirty/workspace");
  assert(dirtyWorkspace.retained);
  assert(fs.existsSync(path.join(dirtyWorkspace.root, "ignored.log")));
  const nested = await submit(
    spec("nested", "nested", { worker_workspace: isolated }),
  );
  const nestedDone = await finished(nested.spec.id);
  const nestedChild = JSON.parse(nestedDone.completion.result.message);
  await finished(nestedChild.spec.id);
  assert.equal(
    (await api(`/v1/runs/${nestedChild.spec.id}/workspace`)).mode,
    "worktree",
  );
  const blocked = await submit(
    spec("blocked", "blocked nesting", { worker_workspace: isolated }),
  );
  assert(
    (await finished(blocked.spec.id)).completion.result.message.includes(
      "cannot delegate into the shared",
    ),
  );
  const heldRun = await submit(
    spec("held", "hold worker", {
      worker_workspace: { ...isolated, keep_worktree: false },
    }),
  );
  await until(() => held.has("hold worker"));
  await api("/v1/runs/held/workspace/cleanup", { confirmed: true }, 409);
  await api("/v1/runs/held/cancel", {});
  assert.equal((await finished("held")).state, "cancelled");
  held.get("hold worker")();
  held.delete("hold worker");
  const heldWorkspace = await api("/v1/runs/held/workspace");
  assert(heldWorkspace.ready_for_input);
  await api("/v1/runs/held/workspace/cleanup", { confirmed: false }, 409);
  const resumed = await api("/v1/runs/held/resume", {
    command_id: "continue-one",
    confirmed: true,
  });
  assert.equal((await finished(resumed.spec.id)).state, "succeeded");
  const resumedWorkspace = await until(async () => {
    const w = await api(`/v1/runs/${resumed.spec.id}/workspace`);
    return w.cleanup?.state === "completed" ? w : null;
  });
  assert.equal(resumedWorkspace.root, heldWorkspace.root);
  assert.equal(
    (
      await api("/v1/runs/held/resume", {
        command_id: "continue-one",
        confirmed: true,
      })
    ).spec.id,
    resumed.spec.id,
  );
  await api(
    "/v1/runs/held/resume",
    { command_id: "different", confirmed: true },
    409,
  );
  await api(
    "/v1/runs",
    spec("forged-resume", "x", { resume_from: "held" }),
    403,
  );
  const confirmed = await submit(
    spec("confirmed", "continue-after-read", { worker_workspace: isolated }),
  );
  await until(() => held.has("continue-after-read"));
  await api("/v1/runs/confirmed/cancel", {});
  await finished("confirmed");
  held.get("continue-after-read")();
  held.delete("continue-after-read");
  const continueKnown = await api("/v1/runs/confirmed/resume", {
    command_id: "no-repeat",
    confirmed: true,
  });
  const repeated = await finished(continueKnown.spec.id);
  assert.equal(repeated.state, "failed");
  assert(repeated.completion.result.message.includes("must not be repeated"));
  const mutation = await submit(spec("mutation", "mutate-once"));
  await until(() => held.has("mutate-once"));
  await api("/v1/runs/mutation/cancel", {});
  await finished("mutation");
  held.get("mutate-once")();
  held.delete("mutate-once");
  const mutationResume = await api("/v1/runs/mutation/resume", {
    command_id: "never-repeat-mutation",
    confirmed: true,
  });
  const mutationStop = await finished(mutationResume.spec.id);
  assert.equal(mutationStop.state, "failed");
  assert(
    mutationStop.completion.result.message.includes(
      "already has a confirmed result",
    ),
  );
  const evidenceDb = new DatabaseSync(path.join(state, "runtime.sqlite"), {
    readOnly: true,
  });
  assert.equal(
    evidenceDb
      .prepare(
        "SELECT count(*) n FROM effects WHERE scope=? AND id='tool:fresh-mutation-id'",
      )
      .get(mutationResume.spec.turn).n,
    0,
  );
  evidenceDb.close();
  const unknown = await submit(spec("unknown", "uncertain"));
  assert.equal((await finished(unknown.spec.id)).state, "failed");
  const callsBefore = requests.length;
  await api(
    "/v1/runs/unknown/resume",
    { command_id: "unsafe", confirmed: true },
    409,
  );
  assert.equal(requests.length, callsBefore);
  const owner = await submit(spec("owner", "hold owner"));
  await until(() => held.has("hold owner"));
  const ownedSpec = spec("owned-child", "hold owned", {
    worker_workspace: isolated,
  });
  Object.assign(ownedSpec, {
    agent: "worker",
    depth: 1,
    parent: owner.spec.session,
    parent_run: owner.spec.id,
  });
  await api("/v1/runs", ownedSpec);
  await until(() => held.has("hold owned"));
  await api("/v1/runs/owned-child/cancel", {});
  await finished("owned-child");
  held.get("hold owned")();
  held.delete("hold owned");
  const ownedResume = await api("/v1/runs/owned-child/resume", {
    command_id: "continue-owned",
    confirmed: true,
  });
  assert.equal(ownedResume.spec.parent_run, owner.spec.id);
  assert.equal((await finished(ownedResume.spec.id)).state, "succeeded");
  await api("/v1/runs/owner/cancel", {});
  await finished("owner");
  held.get("hold owner")();
  held.delete("hold owner");
  const heldCrash = await submit(
    spec("crash", "hold crash", { worker_workspace: isolated }),
  );
  await until(() => held.has("hold crash"));
  await stop("SIGKILL");
  held.get("hold crash")();
  held.delete("hold crash");
  const before = requests.length;
  await start();
  assert.equal((await api("/v1/runs/crash")).state, "interrupted");
  await api("/v1/runs", heldCrash.spec);
  assert.equal(requests.length, before);
  assert((await api("/v1/runs/crash/workspace")).ready_for_input);
  assert.equal(
    (await api("/v1/runs/direct")).completion.result.message,
    "Committed source proof",
  );
  await stop();
  const marker = path.join(root, "git-finished"),
    pidFile = path.join(root, "git-pid"),
    wrapper = path.join(root, "git-wrapper");
  fs.writeFileSync(
    wrapper,
    `#!/bin/sh\n/usr/bin/git "$@"\ncode=$?\ncase " $* " in *" worktree add "*) if [ "$code" = 0 ]; then echo $$ > '${pidFile}'; : > '${marker}'; /bin/sleep 30; fi;; esac\nexit "$code"\n`,
    { mode: 0o700 },
  );
  config.git_binary = wrapper;
  fs.writeFileSync(path.join(root, "config.json"), JSON.stringify(config));
  await start();
  const gitCrash = await submit(
    spec("git-crash", "never-started", { worker_workspace: isolated }),
  );
  await until(() => fs.existsSync(marker));
  wrapperPid = Number(fs.readFileSync(pidFile, "utf8"));
  await stop("SIGKILL");
  try {
    process.kill(-wrapperPid, "SIGKILL");
  } catch {}
  wrapperPid = null;
  config.git_binary = "/usr/bin/git";
  fs.writeFileSync(path.join(root, "config.json"), JSON.stringify(config));
  const modelBefore = requests.length;
  await start();
  assert.equal((await api("/v1/runs/git-crash")).state, "interrupted");
  assert.equal(
    (await api("/v1/runs/git-crash/workspace")).operation.state,
    "dispatched",
  );
  assert.equal(
    (await api("/v1/runs/git-crash/workspace/reconcile", {})).operation.state,
    "completed",
  );
  assert.equal(requests.length, modelBefore);
  await api(
    "/v1/runs/git-crash/resume",
    { command_id: "no-checkpoint", confirmed: true },
    409,
  );
  const cleanup = await api("/v1/runs/git-crash/workspace/cleanup", {
    confirmed: true,
  });
  assert.equal(cleanup.cleanup.state, "completed");
  assert(!fs.existsSync(cleanup.root));
  const duplicate = spawn(
    bin,
    [
      "--state",
      state,
      "--workspace",
      source,
      "--config",
      path.join(root, "config.json"),
      "--token-file",
      path.join(root, "token"),
      "--bind",
      "127.0.0.1:0",
    ],
    { env: { PATH: empty }, stdio: ["ignore", "pipe", "pipe"] },
  );
  let duplicateLog = "";
  duplicate.stdout.on("data", (b) => (duplicateLog += b));
  duplicate.stderr.on("data", (b) => (duplicateLog += b));
  await until(() => duplicate.exitCode !== null);
  assert.notEqual(duplicate.exitCode, 0);
  assert(duplicateLog.includes("owns this state directory"));
  assert((await api("/v1/info")).capabilities.includes("workers.spawn"));
  await stop();
  const otherRoot = path.join(root, "other-workspace");
  fs.mkdirSync(otherRoot);
  await start(otherRoot);
  const beforeSwitch = requests.length;
  await api("/v1/runs", direct.spec, 409);
  assert.equal(requests.length, beforeSwitch);
  assert.equal(
    (await api("/v1/runs/direct")).completion.result.message,
    "Committed source proof",
  );
  console.log(
    JSON.stringify({
      passed: true,
      python_backend: false,
      webui_dependency: false,
      standalone_specialists: true,
      single_writer_lease: true,
      changed_workspace_fenced: true,
      active_cleanup_blocked: true,
      parallel_workers: true,
      foreign_worker_denied: true,
      tool_whitelist_enforced: true,
      confirmed_calls_not_replayed: true,
      fresh_id_mutation_replay_blocked: true,
      unknown_batch_resume_blocked: true,
      git_crash_before_model: true,
      observation_reconciliation: true,
      explicit_cleanup: true,
      isolated_workspace: true,
      source_edits_excluded: true,
      document_root_isolated: true,
      nested_isolation: true,
      shared_escape_blocked: true,
      dirty_output_retained: true,
      confirmed_continuation: true,
      continuation_ancestry_preserved: true,
      same_checkout: true,
      conflicting_resume_rejected: true,
      crash_no_replay: true,
      restart_preserved: true,
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
