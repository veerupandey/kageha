// Delivery consumes the Rust harness's turn-scoped artifact list. Bytes are frozen
// before advancing the event cursor so retries never send a later revision.
import path from "node:path";
import { createHash, randomBytes } from "node:crypto";
import http from "node:http";
import mime from "mime-types";

export function initResults(db) {
  db.exec(`CREATE TABLE IF NOT EXISTS result_bytes(hash TEXT PRIMARY KEY, bytes BLOB NOT NULL);
    CREATE TABLE IF NOT EXISTS result_files(id TEXT PRIMARY KEY, task INTEGER, name TEXT, mime TEXT, hash TEXT, token TEXT UNIQUE);
    CREATE TABLE IF NOT EXISTS result_delivery(id TEXT PRIMARY KEY, file TEXT);
    CREATE TABLE IF NOT EXISTS task_routes(task INTEGER PRIMARY KEY,host TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS result_requests(id TEXT PRIMARY KEY, task INTEGER, channel TEXT, state TEXT DEFAULT 'pending');`);
}

export async function collectResults(api, session, event, request = "") {
  const files = [];
  for (const ref of new Set(event.turn_artifacts ?? event.artifacts ?? [])) {
    if (
      typeof ref !== "string" ||
      !/^(workspace\/outputs|artifacts)\//.test(ref) ||
      ref.split("/").some((s) => !s || s.startsWith(".")) ||
      /[\\\x00-\x1f]/.test(ref)
    )
      continue;
    const name = path.posix.basename(ref);
    try {
      const route =
        "/api/sessions/" +
        encodeURIComponent(session) +
        "/files/" +
        ref.split("/").map(encodeURIComponent).join("/");
      const bytes = await api.file(route);
      files.push({
        ref,
        name,
        mime: mime.lookup(name) || "application/octet-stream",
        bytes,
        hash: createHash("sha256").update(bytes).digest("hex"),
      });
    } catch (err) {
      // Transient transport failures retry the same cursor. A removed or oversized
      // file must not prevent delivery of the rest of a successful turn.
      if (![400, 403, 404, 413].includes(err.status)) throw err;
      files.push({
        ref,
        name,
        missing: err.status === 404,
        error:
          err.status === 413
            ? "exceeds the 64 MiB delivery limit"
            : "is no longer available",
      });
    }
  }
  if (!files.some(file => !file.error)) return files;
  // The turn journal also records temporary outputs that were deleted before completion.
  const mentioned = (request + "\n" + (event.message || "")).toLowerCase();
  return files.filter(file => !file.missing || mentioned.includes(file.name.toLowerCase()));
}

export function queueResults(db, task, channel, files, key, cfg, notice) {
  for (const file of files) {
    const id = task + ":" + file.ref;
    const out = key + ":" + file.ref;
    if (file.error) {
      notice(db, out, channel, file.name + " " + file.error + ".");
      continue;
    }
    db.prepare("INSERT OR IGNORE INTO result_bytes VALUES(?,?)").run(
      file.hash,
      file.bytes,
    );
    db.prepare("INSERT OR IGNORE INTO result_files VALUES(?,?,?,?,?,NULL)").run(
      id,
      task,
      file.name,
      file.mime,
      file.hash,
    );
    const native =
      channel === "imessage" ||
      (channel === "whatsapp" && cfg.whatsappSelfChat);
    if (channel === "local") {
      const record = db
        .prepare(
          "SELECT t.session,r.host FROM tasks t LEFT JOIN task_routes r ON r.task=t.id WHERE t.id=?",
        )
        .get(task);
      const backend =
        record?.host === "shared" ? cfg.agentBackend : cfg.backend;
      if (backend && record?.session) {
        notice(
          db,
          out,
          channel,
          new URL(
            "/api/sessions/" +
              encodeURIComponent(record.session) +
              "/files/" +
              file.ref.split("/").map(encodeURIComponent).join("/"),
            backend,
          ).href,
        );
      }
      continue;
    }
    const page = file.mime === "text/html";
    const origin =
      cfg.shareOrigin ||
      db.prepare("SELECT value FROM meta WHERE id='share-origin'").get()?.value;
    if (native && !(page && origin)) {
      db.prepare(
        "INSERT OR IGNORE INTO outbox(id,channel,text,created) VALUES(?,?,?,?)",
      ).run(out, channel, file.name, Date.now());
      db.prepare("INSERT OR IGNORE INTO result_delivery VALUES(?,?)").run(
        out,
        id,
      );
    }
    if (
      channel === "whatsapp" &&
      /\.md(?:own)?$|\.markdown$/i.test(file.name)
    ) {
      const markdown = file.bytes.toString("utf8").trim();
      if (markdown) {
        const excerpt = Array.from(markdown);
        const preview =
          excerpt.length <= 4000
            ? markdown
            : excerpt
                .slice(0, 1100)
                .join("")
                .replace(/\s+\S*$/, "") + "…\n\nThe file has the full text.";
        notice(db, "result:preview:" + out, channel, preview);
      }
    }
    if (page || !native) {
      if (origin) {
        db.prepare(
          "UPDATE result_files SET token=COALESCE(token,?) WHERE id=?",
        ).run(randomBytes(24).toString("hex"), id);
        const token = db
          .prepare("SELECT token FROM result_files WHERE id=?")
          .get(id).token;
        const linkID = out + ":link";
        notice(db, linkID, channel, file.name + "\n" + origin + "/p/" + token);
        if (channel === "whatsapp" && !cfg.whatsappSelfChat && !page) {
          db.prepare("INSERT OR IGNORE INTO result_delivery VALUES(?,?)").run(
            linkID,
            id,
          );
        }
      } else {
        notice(
          db,
          out + ":link",
          channel,
          native
            ? "I’ve attached " +
                file.name +
                ". A public page link is unavailable right now."
            : file.name +
                " is available in the Kageha chat. This channel has no file link configured.",
        );
      }
    }
  }
}

export function attachment(db, delivery) {
  return db
    .prepare(
      `SELECT f.name,f.mime,b.bytes FROM result_delivery d JOIN result_files f ON f.id=d.file
    JOIN result_bytes b ON b.hash=f.hash WHERE d.id=?`,
    )
    .get(delivery);
}

// This listener has no access routes to the agent, its sessions, or the control API.
export function pageServer(db) {
  return http.createServer((req, res) => {
    const token = /^\/p\/([a-f0-9]{48})$/.exec(req.url)?.[1];
    const file =
      token &&
      ["GET", "HEAD"].includes(req.method) &&
      db
        .prepare(
          "SELECT name,mime,bytes FROM result_files JOIN result_bytes USING(hash) WHERE token=?",
        )
        .get(token);
    if (!file) {
      res.writeHead(404);
      res.end();
      return;
    }
    res.writeHead(200, {
      "Content-Type":
        file.mime + (file.mime.startsWith("text/") ? "; charset=utf-8" : ""),
      "Content-Length": file.bytes.length,
      "Cache-Control": "private, no-store",
      "X-Robots-Tag": "noindex, nofollow",
      "Referrer-Policy": "no-referrer",
      "X-Content-Type-Options": "nosniff",
      "Content-Security-Policy":
        "sandbox allow-scripts; default-src 'none'; img-src data:; style-src 'unsafe-inline'; script-src 'unsafe-inline'; font-src data:; form-action 'none'; base-uri 'none'",
      "Content-Disposition":
        (file.mime === "text/html" ? "inline" : "attachment") +
        "; filename*=UTF-8''" +
        encodeURIComponent(file.name),
    });
    res.end(req.method === "HEAD" ? undefined : file.bytes);
  });
}
