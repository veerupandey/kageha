// Explicit dotenv import through the real running Rust host; no Python or Keychain.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import net from 'node:net';
import {spawn,execFileSync} from 'node:child_process';
import {DatabaseSync} from 'node:sqlite';
const root=fs.mkdtempSync('/tmp/kageha-model-migrate-');fs.chmodSync(root,0o700);
const binary=path.resolve('target/debug/kageha-loop'),token=crypto.randomBytes(24).toString('hex'),secret='MODEL-MIGRATION-CANARY-'+crypto.randomUUID();
const write=(name,value)=>{const p=path.join(root,name);fs.writeFileSync(p,value,{mode:0o600});return p;};
const catalog=write('models.yaml','providers:\n  fixture:\n    protocol: openai_compat\n    base_url: https://provider.example/${ACCOUNT_ID}/v1\n    api_key_env: FIXTURE_KEY\nmodels:\n  - {id: imported, provider: fixture, model: worker, context_window: 12345}\nroles: {default: [imported]}\n');
const source=write('source.env','ACCOUNT_ID=fixture\nFIXTURE_KEY="'+secret+'"\n'),tokenFile=write('api.token',token),key=write('master.key',crypto.randomBytes(32));
const listener=net.createServer();await new Promise(r=>listener.listen(0,'127.0.0.1',r));const port=listener.address().port;await new Promise(r=>listener.close(r));
const state=path.join(root,'state'),base='http://127.0.0.1:'+port,log=write('host.log','');
const env={PATH:'',HOME:root,KAGEHA_SERVER_TOKEN:token,KAGEHA_CREDENTIAL_KEY_FILE:key,KAGEHA_MODELS_FILE:catalog};
let host,db;
async function api(route,body){const r=await fetch(base+route,{method:body?'POST':'GET',headers:{Authorization:'Bearer '+token,'Content-Type':'application/json'},body:body?JSON.stringify(body):undefined});assert.equal(r.status,200);const value=await r.json();assert(!JSON.stringify(value).includes(secret));return value;}
const migrate=(apply)=>JSON.parse(execFileSync(binary,['migrate-models',root,state,source,base,tokenFile,...(apply?['--apply']:[])],{env,encoding:'utf8',stdio:['ignore','pipe','pipe']}));
try{
  host=spawn(binary,['serve',root,state,'127.0.0.1:'+port],{env,stdio:['ignore',fs.openSync(log,'a'),fs.openSync(log,'a')]});
  let ready=false;for(let i=0;i<300;i++){try{await api('/api/models');ready=true;break;}catch{await new Promise(r=>setTimeout(r,25));}}assert(ready);
  const plan=migrate(false);assert.equal(plan.credentials[0].status,'would_import');
  const result=migrate(true);assert.equal(result.credentials[0].status,'imported');
  assert.equal(migrate(true).credentials[0].status,'existing_preserved');
  host.kill('SIGTERM');await new Promise(r=>host.once('exit',r));
  host=spawn(binary,['serve',root,state,'127.0.0.1:'+port],{env,stdio:['ignore',fs.openSync(log,'a'),fs.openSync(log,'a')]});
  ready=false;for(let i=0;i<300;i++){try{await api('/api/models');ready=true;break;}catch{await new Promise(r=>setTimeout(r,25));}}assert(ready);
  db=new DatabaseSync(path.join(state,'native-chats.sqlite'));
  assert.equal(JSON.parse(db.prepare("SELECT body FROM ui_documents WHERE kind='model' AND id='imported'").get().body).base_url,'https://provider.example/fixture/v1');
  assert.equal(migrate(true).planned_model_repairs,0);
  assert.equal(db.prepare('SELECT count(*) n FROM ui_secrets').get().n,0);
  assert.equal(db.prepare("SELECT revoked FROM ui_credential_refs WHERE name='fixture'").get().revoked,0);
  // Updating connection settings preserves retained metadata and rejects stale revisions.
  const current=JSON.parse(db.prepare("SELECT body FROM ui_documents WHERE kind='model' AND id='imported'").get().body);
  await api('/api/provider-setup/model',{provider:'fixture',model_id:'imported',model:'worker',protocol:'openai_compat',base_url:'https://provider.example/v2',expected_revision:current.revision});
  const updated=JSON.parse(db.prepare("SELECT body FROM ui_documents WHERE kind='model' AND id='imported'").get().body);assert.equal(updated.context_window,12345);
  const stale=await fetch(base+'/api/provider-setup/model',{method:'POST',headers:{Authorization:'Bearer '+token,'Content-Type':'application/json'},body:JSON.stringify({provider:'fixture',model_id:'imported',model:'worker',protocol:'openai_compat',base_url:'https://provider.example/v3',expected_revision:current.revision})});assert.equal(stale.status,409);
  await api('/api/credentials/revoke',{name:'fixture'});assert.equal(migrate(true).credentials[0].status,'revoked_preserved');
  const values=db.prepare('SELECT body FROM ui_documents UNION ALL SELECT body FROM ui_document_history').all().map(r=>r.body).join('');assert(!values.includes(secret));assert(!fs.readFileSync(log,'utf8').includes(secret));assert(fs.readFileSync(source,'utf8').includes(secret));
  console.log(JSON.stringify({passed:true,real_vault_import:true,restart_safe:true,revocation_preserved:true,metadata_preserved:true,conditional_updates:true,source_preserved:true,python_backend:false}));
}finally{db?.close();host?.kill('SIGTERM');if(host)await new Promise(r=>host.once('exit',r));if(process.env.KEEP_MODEL_MIGRATION_FIXTURE)console.error(root);else fs.rmSync(root,{recursive:true,force:true});}
