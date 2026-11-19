import {
  credentialFixture,
  credentialServiceFixture,
  switchCredentialFixture,
} from "./credential_fixture.mjs";
// Full native settings, planning, specialist and persistence contract test.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import http from "node:http";
import {DatabaseSync} from "node:sqlite";
import { spawn, execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
const crate = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const binary =
  process.env.KAGEHA_LOOP_BIN ||
  path.resolve(crate, "../../target/release/kageha-loop");
const root = fs.mkdtempSync("/tmp/kageha-runtime-");
const token = "native-runtime-fixture-authentication-token";
const records = [];
const children = [];
const timers = new Set();
let base, child;
const pause = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(check) {
  const end = Date.now() + 15000;
  while (Date.now() < end) {
    const value = await check();
    if (value) return value;
    await pause(50);
  }
  throw Error("Condition did not arrive");
}
let releaseFanoutSlow, fanoutJobs, fanoutAllJobs, fanoutAllPartial = false;
const teamParallelReleases = [];
const teamCancellationReleases = [];
const teamSequenceCalls = [];
const teamRevisionReleases = [];
const fixture = http.createServer(async (req, res) => {
  if (req.url === "/mcp") {
    if (req.method === "DELETE") {
      res.end("{}");
      return;
    }
    let bytes = "";
    for await (const part of req) bytes += part;
    const rpc = JSON.parse(bytes);
    if (rpc.method === "notifications/initialized") {
      res.writeHead(202);
      res.end();
      return;
    }
    const result =
      rpc.method === "initialize"
        ? { protocolVersion: "2024-11-05", capabilities: { tools: {} } }
        : rpc.method === "tools/list"
          ? {
              tools: [
                {
                  name: "echo",
                  description: "HTTP fixture tool",
                  inputSchema: {
                    type: "object",
                    properties: { text: { type: "string" } },
                  },
                },
              ],
            }
          : { content: [{ type: "text", text: "native-mcp-reply" }] };
    res.setHeader("Content-Type", "application/json");
    if (rpc.method === "initialize")
      res.setHeader("Mcp-Session-Id", "native-fixture-session");
    else assert.equal(req.headers["mcp-session-id"], "native-fixture-session");
    res.end(JSON.stringify({ jsonrpc: "2.0", id: rpc.id, result }));
    return;
  }
  if (req.method === "GET") {
    res.setHeader("Content-Type", "application/json");
    res.end(JSON.stringify({ data: [{ id: "fixture-specialist" }] }));
    return;
  }
  let body = "";
  for await (const part of req) body += part;
  const request = JSON.parse(body);
  records.push(request);
  const messages = request.messages;
  const last = messages.at(-1);
  const intent =
    messages.filter((v) => v.role === "user").at(-1)?.content || "";
  const system = messages
    .filter((v) => v.role === "system")
    .map((v) => v.content)
    .join("\n");
  const reply = (content, tool) => {
    if (res.destroyed) return;
    res.setHeader("Content-Type", "application/json");
    res.end(
      JSON.stringify({
        choices: [
          {
            message: tool
              ? {
                  role: "assistant",
                  content: null,
                  tool_calls: [
                    {
                      id: tool.id,
                      type: "function",
                      function: {
                        name: tool.name,
                        arguments: JSON.stringify(tool.args),
                      },
                    },
                  ],
                }
              : { role: "assistant", content },
          },
        ],
      }),
    );
  };
  if (typeof intent === "string" && intent.startsWith("Investigate the user's task as a member of a parallel team.") && intent.includes("team-parallel-fixture")) {
    // Neither response is released until both requests arrive, proving overlap.
    teamParallelReleases.push(() => reply("Parallel worker result"));
    return;
  }
  if (typeof intent === "string" && intent.startsWith("Investigate the user's task as a member of a parallel team.") && intent.includes("team-cancel-fixture")) {
    teamCancellationReleases.push(() => reply("Late result"));
    return;
  }
  if (typeof intent === "string" && intent.startsWith("Investigate the user's task as a member of a sequence team.") && intent.includes("team-sequence-fixture")) {
    teamSequenceCalls.push(intent);
    reply(teamSequenceCalls.length === 1 ? "First worker result" : "Second worker result");
    return;
  }
  if (typeof intent === "string" && intent.startsWith("Investigate the user's task as a member of a sequence team.") && intent.includes("team-revision-fixture")) {
    teamRevisionReleases.push(() => reply("First member result"));
    return;
  }
  if (intent === "fanout slow child") {
    releaseFanoutSlow = () => reply("Slow child done");
    return;
  }
  if (intent === "coordinate fixture") {
    const results = messages.filter(m => m.role === "tool");
    if (!results.length) {
      reply(null, {id:"team-coordinate-spawn", name:"agent_spawn", args:{agent_id:"research", objective:"coordinate child"}});
    } else if (results.length === 1) {
      reply(null, {id:"team-coordinate-wait", name:"agent_wait_any", args:{job_ids:[JSON.parse(results[0].content).id], timeout_ms:30000}});
    } else reply("Coordinator integrated the specialist result.");
    return;
  }
  if (intent === "fanout fast child") {
    reply("Fast child done");
    return;
  }
  if (intent === "fanout all parent") {
    const results = messages.filter(m => m.role === "tool");
    if (!results.length) {
      res.end(JSON.stringify({choices:[{message:{role:"assistant",content:null,tool_calls:["slow","fast"].map(id => ({
        id:`all-spawn-${id}`, type:"function", function:{name:"agent_spawn",arguments:JSON.stringify({agent_id:"main", objective:`fanout ${id} child`})}
      }))}}]}));
      return;
    }
    if (results.length === 2) {
      fanoutAllJobs = results.map(result => JSON.parse(result.content).id);
      reply(null, {id:"wait-all-timeout", name:"agent_wait_all", args:{job_ids:fanoutAllJobs, timeout_ms:0}});
      return;
    }
    if (results.length === 3) {
      const partial = JSON.parse(results.at(-1).content);
      assert.equal(partial.timed_out, true);
      assert(partial.pending_job_ids.includes(fanoutAllJobs[0]));
      fanoutAllPartial = true;
      reply(null, {id:"wait-all", name:"agent_wait_all", args:{job_ids:fanoutAllJobs, timeout_ms:30000}});
      return;
    }
    const done = JSON.parse(results.at(-1).content);
    assert.equal(done.timed_out, false);
    assert.deepEqual(done.pending_job_ids, []);
    assert.equal(done.jobs.length, 2);
    reply("All children collected.");
    return;
  }
  if (intent === "fanout parent") {
    const results = messages.filter((m) => m.role === "tool");
    if (!results.length) {
      res.end(
        JSON.stringify({
          choices: [
            {
              message: {
                role: "assistant",
                content: null,
                tool_calls: [
                  {
                    id: "spawn-slow",
                    type: "function",
                    function: {
                      name: "agent_spawn",
                      arguments: JSON.stringify({
                        agent_id: "main",
                        objective: "fanout slow child",
                      }),
                    },
                  },
                  {
                    id: "spawn-fast",
                    type: "function",
                    function: {
                      name: "agent_spawn",
                      arguments: JSON.stringify({
                        agent_id: "main",
                        objective: "fanout fast child",
                      }),
                    },
                  },
                  {
                    id: "read-parallel",
                    type: "function",
                    function: {
                      name: "read_file",
                      arguments: JSON.stringify({ path: "notes.txt" }),
                    },
                  },
                ],
              },
            },
          ],
        }),
      );
      return;
    }
    if (results.length === 3) {
      assert.deepEqual(
        results.map((r) => r.tool_call_id),
        ["spawn-slow", "spawn-fast", "read-parallel"],
      );
      fanoutJobs = results.slice(0, 2).map((r) => JSON.parse(r.content).id);
      reply(null, {
        id: "wait-first",
        name: "agent_wait_any",
        args: { job_ids: fanoutJobs, timeout_ms: 3000 },
      });
      return;
    }
    const done = JSON.parse(results.at(-1).content);
    assert.equal(done.timed_out, false);
    assert.equal(done.jobs.length, 1);
    assert.equal(done.jobs[0].id, fanoutJobs[1]);
    reply("First child consumed without waiting for slow sibling.");
    return;
  }
  if (intent.includes("native mcp")) {
    if (last.role === "tool") {
      reply(last.content);
      return;
    }
    // MCP tools are reached through mcp_call; the tool list never names them.
    assert(
      request.tools.some((t) => t.function.name === "mcp_call"),
      "mcp_call is listed",
    );
    assert(
      !request.tools.some(
        (t) =>
          t.function.name.startsWith("mcp_") &&
          !["mcp_call", "mcp_receipts"].includes(t.function.name),
      ),
      "individual MCP tools are not listed",
    );
    reply(null, {
      id: "mcp-call",
      name: "mcp_call",
      args: {
        server: intent.includes("stdio") ? "native-stdio" : "native-http",
        tool: "echo",
        arguments: { text: "fixture input" },
      },
    });
    return;
  }
  if (intent.includes("slow worker")) {
    const timer = setTimeout(() => reply("Slow worker finished"), 10000);
    timers.add(timer);
    return;
  }
  if (system.includes("Draft specialist instructions")) {
    reply(
      JSON.stringify({
        description: "Draft description",
        system_prompt: "Draft specialist instructions for review.",
      }),
    );
    return;
  }
  if (system.includes("Choose an execution model")) {
    reply(
      JSON.stringify({
        model_id: "local/fixture-specialist",
        confidence: 0.99,
      }),
    );
    return;
  }
  if (system.includes("Plan this task without executing")) {
    reply(
      JSON.stringify({
        objective: intent,
        outcomes: [
          { id: "result", text: "Report verified findings", required: true },
        ],
        steps: [
          {
            id: "inspect",
            title: "Read notes and report findings",
            depends_on: [],
            acceptance_criteria: ["Use the notes as evidence"],
          },
        ],
      }),
    );
    return;
  }
  if (intent.includes("read specialist knowledge")) {
    if (last.role === "tool") {
      reply(last.content);
      return;
    }
    const content = system.match(/knowledge\/inputs\/[^"\s]+/);
    assert(content);
    reply(null, {
      id: "read-kb",
      name: "read_file",
      args: { path: content[0] },
    });
    return;
  }
  if (system.includes("Execute this approved plan")) {
    const results = messages.filter((v) => v.role === "tool");
    if (intent.includes("unverified plan")) {
      reply("I claim the task is complete without any tool evidence.");
      return;
    }
    if (!results.length) {
      reply(null, {
        id: "plan-read",
        name: "read_file",
        args: { path: "notes.txt" },
      });
      return;
    }
    if (results.length === 1) {
      reply(null, {
        id: "plan-update",
        name: "task_plan_update",
        args: {
          step_id: "inspect",
          status: "completed",
          evidence: ["plan-read"],
          outcome_ids: ["result"],
        },
      });
      return;
    }
    reply("Plan completed with notes as evidence.");
    return;
  }
  if (intent.includes("restricted tool")) {
    if (last.role === "tool") {
      reply(last.content);
      return;
    }
    reply(null, {
      id: "forbidden",
      name: "agent_spawn",
      args: { agent_id: "main", objective: "Should not execute" },
    });
    return;
  }
  if (intent.includes("message worker")) {
    if (!messages.some((v) => v.role === "tool")) {
      const timer = setTimeout(
        () =>
          reply(null, {
            id: "worker-read",
            name: "read_file",
            args: { path: "notes.txt" },
          }),
        300,
      );
      timers.add(timer);
      return;
    }
    reply(
      messages.some((v) => v.content === "Use the revised criteria")
        ? "Steering acknowledged."
        : "Missing steering",
    );
    return;
  }
  if (intent.includes("Use the revised criteria")) {
    reply("Steering acknowledged.");
    return;
  }
  reply(
    system.includes("SPECIALIST_MARKER")
      ? "Specialist answer."
      : intent.includes("What did I say")
        ? "Earlier conversation preserved."
        : "Native answer.",
  );
});
await new Promise((r) => fixture.listen(0, "127.0.0.1", r));
const provider = `http://127.0.0.1:${fixture.address().port}/v1`;
const credentialService = await credentialServiceFixture(root);
if (credentialService.child) children.push(credentialService.child);
async function start() {
  const log = path.join(root, `server-${children.length}.log`);
  child = spawn(
    binary,
    [
      "serve",
      path.join(root, "workspace"),
      path.join(root, "state"),
      "127.0.0.1:0",
    ],
    {
      env: {
        PATH: path.join(root, "bin"),
        ...(process.env.KAGEHA_RUNTIME_SERVICE_URL
          ? {
              KAGEHA_RUNTIME_SERVICE_URL:
                process.env.KAGEHA_RUNTIME_SERVICE_URL,
              KAGEHA_RUNTIME_SERVICE_TOKEN_FILE:
                process.env.KAGEHA_RUNTIME_SERVICE_TOKEN_FILE,
            }
          : {}),
        ...credentialFixture(root),
        ...credentialService.env,
        KAGEHA_SERVER_TOKEN: token,
        KAGEHA_PROVIDER_URL: provider + "/chat/completions",
        KAGEHA_MODEL: "fixture",
        KAGEHA_LEGACY_SESSIONS_DIR: path.join(root, "legacy"),
        ...(process.env.KAGEHA_WEBUI_DIST
          ? { KAGEHA_WEBUI_DIST: process.env.KAGEHA_WEBUI_DIST }
          : {}),
      },
      stdio: ["ignore", fs.openSync(log, "a"), fs.openSync(log, "a")],
    },
  );
  children.push(child);
  base = await until(() => {
    const match = fs.readFileSync(log, "utf8").match(/listening on ([^\s]+)/);
    if (child.exitCode !== null) throw Error(fs.readFileSync(log, "utf8"));
    return match && "http://" + match[1];
  });
}
async function api(url, body, method) {
  const res = await fetch(base + url, {
    method: method || (body ? "POST" : "GET"),
    headers: {
      Authorization: `Bearer ${token}`,
      ...(body ? { "Content-Type": "application/json" } : {}),
    },
    body: body ? JSON.stringify(body) : undefined,
  });
  return { status: res.status, data: await res.json() };
}
async function ok(url, body, method) {
  const result = await api(url, body, method);
  assert.equal(result.status, 200, JSON.stringify(result));
  return result.data;
}
async function turn(chat, id, message, extra = {}) {
  return ok("/api/chat/send", {
    session_id: chat,
    client_request_id: id,
    message,
    ...extra,
  });
}
async function jobDone(id) {
  return until(async () => {
    const job = (await ok("/api/jobs/" + id)).job;
    return (
      ["succeeded", "failed", "cancelled", "interrupted"].includes(job.state) &&
      job
    );
  });
}
try {
  fs.mkdirSync(path.join(root, "workspace"));
  fs.writeFileSync(
    path.join(root, "workspace/notes.txt"),
    "Durable project notes.",
  );
  fs.mkdirSync(path.join(root, "bin"));
  fs.symlinkSync(
    execFileSync("/usr/bin/which", ["curl"], { encoding: "utf8" }).trim(),
    path.join(root, "bin/curl"),
  );
  fs.mkdirSync(path.join(root, "legacy/old-chat/inputs"), { recursive: true });
  fs.writeFileSync(
    path.join(root, "legacy/old-chat/inputs/old.txt"),
    "Original attached evidence",
  );
  fs.writeFileSync(
    path.join(root, "legacy/old-chat/chat.jsonl"),
    JSON.stringify({
      role: "user",
      text: "Original chat message",
      turn_id: "legacy-one",
      attachments: ["inputs/old.txt"],
    }) +
      "\n" +
      JSON.stringify({
        role: "assistant",
        text: "Original response",
        speaker: { id: "older-agent", name: "Older specialist" },
      }) +
      "\n",
  );
  fs.writeFileSync(
    path.join(root, "legacy/old-chat/meta.json"),
    JSON.stringify({ title: "Old chat title" }),
  );
  await start();
  const legacy = await ok("/api/sessions/old-chat");
  assert.equal(legacy.title, "Old chat title");
  assert.equal(legacy.messages[1].speaker.name, "Older specialist");
  const attached = await fetch(
    base + "/api/sessions/old-chat/files/inputs/old.txt",
    { headers: { Authorization: `Bearer ${token}` } },
  );
  assert.equal(await attached.text(), "Original attached evidence");
  const drafted = await ok("/api/agents/draft", {
    name: "Draft researcher",
    description: "Research notes",
  });
  assert(drafted.draft.system_prompt.includes("review"));
  const chat = (await ok("/api/sessions", {})).session_id;
  const requestId = "unavailable-model-receipt";
  const rejected = await api("/api/chat/stream", {
    session_id: chat, client_request_id: requestId,
    model: "nonexistent-fixture-model", message: "Do not start this turn",
  });
  assert.equal(rejected.status, 400);
  const receipt = await ok(`/api/sessions/${chat}/requests/${requestId}`);
  assert.equal(receipt.receipt.state, "rejected");
  assert.equal(receipt.receipt.turn_id, "");
  const otherChat = (await ok("/api/sessions", {})).session_id;
  assert.equal((await api(`/api/sessions/${otherChat}/requests/${requestId}`)).status, 404);
  await turn(chat, "first", "Remember this conversation");
  const agent = {
    id: "research",
    name: "Research specialist",
    system_prompt: "SPECIALIST_MARKER. Read carefully.",
    tool_policy: {
      allow: ["read_file", "task_plan_update"],
      deny: ["agent_spawn"],
    },
  };
  assert.equal((await ok("/api/agents", agent)).agent.id, "research");
  assert.equal((await api("/api/agents", agent)).status, 409);
  await ok(
    "/api/agents/research",
    { ...agent, description: "Updated definition" },
    "PUT",
  );
  assert.equal(
    (await ok("/api/agents/research")).persona.description,
    "Updated definition",
  );
  assert.equal((await ok("/api/agents/research/history")).history.length, 2);
  await ok("/api/agents/research/rollback", { snapshot_id: "1" });
  assert.equal(
    (await ok("/api/agents/research")).persona.description,
    undefined,
  );
  await ok(
    "/api/agents/research",
    { description: "Updated definition" },
    "PUT",
  );
  const knowledge = new FormData();
  knowledge.append(
    "file",
    new Blob(["Specialist reference document."]),
    "reference.txt",
  );
  const ingestion = await fetch(
    base + "/api/agents/research/knowledge/upload",
    {
      method: "POST",
      headers: { Authorization: `Bearer ${token}` },
      body: knowledge,
    },
  );
  assert.equal(ingestion.status, 200, await ingestion.text());
  assert(
    (await ok("/api/agents/research/knowledge")).sources.includes(
      "reference.txt",
    ),
  );
  await ok("/api/teams", {
    id: "reviewers",
    name: "Review team",
    agents: ["main", "research"],
    mode: "parallel",
  });
  assert.equal((await ok("/api/teams")).teams[0].agents.length, 2);
  const parallelChat = (await ok("/api/sessions", {})).session_id;
  const parallelTurn = turn(parallelChat, "team-parallel", "&reviewers team-parallel-fixture");
  await until(() => teamParallelReleases.length === 2);
  teamParallelReleases.forEach(release => release());
  await parallelTurn;
  const parallelWorkerRequests = records.filter(r => r.messages.some(m => typeof m.content === "string" && m.content.startsWith("Investigate the user's task as a member of a parallel team.") && m.content.includes("team-parallel-fixture")));
  assert.equal(parallelWorkerRequests.length, 2);
  for (const req of parallelWorkerRequests) {
    assert(!req.tools.some(tool => ["shell", "write_file", "agent_spawn", "browser_click", "mcp_call"].includes(tool.function.name)), "team workers must not receive mutating tools");
  }
  const parallelCount = teamParallelReleases.length;
  await turn(parallelChat, "team-parallel", "&reviewers team-parallel-fixture");
  assert.equal(teamParallelReleases.length, parallelCount, "retry must not redispatch team members");
  assert.equal((await api("/api/chat/send", {session_id: parallelChat, client_request_id: "late-team", message: "&reviewers retry"})).status, 409);
  const cancelChat = (await ok("/api/sessions", {})).session_id;
  const cancelledTurn = api("/api/chat/send", {session_id: cancelChat, client_request_id: "team-cancel", message: "team-cancel-fixture", team_id: "reviewers"});
  await until(() => teamCancellationReleases.length === 2);
  const teamJobs = await until(async () => { const rows = (await ok(`/api/jobs?session_id=${cancelChat}`)).jobs; return rows.length === 2 && rows; });
  await ok(`/api/jobs/${teamJobs[1].id}/cancel`, {});
  await until(async () => ["cancelled", "cancelling"].includes((await ok(`/api/jobs/${teamJobs[0].id}`)).job.state));
  teamCancellationReleases.forEach(release => release());
  await cancelledTurn;
  assert(!records.some(r => r.messages.some(m => m.role === "user" && m.content === "team-cancel-fixture")), "failed team must not be synthesized as success");
  await ok("/api/teams", {id:"pipeline", name:"Pipeline", agents:["main","research"], mode:"sequence"});
  const sequenceChat = (await ok("/api/sessions", {})).session_id;
  await turn(sequenceChat, "team-sequence", "team-sequence-fixture", {team_id:"pipeline"});
  assert.equal(teamSequenceCalls.length, 2);
  assert(!teamSequenceCalls[0].includes("Previous team member's result"));
  assert(teamSequenceCalls[1].includes("First worker result"));
  await ok("/api/teams", {id:"coordinated", name:"Coordinated", agents:["main","research"], mode:"coordinate"});
  const coordinateChat = (await ok("/api/sessions", {})).session_id;
  const coordinatedAnswer = await turn(coordinateChat, "team-coordinate", "coordinate fixture", {team_id:"coordinated"});
  assert(coordinatedAnswer.message.includes("Coordinator integrated"));
  assert((await ok(`/api/jobs?session_id=${coordinateChat}`)).jobs.some(job => job.spec.persona_id === "research" && job.state === "succeeded"));
  const coordinateRequest = records.findLast(r => r.messages.some(m => m.role === "user" && m.content === "coordinate fixture"));
  assert(coordinateRequest.messages.some(m => m.role === "system" && m.content.includes("team's coordinator")));
  assert(coordinateRequest.messages.some(m => m.role === "assistant" && m.content.includes('"research"')));
  await ok("/api/teams", {id:"research-led", name:"Research led", agents:["research","main"], mode:"coordinate"});
  const researchLedChat = (await ok("/api/sessions", {})).session_id;
  await turn(researchLedChat, "team-led", "&research-led initial request");
  assert.equal((await ok(`/api/sessions/${researchLedChat}`)).persona_id, "research");
  assert.equal((await turn(researchLedChat, "team-followup", "follow up without a team" )).speaker.id, "research");
  const conflictChat = (await ok("/api/sessions", {})).session_id;
  assert.equal((await api("/api/chat/send", {session_id: conflictChat, client_request_id:"team-conflict", message:"&reviewers conflict", team_id:"pipeline"})).status, 400);
  assert.equal((await api("/api/chat/send", {session_id: conflictChat, client_request_id:"team-plan", message:"plan team task", team_id:"pipeline", agent_mode:"plan"})).status, 400);
  await ok("/api/agents", {id:"team-revision-member", name:"Revision member", system_prompt:"Revision one"});
  await ok("/api/teams", {id:"revision-pipeline", name:"Revision pipeline", agents:["main", "team-revision-member"], mode:"sequence"});
  const revisionChat = (await ok("/api/sessions", {})).session_id;
  const revisionTurn = api("/api/chat/send", {session_id:revisionChat, client_request_id:"team-revision", message:"team-revision-fixture", team_id:"revision-pipeline"});
  await until(() => teamRevisionReleases.length === 1);
  await ok("/api/agents/team-revision-member", {system_prompt:"Revision two"}, "PUT");
  teamRevisionReleases[0]();
  await revisionTurn;
  assert.equal(teamRevisionReleases.length, 1, "changed member profile must not be dispatched");
  assert(!records.some(r => r.messages.some(m => m.role === "user" && m.content === "team-revision-fixture")), "profile mismatch must not be synthesized as success");
  {const source=new DatabaseSync(path.join(root,"state/native-chats.sqlite"),{readOnly:true});
   try {assert.equal(source.prepare("SELECT COUNT(*) n FROM ui_documents WHERE kind='agent' AND id='research'").get().n,0);}finally{source.close();}
   const owned=new DatabaseSync(path.join(root,"state/components/agents/profiles.sqlite"),{readOnly:true});
   try {const entry=JSON.parse(owned.prepare("SELECT body FROM profile_entries WHERE kind='agent' AND id='research'").get().body);assert.equal(entry.revisions.at(-1).value.description,"Updated definition");}finally{owned.close();}}

  if (process.env.KAGEHA_TEST_PROFILE_REPOSITORY === "1") {
    const cli=path.resolve(process.env.CARGO_TARGET_DIR || "target","debug/kageha-agent-profiles");
    const args=[root+"/portable-profiles.sqlite","exported-webui-scope"];
    const repo=(...a)=>JSON.parse(execFileSync(cli,[...args,...a],{env:{PATH:root+"/empty"},encoding:"utf8"}));
    await ok("/api/agents",{id:"deleted-profile",name:"Retained history",system_prompt:"original"});
    await ok("/api/agents/deleted-profile",{},"DELETE");
    const exported=await ok("/api/agents/_portable/export");
    assert.equal(exported.repository.component,"agents");
    assert(exported.repository.scope.startsWith("webui-agents:"));
    const bundle=exported.bundle;
    assert(bundle.entries.find(e=>e.id==="deleted-profile").revisions.at(-1).value===null);
    assert(bundle.entries.some(e=>e.kind==="team"&&e.id==="reviewers"));
    assert(bundle.entries.find(e=>e.id==="research").revisions.length>=4);
    fs.writeFileSync(root+"/profiles-export.json",JSON.stringify(exported));
    const imported=repo("import",root+"/profiles-export.json");assert.equal(imported.next,bundle.entries.length);
    assert.deepEqual(repo("export").entries,bundle.entries);
    const researcher=bundle.entries.find(e=>e.id==="research");
    repo("delete","agent","research",String(researcher.revisions.at(-1).number));
    repo("import",root+"/profiles-export.json");
    assert.equal(repo("list").find(e=>e.id==="research").revisions.at(-1).value,null);
    const changed=structuredClone(bundle);changed.entries[0].revisions[0].timestamp=123;
    fs.writeFileSync(root+"/profiles-changed.json",JSON.stringify(changed));
    assert.throws(()=>repo("import",root+"/profiles-changed.json"),/Import source changed/);
    assert.equal((await ok("/api/agents/research")).persona.description,"Updated definition");
  }

  assert.equal(
    (
      await turn(chat, "specialist", "Answer as specialist", {
        recipient_id: "research",
      })
    ).speaker.id,
    "research",
  );
  assert(
    records
      .at(-1)
      .messages.some(
        (m) => m.role === "system" && m.content.includes("SPECIALIST_MARKER"),
      ),
  );
  assert(!records.at(-1).tools.some((t) => t.function.name === "agent_spawn"));
  const kbAnswer = await turn(chat, "knowledge", "read specialist knowledge", {
    recipient_id: "research",
  });
  assert(kbAnswer.message.includes("Specialist reference document."));
  await ok("/api/knowledge/skill", {
    action: "save",
    name: "native-guide",
    content: "NATIVE_SKILL_MARKER: cite the project notes.",
  });
  await ok("/api/knowledge/skill", { action: "pin", name: "native-guide" });
  await turn(chat, "skill", "Use pinned instructions");
  assert(
    records
      .at(-1)
      .messages.some((m) => m.content.includes("NATIVE_SKILL_MARKER")),
  );
  const pinnedChat = (await ok("/api/sessions", { persona_id: "research" }))
    .session_id;
  await turn(pinnedChat, "pinned", "Keep pinned specialist", {
    agent_id: "main",
  });
  assert.equal(
    (await ok("/api/sessions/" + pinnedChat)).persona_id,
    "research",
  );
  assert.equal(
    (
      await api("/api/chat/send", {
        session_id: chat,
        client_request_id: "specialist",
        message: "Answer as specialist",
        recipient_id: "main",
      })
    ).status,
    409,
  );
  const denied = await turn(chat, "restricted", "restricted tool", {
    recipient_id: "research",
  });
  assert(denied.message.includes("excluded by this specialist"));
  const discover = await ok("/api/provider-setup/discover", {
    base_url: provider,
  });
  assert.equal(discover.models[0].id, "fixture-specialist");
  const connected = await ok("/api/provider-setup/connect", {
    provider_name: " Local ",
    base_url: provider,
    models: ["fixture-specialist"],
  });
  assert.equal(connected.models[0], "local/fixture-specialist");
  assert(
    (await ok("/api/capabilities")).providers.some(
      (p) => p.id === "local" && p.capabilities.includes("tool_calling"),
    ),
  );
  assert.equal((await ok("/api/models")).default_model, "fixture");
  await ok("/api/provider-setup/roles", {
    default: "fixture",
    planner: "fixture",
    worker: "local/fixture-specialist",
  });
  await ok("/api/provider-setup/key", {
    provider: "redaction-test",
    api_key: "fixture-private-credential",
  });
  assert(
    !JSON.stringify(await ok("/api/provider-setup")).includes(
      "fixture-private-credential",
    ),
  );
  const decision = {
    mode: "off",
    model: "fixture",
    fallback_models: [],
    judge_fallback: false,
    threshold: 0.9,
    timeout_s: 2,
    overrides: {},
  };
  await ok("/api/models/decisions", decision);
  assert.equal(
    (await api("/api/models/decisions", { ...decision, threshold: 5 })).status,
    400,
  );
  await ok("/api/models/fallback", {
    enabled: false,
    models: {},
    providers: {},
  });
  await ok("/api/models/decisions", { ...decision, mode: "active" });
  await turn(chat, "routed", "Use automatic routing");
  assert.equal(records.at(-1).model, "fixture-specialist");
  await ok("/api/models/decisions", decision);
  for (const url of [
    "/api/mcp",
    "/api/channels",
    "/api/remote",
    "/api/extensions/skills",
    "/api/extensions/mods",
    "/api/integrations/github",
    "/api/audio8-settings",
    "/api/voice-profiles",
  ])
    assert.equal((await api(url)).status, 200, url);
  await ok("/api/mcp", {
    name: "configured-http",
    transport: "http",
    url: provider,
    enabled: false,
  });
  assert(
    (await ok("/api/mcp")).servers.some((v) => v.name === "configured-http"),
  );
  const mcpFile = path.join(root, "mcp-fixture.mjs");
  fs.writeFileSync(
    mcpFile,
    `import readline from 'node:readline';const input=readline.createInterface({input:process.stdin});input.on('line',line=>{const r=JSON.parse(line);if(!r.id)return;const result=r.method==='initialize'?{protocolVersion:'2024-11-05',capabilities:{tools:{}}}:r.method==='tools/list'?{tools:[{name:'echo',description:'stdio fixture tool',inputSchema:{type:'object',properties:{text:{type:'string'}}}}]}:{content:[{type:'text',text:'native-mcp-reply'}]};console.log(JSON.stringify({jsonrpc:'2.0',id:r.id,result}));});`,
  );
  await ok("/api/mcp", {
    name: "native-stdio",
    transport: "stdio",
    command: process.execPath,
    args: [mcpFile],
    env: { FIXTURE_MCP_SECRET: "never-echo-this-token" },
    enabled: true,
  });
  await ok("/api/mcp", {
    name: "native-http",
    transport: "http",
    url: provider.replace("/v1", "/mcp"),
    enabled: true,
  });
  assert(
    !JSON.stringify(await ok("/api/mcp")).includes("never-echo-this-token"),
  );
  assert.equal((await ok(`/api/permissions?session_id=${chat}`)).profile, "full");
  await ok("/api/permissions", { session_id: chat, profile: "ask" });
  for (const transport of ["stdio", "HTTP"]) {
    const action = turn(chat, "mcp-" + transport, "native mcp " + transport);
    const permission = await until(async () => {
      const pending = (await ok(`/api/sessions/${chat}`)).pending_approval;
      return pending?.action.startsWith("mcp_") && pending;
    });
    await ok("/api/approvals", {
      approval_id: permission.approval_id,
      approved: true,
    });
    assert((await action).message.includes("native-mcp-reply"));
  }

  assert.equal(
    (await ok(`/api/sessions/${chat}/mcp/release`, {})).receipts_retained,
    true,
  );

  const planning = turn(chat, "planned", "Read project notes and summarize", {
    agent_mode: "plan",
  });
  const approval = await until(async () => {
    const value = (await ok(`/api/sessions/${chat}`)).pending_approval;
    return value?.action === "approve_plan" && value;
  });
  const design = await ok(`/api/sessions/${chat}/design`);
  assert(design.files["plan.md"].includes("Read notes"));
  assert.equal(design.approved, false);
  const preview = await fetch(base + `/api/sessions/${chat}/files/plan.md`, {
    headers: { Authorization: `Bearer ${token}` },
  });
  assert.equal(preview.status, 200);
  await ok(
    `/api/sessions/${chat}/design`,
    {
      files: {
        "plan.md": design.files["plan.md"].replace(
          "report findings",
          "report verified findings",
        ),
      },
    },
    "PUT",
  );
  await ok("/api/approvals", {
    approval_id: approval.approval_id,
    approved: true,
  });
  const revised = await until(async () => {
    const pending = (await ok(`/api/sessions/${chat}`)).pending_approval;
    return pending && pending.approval_id !== approval.approval_id && pending;
  });
  assert(revised.detail.includes("verified findings"));
  await ok("/api/approvals", {
    approval_id: revised.approval_id,
    approved: true,
  });
  const planned = await planning;
  assert(planned.message.includes("Plan completed"));
  const approved = await ok(`/api/sessions/${chat}/design`);
  assert.equal(approved.approved, true);
  assert.equal(approved.todo_board.complete, true);
  assert.equal(approved.todo_board.outcomes[0].done, true);
  assert.equal(
    (
      await api(
        `/api/sessions/${chat}/design`,
        { plan: { steps: [] } },
        "PATCH",
      )
    ).status,
    409,
  );
  const notVerified = turn(chat, "unverified", "unverified plan", {
    agent_mode: "plan",
    max_steps: 512,
  });
  const unverifiedApproval = await until(async () => {
    const pending = (await ok(`/api/sessions/${chat}`)).pending_approval;
    return pending?.action === "approve_plan" && pending;
  });
  await ok("/api/approvals", {
    approval_id: unverifiedApproval.approval_id,
    approved: true,
  });
  assert.equal((await notVerified).status, "unverified");
  const fanoutChat = (await ok("/api/sessions", {})).session_id;
  const fanoutResult = await turn(fanoutChat, "fanout-test", "fanout parent");
  assert.equal(
    fanoutResult.message,
    "First child consumed without waiting for slow sibling.",
  );
  assert.equal((await ok("/api/jobs/" + fanoutJobs[0])).job.state, "running");
  assert(releaseFanoutSlow);
  releaseFanoutSlow();
  await jobDone(fanoutJobs[0]);
  releaseFanoutSlow = undefined;
  const allChat = (await ok("/api/sessions", {})).session_id;
  const allTurn = turn(allChat, "fanout-all-test", "fanout all parent");
  await until(() => releaseFanoutSlow && fanoutAllJobs && fanoutAllPartial);
  await jobDone(fanoutAllJobs[1]);
  releaseFanoutSlow();
  assert.equal((await allTurn).message, "All children collected.");
  const jobs = await Promise.all(
    ["first worker", "second worker"].map((objective, index) =>
      ok("/api/jobs", {
        session_id: chat,
        agent_id: "research",
        objective,
        client_request_id: "worker-" + index,
      }),
    ),
  );
  for (const spawned of jobs) {
    const job = await jobDone(spawned.id);
    assert.equal(job.state, "succeeded", JSON.stringify(job));
    assert.equal(job.result.speaker.id, "research");
  }
  const workerRequests = records.filter((record) =>
    record.messages.some(
      (m) => m.content === "first worker" || m.content === "second worker",
    ),
  );
  assert(workerRequests.every((r) => r.model === "fixture-specialist"));
  const slow = await ok("/api/jobs", {
    session_id: chat,
    agent_id: "research",
    objective: "slow worker",
    client_request_id: "slow",
  });
  await until(() =>
    records.some((r) => r.messages.some((m) => m.content === "slow worker")),
  );
  await ok(`/api/jobs/${slow.id}/cancel`, {});
  assert.equal((await jobDone(slow.id)).state, "cancelled");
  const steering = await ok("/api/jobs", {
    session_id: chat,
    agent_id: "research",
    objective: "message worker",
    client_request_id: "steering",
  });
  await until(() =>
    records.some((r) => r.messages.some((m) => m.content === "message worker")),
  );
  await ok(`/api/jobs/${steering.id}/message`, {
    message: "Use the revised criteria",
    message_id: "revised",
  });
  assert.equal(
    (await jobDone(steering.id)).result.message,
    "Steering acknowledged.",
  );
  const interrupted = await ok("/api/jobs", {
    session_id: chat,
    agent_id: "research",
    objective: "slow worker restart",
    client_request_id: "crash",
  });
  await until(() =>
    records.some((r) =>
      r.messages.some((m) => m.content === "slow worker restart"),
    ),
  );
  const pendingId = "crash-before-turn-insert";
  const admissionDb = new DatabaseSync(path.join(root, "state/native-chats.sqlite"));
  admissionDb.prepare("INSERT INTO ui_pending_admissions(chat_id,request_id) VALUES (?,?)").run(chat, pendingId);
  admissionDb.close();
  assert.equal((await ok(`/api/sessions/${chat}/requests/${pendingId}`)).receipt.state, "received");
  child.kill("SIGKILL");
  await new Promise((r) => child.once("exit", r));
  await start();
  const interruptedAdmission = await ok(`/api/sessions/${chat}/requests/${pendingId}`);
  assert.equal(interruptedAdmission.receipt.result_status, "interrupted");
  assert.equal(interruptedAdmission.receipt.turn_id, "");
  assert.equal((await api("/api/chat/send", {
    session_id: chat, client_request_id: pendingId, message: "Do not execute a stale admission",
  })).status, 409);
  assert.equal((await jobDone(interrupted.id)).state, "interrupted");
  assert.equal(
    (await jobDone(interrupted.id)).error.code,
    "worker_interrupted",
  );
  assert.equal((await ok("/api/sessions/old-chat")).messages.length, 2);
  assert.equal(
    (await ok("/api/agents/research")).persona.description,
    "Updated definition",
  );
  assert.equal((await ok("/api/models/fallback")).policy.enabled, false);
  const restored = await ok("/api/sessions/" + chat);
  assert(
    restored.messages.some((m) => m.text === "Remember this conversation"),
  );
  assert(restored.messages.some((m) => m.speaker?.id === "research"));
  assert.equal((await ok(`/api/sessions/${chat}/design`)).approved, true);
  assert(
    (await ok("/api/sessions/" + chat)).messages.some((m) => m.job_reply_id),
  );
  await turn("old-chat", "continuation", "What did I say earlier?");
  assert(
    records.at(-1).messages.some((m) => m.content === "Original chat message"),
  );
  console.log(
    JSON.stringify({
      status: "passed",
      base,
      chat,
      root,
      checks: [
        "legacy chats preserved",
        "specialist instructions and tool policy",
        "agent revision history",
        "WebUI writes owned profile store only",
        ...(process.env.KAGEHA_TEST_PROFILE_REPOSITORY === "1" ? ["portable profile export preserves history and tombstones", "owned repository import keeps source untouched"] : []),
        "teams CRUD",
        "provider discovery and connection",
        "credentials redacted",
        "validated model settings",
        "plan review and immutable approval",
        "evidence-backed progress",
        "parallel specialists",
        "worker cancellation",
        "worker steering",
        "crash recovery",
        "settings and chat persistence",
        "no Python in backend PATH",
      ],
    }),
  );
  if (process.argv.includes("--keep")) {
    console.log(`UI: ${base}/login token: ${token}`);
    await new Promise((r) => process.once("SIGTERM", r));
  }
} catch (error) {
  console.error(error);
  for (const file of fs.readdirSync(root).filter((v) => v.endsWith(".log")))
    console.error(fs.readFileSync(path.join(root, file), "utf8"));
  throw error;
} finally {
  for (const proc of children) {
    if (proc.exitCode === null) {
      proc.kill("SIGTERM");
      await Promise.race([
        new Promise((r) => proc.once("exit", r)),
        pause(1000),
      ]);
    }
  }
  for (const timer of timers) clearTimeout(timer);
  fixture.closeAllConnections();
  await new Promise((r) => fixture.close(r));
  if (!process.argv.includes("--keep"))
    fs.rmSync(root, {
      recursive: true,
      force: true,
      maxRetries: 3,
      retryDelay: 100,
    });
}
