// Isolated Chromium numeric inputs and durable history across daemon restarts.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import http from 'node:http';
import net from 'node:net';
import {spawn} from 'node:child_process';
import {BrowserClient} from '../sdk/client.mjs';
import {DatabaseSync} from 'node:sqlite';
const root=fs.mkdtempSync('/tmp/kageha-numeric-journal-');fs.chmodSync(root,0o700);
const sleep=ms=>new Promise(r=>setTimeout(r,ms));
const children=[];let daemon;
function start(bin,args){const fd=fs.openSync(root+'/'+children.length+'.log','a',0o600);const p=spawn(bin,args,{stdio:['ignore',fd,fd]});fs.closeSync(fd);children.push(p);return p;}
async function until(fn){for(let i=0;i<250;i++){try{const v=await fn();if(v)return v;}catch{}await sleep(40);}throw Error('Fixture startup failed: '+root);}
const checked=r=>{assert.equal(r.status,'ok',JSON.stringify(r));return r.result;};
const site=http.createServer((req,res)=>{res.setHeader('content-type','text/html');if(req.url==='/frame'){res.end('<label>Frame number<input id="framed" type="number"></label>');return;}res.end(`<title>Numeric fixture</title><label>Budget<input id="budget" type="number" min="0" max="100" step="0.01" required oninput="document.querySelector('#events').textContent=Number(document.querySelector('#events').textContent)+1; if(this.value==='3.00')this.value='3';"></label><output id="events">0</output><label>Readonly<input id="ro" type="number" readonly value="7"></label><label>Disabled<input id="disabled" type="number" disabled value="7"></label><label>Date<input id="date" type="date"></label><label>Plain<input id="plain"></label><label>${"Long application question with Unicode α and spacing. ".repeat(12)}<textarea id="long-label"></textarea></label><label>${"Another lengthy eligibility question. ".repeat(12)}<select id="long-select"><option value="">Choose</option><option value="yes">Yes</option></select></label><iframe src="/frame"></iframe>`);});
await new Promise(r=>site.listen(0,'127.0.0.1',r));
const probe=net.createServer();await new Promise(r=>probe.listen(0,'127.0.0.1',r));const port=probe.address().port;await new Promise(r=>probe.close(r));
const token='disposable-numeric-journal-test-token';fs.writeFileSync(root+'/token',token,{mode:0o600});
const url='http://127.0.0.1:'+site.address().port;
fs.mkdirSync(root+'/state',{mode:0o700});
fs.writeFileSync(root+'/state/instance.json',JSON.stringify({protocol_version:1,instance_id:'numeric-test',application_id:'fixture'}),{mode:0o600});
const legacy=JSON.stringify(Array.from({length:1024},(_,i)=>({workspace_id:'history',request_id:'historical-'+i,client_id:'agent',service_epoch:'legacy',browser_id:'old',target_id:'historical',document_id:'doc',action:'fill',state:'verified',future_evidence:{retained:i}})));
fs.writeFileSync(root+'/state/action-receipts.json',legacy,{mode:0o600});
const binary=process.env.KAGEHA_BROWSER_BIN;assert(binary);
function startDaemon(){daemon=start(binary,['--state-dir',root+'/state','--instance-id','numeric-test','--application-id','fixture','--chromium',process.env.CHROMIUM_BIN||'/Applications/Google Chrome.app/Contents/MacOS/Google Chrome','--standalone','on','--http-listen','127.0.0.1:'+port,'--http-token-file',root+'/token']);}
const newClient=chat=>new BrowserClient({baseUrl:'http://127.0.0.1:'+port,token,workspaceId:chat,clientId:'agent'});
let client=newClient('numbers'),scope,authority;
async function observe(){return checked(await client.call('observe_tab',{...scope,compact:false}));}
async function fill(text,id='budget') {
  let obs=await observe(),field=obs.controls.find(c=>c.id===id);
  if(!field){const selected=checked(await client.call('browser_selection'));const inspected=checked(await client.call('query_tab',{selection:{...scope,binding_id:selected.binding_id},selector:'#'+id}));obs=inspected.observation;field=inspected.matches.items[0];}
  assert(field?.ref,'Missing field '+id);
  return client.call('fill_observed',{...scope,observation_id:obs.observation_id,target:field.ref,text,...(field.frame_id?{frame_id:field.frame_id}:{})},{authority});
}
try {
  const chrome=start(process.env.CHROMIUM_BIN||'/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',['--headless','--no-first-run','--no-default-browser-check','--remote-debugging-port=0','--user-data-dir='+root+'/chrome',url]);
  const cport=await until(()=>fs.existsSync(root+'/chrome/DevToolsActivePort')&&Number(fs.readFileSync(root+'/chrome/DevToolsActivePort','utf8').split('\n')[0]));
  const endpoint='http://127.0.0.1:'+cport;
  const target=await until(async()=> (await(await fetch(endpoint+'/json/list')).json()).find(t=>t.title==='Numeric fixture'));
  startDaemon();await until(()=>client.connect());
  async function attach(){let s=checked(await client.call('browser_selection'));checked(await client.call('switch_profile',{profile:'chrome',binding_id:s.binding_id,consent:true,endpoint}));s=checked(await client.call('browser_selection'));checked(await client.call('claim_user_tab',{binding_id:s.binding_id,browser_id:s.browser_id,target_id:target.id,url:target.url,title:target.title}));scope={browser_id:s.browser_id,target_id:target.id};authority=checked(await client.call('claim_control',{ttl_ms:60000})).authority;}
  await attach();
  assert.equal(checked(await client.call('browser_selection')).input_status.receipt_count,1024);
  if(process.env.KAGEHA_EXPECT_CONSERVATIVE==='1') {
    assert.equal((await fill('3.00')).error?.code,'unsupported_input_type');
    console.log(JSON.stringify({passed:true,conservativeNumericFence:true,legacyReceipts:1024}));
  } else {
  for(const text of ['3.00','-1','1.005','101','1e1','.5','']){
    const r=checked(await fill(text));assert.equal(r.receipt.state,'verified');assert.equal(r.receipt.input_evidence.comparison,'numeric_decimal_v1');
    if(text==='-1')assert(r.field_validation.range_underflow);
    if(text==='101')assert(r.field_validation.range_overflow);
    if(text==='1.005')assert(r.field_validation.step_mismatch);
    if(text==='')assert(r.field_validation.value_missing);
  }
  for(const text of ['NaN','Infinity','1e309','3,00','+3','1e']){const r=await fill(text);assert.equal(r.error?.code,'numeric_input_invalid');assert.equal(r.execution_phase,'not_dispatched');}
  for(const [id,code] of [['ro','input_readonly'],['disabled','input_disabled'],['date','unsupported_input_type']]){const r=await fill('3',id);assert.equal(r.error?.code,code);}
  checked(await fill('plain text','plain'));
  checked(await fill('long labels retain exact identity','long-label'));
  const longSelection=checked(await client.call('browser_selection'));
  const longObserved=checked(await client.call('query_tab',{selection:{...scope,binding_id:longSelection.binding_id},selector:'#long-select'}));
  assert.equal(longObserved.observation.ax[0].identity_digest.length,64);
  checked(await client.call('batch_observed',{...scope,observation_id:longObserved.observation.observation_id,actions:[{action:'select',target:longObserved.matches.items[0].ref,option:'yes'}]},{authority}));
  checked(await fill('8.00','framed'));
  const numeric=checked(await fill('3.00'));const evidence=numeric.receipt.input_evidence;
  await observe();const probeSelection=checked(await client.call('browser_selection'));
  const probed=checked(await client.call('query_tab',{selection:{...scope,binding_id:probeSelection.binding_id},selector:'#budget',value_probe:{salt:evidence.salt,value_digest:evidence.value_digest,comparison:evidence.comparison,document_id:numeric.receipt.document_id}}));assert(probed.value_probe.value_matches);
  // Simulate a lost final acknowledgement in the isolated fixture only.
  const reconcileDb=new DatabaseSync(root+'/state/action-journal.sqlite');const pending={...numeric.receipt,state:'returned'};
  reconcileDb.prepare('UPDATE receipts SET state=?,payload=? WHERE workspace_id=? AND request_id=?').run('returned',JSON.stringify(pending),'numbers',pending.request_id);reconcileDb.close();
  const beforeReconcile=await observe();const reconciled=checked(await client.call('reconcile_input',{...scope,observation_id:beforeReconcile.observation_id,request_id:pending.request_id},{authority}));assert.equal(reconciled.postcondition_matches,true);

  // Batch numeric fills use the same checks and evidence.
  let obs=await observe();const batch=checked(await client.call('batch_observed',{...scope,observation_id:obs.observation_id,actions:[{action:'fill',target:'#budget',text:'4.00'}]},{authority}));assert(batch);
  // More than the former global ceiling, using genuine browser dispatches.
  let first;
  const n=Number(process.env.KAGEHA_ENDURANCE_ACTIONS||1200);
  for(let i=0;i<n;i++){
    if(i%100===0)authority=checked(await client.call('claim_control',{ttl_ms:60000})).authority;
    const r=checked(await fill(String((i%90)+1)));first??=r.receipt.request_id;
    if(i%200===199)console.log(JSON.stringify({progress:i+1}));
  }
  const oldEpoch=client.info.service_epoch;
  daemon.kill('SIGTERM');await until(()=>daemon.exitCode!==null||daemon.signalCode!==null);await sleep(100);
  startDaemon();client=newClient('numbers');await until(()=>client.connect());assert.notEqual(client.info.service_epoch,oldEpoch);
  assert.equal(checked(await client.call('action_receipt',{request_id:first})).receipt.state,'verified');
  await attach();checked(await fill('9.50'));
  const selected=checked(await client.call('browser_selection'));assert.equal(selected.tab_count,1);assert(selected.input_status.ready);assert(selected.input_status.receipt_count>n);
  const db=new DatabaseSync(root+'/state/action-journal.sqlite',{readOnly:true});assert.equal(db.prepare('PRAGMA integrity_check').get().integrity_check,'ok');assert(db.prepare('SELECT count(*) n FROM receipts').get().n>n);db.close();
  assert.equal(fs.readFileSync(root+'/state/action-receipts.json','utf8'),legacy);
  assert.equal(chrome.exitCode,null);
  console.log(JSON.stringify({passed:true,realNumericActions:n+8,restarted:true,allReceiptsRetained:true,constraintValidity:true,malformedRejected:true,tabCount:1}));
  }
} finally {
  for(const p of children.toReversed())p.kill('SIGTERM');await sleep(300);for(const p of children)if(p.exitCode===null&&p.signalCode===null)p.kill('SIGKILL');
  await new Promise(r=>site.close(r));
}
