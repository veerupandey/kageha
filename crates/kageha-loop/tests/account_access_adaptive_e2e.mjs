// Opt-in real-model account test. The provider relay forwards model requests unchanged; it never chooses actions.
// This uses only disposable local HTTPS account and Gmail fixtures, not real accounts.
// Configure absolute paths: KAGEHA_LOOP_BIN and KAGEHA_BROWSER_BIN; no real accounts are used.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import http from 'node:http';
import https from 'node:https';
import crypto from 'node:crypto';
import {spawn,execFileSync} from 'node:child_process';

const app=process.env.KAGEHA_LOOP_BIN,browser=process.env.KAGEHA_BROWSER_BIN;
const useCodex=process.env.KAGEHA_TEST_CODEX==='1';
const apiKey=process.env.KAGEHA_TEST_API_KEY||process.env.GOOGLE_API_KEY;
const providerUrl=process.env.KAGEHA_TEST_PROVIDER_URL||'https://generativelanguage.googleapis.com/v1beta/openai/chat/completions';
const modelName=process.env.KAGEHA_TEST_MODEL||(useCodex?'gpt-6-luna':'gemini-2.5-flash');
assert(useCodex||apiKey,'Configure KAGEHA_TEST_API_KEY or GOOGLE_API_KEY');
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
let currentPassword=null,signupPassword=null,resetPassword=null,currentCode=null,currentSent=0,siteOrigin='',attempts=[],logins=[],models=[],otpReceipts=[],passed=false;
const site=https.createServer({key:fs.readFileSync(root+'/key'),cert:fs.readFileSync(root+'/cert')},async(req,res)=>{
 const url=new URL(req.url,siteOrigin||'https://localhost');
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
 if(url.pathname==='/signin'){res.setHeader('Content-Type','text/html');res.end(`<!doctype html><title>Sign in</title><p>The exact successful sign-in confirmation is <code>Signed in.</code></p><label>Password<input type="password" id="first" aria-label="Password"></label><button id="save" onclick="signin()">Submit</button><p id="result"></p><script>async function signin(){let r=await fetch('/login',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({password:first.value})});result.textContent=(await r.json()).ok?'Signed in.':'Rejected'}</script>`);return}
 const mode=url.pathname==='/signup'?'signup':'reset';
 res.setHeader('Content-Type','text/html');res.end(`<!doctype html><title>Account ${mode}</title>
 <p>Account: fixture@example.test. The verification email comes from accounts@127.0.0.1. The exact sent confirmation is <code>Verification email sent.</code> The exact successful signup or reset confirmation is <code>Account ready.</code></p>
 <label>New Password<input type="password" id="first" aria-label="New Password"></label>
 <label>Confirm Password<input type="password" id="second" aria-label="Confirm Password"></label>
 <label>Verification Code<input type="text" id="code" aria-label="Verification Code"></label>
 <button id="send" onclick="fetch('/send-code?mode=${mode}').then(()=>{document.getElementById('result').textContent='Verification email sent.'})">Send code</button><button id="save" onclick="save()">Submit</button><p id="result"></p>
 <script>async function save(){let r=await fetch('/submit',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({mode:'${mode}',password:first.value,confirm:second.value,code:code.value})});result.textContent=(await r.json()).ok?'Account ready.':'Rejected'}</script>`);
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
let modelCalls=0;const providerStatuses=[],selectedTools=[],relayErrors=[];
const model=http.createServer(async(req,res)=>{
 try {
  let raw='';for await(const b of req)raw+=b;models.push(raw);modelCalls++;
  for(const secret of [signupCode,resetCode,signupPassword,resetPassword].filter(Boolean)) {
   if(raw.includes(secret)) throw Error('Fixture secret found in provider request; request was not forwarded');
  }
  if(req.url==='/codex-input'){res.statusCode=200;res.end('ok');return}
  const payload=JSON.parse(raw);payload.model=modelName;payload.stream=false;delete payload.stream_options;
  const upstream=await fetch(providerUrl,{method:'POST',headers:{Authorization:'Bearer '+apiKey,'Content-Type':'application/json'},body:JSON.stringify(payload),signal:AbortSignal.timeout(120000)});
  const body=await upstream.text();let value;try{value=JSON.parse(body)}catch{}
  providerStatuses.push({status:upstream.status,error:value?.error?.status||value?.error?.type,finish:value?.choices?.[0]?.finish_reason,content_length:value?.choices?.[0]?.message?.content?.length||0,tool_count:value?.choices?.[0]?.message?.tool_calls?.length||0,usage:value?.usage});
  const calls=value?.choices?.[0]?.message?.tool_calls||[];selectedTools.push(...calls.map(c=>c.function?.name).filter(Boolean));
  res.statusCode=upstream.status;res.setHeader('Content-Type','application/json');res.end(body);
 } catch(error){relayErrors.push(error.message);res.statusCode=502;res.end(JSON.stringify({error:{message:'Model relay failed'}}));}
});await new Promise(r=>model.listen(0,'127.0.0.1',r));
let base,chat;
async function api(route,body,method){
 const data=body?JSON.stringify(body):null;
 return await new Promise((resolve,reject)=>{
  const req=http.request(base+route,{method:method||(body?'POST':'GET'),headers:{Authorization:'Bearer '+token,...(data?{'Content-Type':'application/json','Content-Length':Buffer.byteLength(data)}:{})}},res=>{
   let raw='';res.setEncoding('utf8');res.on('data',b=>raw+=b);res.on('end',()=>{try{const value=JSON.parse(raw);assert.equal(res.statusCode,200,JSON.stringify(value));resolve(value)}catch(e){reject(e)}});
  });req.on('error',reject);if(data)req.write(data);req.end();
 });
}
async function turn(message){let done=false;const result=api('/api/chat/send',{session_id:chat,client_request_id:'fixture-'+crypto.randomUUID(),message,max_steps:80}).finally(()=>done=true);result.catch(()=>{});await until(async()=>{if(done)return true;const s=await api('/api/sessions/'+chat);if(s.pending_approval)await api('/api/approvals',{approval_id:s.pending_approval.approval_id,approved:true});return done},600000);await result;const s=await api('/api/sessions/'+chat);const answer=s.messages.filter(m=>m.role==='assistant').at(-1);return {text:answer?.text||'',status:answer?.result_status||'unknown'}}
try{
 fs.mkdirSync(root+'/workspace');const state=root+'/browser';fs.mkdirSync(state,{mode:0o700});
 start(browser,['--state-dir',state,'--instance-id','account-fixture','--application-id','account-fixture','--chromium',wrapper,'--standalone','on'],{HOME:process.env.HOME,PATH:'/usr/bin:/bin',TMPDIR:process.env.TMPDIR||'/tmp',...(credentialService?{KAGEHA_BROWSER_CREDENTIAL_SERVICE:connectionFile}:{}),KAGEHA_BROWSER_CREDENTIAL_DIR:root+'/vault',KAGEHA_BROWSER_CREDENTIAL_KEY_FILE:key,KAGEHA_CREDENTIAL_KEY_FILE:key});await until(()=>fs.existsSync(state+'/daemon.json'));
 const realCodex=useCodex?(process.env.KAGEHA_CODEX_BIN||execFileSync('/usr/bin/which',['codex'],{encoding:'utf8'}).trim()):'';
 const guardedCodex=root+'/guarded-codex.mjs';
 if(useCodex){
  const guardPort=model.address().port;
  fs.writeFileSync(guardedCodex,`#!/usr/bin/env node\nimport {spawn} from 'node:child_process';\nimport readline from 'node:readline';\nconst child=spawn(${JSON.stringify(realCodex)},process.argv.slice(2),{stdio:['pipe','pipe','pipe']});\nchild.stdout.pipe(process.stdout);child.stderr.pipe(process.stderr);\nchild.on('exit',(code,signal)=>process.exitCode=code??1);\nlet queue=Promise.resolve();\nreadline.createInterface({input:process.stdin}).on('line',line=>{queue=queue.then(async()=>{const obj=JSON.parse(line);if(obj.method==='turn/start'){const r=await fetch('http://127.0.0.1:${guardPort}/codex-input',{method:'POST',body:line});if(!r.ok)throw Error('Model request failed fixture secret guard');}child.stdin.write(line+'\\n');}).catch(()=>{child.kill();process.exitCode=1;});}).on('close',()=>{queue.finally(()=>child.stdin.end());});\n`,{mode:0o700});
 }
 const env={HOME:root,PATH:useCodex?process.env.PATH:'/usr/bin:/bin',...(useCodex?{CODEX_HOME:process.env.CODEX_HOME||path.join(process.env.HOME,'.codex'),KAGEHA_CODEX_BIN:guardedCodex}:{}),KAGEHA_BROWSER_CREDENTIAL_DIR:root+'/vault',KAGEHA_BROWSER_CREDENTIAL_KEY_FILE:key,KAGEHA_CREDENTIAL_KEY_FILE:key,KAGEHA_SERVER_TOKEN:token,KAGEHA_MODEL:useCodex?'adaptive-codex-fixture':modelName,...(useCodex?{}:{KAGEHA_PROVIDER_URL:`http://127.0.0.1:${model.address().port}/v1/chat/completions`}),KAGEHA_BROWSER_STATE_DIR:state,CURL_CA_BUNDLE:root+'/cert',KAGEHA_OAUTH_GMAIL_AUTHORIZE_URL:mailBase+'/authorize',KAGEHA_OAUTH_GMAIL_TOKEN_URL:mailBase+'/oauth/token',KAGEHA_OAUTH_GMAIL_PROFILE_URL:mailBase+'/profile',KAGEHA_OAUTH_GMAIL_API_URL:mailBase};
 if(useCodex){const catalog=root+'/models.yaml';fs.writeFileSync(catalog,`providers:\n  openai-codex-app:\n    protocol: codex_app_server\n    base_url: ''\nmodels:\n  - id: adaptive-codex-fixture\n    provider: openai-codex-app\n    model: ${modelName}\n    capabilities: [tool_calling, reasoning]\nroles: {default: [adaptive-codex-fixture]}\n`,{mode:0o600});env.KAGEHA_MODELS_FILE=catalog;env.KAGEHA_MODEL_ENV_FILE=root+'/empty.env';fs.writeFileSync(env.KAGEHA_MODEL_ENV_FILE,'',{mode:0o600})}
 const log=start(app,['serve',root+'/workspace',root+'/state','127.0.0.1:0'],env);base=await until(()=>{const m=fs.readFileSync(log,'utf8').match(/listening on ([^\s]+)/);return m&&'http://'+m[1]});
 await api('/api/agents',{id:'account-fixture',name:'Local account test',system_prompt:'Use only the local account fixture and connected test mailbox. Never navigate elsewhere.',tool_policy:{allow:['browser_open','browser_tabs','browser_snapshot','browser_inspect','browser_click','browser_fill','browser_credential','browser_fill_credential','browser_email_code_start','browser_fill_email_code','browser_receipts','browser_reconcile','integration_read','record_required_outcomes','record_outcome_evidence','report_blocker','amend_required_outcome','discover_capabilities'],deny:[]}});
 chat=(await api('/api/sessions',{persona_id:'account-fixture'})).session_id;
 if(credentialService){const namespace='browser:'+crypto.createHash('sha256').update(JSON.stringify([chat,siteOrigin])).digest('hex');const config=root+'/credentials.json';fs.writeFileSync(config,JSON.stringify({vault:root+'/service-vault.sqlite',key_id:'browser',key_files:{browser:key},socket,grants:[{principal:'browser-device',namespace,ids:['*'],purposes:['password_fill'],destinations:[siteOrigin],permissions:['metadata','resolve','write']}],clients:[{principal:'browser-device',authentication_generation:1,uid:process.getuid(),local_token_sha256:crypto.createHash('sha256').update(serviceToken).digest('hex')}]}),{mode:0o600});start(process.env.KAGEHA_CREDENTIALS_BIN||path.resolve('target/debug/kageha-credentials'),['serve','--config',config],{PATH:''});await until(()=>fs.existsSync(socket))}
 await api('/api/integrations/gmail/setup',{client_id:'fixture-client',client_secret:'fixture-client-secret',redirect_uri:base+'/api/integrations/gmail/callback'},'PUT');
 const oauth=await api('/api/integrations/gmail/connect',{}),oauthState=new URL(oauth.authorization_url).searchParams.get('state');
 await api('/api/integrations/gmail/callback?state='+oauthState+'&code=fixture-code');await api('/api/integrations/gmail/check',{});await api('/api/integrations/gmail/access',{agents:['account-fixture'],expected_agents:['main']},'PUT');
 const signupAnswer=await turn(`This is a disposable local test only. Create an account for ${account} at ${siteOrigin}/signup and finish verification using the connected test mailbox. Use the site's information about the sender. Confirm the account is created. Do not access external sites or real accounts.`);
 assert(attempts.some(a=>a.mode==='signup'&&a.ok),JSON.stringify({phase:'signup',answer:signupAnswer,tools:selectedTools,statuses:providerStatuses,relayErrors}));
 assert(signupPassword);assert.equal(signupAnswer.status,'success',JSON.stringify({phase:'signup_turn',answer:signupAnswer}));
 const resetAnswer=await turn(`This is a disposable local test only. Reset the password of the same ${account} account at ${siteOrigin}/reset, complete the email verification using the connected test mailbox, and retain the new credential after the reset succeeds. Do not access external sites or real accounts.`);
 assert(attempts.some(a=>a.mode==='reset'&&a.ok),JSON.stringify({phase:'reset',answer:resetAnswer,tools:selectedTools,statuses:providerStatuses,relayErrors}));
 assert(resetPassword&&resetPassword!==signupPassword);
 const signinAnswer=await turn(`Verify that the new password for ${account} signs in at ${siteOrigin}/signin using the saved credential. This is a disposable local test only; do not access external sites or real accounts.`);
 assert(logins.includes(true),JSON.stringify({phase:'signin',answer:signinAnswer,tools:selectedTools,statuses:providerStatuses,relayErrors}));
 const stored=await api('/api/sessions/'+chat);const leaks=[signupCode,resetCode,signupPassword,resetPassword];
 for(const [index,secret] of leaks.entries()){assert(!JSON.stringify(stored).includes(secret),`secret ${index} in session`);for(const [n,request] of models.entries()){const at=request.indexOf(secret);if(at>=0){const excerpt=request.slice(Math.max(0,at-150),at+secret.length+150).replaceAll(secret,'<redacted>');throw Error(`secret ${index} in model request ${n}: ${excerpt}`)}}}
 const journal=JSON.parse(fs.readFileSync(state+'/action-receipts.json','utf8'));
 const otpReceipts=journal.filter(row=>row.action==='fill'&&row.input_evidence==null);
 assert(otpReceipts.length>=2,'Expected protected verification receipts without value-derived evidence');
 // The app state, browser receipts and process logs are all potential durable leak paths.
 function scan(dir){for(const entry of fs.readdirSync(dir,{withFileTypes:true})){const name=path.join(dir,entry.name);if(entry.isDirectory()){if(!['chromium','Default','Cache','Code Cache'].includes(entry.name))scan(name)}else if(entry.isFile()&&fs.statSync(name).size<32*1024*1024){const bytes=fs.readFileSync(name);for(const secret of leaks)assert(!bytes.includes(secret),`secret in ${name}`)}}}
 scan(root+'/state');scan(root+'/workspace');scan(root+'/browser');for(const file of fs.readdirSync(root).filter(x=>x.endsWith('.log'))){const text=fs.readFileSync(path.join(root,file),'utf8');for(const secret of leaks)assert(!text.includes(secret),`secret in ${file}`)}
 assert.equal(resetAnswer.status,'success',JSON.stringify({phase:'reset_turn',answer:resetAnswer,signin_status:signinAnswer.status,signin_succeeded:logins.includes(true)}));
 assert.equal(signinAnswer.status,'success',JSON.stringify({phase:'signin_turn',answer:signinAnswer}));
 passed=true;console.log(JSON.stringify({signup:true,reset:true,signin:true,adaptive:true,provider:useCodex?'codex-app-server':new URL(providerUrl).host,model:modelName,fixture_secret_scan_passed:true,provider_request_scan:true,model_calls:modelCalls,tools:useCodex?undefined:selectedTools}));
}finally{
 if(!passed){
  const secrets=[signupCode,resetCode,signupPassword,resetPassword].filter(Boolean);
  let found=false;
  for(const request of models)for(const secret of secrets)if(request.includes(secret))found=true;
  const scanFailure=dir=>{if(!fs.existsSync(dir))return;for(const entry of fs.readdirSync(dir,{withFileTypes:true})){const f=path.join(dir,entry.name);if(entry.isDirectory()){if(!['chromium','Default','Cache','Code Cache'].includes(entry.name))scanFailure(f)}else if(entry.isFile()&&fs.statSync(f).size<32*1024*1024){const bytes=fs.readFileSync(f);for(const secret of secrets)if(bytes.includes(secret))found=true}}};
  for(const dir of ['state','workspace','browser'])scanFailure(root+'/'+dir);
  for(const file of fs.readdirSync(root).filter(name=>name.endsWith('.log'))){const bytes=fs.readFileSync(root+'/'+file);for(const secret of secrets)if(bytes.includes(secret))found=true}
  console.error(JSON.stringify({modelCalls,attempts,logins,tools:selectedTools,statuses:providerStatuses,relayErrors,fixture_secret_scan_passed:!found}));
  if(found)process.exitCode=1;
 }

 for(const child of children.toReversed())child.kill('SIGTERM');await sleep(200);for(const child of children)if(child.exitCode===null&&child.signalCode===null)child.kill('SIGKILL');
 for(const server of [site,mail,model])server.closeAllConnections();await Promise.all([site,mail,model].map(server=>new Promise(r=>server.close(r))));fs.rmSync(root,{recursive:true,force:true});
}
