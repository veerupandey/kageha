// Two independent chats share one external profile, never the same managed tab.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import net from 'node:net';
import http from 'node:http';
import {spawn} from 'node:child_process';
import {BrowserClient} from '../sdk/client.mjs';
const root=fs.mkdtempSync('/tmp/kageha-shared-profile-');fs.chmodSync(root,0o700);
const children=[];const sleep=ms=>new Promise(r=>setTimeout(r,ms));
async function until(fn){for(let n=0;n<250;n++){try{const v=await fn();if(v)return v;}catch{}await sleep(40);}throw Error('Fixture did not become ready');}
function start(binary,args){const fd=fs.openSync(root+'/'+children.length+'.log','a',0o600);const p=spawn(binary,args,{stdio:['ignore',fd,fd]});fs.closeSync(fd);children.push(p);return p;}
function checked(reply){assert.equal(reply.status,'ok',JSON.stringify(reply.error));return reply.result;}
const page=http.createServer((req,res)=>{res.setHeader('Content-Type','text/html');res.end('<title>Shared profile fixture</title><h1>Unchanged user tab</h1>');});await new Promise(r=>page.listen(0,'127.0.0.1',r));
try{
 const url='http://127.0.0.1:'+page.address().port+'/';fs.mkdirSync(root+'/chrome');
 const external=start(process.env.CHROMIUM_BIN||'/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',['--headless','--no-first-run','--no-default-browser-check','--remote-debugging-port=0','--user-data-dir='+root+'/chrome',url]);
 const port=await until(()=>fs.existsSync(root+'/chrome/DevToolsActivePort')&&Number(fs.readFileSync(root+'/chrome/DevToolsActivePort','utf8').split('\n')[0]));
 const endpoint='http://127.0.0.1:'+port;
 const target=await until(async()=>{const tabs=await(await fetch(endpoint+'/json/list')).json();return tabs.find(t=>t.url===url&&t.title==='Shared profile fixture');});
 const probe=net.createServer();await new Promise(r=>probe.listen(0,'127.0.0.1',r));const apiPort=probe.address().port;await new Promise(r=>probe.close(r));const token='isolated-shared-profile-fixture-token';fs.writeFileSync(root+'/token',token,{mode:0o600});
 start(process.env.KAGEHA_BROWSER_BIN,['--state-dir',root+'/state','--instance-id','shared-test','--application-id','shared-test','--chromium',process.env.CHROMIUM_BIN||'/Applications/Google Chrome.app/Contents/MacOS/Google Chrome','--standalone','on','--http-listen','127.0.0.1:'+apiPort,'--http-token-file',root+'/token']);
 const clients=['chat-a','chat-b'].map(workspaceId=>new BrowserClient({baseUrl:'http://127.0.0.1:'+apiPort,token,workspaceId,clientId:'agent-'+workspaceId}));
 for(const client of clients){await until(()=>client.connect());const selection=checked(await client.call('browser_selection'));const unapproved=await client.call('switch_profile',{profile:'comet',binding_id:selection.binding_id,consent:false,endpoint});assert.equal(unapproved.error?.code,'browser_profile_consent_required');checked(await client.call('switch_profile',{profile:'comet',binding_id:selection.binding_id,consent:true,endpoint}));}
 const a=checked(await clients[0].call('browser_selection')),b=checked(await clients[1].call('browser_selection'));assert.notEqual(a.browser_id,b.browser_id);assert.equal(a.tabs.length,0);assert.equal(b.tabs.length,0);
 checked(await clients[0].call('claim_user_tab',{binding_id:a.binding_id,browser_id:a.browser_id,target_id:target.id,url:target.url,title:target.title}));
 const candidates=checked(await clients[1].call('list_user_tabs'));assert(!candidates.tabs.some(t=>t.tabHandle===target.id));
 const denied=await clients[1].call('claim_user_tab',{binding_id:b.binding_id,browser_id:b.browser_id,target_id:target.id,url:target.url,title:target.title});assert.equal(denied.error?.code,'browser_tab_in_use');
 const ownB=checked(await clients[1].call('open_tab',{browser_id:b.browser_id,url}));
 const currentB=checked(await clients[1].call('browser_selection'));const grant=checked(await clients[1].call('claim_control'));
 const script=checked(await clients[1].call('run_script',{version:3,browser_id:b.browser_id,binding_id:currentB.binding_id,target_id:ownB.target_id,code:`return browser.attachTab(${JSON.stringify(target.id)},${JSON.stringify(target.url)},${JSON.stringify(target.title)});`},{authority:grant.authority}));
 assert.equal(script.error_code,'browser_tab_in_use','Scripts must not bypass another chat\'s ownership');
 const bAfter=checked(await clients[1].call('browser_selection'));const alias=await clients[1].call('switch_profile',{profile:'chrome',binding_id:bAfter.binding_id,consent:true,endpoint});assert.equal(alias.error?.code,'browser_profile_endpoint_conflict','A different browser name must not alias an already attached endpoint');
 const own=checked(await clients[0].call('observe_tab',{browser_id:a.browser_id,target_id:target.id,compact:false}));assert(JSON.stringify(own.text).includes('Unchanged user tab'));
 checked(await clients[1].call('close_browser',{browser_id:b.browser_id}));assert.equal(external.exitCode,null,'Detaching one chat must not close the external browser');assert.equal(checked(await clients[0].call('browser_selection')).target_id,target.id);
 // The close API cannot cross chat ownership, and can close a tab explicitly shared by the user.
 const ownedA=checked(await clients[0].call('browser_selection'));
 const grantA=checked(await clients[0].call('claim_control'));
 const crossClose=await clients[0].call('close_tab',{binding_id:ownedA.binding_id,browser_id:a.browser_id,target_id:ownB.target_id},{authority:grantA.authority});
 assert.equal(crossClose.error?.code,'target_not_owned');
 const unshared=await(await fetch(endpoint+'/json/new?'+encodeURIComponent(url+'?unshared=1'),{method:'PUT'})).json();
 const unsharedClose=await clients[0].call('close_tab',{binding_id:ownedA.binding_id,browser_id:a.browser_id,target_id:unshared.id},{authority:grantA.authority});
 assert.equal(unsharedClose.error?.code,'target_not_owned','An actual unshared personal tab cannot be closed');
 const closedA=checked(await clients[0].call('close_tab',{binding_id:ownedA.binding_id,browser_id:a.browser_id,target_id:target.id},{authority:grantA.authority}));
 assert.equal(closedA.closed,true);assert.equal(external.exitCode,null,'Closing a shared tab must not close the external browser');
 assert((await(await fetch(endpoint+'/json/list')).json()).some(t=>t.id===ownB.target_id),'The other chat tab remains open');
 assert((await(await fetch(endpoint+'/json/list')).json()).some(t=>t.id===unshared.id),'The unshared user tab remains open');
 console.log(JSON.stringify({passed:true,shared_profile:true,independent_chats:2,other_chat_claim_denied:true,script_claim_denied:true,external_browser_preserved:true,page_inputs:0}));
}finally{for(const child of children.toReversed())child.kill('SIGTERM');await sleep(200);for(const child of children)if(child.exitCode===null&&child.signalCode===null)child.kill('SIGKILL');await new Promise(r=>page.close(r));}
