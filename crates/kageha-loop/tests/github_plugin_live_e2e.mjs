// Opt-in live test of the official GitHub plugin, using the existing gh login.
// It makes only a read-only get_me MCP call and keeps all state in a temporary directory.
import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import {execFileSync, spawn} from 'node:child_process';

assert.equal(process.env.KAGEHA_GITHUB_MCP_LIVE, '1', 'Set KAGEHA_GITHUB_MCP_LIVE=1 for the live test');
const binary = process.env.KAGEHA_LOOP_BIN;
assert(binary && path.isAbsolute(binary), 'Set KAGEHA_LOOP_BIN to the absolute runtime binary path');
const credential = execFileSync('gh', ['auth', 'token'], {encoding:'utf8'}).trim();
assert(credential.length > 0, 'A gh login is required');
const login = execFileSync('gh', ['api', 'user', '--jq', '.login'], {encoding:'utf8'}).trim();
assert(login.length > 0, 'The GitHub account could not be identified');
const root = fs.mkdtempSync(path.join(os.tmpdir(), 'kageha-live-github-'));
fs.mkdirSync(root+'/workspace');
fs.writeFileSync(root+'/vault.key', crypto.randomBytes(32), {mode:0o600});
const apiToken = crypto.randomBytes(32).toString('hex');
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
async function until(fn, timeout=90000) {
  const end=Date.now()+timeout; let last;
  while(Date.now()<end) { try { const result=await fn(); if(result) return result; } catch(error) { last=error; } await delay(100); }
  throw last || new Error('Timed out');
}
let modelSawTool = false, validToolResult = false, leakedCredential = false;
const model = http.createServer(async (req,res) => {
  let raw=''; for await(const chunk of req) raw+=chunk;
  const request=JSON.parse(raw), messages=request.messages || [], last=messages.at(-1);
  const reflection=messages[0]?.content?.startsWith?.('You review the journal of a request');
  let message;
  if (reflection) message={role:'assistant',content:'{"lessons":[]}'};
  else if (last?.role==='tool') {
    leakedCredential ||= String(last.content).includes(credential);
    validToolResult ||= String(last.content).toLowerCase().includes(login.toLowerCase()) && !/"isError"\s*:\s*true/.test(String(last.content));
    modelSawTool=true;
    message={role:'assistant',content:'Live GitHub plugin connection verified.'};
  } else message={role:'assistant',content:null,tool_calls:[{id:'github-live-read',type:'function',function:{name:'mcp_call',arguments:JSON.stringify({server:serverName,tool:'get_me',arguments:{}})}}]};
  res.setHeader('Content-Type','application/json');res.end(JSON.stringify({choices:[{message}]}));
});
await new Promise(resolve => model.listen(0,'127.0.0.1',resolve));
let child, base, serverName, passed=false;
async function api(route, body, method) {
  const response=await fetch(base+route,{method:method||(body?'POST':'GET'),headers:{Authorization:'Bearer '+apiToken,...(body?{'Content-Type':'application/json'}:{})},body:body?JSON.stringify(body):undefined});
  const text=await response.text();
  assert(!text.includes(credential), 'Runtime response exposed the credential');
  let data; try {data=JSON.parse(text);} catch {throw new Error(`Invalid response from ${route} (${response.status})`);}
  if(!response.ok) throw new Error(`${route} failed (${response.status}): ${String(data.error || 'request failed').replaceAll(credential,'[redacted]')}`);
  return data;
}
try {
  const log=root+'/host.log', fd=fs.openSync(log,'a');
  child=spawn(binary,['serve',root+'/workspace',root+'/state','127.0.0.1:0'],{env:{...process.env,HOME:root,KAGEHA_HOME:root+'/home',KAGEHA_CREDENTIAL_KEY_FILE:root+'/vault.key',KAGEHA_SERVER_TOKEN:apiToken,KAGEHA_MODEL:'fixture',KAGEHA_PROVIDER_URL:`http://127.0.0.1:${model.address().port}/v1/chat/completions`},stdio:['ignore',fd,fd]});fs.closeSync(fd);
  base=await until(()=>{if(child.exitCode!==null) throw new Error('Runtime exited during startup'); const found=fs.readFileSync(log,'utf8').match(/listening on ([^\s]+)/);return found&&'http://'+found[1];});
  const preview=await api('/api/plugins/preview',{name:'github'});
  assert.equal(preview.installable,true);
  const target=preview.mcp.find(server=>server.transport==='http' && server.endpoint?.startsWith('https://api.githubcopilot.com/'));
  assert(target,'Official GitHub MCP server missing from preview');serverName=target.name;
  const installed=await api('/api/plugins/install',{preview_id:preview.preview_id,approved_summary:preview});
  assert.equal(installed.plugin.status,'installed');
  // The gh token is used as a user-supplied bearer credential in this disposable
  // configuration. This verifies the live plugin transport, not OAuth consent.
  const configured=await api('/api/mcp',{name:serverName,oauth:null,headers:{Authorization:'Bearer '+credential},enabled:true,read_only_tools:['get_me']});
  assert(configured.tools.some(tool=>tool.name==='get_me'),'Live server did not offer get_me');
  assert(configured.servers.some(server=>server.name===serverName && server.plugin==='github' && server.enabled));
  const session=(await api('/api/sessions',{})).session_id;
  let done=false;
  const turn=api('/api/chat/send',{session_id:session,client_request_id:crypto.randomUUID(),message:'Use the installed GitHub plugin to check the connected account.'}).finally(()=>{done=true;});turn.catch(()=>{});
  await until(async()=>{if(done)return true;const state=await api('/api/sessions/'+session);if(state.pending_approval)await api('/api/approvals',{approval_id:state.pending_approval.approval_id,approved:true});return done;});
  await turn;
  assert(modelSawTool,'Agent did not receive the live MCP tool result');
  assert(!leakedCredential,'Token reached model context');
  assert(validToolResult,'Live MCP did not return the connected account');
  const final=await api('/api/sessions/'+session);
  assert(final.messages.some(message=>message.role==='assistant' && message.text?.includes('Live GitHub plugin connection verified.')));
  const disabled=await api('/api/mcp',{name:serverName,enabled:false});
  assert(disabled.servers.some(server=>server.name===serverName && !server.enabled),'Connection was not disabled');
  passed=true;console.log(JSON.stringify({official_plugin_installed:true,live_mcp_connected:true,agent_read_succeeded:true,connection_disabled:true,oauth_consent_tested:false}));
} finally {
  if(child) {child.kill('SIGTERM');await delay(150);if(child.exitCode===null)child.kill('SIGKILL');}
  model.closeAllConnections();await new Promise(resolve=>model.close(resolve));
  fs.rmSync(root,{recursive:true,force:true});
  if(!passed) console.error('Live plugin test did not complete; temporary state was removed.');
}
