//! `learning.sqlite`: the Run_Journal, failure index, lessons and use tracking.
//!
//! Tables follow the design's data model (`learn_runs`, `learn_events`,
//! `learn_failures`, `learn_lessons`, `learn_lesson_uses`,
//! `learn_skill_uses`). Additions beyond the design:
//! - `learn_schema(version)`: schema version guard.
//! - `learn_reflection_queue(request_id PK, queued_at, state)`: requests
//!   waiting for background reflection (task 23.1). `close_request` sets
//!   `state = 'queued'`.
//! - `learn_event_drops(request_id PK, dropped)`: entries refused after a
//!   request reached [`MAX_EVENTS_PER_REQUEST`].
//! - Indexes `learn_runs(ended)` for the metrics window ([`crate::metrics`]) and
//!   `learn_lesson_uses(request_id)` for the uses of a closing request
//!   ([`crate::retirement`]).
//!
//! `learn_events.body` holds the envelope `{"at": <unix ms>, "body": <entry
//! body>}`, because the design's table has no time column. Use
//! [`SqliteLearning::events`] rather than parsing it directly.
use crate::{
    FailureSignature, JournalEntry, JournalKind, KnownFix, LearnedContext, LearningPort,
    OutcomeStatus, RequestSummary,
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};
use std::{
    fs::{self, OpenOptions},
    io,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// Largest serialized entry body kept as is. Larger bodies are replaced by a
/// truncation marker `{"truncated": true, "bytes": <original>, "head": "..."}`
/// of at most this size.
pub const MAX_EVENT_BODY_BYTES: usize = 64 * 1024;
/// Entries kept per request. Later entries are dropped and counted.
pub const MAX_EVENTS_PER_REQUEST: u64 = 100_000;
const SCHEMA_VERSION: i64 = 1;
const MAX_OUTCOMES: usize = 1024;

pub(crate) fn sql(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.to_string())
}
fn to_i64(value: u64, what: &str) -> io::Result<i64> {
    i64::try_from(value).map_err(|_| invalid(&format!("{what} is out of range")))
}
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
fn text_field(value: &str, max: usize, what: &str, allow_empty: bool) -> io::Result<()> {
    if (!allow_empty && value.is_empty())
        || value.len() > max
        || value.chars().any(char::is_control)
    {
        return Err(invalid(&format!("Invalid {what}")));
    }
    Ok(())
}

/// A task type is a short slug: 1–64 bytes of `[a-z0-9_-]`.
pub fn valid_task_type(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// `$KAGEHA_HOME/learning.sqlite`, else `~/.kageha/learning.sqlite`.
pub fn default_path() -> Option<PathBuf> {
    std::env::var_os("KAGEHA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|v| !v.is_empty())
                .map(|home| PathBuf::from(home).join(".kageha"))
        })
        .map(|root| root.join("learning.sqlite"))
}

/// Serializes `body`, replacing it with a truncation marker when it is larger
/// than [`MAX_EVENT_BODY_BYTES`]. The result is always within the limit.
pub(crate) fn bounded_body(body: &Value) -> io::Result<String> {
    let text = serde_json::to_string(body)?;
    if text.len() <= MAX_EVENT_BODY_BYTES {
        return Ok(text);
    }
    // Escaping can only grow the head, so shrinking it by the overflow
    // converges in a few rounds.
    let mut budget = MAX_EVENT_BODY_BYTES - 128;
    loop {
        let mut cut = budget.min(text.len());
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        let marker =
            json!({"truncated": true, "bytes": text.len(), "head": &text[..cut]}).to_string();
        if marker.len() <= MAX_EVENT_BODY_BYTES {
            return Ok(marker);
        }
        budget = cut.saturating_sub(marker.len() - MAX_EVENT_BODY_BYTES);
    }
}

/// One stored Run_Journal entry.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredEvent {
    pub seq: u64,
    pub kind: JournalKind,
    pub body: Value,
    pub at: u64,
}

/// [`LearningPort`] backed by `learning.sqlite`.
pub struct SqliteLearning {
    db: Mutex<Connection>,
    pub(crate) event_limit: u64,
}

