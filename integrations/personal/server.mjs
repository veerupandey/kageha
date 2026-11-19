import http from "node:http";
import fs from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";
import twilio from "twilio";
import {
  active,
  atomic,
  flushDigest,
  journal,
  notice,
  receive,
  watch,
} from "./assistant.mjs";
import { action, initActions } from "./actions.mjs";
import { attachment, pageServer } from "./results.mjs";
import { backend, worker } from "./runtime.mjs";
import { carrier, same } from "./carrier.mjs";

async function body(req, limit) {
  let size = 0;
  const parts = [];
  for await (const part of req) {
    size += part.length;
    if (size > limit)
      throw Object.assign(Error("Request too large"), { status: 413 });
    parts.push(part);
  }
  return Buffer.concat(parts).toString("utf8");
}

async function audio(url, cfg) {
  let u = new URL(url);
  if (
    u.origin !== "https://api.twilio.com" ||
    !u.pathname.startsWith("/2010-04-01/Accounts/" + cfg.sid + "/Messages/") ||
    !/\/Media\/ME[a-f0-9]{32}$/i.test(u.pathname)
  )
    throw Error("Unsupported audio media URL");
  let res = await fetch(u, {
    headers: {
      Authorization:
        "Basic " + Buffer.from(cfg.sid + ":" + cfg.auth).toString("base64"),
    },
    redirect: "manual",
    signal: AbortSignal.timeout(15000),
  });
  if ([301, 302, 303, 307, 308].includes(res.status)) {
    u = new URL(res.headers.get("location"), u);
    if (
      u.protocol !== "https:" ||
      u.username ||
      u.password ||
      !/(^|\.)(twiliocdn\.com|twilio\.com)$/.test(u.hostname)
    )
      throw Error("Unexpected media host");
    res = await fetch(u, {
      redirect: "error",
      signal: AbortSignal.timeout(15000),
    });
  }
  if (!res.ok) throw Error("Audio download failed");
  const mime = res.headers.get("content-type")?.split(";")[0] || "";
  if (!mime.startsWith("audio/")) throw Error("Expected an audio note");
  const data = [];
  let n = 0;
  for await (const part of res.body) {
    n += part.length;
    if (n > 8 * 1024 * 1024)
      throw Object.assign(Error("Voice note exceeds 8 MiB"), { status: 400 });
    data.push(part);
  }
  return new Blob(data, { type: mime });
}

