// Credential lifecycle via public HTTP and MCP; fixture grades booleans only.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import https from 'node:https';
import {once} from 'node:events';
import {execFileSync} from 'node:child_process';
import {runtime, checked, until} from './support.mjs';

const root=fs.mkdtempSync(path.join(os.tmpdir(),'browser-credential-test-'));
const previousChromium=process.env.CHROMIUM_BIN;
const previousVault=process.env.KAGEHA_BROWSER_CREDENTIAL_DIR;
const previousCredentialKey=process.env.KAGEHA_BROWSER_CREDENTIAL_KEY_FILE;
const credentialKey=path.join(root,'credential-master.key');fs.writeFileSync(credentialKey,crypto.getRandomValues(new Uint8Array(32)),{mode:0o600});process.env.KAGEHA_BROWSER_CREDENTIAL_KEY_FILE=credentialKey;
const wrapper=path.join(root,'chromium');
const chrome=process.env.CHROMIUM_BIN??'/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
// Only this disposable browser trusts the fixture's self-signed certificate.
execFileSync('/usr/bin/openssl',['req','-x509','-newkey','rsa:2048','-nodes','-keyout',path.join(root,'key'),'-out',path.join(root,'cert'),'-subj','/CN=localhost','-days','1'],{stdio:'ignore'});
const spki=execFileSync('/bin/sh',['-c','openssl x509 -in "$1" -pubkey -noout | openssl pkey -pubin -outform der | openssl dgst -sha256 -binary | openssl base64','fixture',path.join(root,'cert')],{encoding:'utf8'}).trim();
fs.writeFileSync(wrapper,`#!/bin/sh\nexec '${chrome.replaceAll("'","'\\''")}' --ignore-certificate-errors-spki-list=${spki} "$@"\n`,{mode:0o700});
process.env.CHROMIUM_BIN=wrapper;
process.env.KAGEHA_BROWSER_CREDENTIAL_DIR=path.join(root,'vault');
let committed=0;
const server=https.createServer({key:fs.readFileSync(path.join(root,'key')),cert:fs.readFileSync(path.join(root,'cert'))},(req,res)=>{
 if(req.url==='/commit'){committed++;res.end('ok');return;}
 res.setHeader('Content-Type','text/html');res.end(`<label>Email<input id="email"></label><label>New password<input id="first" type="password"></label><label>Confirm<input id="second" type="password"></label><label>Read only<input id="locked" type="password" readonly></label><button id="save" onclick="if(first.value.length>=28&&first.value===second.value){fetch('/commit');this.textContent='Saved'}">Save password</button><div id="noise"></div><script>setInterval(()=>noise.textContent=Math.random(),30)</script>`);
});
server.listen(0,'127.0.0.1');await once(server,'listening');
const url=`https://127.0.0.1:${server.address().port}/`;
try{
 for(const transport of ['http','mcp']){
  const engine=await runtime(transport);
  try{
   const browser_id=checked(await engine.call('launch_browser')).browser_id;
   const tab=checked(await engine.call('open_tab',{browser_id,url}));
   let selection=checked(await engine.call('browser_selection'));
   const target_id=tab.target_id;
   const authority=checked(await engine.call('claim_control',{ttl_ms:60000})).authority;
   const script=async code=>checked(await engine.call('run_script',{version:3,code,browser_id,target_id,binding_id:selection.binding_id},{authority}));
   const created=await script(`return browser.credential('fixture@example.com');`);
   assert.equal(created.status,'completed');assert(created.result.stored);const credential=created.result.credential;
   assert.match(credential,/^cred_[0-9a-f]{64}$/);
   assert.equal((await script(`return browser.credential('fixture@example.com');`)).result.credential,credential);
   let snapshot=checked(await engine.call('observe_tab',{browser_id,target_id,compact:false}));
   assert.equal(snapshot.managed_credentials.configured,true);
   assert.equal(snapshot.managed_credentials.plaintext_returned,false);
   assert.equal(snapshot.managed_credentials.fill_password,'browser.fillCredential(uniqueLocator, saved.credential)');
   const plain=await engine.call('fill_observed',{browser_id,target_id,observation_id:snapshot.observation_id,target:'#first',text:'do-not-send'},{authority});
   assert.equal(plain.status,'error');assert.equal(plain.execution_phase,'not_dispatched');
   for(const target of ['#email','#locked']){
    const denied=await script(`return browser.fillCredential(${JSON.stringify(target)},${JSON.stringify(credential)});`);
    assert.equal(denied.status,'stopped');assert.equal(denied.failed_batch.error_code,'credential_destination_invalid');
   }
   const first=snapshot.controls.find(c=>c.id==='first');assert(first);
   const identity=`backend=${first.backend_node_id}@${first.document_id}`;
   const stale=await script(`return browser.fillCredential(${JSON.stringify(identity+'stale')},${JSON.stringify(credential)});`);
   assert.equal(stale.status,'stopped');assert.equal(stale.failed_batch.error_code,'document_changed');
   const filled=await script(`browser.fillCredential(${JSON.stringify(identity)},${JSON.stringify(credential)}); return browser.fillCredential('#second',${JSON.stringify(credential)});`);
   assert.equal(filled.status,'completed');assert.equal(filled.result.status,'completed');
   assert.equal(filled.result.steps[0].state,'verified');
   const reportText=JSON.stringify(filled);assert(!reportText.includes('Aa1!'));
   const before=committed;
   const saved=await script(`return browser.batch([{action:'click',target:'#save'}]);`);assert.equal(saved.status,'completed');
   await until(()=>committed===before+1);
   snapshot=checked(await engine.call('observe_tab',{browser_id,target_id,compact:false}));
   assert(!JSON.stringify(snapshot).includes('Aa1!'));
   selection=checked(await engine.call('browser_selection'));
   const other=checked(await engine.call('open_tab',{browser_id,url:url.replace('127.0.0.1','localhost')}));
   const otherSelection=checked(await engine.call('browser_selection'));
   const denied=checked(await engine.call('run_script',{version:3,code:`return browser.fillCredential('#first',${JSON.stringify(credential)});`,browser_id,target_id:other.target_id,binding_id:otherSelection.binding_id},{authority}));
   assert.equal(denied.status,'stopped');assert.equal(denied.failed_batch.error_code,'credential_scope_mismatch');
   console.log(JSON.stringify({transport,passed:true,credential_values_exposed:0}));
  }finally{await engine.close();}
 }
 assert.equal(committed,2);
}finally{
 await new Promise(r=>server.close(r));
 if(previousChromium===undefined)delete process.env.CHROMIUM_BIN;else process.env.CHROMIUM_BIN=previousChromium;
 if(previousCredentialKey===undefined)delete process.env.KAGEHA_BROWSER_CREDENTIAL_KEY_FILE;else process.env.KAGEHA_BROWSER_CREDENTIAL_KEY_FILE=previousCredentialKey;
 if(previousVault===undefined)delete process.env.KAGEHA_BROWSER_CREDENTIAL_DIR;else process.env.KAGEHA_BROWSER_CREDENTIAL_DIR=previousVault;
 fs.rmSync(root,{recursive:true,force:true});
}
