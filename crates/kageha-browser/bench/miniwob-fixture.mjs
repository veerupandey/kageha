// Evaluator-owned fixture setup and grading; never exposed as an agent tool.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import http from 'node:http';
import { once } from 'node:events';
import { execFileSync } from 'node:child_process';
import { until } from './support.mjs';
export const revision='33c3b4ddef8c6eb67c57a29663d844b1eda7e614';
export async function serveFixture(checkout=process.env.MINIWOB_ROOT){
  assert(checkout,'Set MINIWOB_ROOT');
  assert.equal(execFileSync('git',['-C',checkout,'rev-parse','HEAD'],{encoding:'utf8'}).trim(),revision);
  assert.equal(execFileSync('git',['-C',checkout,'status','--porcelain'],{encoding:'utf8'}).trim(),'','Fixture must be clean');
  const root=path.join(path.resolve(checkout),'miniwob/html');
  const server=http.createServer((request,response)=>{
    let name;try{name=path.resolve(root,'.'+decodeURIComponent(new URL(request.url,'http://fixture').pathname));}catch{response.writeHead(400).end();return;}
    if(!name.startsWith(root+path.sep)||!fs.existsSync(name)||!fs.statSync(name).isFile()){response.writeHead(404).end();return;}
    response.setHeader('content-type',name.endsWith('.js')?'application/javascript':name.endsWith('.css')?'text/css':name.endsWith('.html')?'text/html':'application/octet-stream');response.end(fs.readFileSync(name));
  });server.listen(0,'127.0.0.1');await once(server,'listening');
  return {base:`http://127.0.0.1:${server.address().port}/miniwob/`,async close(){server.closeAllConnections();await new Promise(resolve=>server.close(resolve));}};
}
export async function evaluator(engine,target){
  const port=await until(()=>{
    for(const name of fs.readdirSync(engine.stateDir))if(name.startsWith('chromium-')){
      const file=path.join(engine.stateDir,name,'DevToolsActivePort');if(fs.existsSync(file))return Number(fs.readFileSync(file,'utf8').split('\n')[0]);
    }
  });
  const tabs=await(await fetch(`http://127.0.0.1:${port}/json/list`)).json();
  const tab=tabs.find(value=>value.id===target);assert(tab,'Evaluator target absent');
  const socket=new WebSocket(tab.webSocketDebuggerUrl);await once(socket,'open');
  let id=0;const pending=new Map();
  socket.addEventListener('message',event=>{const message=JSON.parse(event.data);const item=pending.get(message.id);if(item){clearTimeout(item.timer);pending.delete(message.id);message.error?item.reject(Error('Evaluator CDP error')):item.resolve(message.result);}});
  async function evaluate(expression){
    const serial=++id;
    const reply=await new Promise((resolve,reject)=>{const timer=setTimeout(()=>{pending.delete(serial);reject(Error('Evaluator timeout'));},5000);pending.set(serial,{resolve,reject,timer});socket.send(JSON.stringify({id:serial,method:'Runtime.evaluate',params:{expression,returnByValue:true}}));});
    if(reply.exceptionDetails)throw Error('Fixture JavaScript setup/grading failed');return reply.result.value;
  }
  return {
    async reset(seed,episodeMs){
      await until(()=>evaluate('typeof core!=="undefined" && !!core.cover_div'));
      await evaluate(`Math.seedrandom(${JSON.stringify(String(seed))});core.EPISODE_MAX_TIME=${Number(episodeMs)};core.startEpisodeReal();true`);
      await until(()=>evaluate('WOB_TASK_READY!==false'));
    },
    async grade(){return evaluate('({done:WOB_DONE_GLOBAL,raw:WOB_RAW_REWARD_GLOBAL,reward:WOB_REWARD_GLOBAL,reason:WOB_REWARD_REASON})');},
    close(){for(const item of pending.values())clearTimeout(item.timer);socket.close();},
  };
}
