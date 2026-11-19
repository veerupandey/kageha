// Real Kageha model/tool loop, local deterministic model, and disposable Git workspace.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import http from 'node:http';
import {spawn,execFileSync} from 'node:child_process';
const root=fs.mkdtempSync('/tmp/kageha-handoff-e2e-');
const workspace=path.join(root,'project'),state=path.join(root,'state');
fs.mkdirSync(workspace);fs.mkdirSync(state);
const token='handoff-disposable-authentication-token-12345';
const binary=process.env.KAGEHA_LOOP_BIN||path.resolve('target/debug/kageha-loop');
const git=(...args)=>execFileSync('/usr/bin/git',args,{cwd:workspace,stdio:'ignore',env:{PATH:'/usr/bin:/bin',HOME:root,GIT_AUTHOR_NAME:'Fixture',GIT_AUTHOR_EMAIL:'fixture@example.test',GIT_COMMITTER_NAME:'Fixture',GIT_COMMITTER_EMAIL:'fixture@example.test'}});
git('init','-q');fs.writeFileSync(path.join(workspace,'baseline.txt'),'baseline\n');git('add','baseline.txt');git('commit','-qm','baseline');
// An unrelated dirty file must survive every integration.
fs.writeFileSync(path.join(workspace,'baseline.txt'),'user dirty work\n');
let firstJob, firstPreview, reviewJob, server, url, sequence=0;const children=[];
const tool=(id,name,args)=>({role:'assistant',content:null,tool_calls:[{id,type:'function',function:{name,arguments:JSON.stringify(args)}}]});
const model=http.createServer(async(req,res)=>{
 try{
  let body='';for await(const chunk of req)body+=chunk;const request=JSON.parse(body||'{}');
  const messages=request.messages||[];const user=messages.filter(m=>m.role==='user').at(-1)?.content||'';
  const results=messages.filter(m=>m.role==='tool');
  const by=id=>results.find(m=>m.tool_call_id===id);
  const send=message=>{res.setHeader('Content-Type','application/json');res.end(JSON.stringify({choices:[{message}]}));};
  const reply=text=>send({role:'assistant',content:text});
  const call=(id,name,args)=>send(tool(id,name,args));
  if(user==='handoff-child' || user==='handoff-stale-child' || user==='review-patch-child'){
   if(!results.length){const name=user==='handoff-child'?'new.py':user==='review-patch-child'?'review.py':'stale.py';return call('child-write','write_file',{path:'src/'+name,content:user==='handoff-child'?'VALUE = 42\n':user==='review-patch-child'?'FIX = true\n':'VALUE = 1\n'});}
   return reply('Worker wrote the source file.');
  }
  if(user==='handoff-parent'||user==='handoff-stale-parent'){
   const stale=user.includes('stale');
   if(!by('spawn'))return call('spawn','agent_spawn',{agent_id:'main',objective:stale?'handoff-stale-child':'handoff-child',title:stale?'Stale worker':'Implementation',isolation:'worktree',keep_worktree:true});
   const spawned=JSON.parse(by('spawn').content);const id=spawned.id;if(!stale)firstJob=id;
   const waits=results.filter(r=>r.tool_call_id?.startsWith('wait-'));
   const finished=waits.some(r=>{try{return JSON.parse(r.content).jobs?.some(j=>j.id===id&&j.state==='succeeded');}catch{return false;}});
   if(!finished)return call('wait-'+(++sequence),'agent_wait_any',{job_ids:[id],timeout_ms:30000});
   if(!by('preview'))return call('preview','agent_handoff_preview',{job_id:id,target:'project'});
   const preview=JSON.parse(by('preview').content);assert.equal(preview.status,'preview');assert.equal(preview.conflicts.length,0);assert(preview.files.some(f=>f.path===(stale?'src/stale.py':'src/new.py')));
   if(!stale)firstPreview=preview.preview_id;
   if(!by('apply')){
    if(stale){fs.mkdirSync(path.join(workspace,'src'),{recursive:true});fs.writeFileSync(path.join(workspace,'src/stale.py'),'USER CHANGE\n');}
    return call('apply','agent_handoff_apply',{preview_id:preview.preview_id});
   }
   if(stale){assert(String(by('apply').content).includes('preflight failed'));return reply('Stale handoff rejected without writing.');}
   const receipt=JSON.parse(by('apply').content);assert.equal(receipt.status,'applied');assert.equal(receipt.evidence_reference,'handoff:'+preview.preview_id);
   if(!by('apply-again'))return call('apply-again','agent_handoff_apply',{preview_id:preview.preview_id});
   assert.equal(JSON.parse(by('apply-again').content).status,'applied');return reply('Handoff applied once.');
  }
  if(user==='review-patch-parent'){
   if(!by('spawn-review'))return call('spawn-review','agent_spawn',{agent_id:'main',objective:'review-patch-child',title:'Review patch',review_patch:true});
   const spawned=JSON.parse(by('spawn-review').content);const id=spawned.id;reviewJob=id;
   assert.equal(spawned.spec.review_patch,true);assert.equal(spawned.spec.isolation,'worktree');assert.equal(spawned.spec.keep_worktree,true);
   const waits=results.filter(r=>r.tool_call_id?.startsWith('review-wait-'));
   const finished=waits.some(r=>{try{return JSON.parse(r.content).jobs?.some(j=>j.id===id&&j.state==='succeeded');}catch{return false;}});
   if(!finished)return call('review-wait-'+(++sequence),'agent_wait_any',{job_ids:[id],timeout_ms:30000});
   if(!by('preview-review'))return call('preview-review','agent_handoff_preview',{job_id:id,target:'project'});
   const preview=JSON.parse(by('preview-review').content);assert.equal(preview.status,'review_required');assert.equal(preview.review_required,true);
   assert(preview.files.some(f=>f.path==='src/review.py'&&f.after_text.includes('FIX = true')));
   if(!by('apply-review'))return call('apply-review','agent_handoff_apply',{preview_id:preview.preview_id});
   assert(String(by('apply-review').content).includes('human review'));return reply('Review patch prepared.');
  }
  if(user==='handoff-cross-owner'){
   if(!results.length)return call('cross','agent_handoff_preview',{job_id:firstJob,target:'project'});
   assert(String(results.at(-1).content).includes('another chat'));return reply('Cross-owner handoff denied.');
  }
  if(user==='handoff-replay'){
   if(!results.length)return call('apply-replayed','agent_handoff_apply',{preview_id:firstPreview});
   assert.equal(JSON.parse(results.at(-1).content).status,'applied');return reply('Original receipt reused.');
  }
  reply('Fixture complete.');
 }catch(error){console.error('Fixture model error:',error);res.statusCode=500;res.end(JSON.stringify({error:String(error)}));}
});
await new Promise(resolve=>model.listen(0,'127.0.0.1',resolve));
const pause=ms=>new Promise(r=>setTimeout(r,ms));
async function until(check){for(const end=Date.now()+25000;Date.now()<end;){const value=await check();if(value)return value;await pause(30);}throw Error('Timed out');}
async function launch(){
 const log=path.join(root,'server.log'), fd=fs.openSync(log,'w');
 server=spawn(binary,['serve',workspace,state,'127.0.0.1:0'],{env:{PATH:'/usr/bin:/bin',HOME:root,KAGEHA_HOME:root,KAGEHA_GIT_BIN:'/usr/bin/git',KAGEHA_SERVER_TOKEN:token,KAGEHA_PROVIDER_URL:`http://127.0.0.1:${model.address().port}/v1/chat/completions`,KAGEHA_MODEL:'fixture',KAGEHA_LEGACY_SESSIONS_DIR:path.join(root,'empty')},stdio:['ignore',fd,fd]});fs.closeSync(fd);children.push(server);
 url=await until(()=>{if(server.exitCode!==null)throw Error(fs.readFileSync(log,'utf8'));const match=fs.readFileSync(log,'utf8').match(/listening on ([^\s]+)/);return match&&'http://'+match[1];});
}
async function request(route,body,timeout=60000){const response=await fetch(url+route,{method:body===undefined?'GET':'POST',headers:{Authorization:'Bearer '+token,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body),signal:AbortSignal.timeout(timeout)});const text=await response.text();assert.equal(response.status,200,route+': '+text);return JSON.parse(text);}
async function stop(){if(server&&server.exitCode===null&&server.signalCode===null){server.kill('SIGTERM');await Promise.race([new Promise(r=>server.once('exit',r)),pause(3000)]);if(server.exitCode===null&&server.signalCode===null){server.kill('SIGKILL');await new Promise(r=>server.once('exit',r));}}}
try{
 await launch();const parent=(await request('/api/sessions',{})).session_id;
 await request('/api/permissions',{session_id:parent,profile:'full',confirm_full:true});
 const result=await request('/api/chat/send',{session_id:parent,client_request_id:'first',message:'handoff-parent'});
 assert.equal(result.status,'success');assert.equal(result.message,'Handoff applied once.');assert.equal(fs.readFileSync(path.join(workspace,'src/new.py'),'utf8'),'VALUE = 42\n');
 assert.equal(fs.readFileSync(path.join(workspace,'baseline.txt'),'utf8'),'user dirty work\n');
 let job=(await request('/api/jobs/'+firstJob)).job;assert.equal(job.handoff.status,'applied');assert.equal(job.handoff.preview_id,firstPreview);
 const review=await request('/api/chat/send',{session_id:parent,client_request_id:'review',message:'review-patch-parent'});
 assert.equal(review.status,'success');assert.equal(review.message,'Review patch prepared.');
 assert(!fs.existsSync(path.join(workspace,'src/review.py')));
 const reviewState=(await request('/api/jobs/'+reviewJob)).job;
 assert.equal(reviewState.spec.review_patch,true);assert.equal(reviewState.handoff.status,'review_required');
 assert.equal(reviewState.handoff.review_required,true);assert.equal(reviewState.workspace.options.keep_worktree,true);
 const other=(await request('/api/sessions',{})).session_id;
 const denied=await request('/api/chat/send',{session_id:other,client_request_id:'cross',message:'handoff-cross-owner'});assert.equal(denied.status,'success');
 const stale=await request('/api/chat/send',{session_id:parent,client_request_id:'stale',message:'handoff-stale-parent'});assert.equal(stale.status,'success');assert.equal(fs.readFileSync(path.join(workspace,'src/stale.py'),'utf8'),'USER CHANGE\n');
 await stop();await launch();
 // A fresh call after process restart must return the old receipt, never touch the user-edited target.
 fs.writeFileSync(path.join(workspace,'src/new.py'),'later user edit\n');
 const replay=await request('/api/chat/send',{session_id:parent,client_request_id:'replay',message:'handoff-replay'});assert.equal(replay.status,'success');assert.equal(fs.readFileSync(path.join(workspace,'src/new.py'),'utf8'),'later user edit\n');
 job=(await request('/api/jobs/'+firstJob)).job;assert.equal(job.handoff.status,'applied');
 console.log(JSON.stringify({passed:true,checks:['review-only patch and refused apply','model tool handoff','worker worktree files','unrelated dirty preserved','cross-owner denied','stale target conflict','idempotent receipt after restart'],root}));
}finally{await stop();await new Promise(r=>model.close(r));}
