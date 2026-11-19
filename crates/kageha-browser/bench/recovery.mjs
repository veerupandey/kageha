// Lost-acknowledgement replay: restore the durable dispatch marker after real input.
// All journal edits are restricted to this disposable fixture's private state directory.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import http from 'node:http';
import {once} from 'node:events';
import {randomBytes,createHash} from 'node:crypto';
import {runtime,checked,until} from './support.mjs';
let trustedInputs=0;
const server=http.createServer((req,res)=>{if(req.url==='/sticky'){res.setHeader('content-type','text/html');res.end(`<style>body{height:300vh}button{position:absolute;top:calc(100vh - 100px)}footer{position:fixed;bottom:0;left:0;right:0;height:200px;background:#ccc}</style><h1 id=done>Ready</h1><button id=sticky onclick="done.textContent='Saved'">Save</button><footer>Sticky footer</footer>`);return;}if(req.url==='/input'){trustedInputs++;res.end('ok');return;}res.setHeader('content-type','text/html');res.end(`<label>City<input id="city"></label><label>Postal<input id="postal"></label><label>Email<input id="email" type="email" value="old@example.test"></label><button id="action">Action</button><label for="current">Current role</label><input id="current" type="checkbox" style="opacity:0;width:24px;height:24px"><script>console.warn("fixture diagnostic");window.inputs=0;city.addEventListener('input',e=>{inputs++;if(e.isTrusted)fetch('/input')});</script>`)});
server.listen(0,'127.0.0.1');await once(server,'listening');
try {for(const transport of ['http','mcp']) {
 const engine=await runtime(transport);
 try {
 const browser_id=checked(await engine.call('launch_browser')).browser_id;
 const target_id=checked(await engine.call('open_tab',{browser_id,url:`http://127.0.0.1:${server.address().port}`})).target_id;
 const authority=checked(await engine.call('claim_control',{ttl_ms:60000})).authority;
 const scope={browser_id,target_id};
 const snapshot=async()=>checked(await engine.call('observe_tab',{...scope,compact:false}));
 const fill=async(id,text,requestId)=>{const observation_id=(await snapshot()).observation_id;return engine.call('fill_observed',{...scope,observation_id,target:id,text},{authority,requestId});};
 const journal=path.join(engine.stateDir,'action-receipts.json');
 const lostAck=id=>{const rows=JSON.parse(fs.readFileSync(journal));const row=rows.find(r=>r.request_id===id);assert(row.input_evidence);assert.equal(row.state,'verified');row.state='dispatching';row.result_document_id=null;fs.writeFileSync(journal,JSON.stringify(rows),{mode:0o600});};
 const beforeFresh=trustedInputs;
 checked(await fill('#city','Vancouver','same-value'));
 await until(()=>trustedInputs===beforeFresh+1);
 checked(await fill('#city','Vancouver','original'));
 await until(()=>trustedInputs===beforeFresh+2);
 const beforeRecovery=trustedInputs;
 lostAck('original');
 let observed=await snapshot();
 let result=checked(await engine.call('reconcile_input',{...scope,observation_id:observed.observation_id,request_id:'original'},{authority}));
 assert.equal(result.postcondition_matches,true);assert.equal(result.input_sent,false);assert.equal(result.receipt.state,'verified');
 assert.equal(trustedInputs,beforeRecovery,'Receipt recovery must not send another input event');
 // A changed field must clear only the convergent hold, never invent success.
 checked(await fill('#city','Burnaby','different'));
 lostAck('different');
 const rows=JSON.parse(fs.readFileSync(journal));const original=rows.find(r=>r.request_id==='different');original.input_evidence.value_digest=createHash('sha256').update(JSON.stringify([original.input_evidence.salt,'not the current value'])).digest('hex');fs.writeFileSync(journal,JSON.stringify(rows),{mode:0o600});
 observed=await snapshot();result=checked(await engine.call('reconcile_input',{...scope,observation_id:observed.observation_id,request_id:'different'},{authority}));
 assert.equal(result.postcondition_matches,false);assert.equal(result.receipt.state,'reconciled');assert.equal(result.input_sent,false);
 // Lost acknowledgements recover automatically before a subsequent safe edit.
 checked(await fill('#city','Richmond','automatic'));lostAck('automatic');
 checked(await fill('#postal','V5A 1A1','next'));
 assert.equal(JSON.parse(fs.readFileSync(journal)).find(r=>r.request_id==='automatic').state,'verified');
 // Styled native checkbox: use its associated visible label, including receipt recovery.
 observed=await snapshot();const check=checked(await engine.call('check_observed',{...scope,observation_id:observed.observation_id,target:'#current',checked:true},{authority,requestId:'hidden-check'}));
 assert.equal(check.receipt.state,'verified');lostAck('hidden-check');observed=await snapshot();const repairedCheck=checked(await engine.call('reconcile_input',{...scope,observation_id:observed.observation_id,request_id:'hidden-check'},{authority}));assert.equal(repairedCheck.postcondition_matches,true);assert.equal(repairedCheck.input_sent,false);
 // Read-only migration probe reports only equality and refuses secure inputs.
 const salt=randomBytes(16).toString('hex');observed=await snapshot();
 const selected=checked(await engine.call('browser_selection'));const selection={browser_id:selected.browser_id,target_id:selected.target_id,binding_id:selected.binding_id};
 const doc=observed.controls.find(c=>c.id==='city').document_id;
 const diagnostics=checked(await engine.call('query_tab',{selection,selector:'body',console:true}));
 assert(diagnostics.messages.some(m=>m.text.includes('fixture diagnostic')));
 const validity=checked(await engine.call('query_tab',{selection,selector:'#city'}));assert.equal(validity.matches.items[0].validity.valid,true);
 const checkboxLabel=checked(await engine.call('query_tab',{selection,selector:'#current'}));assert.deepEqual(checkboxLabel.matches.items[0].associated_labels,['Current role']);const labelMapping=checked(await engine.call('query_tab',{selection,selector:'label[for=current]'}));assert.equal(labelMapping.matches.items[0].label_for,'current');
 const probe=checked(await engine.call('query_tab',{selection,selector:'#city',value_probe:{salt,value_digest:createHash('sha256').update(JSON.stringify([salt,'Richmond'])).digest('hex'),document_id:doc}}));
 assert.equal(probe.value_probe.value_matches,true);assert.equal(probe.value_probe.input_sent,false);
 assert(!JSON.stringify(probe).includes('Richmond'));
 // Email inputs lack text selection APIs: replacement, readback and recovery still use trusted input.
 const emailResult=checked(await fill('#email','new@example.test','email-replacement'));assert.equal(emailResult.receipt.state,'verified');lostAck('email-replacement');observed=await snapshot();const emailRepair=checked(await engine.call('reconcile_input',{...scope,observation_id:observed.observation_id,request_id:'email-replacement'},{authority}));assert.equal(emailRepair.postcondition_matches,true);assert.equal(emailRepair.input_sent,false);checked(await fill('#email','','email-clear'));checked(await fill('#email','again@example.test','email-again'));
 // A short batch yields before dispatching, with no uncertain input residue.
 observed=await snapshot();
 const short=checked(await engine.call('batch_observed',{...scope,observation_id:observed.observation_id,actions:[{action:'fill',target:'#city',text:'Should not be sent'}]},{authority,timeoutMs:1000,requestId:'short'}));
 assert.equal(short.status,'stopped');assert.equal(short.error_code,'batch_deadline_budget_exhausted');
 assert.equal(checked(await engine.call('pending_actions',scope)).receipts.length,0);
 const stickyTarget=checked(await engine.call('open_tab',{browser_id,url:`http://127.0.0.1:${server.address().port}/sticky`})).target_id;const stickyObserved=checked(await engine.call('observe_tab',{browser_id,target_id:stickyTarget}));const stickySaved=checked(await engine.call('click_observed',{browser_id,target_id:stickyTarget,observation_id:stickyObserved.observation_id,target:'#sticky',expected_selector:'#done',expected_text:'Saved',timeout_ms:2000},{authority}));assert.equal(stickySaved.receipt.state,'verified');
 console.log(JSON.stringify({transport,passed:true,checks:20,unverified_actions_replayed:0}));
 }finally{await engine.close();}
}}finally{await new Promise(r=>server.close(r));}
