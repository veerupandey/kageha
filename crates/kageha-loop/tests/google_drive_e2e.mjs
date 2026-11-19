// Native Drive OAuth, granted read, and revocation against disposable HTTPS fixture.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import https from 'node:https';
import http from 'node:http';
import crypto from 'node:crypto';
import {spawn,execFileSync} from 'node:child_process';
const app=process.env.KAGEHA_LOOP_BIN;
assert(app,'Set KAGEHA_LOOP_BIN to an absolute path');
const root=fs.mkdtempSync('/tmp/kageha-drive-e2e-');
const key=root+'/vault.key';fs.writeFileSync(key,crypto.randomBytes(32),{mode:0o600});
execFileSync('/usr/bin/openssl',['req','-x509','-newkey','rsa:2048','-nodes','-keyout',root+'/key','-out',root+'/cert','-subj','/CN=localhost','-addext','subjectAltName=IP:127.0.0.1','-days','1'],{stdio:'ignore'});
const token='drive-test-server-token-123456789';
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
async function until(f,ms=45000){const end=Date.now()+ms;let error;while(Date.now()<end){try{const v=await f();if(v)return v}catch(e){error=e}await sleep(50)}throw error||Error('Timed out')}
let requests=[], seenDriveSchema=false;
const google=https.createServer({key:fs.readFileSync(root+'/key'),cert:fs.readFileSync(root+'/cert')},async(req,res)=>{
 const u=new URL(req.url,'https://127.0.0.1');requests.push(u.pathname);res.setHeader('Content-Type','application/json');
 if(u.pathname==='/token'){let data='';for await(const b of req)data+=b;assert.equal(new URLSearchParams(data).get('code'),'test-code');return res.end(JSON.stringify({access_token:'drive-token-canary',expires_in:3600}));}
 assert.equal(req.headers.authorization,'Bearer drive-token-canary');
 if(u.pathname==='/profile')return res.end(JSON.stringify({sub:'account-1',email:'owner@example.test'}));
 if(u.pathname==='/drive/files'){assert.equal(u.searchParams.get('pageSize'),'10');return res.end(JSON.stringify({files:[{id:'file-1',name:'Fixture document'}]}));}
 res.statusCode=404;res.end('{}');
});await new Promise(r=>google.listen(0,'127.0.0.1',r));const origin=`https://127.0.0.1:${google.address().port}`;
const model=http.createServer(async(req,res)=>{let raw='';for await(const b of req)raw+=b;const p=JSON.parse(raw);const last=p.messages.at(-1);let message;
 const spec=p.tools?.find(tool=>tool.function?.name==='integration_read');
 if(spec?.function?.parameters?.properties?.integration?.enum?.includes('google-drive'))seenDriveSchema=true;
 if(last.role==='tool')message={role:'assistant',content:last.content};
 else message={role:'assistant',content:null,tool_calls:[{id:'drive-call',type:'function',function:{name:'integration_read',arguments:JSON.stringify({integration:'google-drive',path:'files?pageSize=10'})}}]};
 res.setHeader('Content-Type','application/json');res.end(JSON.stringify({choices:[{message}]}));});await new Promise(r=>model.listen(0,'127.0.0.1',r));
let child,base,passed=false;
async function api(route,body,method){const response=await fetch(base+route,{method:method||(body?'POST':'GET'),headers:{Authorization:'Bearer '+token,...(body?{'Content-Type':'application/json'}:{})},body:body?JSON.stringify(body):undefined});const value=await response.json();assert.equal(response.status,200,JSON.stringify(value));return value}
try{
 fs.mkdirSync(root+'/workspace');const log=root+'/host.log';const fd=fs.openSync(log,'a');
 child=spawn(app,['serve',root+'/workspace',root+'/state','127.0.0.1:0'],{env:{HOME:root,PATH:'/usr/bin:/bin',KAGEHA_HOME:root+'/home',KAGEHA_CREDENTIAL_KEY_FILE:key,KAGEHA_SERVER_TOKEN:token,KAGEHA_MODEL:'fixture',KAGEHA_PROVIDER_URL:`http://127.0.0.1:${model.address().port}/v1/chat/completions`,CURL_CA_BUNDLE:root+'/cert',KAGEHA_OAUTH_GOOGLE_DRIVE_AUTHORIZE_URL:origin+'/authorize',KAGEHA_OAUTH_GOOGLE_DRIVE_TOKEN_URL:origin+'/token',KAGEHA_OAUTH_GOOGLE_DRIVE_PROFILE_URL:origin+'/profile',KAGEHA_OAUTH_GOOGLE_DRIVE_API_URL:origin+'/drive'},stdio:['ignore',fd,fd]});fs.closeSync(fd);
 base=await until(()=>{if(child.exitCode!==null)throw Error(fs.readFileSync(log,'utf8'));const m=fs.readFileSync(log,'utf8').match(/listening on ([^\s]+)/);return m&&'http://'+m[1]});
 await api('/api/integrations/google-drive/setup',{client_id:'fixture-client',redirect_uri:base+'/api/integrations/google-drive/callback'},'PUT');
 const flow=await api('/api/integrations/google-drive/connect',{});const auth=new URL(flow.authorization_url);assert(auth.searchParams.get('scope').includes('drive.metadata.readonly'));
 await api('/api/integrations/google-drive/callback?state='+auth.searchParams.get('state')+'&code=test-code');
 const session=await api('/api/sessions',{}),chat=session.session_id;
 async function turn(){let done=false;const pending=api('/api/chat/send',{session_id:chat,client_request_id:crypto.randomUUID(),message:'Read the fixture drive'}).finally(()=>done=true);pending.catch(()=>{});await until(async()=>{if(done)return true;const s=await api('/api/sessions/'+chat);if(s.pending_approval)await api('/api/approvals',{approval_id:s.pending_approval.approval_id,approved:true});return done});await pending;const state=await api('/api/sessions/'+chat);return state.messages.filter(m=>m.role==='assistant').at(-1).text;}
 assert((await turn()).includes('Fixture document'));assert(requests.includes('/drive/files'));assert(seenDriveSchema,'authorized Drive must appear in the model-facing tool schema');
 await api('/api/integrations/google-drive',undefined,'DELETE');const previous=requests.filter(x=>x.includes('/files')).length;
 const after=await turn();assert(!after.includes('Fixture document'));assert.equal(requests.filter(x=>x.includes('/files')).length,previous);
 passed=true;console.log(JSON.stringify({oauth:true,drive_metadata_read:true,revoked_read_blocked:true}));
}finally{if(!passed&&fs.existsSync(root+'/host.log'))console.error(fs.readFileSync(root+'/host.log','utf8').slice(-2000));if(child){child.kill('SIGTERM');await sleep(200);if(child.exitCode===null)child.kill('SIGKILL')}for(const s of [google,model])s.closeAllConnections();await Promise.all([google,model].map(s=>new Promise(r=>s.close(r))));fs.rmSync(root,{recursive:true,force:true});}
