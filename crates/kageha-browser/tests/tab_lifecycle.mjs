// Real Chromium: navigation does not allocate tabs; explicit closing frees capacity.
import assert from 'node:assert/strict';
import {DatabaseSync} from 'node:sqlite';
import fs from 'node:fs';
import http from 'node:http';
import net from 'node:net';
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import {BrowserClient} from '../sdk/client.mjs';
const root = fs.mkdtempSync('/tmp/kageha-tab-lifecycle-');
const token = 'disposable-tab-lifecycle-authentication-token';
fs.writeFileSync(root+'/token',token,{mode:0o600});
const site=http.createServer((req,res)=>{res.setHeader('Content-Type','text/html');res.end('<title>Lifecycle '+req.url+'</title><h1>Lifecycle</h1>');});
await new Promise(resolve=>site.listen(0,'127.0.0.1',resolve));
const probe=net.createServer().listen(0,'127.0.0.1');await once(probe,'listening');const port=probe.address().port;await new Promise(resolve=>probe.close(resolve));
const child=spawn(process.env.KAGEHA_BROWSER_BIN,['--state-dir',root+'/state','--instance-id','lifecycle','--application-id','fixture','--standalone','on','--chromium',process.env.CHROMIUM_BIN||'/Applications/Google Chrome.app/Contents/MacOS/Google Chrome','--http-listen','127.0.0.1:'+port,'--http-token-file',root+'/token'],{stdio:['ignore','ignore','pipe']});
let errors='';child.stderr.on('data',data=>errors+=data);
const sleep=ms=>new Promise(resolve=>setTimeout(resolve,ms));
const checked=reply=>{assert.equal(reply.status,'ok',JSON.stringify(reply));return reply.result;};
const client=new BrowserClient({baseUrl:'http://127.0.0.1:'+port,token,workspaceId:'chat-a',clientId:'agent'});
const url='http://127.0.0.1:'+site.address().port;
try {
  for(let i=0;;i++){try{await client.connect();break;}catch(e){assert(i<200&&child.exitCode===null,errors);await sleep(50);}}
  assert(client.info.methods.includes('close_tab'));
  const {browser_id}=checked(await client.call('launch_browser'));
  const first=checked(await client.call('open_tab',{browser_id,url:url+'/first'}));
  const {authority}=checked(await client.call('claim_control',{ttl_ms:60000}));
  // Repeated independent destinations stay in the same target.
  for(let i=0;i<40;i++) {
    const observation=checked(await client.call('observe_tab',{browser_id,target_id:first.target_id,compact:false}));
    checked(await client.call('navigate_tab',{browser_id,target_id:first.target_id,observation_id:observation.observation_id,url:url+'/search?q='+i},{authority}));
  }
  let selection=checked(await client.call('browser_selection'));
  assert.equal(selection.tab_count,1);assert.equal(selection.tab_limit,32);assert.equal(selection.target_id,first.target_id);
  const ids=[first.target_id];
  for(let i=1;i<32;i++)ids.push(checked(await client.call('open_tab',{browser_id,url:url+'/'+i})).target_id);
  const full=await client.call('open_tab',{browser_id,url:url+'/overflow'});
  assert.equal(full.error.code,'tab_capacity_exceeded');assert.match(full.error.message,/Do not repeat/);
  selection=checked(await client.call('browser_selection'));assert.equal(selection.tab_count,32);
  const closeParams=id=>({browser_id,binding_id:selection.binding_id,target_id:id});
  const unauthorized=await client.call('close_tab',closeParams(ids[0]));assert.notEqual(unauthorized.status,'ok');
  const stale=await client.call('close_tab',{...closeParams(ids[0]),binding_id:'stale'},{authority});assert.equal(stale.error.code,'browser_binding_changed');
  const unknown=await client.call('close_tab',closeParams('unshared-personal-tab'),{authority});assert.equal(unknown.error.code,'target_not_owned');
  const db=new DatabaseSync(root+'/state/action-journal.sqlite');
  const pendingReceipt={request_id:'pending-input',workspace_id:'chat-a',client_id:'agent',service_epoch:client.info.service_epoch,browser_id,target_id:ids[0],document_id:'doc',action:'fill',state:'dispatching'};
  db.prepare('INSERT INTO receipts VALUES(?,?,?,?,?,?,?)').run('chat-a','pending-input',browser_id,ids[0],client.info.service_epoch,'dispatching',JSON.stringify(pendingReceipt));
  const pending=await client.call('close_tab',closeParams(ids[0]),{authority});
  assert.equal(pending.error.code,'action_receipt_pending');
  assert.equal(checked(await client.call('action_receipt',{request_id:'pending-input'})).receipt.state,'dispatching');
  db.prepare('DELETE FROM receipts WHERE request_id=?').run('pending-input');db.close();
  const closed=checked(await client.call('close_tab',closeParams(ids[0]),{authority}));assert.equal(closed.closed,true);assert.equal(closed.tab_count,31);assert.equal(closed.selected_target_id,ids.at(-1));
  const extra=checked(await client.call('open_tab',{browser_id,url:url+'/capacity-recovered'}));
  selection=checked(await client.call('browser_selection'));
  checked(await client.call('close_tab',closeParams(extra.target_id),{authority}));
  selection=checked(await client.call('browser_selection'));assert.equal(selection.target_id,[...ids.slice(1)].sort()[0]);
  // A revoked controller cannot close any further targets.
  checked(await client.call('release_control',{}, {authority}));
  const denied=await client.call('close_tab',closeParams(ids[1]),{authority});assert.notEqual(denied.status,'ok');
  const next=checked(await client.call('claim_control',{ttl_ms:60000}));
  for(const id of ids.slice(1)) {
    selection=checked(await client.call('browser_selection'));
    checked(await client.call('close_tab',closeParams(id),{authority:next.authority}));
  }
  selection=checked(await client.call('browser_selection'));assert.equal(selection.tab_count,0);assert.equal(selection.target_id,null);assert.equal(selection.browser_id,browser_id);
  console.log(JSON.stringify({passed:true,navigations:40,maxTabs:32,capacityRecovered:true,unauthorizedCloseDenied:true,lastTabClosed:true}));
} finally {
  child.kill('SIGTERM');await sleep(300);if(child.exitCode===null&&child.signalCode===null)child.kill('SIGKILL');
  await new Promise(resolve=>site.close(resolve));fs.rmSync(root,{recursive:true,force:true});
}
