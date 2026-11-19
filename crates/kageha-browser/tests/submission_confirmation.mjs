// Genuine Chromium input followed by read-only independent confirmation, no submit replay.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import net from 'node:net';
import http from 'node:http';
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import {createInterface} from 'node:readline';
import {randomUUID} from 'node:crypto';
const root=fs.mkdtempSync('/tmp/kageha-confirmation-');fs.chmodSync(root,0o700);
const sleep=ms=>new Promise(r=>setTimeout(r,ms));let daemon;let submits=0;
const site=http.createServer((req,res)=>{
 if(req.url==='/submit'){submits++;res.end('ok');return;}
 const u=new URL(req.url,'http://fixture');const employer=u.searchParams.get('employer')||'Iris Software Inc.';const account=u.searchParams.get('account')||'Test Person';
 const confirmation=u.searchParams.has('confirmed')?'<section><h2>Application status</h2><p>Application submitted</p></section>':'<section id="status"></section>';
 res.setHeader('content-type','text/html');res.end(`<img data-test-account="${account}"><main><a href="/company/iris">${employer}</a><h1>AI Architect</h1>${confirmation}${u.searchParams.has('description')?'<section data-job-description><h2>Application status</h2><p>Application submitted</p></section>':''}${u.searchParams.has('modal')?'<div role="dialog"><h2>Your application was sent to '+employer+'!</h2></div>':''}<button id="submit" onclick="fetch('/submit');document.querySelector('#status').innerHTML='<h2>Application status</h2><p>Application submitted</p>'">Submit application</button><a href="/resume">View resume</a></main>`);
});await new Promise(r=>site.listen(0,'127.0.0.1',r));const url='http://127.0.0.1:'+site.address().port+'/jobs/view/4489586201/';
function start(binary=process.env.KAGEHA_BROWSER_BIN){const fd=fs.openSync(root+'/daemon.log','a',0o600);daemon=spawn(binary,['--state-dir',root+'/state','--instance-id','confirm','--application-id','fixture','--fixture-actions','on','--chromium',process.env.CHROMIUM_BIN||'/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'],{stdio:['ignore',fd,fd]});fs.closeSync(fd);}
async function ready(){for(let i=0;i<250;i++){if(fs.existsSync(root+'/state/daemon.json')&&fs.existsSync(root+'/state/control.sock'))return;assert.equal(daemon.exitCode,null);await sleep(30);}throw Error('startup');}
async function client(chat='chat'){
 const info=JSON.parse(fs.readFileSync(root+'/state/daemon.json'));const token=fs.readFileSync(root+'/state/auth.token','utf8');const socket=net.createConnection(root+'/state/control.sock');await once(socket,'connect');const reader=createInterface({input:socket});const lines=reader[Symbol.asyncIterator]();let epoch=null;
 async function call(method,params={},authority=null,id=randomUUID().replaceAll('-','')){socket.write(JSON.stringify({protocol_version:1,request_id:id,instance_id:info.instance_id,application_id:info.application_id,state_root:info.state_root,workspace_id:chat,client_id:'agent',service_epoch:epoch,deadline_ms:Date.now()+10000,expected_binding:null,authority,method,params})+'\n');const reply=JSON.parse((await lines.next()).value);assert.equal(reply.request_id,id);return reply;}
 const hello=await call('handshake',{token});assert.equal(hello.status,'ok');epoch=hello.service_epoch;
 return {call,close(){reader.close();socket.destroy();}};
}
const ok=r=>{assert.equal(r.status,'ok',JSON.stringify(r));return r.result;};
let c;
try{
 start();await ready();c=await client();const browser=ok(await c.call('launch_browser')).browser_id;
 const tab=ok(await c.call('open_tab',{browser_id:browser,url}));let selection=ok(await c.call('browser_selection'));
 let context;for(let i=0;i<30;i++){const r=await c.call('submission_context',{browser_id:browser,target_id:tab.target_id,binding_id:selection.binding_id});if(r.status==='ok'){context=r.result.context;break;}assert(['document_changed','cdp_invalid_reply','confirmation_identity_missing','confirmation_read_failed'].includes(r.error?.code));await sleep(50);}assert(context,'Fixture page did not finish loading');
 assert.equal(context.job_id,'4489586201');assert.equal(context.employer,'Iris Software Inc.');assert(context.account_digest);
 const authority=ok(await c.call('claim_control',{ttl_ms:60000})).authority;
 let obs=ok(await c.call('observe_tab',{browser_id:browser,target_id:tab.target_id,compact:false}));const field=obs.controls.find(c=>c.id==='submit');assert(field);
 const request='original-submission';const submitted=await c.call('click_observed',{browser_id:browser,target_id:tab.target_id,observation_id:obs.observation_id,target:field.ref,expected_selector:'[data-test-modal-title]',expected_text:'Application sent',timeout_ms:500,submission_context:context},authority,request);
 assert.equal(submitted.error?.code,'action_outcome_uncertain');assert.equal(ok(await c.call('action_receipt',{request_id:request})).receipt.state,'returned');assert.equal(submits,1);
 for(const suffix of ['?confirmed=1&account=Different','?confirmed=1&employer=Other','?confirmed=1','?description=1'].map((s,i)=>i===2?s.replace('confirmed=1','resume_only=1'):s)){
  const bad=ok(await c.call('open_tab',{browser_id:browser,url:url+suffix}));selection=ok(await c.call('browser_selection'));
  const rejected=await c.call('confirm_submission',{browser_id:browser,target_id:bad.target_id,binding_id:selection.binding_id,request_id:request});assert.equal(rejected.status,'error');assert.equal(submits,1);
 }
 const other=ok(await c.call('open_tab',{browser_id:browser,url:url+'?confirmed=1'+(process.env.KAGEHA_CONFIRM_MODAL?'&modal=1':'')}));selection=ok(await c.call('browser_selection'));
 const params={browser_id:browser,target_id:other.target_id,binding_id:selection.binding_id,request_id:request};
 const confirmed=ok(await c.call('confirm_submission',params));assert.equal(confirmed.receipt.action,'verify');assert.equal(confirmed.receipt.state,'verified');assert.equal(confirmed.input_sent,false);assert.equal(confirmed.original_receipt.state,'returned');assert.equal(confirmed.original_receipt.expected.text,'Application sent');
 assert.equal(confirmed.receipt.confirmation.kind,process.env.KAGEHA_CONFIRM_MODAL?'sent_modal':'application_status');assert.equal(confirmed.original_receipt.resolution.request_id,confirmed.receipt.request_id);
 const again=ok(await c.call('confirm_submission',params));assert.equal(again.receipt.request_id,confirmed.receipt.request_id);
 assert.equal(ok(await c.call('pending_actions',{browser_id:browser,target_id:tab.target_id})).receipts.length,0);assert.equal(submits,1);
 const stranger=await client('stranger');assert.notEqual((await stranger.call('action_receipt',{request_id:request})).status,'ok');stranger.close();
 c.close();c=null;daemon.kill('SIGTERM');await once(daemon,'exit');start(process.env.KAGEHA_FALLBACK_BIN||process.env.KAGEHA_BROWSER_BIN);await ready();c=await client();
 const retained=ok(await c.call('action_receipt',{request_id:request})).receipt;assert.equal(retained.resolution.request_id,confirmed.receipt.request_id);
 assert.equal(ok(await c.call('action_receipt',{request_id:confirmed.receipt.request_id})).receipt.confirmation.context.job_id,'4489586201');
 assert.equal(submits,1);console.log(JSON.stringify({passed:true,submissions:submits,readOnlyConfirmation:true,crossTab:true,restart:true,originalPreserved:true}));
}finally{c?.close();daemon?.kill('SIGTERM');await sleep(250);if(daemon?.exitCode===null&&daemon?.signalCode===null)daemon.kill('SIGKILL');await new Promise(r=>site.close(r));}
