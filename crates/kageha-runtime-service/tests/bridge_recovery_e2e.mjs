// Actual daemon crashes around disposable mutation and durable completion checkpoints.
import fs from 'node:fs';import path from 'node:path';import assert from 'node:assert/strict';import {spawn} from 'node:child_process';import {DatabaseSync} from 'node:sqlite';
const root=fs.mkdtempSync('/tmp/kageha-bridge-recovery-'),token='private-runtime-recovery-fixture-token';fs.writeFileSync(root+'/token',token,{mode:0o600});const owner='a'.repeat(64),logs=[];let child,base,mutations=0;
const pause=ms=>new Promise(r=>setTimeout(r,ms));async function until(fn){for(let end=Date.now()+10000;Date.now()<end;){const v=await fn();if(v)return v;await pause(15);}throw Error('Condition timed out');}
async function start(timeoutSeconds){const log=root+'/daemon-'+logs.length+'.log';logs.push(log);const fd=fs.openSync(log,'a');child=spawn(path.resolve(process.env.CARGO_TARGET_DIR || "target", "debug/kageha-runtime-service"),['bridge','--state',root+'/state','--token-file',root+'/token','--bind','127.0.0.1:0'],{env:{PATH:'',...(timeoutSeconds===undefined?{}:{KAGEHA_BRIDGE_TURN_TIMEOUT_SECONDS:String(timeoutSeconds)})},stdio:['ignore',fd,fd]});fs.closeSync(fd);base=await until(()=>{if(child.exitCode!==null)throw Error(fs.readFileSync(log,'utf8'));const m=fs.readFileSync(log,'utf8').match(/runtime bridge listening on ([^\s]+)/);return m&&'http://'+m[1];});}
async function stop(signal='SIGTERM'){if(child?.exitCode===null){child.kill(signal);await new Promise(r=>child.once('exit',r));}}
async function api(url,body,status=200,headers={}){const r=await fetch(base+url,{method:body?'POST':'GET',headers:{Authorization:'Bearer '+token,'x-kageha-port-owner':owner,'Content-Type':'application/json',...headers},body:body?JSON.stringify(body):undefined,signal:AbortSignal.timeout(10000)});const text=await r.text();assert.equal(r.status,status,text);return text?JSON.parse(text):null;}
const spec=id=>({id,session:'session-'+id,turn:id,request:id,parent:null,parent_run:null,agent:'test',objective:'disposable mutation proof',depth:0,input:{owner,start:{version:1,max_model_calls:4,messages:[{role:'user',content:'write disposable counter'}]}}});
async function reply(id,command,value){return api(`/v1/runs/${id}/commands/${command.id}/reply`,{ok:true,value});}
async function drive(id,{stopAtTool=false}={}){let models=0;for(let end=Date.now()+10000;Date.now()<end;){const packet=await api(`/v1/runs/${id}/commands`);if(packet.run?.state==='succeeded')return packet.run;for(const c of packet.commands){assert.equal((await api(`/v1/runs/${id}/commands/${c.id}/claim`,{})).claimed,true);let value;
 if(c.kind==='tool'){if(stopAtTool)return c;mutations++;fs.appendFileSync(root+'/counter',id+'\n');value={content:'committed',is_error:false};}
 else if(c.kind==='model'){value=models++===0?{content:null,tool_calls:[{id:'mutation',name:'write',arguments:{}}]}:{content:'finished',tool_calls:[]};}
 else if(c.kind==='discover')value=[{function:{name:'write',parameters:{type:'object'}}}];else if(c.kind==='access')value='Exclusive';else if(c.kind==='checkpoint')value=[];else if(c.kind==='observe')value=null;else throw Error(c.kind);
 await reply(id,c,value);
 }}throw Error('Driver timed out');}
try{
 await start();assert.equal((await api('/v1/info')).turn_timeout_seconds,7200);await api('/v1/info',undefined,401,{Authorization:'Bearer wrong'});
 await api('/v1/runs',spec('unknown'));const c=await drive('unknown',{stopAtTool:true});mutations++;fs.appendFileSync(root+'/counter','unknown\n');
 await stop('SIGKILL');await start();assert.equal((await api('/v1/runs/unknown')).state,'interrupted');assert.deepEqual((await api('/v1/runs/unknown/commands')).commands,[]);assert.equal((await api('/v1/runs',spec('unknown'))).state,'interrupted');
 await api('/v1/runs/unknown',undefined,409,{'x-kageha-port-owner':'b'.repeat(64)});assert.equal(mutations,1);
 let db=new DatabaseSync(root+'/state/turns.sqlite');assert.equal(db.prepare('SELECT result FROM effects WHERE scope=? AND id=?').get('unknown','port:'+c.id).result,null);
 db.exec("CREATE TRIGGER hold_finish BEFORE UPDATE ON runs WHEN NEW.state='succeeded' AND NEW.id='committed' BEGIN SELECT RAISE(FAIL,'fixture receipt publication hold'); END;");db.close();
 await api('/v1/runs',spec('committed'));let done=false;const driving=drive('committed').catch(e=>{if(!done)throw e;});
 await until(()=>{const db=new DatabaseSync(root+'/state/turns.sqlite',{readOnly:true});try{return db.prepare("SELECT result FROM effects WHERE scope='committed' AND id='bridge:completion'").get()?.result;}finally{db.close();}});
 done=true;await stop('SIGKILL');await driving;db=new DatabaseSync(root+'/state/turns.sqlite');db.exec('DROP TRIGGER hold_finish');db.close();await start();
 assert.equal((await api('/v1/runs/committed')).state,'succeeded');assert.deepEqual((await api('/v1/runs/committed/commands')).commands,[]);assert.equal(mutations,2);
 await api('/v1/runs',spec('cancelled'));const late=await drive('cancelled',{stopAtTool:true});await api('/v1/runs/cancelled/cancel',{});await until(async()=> (await api('/v1/runs/cancelled')).state==='cancelled');
 await reply('cancelled',late,{content:'late original receipt',is_error:false});await reply('cancelled',late,{content:'late original receipt',is_error:false});assert.equal((await api('/v1/runs/cancelled')).state,'cancelled');await api(`/v1/runs/cancelled/commands/${late.id}/reply`,{ok:true,value:'changed'},409);
 await stop();await start(43200);assert.equal((await api('/v1/info')).turn_timeout_seconds,43200);
 await stop();await start(1);await api('/v1/runs',spec('deadline'));
 const expired=await until(async()=>{const run=await api('/v1/runs/deadline');return run.state==='failed'&&run;});
 assert.equal(expired.completion.result.deadline_exceeded,true);
 await stop();await start(2);await api('/v1/runs',spec('tool-deadline'));
 await drive('tool-deadline',{stopAtTool:true});
 const uncertain=await until(async()=>{const run=await api('/v1/runs/tool-deadline');return run.state==='failed'&&run;});
 assert.equal(uncertain.completion.result.uncertain,true);
 console.log(JSON.stringify({passed:true,python_backend:false,checks:['claim-before-reply crash interrupted without replay','cached completion recovered before run receipt publication','wrong owner and authentication rejected','late original reply journaled without reactivation','duplicate reply idempotent and changed reply rejected','12-hour configuration accepted','short deadline enforced','claimed tool deadline preserves uncertainty'],mutations,fixture:root}));
}finally{await stop();}
