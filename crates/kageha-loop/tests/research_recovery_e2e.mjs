// Disposable public-source recovery fixture. No external model or website is contacted.
// KAGEHA_LOOP_BIN=... KAGEHA_BROWSER_BIN=... node research_recovery_e2e.mjs
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import http from "node:http";
import https from "node:https";
import { spawn, execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { DatabaseSync } from "node:sqlite";
import { credentialFixture } from "./credential_fixture.mjs";
import { isolatedModelFixture } from "./isolated_model_fixture.mjs";

const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../..");
const root = fs.mkdtempSync("/tmp/kageha-research-e2e-");
const binary = process.env.KAGEHA_LOOP_BIN || path.join(repo, "target/debug/kageha-loop");
const browser = process.env.KAGEHA_BROWSER_BIN || path.join(repo, "target/debug/kageha-browser");
const chrome = process.env.KAGEHA_CHROMIUM || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const children = [];
const token = "disposable-research-recovery-fixture-token";
const curlCalls = path.join(root, "curl-calls");
const quote = (value) => "'" + value.replaceAll("'", "'\"'\"'") + "'";
let controlChat;
let base, url, sourceId, sourceRequests = 0, providerRequests = 0;
const sourceText = "é".repeat(45000) + "🌲needle café終";
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
async function until(check, timeout = 20000) {
  for (const end = Date.now() + timeout; Date.now() < end; ) {
    const value = await check(); if (value) return value;
    await sleep(50);
  }
  throw Error("Fixture timed out; logs at " + root);
}
function start(bin, args, env, name) {
  const log = path.join(root, name + ".log");
  const child = spawn(bin, args, { env, stdio: ["ignore", fs.openSync(log, "a"), fs.openSync(log, "a")] });
  children.push(child); return { child, log };
}
async function api(route, body) {
  const response = await fetch(base + route, {signal:AbortSignal.timeout(60000),method:body ? "POST" : "GET",
    headers:{Authorization:`Bearer ${token}`,"Content-Type":"application/json"},
    body:body ? JSON.stringify(body) : undefined});
  const value = await response.json(); assert.equal(response.status, 200, JSON.stringify(value)); return value;
}
execFileSync("/usr/bin/openssl", ["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1", "-subj", "/CN=localhost",
  "-keyout", path.join(root,"key.pem"), "-out", path.join(root,"cert.pem")], {stdio:"ignore"});
// The oversized adapter deterministically exercises the exact screenshot error.
const curl = path.join(root, "oversized-curl");
fs.writeFileSync(curl, `#!/bin/sh\necho attempt >> ${quote(curlCalls)}\n/usr/bin/head -c 10616833 /dev/zero\n`, {mode:0o700});
// Only this disposable Chromium fixture accepts this disposable TLS certificate.
const chromium = path.join(root, "fixture-chromium");
fs.writeFileSync(chromium, `#!/bin/sh\nexec ${quote(chrome)} --ignore-certificate-errors "$@"\n`, {mode:0o700});
const source = https.createServer({key:fs.readFileSync(path.join(root,"key.pem")),cert:fs.readFileSync(path.join(root,"cert.pem"))}, (req,res) => {
  if(req.url === "/form") {res.setHeader("Content-Type","text/html");res.end('<!doctype html><title>Validation fixture</title><input id="email" type="email" required value="test@example.com"><input id="secret" type="password" value="do-not-expose">');return;}
  if(req.url !== "/source") {res.writeHead(404);res.end();return;}
  sourceRequests++;
  res.setHeader("Content-Type","text/html; charset=utf-8");
  res.end(`<!doctype html><title>Unicode source</title><body><p>${sourceText}</p></body>`);
});
const model = http.createServer(async (req,res) => {
  try {
    let bytes="";for await(const part of req) bytes+=part;
    const input=JSON.parse(bytes);
    providerRequests++;
    const user=input.messages.filter(message=>message.role==="user").at(-1)?.content || "";
    const results=input.messages.filter(message=>message.role==="tool");
    let call, answer;
    if(user.includes("Control fixture")) {
      if(!results.length) call={name:"browser_open",arguments:{url:url.replace("/source","/form")}};
      else if(results.length===1) call={name:"record_required_outcomes",arguments:{outcomes:[{id:"input",kind:"action",description:"Perform the requested browser input with verified evidence"}]}};
      else if(results.length===2) {
        const browserState=await api(`/api/sessions/${controlChat}/browser`);
        await api(`/api/sessions/${controlChat}/browser/lease`,{action:"take",bindingId:browserState.bindingId});
        call={name:"browser_click",arguments:{target:"unused-preflight",observation_id:"unused-preflight"}};
      } else {
        assert(results.at(-1).content.includes("browser_user_control"));
        assert.equal(input.tools?.length || 0,0,"Human handoff must remove tools on the next model decision");
        answer="Unverified input remains blocked.";
      }
    } else if(user.includes("Validate fixture")) {
      if(!results.length) call={name:"browser_open",arguments:{url:url.replace("/source","/form")}};
      else if(results.length===1) call={name:"record_required_outcomes",arguments:{outcomes:[{id:"fields",kind:"validation",description:"Check the explicitly requested email and native validity"}]}};
      else if(results.length===2) call={name:"browser_validate",arguments:{assertions:[{selector:"#email",property:"value",expected:"test@example.com"},{selector:"#email",property:"validity.valid",expected:true}]}};
      else if(results.length===3) {
        const checked=JSON.parse(results.at(-1).content);assert.equal(checked.validation.passed,true);assert.equal(checked.validation.checks.length,2);
        assert(!JSON.stringify(checked).includes("do-not-expose"));
        call={name:"record_outcome_evidence",arguments:{outcome_id:"fields",reference:"read-2"}};
      } else {assert.equal(JSON.parse(results.at(-1).content).verified_reference,"read-2");answer="Explicit browser field checks verified.";}
    } else if(user.includes("Cross-scope")) {
      if(!results.length) call={name:"web_fetch",arguments:{source_id:sourceId}};
      else { assert(results.at(-1).content.includes("research_source_expired")); answer="Expected source scope rejection verified."; }
    } else if(!results.length) call={name:"web_fetch",arguments:{url}};
    else {
      const result=JSON.parse(results.at(-1).content);
      assert.equal(result.ok,true,JSON.stringify(result));
      assert(!Object.hasOwn(result,"source_text"),"Full source text leaked into model preview");
      if(results.length===1) {
        sourceId=result.source_id; assert(sourceId);
        assert.equal(result.recovery.recovered,true);
        assert.equal(result.recovery.reason,"research_response_too_large");
        assert.equal(Array.from(result.text).length,8000);
        call={name:"web_fetch",arguments:{source_id:sourceId,offset:45000}};
      } else if(results.length===2) {
        assert.equal(result.text,"🌲needle café終");
        call={name:"web_fetch",arguments:{source_id:sourceId,search_phrase:"needle"}};
      } else if(results.length===3) {
        assert.equal(result.match_offset,45001);assert.equal(result.text,"needle café終");
        call={name:"web_fetch",arguments:{url}};
      } else {assert.equal(result.source_id,sourceId);answer="Source recovered and selective reading verified.";}
    }
    res.setHeader("Content-Type","application/json");
    res.end(JSON.stringify({choices:[{message:call ? {role:"assistant",content:null,tool_calls:[{id:`read-${results.length}`,type:"function",function:{name:call.name,arguments:JSON.stringify(call.arguments)}}]} : {role:"assistant",content:answer}}]}));
  } catch(error) {console.error(error);res.writeHead(500);res.end(JSON.stringify({error:String(error.stack||error)}));}
});
try {
  await new Promise(resolve=>source.listen(0,"127.0.0.1",resolve));
  await new Promise(resolve=>model.listen(0,"127.0.0.1",resolve));
  url=`https://127.0.0.1:${source.address().port}/source`;
  const daemonRoot=path.join(root,"browser");fs.mkdirSync(daemonRoot,{mode:0o700});
  start(browser,["--state-dir",daemonRoot,"--instance-id","research-fixture","--application-id","fixture","--standalone","on","--chromium",chromium,"--curl",curl],{PATH:"/usr/bin:/bin"},"browser");
  await until(()=>fs.existsSync(path.join(daemonRoot,"daemon.json")));
  fs.mkdirSync(path.join(root,"workspace"));
  const runtimeEnv = {};
  if (process.env.KAGEHA_RESEARCH_BRIDGE === "1") {
    const runtimeToken = path.join(root,"runtime.token"); fs.writeFileSync(runtimeToken,token,{mode:0o600});
    const runtime=start(process.env.KAGEHA_RUNTIME_BIN || path.join(repo,"target/debug/kageha-runtime-service"),
      ["bridge","--state",path.join(root,"runtime"),"--token-file",runtimeToken,"--bind","127.0.0.1:0"],{PATH:"/usr/bin:/bin"},"runtime");
    runtimeEnv.KAGEHA_RUNTIME_SERVICE_URL=await until(()=>{
      if(runtime.child.exitCode!==null)throw Error(fs.readFileSync(runtime.log,"utf8"));
      const address=fs.readFileSync(runtime.log,"utf8").match(/runtime bridge listening on ([^\s]+)/)?.[1];return address && `http://${address}`;
    });
    runtimeEnv.KAGEHA_RUNTIME_SERVICE_TOKEN_FILE=runtimeToken;
  }
  const host=start(binary,["serve",path.join(root,"workspace"),path.join(root,"state"),"127.0.0.1:0"], {
    PATH:"/usr/bin:/bin",...credentialFixture(root),...isolatedModelFixture(root),
    ...runtimeEnv,
    KAGEHA_SERVER_TOKEN:token,KAGEHA_BROWSER_STATE_DIR:daemonRoot,KAGEHA_MODEL:"fixture",
    KAGEHA_PROVIDER_URL:`http://127.0.0.1:${model.address().port}/v1/chat/completions`,
  },"host");
  base=await until(()=>{
    if(host.child.exitCode!==null)throw Error(fs.readFileSync(host.log,"utf8"));
    const address=fs.readFileSync(host.log,"utf8").match(/listening on ([^\s]+)/)?.[1];return address && `http://${address}`;
  });
  const policy=(await api("/api/settings/persistence")).policy;
  assert.equal(policy.max_model_calls,5000);assert.equal(policy.max_duration_seconds,86400);
  const chat=(await api("/api/sessions",{})).session_id;
  const result=await api("/api/chat/send",{session_id:chat,client_request_id:"recover-source",message:"Read the public source, inspect the later characters, and find needle."});
  assert.equal(result.message,"Source recovered and selective reading verified.");
  assert.equal(sourceRequests,1,"Selective reads downloaded the source again");
  assert.equal(fs.readFileSync(curlCalls,"utf8").trim().split("\n").length,1,"HTTP adapter repeated");
  const events=(await api(`/api/sessions/${chat}/events`)).events;
  assert(events.some(event=>event.kind==="status" && event.payload?.detail==="Trying browser extraction"));
  assert(events.some(event=>event.tool_card?.researchRecovery?.recovered===true),"Successful fallback was not represented as recovered");
  const journal = new DatabaseSync(path.join(root,"state/native-chats.sqlite"),{readOnly:true});
  try {
    const budgets=journal.prepare("SELECT used_model_calls,max_model_calls,max_duration_ms FROM ui_request_budgets").all();
    assert.equal(budgets.length,1);assert.equal(budgets[0].used_model_calls,providerRequests,"Actual provider calls were charged incorrectly");
    assert.equal(budgets[0].max_model_calls,5000);assert.equal(budgets[0].max_duration_ms,86400000);
  } finally {journal.close();}
  const other=(await api("/api/sessions",{})).session_id;
  assert.equal((await api("/api/chat/send",{session_id:other,client_request_id:"reject-cross-scope",message:"Cross-scope retained source check."})).message,"Expected source scope rejection verified.");
  assert.equal(sourceRequests,1);assert.equal(fs.readFileSync(curlCalls,"utf8").trim().split("\n").length,1);
  const formChat=(await api("/api/sessions",{})).session_id;
  assert.equal((await api("/api/chat/send",{session_id:formChat,client_request_id:"validate-fields",message:"Validate fixture email equals test@example.com and its native validity is true. Read only."})).message,"Explicit browser field checks verified.");
  controlChat=(await api("/api/sessions",{})).session_id;
  const blocked=await api("/api/chat/send",{session_id:controlChat,client_request_id:"human-control",message:"Control fixture: exercise the user handoff on a disposable page."});
  assert(blocked.message.includes("Return control to the agent"),JSON.stringify(blocked));
  assert(!blocked.message.includes("Continue actionable work"));
  const controlEvents=(await api(`/api/sessions/${controlChat}/events`)).events;
  const blockedCards=controlEvents.filter(event=>event.tool_card?.name==="browser_click" && event.tool_card?.status==="failed");
  assert.equal(blockedCards.length,1,"Browser input retried after handoff");
  assert(blockedCards[0].tool_card.result_preview.includes("not_dispatched"));
  console.log(JSON.stringify({ok:true,bridge:process.env.KAGEHA_RESEARCH_BRIDGE==="1",sourceRequests,httpAttempts:1,selectiveReads:3,scopeRejected:true,providerRequests,fixture:root}));
} finally {
  for(const child of children.reverse())if(child.exitCode===null)child.kill("SIGTERM");
  source.closeAllConnections();model.closeAllConnections();
  await Promise.all([new Promise(resolve=>source.close(resolve)),new Promise(resolve=>model.close(resolve))]);
}