export function createService(cfg, db, options = {}) {
  if (cfg.channelModels !== undefined && (
    !Array.isArray(cfg.channelModels) || cfg.channelModels.length > 8 ||
    cfg.channelModels.some(id => typeof id !== "string" || !id.trim() || id !== id.trim() || Buffer.byteLength(id) > 256 || /[\x00-\x1f\x7f]/.test(id))
  )) {
    throw Error("channelModels must be an ordered array of up to eight model IDs");
  }
  initActions(db);
  const api = options.api || backend(cfg.backend, cfg.backendToken);
  const transport = carrier(
    db,
    cfg,
    options.client ||
      (cfg.sid && cfg.auth
        ? twilio(cfg.sid, cfg.auth, { timeout: 9000, autoRetry: false })
        : null),
  );
  const sharedApi = cfg.agentBackend && backend(cfg.agentBackend,fs.readFileSync(cfg.agentTokenFile,"utf8").trim());
  const tick = worker(db, api, async (media, session) => {
    const blob = await audio(media, cfg);
    const form = new FormData();
    form.set(
      "file",
      blob,
      "voice." +
        ({
          "audio/ogg": "ogg",
          "audio/mpeg": "mp3",
          "audio/mp4": "m4a",
          "audio/wav": "wav",
        }[blob.type] || "webm"),
    );
    const result = await api("/api/sessions/" + session + "/stt", form);
    if (!result.text?.trim())
      throw Object.assign(Error("No speech was recognized"), { status: 400 });
    return result.text;
  }, cfg, sharedApi);
  const configuredWatches = [...(cfg.watches || [])];
  if (cfg.monitorInboxAndCalendar)
    configuredWatches.push({
      id: "inbox-calendar",
      minutes: 30,
      channel: cfg.defaultChannel || "local",
      instruction:
        "Review newly changed messages and upcoming events in the connected inbox and calendar. Check open commitments. Report urgent deadlines promptly; queue useful nonurgent changes with personal_digest. Use personal_follow_up for specific outstanding commitments. If no urgent update is needed, return exactly NO_UPDATE.",
    });
  for (const w of configuredWatches) {
    // Only create configured watches once, including after a one-shot fires.
    atomic(db, () => {
      if (
        db
          .prepare("INSERT OR IGNORE INTO meta VALUES(?,?)")
          .run("configured-watch:" + w.id, "1").changes
      )
        watch(
          db,
          "config:" + w.id,
          w.instruction,
          Date.now() + 60000,
          w.minutes * 60000,
          w.channel,
        );
    });
  }
  let sending = false;
  async function dispatch() {
    if (sending || !cfg.sid || !cfg.auth) return;
    sending = true;
    try {
      const rows = db
        .prepare(
          "SELECT * FROM outbox WHERE (channel='sms' OR (channel='whatsapp' AND ?=0)) AND state='pending' AND retry<=? ORDER BY rowid LIMIT 32",
        )
        .all(cfg.whatsappSelfChat ? 1 : 0, Date.now());
      for (const row of rows) await transport.deliver(row);
      for (const row of db
        .prepare(
          "SELECT * FROM calls WHERE status='authorized' ORDER BY rowid LIMIT 4",
        )
        .all())
        await transport.dial(row);
    } finally {
      sending = false;
    }
  }
  const server = http.createServer(async (req, res) => {
    const u = new URL(req.url, "http://localhost"),
      route = u.pathname;
    try {
      if (req.method !== "POST")
        throw Object.assign(Error("POST required"), { status: 405 });
      const auth = req.headers.authorization;
      let out,
        type = "application/json";
      if (route.startsWith("/carrier/")) {
        if (
          !cfg.sid ||
          !cfg.auth ||
          !req.headers["content-type"]?.startsWith(
            "application/x-www-form-urlencoded",
          )
        )
          throw Object.assign(Error("Carrier disabled or invalid body"), {
            status: 403,
          });
        const form = new URLSearchParams(await body(req, 65536));
        const params = Object.fromEntries(form);
        if (
          Object.keys(params).length !== [...form].length ||
          !twilio.validateRequest(
            cfg.auth,
            String(req.headers["x-twilio-signature"] || ""),
            cfg.publicUrl + req.url,
            params,
          )
        )
          throw Object.assign(Error("Invalid carrier signature"), {
            status: 403,
          });
        out = transport.incoming(route, u, params);
        type = "text/xml";
      } else if (route.startsWith("/bridge/")) {
        if (!cfg.bridgeToken || !same(auth, "Bearer " + cfg.bridgeToken))
          throw Object.assign(Error("Unknown bridge"), { status: 403 });
        const data = JSON.parse((await body(req, 65536)) || "{}");
        if (route === "/bridge/attachment") {
          const owned = db
            .prepare(
              "SELECT 1 FROM outbox WHERE id=? AND channel='imessage' AND state='pending'",
            )
            .get(data.id);
          const file = owned && attachment(db, data.id);
          if (!file)
            throw Object.assign(Error("File unavailable"), { status: 404 });
          res.writeHead(200, {
            "Content-Type": file.mime,
            "Content-Length": file.bytes.length,
          });
          res.end(file.bytes);
          return;
        } else if (route === "/bridge/receive") {
          if (typeof data.id !== "string" || !data.id.trim())
            throw Error("Missing message ID");
          if (!cfg.macOwner || data.sender !== cfg.macOwner)
            throw Object.assign(Error("Unknown owner"), { status: 403 });
          out = {
            reply: receive(db, {
              id: "imsg:" + data.id,
              channel: "imessage",
              text: data.text,
            }),
          };
        } else if (route === "/bridge/poll") {
          db.prepare("INSERT OR REPLACE INTO meta VALUES('mac-seen',?)").run(
            String(Date.now()),
          );
          const messages = db
            .prepare(
              "SELECT o.id,o.text,f.name AS attachment FROM outbox o LEFT JOIN result_delivery d ON d.id=o.id LEFT JOIN result_files f ON f.id=d.file WHERE channel='imessage' AND state='pending' ORDER BY o.rowid LIMIT 10",
            )
            .all();
          const files = db
            .prepare(
              "SELECT id,path FROM reveals WHERE status='authorized' ORDER BY rowid LIMIT 4",
            )
            .all();
          out = { messages, files };
        } else if (route === "/bridge/receipt") {
          if (!["sent", "uncertain", "failed"].includes(data.state))
            throw Error("Invalid receipt");
          atomic(db, () => {
            if (data.kind === "message")
              db.prepare(
                "UPDATE outbox SET state=? WHERE channel='imessage' AND id=? AND state='pending'",
              ).run(data.state, data.id);
            else if (data.kind === "file") {
              const r = db
                .prepare(
                  "SELECT task,channel FROM reveals WHERE id=? AND status='authorized'",
                )
                .get(data.id);
              if (r) {
                db.prepare("UPDATE reveals SET status=? WHERE id=?").run(
                  data.state,
                  data.id,
                );
                notice(
                  db,
                  "mac-result:" + data.id,
                  r.channel,
                  "#" +
                    r.task +
                    " Finder reveal " +
                    (data.state === "sent" ? "dispatched" : data.state) +
                    ".",
                );
              }
            } else throw Error("Invalid receipt kind");
          });
          out = { ok: true };
        } else throw Object.assign(Error("Unknown route"), { status: 404 });
      } else if (route.startsWith("/owner/")) {
        if (!same(auth, "Bearer " + cfg.backendToken))
          throw Object.assign(Error("Unauthorized"), { status: 403 });
        const data = JSON.parse((await body(req, 64000)) || "{}");
        if (route === "/owner/message") {
          if (typeof data.id !== "string" || !data.id.trim())
            throw Error("Missing message ID");
          out = {
            reply: receive(db, {
              id: "local:" + data.id,
              channel: "local",
              text: data.text,
            }),
          };
        } else if (route === "/owner/tasks")
          out = {
            tasks: db
              .prepare(
                "SELECT id,text,state,progress,session FROM tasks ORDER BY id DESC LIMIT 30",
              )
              .all(),
          };
        else if (route === "/owner/delivery")
          out = {
            messages: db
              .prepare(
                "SELECT rowid AS cursor,text FROM outbox WHERE channel='local' AND rowid>? ORDER BY rowid LIMIT 100",
              )
              .all(Number(data.after) || 0),
          };
        else throw Object.assign(Error("Unknown route"), { status: 404 });
      } else if (route.startsWith("/internal/")) {
        if (!cfg.toolToken || !same(auth, "Bearer " + cfg.toolToken))
          throw Object.assign(Error("Unauthorized"), { status: 403 });
        const data = JSON.parse((await body(req, 65536)) || "{}");
        if (route === "/internal/action") {
          // A task started in the private WebUI can use the same personal tools.
          if (
            !db
              .prepare(
                "SELECT 1 FROM tasks WHERE session=? AND state IN ('queued','working','waiting')",
              )
              .get(data.session || "")
          ) {
            if (
              !/^[a-z0-9-]{1,80}$/i.test(data.session) ||
              !/^[0-9a-f]{32}$/.test(data.turn)
            )
              throw Error("Unknown task");
            const r = await api(
              "/api/sessions/" +
                data.session +
                "/events?turn_id=" +
                data.turn +
                "&include_live=0&limit=1",
            );
            if (!r.turn?.running || r.turn.turn_id !== data.turn)
              throw Error("Unknown active task");
            db.prepare(
              "INSERT OR IGNORE INTO tasks(request,text,channel,session,turn,state) VALUES(?,?,?,?,?,'working')",
            ).run(
              r.turn.request_id,
              "Task from the Kageha web UI",
              cfg.defaultChannel || "local",
              data.session,
              data.turn,
            );
          }
          out = action(db, cfg, data);
        } else if (route === "/internal/health")
          out = {
            tasks: active(db).length,
            macLastSeen: Number(
              db.prepare("SELECT value FROM meta WHERE id='mac-seen'").get()
                ?.value || 0,
            ),
            delivery: db
              .prepare(
                "SELECT state,count(*) AS count FROM outbox GROUP BY state",
              )
              .all(),
          };
        else throw Object.assign(Error("Unknown route"), { status: 404 });
      } else throw Object.assign(Error("Unknown route"), { status: 404 });
      res.writeHead(200, { "Content-Type": type });
      res.end(type === "text/xml" ? out : JSON.stringify(out));
    } catch (e) {
      res.writeHead(e.status || 400, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ error: e.message }));
    }
  });
  const zone = cfg.timezone || "UTC";
  new Intl.DateTimeFormat("en", { timeZone: zone });
  const timer = setInterval(() => {
    tick();
    flushDigest(db, zone, cfg.digestHour ?? 18);
    void dispatch().catch(() => console.error("Carrier dispatch unavailable"));
  }, 400);
  server.once("close", () => clearInterval(timer));
  return server;
}

