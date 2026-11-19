import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import readline from "node:readline";
import qrcode from "qrcode-terminal";
import makeWASocket, { DisconnectReason, downloadMediaMessage, useMultiFileAuthState, BufferJSON, generateWAMessage } from "@whiskeysockets/baileys";
import { whatsappMedia, whatsappText } from "../channels/whatsapp.mjs";
import mime from "mime-types";
import P from "pino";

import { toWhatsAppJid, routeInbound } from "./routing.mjs";
import { RetryMessageStore } from "./retry-store.mjs";
import { ReliableSender } from "./delivery.mjs";

const logger = P({ level: "silent" });

const args = process.argv.slice(2);
const valueAfter = (flag, fallback) => {
  const index = args.indexOf(flag);
  return index >= 0 ? args[index + 1] : fallback;
};
const authDir = valueAfter("--auth-dir", path.join(process.cwd(), ".kageha-whatsapp"));
const inboundDir = valueAfter("--inbound-dir", path.join(authDir, "inbound"));
fs.mkdirSync(inboundDir, { recursive: true });
const sentMessageIds = new Set();

const emit = (value) => process.stdout.write(`${JSON.stringify(value)}\n`);
const rememberSent = (result) => {
  const id = result?.key?.id;
  if (id) {
    sentMessageIds.add(id);
    setTimeout(() => sentMessageIds.delete(id), 120000).unref();
  }
};
const sendMedia = async (sock, to, runId, relative, queueId, part) => {
  const sessionRoot = path.resolve(process.env.KAGEHA_HOME || path.join(process.env.HOME || ".", ".kageha"), "sessions", runId);
  const file = path.resolve(sessionRoot, relative);
  if (!file.startsWith(sessionRoot + path.sep) || !fs.existsSync(file)) throw new Error("Attachment unavailable");
  const content = fs.readFileSync(file);
  const contentType = mime.lookup(file) || "application/octet-stream";
  const payload = whatsappMedia(content,path.basename(file),contentType);
  const sent = queueId ? await reliableSender.send(sock, to, payload, queueId, part) : await sock.sendMessage(to, payload);
  rememberSent(sent);
  if (!queueId) rememberMsg(sent?.key, sent?.message, true);
};

class MemoryCache {
  constructor(ttlMs = 300000) {
    this.ttlMs = ttlMs;
    this.map = new Map();
  }
  get(key) {
    const item = this.map.get(key);
    if (!item) return undefined;
    if (Date.now() > item.expires) {
      this.map.delete(key);
      return undefined;
    }
    return item.val;
  }
  set(key, val) {
    this.map.set(key, { val, expires: Date.now() + this.ttlMs });
    return true;
  }
  del(key) {
    return this.map.delete(key);
  }
  flushAll() {
    this.map.clear();
  }
}

const msgRetryCounterCache = new MemoryCache();
const messageStore = new Map();
const retryStore = new RetryMessageStore(authDir, BufferJSON);
const groupCache = new MemoryCache();
const reliableSender = new ReliableSender(retryStore, (sock, jid, payload, messageId) =>
  generateWAMessage(jid, payload, { userJid: sock.user.id, upload: sock.waUploadToServer, logger, messageId }));
const rememberMsg = (key, msg, persistent = false) => {
  if (persistent) retryStore.put(key, msg);
  if (key?.id) {
    messageStore.set(key.id, msg);
    setTimeout(() => messageStore.delete(key.id), 300000).unref();
  }
};

process.on("unhandledRejection", (err) => {
  if (String(err?.message || "").includes("Bad MAC")) {
    emit({ type: "transport_warning", code: "decryption_retry" });
    // Baileys negotiates targeted key retries. Never erase encryption sessions.
  } else {
    emit({ type: "error", error: "WhatsApp bridge failed; restarting automatically." });
    process.exitCode = 1;
    currentSock?.end?.(new Error("Bridge restart"));
    setTimeout(() => process.exit(1), 100).unref();
  }
});

let currentSock = null;
let connected = false;
let shuttingDown = false;
setInterval(() => emit({ type: "heartbeat", connected }), 10000).unref();
const processedInboundIds = new Set();
const rememberInbound = (id) => {
  if (id) {
    processedInboundIds.add(id);
    setTimeout(() => processedInboundIds.delete(id), 60000).unref();
  }
};

const rl = readline.createInterface({ input: process.stdin });
const shutdown = () => {
  if (shuttingDown) return;
  shuttingDown = true;
  connected = false;
  currentSock?.end?.(new Error("Bridge owner stopped"));
  setTimeout(() => process.exit(0), 100);
};
rl.on("close", shutdown);
process.on("SIGTERM", shutdown);
process.on("SIGINT", shutdown);
rl.on("line", async (line) => {
  let command;
  try {
    command = JSON.parse(line);
    const sock = currentSock;
    if (!sock || !connected) {
      if (command.type === "send") emit({ type: "send_error", queue_id: command.queue_id || "", code: "disconnected" });
      return;
    }

    if (command.type === "presence") {
      const jid = toWhatsAppJid(command.to);
      const state = command.state || "composing";
      try {
        if (state === "available" || state === "unavailable") {
          await sock.sendPresenceUpdate(state);
        } else {
          await sock.presenceSubscribe(jid);
          await sock.sendPresenceUpdate(state, jid);
        }
      } catch (error) {
        emit({ type: "presence_error", error: String(error) });
      }
      return;
    }
    if (command.type !== "send") return;
    const jid = toWhatsAppJid(command.to);
    sock.sendPresenceUpdate("paused", jid).catch(() => {});
    if (command.text) {
      const sent = command.queue_id
        ? await reliableSender.send(sock, jid, { text: whatsappText(command.text) }, command.queue_id, "text")
        : await sock.sendMessage(jid, { text: whatsappText(command.text) });
      rememberSent(sent);
      if (!command.queue_id) rememberMsg(sent?.key, sent?.message, true);
      command.external_id = sent?.key?.id || "";
    }
    for (const artifact of command.artifacts || []) {
      await sendMedia(sock, jid, command.run_id, artifact, command.queue_id, `artifact:${artifact}`);
    }
    if (command.queue_id) emit({ type: "send_result", queue_id: command.queue_id,
      ok: true, external_id: command.external_id || "" });
  } catch (error) {
    emit({ type: "send_error", queue_id: command?.queue_id || "", error: "WhatsApp send failed; will retry." });
  }
});

