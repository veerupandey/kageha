import { test } from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { DatabaseSync } from "node:sqlite";
import { authorizeVaultIfRequested, prepareVaultSnapshot } from "./agent_benchmark_credentials.mjs";

function fixture() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "kageha-vault-test-"));
  const source = path.join(root, "source"), state = path.join(root, "state");
  fs.mkdirSync(path.join(source, "credentials"), {recursive:true, mode:0o700});
  fs.mkdirSync(state, {mode:0o700});
  const db = new DatabaseSync(path.join(source, "native-chats.sqlite"));
  db.exec(`CREATE TABLE ui_documents(kind TEXT,id TEXT,body TEXT,revision INTEGER,updated REAL);
    CREATE TABLE ui_credential_owner(singleton INTEGER,owner TEXT);
    CREATE TABLE ui_credential_refs(name TEXT,namespace TEXT,credential_id TEXT,revision INTEGER,revoked INTEGER);`);
  db.prepare("INSERT INTO ui_documents VALUES('model',?,?,1,0)").run("chosen", JSON.stringify({id:"chosen",provider:"fixture",model:"model-name",base_url:"https://fixture.test/v1",protocol:"openai_compat"}));
  db.prepare("INSERT INTO ui_documents VALUES('model','other',?,1,0)").run("{}");
  db.exec("INSERT INTO ui_credential_owner VALUES(1,'owner'); INSERT INTO ui_credential_refs VALUES('fixture','owner/provider','fixture',3,0); INSERT INTO ui_credential_refs VALUES('unrelated','owner/provider','unrelated',1,0)");
  db.close();
  const vault = new DatabaseSync(path.join(source, "credentials/vault.sqlite"));
  vault.exec("CREATE TABLE encrypted_payload(value TEXT); INSERT INTO encrypted_payload VALUES('fixture-ciphertext')"); vault.close();
  const registry = new DatabaseSync(path.join(source, "credentials/registry.sqlite"));
  registry.exec("CREATE TABLE credential_binding(singleton INTEGER,body TEXT); CREATE TABLE credential_replacements(id TEXT,body TEXT)");
  registry.prepare("INSERT INTO credential_binding VALUES(1,?)").run(JSON.stringify({backend:"embedded"})); registry.close();
  return {root, source, state};
}

test("copies only selected model and reference into isolated private state", async () => {
  const f = fixture();
  try {
    const provider = await prepareVaultSnapshot(path.join(f.source,"native-chats.sqlite"), "chosen", f.state);
    assert.equal(provider.modelId, "chosen");
    assert.equal(provider.model, "model-name");
    assert.equal("key" in provider, false);
    const db = new DatabaseSync(path.join(f.state,"native-chats.sqlite"), {readOnly:true});
    try {
      assert.deepEqual(db.prepare("SELECT kind,id FROM ui_documents ORDER BY kind").all().map(r=>[r.kind,r.id]), [["model","chosen"],["setting","roles"]]);
      assert.deepEqual(db.prepare("SELECT name FROM ui_credential_refs").all().map(r=>r.name), ["fixture"]);
      assert.equal(JSON.parse(db.prepare("SELECT body FROM ui_documents WHERE kind='setting'").get().body).default, "chosen");
    } finally { db.close(); }
    for (const p of ["native-chats.sqlite", "credentials/vault.sqlite", "credentials/registry.sqlite"]) {
      assert.equal(fs.statSync(path.join(f.state,p)).mode & 0o077, 0);
    }
    assert.equal(fs.statSync(path.join(f.state,"credentials")).mode & 0o077, 0);
  } finally { fs.rmSync(f.root,{recursive:true,force:true}); }
});

test("fails closed for a revoked binding or an active replacement", async () => {
  const f = fixture();
  try {
    const db = new DatabaseSync(path.join(f.source,"native-chats.sqlite"));
    db.exec("UPDATE ui_credential_refs SET revoked=1 WHERE name='fixture'"); db.close();
    await assert.rejects(prepareVaultSnapshot(path.join(f.source,"native-chats.sqlite"),"chosen",f.state), /no active vault credential/);
    const db2 = new DatabaseSync(path.join(f.source,"native-chats.sqlite"));
    db2.exec("UPDATE ui_credential_refs SET revoked=0 WHERE name='fixture'"); db2.close();
    const registry = new DatabaseSync(path.join(f.source,"credentials/registry.sqlite"));
    registry.exec("INSERT INTO credential_replacements VALUES('pending','{}')"); registry.close();
    await assert.rejects(prepareVaultSnapshot(path.join(f.source,"native-chats.sqlite"),"chosen",f.state), /replacement records/);
  } finally { fs.rmSync(f.root,{recursive:true,force:true}); }
});

test("uses a configured environment credential only when no vault binding exists", async () => {
  const f = fixture();
  try {
    const db = new DatabaseSync(path.join(f.source,"native-chats.sqlite"));
    const row = JSON.parse(db.prepare("SELECT body FROM ui_documents WHERE id='chosen'").get().body);
    row.api_key_env="FIXTURE_ENV_TOKEN";
    db.prepare("UPDATE ui_documents SET body=? WHERE id='chosen'").run(JSON.stringify(row));
    db.exec("DELETE FROM ui_credential_refs WHERE name='fixture'"); db.close();
    await assert.rejects(prepareVaultSnapshot(path.join(f.source,"native-chats.sqlite"),"chosen",f.state,{}), /no active vault credential/);
    const selected=await prepareVaultSnapshot(path.join(f.source,"native-chats.sqlite"),"chosen",f.state,{FIXTURE_ENV_TOKEN:"synthetic-env-token"});
    assert.equal(selected.environment,true);
    assert.equal(selected.key,"synthetic-env-token");
    assert.deepEqual(fs.readdirSync(f.state), []);
  } finally { fs.rmSync(f.root,{recursive:true,force:true}); }
});

test("Keychain authorization is requested only when explicitly selected", async () => {
  const calls=[];
  const api=async (...args)=>calls.push(args);
  await authorizeVaultIfRequested(false,false,api);
  assert.deepEqual(calls,[]);
  await authorizeVaultIfRequested(true,false,api);
  assert.deepEqual(calls,[["/api/provider-setup/unlock",{}]]);
  await assert.rejects(authorizeVaultIfRequested(true,true,api), /saved Keychain-backed provider/);
  assert.equal(calls.length,1);
});