impl SqliteLearning {
    /// Opens the store at [`default_path`].
    pub fn open_default() -> io::Result<Self> {
        let path = default_path().ok_or_else(|| invalid("KAGEHA_HOME or HOME is required"))?;
        Self::open(&path)
    }

    /// Opens or creates the store. The directory is created `0700`, the file
    /// `0600` without following symlinks. An existing directory must not be
    /// a symlink or writable by group or others.
    pub fn open(path: &Path) -> io::Result<Self> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| invalid("Learning store requires a directory"))?;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
        let meta = fs::symlink_metadata(parent)?;
        if !meta.is_dir() || meta.file_type().is_symlink() || meta.permissions().mode() & 0o022 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Learning store directory must be private",
            ));
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        let file_meta = file.metadata()?;
        if !file_meta.is_file() {
            return Err(invalid("Learning store must be a regular file"));
        }
        if file_meta.permissions().mode() & 0o077 != 0 {
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        drop(file);
        let db = Connection::open(path).map_err(sql)?;
        db.busy_timeout(Duration::from_secs(5)).map_err(sql)?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS learn_schema(version INTEGER NOT NULL);")
            .map_err(sql)?;
        let version: Option<i64> = db
            .query_row("SELECT version FROM learn_schema LIMIT 1", [], |r| r.get(0))
            .optional()
            .map_err(sql)?;
        if version.is_some_and(|v| v != SCHEMA_VERSION) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Unsupported learning store version",
            ));
        }
        db.execute_batch(
            "PRAGMA journal_mode=WAL;PRAGMA synchronous=NORMAL;BEGIN IMMEDIATE;
            INSERT INTO learn_schema SELECT 1 WHERE NOT EXISTS(SELECT 1 FROM learn_schema);
            CREATE TABLE IF NOT EXISTS learn_runs(request_id TEXT PRIMARY KEY,agent TEXT NOT NULL,chat_id TEXT NOT NULL,task_type TEXT,started INTEGER NOT NULL,ended INTEGER NOT NULL,status TEXT NOT NULL,model_calls INTEGER NOT NULL,cost_usd REAL NOT NULL,cached_tokens INTEGER NOT NULL,outcomes TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS learn_runs_task ON learn_runs(task_type,ended);
            CREATE INDEX IF NOT EXISTS learn_runs_ended ON learn_runs(ended);
            CREATE TABLE IF NOT EXISTS learn_events(request_id TEXT NOT NULL,seq INTEGER NOT NULL,kind TEXT NOT NULL,body TEXT NOT NULL,PRIMARY KEY(request_id,seq));
            CREATE TABLE IF NOT EXISTS learn_failures(signature TEXT NOT NULL,fix_hash TEXT NOT NULL,example TEXT NOT NULL,steps TEXT NOT NULL,successes INTEGER NOT NULL DEFAULT 0,failures INTEGER NOT NULL DEFAULT 0,last_seen INTEGER NOT NULL,PRIMARY KEY(signature,fix_hash));
            CREATE TABLE IF NOT EXISTS learn_lessons(id INTEGER PRIMARY KEY,agent TEXT NOT NULL,task_type TEXT,\"trigger\" TEXT NOT NULL,action TEXT NOT NULL,status TEXT NOT NULL,support TEXT NOT NULL,shown INTEGER NOT NULL DEFAULT 0,created INTEGER NOT NULL,retired_at INTEGER);
            CREATE INDEX IF NOT EXISTS learn_lessons_agent ON learn_lessons(agent,status);
            CREATE TABLE IF NOT EXISTS learn_lesson_uses(lesson_id INTEGER NOT NULL,request_id TEXT NOT NULL,verified INTEGER,PRIMARY KEY(lesson_id,request_id));
            CREATE INDEX IF NOT EXISTS learn_lesson_uses_request ON learn_lesson_uses(request_id);
            CREATE TABLE IF NOT EXISTS learn_skill_uses(skill TEXT NOT NULL,revision INTEGER NOT NULL,request_id TEXT NOT NULL,success INTEGER,at INTEGER NOT NULL,PRIMARY KEY(skill,request_id));
            CREATE TABLE IF NOT EXISTS learn_reflection_queue(request_id TEXT PRIMARY KEY,queued_at INTEGER NOT NULL,state TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS learn_reflection_state ON learn_reflection_queue(state,queued_at);
            CREATE TABLE IF NOT EXISTS learn_event_drops(request_id TEXT PRIMARY KEY,dropped INTEGER NOT NULL);
            COMMIT;",
        )
        .map_err(sql)?;
        crate::lessons::migrate(&db).map_err(sql)?;
        crate::skills::migrate(&db).map_err(sql)?;
        Ok(Self {
            db: Mutex::new(db),
            event_limit: MAX_EVENTS_PER_REQUEST,
        })
    }

    pub(crate) fn lock(&self) -> io::Result<std::sync::MutexGuard<'_, Connection>> {
        self.db
            .lock()
            .map_err(|_| io::Error::other("Learning store lock poisoned"))
    }

    /// Up to `limit` entries of a request with `seq > after`, in order.
    pub fn events(&self, request: &str, after: u64, limit: usize) -> io::Result<Vec<StoredEvent>> {
        let db = self.lock()?;
        let mut query = db
            .prepare(
                "SELECT seq,kind,body FROM learn_events WHERE request_id=? AND seq>? ORDER BY seq LIMIT ?",
            )
            .map_err(sql)?;
        let rows = query
            .query_map(
                params![
                    request,
                    to_i64(after, "seq")?,
                    limit.min(i64::MAX as usize) as i64
                ],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )
            .map_err(sql)?;
        rows.map(|row| {
            let (seq, kind, body) = row.map_err(sql)?;
            let mut envelope: Value = serde_json::from_str(&body)?;
            Ok(StoredEvent {
                seq: seq as u64,
                kind: JournalKind::parse(&kind).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "Unknown journal kind")
                })?,
                at: envelope["at"].as_u64().unwrap_or(0),
                body: envelope["body"].take(),
            })
        })
        .collect()
    }

    /// Entries refused for a request after it reached the per-request limit.
    pub fn dropped_events(&self, request: &str) -> io::Result<u64> {
        let db = self.lock()?;
        let dropped: Option<i64> = db
            .query_row(
                "SELECT dropped FROM learn_event_drops WHERE request_id=?",
                [request],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql)?;
        Ok(dropped.unwrap_or(0) as u64)
    }

    /// The stored summary of a closed request.
    pub fn run(&self, request: &str) -> io::Result<Option<RequestSummary>> {
        let db = self.lock()?;
        let row = db
            .query_row(
                "SELECT agent,chat_id,task_type,started,ended,status,model_calls,cost_usd,cached_tokens,outcomes FROM learn_runs WHERE request_id=?",
                [request],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, i64>(6)?,
                        r.get::<_, f64>(7)?,
                        r.get::<_, i64>(8)?,
                        r.get::<_, String>(9)?,
                    ))
                },
            )
            .optional()
            .map_err(sql)?;
        row.map(
            |(agent, chat_id, task_type, started, ended, status, calls, cost, cached, outcomes)| {
                Ok(RequestSummary {
                    agent,
                    chat_id,
                    task_type,
                    status,
                    model_calls: calls as u64,
                    cost_usd: cost,
                    cached_tokens: cached as u64,
                    outcomes: serde_json::from_str::<Vec<OutcomeStatus>>(&outcomes)?,
                    started: started as u64,
                    ended: ended as u64,
                })
            },
        )
        .transpose()
    }

    /// Reflection queue state (`queued`, `running`, ...) and queue time.
    pub fn reflection_state(&self, request: &str) -> io::Result<Option<(String, u64)>> {
        let db = self.lock()?;
        db.query_row(
            "SELECT state,queued_at FROM learn_reflection_queue WHERE request_id=?",
            [request],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64)),
        )
        .optional()
        .map_err(sql)
    }
}

