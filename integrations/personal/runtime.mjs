import { uploadIncomingFiles } from "./incoming-files.mjs";
import { active, approvalQuestion, atomic, isTextChannel, notice, requestId, runDue } from "./assistant.mjs";

import { collectResults, queueResults } from "./results.mjs";

export function backend(origin, token) {
  const base = new URL(origin);
  if (
    !["localhost", "127.0.0.1", "[::1]"].includes(base.hostname) &&
    base.protocol !== "https:"
  )
    throw Error("Use a loopback or HTTPS backend");
  const api = async (path, data) => {
    const upload = data instanceof FormData;
    const res = await fetch(new URL(path, base), {
      method: data === undefined ? "GET" : "POST",
      redirect: "error",
      signal: AbortSignal.timeout(20000),
      headers: {
        Authorization: "Bearer " + token,
        ...(upload ? {} : { "Content-Type": "application/json" }),
      },
      body: upload ? data : JSON.stringify(data),
    });
    const body = await res.json();
    if (!res.ok)
      throw Object.assign(
        Error(String(body.error || "Kageha request failed").slice(0, 250)),
        { status: res.status },
      );
    return body;
  };
  api.file = async (route) => {
    const response = await fetch(new URL(route, base), {
      headers: { Authorization: "Bearer " + token },
      redirect: "error",
      signal: AbortSignal.timeout(60000),
    });
    if (!response.ok) {
      await response.body?.cancel();
      throw Object.assign(Error("File download failed"), {
        status: response.status,
      });
    }
    const parts = [];
    let size = 0;
    for await (const part of response.body) {
      size += part.length;
      if (size > 64 * 1024 * 1024)
        throw Object.assign(Error("File exceeds delivery limit"), {
          status: 413,
        });
      parts.push(part);
    }
    return Buffer.concat(parts, size);
  };
  return api;
}

