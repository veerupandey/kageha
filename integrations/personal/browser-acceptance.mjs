// Invoked by webui_browser_e2e.mjs --personal-channels.
import assert from "node:assert/strict";
import { fork } from "node:child_process";
import { EventEmitter, once } from "node:events";
import { DatabaseSync } from "node:sqlite";
import path from "node:path";
import { journal, receive } from "./assistant.mjs";
import { createService } from "./server.mjs";
import { linkedWhatsApp } from "../whatsapp-qr/personal.mjs";

const owner = "15550100100@s.whatsapp.net";
const refusal = "I cannot control that browser.";

if (process.argv[2] === "--gateway-fixture") {
  const [root, backend, backendToken, pauseOn] = process.argv.slice(3);
  const db = journal(path.join(root, "channel.sqlite"));
  const service = createService(
    { backend, backendToken, whatsappSelfChat: true },
    db,
  );
  service.listen(0, "127.0.0.1");
  const ev = new EventEmitter();
  linkedWhatsApp(db, path.join(root, "device"), receive, () => {
    setImmediate(() => {
      ev.emit("connection.update", { connection: "open" });
      process.send({ ready: true });
    });
    return {
      ev,
      user: { id: owner },
      sendPresenceUpdate: async () => {},
      end() {},
      async relayMessage(jid, body, { messageId }) {
        const text = body.conversation ?? body.extendedTextMessage?.text;
        assert.equal(jid, owner);
        if (pauseOn && text.includes(pauseOn)) {
          process.send({ paused: messageId });
          await new Promise(() => {}); // Terminate before the recipient accepts the reply.
        }
        process.send({ jid, text, messageId });
      },
    };
  });
  process.on("message", ({ id, text, quotedReceipt }) =>
    ev.emit("messages.upsert", {
      type: "notify",
      messages: [
        {
          key: { id, fromMe: true, remoteJid: owner },
          message: quotedReceipt
            ? { extendedTextMessage: { text, contextInfo: { stanzaId: quotedReceipt } } }
            : { conversation: text },
        },
      ],
    }),
  );
}

function addHistory(root, chat) {
  const db = new DatabaseSync(path.join(root, "state/native-chats.sqlite"));
  try {
    const insert = db.prepare(
      "INSERT INTO native_chat_turns(chat_id,request_id,message,answer,status) VALUES(?,?,?,?,'completed')",
    );
    for (let i = 0; i < 15; i++)
      insert.run(
        chat,
        "past-" + i,
        "Check saved listing " + i,
        "Checked listing " + i,
      );
    insert.run(chat, "past-browser", "Can you control my browser?", refusal);
  } finally {
    db.close();
  }
}

async function webComposer(endpoint, base, chat, token) {
  const target = await (
    await fetch(endpoint + "/json/new?about:blank", { method: "PUT" })
  ).json();
  const socket = new WebSocket(target.webSocketDebuggerUrl);
  await once(socket, "open");
  let id = 0;
  async function cdp(method, params) {
    const current = ++id;
    const reply = new Promise((resolve, reject) => {
      function onMessage(event) {
        const data = JSON.parse(event.data);
        if (data.id !== current) return;
        socket.removeEventListener("message", onMessage);
        if (data.error) reject(Error(data.error.message));
        else resolve(data.result);
      }
      socket.addEventListener("message", onMessage);
    });
    socket.send(JSON.stringify({ id: current, method, params }));
    return reply;
  }
  async function evaluate(expression) {
    const { result, exceptionDetails } = await cdp("Runtime.evaluate", {
      expression,
      returnByValue: true,
    });
    if (exceptionDetails) throw Error(result.description);
    return result.value;
  }
  await cdp("Network.setCookie", {
    url: base,
    name: "kageha_native_session",
    value: token,
    httpOnly: true,
  });
  await cdp("Page.navigate", { url: base + "/?session=" + chat });
  return {
    evaluate,
    async send(text) {
      await evaluate('document.querySelector("#message-input").focus()');
      await cdp("Input.insertText", { text });
      for (const type of ["keyDown", "keyUp"])
        await cdp("Input.dispatchKeyEvent", {
          type,
          key: "Enter",
          code: "Enter",
          windowsVirtualKeyCode: 13,
        });
    },
    close: () => socket.close(),
  };
}

