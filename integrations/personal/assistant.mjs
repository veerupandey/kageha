// A single owner's delivery journal. Kageha owns the actual tasks and their evidence.
import { initIncomingFiles, bindIncomingFiles } from "./incoming-files.mjs";
import { initResults } from "./results.mjs";
import { whatsappText } from "../channels/whatsapp.mjs";
import { decideAction } from "./actions.mjs";
import { DatabaseSync } from "node:sqlite";
import { createHash } from "node:crypto";

export function journal(path) {
  const db = new DatabaseSync(path);
  db.exec(`PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
    CREATE TABLE IF NOT EXISTS inbox(id TEXT PRIMARY KEY, channel TEXT, text TEXT, reply TEXT);
    CREATE TABLE IF NOT EXISTS tasks(id INTEGER PRIMARY KEY, request TEXT UNIQUE, text TEXT, channel TEXT,
      created INTEGER DEFAULT (unixepoch()*1000), session TEXT, turn TEXT, cursor INTEGER DEFAULT 0, state TEXT DEFAULT 'queued', progress TEXT DEFAULT '',
      decision TEXT, media TEXT, attempts INTEGER DEFAULT 0, retry INTEGER DEFAULT 0);
    CREATE TABLE IF NOT EXISTS commands(id TEXT PRIMARY KEY, task INTEGER, kind TEXT, text TEXT, decision TEXT,
      state TEXT DEFAULT 'pending');
    CREATE TABLE IF NOT EXISTS outbox(id TEXT PRIMARY KEY, channel TEXT, text TEXT, state TEXT DEFAULT 'pending',
      receipt TEXT, created INTEGER, retry INTEGER DEFAULT 0);
    CREATE TABLE IF NOT EXISTS watches(id TEXT PRIMARY KEY, instruction TEXT, due INTEGER, interval_ms INTEGER, channel TEXT);
    CREATE TABLE IF NOT EXISTS digest(id TEXT PRIMARY KEY, channel TEXT, note TEXT, created INTEGER);
    CREATE TABLE IF NOT EXISTS grants(id TEXT PRIMARY KEY, detail TEXT);
    CREATE TABLE IF NOT EXISTS meta(id TEXT PRIMARY KEY, value TEXT);
    UPDATE outbox SET state='uncertain' WHERE state='sending';`);
  initResults(db);
  initIncomingFiles(db);
  return db;
}

export function atomic(db, operation) {
  db.exec("BEGIN IMMEDIATE");
  try {
    const value = operation();
    db.exec("COMMIT");
    return value;
  } catch (error) {
    db.exec("ROLLBACK");
    throw error;
  }
}

export function notice(db, id, channel, text) {
  if (isTextChannel(channel) && id.startsWith("result:")) {
    text = whatsappText(text, {plain: channel !== "whatsapp"});
  }
  const chars = Array.from(text);
  const insert = db.prepare(
    "INSERT OR IGNORE INTO outbox(id,channel,text,created) VALUES(?,?,?,?)",
  );
  for (let offset = 0; offset < chars.length; ) {
    let chunk = chars.slice(offset, offset + 1200);
    if (isTextChannel(channel) && offset + chunk.length < chars.length) {
      const newline = chunk.lastIndexOf("\n");
      const boundary = newline >= 600 ? newline : chunk.lastIndexOf(" ");
      if (boundary >= 600) chunk = chunk.slice(0, boundary + 1);
    }
    insert.run(
      offset ? id + ":" + offset : id,
      channel,
      chunk.join(""),
      Date.now(),
    );
    offset += chunk.length;
  }
}

