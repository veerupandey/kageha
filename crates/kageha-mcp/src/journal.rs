use fs2::FileExt;
use kageha_contracts::{execution::*, ErrorCode, ServiceError};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use std::{io, path::Path, sync::Mutex};
pub(crate) struct Journal {
    db: Mutex<Connection>,
    _lock: std::fs::File,
}
fn err(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(format!("MCP journal: {e}"))
}
pub fn uncertain() -> io::Error {
    io::Error::other(ServiceError::new(ErrorCode::Uncertain,"MCP outcome is uncertain. Inspect the affected server receipt before further calls; unrelated bindings remain available."))
}
impl Journal {
    pub fn open(path: &Path) -> io::Result<Self> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)?;
        }
        for file in [path.to_owned(), path.with_extension("lock")] {
            if std::fs::symlink_metadata(file).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(err("Journal paths cannot be symlinks"));
            }
        }
        let mut options = std::fs::OpenOptions::new();
        options.create(true).read(true).write(true).mode(0o600);
        let lock = options.open(path.with_extension("lock"))?;
        lock.try_lock_exclusive()
            .map_err(|_| err("Another MCP journal writer is active"))?;
        drop(options.open(path)?);
        let db = Connection::open(path).map_err(err)?;
        db.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(err)?;
        let version: u32 = db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(err)?;
        if version > 1 {
            return Err(err("MCP journal schema is newer than this client"));
        }
        {
            let mut q=db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'").map_err(err)?;
            let names = q
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(err)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(err)?;
            if names
                .iter()
                .any(|n| !matches!(n.as_str(), "receipts" | "tombstones"))
            {
                return Err(err("Expected a dedicated MCP journal"));
            }
        }
        db.execute_batch("PRAGMA journal_mode=WAL;PRAGMA synchronous=FULL;CREATE TABLE IF NOT EXISTS receipts(session TEXT,turn TEXT,id TEXT,server TEXT,digest TEXT,result TEXT,PRIMARY KEY(session,turn,id));CREATE INDEX IF NOT EXISTS mcp_uncertain ON receipts(session,server) WHERE result IS NULL;CREATE TABLE IF NOT EXISTS tombstones(session TEXT PRIMARY KEY);PRAGMA user_version=1;").map_err(err)?;
        Ok(Self {
            db: Mutex::new(db),
            _lock: lock,
        })
    }
    pub fn preflight(
        &self,
        context: &ToolContext,
        id: &str,
        server: &str,
        digest: &str,
    ) -> io::Result<Option<Value>> {
        let db = self.db.lock().map_err(err)?;
        let erased: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM tombstones WHERE session=?)",
                [&context.session],
                |r| r.get(0),
            )
            .map_err(err)?;
        if erased {
            return Err(err("This MCP session was erased"));
        }
        let existing: Option<(String, Option<String>)> = db
            .query_row(
                "SELECT digest,result FROM receipts WHERE session=? AND turn=? AND id=?",
                params![context.session, context.turn, id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(err)?;
        if let Some((prior, result)) = existing {
            if prior != digest {
                return Err(err("MCP call ID reused for different input or authority"));
            }
            return result
                .map(|v| serde_json::from_str(&v).map_err(err))
                .transpose()?
                .map(Some)
                .ok_or_else(uncertain);
        }
        let blocked:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM receipts WHERE session=? AND server=? AND result IS NULL)",params![context.session,server],|r|r.get(0)).map_err(err)?;
        if blocked {
            return Err(uncertain());
        }
        Ok(None)
    }
    pub fn begin(
        &self,
        context: &ToolContext,
        id: &str,
        server: &str,
        digest: &str,
    ) -> io::Result<Option<Value>> {
        let mut db = self.db.lock().map_err(err)?;
        let tx = db.transaction().map_err(err)?;
        let erased: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM tombstones WHERE session=?)",
                [&context.session],
                |r| r.get(0),
            )
            .map_err(err)?;
        if erased {
            return Err(err("This MCP session was erased"));
        }
        let existing: Option<(String, Option<String>)> = tx
            .query_row(
                "SELECT digest,result FROM receipts WHERE session=? AND turn=? AND id=?",
                params![context.session, context.turn, id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(err)?;
        if let Some((prior, result)) = existing {
            if prior != digest {
                return Err(err("MCP call ID reused for different input or authority"));
            }
            return result
                .map(|v| serde_json::from_str(&v).map_err(err))
                .transpose()?
                .map(Some)
                .ok_or_else(uncertain);
        }
        let blocked:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM receipts WHERE session=? AND server=? AND result IS NULL)",params![context.session,server],|r|r.get(0)).map_err(err)?;
        if blocked {
            return Err(uncertain());
        }
        tx.execute(
            "INSERT INTO receipts VALUES (?,?,?,?,?,NULL)",
            params![context.session, context.turn, id, server, digest],
        )
        .map_err(err)?;
        tx.commit().map_err(err)?;
        Ok(None)
    }
    pub fn finish(
        &self,
        context: &ToolContext,
        id: &str,
        digest: &str,
        result: &Value,
    ) -> io::Result<()> {
        let changed=self.db.lock().map_err(err)?.execute("UPDATE receipts SET result=? WHERE session=? AND turn=? AND id=? AND digest=? AND result IS NULL",params![result.to_string(),context.session,context.turn,id,digest]).map_err(err)?;
        if changed != 1 {
            return Err(err("MCP receipt no longer matches dispatch"));
        }
        Ok(())
    }
    pub fn receipts(&self, session: &str) -> io::Result<Vec<Value>> {
        let db = self.db.lock().map_err(err)?;
        let mut q=db.prepare("SELECT id,server,digest,result,turn FROM receipts WHERE session=? ORDER BY rowid DESC LIMIT 128").map_err(err)?;
        let rows = q
            .query_map(params![session], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })
            .map_err(err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(err)?;
        rows.into_iter().map(|(id,server,digest,result,turn)|Ok(serde_json::json!({"id":id,"turn":turn,"server":server,"input_digest":digest,"state":if result.is_some(){"completed"}else{"uncertain"},"result":result.map(|v|serde_json::from_str::<Value>(&v)).transpose().map_err(err)?}))).collect()
    }
    pub fn erase(&self, session: &str) -> io::Result<()> {
        let mut db = self.db.lock().map_err(err)?;
        let tx = db.transaction().map_err(err)?;
        tx.execute("INSERT OR IGNORE INTO tombstones VALUES (?)", [session])
            .map_err(err)?;
        tx.execute("DELETE FROM receipts WHERE session=?", [session])
            .map_err(err)?;
        tx.commit().map_err(err)
    }
}
