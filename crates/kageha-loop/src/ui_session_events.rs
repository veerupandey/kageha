//! Durable, read-only session feed. Transport reconnect never admits execution.
use crate::{
    server,
    webui::{self, Database},
};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{
    io::{self, Read, Write},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Condvar, Mutex, OnceLock,
    },
    time::Duration,
};
use tiny_http::Request;
static STREAMS: AtomicUsize = AtomicUsize::new(0);
static WAKE: OnceLock<(Mutex<u64>, Condvar)> = OnceLock::new();
const MAX_STREAMS: usize = 8;
fn wake() -> &'static (Mutex<u64>, Condvar) {
    WAKE.get_or_init(|| (Mutex::new(0), Condvar::new()))
}
pub(crate) fn notify() {
    if let Ok(mut version) = wake().0.lock() {
        *version = version.wrapping_add(1);
        wake().1.notify_all();
    }
}
fn sql(error: impl std::fmt::Display) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(io::Error::other(error.to_string())))
}
pub(crate) fn watermark(db: &Connection, chat: &str) -> rusqlite::Result<i64> {
    db.query_row(
        "SELECT COALESCE(MAX(sequence),0) FROM ui_events WHERE chat_id=?",
        [chat],
        |r| r.get(0),
    )
}
pub(crate) fn worker_pending(db: &Connection, child: &str) -> rusqlite::Result<Option<Value>> {
    let rows = crate::ui_approvals::owned(db)
        .map_err(sql)?
        .pending_family(child)
        .map_err(sql)?;
    Ok(rows
        .iter()
        .rev()
        .find(|r| r.request.authority.session == child)
        .map(|r| json!({"detail":r.request.detail,"approval_id":r.id})))
}
pub(crate) fn workers(db: &Connection, chat: &str) -> rusqlite::Result<Vec<Value>> {
    let mut query=db.prepare("WITH RECURSIVE family(chat,depth) AS (SELECT ?,0 UNION ALL SELECT j.child_chat,f.depth+1 FROM ui_jobs j JOIN family f ON j.parent_chat=f.chat WHERE f.depth<4 AND j.child_chat<>f.chat) SELECT DISTINCT j.id,j.created FROM ui_jobs j JOIN family f ON j.parent_chat=f.chat OR j.child_chat=f.chat ORDER BY j.created DESC,j.id LIMIT 1000")?;
    let ids = query
        .query_map(params![chat], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ids.iter()
        .map(|id| crate::ui_jobs::job(db, id))
        .collect::<rusqlite::Result<Vec<_>>>()
        .map(|rows| rows.into_iter().flatten().collect())
}
fn append(
    db: &Connection,
    chat: &str,
    turn: &str,
    event: &str,
    mut payload: Value,
) -> rusqlite::Result<()> {
    payload["session_id"] = json!(chat);
    payload["thread_id"] = json!(format!("web-{chat}"));
    payload["turn_id"] = json!(turn);
    db.execute(
        "INSERT INTO ui_events(chat_id,turn_id,event,payload) VALUES (?,?,?,?)",
        params![chat, turn, event, payload.to_string()],
    )?;
    Ok(())
}
// Bound each replay page without skipping rows. A single large original event is
// still delivered intact; subsequent events remain behind the returned cursor.
fn replay_page(db: &Connection, chat: &str, after: i64) -> rusqlite::Result<Vec<Value>> {
    let mut statement=db.prepare("SELECT sequence,turn_id,event,payload FROM ui_events WHERE chat_id=? AND sequence>? ORDER BY sequence LIMIT 100")?;
    let mut rows = statement.query(params![chat, after])?;
    let mut frames = Vec::new();
    let mut size = 0;
    while let Some(row) = rows.next()? {
        let payload: String = row.get(3)?;
        if !frames.is_empty() && size + payload.len() > 2 * 1024 * 1024 {
            break;
        }
        size += payload.len();
        frames.push(webui::event_frame(
            row.get(0)?,
            &row.get::<_, String>(1)?,
            &row.get::<_, String>(2)?,
            &serde_json::from_str(&payload).unwrap_or(json!({})),
        ));
    }
    Ok(frames)
}
fn audiences(db: &Connection, parent: &str) -> rusqlite::Result<Vec<String>> {
    let mut rows = Vec::new();
    let mut current = parent.to_owned();
    for _ in 0..4 {
        if rows.contains(&current) {
            break;
        }
        rows.push(current.clone());
        let next: Option<String> = db
            .query_row(
                "SELECT parent_chat FROM ui_jobs WHERE child_chat=? LIMIT 1",
                [&current],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(next) = next {
            current = next
        } else {
            break;
        }
    }
    Ok(rows)
}
/// Called in the same transaction as the authoritative lifecycle projection.
pub(crate) fn publish_worker(db: &Connection, id: &str, turn: &str) -> rusqlite::Result<()> {
    if let Some(job) = crate::ui_jobs::job(db, id)? {
        if let Some(parent) = job["parent_session"].as_str() {
            for audience in audiences(db, parent)? {
                append(
                    db,
                    &audience,
                    turn,
                    "worker",
                    json!({"job":job,"job_id":id,"source_session_id":job["session_id"],"source_turn_id":turn}),
                )?;
            }
        }
    }
    Ok(())
}
/// Project child activity into its parent without recursively duplicating it.
/// Parent tool cards keep their own turn identity; child cards are a distinct kind.
pub(crate) fn project_child_event(
    db: &Connection,
    chat: &str,
    turn: &str,
    event: &str,
    payload: &Value,
) -> rusqlite::Result<()> {
    if matches!(event, "worker" | "worker_activity") {
        return Ok(());
    }
    let row: Option<(String, String)> = db
        .query_row(
            "SELECT id,parent_chat FROM ui_jobs WHERE child_chat=? ORDER BY created LIMIT 1",
            [chat],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((id, parent)) = row {
        for audience in audiences(db, &parent)? {
            append(
                db,
                &audience,
                turn,
                "worker_activity",
                json!({"job_id":id,"source_session_id":chat,"source_turn_id":turn,"activity":webui::event_frame(0,turn,event,payload)}),
            )?;
        }
        publish_worker(db, &id, turn)?;
    }
    notify();
    Ok(())
}

struct SessionEvents {
    db: Database,
    chat: String,
    cursor: i64,
    bytes: io::Cursor<Vec<u8>>,
}
impl Drop for SessionEvents {
    fn drop(&mut self) {
        STREAMS.fetch_sub(1, Ordering::AcqRel);
    }
}
impl Read for SessionEvents {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        loop {
            let count = self.bytes.read(output)?;
            if count != 0 {
                return Ok(count);
            }
            let observed = *wake()
                .0
                .lock()
                .map_err(|_| io::Error::other("Session event notifier unavailable"))?;
            let frames = {
                let db = self
                    .db
                    .lock()
                    .map_err(|_| io::Error::other("Database unavailable"))?;
                let exists: bool = db
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM ui_sessions WHERE id=?)",
                        [&self.chat],
                        |r| r.get(0),
                    )
                    .map_err(io::Error::other)?;
                if !exists {
                    return Ok(0);
                }
                replay_page(&db, &self.chat, self.cursor).map_err(io::Error::other)?
            };
            if !frames.is_empty() {
                self.cursor = frames
                    .last()
                    .and_then(|f| f["sequence"].as_i64())
                    .unwrap_or(self.cursor);
                self.bytes = io::Cursor::new(
                    format!(
                        "id: {}\nevent: update\ndata: {}\n\n",
                        self.cursor,
                        json!({"events":frames})
                    )
                    .into_bytes(),
                );
                continue;
            }
            // No database guard is held during either sleep or socket writes.
            let version = wake()
                .0
                .lock()
                .map_err(|_| io::Error::other("Session event notifier unavailable"))?;
            let _ = wake()
                .1
                .wait_timeout_while(version, Duration::from_secs(1), |v| *v == observed)
                .map_err(|_| io::Error::other("Session event notifier unavailable"))?;
            self.bytes = io::Cursor::new(b": heartbeat\n\n".to_vec());
        }
    }
}
pub(crate) fn stream(request: Request, db: Database, chat: String, after: i64) -> io::Result<()> {
    if after < 0 {
        server::response(
            request,
            400,
            json!({"error":"after_sequence must be nonnegative"}),
        );
        return Ok(());
    }
    if STREAMS
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            (n < MAX_STREAMS).then_some(n + 1)
        })
        .is_err()
    {
        server::response(
            request,
            503,
            json!({"error":"Session stream capacity reached; reconnect with backoff"}),
        );
        return Ok(());
    }
    // Snapshot and watermark share one application mutex/SQLite read boundary.
    // Keep cursor=after: old events may still be needed by an admission handshake.
    let snapshot = (|| -> io::Result<Value> {
        let connection = db
            .lock()
            .map_err(|_| io::Error::other("Database unavailable"))?;
        let exists: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM ui_sessions WHERE id=?)",
                [&chat],
                |r| r.get(0),
            )
            .map_err(io::Error::other)?;
        if !exists {
            return Err(io::Error::new(io::ErrorKind::NotFound, "Session not found"));
        }
        Ok(
            json!({"sequence":watermark(&connection,&chat).map_err(io::Error::other)?,"session_id":chat,"workers":workers(&connection,&chat).map_err(io::Error::other)?,"live":webui::session(&connection,&chat).map_err(io::Error::other)?}),
        )
    })();
    let snapshot = match snapshot {
        Ok(v) => v,
        Err(e) => {
            STREAMS.fetch_sub(1, Ordering::AcqRel);
            server::response(
                request,
                if e.kind() == io::ErrorKind::NotFound {
                    404
                } else {
                    500
                },
                json!({"error":e.to_string()}),
            );
            return Ok(());
        }
    };
    let mut reader = SessionEvents {
        db,
        chat,
        cursor: after,
        bytes: io::Cursor::new(format!("event: snapshot\ndata: {snapshot}\n\n").into_bytes()),
    };
    // tiny_http's normal chunk encoder buffers 8 KiB before flushing. That
    // delays small tool/worker events indefinitely on an otherwise idle chat.
    // Own this response's chunk framing so every event and heartbeat flushes.
    let mut writer = request.into_writer();
    // The feed only reads `ui_events`; turns run on their own workers. A client that
    // goes away (a closed tab, a dropped network) ends this response and nothing else.
    match write_feed(&mut reader, &mut *writer) {
        Err(error) if server::client_disconnected(&error) => Ok(()),
        result => result,
    }
}
fn write_feed(reader: &mut impl Read, writer: &mut dyn Write) -> io::Result<()> {
    writer.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache, no-store\r\nX-Accel-Buffering: no\r\nTransfer-Encoding: chunked\r\n\r\n")?;
    writer.flush()?;
    let mut buffer = [0u8; 8192];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            writer.write_all(b"0\r\n\r\n")?;
            return writer.flush();
        }
        write!(writer, "{count:x}\r\n")?;
        writer.write_all(&buffer[..count])?;
        writer.write_all(b"\r\n")?;
        writer.flush()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_replay_pages_do_not_skip_events() {
        let db = db();
        for i in 0..105 {
            append(
                &db,
                "parent",
                "turn",
                "delta",
                json!({"text":i.to_string()}),
            )
            .unwrap();
        }
        let first = replay_page(&db, "parent", 0).unwrap();
        assert_eq!(first.len(), 100);
        let after = first.last().unwrap()["sequence"].as_i64().unwrap();
        let rest = replay_page(&db, "parent", after).unwrap();
        assert_eq!(rest.len(), 5);
        assert_eq!(rest[0]["sequence"], after + 1);
        assert_eq!(
            rest.last().unwrap()["sequence"],
            watermark(&db, "parent").unwrap()
        );
    }
    #[test]
    fn spawn_cards_keep_worker_identity_on_session_replay() {
        let db = db();
        let job = json!({"id":"job","objective":"read source","state":"running",
            "spec":{"persona_id":"main"},"padding":"x".repeat(10_000)});
        let original = job.to_string();
        webui::insert_event(&db, "parent", "parent-turn", "tool_card", json!({
            "id":"spawn-call","name":"agent_spawn","status":"done","result_preview":original
        })).unwrap();
        let first = replay_page(&db, "parent", 0).unwrap();
        let card = &first[0]["tool_card"];
        assert_eq!(card["subagent_graph"][0]["job_id"], "job");
        assert_eq!(card["subagent_graph"][0]["task"], "read source");
        assert_eq!(card["subagent_graph"][0]["status"], "running");
        assert_eq!(card["result_preview"], job.to_string());
        let cursor = first[0]["sequence"].as_i64().unwrap();
        assert!(replay_page(&db, "parent", cursor).unwrap().is_empty());
        assert_eq!(replay_page(&db, "parent", 0).unwrap(), first);
    }
    #[test]
    fn worker_projection_lists_only_registered_child_artifacts() {
        let db = db();
        db.execute("INSERT INTO ui_uploads(chat_id,path,filename,mime,sha256,size) VALUES ('child','workspace/outputs/worker.py','worker.py','text/plain','abc',1)",[]).unwrap();
        db.execute("INSERT INTO ui_turn_artifacts VALUES ('child','child-turn','workspace/outputs/worker.py')",[]).unwrap();
        db.execute("INSERT INTO ui_turn_artifacts VALUES ('child','child-turn','workspace/outputs/unregistered.py')",[]).unwrap();
        db.execute("INSERT INTO ui_uploads(chat_id,path,filename,mime,sha256,size) VALUES ('other','workspace/outputs/other.py','other.py','text/plain','abc',1)",[]).unwrap();
        db.execute("INSERT INTO ui_turn_artifacts VALUES ('other','other-turn','workspace/outputs/other.py')",[]).unwrap();
        let jobs=workers(&db,"parent").unwrap();
        assert_eq!(jobs[0]["artifacts"],json!(["workspace/outputs/worker.py"]));
    }
    fn db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE native_chat_turns(sequence INTEGER PRIMARY KEY,chat_id TEXT,request_id TEXT,message TEXT,answer TEXT,status TEXT);").unwrap();
        webui::initialize(&db).unwrap();
        for chat in ["parent", "child", "other"] {
            db.execute(
                "INSERT INTO ui_sessions(id,title,created_at,updated_at) VALUES (?,'fixture',0,0)",
                [chat],
            )
            .unwrap();
        }
        db.execute("INSERT INTO ui_jobs VALUES ('job','parent','child','main','read source','running',3,0,NULL,'request',1)",[]).unwrap();
        db
    }
    #[test]
    fn worker_and_child_tools_share_parent_journal_with_stable_identity() {
        let mut db = db();
        let tx = db.transaction().unwrap();
        webui::insert_event(
            &tx,
            "child",
            "child-turn",
            "tool_card",
            json!({"id":"read-1","name":"read_file","status":"done","result_preview":"source"}),
        )
        .unwrap();
        tx.commit().unwrap();
        let frames = webui::event_frames(&db, "parent", "", 0).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0]["kind"], "worker_activity");
        assert_eq!(frames[0]["job_id"], "job");
        assert_eq!(frames[0]["source_session_id"], "child");
        assert_eq!(frames[0]["session_id"], "parent");
        assert_eq!(frames[0]["turn_id"], "child-turn");
        assert_eq!(frames[0]["activity"]["kind"], "tool_card");
        assert_eq!(frames[1]["kind"], "worker");
        assert_eq!(frames[1]["job"]["state"], "running");
        assert!(frames[0]["sequence"].as_i64() < frames[1]["sequence"].as_i64());
        assert!(webui::event_frames(&db, "other", "", 0).unwrap().is_empty());
        assert!(
            webui::event_frames(&db, "parent", "parent-turn", 0)
                .unwrap()
                .is_empty(),
            "child tools must not become parent turn tools"
        );
        let after = frames[0]["sequence"].as_i64().unwrap();
        assert_eq!(
            webui::event_frames(&db, "parent", "", after).unwrap().len(),
            1
        );
        // Reading snapshots/replay cannot create runs or more events.
        let checkpoint = watermark(&db, "parent").unwrap();
        let snapshot = workers(&db, "parent").unwrap();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0]["id"], "job");
        assert_eq!(watermark(&db, "parent").unwrap(), checkpoint);
        let runs: i64 = db
            .query_row("SELECT count(*) FROM ui_runtime_runs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(runs, 0);
    }
    #[test]
    fn child_projection_rolls_back_with_its_tool_record() {
        let mut db = db();
        let tx = db.transaction().unwrap();
        webui::insert_event(
            &tx,
            "child",
            "child-turn",
            "tool_card",
            json!({"id":"x","status":"running"}),
        )
        .unwrap();
        tx.rollback().unwrap();
        assert_eq!(watermark(&db, "child").unwrap(), 0);
        assert_eq!(watermark(&db, "parent").unwrap(), 0);
    }
    #[test]
    fn parent_done_does_not_hide_later_worker_events() {
        let db = db();
        webui::insert_event(
            &db,
            "parent",
            "parent-turn",
            "done",
            json!({"status":"success"}),
        )
        .unwrap();
        let after = watermark(&db, "parent").unwrap();
        db.execute(
            "UPDATE ui_jobs SET state='succeeded',revision=4,result=? WHERE id='job'",
            [json!({"message":"finished","job_reply_id":"job:job:result"}).to_string()],
        )
        .unwrap();
        publish_worker(&db, "job", "child-turn").unwrap();
        let later = webui::event_frames(&db, "parent", "", after).unwrap();
        assert_eq!(later.len(), 1);
        assert_eq!(later[0]["job"]["state"], "succeeded");
        assert_eq!(later[0]["job"]["result"]["job_reply_id"], "job:job:result");
    }
    #[test]
    fn approval_updates_are_streamed_only_for_the_worker_that_owns_them() {
        use kageha_contracts::policy::*;
        let db = db();
        db.execute(
            "INSERT INTO ui_sessions VALUES ('sibling','fixture',0,0)",
            [],
        )
        .unwrap();
        db.execute("INSERT INTO ui_jobs VALUES ('other-job','parent','sibling','main','other work','running',3,0,NULL,'request',1)",[]).unwrap();
        let journal = crate::ui_approvals::owned(&db).unwrap();
        journal.delegate("parent", "child").unwrap();
        journal.delegate("parent", "sibling").unwrap();
        let request = ApprovalRequest {
            authority: Authority {
                session: "child".into(),
                turn: "child-turn".into(),
                binding: "browser.private".into(),
                generation: 1,
                resource: "tab".into(),
                input_digest: "a".repeat(64),
            },
            action: "browser_click".into(),
            class: ActionClass::Mutation,
            detail: "Click the observed control".into(),
        };
        let approval = journal.create("approval", &request, false).unwrap();
        webui::insert_event(
            &db,
            "child",
            "child-turn",
            "event",
            json!({"kind":"approval_required","payload":{"approval_id":approval.id}}),
        )
        .unwrap();
        let snapshot = workers(&db, "parent").unwrap();
        assert_eq!(
            snapshot.iter().find(|j| j["id"] == "job").unwrap()["state"],
            "waiting_input"
        );
        assert_eq!(
            snapshot.iter().find(|j| j["id"] == "other-job").unwrap()["state"],
            "running"
        );
        journal
            .decide(
                "approval",
                &ApprovalDecision {
                    expected_revision: approval.revision,
                    authority: request.authority,
                    approved: false,
                    scope: GrantScope::Once,
                },
            )
            .unwrap();
        webui::insert_event(
            &db,
            "child",
            "child-turn",
            "event",
            json!({"kind":"approval_resolved","payload":{"approval_id":"approval"}}),
        )
        .unwrap();
        let frames = webui::event_frames(&db, "parent", "", 0).unwrap();
        assert_eq!(frames.last().unwrap()["job"]["state"], "running");
    }
}

