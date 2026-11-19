import { createHash } from "node:crypto";
import { atomic, notice, watch } from "./assistant.mjs";

export function initActions(db) {
  db.exec(`CREATE TABLE IF NOT EXISTS calls(id TEXT PRIMARY KEY, task INTEGER, channel TEXT, phone TEXT,
    message TEXT, status TEXT, sid TEXT, answer TEXT);
    CREATE TABLE IF NOT EXISTS reveals(id TEXT PRIMARY KEY, task INTEGER, channel TEXT,path TEXT,status TEXT);
    CREATE TABLE IF NOT EXISTS action_receipts(id TEXT PRIMARY KEY, result TEXT);
    UPDATE calls SET status='uncertain' WHERE status='dispatching';`);
}

function key(...parts) {
  return createHash("sha256")
    .update(JSON.stringify(parts))
    .digest("hex")
    .slice(0, 24);
}
export function action(
  db,
  cfg,
  { session, turn, id, action, arguments: args },
) {
  const t = db
    .prepare(
      "SELECT * FROM tasks WHERE session=? AND state IN ('working','waiting','queued') ORDER BY id DESC LIMIT 1",
    )
    .get(session);
  if (
    !t ||
    !id ||
    !args ||
    createHash("sha256")
      .update(session + ":" + t.request)
      .digest("hex")
      .slice(0, 32) !== turn
  )
    throw Error("Unknown personal task");
  const reference = "P-" + key(turn, id);
  if (action === "personal_status")
    return {
      calls: db
        .prepare("SELECT id,phone,status,answer FROM calls WHERE task=?")
        .all(t.id),
      files: db
        .prepare("SELECT id,path,status FROM reveals WHERE task=?")
        .all(t.id),
      watches: db
        .prepare(
          "SELECT id,instruction,due,interval_ms FROM watches WHERE id LIKE ?",
        )
        .all("task:" + t.id + ":%"),
      mac_connected:
        Date.now() -
          Number(
            db.prepare("SELECT value FROM meta WHERE id='mac-seen'").get()
              ?.value || 0,
          ) <
        30000,
    };
  return atomic(db, () => {
    const prior = db
      .prepare("SELECT result FROM action_receipts WHERE id=?")
      .get(reference);
    if (prior) return JSON.parse(prior.result);
    let result;
    if (action === "personal_digest") {
      if (
        typeof args.note !== "string" ||
        !args.note.trim() ||
        args.note.length > 1500
      )
        throw Error("Provide a bounded digest item");
      db.prepare("INSERT OR IGNORE INTO digest VALUES(?,?,?,?)").run(
        key(t.channel, args.note.trim()),
        t.channel,
        args.note.trim(),
        Date.now(),
      );
      result = { queued: true };
    } else if (action === "personal_follow_up") {
      if (args.cancel_id) {
        if (!String(args.cancel_id).startsWith("task:" + t.id + ":"))
          throw Error("Unknown follow-up");
        result = {
          cancelled:
            db.prepare("DELETE FROM watches WHERE id=?").run(args.cancel_id)
              .changes === 1,
        };
      } else {
        const due = Date.parse(args.due),
          interval = (args.interval_minutes ?? 0) * 60000;
        if (
          typeof args.instruction !== "string" ||
          args.instruction.length > 8000 ||
          due < Date.now() ||
          typeof args.due !== "string" ||
          !/(Z|[+-]\d\d:\d\d)$/i.test(args.due)
        )
          throw Error(
            "Provide a future time with timezone and a bounded instruction",
          );
        const wid = "task:" + t.id + ":" + reference;
        watch(db, wid, args.instruction, due, interval, t.channel);
        result = { watch_id: wid, due: new Date(due).toISOString() };
      }
    } else {
      let detail;
      if (action === "personal_call") {
        if (
          !/^\+[1-9]\d{6,14}$/.test(args.phone) ||
          typeof args.message !== "string" ||
          !args.message.trim() ||
          args.message.length > 1500
        )
          throw Error(
            "An E.164 phone number and a question of at most 1500 characters are required",
          );
        const authorized = (cfg.callRules || []).some(
          (r) => r.phone === args.phone && r.message === args.message,
        );
        result = {
          id: reference,
          status: authorized ? "authorized" : "awaiting_owner",
        };
        db.prepare(
          "INSERT INTO calls(id,task,channel,phone,message,status) VALUES(?,?,?,?,?,?)",
        ).run(
          reference,
          t.id,
          t.channel,
          args.phone,
          args.message,
          result.status,
        );
        detail = "Call " + args.phone + " and say: " + args.message;
      } else if (action === "personal_reveal") {
        if (
          typeof args.path !== "string" ||
          !args.path.startsWith("/") ||
          /[\x00-\x1f]/.test(args.path) ||
          args.path.length > 4096
        )
          throw Error("Provide an absolute Mac path");
        result = { id: reference, status: "awaiting_owner" };
        db.prepare("INSERT INTO reveals VALUES(?,?,?,?,?)").run(
          reference,
          t.id,
          t.channel,
          args.path,
          result.status,
        );
        detail = "Reveal in Finder: " + args.path;
      } else throw Error("Unknown action");
      if (result.status === "awaiting_owner")
        notice(
          db,
          "pending:" + reference,
          t.channel,
          "#" +
            t.id +
            " " +
            detail +
            "\nReply approve #" +
            t.id +
            " " +
            reference +
            " or deny #" +
            t.id +
            " " +
            reference +
            ".",
        );
    }
    db.prepare("INSERT INTO action_receipts VALUES(?,?)").run(
      reference,
      JSON.stringify(result),
    );
    return result;
  });
}

export function decideAction(db, text) {
  const match = /^(approve|deny)\s+#(\d+)\s+(P-[a-f0-9]{24})[.!]?$/i.exec(
    text.trim(),
  );
  if (!match) return null;
  const [, op, task, id] = match;
  for (const table of ["calls", "reveals"]) {
    const r = db
      .prepare("SELECT status FROM " + table + " WHERE id=? AND task=?")
      .get(id, Number(task));
    if (!r) continue;
    const status = op.toLowerCase() === "approve" ? "authorized" : "denied";
    if (r.status === "awaiting_owner")
      db.prepare(
        "UPDATE " +
          table +
          " SET status=? WHERE id=? AND status='awaiting_owner'",
      ).run(status, id);
    return (
      "#" +
      task +
      " action " +
      (r.status === "awaiting_owner" ? status : r.status) +
      "."
    );
  }
  return "No matching personal action.";
}
