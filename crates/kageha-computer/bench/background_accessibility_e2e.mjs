// Native opt-in regression. Opens two disposable AppKit fixtures and sends test input only while the foreground fixture holds focus.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import assert from 'node:assert/strict';
import {spawn,execFileSync} from 'node:child_process';
import {createInterface} from 'node:readline';
import {once} from 'node:events';
const root=fs.mkdtempSync(path.join(os.tmpdir(),'kageha-bg-'));
const delay=ms=>new Promise(r=>setTimeout(r,ms));
const live=[];
const bin=process.env.KAGEHA_COMPUTER_BIN;
assert(bin && path.isAbsolute(bin), 'Set KAGEHA_COMPUTER_BIN to the absolute helper path');
const fixtureSource=path.resolve('crates/kageha-computer/bench/BenchmarkFixture.swift');
async function until(fn) {
  for(let i=0;i<60;i++){const result=await fn(); if(result)return result;await delay(50);}
  throw Error('condition timed out');
}
async function fixture(label) {
  const bundle=path.join(root,label+'.app');
  const mac=path.join(bundle,'Contents/MacOS');
  fs.mkdirSync(mac,{recursive:true});
  fs.writeFileSync(path.join(bundle,'Contents/Info.plist'),`<plist version="1.0"><dict><key>CFBundleIdentifier</key><string>test.kageha.background.${label}</string><key>CFBundleExecutable</key><string>Fixture</string><key>CFBundlePackageType</key><string>APPL</string></dict></plist>`);
  const executable=path.join(mac,'Fixture');
  if(label==='target') execFileSync('xcrun',['swiftc','-O',fixtureSource,'-o',executable]);
  else fs.copyFileSync(path.join(root,'target.app/Contents/MacOS/Fixture'),executable);
  execFileSync('codesign',['--force','--sign','-',bundle],{stdio:'ignore'});
  const oracle=path.join(root,label+'.json');
  const child=spawn(executable,[oracle,'71001'],{stdio:['pipe','pipe','inherit']});live.push(child);
  const output=createInterface({input:child.stdout});
  const state=()=>fs.existsSync(oracle)&&JSON.parse(fs.readFileSync(oracle,'utf8'));
  await until(()=>state()?.ready);
  const command=async text=>{const line=once(output,'line');child.stdin.write(text+'\n');await line;};
  return {child,state,command};
}
const helper=spawn(bin,['control','--journal',path.join(root,'actions.sqlite'),'--allow-input'],{stdio:['pipe','pipe','inherit']});
live.push(helper);
const output=createInterface({input:helper.stdout});
let request=0,epoch='';
async function call(operation,args={}){
  const id=++request;
  const line=once(output,'line',{signal:AbortSignal.timeout(7000)});
  helper.stdin.write(JSON.stringify({id,method:'control',args:{protocol:2,service_epoch:epoch,workspace_id:'background-fixture',client_id:'fixture',operation,args,request_id:'fixture-'+id}})+'\n');
  const [reply]=await line;
  return JSON.parse(reply).result;
}
try {
  const info=await call('info');epoch=info.service_epoch;assert.equal(info.background_accessibility,true);
  const permission=(await call('permissions')).result;
  assert.equal(permission.accessibility,true,JSON.stringify(permission));
  assert.equal(permission.input_monitoring,true,JSON.stringify(permission));
  const target=await fixture('target');
  const foreground=await fixture('foreground');
  const front=async()=>(await call('list_apps')).result.apps.find(app=>app.frontmost)?.pid;
  await foreground.command('reset');
  const foregroundPid=foreground.child.pid;
  await until(async()=>await front()===foregroundPid);
  const lease=await call('acquire',{ttl_ms:60000,background:true});
  assert.equal(lease.status,'ok',JSON.stringify(lease));
  const observe=()=>call('observe',{lease_id:lease.lease_id,pid:target.child.pid,window_id:target.state().primary_window});
  const execute=(view,method,args)=>call('execute',{lease_id:lease.lease_id,observation_id:view.observation_id,action:{method,args}});
  let view=await observe();assert.equal(view.status,'ok',JSON.stringify(view));
  const field=view.result.elements.find(e=>e.label==='Primary field');
  assert(field);
  const before=(await call('activity')).result.revision;
  const foregroundView=(await call('inspect',{pid:foregroundPid,window_id:foreground.state().primary_window})).result;
  const point=foregroundView.bounds;
  const source=path.join(root,'UnrelatedInput.swift');
  fs.writeFileSync(source,`import AppKit
import ApplicationServices
let expected: pid_t = ${foregroundPid}
let point = CGPoint(x:${point.x + 20}, y:${point.y + 35})
guard let source = CGEventSource(stateID:.privateState) else { exit(1) }
func send(_ event: CGEvent?) {
  guard NSWorkspace.shared.frontmostApplication?.processIdentifier == expected,
    let event else { exit(1) }
  event.post(tap:.cgSessionEventTap)
}
send(CGEvent(mouseEventSource:source, mouseType:.mouseMoved, mouseCursorPosition:point, mouseButton:.left))
send(CGEvent(mouseEventSource:source, mouseType:.leftMouseDown, mouseCursorPosition:point, mouseButton:.left))
send(CGEvent(mouseEventSource:source, mouseType:.leftMouseUp, mouseCursorPosition:point, mouseButton:.left))
send(CGEvent(keyboardEventSource:source, virtualKey:0, keyDown:true))
send(CGEvent(keyboardEventSource:source, virtualKey:0, keyDown:false))
let scroll = CGEvent(scrollWheelEvent2Source:source, units:.line, wheelCount:1, wheel1:1, wheel2:0, wheel3:0)
scroll?.location = point
send(scroll)
`);
  execFileSync('xcrun',['swift',source]);
  await until(async()=>(await call('activity')).result.revision!==before);
  const write=await execute(view,'set_value',{element_token:field.element_token,value:'changed in background'});
  assert.equal(write.receipt?.state,'verified',JSON.stringify(write));
  await until(()=>target.state().primary==='changed in background');
  assert.equal(await front(),foregroundPid);
  assert.equal(foreground.state().primary,'initial-primary');
  view=await observe();assert.equal(view.status,'ok',JSON.stringify(view));
  const button=view.result.elements.find(e=>e.role==='AXButton'&&(e.label==='Increment once'||e.title==='Increment once'));
  assert(button,JSON.stringify(view.result.elements.filter(e=>e.role==='AXButton')));
  const press=await execute(view,'click',{element_token:button.element_token});
  assert.equal(press.status,'ok',JSON.stringify(press));
  await until(()=>target.state().primary_clicks===1);
  assert.equal(await front(),foregroundPid);
  view=await observe();assert.equal(view.status,'ok',JSON.stringify(view));
  await target.command('reset');
  await until(async()=>await front()===target.child.pid);
  const denied=await execute(view,'set_value',{element_token:view.result.elements.find(e=>e.label==='Primary field').element_token,value:'must not land'});
  assert.equal(denied.error?.code,'background_target_in_use',JSON.stringify(denied));
  assert.equal(target.state().primary,'initial-primary');
  await foreground.command('reset');
  await until(async()=>await front()===foregroundPid);
  view=await observe();assert.equal(view.status,'ok',JSON.stringify(view));
  const movedField=view.result.elements.find(e=>e.label==='Primary field');
  assert(movedField);
  await target.command('reparent');
  const moved=await execute(view,'set_value',{element_token:movedField.element_token,value:'must not reach the other window'});
  assert.equal(moved.execution_phase,'not_dispatched',JSON.stringify(moved));
  assert.equal(target.state().primary,'initial-primary');
  assert.equal((await call('release',{lease_id:lease.lease_id})).status,'ok');
  console.log(JSON.stringify({backgroundWrite:true,backgroundClick:true,unrelatedInputObserved:true,focusPreserved:true,activeTargetPaused:true,freshObservationRecovered:true,crossWindowElementRefused:true}));
} finally {
  for(const child of live) {
    child.stdin.end();
    if(child.exitCode===null)await Promise.race([once(child,'exit'),delay(600)]);
    if(child.exitCode===null)child.kill();
  }
}
