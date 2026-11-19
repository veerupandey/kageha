//! Durable local receipt history. Legacy JSON is imported once and never rewritten.
use crate::{
    action_journal::{private_open, Receipt},
    cdp::Result,
};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::de::{DeserializeSeed, SeqAccess, Visitor};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

static ERRORS: OnceLock<Mutex<HashMap<PathBuf, (&'static str, Instant)>>> = OnceLock::new();
fn errors() -> &'static Mutex<HashMap<PathBuf, (&'static str, Instant)>> {
    ERRORS.get_or_init(Default::default)
}
pub fn failure(root: &Path, code: &'static str) -> &'static str {
    if code != "action_control_busy" {
        errors()
            .lock()
            .unwrap()
            .insert(root.to_owned(), (code, Instant::now()));
    }
    code
}
pub fn sql_error(root: &Path, error: rusqlite::Error) -> &'static str {
    use rusqlite::ErrorCode;
    let code = match error.sqlite_error_code() {
        Some(ErrorCode::DiskFull) => "journal_storage_full",
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => "action_control_busy",
        Some(ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase) => "journal_corrupt",
        _ => "journal_storage_unavailable",
    };
    failure(root, code)
}
fn checksum(path: &Path) -> Result<String> {
    let mut file = private_open(path, false).map_err(|_| "journal_migration_failed")?;
    let mut digest = Sha256::new();
    let mut buf = [0; 8192];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|_| "journal_migration_failed")?;
        if n == 0 {
            break;
        }
        digest.update(&buf[..n]);
    }
    Ok(data_encoding::HEXLOWER.encode(&digest.finalize()))
}
// Stream the legacy array into one transaction instead of loading all history.
struct Import<'a>(&'a Connection, usize);
impl<'de> Visitor<'de> for &mut Import<'_> {
    type Value = usize;
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("receipt array")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut rows: A) -> std::result::Result<usize, A::Error> {
        while let Some(value) = rows.next_element::<Value>()? {
            let receipt: Receipt =
                serde_json::from_value(value.clone()).map_err(serde::de::Error::custom)?;
            let payload = value.to_string();
            self.0.execute("INSERT INTO receipts(workspace_id,request_id,browser_id,target_id,service_epoch,state,payload) VALUES(?,?,?,?,?,?,?)",
                        params![receipt.workspace_id,receipt.request_id,receipt.browser_id,receipt.target_id,receipt.service_epoch,value["state"].as_str().unwrap_or(""),payload]).map_err(serde::de::Error::custom)?;
            let saved: String = self
                .0
                .query_row(
                    "SELECT payload FROM receipts WHERE workspace_id=? AND request_id=?",
                    params![receipt.workspace_id, receipt.request_id],
                    |r| r.get(0),
                )
                .map_err(serde::de::Error::custom)?;
            if saved != payload {
                return Err(serde::de::Error::custom("receipt import mismatch"));
            }
            self.1 += 1;
        }
        Ok(self.1)
    }
}
impl<'de> DeserializeSeed<'de> for &mut Import<'_> {
    type Value = usize;
    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        d: D,
    ) -> std::result::Result<usize, D::Error> {
        d.deserialize_seq(self)
    }
}

