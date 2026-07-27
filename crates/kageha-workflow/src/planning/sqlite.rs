//! Dedicated plan journal. No host/application tables are shared.
use super::*;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::{path::Path, sync::Mutex};
pub struct SqliteStore {
    db: Mutex<Connection>,
    _lease: std::fs::File,
}
fn db_error(e: impl fmt::Display) -> Error {
    failure(Code::Unavailable, format!("Plan journal: {e}"))
}
fn decode(raw: String) -> Result<Record> {
    serde_json::from_str(&raw).map_err(db_error)
}
impl SqliteStore {
    pub fn open(path: &Path) -> Result<Self> {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(db_error)?;
        }
        let open = |p: &Path| -> Result<std::fs::File> {
            let f = std::fs::OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(p)
                .map_err(db_error)?;
            let m = f.metadata().map_err(db_error)?;
            if !m.is_file() || m.permissions().mode() & 0o077 != 0 {
                return Err(failure(
                    Code::Invalid,
                    "Plan journal must be a private regular file",
                ));
            }
            Ok(f)
        };
        let lease = open(&path.with_extension("lock"))?;
        fs2::FileExt::try_lock_exclusive(&lease)
            .map_err(|_| failure(Code::Conflict, "Another plan journal writer is active"))?;
        let _file = open(path)?;
        let db = Connection::open(path).map_err(db_error)?;
        db.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(db_error)?;
        let version: u64 = db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(db_error)?;
        if version > 1 {
            return Err(failure(
                Code::Invalid,
                "Plan journal schema is newer than this client",
            ));
        }
        {
            let mut q=db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'").map_err(db_error)?;
            for table in q
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(db_error)?
            {
                if !matches!(table.map_err(db_error)?.as_str(), "plans" | "plan_receipts") {
                    return Err(failure(Code::Invalid, "Expected a dedicated plan journal"));
                }
            }
        }
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS plans(session TEXT,turn TEXT,revision INTEGER,body TEXT,PRIMARY KEY(session,turn)); CREATE TABLE IF NOT EXISTS plan_receipts(session TEXT,turn TEXT,operation TEXT,digest TEXT,body TEXT,PRIMARY KEY(session,turn,operation)); PRAGMA user_version=1;").map_err(db_error)?;
        Ok(Self {
            db: Mutex::new(db),
            _lease: lease,
        })
    }
}
fn receipt(db: &Connection, key: &Key, operation: &Operation) -> Result<Option<Record>> {
    let row: Option<(String, String)> = db
        .query_row(
            "SELECT digest,body FROM plan_receipts WHERE session=? AND turn=? AND operation=?",
            params![key.session, key.turn, operation.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(db_error)?;
    match row {
        None => Ok(None),
        Some((digest, body)) => {
            if digest != operation.digest {
                return Err(failure(
                    Code::Conflict,
                    "Plan operation identity reused for different input",
                ));
            }
            Ok(Some(decode(body)?))
        }
    }
}
impl Store for SqliteStore {
    fn load(&self, key: &Key) -> Result<Option<Record>> {
        key.validate()?;
        let db = self.db.lock().map_err(db_error)?;
        db.query_row(
            "SELECT body FROM plans WHERE session=? AND turn=?",
            params![key.session, key.turn],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(db_error)?
        .map(decode)
        .transpose()
    }
    fn receipt(&self, key: &Key, operation: &Operation) -> Result<Option<Record>> {
        key.validate()?;
        let db = self.db.lock().map_err(db_error)?;
        receipt(&db, key, operation)
    }
    fn commit(
        &self,
        expected: Option<u64>,
        next: &Record,
        operation: &Operation,
    ) -> Result<Record> {
        next.key.validate()?;
        validate(&next.document)?;
        if next.revision
            != expected
                .unwrap_or(0)
                .checked_add(1)
                .ok_or_else(|| failure(Code::Invalid, "Plan revision overflow"))?
            || next.revision > i64::MAX as u64
            || operation.id.is_empty()
            || operation.id.len() > 256
            || operation.digest.len() != 64
        {
            return Err(failure(Code::Invalid, "Invalid plan commit"));
        }
        let mut db = self.db.lock().map_err(db_error)?;
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        if let Some(cached) = receipt(&tx, &next.key, operation)? {
            return Ok(cached);
        }
        let body = serde_json::to_string(next).map_err(db_error)?;
        let changed = if let Some(revision) = expected {
            tx.execute(
                "UPDATE plans SET revision=?,body=? WHERE session=? AND turn=? AND revision=?",
                params![
                    next.revision,
                    body,
                    next.key.session,
                    next.key.turn,
                    revision
                ],
            )
        } else {
            tx.execute(
                "INSERT OR IGNORE INTO plans VALUES (?,?,?,?)",
                params![next.key.session, next.key.turn, next.revision, body],
            )
        }
        .map_err(db_error)?;
        if changed != 1 {
            return Err(failure(
                Code::Conflict,
                "Plan changed; the newer revision was not overwritten",
            ));
        }
        tx.execute(
            "INSERT INTO plan_receipts VALUES (?,?,?,?,?)",
            params![
                next.key.session,
                next.key.turn,
                operation.id,
                operation.digest,
                body
            ],
        )
        .map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        Ok(next.clone())
    }
}