// The task's client_request_id survives lost HTTP responses and process restarts.
export async function advance(db, task, api, transcribe, cfg = {}, sharedApi) {
  let { id, session, turn, cursor } = task;
  if (!session && /^\/agents$/i.test(task.text.trim())) {
    const clients = sharedApi ? [api, sharedApi] : [api];
    const catalogs = await Promise.allSettled(
      clients.map((client) => client("/api/agents")),
    );
    const available = catalogs.filter((r) => r.status === "fulfilled");
    if (!available.length) throw Error("Agent directory unavailable");
    const agents = [
      ...new Map(
        available
          .flatMap((r) => r.value.agents)
          .map((agent) => [agent.id, agent]),
      ).values(),
    ];
    let reply =
      agents.map((a) => "/agent " + a.id + " — " + a.name).join("\n") +
      "\nAdd your request after the agent ID.";
    if (available.length !== clients.length)
      reply += "\nOne connected agent directory is temporarily offline.";
    atomic(db, () => {
      db.prepare("DELETE FROM outbox WHERE id=? AND state='scheduled'").run(
        "task-ack:" + id,
      );
      db.prepare("UPDATE tasks SET state='success',progress=? WHERE id=?").run(
        reply,
        id,
      );
      notice(db, "result:" + id, task.channel, reply);
    });
    return;
  }
  const addressed = /^\/agent\s+([a-z0-9][\w.-]{0,79})\s+([\s\S]+)$/i.exec(
    task.text,
  );
  if (!session) {
    session = (
      await api("/api/sessions", addressed ? { persona_id: addressed[1] } : {})
    ).session_id;
    db.prepare("UPDATE tasks SET session=? WHERE id=?").run(session, id);
  }
  const root = "/api/sessions/" + encodeURIComponent(session);
  if (!turn) {
    let receipt;
    try {
      receipt = (await api(root + "/requests/" + task.request)).receipt;
    } catch (err) {
      if (err.status !== 404) throw err;
    }
    if (receipt && !receipt.turn_id) {
      if (receipt.state === "received") return;
      throw Object.assign(
        Error("Kageha interrupted this admission before starting"),
        { status: 400 },
      );
    }
    if (receipt) turn = receipt.turn_id;
    else {
      await api("/api/permissions", { session_id: session, profile: "ask" });
      if (task.media) {
        task.text = await transcribe(task.media, session);
        db.prepare("UPDATE tasks SET text=?,media=NULL WHERE id=?").run(
          task.text,
          id,
        );
      }
      const channelModels = api === sharedApi ? [] : cfg.channelModels || [];
      let model = channelModels[0];
      if (api === sharedApi && cfg.agentFallbackModel) {
        const catalog = await api("/api/models");
        if (!catalog.default_model && !catalog.default) model = cfg.agentFallbackModel;
      }
      const attachments = await uploadIncomingFiles(db,id,session,api);
      const accepted = await api("/api/chat/stream", {
        ...(model && { model }),
        ...(channelModels.length > 1 && { model_roles: { executor: channelModels } }),
        ...(attachments.length && { attachments }),
        session_id: session,
        client_request_id: task.request,
        message: addressed ? addressed[2] : task.text,
        event_stream: "session",
      });
      turn = accepted.turn_id;
    }
    if (!turn) throw Error("Missing Kageha turn receipt");
    db.prepare("UPDATE tasks SET turn=?,state='working' WHERE id=?").run(
      turn,
      id,
    );
  }
  const page = await api(
    root +
      "/events?turn_id=" +
      encodeURIComponent(turn) +
      "&after_sequence=" +
      cursor +
      "&limit=200&include_live=decisions",
  );
  const completion = page.events.find((event) => event.kind === "done");
  const files = completion
    ? await collectResults(api, session, completion.payload, task.text)
    : [];
  atomic(db, () => {
    const destination = db
      .prepare("SELECT channel FROM tasks WHERE id=?")
      .get(id).channel;
    let progress = task.progress;
    let update;
    let state = "working";
    let pending = page.live?.pending_approval;
    if (task.decision && !pending && progress.startsWith("Waiting for you: ")) progress = "";
    for (const event of page.events) {
      if (event.kind === "tool_card") {
        const card = event.tool_card;
        if (card?.name && (!isTextChannel(destination) || !progress))
          progress = card.name + " (" + (card.status || "recorded") + ")";
      }
      if (event.kind === "assistant_narrative" && event.payload?.text) {
        update = event.payload.text.trim();
        progress = event.payload.text.slice(0, 380);
        if (isTextChannel(destination) && update && update.length <= 240) {
          db.prepare("UPDATE outbox SET text=? WHERE id=? AND state='scheduled'").run(update, "task-ack:" + id);
        }
      }
      if (event.kind === "done") {
        state = event.payload?.status || "finished";
        if (["working", "waiting", "queued"].includes(state))
          state = "finished";
        progress =
          event.payload?.message ||
          "Kageha ended this turn with status " + state + ".";
        pending = null;
        db
          .prepare("DELETE FROM outbox WHERE id=? AND state='scheduled'")
          .run("task-ack:" + id);
        if (
          !task.request.startsWith("watch-") ||
          progress.trim() !== "NO_UPDATE"
        ) {
          const text = isTextChannel(destination) ? progress : "#" + id + " " + progress;
          notice(db, "result:" + id, destination, text);
          queueResults(db, id, destination, files, "file:" + id, cfg, notice);
        }
      }
    }
    if (pending) {
      db.prepare("DELETE FROM outbox WHERE id=? AND state='scheduled'").run(
        "task-ack:" + id,
      );
      state = "waiting";
      const detail = (pending.action === "ask_human" && pending.question) || pending.detail || pending.question || pending.action;
      progress = "Waiting for you: " + detail;
      const phone = isTextChannel(destination);
      let prompt = progress;
      if (pending.action === "ask_human") {
        const needsNumber = !phone || active(db).some(t => t.id !== id && (destination !== "whatsapp" || t.channel === destination) && t.decision && JSON.parse(t.decision).action === "ask_human");
        prompt = phone ? detail : progress;
        if (needsNumber) prompt += "\nReply with #" + id + " and your answer.";
      } else {
        if (phone) prompt = approvalQuestion(pending.action, detail);
        const reference = "#" + id + " " + pending.approval_id;
        prompt += destination === "whatsapp" && cfg.whatsappSelfChat
          ? "\nReply yes or no."
          : "\nReply approve " + reference + " or deny " + reference + ".";
      }
      if (
        pending.standing_key &&
        db.prepare("SELECT 1 FROM grants WHERE id=?").get(pending.standing_key)
      ) {
        db.prepare(
          "INSERT OR IGNORE INTO commands(id,task,kind,text,decision) VALUES(?,?,'approval','approve',?)",
        ).run("grant:" + pending.approval_id, id, JSON.stringify(pending));
      } else
        notice(
          db,
          "decision:" + id + ":" + pending.approval_id,
          destination,
          prompt,
        );
    }
    if (
      state === "working" &&
      update &&
      update !== task.progress &&
      (!isTextChannel(destination) || update.length <= 500)
    ) {
      const last = db
        .prepare("SELECT max(created) AS time FROM outbox WHERE id LIKE ?")
        .get("milestone:" + id + ":%").time;
      const first = db
        .prepare("SELECT created FROM tasks WHERE id=?")
        .get(id).created;
      if (Date.now() - (last ?? first ?? Date.now()) >= 300000)
        notice(
          db,
          "milestone:" + id + ":" + page.next_after,
          destination,
          isTextChannel(destination) ? update : "#" + id + " " + progress,
        );
    }
    db.prepare(
      "UPDATE tasks SET cursor=?,state=?,progress=?,decision=?,attempts=0,retry=0 WHERE id=?",
    ).run(
      page.next_after ?? cursor,
      state,
      progress,
      pending ? JSON.stringify(pending) : null,
      id,
    );
  });
}

