//! Optional SQLite persistence. Other frameworks can implement Store themselves.
use crate::{State, Store};
use rusqlite::{params, Connection, OptionalExtension};
use std::{io, path::Path, sync::Mutex};
pub struct SqliteStore(Mutex<Connection>);
impl SqliteStore {
    pub fn open(path: &Path) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let _ = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .mode(0o600)
                .open(path)?;
            let m = std::fs::symlink_metadata(path)?;
            use std::os::unix::fs::PermissionsExt;
            if m.file_type().is_symlink() || !m.is_file() || m.permissions().mode() & 0o077 != 0 {
                return Err(io::Error::other(
                    "Workflow store must be a private regular file",
                ));
            }
        }
        let db = Connection::open(path).map_err(io::Error::other)?;
        db.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(io::Error::other)?;
        db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS workflows(namespace TEXT NOT NULL,id TEXT NOT NULL,revision INTEGER NOT NULL,body TEXT NOT NULL,PRIMARY KEY(namespace,id)); CREATE TABLE IF NOT EXISTS workflow_events(namespace TEXT NOT NULL,id TEXT NOT NULL,revision INTEGER NOT NULL,phase TEXT NOT NULL,reason TEXT NOT NULL,PRIMARY KEY(namespace,id,revision));").map_err(io::Error::other)?;
        Ok(Self(Mutex::new(db)))
    }
}
impl Store for SqliteStore {
    fn load(&self, namespace: &str, id: &str) -> io::Result<Option<State>> {
        self.0
            .lock()
            .map_err(|_| io::Error::other("Workflow store unavailable"))?
            .query_row(
                "SELECT body FROM workflows WHERE namespace=? AND id=?",
                params![namespace, id],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(io::Error::other)?
            .map(|v| serde_json::from_str(&v).map_err(io::Error::other))
            .transpose()
    }
    fn save(&self, previous: Option<u64>, state: &State) -> io::Result<()> {
        let mut db = self
            .0
            .lock()
            .map_err(|_| io::Error::other("Workflow store unavailable"))?;
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(io::Error::other)?;
        let body = serde_json::to_string(state)?;
        let changed = if let Some(revision) = previous {
            tx.execute(
                "UPDATE workflows SET revision=?,body=? WHERE namespace=? AND id=? AND revision=?",
                params![
                    state.revision,
                    body,
                    state.spec.namespace,
                    state.spec.id,
                    revision
                ],
            )
        } else {
            tx.execute(
                "INSERT OR IGNORE INTO workflows VALUES (?,?,?,?)",
                params![state.spec.namespace, state.spec.id, state.revision, body],
            )
        }
        .map_err(io::Error::other)?;
        if changed != 1 {
            return Err(io::Error::other(
                "Workflow revision conflict; newer state was not overwritten",
            ));
        }
        tx.execute(
            "INSERT INTO workflow_events VALUES (?,?,?,?,?)",
            params![
                state.spec.namespace,
                state.spec.id,
                state.revision,
                serde_json::to_string(&state.phase)?,
                state.reason
            ],
        )
        .map_err(io::Error::other)?;
        tx.commit().map_err(io::Error::other)
    }
}
