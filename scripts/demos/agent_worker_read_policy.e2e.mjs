// Disposable native server and mock model. No external requests or credentials.
import { test } from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import { DatabaseSync } from "node:sqlite";
import { analyzeTimings } from "./agent_speed_benchmark_lib.mjs";

test("benchmark worker inherits read_file and reads its project fixture", {skip: !process.env.KAGEHA_LOOP_BIN, timeout:30000}, async () => {
  const root=fs.mkdtempSync(path.join(os.tmpdir(),"kageha-benchmark-read-"));
  const workspace=path.join(root,"workspace"), state=path.join(root,"state");
  fs.mkdirSync(path.join(workspace,"fixtures"),{recursive:true,mode:0o700}); fs.mkdirSync(state,{mode:0o700});
  fs.writeFileSync(path.join(workspace,"fixtures/task-1.txt"),"KAGEHA_READ_FIXTURE_TOKEN\n");
  const key=path.join(root,"key"); fs.writeFileSync(key,crypto.randomBytes(32),{mode:0o600});
  const token=crypto.randomBytes(32).toString("hex");
  let workerHadRead=false, workerReadResult=false, parentRead=false;
  const model=http.createServer(async (req,res)=>{
    let bytes=""; for await (const part of req) bytes+=part;
    const data=JSON.parse(bytes), messages=data.messages, intent=messages.filter(m=>m.role==="user").at(-1)?.content || "";
    const toolResults=messages.filter(m=>m.role==="tool");
    const call=(id,name,args)=>({role:"assistant",content:null,tool_calls:[{id,type:"function",function:{name,arguments:JSON.stringify(args)}}]});
    let message;
    if (intent==="benchmark-policy-parent") {
      parentRead ||= messages.some(m=>m.role==="assistant" && m.tool_calls?.some(t=>t.function?.name==="read_file"));
      if(!toolResults.length) message=call("spawn","agent_spawn",{agent_id:"benchmark-worker",objective:"benchmark-policy-worker"});
      else if(toolResults.length===1) message=call("wait","agent_wait_any",{job_ids:[JSON.parse(toolResults[0].content).id],timeout_ms:30000});
      else message={role:"assistant",content:"KAGEHA_READ_FIXTURE_TOKEN"};
    } else if (intent==="benchmark-policy-worker") {
      workerHadRead=data.tools.some(t=>t.function?.name==="read_file");
      if(!toolResults.length) message=call("read","read_file",{path:"fixtures/task-1.txt"});
      else {workerReadResult=String(toolResults.at(-1).content).includes("KAGEHA_READ_FIXTURE_TOKEN"); message={role:"assistant",content:"KAGEHA_READ_FIXTURE_TOKEN"};}
    } else message={role:"assistant",content:"unexpected request"};
    res.setHeader("Content-Type","application/json"); res.end(JSON.stringify({choices:[{message}]}));
  });
  await new Promise(resolve=>model.listen(0,"127.0.0.1",resolve));
  const child=spawn(process.env.KAGEHA_LOOP_BIN,["serve",workspace,state,"127.0.0.1:0"],{
    env:{HOME:root,PATH:"",KAGEHA_HOME:path.join(root,"home"),KAGEHA_LEGACY_SESSIONS_DIR:path.join(root,"no-legacy"),KAGEHA_SERVER_TOKEN:token,KAGEHA_CREDENTIAL_KEY_FILE:key,KAGEHA_BENCHMARK_TIMING:"1",KAGEHA_MODEL:"fixture",KAGEHA_PROVIDER_URL:`http://127.0.0.1:${model.address().port}/v1/chat/completions`},
    stdio:["ignore","ignore","pipe"]});
  try {
    const base=await new Promise((resolve,reject)=>{
      let output=""; const timer=setTimeout(()=>reject(Error("Fixture server did not start")),10000);
      child.stderr.on("data",data=>{output+=data;const match=output.match(/listening on ([^\s]+)/);if(match){clearTimeout(timer);resolve(`http://${match[1]}`)}});
      child.on("exit",code=>{clearTimeout(timer);reject(Error(`Fixture server exited ${code}`))});
    });
    const api=async (route,body)=>{
      const response=await fetch(base+route,{method:body?"POST":"GET",headers:{Authorization:`Bearer ${token}`,"Content-Type":"application/json"},body:body?JSON.stringify(body):undefined});
      const value=await response.json();assert.equal(response.status,200,JSON.stringify(value));return value;
    };
    await api("/api/agents",{id:"benchmark-worker",name:"Reader",system_prompt:"Read the file.",tool_policy:{allow:["read_file"],deny:["agent_spawn"]}});
    await api("/api/agents",{id:"benchmark-coordinator",name:"Coordinator",system_prompt:"Delegate the task.",tool_policy:{allow:["agent_spawn","agent_wait_any","agent_status","read_file"]}});
    const chat=(await api("/api/sessions",{persona_id:"benchmark-coordinator"})).session_id;
    const answer=await api("/api/chat/send",{session_id:chat,client_request_id:"policy-fixture",message:"benchmark-policy-parent",recipient_id:"benchmark-coordinator",max_steps:32});
    assert.equal(answer.status,"success"); assert(workerHadRead,"worker model did not receive read_file");
    assert(workerReadResult,"worker could not read the project fixture"); assert.equal(parentRead,false);
    const jobs=(await api(`/api/jobs?session_id=${chat}`)).jobs;
    assert.equal(jobs.length,1); assert.equal(jobs[0].state,"succeeded");
    const db = new DatabaseSync(path.join(state,"native-chats.sqlite"), {readOnly:true});
    try {
      const timing = db.prepare("SELECT payload FROM ui_benchmark_timing WHERE event='benchmark_model_timing' AND chat_id=? ORDER BY sequence").all(jobs[0].session_id).map(row=>JSON.parse(row.payload));
      assert(timing.length >= 2, "worker model calls should be timed");
      assert(timing.every(frame=>frame.kind === "model_timing" && frame.success === true && frame.elapsed_ms >= 0));
      assert(!JSON.stringify(timing).includes("KAGEHA_READ_FIXTURE_TOKEN"), "timing must not contain model content");
      assert.equal(db.prepare("SELECT COUNT(*) AS count FROM ui_events WHERE event LIKE 'benchmark_model_%'").get().count, 0, "diagnostics must not enter conversation events");
      const starts = db.prepare("SELECT payload FROM ui_benchmark_timing WHERE event='benchmark_model_start' AND chat_id=? ORDER BY sequence").all(jobs[0].session_id).map(row=>({chat_id:jobs[0].session_id,...JSON.parse(row.payload)}));
      const diagnostics = analyzeTimings(jobs, timing.map(frame=>({chat_id:jobs[0].session_id,...frame})), 10000, starts);
      assert(diagnostics.workers[0].model_adapter_ms !== null, "complete model step timings should be accounted for");
    } finally {db.close();}
  } finally {
    if(child.exitCode===null && child.signalCode===null) {const exited=new Promise(resolve=>child.once("exit",resolve)); child.kill("SIGTERM"); await exited;}
    await new Promise(resolve=>model.close(resolve)); fs.rmSync(root,{recursive:true,force:true});
  }
});
