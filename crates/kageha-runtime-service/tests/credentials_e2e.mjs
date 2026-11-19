// Real Rust vault + standalone Kageha turn, including a key change during an active tool round trip.
import assert from 'node:assert/strict';
import fs from 'node:fs';import path from 'node:path';import os from 'node:os';
import https from 'node:https';import crypto from 'node:crypto';
import {spawn,execFileSync} from 'node:child_process';
const root=fs.mkdtempSync(path.join(os.tmpdir(),'kageha-credential-runtime-'));fs.chmodSync(root,0o700);
const vaultBin=process.env.KAGEHA_CREDENTIALS_BIN||path.resolve('target/debug/kageha-credentials');
const runtimeBin=process.env.KAGEHA_RUNTIME_BIN||path.resolve('target/debug/kageha-runtime-service');
const children=[];const token='runtime-disposable-auth-token-1234567890';const clientToken='vault-workload-disposable-token-1234567890';
const first='first-disposable-credential',second='second-disposable-credential';let expected=first;let switched=false;let base;
const calls=[];const sleep=ms=>new Promise(r=>setTimeout(r,ms));
async function until(fn){let end=Date.now()+20000;while(Date.now()<end){const v=await fn();if(v)return v;await sleep(30);}throw Error('Credential runtime condition timed out');}
function write(name,value,mode=0o600){let file=path.join(root,name);fs.writeFileSync(file,value,{mode});return file;}
function cli(command,extra=[]){return execFileSync(vaultBin,[command,'--config',path.join(root,'vault.json'),...extra],{env:{PATH:''},encoding:'utf8'});}
function importKey(value,revision,id){const file=write('import.json',JSON.stringify([{operation_id:id,reference:{namespace:'models',id:'primary'},kind:'api_key',expected_revision:revision,secret_base64:Buffer.from(value).toString('base64'),expires_at_ms:null}]));cli('migrate',['--input',file]);fs.rmSync(file);}
function start(binary,args,name,env){const log=write(name+'.log','');const child=spawn(binary,args,{env:{PATH:'',...env},stdio:['ignore',fs.openSync(log,'a'),fs.openSync(log,'a')]});children.push(child);return{child,log};}
async function api(route,body){const response=await fetch(base+route,{method:body?'POST':'GET',headers:{Authorization:'Bearer '+token,'Content-Type':'application/json'},body:body?JSON.stringify(body):undefined});const value=await response.json();assert.equal(response.status,200,JSON.stringify(value));return value;}
const openssl=args=>execFileSync('/usr/bin/openssl',args,{stdio:'ignore'});
const caConfig=write('ca.cnf','[req]\ndistinguished_name=dn\nx509_extensions=ext\nprompt=no\n[dn]\nCN=Credential Test CA\n[ext]\nbasicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign,cRLSign\n');
const leafConfig=write('leaf.cnf','[req]\ndistinguished_name=dn\nprompt=no\n[dn]\nCN=localhost\n[ext]\nsubjectAltName=DNS:localhost,IP:127.0.0.1\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n');
openssl(['req','-x509','-newkey','rsa:2048','-nodes','-days','1','-keyout',path.join(root,'ca.key'),'-out',path.join(root,'ca.pem'),'-config',caConfig]);
openssl(['req','-newkey','rsa:2048','-nodes','-keyout',path.join(root,'leaf.key'),'-out',path.join(root,'leaf.csr'),'-config',leafConfig]);
openssl(['x509','-req','-in',path.join(root,'leaf.csr'),'-CA',path.join(root,'ca.pem'),'-CAkey',path.join(root,'ca.key'),'-CAcreateserial','-days','1','-out',path.join(root,'leaf.pem'),'-extfile',leafConfig,'-extensions','ext']);
const model=https.createServer({key:fs.readFileSync(path.join(root,'leaf.key')),cert:fs.readFileSync(path.join(root,'leaf.pem'))},async(req,res)=>{
 try{
  assert.equal(req.url,'/v1/chat/completions');assert.equal(req.headers.authorization,'Bearer '+expected);calls.push(req.headers.authorization);
  let bytes='';for await(const chunk of req)bytes+=chunk;const request=JSON.parse(bytes);const result=request.messages.some(m=>m.role==='tool');
  if(!switched){importKey(second,1,'rotate-during-active-turn');expected=second;switched=true;}
  const message=result?{role:'assistant',content:'Credential-backed proof read.'}:{role:'assistant',content:null,tool_calls:[{id:'read-proof',type:'function',function:{name:'read_file',arguments:JSON.stringify({path:'notes.txt'})}}]};
  res.setHeader('Content-Type','application/json');res.end(JSON.stringify({choices:[{message}]}));
 }catch(e){res.writeHead(500);res.end('{}');throw e;}
});
await new Promise(r=>model.listen(0,'127.0.0.1',r));
try{
 const origin=`https://localhost:${model.address().port}`;const keyFile=write('vault.key',crypto.randomBytes(32));const socket=path.join(root,'credentials.sock');
 const admin={principal:'operator',namespace:'models',ids:['primary'],purposes:['admin'],destinations:['local'],permissions:['write','metadata','revoke']};
 write('vault.json',JSON.stringify({vault:path.join(root,'vault.sqlite'),key_id:'default',key_files:{default:keyFile},grants:[admin,{principal:'runtime',namespace:'models',ids:['primary'],purposes:['model'],destinations:[origin],permissions:['resolve','metadata']}],clients:[{principal:'runtime',authentication_generation:1,local_token_sha256:crypto.createHash('sha256').update(clientToken).digest('hex'),uid:process.getuid(),certificate_sha256:null}],socket}));
 importKey(first,0,'initial-key');const vault=start(vaultBin,['serve','--config',path.join(root,'vault.json')],'vault',{});await until(()=>{if(vault.child.exitCode!==null)throw Error(fs.readFileSync(vault.log,'utf8'));return fs.existsSync(socket);});
 const tokenFile=write('token',token),clientTokenFile=write('client.token',clientToken);
 fs.mkdirSync(path.join(root,'workspace'));fs.writeFileSync(path.join(root,'workspace/notes.txt'),'Scoped credential proof');
 const cfg=write('runtime.json',JSON.stringify({credential_service:{transport:'unix',socket,token_file:clientTokenFile,principal:'runtime'},providers:[{endpoint:origin+'/v1/chat/completions',model:'fixture',protocol:'openai_compat',credential_ref:{namespace:'models',id:'primary'}}]}));
 const runtime=start(runtimeBin,['--state',path.join(root,'state'),'--workspace',path.join(root,'workspace'),'--config',cfg,'--token-file',tokenFile,'--bind','127.0.0.1:0'],'runtime',{KAGEHA_CA_BUNDLE:path.join(root,'ca.pem'),KAGEHA_API_KEY:'stale-fallback-must-not-be-used'});
 base=await until(()=>{if(runtime.child.exitCode!==null)throw Error(fs.readFileSync(runtime.log,'utf8'));const match=fs.readFileSync(runtime.log,'utf8').match(/listening on (127\.0\.0\.1:\d+)\r?\n/);return match&&'http://'+match[1];});
 async function run(id){await api('/v1/runs',{id,session:'same-chat',turn:'turn-'+id,request:id,parent:null,parent_run:null,agent:'main',objective:'Read notes.txt',depth:0,input:{messages:[{role:'user',content:'Read notes.txt'}],max_model_calls:4,timeout_ms:15000}});return until(async()=>{const run=await api('/v1/runs/'+id);return ['succeeded','failed','cancelled','interrupted'].includes(run.state)&&run;});}
 assert.equal((await run('first')).state,'succeeded');assert.deepEqual(calls,['Bearer '+first,'Bearer '+second]);
 assert.equal((await run('second')).state,'succeeded');assert.equal(calls.length,4);assert(calls.slice(1).every(v=>v==='Bearer '+second));
 cli('revoke',['--namespace','models','--id','primary','--revision','2','--operation-id','revoke-model-key']);
 assert.equal((await run('revoked')).state,'failed');assert.equal(calls.length,4);
 const publicState=JSON.stringify(await api('/v1/runs/first'));assert(!publicState.includes(first)&&!publicState.includes(second));
 for(const log of [vault.log,runtime.log]){const text=fs.readFileSync(log,'utf8');assert(!text.includes(first)&&!text.includes(second));}
 const database=fs.readFileSync(path.join(root,'vault.sqlite'));assert(!database.includes(Buffer.from(first))&&!database.includes(Buffer.from(second)));
 console.log(JSON.stringify({passed:true,python_backend:false,model_requests:calls.length,key_rotation_during_active_turn:true,same_chat:true,revocation_without_environment_fallback:true,plaintext_absent_from_database_logs_and_run_state:true}));
}finally{for(const child of children.reverse()){if(child.exitCode===null){child.kill('SIGTERM');await new Promise(r=>child.once('exit',r));}}model.closeAllConnections();await new Promise(r=>model.close(r));fs.rmSync(root,{recursive:true,force:true});}
