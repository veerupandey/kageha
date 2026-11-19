// One chat request, no corrective messages/approval replies during execution.
// Default uses a deliberately premature model; --real uses the configured provider.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import http from 'node:http';
import {spawn,execFileSync} from 'node:child_process';
import {page,facts} from './workflow_site.mjs';
const app=process.env.KAGEHA_LOOP_BIN,browser=process.env.KAGEHA_BROWSER_BIN;
assert(app&&browser,'Set absolute Rust host and browser binaries');
const real=process.argv.includes('--real');
const ordinarySearch=process.argv.includes('--ordinary-search');
const ordinaryTabs=process.argv.includes('--ordinary-tabs');
const ordinaryChat=ordinaryTabs||ordinarySearch||process.argv.includes('--ordinary-chat');
const overshoot=process.argv.includes('--overshoot');
const duplicateUpload=process.argv.includes('--duplicate-upload');
const wrongNavigation=process.argv.includes('--wrong-navigation');
const unverifiedClick=process.argv.includes('--unverified-click');
const badUploadLocator=process.argv.includes('--bad-upload-locator');
let injectedClick=false;
let badUploadAttempts=0;
const decisionMode=process.argv.find(v=>v.startsWith('--decisions='))?.split('=')[1]||'off';
assert(['off','shadow','active'].includes(decisionMode));
const decisionThreshold=Number(process.env.KAGEHA_TEST_DECISION_THRESHOLD||0.9);
const confidenceCutoff=process.env.KAGEHA_TEST_CONFIDENCE_CUTOFF===undefined?undefined:Number(process.env.KAGEHA_TEST_CONFIDENCE_CUTOFF);
assert(confidenceCutoff===undefined||(Number.isFinite(confidenceCutoff)&&confidenceCutoff>=0&&confidenceCutoff<=1));
const missingConfidence=process.env.KAGEHA_TEST_MISSING_CONFIDENCE==='1';
const batchThreshold=Number(process.env.KAGEHA_TEST_BATCH_THRESHOLD||0.99);
assert([decisionThreshold,batchThreshold].every(v=>Number.isFinite(v)&&v>=0.5&&v<=1));
const decisionFailure=process.argv.includes('--decision-failure');
const fixtureRejected=decisionFailure||missingConfidence;
const realDecisions=process.argv.includes('--real-decisions');
const probeOnly=process.argv.includes('--probe-only');
const decisionFirst=process.argv.includes('--decision-first')||process.argv.includes('--typed-decisions');
const typedDecisions=decisionFirst;
const decisionModel=process.env.KAGEHA_TEST_DECISION_MODEL||(typedDecisions?'@cf/cloudflare/clef-flash':'advice-fixture');
assert(!realDecisions||real,'Real decisions require --real');
let runStarted,runElapsedMs,benchmark;
let decisionCalls=0;
// Optional local credential file; never print or copy its contents to fixture logs.
if(process.env.KAGEHA_TEST_ENV_FILE){for(const line of fs.readFileSync(process.env.KAGEHA_TEST_ENV_FILE,'utf8').split('\n')){const m=line.match(/^\s*(?:export\s+)?(OPENAI_API_KEY|CLOUDFLARE_API_TOKEN|CLOUDFLARE_ACCOUNT_ID|CMD_API_KEY|GEMINI_API_KEY)\s*=\s*(.*)$/);if(m&&!process.env[m[1]])process.env[m[1]]=m[2].trim().replace(/^(["'])(.*)\1$/,'$2');}}
if(real&&!process.env.KAGEHA_TEST_API_KEY)process.env.KAGEHA_TEST_API_KEY=process.env.OPENAI_API_KEY;
if(realDecisions){
 const key=decisionModel.startsWith('typesafe/')?'CMD_API_KEY':decisionModel==='gpt-6-luna'?'OPENAI_API_KEY'
  :decisionModel==='gemini-decision-1'?'GEMINI_API_KEY':'CLOUDFLARE_API_TOKEN';
 if(!process.env[key]||(key==='CLOUDFLARE_API_TOKEN'&&!process.env.CLOUDFLARE_ACCOUNT_ID)){
  console.log(JSON.stringify({preflight_error:'decision_credentials_unavailable',credential:key}));
  throw new Error('Configure isolated credentials for the selected decision provider');
 }
}
if(real&&(!probeOnly||process.argv.includes('--require-main-credentials'))&&!process.env.KAGEHA_TEST_API_KEY){
 console.log(JSON.stringify({preflight_error:'main_model_credentials_unavailable'}));
 throw new Error('Configure KAGEHA_TEST_API_KEY or OPENAI_API_KEY for the isolated benchmark');
}
const root=fs.mkdtempSync('/tmp/kageha-workflow-e2e-');fs.chmodSync(root,0o700);
const children=[],audit=[],requests=[],providerReplies=[];let modelCalls=0,earlyFinals=0,workflowRequests=0,chat,base,uploadPath;
const token='workflow-isolated-test-authentication-token';
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
async function until(read,ms=30000){const end=Date.now()+ms;let last;while(Date.now()<end){try{const r=await read();if(r)return r;}catch(e){last=e;}await sleep(100);}throw last||Error('Timed out');}
function start(bin,args,env){const log=root+'/'+children.length+'.log';const fd=fs.openSync(log,'a',0o600);const child=spawn(bin,args,{env,stdio:['ignore',fd,fd]});fs.closeSync(fd);children.push(child);return log;}
async function forwardModel(res, raw, role) {
 const started=performance.now(), controller=new AbortController();
 const row={role,model:role==='decision'?decisionModel:(process.env.KAGEHA_TEST_MODEL||'gpt-5.4')};
 providerReplies.push(row);
 const abort=()=>{if(!res.writableEnded)controller.abort();};res.on('close',abort);
 const timer=setTimeout(()=>controller.abort(),120000);
 try {
  const response=await fetch(process.env.KAGEHA_TEST_PROVIDER_URL||'https://api.openai.com/v1/chat/completions',{method:'POST',headers:{Authorization:'Bearer '+process.env.KAGEHA_TEST_API_KEY,'Content-Type':'application/json'},body:raw,signal:controller.signal});
  const body=await response.text();let value;try{value=JSON.parse(body);}catch{}
  Object.assign(row,{status:response.status,error_code:value?.error?.code,error_type:value?.error?.type,choices:value?.choices?.length,usage:value?.usage});
  if(!res.destroyed){res.statusCode=response.status;res.setHeader('Content-Type','application/json');if(response.headers.has('retry-after'))res.setHeader('Retry-After',response.headers.get('retry-after'));res.end(body);}
 } catch(error) {row.error=controller.signal.aborted?'cancelled_or_timed_out':error.name;if(!res.destroyed){res.statusCode=502;res.end(JSON.stringify({error:{message:'Benchmark provider relay failed'}}));}}
 finally {clearTimeout(timer);res.off('close',abort);row.elapsed_ms=Math.round(performance.now()-started);}
}
let actions=[];
const add=(name,id,args={})=>actions.push({name,id,args});
for(const [id,text]of[['name',facts.name],['email',facts.email]])add('browser_fill',id,{text});add('browser_click','next');
add('early');add('browser_fill','company',{text:facts.company});add('browser_fill','month',{text:'1'});add('browser_press','month',{key:'ArrowRight'});add('browser_fill','year',{text:'2023'});add('browser_check','current',{checked:true});add('browser_upload','file');add('repair');add('early');add('browser_click','next');
for(const [id,choice]of[['age','Yes'],['school','Yes'],['authorized','Yes'],['sponsor','No']]){add('browser_click',id);actions.push({name:'browser_click',choice});}add('browser_click','next');add('browser_check','consent',{checked:true});add('browser_click','next');add('browser_fill','signature',{text:facts.name});add('browser_fill','date',{text:facts.date});add('browser_check','decline',{checked:true});add('browser_click','next');
if(duplicateUpload)actions.splice(actions.findIndex(a=>a.name==='repair')+1,0,{name:'browser_upload',id:'file',selector:'input[type=file]',args:{}});
if(ordinarySearch) {
 actions=[{name:'browser_fill',id:'query',args:{text:'wireless keyboards'}},{name:'browser_check',id:'stock',args:{checked:true}},{name:'browser_select',id:'sort',args:{option:'Price'}},{name:'browser_click',id:'next',args:{}}];
}
function searchPage(second) {
 return `<title>Search fixture</title><main>${second?'Second page':'<label>Search<input type="search" id="query"></label><label hidden id="stockRow"><input type="checkbox" id="stock">In stock</label><label hidden id="sortRow">Sort<select id="sort"><option>Default</option><option>Price</option></select></label><a hidden id="next" href="?page=2">Next</a>'}</main><script>
 const emit=(data)=>fetch('/audit',{method:'POST',body:JSON.stringify(data)});
 emit({kind:'stage',step:${second?2:1}});
 document.addEventListener('input',e=>{
   const id=e.target.id,value=id==='stock'?e.target.checked:e.target.value;
   emit({kind:'input',id,value,trusted:e.isTrusted});
   if(id==='query'&&value==='wireless keyboards')stockRow.hidden=false;
   if(id==='stock'&&value)sortRow.hidden=false;
   if(id==='sort'&&value==='Price')next.hidden=false;
 });
 </script>`;
}
const site=http.createServer(async(req,res)=>{try{
 requests.push({method:req.method,url:req.url});
 if(req.url==='/audit'){let raw='';for await(const b of req)raw+=b;const row=JSON.parse(raw);audit.push(row);res.end('ok');return;}
 if(req.url.startsWith('/typed')){
  let raw='';for await(const b of req)raw+=b;let p=JSON.parse(raw);
  const probeQuestion = (Array.isArray(p.questions) ? p.questions.find(q=>q.name==='observed_color') : p.questions?.observed_color);
  if(!probeQuestion)decisionCalls++;
  const nativeOpenAI=Array.isArray(p.questions);
  const gemini=decisionModel==='gemini-decision-1';
  assert(!p.messages&&!p.tools&&p.questions&&(p.state||p.input),'Expected typed decision payload, not chat');
  if(process.env.KAGEHA_TEST_DECISION_DEBUG==='1'&&decisionCalls===1)fs.writeFileSync(root+'/first-typed-request.json',JSON.stringify(p,null,2),{mode:0o600});
  if(realDecisions){
   const jev=decisionModel.startsWith('typesafe/');const endpoint=gemini?'https://generativelanguage.googleapis.com/v1beta/decisions':nativeOpenAI?'https://api.openai.com/v1/decisions':jev?'https://api.commandcode.ai/provider/v1/systemone':`https://api.cloudflare.com/client/v4/accounts/${process.env.CLOUDFLARE_ACCOUNT_ID}/ai/run/${decisionModel.startsWith('@cf/')?decisionModel:'@cf/cloudflare/'+decisionModel}`;
   const key=gemini?process.env.GEMINI_API_KEY:nativeOpenAI?process.env.OPENAI_API_KEY:jev?process.env.CMD_API_KEY:process.env.CLOUDFLARE_API_TOKEN;
   const row={role:'decision',model:decisionModel,questions:Object.keys(p.questions).length};providerReplies.push(row);const started=performance.now();
   const controller=new AbortController();const abort=()=>{if(!res.writableEnded)controller.abort()};res.on('close',abort);const timeout=setTimeout(()=>controller.abort(),15000);
   try{const response=await fetch(endpoint,{method:'POST',headers:{...(gemini?{'x-goog-api-key':key}:{Authorization:'Bearer '+key}),'Content-Type':'application/json'},body:raw,signal:controller.signal});const body=await response.text();let value;try{value=JSON.parse(body);}catch{};if(process.env.KAGEHA_TEST_DECISION_DEBUG==='1'&&decisionCalls===1)fs.writeFileSync(root+'/first-typed-response.json',body,{mode:0o600});Object.assign(row,{status:response.status,usage:value?.result?.usage||value?.usage});if(!res.destroyed){res.statusCode=response.status;res.setHeader('Content-Type','application/json');res.end(body);}}
   catch(error){row.error=controller.signal.aborted?'cancelled_or_timed_out':error.name;if(!res.destroyed){res.statusCode=502;res.end('{}');}}finally{row.elapsed_ms=Math.round(performance.now()-started);clearTimeout(timeout);res.off('close',abort);}return;
  }
  if(gemini){assert.equal(typeof p.input,'string');p.state=JSON.parse(p.input);}
  if(nativeOpenAI)p={state:p.input[0].content[0].data,questions:Object.fromEntries(p.questions.map(q=>[q.name,{instructions:q.instructions,criteria:Object.fromEntries(q.choices.map(c=>[c.value,c.description]))}]))};
  const state=typeof p.state==='string'?JSON.parse(p.state):p.state;
  if(probeQuestion){
   const answer={type:'choice',choice:'blue',confidence:missingConfidence?undefined:1,probabilities:{blue:1,red:0}};
   const answers=nativeOpenAI?[{...answer,name:'observed_color',probabilities:[{value:'blue',probability:1},{value:'red',probability:0}]}]:{observed_color:answer};
   res.setHeader('Content-Type','application/json');res.end(JSON.stringify({answers}));return;
  }
  const controls=state.page_reference_data.controls;const answers={};let next=actions[0];
  const chosen={};
  if(ordinaryTabs){
   const question=Object.entries(p.questions).find(([id,q])=>id.startsWith('nav_')&&q.instructions.startsWith('Switching to managed browser tab'));
   if(question)chosen[question[0]]='true';
  }else if(next?.name==='browser_click'){
   const c=controls.find(c=>next.choice?c.role==='option'&&c.name===next.choice:c.id===next.id);
   if(c){const question=Object.entries(p.questions).find(([id,q])=>id.startsWith('nav_')&&q.instructions.startsWith('Clicking the currently observed control ref '+c.ref+' '));if(question){chosen[question[0]]='true';if(!fixtureRejected&&decisionMode==='active')actions.shift();}}
  }else{
   while(['browser_fill','browser_check','browser_select'].includes(next?.name)){
    const c=controls.find(c=>c.id===next.id),value=next.name==='browser_fill'?next.args.text:next.args.option??next.args.checked;
    const question=Object.entries(p.questions).find(([id,q])=>id.startsWith('field_')&&q.instructions.includes('Map observed control ref '+c?.ref+' '));
    const option=question&&Object.keys(question[1].criteria).find(k=>question[1].criteria[k].includes('('+JSON.stringify(value)+')'));
    if(!question||!option)break;chosen[question[0]]=option;
    if(fixtureRejected||decisionMode!=='active')break;actions.shift();next=actions[0];
   }
  }
  for(const[id,q]of Object.entries(p.questions)){const choice=chosen[id]||((id==='batch_safe'||id.startsWith('eligible_'))?'true':id.startsWith('nav_')?'false':'none');assert(choice in q.criteria);answers[id]={type:'choice',choice,confidence:missingConfidence?undefined:1,probabilities:Object.fromEntries(Object.keys(q.criteria).map(k=>[k,k===choice?1:0]))};}
  res.setHeader('Content-Type','application/json');res.end(JSON.stringify(nativeOpenAI?{answers:decisionFailure?[]:Object.entries(answers).map(([name,a])=>({...a,name,probabilities:Object.entries(a.probabilities).map(([value,probability])=>({value,probability}))})),usage:{input_tokens:100,output_tokens:0}}:{answers:decisionFailure?{}:answers,usage:{input_tokens:100,output_tokens:0}}));return;
 }
 if(req.url==='/v1/chat/completions'){
  let raw='';for await(const b of req)raw+=b;const p=JSON.parse(raw);
  if(p.model==='browser-decision-fixture'){
   decisionCalls++;
   assert(!p.tools?.length,'Decision model must not receive action tools');
   if(realDecisions){p.model=decisionModel;await forwardModel(res,JSON.stringify(p),'decision');return;}
   const answer=decisionFailure?'malformed decision output':JSON.stringify({recommendation:'use_observed_controls',confidence:0.99,rationale:'Use observed controls on the current form'});
   res.setHeader('Content-Type','application/json');res.end(JSON.stringify({choices:[{message:{role:'assistant',content:answer}}]}));return;
  }
  const system=p.messages[0]?.content;
  const reflection=typeof system==='string'&&system.startsWith('You review the journal of a request');
  if(!real && (reflection || (typeof system==='string'&&system.startsWith('Write one brief')&&system.includes('acknowledg')))){
   res.setHeader('Content-Type','application/json');res.end(JSON.stringify({choices:[{message:{role:'assistant',content:reflection?'{"lessons":[]}':'I will check the form.'}}]}));return;
  }
  modelCalls++;
  if(real){
   if(process.env.KAGEHA_TEST_REASONING){p.reasoning_effort=process.env.KAGEHA_TEST_REASONING;raw=JSON.stringify(p);}
   await forwardModel(res,raw,'executor');return;
  }
  if(ordinaryChat){
   // Read the last observed controls the same way a regular executor does.
   const snapshots=p.messages.filter(m=>m.role==='tool').flatMap(m=>{try{return [JSON.parse(m.content)]}catch{return []}});
   const snapshot=snapshots.filter(v=>v.controls).at(-1);
   const field=['name','email'].find(id=>!audit.some(e=>e.kind==='input'&&e.id===id&&e.value===facts[id]));
   let call;
   if(ordinaryTabs){
    if(!snapshots.some(v=>v.observation_id))call={name:'browser_open',arguments:{url:origin+'/other'}};
   }else if(ordinarySearch){
    const next=actions[0];
    if(next){
     const fresh=snapshots.at(-1);
     const control=fresh?.controls?.find(c=>c.id===next.id);
     if(!control)call={name:'browser_snapshot',arguments:{}};
     else {call={name:next.name,arguments:{...next.args,target:control.ref,observation_id:fresh.observation_id}};actions.shift();}
    }
   }else if(field&&!snapshot)call={name:'browser_snapshot',arguments:{}};
   else if(field)call={name:'browser_fill',arguments:{observation_id:snapshot.observation_id,target:snapshot.controls.find(c=>c.id===field).ref,text:facts[field]}};
   const message=call?{role:'assistant',content:null,tool_calls:[{id:'chat-'+modelCalls,type:'function',function:{name:call.name,arguments:JSON.stringify(call.arguments)}}]}:{role:'assistant',content:'Filled both fields.'};
   res.setHeader('Content-Type','application/json');res.end(JSON.stringify({choices:[{message}]}));return;
  }
  const c=p.messages.filter(m=>m.role==='system'&&m.content?.includes('"fresh_browser_observation"')).at(-1);assert(c,'Workflow context must accompany every model call');
  const context=JSON.parse(c.content.slice(c.content.indexOf('\n{')+1));const obs=context.fresh_browser_observation;
  let action=actions[0],call,answer='I finished or need another user turn.';
  if(action?.name==='early'){actions.shift();earlyFinals++;}
  else if(action?.name==='repair'){
   const last=p.messages.filter(m=>m.role==='tool').at(-1);let report;try{report=JSON.parse(last?.content);}catch{}
   if(context.pending_original_actions?.length===0&&obs.text?.includes(facts.resume)){
    assert(context.verified_recoveries.some(r=>r.action==='upload'&&r.visible_attachment_names?.includes(facts.resume)),
      'Model context must supersede the stale upload error with verified filename evidence');
    actions.shift();answer='Upload recovered; I will stop here.';earlyFinals++;
   }
   else if(last?.content?.includes('"state":"verified"')&&last?.content?.includes('visible_attachment_name')){actions.shift();answer='Upload recovered; I will stop here.';earlyFinals++;}
   else if(report?.receipts){const receipt=report.receipts[0];assert(receipt?.action==='upload');call={name:'browser_reconcile',arguments:{request_id:receipt.request_id}};}
   else call={name:'browser_receipts',arguments:{}};
  }else if(action){
   const control=obs.controls.find(c=>action.choice?c.role==='option'&&c.name===action.choice:c.id===action.id);
   assert(control,JSON.stringify({missing:action,controls:obs.controls.map(c=>({id:c.id,name:c.name,role:c.role}))}));
   call={name:action.name,arguments:action.name==='browser_upload'?{target:action.selector||'#file',path:uploadPath}:{target:control.ref,observation_id:obs.observation_id,...action.args}};
   if (badUploadLocator && action.name==='browser_upload' && badUploadAttempts<2) {
    call.arguments.target=badUploadAttempts++===0?'e99':'#fixture-file-input-that-does-not-exist';
   } else if (!injectedClick && action.name==='browser_click' && (wrongNavigation || unverifiedClick)) {
    injectedClick=true;
    if (wrongNavigation) call.arguments.expected_url=origin+'/form?step=2';
    else call.arguments.expected_selector='#fixture-condition-that-never-appears';
   } else actions.shift();
  }
  const message=call?{role:'assistant',content:null,tool_calls:[{id:'fixture-'+modelCalls,type:'function',function:{name:call.name,arguments:JSON.stringify(call.arguments)}}]}:{role:'assistant',content:answer};
  if(overshoot&&call&&actions.length===0){message.tool_calls.push({id:'fixture-overshoot',type:'function',function:{name:'browser_click',arguments:JSON.stringify({target:'#submit',observation_id:obs.observation_id})}});}
  res.setHeader('Content-Type','application/json');res.end(JSON.stringify({choices:[{message}]}));return;
 }
 res.setHeader('Content-Type','text/html');res.end(ordinarySearch?searchPage(req.url.includes('page=2')):page());
}catch(error){console.error(JSON.stringify({fixture_error:error.message}));res.statusCode=500;res.end(JSON.stringify({error:'fixture failed'}));}});
await new Promise(r=>site.listen(0,'127.0.0.1',r));const origin='http://127.0.0.1:'+site.address().port;
async function api(route,body){const r=await fetch(base+route,{method:body?'POST':'GET',headers:{Authorization:'Bearer '+token,...(body?{'Content-Type':'application/json'}:{})},body:body?JSON.stringify(body):undefined});const j=await r.json();assert.equal(r.status,200,JSON.stringify(j));return j;}
async function browserPost(op,body={}){const s=await api('/api/sessions/'+chat+'/browser/state');return api('/api/sessions/'+chat+'/browser/'+op,{bindingId:s.bindingId,...body});}
function decisionRecords(){
 const db=root+'/state/native-chats.sqlite';
 const rows=JSON.parse(execFileSync('sqlite3',['-json',db,"SELECT payload FROM ui_events WHERE event='event' AND payload LIKE '%browser_decision%'"],{encoding:'utf8'}).trim()||'[]');
 return rows.map(row=>JSON.parse(row.payload)).filter(event=>event.kind==='browser_decision'&&event.payload.purpose==='browser_action').map(event=>event.payload.decision).filter(Boolean);
}
function decisionProof(){
 const records=decisionRecords();
 const sent=records.filter(record=>record.status==='sent_to_executor');
 const verified=records.filter(record=>record.status==='verified');
 for(const receipt of verified){
  assert(sent.some(decision=>decision.decision_id===receipt.decision_id&&decision.calls[receipt.call_id]?.action===receipt.action),
    'A verified decision must identify its original dispatch');
 }
 const dispatches=sent.flatMap(record=>Object.keys(record.calls));
 assert.equal(new Set(dispatches).size,dispatches.length,'Decision dispatch IDs must be unique');
 assert(sent.every(record=>record.main_model_skipped===true),'A dispatched decision must replace the main model call');
 if(decisionMode!=='active'||fixtureRejected){
  assert.equal(dispatches.length,0,'Off, Compare and invalid responses must not dispatch a decision');
 }else if(!real){
  assert(verified.length>0,'An active decision must receive a verified native result');
 }
 if(fixtureRejected&&decisionFirst){
  const reason=missingConfidence?'missing_confidence':'invalid_response';
  assert(records.some(record=>record.attempts?.some(attempt=>attempt.reason===reason)),'Typed failures must have an actionable audit reason');
 }
 return {
  decision_dispatches:dispatches.length,decision_verified:verified.length,executor_calls_skipped:sent.length,
  fallback_reasons:records.flatMap(record=>record.attempts||[]).map(attempt=>attempt.reason).filter(reason=>reason!=='accepted'),
 };
}
let passed=false;
try{
 fs.mkdirSync(root+'/workspace');fs.mkdirSync(root+'/browser',{mode:0o700});
 const env={HOME:root,PATH:'/usr/bin:/bin',KAGEHA_SERVER_TOKEN:token,KAGEHA_BROWSER_STATE_DIR:root+'/browser',KAGEHA_MODEL:real?process.env.KAGEHA_TEST_MODEL||'gpt-5.4':'workflow-fixture',KAGEHA_PROVIDER_URL:origin+'/v1/chat/completions',KAGEHA_API_KEY:''};
 // Chromium uses an explicitly isolated user-data-dir. Keep its OS home valid:
 // inventing a macOS home can stall the network service before the first load.
 start(browser,['--state-dir',root+'/browser','--instance-id','workflow-fixture','--application-id','workflow-fixture','--chromium','/Applications/Google Chrome.app/Contents/MacOS/Google Chrome','--standalone','on'],{...process.env});
 await until(()=>fs.existsSync(root+'/browser/daemon.json'));
 const log=start(app,['serve',root+'/workspace',root+'/state','127.0.0.1:0'],env);
 base=await until(()=>{const m=fs.readFileSync(log,'utf8').match(/listening on (\S+)/);return m&&'http://'+m[1];});
 chat=(await api('/api/sessions',{})).session_id;
 if(decisionMode!=='off'){
  await api('/api/provider-setup/model',{provider:'fixture',model:typedDecisions?decisionModel:'browser-decision-fixture',model_id:'fixture/browser-guidance',base_url:origin+(typedDecisions?'/typed':'/v1/chat/completions'),protocol:typedDecisions?(decisionModel==='gemini-decision-1'?'gemini_decisions':decisionModel==='gpt-6-luna'?'openai_decisions':decisionModel.startsWith('typesafe/')?'system_one':'clef'):'openai_compat'});
  await api('/api/models/decisions',{
   mode:decisionFirst?decisionMode:'off',browser_actions_follow_mode:decisionFirst,
   model:'fixture/browser-guidance',fallback_models:[],judge_fallback:false,threshold:decisionThreshold,
   confidence_cutoff:confidenceCutoff,batch_confidence_cutoff:batchThreshold,batch_threshold:batchThreshold,
   timeout_s:3,max_states:3,overrides:decisionFirst?{}:{browser_guidance:{mode:decisionMode},recovery:{mode:decisionMode}},
  });
  if(decisionFirst&&(!real||probeOnly)){
   const probe=await api('/api/models/decisions/test',{
    model_id:'fixture/browser-guidance',score_metric:confidenceCutoff===undefined?'selected_probability':'provider_confidence',
   });
   if(probeOnly)console.log(JSON.stringify({provider_probe:probe}));
   const expected=!missingConfidence||confidenceCutoff===undefined;
   assert.equal(probe.ok,expected,JSON.stringify(probe));
  }
 }

 if(probeOnly){passed=true;}else{
 await api('/api/permissions',{session_id:chat,profile:'full',confirm_full:true});
 await browserPost('connect');await browserPost('lease',{action:'take'});let s=await api('/api/sessions/'+chat+'/browser/state');
 await browserPost('action',{action:'navigate',url:origin+'/form',controlGeneration:s.controlGeneration});
 const staleLease=await fetch(base+'/api/sessions/'+chat+'/browser/lease',{method:'POST',headers:{Authorization:'Bearer '+token,'Content-Type':'application/json'},body:JSON.stringify({bindingId:'old-document-binding',action:'return',controlGeneration:s.controlGeneration+1})});assert.equal(staleLease.status,409,'Stale control generation cannot release another lease');
 await api('/api/sessions/'+chat+'/browser/lease',{bindingId:'old-document-binding',action:'return',controlGeneration:s.controlGeneration});s=await api('/api/sessions/'+chat+'/browser/state');
 await until(()=>audit.some(e=>e.kind==='stage'&&e.step===1));await sleep(150);s=await api('/api/sessions/'+chat+'/browser/state');
 const form=new FormData();form.append('file',new Blob(['synthetic resume fixture; no real applicant data']),facts.resume);const r=await fetch(base+'/api/sessions/'+chat+'/upload',{method:'POST',headers:{Authorization:'Bearer '+token},body:form});assert.equal(r.status,200);uploadPath=(await r.json()).path;
 const goal=(id,description,selector,equals,text)=>({id,description,selector,equals,...(text?{text}:{}),...(id==='review'?{}:{depends_on:['review']})});
 const workflow={version:1,profile:'private',target_id:s.tabs.find(t=>t.active).tabHandle,origin,limits:{continuations:24,no_progress:5,checkpoints:1000},goals:[goal('review','Application draft at Review without submitting','h1','Review'),goal('resume','Original resume is visibly attached','div','Resume/CV '+facts.resume,'Resume/CV'),goal('authorization','US work authorization is Yes','div','Are you legally authorized to work in the United States? Yes','Are you legally authorized'),goal('sponsorship','Employer sponsorship is No','div','Do you now or in the future require sponsorship? No','Do you now'),goal('disability','Disability self-identification is declined','div','Disability I do not want to answer','Disability')],known_values:{name:facts.name,email:facts.email,company:facts.company,month:'1',year:'2023',signature:facts.name,date:facts.date,current:true,consent:true,decline:true,authorized:'Yes',sponsor:'No',age:'Yes',school:'Yes'},known_context:`Synthetic test facts: ${JSON.stringify(facts)}. Complete all six form steps. Age18+=Yes, highschool/GED=Yes, USauthorized=Yes, sponsorship=No, currentjob=true, fixturetermsconsent=approved, disability=I do not want to answer, signature=${facts.name}, date=${facts.date}, resume chat-owned path=${uploadPath}. Start month1/year2023; the year segment can redirect focus, so use Month then ArrowRight to focusedYear. Do not submit. No real account or legal agreement is involved.`};
 const message=`Complete the existing six-step application draft using the supplied known facts and attached resume. Stop at Review WITHOUT submitting. Preserve all entered values. Use only native browser tools. Do not open another tab/draft/account. Inspect exact question IDs and ancestor_context before choosing generic dropdown options. Uploaded widgets can clear FileList: inspect/reconcile the original receipt rather than upload again. Current job checkbox is styled with a visible associated label. Date widgets may require keyboard navigation from Month to Year. Continue after recoverable undispatched errors with fresh observations; do not ask me to prompt you again for an intermediate step. Use ${uploadPath} for the original resume. All necessary test facts and fixture consent are supplied in the caller workflow context.`;
 workflowRequests++;let finished=false;runStarted=performance.now();
 const task=(async()=>{
  const accepted=await api('/api/chat/stream',{session_id:chat,client_request_id:'one-request',max_steps:300,...(ordinaryChat?{message:ordinaryTabs?`Open ${origin}/other and then switch to the existing tab ${origin}/form`:ordinarySearch?'In the browser search for "wireless keyboards", filter by "In stock", select "Price", then go to the next page.':`In the browser fill Name with "${facts.name}" and Email with "${facts.email}". Stay on this page.`}:{message,workflow}),event_stream:'session'});
  assert.equal(accepted.status,'accepted');
  const stream=await fetch(base+'/api/sessions/'+chat+'/events/stream?turn_id='+encodeURIComponent(accepted.turn_id),{headers:{Authorization:'Bearer '+token}});
  assert.equal(stream.status,200);
  let buffer='';
  const decoder=new TextDecoder();
  for await(const chunk of stream.body){
   buffer+=decoder.decode(chunk,{stream:true});
   let end;
   while((end=buffer.indexOf('\n\n'))>=0){
    const frame=buffer.slice(0,end).split('\n');buffer=buffer.slice(end+2);
    if(frame.includes('event: update')){
     const update=JSON.parse(frame.filter(line=>line.startsWith('data: ')).map(line=>line.slice(6)).join('\n'));
     const done=update.events?.find(event=>event.kind==='done'&&event.turn_id===accepted.turn_id);
     if(done)return done.payload;
    }
   }
  }
  throw Error('Workflow event stream ended without a final result');
 })().finally(()=>finished=true);
 // Observe rejection immediately while polling so cleanup cannot mask the original failure.
 void task.catch(()=>{});
 let approvalRequests=0, changedDecisionSettings=false;
 const runDeadline=Date.now()+12*60*1000;
 while(!finished){
  if(process.argv.includes('--settings-change') && !changedDecisionSettings && decisionCalls>0){
   await api('/api/models/decisions',{mode:'off',model:'fixture/browser-guidance',fallback_models:[],judge_fallback:false,threshold:decisionThreshold,timeout_s:3,confidence_cutoff:.999,overrides:{browser_action:{mode:'off'}}});
   changedDecisionSettings=true;
  }
  assert(Date.now()<runDeadline,'Agent exceeded isolated benchmark time budget');const current=await api('/api/sessions/'+chat);if(current.pending_approval){approvalRequests++;throw Error('Unexpected approval: test authority was configured before execution');}await sleep(500);}
 const answer=await task;
 if(ordinaryChat){
  assert.equal(answer.status,'success',JSON.stringify(answer));
  const proof=decisionProof();
  if(ordinaryTabs){
   const current=await api('/api/sessions/'+chat+'/browser/state');
   assert(current.tabs.some(tab=>tab.active&&tab.url===origin+'/form'));
   assert(current.tabs.some(tab=>tab.url===origin+'/other'));
   assert.equal(current.decisionRoute.last_action,'browser_tabs');
  }else{
   const expected=ordinarySearch?{query:'wireless keyboards',stock:true,sort:'Price'}:{name:facts.name,email:facts.email};
   if(ordinarySearch)assert.equal(audit.filter(event=>event.kind==='stage'&&event.step===2).length,1,'Pagination must reach page two once');
   const inputs=audit.filter(event=>event.kind==='input');
   for(const [id,value] of Object.entries(expected)){
    const writes=inputs.filter(input=>input.id===id);
    // Native select uses DOM events and validates the result in its browser receipt.
    assert.equal(writes.filter(input=>input.value===value&&(id==='sort'||input.trusted)).length,1,'Expected one verified write of '+id);
    // Chromium may report partial characters while typing. A different value,
    // a second completion or a reset after completion is a wrong or duplicate write.
    assert(writes.every((input,index)=>index===writes.length-1
      ?input.value===value
      :typeof value==='string'&&typeof input.value==='string'&&input.value!==value&&value.startsWith(input.value)),
      'Wrong or duplicate input for '+id);
   }
   assert(inputs.every(input=>Object.hasOwn(expected,input.id)),'The fixture received unexpected field input');
  }
  const db=root+'/state/native-chats.sqlite';
  const routed=Number(execFileSync('sqlite3',[db,"select count(*) from ui_events where json_extract(payload,'$.payload.decision.status')='routed'"]));
  if(!real || decisionMode!=='active')assert.equal(routed,decisionMode==='active'&&!fixtureRejected?(ordinarySearch?4:1):0);
  assert.equal(audit.filter(e=>e.kind==='SUBMITTED').length,0);
  const elapsed_ms=Math.round(performance.now()-runStarted);
  const counts=JSON.parse(execFileSync('sqlite3',['-json',db,"select count(*) as tools,coalesce(sum(json_extract(payload,'$.status') in ('failed','blocked')),0) as failed_tools from ui_events where event='tool_card' and json_extract(payload,'$.status') in ('done','failed','blocked')"],{encoding:'utf8'}))[0];
  const fallbacks=Number(execFileSync('sqlite3',[db,"select count(*) from ui_events where json_extract(payload,'$.payload.decision.status')='fallback'"]));
  benchmark={...counts,...proof,wrong_or_duplicate_actions:0,elapsed_ms,executor_calls:modelCalls,decision_calls:decisionCalls,decision_applied:routed,decision_fallback:fallbacks,provider_calls:providerReplies};
  fs.writeFileSync(root+'/result.json',JSON.stringify({benchmark,audit}),{mode:0o600});
  passed=true;console.log(JSON.stringify({passed,ordinaryChat,decisionMode,modelCalls,decisionCalls,routed,elapsed_ms,root}));
 } else {
 if(unverifiedClick){
  const status=await api('/api/sessions/'+chat+'/workflow');
  assert.equal(status.workflow.phase,'awaiting_input',JSON.stringify(status));
  assert.match(answer.message,/last browser action could not be verified/i);
  assert.equal(status.workflow.pending.length,1);
  assert.equal(audit.filter(e=>e.kind==='upload'||e.kind==='SUBMITTED').length,0);
  passed=true;
  console.log(JSON.stringify({passed:true,unverified_click:'paused for one handoff',modelCalls,root}));
  process.exitCode=0;
 } else {
 if(decisionFirst && decisionMode!=='off'){
  const browser=await api('/api/sessions/'+chat+'/browser/state');
  assert.equal(browser.decisionRoute.model,'fixture/browser-guidance','Panel identifies the configured decision backend');
  assert.equal(browser.decisionRoute.cutoff,confidenceCutoff??decisionThreshold,'Panel preserves the run-frozen cutoff');
  assert.notEqual(browser.decisionRoute.status,'off','Changing settings does not disable an in-flight run');
  assert(!JSON.stringify(browser.decisionRoute).includes('known_values'),'Route metadata does not expose caller facts');
  if(process.argv.includes('--settings-change'))assert(changedDecisionSettings,'Settings change must happen during execution');
 }
 runElapsedMs=Math.round(performance.now()-runStarted);assert.equal(answer.status,'success',JSON.stringify(answer));
 const status=await api('/api/sessions/'+chat+'/workflow');assert.equal(status.workflow.phase,'verified',JSON.stringify(status));assert.equal(status.workflow.satisfied.length,5);assert.equal(status.workflow.pending.length,0);
 assert.equal(workflowRequests,1);assert.equal(approvalRequests,0);assert.equal(audit.filter(e=>e.kind==='SUBMITTED').length,0);assert.equal(audit.filter(e=>e.kind==='upload').length,1);assert(audit.some(e=>e.kind==='stage'&&e.step===6));
 assert(audit.filter(e=>e.kind==='input').every(e=>e.trusted),'Field writes must come from trusted browser input');
 if(!real){assert(earlyFinals>=2);assert(status.workflow.continuations>=2);if(decisionFirst&&decisionMode==='active'&&!fixtureRejected)assert(modelCalls<28,'Some routine actions must skip the full controlling model');}
 const toolCount=Number(execFileSync('sqlite3',[root+'/state/native-chats.sqlite',"SELECT count(*) FROM ui_events WHERE event='tool_card' AND json_extract(payload,'$.status')='done'"],{encoding:'utf8'}).trim());assert(toolCount>20,'Expected real native browser tool dispatches');
 if(overshoot){const rejected=Number(execFileSync('sqlite3',[root+'/state/native-chats.sqlite',"SELECT count(*) FROM ui_events WHERE event='tool_card' AND payload LIKE '%workflow_boundary_reached%'"],{encoding:'utf8'}).trim());assert.equal(rejected,1,'A second mutation in the completing reply must stop at the fresh goal boundary');}
 if(duplicateUpload){const rejected=Number(execFileSync('sqlite3',[root+'/state/native-chats.sqlite',"SELECT count(*) FROM ui_events WHERE event='tool_card' AND payload LIKE '%workflow_upload_already_attempted%'"],{encoding:'utf8'}).trim());assert.equal(rejected,1,'A second upload attempt must be refused before input');}
 if(wrongNavigation){const rejected=Number(execFileSync('sqlite3',[root+'/state/native-chats.sqlite',"SELECT count(*) FROM ui_events WHERE event='tool_card' AND payload LIKE '%workflow_click_url%'"],{encoding:'utf8'}).trim());assert.equal(rejected,1,'The guessed URL must be rejected before dispatch');}
 if(badUploadLocator){assert.equal(badUploadAttempts,2);assert(audit.some(e=>e.kind==='upload'),'A correct locator must still be able to upload after undispatched attempts');}
 if(decisionMode!=='off'){
  assert(decisionCalls>0&&decisionCalls<=(decisionFirst?64:3),'One frozen run must invoke the decision model within its state budget');
  const rows=JSON.parse(execFileSync('sqlite3',['-json',root+'/state/native-chats.sqlite',"SELECT payload FROM ui_events WHERE payload LIKE '%browser_decision%'"],{encoding:'utf8'}));
  const records=rows.map(r=>JSON.parse(r.payload)).filter(r=>r.kind==='browser_decision'&&(!decisionFirst||r.payload.purpose==='browser_action')).map(r=>r.payload.decision).filter(Boolean);
  assert(records.length>0,'Browser decisions must be audited through Kageha');
  assert(records.every(r=>(decisionMode==='shadow'||fixtureRejected)?(!r.applied&&r.status!=='routed'):(!r.applied||r.status==='applied')),'Only valid active advice may be applied');
  if(!realDecisions&&!fixtureRejected&&decisionMode==='active')assert(records.some(r=>r.applied||r.status==='routed'),'Valid fixture advice must be applied');
 }
 const failedTools=Number(execFileSync('sqlite3',[root+'/state/native-chats.sqlite',"SELECT count(*) FROM ui_events WHERE event='tool_card' AND json_extract(payload,'$.status') IN ('failed','blocked')"],{encoding:'utf8'}).trim());
 const decisionEvents=JSON.parse(execFileSync('sqlite3',['-json',root+'/state/native-chats.sqlite',"SELECT payload FROM ui_events WHERE payload LIKE '%browser_decision%'"],{encoding:'utf8'}).trim()||'[]').map(r=>JSON.parse(r.payload)).filter(r=>r.kind==='browser_decision').map(r=>r.payload.decision).filter(Boolean);
 benchmark={...decisionProof(),confidence_cutoff:confidenceCutoff,score_metric:confidenceCutoff===undefined?'selected_probability':'provider_confidence',decision_threshold:decisionThreshold,confidence_cutoff:confidenceCutoff,batch_confidence_cutoff:batchThreshold,batch_threshold:batchThreshold,elapsed_ms:runElapsedMs,executor_calls:modelCalls,decision_calls:decisionCalls,tools:toolCount,failed_tools:failedTools,decision_first:decisionFirst,routed_actions:decisionEvents.reduce((n,d)=>n+(d.kinds?.length||0),0),multi_action_batches:decisionEvents.filter(d=>(d.kinds?.length||0)>1).length,decision_applied:decisionEvents.filter(d=>d.applied||d.status==='routed').length,decision_fallback:decisionEvents.filter(d=>d.status==='fallback').length,provider_calls:providerReplies};
 if(typedDecisions&&!real&&!fixtureRejected&&decisionMode==='active')assert(benchmark.multi_action_batches>0,'Typed integration must execute an actual multi-action batch');
 passed=true;console.log(JSON.stringify({passed:true,multi_action_batches:benchmark.multi_action_batches,routed_actions:benchmark.routed_actions,elapsed_ms:runElapsedMs,modelCalls,failedTools,realDecisions,decisionModel:realDecisions?decisionModel:undefined,decisionMode,decisionFailure,decisionCalls,provider:real?env.KAGEHA_MODEL:'premature-scripted-model',overshoot,root,chat,user_requests:workflowRequests,corrective_messages:0,approval_replies_during_run:approvalRequests,continuations:status.workflow.continuations,checkpoints:status.workflow.checkpoints,uploads:1,submissions:0,stage:6,tools:toolCount}));
 fs.writeFileSync(root+'/result.json',JSON.stringify({answer,status,audit,benchmark}),{mode:0o600});
 }
 }
 }
}finally{
 if(!passed){fs.writeFileSync(root+'/benchmark-failure.json',JSON.stringify({passed:false,decisionMode,elapsed_ms:runStarted?Math.round(performance.now()-runStarted):null,modelCalls,decisionCalls,provider_calls:providerReplies}),{mode:0o600});console.error(JSON.stringify({failed:true,root,modelCalls,providerReplies:providerReplies.slice(-4),requests:requests.slice(-8),remaining:actions[0],lastAudit:audit.slice(-4)}));}
 for(const child of children.toReversed())child.kill('SIGTERM');await sleep(200);for(const child of children)if(child.exitCode===null&&child.signalCode===null)child.kill('SIGKILL');
 await new Promise(r=>site.close(r));
}