export async function testPersonalBrowser({
  root,
  base,
  token,
  fixture,
  pageTitle,
  visits,
  endpoints,
  ok,
  until,
}) {
  const messages = [];
  let child, paused;
  async function startGateway(pauseOn = "") {
    let ready = false;
    child = fork(new URL(import.meta.url), [
      "--gateway-fixture",
      root,
      base,
      token,
      pauseOn,
    ]);
    child.on("message", (value) => {
      if (value.ready) ready = true;
      else if (value.paused) paused = value.paused;
      else messages.push(value);
    });
    await until(() => ready);
  }
  async function stopGateway() {
    if (!child || child.exitCode !== null || child.signalCode) return;
    const stopped = once(child, "exit");
    child.kill("SIGKILL");
    await stopped;
  }
  const request = (channel) =>
    "Open " + fixture + "/channel-page/" + channel + " in my comet browser";
  const deny = "Open LinkedIn in my comet browser";
  const sent = (id, text, quotedReceipt) => child.send({ id, text, quotedReceipt });
  const find = (pattern) => messages.find((m) => pattern.test(m.text));
  async function checkProfile(chat, route) {
    const state = await ok("/api/sessions/" + chat + "/browser/state");
    const tabs = await (await fetch(endpoints.comet + "/json/list")).json();
    if (route) {
      assert.equal(state.selectedProfile, "comet");
      assert.ok(tabs.some((tab) => tab.url === fixture + route));
    } else {
      assert.notEqual(state.selectedProfile, "comet");
      assert.ok(tabs.every((tab) => !tab.url.includes("linkedin.com")));
    }
  }
  let db, ui;
  try {
    await startGateway(pageTitle);
    db = new DatabaseSync(path.join(root, "channel.sqlite"), {
      readOnly: true,
    });
    sent("first", "Can you control my browser?");
    await until(() => find(/cannot control/));
    const chat = db
      .prepare("SELECT session FROM tasks WHERE id=1")
      .get().session;
    addHistory(root, chat);
    for (const [id, text, accepted, site] of [
      ["linkedin", deny, false, "linkedin.com"],
      ["fixture", request("whatsapp"), true, fixture],
    ]) {
      sent(id, text);
      const task = await until(() =>
        db
          .prepare("SELECT * FROM tasks WHERE text=? AND state='waiting'")
          .get(text),
      );
      assert.equal(task.session, chat);
      const decision = JSON.parse(task.decision);
      assert.equal(decision.action, "browser_open");
      const approvalNotice = "decision:" + task.id + ":" + decision.approval_id;
      const prompt = await until(() => {
        const notice = db.prepare("SELECT receipt FROM outbox WHERE id=? AND state='sent'").get(approvalNotice);
        return notice?.receipt && messages.find((message) => message.messageId === notice.receipt);
      });
      assert.match(prompt.text, /comet/i);
      assert.ok(prompt.text.includes(site));
      assert.match(prompt.text, /Reply yes or no\.$/);
      assert.deepEqual(visits, []);
      const reply = accepted ? "yes" : "no";
      sent(id + "-decision", reply, prompt.messageId);
      if (accepted) {
        await until(() => paused);
        assert.deepEqual(visits, ["/channel-page/whatsapp"]);
        await stopGateway();
        await startGateway();
        sent(id + "-decision", reply, prompt.messageId); // Replayed inbound notification on reconnect.
        await until(() =>
          messages.find(
            (m) => m.messageId === paused && m.text.includes(pageTitle),
          ),
        );
      } else {
        await until(() =>
          db
            .prepare("SELECT 1 FROM outbox WHERE id=? AND state='sent'")
            .get("result:" + task.id),
        );
      }
      await checkProfile(chat, accepted && "/channel-page/whatsapp");
      assert.equal(
        messages.filter((message) => message.text === prompt.text).length,
        1,
      );
    }
    assert.equal(messages.filter((m) => m.text.includes(pageTitle)).length, 1);
    assert.equal(db.prepare("SELECT count(*) AS n FROM tasks").get().n, 3);
    console.log(
      "WhatsApp: same chat, denial, approval, gateway restart and one verified reply.",
    );

    const webChat = (await ok("/api/sessions", {})).session_id;
    addHistory(root, webChat);
    await ok("/api/permissions", { session_id: webChat, profile: "ask" });
    ui = await webComposer(endpoints.chrome, base, webChat, token);
    await until(() =>
      ui.evaluate(
        'document.querySelector("#message-input") && document.body.innerText.includes(' +
          JSON.stringify(refusal) +
          ")",
      ),
    );
    for (const [text, accept, site] of [
      [deny, false, "linkedin.com"],
      [request("web"), true, fixture],
    ]) {
      await ui.send(text);
      const approval = await until(
        async () => (await ok("/api/sessions/" + webChat)).pending_approval,
      );
      assert.equal(approval.action, "browser_open");
      assert.match(approval.detail, /comet/i);
      assert.ok(approval.detail.includes(site));
      const button = accept
        ? "/^(Allow once|Allow permission|Share once)$/i"
        : "/^Deny$/i";
      await until(() =>
        ui.evaluate(`(() => {
        const button = [...document.querySelectorAll('button')].find(b => ${button}.test(b.textContent.trim()) && b.offsetWidth);
        if (!button) return false;
        button.click(); return true;
      })()`),
      );
      await until(async () => {
        const events = (await ok("/api/sessions/" + webChat + "/events"))
          .events;
        return (
          events.filter((e) => e.kind === "done").length >= (accept ? 2 : 1)
        );
      });
      await checkProfile(webChat, accept && "/channel-page/web");
      if (!accept) assert.deepEqual(visits, ["/channel-page/whatsapp"]);
    }
    await until(() =>
      ui.evaluate(
        "document.body.innerText.includes(" +
          JSON.stringify("I found " + pageTitle) +
          ")",
      ),
    );
    assert.deepEqual(visits, ["/channel-page/whatsapp", "/channel-page/web"]);
    assert.equal(messages.filter((m) => m.text.includes(pageTitle)).length, 1);
    console.log(
      "Web: submitted both commands, denied and approved in the UI, and read the observed page title in chat.",
    );
  } finally {
    ui?.close();
    await stopGateway();
    db?.close();
  }
}