if (
  process.argv[1] &&
  pathToFileURL(path.resolve(process.argv[1])).href === import.meta.url
) {
  process.umask(0o077);
  const cfg = JSON.parse(
    fs.readFileSync(process.argv[2] || "personal.json", "utf8"),
  );
  if (
    !cfg.backendToken ||
    !cfg.toolToken ||
    cfg.toolToken.length < 32 ||
    !cfg.bridgeToken ||
    cfg.bridgeToken.length < 32 ||
    cfg.toolToken === cfg.bridgeToken ||
    ((cfg.publicUrl || cfg.sid) && !/^https:\/\/[^/?#]+$/.test(cfg.publicUrl))
  )
    throw Error("Invalid personal setup; see README");
  if (
    cfg.sid &&
    (!/^AC[0-9a-f]{32}$/i.test(cfg.sid) ||
      !cfg.auth ||
      !/^\+[1-9]\d{6,14}$/.test(cfg.number) ||
      !/^\+[1-9]\d{6,14}$/.test(cfg.ownerPhone) ||
      !/^\d{6,12}$/.test(cfg.voicePin))
  )
    throw Error("Incomplete carrier setup");
  const db=journal(cfg.database);
  const server = createService(cfg, db);
  if (cfg.sharePort) {
    if (cfg.shareOrigin && new URL(cfg.shareOrigin).origin !== cfg.shareOrigin)
      throw Error("Use an HTTPS share origin");
    if (cfg.shareOrigin && !cfg.shareOrigin.startsWith("https://"))
      throw Error("Use an HTTPS share origin");
    const pages = pageServer(db);
    pages.listen(cfg.sharePort, "127.0.0.1");
    server.once("close", () => pages.close());
  }
  if(cfg.whatsappSelfChat){
    const {linkedWhatsApp}=await import('../whatsapp-qr/personal.mjs');
    server.once('close',linkedWhatsApp(db,path.join(path.dirname(cfg.database),'whatsapp-device'),receive));
  }
  server.listen(cfg.port || 8791, "127.0.0.1", () =>
    console.log("Kageha Personal listening on loopback."),
  );
}