#[cfg(test)]
mod tree_tests {
    use super::*;
    #[test]
    fn grandchild_updates_reach_root_but_not_sibling_chats() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE native_chat_turns(sequence INTEGER PRIMARY KEY,chat_id TEXT,request_id TEXT,message TEXT,answer TEXT,status TEXT);").unwrap();
        crate::webui::initialize(&db).unwrap();
        for (id, parent, child, depth) in [
            ("one", "root", "child", 1),
            ("two", "child", "grandchild", 2),
            ("other", "elsewhere", "sibling", 1),
        ] {
            db.execute(
                "INSERT INTO ui_jobs VALUES (?,?,?,'main','review','running',1,1,NULL,?,?)",
                params![id, parent, child, id, depth],
            )
            .unwrap();
        }
        assert_eq!(workers(&db, "root").unwrap().len(), 2);
        project_child_event(
            &db,
            "grandchild",
            "turn",
            "tool_card",
            &json!({"id":"read","name":"read_file","status":"running"}),
        )
        .unwrap();
        let root = webui::event_frames(&db, "root", "", 0).unwrap();
        assert!(root
            .iter()
            .any(|frame| frame["kind"] == "worker_activity" && frame["job_id"] == "two"));
        assert!(webui::event_frames(&db, "elsewhere", "", 0)
            .unwrap()
            .is_empty());
        let grandchild = workers(&db, "root")
            .unwrap()
            .into_iter()
            .find(|job| job["id"] == "two")
            .unwrap();
        assert_eq!(grandchild["parent_job_id"], "one");
        assert_eq!(grandchild["root_session_id"], "root");
    }
}
