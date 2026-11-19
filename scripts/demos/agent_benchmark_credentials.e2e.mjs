// Run with KAGEHA_LOOP_BIN=/absolute/path/to/kageha-loop node --test ...
// Uses only a disposable vault, key and server; no provider requests are made.
import { test } from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { prepareVaultSnapshot } from "./agent_benchmark_credentials.mjs";

test("native server resolves a copied synthetic encrypted vault", {skip: !process.env.KAGEHA_LOOP_BIN, timeout: 30000}, async () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "kageha-benchmark-e2e-"));
  const workspace = path.join(root,"workspace"), source = path.join(root,"source"), snapshot = path.join(root,"snapshot");
  for (const dir of [workspace, source, snapshot]) fs.mkdirSync(dir,{mode:0o700});
  const key = path.join(root,"key"); fs.writeFileSync(key,crypto.randomBytes(32),{mode:0o600});
  const token = crypto.randomBytes(32).toString("hex");
  const children = [];
  async function start(state) {
    const child = spawn(process.env.KAGEHA_LOOP_BIN,["serve",workspace,state,"127.0.0.1:0"],{
      env:{HOME:root, PATH:"", KAGEHA_HOME:path.join(state,"home"), KAGEHA_SERVER_TOKEN:token, KAGEHA_CREDENTIAL_KEY_FILE:key, KAGEHA_LEGACY_SESSIONS_DIR:path.join(root,"no-legacy")},
      stdio:["ignore","ignore","pipe"]});
    children.push(child);
    const base = await new Promise((resolve, reject) => {
      let output="";
      const timer=setTimeout(()=>reject(Error("Fixture server did not start")),10000);
      child.stderr.on("data", data => {output+=data; const match=output.match(/listening on ([^\s]+)/); if(match){clearTimeout(timer);resolve(`http://${match[1]}`)}});
      child.on("exit", code => {clearTimeout(timer);reject(Error(`Fixture server exited ${code}`))});
    });
    return async (route, body) => {
      const response=await fetch(base+route,{method:body?"POST":"GET",headers:{Authorization:`Bearer ${token}`,"Content-Type":"application/json"},body:body?JSON.stringify(body):undefined});
      const value=await response.json(); if(!response.ok) throw Error(`Fixture API HTTP ${response.status}`); return value;
    };
  }
  try {
    const api = await start(source);
    await api("/api/provider-setup/model",{provider_name:"fixture",model_id:"chosen",model:"test-model",base_url:"https://fixture.invalid/v1",api_key:"disposable-synthetic-token"});
    await prepareVaultSnapshot(path.join(source,"native-chats.sqlite"),"chosen",snapshot);
    const cloned = await start(snapshot);
    const models = await cloned("/api/models");
    assert.equal(models.default_model,"chosen");
    assert.equal(models.all.find(model=>model.id==="chosen")?.ready,true);
    assert(!JSON.stringify(models).includes("disposable-synthetic-token"));
  } finally {
    for (const child of children) if (child.exitCode===null) child.kill("SIGTERM");
    await Promise.all(children.map(child => child.exitCode===null ? new Promise(resolve=>child.once("exit",resolve)) : undefined));
    fs.rmSync(root,{recursive:true,force:true});
  }
});

test("interrupting the benchmark stops its child and removes private trial state", {timeout: 20000}, async () => {
  const root=fs.mkdtempSync(path.join(os.tmpdir(),"kageha-benchmark-signal-"));
  const fake=path.join(root,"fake-server"), output=path.join(root,"results");
  fs.writeFileSync(fake,`#!${process.execPath}\nimport('node:fs').then(fs=>{fs.writeFileSync(process.argv[4]+'/started',String(process.pid));process.on('SIGTERM',()=>process.exit(0));setInterval(()=>{},1000)});\n`,{mode:0o700});
  const child=spawn(process.execPath,[new URL("./agent_speed_benchmark.mjs",import.meta.url).pathname,"--output",output,"--rounds","1","--binary",fake],{
    env:{...process.env,HOME:root,TMPDIR:root,KAGEHA_PROVIDER_URL:"https://fixture.invalid/v1",KAGEHA_MODEL:"fixture",KAGEHA_API_KEY:"synthetic"},stdio:"ignore"});
  try {
    let state;
    for(let i=0;i<200;i++) {
      state=fs.readdirSync(root).find(name=>name.startsWith("kageha-real-review-state-"));
      if(state && fs.existsSync(path.join(root,state,"started"))) break;
      await new Promise(resolve=>setTimeout(resolve,25));
    }
    assert(state && fs.existsSync(path.join(root,state,"started")),"fixture server did not start");
    const serverPid=Number(fs.readFileSync(path.join(root,state,"started"),"utf8"));
    const exited=new Promise(resolve=>child.once("exit",resolve));
    child.kill("SIGINT");
    await exited;
    assert.equal(fs.existsSync(path.join(root,state)),false);
    assert.throws(()=>process.kill(serverPid,0),{code:"ESRCH"});
    const summary=JSON.parse(fs.readFileSync(path.join(output,"summary.json"),"utf8"));
    assert.equal(summary.stop_reason,"Benchmark interrupted.");
  } finally {
    if(child.exitCode===null && child.signalCode===null) child.kill("SIGKILL");
    fs.rmSync(root,{recursive:true,force:true});
  }
});