async function command(db, row, api) {
  const t = db.prepare("SELECT * FROM tasks WHERE id=?").get(row.task);
  if (!t.turn && ["queued", "working"].includes(t.state)) return;
  const current = t.decision && JSON.parse(t.decision);
  const original = row.decision && JSON.parse(row.decision);
  let result;
  let quiet = false;
  async function withFiles() {
    const paths = await uploadIncomingFiles(db,t.id,t.session,api,row.id);
    return paths.length ? row.text+"\n\nAttached files:\n"+paths.map(path=>"- "+path).join("\n") : row.text;
  }
  if (!["working", "waiting"].includes(t.state)) {
    // A phone follow-up can arrive just before the worker observes completion.
    if (t.channel === "whatsapp" && t.session && ["answer", "steer"].includes(row.kind)) {
      atomic(db, () => {
        const next = db.prepare(
          "INSERT INTO tasks(request,text,channel,session) VALUES(?,?,?,?)",
        ).run(requestId(row.id), row.text, t.channel, t.session);
        db.prepare("UPDATE incoming_files SET task=?,command=NULL WHERE task=? AND command=?")
          .run(next.lastInsertRowid, t.id, row.id);
        db.prepare("UPDATE commands SET state='done' WHERE id=?").run(row.id);
      });
      return;
    }
    result = "The task already ended.";
  } else if (row.kind === "cancel") {
    const answer = await api("/api/chat/cancel", {
      session_id: t.session,
      turn_id: t.turn,
    });
    result = answer.cancelled
      ? "I’ve stopped that request."
      : "That request is no longer running.";
  } else if (row.kind === "steer") {
    await api("/api/chat/steer", {
      session_id: t.session,
      turn_id: t.turn,
      message_id: row.id,
      message: await withFiles(),
    });
    result = "The running task received your update.";
    quiet = isTextChannel(t.channel);
  } else if (!original || current?.approval_id !== original.approval_id)
    result = "That request has expired.";
  else if (row.kind === "approval") {
    const res = await api("/api/approvals", {
      approval_id: original.approval_id,
      approved: /^(always )?approve/i.test(row.text),
      scope: "once",
    });
    if (
      /^always approve/i.test(row.text) &&
      res.approved &&
      !res.expired &&
      original.standing_key
    )
      db.prepare("INSERT OR REPLACE INTO grants VALUES(?,?)").run(
        original.standing_key,
        original.detail,
      );
    result = res.expired ? "That approval expired." : "Decision recorded.";
    if (isTextChannel(t.channel) && !res.expired) result = res.approved ? "Approved." : "Declined.";
  } else {
    await api(
      "/api/sessions/" +
        t.session +
        "/questions/" +
        encodeURIComponent(original.approval_id),
      { answer: await withFiles() },
    );
    result = "Answer recorded.";
    quiet = isTextChannel(t.channel);
  }
  atomic(db, () => {
    db.prepare("UPDATE commands SET state='done' WHERE id=?").run(row.id);
    if (!quiet) notice(db, "applied:" + row.id, t.channel, isTextChannel(t.channel) ? result : "#" + t.id + " " + result);
  });
}

