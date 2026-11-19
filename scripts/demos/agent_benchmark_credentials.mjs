// Isolate the selected provider without exporting any vault credential.
import fs from "node:fs";
import path from "node:path";
import { DatabaseSync, backup } from "node:sqlite";

/** Explicit opt-in only: the native server may ask the user to unlock Keychain. */
export async function authorizeVaultIfRequested(requested, environmentProvider, api) {
  if (!requested) return;
  if (environmentProvider) throw Error("--unlock-vault requires a saved Keychain-backed provider.");
  await api("/api/provider-setup/unlock", {});
}

async function privateBackup(sourcePath, destination) {
  if (!fs.statSync(sourcePath).isFile()) throw Error("Credential database is unavailable.");
  fs.closeSync(fs.openSync(destination, "wx", 0o600));
  const source = new DatabaseSync(sourcePath, {readOnly: true});
  try { await backup(source, destination); } finally { source.close(); }
  fs.chmodSync(destination, 0o600);
}

/** Seed only one model and its vault binding into a fresh private server state. */
export async function prepareVaultSnapshot(settingsPath, modelId, state, env = process.env) {
  const source = new DatabaseSync(settingsPath, {readOnly: true});
  let model, row, owner, ref;
  try {
    row = source.prepare("SELECT body,revision,updated FROM ui_documents WHERE kind='model' AND id=?").get(modelId);
    if (!row) throw Error("Model is not configured; pass --model-id or explicit provider environment variables.");
    model = JSON.parse(row.body);
    const provider = model.provider;
    if (typeof provider !== "string" || !provider) throw Error("Model provider is not configured.");
    owner = source.prepare("SELECT owner FROM ui_credential_owner WHERE singleton=1").get();
    ref = source.prepare("SELECT name,namespace,credential_id,revision,revoked FROM ui_credential_refs WHERE name=?").get(provider);
  } finally { source.close(); }
  if (model.protocol !== "openai_compat" || !model.base_url?.startsWith("https://")) {
    throw Error("Benchmark requires an HTTPS OpenAI-compatible provider.");
  }
  if (ref?.revoked) throw Error("Selected provider has no active vault credential; configure it in Kageha first.");
  if (!ref) {
    const name = model.provider === "environment" ? "KAGEHA_API_KEY" : model.api_key_env;
    const key = typeof name === "string" && name ? env[name] : undefined;
    if (typeof key !== "string" || !key) throw Error("Selected provider has no active vault credential; configure it in Kageha first.");
    return {url: model.base_url, model: model.model, protocol: model.protocol, modelId, key, environment:true};
  }
  if (!owner) throw Error("Selected provider has no active vault credential; configure it in Kageha first.");
  const sourceDir = path.join(path.dirname(settingsPath), "credentials");
  const registry = new DatabaseSync(path.join(sourceDir, "registry.sqlite"), {readOnly: true});
  try {
    const binding = registry.prepare("SELECT body FROM credential_binding WHERE singleton=1").get();
    const bindingBody = binding && (typeof binding.body === "string" ? binding.body : Buffer.from(binding.body).toString("utf8"));
    if (!bindingBody || JSON.parse(bindingBody).backend !== "embedded") throw Error("Benchmark requires the embedded credential vault.");
    if (registry.prepare("SELECT COUNT(*) AS count FROM credential_replacements").get().count !== 0) {
      throw Error("Credential replacement records are present; use a stable embedded vault for the benchmark.");
    }
  } finally { registry.close(); }
  const credentials = path.join(state, "credentials");
  fs.mkdirSync(credentials, {mode: 0o700});
  await privateBackup(path.join(sourceDir, "vault.sqlite"), path.join(credentials, "vault.sqlite"));
  await privateBackup(path.join(sourceDir, "registry.sqlite"), path.join(credentials, "registry.sqlite"));
  const dbPath = path.join(state, "native-chats.sqlite");
  fs.closeSync(fs.openSync(dbPath, "wx", 0o600));
  const target = new DatabaseSync(dbPath);
  try {
    target.exec(`CREATE TABLE ui_documents(kind TEXT NOT NULL,id TEXT NOT NULL,body TEXT NOT NULL,revision INTEGER NOT NULL,updated REAL NOT NULL,PRIMARY KEY(kind,id));
      CREATE TABLE ui_credential_owner(singleton INTEGER PRIMARY KEY CHECK(singleton=1),owner TEXT NOT NULL);
      CREATE TABLE ui_credential_refs(name TEXT PRIMARY KEY,namespace TEXT NOT NULL,credential_id TEXT NOT NULL,revision INTEGER NOT NULL,revoked INTEGER NOT NULL);`);
    target.prepare("INSERT INTO ui_documents VALUES('model',?,?,?,?)").run(modelId, row.body, row.revision, row.updated);
    target.prepare("INSERT INTO ui_documents VALUES('setting','roles',?,1,0)").run(JSON.stringify({default:modelId}));
    target.prepare("INSERT INTO ui_credential_owner VALUES(1,?)").run(owner.owner);
    target.prepare("INSERT INTO ui_credential_refs VALUES(?,?,?,?,?)").run(ref.name, ref.namespace, ref.credential_id, ref.revision, ref.revoked);
  } finally { target.close(); }
  return {url: model.base_url, model: model.model, protocol: model.protocol, modelId};
}
