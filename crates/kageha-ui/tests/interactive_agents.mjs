// Interactive selection/controls use the same session feed and never replay mutations.
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
const root=fs.mkdtempSync('/tmp/kageha-interactive-agents-');
const token='private-interactive-agents-fixture-token';
fs.writeFileSync(root+'/token',token,{mode:0o600});
let sequence=1,connections=[],requests=[],posts=[],disconnectFirstMessage=true;
let workers=[
 {id:'root',label:'Reviewer',session_id:'root-chat',parent_session:'test',state:'running',revision:1,progress:{turn_id:'root-turn',steps:1,current_tool:{id:'read',name:'read_file',status:'done'},elapsed_s:3},objective:'Review the code'},
 {id:'child',label:'Tests',session_id:'child-chat',parent_session:'root-chat',parent_job_id:'root',state:'running',revision:1,progress:{turn_id:'child-turn',steps:2,current_tool:{id:'test',name:'run_tests',status:'running'},elapsed_s:5},objective:'Check the changed lines',can_cancel:true,can_resume:false},
];
function packet(res,event,data){res.write('event: '+event+'\ndata: '+JSON.stringify(data)+'\n\n');}
function workerUpdate(){for(const res of connections)if(!res.destroyed)packet(res,'update',{events:[{sequence:++sequence,kind:'worker',job:workers[1]}]});}
const server=http.createServer(async(req,res)=>{
 assert.equal(req.headers.authorization,'Bearer '+token);
 requests.push({method:req.method,url:req.url});
 if(req.method==='GET'&&req.url.startsWith('/api/sessions/test/live/stream')){
  res.writeHead(200,{'Content-Type':'text/event-stream'});packet(res,'snapshot',{sequence,workers});connections.push(res);return;
 }
 if(req.method==='GET'&&req.url==='/api/jobs?session_id=test'){res.end(JSON.stringify({jobs:workers}));return;}
 let text='';for await(const b of req)text+=b;const body=JSON.parse(text);posts.push({url:req.url,body});
 assert.equal(req.method,'POST');assert(req.url.startsWith('/api/jobs/child/'));
 if(req.url.endsWith('/message')){
  assert.equal(body.expected_turn_id,workers[1].progress.turn_id);assert(body.message_id);
  if(disconnectFirstMessage){disconnectFirstMessage=false;res.destroy();return;}
  res.end(JSON.stringify({queued:true}));return;
 }
 if(req.url.endsWith('/cancel')){assert.equal(body.expected_turn_id,'child-turn');workers[1]={...workers[1],state:'cancelled',can_resume:true,resume:{allowed:true},ended_at:10,revision:2};res.end('{"cancelled":true}');workerUpdate();return;}
 if(req.url.endsWith('/resume')){assert.equal(body.confirmed,true);assert.equal(body.expected_turn_id,'child-turn');assert.equal(body.expected_revision,2);assert(body.client_request_id);workers[1]={...workers[1],state:'running',can_resume:false,revision:3,progress:{...workers[1].progress,turn_id:'resumed-turn'}};res.end(JSON.stringify({job:workers[1]}));workerUpdate();return;}
 if(req.url.endsWith('/approve')){assert.equal(body.approval_id,'approval-a');assert.equal(body.approved,true);assert.equal(body.scope,'once');assert.equal(body.expected_turn_id,'resumed-turn');workers[1]={...workers[1],state:'running',question:null,revision:5};res.end('{"ok":true,"approved":true}');workerUpdate();return;}
 res.writeHead(404);res.end('{"error":"not found"}');
});
await new Promise(r=>server.listen(0,'127.0.0.1',r));
const base='http://127.0.0.1:'+server.address().port;
const binary=process.env.KAGEHA_AGENTS_BIN||path.resolve('target/debug/kageha-agents');
const args=['--session','test','--url',base,'--token-file',root+'/token'];
function launch(extra){const child=spawn(binary,[...args,...extra],{env:{PATH:'',COLUMNS:'70'},stdio:['pipe','pipe','pipe']});child.out='';child.err='';child.stdout.on('data',b=>child.out+=b);child.stderr.on('data',b=>child.err+=b);return child;}
const pause=ms=>new Promise(r=>setTimeout(r,ms));
async function until(check,child){for(let end=Date.now()+10000;Date.now()<end;){if(check())return;if(child?.exitCode!==null&&child?.exitCode!==undefined)throw Error('CLI stopped: '+child.err);await pause(10);}throw Error('Condition timed out: '+(child?.out||'')+'\n'+(child?.err||''));}
const child=launch(['--interactive','--plain']);
try{
 await until(()=>child.out.includes('2 active'),child);
 assert(child.out.includes('└─'));assert(child.out.includes('Current tool: run_tests'));assert(child.out.includes('5s'));
 child.stdin.write('use 2\n');await until(()=>child.out.includes('>  2'),child);
 child.stdin.write('inspect\n');await until(()=>child.out.includes('Check the changed lines'),child);
 child.stdin.write('message first steering\n');await until(()=>child.err.includes('outcome is unknown'),child);await pause(150);assert.equal(posts.length,1,'ambiguous command must not replay');
 child.stdin.write('message use current source\n');await until(()=>child.out.includes('Message queued'),child);assert.equal(posts.length,2);assert.notEqual(posts[0].body.message_id,posts[1].body.message_id);
 child.stdin.write('cancel\n');await until(()=>child.out.includes('Cancellation requested'),child);await until(()=>workers[1].state==='cancelled',child);
 child.stdin.write('resume\n');await until(()=>child.err.includes('requires explicit confirmation'),child);assert.equal(posts.filter(p=>p.url.endsWith('/resume')).length,0);
 child.stdin.write('resume confirm\n');await until(()=>child.out.includes('Resume admitted'),child);assert.equal(posts.filter(p=>p.url.endsWith('/resume')).length,1);
 workers[1]={...workers[1],state:'waiting_input',revision:4,question:{question:'Allow this upload?',approval_id:'approval-a'},error:{approval_id:'approval-a'}};workerUpdate();
 await until(()=>child.out.includes('Allow this upload?'),child);
 child.stdin.write('approve old-approval\n');await until(()=>child.err.includes('approval changed'),child);assert.equal(posts.filter(p=>p.url.endsWith('/approve')).length,0);
 child.stdin.write('approve approval-a\n');await until(()=>child.out.includes('Approved this request once'),child);
 const lastSequence=sequence;for(const res of connections)res.end();await until(()=>connections.length===2,child);
 assert(requests.some(r=>r.url.includes('after_sequence='+lastSequence)),'reconnect carries durable cursor');
 assert.equal(posts.length,5,'reconnect cannot resubmit controls');assert(!child.out.includes('\x1b'));
 child.stdin.write('quit\n');assert.equal(await new Promise(r=>child.once('exit',r)),0);assert.equal(posts.length,5,'quit does not stop agents');
 const wrong=launch(['--agent','outside-session','--cancel']);assert.equal(await new Promise(r=>wrong.once('exit',r)),1);assert.equal(posts.length,5,'unlisted worker cannot receive a control');
 console.log(JSON.stringify({passed:true,checks:['parent/child tree','current tool and elapsed','selection and inspection','explicit steering','cancellation confirmation','resume confirmation and original turn binding','exact approval identity','unknown outcome never retried','cursor reconnect without control replay','quit leaves agents running','scoped agent selection','plain output']}));
}finally{if(child.exitCode===null)child.kill('SIGTERM');for(const res of connections)res.end();server.closeAllConnections();server.close();}
