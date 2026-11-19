import { strict as assert } from 'node:assert';
import { createServer } from 'node:http';
import { spawn } from 'node:child_process';
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs';
import { randomBytes } from 'node:crypto';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { setTimeout as sleep } from 'node:timers/promises';
import { DatabaseSync } from 'node:sqlite';
const root=mkdtempSync('/tmp/kageha-goal-mode-');
const workspace=join(root,'workspace');mkdirSync(workspace);
writeFileSync(join(workspace,'first.txt'),'First verified result.');
writeFileSync(join(workspace,'second.txt'),'Second verified result.');
writeFileSync(join(root,'key'),randomBytes(32),{mode:0o600});
writeFileSync(join(root,'models.yaml'),'models: []\nroles: {}\n');
const token=randomBytes(24).toString('hex');
const calls=[];
let base;
const binary = process.env.KAGEHA_LOOP_BIN || fileURLToPath(new URL('../../../target/debug/kageha-loop', import.meta.url));
function call(name, args, id) { return {content:null,tool_calls:[{id,type:'function',function:{name,arguments:JSON.stringify(args)}}]}; }
const provider=createServer(async (req,res)=>{
  const parts=[];for await(const part of req) parts.push(part);
  const body=JSON.parse(Buffer.concat(parts));
  const messages=body.messages;
  const last=messages.at(-1);
  const planning=messages.some(m=>String(m.content).includes('Plan this task without executing it'));
  const goal=messages.some(m=>String(m.content).includes('Pursue the durable goal'));
  const continuation=messages.some(m=>String(m.content).includes('Continue the active goal from the verified checkpoint'));
  const blocked=messages.some(m=>String(m.content).includes('Blocked goal fixture'));
  const prompt = messages.filter(message => message.role === "user").map(message => message.content).join("\n");
  const proposal = prompt.includes("Use a new scope.") ? null
    : prompt.includes("Suggest Plan fixture") ? "plan"
    : prompt.includes("Suggest Goal fixture") ? "goal"
    : prompt.includes("Extend fixture") || prompt.includes("Access blocker fixture") ? "extend"
    : null;
  const id=continuation?'second':'first';
  let reply;
  if(planning) reply={content:JSON.stringify({objective:'Review fixture',steps:[{id:'read',title:'Read first fixture',depends_on:[],acceptance_criteria:['Fixture read']}]})};
  else if(goal && messages.some(m=>String(m.content).includes('Pause fixture'))) { await sleep(1200); reply={content:'Paused fixture.'}; }
  else if (!goal && proposal) {
    if (prompt.includes("I accepted Extend execution budget")) {
      reply = { content: "The approved extra turn finished." };
    } else if (last.role === "tool" && last.tool_call_id === "proposal") {
      reply = String(last.content).includes('"status":"continuing"')
        ? call("read_file", {path: "second.txt"}, "continued-read")
        : {content: "The verified result and next step are saved."};
    } else if (last.tool_call_id === "continued-read") {
      reply = {content: "Verified both files in the original turn."};
    } else if (proposal === "extend" && last.role !== "tool") {
      reply = call("read_file", { path: "first.txt" }, "extension-evidence");
    } else if (prompt.includes("Access blocker fixture") && last.tool_call_id === "extension-evidence") {
      reply = call("integration_read", { integration: "gmail", operation: "status" }, "access");
    } else {
      reply = call("propose_run_change", {
        kind: proposal,
        reason: "Review the next step from the checkpoint.",
        next_action: "Verify the remaining fixture.",
        completion_condition: "Both fixture files have been verified.",
        evidence: proposal === "extend" ? ["extension-evidence"] : [],
      }, "proposal");
    }
  }
  else if (prompt.includes('Resume checkpoint fixture.')) {
    reply = prompt.includes('Continue from the saved checkpoint.')
      ? { content: messages.some(message => message.tool_call_id === 'resume-read') ? 'Checkpoint retained.' : 'Checkpoint lost.' }
      : last.role === 'tool' ? { content: 42 } : call('read_file', { path: 'first.txt' }, 'resume-read');
  }
  else if(!goal) reply={content:'Normal mode result.'};
  else if(last.role==='tool' && String(last.content).includes('Conclude this turn')) reply={content:blocked?'Waiting for the missing fixture.':continuation?'Both results are verified.':'First result verified. The second is next.'};
  else if(last.role==='tool') reply=call('goal_update',{status:continuation?'completed':'progress',reason:continuation?'Both fixture files verified.':'First file read. Read the second file next.',evidence:[id]},'decision-'+id);
  else if(blocked) reply=call('goal_update',{status:'blocked',reason:'The external prerequisite is unavailable.'},'decision-blocked');
  else reply=call('read_file',{path:id+'.txt'},id);
  calls.push({planning,goal,continuation,tool:reply.tool_calls?.[0]?.function.name});
  res.setHeader('Content-Type','application/json');
  res.end(JSON.stringify({choices:[{message:{role:'assistant',...reply},finish_reason:reply.tool_calls?'tool_calls':'stop'}]}));
});
await new Promise(resolve=>provider.listen(0,'127.0.0.1',resolve));
let child;
let startupLog = "";
function startBackend() {
  base = undefined;
  startupLog = "";
  child = spawn(binary, ["serve", workspace, join(root, "state"), "127.0.0.1:0"], {
    env: {
      HOME: process.env.HOME,
      PATH: process.env.PATH,
      KAGEHA_SERVER_TOKEN: token,
      KAGEHA_CREDENTIAL_KEY_FILE: join(root, "key"),
      KAGEHA_MODELS_FILE: join(root, "models.yaml"),
      KAGEHA_LEGACY_SESSIONS_DIR: join(root, "no-legacy"),
      KAGEHA_PROVIDER_URL: `http://127.0.0.1:${provider.address().port}/v1/chat/completions`,
      KAGEHA_MODEL: "goal-fixture",
    },
    stdio: ["ignore", "ignore", "pipe"],
  });
  child.stderr.on("data", chunk => {
    startupLog += chunk;
    const address = startupLog.match(/Rust chat service listening on (127\.0\.0\.1:\d+)/)?.[1];
    if (address) base = `http://${address}`;
  });
}
async function stopBackend() {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  const exited = new Promise(resolve => child.once("exit", resolve));
  child.kill();
  await exited;
}
async function api(path,body) {
  const response=await fetch(base+path,{method:body?'POST':'GET',headers:{Authorization:'Bearer '+token,'Content-Type':'application/json'},body:body?JSON.stringify(body):undefined,signal:AbortSignal.timeout(10000)});
  const data=await response.json();
  assert(response.ok,JSON.stringify(data));
  return data;
}
async function until(fn) {
  for(let i=0;i<100;i++){try{const result=await fn();if(result)return result;}catch{} await sleep(100);}
  throw Error('Timed out: '+JSON.stringify(calls)+'\n'+startupLog);
}
async function session(){return (await api('/api/sessions',{})).session_id;}
async function run(chat,mode,message,path='/api/chat/send',extra={}) {
  return api(path,{session_id:chat,client_request_id:randomBytes(12).toString('hex'),agent_mode:mode,message,...extra});
}
try{
  startBackend();
  await until(()=>api('/health'));
  const normal=await session();await run(normal,'normal','Give a normal answer.');
  assert.equal((await api('/api/sessions/'+normal+'/goal')).goal,null);
  assert(!calls.some(c=>c.planning));
  const goal=await session();await run(goal,'goal','Verify both fixture files.');
  const completed=await until(async()=>{
    const {goal:state}=await api('/api/sessions/'+goal+'/goal');
    return state?.state==='completed'&&state;
  });
  assert.equal(completed.turns,2);
  assert.deepEqual(calls.filter(c=>c.goal&&c.tool==='read_file').map(c=>c.continuation),[false,true]);
  const beforeRestart = calls.length;
  await stopBackend();
  startBackend();
  await until(() => api("/health"));
  assert.equal((await api(`/api/sessions/${goal}/goal`)).goal.state, "completed");
  await sleep(750);
  assert.equal(calls.length, beforeRestart, "restart must not replay a completed goal");
  const blocked=await session();await run(blocked,'goal','Blocked goal fixture.');
  await until(async()=> (await api('/api/sessions/'+blocked+'/goal')).goal?.state==='blocked');
  await sleep(700);
  assert.equal((await api('/api/sessions/'+blocked+'/goal')).goal.turns,1);
  const paused=await session();
  await run(paused,'goal','Pause fixture.','/api/chat/stream',{event_stream:'session'});
  await until(async()=> (await api('/api/sessions/'+paused+'/goal')).goal?.state==='active');
  assert.equal((await api('/api/sessions/'+paused+'/goal',{action:'pause'})).goal.state,'paused');
  await sleep(750);
  assert.equal((await api('/api/sessions/'+paused+'/goal')).goal.turns,1);
  async function pending(chat) {
    return until(async () => (await api(`/api/sessions/${chat}/run-change`)).proposal);
  }
  const promotedPlan = await session();
  await run(promotedPlan, "normal", "Suggest Plan fixture.");
  const planProposal = await pending(promotedPlan);
  const acceptedPlan = await api(`/api/sessions/${promotedPlan}/run-change`, { action: "accept", turn: planProposal.turn });
  assert(acceptedPlan.proposal.successor);
  await until(async () => (await api(`/api/sessions/${promotedPlan}/design`)).awaiting_build);
  const promotedGoal = await session();
  await run(promotedGoal, "normal", "Suggest Goal fixture.");
  const goalProposal = await pending(promotedGoal);
  await api(`/api/sessions/${promotedGoal}/run-change`, { action: "accept", turn: goalProposal.turn });
  const goalState = await until(async () => {
    const { goal } = await api(`/api/sessions/${promotedGoal}/goal`);
    return goal?.state === "completed" && goal;
  });
  assert(goalState.objective.includes("Both fixture files have been verified."));

  const previousPolicy = (await api("/api/settings/persistence")).policy;
  await api("/api/settings/persistence", {...previousPolicy, enabled: true});
  const autonomous = await session();
  await run(autonomous, "normal", "Extend fixture.");
  const detail = await api(`/api/sessions/${autonomous}`);
  assert(detail.messages.some(message => message.text === "Verified both files in the original turn."));
  assert.equal((await api(`/api/sessions/${autonomous}/run-change`)).proposal, null);
  // Requests pinned to legacy mode still use a reviewed successor.
  await api("/api/settings/persistence", {...previousPolicy, enabled: false});
  const extended = await session();
  await run(extended, "normal", "Extend fixture.");
  const extension = await pending(extended);
  assert.equal(extension.model_calls, 128);
  await stopBackend();
  startBackend();
  await until(() => api("/health"));
  assert.equal((await pending(extended)).turn, extension.turn);
  const decision = { action: "accept", turn: extension.turn };
  const [firstAcceptance, secondAcceptance] = await Promise.all([
    api(`/api/sessions/${extended}/run-change`, decision),
    api(`/api/sessions/${extended}/run-change`, decision),
  ]);
  assert.equal(firstAcceptance.proposal.successor, secondAcceptance.proposal.successor);
  await until(async () => {
    const result = await api(`/api/sessions/${extended}/events`);
    return result.events?.some(event => event.turn_id === firstAcceptance.proposal.successor && event.kind === "done");
  });

  await api("/api/settings/persistence", previousPolicy);
  const dismissed = await session();
  await run(dismissed, "normal", "Suggest Goal fixture.");
  const dismissal = await pending(dismissed);
  await api(`/api/sessions/${dismissed}/run-change`, { action: "dismiss", turn: dismissal.turn });
  assert.equal((await api(`/api/sessions/${dismissed}/run-change`)).proposal, null);
  assert.equal((await api(`/api/sessions/${dismissed}/goal`)).goal, null);

  const superseded = await session();
  await run(superseded, "normal", "Suggest Goal fixture.");
  const stale = await pending(superseded);
  await run(superseded, "normal", "Use a new scope.");
  await assert.rejects(
    api(`/api/sessions/${superseded}/run-change`, { action: "accept", turn: stale.turn }),
    /no longer pending/,
  );
  assert.equal((await api(`/api/sessions/${superseded}/goal`)).goal, null);

  const missingAccess = await session();
  await run(missingAccess, "normal", "Access blocker fixture.");
  assert.equal((await api(`/api/sessions/${missingAccess}/run-change`)).proposal, null);
  const plan=await session();
  await run(plan,'plan','Make a plan to read the first fixture.','/api/chat/stream',{event_stream:'session'});
  await until(async()=> (await api('/api/sessions/'+plan+'/design')).awaiting_build===true);
  assert.equal((await api('/api/sessions/'+plan+'/goal')).goal,null);
  assert.equal(calls.filter(c=>c.planning).length,2);
  const resumeChat = await session();
  await run(resumeChat, 'normal', 'Resume checkpoint fixture.');
  const saved = await until(async () => {
    const detail = await api(`/api/sessions/${resumeChat}`);
    return detail.messages?.find(message => message.resume?.allowed);
  });
  const resumeBody = {
    session_id: resumeChat, client_request_id: 'resume-fixture', message: 'Resume task',
    resume_turn_id: saved.turn_id,
  };
  const journal = new DatabaseSync(join(root, 'state', 'native-chats.sqlite'));
  try {
    const setPhase = journal.prepare('UPDATE ui_turn_checkpoints SET phase=? WHERE turn_id=?');
    setPhase.run('unverified', saved.turn_id);
    await assert.rejects(api('/api/chat/send', resumeBody), /original receipt/);
    setPhase.run('ready', saved.turn_id);
  } finally { journal.close(); }
  await api('/api/chat/send', resumeBody);
  await api('/api/chat/send', resumeBody);
  const resumed = await api(`/api/sessions/${resumeChat}`);
  assert.equal(resumed.messages.filter(message => message.text === 'Checkpoint retained.').length, 1);
  console.log('PASS: Normal direct, Goal verified continuation, restart, blocker and pause, Plan awaiting approval, mode proposals, one successor and blocked extension');
} finally {
  await stopBackend();
  provider.closeAllConnections();provider.close();
  rmSync(root,{recursive:true,force:true});
}