async function resendFiles(db, api, row, cfg) {
  const task = db.prepare("SELECT * FROM tasks WHERE id=?").get(row.task);
  const saved = db
    .prepare(
      "SELECT f.id,f.name,f.mime,f.hash,b.bytes FROM result_files f JOIN result_bytes b USING(hash) WHERE task=?",
    )
    .all(task.id);
  if (saved.length) {
    atomic(db, () => {
      queueResults(
        db,
        task.id,
        row.channel,
        saved.map((f) => ({
          ...f,
          ref: f.id.slice(String(task.id).length + 1),
          bytes: Buffer.from(f.bytes),
        })),
        "resend:" + row.id,
        cfg,
        notice,
      );
      db.prepare("UPDATE result_requests SET state='done' WHERE id=?").run(
        row.id,
      );
    });
    return;
  }
  // A completed task's cursor points to the last journal event.
  let cursor = Math.max(0, task.cursor - 1);
  let page = await api(
    "/api/sessions/" +
      encodeURIComponent(task.session) +
      "/events?turn_id=" +
      encodeURIComponent(task.turn) +
      "&after_sequence=" +
      cursor +
      "&limit=200",
  );
  let done = page.events.find((e) => e.kind === "done");
  if (!done && !active(db).some((t) => t.id === task.id)) {
    cursor = 0;
    do {
      page = await api(
        "/api/sessions/" +
          encodeURIComponent(task.session) +
          "/events?turn_id=" +
          encodeURIComponent(task.turn) +
          "&after_sequence=" +
          cursor +
          "&limit=200",
      );
      done = page.events.find((e) => e.kind === "done");
      if (!page.has_more || page.next_after <= cursor) break;
      cursor = page.next_after;
    } while (!done);
  }
  const files = done
    ? await collectResults(api, task.session, done.payload, task.text)
    : [];
  atomic(db, () => {
    if (files.length)
      queueResults(
        db,
        task.id,
        row.channel,
        files,
        "resend:" + row.id,
        cfg,
        notice,
      );
    else
      notice(
        db,
        "files:" + row.id,
        row.channel,
        done
          ? "This task has no saved deliverables."
          : "This task is still working.",
      );
    db.prepare("UPDATE result_requests SET state='done' WHERE id=?").run(
      row.id,
    );
  });
}

