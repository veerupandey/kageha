import twilio from "twilio";
import { createHash, timingSafeEqual } from "node:crypto";
import { receive, atomic, notice } from "./assistant.mjs";

export function same(a, b) {
  return (
    typeof a === "string" &&
    typeof b === "string" &&
    a.length > 0 &&
    b.length > 0 &&
    timingSafeEqual(
      createHash("sha256").update(a).digest(),
      createHash("sha256").update(b).digest(),
    )
  );
}
const value = (db, k) =>
  db.prepare("SELECT value FROM meta WHERE id=?").get(k)?.value;
const put = (db, k, v) =>
  db.prepare("INSERT OR REPLACE INTO meta VALUES(?,?)").run(k, String(v));
export function carrier(db, cfg, client) {
  const url = (path) => cfg.publicUrl + path;
  const empty = () => new twilio.twiml.MessagingResponse().toString();
  const voice = () => new twilio.twiml.VoiceResponse();
  function gather(prompt, path, pin = false) {
    const v = voice();
    const g = v.gather({
      input: pin ? "dtmf" : "speech dtmf",
      action: url(path),
      method: "POST",
      timeout: pin ? 8 : 4,
      ...(pin
        ? { numDigits: cfg.voicePin?.length || 8 }
        : { speechTimeout: "auto", actionOnEmptyResult: true }),
    });
    if (prompt) g.say(prompt);
    return v.toString();
  }
  function incoming(route, u, p) {
    if (p.AccountSid !== cfg.sid)
      throw Object.assign(Error("Unexpected account"), { status: 403 });
    if (route === "/carrier/status") {
      const id = u.searchParams.get("id");
      const state = p.MessageStatus;
      if (id && p.MessageSid)
        db.prepare(
          `UPDATE outbox SET receipt=?,state=CASE WHEN ? IN ('failed','undelivered') THEN 'failed'
        WHEN state='failed' THEN state ELSE 'sent' END WHERE id=? AND state IN ('sending','uncertain','sent','failed')
        AND (receipt IS NULL OR receipt=?)`,
        ).run(p.MessageSid, state, id, p.MessageSid);
      return empty();
    }
    if (route === "/carrier/business" || route === "/carrier/call-status")
      return business(route, u, p);
    const isWa = p.From?.startsWith("whatsapp:");
    if (
      p.From !== (isWa ? "whatsapp:" + cfg.ownerPhone : cfg.ownerPhone) ||
      p.To !== (isWa ? cfg.wa : cfg.number)
    )
      throw Object.assign(Error("Unknown address"), { status: 403 });
    if (route === "/carrier/message") {
      const channel = isWa ? "whatsapp" : "sms";
      if (!p.MessageSid) throw Error("Missing message ID");
      if (isWa)
        atomic(db, () => {
          put(db, "wa-inbound", Date.now());
          db.prepare(
            "UPDATE outbox SET retry=0 WHERE channel='whatsapp' AND state='pending'",
          ).run();
        });
      const media =
        Number(p.NumMedia) > 0 && p.MediaContentType0?.startsWith("audio/")
          ? p.MediaUrl0
          : null;
      receive(db, {
        id: "msg:" + p.MessageSid,
        channel,
        text: p.Body || "",
        media,
      });
      return empty();
    }
    if (route !== "/carrier/voice" || !p.CallSid)
      throw Object.assign(Error("Unknown route"), { status: 404 });
    const key = "caller:" + p.CallSid;
    if (value(db, key) !== "verified") {
      if (!p.Digits)
        return gather("Enter your assistant PIN.", "/carrier/voice", true);
      if (!same(cfg.voicePin, p.Digits)) {
        const v = voice();
        v.hangup();
        return v.toString();
      }
      put(db, key, "verified");
      put(db, "voice-start:" + p.CallSid, Date.now());
      return gather("How can I help?", "/carrier/voice?step=1");
    }
    const step = Number(u.searchParams.get("step") || 1);
    if (!Number.isSafeInteger(step) || step < 1 || step > 10000)
      throw Error("Invalid call step");
    let reply = "";
    if (p.SpeechResult)
      reply = receive(
        db,
        { id: key + ":" + step, channel: "voice", text: p.SpeechResult },
        true,
      );
    else {
      const last = db
        .prepare(
          "SELECT text,id FROM outbox WHERE channel='sms' AND id LIKE 'result:%' AND created>=? ORDER BY rowid DESC LIMIT 1",
        )
        .get(Number(value(db, "voice-start:" + p.CallSid) || Date.now()));
      if (last && value(db, "voice-last:" + p.CallSid) !== last.id) {
        reply = last.text;
        put(db, "voice-last:" + p.CallSid, last.id);
      }
    }
    return gather(reply.slice(0, 1200), "/carrier/voice?step=" + (step + 1));
  }
  function business(route, u, p) {
    const key = u.searchParams.get("job");
    const row = db.prepare("SELECT * FROM calls WHERE id=?").get(key);
    if (
      !row ||
      p.To !== row.phone ||
      p.From !== cfg.number ||
      (row.sid && row.sid !== p.CallSid)
    )
      throw Object.assign(Error("Unknown call"), { status: 403 });
    db.prepare("UPDATE calls SET sid=? WHERE id=?").run(p.CallSid, key);
    const v = voice();
    if (route.endsWith("call-status")) {
      db.prepare("UPDATE calls SET status=? WHERE id=?").run(p.CallStatus, key);
      if (
        ["completed", "busy", "no-answer", "canceled", "failed"].includes(
          p.CallStatus,
        )
      )
        notice(
          db,
          "call-end:" + key,
          row.channel,
          "#" +
            row.task +
            " call " +
            p.CallStatus +
            (row.answer
              ? ". Response: " + row.answer
              : ". No spoken response recorded."),
        );
    } else if (p.SpeechResult) {
      db.prepare("UPDATE calls SET answer=? WHERE id=?").run(
        p.SpeechResult.slice(0, 4000),
        key,
      );
      v.say("Thank you. I will pass that on.");
      v.hangup();
    } else {
      const g = v.gather({
        input: "speech",
        action: url("/carrier/business?job=" + encodeURIComponent(key)),
        speechTimeout: "auto",
        timeout: 8,
      });
      g.say(
        "Hello. I am an automated personal assistant calling on behalf of my owner. " +
          row.message,
      );
    }
    return v.toString();
  }
  async function deliver(row) {
    const outside =
      row.channel === "whatsapp" &&
      Date.now() - Number(value(db, "wa-inbound") || 0) >= 86400000;
    if (outside && !cfg.template) {
      db.prepare(
        "UPDATE outbox SET retry=? WHERE id=? AND state='pending'",
      ).run(Date.now() + 600000, row.id);
      return;
    }
    if (
      !db
        .prepare(
          "UPDATE outbox SET state='sending' WHERE id=? AND state='pending'",
        )
        .run(row.id).changes
    )
      return;
    const to = (row.channel === "whatsapp" ? "whatsapp:" : "") + cfg.ownerPhone;
    try {
      const hasMedia = row.channel === "whatsapp" && db.prepare("SELECT 1 FROM result_delivery WHERE id=?").get(row.id);
      const res = await client.messages.create({
        to,
        from: row.channel === "whatsapp" ? cfg.wa : cfg.number,
        statusCallback: url("/carrier/status?id=" + encodeURIComponent(row.id)),
        ...(outside
          ? {
              contentSid: cfg.template,
              contentVariables: JSON.stringify({ 1: row.text }),
            }
          : hasMedia ? {body:row.text.split("\n")[0],mediaUrl:[row.text.split("\n").at(-1)]} : { body: row.text }),
      });
      db.prepare(
        "UPDATE outbox SET state='sent',receipt=? WHERE id=? AND state='sending'",
      ).run(res.sid, row.id);
    } catch (err) {
      const state =
        err.status === 429
          ? "pending"
          : err.status >= 400 && err.status < 500
            ? "failed"
            : "uncertain";
      db.prepare(
        "UPDATE outbox SET state=?,retry=? WHERE id=? AND state='sending'",
      ).run(state, Date.now() + 30000, row.id);
    }
  }
  async function dial(row) {
    if (
      !db
        .prepare(
          "UPDATE calls SET status='dispatching' WHERE id=? AND status='authorized'",
        )
        .run(row.id).changes
    )
      return;
    try {
      const reply = await client.calls.create({
        to: row.phone,
        from: cfg.number,
        url: url("/carrier/business?job=" + encodeURIComponent(row.id)),
        statusCallback: url(
          "/carrier/call-status?job=" + encodeURIComponent(row.id),
        ),
        statusCallbackEvent: ["completed"],
      });
      db.prepare(
        "UPDATE calls SET sid=?,status='submitted' WHERE id=? AND status='dispatching'",
      ).run(reply.sid, row.id);
    } catch (err) {
      const state =
        err.status >= 400 && err.status < 500 ? "failed" : "uncertain";
      db.prepare(
        "UPDATE calls SET status=? WHERE id=? AND status='dispatching'",
      ).run(state, row.id);
      notice(
        db,
        "dial-error:" + row.id,
        row.channel,
        "#" + row.task + " call dispatch " + state + ".",
      );
    }
  }
  return { incoming, deliver, dial };
}
