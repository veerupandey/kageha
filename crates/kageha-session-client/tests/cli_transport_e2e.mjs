// Actual CLI fault injection. Never sends desktop input or provider requests.
import assert from 'node:assert/strict';
import http from 'node:http';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {spawn} from 'node:child_process';
const binary=process.env.KAGEHA_CHAT_BIN;assert(binary);
const root=fs.mkdtempSync(path.join(os.tmpdir(),'kageha-cli-transport-'));fs.chmodSync(root,0o700);
const tokenFile=path.join(root,'token');fs.writeFileSync(tokenFile,'cli-transport-disposable-auth-token',{mode:0o600});
const pause=ms=>new Promise(r=>setTimeout(r,ms));
async function until(fn){const end=Date.now()+10000;while(Date.now()<end){const value=fn();if(value)return value;await pause(10)}throw Error('CLI test timed out')}
function start(url,args,bin=binary,prefix=[]){const child=spawn(bin,[...prefix,'--url',url,'--token-file',tokenFile,'--session','chat','--json',...args],{stdio:['pipe','pipe','pipe']});child.text='';child.errors='';child.stdout.on('data',c=>child.text+=c);child.stderr.on('data',c=>child.errors+=c);return child}
async function exit(child,code){await until(()=>child.exitCode!==null);assert.equal(child.exitCode,code,child.errors||child.text);return child.text.trim().split('\n').filter(Boolean).map(line=>JSON.parse(line))}
function sse(res,packets){res.writeHead(200,{'Content-Type':'text/event-stream'});res.flushHeaders();for(const [event,data]of packets)res.write(`event: ${event}\ndata: ${JSON.stringify(data)}\n\n`)}
const snapshot={session_id:'chat',sequence:5,live:{turns:[{turn_id:'turn',status:'running'}]}};
const running={sequence:1,turn_id:'turn',kind:'tool_card',tool_card:{id:'write-once',name:'computer_execute',status:'running'}};
const done={sequence:2,turn_id:'turn',kind:'done',payload:{status:'success',message:'Finished once.'}};
let mode='reconnect',posts=0,feeds=0,all=[],lastFeed;
const fixture=http.createServer(async(req,res)=>{try{
 all.push(req.method+' '+req.url);let body='';for await(const c of req)body+=c;
 if(req.url==='/api/sessions/chat'){res.end(JSON.stringify({session_id:'chat',active:false,messages:[]}));return}
 if(req.url==='/api/chat/stream'){
  posts++;if(mode==='detach')assert(JSON.parse(body).client_request_id);else assert.equal(JSON.parse(body).client_request_id,'admitted');
  if(mode==='lost-admission'){res.socket.destroy();return}
  if(mode==='redirect'){res.writeHead(307,{Location:'http://127.0.0.1:'+trap.address().port+'/leak'});res.end('{}');return}
  res.end(JSON.stringify({turn_id:'turn',after_sequence:0,status:'accepted'}));return;
 }
 if(req.url==='/api/sessions/chat/requests/admitted'){res.end(JSON.stringify({turn:{turn_id:'turn',status:'running'},receipt:{state:'accepted'}}));return}
 if(req.url.startsWith('/api/sessions/chat/live/stream?')){
  feeds++;
  if(mode==='unauthorized-feed'){res.writeHead(401);res.end('{}');return}
  const after=new URL(req.url,'http://fixture').searchParams.get('after_sequence');
  if(mode==='reconnect'&&feeds===1){assert.equal(after,'0');sse(res,[['snapshot',snapshot],['update',{events:[running]}]]);setTimeout(()=>res.socket?.destroy(),80);return}
  if(mode==='reconnect'){assert.equal(after,'1');sse(res,[['snapshot',snapshot],['update',{events:[running,done]}]]);return}
  if(mode==='attached-approval'){sse(res,[['snapshot',{...snapshot,live:{active_turn:{turn_id:'turn'},pending_approval:{approval_id:'existing-approval',detail:'Owned action awaits approval'}}}]]);return}
  if(mode==='pipe-close'){lastFeed=res;sse(res,[['snapshot',snapshot],['update',{events:[running]}]]);return}
  if(mode==='detach'){sse(res,[['snapshot',snapshot],['update',{events:[{...running,sequence:6}]}]]);return}
  sse(res,[['snapshot',snapshot],['update',{events:[done]}]]);return;
 }
 res.writeHead(404);res.end('{}');
}catch(error){res.writeHead(500);res.end(JSON.stringify({error:String(error)}))}});
let leaked=0;const trap=http.createServer((req,res)=>{leaked++;res.end('{}')});
await new Promise(r=>fixture.listen(0,'127.0.0.1',r));await new Promise(r=>trap.listen(0,'127.0.0.1',r));
const url='http://127.0.0.1:'+fixture.address().port;
const report={status:'running',checks:[]};const children=[];
try{
 let child=start(url,['--message','one action','--request-id','admitted']);children.push(child);let packets=await exit(child,0);assert.equal(posts,1);assert.equal(feeds,2);assert.equal(packets.filter(p=>p.frame?.tool_card?.id==='write-once').length,1);assert.equal(packets.filter(p=>p.frame?.kind==='done').length,1);assert(child.text.includes('reconnecting read-only after sequence 1'));report.checks.push('CLI read-only reconnect uses delivered cursor, ignores snapshot watermark, and does not repeat tool or prompt');
 mode='lost-admission';posts=0;feeds=0;child=start(url,['--message','one action','--request-id','admitted']);children.push(child);await exit(child,1);assert.equal(posts,1);assert.equal(feeds,0);assert(child.errors.includes('--request-id admitted --follow'));report.checks.push('lost admission response reports original request; no automatic resend');
 mode='recover';child=start(url,['--request-id','admitted','--follow']);children.push(child);packets=await exit(child,0);assert.equal(posts,1);assert(packets.some(p=>p.frame?.payload?.message==='Finished once.'));report.checks.push('explicit receipt-based reattachment completes without POST');
 mode='redirect';posts=0;child=start(url,['--message','one action','--request-id','admitted']);children.push(child);await exit(child,1);assert.equal(posts,1);assert.equal(leaked,0);report.checks.push('redirect cannot forward bearer token or replay admission');
 mode='unauthorized-feed';feeds=0;child=start(url,['--follow']);children.push(child);await exit(child,1);assert.equal(feeds,1);report.checks.push('unauthorized feed fails promptly without reconnect loop');
 mode='detach';posts=0;all=[];child=start(url,['--interactive']);children.push(child);await until(()=>child.text.includes('snapshot'));child.stdin.write('one action\n');await until(()=>child.text.includes('computer_execute'));child.stdin.end();await exit(child,0);assert.equal(posts,1);assert(!all.some(route=>/cancel|approvals/.test(route)));report.checks.push('interactive EOF detaches without cancelling or approving work');
 mode='attached-approval';posts=0;child=start(url,['--interactive']);children.push(child);await until(()=>child.text.includes('existing-approval'));child.stdin.write('do not start a second turn\n');await until(()=>child.text.includes('turn is still running'));assert.equal(posts,0);child.stdin.write('/quit\n');await exit(child,0);report.checks.push('reattached snapshot mirrors an active UI turn and its pending approval without starting a second turn');
 mode='pipe-close';posts=0;child=start(url,['--message','one action','--request-id','admitted']);children.push(child);await until(()=>child.text.includes('computer_execute'));child.stdout.destroy();lastFeed.write(`event: update\ndata: ${JSON.stringify({events:[{sequence:2,turn_id:'turn',kind:'status',label:'working'}]})}\n\n`);await exit(child,0);assert.equal(posts,1);assert(!child.errors.includes('panicked'));report.checks.push('closed output pipe detaches cleanly instead of panicking or cancelling');
 mode='detach';posts=0;child=start(url,['--interactive']);children.push(child);await until(()=>child.text.includes('snapshot'));child.stdin.write('x'.repeat(262145)+'\n');await exit(child,1);assert.equal(posts,0);assert(child.errors.includes('exceeds'));report.checks.push('oversized stdin is bounded and refused before admission');
 if(process.env.KAGEHA_LOOP_BIN){mode='recover';all=[];child=start(url,['--panel-url'],process.env.KAGEHA_LOOP_BIN,['chat']);children.push(child);await exit(child,0);assert.deepEqual(all,['GET /api/sessions/chat']);report.checks.push('kageha-loop chat attaches to the same authenticated session client')}
 report.status='passed';
}catch(error){report.status='failed';report.error=String(error);process.exitCode=1}
finally{for(const child of children)if(child.exitCode===null)child.kill('SIGTERM');fixture.closeAllConnections();trap.closeAllConnections();await new Promise(r=>fixture.close(r));await new Promise(r=>trap.close(r));fs.writeFileSync(path.join(root,'report.json'),JSON.stringify(report,null,2));console.log(JSON.stringify({status:report.status,error:report.error,report:path.join(root,'report.json')}))}
