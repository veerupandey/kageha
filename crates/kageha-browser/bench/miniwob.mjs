// Public MiniWoB++ subset. Episode setup and reward reading are evaluator-only CDP.
// Every task action goes through the public Rust API or native MCP transport.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import http from 'node:http';
import { once } from 'node:events';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { runtime, checked, until } from './support.mjs';
const args=Object.fromEntries(process.argv.slice(2).map(v=>v.replace(/^--/,'').split('=')));
const checkout=process.env.MINIWOB_ROOT;
if(!checkout)throw Error('Set MINIWOB_ROOT to a clean MiniWoB++ checkout');
const revision=execFileSync('git',['-C',checkout,'rev-parse','HEAD'],{encoding:'utf8'}).trim();
const PIN='33c3b4ddef8c6eb67c57a29663d844b1eda7e614';
assert.equal(revision,PIN,'MiniWoB revision differs from reviewed benchmark pin');
assert.equal(execFileSync('git',['-C',checkout,'status','--porcelain'],{encoding:'utf8'}).trim(),'','MiniWoB files must be unmodified');
const htmlRoot=path.join(checkout,'miniwob/html');
const seeds=Number(args.seeds??10);assert(Number.isInteger(seeds)&&seeds>0&&seeds<=100);
const seedStart=Number(args['seed-start']??1);assert(Number.isSafeInteger(seedStart)&&seedStart>=1&&seedStart<=1000000);
const transports=args.transport&&args.transport!=='both'?[args.transport]:['http','mcp'];assert(transports.every(t=>['http','mcp'].includes(t)));
const server=http.createServer((request,response)=>{
  const name=path.resolve(htmlRoot,'.'+decodeURIComponent(new URL(request.url,'http://fixture').pathname));
  if(!name.startsWith(htmlRoot+path.sep)||!fs.existsSync(name)||!fs.statSync(name).isFile()){response.writeHead(404).end();return;}
  response.setHeader('content-type',name.endsWith('.js')?'application/javascript':name.endsWith('.css')?'text/css':name.endsWith('.html')?'text/html':'application/octet-stream');
  response.end(fs.readFileSync(name));
});server.listen(0,'127.0.0.1');await once(server,'listening');
async function evaluator(engine,target) {
  const port=await until(()=>{
    for(const name of fs.readdirSync(engine.stateDir))if(name.startsWith('chromium-')){
      const file=path.join(engine.stateDir,name,'DevToolsActivePort');if(fs.existsSync(file))return Number(fs.readFileSync(file,'utf8').split('\n')[0]);
    }
  });
  const tabs=await(await fetch(`http://127.0.0.1:${port}/json/list`)).json();
  const tab=tabs.find(value=>value.id===target);assert(tab,'Evaluator target not found in disposable instance');
  const socket=new WebSocket(tab.webSocketDebuggerUrl);await once(socket,'open');
  let id=0;const pending=new Map();
  socket.addEventListener('message',event=>{const message=JSON.parse(event.data);const waiter=pending.get(message.id);if(waiter){clearTimeout(waiter.timer);pending.delete(message.id);message.error?waiter.reject(Error(JSON.stringify(message.error))):waiter.resolve(message.result);}});
  return {
    async evaluate(expression){const serial=++id;const reply=await new Promise((resolve,reject)=>{const timer=setTimeout(()=>reject(Error('Evaluator timed out')),5000);pending.set(serial,{resolve,reject,timer});socket.send(JSON.stringify({id:serial,method:'Runtime.evaluate',params:{expression,returnByValue:true}}));});if(reply.exceptionDetails)throw Error(JSON.stringify(reply.exceptionDetails));return reply.result.value;},
    close(){for(const waiter of pending.values())clearTimeout(waiter.timer);socket.close();},
  };
}
const tasks=['click-button','enter-text','click-checkboxes','click-checkboxes-large','enter-text-dynamic','click-dialog','click-button-sequence'];
const report={version:1,suite:'miniwob-public-subset',source_revision:revision,binary_sha256:createHash('sha256').update(fs.readFileSync(process.env.KAGEHA_BROWSER_BIN)).digest('hex'),started_at:new Date().toISOString(),seeds,seed_start:seedStart,tasks,driver:'Deterministic instruction parser, no language model; not an agent leaderboard score',results:[]};
try {
  for(const transport of transports){const engine=await runtime(transport);try{
    for(let seed=seedStart;seed<seedStart+seeds;seed++){
      const browser_id=checked(await engine.call('launch_browser')).browser_id;
      const authority=checked(await engine.call('claim_control',{ttl_ms:60000})).authority;
      for(const task of tasks){let evalClient,error,reward;const start=performance.now();const calls=engine.latencies.length;
        engine.setBudget(40);
        try {
          const tab=checked(await engine.call('open_tab',{browser_id,url:`http://127.0.0.1:${server.address().port}/miniwob/${task}.html`}));
          evalClient=await evaluator(engine,tab.target_id);
          await until(()=>evalClient.evaluate('typeof core!=="undefined" && !!core.cover_div'));
          // Standard benchmark reset: seed generator and start a fresh episode.
          // No task goals/actions are read or executed through this channel.
          await evalClient.evaluate(`Math.seedrandom(${JSON.stringify(String(seed))});core.startEpisodeReal();true`);
          const observe=async()=>checked(await engine.call('observe_tab',{browser_id,target_id:tab.target_id,compact:false}));
          let observation=await observe();
          const act=async(method,control,params={})=>{
            assert(control,`Missing control for ${task}`);
            checked(await engine.call(method,{browser_id,target_id:tab.target_id,observation_id:observation.observation_id,target:control.ref,...params},{authority}));
            observation=await observe();
          };
          const text=observation.text.join('');
          if(task==='click-button'){
            const goal=text.match(/Click on the "([^"]+)" button/);assert(goal,`Missing instruction: ${text}`);
            await act('click_observed',observation.controls.find(c=>c.role==='button'&&c.name===goal[1]),{timeout_ms:1000});
          }else if(task==='enter-text'||task==='enter-text-dynamic'){
            const goal=text.match(/Enter "([^"]+)" into/);assert(goal,`Missing instruction: ${text}`);
            await act('fill_observed',observation.controls.find(c=>c.role==='textbox'),{text:goal[1]});
            await act('click_observed',observation.controls.find(c=>c.name==='Submit'),{timeout_ms:1000});
          }else if(task==='click-checkboxes'||task==='click-checkboxes-large'){
            const goal=text.match(/Select (.*?) and click Submit/);assert(goal,`Missing instruction: ${text}`);
            for(const name of goal[1]==='nothing'?[]:goal[1].split(', '))await act('check_observed',observation.controls.find(c=>c.role==='checkbox'&&c.name===name),{checked:true});
            await act('click_observed',observation.controls.find(c=>c.name==='Submit'),{timeout_ms:1000});
          }else if(task==='click-dialog'){
            await act('click_observed',observation.controls.find(c=>c.role==='button'&&c.name.toLowerCase()==='close'),{timeout_ms:1000});
          }else{
            await act('click_observed',observation.controls.find(c=>c.name==='ONE'),{timeout_ms:1000});
            await act('click_observed',observation.controls.find(c=>c.name==='TWO'),{timeout_ms:1000});
          }
          reward=await evalClient.evaluate('({done:WOB_DONE_GLOBAL,raw:WOB_RAW_REWARD_GLOBAL,reward:WOB_REWARD_GLOBAL,reason:WOB_REWARD_REASON})');
          assert.equal(reward.done,true,JSON.stringify(reward));assert.equal(reward.raw,1,JSON.stringify(reward));
        }catch(failure){error=failure.message;}finally{evalClient?.close();}
        report.results.push({transport,seed,task,passed:!error,ms:Math.round(performance.now()-start),calls:engine.latencies.length-calls,...(reward?{reward}:{}),...(error?{error}:{})});
        console.log(`${error?'FAIL':'PASS'} ${transport}/${task}/${seed}${error?' '+error.slice(0,160):''}`);
      }
      checked(await engine.call('close_browser',{browser_id}));
    }
  }finally{await engine.close();}}
}finally{
  server.closeAllConnections();await new Promise(resolve=>server.close(resolve));
  report.finished_at=new Date().toISOString();report.summary={passed:report.results.filter(v=>v.passed).length,total:report.results.length};
  report.rpc_step_limit=40;
  if(args.output)fs.writeFileSync(args.output,JSON.stringify(report,null,2)+'\n');console.log(JSON.stringify(report.summary));
  if(report.summary.passed!==report.summary.total)process.exitCode=1;
}
