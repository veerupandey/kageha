// Live remote approvals with a separate policy database. No Python/Node/curl backend PATH.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import http from "node:http";
import { spawn } from "node:child_process";
import { DatabaseSync } from "node:sqlite";
const binary =
  process.env.KAGEHA_RUNTIME_BIN ||
  path.resolve("target/debug/kageha-runtime-service");
const root = fs.mkdtempSync("/tmp/kageha-remote-approval-");
const token = "remote-policy-fixture-authentication-token";
const pause = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(fn) {
  for (let end = Date.now() + 15000; Date.now() < end; ) {
    const value = await fn();
    if (value) return value;
    await pause(20);
  }
  throw Error("Condition timed out");
}
let child,
  base,
  models = 0,
  toolReplies = 0;
const children = [],
  logs = [];
let releaseParent;
let loopDenials = false,
  repeatedCalls = 0;
let reviewVerdict = "allow",
  reviews = 0;
const model = http.createServer(async (req, res) => {
  let bytes = "";
  for await (const chunk of req) bytes += chunk;
  const body = JSON.parse(bytes);
  if (body.messages[0]?.content?.startsWith("You independently review")) {
    reviews++;
    const context = JSON.parse(body.messages[1].content);
    const request = context.request;
    res.end(
      JSON.stringify({
        choices: [
          {
            message: {
              role: "assistant",
              content:
                reviewVerdict === "invalid"
                  ? "invalid"
                  : JSON.stringify({
                      input_digest: request.authority.input_digest,
                      verdict: context.explicit_user_retry
                        ? "allow"
                        : reviewVerdict,
                      rationale: "Fixture review for exact request",
                    }),
            },
          },
        ],
      }),
    );
    return;
  }
  models++;
  if (body.messages.at(-1)?.content === "parent-hold") {
    await new Promise((resolve) => {
      releaseParent = resolve;
    });
    res.end(
      JSON.stringify({
        choices: [
          {
            message: { role: "assistant", content: "Parent fixture finished" },
          },
        ],
      }),
    );
    return;
  }
  let message;
  if (body.messages.at(-1).role === "tool" && !loopDenials) {
    toolReplies++;
    message = {
      role: "assistant",
      content: body.messages.at(-1).content.includes("approved fixture")
        ? "Read completed."
        : "Read denied.",
    };
  } else
    message = {
      role: "assistant",
      content: null,
      tool_calls: [
        {
          id: loopDenials ? `denied-${++repeatedCalls}` : "read-one",
          type: "function",
          function: {
            name: "read_file",
            arguments: JSON.stringify({ path: "notes.txt" }),
          },
        },
      ],
    };
  res.end(JSON.stringify({ choices: [{ message }] }));
});
await new Promise((r) => model.listen(0, "127.0.0.1", r));
async function start(requirements) {
  const log = path.join(root, `server-${logs.length}.log`);
  logs.push(log);
  child = spawn(
    binary,
    [
      "--state",
      path.join(root, "state"),
      "--workspace",
      path.join(root, "workspace"),
      "--config",
      path.join(root, "config.json"),
      "--token-file",
      path.join(root, "token"),
      "--bind",
      "127.0.0.1:0",
    ],
    {
      env: {
        PATH: path.join(root, "empty"),
        ...(requirements ? { KAGEHA_POLICY_REQUIREMENTS: requirements } : {}),
      },
      stdio: ["ignore", fs.openSync(log, "a"), fs.openSync(log, "a")],
    },
  );
  children.push(child);
  base = await until(() => {
    if (child.exitCode !== null) throw Error(fs.readFileSync(log, "utf8"));
    const m = fs.readFileSync(log, "utf8").match(/listening on (\S+)/);
    return m && "http://" + m[1];
  });
}
async function api(route, body, expected = 200) {
  const res = await fetch(base + route, {
    method: body ? "POST" : "GET",
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
    },
    body: body ? JSON.stringify(body) : undefined,
    signal: AbortSignal.timeout(10000),
  });
  const data = await res.json();
  assert.equal(res.status, expected, JSON.stringify(data));
  return data;
}
const spec = (id, timeout = 120000) => ({
  id,
  session: "chat",
  turn: "turn-" + id,
  request: id,
  agent: "main",
  parent: null,
  parent_run: null,
  depth: 0,
  objective: "Read notes",
  input: {
    messages: [{ role: "user", content: "Read notes" }],
    max_model_calls: 4,
    timeout_ms: timeout,
  },
});
const decide = (r, approved = true) => ({
  expected_revision: r.revision,
  authority: r.request.authority,
  approved,
  scope: "once",
});
async function pending(turn) {
  return until(
    async () => (await api("/v1/approvals?turn=" + turn)).approvals[0],
  );
}
async function done(id) {
  return until(async () => {
    const run = await api("/v1/runs/" + id);
    return (
      ["succeeded", "failed", "cancelled", "interrupted"].includes(run.state) &&
      run
    );
  });
}
async function stop(c, signal = "SIGKILL") {
  if (c && c.exitCode === null && c.signalCode === null) {
    c.kill(signal);
    await new Promise((r) => c.once("exit", r));
  }
}
try {
  fs.mkdirSync(path.join(root, "workspace"));
  fs.mkdirSync(path.join(root, "empty"));
  fs.writeFileSync(path.join(root, "workspace/notes.txt"), "approved fixture");
  fs.writeFileSync(path.join(root, "token"), token, { mode: 0o600 });
  fs.writeFileSync(
    path.join(root, "config.json"),
    JSON.stringify({
      providers: [
        {
          endpoint: `http://127.0.0.1:${model.address().port}/model`,
          model: "fixture",
          protocol: "openai_compat",
        },
      ],
      approval_tools: ["read_file"],
    }),
  );
  await start();
  assert.equal((await api("/v1/info")).remote_approvals, true);
  assert.equal((await fetch(base + "/v1/approvals")).status, 401);
  await api("/v1/runs", spec("accept"));
  let r = await pending("turn-accept");
  assert(r.request.detail.includes("notes.txt"));
  assert.equal(toolReplies, 0, "No input before consent");
  const dispatches = new DatabaseSync(path.join(root, "state/runtime.sqlite"));
  assert.equal(
    dispatches
      .prepare("SELECT count(*) AS n FROM effects WHERE id='tool:read-one'")
      .get().n,
    0,
  );
  dispatches.close();
  const bad = decide(r);
  bad.authority = { ...bad.authority, generation: 2 };
  await api(`/v1/approvals/${r.id}/decision`, bad, 409);
  await api(
    `/v1/approvals/${r.id}/decision`,
    { ...decide(r), expected_revision: 99 },
    409,
  );
  assert.equal(toolReplies, 0);
  await api(`/v1/approvals/${r.id}/decision`, decide(r));
  assert.equal(
    (await done("accept")).completion.result.message,
    "Read completed.",
  );
  assert.equal((await api(`/v1/approvals/${r.id}`)).state, "consumed");
  const before = toolReplies;
  await api(`/v1/approvals/${r.id}/decision`, decide(r));
  assert.equal(toolReplies, before);
  await api(`/v1/approvals/${r.id}/decision`, decide(r, false), 409);
  await api("/v1/runs", spec("deny"));
  r = await pending("turn-deny");
  await api(`/v1/approvals/${r.id}/decision`, decide(r, false));
  assert.equal((await done("deny")).completion.result.message, "Read denied.");
  assert.equal((await api(`/v1/approvals/${r.id}`)).state, "denied");
  await api("/v1/runs", spec("cancel"));
  r = await pending("turn-cancel");
  await api("/v1/runs/cancel/cancel", {});
  assert.equal((await done("cancel")).state, "cancelled");
  await api(`/v1/approvals/${r.id}/decision`, decide(r), 409);
  assert.equal((await api(`/v1/approvals/${r.id}`)).state, "cancelled");
  await api("/v1/runs", spec("timeout", 1000));
  r = await pending("turn-timeout");
  assert.equal((await done("timeout")).state, "failed");
  await api(`/v1/approvals/${r.id}/decision`, decide(r), 409);
  await api("/v1/runs", spec("crash"));
  r = await pending("turn-crash");
  const calls = models;
  await stop(child);
  await start();
  assert.equal((await done("crash")).state, "interrupted");
  assert.equal(models, calls);
  assert.equal((await api(`/v1/approvals/${r.id}`)).state, "interrupted");
  await api(`/v1/approvals/${r.id}/decision`, decide(r), 409);
  assert.equal((await api("/v1/approvals")).approvals.length, 0);

  await api("/v1/permissions", { session: "chat", profile: "full" }, 400);
  await api("/v1/permissions", { session: "chat", profile: "auto_review" });
  await api("/v1/runs", spec("review-allow"));
  assert.equal(
    (await done("review-allow")).completion.result.message,
    "Read completed.",
  );
  assert.equal(reviews, 1);
  assert.equal(
    (await api("/v1/approvals?turn=turn-review-allow&pending=false"))
      .approvals[0].state,
    "consumed",
  );
  reviewVerdict = "deny";

  await api("/v1/runs", spec("review-deny"));
  await done("review-deny");
  assert.equal(
    (await api("/v1/approvals?turn=turn-review-deny&pending=false"))
      .approvals[0].state,
    "denied",
  );
  // A tool error may be returned to the model, but no successful file read occurred.
  assert.equal(
    (await api("/v1/approvals?turn=turn-review-deny")).approvals.length,
    0,
  );
  reviewVerdict = "invalid";
  await api("/v1/runs", spec("review-invalid"));
  let uncertain = await pending("turn-review-invalid");
  await api("/v1/permissions", { session: "chat", profile: "ask" });
  await done("review-invalid");
  await api(`/v1/approvals/${uncertain.id}/decision`, decide(uncertain), 409);
  const countReviews = reviews;
  await api("/v1/permissions", {
    session: "chat",
    profile: "full",
    confirm_full: true,
  });
  await api("/v1/runs", spec("full-access"));
  assert.equal(
    (await done("full-access")).completion.result.message,
    "Read completed.",
  );
  assert.equal(reviews, countReviews);
  assert.equal(
    (await api("/v1/permissions?session=other-chat")).profile,
    "ask",
  );
  await stop(child);
  await start();
  assert.equal((await api("/v1/permissions?session=chat")).profile, "full");

  const parent = spec("parent-active");
  parent.objective = "Keep the parent active while its worker runs";
  parent.input.messages = [{ role: "user", content: "parent-hold" }];
  await api("/v1/runs", parent);
  await until(() => releaseParent);
  const childSpec = (id) => ({
    ...spec(id),
    session: "worker-chat",
    parent: "chat",
    parent_run: "parent-active",
    depth: 1,
  });
  await api("/v1/runs", childSpec("worker-full"));
  assert.equal(
    (await done("worker-full")).completion.result.message,
    "Read completed.",
  );
  assert.equal(
    (await api("/v1/permissions?session=worker-chat")).owner_session,
    "chat",
  );
  await api(
    "/v1/permissions",
    { session: "worker-chat", profile: "full", confirm_full: true },
    409,
  );
  await api("/v1/permissions", { session: "chat", profile: "ask" });
  await api("/v1/runs", childSpec("worker-held"));
  const held = await pending("turn-worker-held");
  await api("/v1/permissions", {
    session: "chat",
    profile: "full",
    confirm_full: true,
  });
  await done("worker-held");
  assert.equal((await api(`/v1/approvals/${held.id}`)).state, "cancelled");
  releaseParent();
  await done("parent-active");
  await api("/v1/permissions", { session: "chat", profile: "auto_review" });
  reviewVerdict = "deny";
  loopDenials = true;
  const reviewCount = reviews;
  await api("/v1/runs", spec("denial-limit"));
  const stopped = await done("denial-limit");
  assert.equal(stopped.state, "failed");
  assert.match(stopped.completion.result.message, /denial limit/);
  assert.equal(reviews - reviewCount, 3);
  const denied = (await api("/v1/review-denials?session=chat")).denials.find(
    (d) => d.record.request.authority.turn === "turn-denial-limit",
  ).record;
  await api(
    "/v1/review-overrides",
    {
      approval_id: denied.id,
      authority: { ...denied.request.authority, generation: 999 },
      expected_revision: denied.revision,
    },
    409,
  );
  const retry = await api("/v1/review-overrides", {
    approval_id: denied.id,
    authority: denied.request.authority,
    expected_revision: denied.revision,
  });
  assert.equal(retry.input_dispatched, false);
  loopDenials = false;
  await api("/v1/runs", spec("reviewed-retry"));
  assert.equal(
    (await done("reviewed-retry")).completion.result.message,
    "Read completed.",
  );
  await stop(child);
  const managed = path.join(root, "requirements.json");
  fs.writeFileSync(
    managed,
    JSON.stringify({
      allowed_profiles: ["ask"],
      forced_profile: "ask",
      denied_actions: ["read_file"],
    }),
  );
  await start(managed);
  assert.equal((await api("/v1/permissions?session=chat")).profile, "ask");
  await api(
    "/v1/permissions",
    { session: "chat", profile: "full", confirm_full: true },
    403,
  );
  await api("/v1/runs", spec("managed-deny"));
  const managedRun = await done("managed-deny");
  assert.equal(managedRun.state, "failed");
  assert.match(managedRun.completion.result.message, /Managed policy denies/);
  const journal = new DatabaseSync(path.join(root, "state/policy.sqlite"));
  const tables = journal
    .prepare("SELECT name FROM sqlite_master WHERE type='table'")
    .all()
    .map((r) => r.name);
  assert(tables.includes("approvals"));
  assert(!tables.includes("ui_approvals"));
  assert(
    journal.prepare("SELECT count(*) AS n FROM policy_events").get().n > 5,
  );
  journal.close();
  console.log(
    JSON.stringify({
      passed: true,
      python_backend: false,
      policy_database: "component-owned",
      checks: [
        "exact remote approval",
        "denial",
        "one-time consumption",
        "stale authority/revision",
        "cancel and deadline",
        "restart interruption",
        "no tool replay",
        "separate audit journal",
        "chat-scoped profiles and full confirmation",
        "independent model review allow/deny/invalid",
        "profile revocation and restart",
        "three denials stop the Rust loop",
        "exact one-use user retry reviewed again",
        "managed restrictions survive restart and block full access",
        "child permission inheritance and parent revocation",
      ],
      fixture: root,
    }),
  );
} catch (error) {
  console.error(error);
  for (const log of logs) console.error(fs.readFileSync(log, "utf8"));
  process.exitCode = 1;
} finally {
  for (const c of children) await stop(c);
  model.closeAllConnections();
  await new Promise((r) => model.close(r));
}
