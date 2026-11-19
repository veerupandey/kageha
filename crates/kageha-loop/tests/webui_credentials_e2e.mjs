// Native Rust chat host -> standalone Rust browser -> HTTPS account fixture.
// No Python, real user accounts, model API keys, or plaintext credential outputs.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import http from 'node:http';
import https from 'node:https';
import {createHash} from 'node:crypto';
import {spawn,execFileSync} from 'node:child_process';
const app=process.env.KAGEHA_LOOP_BIN,browser=process.env.KAGEHA_BROWSER_BIN;
const credentialService=process.env.KAGEHA_TEST_BROWSER_CREDENTIAL_SERVICE==='1';
assert(app&&browser,'Set absolute KAGEHA_LOOP_BIN and KAGEHA_BROWSER_BIN');
const root=fs.mkdtempSync('/tmp/kageha-native-credentials-');
const credentialKey=root+'/credential-master.key';fs.writeFileSync(credentialKey,crypto.getRandomValues(new Uint8Array(32)),{mode:0o600});
const credentialSocket=root+'/credential-service.sock';const serviceToken=crypto.randomUUID()+crypto.randomUUID();const serviceTokenFile=root+'/browser-client.token';fs.writeFileSync(serviceTokenFile,serviceToken,{mode:0o600});const serviceConnection=root+'/credential-connection.json';fs.writeFileSync(serviceConnection,JSON.stringify({transport:'unix',socket:credentialSocket,token_file:serviceTokenFile}),{mode:0o600});
const children=[];const token='native-credential-fixture-authentication-token';
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
async function until(fn,ms=45000){const end=Date.now()+ms;let last;while(Date.now()<end){try{const v=await fn();if(v)return v;}catch(e){last=e;}await sleep(40);}throw last??Error('Timed out');}
function start(bin,args,env){const err=path.join(root,`${children.length}.err`);const fd=fs.openSync(err,'a');const child=spawn(bin,args,{env,stdio:['ignore',fd,fd]});fs.closeSync(fd);children.push(child);return err;}
execFileSync('/usr/bin/openssl',['req','-x509','-newkey','rsa:2048','-nodes','-keyout',root+'/key','-out',root+'/cert','-subj','/CN=localhost','-days','1'],{stdio:'ignore'});
const spki=execFileSync('/bin/sh',['-c','openssl x509 -in "$1" -pubkey -noout | openssl pkey -pubin -outform der | openssl dgst -sha256 -binary | openssl base64','fixture',root+'/cert'],{encoding:'utf8'}).trim();
const chrome='/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const wrapper=root+'/chromium';fs.writeFileSync(wrapper,`#!/bin/sh\nexec '${chrome}' --ignore-certificate-errors-spki-list=${spki} "$@"\n`,{mode:0o700});
let commits=0,modelCalls=0,passed=false;
const site=https.createServer({key:fs.readFileSync(root+'/key'),cert:fs.readFileSync(root+'/cert')},(req,res)=>{
 if(req.url==='/commit'){commits++;res.end('ok');return;}
 res.setHeader('Content-Type','text/html');res.end(`<label>New Password<input type="password" id="first" aria-label="New Password"></label><label>Confirm Password<input type="password" id="second" aria-label="Confirm Password"></label><button onclick="if(first.value===second.value&&first.value.length>=28){fetch('/commit');this.textContent='Account ready'}">Save password</button>`);
});
await new Promise(r=>site.listen(0,'127.0.0.1',r));const url=`https://127.0.0.1:${site.address().port}/`;
const model=http.createServer(async(req,res)=>{
 let raw='';for await(const b of req)raw+=b;
 assert(!raw.includes('Aa1!'),'Password exposed to model');
 const payload=JSON.parse(raw);modelCalls++;
 assert(payload.tools.some(t=>t.function.name==='browser_credential'));
 assert(payload.tools.some(t=>t.function.name==='browser_fill_credential'));
 const last=payload.messages.at(-1),intent=payload.messages.filter(m=>m.role==='user').at(-1).content;
 let call,answer='';
 if(last.role==='tool'){
  if(last.name==='browser_snapshot'||(last.content?.includes('"observation_id"')&&/^(first|confirm|save)/.test(intent))){
   const snap=JSON.parse(last.content),target=snap.controls.find(c=>c.name===(intent.startsWith('first')?'New Password':intent.startsWith('confirm')?'Confirm Password':'Save password'));
   assert(target);
   call=intent.startsWith('save')?{name:'browser_click',arguments:{target:target.ref,observation_id:snap.observation_id}}:
    {name:'browser_fill_credential',arguments:{target:target.ref,observation_id:snap.observation_id,credential:intent.split(' ')[1]}};
  }else answer=last.content;
 }else if(intent==='open')call={name:'browser_open',arguments:{url}};
 else if(intent==='credential')call={name:'browser_credential',arguments:{account:'fixture@example.com'}};
 else if(intent==='receipts')call={name:'browser_receipts',arguments:{}};
 else if(intent==='inspect')call={name:'browser_inspect',arguments:{selector:'button'}};
 else if(intent==='tabs')call={name:'browser_tabs',arguments:{}};
 else if(intent.startsWith('tabs '))call={name:'browser_tabs',arguments:{tab_handle:intent.slice(5)}};
 else if(intent.startsWith('reconcile '))call={name:'browser_reconcile',arguments:{request_id:intent.slice(10)}};
 else call={name:'browser_snapshot',arguments:{}};
 const message=call?{role:'assistant',content:null,tool_calls:[{id:`call-${modelCalls}`,type:'function',function:{name:call.name,arguments:JSON.stringify(call.arguments)}}]}:{role:'assistant',content:answer};
 res.setHeader('Content-Type','application/json');res.end(JSON.stringify({choices:[{message}]}));
});
await new Promise(r=>model.listen(0,'127.0.0.1',r));
let base,chat;
async function api(route,body){const response=await fetch(base+route,{method:body?'POST':'GET',headers:{Authorization:'Bearer '+token,...(body?{'Content-Type':'application/json'}:{})},body:body?JSON.stringify(body):undefined});const value=await response.json();assert.equal(response.status,200,JSON.stringify(value));return value;}
async function turn(message,browser_constraints=null,raw=false){let done=false;const result=api('/api/chat/send',{session_id:chat,client_request_id:'fixture-'+crypto.randomUUID(),message,browser_constraints}).finally(()=>done=true);result.catch(()=>{});await until(async()=>{if(done)return true;const s=await api('/api/sessions/'+chat);if(s.pending_approval)await api('/api/approvals',{approval_id:s.pending_approval.approval_id,approved:true});return done;});await result;const s=await api('/api/sessions/'+chat);const answer=s.messages.filter(m=>m.role==='assistant').at(-1).text;return raw?answer:JSON.parse(answer);}
try{
 fs.mkdirSync(root+'/workspace');
 const state=root+'/browser';fs.mkdirSync(state,{mode:0o700});
 const env={HOME:root,PATH:'/usr/bin:/bin',KAGEHA_BROWSER_CREDENTIAL_DIR:root+'/vault',KAGEHA_BROWSER_CREDENTIAL_KEY_FILE:credentialKey,KAGEHA_CREDENTIAL_KEY_FILE:credentialKey};
 start(browser,['--state-dir',state,'--instance-id','credential-fixture','--application-id','credential-fixture','--chromium',wrapper,'--standalone','on'],{...process.env,...(credentialService?{KAGEHA_BROWSER_CREDENTIAL_SERVICE:serviceConnection}:{}),KAGEHA_BROWSER_CREDENTIAL_DIR:root+'/vault',KAGEHA_BROWSER_CREDENTIAL_KEY_FILE:credentialKey,KAGEHA_CREDENTIAL_KEY_FILE:credentialKey});
 await until(()=>fs.existsSync(state+'/daemon.json'));
 const log=start(app,['serve',root+'/workspace',root+'/state','127.0.0.1:0'],{...env,KAGEHA_SERVER_TOKEN:token,KAGEHA_MODEL:'fixture',KAGEHA_PROVIDER_URL:`http://127.0.0.1:${model.address().port}/v1/chat/completions`,KAGEHA_BROWSER_STATE_DIR:state});
 base=await until(()=>{const m=fs.readFileSync(log,'utf8').match(/listening on ([^\s]+)/);return m&&'http://'+m[1];});
 chat=(await api('/api/sessions',{})).session_id;
 if(credentialService){const site=new URL(url).origin;const namespace='browser:'+createHash('sha256').update(JSON.stringify([chat,site])).digest('hex');const config=root+'/credentials.json';fs.writeFileSync(config,JSON.stringify({vault:root+'/service-vault.sqlite',key_id:'browser',key_files:{browser:credentialKey},socket:credentialSocket,grants:[{principal:'browser-device',namespace,ids:['*'],purposes:['password_fill'],destinations:[site],permissions:['metadata','resolve','write']}],clients:[{principal:'browser-device',authentication_generation:1,uid:process.getuid(),local_token_sha256:createHash('sha256').update(serviceToken).digest('hex')}]}),{mode:0o600});start(process.env.KAGEHA_CREDENTIALS_BIN||path.resolve('target/debug/kageha-credentials'),['serve','--config',config],{PATH:''});await until(()=>fs.existsSync(credentialSocket));}

 await turn('open');
 const selected=await turn('tabs');assert.equal(selected.tabs.length,1);
 const observed=await turn('tabs '+selected.target_id);assert(observed.observation_id);
 const created=await turn('credential');assert.equal(created.status,'completed');const credential=created.result.credential;assert.match(credential,/^cred_[0-9a-f]{64}$/);
 assert.equal((await turn('credential')).result.credential,credential);
 const blocked=await turn('first '+credential,{read_only_tabs:[selected.target_id],blocked_control_names:['Submit']},true);assert(blocked.includes('browser_task_scope_read_only'));
 if(fs.existsSync(state+'/action-receipts.json'))assert(!JSON.parse(fs.readFileSync(state+'/action-receipts.json','utf8')).some(r=>r.action==='fill'));

 for(const field of ['first','confirm']){const filled=await turn(field+' '+credential);assert.equal(filled.status,'completed');assert.equal(filled.result.steps[0].state,'verified');
  if(field==='first'){
   const journal=state+'/action-receipts.json';const rows=JSON.parse(fs.readFileSync(journal));
   const receipt=rows.find(r=>r.action==='fill'&&r.state==='verified');assert(receipt?.input_evidence);
   receipt.state='dispatching';receipt.result_document_id=null;fs.writeFileSync(journal,JSON.stringify(rows),{mode:0o600});
   const pending=await turn('receipts');assert.equal(pending.receipts.length,1);
   const recovered=await turn('reconcile '+receipt.request_id);
   assert.equal(recovered.postcondition_matches,true);assert.equal(recovered.input_sent,false);assert.equal(recovered.receipt.state,'verified');
  }
 }
 await turn('save',{blocked_control_names:['Submit']});await until(()=>commits===1);
 const inspected=await turn('inspect');assert.equal(inspected.matches.items[0].text,'Account ready');
 const session=await api('/api/sessions/'+chat);assert(!JSON.stringify(session).includes('Aa1!'));
 passed=true;console.log(JSON.stringify({native_credentials:true,credential_service:credentialService,native_receipt_recovery:true,native_dom_inspection:true,native_tab_selection:true,task_tab_fence:true,password_confirmation_verified:true,commits,plaintext_exposed_to_model:false,model_calls:modelCalls}));
}finally{
 if(!passed){console.error(JSON.stringify({modelCalls,commits,fixture:root}));for(const file of fs.readdirSync(root).filter(name=>name.endsWith('.err')))console.error(file,fs.readFileSync(path.join(root,file),'utf8').slice(-1000));}
 for(const child of children.toReversed())child.kill('SIGTERM');await sleep(200);
 for(const child of children)if(child.exitCode===null&&child.signalCode===null)child.kill('SIGKILL');
 site.closeAllConnections();model.closeAllConnections();await Promise.all([new Promise(r=>site.close(r)),new Promise(r=>model.close(r))]);
 fs.rmSync(root,{recursive:true,force:true});
}