pub struct Store {
    db: Mutex<Connection>,
    pub root: PathBuf,
}
impl Store {
    pub fn read(root: &Path) -> Result<Self> {
        let path = fs::canonicalize(root)
            .map_err(|_| "journal_storage_unavailable")?
            .join("action-journal.sqlite");
        for suffix in ["", "-wal", "-shm"] {
            let file = root.join(format!("action-journal.sqlite{suffix}"));
            if fs::symlink_metadata(&file).is_ok() {
                private_open(&file, false).map_err(|_| "journal_storage_unavailable")?;
            }
        }
        let db = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(|e| sql_error(root, e))?;
        let done: Option<String> = db
            .query_row(
                "SELECT value FROM meta WHERE key='legacy_import'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| sql_error(root, e))?;
        if done.is_none() {
            return Err("journal_migration_failed");
        }
        Ok(Self {
            db: Mutex::new(db),
            root: root.to_owned(),
        })
    }
    pub fn open(root: &Path) -> Result<Self> {
        let path = fs::canonicalize(root)
            .map_err(|_| "journal_storage_unavailable")?
            .join("action-journal.sqlite");
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let file = root.join(format!("action-journal.sqlite{suffix}"));
            if fs::symlink_metadata(&file).is_ok() {
                private_open(&file, false)
                    .map_err(|_| failure(root, "journal_storage_unavailable"))?;
            }
        }
        private_open(&path, true).map_err(|_| failure(root, "journal_storage_unavailable"))?;
        let db = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(|e| sql_error(root, e))?;
        db.busy_timeout(Duration::ZERO)
            .map_err(|e| sql_error(root, e))?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA trusted_schema=OFF;
            CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS receipts(workspace_id TEXT NOT NULL,request_id TEXT NOT NULL,browser_id TEXT NOT NULL,target_id TEXT NOT NULL,service_epoch TEXT NOT NULL,state TEXT NOT NULL,payload TEXT NOT NULL,PRIMARY KEY(workspace_id,request_id));
            CREATE INDEX IF NOT EXISTS receipt_pending ON receipts(workspace_id,target_id,state);
            CREATE TABLE IF NOT EXISTS grants(workspace_id TEXT PRIMARY KEY,payload TEXT NOT NULL);") .map_err(|e|sql_error(root,e))?;
        let mut store = Self {
            db: Mutex::new(db),
            root: root.to_owned(),
        };
        store.migrate().map_err(|code| failure(root, code))?;
        store.fence().map_err(|code| failure(root, code))?;
        // A real durable write proves writable readiness, including after disk recovery.
        let recovering = errors().lock().unwrap().contains_key(root);
        let tx = store
            .db
            .get_mut()
            .unwrap()
            .transaction()
            .map_err(|e| sql_error(root, e))?;
        tx.execute("INSERT INTO meta VALUES('writer_probe',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[crate::protocol::now_ms().to_string()]).map_err(|e|sql_error(root,e))?;
        if recovering {
            tx.execute(
                "UPDATE meta SET value=CAST(value AS INTEGER)+1 WHERE key='generation'",
                [],
            )
            .map_err(|e| sql_error(root, e))?;
        }
        tx.commit().map_err(|e| sql_error(root, e))?;
        errors().lock().unwrap().remove(root);
        Ok(store)
    }
    fn migrate(&mut self) -> Result<()> {
        let done: Option<String> = self
            .db
            .get_mut()
            .unwrap()
            .query_row(
                "SELECT value FROM meta WHERE key='legacy_import'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| sql_error(&self.root, e))?;
        if done.is_some() {
            return Ok(());
        }
        let tx = self
            .db
            .get_mut()
            .unwrap()
            .transaction()
            .map_err(|e| sql_error(&self.root, e))?;
        let mut sources = json!({});
        let path = self.root.join("action-receipts.json");
        let mut imported = 0;
        if fs::symlink_metadata(&path).is_ok() {
            let hash = checksum(&path)?;
            let file = private_open(&path, false).map_err(|_| "journal_migration_failed")?;
            let mut decoder = serde_json::Deserializer::from_reader(file);
            imported = (&mut Import(&tx, 0))
                .deserialize(&mut decoder)
                .map_err(|_| "journal_migration_failed")?;
            decoder.end().map_err(|_| "journal_migration_failed")?;
            if checksum(&path)? != hash {
                return Err("journal_migration_failed");
            }
            sources["receipts"] = json!({"sha256":hash,"count":imported});
        }
        let grants = self.root.join("action-grants.json");
        if fs::symlink_metadata(&grants).is_ok() {
            let hash = checksum(&grants)?;
            let values: HashMap<String, Value> = serde_json::from_reader(
                private_open(&grants, false).map_err(|_| "journal_migration_failed")?,
            )
            .map_err(|_| "journal_migration_failed")?;
            for (chat, value) in &values {
                let _: super::action_journal::Grant = serde_json::from_value(value.clone())
                    .map_err(|_| "journal_migration_failed")?;
                tx.execute(
                    "INSERT INTO grants VALUES(?,?)",
                    params![chat, value.to_string()],
                )
                .map_err(|_| "journal_migration_failed")?;
            }
            if checksum(&grants)? != hash {
                return Err("journal_migration_failed");
            }
            sources["grants"] = json!({"sha256":hash,"count":values.len()});
        }
        let count: i64 = tx
            .query_row("SELECT count(*) FROM receipts", [], |r| r.get(0))
            .map_err(|_| "journal_migration_failed")?;
        if count != imported as i64 {
            return Err("journal_migration_failed");
        }
        tx.execute(
            "INSERT INTO meta VALUES('legacy_import',?)",
            [sources.to_string()],
        )
        .map_err(|e| sql_error(&self.root, e))?;
        tx.execute("INSERT INTO meta VALUES('generation','1')", [])
            .map_err(|e| sql_error(&self.root, e))?;
        tx.commit().map_err(|e| sql_error(&self.root, e))
    }
    fn fence(&self) -> Result<()> {
        let path = self.root.join("action-backend");
        if !path.exists() {
            return Ok(());
        }
        let mut mode = String::new();
        private_open(&path, false)
            .and_then(|mut f| f.read_to_string(&mut mode))
            .map_err(|_| "journal_storage_unavailable")?;
        if matches!(mode.as_str(), "standalone" | "fixture") {
            let mut next = tempfile::NamedTempFile::new_in(&self.root)
                .map_err(|_| "journal_storage_unavailable")?;
            next.write_all(format!("{mode}-sqlite-v1").as_bytes())
                .and_then(|_| next.as_file().sync_all())
                .map_err(|_| "journal_storage_unavailable")?;
            next.persist(path)
                .map_err(|_| "journal_storage_unavailable")?;
            File::open(&self.root)
                .and_then(|f| f.sync_all())
                .map_err(|_| "journal_storage_unavailable")?;
        }
        Ok(())
    }
    pub fn connection(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.db.lock().unwrap()
    }
    pub fn error(&self, e: rusqlite::Error) -> &'static str {
        sql_error(&self.root, e)
    }
}

pub fn status(root: &Path) -> Value {
    let retry = errors()
        .lock()
        .unwrap()
        .get(root)
        .is_some_and(|(_, at)| at.elapsed() > Duration::from_secs(5));
    if retry {
        let _ = crate::action_journal::Journal::lock(root);
    }
    let error = errors().lock().unwrap().get(root).map(|v| v.0);
    let read = (|| -> rusqlite::Result<(i64, i64, String)> {
        let path = fs::canonicalize(root)
            .map_err(|_| rusqlite::Error::InvalidPath(root.to_owned()))?
            .join("action-journal.sqlite");
        private_open(&path, false).map_err(|_| rusqlite::Error::InvalidPath(path.clone()))?;
        let db = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        Ok((
            db.query_row("SELECT count(*) FROM receipts", [], |r| r.get(0))?,
            db.query_row(
                "SELECT count(*) FROM receipts WHERE state IN ('dispatching','returned')",
                [],
                |r| r.get(0),
            )?,
            db.query_row("SELECT value FROM meta WHERE key='generation'", [], |r| {
                r.get(0)
            })?,
        ))
    })();
    match read {
        Ok((count, pending, generation)) => {
            json!({"ready":error.is_none(),"reason":error,"storage":"sqlite-v1","receipt_count":count,"pending_count":pending,"revision":format!("sqlite-v1:{generation}")})
        }
        Err(_) => {
            json!({"ready":false,"reason":error.unwrap_or("journal_storage_unavailable"),"storage":"sqlite-v1","revision":format!("sqlite-v1:{}",error.unwrap_or("journal_storage_unavailable"))})
        }
    }
}
