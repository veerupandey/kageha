import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import http from "node:http";
import { EventEmitter, once } from "node:events";
import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import twilio from "twilio";
import { initActions, action } from "./actions.mjs";
import { journal, receive, runDue, flushDigest, notice } from "./assistant.mjs";
import { attachment,pageServer,queueResults } from "./results.mjs";
import { whatsappMedia } from "../channels/whatsapp.mjs";
import { carrier } from "./carrier.mjs";
import { createService } from "./server.mjs";
import { advance, backend, worker } from "./runtime.mjs";
import { credentialFixture } from "../../crates/kageha-loop/tests/credential_fixture.mjs";
import { isolatedModelFixture } from "../../crates/kageha-loop/tests/isolated_model_fixture.mjs";
const cfg = {
  publicUrl: "https://assistant.example",
  sid: "AC" + "1".repeat(32),
  auth: "fixture",
  number: "+15550000000",
  ownerPhone: "+15550000001",
  wa: "whatsapp:+15550000000",
  macOwner: "owner@example.test",
  voicePin: "12345678",
  bridgeToken: "bridge-fixture",
  toolToken: "tool-fixture",
};
const pause = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(fn, ms = 20000) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    const r = await fn();
    if (r) return r;
    await pause(100);
  }
  throw Error("Timed out");
}