fn check_request(request: &str) -> io::Result<()> {
    text_field(request, 256, "request id", false)
}

fn check_summary(summary: &RequestSummary) -> io::Result<()> {
    text_field(&summary.agent, 128, "agent", false)?;
    text_field(&summary.chat_id, 256, "chat id", true)?;
    text_field(&summary.status, 64, "status", false)?;
    if summary
        .task_type
        .as_deref()
        .is_some_and(|t| !valid_task_type(t))
    {
        return Err(invalid("Invalid task type"));
    }
    if !summary.cost_usd.is_finite() || summary.cost_usd < 0.0 {
        return Err(invalid("Invalid cost"));
    }
    if summary.ended < summary.started {
        return Err(invalid("Request ended before it started"));
    }
    if summary.outcomes.len() > MAX_OUTCOMES {
        return Err(invalid("Too many outcomes"));
    }
    for outcome in &summary.outcomes {
        text_field(&outcome.id, 256, "outcome id", false)?;
        text_field(&outcome.status, 64, "outcome status", false)?;
    }
    Ok(())
}

impl LearningPort for SqliteLearning {
    fn journal(&self, request: &str, entry: JournalEntry) -> io::Result<()> {
        check_request(request)?;
        let body = bounded_body(&entry.body)?;
        let envelope = format!("{{\"at\":{},\"body\":{body}}}", entry.at);
        let mut db = self.lock()?;
        // IMMEDIATE takes the write lock before reading MAX(seq), so writers
        // on other connections cannot be handed the same seq.
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql)?;
        let last: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(seq),0) FROM learn_events WHERE request_id=?",
                [request],
                |r| r.get(0),
            )
            .map_err(sql)?;
        if last as u64 >= self.event_limit {
            tx.execute(
                "INSERT INTO learn_event_drops(request_id,dropped) VALUES(?,1)
                 ON CONFLICT(request_id) DO UPDATE SET dropped=dropped+1",
                [request],
            )
            .map_err(sql)?;
        } else {
            tx.execute(
                "INSERT INTO learn_events(request_id,seq,kind,body) VALUES(?,?,?,?)",
                params![request, last + 1, entry.kind.as_str(), envelope],
            )
            .map_err(sql)?;
        }
        tx.commit().map_err(sql)
    }

    fn known_fix(&self, signature: &FailureSignature) -> io::Result<Option<KnownFix>> {
        // Failure index: see `failures`.
        self.lookup_known_fix(signature)
    }

    fn context_for(
        &self,
        agent: &str,
        request_text: &str,
        task_type: Option<&str>,
    ) -> io::Result<LearnedContext> {
        // Lessons only; the Host passes its skill candidates to `context_with`.
        self.context_with(agent, request_text, task_type, &[])
    }

    fn close_request(&self, request: &str, summary: RequestSummary) -> io::Result<()> {
        check_request(request)?;
        check_summary(&summary)?;
        let outcomes = serde_json::to_string(&summary.outcomes)?;
        let mut db = self.lock()?;
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql)?;
        tx.execute(
            "INSERT INTO learn_runs(request_id,agent,chat_id,task_type,started,ended,status,model_calls,cost_usd,cached_tokens,outcomes)
             VALUES(?,?,?,?,?,?,?,?,?,?,?)
             ON CONFLICT(request_id) DO UPDATE SET agent=excluded.agent,chat_id=excluded.chat_id,task_type=excluded.task_type,
             started=excluded.started,ended=excluded.ended,status=excluded.status,model_calls=excluded.model_calls,
             cost_usd=excluded.cost_usd,cached_tokens=excluded.cached_tokens,outcomes=excluded.outcomes",
            params![
                request,
                summary.agent,
                summary.chat_id,
                summary.task_type,
                to_i64(summary.started, "start time")?,
                to_i64(summary.ended, "end time")?,
                summary.status,
                to_i64(summary.model_calls, "model calls")?,
                summary.cost_usd,
                to_i64(summary.cached_tokens, "cached tokens")?,
                outcomes,
            ],
        )
        .map_err(sql)?;
        // A request closed again (for example after a resume) is reflected
        // again, unless reflection is running right now.
        tx.execute(
            "INSERT INTO learn_reflection_queue(request_id,queued_at,state) VALUES(?,?,'queued')
             ON CONFLICT(request_id) DO UPDATE SET queued_at=excluded.queued_at,state='queued'
             WHERE learn_reflection_queue.state<>'running'",
            params![request, to_i64(now_ms(), "queue time")?],
        )
        .map_err(sql)?;
        tx.commit().map_err(sql)
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
