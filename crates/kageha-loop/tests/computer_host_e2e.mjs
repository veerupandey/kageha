// Rust host -> standalone Rust computer -> Swift/macOS, no Python or desktop input.
import {credentialFixture} from './credential_fixture.mjs';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import http from 'node:http';
import {spawn} from 'node:child_process';
const binary=process.env.KAGEHA_LOOP_BIN;
const computer=process.env.KAGEHA_COMPUTER_BIN;
assert(binary && computer,'Set absolute KAGEHA_LOOP_BIN and KAGEHA_COMPUTER_BIN');
const root=fs.mkdtempSync(path.join(os.tmpdir(),'kageha-computer-host-'));
for(const name of ['workspace','computer','legacy'])fs.mkdirSync(path.join(root,name),{mode:0o700});
const token='disposable-computer-host-integration-token';
const requests=[];
const target=process.env.KAGEHA_PREVIEW_TEST_TARGET?JSON.parse(fs.readFileSync(process.env.KAGEHA_PREVIEW_TEST_TARGET)):null;
let providerFailure;
const provider=http.createServer(async(req,res)=>{
 try {
  let body='';for await(const part of req)body+=part;
  const value=JSON.parse(body);requests.push(value);
  res.setHeader('Content-Type','application/json');
  if (!value.tools?.length) {
    res.end(JSON.stringify({choices:[{message:{role:'assistant',content:'{}'}}]}));
    return;
  }
  const names=value.tools.map(tool=>tool.function.name);
  assert(names.includes('computer_info'));
  assert(names.includes('computer_observe'));
  for (const name of ['computer_execute','computer_request_control','computer_launch_app']) assert(names.includes(name),name);
  const userIndex=value.messages.findLastIndex(m=>m.role==='user');
  const prompt=String(value.messages[userIndex].content);
  const tool=value.messages.slice(userIndex+1).filter(m=>m.role==='tool').at(-1);
  const denied=prompt.includes('denied-input');
  const control=prompt.includes('grant-input');
  const decline=prompt.includes('decline-input');
  const statusOnly=prompt.includes('status-only');
  let message;
  if(!tool){
    const name=denied?'computer_execute':control||decline?'computer_request_control':target&&!statusOnly?'computer_inspect':'computer_info';
    const args=denied?{observation_id:'never',method:'set_value',args:{element_token:'never',value:'fixture'}}:control||decline?{reason:'Check the turn-scoped grant on this disposable host'}:target&&!statusOnly?{pid:target.pid,window_id:target.window_id,include_screenshot:false}:{};
    message={role:'assistant',content:null,tool_calls:[{id:'first',type:'function',function:{name,arguments:JSON.stringify(args)}}]};
  }else if(decline){assert.match(tool.content,/Desktop access declined/);message={role:'assistant',content:'Desktop access declined.'};
  }else if(control){assert.equal(JSON.parse(tool.content).input_authorized,true);message={role:'assistant',content:'Turn-scoped control granted.'};
  }else if(denied){assert.match(tool.content,/computer_request_control/);message={role:'assistant',content:'Input denied before approval or dispatch.'};
  }else if(tool.tool_call_id==='first'){
    const info=JSON.parse(tool.content);if(target&&!statusOnly){assert.equal(info.status,'ok')}else{assert.equal(info.protocol,2);assert.equal(info.input_authorized,false)};
    message={role:'assistant',content:null,tool_calls:[{id:'permissions',type:'function',function:{name:'computer_permissions',arguments:'{}'}}]};
  }else{const permissions=JSON.parse(tool.content);assert.equal(permissions.status,'ok');assert.equal(typeof permissions.result.accessibility,'boolean');message={role:'assistant',content:'Native computer ready; permission probe completed without input.'};}
  res.end(JSON.stringify({choices:[{message}]}));
 } catch (error) {
   providerFailure ??= error;
   res.end(JSON.stringify({choices:[{message:{role:'assistant',content:'Fixture assertion failed.'}}]}));
 }
});
await new Promise(resolve=>provider.listen(0,'127.0.0.1',resolve));
let child,base;
async function api(url,body){const response=await fetch(base+url,{method:body?'POST':'GET',headers:{Authorization:`Bearer ${token}`,'Content-Type':'application/json'},body:body?JSON.stringify(body):undefined});const value=await response.json();if(providerFailure)throw providerFailure;assert.equal(response.status,200,JSON.stringify(value));return value;}
async function until(fn){const end=Date.now()+30000;while(Date.now()<end){const v=await fn();if(v)return v;await new Promise(r=>setTimeout(r,40));}throw Error('Timed out');}
try{
  const log=path.join(root,'host.log');const fd=fs.openSync(log,'a');
  const env={...process.env,...credentialFixture(root),KAGEHA_HOME:path.join(root,'state','home'),KAGEHA_SERVER_TOKEN:token,KAGEHA_PROVIDER_URL:`http://127.0.0.1:${provider.address().port}/v1/chat/completions`,KAGEHA_MODEL:'computer-fixture',KAGEHA_COMPUTER_BIN:computer,KAGEHA_COMPUTER_STATE_DIR:path.join(root,'computer')};
  delete env.KAGEHA_COMPUTER_ALLOW_INPUT;delete env.KAGEHA_BROWSER_STATE_DIR;env.KAGEHA_LEGACY_SESSIONS_DIR=path.join(root,'legacy');
  child=spawn(binary,['serve',path.join(root,'workspace'),path.join(root,'state'),'127.0.0.1:0'],{env,stdio:['ignore',fd,fd]});fs.closeSync(fd);
  base=await until(()=>{if(child.exitCode!==null)throw Error(fs.readFileSync(log,'utf8'));const found=fs.readFileSync(log,'utf8').match(/listening on ([^\s]+)/);return found&&`http://${found[1]}`;});
  assert.equal((await api('/api/slash-catalog')).capabilities.computer,true);
  const chat=(await api('/api/sessions',{})).session_id;
  const result=await api('/api/chat/send',{session_id:chat,client_request_id:'native-readonly',message:'Check native computer availability'});
  assert.match(result.message,/permission probe completed/);
  let status=await api(`/api/sessions/${chat}/computer`);assert.equal(status.connected,true);assert.equal(status.native_adapter,'swift');assert.equal(status.previewAvailable,true);
  status=await api(`/api/sessions/${chat}/computer/lease`,{action:'take'});assert.equal(status.lockedBy,'user');
  const preview=await api(`/api/sessions/${chat}/computer/preview`);assert.equal(preview.status,target?'ok':'error');if(target){assert.equal(preview.result.image.mime_type,'image/jpeg');assert(Buffer.from(preview.result.image.data,'base64').length>1000);}
  await api('/api/chat/send',{session_id:chat,client_request_id:'held-by-user',message:'status-only Check native computer availability'});
  assert.equal((await api(`/api/sessions/${chat}/computer`)).lockedBy,'user');
  status=await api(`/api/sessions/${chat}/computer/lease`,{action:'return'});assert.equal(status.lockedBy,null);
  const denied=await api('/api/chat/send',{session_id:chat,client_request_id:'denied-input',message:'denied-input'});assert.match(denied.message,/denied before approval/);
  assert.equal((await api(`/api/sessions/${chat}`)).pending_approval,null);
  const grantChat=(await api('/api/sessions',{})).session_id;
  const askChat=(await api('/api/sessions',{})).session_id;
  await api('/api/permissions',{session_id:askChat,profile:'ask'});
  const awaiting=api('/api/chat/send',{session_id:askChat,client_request_id:'decline-input',message:'decline-input Check desktop permission'});
  const pending=await until(async()=>(await api(`/api/sessions/${askChat}`)).pending_approval);
  assert.equal(pending.action,'computer_request_control');
  assert.match(pending.detail,/background Accessibility actions/);
  await api('/api/approvals',{approval_id:pending.approval_id,approved:false,scope:'once'});
  assert.match((await awaiting).message,/Desktop access declined/);
  assert.equal((await api(`/api/sessions/${askChat}/computer`)).inputAuthorized,false);
  const granted=await api('/api/chat/send',{session_id:grantChat,client_request_id:'grant-input',message:'grant-input Check desktop control for this turn'});
  assert.match(granted.message,/Turn-scoped control granted/);
  await api('/api/chat/send',{session_id:grantChat,client_request_id:'scope-check',message:'status-only Check native computer availability'});
  status=await api(`/api/sessions/${chat}/computer/access`,{allow:true});assert.equal(status.inputAuthorized,true);
  status=await api(`/api/sessions/${chat}/computer/access`,{allow:false});assert.equal(status.inputAuthorized,false);assert.equal(status.lockedBy,'user');
  console.log(JSON.stringify({status:'ok',scope:'disposable Rust host and configured helper, no desktop input dispatched',tools:true,turn_scoped_grant:true,takeover:true,unauthorized_input_denied:true,provider_requests:requests.length}));
}catch(error){
  const log=path.join(root,'host.log');if(fs.existsSync(log))process.stderr.write(fs.readFileSync(log,'utf8').slice(-4000));
  throw error;
}finally{
  if(child&&child.exitCode===null){child.kill('SIGTERM');await new Promise(resolve=>child.once('exit',resolve));}
  await new Promise(resolve=>provider.close(resolve));
  fs.rmSync(root,{recursive:true,force:true});
}
