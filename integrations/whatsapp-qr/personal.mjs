// A linked device for one owner's "Message yourself" conversation.
import fs from "node:fs";
import path from "node:path";
import { createHash } from "node:crypto";
import mime from "mime-types";
import QRCode from "qrcode-terminal/vendor/QRCode/index.js";
import logger from "pino";
import makeWASocket, {
  BufferJSON,
  proto,
  DisconnectReason,
  generateWAMessage,
  downloadMediaMessage,
  normalizeMessageContent,
  jidNormalizedUser,
  useMultiFileAuthState,
} from "baileys-personal";
import { whatsappMedia } from "../channels/whatsapp.mjs";
import { attachment } from "../personal/results.mjs";
import { ReliableSender } from "./delivery.mjs";
import { RetryMessageStore } from "./retry-store.mjs";

const legacyReplyPrefix = "Kageha Personal · ";

export function selfInput(message, identities, sent) {
  const { key, message: body } = message;
  if (
    !key?.id ||
    !key.fromMe ||
    sent.hasId(key.id) ||
    !identities
      .filter(Boolean)
      .some(
        (jid) => jidNormalizedUser(jid) === jidNormalizedUser(key.remoteJid),
      )
  )
    return null;
  const content = normalizeMessageContent(body);
  const media = content?.documentMessage || content?.imageMessage || content?.videoMessage || content?.audioMessage;
  const document = media && !media.viewOnce ? media : null;
  const text = document?.caption ?? (document ? "" : content?.conversation || content?.extendedTextMessage?.text);
  if (typeof text !== "string" || text.startsWith(legacyReplyPrefix)) return null;
  const instruction = text.trim().replace(/^Kageha:\s*/i, "").trim();
  const quotedReceipt = content?.extendedTextMessage?.contextInfo?.stanzaId;
  return document || instruction ? {text: instruction, document, quotedReceipt} : null;
}

export function selfCommand(message, identities, sent) {
  return selfInput(message, identities, sent)?.text || null;
}

export async function readDocument(header, download) {
  const limit = 16 * 1024 * 1024; // Matches the native chat upload API.
  const tooLarge = () => Object.assign(Error("File exceeds 16 MiB"), {code:"FILE_TOO_LARGE"});
  if (Number(header.fileLength) > limit) throw tooLarge();
  const timeout = AbortSignal.timeout(60000);
  let stream;
  let abort;
  const deadline = new Promise((_, reject) => {
    abort = () => {
      stream?.destroy?.();
      reject(timeout.reason);
    };
    timeout.addEventListener("abort", abort, { once: true });
  });
  async function read() {
    stream = await download();
    if (timeout.aborted) {
      stream.destroy?.();
      throw timeout.reason;
    }
    const chunks = [];
    let size = 0;
    for await (const chunk of stream) {
      size += chunk.length;
      if (size > limit) throw tooLarge();
      chunks.push(chunk);
    }
    if (!size) throw Error("Empty document");
    const bytes = Buffer.concat(chunks, size);
    if (header.fileSha256 && !createHash("sha256").update(bytes).digest().equals(Buffer.from(header.fileSha256))) {
      throw Error("Attachment checksum mismatch");
    }
    const mimeType = header.mimetype || "application/octet-stream";
    const filename = header.fileName || "attachment." + (mime.extension(mimeType) || "bin");
    const name = path.basename(filename.replaceAll("\\", "/")).replace(/[\x00-\x1f]/g, "").slice(0, 200) || "document";
    return { name, mime: mimeType, bytes };
  }
  try {
    return await Promise.race([read(), deadline]);
  } finally {
    timeout.removeEventListener("abort", abort);
  }
}

// WhatsApp may ask for the same message under a device, phone, or linked identity.
export function savedSelfMessage(store, key, identities) {
  const peers = identities.filter(Boolean).map(jidNormalizedUser);
  if (!key?.id || !peers.includes(jidNormalizedUser(key.remoteJid)))
    return undefined;
  for (const remoteJid of peers) {
    const message = store.get({ remoteJid, id: key.id });
    if (message) return message;
  }
}

function pairingPage(dir, label, value) {
  let image = "";
  if (value) {
    const qr = new QRCode(-1, 1);
    qr.addData(value);
    qr.make();
    const n = qr.getModuleCount(),
      pixels = [];
    for (let y = 0; y < n; y++)
      for (let x = 0; x < n; x++)
        if (qr.isDark(y, x)) pixels.push("M" + x + " " + y + "h1v1h-1z");
    image =
      '<svg width="360" height="360" viewBox="-4 -4 ' +
      (n + 8) +
      " " +
      (n + 8) +
      '" shape-rendering="crispEdges"><path d="' +
      pixels.join("") +
      '"/></svg>';
  }
  const file = path.join(dir, "pair.html");
  fs.writeFileSync(
    file + ".tmp",
    '<!doctype html><meta charset="utf-8"><meta http-equiv="refresh" content="5"><title>Kageha Personal · WhatsApp</title><main style="max-width:42rem;margin:6vh auto;font:18px system-ui"><h1>WhatsApp for Kageha Personal</h1><p>' +
      label +
      "</p>" +
      image +
      "<p>In Message yourself, send <b>hi</b> or <b>status</b>.</p></main>",
    { mode: 0o600 },
  );
  fs.renameSync(file + ".tmp", file);
}