const ongoing = "state IN ('queued','working','waiting')";
export const isTextChannel = channel => ["whatsapp", "imessage", "sms"].includes(channel);
export function active(db) {
  return db
    .prepare("SELECT * FROM tasks WHERE " + ongoing + " ORDER BY id DESC")
    .all();
}
function summary(db, task, numbered = true) {
  const call = db
    .prepare(
      "SELECT status,answer FROM calls WHERE task=? ORDER BY rowid DESC LIMIT 1",
    )
    .get(task.id);
  const follow = call
    ? "\nCall " + call.status + (call.answer ? ": " + call.answer : "")
    : "";
  if (!numbered) {
    if (call?.answer) return "They said: “" + call.answer + "”";
    if (task.decision) {
      const pending = JSON.parse(task.decision);
      const prompt = db.prepare("SELECT text FROM outbox WHERE id=?").get("decision:" + task.id + ":" + pending.approval_id);
      return prompt?.text.replace("Swipe right on this message", "Swipe right on the original approval message") || pending.question || pending.detail || "I’m waiting for your reply.";
    }
    if (task.progress && !/^\w+ \([\w -]+\)$/.test(task.progress)) return task.progress;
    const subject = task.text.trim().replace(/\s+/g, " ").slice(0, 90);
    if (task.state === "queued") return "I have “" + subject + "” queued.";
    if (task.state === "working") return "I’m still working on “" + subject + "”.";
    return "The request ended with status “" + task.state + "”.";
  }
  return (
    "#" +
    task.id +
    " " +
    task.text.slice(0, 90) +
    " — " +
    (task.progress || task.state) +
    follow
  );
}
const statusRequest =
  /^(status|progress|updates?|any (news|updates?)|what(?:'s|’s| is) (happening|the status)|what are you (doing|working on)|what did (they|the office|the vendor) say|did (they|the office|the vendor) (reply|answer|call back)|how(?:'s|’s| is) (it|that)( going)?|where are we)[\s.!?]*$/i;
const correction =
  /^(only\b|actually\b|instead\b|make (it|that)\b|also\b|change\b|use\b.*instead|after \d|before \d|steer\b)/i;

function smallTalk(text) {
  const greeting = text.trim();
  if (/^👋[🏻-🏿]?$/u.test(greeting)) return "Hey 👋 What’s on your mind?";
  const words = greeting
    .toLowerCase()
    .replace(/(?:[.!?,…\s]|(?:👋|🙏|😊|☀|🌙)[\uFE0F🏻-🏿]*)+$/u, "")
    .trim();
  if (/^(?:hi|hey|hello)(?: there| kageha)?$/.test(words))
    return "Hey 👋 What’s on your mind?";
  if (
    /^(?:there|you there|are you there|still there|are you around|you around)$/.test(
      words,
    )
  )
    return "Yep, I’m here.";
  if (/^(?:thanks|thank you|thanks a lot|thank you so much)$/.test(words))
    return "Anytime 😊";
  if (words === "good morning") return "Morning ☀️";
  if (words === "good night" || words === "goodnight") return "Good night 🌙";
  if (["good afternoon", "good evening"].includes(words))
    return words[0].toUpperCase() + words.slice(1) + "!";
  return null;
}

function checkIn(db, text, tasks) {
  if (!/^(?:oye[\s!.?,…]*|[?？][?？!\s]*|(?:are you |you |still )there[?!.\s]*)$/iu.test(text.trim())) return null;
  if (!tasks.length && /there/i.test(text)) return "Yep, I’m here.";
  if (!tasks.length) return "I’m here 👋 What’s up?";
  if (tasks.length > 1) return "I’m here. Which request would you like an update on?";
  const [task] = tasks;
  return summary(db, task, false);
}

export function approvalQuestion(action, detail) {
  const control = /^(browser_fill|browser_click) on (https?:\/\/\S+) using observed control (.+)$/u.exec(detail);
  if (control && control[1] === action) {
    const question = action === "browser_fill"
      ? "May I fill in “" + control[3] + "” on this form?"
      : "May I click “" + control[3] + "” on this page?";
    return question + "\n" + control[2];
  }
  const copy = action === "attach_past_file" && /^Copy (.+) from an earlier chat into this chat$/u.exec(detail);
  if (copy) return "May I bring " + copy[1] + " into this chat from our earlier conversation?";
  return "May I do this?\n" + detail;
}

