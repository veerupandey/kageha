// Disposable real-UI fixture; Rust hosts/runtime, Node only supplies deterministic model replies.
import fs from 'node:fs';import path from 'node:path';import http from 'node:http';import {spawn} from 'node:child_process';
const root=fs.mkdtempSync('/tmp/kageha-ui-stream-browser-');const token='browser-ui-disposable-integration-token';fs.writeFileSync(root+'/token',token,{mode:0o600});fs.mkdirSync(root+'/workspace');fs.mkdirSync(root+'/empty');fs.writeFileSync(root+'/workspace/notes.txt','Read-only UI proof');
let releaseBeta;const model=http.createServer(async(req,res)=>{
 if(req.url==='/release'){releaseBeta?.();res.end('released');return;}
 if(req.method==='GET'){res.end(JSON.stringify({data:[{id:'fixture'}]}));return;}
 let data='';for await(const b of req)data+=b;const body=JSON.parse(data);const intent=body.messages.filter(m=>m.role==='user').at(-1)?.content||'';const tools=body.messages.filter(m=>m.role==='tool');const reply=message=>{if(!res.destroyed)res.end(JSON.stringify({choices:[{message}]}));};
 if(intent.includes('Alpha')||intent.includes('Beta')){
  if(!tools.length){reply({role:'assistant',content:null,tool_calls:[{id:'read-source',type:'function',function:{name:'read_file',arguments:JSON.stringify({path:'notes.txt'})}}]});return;}
  if(intent.includes('Beta'))await new Promise(r=>{releaseBeta=r;});reply({role:'assistant',content:intent.includes('Beta')?'Beta worker finished after parent.':'Alpha worker finished.'});return;
 }
 if(!tools.length){reply({role:'assistant',content:null,tool_calls:['Alpha','Beta'].map(name=>({id:'spawn-'+name,type:'function',function:{name:'agent_spawn',arguments:JSON.stringify({agent_id:'main',objective:name+' read notes.txt then report the proof.'})}}))});return;}
 reply({role:'assistant',content:'Parent finished; background workers continue.'});
});await new Promise(r=>model.listen(0,'127.0.0.1',r));const pause=ms=>new Promise(r=>setTimeout(r,ms));
const children=[];async function launch(binary,args,env,pattern,label){const log=root+'/'+label+'.log';const fd=fs.openSync(log,'a');const child=spawn(binary,args,{env,stdio:['ignore',fd,fd]});children.push(child);fs.closeSync(fd);for(let end=Date.now()+10000;Date.now()<end;){if(child.exitCode!==null)throw Error(fs.readFileSync(log,'utf8'));const m=fs.readFileSync(log,'utf8').match(pattern);if(m)return 'http://'+m[1];await pause(20);}throw Error('Startup timeout');}
const runtime=await launch(path.resolve('target/debug/kageha-runtime-service'),['bridge','--state',root+'/runtime','--token-file',root+'/token','--bind','127.0.0.1:0'],{PATH:''},/runtime bridge listening on ([^\s]+)/,'runtime');
const app=await launch(path.resolve('target/debug/kageha-loop'),['serve',root+'/workspace',root+'/application','127.0.0.1:0'],{PATH:'',HOME:root,KAGEHA_SERVER_TOKEN:token,KAGEHA_PROVIDER_URL:`http://127.0.0.1:${model.address().port}/v1/chat/completions`,KAGEHA_MODEL:'fixture',KAGEHA_LEGACY_SESSIONS_DIR:root+'/empty',KAGEHA_WEBUI_DIST:path.resolve('frontend/dist'),KAGEHA_RUNTIME_SERVICE_URL:runtime,KAGEHA_RUNTIME_SERVICE_TOKEN_FILE:root+'/token'},/listening on ([^\s]+)/,'app');
fs.writeFileSync('/tmp/kageha-ui-stream-browser-connection.json',JSON.stringify({root,app,token,release:`http://127.0.0.1:${model.address().port}/release`,runtime}));console.log(JSON.stringify({root,app,runtime}));
const stop=()=>{for(const child of children)child.kill('SIGTERM');model.closeAllConnections();model.close();setTimeout(()=>process.exit(0),1000);};process.on('SIGTERM',stop);process.on('SIGINT',stop);
