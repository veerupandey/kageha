use fs2::FileExt;
use kageha_contracts::execution::{uncertain_stop, Control, ToolContext, ToolReply};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io,
    path::Path,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
pub struct Journal {
    db: Mutex<Connection>,
    lanes: Mutex<HashMap<String, Weak<Mutex<()>>>>,
    _lock: std::fs::File,
}
fn err(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(format!("Execution journal: {e}"))
}
impl Journal {
    pub fn open(path: &Path) -> io::Result<Self> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        if let Some(parent) = path.parent() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)?;
        }
        for p in [path.to_owned(), path.with_extension("lock")] {
            if std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(err("Journal cannot be a symlink"));
            }
        }
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(path.with_extension("lock"))?;
        lock.try_lock_exclusive()
            .map_err(|_| err("Another execution writer is active"))?;
        drop(
            std::fs::OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .mode(0o600)
                .open(path)?,
        );
        let db = Connection::open(path).map_err(err)?;
        let version: u32 = db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(err)?;
        if version > 1 {
            return Err(err("Execution schema is newer than this client"));
        }
        {
            let mut q=db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'").map_err(err)?;
            for name in q.query_map([], |r| r.get::<_, String>(0)).map_err(err)? {
                if !matches!(
                    name.map_err(err)?.as_str(),
                    "execution_receipts" | "execution_acks" | "execution_tombstones"
                ) {
                    return Err(err("Expected a dedicated execution journal"));
                }
            }
        }
        db.execute_batch("PRAGMA journal_mode=WAL;PRAGMA synchronous=FULL;CREATE TABLE IF NOT EXISTS execution_receipts(receipt TEXT PRIMARY KEY,session TEXT NOT NULL,turn TEXT NOT NULL,call TEXT NOT NULL,target TEXT NOT NULL,digest TEXT NOT NULL,state TEXT NOT NULL,result TEXT,UNIQUE(session,turn,call));CREATE TABLE IF NOT EXISTS execution_acks(receipt TEXT PRIMARY KEY,target TEXT NOT NULL);CREATE TABLE IF NOT EXISTS execution_tombstones(session TEXT PRIMARY KEY);UPDATE execution_receipts SET state='uncertain' WHERE state='pending';PRAGMA user_version=1;").map_err(err)?;
        Ok(Self {
            db: Mutex::new(db),
            lanes: Mutex::new(HashMap::new()),
            _lock: lock,
        })
    }
    /// Inspect an exact prior call before requesting another authorization.
    /// Reading this receipt never dispatches or clears an unknown outcome.
    pub fn completed(&self,context:&ToolContext,call:&str,target:&str,digest:&str)->io::Result<Option<ToolReply>> {
        if [&context.session,&context.turn,call].iter().any(|v|v.is_empty()||v.len()>256||v.chars().any(char::is_control))||[target,digest].iter().any(|v|v.len()!=64||!v.bytes().all(|b|b.is_ascii_hexdigit())){return Err(err("Invalid execution scope or digest"));}
        self.ensure_session(&context.session)?;
        let db=self.db.lock().map_err(err)?;
        let row:Option<(String,String,Option<String>)>=db.query_row("SELECT target,digest,result FROM execution_receipts WHERE session=? AND turn=? AND call=?",params![context.session,context.turn,call],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(err)?;
        match row {None=>Ok(None),Some((old_target,old_digest,result))=>{if old_target!=target||old_digest!=digest{return Err(err("Execution call identity reused for different input or authority"));}match result{Some(result)=>serde_json::from_str(&result).map(Some).map_err(err),None=>Err(uncertain_stop("Earlier execution is unverified; inspect its receipt without replay"))}}}
    }
    pub fn run(
        &self,
        context: &ToolContext,
        call: &str,
        target: &str,
        digest: &str,
        control: &Control<'_>,
        action: impl FnOnce() -> io::Result<ToolReply>,
    ) -> io::Result<ToolReply> {
        if [&context.session, &context.turn, call]
            .iter()
            .any(|s| s.is_empty() || s.len() > 256 || s.chars().any(char::is_control))
            || [target, digest]
                .iter()
                .any(|s| s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err(err("Invalid execution scope or digest"));
        }
        let lane = {
            let mut lanes = self.lanes.lock().map_err(err)?;
            if let Some(lane) = lanes.get(target).and_then(Weak::upgrade) {
                lane
            } else {
                lanes.retain(|_, v| v.strong_count() > 0);
                if lanes.len() >= 128 {
                    return Err(err("Execution lane capacity reached"));
                }
                let lane = Arc::new(Mutex::new(()));
                lanes.insert(target.into(), Arc::downgrade(&lane));
                lane
            }
        };
        let _guard = loop {
            control.check()?;
            match lane.try_lock() {
                Ok(guard) => break guard,
                Err(std::sync::TryLockError::WouldBlock) => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(std::sync::TryLockError::Poisoned(error)) => break error.into_inner(),
            }
        };
        let receipt = {
            let db = self.db.lock().map_err(err)?;
            let erased: bool = db
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM execution_tombstones WHERE session=?)",
                    [&context.session],
                    |r| r.get(0),
                )
                .map_err(err)?;
            if erased {
                return Err(err("Execution session was erased"));
            }
            let existing:Option<(String,String,Option<String>)>=db.query_row("SELECT target,digest,result FROM execution_receipts WHERE session=? AND turn=? AND call=?",params![context.session,context.turn,call],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(err)?;
            if let Some((old_target, old_digest, result)) = existing {
                if old_target != target || old_digest != digest {
                    return Err(err(
                        "Execution call identity reused for different input or authority",
                    ));
                }
                return match result{Some(result)=>serde_json::from_str(&result).map_err(err),None=>Err(uncertain_stop("Earlier execution is unverified; never repeat this call with a new identity"))};
            }
            let blocked:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM execution_receipts WHERE target=? AND state IN ('pending','uncertain'))",[target],|r|r.get(0)).map_err(err)?;
            if blocked {
                return Err(uncertain_stop("An earlier action on this execution target is unverified. Inspect its receipt; reads and other targets remain available."));
            }
            control.check()?;
            let id = uuid::Uuid::new_v4().to_string();
            db.execute(
                "INSERT INTO execution_receipts VALUES (?,?,?,?,?,?,'pending',NULL)",
                params![id, context.session, context.turn, call, target, digest],
            )
            .map_err(err)?;
            id
        };
        let result = action();
        let result = match result {
            Ok(reply) => reply,
            Err(_) => {
                let _ = self.db.lock().map_err(err)?.execute(
                    "UPDATE execution_receipts SET state='uncertain' WHERE receipt=?",
                    [&receipt],
                );
                // Once dispatched, cancellation or a child deadline cannot prove
                // that no external effect happened. Keep the uncertainty typed.
                return Err(uncertain_stop("Execution outcome is unverified. Inspect its receipt before further input; do not replay it."));
            }
        };
        let changed=self.db.lock().map_err(err)?.execute("UPDATE execution_receipts SET state='completed',result=? WHERE receipt=? AND state='pending' AND NOT EXISTS(SELECT 1 FROM execution_tombstones WHERE session=execution_receipts.session)",params![serde_json::to_string(&result)?,receipt]).map_err(|_|uncertain_stop("Execution finished but its receipt could not be published; stop and inspect without replay"))?;
        if changed != 1 {
            return Err(uncertain_stop("Execution receipt no longer matches dispatch"));
        }
        Ok(result)
    }
    /// Erased sessions may inspect their empty history but cannot issue new remote operations.
    pub fn ensure_session(&self, session: &str) -> io::Result<()> {
        let erased: bool = self
            .db
            .lock()
            .map_err(err)?
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM execution_tombstones WHERE session=?)",
                [session],
                |r| r.get(0),
            )
            .map_err(err)?;
        if erased {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Execution session was erased",
            ));
        }
        Ok(())
    }
    pub fn receipts(&self, session: Option<&str>) -> io::Result<Vec<Value>> {
        let db = self.db.lock().map_err(err)?;
        if let Some(session) = session {
            let erased: bool = db
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM execution_tombstones WHERE session=?)",
                    [session],
                    |r| r.get(0),
                )
                .map_err(err)?;
            if erased {
                return Ok(Vec::new());
            }
        }
        let mut q=db.prepare("SELECT receipt,session,turn,call,target,state,result FROM execution_receipts WHERE (?1 IS NULL OR session=?1) ORDER BY rowid DESC LIMIT 128").map_err(err)?;
        let rows = q
            .query_map([session], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, Option<String>>(6)?,
                ))
            })
            .map_err(err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(err)?;
        Ok(rows.into_iter().map(|(receipt,session,turn,call,target,state,result)|json!({"receipt":receipt,"session":session,"turn":turn,"call":call,"target":target,"state":state,"result":result.and_then(|v|serde_json::from_str::<ToolReply>(&v).ok()).map(|reply|json!({"is_error":reply.is_error,"output_bytes":reply.content.len(),"output_sha256":crate::hash(reply.content.as_bytes())}))})).collect())
    }
    pub fn erase(&self, session: &str) -> io::Result<()> {
        let mut db = self.db.lock().map_err(err)?;
        let tx = db.transaction().map_err(err)?;
        tx.execute(
            "INSERT OR IGNORE INTO execution_tombstones VALUES (?)",
            [session],
        )
        .map_err(err)?;
        tx.execute(
            "UPDATE execution_receipts SET result=NULL,state=CASE WHEN state='pending' THEN 'uncertain' ELSE state END WHERE session=?",
            [session],
        )
        .map_err(err)?;
        tx.commit().map_err(err)
    }
    /// Operator decision permits future distinct work; it never verifies/replays the historical action.
    pub fn acknowledge(&self, receipt: &str, target: &str) -> io::Result<()> {
        let lane = self
            .lanes
            .lock()
            .map_err(err)?
            .get(target)
            .and_then(Weak::upgrade);
        let _guard = if let Some(lane) = lane.as_ref() {
            Some(match lane.try_lock() {
                Ok(guard) => guard,
                Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
                Err(_) => return Err(err("Execution is still active")),
            })
        } else {
            None
        };
        let mut db = self.db.lock().map_err(err)?;
        let tx = db.transaction().map_err(err)?;
        let row: Option<(String, String)> = tx
            .query_row(
                "SELECT target,state FROM execution_receipts WHERE receipt=?",
                [receipt],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(err)?;
        let Some((actual, state)) = row else {
            return Err(err("Unknown execution receipt"));
        };
        if actual != target
            || !matches!(
                state.as_str(),
                "pending" | "uncertain" | "acknowledged_unknown"
            )
        {
            return Err(err("Receipt target/state changed"));
        }
        tx.execute(
            "UPDATE execution_receipts SET state='acknowledged_unknown' WHERE receipt=?",
            [receipt],
        )
        .map_err(err)?;
        tx.execute(
            "INSERT OR IGNORE INTO execution_acks VALUES (?,?)",
            params![receipt, target],
        )
        .map_err(err)?;
        tx.commit().map_err(err)
    }
}
