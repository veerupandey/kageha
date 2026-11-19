// Real Chromium + standalone Rust browser + native HTTP API. No Python process.
// Usage: KAGEHA_BROWSER_BIN=/absolute/kageha-browser node tests/webui_browser_e2e.mjs
// Add --live-profiles for a live Codex model and disposable public/signed-in job pages.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import http from "node:http";
import { spawn, execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
const crate = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const app =
  process.env.KAGEHA_LOOP_BIN || path.resolve(crate, "../../target/release/kageha-loop");
const browser = process.env.KAGEHA_BROWSER_BIN;
const chrome =
  process.env.KAGEHA_CHROMIUM ||
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
assert(browser, "Set KAGEHA_BROWSER_BIN to the standalone Rust daemon");
const root = fs.mkdtempSync("/tmp/kageha-native-browser-");
const children = [];
const liveProfiles = process.argv.includes("--live-profiles");
const personalChannels = process.argv.includes("--personal-channels");
const pageTitle = "Maple opening " + root.split("-").at(-1);
const visits = [];
const token = "native-browser-fixture-authentication-token";
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
async function until(fn, timeout = 20000) {
  const end = Date.now() + timeout;
  let last;
  while (Date.now() < end) {
    try {
      const result = await fn();
      if (result) return result;
    } catch (error) {
      last = error;
    }
    await sleep(100);
  }
  throw last || Error("Timed out");
}
function start(bin, args, env = process.env) {
  const child = spawn(bin, args, {
    env,
    stdio: [
      "ignore",
      fs.openSync(path.join(root, `${children.length}.log`), "a"),
      fs.openSync(path.join(root, `${children.length}.err`), "a"),
    ],
  });
  children.push(child);
  return child;
}
let reviewVerdict="allow", reviewCalls=0;
let notifyModelStarted, releaseModel;
const modelStarted = new Promise(resolve => { notifyModelStarted = resolve; });
const panelChanged = new Promise(resolve => { releaseModel = resolve; });
let submissions = 0;
let animateNextForm = false;
let renderedConfirmation = false;
let releaseSubmission;
const submissionResponse = new Promise(resolve => { releaseSubmission = resolve; });
const server = http.createServer(async (req, res) => {
  if (personalChannels && req.url.startsWith("/channel-page/")) {
    visits.push(req.url);
    res.setHeader("Content-Type", "text/html");
    res.end(`<title>${pageTitle}</title><h1>${pageTitle}</h1>`);
    return;
  }
  if (liveProfiles) {
    const pages = {"/seed":"Fixture ready", "/public":"Maple compiler engineer", "/saved":"Aspen compiler engineer", "/uas/login":"Sign in to view your saved jobs"};
    if (req.url === "/seed") res.setHeader("Set-Cookie", "fixture_member=yes; HttpOnly; SameSite=Lax; Path=/");
    if (req.url === "/saved" && !String(req.headers.cookie).includes("fixture_member=yes")) {
      res.writeHead(302, {Location:"/uas/login"}).end();
      return;
    }
    res.setHeader("Content-Type", "text/html");
    res.end("<h1>" + (pages[req.url] || "Local fixture") + "</h1>");
    return;
  }
  if (req.url === "/uas/login") {
    res.setHeader("Content-Type", "text/html");
    res.end("<title>Just a moment...</title><h1>Performing security verification</h1>");
    return;
  }
  if (req.url === "/application" && req.method === "GET") {
    res.setHeader("Content-Type", "text/html");
    res.end(`<!doctype html><h1>Review application</h1><button id="submit">Submit application</button>
      <script>document.querySelector("#submit").onclick = async () => {
        const response = await fetch("/application", {method:"POST"});
        if (response.ok) {
          document.querySelector("h1").textContent = "Application received";
          await fetch("/application/confirmation", {method:"POST"});
        }
      };</script>`);
    return;
  }
  if (req.url === "/application" && req.method === "POST") {
    submissions++;
    await submissionResponse;
    res.end("Application received");
    return;
  }
  if (req.url === "/application/confirmation" && req.method === "POST") {
    renderedConfirmation = true;
    res.writeHead(204).end();
    return;
  }
  if(req.url==='/frame'){
    res.setHeader('Content-Type','text/html');res.end('<label>Frame email<input id="frame-email" aria-label="Frame email"></label>');return;
  }
  if (req.url === "/form" || req.url.startsWith("/form?")) {
    res.setHeader("Content-Type", "text/html");
    res.end(
      '<!doctype html><title>Browser integration fixture</title><h1>Browser integration fixture</h1><label>Email<input id="email" aria-label="Email"></label><label>Departments<select multiple id="departments" aria-label="Departments"><option value="engineering">Engineering</option><option value="research">Research</option><option value="sales" disabled>Sales</option></select></label><label>No<input id="no" type="radio" name="answer" aria-label="No" value="no"></label><iframe src="/frame" title="Form frame"></iframe><label>Resume<input id="file" type="file" aria-label="Resume"></label><button id="save" onclick="document.querySelector(\'h1\').textContent=\'Saved\'">Save</button>' +
      (animateNextForm ? '<canvas id="preview-animation" aria-hidden="true" width="12" height="12"></canvas><script>let frame=0;setInterval(()=>{const c=document.querySelector("#preview-animation").getContext("2d");c.fillStyle=`hsl(${frame++%360} 100% 50%)`;c.fillRect(0,0,12,12)},80)</script>' : ''),
    );
    animateNextForm = false;
    return;
  }
  if (req.url !== "/v1/chat/completions") {
    res.writeHead(404);
    res.end();
    return;
  }
  let bytes = "";
  for await (const part of req) bytes += part;
  const payload = JSON.parse(bytes);
  if (personalChannels) {
    const last = payload.messages.at(-1);
    const fixture = `http://127.0.0.1:${server.address().port}/channel-page/`;
    const intent = payload.messages.findLast(message => message.role === "user")?.content;
    const requestedPage = ["whatsapp", "web"].find(channel => intent === `Open ${fixture}${channel} in my comet browser`);
    const url = intent === "Open LinkedIn in my comet browser" ? "https://www.linkedin.com/" :
      requestedPage ? fixture + requestedPage : undefined;
    if (last.role === "user" && url && payload.tools?.some(tool => tool.function?.name === "browser_open")) {
      res.setHeader("Content-Type", "application/json");
      res.end(JSON.stringify({ choices: [{ message: { role: "assistant", content: null, tool_calls: [{
        id: "personal-browser-open", type: "function",
        function: { name: "browser_open", arguments: JSON.stringify({ url, profile: "comet" }) },
      }] } }] }));
      return;
    }
    const observed = last.role === "tool" && last.content.includes(pageTitle);
    const content = observed ? "I found " + pageTitle + "." :
      last.role === "tool" ? "The browser request did not open the page." : "I cannot control that browser.";
    res.setHeader("Content-Type", "application/json");
    res.end(JSON.stringify({ choices: [{ message: { role: "assistant", content } }] }));
    return;
  }
  if (payload.messages[0]?.content?.startsWith("You independently review")) {
    reviewCalls++; const context=JSON.parse(payload.messages[1].content); assert(context.user_intent.includes("fill"));
    res.end(JSON.stringify({choices:[{message:{role:"assistant",content:reviewVerdict==="invalid"?"not JSON":JSON.stringify({input_digest:context.request.authority.input_digest,verdict:reviewVerdict,rationale:"Fixture approval review"})}}]}));return;
  }
  const messages = payload.messages;
  const last = messages.at(-1);
  const intent = messages.filter((m) => m.role === "user").at(-1).content;
  let call;
  let answer = "Browser action finished.";
  if (last.role === "tool") {
    let recordedOutcomes = false;
    try { recordedOutcomes = Array.isArray(JSON.parse(last.content).accepted); } catch {}
    if (intent.startsWith("lifecycle navigate ")) {
      if (recordedOutcomes) call={name:"browser_snapshot",arguments:{}};
      else if (last.content.includes('"observation_id"') && !last.content.includes('"next_step"')) {
        const snapshot=JSON.parse(last.content);
        call={name:"browser_navigate",arguments:{url:intent.slice("lifecycle navigate ".length),observation_id:snapshot.observation_id}};
      } else answer=last.content;
    } else if (intent.startsWith("lifecycle close ") && recordedOutcomes) {
      call={name:"browser_close",arguments:{tab_handle:intent.slice("lifecycle close ".length)}};
    } else if (intent === "inspect security fixture") {
      answer = JSON.stringify({browser_access: JSON.parse(last.content).browser_access});
    } else if (intent === "use Comet for the account fixture") {
      answer = "Opened the fixture.";
    } else if (intent.startsWith("Check submission receipt ") && recordedOutcomes) {
      call = {name:"browser_receipts", arguments:{request_id:intent.split(" ").at(-1)}};
    } else if ((intent === "Submit the disposable application" || /^(fill|select|check|press)/.test(intent)) && recordedOutcomes) {
      call = {name:"browser_snapshot", arguments:{}};
    } else if (intent.startsWith("upload") && recordedOutcomes) {
      const match = intent.match(/inputs\/[^`\n]+/);
      call = {name:"browser_upload", arguments:{target:"#file", path:match[0]}};
    } else if (intent.startsWith("open") && recordedOutcomes) {
      call = {name:"browser_open", arguments:{url:`http://127.0.0.1:${server.address().port}/form`}};
    } else if (intent === "Submit the disposable application" && last.content?.includes('"controls"')) {
      const snapshot = JSON.parse(last.content);
      const submit = snapshot.controls.find(control => control.name === "Submit application");
      call = {name:"browser_click", arguments:{
        target:submit.ref, observation_id:snapshot.observation_id,
        expected_selector:"h1", expected_text:"Application received", application_submission:true, outcome_id:"application-submitted",
      }};
    } else if (
      (last.name === "browser_snapshot" && /^(fill|select|check|press)/.test(intent)) ||
      (last.content?.includes("observation_id") && /^(fill|select|check|press)/.test(intent))
    ) {
      const snapshot = JSON.parse(last.content);
      const field = snapshot.controls.find((c) => c.name === (intent.startsWith('select')?'Departments':intent.startsWith('check')?'No':intent.startsWith('fill frame')?'Frame email':'Email'));
      call = intent.startsWith('select')?{name:'browser_select',arguments:{target:field.ref,observation_id:snapshot.observation_id,option:['engineering','research']}}:
        intent.startsWith('check')?{name:'browser_check',arguments:{target:field.ref,observation_id:snapshot.observation_id,checked:true}}:
        intent.startsWith('press')?{name:'browser_press',arguments:{target:field.ref,observation_id:snapshot.observation_id,key:'.'}}:{
        name: "browser_fill",
        arguments: {
          target: field.ref,
          observation_id: snapshot.observation_id,
          text: "native@example.test",
        },
      };
    } else {
      answer = last.content;
    }
  } else if (intent.startsWith("lifecycle ")) {
    call={name:"record_required_outcomes",arguments:{outcomes:[{id:"lifecycle-report",description:"Report tab lifecycle action",kind:"answer",verify:"Report the observed result"}]}};
  } else if (intent === "wait for panel switch") {
    notifyModelStarted();
    await panelChanged;
    answer = "This stale response should be superseded.";
  } else if (intent.includes("Browser panel update:") && intent.includes("selected profile is comet")) {
    answer = "The executor received the Comet selection.";
  } else if (intent === "use Comet for the account fixture") {
    call = {name:"browser_open", arguments:{profile:"comet",url:`http://127.0.0.1:${server.address().port}/uas/login`}};
  } else if (intent === "inspect security fixture") {
    call = {name:"browser_snapshot", arguments:{}};
  } else if (intent === "Submit the disposable application")
    call = {name:"record_required_outcomes", arguments:{outcomes:[{id:"application-submitted",description:"Submit the disposable application",kind:"action",verify:"Verified site confirmation"}]}};
  else if (intent.startsWith("preview status"))
    call = { name: "browser_preview_status", arguments: {} };
  else if (intent === "receipt inventory")
    call = { name: "browser_receipts", arguments: {} };
  else if (intent.startsWith("Check submission receipt "))
    call = {name:"record_required_outcomes", arguments:{continue_previous:true,outcomes:[{id:"report-submission-recovery",description:"Report submission recovery",kind:"answer",verify:"Report the recovered receipt",origin:"agent"}]}};
  else if (intent === "workspace inventory")
    call = { name: "list_workspace_files", arguments: {} };
  else if (/^(fill|select|check|press|upload|open)/.test(intent))
    call = {name:"record_required_outcomes", arguments:{outcomes:[{id:"report-browser-action",description:"Report the browser action result",kind:"answer",verify:"Report the action result"}]}};
  const message = call
    ? {
        role: "assistant",
        content: null,
        tool_calls: [
          {
            id: `call-${messages.length}`,
            type: "function",
            function: {
              name: call.name,
              arguments: JSON.stringify(call.arguments),
            },
          },
        ],
      }
    : { role: "assistant", content: answer };
  res.setHeader("Content-Type", "application/json");
  res.end(JSON.stringify({ choices: [{ message }] }));
});
await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
const fixture = `http://127.0.0.1:${server.address().port}`;
let base, chat;
async function api(route, body, method) {
  const response = await fetch(base + route, {
    method: method || (body ? "POST" : "GET"),
    headers: {
      Authorization: `Bearer ${token}`,
      ...(body ? { "Content-Type": "application/json" } : {}),
    },
    body: body ? JSON.stringify(body) : undefined,
  });
  const data = await response.json();
  return { status: response.status, data };
}
async function ok(route, body, method) {
  const result = await api(route, body, method);
  assert.equal(result.status, 200, JSON.stringify(result));
  // Event history is paginated. Longer lifecycle coverage must not make later
  // assertions silently inspect only the first page of this chat.
  if (!body && /\/events(?:\?|$)/.test(route) && Array.isArray(result.data.events)) {
    const events=[...result.data.events];
    let page=result.data;
    while(page.has_more) {
      const next=new URL(route,"http://fixture");next.searchParams.set("after",page.next_after);
      const response=await api(next.pathname+next.search);
      assert.equal(response.status,200,JSON.stringify(response));
      page=response.data;events.push(...page.events);
    }
    return {...result.data,events,next_after:page.next_after,has_more:false};
  }
  return result.data;
}
async function state() {
  const value=await ok(`/api/sessions/${chat}/browser/state`);
  assert(!value.targetIdentity.includes('"'), "Target identity must contain raw IDs, not JSON-quoted IDs");
  return value;
}
async function browserPost(name, payload = {}) {
  const current = await state();
  return ok(`/api/sessions/${chat}/browser/${name}`, {
    bindingId: current.bindingId,
    ...payload,
  });
}
async function turn(message, id) {
  return ok("/api/chat/send", {
    session_id: chat,
    client_request_id: id,
    message,
  });
}
async function pending() {
  return until(async () => {
    const session = await ok(`/api/sessions/${chat}`);
    if (session.pending_approval) {
      const replay = await ok(`/api/sessions/${chat}/events`);
      assert(replay.events.some((event) => event.kind === "approval_required"));
    }
    return session.pending_approval;
  });
}
async function pageValue(expression) {
  const targets = await (await fetch(profileEndpoint + "/json/list")).json();
  const tab = targets.find((t) => t.url === fixture + "/form");
  assert(tab, "Managed profile tab exists");
  const socket = new WebSocket(tab.webSocketDebuggerUrl);
  await new Promise((r) => socket.addEventListener("open", r, { once: true }));
  const result = new Promise((r) =>
    socket.addEventListener("message", (event) => {
      const value = JSON.parse(event.data);
      if (value.id === 1) r(value.result.result.value);
    }),
  );
  socket.send(
    JSON.stringify({
      id: 1,
      method: "Runtime.evaluate",
      params: { expression, returnByValue: true },
    }),
  );
  const value = await result;
  socket.close();
  return value;
}
async function testTabLifecycle(originalTab, lifecycleCount) {
  const navigated=await turn("lifecycle navigate "+fixture+"/form?lifecycle=1","lifecycle-navigate");
  assert.equal(JSON.parse(navigated.message).target_id,originalTab,JSON.stringify(navigated));
  assert.equal((await state()).tabs.length,lifecycleCount);
  await turn("lifecycle navigate "+fixture+"/form","lifecycle-navigate-back");
  await turn("open disposable lifecycle fixture","lifecycle-open");
  const disposable=(await state()).tabs.find(tab=>tab.active).tabHandle;
  assert.notEqual(disposable,originalTab);
  await ok("/api/permissions",{session_id:chat,profile:"ask"});
  const closingDenied=turn("lifecycle close "+disposable,"lifecycle-close-denied");
  const closeApproval=await pending();assert.equal(closeApproval.action,"browser_close");
  await ok("/api/approvals",{approval_id:closeApproval.approval_id,approved:false,scope:"once"});
  await closingDenied;
  assert((await state()).tabs.some(tab=>tab.tabHandle===disposable));
  // Approval for an exact target cannot survive a user profile change.
  const originalProfile=(await state()).selectedProfile;
  const changedClose=turn("lifecycle close "+disposable,"lifecycle-close-stale");
  const staleApproval=await pending();
  await browserPost("promote",{target:"profile:"+(originalProfile==="private"?"alternate":"private")});
  await ok("/api/approvals",{approval_id:staleApproval.approval_id,approved:true,scope:"once"});
  await changedClose;
  await browserPost("promote",{target:"profile:"+originalProfile,consent:true});
  assert((await state()).tabs.some(tab=>tab.tabHandle===disposable),"A stale close approval must preserve the original target");
  // Take over after the tool is admitted: a new explicit chat turn normally
  // returns browser control, so taking over before sending it would test that flow.
  const userClose=turn("lifecycle close "+disposable,"lifecycle-close-user-control");
  const userApproval=await pending();
  await browserPost("lease",{action:"take"});
  await ok("/api/approvals",{approval_id:userApproval.approval_id,approved:true,scope:"once"});
  await userClose;
  const controlled=await state();
  assert(controlled.tabs.some(tab=>tab.tabHandle===disposable),"A user takeover during approval must prevent closing");
  await browserPost("lease",{action:"return",controlGeneration:controlled.controlGeneration});
  await ok("/api/permissions",{session_id:chat,profile:"full",confirm_full:true});
  const closed=await turn("lifecycle close "+disposable,"lifecycle-close");
  assert.equal(JSON.parse(closed.message).closed,true,JSON.stringify(closed));
  const current=await state();assert(!current.tabs.some(tab=>tab.tabHandle===disposable));
  assert(current.tabs.some(tab=>tab.tabHandle===originalTab));

}

async function testPersonalProfilePermission() {
  assert.equal((await ok(`/api/permissions?session_id=${chat}`)).profile, "full");
  const shared = await Promise.race([
    turn("use Comet for the account fixture", "implicit-personal"),
    new Promise((_, reject) => setTimeout(() => reject(Error("Full access waited for approval")), 20000).unref()),
  ]);
  assert.equal(shared.status, "success");
  assert.equal((await state()).selectedProfile, "comet");
  const shareEvents = (await ok(`/api/sessions/${chat}/events?turn_id=${shared.turn_id}`)).events;
  assert(shareEvents.some(event => event.tool_card?.name === "browser_open" && event.tool_card.status === "done"));
  assert(!shareEvents.some(event => event.kind === "approval_required"));
  await browserPost("promote", { target: "profile:private" });
  await ok("/api/permissions", { session_id: chat, profile: "ask" });
  const attaching = turn("use Comet for the account fixture", "signed-in-attach");
  const consent = await pending();
  assert.equal(consent.action, "browser_open");
  assert.match(consent.detail, /comet/i);
  assert.equal((await state()).selectedProfile, "private");
  await ok("/api/approvals", { approval_id: consent.approval_id, approved: true, scope: "once" });
  await attaching;
  await ok("/api/permissions", { session_id: chat, profile: "full", confirm_full: true });
  assert.equal((await state()).selectedProfile, "comet");
}

async function testLiveProfileChoice() {
  await ok("/api/agents", {id:"profile-tester", name:"Profile tester",
    system_prompt:"Use only the disposable local website specified by the user. Return the observed job title.",
    tool_policy:{allow:["browser_open","browser_tabs","browser_snapshot"],deny:[]}});
  for (const [request, route, title, expected] of [
    ["Read this public job listing", "/public", "Maple compiler engineer", "private"],
    ["Find the one role on my saved shortlist", "/saved", "Aspen compiler engineer", "comet"],
    ["Use Comet to read my saved shortlist", "/saved", "Aspen compiler engineer", "comet"],
  ]) {
    chat = (await ok("/api/sessions", {persona_id:"profile-tester"})).session_id;
    const work = turn(request + " at " + fixture + route + ". What is the job title?", "check");
    const deadline = Date.now() + 150000;
    let approvalId, result;
    while (!(result = await Promise.race([work, sleep(200).then(() => null)]))) {
      assert(Date.now() < deadline, "The model did not finish before the fixture deadline");
      const approval = (await ok("/api/sessions/" + chat)).pending_approval;
      if (approval && approval.approval_id !== approvalId) {
        assert.equal(expected, "comet");
        assert.equal(approval.action, "browser_open");
        assert(approval.detail.includes("comet") && approval.detail.includes(fixture), "Only the disposable Comet profile and local origin may be approved");
        await ok("/api/approvals", {approval_id:approval.approval_id, approved:true, scope:"once"});
        approvalId = approval.approval_id;
      }
    }
    const selected = (await state()).selectedProfile;
    if (selected !== expected || !result.message.includes(title)) {
      const events = (await ok(`/api/sessions/${chat}/events`)).events;
      const cards = events.map(event => event.tool_card).filter(card => card && card.status !== "running")
        .map(card => ({name:card.name, status:card.status, error:card.status === "failed" ? card.result_preview?.slice(0,300) : undefined}));
      assert.fail(JSON.stringify({request, expected, selected, approved:Boolean(approvalId), cards, answer:result.message}));
    }
    assert.equal(Boolean(approvalId), expected === "comet");
    console.log(JSON.stringify({request, profile:expected, approved:Boolean(approvalId), title}));
  }
}

let profileEndpoint;
async function run() {
  fs.mkdirSync(path.join(root, "workspace"));
  fs.mkdirSync(path.join(root, "bin"));
  fs.symlinkSync(
    execFileSync("/usr/bin/which", ["curl"], { encoding: "utf8" }).trim(),
    path.join(root, "bin/curl"),
  );
  const endpoints = {};
  for (const name of ["chrome", "comet"]) {
    const profile = path.join(root, name);
    fs.mkdirSync(profile);
    start(chrome, [
      "--headless=new",
      "--no-first-run",
      "--no-default-browser-check",
      "--remote-debugging-port=0",
      `--user-data-dir=${profile}`,
      liveProfiles && name === "comet" ? fixture + "/seed" : "about:blank",
    ]);
    const port = await until(
      () =>
        fs.existsSync(path.join(profile, "DevToolsActivePort")) &&
        fs
          .readFileSync(path.join(profile, "DevToolsActivePort"), "utf8")
          .split("\n")[0],
    );
    endpoints[name] = `http://127.0.0.1:${port}`;
  }
  profileEndpoint = endpoints.chrome;
  const daemonRoot = path.join(root, "browser");
  fs.mkdirSync(daemonRoot, { mode: 0o700 });
  start(browser, [
    "--state-dir",
    daemonRoot,
    "--instance-id",
    "native-test",
    "--application-id",
    "native-test",
    "--chromium",
    chrome,
    "--standalone",
    "on",
  ]);
  await until(() => fs.existsSync(path.join(daemonRoot, "daemon.json")));
  const env = {
    PATH: path.join(root, "bin"),
    KAGEHA_SERVER_TOKEN: token,
    KAGEHA_MODEL: "fixture",
    KAGEHA_PROVIDER_URL: fixture + "/v1/chat/completions",
    KAGEHA_BROWSER_STATE_DIR: daemonRoot,
    KAGEHA_CHROME_CDP_ENDPOINT: profileEndpoint,
    KAGEHA_COMET_CDP_ENDPOINT: endpoints.comet,
  };
  if (liveProfiles) {
    Object.assign(env, {
      PATH:process.env.PATH, HOME:root, CODEX_HOME:process.env.CODEX_HOME || path.join(process.env.HOME, ".codex"),
      KAGEHA_CODEX_BIN:process.env.KAGEHA_CODEX_BIN || execFileSync("/usr/bin/which", ["codex"], {encoding:"utf8"}).trim(),
      KAGEHA_MODEL:"gpt-codex-6-luna", KAGEHA_MODELS_FILE:path.resolve(crate, "../../models.yaml"),
      KAGEHA_MODEL_ENV_FILE:path.join(root, "empty.env"),
    });
    fs.writeFileSync(env.KAGEHA_MODEL_ENV_FILE, "");
    delete env.KAGEHA_PROVIDER_URL;
    delete env.KAGEHA_CHROME_CDP_ENDPOINT;
  }
  if (personalChannels) env.KAGEHA_WEBUI_DIST = path.resolve(crate, "../../frontend/dist");
  if (process.env.KAGEHA_WEBUI_DIST)
    env.KAGEHA_WEBUI_DIST = process.env.KAGEHA_WEBUI_DIST;
  // Each launch has a separate log; stderr includes the ephemeral API address.
  async function startHost() {
    const appLog = path.join(root, `${children.length}.err`);
    const child = start(app, [
      "serve", path.join(root, "workspace"), path.join(root, "state"), "127.0.0.1:0",
    ], env);
    base = await until(() => {
      const match = fs.readFileSync(appLog, "utf8").match(/^Rust chat service listening on (127\.0\.0\.1:\d+)\r?\n/m);
      return match && "http://" + match[1];
    });
    return child;
  }
  const host = await startHost();
  if (personalChannels) {
    const { testPersonalBrowser } = await import("../../../integrations/personal/browser-acceptance.mjs");
    return testPersonalBrowser({ root, base, token, fixture, pageTitle, visits, endpoints, ok, until });
  }
  if (liveProfiles) return testLiveProfileChoice();
  chat = (await ok("/api/sessions", {})).session_id;
  if (process.argv.includes("--tab-lifecycle")) {
    await ok("/api/permissions",{session_id:chat,profile:"full",confirm_full:true});
    await browserPost("connect");
    await turn("open fixture","lifecycle-seed");
    const current=await state();
    const original=current.tabs.find(tab=>tab.active).tabHandle;
    await testTabLifecycle(original,current.tabs.length);
    const events=(await ok(`/api/sessions/${chat}/events`)).events;
    for(const name of ["browser_navigate","browser_close"]) {
      assert(events.some(event=>event.tool_card?.name===name&&event.tool_card.status==="done"),name+" must execute through native tools");
    }
    console.log(JSON.stringify({passed:true,nativeTabLifecycle:true,navigationReusesTarget:true,closeApprovalDenied:true,explicitClose:true}));
    return;
  }
  if (process.argv.includes("--profile-permissions")) {
    await testPersonalProfilePermission();
    console.log("Personal profile opened automatically in Full access and required approval in Ask mode.");
    return;
  }
  const noTab = JSON.parse((await turn("receipt inventory", "no-tab-receipts")).message);
  assert.equal(noTab.scope, "no_selected_tab");
  assert.deepEqual(noTab.receipts, []);
  assert.deepEqual((await state()).availableProfiles, [
    "private",
    "alternate",
    "chrome",
    "comet",
  ]);
  await browserPost("connect");
  let current = await state();
  assert.equal(current.selectedProfile, "private");
  assert(current.connected);
  await browserPost("lease", { action: "take" });
  current = await state();
  let generation = current.controlGeneration;
  assert(generation);
  // Static preview frames are intentionally deduplicated. Animate pixels so this
  // connection-lifetime check measures 72 distinct frames rather than keepalives.
  animateNextForm = true;
  await browserPost("action", {
    action: "navigate",
    url: fixture + "/form",
    controlGeneration: generation,
  });
  const stale = await api(`/api/sessions/${chat}/browser/action`, {
    bindingId: "stale",
    action: "reload",
    controlGeneration: generation,
  });
  assert.equal(stale.status, 409);
  await browserPost("layout", { mode: "panel", width: 900, height: 600 });
  current = await state();
  assert.equal(current.viewport.width, 900);
  assert.equal(current.previewAcknowledgment, true);
  assert.equal((await browserPost("paint", { displayed: false })).displayed, false);
  assert.equal((await api(`/api/sessions/${chat}/browser/paint`, {
    bindingId: "stale-paint", displayed: true,
  })).status, 409);
  await browserPost("layout", { mode: "panel", width: 900, height: 600 });
  assert.equal((await state()).bindingId, current.bindingId);
  const previewStream = await fetch(
    `${base}/api/sessions/${chat}/browser/stream?binding=${encodeURIComponent(current.bindingId)}&viewport=900x600`,
    { headers: { Authorization: `Bearer ${token}` }, signal: AbortSignal.timeout(45000) },
  );
  assert.equal(previewStream.status, 200);
  const reader = previewStream.body.getReader();
  let pendingFrames = Buffer.alloc(0);
  let renderedFrames = 0;
  let renewed = false;
  try {
    while (renderedFrames < 72) {
      const chunk = await reader.read();
      assert(!chunk.done, "Preview ended at the daemon connection request limit");
      pendingFrames = Buffer.concat([pendingFrames, chunk.value]);
      while (true) {
        const end = pendingFrames.indexOf("\r\n\r\n");
        if (end < 0) break;
        const length = Number(/Content-Length:\s*(\d+)/i.exec(pendingFrames.subarray(0, end).toString())[1]);
        if (pendingFrames.length < end + 4 + length) break;
        pendingFrames = pendingFrames.subarray(end + 4 + length);
        renderedFrames++;
      }
      if (!renewed && renderedFrames >= 36) {
        const lease = await browserPost("lease", { action: "renew", controlGeneration: generation });
        generation = lease.controlGeneration;
        renewed = true;
      }
    }
  } finally {
    await reader.cancel();
  }
  await browserPost("lease", {
    action: "return",
    controlGeneration: generation,
  });
  assert.equal((await browserPost("paint", { displayed: true })).displayed, true);
  await turn("preview status visible", "preview-visible");
  let observedPreview = (await ok(`/api/sessions/${chat}`)).messages
    .filter(message => message.role === "assistant").at(-1).text;
  assert.equal(JSON.parse(observedPreview).displayed, true);
  assert.equal((await browserPost("paint", { displayed: false })).displayed, false);
  await turn("preview status hidden", "preview-hidden");
  observedPreview = (await ok(`/api/sessions/${chat}`)).messages
    .filter(message => message.role === "assistant").at(-1).text;
  assert.equal(JSON.parse(observedPreview).displayed, false);
  assert.equal((await state()).bindingId, current.bindingId);
  assert.equal(
    (await api(`/api/sessions/${chat}/browser/stream?binding=wrong`)).status,
    409,
  );
  const frame = await fetch(base + current.frame.url, {
    headers: { Authorization: `Bearer ${token}` },
  });
  assert.equal(frame.status, 200);
  assert.equal(frame.headers.get("content-type"), "image/jpeg");
  assert((await frame.arrayBuffer()).byteLength > 1000);
  const noConsent = await api(`/api/sessions/${chat}/browser/promote`, {
    bindingId: (await state()).bindingId,
    target: "profile:chrome",
  });
  assert.equal(noConsent.status, 409);
  await testPersonalProfilePermission();
  const security = await turn("inspect security fixture", "security-handoff");
  assert.equal(JSON.parse(security.message).browser_access.kind, "security_check");
  await browserPost("promote", { target: "profile:private" });
  const steering = turn("wait for panel switch", "panel-steering");
  await Promise.race([modelStarted, sleep(10000).then(() => { throw Error("Fixture model did not start"); })]);
  try {
    await browserPost("promote", { target: "profile:comet", consent: true });
  } finally {
    releaseModel();
  }
  assert.equal((await steering).message, "The executor received the Comet selection.");
  for (const name of ["chrome", "comet", "alternate", "private", "chrome"]) {
    await browserPost("promote", { target: `profile:${name}`, consent: true });
    assert.equal((await state()).selectedProfile, name);
  }
  const openedFixture = await turn("open fixture", "open");
  current = await state();
  assert.equal(current.url, fixture + "/form", JSON.stringify(openedFixture).slice(0,1000));
  assert(current.tabs.length > 0);
  const originalTab = current.tabs.find(tab => tab.active).tabHandle;
  await testTabLifecycle(originalTab,current.tabs.length);
  current=await state();
  const replay = await ok(`/api/sessions/${chat}/events`);
  const openCard = replay.events.find(({ tool_card }) =>
    tool_card?.name === "browser_open" && tool_card.browserTarget?.tabId === originalTab
  )?.tool_card;
  assert(openCard, "The completed browser card has a persisted target");
  assert.equal(openCard.browserTarget.tabId, originalTab);
  assert.equal(current.targetIdentity, `rust:${openCard.browserTarget.browserId}:${originalTab}`);
  await browserPost("lease", { action: "take" });
  const tabControl = (await state()).controlGeneration;
  await browserPost("action", { action: "tab_new", controlGeneration: tabControl });
  const closedTab = (await state()).tabs.find(tab => tab.active).tabHandle;
  assert.notEqual(closedTab, originalTab);
  assert((await fetch(profileEndpoint + "/json/close/" + closedTab)).ok);
  const recovered = await until(async () => {
    const current = await state();
    return current.tabs.some(tab => tab.active && tab.tabHandle === originalTab) && current;
  });
  assert.equal(recovered.tabs.find(tab => tab.active).tabHandle, originalTab);
  assert.equal(recovered.url, fixture + "/form");
  await browserPost("lease", { action: "return", controlGeneration: tabControl });
  let form = new FormData();
  form.append("file", new Blob(["resume-fixture-bytes"]), "resume.txt");
  const uploadedResponse = await fetch(`${base}/api/sessions/${chat}/upload`, {
    method: "POST",
    headers: { Authorization: `Bearer ${token}` },
    body: form,
  });
  assert.equal(uploadedResponse.status, 200);
  const upload = await uploadedResponse.json();
  assert.deepEqual(JSON.parse((await turn("workspace inventory", "chat-inventory")).message).chat_files, [upload.path]);
  const download = await fetch(
    `${base}/api/sessions/${chat}/files/${upload.path}`,
    { headers: { Authorization: `Bearer ${token}` } },
  );
  assert.equal(await download.text(), "resume-fixture-bytes");
  assert(
    (await ok(`/api/sessions/${chat}/artifacts`)).artifacts.some(
      (item) => item.path === upload.path,
    ),
  );
  assert.equal((await ok(`/api/sessions/${chat}/ui-panel`)).enabled, false);
  const truth = await ok(`/api/sessions/${chat}/state`);
  assert.equal(truth.schema_version, 1);
  assert.equal(truth.workspace.schema_version, 1);
  assert.equal(truth.turn.actual_target, "local");
  assert(Number.isFinite(truth.observed_at));
  const second = (await ok("/api/sessions", {})).session_id;
  assert.equal((await ok(`/api/permissions?session_id=${second}`)).profile, "full");
  await ok("/api/permissions", { session_id: second, profile: "ask" });
  assert.deepEqual(JSON.parse((await ok("/api/chat/send", {
    session_id: second, client_request_id: "empty-inventory", message: "workspace inventory",
  })).message).chat_files, []);
  assert.equal(
    (
      await api("/api/chat/send", {
        session_id: second,
        client_request_id: "foreign-upload",
        message: "read",
        attachments: [upload.path],
      })
    ).status,
    400,
  );
  assert.equal((await api(`/api/sessions/${chat}/upload`, {})).status, 400);
  assert.equal((await ok(`/api/permissions?session_id=${chat}`)).profile, "full");
  await ok("/api/permissions", { session_id: chat, profile: "ask" });
  const filled = turn("fill approved", "fill");
  let approval = await pending();
  assert.equal(approval.action, "browser_fill");
  // Hold approval beyond the browser's observation TTL, then resolve it safely.
  await sleep(31000);
  await ok("/api/approvals", {
    approval_id: approval.approval_id,
    approved: true,
  });
  let finished = await filled;
  assert(
    !JSON.stringify(finished).includes("Tool failed:"),
    JSON.stringify(finished),
  );
  assert.equal(
    await pageValue('document.querySelector("#email").value'),
    "native@example.test",
  );
  for(const action of ['press','select','check']){
    const work=turn(`${action} approved`,action);
    approval=await pending();assert.equal(approval.action,`browser_${action}`);
    await ok('/api/approvals',{approval_id:approval.approval_id,approved:true});
    const result=await work;assert(!JSON.stringify(result).includes('Tool failed:'),JSON.stringify(result));
  }
  assert.equal(await pageValue('document.querySelector("#email").value'),'native@example.test.');
  assert.deepEqual(await pageValue('Array.from(document.querySelector("#departments").selectedOptions,o=>o.value)'),['engineering','research']);
  assert.equal(await pageValue('document.querySelector("#no").checked'),true);
  const framed=turn('fill frame approved','frame-fill');
  approval=await pending();assert.equal(approval.action,'browser_fill');
  await ok('/api/approvals',{approval_id:approval.approval_id,approved:true});
  assert(!JSON.stringify(await framed).includes('Tool failed:'));
  assert.equal(await pageValue('document.querySelector("iframe").contentDocument.querySelector("input").value'),'native@example.test');
  const attached = turn(`upload approved ${upload.path}`, "upload");
  approval = await pending();
  assert(approval.detail.includes("SHA-256"));
  await ok("/api/approvals", {
    approval_id: approval.approval_id,
    approved: true,
    scope: "once",
  });
  finished = await attached;
  assert(
    !JSON.stringify(finished).includes("Tool failed:"),
    JSON.stringify(finished),
  );
  assert.equal(
    await pageValue('document.querySelector("#file").files.length'),
    1,
  );
  assert.equal(
    await pageValue('document.querySelector("#file").files[0].size'),
    20,
  );
  assert.equal(await pageValue('document.querySelector("#file").files[0].name'),"resume.txt","The browser must receive the original approved filename");
  const changed = turn(`upload changed ${upload.path}`, "changed");
  approval = await pending();
  fs.writeFileSync(
    path.join(root, "state/sessions", chat, upload.path),
    "changed-file-bytes",
  );
  await ok("/api/approvals", {
    approval_id: approval.approval_id,
    approved: true,
  });
  assert(JSON.stringify(await changed).includes("upload bytes changed"));
  const denied = turn("fill denied", "denied");
  approval = await pending();
  await ok("/api/approvals", {
    approval_id: approval.approval_id,
    approved: false,
  });
  assert(JSON.stringify(await denied).includes("denied"));
  const cancelled = turn("fill cancel", "cancel");
  approval = await pending();
  await ok("/api/chat/cancel", { session_id: chat, turn_id: approval.turn_id });
  await cancelled;
  assert.equal((await ok(`/api/sessions/${chat}`)).pending_approval, null);
  assert.equal(
    (
      await ok("/api/approvals", {
        approval_id: approval.approval_id,
        approved: true,
      })
    ).expired,
    true,
  );
  const switched = turn("fill switch", "switch");
  approval = await pending();
  await browserPost("promote", { target: "profile:private" });
  await ok("/api/approvals", {
    approval_id: approval.approval_id,
    approved: true,
  });
  await switched;
  const switchedEvents = (await ok(`/api/sessions/${chat}/events`)).events;
  assert(switchedEvents.some(event => event.tool_card?.name === "browser_fill" &&
    JSON.stringify(event.tool_card).includes("Browser selection changed")), "Stale input must be refused after a panel switch");
  const granting = turn("fill grant", "grant");
  approval = await pending();
  const scopedGrant = await ok("/api/approvals", {
    approval_id: approval.approval_id,
    approved: true,
    scope: "session",
  });
  await granting;
  assert.equal(scopedGrant.scope, "once");
  assert.equal((await ok(`/api/permissions?session_id=${chat}`)).profile, "ask");
  const stillAsks = turn("fill automatic", "automatic");
  approval = await pending();
  await ok("/api/approvals", { approval_id: approval.approval_id, approved: true });
  assert(!JSON.stringify(await stillAsks).includes("Tool failed:"));
  assert.equal((await ok(`/api/sessions/${chat}`)).pending_approval, null);
  fs.writeFileSync(
    path.join(root, "state/sessions", chat, upload.path),
    "resume-fixture-bytes",
  );
  const uploadWithGrant = turn(
    `upload still asks ${upload.path}`,
    "upload-with-grant",
  );
  approval = await pending();
  await ok("/api/approvals", {
    approval_id: approval.approval_id,
    approved: false,
  });
  await uploadWithGrant;
  await ok("/api/permissions", { mode: "full" });
  assert.equal((await ok("/api/permissions")).scope, "full");
  await ok("/api/permissions", { mode: "ask" });

  await ok("/api/permissions",{session_id:chat,profile:"auto_review"});
  assert.equal((await ok(`/api/permissions?session_id=${chat}`)).profile,"auto_review");
  assert(!JSON.stringify(await turn("fill reviewed","review-allow")).includes("Tool failed:"));
  assert.equal(reviewCalls,1);
  assert.equal((await ok(`/api/permissions?session_id=${second}`)).profile,"ask");
  reviewVerdict="deny";
  assert(JSON.stringify(await turn("fill denied by reviewer","review-deny")).includes("Automatic approval review denied"));
  const reviewsState=await ok(`/api/permissions?session_id=${chat}`);
  const reviewedDenial=reviewsState.denials.find(d=>d.record.request.action==="browser_fill").record;
  assert.equal((await api("/api/review-overrides",{approval_id:reviewedDenial.id,authority:reviewedDenial.request.authority,expected_revision:reviewedDenial.revision+1})).status,409);
  assert.equal((await ok("/api/review-overrides",{approval_id:reviewedDenial.id,authority:reviewedDenial.request.authority,expected_revision:reviewedDenial.revision})).input_dispatched,false);

  reviewVerdict="invalid";
  const uncertain=turn("fill manual fallback","review-invalid");
  approval=await pending();
  await ok("/api/approvals",{approval_id:approval.approval_id,approved:false});
  await uncertain;
  assert.equal((await api("/api/permissions",{session_id:chat,profile:"full"})).status,400);
  await ok("/api/permissions",{session_id:chat,profile:"full",confirm_full:true});
  const countReviews=reviewCalls;
  assert(!JSON.stringify(await turn("fill full access","profile-full")).includes("Tool failed:"));
  assert.equal(reviewCalls,countReviews);
  await browserPost("lease", {action:"take"});
  await browserPost("action", {
    action:"navigate", url:fixture + "/application", controlGeneration:(await state()).controlGeneration,
  });
  await until(async () => (await state()).tabs.some(tab => tab.url?.endsWith("/application")));
  await browserPost("lease", {action:"return", controlGeneration:(await state()).controlGeneration});
  const submitIntent = "Submit the disposable application";
  const submitRequest = "submit-crash-replay";
  const original = turn(submitIntent, submitRequest).then(
    () => "completed", () => "disconnected",
  );
  await until(() => submissions > 0);
  assert.equal(submissions, 1);
  const hostExited = new Promise(resolve => host.once("exit", resolve));
  host.kill("SIGKILL");
  await hostExited;
  assert.equal(await original, "disconnected");
  releaseSubmission();
  await startHost();
  const recoveredSubmission = await until(async () => {
    const result = await ok(`/api/sessions/${chat}/requests/${submitRequest}`);
    return result.receipt.state !== "accepted" && result;
  }, 60000);
  assert(["interrupted", "finished"].includes(recoveredSubmission.receipt.state));
  const replayed = await turn(submitIntent, submitRequest);
  assert.equal(replayed.turn_id, recoveredSubmission.receipt.turn_id);
  await until(() => renderedConfirmation);
  const originalEvents = async () => (await ok(`/api/sessions/${chat}/events?turn_id=${recoveredSubmission.receipt.turn_id}`)).events;
  const pendingSubmission = (await originalEvents()).find(event =>
    event.kind === "outcome_snapshot" && event.payload?.outcomes?.[0]?.status === "unresolved");
  assert(pendingSubmission, "An unverified submission state survives the crash");
  await assert.rejects(
    ok("/api/chat/send", {
      session_id: chat, client_request_id: "unsafe-submit-resume", message: "Resume task",
      resume_turn_id: recoveredSubmission.receipt.turn_id,
    }),
    /original.*receipt/i,
  );
  assert.equal(submissions, 1, "Resume must not repeat an unresolved submission");
  const receiptId = pendingSubmission.payload.outcomes[0].id.replace("application-", "");
  const recovery = await turn(`Check submission receipt ${receiptId}`, "read-submission-receipt");
  const recoveredReceipt = JSON.parse(recovery.message);
  assert.equal(recoveredReceipt.receipt.request_id, receiptId);
  assert.equal(recoveredReceipt.receipt.action, "click");
  assert.equal(recoveredReceipt.receipt.state, "verified");
  assert.equal(recoveredReceipt.submission_evidence.verified_reference, `receipt:${receiptId}`);
  assert((await originalEvents()).some(event =>
    event.kind === "outcome_snapshot" && event.payload?.outcomes?.[0]?.label === "Submitted"));
  assert.equal(submissions, 1, "Replay must not send a second application POST");

  // A post-action hook runs only after the browser confirms the click. Its denial
  // must not discard the original receipt or the linked outcome evidence.
  await browserPost("lease", {action:"take"});
  await browserPost("action", {
    action:"navigate", url:fixture + "/application", controlGeneration:(await state()).controlGeneration,
  });
  await until(async () => (await state()).tabs.some(tab => tab.url?.endsWith("/application")));
  await browserPost("lease", {action:"return", controlGeneration:(await state()).controlGeneration});
  const hookDir = path.join(root, "workspace", ".kageha");
  fs.mkdirSync(hookDir, {recursive:true});
  fs.writeFileSync(path.join(hookDir, "hooks.json"), JSON.stringify({
    hooks:{postToolUse:[{matcher:"browser_click",deny:"fixture post-action denial"}]},
  }));
  const hookedRequest = "submit-post-hook-denial";
  const hooked = await turn(submitIntent, hookedRequest);
  assert.equal(submissions, 2, "The hooked submission must send exactly one new POST");
  const hookedStatus = await ok(`/api/sessions/${chat}/requests/${hookedRequest}`);
  const hookedEvents = (await ok(`/api/sessions/${chat}/events?turn_id=${hookedStatus.receipt.turn_id}`)).events;
  const hookedEvidence = hookedEvents.find(event => event.kind === "submission_evidence"
    && event.payload?.outcome_id === "application-submitted"
    && event.payload?.result?.verified_reference?.startsWith("receipt:"));
  assert(hookedEvidence, `The verified receipt must attach before the failing post-action hook: ${JSON.stringify(hookedEvents)}`);
  const hookedReceiptId = hookedEvidence.payload.request_id;
  assert.equal(hookedEvidence.payload.result.verified_reference, `receipt:${hookedReceiptId}`);
  assert(JSON.stringify(hooked).includes("fixture post-action denial"),
    `The fixture must actually run the denying hook: ${JSON.stringify(hooked)}`);
  assert.equal(submissions, 2, "The failing post-action hook must not repeat the click");
  await ok("/api/permissions",{session_id:chat,profile:"ask"});

  console.log(
    JSON.stringify({
      status: "passed",
      base,
      chat,
      root,
      checks: [
        "private navigation",
        "layout and JPEG preview",
        "continuous preview across repeated daemon connection rollover",
        "paint acknowledgment requires the current binding and supports withdrawal",
        "external tab close restores the sole remaining managed tab",
        "Chrome/Comet profile switching",
        "one-time agent-requested personal attachment to a disposable profile",
        "security challenge reported on a login URL",
        "mid-turn panel selection supersedes an in-flight model answer",
        "consent",
        "binding fencing",
        "chat-owned multipart uploads",
        "exact upload approval",
        "fill verified via CDP",
        "focused punctuation key through agent tool",
        "multi-select and radio through approved agent tools",
        "frame-scoped fill through agent tool",
        "deny",
        "cancel",
        "stale approval",
        "selection change during approval",
        "no Python on backend PATH",
        "chat-scoped permission profiles",
        "real reviewer allow/deny/invalid fallback",
        "full access confirmation and browser fill",
        "server counted exactly one application submission after host crash and request replay",
        "original verified click receipt attached to the continued submission outcome",
        "verified submission evidence survives a denying post-action hook",
      ],
    }),
  );
  if (process.argv.includes("--keep")) {
    console.log(`UI: ${base}/login token: ${token}`);
    await new Promise((resolve) => process.once("SIGTERM", resolve));
  }
}
try {
  await run();
} catch (error) {
  console.error(error);
  for (const file of fs
    .readdirSync(root)
    .filter((file) => file.endsWith(".err")))
    console.error(
      file,
      fs.readFileSync(path.join(root, file), "utf8").slice(-2000),
    );
  throw error;
} finally {
  releaseSubmission?.();
  for (const child of children.reverse()) {
    child.kill("SIGTERM");
    await Promise.race([
      new Promise((resolve) => child.once("exit", resolve)),
      sleep(2000),
    ]);
  }
  server.closeAllConnections();
  await new Promise((resolve) => server.close(resolve));
  if (!process.argv.includes("--keep"))
    fs.rmSync(root, {
      recursive: true,
      force: true,
      maxRetries: 5,
      retryDelay: 200,
    });
}
