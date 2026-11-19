// Run in the signed-in macOS user's session. The server cannot submit shell commands.
import os from "node:os";
import path from "node:path";
import { pipeline } from "node:stream/promises";
import fs from "node:fs";
import { spawn, execFile } from "node:child_process";
import { promisify } from "node:util";
import { createInterface } from "node:readline";
import { DatabaseSync } from "node:sqlite";
const exec = promisify(execFile);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
process.umask(0o077);
const cfg = JSON.parse(fs.readFileSync(process.argv[2], "utf8"));
const origin = new URL(cfg.publicUrl);
const local = origin.protocol === "http:" && origin.hostname === "127.0.0.1";
if (
  process.platform !== "darwin" ||
  !Number.isSafeInteger(cfg.chatId) ||
  (cfg.selfChat ? cfg.chatId < 0 : cfg.chatId < 1) ||
  !cfg.owner ||
  (!local && origin.protocol !== "https:") ||
  cfg.publicUrl !== origin.origin ||
  typeof cfg.token !== "string" ||
  cfg.token.length < 32
)
  throw Error(
    "Provide a paired chat, owner, bridge token and HTTPS or local gateway origin",
  );
const db = new DatabaseSync(cfg.database);
db.exec(`PRAGMA journal_mode=WAL;CREATE TABLE IF NOT EXISTS effects(id TEXT PRIMARY KEY,state TEXT);
  CREATE TABLE IF NOT EXISTS cursor(id INTEGER PRIMARY KEY CHECK(id=1),last INTEGER);
  UPDATE effects SET state='uncertain' WHERE state='dispatching';`);
async function post(route, data) {
  const res = await fetch(cfg.publicUrl + "/bridge/" + route, {
    method: "POST",
    redirect: "error",
    signal: AbortSignal.timeout(15000),
    headers: {
      Authorization: "Bearer " + cfg.token,
      "Content-Type": "application/json",
    },
    body: JSON.stringify(data),
  });
  if (!res.ok) throw Error("Bridge HTTP " + res.status);
  return res.json();
}
let watching = false;
async function accept(m, chatId) {
  if (
    m.type ||
    !Number.isSafeInteger(m.id) ||
    m.chat_id !== chatId ||
    m.is_group
  )
    return;
  const text = cfg.selfChat
    ? m.is_from_me && /^\s*Kageha:\s*([\s\S]+)$/i.exec(m.text || "")?.[1].trim()
    : !m.is_from_me && m.sender === cfg.owner && m.text?.trim();
  if (text && text.length <= 16000)
    await post("receive", {
      id: m.guid || String(m.id),
      sender: cfg.owner,
      text,
    });
}
async function incoming() {
  let chatId = cfg.chatId;
  if (cfg.selfChat) {
    const { stdout } = await exec(
      "imsg",
      ["chats", "--no-contacts", "--limit", "1000", "--json"],
      { maxBuffer: 4 * 1024 * 1024, timeout: 15000 },
    );
    const direct = stdout
      .trim()
      .split("\n")
      .filter(Boolean)
      .map((line) => JSON.parse(line))
      .find(
        (c) =>
          c.service === "iMessage" &&
          c.identifier === cfg.owner &&
          !c.is_group &&
          (!chatId || c.id === chatId),
      );
    if (!direct) return;
    chatId = direct.id;
  }
  let last = db.prepare("SELECT last FROM cursor WHERE id=1").get()?.last;
  // Establish the enrollment watermark before opening the persistent watcher.
  if (last === undefined) {
    const args = [
      "history",
      "--chat-id",
      String(chatId),
      "--limit",
      cfg.enrolledAt ? "100" : "1",
      "--json",
    ];
    const { stdout } = await exec("imsg", args);
    const rows = stdout
      .trim()
      .split("\n")
      .filter(Boolean)
      .map((line) => JSON.parse(line))
      .sort((a, b) => a.id - b.id);
    if (cfg.selfChat && cfg.enrolledAt)
      for (const m of rows) {
        if (Date.parse(m.created_at) >= Date.parse(cfg.enrolledAt))
          await accept(m, chatId);
      }
    last = rows.at(-1)?.id || 0;
    db.prepare("INSERT INTO cursor VALUES(1,?)").run(last);
  }
  const child = spawn(
    "imsg",
    [
      "watch",
      "--chat-id",
      String(chatId),
      "--since-rowid",
      String(last),
      "--json",
    ],
    { stdio: ["ignore", "pipe", "inherit"] },
  );
  child.on("error", () => child.stdout.destroy());
  try {
    watching = true;
    for await (const line of createInterface({ input: child.stdout })) {
      const m = JSON.parse(line);
      if (m.type || !Number.isSafeInteger(m.id) || m.id <= last) continue;
      await accept(m, chatId);
      last = m.id;
      db.prepare("UPDATE cursor SET last=? WHERE id=1").run(last);
    }
  } finally {
    watching = false;
    child.kill();
  }
}
async function outgoing() {
  if (!watching) return;
  const { messages, files } = await post("poll", {});
  for (const [kind, rows] of [
    ["message", messages],
    ["file", files],
  ])
    for (const item of rows) {
      let state = db
        .prepare("SELECT state FROM effects WHERE id=?")
        .get(item.id)?.state;
      if (!state) {
        db.prepare("INSERT INTO effects VALUES(?,'dispatching')").run(item.id);
        try {
          if (kind === "file") {
            if (!item.path.startsWith("/") || /[\x00-\x1f]/.test(item.path))
              throw Error("Invalid path");
            await exec("/usr/bin/open", ["-R", item.path], { timeout: 15000 });
          } else if (item.attachment) {
            const dir = fs.mkdtempSync(path.join(os.tmpdir(), "kageha-imsg-"));
            try {
              const file = path.join(dir, path.basename(item.attachment));
              const response = await fetch(
                new URL("/bridge/attachment", origin),
                {
                  method: "POST",
                  headers: {
                    Authorization: "Bearer " + cfg.token,
                    "Content-Type": "application/json",
                  },
                  redirect: "error",
                  body: JSON.stringify({ id: item.id }),
                  signal: AbortSignal.timeout(60000),
                },
              );
              if (!response.ok) throw Error("File download failed");
              await pipeline(
                response.body,
                fs.createWriteStream(file, { flags: "wx", mode: 0o600 }),
              );
              await exec(
                "imsg",
                [
                  "send",
                  "--to",
                  cfg.owner,
                  "--service",
                  "imessage",
                  "--file",
                  file,
                ],
                { timeout: 60000 },
              );
            } finally {
              fs.rmSync(dir, { recursive: true, force: true });
            }
          } else
            await exec(
              "imsg",
              [
                "send",
                "--to",
                cfg.owner,
                "--service",
                "imessage",
                "--text",
                cfg.selfChat ? "Kageha Personal · " + item.text : item.text,
              ],
              { timeout: 30000 },
            );
          state = "sent";
        } catch {
          state = "uncertain";
        }
        db.prepare("UPDATE effects SET state=? WHERE id=?").run(state, item.id);
      }
      await post("receipt", { id: item.id, kind, state });
    }
}
async function loop(fn, delay) {
  let reported = false;
  for (;;) {
    try {
      await fn();
      reported = false;
    } catch (e) {
      if (!reported)
        console.error("Bridge connection or Messages operation unavailable");
      reported = true;
    }
    await sleep(delay);
  }
}
await Promise.all([loop(incoming, 2000), loop(outgoing, 1000)]);
