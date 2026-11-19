// Native intervention/restart proof. Only a disposable fixture is mutated.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import assert from 'node:assert/strict';
import {spawn,spawnSync} from 'node:child_process';
const helper=process.env.KAGEHA_COMPUTER_BIN;assert(helper,'Set KAGEHA_COMPUTER_BIN');
const root=fs.mkdtempSync(path.join(os.tmpdir(),'kageha-intervention-'));fs.chmodSync(root,0o700);
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
const run=(cmd,args)=>{const r=spawnSync(cmd,args,{encoding:'utf8'});assert.equal(r.status,0,r.stderr);return r.stdout};
const contents=path.join(root,'Fixture.app','Contents');fs.mkdirSync(path.join(contents,'MacOS'),{recursive:true});
fs.writeFileSync(path.join(contents,'Info.plist'),`<plist version="1.0"><dict><key>CFBundleIdentifier</key><string>test.kageha.intervention.${Date.now()}</string><key>CFBundleExecutable</key><string>Fixture</string><key>CFBundlePackageType</key><string>APPL</string></dict></plist>`);
const binary=path.join(contents,'MacOS','Fixture');run('xcrun',['swiftc','-O','crates/kageha-computer/bench/BenchmarkFixture.swift','-o',binary]);run('codesign',['--force','--sign','-',path.join(root,'Fixture.app')]);
const oracle=path.join(root,'oracle.json');const fixture=spawn(binary,[oracle,'71001'],{stdio:['pipe','pipe','ignore']});
const deadline=Date.now()+10000;while(!fs.existsSync(oracle)||!JSON.parse(fs.readFileSync(oracle)).ready){assert(Date.now()<deadline,'fixture startup');await sleep(25)}
const ack=new Promise(r=>fixture.stdout.once('data',r));fixture.stdin.write('reset\n');await ack;
const initial=JSON.parse(fs.readFileSync(oracle));const journal=path.join(root,'actions.sqlite');
let child,epoch,serial=0,pending=new Map();
function launch(resume=false){
 child=spawn(helper,['control','--journal',journal,'--allow-input',...(resume?['--resume-control']:[])],{stdio:['pipe','pipe','pipe']});
 let buffer='';const local=pending=new Map();
 child.stdout.on('data',chunk=>{buffer+=chunk;while(buffer.includes('\n')){const at=buffer.indexOf('\n');const frame=JSON.parse(buffer.slice(0,at));buffer=buffer.slice(at+1);local.get(frame.id)?.(frame.result);local.delete(frame.id)}});
 child.on('exit',()=>{for(const resolve of local.values())resolve({transport_closed:true});local.clear()});
 child.stderr.on('data',c=>fs.appendFileSync(path.join(root,'helper.log'),c));
}
async function call(operation,args={},request_id=`request-${++serial}`){
 const id=++serial;return await Promise.race([new Promise(resolve=>{pending.set(id,resolve);child.stdin.write(JSON.stringify({id,method:'control',args:{protocol:2,service_epoch:epoch||'',workspace_id:'intervention',client_id:'fixture',request_id,operation,args}})+'\n')}),sleep(10000).then(()=>{throw Error(`timeout ${operation}`)})]);
}
async function close(){if(child?.exitCode===null){const done=new Promise(r=>child.once('exit',r));child.stdin.end();await Promise.race([done,sleep(500)]);if(child.exitCode===null)child.kill()}await sleep(100)}
const report={status:'running',checks:[],root};
try{
 launch();let info=await call('info');epoch=info.service_epoch;report.permissions=await call('permissions');
 assert.equal(report.permissions.result.input_monitoring,true);
 let activity=await call('activity');assert.equal(activity.result.available,true,JSON.stringify(activity));report.checks.push('real listen-only monitor available');
 const idleUntil=Date.now()+15000; while(true){const prior=activity.result.revision;await sleep(250);activity=await call('activity');if(activity.result.active===false && activity.result.revision===prior)break;assert(Date.now()<idleUntil,'Desktop did not become idle; no input attempted')}
 report.initial_activity=activity;
 let lease=await call('acquire',{ttl_ms:60000});assert.equal(lease.status,'ok',JSON.stringify(lease));
 let view=await call('observe',{lease_id:lease.lease_id,pid:initial.pid,window_id:initial.primary_window});assert.equal(view.status,'ok',JSON.stringify(view));
 let field=view.result.elements.find(e=>e.label==='Primary field');assert(field);
 const intent={lease_id:lease.lease_id,observation_id:view.observation_id,action:{method:'set_value',args:{element_token:field.element_token,value:'verified-before-crash'}}};
 const write=await call('execute',intent,'write-once');assert.equal(write.receipt?.state,'verified',JSON.stringify(write));await sleep(75);assert.equal(JSON.parse(fs.readFileSync(oracle)).primary,'verified-before-crash');report.checks.push('native write verified independently');
 // Own key events must not look like physical intervention.
 report.own_input=[];
 for(const method of ['focus_window','focus_element','press_key']) {
   view=await call('observe',{lease_id:lease.lease_id,pid:initial.pid,window_id:initial.primary_window});
   field=view.result.elements.find(e=>e.label==='Primary field');
   const args=method==='focus_window'?{}:method==='focus_element'?{element_token:field.element_token}:{key:'cmd+a',delivery_mode:'foreground'};
   const before=await call('activity');const outcome=await call('execute',{lease_id:lease.lease_id,observation_id:view.observation_id,action:{method,args}});await sleep(75);const after=await call('activity');report.own_input.push({method,before,outcome,after});
   assert(!outcome.control,JSON.stringify({method,outcome,after}));assert.equal(after.result.active,false,JSON.stringify(after));
 }
 report.checks.push('own focus and synthetic keyboard events do not pause control');
 // Interrupt a real delayed AX action after its durable reservation.
 view=await call('observe',{lease_id:lease.lease_id,pid:initial.pid,window_id:initial.primary_window});
 const delayed=view.result.elements.find(e=>e.title==='Delayed action'||e.label==='Delayed action');assert(delayed);
 const uncertainIntent={lease_id:lease.lease_id,observation_id:view.observation_id,action:{method:'click',args:{element_token:delayed.element_token}}};
 const unfinished=call('execute',uncertainIntent,'unknown-once');
 const reserveUntil=Date.now()+2000;
 while(run('/usr/bin/sqlite3',[journal,"SELECT COUNT(*) FROM receipts WHERE request_id='unknown-once' AND response IS NULL;"]).trim()!=='1') {assert(Date.now()<reserveUntil,'Missing pending reservation');await sleep(5)}
 // Kill only this test's LaunchServices worker after verifying its exact journal.
 const command=run('ps',['-p',String(info.process_id),'-o','command=']);assert(command.includes(journal));process.kill(info.process_id,'SIGKILL');await unfinished;await close();await sleep(350);
 const committedCount=JSON.parse(fs.readFileSync(oracle)).delayed_clicks;assert([0,1].includes(committedCount));
 launch();info=await call('info');epoch=info.service_epoch;assert.equal(info.control?.reason,'helper_restarted');
 assert.equal((await call('receipt',{request_id:'write-once'})).receipt?.state,'verified');
 assert.equal((await call('receipt',{request_id:'unknown-once'})).receipt?.state,'unknown');
 assert.equal((await call('execute',uncertainIntent,'unknown-once')).receipt?.state,'unknown');
 await sleep(100);assert.equal(JSON.parse(fs.readFileSync(oracle)).delayed_clicks,committedCount);report.checks.push('real reserved action remains unknown after crash; duplicate does not dispatch again');
 let read=await call('inspect',{pid:initial.pid,window_id:initial.primary_window});assert.equal(read.status,'ok',JSON.stringify(read));assert(read.result.elements.some(e=>e.value==='verified-before-crash'));
 assert.equal((await call('acquire',{ttl_ms:60000})).error?.code,'human_intervention');
 const duplicate=await call('execute',intent,'write-once');assert.equal(duplicate.receipt?.state,'verified');assert.equal(JSON.parse(fs.readFileSync(oracle)).primary,'verified-before-crash');report.checks.push('crash preserves value and receipt; duplicate never replays; read-only inspection works while paused');
 await close();launch(true);info=await call('info');epoch=info.service_epoch;assert(!info.control);
 lease=await call('acquire',{ttl_ms:60000});assert.equal(lease.status,'ok',JSON.stringify(lease));
 view=await call('observe',{lease_id:lease.lease_id,pid:initial.pid,window_id:initial.primary_window});assert.equal(view.status,'ok');
 // External process injects a mouse movement inside this fixture window only.
 // This proves external-input handling, not a claim of physical hardware input.
 const injector=path.join(root,'ExternalInput.swift');fs.writeFileSync(injector,`import ApplicationServices\nlet event=CGEvent(mouseEventSource:CGEventSource(stateID:.privateState),mouseType:.mouseMoved,mouseCursorPosition:CGPoint(x:${view.result.bounds.x+20},y:${view.result.bounds.y+40}),mouseButton:.left)!\nevent.setIntegerValueField(.eventSourceUnixProcessID,value:Int64(getpid()))\nevent.post(tap:.cgSessionEventTap)\n`);
 run('xcrun',['swift',injector]);await sleep(100);
 const paused=await call('activity');assert.equal(paused.control?.reason,'human_input',JSON.stringify(paused));
 field=view.result.elements.find(e=>e.label==='Primary field');const denied=await call('execute',{lease_id:lease.lease_id,observation_id:view.observation_id,action:{method:'set_value',args:{element_token:field.element_token,value:'must-never-land'}}},'after-external-input');assert.equal(denied.execution_phase,'not_dispatched',JSON.stringify(denied));
 read=await call('inspect',{pid:initial.pid,window_id:initial.primary_window});assert.equal(read.status,'ok');assert.equal(JSON.parse(fs.readFileSync(oracle)).primary,'verified-before-crash');report.checks.push('external-process movement pauses input and preserves read-only recovery');
 await close();launch(true);info=await call('info');epoch=info.service_epoch;
 lease=await call('acquire',{ttl_ms:60000});assert.equal(lease.status,'ok');
 view=await call('observe',{lease_id:lease.lease_id,pid:initial.pid,window_id:initial.primary_window});
 const fixtureCommand=async command=>{const ack=new Promise(r=>fixture.stdout.once('data',r));fixture.stdin.write(command+'\n');await ack};
 await fixtureCommand('secure_on');
 const secure=await call('activity');assert.equal(secure.result.available,false);assert.equal(secure.result.unavailable_reason,'secure_input');assert.equal(secure.control?.reason,'monitor_unavailable');
 assert.equal((await call('inspect',{pid:initial.pid,window_id:initial.primary_window})).status,'ok');
 await fixtureCommand('secure_off');assert.equal((await call('activity')).result.available,true);
 assert.equal((await call('acquire',{ttl_ms:60000})).error?.code,'human_intervention');
 report.checks.push('Secure Input pauses an active lease; inspection works and restoring coverage does not auto-resume');
 report.oracle=JSON.parse(fs.readFileSync(oracle));assert.equal(report.oracle.saves,0);assert.equal(report.oracle.secondary,'initial-secondary');report.status='passed';
}catch(error){report.status='failed';report.error=String(error);process.exitCode=1}
finally{await close();fixture.stdin.end();await sleep(100);if(fixture.exitCode===null)fixture.kill();fs.writeFileSync(path.join(root,'report.json'),JSON.stringify(report,null,2));console.log(JSON.stringify({status:report.status,error:report.error,report:path.join(root,'report.json')}))}
