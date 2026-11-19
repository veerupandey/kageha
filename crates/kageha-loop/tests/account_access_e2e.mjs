// Local HTTPS account + Gmail fixture, exercised through chat tools and a scripted model.
// Configure absolute paths: KAGEHA_LOOP_BIN and KAGEHA_BROWSER_BIN; no real accounts are used.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import http from 'node:http';
import https from 'node:https';
import crypto from 'node:crypto';
import {spawn,execFileSync} from 'node:child_process';

const app=process.env.KAGEHA_LOOP_BIN,browser=process.env.KAGEHA_BROWSER_BIN;
assert(app&&browser,'Set KAGEHA_LOOP_BIN and KAGEHA_BROWSER_BIN');
const root=fs.mkdtempSync('/tmp/kageha-account-e2e-'), children=[], token='account-fixture-authentication-token-123456';
const credentialService=process.env.KAGEHA_TEST_BROWSER_CREDENTIAL_SERVICE==='1';
const socket=root+'/credential.sock',serviceToken=crypto.randomUUID()+crypto.randomUUID(),tokenFile=root+'/service.token',connectionFile=root+'/connection.json';
fs.writeFileSync(tokenFile,serviceToken,{mode:0o600});fs.writeFileSync(connectionFile,JSON.stringify({transport:'unix',socket,token_file:tokenFile}),{mode:0o600});
const key=root+'/vault.key';fs.writeFileSync(key,crypto.randomBytes(32),{mode:0o600});
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
async function until(fn,ms=45000){const end=Date.now()+ms;let last;while(Date.now()<end){try{const v=await fn();if(v)return v}catch(e){last=e}await sleep(50)}throw last??Error('Timed out')}
function start(bin,args,env){const log=path.join(root,`${children.length}.log`),fd=fs.openSync(log,'a');const child=spawn(bin,args,{env,stdio:['ignore',fd,fd]});fs.closeSync(fd);children.push(child);return log}
execFileSync('/usr/bin/openssl',['req','-x509','-newkey','rsa:2048','-nodes','-keyout',root+'/key','-out',root+'/cert','-subj','/CN=localhost','-addext','subjectAltName=IP:127.0.0.1','-days','1'],{stdio:'ignore'});
const spki=execFileSync('/bin/sh',['-c','openssl x509 -in "$1" -pubkey -noout | openssl pkey -pubin -outform der | openssl dgst -sha256 -binary | openssl base64','fixture',root+'/cert'],{encoding:'utf8'}).trim();
const chrome=process.env.KAGEHA_TEST_CHROMIUM||'/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const wrapper=root+'/chromium';fs.writeFileSync(wrapper,`#!/bin/sh\nexec '${chrome.replaceAll("'","'\\''")}' --ignore-certificate-errors-spki-list=${spki} "$@"\n`,{mode:0o700});
const signupCode='736584',resetCode='946278',account='fixture@example.test';
const splitOtp=process.env.KAGEHA_FIXTURE_SPLIT_OTP==='1', breakOtp=process.env.KAGEHA_FIXTURE_BREAK_OTP==='1';
assert(!breakOtp || splitOtp);
let currentPassword=null,signupPassword=null,resetPassword=null,currentCode=null,currentSent=0,siteOrigin='',attempts=[],logins=[],models=[],otpReceipts=[],partialInputs=0,passed=false;
const site=https.createServer({key:fs.readFileSync(root+'/key'),cert:fs.readFileSync(root+'/cert')},async(req,res)=>{
 const url=new URL(req.url,siteOrigin||'https://localhost');
 if(url.pathname==='/partial-input'){partialInputs++;res.end('ok');return}
 if(url.pathname==='/send-code'){currentCode=url.searchParams.get('mode')==='signup'?signupCode:resetCode;currentSent=Date.now();res.end('sent');return}
 if(url.pathname==='/submit'){
  let raw='';for await(const b of req)raw+=b;const body=JSON.parse(raw);
  const ok=body.code===currentCode&&body.password===body.confirm&&body.password.length>=28&&
    (body.mode==='signup'?!currentPassword:body.password!==currentPassword);
  attempts.push({mode:body.mode,ok});
  if(ok){currentPassword=body.password;if(body.mode==='signup')signupPassword=body.password;else resetPassword=body.password;currentCode=null}
  res.setHeader('Content-Type','application/json');res.end(JSON.stringify({ok}));return;
 }
 if(url.pathname==='/login'){
  let raw='';for await(const b of req)raw+=b;const body=JSON.parse(raw);const ok=body.password===currentPassword;logins.push(ok);res.end(JSON.stringify({ok}));return;
 }
 if(url.pathname==='/signin'){res.setHeader('Content-Type','text/html');res.end(`<!doctype html><title>Sign in</title><label>Password<input type="password" id="first" aria-label="New Password"></label><button id="save" onclick="signin()">Submit</button><p id="result"></p><script>async function signin(){let r=await fetch('/login',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({password:first.value})});result.textContent=(await r.json()).ok?'Signed in':'Rejected'}</script>`);return}
 const mode=url.pathname==='/signup'?'signup':'reset';
 const codeFields=splitOtp?`<div role="group" aria-label="Verification code">${Array.from({length:6},(_,i)=>`<input type="text" maxlength="1" class="otp-part" aria-label="${i===0?'Verification Code':`Code digit ${i+1}`}" ${breakOtp&&i===0?`oninput="this.parentElement.children[1].disabled=true;setTimeout(()=>this.parentElement.children[1].disabled=false,2000);fetch('/partial-input')"`:''}>`).join('')}</div>`:`<label>Verification Code<input type="text" id="code" aria-label="Verification Code"></label>`;
 const codeValue=splitOtp?"Array.from(document.querySelectorAll('.otp-part'), e => e.value).join('')":"code.value";
 res.setHeader('Content-Type','text/html');res.end(`<!doctype html><title>Account ${mode}</title>
 <label>New Password<input type="password" id="first" aria-label="New Password"></label>
 <label>Confirm Password<input type="password" id="second" aria-label="Confirm Password"></label>
 ${codeFields}
 <button id="send" onclick="fetch('/send-code?mode=${mode}')">Send code</button><button id="save" onclick="save()">Submit</button><p id="result"></p>
 <script>async function save(){let r=await fetch('/submit',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({mode:'${mode}',password:first.value,confirm:second.value,code:${codeValue}})});result.textContent=(await r.json()).ok?'Account ready':'Rejected'}</script>`);
});
await new Promise(r=>site.listen(0,'127.0.0.1',r));siteOrigin=`https://127.0.0.1:${site.address().port}`;
// OAuth and Gmail endpoints; the code appears only in the simulated mailbox response.
const mail=https.createServer({key:fs.readFileSync(root+'/key'),cert:fs.readFileSync(root+'/cert')},async(req,res)=>{
 res.setHeader('Content-Type','application/json');
 const u=new URL(req.url,'http://localhost');
 if(u.pathname==='/oauth/token')return res.end(JSON.stringify({access_token:'fixture-access-token',expires_in:3600}));
 if(u.pathname==='/profile')return res.end(JSON.stringify({emailAddress:account,id:'fixture-account'}));
 assert.equal(req.headers.authorization,'Bearer fixture-access-token');
 if(u.pathname==='/messages')return res.end(JSON.stringify({messages:currentCode?[{id:'message1',threadId:'thread1'}]:[]}));
 if(u.pathname==='/messages/message1')return res.end(JSON.stringify({id:'message1',internalDate:String(currentSent),snippet:`Your verification code is ${currentCode}`,payload:{mimeType:'text/plain',headers:[{name:'From',value:'accounts@127.0.0.1'},{name:'To',value:account},{name:'Subject',value:'Your verification code'},{name:'Authentication-Results',value:'mx.google.com; dmarc=pass header.from=127.0.0.1'}],body:{data:Buffer.from(`Your verification code is ${currentCode}`).toString('base64url')}}}));
 res.statusCode=404;res.end('{}');
});await new Promise(r=>mail.listen(0,'127.0.0.1',r));const mailBase=`https://127.0.0.1:${mail.address().port}`;
let modelCalls=0;
const model=http.createServer(async(req,res)=>{
 let raw='';for await(const b of req)raw+=b;models.push(raw);const payload=JSON.parse(raw);modelCalls++;
 const userIndex=payload.messages.findLastIndex(m=>m.role==='user');const intent=payload.messages[userIndex].content,last=payload.messages.at(-1);let call,answer='';
 const needsRecord=/^(first|confirm|code|send|submit)( |$)/.test(intent);
 const afterUser=payload.messages.slice(userIndex+1);const recorded=afterUser.some(m=>m.role==='assistant'&&m.tool_calls?.some(c=>c.function.name==='record_required_outcomes'));
 const justRecorded=last.role==='tool'&&payload.messages.at(-2)?.tool_calls?.some(c=>c.function.name==='record_required_outcomes');
 if(needsRecord&&!recorded)call={name:'record_required_outcomes',arguments:{outcomes:[{id:'response',description:'Respond with the observed result',kind:'answer',verify:'Return tool result'}]}};
 else if(justRecorded)call={name:'browser_snapshot',arguments:{}};
 else if(last.role==='tool'){
  if(last.name==='browser_snapshot'||(last.content?.includes('\"observation_id\"')&&/^(first|confirm|code|send|submit)/.test(intent))){
   const snap=JSON.parse(last.content);const [action,ref]=intent.split(' ');
   const label=action==='first'?'New Password':action==='confirm'?'Confirm Password':action==='code'?'Verification Code':action==='send'?'Send code':'Submit';
   const target=snap.controls.find(c=>c.name===label);assert(target,JSON.stringify(snap));
   if(action==='first'||action==='confirm')call={name:'browser_fill_credential',arguments:{target:target.ref,observation_id:snap.observation_id,credential:ref}};
   else if(action==='code')call={name:'browser_fill_email_code',arguments:{target:target.ref,observation_id:snap.observation_id,sender:'accounts@127.0.0.1',recipient:account,attempt_id:ref}};
   else call={name:'browser_click',arguments:{target:target.ref,observation_id:snap.observation_id}};
  }else answer=last.content;
 }else if(intent.startsWith('open '))call={name:'browser_open',arguments:{url:siteOrigin+'/'+intent.slice(5)}};
 else if(intent==='start')call={name:'browser_email_code_start',arguments:{}};
 else if(intent==='credential')call={name:'browser_credential',arguments:{account}};
 else if(intent==='rotate')call={name:'browser_credential',arguments:{account,operation:'stage_rotation'}};
 else if(intent.startsWith('commit '))call={name:'browser_credential',arguments:{account,operation:'commit_rotation',credential:intent.slice(7)}};
 else if(intent==='inspect')call={name:'browser_inspect',arguments:{selector:'#result'}};
 else call={name:'browser_snapshot',arguments:{}};
 const message=call?{role:'assistant',content:null,tool_calls:[{id:`call-${modelCalls}`,type:'function',function:{name:call.name,arguments:JSON.stringify(call.arguments)}}]}:{role:'assistant',content:answer};
 res.setHeader('Content-Type','application/json');res.end(JSON.stringify({choices:[{message}]}));
});await new Promise(r=>model.listen(0,'127.0.0.1',r));
let base,chat;
async function api(route,body,method){const response=await fetch(base+route,{method:method||(body?'POST':'GET'),headers:{Authorization:'Bearer '+token,...(body?{'Content-Type':'application/json'}:{})},body:body?JSON.stringify(body):undefined});const value=await response.json();assert.equal(response.status,200,JSON.stringify(value));return value}
async function turn(message,raw=false){let done=false;const result=api('/api/chat/send',{session_id:chat,client_request_id:'fixture-'+crypto.randomUUID(),message}).finally(()=>done=true);result.catch(()=>{});await until(async()=>{if(done)return true;const s=await api('/api/sessions/'+chat);if(s.pending_approval)await api('/api/approvals',{approval_id:s.pending_approval.approval_id,approved:true});return done});await result;const s=await api('/api/sessions/'+chat);const answer=s.messages.filter(m=>m.role==='assistant').at(-1).text;return raw?answer:JSON.parse(answer)}
try{
 fs.mkdirSync(root+'/workspace');const state=root+'/browser';fs.mkdirSync(state,{mode:0o700});
 start(browser,['--state-dir',state,'--instance-id','account-fixture','--application-id','account-fixture','--chromium',wrapper,'--standalone','on'],{...process.env,...(credentialService?{KAGEHA_BROWSER_CREDENTIAL_SERVICE:connectionFile}:{}),KAGEHA_BROWSER_CREDENTIAL_DIR:root+'/vault',KAGEHA_BROWSER_CREDENTIAL_KEY_FILE:key,KAGEHA_CREDENTIAL_KEY_FILE:key});await until(()=>fs.existsSync(state+'/daemon.json'));
 const env={HOME:root,PATH:'/usr/bin:/bin',KAGEHA_BROWSER_CREDENTIAL_DIR:root+'/vault',KAGEHA_BROWSER_CREDENTIAL_KEY_FILE:key,KAGEHA_CREDENTIAL_KEY_FILE:key,KAGEHA_SERVER_TOKEN:token,KAGEHA_MODEL:'fixture',KAGEHA_PROVIDER_URL:`http://127.0.0.1:${model.address().port}/v1/chat/completions`,KAGEHA_BROWSER_STATE_DIR:state,CURL_CA_BUNDLE:root+'/cert',KAGEHA_OAUTH_GMAIL_AUTHORIZE_URL:mailBase+'/authorize',KAGEHA_OAUTH_GMAIL_TOKEN_URL:mailBase+'/oauth/token',KAGEHA_OAUTH_GMAIL_PROFILE_URL:mailBase+'/profile',KAGEHA_OAUTH_GMAIL_API_URL:mailBase};
 const log=start(app,['serve',root+'/workspace',root+'/state','127.0.0.1:0'],env);base=await until(()=>{const m=fs.readFileSync(log,'utf8').match(/listening on ([^\s]+)/);return m&&'http://'+m[1]});
 chat=(await api('/api/sessions',{})).session_id;
 if(credentialService){const namespace='browser:'+crypto.createHash('sha256').update(JSON.stringify([chat,siteOrigin])).digest('hex');const config=root+'/credentials.json';fs.writeFileSync(config,JSON.stringify({vault:root+'/service-vault.sqlite',key_id:'browser',key_files:{browser:key},socket,grants:[{principal:'browser-device',namespace,ids:['*'],purposes:['password_fill'],destinations:[siteOrigin],permissions:['metadata','resolve','write']}],clients:[{principal:'browser-device',authentication_generation:1,uid:process.getuid(),local_token_sha256:crypto.createHash('sha256').update(serviceToken).digest('hex')}]}),{mode:0o600});start(process.env.KAGEHA_CREDENTIALS_BIN||path.resolve('target/debug/kageha-credentials'),['serve','--config',config],{PATH:''});await until(()=>fs.existsSync(socket))}
 await api('/api/integrations/gmail/setup',{client_id:'fixture-client',client_secret:'fixture-client-secret',redirect_uri:base+'/api/integrations/gmail/callback'},'PUT');
 const oauth=await api('/api/integrations/gmail/connect',{}),oauthState=new URL(oauth.authorization_url).searchParams.get('state');
 await api('/api/integrations/gmail/callback?state='+oauthState+'&code=fixture-code');await api('/api/integrations/gmail/check',{});
 await turn('open signup');const signupAttempt=await turn('start');await turn('send');const created=await turn('credential');assert.equal(created.status,'completed');const old=created.result.credential;
 for(const field of ['first','confirm'])assert.equal((await turn(field+' '+old)).status,'completed');
 if(breakOtp) {
  const failed=await turn('code '+signupAttempt.attempt_id,true);assert(failed.includes('action_outcome_uncertain'), failed);
  await until(()=>partialInputs===1);
  const journal=JSON.parse(fs.readFileSync(state+'/action-receipts.json','utf8'));
  const pending=journal.filter(item=>item.action==='fill'&&['dispatching','returned'].includes(item.state));
  assert.equal(pending.length,1);assert(pending[0].input_evidence == null);
  const retry=await turn('code '+signupAttempt.attempt_id,true);assert(retry.includes('not_dispatched'),retry);
  assert.equal(partialInputs,1);assert(!JSON.stringify(journal).includes(signupCode));
  passed=true;console.log(JSON.stringify({split_partial_outcome_uncertain:true,retry_blocked:true,no_otp_digest:true}));
 } else {
 {const filled=await turn('code '+signupAttempt.attempt_id);assert.equal(filled.receipt?.state,'verified',JSON.stringify(filled));otpReceipts.push(filled.receipt.request_id)};await turn('submit');await until(()=>attempts.length===1);assert.equal(attempts[0].ok,true);assert(signupPassword);
 await turn('open reset');const resetAttempt=await turn('start');await turn('send');const candidate=await turn('rotate');assert.equal(candidate.status,'completed');const replacement=candidate.result.credential;assert.notEqual(replacement,old);
 // Failure before verification must not change the credential or the site's password.
 for(const field of ['first','confirm'])assert.equal((await turn(field+' '+replacement)).status,'completed');
 await turn('submit');await until(()=>attempts.length===2);assert.equal(attempts[1].ok,false);assert.equal((await turn('credential')).result.credential,old);assert.equal(currentPassword,signupPassword);
 await turn('open signin');assert.equal((await turn('first '+old)).status,'completed');await turn('submit');await until(()=>logins.length===1);assert.equal(logins[0],true);
 await turn('open reset');const retryAttempt=await turn('start');await turn('send');for(const field of ['first','confirm'])assert.equal((await turn(field+' '+replacement)).status,'completed');
 {const filled=await turn('code '+retryAttempt.attempt_id);assert.equal(filled.receipt?.state,'verified',JSON.stringify(filled));otpReceipts.push(filled.receipt.request_id)};await turn('submit');await until(()=>attempts.length===3);assert.equal(attempts[2].ok,true);assert.notEqual(resetPassword,signupPassword);
 // Until confirmation is explicitly committed, the canonical credential remains old even if the remote outcome is uncertain.
 assert.equal((await turn('credential')).result.credential,old);
 await turn('open signin');assert.equal((await turn('first '+old)).status,'completed');await turn('submit');await until(()=>logins.length===2);assert.equal(logins[1],false);
 const promoted=await turn('commit '+replacement);assert.equal(promoted.status,'completed');
 assert.deepEqual(promoted.result.commit_receipt,{state:'verified',operation:'commit_rotation',workspace_id:chat,origin:siteOrigin,account,candidate:replacement,active:old});
 assert.equal((await turn('credential')).result.credential,old);
 assert.equal((await turn('first '+old)).status,'completed');await turn('submit');await until(()=>logins.length===3);assert.equal(logins[2],true);
 const stored=await api('/api/sessions/'+chat);const leaks=[signupCode,resetCode,signupPassword,resetPassword];
 for(const [index,secret] of leaks.entries()){assert(!JSON.stringify(stored).includes(secret),`secret ${index} in session`);for(const [n,request] of models.entries()){const at=request.indexOf(secret);if(at>=0){const excerpt=request.slice(Math.max(0,at-150),at+secret.length+150).replaceAll(secret,'<redacted>');throw Error(`secret ${index} in model request ${n}: ${excerpt}`)}}}
 const journal=JSON.parse(fs.readFileSync(state+'/action-receipts.json','utf8'));
 for(const id of otpReceipts){const receipt=journal.find(row=>row.request_id===id);assert(receipt,`missing OTP receipt ${id}`);assert(receipt.input_evidence==null,'OTP receipt must not persist an offline-guessable digest')}
 // The app state, browser receipts and process logs are all potential durable leak paths.
 function scan(dir){for(const entry of fs.readdirSync(dir,{withFileTypes:true})){const name=path.join(dir,entry.name);if(entry.isDirectory()){if(!['chromium','Default','Cache','Code Cache'].includes(entry.name))scan(name)}else if(entry.isFile()&&fs.statSync(name).size<32*1024*1024){const bytes=fs.readFileSync(name);for(const secret of leaks)assert(!bytes.includes(secret),`secret in ${name}`)}}}
 scan(root+'/state');scan(root+'/workspace');scan(root+'/browser');for(const file of fs.readdirSync(root).filter(x=>x.endsWith('.log'))){const text=fs.readFileSync(path.join(root,file),'utf8');for(const secret of leaks)assert(!text.includes(secret),`secret in ${file}`)}
 passed=true;console.log(JSON.stringify({signup:true,failed_reset_preserved_old:true,reset:true,credential_service:credentialService,split_otp:splitOtp,verification_codes_protected:true,model_calls:modelCalls}));
 }
}finally{
 if(!passed){console.error(JSON.stringify({modelCalls,attempts,fixture:root}));for(const file of fs.readdirSync(root).filter(name=>name.endsWith('.log')))console.error(file,fs.readFileSync(path.join(root,file),'utf8').slice(-2000))}
 for(const child of children.toReversed())child.kill('SIGTERM');await sleep(200);for(const child of children)if(child.exitCode===null&&child.signalCode===null)child.kill('SIGKILL');
 for(const server of [site,mail,model])server.closeAllConnections();await Promise.all([site,mail,model].map(server=>new Promise(r=>server.close(r))));fs.rmSync(root,{recursive:true,force:true});
}