test("owner task routing, exact decisions, scheduled follow-up and crash recovery", () => {
  const tmp = fs.mkdtempSync(path.join(os.tmpdir(), "kageha-personal-"));
  try {
    const file = path.join(tmp, "delivery.db");
    let db = journal(file);
    initActions(db);
    const msg = (id, text) => receive(db, { id, channel: "sms", text });
    assert.equal(msg("1", "Find a dentist"), msg("1", "Find a dentist"));
    assert.match(msg("2", "Only after four"), /#1/);
    msg("3", "Find a restaurant");
    assert.match(msg("4", "Only after four"), /Which task/);
    msg("5", "send result via WhatsApp #1");
    db.prepare(
      "UPDATE tasks SET session='test',state='working' WHERE id=1",
    ).run();
    const t = db.prepare("SELECT * FROM tasks WHERE id=1").get();
    const call = {
      id: "a",
      session: "test",
      turn: createHash("sha256")
        .update("test:" + t.request)
        .digest("hex")
        .slice(0, 32),
      action: "personal_call",
      arguments: { phone: "+15550000002", message: "When are you open?" },
    };
    const receipt = action(db, cfg, call);
    assert.equal(action(db, cfg, call).id, receipt.id);
    assert.match(msg("6", "approve #2 " + receipt.id), /No matching/);
    msg("7", "approve #1 " + receipt.id);
    assert.equal(
      db.prepare("SELECT status FROM calls").get().status,
      "authorized",
    );
    const due = Date.now() + 60000;
    action(db, cfg, {
      ...call,
      id: "b",
      action: "personal_follow_up",
      arguments: {
        instruction: "Check the vendor reply",
        due: new Date(due).toISOString(),
      },
    });
    db.prepare("UPDATE outbox SET state='sending' WHERE id='task-ack:1'").run();
    db.close();
    db = journal(file);
    initActions(db);
    runDue(db, due);
    runDue(db, due);
    assert.equal(db.prepare("SELECT COUNT(*) AS n FROM tasks").get().n, 3);
    assert.equal(
      db.prepare("SELECT state FROM outbox WHERE id='task-ack:1'").get().state,
      "uncertain",
    );
    assert.equal(
      db.prepare("SELECT channel FROM tasks WHERE id=1").get().channel,
      "whatsapp",
    );
    db.close();
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
});

test("WhatsApp conversations use one bubble for a greeting, a check-in, or a quick answer", async () => {
  const db = journal(":memory:");
  initActions(db);
  try {
    const say = (id, text) => receive(db, { id, channel: "whatsapp", text });
    assert.equal(say("a", "hi"), "Hey 👋 What’s on your mind?");
    assert.equal(say("a", "hi"), "Hey 👋 What’s on your mind?");
    assert.equal(say("b", "There?"), "Yep, I’m here.");
    assert.equal(db.prepare("SELECT count(*) n FROM tasks").get().n, 0);
    assert.equal(db.prepare("SELECT count(*) n FROM outbox").get().n, 2);
    const answer = "For **two** cups, use `**literal**`.";
    const done = async () => ({
      events: [
        { kind: "done", payload: { status: "success", message: answer } },
      ],
    });
    say("c", "Hey, how much coffee should I grind?");
    db.prepare(
      "UPDATE tasks SET session='chat',turn='turn',state='working' WHERE id=1",
    ).run();
    await advance(db, db.prepare("SELECT * FROM tasks WHERE id=1").get(), done);
    assert.equal(
      db.prepare("SELECT text FROM outbox WHERE id='result:1'").get().text,
      "For *two* cups, use `**literal**`.",
    );
    assert.equal(db.prepare("SELECT count(*) n FROM outbox").get().n, 3);
    say("d", "Find a repair shop");
    db.prepare(
      "UPDATE tasks SET session='chat2',turn='slow-turn',state='working' WHERE id=2",
    ).run();
    db.prepare("UPDATE outbox SET retry=0 WHERE id='task-ack:2'").run();
    worker(db, done)();
    await until(() =>
      db.prepare("SELECT 1 FROM outbox WHERE id='result:2'").get(),
    );
    assert.equal(
      db
        .prepare(
          "SELECT text FROM outbox WHERE id='task-ack:2' AND state='pending'",
        )
        .get().text,
      "I’m on it.",
    );
    assert.equal(
      db.prepare("SELECT text FROM outbox WHERE id='result:2'").get().text,
      "For *two* cups, use `**literal**`.",
    );
    say("e", "Please ask me a question");
    db.prepare("UPDATE tasks SET decision=?,state='waiting' WHERE id=3").run(
      JSON.stringify({ action: "ask_human", approval_id: "abc" }),
    );
    say("f", "hi");
    assert.equal(
      db.prepare("SELECT kind FROM commands WHERE id='f'").get().kind,
      "answer",
    );
  } finally {
    db.close();
  }
});

test("phone chats use the actual update, ask one question and deliver the answer once", async () => {
  for (const channel of ["whatsapp", "imessage", "sms"]) {
    const db = journal(":memory:");
    try {
      initActions(db);
      const say = (id, text) => receive(db, {id,channel,text});
      const task = () => db.prepare("SELECT * FROM tasks").get();
      const bubble = id => db.prepare("SELECT text FROM outbox WHERE id=?").get(id)?.text;
      say("hello","hey");
      assert.match(bubble("ack:hello"), /Hey/);
      assert.equal(task(), undefined);
      say("job","Update my résumé");
      db.exec("UPDATE tasks SET session='chat',turn='turn',state='working'");
      const text = "I’m reading the résumé you sent.";
      await advance(db,task(),async () => ({events:[{kind:"assistant_narrative",payload:{text}}]}));
      assert.equal(bubble("task-ack:1"),text);
      assert.equal(say("check","Are you there?"),text);
      const pending = {action:"ask_human",approval_id:"pick",question:"Which city should I use?"};
      const api = async route => route.includes("/questions/") ? {} : {events:[],live:{pending_approval:pending}};
      await advance(db,task(),api);
      assert.equal(bubble("decision:1:pick"),pending.question);
      say("city","Vancouver");
      worker(db,api)();
      await until(() => db.prepare("SELECT 1 FROM commands WHERE state='done'").get());
      assert.equal(bubble("ack:city"),undefined);
      assert.equal(bubble("applied:city"),undefined);
      await advance(db,task(),async () => ({events:[{kind:"done",payload:{status:"success",message:"Your **draft** is ready."}}]}));
      assert.equal(bubble("result:1"),channel === "whatsapp" ? "Your *draft* is ready." : "Your draft is ready.");
    } finally { db.close(); }
  }
});

test("WhatsApp attention pings never start or steer work, even after an earlier job", () => {
  const db = journal(":memory:");
  try {
    initActions(db);
    const say = (id, text) => receive(db, { id, channel: "whatsapp", text });
    say("old", "Prepare the application draft");
    db.exec("UPDATE tasks SET session='draft',state='success'");
    for (const [id, text] of [["ping", "Oye"], ["again", "??"], ["again", "??"]]) {
      assert.equal(say(id, text), "I’m here 👋 What’s up?");
    }
    assert.equal(db.prepare("SELECT count(*) n FROM tasks").get().n, 1);
    assert.equal(db.prepare("SELECT count(*) n FROM outbox WHERE id LIKE 'ack:%'").get().n, 2);
    say("next", "Oye, find the document");
    assert.match(say("queued", "??"), /queued/);
    assert.equal(db.prepare("SELECT id FROM outbox WHERE id='task-ack:2'").get(), undefined);
    db.exec("UPDATE tasks SET state='working' WHERE id=2");
    assert.match(say("working", "?"), /working/);
    db.prepare("UPDATE tasks SET state='waiting',decision=? WHERE id=2")
      .run(JSON.stringify({action: "ask_human", question: "Which document?"}));
    assert.equal(say("waiting", "??"), "Which document?");
    assert.equal(db.prepare("SELECT count(*) n FROM commands").get().n, 0);
    assert.equal(db.prepare("SELECT count(*) n FROM tasks").get().n, 2);
  } finally {
    db.close();
  }
});

test("signed SMS and WhatsApp, paired Mac and PIN-authenticated calls", async () => {
  const db = journal(":memory:");
  const server = createService(cfg, db, {
    api: async () => {
      throw Error("fixture offline");
    },
    client: { messages: { create: async () => ({ sid: "test" }) } },
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const base = "http://127.0.0.1:" + server.address().port;
  const post = (route, p, signature) =>
    fetch(base + route, {
      method: "POST",
      headers: {
        "Content-Type": "application/x-www-form-urlencoded",
        "X-Twilio-Signature":
          signature ??
          twilio.getExpectedTwilioSignature(cfg.auth, cfg.publicUrl + route, p),
      },
      body: new URLSearchParams(p),
    });
  try {
    const p = {
      AccountSid: cfg.sid,
      From: cfg.ownerPhone,
      To: cfg.number,
      MessageSid: "SMfixture",
      Body: "Find a dentist",
    };
    assert.equal((await post("/carrier/message", p, "invalid")).status, 403);
    assert.equal(
      (await post("/carrier/message", { ...p, From: "+15551112222" })).status,
      403,
    );
    assert.equal((await post("/carrier/message", p)).status, 200);
    await post("/carrier/message", p);
    assert.equal(db.prepare("SELECT count(*) AS n FROM tasks").get().n, 1);
    const wa = {
      ...p,
      From: "whatsapp:" + cfg.ownerPhone,
      To: cfg.wa,
      MessageSid: "SMfixture2",
      Body: "Only after four",
    };
    assert.equal((await post("/carrier/message", wa)).status, 200);
    assert.equal(db.prepare("SELECT task FROM commands").get().task, 1);
    const v = {
      AccountSid: cfg.sid,
      From: cfg.ownerPhone,
      To: cfg.number,
      CallSid: "CAfixture",
    };
    assert.match(await (await post("/carrier/voice", v)).text(), /PIN/);
    assert.match(
      await (
        await post("/carrier/voice", { ...v, Digits: cfg.voicePin })
      ).text(),
      /How can I help/,
    );
    assert.match(
      await (
        await post("/carrier/voice?step=1", {
          ...v,
          SpeechResult: "What is the status?",
        })
      ).text(),
      /dentist/i,
    );
    const bridge = await fetch(base + "/bridge/receive", {
      method: "POST",
      headers: { Authorization: "Bearer " + cfg.bridgeToken },
      body: JSON.stringify({
        sender: cfg.macOwner,
        id: "mac1",
        text: "status",
      }),
    });
    assert.match((await bridge.json()).reply, /dentist/);
  } finally {
    server.close();
    await once(server, "close");
    db.close();
  }
});

test("a vendor call needs the exact owner decision and reports its spoken result; a digest sends once", async () => {
  const db = journal(":memory:");
  initActions(db);
  try {
    const message = (id, text) => receive(db, { id, channel: "sms", text });
    message("start", "Call the repair shop");
    db.prepare(
      "UPDATE tasks SET session='vendor',state='working' WHERE id=1",
    ).run();
    const { request } = db
      .prepare("SELECT request FROM tasks WHERE id=1")
      .get();
    const source = {
      session: "vendor",
      turn: createHash("sha256")
        .update("vendor:" + request)
        .digest("hex")
        .slice(0, 32),
    };
    const phone = "+15550000002",
      question = "When can I collect the repaired bike?";
    const call = action(db, cfg, {
      ...source,
      id: "call",
      action: "personal_call",
      arguments: { phone, message: question },
    });
    const calls = [];
    const transport = carrier(db, cfg, {
      calls: {
        create: async (args) => {
          calls.push(args);
          return { sid: "CAvendor" };
        },
      },
    });
    const read = () =>
      db.prepare("SELECT * FROM calls WHERE id=?").get(call.id);
    await transport.dial(read());
    assert.equal(calls.length, 0);
    message("decision", "approve #1 " + call.id);
    await transport.dial(read());
    await transport.dial(read());
    assert.equal(calls.length, 1);
    const params = {
      AccountSid: cfg.sid,
      From: cfg.number,
      To: phone,
      CallSid: "CAvendor",
    };
    const hook = new URL(calls[0].url),
      end = new URL(calls[0].statusCallback);
    assert.match(
      transport.incoming(hook.pathname, hook, params),
      /automated personal assistant/,
    );
    transport.incoming(hook.pathname, hook, {
      ...params,
      SpeechResult: "Tomorrow after ten.",
    });
    transport.incoming(end.pathname, end, {
      ...params,
      CallStatus: "completed",
    });
    assert.match(
      message("receipt", "status #1"),
      /completed: Tomorrow after ten/,
    );
    action(db, cfg, {
      ...source,
      id: "digest",
      action: "personal_digest",
      arguments: { note: "Shop opens at ten." },
    });
    const now = new Date("2026-06-15T19:00:00Z");
    flushDigest(db, "UTC", 18, now);
    flushDigest(db, "UTC", 18, now);
    assert.equal(
      db
        .prepare("SELECT COUNT(*) AS n FROM outbox WHERE id LIKE 'digest:%'")
        .get().n,
      1,
    );
  } finally {
    db.close();
  }
});

test(
  "real Rust admission stays responsive while another turn is occupied for sixty seconds",
  { skip: !process.env.KAGEHA_LOOP_BIN, timeout: 150000 },
  async () => {
    const temp = fs.mkdtempSync(path.join(os.tmpdir(), "personal-loop-"));
    const db = journal(path.join(temp, "personal.sqlite"));
    const token = "fixture-only-private-token-at-least-32-characters";
    let release,
      entered = false,
      hasTools = false,
      scheduledDigest = false;
    const hold = new Promise((r) => {
      release = r;
    });
    const model = http.createServer(async (req, res) => {
      let buf = "";
      for await (const b of req) buf += b;
      const input = JSON.parse(buf);
      const user = input.messages.findLast((m) => m.role === "user");
      if (input.tools?.length) {
        hasTools ||= input.tools.some(
          (t) => t.function.name === "personal_follow_up",
        );
        if (JSON.stringify(user).includes("dentist fixture") && !entered) {
          entered = true;
          await hold;
        }
      }
      if (
        input.tools?.some((t) => t.function.name === "personal_digest") &&
        JSON.stringify(user).includes("two plus two") &&
        !scheduledDigest
      ) {
        scheduledDigest = true;
        res.writeHead(200, { "Content-Type": "application/json" });
        res.end(
          JSON.stringify({
            choices: [
              {
                message: {
                  role: "assistant",
                  content: null,
                  tool_calls: [
                    {
                      id: "digest-proof",
                      type: "function",
                      function: {
                        name: "personal_digest",
                        arguments: JSON.stringify({
                          note: "Fixture grounded digest",
                        }),
                      },
                    },
                  ],
                },
                finish_reason: "tool_calls",
              },
            ],
          }),
        );
        return;
      }
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(
        JSON.stringify({
          choices: [
            {
              message: {
                role: "assistant",
                content: input.tools?.length ? "Fixture answer." : "{}",
              },
              finish_reason: "stop",
            },
          ],
        }),
      );
    });
    model.listen(0, "127.0.0.1");
    await once(model, "listening");
    let child,
      server,
      logs = "";
    try {
      const work = path.join(temp, "workspace");
      fs.mkdirSync(work);
      fs.mkdirSync(path.join(temp, "legacy"));
      let api;
      server = createService(
        { ...cfg, sid: "", toolToken: token, backendToken: token },
        db,
        { api: (...args) => api(...args) },
      );
      server.listen(0, "127.0.0.1");
      await once(server, "listening");
      const env = {
        ...process.env,
        ...credentialFixture(temp),
        ...isolatedModelFixture(temp),
        KAGEHA_SERVER_TOKEN: token,
        KAGEHA_LEGACY_SESSIONS_DIR: path.join(temp, "legacy"),
        KAGEHA_PROVIDER_URL:
          "http://127.0.0.1:" + model.address().port + "/v1/chat/completions",
        KAGEHA_MODEL: "personal-fixture",
        KAGEHA_PERSONAL_GATEWAY: "http://127.0.0.1:" + server.address().port,
        KAGEHA_PERSONAL_TOKEN: token,
      };
      child = spawn(
        process.env.KAGEHA_LOOP_BIN,
        ["serve", work, path.join(temp, "state"), "127.0.0.1:0"],
        { env, stdio: ["ignore", "pipe", "pipe"] },
      );
      for (const file of [child.stdout, child.stderr])
        file.on("data", (b) => {
          logs += b;
        });
      const host = await until(() => logs.match(/listening on ([^\s]+)/)?.[1]);
      api = backend("http://" + host, token);
      let now = performance.now();
      receive(db, {
        id: "local1",
        channel: "imessage",
        text: "dentist fixture",
      });
      assert(performance.now() - now < 1000);
      await until(() => entered);
      await until(
        () => db.prepare("SELECT turn FROM tasks WHERE id=1").get().turn,
      );
      now = performance.now();
      assert.match(
        receive(db, { id: "voice", channel: "voice", text: "status" }, true),
        /dentist fixture/,
      );
      assert(performance.now() - now < 2000);
      receive(db, { id: "change", channel: "whatsapp", text: "Only after 4" });
      receive(db, {
        id: "parallel",
        channel: "sms",
        text: "What is two plus two?",
      });
      await until(() =>
        db.prepare("SELECT 1 FROM outbox WHERE id='result:2'").get(),
      );
      await until(() =>
        db
          .prepare("SELECT 1 FROM commands WHERE id='change' AND state='done'")
          .get(),
      );
      assert(
        db
          .prepare("SELECT 1 FROM action_receipts WHERE result LIKE '%queued%'")
          .get(),
        "Rust must actually invoke the personal companion",
      );
      const pdf = Buffer.from("%PDF-1.4\nRésumé attachment fixture\n%%EOF");
      receive(db, {id: "resume-test", channel: "whatsapp", text: "Read this résumé", document: {
        name: "resume.pdf", mime: "application/pdf", bytes: pdf,
      }});
      await until(() => db.prepare("SELECT 1 FROM outbox WHERE id='result:3'").get());
      const uploaded = db.prepare("SELECT session,path FROM incoming_files WHERE id='resume-test'").get();
      assert.ok(uploaded.path.startsWith("inputs/"));
      assert.deepEqual(await api.file("/api/sessions/" + uploaded.session + "/files/" + uploaded.path), pdf);
      await pause(60000);
      release();
      await until(() =>
        db.prepare("SELECT 1 FROM outbox WHERE id='result:1'").get(),
      );
      assert(
        hasTools,
        "Personal tools must be offered on the ordinary Rust turn",
      );
    } catch (e) {
      console.error(logs.slice(-800));
      throw e;
    } finally {
      release();
      if (server) {
        server.close();
        await once(server, "close");
      }
      if (child && child.exitCode === null) {
        const exit = once(child, "exit");
        child.kill();
        await exit;
      }
      model.close();
      await once(model, "close");
      db.close();
      fs.rmSync(temp, { recursive: true, force: true });
    }
  },
);

test("plain WhatsApp replies answer only the last delivered question, including with two tasks waiting", async () => {
  const { linkedWhatsApp } = await import("../whatsapp-qr/personal.mjs");
  const db = journal(":memory:");
  const dir = fs.mkdtempSync(path.join(os.tmpdir(),"wa-decision-"));
  const ev = new EventEmitter();
  const owner = "15550000000@s.whatsapp.net";
  const stop = linkedWhatsApp(db,dir,receive,() => {
    setImmediate(() => ev.emit("connection.update",{connection:"open"}));
    return {ev,user:{id:owner},sendPresenceUpdate:async()=>{},relayMessage:async()=>{},end(){}};
  });
  const prompt = (id, action) => {
    const pending = {approval_id:"key-"+id,action,detail:action==="browser_fill"
      ? "browser_fill on https://example.org using observed control First Name"
      : "Copy resume.pdf from an earlier chat into this chat"};
    db.prepare("INSERT INTO tasks(id,text,channel,state,decision) VALUES(?,?,'whatsapp','waiting',?)").run(id,"Apply",JSON.stringify(pending));
    notice(db,"decision:"+id+":"+pending.approval_id,"whatsapp",pending.detail+"\nReply yes or no.");
  };
  const send = (id,text,time) => ev.emit("messages.upsert",{type:"notify",messages:[{
    key:{fromMe:true,remoteJid:owner,id},message:{conversation:text},messageTimestamp:Math.floor(time/1000)
  }]});
  const focus = () => JSON.parse(db.prepare("SELECT value FROM meta WHERE id='wa-reply-prompt'").get()?.value || "null");
  try {
    prompt(55,"browser_fill");
    prompt(56,"attach_past_file");
    await until(() => db.prepare("SELECT receipt FROM outbox WHERE id='decision:56:key-56'").get().receipt);
    send("out-of-order","Yes",focus().sentAt-1000);
    await until(() => db.prepare("SELECT receipt FROM outbox WHERE id LIKE '%:confirm-%'").get()?.receipt);
    assert.equal(db.prepare("SELECT count(*) n FROM commands").get().n,0);
    const sentAt = focus().sentAt + 1000;
    send("plain","Yes",sentAt);
    send("plain","Yes",sentAt);
    await until(() => db.prepare("SELECT 1 FROM commands").get());
    const [cmd] = db.prepare("SELECT task,text,decision FROM commands").all();
    assert.equal(cmd.task,56);
    assert.equal(cmd.text,"approve key-56");
    assert.equal(JSON.parse(cmd.decision).action,"attach_past_file");
    send("repeat","Yes",sentAt+1000);
    await until(() => db.prepare("SELECT 1 FROM inbox WHERE id='wa-self:repeat'").get());
    assert.equal(db.prepare("SELECT count(*) n FROM commands").get().n,1);
    prompt(57,"browser_fill");
    await until(() => db.prepare("SELECT receipt FROM outbox WHERE id='decision:57:key-57'").get()?.receipt);
    send("decline","No",focus().sentAt+1000);
    await until(() => db.prepare("SELECT 1 FROM commands WHERE task=57").get());
    assert.equal(db.prepare("SELECT text FROM commands WHERE task=57").get().text,"deny key-57");
    assert.equal(db.prepare("SELECT count(*) n FROM commands WHERE task=55").get().n,0);
  } finally {
    stop(); db.close(); fs.rmSync(dir,{recursive:true,force:true});
  }
});

test("a quoted WhatsApp decision is bound to the exact delivered approval", async () => {
  const db = journal(":memory:");
  try {
    const {selfInput} = await import("../whatsapp-qr/personal.mjs");
    const owner = "15555550100@s.whatsapp.net";
    receive(db, {id:"start", channel:"whatsapp", text:"Prepare a draft"});
    db.exec("UPDATE tasks SET session='chat', turn='turn', state='working'");
    const pending = {action:"browser_fill", approval_id:"original", detail:"browser_fill on https://example.org/jobs/1 using observed control First Name"};
    const decisions = [];
    const api = async (url, data) => {
      if (url === "/api/approvals") {
        decisions.push(data);
        return {approved:data.approved};
      }
      return {events:[], live:{pending_approval:pending}};
    };
    const cfg = {whatsappSelfChat:true};
    await advance(db, db.prepare("SELECT * FROM tasks").get(), api, null, cfg);
    const bubble = db.prepare("SELECT id,text FROM outbox WHERE id LIKE 'decision:%'").get();
    assert.match(bubble.text, /May I fill in “First Name”/);
    assert.doesNotMatch(bubble.text, /original|#1/);
    db.prepare("UPDATE outbox SET state='sent',receipt='message-id' WHERE id=?").run(bubble.id);
    assert.match(receive(db,{id:"wa-self:plain",channel:"whatsapp",text:"yes"}), /First Name[\s\S]+Reply yes or no/);
    assert.equal(db.prepare("SELECT count(*) n FROM tasks").get().n, 1);
    assert.equal(db.prepare("SELECT count(*) n FROM commands").get().n, 0);
    const reply = (id, text) => receive(db, {id,channel:"whatsapp",...selfInput({
      key:{id,remoteJid:owner,fromMe:true},
      message:{extendedTextMessage:{text, contextInfo:{stanzaId:"message-id"}}}
    }, [owner], {hasId:()=>false})});
    reply("owner-answer","Yes");
    reply("owner-answer","Yes");
    worker(db,api,null,cfg)();
    await until(() => db.prepare("SELECT 1 FROM commands WHERE state='done'").get());
    assert.deepEqual(decisions,[{approval_id:"original",approved:true,scope:"once"}]);
    pending.approval_id = "later";
    await advance(db,db.prepare("SELECT * FROM tasks").get(),api,null,cfg);
    assert.match(reply("late-reply","No"), /expired/);
    assert.equal(db.prepare("SELECT count(*) n FROM commands").get().n,1);
  } finally { db.close(); }
});

test("a completed turn delivers named files, a readable Markdown preview, an inline image and an isolated web link", async () => {
  const db = journal(":memory:");
  const pages = pageServer(db);
  pages.listen(0, "127.0.0.1");
  await once(pages, "listening");
  const shareOrigin = "http://127.0.0.1:" + pages.address().port;
  try {
    receive(db, { id: "incoming", channel: "whatsapp", text: "Make a brief" });
    db.prepare(
      "UPDATE tasks SET session='session',turn='turn',state='working' WHERE id=1",
    ).run();
    const task = db.prepare("SELECT * FROM tasks WHERE id=1").get();
    const entries = {
      "brief.md": "# Brief\n\n**Ready** [source](https://example.com)",
      "photo.png": "png-bytes",
      "letter.pdf": "pdf-bytes",
      "index.html": "<!doctype html><title>Shared page</title>",
    };
    const artifacts = [
      ...Object.keys(entries).map((n) => "workspace/outputs/" + n),
      "workspace/notes/private.md",
      "workspace/outputs/../private",
      "workspace/outputs/brief.md",
      "workspace/outputs/render.err",
      "workspace/outputs/brief.csv",
    ];
    const api = async () => ({
      events: [
        {
          kind: "done",
          payload: {
            status: "success",
            message: "Your brief and brief.csv",
            turn_artifacts: artifacts,
          },
        },
      ],
    });
    api.file = async (route) => {
      const value = entries[route.split("/").at(-1)];
      if (value === undefined) throw Object.assign(Error("File missing"), {status:404});
      return Buffer.from(value);
    };
    await advance(db, task, api, null, { shareOrigin, whatsappSelfChat: true });
    const rows = db.prepare("SELECT id,text FROM outbox ORDER BY rowid").all();
    assert.ok(!rows.some(r => r.text.includes("render.err")));
    assert.ok(rows.some(r => r.text === "brief.csv is no longer available."));
    const file = (name) => attachment(db, rows.find((r) => r.text === name).id);
    assert.equal(
      whatsappMedia(file("photo.png").bytes, "photo.png", "image/png").image
        .length,
      9,
    );
    assert.equal(
      whatsappMedia(file("letter.pdf").bytes, "letter.pdf", "application/pdf")
        .fileName,
      "letter.pdf",
    );
    assert.ok(
      rows.some(
        (r) =>
          r.text.includes("*Brief*") &&
          r.text.includes("*Ready* source — https://example.com"),
      ),
    );
    assert.equal(db.prepare("SELECT count(*) n FROM result_files").get().n, 4);
    const url = rows
      .find((r) => r.text.startsWith("index.html\n"))
      .text.split("\n")[1];
    const res = await fetch(url);
    assert.match(res.headers.get("Content-Security-Policy"), /sandbox/);
    assert.equal(await res.text(), entries["index.html"]);
    assert.equal((await fetch(shareOrigin + "/internal/health")).status, 404);
    assert.equal((await fetch(url + "x")).status, 404);
    await advance(db, task, api, null, { shareOrigin, whatsappSelfChat: true });
    assert.equal(
      db.prepare("SELECT count(*) n FROM outbox").get().n,
      rows.length,
    );
    receive(db, { id: "again", channel: "imessage", text: "/files #1" });
    worker(
      db,
      async () => {
        throw Error("should use saved bytes");
      },
      null,
      { shareOrigin },
    )();
    await until(() =>
      db.prepare("SELECT 1 FROM result_requests WHERE state='done'").get(),
    );
    assert.ok(
      db
        .prepare(
          "SELECT 1 FROM outbox WHERE channel='imessage' AND text='letter.pdf'",
        )
        .get(),
    );
  } finally {
    pages.close();
    db.close();
  }
});

test("personal channel turns use the configured Rust executor chain", async () => {
  const db = journal(":memory:");
  try {
    receive(db, {id:"channel-model", channel:"whatsapp", text:"Read my résumé"});
    const models = ["cloud-primary", "other-provider"];
    const api = async (route, body) => {
      if (route === "/api/sessions") return {session_id:"personal"};
      if (route.includes("/requests/")) throw Object.assign(Error("missing"), {status:404});
      if (route === "/api/chat/stream") {
        assert.equal(body.model, models[0]);
        assert.deepEqual(body.model_roles.executor, models);
        return {turn_id:"turn"};
      }
      if (route.includes("/events?")) return {events:[], next_after:0};
      return {};
    };
    await advance(db, db.prepare("SELECT * FROM tasks").get(), api, null, {channelModels:models});
    assert.equal(db.prepare("SELECT turn FROM tasks").get().turn, "turn");
  } finally { db.close(); }
});

test("an addressed request reaches the existing agent on the shared Rust backend", async () => {
  const db = journal(":memory:");
  try {
    receive(db, {
      id: "agent",
      channel: "whatsapp",
      text: "/agent sterling Draft a letter",
    });
    const calls = [];
    const shared = async (route, body) => {
      calls.push([route, body]);
      if (route === "/api/agents")
        return { agents: [{ id: "sterling", name: "Sterling" }] };
      if (route === "/api/sessions") return { session_id: "shared" };
      if (route.includes("/requests/"))
        throw Object.assign(Error("missing"), { status: 404 });
      if (route === "/api/chat/stream") return { turn_id: "turn" };
      if (route.includes("/events?"))
        return {
          events: [
            { kind: "done", payload: { status: "success", message: "Ready" } },
          ],
        };
      return {};
    };
    worker(
      db,
      async () => {
        throw Error("wrong backend");
      },
      null,
      {channelModels:["personal-only"]},
      shared,
    )();
    await until(() =>
      db.prepare("SELECT 1 FROM tasks WHERE state='success'").get(),
    );
    assert.equal(
      calls.find((c) => c[0] === "/api/sessions")[1].persona_id,
      "sterling",
    );
    assert.equal(
      calls.find((c) => c[0] === "/api/chat/stream")[1].message,
      "Draft a letter",
    );
    assert.equal(calls.find((c) => c[0] === "/api/chat/stream")[1].model, undefined);
  } finally {
    db.close();
  }
});

test("iMessage retrieves the attachment bytes and carrier WhatsApp sends a media URL", async () => {
  const db = journal(":memory:");
  const input = Buffer.from("office document bytes");
  const document = {
    ref: "workspace/outputs/notes.docx",
    name: "notes.docx",
    mime: "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    hash: createHash("sha256").update(input).digest("hex"),
    bytes: input,
  };
  const profile = { ...cfg, shareOrigin: "https://files.example" };
  const sends = [];
  const client = {
    messages: {
      create: async (args) => {
        sends.push(args);
        return { sid: "test-receipt" };
      },
    },
  };
  const server = createService(profile, db, { api: async () => ({}), client });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  try {
    for (const channel of ["imessage", "whatsapp", "sms"])
      queueResults(
        db,
        1,
        channel,
        [document],
        "example:" + channel,
        profile,
        notice,
      );
    const base = "http://127.0.0.1:" + server.address().port;
    const call = (route, data, token) =>
      fetch(base + "/bridge/" + route, {
        method: "POST",
        headers: {
          Authorization: "Bearer " + token,
          "Content-Type": "application/json",
        },
        body: JSON.stringify(data),
      });
    const { messages } = await (await call("poll", {}, cfg.bridgeToken)).json();
    assert.equal(messages[0].attachment, "notes.docx");
    assert.equal(
      (await call("attachment", { id: messages[0].id }, "wrong")).status,
      403,
    );
    const res = await call(
      "attachment",
      { id: messages[0].id },
      cfg.bridgeToken,
    );
    assert.equal(res.headers.get("Content-Type"), document.mime);
    assert.deepEqual(Buffer.from(await res.arrayBuffer()), input);
    db.prepare("INSERT OR REPLACE INTO meta VALUES('wa-inbound',?)").run(
      String(Date.now()),
    );
    const wa = db
      .prepare("SELECT * FROM outbox WHERE channel='whatsapp'")
      .get();
    await carrier(db, profile, client).deliver(wa);
    const link = wa.text.split("\n")[1];
    assert.deepEqual(sends[0].mediaUrl, [link]);
    assert.equal(
      db
        .prepare("SELECT text FROM outbox WHERE channel='sms'")
        .get()
        .text.split("\n")[1],
      link,
    );
  } finally {
    server.close();
    db.close();
  }
});

test("a WhatsApp file answers a question without numbered receipts", async () => {
  const db = journal(":memory:");
  try {
    receive(db, {id: "job", channel: "whatsapp", text: "Review my résumé"});
    db.exec("UPDATE tasks SET state='working',session='jobs',turn='first'");
    const pending = {action: "ask_human", approval_id: "q1", question: "Which résumé should I use?"};
    let answer;
    const api = async (route, data) => {
      if (route.includes("/events?")) return {events: [], live: {pending_approval: pending}};
      if (route.endsWith("/upload")) return {path: "inputs/resume.pdf"};
      if (route.endsWith("/questions/q1")) { answer = data.answer; return {}; }
      throw Error("Unexpected request " + route);
    };
    await advance(db, db.prepare("SELECT * FROM tasks").get(), api);
    assert.equal(db.prepare("SELECT text FROM outbox WHERE id LIKE 'decision:%'").get().text, pending.question);
    receive(db, {id: "file-answer", channel: "whatsapp", text: "", document: {
      name: "resume.pdf", mime: "application/pdf", bytes: Buffer.from("%PDF-1.7")
    }});
    worker(db, api)();
    await until(() => db.prepare("SELECT state FROM commands").get()?.state === "done");
    assert.match(answer, /inputs\/resume\.pdf/);
    assert.equal(db.prepare("SELECT count(*) AS n FROM outbox WHERE id LIKE 'applied:%' OR id='ack:file-answer'").get().n, 0);
  } finally { db.close(); }
});

test("a WhatsApp attachment arriving as a turn ends continues in the same chat", async () => {
  const db = journal(":memory:");
  try {
    receive(db, {id:"start",channel:"whatsapp",text:"Review the document"});
    db.exec("UPDATE tasks SET state='working',session='review',turn='first'");
    receive(db, {id:"new-file",channel:"whatsapp",text:"Use this version",document:{
      name:"revision.pdf",mime:"application/pdf",bytes:Buffer.from("revision")
    }});
    worker(db, async () => ({events:[{kind:"done",payload:{status:"success",message:"Ready."}}]}))();
    const next = await until(() => db.prepare("SELECT * FROM tasks WHERE id=2").get());
    assert.equal(next.session,"review");
    assert.equal(next.text,"Use this version");
    assert.equal(db.prepare("SELECT task FROM incoming_files").get().task,next.id);
  } finally { db.close(); }
});
