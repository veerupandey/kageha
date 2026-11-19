// Independent agent-framework acceptance test. Public MCP only; no Kageha loop.
import fs from 'node:fs';
import os from 'node:os';
import crypto from 'node:crypto';
import path from 'node:path';
import assert from 'node:assert/strict';
import {createRequire} from 'node:module';
import {fileURLToPath} from 'node:url';
import {spawn,spawnSync} from 'node:child_process';
const require=createRequire(path.join(process.env.KAGEHA_AGENT_NODE_MODULES||path.join(path.dirname(fileURLToPath(import.meta.url)),'agent-integration'),'package.json'));
const {createAgent}=require('langchain');
const {ChatOpenAI}=require('@langchain/openai');
const {MCPAdapter}=require('@langchain/mcp-adapters');
const env={...process.env};if(env.KAGEHA_MODEL_BENCH_ENV_FILE){for(const line of fs.readFileSync(env.KAGEHA_MODEL_BENCH_ENV_FILE,'utf8').split('\n')){const m=line.match(/^\s*([A-Z_][A-Z_0-9]*)\s*=\s*(.*)$/);if(m&&!env[m[1]])env[m[1]]=m[2].replace(/^(["'])(.*)\1$/,'$2')}}
assert(env.CLOUDFLARE_API_TOKEN&&/^[a-f0-9]{32}$/i.test(env.CLOUDFLARE_ACCOUNT_ID||''),'Configure Cloudflare credentials');
const helper=env.KAGEHA_COMPUTER_BIN;assert(path.isAbsolute(helper||''),'Set absolute helper path');
const root=fs.mkdtempSync(path.join(os.tmpdir(),'kageha-langchain-mcp-'));fs.chmodSync(root,0o700);
const contents=path.join(root,'Fixture.app','Contents');fs.mkdirSync(path.join(contents,'MacOS'),{recursive:true});fs.writeFileSync(path.join(contents,'Info.plist'),`<plist version="1.0"><dict><key>CFBundleIdentifier</key><string>test.kageha.langchain.${Date.now()}</string><key>CFBundleExecutable</key><string>Fixture</string><key>CFBundlePackageType</key><string>APPL</string></dict></plist>`);
const app=path.join(contents,'MacOS','Fixture');assert.equal(spawnSync('xcrun',['swiftc','-O','crates/kageha-computer/bench/BenchmarkFixture.swift','-o',app]).status,0);assert.equal(spawnSync('codesign',['--force','--sign','-',path.join(root,'Fixture.app')],{stdio:'ignore'}).status,0);
const oracle=path.join(root,'oracle.json');const fixture=spawn(app,[oracle,'51001'],{stdio:['pipe','pipe','ignore']});
const sleep=ms=>new Promise(r=>setTimeout(r,ms));const end=Date.now()+15000;while((!fs.existsSync(oracle)||!JSON.parse(fs.readFileSync(oracle)).ready)&&Date.now()<end)await sleep(50);
const initial=JSON.parse(fs.readFileSync(oracle)),pid=initial.pid,wid=initial.primary_window;
const ack=new Promise(r=>fixture.stdout.once('data',r));fixture.stdin.write('reset\n');await ack;
const hash=p=>crypto.createHash('sha256').update(fs.readFileSync(p)).digest('hex');
const report={helper_sha256:hash(helper),harness_sha256:hash(new URL(import.meta.url)),versions:{langchain:require('langchain/package.json').version},framework:'LangChain createAgent',transport:'Public MCP stdio',model:'@cf/deepseek-ai/deepseek-v4.1-flash-1017',pid,wid,calls:[],turns:[]};
const adapter=new MCPAdapter({servers:{computer:{transport:'stdio',mode:'legacy',command:helper,args:['control','--journal',path.join(root,'actions.sqlite'),'--allow-input','--mcp','--workspace','external-agent','--client','langchain'],restart:{enabled:false}}},beforeToolCall:async request=>{
  report.calls.push({name:request.name,args:request.args});
  if(request.name.endsWith('computer_call')){const v=request.args;if(v.operation==='observe'){assert.equal(v.args.pid,pid);assert.equal(v.args.window_id,wid);assert.notEqual(v.args.include_screenshot,true)}assert(!['list_apps','list_windows'].includes(v.operation),'Do not inspect unrelated applications');}
}});
try{
 const tools=await adapter.listTools();report.tools=tools.map(t=>t.name);
 const model=new ChatOpenAI({model:report.model,apiKey:env.CLOUDFLARE_API_TOKEN,configuration:{baseURL:`https://api.cloudflare.com/client/v4/accounts/${env.CLOUDFLARE_ACCOUNT_ID}/ai/v1`},modelKwargs:{reasoning_effort:'low'},maxRetries:0,timeout:90000,parallelToolCalls:false});
 const agent=createAgent({model,tools,systemPrompt:`Use only these public MCP computer tools. Only process ${pid}, window ${wid} is authorized. Get computer_info to discover the epoch. computer_call takes service_epoch, a UNIQUE request_id, operation, args. First acquire args {ttl_ms:60000}; then observe args {lease_id,pid:${pid},window_id:${wid},include_screenshot:false}. Execute args {lease_id,observation_id,action:{method,args}}. Set a field using method set_value, args {element_token,value}. Click checkbox/radio/button using method click and {element_token}. Every execute consumes the observation; observe AGAIN before every action. focus_window args {}. Never resend unknown input; look up its receipt. Lease renewal uses renew with {lease_id,ttl_ms:60000}. Never use shell/filesystem/network tools or change other windows.`});
 let messages=[{role:'user',content:'Set Primary field to exactly doc-LangChain-東京, enable Enable setting, select Option B, click Save document exactly once, and verify the field/toggles. Do not touch the secondary window.'}];
 for(const [id,prompt] of [['save',null],['followup','Now uncheck Enable setting, select Option A, and replace Primary field with draft-UNSAVED. Do not save again. Preserve the saved document.']]){
   if(prompt)messages.push({role:'user',content:prompt});
   const result=await agent.invoke({messages},{recursionLimit:90,signal:AbortSignal.timeout(240000)});messages=result.messages;
   const state=JSON.parse(fs.readFileSync(oracle)),saved=JSON.parse(fs.readFileSync(path.join(root,'saved.json')));
   const passed=id==='save'?state.primary==='doc-LangChain-東京'&&state.checkbox===1&&state.radio_b===1&&state.saves===1:state.primary==='draft-UNSAVED'&&state.checkbox===0&&state.radio_a===1&&state.saves===1;
   assert.equal(saved.title,'doc-LangChain-東京');assert.equal(state.secondary,'initial-secondary');assert.equal(state.secondary_clicks,0);report.turns.push({id,passed,state,answer:result.messages.at(-1).content,model_metadata:result.messages.filter(m=>m._getType?.()==='ai').map(m=>m.response_metadata),tool_errors:result.messages.filter(m=>m.status==='error').length});assert(passed,`Independent agent failed ${id}`);
 }
 report.status='passed';
}catch(e){report.status='failed';report.error=String(e);process.exitCode=1}
finally{await adapter.close();fixture.stdin.end();await sleep(100);if(fixture.exitCode===null)fixture.kill();fs.writeFileSync(path.join(root,'report.json'),JSON.stringify(report,null,2));console.log(JSON.stringify({report:path.join(root,'report.json'),status:report.status,error:report.error,turns:report.turns.map(t=>({id:t.id,passed:t.passed}))}));}
