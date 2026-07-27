// Real process death during import, using only the standalone Rust operator tool.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {spawn,execFileSync} from 'node:child_process';
import {DatabaseSync} from 'node:sqlite';
import {createHash} from 'node:crypto';
const root=fs.mkdtempSync('/tmp/kageha-profile-import-');
const bin=path.resolve(process.env.CARGO_TARGET_DIR||'target','debug/kageha-agent-profiles');
fs.mkdirSync(root+'/empty');
const base=[root+'/profiles.sqlite','disposable-import-scope'];
const run=(...args)=>JSON.parse(execFileSync(bin,[...base,...args],{env:{PATH:root+'/empty'},encoding:'utf8',maxBuffer:8*1024*1024,stdio:['ignore','pipe','pipe']}));
const digest=()=>createHash('sha256').update(fs.readFileSync(root+'/source.json')).digest('hex');
let child;
try {
  const bundle={schema_version:1,source:'immutable-fixture-source',entries:Array.from({length:1024},(_,i)=>{
    const id='profile-'+String(i).padStart(4,'0');return {kind:'agent',id,revisions:[{number:1,value:{id,name:id,system_prompt:'Imported instructions',revision:1},timestamp:1,source_revision:'historical-v1'}]};
  })};
  fs.writeFileSync(root+'/source.json',JSON.stringify(bundle));const original=digest();
  child=spawn(bin,[...base,'import',root+'/source.json'],{env:{PATH:root+'/empty'},stdio:['ignore','pipe','pipe']});
  let stderr='';child.stderr.on('data',b=>stderr+=b);let exited=false;
  const exit=new Promise(r=>child.once('exit',(code,signal)=>{exited=true;r({code,signal});}));
  let checkpoint;
  for(let end=Date.now()+15000;Date.now()<end;) {
    if(exited) throw Error('Import finished before crash checkpoint: '+stderr);
    try {const db=new DatabaseSync(root+'/profiles.sqlite',{readOnly:true});try {const row=db.prepare('SELECT body FROM profile_imports').get();if(row){const p=JSON.parse(row.body);if(p.next>=3&&p.next<p.total){checkpoint=p;break;}}}finally{db.close();}}catch{}
    await new Promise(r=>setTimeout(r,2));
  }
  assert(checkpoint,'A committed import checkpoint is required');child.kill('SIGKILL');assert.equal((await exit).signal,'SIGKILL');child=null;
  const status=run('import',root+'/source.json');assert.equal(status.next,1024);assert.equal(status.total,1024);
  assert.deepEqual(run('export').entries,bundle.entries);
  run('delete','agent','profile-0000','1');
  run('import',root+'/source.json');assert.equal(run('list')[0].revisions.at(-1).value,null);
  const restored=run('restore','agent','profile-0000','2','1');assert.equal(restored.revisions.at(-1).number,3);assert.equal(restored.revisions.at(-1).value.system_prompt,'Imported instructions');
  assert.throws(()=>run('write','agent','profile-0000','1',root+'/source.json'),/Profile revision conflict/);
  const changed=structuredClone(bundle);changed.entries[0].revisions[0].value.name='Changed source';fs.writeFileSync(root+'/changed.json',JSON.stringify(changed));
  assert.throws(()=>run('import',root+'/changed.json'),/Import source changed/);
  assert.equal(digest(),original);
  console.log(JSON.stringify({passed:true,python_backend:false,node_backend:false,checks:['actual process crash during profile import','checkpoint resumes without duplicate revisions','source fingerprints and aliases preserved','repeat import cannot resurrect deletion','restore appends with revision fencing','source files untouched'],committed_before_crash:checkpoint.next}));
} finally {if(child){const p=new Promise(r=>child.once('exit',r));child.kill('SIGKILL');await p;}fs.rmSync(root,{recursive:true,force:true});}