export function worker(db, api, transcribe, cfg = {}, sharedApi) {
  async function taskApi(task) {
    const previous = db
      .prepare(
        "SELECT r.host FROM task_routes r JOIN tasks t ON t.id=r.task WHERE t.id=? OR (t.session IS NOT NULL AND t.session=?) ORDER BY t.id DESC LIMIT 1",
      )
      .get(task.id, task.session);
    let host = previous?.host || "personal";
    const agent = /^\/agent\s+([a-z0-9][\w.-]{0,79})\s+/i.exec(task.text);
    if (!previous && sharedApi && agent && !task.session) {
      const { agents } = await sharedApi("/api/agents");
      if (agents.some((a) => a.id === agent[1])) host = "shared";
    }
    db.prepare("INSERT OR IGNORE INTO task_routes VALUES(?,?)").run(
      task.id,
      host,
    );
    if (host === "shared" && !sharedApi)
      throw Error("Shared Kageha backend unavailable");
    return host === "shared" ? sharedApi : api;
  }
  let retrieving = false;
  let retryAt = 0;
  const inflight = new Set();
  let lastStarted = 0;
  async function start(t) {
    inflight.add(t.id);
    try {
      const selected = await taskApi(t);
      await advance(db, t, selected, transcribe, cfg, sharedApi);
      for (const c of db
        .prepare(
          "SELECT * FROM commands WHERE task=? AND state='pending' ORDER BY rowid",
        )
        .all(t.id)) {
        try {
          await command(db, c, selected);
        } catch (e) {
          if (![400, 403, 404, 409].includes(e.status)) throw e;
          atomic(db, () => {
            db.prepare("UPDATE commands SET state='failed' WHERE id=?").run(
              c.id,
            );
            notice(
              db,
              "error:" + c.id,
              t.channel,
              isTextChannel(t.channel) ? "I couldn’t apply that change. " + e.message : "#" + t.id + " update could not be applied: " + e.message,
            );
          });
        }
      }
    } catch (e) {
      const permanent =
        [400, 403, 422].includes(e.status) || (!t.session && e.status === 404);
      const delay = Math.min(60, 2 ** Math.min(t.attempts, 6));
      const reason = permanent
        ? "Could not run this task: " + e.message
        : "Waiting for Kageha to reconnect.";
      db.prepare(
        "UPDATE tasks SET state=?,progress=?,attempts=attempts+1,retry=? WHERE id=?",
      ).run(
        permanent ? "failed" : t.state,
        reason,
        Date.now() + delay * 1000,
        t.id,
      );
      if (permanent || t.attempts === 3) {
        db.prepare("DELETE FROM outbox WHERE id=? AND state='scheduled'").run(
          "task-ack:" + t.id,
        );
        notice(
          db,
          "failure:" + t.id + ":" + permanent,
          t.channel,
          isTextChannel(t.channel) ? reason : "#" + t.id + " " + reason,
        );
      }
    } finally {
      inflight.delete(t.id);
    }
  }
  return () => {
    runDue(db);
    const lookup =
      !retrieving &&
      Date.now() >= retryAt &&
      db
        .prepare(
          "SELECT * FROM result_requests WHERE state='pending' ORDER BY rowid LIMIT 1",
        )
        .get();
    if (lookup) {
      retrieving = true;
      void taskApi(
        db.prepare("SELECT * FROM tasks WHERE id=?").get(lookup.task),
      )
        .then((selected) => resendFiles(db, selected, lookup, cfg))
        .catch((err) => {
          if ([400, 403, 404].includes(err.status))
            atomic(db, () => {
              notice(
                db,
                "lookup-error:" + lookup.id,
                lookup.channel,
                "I couldn’t retrieve the files for #" + lookup.task + ".",
              );
              db.prepare(
                "UPDATE result_requests SET state='failed' WHERE id=?",
              ).run(lookup.id);
            });
          retryAt = Date.now() + 10000;
        })
        .finally(() => {
          retrieving = false;
        });
    }
    db.prepare(
      "UPDATE outbox SET state='pending' WHERE state='scheduled' AND retry<=?",
    ).run(Date.now());
    const ready = active(db)
      .reverse()
      .filter((t) => t.retry <= Date.now() && !inflight.has(t.id));
    for (const t of [
      ...ready.filter((t) => t.id > lastStarted),
      ...ready.filter((t) => t.id <= lastStarted),
    ]) {
      if (inflight.size >= 32) break;
      lastStarted = t.id;
      void start(t);
    }
  };
}
