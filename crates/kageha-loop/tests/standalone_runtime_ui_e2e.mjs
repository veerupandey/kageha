// The same retained UI contracts run against a separate Rust turn process.
import fs from 'node:fs';import path from 'node:path';import assert from 'node:assert/strict';import {spawn} from 'node:child_process';import {DatabaseSync} from 'node:sqlite';
const root=fs.mkdtempSync('/tmp/kageha-standalone-ui-');const token='standalone-runtime-private-fixture-token';fs.writeFileSync(root+'/token',token,{mode:0o600});const log=root+'/runtime.log';const fd=fs.openSync(log,'a');
const daemon=spawn(path.resolve(process.env.CARGO_TARGET_DIR || "target", "debug/kageha-runtime-service"),['bridge','--state',root+'/state','--token-file',root+'/token','--bind','127.0.0.1:0'],{env:{PATH:''},stdio:['ignore',fd,fd]});fs.closeSync(fd);
const pause=ms=>new Promise(r=>setTimeout(r,ms));let url;
try{
 for(let until=Date.now()+10000;Date.now()<until;){if(daemon.exitCode!==null)throw Error(fs.readFileSync(log,'utf8'));const m=fs.readFileSync(log,'utf8').match(/runtime bridge listening on ([^\s]+)/);if(m){url='http://'+m[1];break;}await pause(20);}assert(url);
 const info=await fetch(url+'/v1/info',{headers:{Authorization:'Bearer '+token}}).then(r=>r.json());assert.equal(info.mode,'standalone-client-ports');assert.equal(info.pid,daemon.pid);
 for(const file of ['webui_runtime_e2e.mjs','session_events_e2e.mjs']){
  const child=spawn(process.execPath,[path.resolve('crates/kageha-loop/tests/'+file)],{env:{...process.env,KAGEHA_LOOP_BIN:path.resolve(process.env.CARGO_TARGET_DIR || "target", "debug/kageha-loop"),KAGEHA_RUNTIME_SERVICE_URL:url,KAGEHA_RUNTIME_SERVICE_TOKEN_FILE:root+'/token'},stdio:'inherit'});
  assert.equal(await new Promise(r=>child.once('exit',r)),0,file);
 }
 const db=new DatabaseSync(root+'/state/turns.sqlite',{readOnly:true});
 const states=db.prepare('SELECT state,count(*) n FROM runs GROUP BY state').all();assert(states.some(r=>r.state==='succeeded'&&r.n>=3));
 const effects=db.prepare("SELECT count(*) n FROM effects WHERE id LIKE 'port:%' AND result IS NOT NULL").get().n;assert(effects>10);db.close();
 console.log(JSON.stringify({passed:true,separate_runtime_pid:daemon.pid,port_receipts:effects,states,python_backend:false,fixture:root}));
}finally{if(daemon.exitCode===null){daemon.kill('SIGTERM');await Promise.race([new Promise(r=>daemon.once('exit',r)),pause(3000)]);if(daemon.exitCode===null)daemon.kill('SIGKILL');}}
