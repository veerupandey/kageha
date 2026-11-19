// A submit without guessed success wording must remain fenced until independent proof.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import net from 'node:net';
import http from 'node:http';
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import {createInterface} from 'node:readline';
import {randomUUID} from 'node:crypto';
const root=fs.mkdtempSync('/tmp/kageha-deferred-submit-');fs.chmodSync(root,0o700);
const sleep=ms=>new Promise(r=>setTimeout(r,ms));let daemon,c;const counts={};
const site=http.createServer((req,res)=>{
 const u=new URL(req.url,'http://fixture');
 if(u.pathname==='/submit'){const id=u.searchParams.get('id');counts[id]=(counts[id]||0)+1;res.end('ok');return;}
 const id=u.pathname.match(/AI_(R\d+(?:-\d+)?)/)?.[1]||'generic';const rejected=u.searchParams.has('reject');const completion=u.searchParams.has('completion');const crossTenant=u.searchParams.has('crossTenant');const spoof=u.searchParams.has('spoof');
 res.setHeader('content-type','text/html');res.end(`<main><section data-automation-id="jobPostingDescription"><h2>Your application has been submitted.</h2></section><section id="result"></section><form data-automation-id="jobApplicationReviewPage"><h1>Review</h1><button id="submit" onclick="event.preventDefault();fetch('/submit?id=${id}');${rejected?"document.querySelector('#result').innerHTML='<h2>Errors found</h2><p>Title is required.</p>'":(completion?`this.form.remove();history.pushState({},'', '/en-US/${crossTenant?'OtherTenant':'Careers'}/jobTasks/completed/application');document.querySelector('#result').innerHTML='${spoof?'<section data-automation-id=jobPostingDescription>':'<section role=dialog>'}<h2>Application Submitted</h2><p>Thank you for your application.</p></section>'`:"this.form.remove();document.querySelector('#result').innerHTML='<h2>Your application has been successfully submitted.</h2>'")}">Submit</button></form><button id="other" onclick="fetch('/submit?id=${id}')">Another action</button></main>`);
});await new Promise(r=>site.listen(0,'127.0.0.1',r));
const base='http://127.0.0.1:'+site.address().port;
function start(){const fd=fs.openSync(root+'/daemon.log','a',0o600);daemon=spawn(process.env.KAGEHA_BROWSER_BIN,['--state-dir',root+'/state','--instance-id','deferred','--application-id','fixture','--fixture-actions','on','--chromium',process.env.CHROMIUM_BIN||'/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'],{stdio:['ignore',fd,fd]});fs.closeSync(fd);}
async function connect(){
 for(let i=0;i<250;i++){if(fs.existsSync(root+'/state/control.sock'))break;assert.equal(daemon.exitCode,null);await sleep(30);}
 const info=JSON.parse(fs.readFileSync(root+'/state/daemon.json'));const token=fs.readFileSync(root+'/state/auth.token','utf8');const socket=net.createConnection(root+'/state/control.sock');await once(socket,'connect');socket.on('error',()=>{});const reader=createInterface({input:socket});const lines=reader[Symbol.asyncIterator]();let epoch=null;
 async function call(method,params={},authority=null,id=randomUUID().replaceAll('-','')){socket.write(JSON.stringify({protocol_version:1,request_id:id,instance_id:info.instance_id,application_id:info.application_id,state_root:info.state_root,workspace_id:'chat',client_id:'agent',service_epoch:epoch,deadline_ms:Date.now()+10000,expected_binding:null,authority,method,params})+'\n');return JSON.parse((await lines.next()).value);}
 epoch=ok(await call('handshake',{token})).service_epoch||info.service_epoch;
 return {call,close(){reader.close();socket.destroy();}};
}
const ok=r=>{assert.equal(r.status,'ok',JSON.stringify(r));return r.result;};
async function context(browser,tab){for(let i=0;i<30;i++){const s=ok(await c.call('browser_selection'));const r=await c.call('submission_context',{browser_id:browser,target_id:tab,binding_id:s.binding_id});if(r.status==='ok')return r.result;await sleep(40);}throw Error('No context');}
try{
 start();c=await connect();const browser=ok(await c.call('launch_browser')).browser_id;
 for(const [id,reject,mode] of [['R15157',false,''],['R15158',true,''],['R15159-1',false,'&completion=1'],['R15160',false,'&completion=1&crossTenant=1'],['R15161',false,'&completion=1&spoof=1']]){
  c.close();c=await connect();
  if(process.env.DEBUG)console.error('case',id);
  const applicationUrl=base+'/en-US/Careers/job/Remote-'+('long-location-'.repeat(20))+'/AI_'+id+'/apply'+((reject||mode)?'?fixture=1'+(reject?'&reject=1':'')+mode:'');
  const tab=ok(await c.call('open_tab',{browser_id:browser,url:applicationUrl})).target_id;
  const before=await context(browser,tab);assert.equal(before.context.job_id,id);assert.equal(before.confirmation,null,'job description must not be proof');
  const selected=ok(await c.call('browser_selection'));
  assert.equal(selected.tabs.find(x=>x.target_id===tab).url,applicationUrl,'selection URL must match exact observation identity');
  const authority=ok(await c.call('claim_control',{ttl_ms:60000})).authority;
  const obs=ok(await c.call('observe_tab',{browser_id:browser,target_id:tab,compact:false}));const submit=obs.controls.find(x=>x.id==='submit');assert(submit);
  assert.equal(obs.url,applicationUrl,'observation URL identity must not be display-clipped');
  const inspected=ok(await c.call('query_tab',{selection:{browser_id:browser,target_id:tab,binding_id:selected.binding_id},selector:'#submit',limit:1}));
  assert.equal(inspected.observation.url,applicationUrl,'inspection retains the same full URL');
  const request='submit-'+id;const sent=ok(await c.call('click_observed',{browser_id:browser,target_id:tab,observation_id:inspected.observation_id,target:inspected.matches.items[0].ref,await_confirmation:true,submission_context:before.context},authority,request));
  assert.equal(sent.verification,'awaiting_site_confirmation');assert.equal(sent.receipt.state,'returned');assert.equal(sent.receipt.expected,undefined);
  const fresh=ok(await c.call('observe_tab',{browser_id:browser,target_id:tab,compact:false}));const retry=fresh.controls.find(x=>x.id===(reject?'submit':'other'));assert(retry);
  const duplicate=await c.call('click_observed',{browser_id:browser,target_id:tab,observation_id:fresh.observation_id,target:retry.ref,await_confirmation:true,submission_context:before.context},authority);
  assert(['action_receipt_pending','submission_identity_changed'].includes(duplicate.error?.code),JSON.stringify(duplicate));
  await sleep(100);assert.equal(counts[id],1);
  const s=ok(await c.call('browser_selection'));const args={browser_id:browser,target_id:tab,binding_id:s.binding_id,request_id:request};
  if(mode && !mode.includes('crossTenant') && !mode.includes('spoof')) {const probe=ok(await c.call('submission_context',args));assert.equal(probe.context.job_id,id);assert.equal(probe.confirmation.kind,'workday_completion');assert.equal(probe.input_sent,false);assert.equal(ok(await c.call('action_receipt',{request_id:request})).receipt.resolution,undefined);}
  const result=await c.call('confirm_submission',args);
  if(reject||mode.includes('crossTenant')||mode.includes('spoof')){assert(['postcondition_unobserved','confirmation_identity_missing'].includes(result.error?.code),JSON.stringify(result));assert.equal(ok(await c.call('action_receipt',{request_id:request})).receipt.resolution,undefined);}
  else {const proof=ok(result);assert.equal(proof.input_sent,false);assert.equal(proof.receipt.state,'verified');assert.equal(proof.receipt.confirmation.kind,mode?'workday_completion':'workday_confirmation');assert.equal(proof.receipt.confirmation.context.job_id,id);assert.equal(proof.original_receipt.state,'returned');assert.equal(proof.original_receipt.expected,undefined);assert.equal(ok(await c.call('confirm_submission',args)).receipt.request_id,proof.receipt.request_id);}
 }
 const generic=ok(await c.call('open_tab',{browser_id:browser,url:base+'/other-provider/application'})).target_id;
 let gs=ok(await c.call('browser_selection'));
 const gc=ok(await c.call('submission_context',{browser_id:browser,target_id:generic,binding_id:gs.binding_id,allow_generic:true}));
 assert(gc.context.job_id.startsWith('url-sha256:'));assert.equal(gc.confirmation,null);
 const ga=ok(await c.call('claim_control',{ttl_ms:60000})).authority;
 const go=ok(await c.call('observe_tab',{browser_id:browser,target_id:generic,compact:false}));
 const gr=ok(await c.call('click_observed',{browser_id:browser,target_id:generic,observation_id:go.observation_id,target:go.controls.find(x=>x.id==='submit').ref,await_confirmation:true,submission_context:gc.context},ga,'generic-submit'));
 assert.equal(gr.receipt.state,'returned');await sleep(100);assert.equal(counts.generic,1);
 gs=ok(await c.call('browser_selection'));assert.equal((await c.call('confirm_submission',{browser_id:browser,target_id:generic,binding_id:gs.binding_id,request_id:'generic-submit'})).status,'error');
 c.close();c=null;daemon.kill('SIGTERM');await once(daemon,'exit');start();c=await connect();const pending=ok(await c.call('action_receipt',{request_id:'submit-R15158'})).receipt;assert.equal(pending.state,'returned');assert.equal(pending.submission_context.job_id,'R15158');assert.equal(counts.R15157,1);assert.equal(counts.R15158,1);
 console.log(JSON.stringify({passed:true,oneDispatchPerApplication:true,noGuessedText:true,pendingFenceSurvivesRestart:true,independentProof:true,descriptionSpoofRejected:true,validationFailureNotSuccess:true}));
}finally{c?.close();daemon?.kill('SIGTERM');await sleep(250);if(daemon?.exitCode===null&&daemon?.signalCode===null)daemon.kill('SIGKILL');await new Promise(r=>site.close(r));}
