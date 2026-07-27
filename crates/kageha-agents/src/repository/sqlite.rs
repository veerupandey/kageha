//! Dedicated, leased SQLite profile repository. No application tables are read.
use super::*;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use std::path::Path;
pub struct SqliteRepository {
    scope: String,
    db: Mutex<Connection>,
    _lease: std::fs::File,
}
fn db(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(format!("Profile repository: {e}"))
}
fn load(c: &Connection, k: Kind, id: &str) -> io::Result<Option<Entry>> {
    c.query_row(
        "SELECT body FROM profile_entries WHERE kind=? AND id=?",
        params![k.name(), id],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .map_err(db)?
    .map(|s| {
        let e: Entry = serde_json::from_str(&s)?;
        e.validate()?;
        if e.kind != k || e.id != id {
            return Err(invalid("Stored profile identity mismatch"));
        }
        Ok(e)
    })
    .transpose()
}
fn save(c: &Connection, e: &Entry) -> io::Result<()> {
    c.execute("INSERT INTO profile_entries VALUES (?,?,?) ON CONFLICT(kind,id) DO UPDATE SET body=excluded.body",params![e.kind.name(),e.id,serde_json::to_string(e)?]).map_err(db)?;
    Ok(())
}
impl SqliteRepository {
    pub fn open(path: &Path, scope: &str) -> io::Result<Self> {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        label(scope)?;
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let open = |p: &Path| -> io::Result<std::fs::File> {
            let f = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(p)?;
            let m = f.metadata()?;
            if !m.is_file() || m.permissions().mode() & 0o077 != 0 {
                return Err(invalid("Profile repository must be a private regular file"));
            }
            Ok(f)
        };
        let lease = open(&path.with_extension("lock"))?;
        fs2::FileExt::try_lock_exclusive(&lease)
            .map_err(|_| conflict("Another profile writer is active"))?;
        let _file = open(path)?;
        let mut c = Connection::open(path).map_err(db)?;
        c.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(db)?;
        let version: u64 = c
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(db)?;
        if version > 1 {
            return Err(invalid(
                "Profile repository schema is newer than this client",
            ));
        }
        {
            let mut q=c.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'").map_err(db)?;
            for t in q.query_map([], |r| r.get::<_, String>(0)).map_err(db)? {
                if !matches!(
                    t.map_err(db)?.as_str(),
                    "profile_entries" | "profile_imports" | "profile_metadata"
                ) {
                    return Err(invalid("Expected a dedicated profile repository"));
                }
            }
        }
        c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS profile_metadata(scope TEXT NOT NULL);CREATE TABLE IF NOT EXISTS profile_entries(kind TEXT,id TEXT,body TEXT NOT NULL,PRIMARY KEY(kind,id));CREATE TABLE IF NOT EXISTS profile_imports(source TEXT PRIMARY KEY,body TEXT NOT NULL); PRAGMA user_version=1;").map_err(db)?;
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db)?;
        let rows: Vec<String> = {
            let mut q = tx
                .prepare("SELECT scope FROM profile_metadata")
                .map_err(db)?;
            let v = q
                .query_map([], |r| r.get(0))
                .map_err(db)?
                .collect::<Result<_, _>>()
                .map_err(db)?;
            v
        };
        if rows.is_empty() {
            tx.execute("INSERT INTO profile_metadata VALUES (?)", [scope])
                .map_err(db)?;
        } else if rows != [scope] {
            return Err(invalid("Profile repository belongs to another scope"));
        }
        tx.commit().map_err(db)?;
        Ok(Self {
            scope: scope.into(),
            db: Mutex::new(c),
            _lease: lease,
        })
    }
}
impl Repository for SqliteRepository {
    fn import_status(&self, source: &str) -> io::Result<Option<ImportStatus>> {
        label(source)?;
        self.db
            .lock()
            .map_err(db)?
            .query_row(
                "SELECT body FROM profile_imports WHERE source=?",
                [source],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(db)?
            .map(|raw| serde_json::from_str(&raw).map_err(io::Error::other))
            .transpose()
    }
    fn scope(&self) -> &str {
        &self.scope
    }
    fn entries(&self) -> io::Result<Vec<Entry>> {
        let c = self.db.lock().map_err(db)?;
        let mut q = c
            .prepare("SELECT kind,id FROM profile_entries ORDER BY kind,id")
            .map_err(db)?;
        let keys = q
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(db)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db)?;
        keys.into_iter()
            .map(|(kind, id)| {
                let k = match kind.as_str() {
                    "agent" => Kind::Agent,
                    "team" => Kind::Team,
                    _ => return Err(invalid("Stored profile kind invalid")),
                };
                load(&c, k, &id)?.ok_or_else(|| invalid("Missing profile entry"))
            })
            .collect()
    }
    fn entry(&self, k: Kind, id: &str) -> io::Result<Option<Entry>> {
        load(&*self.db.lock().map_err(db)?, k, id)
    }
    fn write(
        &self,
        k: Kind,
        id: &str,
        expected: u64,
        value: Option<Value>,
        timestamp: f64,
    ) -> io::Result<Entry> {
        let mut c = self.db.lock().map_err(db)?;
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db)?;
        let e = changed(load(&tx, k, id)?, k, id, expected, value, timestamp)?;
        save(&tx, &e)?;
        tx.commit().map_err(db)?;
        Ok(e)
    }
    fn import(&self, b: &Bundle, limit: usize) -> io::Result<ImportStatus> {
        if limit == 0 || limit > 4096 {
            return Err(invalid("Invalid import batch size"));
        }
        b.validate()?;
        let mut c = self.db.lock().map_err(db)?;
        // Persist source identity before the first entry so even an initial
        // conflict cannot silently change the source of a retry.
        {
            let tx = c
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(db)?;
            let raw: Option<String> = tx
                .query_row(
                    "SELECT body FROM profile_imports WHERE source=?",
                    [&b.source],
                    |r| r.get(0),
                )
                .optional()
                .map_err(db)?;
            let p = status(b, raw.map(|s| serde_json::from_str(&s)).transpose()?)?;
            tx.execute(
                "INSERT OR IGNORE INTO profile_imports VALUES (?,?)",
                params![b.source, serde_json::to_string(&p)?],
            )
            .map_err(db)?;
            tx.commit().map_err(db)?;
        }
        let mut p: ImportStatus = serde_json::from_str(
            &c.query_row(
                "SELECT body FROM profile_imports WHERE source=?",
                [&b.source],
                |r| r.get::<_, String>(0),
            )
            .map_err(db)?,
        )?;
        let stop = (p.next + limit).min(p.total);
        while p.next < stop {
            let tx = c
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(db)?;
            let e = &b.entries[p.next];
            if load(&tx, e.kind, &e.id)?.is_some() {
                return Err(conflict(
                    "Import cannot overwrite or resurrect an existing profile",
                ));
            }
            save(&tx, e)?;
            p.next += 1;
            tx.execute(
                "UPDATE profile_imports SET body=? WHERE source=?",
                params![serde_json::to_string(&p)?, b.source],
            )
            .map_err(db)?;
            tx.commit().map_err(db)?;
        }
        Ok(p)
    }
}
impl Port for SqliteRepository {
    fn get(&self, id: &str) -> io::Result<Option<Definition>> {
        self.entry(Kind::Agent, id)?
            .and_then(|e| e.head().value.clone())
            .map(Definition::parse)
            .transpose()
    }
    fn list(&self) -> io::Result<Vec<Definition>> {
        definitions(self.entries()?)
    }
}
