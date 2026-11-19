// Offline Rust migration -> unchanged WebUI API, entirely disposable state.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import os from "node:os";
import crypto from "node:crypto";
import { spawn, spawnSync } from "node:child_process";
import { DatabaseSync } from "node:sqlite";
import { credentialFixture } from "../../kageha-loop/tests/credential_fixture.mjs";
import { isolatedModelFixture } from "../../kageha-loop/tests/isolated_model_fixture.mjs";

const root = fs.mkdtempSync(
  path.join(os.tmpdir(), "kageha-history-migration-"),
);
const bin = path.resolve(
  process.env.CARGO_TARGET_DIR || "target/migration-acceptance",
  "debug",
);
const launcher = path.join(bin, "kageha");
const source = path.join(root, "python-sessions");
const state = path.join(root, "rust-state");
const chat = "historical-chat";
const token = "disposable-history-migration-access-token";
fs.mkdirSync(path.join(source, chat, "inputs"), { recursive: true });
fs.mkdirSync(path.join(root, "workspace"));
fs.mkdirSync(path.join(root, "empty"));
const records = [
  {
    role: "user",
    text: "Original request",
    turn_id: "keep",
    attachments: ["inputs/proof.txt"],
  },
  {
    role: "assistant",
    text: "Original response",
    turn_id: "keep",
    speaker: { id: "old-specialist", name: "Older specialist" },
  },
  { role: "assistant", text: "Deleted response", turn_id: "deleted" },
  { role: "tool", content: "raw provenance" },
];
fs.writeFileSync(
  path.join(source, chat, "chat.jsonl"),
  records.map((r) => JSON.stringify(r)).join("\n") + "\n",
);
fs.writeFileSync(
  path.join(source, chat, "meta.json"),
  JSON.stringify({
    title: "Historical title",
    pinned: true,
    created_at: 123,
    updated_at: 456,
    deleted_history_turn_ids: ["deleted"],
  }),
);
fs.writeFileSync(
  path.join(source, chat, "inputs/proof.txt"),
  "Portable historical evidence",
);
const sha = (file) =>
  crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");
const before = ["chat.jsonl", "meta.json", "inputs/proof.txt"].map((p) =>
  sha(path.join(source, chat, p)),
);
const env = {
  PATH: path.join(root, "empty"),
  HOME: root,
  ...isolatedModelFixture(root),
  KAGEHA_SERVER_TOKEN: token,
  KAGEHA_LEGACY_SESSIONS_DIR: path.join(root, "empty"),
};
function migrate(from = source, to = state, success = true) {
  const result = spawnSync(
    launcher,
    ["migrate", "--legacy-sessions", from, "--state-dir", to],
    { cwd: root, env, encoding: "utf8", timeout: 15000 },
  );
  if (success) {
    assert.equal(result.status, 0, result.stderr);
    const data = JSON.parse(result.stdout);
    assert.equal(data.domain, "legacy-conversation-history");
    assert.equal(data.full_migration_complete, false);
    return data.report;
  }
  assert.notEqual(result.status, 0);
  return result;
}
assert.equal(migrate().imported, 1);
assert.equal(
  fs.existsSync(path.join(state, "components/credentials.sqlite")),
  false,
  "Offline import never initializes credentials",
);
const repeated = migrate();
assert.equal(repeated.imported, 0);
assert.equal(repeated.already_present, 1);
// Fault-inject the durable state left by a crash after the host transaction,
// before publication of the importer's acknowledgement. Do not change projection.
const journal = new DatabaseSync(
  path.join(state, "history-import-v1/migration.sqlite"),
);
journal.exec("UPDATE history_import_v1 SET state='prepared'");
journal.close();
assert.equal(migrate().imported, 1);
assert.deepEqual(
  ["chat.jsonl", "meta.json", "inputs/proof.txt"].map((p) =>
    sha(path.join(source, chat, p)),
  ),
  before,
);
const db = new DatabaseSync(path.join(state, "native-chats.sqlite"), {
  readOnly: true,
});
assert.equal(
  db.prepare("SELECT COUNT(*) AS n FROM ui_legacy_messages").get().n,
  2,
);
const file = db
  .prepare("SELECT source,sha256 FROM ui_legacy_files WHERE chat_id=?")
  .get(chat);
assert(file.source.startsWith(fs.realpathSync(state) + path.sep));
assert.equal(sha(file.source), before[2]);
assert.equal(
  db.prepare("SELECT created_at FROM ui_sessions WHERE id=?").get(chat)
    .created_at,
  123,
);
db.close();
// Adopt the previous one-time import format without rewriting messages or
// resetting user changes made after initial import.
const adoptionState = path.join(root, "adoption-state");
migrate(source, adoptionState);
const adoption = new DatabaseSync(
  path.join(adoptionState, "native-chats.sqlite"),
);
adoption.exec(
  "DELETE FROM ui_portable_history_v1; UPDATE ui_sessions SET title='User edited title'",
);
adoption
  .prepare("UPDATE ui_legacy_files SET source=? WHERE chat_id=?")
  .run(path.join(source, chat, "inputs/proof.txt"), chat);
