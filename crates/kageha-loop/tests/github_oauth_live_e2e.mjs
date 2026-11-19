// Opt-in live OAuth test. A person completes GitHub sign-in and consent in a
// browser; the authorization URL, code, and tokens never enter model context.
import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import {execFileSync, spawn} from 'node:child_process';

assert.equal(process.env.KAGEHA_GITHUB_OAUTH_LIVE, '1', 'Set KAGEHA_GITHUB_OAUTH_LIVE=1 for this live test');
const binary=process.env.KAGEHA_LOOP_BIN;
assert(binary && path.isAbsolute(binary), 'Set KAGEHA_LOOP_BIN to an absolute runtime binary path');
const login=execFileSync('gh',['api','user','--jq','.login'],{encoding:'utf8'}).trim();
assert(login.length>0,'A GitHub CLI login is required to identify the expected account');
const root=fs.mkdtempSync(path.join(os.tmpdir(),'kageha-live-oauth-'));
fs.mkdirSync(root+'/workspace');fs.writeFileSync(root+'/vault.key',crypto.randomBytes(32),{mode:0o600});
const hostToken=crypto.randomBytes(32).toString('hex'), route='/'+crypto.randomBytes(24).toString('hex');
const delay=ms=>new Promise(resolve=>setTimeout(resolve,ms));
let stopped=false;
for(const signal of ['SIGINT','SIGTERM']) process.once(signal,()=>{stopped=true;});
async function until(fn,timeout=90000){const end=Date.now()+timeout;let last;while(Date.now()<end && !stopped){try{const result=await fn();if(result)return result;}catch(error){last=error;}await delay(200);}throw stopped?new Error('Test cancelled'):last||new Error('Timed out');}
let serverName, modelSawAccount=false, authorizationUrl='', child, base, completed=false;
const model=http.createServer(async(req,res)=>{
  let body='';for await(const chunk of req)body+=chunk;
  try {
    const messages=JSON.parse(body).messages||[],last=messages.at(-1);
    const reflection=messages[0]?.content?.startsWith?.('You review the journal of a request');
    let message;
    if(reflection) message={role:'assistant',content:'{"lessons":[]}'};
    else if(last?.role==='tool'){modelSawAccount=String(last.content).toLowerCase().includes(login.toLowerCase())&&!/"isError"\s*:\s*true/.test(String(last.content));message={role:'assistant',content:'GitHub OAuth connection checked.'};}
    else message={role:'assistant',content:null,tool_calls:[{id:'live-oauth-read',type:'function',function:{name:'mcp_call',arguments:JSON.stringify({server:serverName,tool:'get_me',arguments:{}})}}]};
    res.setHeader('Content-Type','application/json');res.end(JSON.stringify({choices:[{message}]}));
  } catch {res.statusCode=500;res.end('{}');}
});
const handoff=http.createServer((req,res)=>{
  if(req.url!==route || !authorizationUrl){res.writeHead(404);res.end();return;}
  res.writeHead(302,{Location:authorizationUrl,'Cache-Control':'no-store','Referrer-Policy':'no-referrer'});res.end();
});
await Promise.all([model,handoff].map(server=>new Promise(resolve=>server.listen(0,'127.0.0.1',resolve))));
async function api(route,body,method){
  const response=await fetch(base+route,{method:method||(body?'POST':'GET'),headers:{Authorization:'Bearer '+hostToken,...(body?{'Content-Type':'application/json'}:{})},body:body?JSON.stringify(body):undefined});
  const data=await response.json();if(!response.ok)throw new Error(`${route} failed (${response.status}): ${String(data.error||'request failed').slice(0,300)}`);return data;
}
try{
  const log=root+'/host.log',fd=fs.openSync(log,'a');
  child=spawn(binary,['serve',root+'/workspace',root+'/state','127.0.0.1:0'],{env:{...process.env,HOME:root,KAGEHA_HOME:root+'/home',KAGEHA_CREDENTIAL_KEY_FILE:root+'/vault.key',KAGEHA_SERVER_TOKEN:hostToken,KAGEHA_MODEL:'fixture',KAGEHA_PROVIDER_URL:`http://127.0.0.1:${model.address().port}/v1/chat/completions`},stdio:['ignore',fd,fd]});fs.closeSync(fd);
  base=await until(()=>{if(child.exitCode!==null)throw new Error('Runtime exited during startup');const match=fs.readFileSync(log,'utf8').match(/listening on ([^\s]+)/);return match&&'http://'+match[1];});
  const preview=await api('/api/plugins/preview',{name:'github'});
  const target=preview.mcp.find(server=>server.transport==='http' && server.endpoint?.startsWith('https://api.githubcopilot.com/') && server.oauth);
  assert(target,'Official GitHub plugin OAuth server missing');serverName=target.name;
  const installed=await api('/api/plugins/install',{preview_id:preview.preview_id,approved_summary:preview});assert.equal(installed.plugin.status,'installed');
  const started=await api('/api/mcp/oauth/start',{name:serverName});authorizationUrl=started.authorization_url;
  assert(new URL(authorizationUrl).hostname==='github.com','Unexpected authorization provider');
  console.log(JSON.stringify({handoff:`http://127.0.0.1:${handoff.address().port}${route}`,state:'awaiting_user'}));
  await until(async()=>{const status=await api('/api/mcp/oauth/status',{name:serverName});if(status.status==='ready'&&!status.pending)return true;return false;},600000);
  authorizationUrl='';
  const connected=await api('/api/mcp',{name:serverName,enabled:true,read_only_tools:['get_me']});
  assert(connected.tools.some(tool=>tool.name==='get_me'),'GitHub MCP did not advertise get_me');
  const session=(await api('/api/sessions',{})).session_id;
  let done=false;const turn=api('/api/chat/send',{session_id:session,client_request_id:crypto.randomUUID(),message:'Use the GitHub plugin to check the connected account.'}).finally(()=>{done=true;});turn.catch(()=>{});
  await until(async()=>{if(done)return true;const state=await api('/api/sessions/'+session);if(state.pending_approval)await api('/api/approvals',{approval_id:state.pending_approval.approval_id,approved:true});return done;});await turn;
  assert(modelSawAccount,'The live OAuth MCP call did not return the expected GitHub account');
  await api('/api/mcp/oauth/disconnect',{name:serverName});
  const status=await api('/api/mcp/oauth/status',{name:serverName});assert.equal(status.status,'not_connected');assert.equal(status.enabled,false);
  completed=true;console.log(JSON.stringify({official_plugin_installed:true,live_oauth:true,agent_read_succeeded:true,local_grant_removed:true}));
} finally {
  authorizationUrl='';if(child){child.kill('SIGTERM');await delay(150);if(child.exitCode===null)child.kill('SIGKILL');}
  for(const server of [model,handoff])server.closeAllConnections();await Promise.all([model,handoff].map(server=>new Promise(resolve=>server.close(resolve))));
  fs.rmSync(root,{recursive:true,force:true});if(!completed)console.error('OAuth test did not complete; temporary runtime state was removed.');
}
