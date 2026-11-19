// Disposable HTTPS fixture: modal recovery and target-bound managed credentials.
// Values remain inside the browser; assertions use only booleans and status text.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import https from 'node:https';
import { once } from 'node:events';
import { execFileSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { runtime, checked, until } from './support.mjs';

const require = createRequire(new URL('../../../frontend/package.json', import.meta.url));
const esbuild = require('esbuild');
const root = fs.mkdtempSync(path.join(os.tmpdir(), 'browser-focus-test-'));
const entry = path.join(root, 'react-fixture.jsx');
const modules = path.resolve('frontend/node_modules');
fs.writeFileSync(entry, `import React, {useState} from 'react'; import {createRoot} from 'react-dom/client';
function App(){const [first,setFirst]=useState(''), [second,setSecond]=useState('');
return <><label>First password<input id="first" type="password" value={first} onChange={e=>{setFirst(e.target.value); document.getElementById('decoy').focus();}} /></label>
<label>Confirm password<input id="second" type="password" value={second} onChange={e=>setSecond(e.target.value)} /></label>
<label>Decoy password<input id="decoy" type="password" /></label><p id="state">{first.length>=28 && first===second ? 'matching controlled state' : 'incomplete'}</p></>};
createRoot(document.getElementById('app')).render(<App/>);`);
const js = esbuild.buildSync({entryPoints:[entry], bundle:true, write:false, format:'iife', nodePaths:[modules], jsx:'automatic'}).outputFiles[0].text;
const chrome = process.env.CHROMIUM_BIN ?? '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
execFileSync('/usr/bin/openssl',['req','-x509','-newkey','rsa:2048','-nodes','-keyout',path.join(root,'key'),'-out',path.join(root,'cert'),'-subj','/CN=localhost','-days','1'],{stdio:'ignore'});
const spki=execFileSync('/bin/sh',['-c','openssl x509 -in "$1" -pubkey -noout | openssl pkey -pubin -outform der | openssl dgst -sha256 -binary | openssl base64','fixture',path.join(root,'cert')],{encoding:'utf8'}).trim();
const wrapper=path.join(root,'chromium');fs.writeFileSync(wrapper,`#!/bin/sh\nexec '${chrome.replaceAll("'","'\\''")}' --ignore-certificate-errors-spki-list=${spki} "$@"\n`,{mode:0o700});
const old = {chrome:process.env.CHROMIUM_BIN,vault:process.env.KAGEHA_BROWSER_CREDENTIAL_DIR,key:process.env.KAGEHA_BROWSER_CREDENTIAL_KEY_FILE};
process.env.CHROMIUM_BIN=wrapper;process.env.KAGEHA_BROWSER_CREDENTIAL_DIR=path.join(root,'vault');process.env.KAGEHA_BROWSER_CREDENTIAL_KEY_FILE=path.join(root,'master.key');fs.writeFileSync(process.env.KAGEHA_BROWSER_CREDENTIAL_KEY_FILE,crypto.getRandomValues(new Uint8Array(32)),{mode:0o600});
const auditValues=[];
const server=https.createServer({key:fs.readFileSync(path.join(root,'key')),cert:fs.readFileSync(path.join(root,'cert'))},(req,res)=>{
 if(req.url==='/audit' && req.method==='POST'){let body='';req.on('data',chunk=>body+=chunk);req.on('end',()=>{auditValues.push(JSON.parse(body));res.end('ok')});return;}
 if(req.url==='/bundle.js'){res.setHeader('Content-Type','text/javascript');res.end(js);return;}
 res.setHeader('Content-Type','text/html');res.end(`<!doctype html><div id="app"></div><label>Name <input id="name"></label><div id="modal" role="dialog" aria-modal="true"><a id="policy" href="#">Cookie Policy</a><button id="reject">Reject all</button></div><div id="events"></div><script>window.cookieBlocked=true; let blocked=true; document.addEventListener('focusin',e=>{if(blocked&&!document.getElementById('modal').contains(e.target)){document.getElementById('events').textContent='trap';document.getElementById('policy').focus()}},true);document.getElementById('reject').onclick=()=>{blocked=false;window.cookieBlocked=false;document.getElementById('modal').style.display='none'};document.addEventListener('input',e=>{if(e.target.type==='password'){document.getElementById('events').textContent=(e.data===null&&!e.isTrusted?'safe synthetic event':'unexpected event data');fetch('/audit',{method:'POST',body:JSON.stringify({id:e.target.id,value:e.target.value})})}});document.getElementById('policy').focus()</script><script src="/bundle.js"></script>`);
});server.listen(0,'127.0.0.1');await once(server,'listening');
try{
 const engine=await runtime('http');try{
  const browser_id=checked(await engine.call('launch_browser')).browser_id;
  const {target_id}=checked(await engine.call('open_tab',{browser_id,url:`https://127.0.0.1:${server.address().port}/`}));
  const selection=checked(await engine.call('browser_selection'));
  const authority=checked(await engine.call('claim_control',{ttl_ms:60000})).authority;
  const run=async code=>checked(await engine.call('run_script',{version:3,code,browser_id,target_id,binding_id:selection.binding_id},{authority}));
  const secret=(await run(`return browser.credential('fixture@example.com')`)).result.credential;
  assert.equal((await run(`return browser.batch([{action:'click',target:'#policy'}])`)).status,'completed');
  let blocked=await run(`return browser.fillCredential('#first',${JSON.stringify(secret)})`);
  assert.equal(blocked.status,'stopped');assert.equal(blocked.failed_batch.error_code,'focus_blocked_by_modal');assert.equal(blocked.failed_batch.steps[0].state,'not_dispatched');
  const observation=checked(await engine.call('observe_tab',{browser_id,target_id,compact:false}));
  const textBlocked=await engine.call('fill_observed',{browser_id,target_id,observation_id:observation.observation_id,target:'#name',text:'non-secret'},{authority});
  assert.equal(textBlocked.status,'error');assert.equal(textBlocked.error.code,'focus_blocked_by_modal');
  assert.equal((await run(`return browser.batch([{action:'click',target:'#reject'}])`)).status,'completed');
  const first=await run(`return browser.fillCredential('#first',${JSON.stringify(secret)})`);
  assert.equal(first.status,'completed');assert.equal(first.result.steps[0].state,'verified');
  const second=await run(`return browser.fillCredential('#second',${JSON.stringify(secret)})`);
  assert.equal(second.status,'completed');assert.equal(second.result.steps[0].state,'verified');
  await until(()=>auditValues.length>=2);assert.deepEqual(auditValues.map(v=>v.id),['first','second']);assert(auditValues[0].value.length>=28);assert.equal(auditValues[0].value,auditValues[1].value);
  const final=checked(await engine.call('observe_tab',{browser_id,target_id,compact:false}));
  const serialized=JSON.stringify(final);
  assert(serialized.includes('matching controlled state'));assert(serialized.includes('safe synthetic event'));
  // The first input handler redirected focus to decoy. The native setter must not write there.
  const decoy=final.controls.find(c=>c.id==='decoy');assert(decoy);assert(!decoy.value_length);
  assert(!JSON.stringify([blocked,first,second,final]).includes(auditValues[0].value));
  console.log('modal recovery and React managed-credential focus isolation passed');
 }finally{await engine.close()}
}finally{await new Promise(r=>server.close(r)); for(const [name,value] of [['CHROMIUM_BIN',old.chrome],['KAGEHA_BROWSER_CREDENTIAL_DIR',old.vault],['KAGEHA_BROWSER_CREDENTIAL_KEY_FILE',old.key]]){if(value===undefined)delete process.env[name];else process.env[name]=value;}fs.rmSync(root,{recursive:true,force:true})}
