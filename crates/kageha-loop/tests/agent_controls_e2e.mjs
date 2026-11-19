// Actual Rust WebUI + separate Rust bridge. Disposable local model/MCP fixtures only.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import http from 'node:http';
import {spawn} from 'node:child_process';
import {DatabaseSync} from 'node:sqlite';
const root=fs.mkdtempSync('/tmp/kageha-agent-controls-');
const token='agent-controls-disposable-authentication-token';
const appBin=process.env.KAGEHA_LOOP_BIN||path.resolve('target/debug/kageha-loop');
const runtimeBin=process.env.KAGEHA_RUNTIME_BIN||path.resolve('target/debug/kageha-runtime-service');
fs.writeFileSync(root+'/token',token,{mode:0o600});
fs.mkdirSync(root+'/workspace');fs.mkdirSync(root+'/empty');
fs.writeFileSync(root+'/workspace/notes.txt','Confirmed proof before interruption.');
const pause=ms=>new Promise(r=>setTimeout(r,ms));
async function until(check,label='condition'){for(let end=Date.now()+18000;Date.now()<end;){const result=await check();if(result)return result;await pause(15);}throw Error('Timed out: '+label);}
let app,appUrl,runtime,runtimeUrl,modelCalls=0,mcpCalls=0,resumeInput,releaseSteer,resumeWaitId;
const requests=[],feeds=[],children=[],logs=[];
function has(body,text){return body.messages.some(m=>m.role==='user'&&m.content===text);}
function toolMessage(id,name,args){return{role:'assistant',content:null,tool_calls:[{id,type:'function',function:{name,arguments:JSON.stringify(args)}}]};}
const model=http.createServer(async(req,res)=>{
 try{
  if(req.url==="/release-steer"){releaseSteer?.();res.end("released");return;}
  let bytes='';for await(const b of req)bytes+=b;const body=JSON.parse(bytes||'{}');
  if(req.url==='/mcp'){
   const result=body.method==='initialize'?{protocolVersion:'2024-11-05',capabilities:{tools:{}},serverInfo:{name:'fixture',version:'1'}}:body.method==='tools/list'?{tools:[{name:'guarded',description:'Disposable approval proof',inputSchema:{type:'object',properties:{},additionalProperties:false}}]}:body.method==='tools/call'?(mcpCalls++,{content:[{type:'text',text:'Approved exact request.'}]}):{};
   if(body.method?.startsWith('notifications/')){res.writeHead(202);res.end();return;}res.setHeader('Content-Type','application/json');res.end(JSON.stringify({jsonrpc:'2.0',id:body.id,result}));return;
  }
  modelCalls++;requests.push(body);
  const reply=message=>{if(!res.destroyed){res.setHeader('Content-Type','application/json');res.end(JSON.stringify({choices:[{message}]}));}};
  const results=body.messages.filter(m=>m.role==='tool');
  if(has(body,'Inspect continued child')){
   if(!results.length)reply(toolMessage('wait-continued','agent_wait_any',{job_ids:[resumeWaitId],timeout_ms:1000}));
   else {const value=JSON.parse(results.at(-1).content);assert.equal(value.jobs[0].id,resumeWaitId);assert.equal(value.jobs[0].state,'succeeded');reply({role:'assistant',content:'Continued child inspected with stable identity.'});}return;
  }
  if(has(body,'tree parent')){
   if(!results.length)reply(toolMessage('spawn-nested','agent_spawn',{agent_id:'main',objective:'nested child'}));
   else reply({role:'assistant',content:'Parent assignment finished; child continues.'});return;
  }
  if(has(body,'nested child')){
   if(!results.length)reply(toolMessage('nested-read','read_file',{path:'notes.txt'}));
   else reply({role:'assistant',content:'Nested child verified the fixture.'});return;
  }
  if(has(body,'resume checkpoint')||has(body,'unknown checkpoint')){
   if(body.messages.some(m=>m.role==='user'&&String(m.content).startsWith('Continue the existing assignment'))){resumeInput=body;assert(results.some(m=>m.tool_call_id==='resume-read'&&m.content==='Confirmed proof before interruption.'));reply({role:'assistant',content:'Continued from the confirmed tool result.'});return;}
   if(!results.length)reply(toolMessage('resume-read','read_file',{path:'notes.txt'}));
   else reply({role:'assistant',content:42}); // Real model protocol failure after the confirmed read checkpoint.
   return;
  }
  if(has(body,'steering worker')){
   if(has(body,'Use revised criteria')){reply({role:'assistant',content:'Steering delivered exactly once.'});return;}
   await new Promise(r=>{releaseSteer=()=>{reply({role:'assistant',content:'Original unsteered answer.'});r();};res.once('close',r);});return;
  }
  if(has(body,'approval worker')){
   if(!results.length){assert(body.tools.some(t=>t.function?.name==='mcp_call'),'MCP tools are reached through mcp_call');reply(toolMessage('guarded-call','mcp_call',{server:'approval-fixture',tool:'guarded',arguments:{}}));}
   else reply({role:'assistant',content:'Exact approval completed.'});return;
  }
  reply({role:'assistant',content:'Fixture task completed.'});
 }catch(error){res.statusCode=500;res.end(JSON.stringify({error:String(error)}));}
});
await new Promise(r=>model.listen(0,'127.0.0.1',r));
async function launch(binary,args,env,name,pattern){const log=root+'/'+name+'-'+logs.length+'.log';logs.push(log);const fd=fs.openSync(log,'a');const child=spawn(binary,args,{env,stdio:['ignore',fd,fd]});fs.closeSync(fd);children.push(child);const url=await until(()=>{if(child.exitCode!==null||child.signalCode!==null)throw Error(fs.readFileSync(log,'utf8'));const output=fs.readFileSync(log,'utf8');const m=output.slice(0,output.lastIndexOf('\n')+1).match(pattern);return m&&'http://'+m[1];},name+' startup');return{child,url};}
async function startRuntime(){const launched=await launch(runtimeBin,['bridge','--state',root+'/runtime','--token-file',root+'/token','--bind','127.0.0.1:0'],{PATH:''},'runtime',/runtime bridge listening on ([^\s]+)/);runtime=launched.child;runtimeUrl=launched.url;}
async function startApp(){const launched=await launch(appBin,['serve',root+'/workspace',root+'/application','127.0.0.1:0'],{PATH:'',HOME:root,KAGEHA_SERVER_TOKEN:token,KAGEHA_PROVIDER_URL:`http://127.0.0.1:${model.address().port}/v1/chat/completions`,KAGEHA_MODEL:'fixture',KAGEHA_LEGACY_SESSIONS_DIR:root+'/empty',KAGEHA_RUNTIME_SERVICE_URL:runtimeUrl,KAGEHA_RUNTIME_SERVICE_TOKEN_FILE:root+'/token',...(process.argv.includes("--browser")?{KAGEHA_WEBUI_DIST:path.resolve("frontend/dist")}:{})},'app',/listening on ([^\s]+)/);app=launched.child;appUrl=launched.url;}
async function stopApp(){for(const f of feeds){f.abort.abort();await f.pump;}if(app?.exitCode===null&&app?.signalCode===null){app.kill('SIGTERM');await Promise.race([new Promise(r=>app.once('exit',r)),pause(1500)]);if(app.exitCode===null&&app.signalCode===null){app.kill('SIGKILL');await new Promise(r=>app.once('exit',r));}}}
async function request(route,body,expected=200,method){const response=await fetch(appUrl+route,{method:method||(body===undefined?'GET':'POST'),headers:{Authorization:'Bearer '+token,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body),signal:AbortSignal.timeout(18000)});const text=await response.text();assert.equal(response.status,expected,route+': '+text);return text?JSON.parse(text):null;}
const getJob=async id=>(await request('/api/jobs/'+id)).job;
const expected=row=>({expected_turn_id:row.progress.turn_id,expected_revision:row.revision});
async function createJob(session,objective,id){return request('/api/jobs',{session_id:session,agent_id:'main',objective,client_request_id:id});}
async function feed(session,after=0){const abort=new AbortController();const response=await fetch(appUrl+`/api/sessions/${session}/live/stream?after_sequence=${after}`,{headers:{Authorization:'Bearer '+token},signal:abort.signal});assert.equal(response.status,200);const f={abort,frames:[],last:after};feeds.push(f);const reader=response.body.getReader();f.pump=(async()=>{let buffer='';const decoder=new TextDecoder();try{for(;;){const chunk=await reader.read();if(chunk.done)break;buffer+=decoder.decode(chunk.value,{stream:true});let at;while((at=buffer.indexOf('\n\n'))>=0){const text=buffer.slice(0,at);buffer=buffer.slice(at+2);const event=text.split('\n').find(l=>l.startsWith('event:'))?.slice(6).trim();const raw=text.split('\n').filter(l=>l.startsWith('data:')).map(l=>l.slice(5).trim()).join('\n');if(!raw)continue;const data=JSON.parse(raw);f.frames.push({event,data});for(const frame of data.events||[])f.last=Math.max(f.last,frame.sequence||0);}}}catch(error){if(!abort.signal.aborted)throw error;}finally{reader.releaseLock();}})();return f;}
const events=f=>f.frames.filter(e=>e.event==='update').flatMap(e=>e.data.events);
const completed=async id=>until(async()=>{const job=await getJob(id);return ['succeeded','failed','cancelled','interrupted'].includes(job.state)&&job;},'worker '+id+' terminal');
try{
 await startRuntime();await startApp();assert.notEqual(runtime.pid,app.pid);
 const health=await request('/health');assert.equal(health.agent_runtime.mode,'service');assert.equal(health.agent_runtime.service.pid,runtime.pid);
 const session=(await request('/api/sessions',{})).session_id;
 const live=await feed(session);await until(()=>live.frames.some(f=>f.event==='snapshot'));
 const parentRequest={session_id:session,agent_id:'main',objective:'tree parent',client_request_id:'tree-parent',title:'Coordinator'};
 const parent=await request('/api/jobs',parentRequest);
 assert.equal(parent.title,'Coordinator');assert.equal(parent.label,'Coordinator');
 assert.equal((await request('/api/jobs',parentRequest)).id,parent.id);
 await request('/api/jobs',{...parentRequest,title:'Other title'},409);
 await request('/api/jobs',{...parentRequest,client_request_id:'bad-title',title:'bad\ntitle'},400);
 const parentDone=await completed(parent.id);assert.equal(parentDone.state,'succeeded');
 const nested=await until(async()=>{const rows=(await request('/api/jobs?session_id='+session)).jobs;return rows.find(j=>j.parent_job_id===parent.id);},'nested worker in root tree');
 const nestedDone=await completed(nested.id);assert.equal(nestedDone.state,'succeeded');assert.equal(nestedDone.root_session_id,session);assert.equal(nestedDone.parent_session,parent.session_id);assert.equal(nestedDone.depth,2);
 assert.equal(nestedDone.title,null);assert.equal(nestedDone.label,'nested child');
 await until(()=>events(live).some(e=>e.kind==='worker'&&e.job.id===nested.id&&e.job.state==='succeeded'),'nested root terminal event');
 assert(events(live).some(e=>e.kind==='worker'&&e.job.id===nested.id&&e.job.progress.current_tool?.name==='read_file'),'root feed records exact nested tool before completion');
 assert(nestedDone.started_at>0&&nestedDone.ended_at>=nestedDone.started_at);assert(nestedDone.progress.elapsed_s>=0);assert.equal(nestedDone.progress.current_tool,null);
 const frozen=nestedDone.progress.elapsed_s;await pause(30);assert.equal((await getJob(nested.id)).progress.elapsed_s,frozen,'terminal elapsed time does not keep growing');
 for(const control of['cancel','message','approve','resume','resolve','attach'])await request('/api/jobs/'+parent.id+'/'+control,undefined,405);
 const steering=await createJob(session,'steering worker','steering');await until(()=>releaseSteer,'held model before steering');let steeringRow=await getJob(steering.id);
 await request('/api/jobs/'+steering.id+'/message',{message:'Do not accept stale',message_id:'stale-message',expected_turn_id:'old-turn'},409);
 await request('/api/jobs/'+steering.id+'/cancel',{expected_turn_id:'old-turn'},409);
 await request('/api/jobs/'+steering.id+'/cancel',{expected_turn_id:steeringRow.progress.turn_id,expected_revision:0},409);
 const message={message:'Use revised criteria',message_id:'stable-steering',...expected(steeringRow)};
 await request('/api/jobs/'+steering.id+'/message',message);await request('/api/jobs/'+steering.id+'/message',message);releaseSteer();
 const steered=await completed(steering.id);assert.equal(steered.state,'succeeded');assert(steered.result.message.includes('Steering delivered exactly once.'));
 const steeredCalls=requests.filter(r=>has(r,'Use revised criteria'));assert.equal(steeredCalls.length,1);assert.equal(steeredCalls[0].messages.filter(m=>m.content==='Use revised criteria').length,1);
 await request('/api/mcp',{name:'approval-fixture',transport:'http',url:`http://127.0.0.1:${model.address().port}/mcp`,enabled:true});
 // Keep this approval-specific case independent of the host's default access profile.
 await request('/api/permissions',{session_id:session,profile:'ask'});
 assert.equal((await request('/api/permissions?session_id='+session)).profile,'ask');
 assert.equal(mcpCalls,0);
 const approval=await createJob(session,'approval worker','approval');const pending=await until(async()=>{const job=await getJob(approval.id);return job.question?.approval_id&&job;},'exact worker approval');
 assert.equal(pending.controls.approve,true);assert(pending.progress.current_tool?.name.startsWith('mcp_'));
 await request('/api/jobs/'+approval.id+'/approve',{approval_id:'wrong-approval',approved:true,...expected(pending)},409);assert.equal(mcpCalls,0,'wrong approval never dispatches');
 const pendingAgain=await getJob(approval.id);assert.equal(pendingAgain.question.approval_id,pending.question.approval_id);
 await request('/api/jobs/'+approval.id+'/approve',{approval_id:pending.question.approval_id,approved:true,...expected(pendingAgain)});assert.equal((await completed(approval.id)).state,'succeeded');assert.equal(mcpCalls,1);
 const stopped=await createJob(session,'resume checkpoint','resume');const stoppedRow=await completed(stopped.id);assert.equal(stoppedRow.state,'failed');assert.equal(stoppedRow.can_resume,true);assert.equal(stoppedRow.progress.steps,1);
 const originalTurn=stoppedRow.progress.turn_id;const originalChild=stoppedRow.session_id;
 const confirmation={confirmed:true,client_request_id:'continue-once',...expected(stoppedRow)};
 const resumed=await request('/api/jobs/'+stopped.id+'/resume',confirmation);assert.equal(resumed.job.id,stopped.id);assert.equal(resumed.job.session_id,originalChild);assert.notEqual(resumed.job.progress.turn_id,originalTurn);
 const finished=await completed(stopped.id);assert.equal(finished.state,'succeeded');assert(finished.result.message.includes('Continued from the confirmed tool result.'));assert(resumeInput,'resumed model received history');
 assert.equal(resumeInput.messages.filter(m=>m.role==='tool'&&m.tool_call_id==='resume-read').length,1,'confirmed tool result survives continuation');
 resumeWaitId=stopped.id;
 const waitResult=await request('/api/chat/send',{session_id:session,client_request_id:'inspect-continued-child',message:'Inspect continued child'});assert.equal(waitResult.message,'Continued child inspected with stable identity.');
 const again=await request('/api/jobs/'+stopped.id+'/resume',confirmation);assert.equal(again.run_id,resumed.run_id);assert.equal(again.job.progress.turn_id,finished.progress.turn_id);
 await request('/api/jobs/'+stopped.id+'/message',{message:'Old control',message_id:'old-control',expected_turn_id:originalTurn},409);
 await request('/api/jobs/'+stopped.id+'/cancel',{expected_turn_id:originalTurn},409);
 const db=new DatabaseSync(root+'/application/native-chats.sqlite');
 assert.equal(db.prepare("SELECT count(*) n FROM ui_events WHERE chat_id=? AND event='tool_card' AND json_extract(payload,'$.name')='read_file' AND json_extract(payload,'$.status')='pending'").get(originalChild).n,1,'continuation never replays completed read dispatch');
 assert.equal(db.prepare('SELECT count(*) n FROM ui_job_resumes WHERE job_id=?').get(stopped.id).n,1,'stable control command admits exactly one continuation');db.close();
 const unknown=await createJob(session,'unknown checkpoint','unknown');const unknownRow=await completed(unknown.id);assert.equal(unknownRow.state,'failed');
 // Explicit crash-window fixture: an outstanding tool dispatch lacks a confirmed reply.
 const unknownDb=new DatabaseSync(root+'/application/native-chats.sqlite');unknownDb.prepare("UPDATE ui_turn_checkpoints SET phase='dispatching',calls=? WHERE turn_id=?").run(JSON.stringify([{id:'unknown-input',name:'mcp_guarded',arguments:{}}]),unknownRow.progress.turn_id);unknownDb.close();
 const blocked=await getJob(unknown.id);assert.equal(blocked.can_resume,false);assert(blocked.resume_blocker.includes('original receipt'));
 await request('/api/jobs/'+unknown.id+'/resume',{confirmed:true,client_request_id:'must-not-replay',...expected(blocked)},409);
 const beforeReplay=modelCalls;const cursor=live.last;live.abort.abort();await live.pump;
 const replay=await feed(session,cursor);const snapshot=await until(()=>replay.frames.find(f=>f.event==='snapshot'));assert(snapshot.data.workers.some(j=>j.id===nested.id&&j.parent_job_id===parent.id));await pause(40);assert.equal(modelCalls,beforeReplay,'read-only reconnect never submits work');
 await stopApp();await startApp();const restarted=await feed(session,cursor);const restored=await until(()=>restarted.frames.find(f=>f.event==='snapshot'));assert(restored.data.workers.some(j=>j.id===stopped.id&&j.progress.turn_id===finished.progress.turn_id));
 assert.equal((await getJob(stopped.id)).session_id,originalChild);assert.equal((await getJob(unknown.id)).can_resume,false);assert.equal(modelCalls,beforeReplay,'backend restart and subscription are read-only');
 const history=await request('/api/sessions/'+originalChild);assert(history.messages.some(m=>String(m.text).includes('Continued from the confirmed tool result.')));
 console.log(JSON.stringify({passed:true,python_backend:false,separate_runtime_pid:runtime.pid,model_calls:modelCalls,checks:['nested root session tree and tool attribution','frozen elapsed timestamps','exact-turn steering/cancel guards','POST-only controls','exact approval identity','logical worker continuation with confirmed history','idempotent continuation command','unknown dispatch blocks continuation','read-only reconnect and restart preservation'],root}));
 if(process.argv.includes('--browser')){
  const continuation=await createJob(session,'resume checkpoint','browser-continuation');await completed(continuation.id);
  const liveWorker=await createJob(session,'steering worker','browser-steering');
  const pendingWorker=await createJob(session,'approval worker','browser-approval');await until(async()=>!!(await getJob(pendingWorker.id)).question?.approval_id,'browser pending approval');
  const connection={root,app:appUrl,runtime:runtimeUrl,session,token,parent:parent.id,nested:nested.id,continuation:continuation.id,liveWorker:liveWorker.id,approval:pendingWorker.id,release:'http://127.0.0.1:'+model.address().port+'/release-steer'};
  fs.writeFileSync(root+'/browser.json',JSON.stringify(connection),{mode:0o600});fs.writeFileSync('/tmp/kageha-agent-controls-browser.json',JSON.stringify(connection),{mode:0o600});console.log(JSON.stringify({browserFixtureReady:true,app:appUrl,session}));
  await new Promise(resolve=>{process.once('SIGTERM',resolve);process.once('SIGINT',resolve);});
 }
}finally{
 await stopApp();for(const child of children){if(child.exitCode===null&&child.signalCode===null){child.kill('SIGTERM');await Promise.race([new Promise(r=>child.once('exit',r)),pause(1000)]);if(child.exitCode===null&&child.signalCode===null)child.kill('SIGKILL');}}
 model.closeAllConnections();model.close();
}