export function requestId(messageId) {
  return "personal-" + createHash("sha256").update(messageId).digest("hex").slice(0, 32);
}

export function receive(db, { id, channel, text, media, document, quotedReceipt, messageTime }, spoken = false) {
  if (
    !id ||
    id.length > 200 ||
    typeof text !== "string" ||
    text.length > 16000 ||
    (!text.trim() && !media && !document)
  )
    throw Error("Invalid message");
  return atomic(db, () => {
    const prior = db.prepare("SELECT reply FROM inbox WHERE id=?").get(id);
    if (prior) return prior.reply;
    const whatsapp = channel === "whatsapp";
    const incomingText = text;
    let quotedDecision = false;
    const selfChat = whatsapp && id.startsWith("wa-self:");
    const binaryReply = !media && !document && /^(yes|approve|no|deny)[.!]?$/i.test(text.trim());
    if (selfChat) {
      const row = db.prepare("DELETE FROM meta WHERE id='wa-reply-prompt' RETURNING value").get();
      if (!quotedReceipt && binaryReply && row) {
        const {receipt,sentAt} = JSON.parse(row.value);
        if (Number.isFinite(messageTime) && messageTime >= sentAt) quotedReceipt = receipt;
      }
    }
    if (whatsapp && quotedReceipt && binaryReply) {
      const quoted = db.prepare("SELECT id FROM outbox WHERE channel='whatsapp' AND receipt=? AND id LIKE 'decision:%'").get(quotedReceipt);
      const match = /^decision:(\d+):([^:]+)(?::confirm-[\w-]+)?$/.exec(quoted?.id || "");
      if (match) {
        const task = db.prepare("SELECT channel,decision FROM tasks WHERE id=? AND state='waiting'").get(Number(match[1]));
        const pending = task?.decision && JSON.parse(task.decision);
        if (task?.channel !== channel || pending?.approval_id !== match[2]) {
          const expired = "That request has expired. Please reply to the latest request.";
          db.prepare("INSERT INTO inbox VALUES(?,?,?,?)").run(id,channel,text,expired);
          if (!spoken) notice(db, "ack:" + id, channel, expired);
          return expired;
        }
        if (pending.action !== "ask_human") {
          quotedDecision = true;
          text = (/^(yes|approve)/i.test(text.trim()) ? "approve" : "deny") + " #" + match[1] + " " + match[2];
        } else {
          text = "#" + match[1] + " " + text;
        }
      }
    }
    if (document && whatsapp) {
      db.prepare("INSERT INTO incoming_files(id,name,mime,bytes) VALUES(?,?,?,?)").run(id,document.name,document.mime,document.bytes);
    }
    const allTasks = active(db);
    const sameChannel = allTasks.filter(t => t.channel === channel);
    const tasks = whatsapp ? sameChannel : allTasks;
    const number = text.match(/(?:^|\s)#(\d+)\b/);
    const conversational =
      /^(and |also |actually |only |instead |what about |what did |did they |did it |can you (change|move|update) |tell me more|why (did|was) |what do you mean)/i.test(
        text,
      );
    const recent = whatsapp
      ? db.prepare("SELECT * FROM tasks WHERE channel=? AND session IS NOT NULL ORDER BY id DESC LIMIT 1").get(channel)
      : !tasks.length && conversational
        ? db.prepare("SELECT * FROM tasks ORDER BY id DESC LIMIT 1").get()
        : null;
    const questions = tasks.filter(t => t.decision && JSON.parse(t.decision).action === "ask_human");
    let target;
    if (number) {
      target = db.prepare("SELECT * FROM tasks WHERE id=?").get(Number(number[1]));
    } else if (tasks.length === 1) {
      target = tasks[0];
    } else if (isTextChannel(channel) && questions.length === 1 && questions[0].channel === channel) {
      target = questions[0];
    } else if (!tasks.length) {
      target = recent;
      if (!target && allTasks.length === 1 && (conversational || correction.test(text))) {
        target = allTasks[0];
      }
    }
    const instruction = text.replace(/(?:^|\s)#\d+\b/, "").trim();
    const fresh = whatsapp && /^\/(?:agents?\b|new(?:\s|$))/i.test(text);
    let slowAck = false;
    let quiet = false;
    const plainText = isTextChannel(channel) && !document && !media;
    const ping = plainText && checkIn(db, text, sameChannel);
    if (ping && sameChannel.length === 1 && !spoken) {
      db.prepare("DELETE FROM outbox WHERE id=? AND state='scheduled'").run("task-ack:" + sameChannel[0].id);
    }
    const casual = ping || (plainText && !target?.decision && smallTalk(text));
    let reply = casual || decideAction(db, text);
    let replyKey = "ack:" + id;
    if (!reply && selfChat && binaryReply && !number && sameChannel.some(t => t.decision && JSON.parse(t.decision).action !== "ask_human")) {
      // Context changed, or this reply predates the prompt. Restate a specific
      // delivered action and wait for a fresh answer before recording consent.
      const last = db.prepare(`SELECT o.id,t.id AS task,t.decision FROM tasks t JOIN outbox o
        ON o.id='decision:'||t.id||':'||json_extract(t.decision,'$.approval_id')
        WHERE t.channel='whatsapp' AND o.channel=t.channel AND t.state='waiting'
        AND o.receipt IS NOT NULL ORDER BY o.rowid DESC LIMIT 1`).get();
      if (last) {
        const pending = JSON.parse(last.decision);
        if (db.prepare("SELECT 1 FROM commands WHERE task=? AND json_extract(decision,'$.approval_id')=? AND state IN ('pending','done')").get(last.task,pending.approval_id)) {
          reply = "I’ve got your answer.";
        } else {
          replyKey = last.id + ":confirm-" + requestId(id);
          reply = pending.action === "ask_human" ? pending.question || pending.detail :
            approvalQuestion(pending.action, pending.detail || pending.action) + "\nReply yes or no.";
        }
      } else reply = "I’ll send the question here as soon as it’s ready.";
    }
    const activeFileTarget = target && tasks.length === 1 && !target.decision;
    if (document && whatsapp && !text.trim() && !activeFileTarget && !(target?.decision && JSON.parse(target.decision).action==="ask_human")) {
      reply = "I’ve got " + document.name + ".";
    } else if (whatsapp && !target && questions.length>1 && !fresh && !reply) {
      reply = "Which conversation is this for? " + questions.slice(0,4).map(t=>"#"+t.id+" ("+t.text.slice(0,60)+")").join(" or ") + "?";
    } else if (reply) {
      // Already answered without starting another task.
    } else if (channel !== "voice" && /^\/?(?:files|resend files)$/i.test(instruction)) {
      const fileTask = target || db.prepare("SELECT * FROM tasks WHERE turn IS NOT NULL ORDER BY id DESC LIMIT 1").get();
      if (fileTask && fileTask.turn) {
        db.prepare("INSERT OR IGNORE INTO result_requests(id,task,channel) VALUES(?,?,?)").run(id,fileTask.id,channel);
        reply = whatsapp ? "I’ll look up those files." : "I’ll look up the files for #" + fileTask.id + ".";
      } else reply = "Which task? Send /files followed by its number.";
    } else if (/^list permissions[.!]?$/i.test(instruction)) {
      reply =
        db
          .prepare("SELECT * FROM grants ORDER BY rowid")
          .all()
          .map((g) => g.id + " " + g.detail)
          .join("\n") || "No saved action grants.";
    } else if (/^revoke [a-f0-9]+$/i.test(instruction)) {
      reply = db
        .prepare("DELETE FROM grants WHERE id=?")
        .run(instruction.split(" ")[1]).changes
        ? "Standing permission revoked."
        : "Unknown permission.";
    } else if (number && !target)
      reply = "I could not find task #" + number[1] + ".";
    else if (
      /^(send|reply|deliver)(?: the)?(?: result)? (?:on|by|via|to) (whatsapp|sms|imessage)[.!]?$/i.test(
        instruction,
      )
    ) {
      if (!target)
        reply = "Include the task number to change its reply channel.";
      else {
        const channelName = instruction
          .toLowerCase()
          .match(/(whatsapp|sms|imessage)[.!]?$/)[1];
        db.prepare("UPDATE tasks SET channel=? WHERE id=?").run(
          channelName,
          target.id,
        );
        if (target.turn && !["queued","working","waiting"].includes(target.state)) {
          notice(db,"redirect:"+id,channelName,"#"+target.id+" "+target.progress);
          db.prepare("INSERT OR IGNORE INTO result_requests(id,task,channel) VALUES(?,?,?)").run(id,target.id,channelName);
        }
        reply =
          "I’ll send the result for #" +
          target.id +
          " via " +
          channelName +
          ".";
      }
    } else if (statusRequest.test(instruction) || (number && !instruction)) {
      const rows = target
        ? [target]
        : tasks.length
          ? tasks
          : db.prepare("SELECT * FROM tasks ORDER BY id DESC LIMIT 1").all();
      reply = rows.length
        ? rows
            .slice(0, 6)
            .map((t) => summary(db, t, !!number || rows.length > 1 || !isTextChannel(channel)))
            .join("\n")
        : "Nothing’s in progress yet.";
    } else {
      const waiting = target?.decision && JSON.parse(target.decision);
      const decision = /^(always approve|approve|deny)(?:\s|$)/i.exec(
        instruction,
      );
      const cancel = /^(cancel|stop)(?:\s+(it|that|task))?[.!]?$/i.test(
        instruction,
      );
      let kind = decision
        ? "approval"
        : cancel
          ? "cancel"
          : waiting?.action === "ask_human"
            ? "answer"
            : number || conversational || correction.test(instruction)
              ? "steer"
              : "new";
      const continueSession =
        kind === "steer" &&
        target?.session &&
        !["queued", "working", "waiting"].includes(target.state);
      if (continueSession) kind = "new";
      if (whatsapp && tasks.length === 1 && kind === "new" && !number && !continueSession && !target?.decision) kind = "steer";
      if (fresh || (whatsapp && kind === "steer" && !target && !tasks.length)) kind = "new";
      if (kind !== "new") {
        if (!target)
          reply = tasks.length
            ? "Which task? Include its number and your instruction. " +
              tasks
                .slice(0, 6)
                .map((t) => "#" + t.id)
                .join(", ")
            : "There is no active task for that update.";
        else if (
          kind === "approval" &&
          (!waiting ||
            waiting.action === "ask_human" ||
            !number ||
            instruction.toLowerCase() !==
              decision[1].toLowerCase() +
                " " +
                waiting.approval_id.toLowerCase())
        )
          reply =
            "Use the approval ID and task number from the current approval request.";
        else if (!["queued", "working", "waiting"].includes(target.state))
          reply = summary(db, target);
        else {
          db.prepare(
            "INSERT INTO commands(id,task,kind,text,decision) VALUES(?,?,?,?,?)",
          ).run(
            id,
            target.id,
            kind,
            instruction || (document ? "Here’s the file I mentioned." : ""),
            waiting ? JSON.stringify(waiting) : null,
          );
          if (quotedDecision || (isTextChannel(channel) && target.channel === channel)) quiet = true;
          if (whatsapp && ["answer","steer"].includes(kind)) {
            bindIncomingFiles(db,target.id,id);
            quiet = true;
          }
          reply =
            "Received your " +
            {
              steer: "update",
              cancel: "cancellation request",
              approval: "decision",
              answer: "answer",
            }[kind] +
            " for #" +
            target.id +
            ".";
        }
      } else {
        const request = requestId(id);
        const row = db
          .prepare(
            "INSERT INTO tasks(request,text,channel,media,session) VALUES(?,?,?,?,?)",
          )
          .run(
            request,
            (whatsapp ? text.replace(/^\/new(?:\s+|$)/i,"").trim() : text.trim()) || (document ? "Please read the attached file." : media ? "Voice note" : "Start a new conversation."),
            channel === "voice" ? "sms" : channel,
            media || null,
            !fresh && (continueSession || (whatsapp && !tasks.length && !number)) ? target?.session || null : null,
          );
        if (whatsapp) bindIncomingFiles(db,row.lastInsertRowid);
        slowAck = isTextChannel(channel) && !spoken;
        reply = slowAck
          ? "I’m on it."
          : "Received your request (#" + row.lastInsertRowid + ").";
        if (slowAck) {
          const now = Date.now();
          db.prepare(
            "INSERT INTO outbox(id,channel,text,state,created,retry) VALUES(?,?,?,'scheduled',?,?)",
          ).run(
            "task-ack:" + row.lastInsertRowid,
            channel,
            reply,
            now,
            now + 8000,
          );
        }
      }
    }
    db.prepare("INSERT INTO inbox VALUES(?,?,?,?)").run(
      id,
      channel,
      incomingText,
      reply,
    );
    if (!spoken && !slowAck && !quiet) notice(db, replyKey, channel, reply);
    return reply;
  });
}

export function watch(db, id, instruction, due, interval, channel) {
  if (
    !instruction?.trim() ||
    !Number.isSafeInteger(due) ||
    !Number.isSafeInteger(interval) ||
    interval < 0 ||
    (interval && interval < 60000)
  )
    throw Error("Invalid watch");
  db.prepare("INSERT OR IGNORE INTO watches VALUES(?,?,?,?,?)").run(
    id,
    instruction,
    due,
    interval,
    channel,
  );
}

export function runDue(db, now = Date.now()) {
  for (const row of db
    .prepare("SELECT * FROM watches WHERE due<=? ORDER BY due LIMIT 16")
    .all(now)) {
    const key = "watch:" + row.id + ":" + row.due;
    atomic(db, () => {
      db.prepare(
        "INSERT OR IGNORE INTO tasks(request,text,channel) VALUES(?,?,?)",
      ).run(
        "watch-" + createHash("sha256").update(key).digest("hex").slice(0, 24),
        row.instruction,
        row.channel,
      );
      if (row.interval_ms)
        db.prepare("UPDATE watches SET due=? WHERE id=?").run(
          row.due +
            (Math.floor((now - row.due) / row.interval_ms) + 1) *
              row.interval_ms,
          row.id,
        );
      else db.prepare("DELETE FROM watches WHERE id=?").run(row.id);
    });
  }
}

// One digest per owner-local day. A restart after the scheduled hour catches up.
export function flushDigest(db, zone, hour, now = new Date()) {
  const parts = new Intl.DateTimeFormat("en-CA", {
    timeZone: zone,
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    hourCycle: "h23",
  }).formatToParts(now);
  const field = (k) => parts.find((p) => p.type === k).value;
  if (Number(field("hour")) < hour) return;
  const day = [field("year"), field("month"), field("day")].join("-");
  for (const { channel } of db
    .prepare("SELECT DISTINCT channel FROM digest")
    .all()) {
    atomic(db, () => {
      const key = "digest:" + day + ":" + channel;
      if (db.prepare("SELECT 1 FROM outbox WHERE id=?").get(key)) return;
      const rows = db
        .prepare(
          "SELECT * FROM digest WHERE channel=? ORDER BY created LIMIT 24",
        )
        .all(channel);
      notice(db, key, channel, rows.map((r) => "• " + r.note).join("\n"));
      for (const r of rows)
        db.prepare("DELETE FROM digest WHERE id=?").run(r.id);
    });
  }
}