let isReconnecting = false;
let reconnectAttempts = 0;

const start = async () => {
  if (isReconnecting) return;
  const { state, saveCreds } = await useMultiFileAuthState(authDir);
  const sock = makeWASocket({
    auth: state,
    logger,
    printQRInTerminal: false,
    markOnlineOnConnect: false,
    msgRetryCounterCache,
    syncFullHistory: false,
    getMessage: async (key) => {
      return retryStore.get(key) || messageStore.get(key.id) || undefined;
    },
    cachedGroupMetadata: async (jid) => {
      const cached = groupCache.get(jid);
      if (cached) return cached;
      const metadata = await sock.groupMetadata(jid);
      groupCache.set(jid, metadata);
      return metadata;
    },
    defaultQueryTimeoutMs: 60000,
  });
  currentSock = sock;

  sock.ev.on("creds.update", saveCreds);
  const refreshGroup = async (jid) => {
    try { groupCache.set(jid, await sock.groupMetadata(jid)); } catch { groupCache.del(jid); }
  };
  sock.ev.on("groups.update", (events) => { for (const event of events) void refreshGroup(event.id); });
  sock.ev.on("group-participants.update", (event) => { void refreshGroup(event.id); });
  sock.ev.on("connection.update", ({ connection, lastDisconnect, qr }) => {
    if (qr) {
      emit({ type: "qr", value: qr });
      qrcode.generate(qr, { small: true }, (display) => process.stderr.write(`${display}\n`));
    }
    if (connection === "close") {
      if (shuttingDown) return;
      if (currentSock !== sock) return;
      connected = false;
      emit({ type: "reconnecting" });
      const code = lastDisconnect?.error?.output?.statusCode;
      if (code === DisconnectReason.loggedOut) {
        emit({ type: "logged_out", error: "WhatsApp logged out; link this account again" });
        process.exit(1);
      } else {
        // Clean up socket listeners and re-init with backoff
        if (!isReconnecting) {
          isReconnecting = true;
          currentSock = null;
          try {
            sock.ws?.close?.();
          } catch (_) {}
          setTimeout(() => {
            isReconnecting = false;
            start().catch((error) => {
              emit({ type: "error", error: String(error) });
              process.exit(1);
            });
          }, Math.min(30000, 1500 * 2 ** Math.min(reconnectAttempts++, 5)) + Math.floor(Math.random() * 500)).unref();
        }
      }
    }
    if (connection === "open") {
      if (currentSock !== sock) return;
      connected = true;
      reconnectAttempts = 0;
      isReconnecting = false;
      emit({ type: "ready" });
    }
  });

  sock.ev.on("messages.upsert", async ({ messages }) => {
    for (const message of messages) {
      if (message.message && message.key?.id) {
        rememberMsg(message.key, message.message);
      }
      if (!message.message) continue;
      const messageId = message.key?.id;
      if (messageId && processedInboundIds.has(messageId)) continue;

      const ownJids = [state.creds.me?.id, sock.user?.id, state.creds.me?.lid].filter(Boolean);
      const knownSentIds = { has: (id) => sentMessageIds.has(id) || retryStore.hasId(id) };
      const route = routeInbound(message, ownJids, knownSentIds, process.env.WHATSAPP_QR_GROUP_MODE || "addressed");
      if (!route) continue;
      if (!message.key.fromMe) sock.readMessages([message.key]).catch(() => {});
      const { sender, chat, group, content: body, text: content } = route;
      if (messageId) rememberInbound(messageId);
      emit({ type: "message_received", from: sender, jid: chat });
      const media = [];
      let attachmentError = false;
      const mediaNode = body.imageMessage || body.videoMessage || body.audioMessage || body.documentMessage;
      if (mediaNode) {
        try {
          const buffer = await downloadMediaMessage(message, "buffer", {});
          const filename = mediaNode.fileName || `${message.key.id}.bin`;
          const target = path.join(inboundDir, `${Date.now()}-${filename.replace(/[^A-Za-z0-9._-]/g, "_")}`);
          fs.writeFileSync(target, buffer);
          media.push({ kind: mediaNode.mimetype?.split("/")[0] || "file", filename, content_type: mediaNode.mimetype, external_id: message.key.id, local_path: target, size_bytes: buffer.length });
        } catch { attachmentError = true; }
      }
      emit({ type: "message", id: message.key.id, from: sender, chat_jid: chat, is_group: group, addressed: route.addressed, text: content || (attachmentError ? "My attachment could not be downloaded. Please ask me to resend it." : ""), media });
    }
  });
};

start().catch((error) => { emit({ type: "error", error: String(error) }); process.exitCode = 1; });

