// A real upload widget consumes FileList immediately; verify without re-upload.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import http from 'node:http';
import {once} from 'node:events';
import {createHash} from 'node:crypto';
import {runtime,checked} from './support.mjs';
let uploads=0;
const server=http.createServer((req,res)=>{if(req.url==='/attached'){uploads++;res.end('ok');return;}res.setHeader('content-type','text/html');res.end(`<section><label>Resume<input type="file" style="display:none"></label></section><section><label>Cover Letter<input type="file" style="display:none"></label></section><span id="attachment"></span><button id="next">Next</button><script>const file=document.querySelector('input[type=file]');file.addEventListener('change',()=>{attachment.textContent=file.files[0].name;file.value='';fetch('/attached');});</script>`);});
server.listen(0,'127.0.0.1');await once(server,'listening');
const origin=`http://127.0.0.1:${server.address().port}`;
try {for(const transport of ['http','mcp']){
 const engine=await runtime(transport);
 try{
 const browser_id=checked(await engine.call('launch_browser')).browser_id;
 const target_id=checked(await engine.call('open_tab',{browser_id,url:origin})).target_id;
 let selection=checked(await engine.call('browser_selection'));
 const authority=checked(await engine.call('claim_control',{ttl_ms:60000})).authority;
 const owner=Buffer.from('benchmark').toString('base64url');const dir=path.join(engine.stateDir,'inputs',owner);fs.mkdirSync(dir,{recursive:true,mode:0o700});
 const bytes=Buffer.from('exact approved bytes');fs.writeFileSync(path.join(dir,'resume.pdf'),bytes,{mode:0o600});
 // Opening a tab may still be publishing its final binding. Rebind only a
 // read rejected before dispatch; never retry the upload itself.
 const inspect=async selector=>{
   for(let attempt=0;attempt<3;attempt++){
     selection=checked(await engine.call('browser_selection'));
     const reply=await engine.call('query_tab',{selection:{browser_id,target_id,binding_id:selection.binding_id},selector,limit:2});
     if(reply.status==='ok') return reply.result;
     if(reply.error?.code!=='browser_binding_changed' || reply.execution_phase!=='not_dispatched') return checked(reply);
   }
   throw Error('Read binding did not settle');
 };
 const inputs=await inspect('input[type=file]');
 assert.equal(inputs.matches.items.length,2);
 const [resume,cover]=inputs.matches.items;
 assert.equal(resume.visible,false);
 assert(resume.unique_selector && cover.unique_selector);
 assert.notEqual(resume.unique_selector,cover.unique_selector);
 for(const item of [resume,cover]){
   const match=await inspect(item.unique_selector);
   assert.equal(match.matches.count,1);
 }
 const report=checked(await engine.call('run_script',{version:3,browser_id,binding_id:selection.binding_id,target_id,code:`return browser.upload(${JSON.stringify(resume.unique_selector)}, ["inputs/resume.pdf"]);`,upload:{origin,files:[{path:'inputs/resume.pdf',sha256:createHash('sha256').update(bytes).digest('hex')}]}},{authority,requestId:'upload-once'}));
 assert.equal(report.error_code,'script_upload_unconfirmed');
 assert.equal(report.input_dispatched,true);
 assert.deepEqual(report.upload_receipt_ids,['upload-once:upload:0']);
 const scope={browser_id,target_id};const snapshot=()=>engine.call('observe_tab',scope).then(checked);
 let observed=await snapshot();
 const reconcile=(names)=>engine.call('reconcile_upload',{...scope,observation_id:observed.observation_id,request_id:'upload-once:upload:0',...(names?{attachment_names:names}:{})},{authority});
 let wrong=await reconcile(['other.pdf']);assert.equal(wrong.error.code,'upload_evidence_mismatch');
 observed=await snapshot();const verified=checked(await reconcile());assert.equal(verified.receipt.state,'verified');assert.equal(verified.input_sent,false);assert.equal(verified.server_bytes_verified,false);
 // Simulate old daemon metadata only inside the disposable fixture journal.
 const file=path.join(engine.stateDir,'action-receipts.json');let rows=JSON.parse(fs.readFileSync(file));rows[0].state='returned';delete rows[0].upload_evidence;fs.writeFileSync(file,JSON.stringify(rows),{mode:0o600});
 observed=await snapshot();wrong=await reconcile();assert.equal(wrong.error.code,'upload_evidence_missing');
 observed=await snapshot();wrong=await reconcile(['absent.pdf']);assert.equal(wrong.error.code,'upload_attachment_unobserved');
 observed=await snapshot();const legacy=checked(await reconcile(['resume.pdf']));assert.equal(legacy.receipt.state,'reconciled');assert.equal(legacy.legacy_metadata,true);assert.equal(legacy.input_sent,false);
 rows=JSON.parse(fs.readFileSync(file));rows[0].state='dispatching';fs.writeFileSync(file,JSON.stringify(rows),{mode:0o600});observed=await snapshot();wrong=await reconcile(['resume.pdf']);assert.equal(wrong.error.code,'upload_dispatch_not_acknowledged');
 rows[0].state='returned';const originalDocument=rows[0].document_id;rows[0].document_id='changed-document';fs.writeFileSync(file,JSON.stringify(rows),{mode:0o600});observed=await snapshot();wrong=await reconcile(['resume.pdf']);assert.equal(wrong.error.code,'receipt_document_changed');rows[0].document_id=originalDocument;fs.writeFileSync(file,JSON.stringify(rows),{mode:0o600});observed=await snapshot();checked(await reconcile(['resume.pdf']));
 assert.equal(checked(await engine.call('pending_actions',scope)).receipts.length,0);
 console.log(JSON.stringify({transport,passed:true,upload_dispatches:1,legacy_hold_retired:true,server_hash_claimed:false}));
 }finally{await engine.close();}
}assert.equal(uploads,2,'Exactly one upload per transport, never a recovery replay');}finally{await new Promise(r=>server.close(r));}
