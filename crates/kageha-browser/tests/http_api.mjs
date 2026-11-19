// Run with KAGEHA_BROWSER_BIN=/absolute/binary node tests/http_api.mjs.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import net from 'node:net';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { randomBytes, randomUUID } from 'node:crypto';
const binary = process.env.KAGEHA_BROWSER_BIN;
assert(binary && path.isAbsolute(binary), 'Set KAGEHA_BROWSER_BIN');
const chromium = process.env.CHROMIUM_BIN || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const root = fs.mkdtempSync(path.join(os.tmpdir(),'kbr-http-'));
const token = randomBytes(32).toString('hex');
const tokenFile = path.join(root,'http.token');
fs.writeFileSync(tokenFile, token, {mode:0o600});
const portProbe = net.createServer(); portProbe.listen(0,'127.0.0.1'); await once(portProbe,'listening');
const port = portProbe.address().port; await new Promise(resolve=>portProbe.close(resolve));
const base = `http://127.0.0.1:${port}`;
let decisionCalls=0,draftEdits=0;
const decisionProvider=http.createServer(async(req,res)=>{
  let raw='';for await(const chunk of req)raw+=chunk;
  const body=JSON.parse(raw);assert(body.questions&&!body.messages);
  decisionCalls++;const answers={};
  for(const [id,q] of Object.entries(body.questions)){
    const choice=id.startsWith('field_')?Object.keys(q.criteria).find(k=>k!=='none'):'true';
    answers[id]={type:'choice',choice,confidence:0.94,probabilities:Object.fromEntries(Object.keys(q.criteria).map(k=>[k,k===choice?0.8:0.2/(Object.keys(q.criteria).length-1)]))};
  }
  res.setHeader('content-type','application/json');res.end(JSON.stringify({answers}));
});
decisionProvider.listen(0,'127.0.0.1');await once(decisionProvider,'listening');
const decisionConfig=path.join(root,'decisions.json');
fs.writeFileSync(decisionConfig,JSON.stringify({version:1,decision:{decision_model:'fixture/decisions'},models:{'fixture/decisions':{url:`http://127.0.0.1:${decisionProvider.address().port}/systemone`,model:'fixture',protocol:'system_one'}},contexts:[{workspace:'independent-workflow',client:'agent-one',run:'standalone-run',intent:'Fill only the draft name from my supplied name. Do not save or submit.',values:{name:'External HTTP'}}]}),{mode:0o600});
const child = spawn(binary,['--state-dir',path.join(root,'state'),'--instance-id','http-test','--application-id','independent-agent','--chromium',chromium,'--standalone','on','--http-listen',`127.0.0.1:${port}`,'--http-token-file',tokenFile,'--decision-config',decisionConfig],{stdio:['ignore','ignore','pipe']});
let stderr=''; child.stderr.on('data',chunk=>stderr=(stderr+chunk).slice(-8192));
const headers={authorization:`Bearer ${token}`};
let markStarted;const started=new Promise(resolve=>markStarted=resolve);
const page = http.createServer((req,res)=>{
  if(req.url==='/draft-edited'){draftEdits++;res.end('ok');return;}
  if(req.url==='/started'){markStarted();res.end('ok');return;}
  res.setHeader('content-type','text/html');
  res.end(`<title>Standalone API fixture</title><label>Draft name<input id="draft-name" oninput="fetch('/draft-edited')"></label><label><input type="radio" name="answer" value="yes">Yes</label><label><input type="radio" name="answer" value="no" onchange="document.querySelector('#count').textContent=Number(document.querySelector('#count').textContent)+1">No</label><p id="count">0</p><label><input type="checkbox">Confirm</label><button onclick="fetch('/started').then(()=>setTimeout(()=>document.querySelector('#done').textContent='Done',300))">Delayed</button><p id="done"></p>`);
});
page.listen(0,'127.0.0.1');await once(page,'listening');
try {
  const deadline=Date.now()+5000;
  while(true){try { if((await fetch(base+'/health',{headers})).ok)break;}catch{}assert(child.exitCode===null,stderr);assert(Date.now()<deadline,'HTTP startup timed out');await new Promise(r=>setTimeout(r,25));}
  assert.equal((await fetch(base+'/health')).status,401);
  assert.equal((await fetch(base+'/health',{headers:{authorization:'Bearer wrong-token'}})).status,401);
  assert.equal((await fetch(base+'/health',{headers:{...headers,origin:'https://example.com'}})).status,403);
  const info=await (await fetch(base+'/v1/info',{headers})).json();
  assert(info.methods.includes('claim_control'));assert(!info.methods.includes('shutdown'));
  assert.equal((await fetch(base+'/v1/schema',{headers})).status,200);
  assert.equal((await fetch(base+'/v1/rpc',{method:'POST',headers,body:'{}'})).status,415);
  assert.equal((await fetch(base+'/v1/rpc',{method:'POST',headers:{...headers,'content-type':'application/json'},body:'x'.repeat(65537)})).status,413);
  const stalled=await Promise.all(Array.from({length:info.limits.concurrent_requests},async()=>{
    const socket=net.createConnection(port,'127.0.0.1');await once(socket,'connect');
    socket.write(`POST /v1/rpc HTTP/1.1\r\nHost: 127.0.0.1:${port}\r\nAuthorization: Bearer ${token}\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n{`);
    return socket;
  }));
  try {
    await new Promise(resolve=>setTimeout(resolve,100));
    assert.equal((await fetch(base+'/health',{headers})).status,429);
  } finally { stalled.forEach(socket=>socket.destroy()); }
  await new Promise(resolve=>setTimeout(resolve,100));
  assert.equal((await fetch(base+'/health',{headers})).status,200);
  const scope={workspace_id:'independent-workflow',client_id:'agent-one',service_epoch:info.service_epoch};
  async function rpc(method,params={},extra={}){
    const response=await fetch(base+'/v1/rpc',{method:'POST',headers:{...headers,'content-type':'application/json'},body:JSON.stringify({...scope,request_id:randomUUID(),method,params,...extra})});
    return {status:response.status,body:await response.json()};
  }
  assert.equal((await rpc('handshake')).status,403);
  assert.equal((await rpc('shutdown')).status,403);
  assert.equal((await rpc('health',{}, {service_epoch:'obsolete'})).body.error.code,'stale_service_epoch');
  assert.equal((await rpc('health',{}, {timeout_ms:30001})).status,400);
  assert.equal((await rpc('health',{}, {unknown:true})).status,400);
  const id=randomUUID();const launch=(await rpc('launch_browser',{}, {request_id:id})).body;
  assert.equal(launch.status,'ok',JSON.stringify(launch));
  assert.equal((await rpc('launch_browser',{}, {request_id:id})).body.error.code,'duplicate_request');
  const browser_id=launch.result.browser_id;
  const tab=(await rpc('open_tab',{browser_id,url:`http://127.0.0.1:${page.address().port}/`})).body;
  assert.equal(tab.status,'ok',JSON.stringify(tab));
  const observation=(await rpc('observe_tab',{browser_id,target_id:tab.result.target_id,compact:true})).body;
  assert.equal(observation.status,'ok',JSON.stringify(observation));
  assert(JSON.stringify(observation.result).includes('Yes'));
  const decisionInfo=(await rpc('browser_decision_info')).body.result;
  assert.equal(decisionInfo.owner,'browser-service');assert.equal(decisionInfo.action_mode,'active');
  assert.equal(decisionInfo.cutoff,0.90);assert.equal(decisionInfo.run_id,'standalone-run');
  const proposal=(await rpc('browser_decide',{run_id:decisionInfo.run_id,purpose:'browser_action',observation_id:observation.result.observation_id})).body;
  assert.equal(proposal.status,'ok',JSON.stringify(proposal));assert.equal(proposal.result.score_metric,'provider_confidence');
  assert.equal(proposal.result.minimum_score,0.94,JSON.stringify(proposal));assert.equal(proposal.result.minimum_selected_probability,0.8);
  assert.equal(proposal.result.actions.length,1);assert.equal(proposal.result.actions[0].kind,'fill');
  assert.equal(draftEdits,0,'A decision proposal must not send browser input');assert.equal(decisionCalls,1);
  const forged=(await rpc('browser_decide',{run_id:decisionInfo.run_id,purpose:'browser_action',observation_id:observation.result.observation_id,confidence_cutoff:0.1})).body;
  assert.equal(forged.status,'error');assert.equal(decisionCalls,1,'Tool parameters cannot reconfigure operator-owned cutoff');
  const grant=(await rpc('claim_control')).body;
  assert.equal(grant.status,'ok',JSON.stringify(grant));
  const denied=(await rpc('claim_control',{}, {client_id:'agent-two'})).body;
  assert.equal(denied.status,'error');
  const no=observation.result.controls.find(control=>control.name==='No');
  assert(no);
  const checked=(await rpc('check_observed',{browser_id,target_id:tab.result.target_id,observation_id:observation.result.observation_id,target:no.ref,checked:true},{authority:grant.result.authority})).body;
  assert.equal(checked.status,'ok',JSON.stringify(checked));
  const refreshed=(await rpc('observe_tab',{browser_id,target_id:tab.result.target_id,compact:true})).body.result;
  const again=(await rpc('check_observed',{browser_id,target_id:tab.result.target_id,observation_id:refreshed.observation_id,target:refreshed.controls.find(control=>control.name==='No').ref,checked:true},{authority:grant.result.authority})).body;
  assert.equal(again.status,'ok',JSON.stringify(again));
  const unchanged=(await rpc('observe_tab',{browser_id,target_id:tab.result.target_id,compact:true})).body.result;
  const refused=(await rpc('check_observed',{browser_id,target_id:tab.result.target_id,observation_id:unchanged.observation_id,target:unchanged.controls.find(control=>control.name==='No').ref,checked:false},{authority:grant.result.authority})).body;
  assert.equal(refused.status,'error');assert.equal(refused.execution_phase,'not_dispatched');
  const afterRefusal=(await rpc('observe_tab',{browser_id,target_id:tab.result.target_id,compact:true})).body.result;
  const checkbox=afterRefusal.controls.find(control=>control.name==='Confirm');
  const confirmed=(await rpc('check_observed',{browser_id,target_id:tab.result.target_id,observation_id:afterRefusal.observation_id,target:checkbox.ref,checked:true},{authority:grant.result.authority})).body;
  assert.equal(confirmed.status,'ok',JSON.stringify(confirmed));
  const selection=(await rpc('browser_selection')).body.result;
  const count=(await rpc('query_tab',{selection:{browser_id,target_id:tab.result.target_id,binding_id:selection.binding_id},selector:'#count'})).body;
  assert.equal(count.status,'ok',JSON.stringify(count));
  assert.equal(count.result.matches.items[0].text,'1',JSON.stringify(count));
  const frame=(await rpc('preview_tab',{browser_id,target_id:tab.result.target_id,binding_id:selection.binding_id})).body;
  assert.equal(frame.status,'ok',JSON.stringify(frame));
  const image=Buffer.from(frame.binary_base64,'base64');
  assert(image.length>100);assert.equal(image[0],0xff);assert.equal(image[1],0xd8);
  const beforeDelay=(await rpc('observe_tab',{browser_id,target_id:tab.result.target_id,compact:true})).body.result;
  const delayed=beforeDelay.controls.find(control=>control.name==='Delayed');
  const controller=new AbortController();const delayedId=randomUUID();
  const dropped=fetch(base+'/v1/rpc',{method:'POST',signal:controller.signal,headers:{...headers,'content-type':'application/json'},body:JSON.stringify({...scope,request_id:delayedId,method:'click_observed',params:{browser_id,target_id:tab.result.target_id,observation_id:beforeDelay.observation_id,target:delayed.ref,expected_selector:'#done',expected_text:'Done',timeout_ms:3000},authority:grant.result.authority})}).catch(()=>null);
  await Promise.race([started,new Promise((_,reject)=>setTimeout(()=>reject(Error('Delayed click did not dispatch')),4000))]);
  controller.abort();await dropped;
  await new Promise(resolve=>setTimeout(resolve,500));
  const receipt=(await rpc('action_receipt',{request_id:delayedId})).body;
  assert.equal(receipt.status,'ok',JSON.stringify(receipt));
  assert(JSON.stringify(receipt.result).includes('verified'),JSON.stringify(receipt.result));
  const other=(await rpc('list_tabs',{browser_id},{workspace_id:'different-workflow'})).body;
  assert.equal(other.status,'error');
  assert.equal((await rpc('close_browser',{browser_id})).body.status,'ok');
  const example=spawn(process.execPath,[new URL('../sdk/example.mjs',import.meta.url).pathname],{env:{...process.env,BROWSER_API_URL:base,BROWSER_API_TOKEN_FILE:tokenFile,BROWSER_DEMO_URL:`http://127.0.0.1:${page.address().port}/`},stdio:['ignore','pipe','pipe']});
  let exampleOutput='';let exampleError='';
  example.stdout.on('data',chunk=>exampleOutput+=chunk);
  example.stderr.on('data',chunk=>exampleError+=chunk);
  const [exampleCode]=await once(example,'exit');
  assert.equal(exampleCode,0,exampleError);
  assert(JSON.parse(exampleOutput).status==='ok');
  console.log('Standalone HTTP API passed authentication, Origin rejection, strict schema, restart fence, duplicate input protection, workspace isolation, control fencing, real radio check and JPEG preview.');
} finally {
  decisionProvider.closeAllConnections();await new Promise(resolve=>decisionProvider.close(resolve));
  page.closeAllConnections();await new Promise(resolve=>page.close(resolve));
  child.kill('SIGINT');await Promise.race([once(child,'exit'),new Promise(resolve=>setTimeout(resolve,2000))]);
  if(child.exitCode===null)child.kill('SIGKILL');
  fs.rmSync(root,{recursive:true,force:true});
}