// An initial acknowledgement represents the brief window while the assistant
// prepares its first reply. A background job can keep working after that.
export function replyPresence(db, socket, jid) {
  const pendingText = db.prepare(`
    SELECT 1 FROM outbox AS o WHERE channel='whatsapp' AND (
      (state='scheduled' AND id LIKE 'task-ack:%') OR
      (state='pending' AND retry=0 AND NOT EXISTS
        (SELECT 1 FROM result_delivery d WHERE d.id=o.id))
    ) LIMIT 1`);
  let lastComposing;
  return (now = Date.now()) => {
    const composing = Boolean(pendingText.get());
    if (composing) {
      if (lastComposing !== undefined && now - lastComposing < 4000) return;
      lastComposing = now;
    } else {
      if (lastComposing === undefined) return;
      lastComposing = undefined;
    }
    // Presence is best effort and must never delay or retry the actual message.
    void socket.sendPresenceUpdate(composing ? "composing" : "paused", jid).catch(() => {});
  };
}

export function linkedWhatsApp(db, dir, receive, createSocket = makeWASocket) {
  fs.mkdirSync(dir, { recursive: true, mode: 0o700 });
  const log = logger({ level: "silent" }),
    store = new RetryMessageStore(dir, BufferJSON);
  // A self-chat marks both people’s messages and our replies as fromMe. Keep
  // recognizing our protocol IDs even after the short resend cache expires.
  const sentReceipt = db.prepare(
    "SELECT 1 FROM outbox WHERE channel='whatsapp' AND receipt=? LIMIT 1",
  );
  const sent = { hasId: (id) => store.hasId(id) || !!sentReceipt.get(id) };
  const sender = new ReliableSender(store, (sock, jid, payload, messageId) =>
    generateWAMessage(jid, payload, {
      userJid: sock.user.id,
      upload: sock.waUploadToServer,
      logger: log,
      messageId,
    }),
  );
  let receiving = Promise.resolve();
  let socket,
    presence,
    owner,
    retry,
    closed = false,
    busy = false,
    failures = 0;
  const setState = (status) =>
    db
      .prepare("INSERT OR REPLACE INTO meta VALUES('whatsapp-link',?)")
      .run(status);
  const waiting = "Waiting for WhatsApp to issue a linking code.";
  async function connect() {
    if (closed) return;
    const { state, saveCreds } = await useMultiFileAuthState(dir);
    if (closed) return;
    setState("connecting");
    pairingPage(dir, waiting);
    const sock = createSocket({
      auth: state,
      logger: log,
      markOnlineOnConnect: false,
      syncFullHistory: false,
      getMessage: async (key) =>
        savedSelfMessage(store, key, [state.creds.me?.id, state.creds.me?.lid]),
    });
    socket = sock;
    sock.ev.on("creds.update", saveCreds);
    sock.ev.on("connection.update", ({ connection, qr, lastDisconnect }) => {
      if (closed || socket !== sock) return;
      if (qr) {
        setState("awaiting_scan");
        pairingPage(
          dir,
          "On your phone, open WhatsApp → Linked devices → Link a device and scan this code.",
          qr,
        );
      }
      if (connection === "open") {
        const id = jidNormalizedUser(sock.user?.id);
        const saved = db
          .prepare("SELECT value FROM meta WHERE id='whatsapp-owner'")
          .get();
        if (!id?.endsWith("@s.whatsapp.net") || (saved && saved.value !== id)) {
          closed = true;
          sock.end(undefined);
          setState("wrong_account");
          pairingPage(
            dir,
            "The linked account differs from the enrolled owner.",
          );
          return;
        }
        db.prepare("INSERT OR IGNORE INTO meta VALUES('whatsapp-owner',?)").run(
          id,
        );
        owner = id;
        presence = replyPresence(db, sock, owner);
        failures = 0;
        setState("connected");
        pairingPage(dir, "Connected to your WhatsApp account.");
      }
      if (connection === "close") {
        presence = undefined;
        owner = null;
        socket = null;
        if (
          lastDisconnect?.error?.output?.statusCode ===
          DisconnectReason.loggedOut
        ) {
          setState("logged_out");
          pairingPage(
            dir,
            "WhatsApp unlinked this device. Start a new pairing to reconnect.",
          );
          return;
        }
        setState("reconnecting");
        pairingPage(dir, waiting);
        retry = setTimeout(
          start,
          Math.min(30000, 1000 * 2 ** Math.min(failures++, 5)),
        );
      }
    });
    sock.ev.on("messages.update", (updates) => {
      for (const { key, update } of updates) {
        if (!key.fromMe || !store.hasId(key.id)) continue;
        const status = update.status;
        if (status >= proto.WebMessageInfo.Status.DELIVERY_ACK) {
          db.prepare(
            "UPDATE outbox SET state=CASE WHEN state='read' THEN state ELSE ? END WHERE channel='whatsapp' AND receipt=?",
          ).run(
            status >= proto.WebMessageInfo.Status.READ ? "read" : "delivered",
            key.id,
          );
        }
      }
    });
    // Process inbound notifications in order: a text sent immediately after a
    // PDF must see that attachment even if its download is still in flight.
    sock.ev.on("messages.upsert", ({ messages, type }) => {
      if (!owner || socket !== sock || type !== "notify") return;
      for (const m of messages) {
        const input = selfInput(m,[owner,sock.user?.lid,state.creds.me?.lid],sent);
        if (!input || input.text.length > 16000) continue;
        const id = "wa-self:" + m.key.id;
        receiving = receiving.then(async () => {
          if (db.prepare("SELECT 1 FROM inbox WHERE id=?").get(id)) return;
          let document;
          if (input.document) {
            try {
              document = await readDocument(input.document, () => downloadMediaMessage(m,"stream",{},
                {logger:log,reuploadRequest:sock.updateMediaMessage}));
            } catch (error) {
              const text = error.code === "FILE_TOO_LARGE" ? "That file is too large to open here. Can you send a copy under 16 MiB?" : "I couldn’t open that attachment. Could you resend the file?";
              db.prepare("INSERT OR IGNORE INTO outbox(id,channel,text,created) VALUES(?,'whatsapp',?,?)").run("download:"+id,text,Date.now());
              return;
            }
          }
          receive(db,{id,channel:"whatsapp",text:input.text,document,quotedReceipt:input.quotedReceipt,messageTime:Number(m.messageTimestamp)*1000});
          presence?.();
        }).catch(() => log.error("Could not record incoming WhatsApp message"));
      }
    });
  }
  function start() {
    connect().catch(() => {
      if (!closed) {
        setState("reconnecting");
        retry = setTimeout(start, 5000);
      }
    });
  }
  start();
  const timer = setInterval(async () => {
    if (!owner || closed) return;
    presence?.();
    if (busy) return;
    const row = db
      .prepare(
        "SELECT id,text,state,receipt FROM outbox WHERE channel='whatsapp' AND state IN ('pending','redeliver') AND retry<=? ORDER BY rowid LIMIT 1",
      )
      .get(Date.now());
    if (!row) return;
    busy = true;
    try {
      db.prepare("DELETE FROM meta WHERE id='wa-reply-prompt'").run();
      if (row.state === "redeliver") {
        const message = savedSelfMessage(
          store,
          { remoteJid: owner, id: row.receipt },
          [owner, socket.user?.lid],
        );
        if (!message) throw Error("Original WhatsApp message is unavailable");
        await socket.relayMessage(owner, message, {
          messageId: row.receipt,
          useUserDevicesCache: false,
        });
        db.prepare(
          "UPDATE outbox SET state='sent' WHERE id=? AND state='redeliver'",
        ).run(row.id);
      } else {
        const file = attachment(db,row.id);
        const payload = file ? whatsappMedia(Buffer.from(file.bytes),file.name,file.mime) : {text:row.text};
        const msg = await sender.send(
          socket,
          owner,
          payload,
          row.id,
          file ? "attachment" : "text",
        );
        db.prepare(
          "UPDATE outbox SET state='sent',receipt=? WHERE id=? AND state='pending'",
        ).run(msg.key.id, row.id);
        if (row.id.startsWith("decision:")) {
          db.prepare("INSERT OR REPLACE INTO meta VALUES('wa-reply-prompt',?)")
            .run(JSON.stringify({receipt:msg.key.id, sentAt:Date.now()}));
        }
      }
    } catch {
      db.prepare("UPDATE outbox SET retry=? WHERE id=?").run(
        Date.now() + 30000,
        row.id,
      );
    } finally {
      busy = false;
      if (!closed) presence?.();
    }
  }, 500);
  return () => {
    closed = true;
    clearTimeout(retry);
    clearInterval(timer);
    socket?.end(undefined);
  };
}
