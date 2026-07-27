// Real Rust CLI -> shared Rust host -> standalone native helper -> owned AppKit app.
// The provider is deterministic. Browser verification can attach at the checkpoint.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import http from 'node:http';
import crypto from 'node:crypto';
import assert from 'node:assert/strict';
import {spawn,spawnSync} from 'node:child_process';
import {credentialFixture} from '../../kageha-loop/tests/credential_fixture.mjs';
const hostBin=process.env.KAGEHA_LOOP_BIN,cliBin=process.env.KAGEHA_CHAT_BIN,helper=process.env.KAGEHA_COMPUTER_BIN;
assert(hostBin&&cliBin&&helper,'Set the three Rust executable paths');
const root=fs.mkdtempSync(path.join(os.tmpdir(),'kageha-shared-cli-'));fs.chmodSync(root,0o700);
const token='shared-cli-disposable-authentication-token';const tokenFile=path.join(root,'token');fs.writeFileSync(tokenFile,token,{mode:0o600});
const pause=ms=>new Promise(r=>setTimeout(r,ms));
async function until(fn,timeout=20000){const end=Date.now()+timeout;while(Date.now()<end){const v=await fn();if(v)return v;await pause(25)}throw Error('Test condition timed out')}
const hash=file=>crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
const report={root,status:'running',scope:'Actual Rust CLI and server, deterministic provider, owned AppKit window; no Python and no public model score',checks:[],hashes:{host:hash(hostBin),cli:hash(cliBin),helper:hash(helper)}};
const processes=[];let host,base,chat,native,initial,oracle,nativeObserved,nativeWrite,releaseHold,holding=false,providerCalls=0,interactiveClient,recoveryCount=0;
function start(binary,args,options={}){const child=spawn(binary,args,{stdio:['pipe','pipe','pipe'],...options});processes.push(child);return child}
function cli(args){const child=start(cliBin,['--url',base,'--token-file',tokenFile,'--json',...args]);child.packets=[];child.text='';child.errors='';let buffered='';child.stdout.on('data',chunk=>{child.text+=chunk;buffered+=chunk;while(buffered.includes('\n')){const at=buffered.indexOf('\n'),line=buffered.slice(0,at);buffered=buffered.slice(at+1);if(line)child.packets.push(JSON.parse(line))}});child.stderr.on('data',c=>child.errors+=c);return child}
async function exited(child,expected=0){await until(()=>child.exitCode!==null||child.signalCode,30000);assert.equal(child.exitCode,expected,child.errors||child.text);return child}
async function api(route,body,expected=200){const response=await fetch(base+route,{method:body?'POST':'GET',headers:{Authorization:'Bearer '+token,'Content-Type':'application/json'},body:body?JSON.stringify(body):undefined});const value=await response.json();assert.equal(response.status,expected,JSON.stringify(value));return value}
function playwright(command,...args){
 const result=spawnSync('npx',['--yes','--package','@playwright/cli','playwright-cli','-s=shared-cli-'+chat,command,...args],{encoding:'utf8',timeout:25000});
 assert.equal(result.status,0,result.stderr||result.stdout);return result.stdout;
}
async function verifyBrowser(panel){
 const artifacts=path.resolve('output/playwright/shared-cli-'+chat);fs.mkdirSync(artifacts,{recursive:true});
 try{
  playwright('open',panel);let snapshot=playwright('snapshot');
  assert(snapshot.includes('Kageha sign in'),snapshot);
  let input=snapshot.match(/textbox "Access token" \[ref=([^\]]+)\]/)?.[1];assert(input,snapshot);playwright('fill',input,token);
  snapshot=playwright('snapshot');let signIn=snapshot.match(/button "Sign in" \[ref=([^\]]+)\]/)?.[1];assert(signIn,snapshot);const signed=playwright('click',signIn);fs.writeFileSync(path.join(artifacts,'sign-in.txt'),signed);
  snapshot=playwright('snapshot');const location=snapshot.match(/Page URL: ([^\n]+)/)?.[1];assert.equal(new URL(location).searchParams.get('session'),chat);assert.equal(new URL(location).searchParams.get('surface'),'computer');
  snapshot=await until(()=>{const tree=playwright('snapshot');return tree.includes('tab "Computer" [selected]')&&tree.includes('img "Live desktop"')&&tree},20000);
  fs.writeFileSync(path.join(artifacts,'desktop.txt'),snapshot);playwright('resize','1440','1000');playwright('screenshot','--filename',path.join(artifacts,'desktop.png'));
  playwright('resize','800','900');const narrow=playwright('snapshot');assert(narrow.includes('region "Live desktop control"'),narrow);playwright('screenshot','--filename',path.join(artifacts,'narrow.png'));
  return {status:'passed',login_preserved_link:true,session:chat,computer_selected:true,live_image:true,viewports:[[1440,1000],[800,900]],artifacts};
 }finally{playwright('close')}
}
const model=http.createServer(async(req,res)=>{try{
 let raw='';for await(const chunk of req)raw+=chunk;const request=JSON.parse(raw);providerCalls++;
 const users=request.messages.map((m,i)=>m.role==='user'?i:-1).filter(i=>i>=0),last=users.at(-1),prompt=request.messages[last].content;
 const tools=request.messages.slice(last+1).filter(m=>m.role==='tool');
 const reply=message=>res.end(JSON.stringify({choices:[{message}]}));
 if(prompt==='remember shared history'){reply({role:'assistant',content:'Shared history remembered.'});return}
 if(prompt==='hold shared turn'){holding=true;await new Promise(r=>releaseHold=r);reply({role:'assistant',content:'Held turn finished once.'});return}
 if(prompt==='UI follow-up'){reply({role:'assistant',content:'UI and CLI share this conversation.'});return}
 assert.equal(prompt,'update the owned fixture');
 assert(request.messages.some(m=>m.role==='assistant'&&m.content==='Shared history remembered.'));
 const names=request.tools.map(t=>t.function.name);for(const name of ['computer_info','computer_permissions','computer_apps','computer_windows','computer_activity','computer_inspect','computer_observe','computer_execute','computer_receipt','computer_release'])assert(names.includes(name),name+' missing from shared tool catalog');
 const parsed=tools.map(t=>{const raw=t.content.startsWith('Tool failed: ')?t.content.slice('Tool failed: '.length):t.content;return JSON.parse(raw)});const target={pid:initial.pid,window_id:initial.primary_window};
 const sequence=[['computer_info',{}],['computer_permissions',{}],['computer_activity',{}],['computer_windows',{pid:initial.pid}],['computer_inspect',target],['computer_observe',target]];
 let call,id='native-'+tools.length;
 const lastId=tools.at(-1)?.tool_call_id,lastResult=parsed.at(-1),outcome=lastResult?.outcome||lastResult;
 if(outcome?.status==='error'){
   // Test-only operator simulation. Never retry dispatched/unknown outcomes.
   assert.equal(outcome.execution_phase,'not_dispatched',JSON.stringify(outcome));assert.equal(outcome.control?.reason,'human_input',JSON.stringify(outcome));assert(++recoveryCount<=3,'Desktop kept receiving external input');assert.equal(JSON.parse(fs.readFileSync(oracle)).primary,'initial-primary');
   (report.operator_recoveries??=[]).push({request_id:lastResult.request_id,execution_phase:outcome.execution_phase,reason:outcome.control.reason});
   interactiveClient.stdin.write('/computer agent\n');await until(async()=>{const state=await api('/api/sessions/'+chat+'/computer');return state.connected&&state.lockedBy!=='user'});
   call=['computer_observe',target];id='recover-observe-'+recoveryCount;
 }else if(tools.length<sequence.length)call=sequence[tools.length];
 else if(lastId==='native-5'||lastId?.startsWith('recover-observe-')){nativeObserved=lastResult;assert.equal(nativeObserved.status,'ok',JSON.stringify(nativeObserved));const field=nativeObserved.result.elements.find(e=>e.label==='Primary field');assert(field);call=['computer_execute',{observation_id:nativeObserved.observation_id,method:'set_value',args:{element_token:field.element_token,value:'cli-shared-東京'}}];id=recoveryCount?'recover-execute-'+recoveryCount:'native-6'}
 else if(lastId==='native-6'||lastId?.startsWith('recover-execute-')){nativeWrite=lastResult;assert.equal(nativeWrite.outcome.receipt.state,'verified');call=['computer_receipt',{request_id:nativeWrite.request_id}];id='receipt-'+recoveryCount}
 else if(lastId?.startsWith('receipt-')){assert.equal(lastResult.receipt.state,'verified');call=['computer_release',{}];id='release-'+recoveryCount}
 else {reply({role:'assistant',content:'Owned fixture updated; shared history verified.'});return}
 reply({role:'assistant',content:null,tool_calls:[{id,type:'function',function:{name:call[0],arguments:JSON.stringify(call[1])}}]});
}catch(error){report.provider_error=String(error);res.statusCode=500;res.end(JSON.stringify({error:{message:String(error)}}))}});
await new Promise(r=>model.listen(0,'127.0.0.1',r));
try{
 const contents=path.join(root,'Fixture.app','Contents');fs.mkdirSync(path.join(contents,'MacOS'),{recursive:true});fs.writeFileSync(path.join(contents,'Info.plist'),`<plist version="1.0"><dict><key>CFBundleIdentifier</key><string>test.kageha.shared.${Date.now()}</string><key>CFBundleExecutable</key><string>Fixture</string><key>CFBundlePackageType</key><string>APPL</string></dict></plist>`);
 const app=path.join(contents,'MacOS','Fixture');let built=spawnSync('xcrun',['swiftc','-O','crates/kageha-computer/bench/BenchmarkFixture.swift','-o',app],{encoding:'utf8'});assert.equal(built.status,0,built.stderr);assert.equal(spawnSync('codesign',['--force','--sign','-',path.join(root,'Fixture.app')]).status,0);
 oracle=path.join(root,'oracle.json');native=start(app,[oracle,'81001']);await until(()=>fs.existsSync(oracle)&&JSON.parse(fs.readFileSync(oracle)).ready);const reset=new Promise(r=>native.stdout.once('data',r));native.stdin.write('reset\n');await reset;initial=JSON.parse(fs.readFileSync(oracle));
 for(const dir of ['workspace','computer','legacy'])fs.mkdirSync(path.join(root,dir),{mode:0o700});const helperLink=path.join(root,'computer-helper');fs.symlinkSync(helper,helperLink);
 const hostLog=path.join(root,'host.log');const fd=fs.openSync(hostLog,'a');host=start(hostBin,['serve',path.join(root,'workspace'),path.join(root,'application'),'127.0.0.1:0'],{env:{...process.env,...credentialFixture(root),KAGEHA_SERVER_TOKEN:token,KAGEHA_PROVIDER_URL:`http://127.0.0.1:${model.address().port}/v1/chat/completions`,KAGEHA_MODEL:'shared-cli-fixture',KAGEHA_COMPUTER_BIN:helperLink,KAGEHA_COMPUTER_STATE_DIR:path.join(root,'computer'),KAGEHA_COMPUTER_ALLOW_INPUT:'1',KAGEHA_LEGACY_SESSIONS_DIR:path.join(root,'legacy'),KAGEHA_WEBUI_DIST:path.resolve('frontend/dist')},stdio:['ignore',fd,fd]});fs.closeSync(fd);
 base=await until(()=>{assert.equal(host.exitCode,null,fs.readFileSync(hostLog,'utf8'));return fs.readFileSync(hostLog,'utf8').match(/listening on ([^\s]+)/)?.[1]}).then(a=>'http://'+a);
 const created=await exited(cli(['--new','--panel-url']));const metadata=created.packets.find(v=>v.type==='session');chat=metadata.session_id;assert(chat);const panel=metadata.computer_url;assert.equal(new URL(panel).searchParams.get('session'),chat);assert.equal(new URL(panel).searchParams.get('surface'),'computer');assert(!panel.includes(token));assert.equal((await api('/api/sessions/'+chat)).session_id,chat);report.checks.push('CLI creates the same durable session and a credential-free Computer deep link');
 if(process.env.KAGEHA_TEST_RELEASE_READONLY_CONNECTION==='1') {
   // Explicit test setup: close only the previous read-only production connection.
   const rows=spawnSync('ps',['-axo','pid=,command='],{encoding:'utf8'}).stdout.split('\n');
   const prefix=helper+' control --journal '+path.join(os.homedir(),'.kageha/runtime/computer-rust/actions.sqlite');
   for(const row of rows){const match=row.trim().match(/^(\d+) (.*)$/);if(match&&(match[2]===prefix||match[2].startsWith(prefix+' --socket '))&&!match[2].includes('--allow-input')){try{process.kill(Number(match[1]),'SIGTERM')}catch{}}}
   await pause(100);
 }
 assert.equal((await api('/api/sessions/'+chat+'/computer/preview')).status,'error');
 const watcher=cli(['--session',chat,'--follow']);await until(()=>watcher.packets.some(p=>p.type==='snapshot'));
 const beforeWatch=providerCalls;await pause(100);assert.equal(providerCalls,beforeWatch);report.checks.push('read-only CLI attach starts no model or tools');
 await api('/api/chat/send',{session_id:chat,client_request_id:'ui-seed',message:'remember shared history'});await until(()=>watcher.packets.some(p=>p.frame?.kind==='done'));report.checks.push('CLI sees a turn sent from the UI API');
 await api('/api/permissions',{session_id:chat,profile:'ask',mode:'ask'});
 const interactive=interactiveClient=cli(['--session',chat,'--interactive']);await until(()=>interactive.packets.some(p=>p.type==='snapshot'));interactive.stdin.write('update the owned fixture\n');
 const approval=await until(()=>interactive.packets.find(p=>p.frame?.kind==='approval_required'),30000);assert.equal(JSON.parse(fs.readFileSync(oracle)).primary,'initial-primary');
 assert(interactive.packets.some(p=>p.frame?.tool_card?.name==='computer_observe'&&p.frame.tool_card.status==='done'));assert(!interactive.packets.some(p=>p.frame?.kind==='done'));report.checks.push('computer actions stream before completion; real native input waits for operator approval');
 interactive.stdin.write('/approve wrong-session-approval\n');await until(()=>interactive.packets.some(p=>p.type==='notice'&&p.message.includes('no longer pending')));assert.equal((await api('/api/sessions/'+chat)).pending_approval.approval_id,approval.frame.payload.approval_id);
 const decided=new Set([approval.frame.payload.approval_id]);interactive.stdin.write('/approve '+approval.frame.payload.approval_id+'\n');await until(()=>{
 if(report.provider_error)throw Error(report.provider_error);
 for(const packet of interactive.packets){if(packet.frame?.kind==='approval_required'&&!decided.has(packet.frame.payload.approval_id)){assert.equal(JSON.parse(fs.readFileSync(oracle)).primary,'initial-primary');decided.add(packet.frame.payload.approval_id);interactive.stdin.write('/approve '+packet.frame.payload.approval_id+'\n')}}
 return interactive.packets.some(p=>p.frame?.kind==='done'&&p.frame.payload.message==='Owned fixture updated; shared history verified.');
},45000);
 await until(()=>JSON.parse(fs.readFileSync(oracle)).primary==='cli-shared-東京');report.checks.push('CLI approves the exact action once; native field and receipt verify independently');
 const session=await api('/api/sessions/'+chat);assert(session.messages.some(m=>m.role==='user'&&m.text==='update the owned fixture'));assert(session.messages.some(m=>m.role==='assistant'&&m.text==='Owned fixture updated; shared history verified.'));report.checks.push('UI reads CLI transcript and CLI continuation retains prior UI history; all computer tools are exposed');
 const frame=await api('/api/sessions/'+chat+'/computer/preview');assert.equal(frame.status,'ok',JSON.stringify(frame));assert(frame.result.image.data.length>1000);report.preview={width:frame.result.width,height:frame.result.height};report.checks.push('same-session live native Computer preview is available');
 interactive.stdin.write('/computer status\n');await until(()=>interactive.packets.some(p=>p.type==='computer'&&p.state.runtime==='rust'));
 const epoch=(await api('/api/sessions/'+chat+'/computer')).serviceEpoch;interactive.stdin.write('/computer take\n');await until(()=>interactive.packets.some(p=>p.type==='computer'&&p.state.lockedBy==='user'));assert.equal((await api('/api/sessions/'+chat+'/computer')).serviceEpoch,epoch);interactive.stdin.write('/computer agent\n');await until(()=>interactive.packets.some(p=>p.type==='computer'&&p.state.lockedBy===null));report.checks.push('CLI takeover and return share UI ownership and keep the healthy desktop connection');
 const nativeRequest=interactive.packets.find(p=>p.type==='request').request_id;
 const late=await exited(cli(['--session',chat,'--request-id',nativeRequest,'--follow','--after-sequence',String(session.turns.at(-1)?(await api('/api/sessions/'+chat+'/events')).events.at(-1).sequence:0)]));assert(late.packets.some(p=>p.frame?.payload?.message==='Owned fixture updated; shared history verified.'));report.checks.push('completed request reattaches from a late cursor without executing again');
 interactive.stdin.write('/quit\n');await exited(interactive);watcher.kill('SIGTERM');
 if(process.env.KAGEHA_CLI_BROWSER_TEST==='1'){report.ui=await verifyBrowser(panel);report.checks.push('real browser sign-in preserves CLI session and opens live Computer panel at desktop and narrow sizes')}
 // Recovery must preserve this same draft even if the helper is temporarily unavailable.
 const worker=spawnSync('ps',['-axo','pid=,command='],{encoding:'utf8'}).stdout.split('\n').find(line=>line.includes(`control --journal ${fs.realpathSync(path.join(root,'computer','actions.sqlite'))}`)&&line.includes('--socket'));
 assert(worker,'Owned worker missing');process.kill(Number(worker.trim().match(/^\d+/)[0]),'SIGKILL');await pause(100);assert.equal((await api('/api/sessions/'+chat+'/computer/preview')).status,'error');
 fs.unlinkSync(helperLink);await api('/api/sessions/'+chat+'/computer/lease',{action:'take'},409);assert.equal((await api('/api/sessions/'+chat+'/computer')).lockedBy,'user');
 fs.symlinkSync(helper,helperLink);await api('/api/sessions/'+chat+'/computer/lease',{action:'take'});assert.equal((await api('/api/sessions/'+chat+'/computer/preview')).status,'ok');assert.equal(JSON.parse(fs.readFileSync(oracle)).primary,'cli-shared-東京');await api('/api/sessions/'+chat+'/computer/lease',{action:'return'});
 report.checks.push('helper crash and failed reconnect preserve selection and native draft; explicit recovery restores the same live preview');
 const detached=cli(['--session',chat,'--message','hold shared turn','--request-id','detach-proof']);await until(()=>holding);detached.kill('SIGTERM');await until(()=>detached.signalCode);assert((await api('/api/sessions/'+chat)).active);const count=providerCalls;releaseHold();const attached=await exited(cli(['--session',chat,'--request-id','detach-proof','--follow']));assert(attached.packets.some(p=>p.frame?.payload?.message==='Held turn finished once.'));assert.equal(providerCalls,count);report.checks.push('CLI exit detaches; the shared turn survives and reattachment does not resend');
 const final=JSON.parse(fs.readFileSync(oracle));assert.equal(final.primary,'cli-shared-東京');assert.equal(final.secondary,'initial-secondary');assert.equal(final.saves,0);report.oracle=final;report.session=chat;report.provider_calls=providerCalls;report.status='passed';
}catch(error){report.status='failed';report.error=String(error);process.exitCode=1;console.error(report.error)}
finally{if(releaseHold)releaseHold();for(const process of processes.reverse()){if(process.exitCode===null&&process.signalCode===null){process.stdin?.end();process.kill('SIGTERM')}}await pause(250);await new Promise(r=>model.close(r));report.artifact_changed=report.hashes.helper!==hash(helper)||report.hashes.host!==hash(hostBin)||report.hashes.cli!==hash(cliBin);if(report.artifact_changed){report.status='failed';report.error='Runtime artifact changed during acceptance';process.exitCode=1}fs.writeFileSync(path.join(root,'report.json'),JSON.stringify(report,null,2));console.log(JSON.stringify({status:report.status,report:path.join(root,'report.json'),error:report.error}))}