adoption.close();
fs.rmSync(path.join(adoptionState, "history-import-v1"), { recursive: true });
assert.equal(migrate(source, adoptionState).imported, 1);
const adopted = new DatabaseSync(
  path.join(adoptionState, "native-chats.sqlite"),
  { readOnly: true },
);
assert.equal(
  adopted.prepare("SELECT title FROM ui_sessions WHERE id=?").get(chat).title,
  "User edited title",
);
assert(
  adopted
    .prepare("SELECT source FROM ui_legacy_files WHERE chat_id=?")
    .get(chat)
    .source.startsWith(fs.realpathSync(adoptionState) + path.sep),
);
assert.equal(
  adopted.prepare("SELECT COUNT(*) AS n FROM ui_legacy_messages").get().n,
  2,
);
adopted.close();
const copy = path.join(root, "different-source");
fs.cpSync(source, copy, { recursive: true });
migrate(copy, state, false);
fs.writeFileSync(
  path.join(source, chat, "inputs/proof.txt"),
  "Changed historical source",
);
assert(migrate(source, state, false).stderr.includes("changed"));
fs.writeFileSync(
  path.join(source, chat, "inputs/proof.txt"),
  "Portable historical evidence",
);
migrate();
// Hide the source entirely: runtime attachment reads must use the owned copy.
fs.renameSync(source, path.join(root, "preserved-source"));
let child, base;
async function start(legacySource = path.join(root, "empty")) {
  let log = "";
  child = spawn(
    path.join(bin, "kageha-loop"),
    ["serve", path.join(root, "workspace"), state, "127.0.0.1:0"],
    {
      cwd: root,
      env: {
        ...env,
        ...credentialFixture(root),
        KAGEHA_LEGACY_SESSIONS_DIR: legacySource,
      },
      stdio: ["ignore", "pipe", "pipe"],
    },
  );
  child.stdout.on("data", (b) => {
    log += b;
  });
  child.stderr.on("data", (b) => {
    log += b;
  });
  for (const end = Date.now() + 15000; Date.now() < end; ) {
    if (child.exitCode !== null) throw Error(log);
    const match = log.match(/listening on (127\.0\.0\.1:\d+)/);
    if (match) {
      base = `http://${match[1]}`;
      return;
    }
    await new Promise((r) => setTimeout(r, 25));
  }
  throw Error(log || "Host startup timed out");
}
async function stop() {
  if (child && child.exitCode === null && child.signalCode === null) {
    const done = new Promise((r) => child.once("exit", r));
    child.kill("SIGTERM");
    await done;
  }
}
async function request(route, method = "GET", body) {
  return fetch(base + route, {
    method,
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
    },
    body: body ? JSON.stringify(body) : undefined,
    signal: AbortSignal.timeout(10000),
  });
}
try {
  await start();
  const session = await (await request(`/api/sessions/${chat}`)).json();
  assert.equal(session.session_id, chat);
  assert.equal(session.title, "Historical title");
  assert.equal(session.pinned, true);
  assert.equal(session.messages.length, 2);
  assert.equal(session.messages[1].speaker.name, "Older specialist");
  assert(!JSON.stringify(session).includes("Deleted response"));
  const attachment = await request(
    `/api/sessions/${chat}/files/inputs/proof.txt`,
  );
  assert.equal(attachment.status, 200);
  assert.equal(await attachment.text(), "Portable historical evidence");
  const another = (await (await request("/api/sessions", "POST", {})).json())
    .session_id;
  assert.notEqual(
    (await request(`/api/sessions/${another}/files/inputs/proof.txt`)).status,
    200,
  );
  // Re-point to the preserved fixture only for the lock check, never for serving.
  const locked = migrate(path.join(root, "preserved-source"), state, false);
  assert(
    /WouldBlock|temporarily unavailable|os error (11|35)/i.test(locked.stderr),
    locked.stderr,
  );
  assert.equal((await request(`/api/sessions/${chat}`, "DELETE")).status, 200);
  assert.equal((await request(`/api/sessions/${chat}`)).status, 404);
  await stop();
  fs.renameSync(path.join(root, "preserved-source"), source);
  assert(migrate(source, state, false).stderr.includes("removed"));
  const deleted = new DatabaseSync(path.join(state, "native-chats.sqlite"), {
    readOnly: true,
  });
  assert.equal(
    deleted
      .prepare("SELECT COUNT(*) AS n FROM ui_sessions WHERE id=?")
      .get(chat).n,
    0,
  );
  deleted.close();
  const alias = path.join(root, "legacy-path-alias");
  fs.symlinkSync(source, alias);
  await start(alias);
  assert.equal((await request(`/api/sessions/${chat}`)).status, 404);
  await stop();
  console.log(
    JSON.stringify({
      passed: true,
      python_backend: false,
      node_backend: false,
      offline_history_import: true,
      repeat_import: true,
      projection_ack_recovery: true,
      legacy_projection_adoption: true,
      source_change_rejected: true,
      source_independent_attachments: true,
      deleted_turn_hidden: true,
      stable_chat_ids: true,
      writer_lock: true,
      deleted_chat_not_resurrected: true,
      fixture: root,
    }),
  );
} finally {
  await stop();
}
