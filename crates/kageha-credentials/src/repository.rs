//! SQLite stores authenticated ciphertext only; admission, CAS, receipts and audit are atomic.
use crate::{
    crypto::{err, json, parse},
    *,
};
use kageha_contracts::{ErrorCode, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::sync::{Arc, Mutex};
#[derive(Clone)]
pub struct SqliteRepository {
    db: Arc<Mutex<Connection>>,
}
fn storage(_: impl std::fmt::Display) -> kageha_contracts::ServiceError {
    err(
        ErrorCode::Unavailable,
        "Credential repository operation failed",
    )
}
pub(crate) fn private_connection(path: &std::path::Path) -> Result<Connection> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        let mut dir = std::fs::DirBuilder::new();
        dir.recursive(true).mode(0o700);
        dir.create(parent).map_err(storage)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(storage)?;
    use std::os::unix::fs::MetadataExt;
    let meta = file.metadata().map_err(storage)?;
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err(err(
            ErrorCode::Unauthorized,
            "Credential vault must be private and owned by this user",
        ));
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    let parent_meta = std::fs::metadata(parent).map_err(storage)?;
    if parent_meta.uid() != unsafe { libc::geteuid() } || parent_meta.mode() & 0o077 != 0 {
        return Err(err(
            ErrorCode::Unauthorized,
            "Credential vault directory must be private and owned by this user",
        ));
    }
    // macOS /var and /tmp can be symlinks. Resolve the checked private parent, never the final file.
    let canonical_parent = parent.canonicalize().map_err(storage)?;
    let name = path
        .file_name()
        .ok_or_else(|| err(ErrorCode::Invalid, "Credential vault filename missing"))?;
    let connection = Connection::open_with_flags(
        canonical_parent.join(name),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
            | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(storage)?;
    Ok(connection)
}

impl SqliteRepository {
    pub fn open(path: &std::path::Path) -> Result<Self> {
        Self::from_connection(private_connection(path)?)
    }
    pub fn in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory().map_err(storage)?)
    }
    fn from_connection(db: Connection) -> Result<Self> {
        db.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(storage)?;
        db.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; PRAGMA secure_delete=ON;
        CREATE TABLE IF NOT EXISTS credential_header(singleton INTEGER PRIMARY KEY CHECK(singleton=1),body BLOB NOT NULL);
        CREATE TABLE IF NOT EXISTS credential_records(namespace TEXT NOT NULL,id TEXT NOT NULL,revision INTEGER NOT NULL,body BLOB NOT NULL,PRIMARY KEY(namespace,id));
        CREATE TABLE IF NOT EXISTS credential_operations(id TEXT PRIMARY KEY,body BLOB NOT NULL);
        CREATE TABLE IF NOT EXISTS credential_audit(sequence INTEGER PRIMARY KEY AUTOINCREMENT,body BLOB NOT NULL);").map_err(storage)?;
        Ok(Self {
            db: Arc::new(Mutex::new(db)),
        })
    }
    async fn run<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || f(&mut *db.lock().map_err(storage)?))
            .await
            .map_err(storage)?
    }
}
fn header(db: &Connection) -> Result<Option<VaultHeader>> {
    db.query_row(
        "SELECT body FROM credential_header WHERE singleton=1",
        [],
        |r| r.get::<_, Vec<u8>>(0),
    )
    .optional()
    .map_err(storage)?
    .map(|b| parse(&b))
    .transpose()
}
fn audit(db: &Connection, entry: &CredentialAudit) -> Result<()> {
    db.execute(
        "INSERT INTO credential_audit(body) VALUES (?)",
        [json(entry)?],
    )
    .map_err(storage)?;
    Ok(())
}
#[async_trait::async_trait]
impl CredentialRepository for SqliteRepository {
    async fn header(&self) -> Result<Option<VaultHeader>> {
        self.run(|db| header(db)).await
    }
    async fn initialize(&self, value: VaultHeader) -> Result<VaultHeader> {
        self.run(move |db| {
            let tx = db.transaction().map_err(storage)?;
            if let Some(old) = header(&tx)? {
                return Ok(old);
            }
            tx.execute("INSERT INTO credential_header VALUES(1,?)", [json(&value)?])
                .map_err(storage)?;
            tx.commit().map_err(storage)?;
            Ok(value)
        })
        .await
    }
    async fn record(&self, r: &CredentialRef) -> Result<Option<CredentialEnvelope>> {
        let r = r.clone();
        self.run(move |db| {
            db.query_row(
                "SELECT body FROM credential_records WHERE namespace=? AND id=?",
                params![r.namespace, r.id],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()
            .map_err(storage)?
            .map(|b| parse(&b))
            .transpose()
        })
        .await
    }
    async fn operation(&self, id: &str) -> Result<Option<StoredOperation>> {
        let id = id.to_owned();
        self.run(move |db| {
            db.query_row(
                "SELECT body FROM credential_operations WHERE id=?",
                [id],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .optional()
            .map_err(storage)?
            .map(|b| parse(&b))
            .transpose()
        })
        .await
    }
    async fn commit(
        &self,
        generation: u64,
        expected: u64,
        envelope: CredentialEnvelope,
        operation: StoredOperation,
        entry: CredentialAudit,
    ) -> Result<StoredOperation> {
        self.run(move|db|{
        let tx=db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(storage)?;
        if header(&tx)?.is_none_or(|h|h.generation!=generation){return Err(err(ErrorCode::StaleGeneration,"Credential authority changed"));}
        let old:Option<Vec<u8>>=tx.query_row("SELECT body FROM credential_operations WHERE id=?",[&operation.operation.operation_id],|r|r.get(0)).optional().map_err(storage)?;
        if let Some(old)=old {let old:StoredOperation=parse(&old)?;if old.fingerprint!=operation.fingerprint{return Err(err(ErrorCode::Conflict,"Credential operation ID already has different input"));}return Ok(old);}
        let r=&envelope.metadata.reference;
        let actual:u64=tx.query_row("SELECT revision FROM credential_records WHERE namespace=? AND id=?",params![r.namespace,r.id],|r|r.get(0)).optional().map_err(storage)?.unwrap_or(0);
        if actual!=expected||envelope.metadata.revision!=expected+1{return Err(err(ErrorCode::Conflict,"Credential revision changed"));}
        tx.execute("INSERT INTO credential_records VALUES(?,?,?,?) ON CONFLICT(namespace,id) DO UPDATE SET revision=excluded.revision,body=excluded.body",params![r.namespace,r.id,envelope.metadata.revision,json(&envelope)?]).map_err(storage)?;
        tx.execute("INSERT INTO credential_operations VALUES(?,?)",params![operation.operation.operation_id,json(&operation)?]).map_err(storage)?;
        audit(&tx,&entry)?;tx.commit().map_err(storage)?;Ok(operation)
    }).await
    }
    async fn snapshot(&self) -> Result<(VaultHeader, Vec<CredentialEnvelope>)> {
        self.run(|db| {
            let tx = db.transaction().map_err(storage)?;
            let h = header(&tx)?
                .ok_or_else(|| err(ErrorCode::Unavailable, "Credential vault not initialized"))?;
            let rows = {
                let mut statement = tx
                    .prepare(
                        "SELECT body FROM credential_records ORDER BY namespace,id LIMIT 10001",
                    )
                    .map_err(storage)?;
                let blobs = statement
                    .query_map([], |r| r.get::<_, Vec<u8>>(0))
                    .map_err(storage)?
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(storage)?;
                if blobs.len() > 10000 {
                    return Err(err(
                        ErrorCode::Unsupported,
                        "Credential snapshot limit exceeded",
                    ));
                }
                blobs.iter().map(|b| parse(b)).collect::<Result<Vec<_>>>()?
            };
            tx.commit().map_err(storage)?;
            Ok((h, rows))
        })
        .await
    }
    async fn replace(
        &self,
        expected: u64,
        h: VaultHeader,
        rows: Vec<CredentialEnvelope>,
        entry: CredentialAudit,
    ) -> Result<()> {
        self.run(move |db| {
            let tx = db
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(storage)?;
            if header(&tx)?.is_none_or(|v| v.generation != expected) {
                return Err(err(
                    ErrorCode::StaleGeneration,
                    "Credential authority changed",
                ));
            }
            // Re-check exact revisions to fence changes confirmed after the snapshot.
            let count: u64 = tx
                .query_row("SELECT COUNT(*) FROM credential_records", [], |r| r.get(0))
                .map_err(storage)?;
            if count != rows.len() as u64 {
                return Err(err(ErrorCode::Conflict, "Credential snapshot changed"));
            }
            for row in &rows {
                let r = &row.metadata.reference;
                let current: Option<u64> = tx
                    .query_row(
                        "SELECT revision FROM credential_records WHERE namespace=? AND id=?",
                        params![r.namespace, r.id],
                        |v| v.get(0),
                    )
                    .optional()
                    .map_err(storage)?;
                if current != Some(row.metadata.revision) {
                    return Err(err(ErrorCode::Conflict, "Credential snapshot changed"));
                }
            }
            for row in rows {
                let r = &row.metadata.reference;
                tx.execute(
                    "UPDATE credential_records SET body=? WHERE namespace=? AND id=?",
                    params![json(&row)?, r.namespace, r.id],
                )
                .map_err(storage)?;
            }
            tx.execute(
                "UPDATE credential_header SET body=? WHERE singleton=1",
                [json(&h)?],
            )
            .map_err(storage)?;
            audit(&tx, &entry)?;
            tx.commit().map_err(storage)?;
            Ok(())
        })
        .await
    }
    async fn restore_empty(
        &self,
        expected: u64,
        snapshot: CredentialRepositorySnapshot,
        entry: CredentialAudit,
    ) -> Result<()> {
        self.run(move |db| {
            let tx = db
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(storage)?;
            if header(&tx)?.is_none_or(|v| v.generation != expected) {
                return Err(err(
                    ErrorCode::StaleGeneration,
                    "Credential authority changed",
                ));
            }
            let count: u64 = tx
                .query_row("SELECT COUNT(*) FROM credential_records", [], |r| r.get(0))
                .map_err(storage)?;
            if count != 0 {
                return Err(err(
                    ErrorCode::Conflict,
                    "Recovery requires an empty destination vault",
                ));
            }
            for row in snapshot.records {
                let r = &row.metadata.reference;
                tx.execute(
                    "INSERT INTO credential_records VALUES(?,?,?,?)",
                    params![r.namespace, r.id, row.metadata.revision, json(&row)?],
                )
                .map_err(storage)?;
            }
            for operation in snapshot.operations {
                tx.execute(
                    "INSERT INTO credential_operations VALUES(?,?)",
                    params![operation.operation.operation_id, json(&operation)?],
                )
                .map_err(storage)?;
            }
            for item in snapshot.audit {
                audit(&tx, &item)?;
            }
            tx.execute(
                "UPDATE credential_header SET body=? WHERE singleton=1",
                [json(&snapshot.header)?],
            )
            .map_err(storage)?;
            audit(&tx, &entry)?;
            tx.commit().map_err(storage)?;
            Ok(())
        })
        .await
    }
    async fn audit(&self, after: u64, limit: usize) -> Result<Vec<CredentialAudit>> {
        self.run(move|db|{let mut s=db.prepare("SELECT sequence,body FROM credential_audit WHERE sequence>? ORDER BY sequence LIMIT ?").map_err(storage)?;let rows=s.query_map(params![after,limit.min(1000) as u64],|r|Ok((r.get::<_,u64>(0)?,r.get::<_,Vec<u8>>(1)?))).map_err(storage)?.collect::<std::result::Result<Vec<_>,_>>().map_err(storage)?;rows.into_iter().map(|(id,b)|{let mut e:CredentialAudit=parse(&b)?;e.sequence=id;Ok(e)}).collect()}).await
    }
    async fn snapshot_full(&self) -> Result<CredentialRepositorySnapshot> {
        self.run(|db| {
            let tx = db.transaction().map_err(storage)?;
            let snapshot = Self::full(&tx)?;
            tx.commit().map_err(storage)?;
            Ok(snapshot)
        })
        .await
    }
    async fn update_header(
        &self,
        expected: u64,
        h: VaultHeader,
        require_quiescent: bool,
        entry: CredentialAudit,
    ) -> Result<()> {
        self.run(move|db|{let tx=db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(storage)?;if header(&tx)?.is_none_or(|v|v.generation!=expected){return Err(err(ErrorCode::StaleGeneration,"Credential authority changed"));}if require_quiescent{let pending:u64=tx.query_row("SELECT COUNT(*) FROM credential_records WHERE json_extract(body,'$.metadata.state') IN ('refresh_prepared','refresh_dispatching')",[],|r|r.get(0)).map_err(storage)?;if pending>0{return Err(err(ErrorCode::Conflict,"Outstanding OAuth refresh receipts must be resolved before replacement"));}}tx.execute("UPDATE credential_header SET body=? WHERE singleton=1",[json(&h)?]).map_err(storage)?;audit(&tx,&entry)?;tx.commit().map_err(storage)?;Ok(())}).await
    }
    async fn replace_all(
        &self,
        expected: u64,
        snapshot: CredentialRepositorySnapshot,
        entry: CredentialAudit,
    ) -> Result<()> {
        self.run(move |db| {
            let tx = db
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(storage)?;
            if header(&tx)?
                .is_none_or(|v| v.generation != expected || v.admission != VaultAdmission::Fenced)
            {
                return Err(err(
                    ErrorCode::StaleGeneration,
                    "Credential destination is not fenced at the expected generation",
                ));
            }
            tx.execute("DELETE FROM credential_records", [])
                .map_err(storage)?;
            tx.execute("DELETE FROM credential_operations", [])
                .map_err(storage)?;
            tx.execute("DELETE FROM credential_audit", [])
                .map_err(storage)?;
            for row in snapshot.records {
                let r = &row.metadata.reference;
                tx.execute(
                    "INSERT INTO credential_records VALUES(?,?,?,?)",
                    params![r.namespace, r.id, row.metadata.revision, json(&row)?],
                )
                .map_err(storage)?;
            }
            for operation in snapshot.operations {
                tx.execute(
                    "INSERT INTO credential_operations VALUES(?,?)",
                    params![operation.operation.operation_id, json(&operation)?],
                )
                .map_err(storage)?;
            }
            for item in snapshot.audit {
                tx.execute(
                    "INSERT INTO credential_audit(sequence,body) VALUES(?,?)",
                    params![item.sequence, json(&item)?],
                )
                .map_err(storage)?;
            }
            tx.execute(
                "UPDATE credential_header SET body=? WHERE singleton=1",
                [json(&snapshot.header)?],
            )
            .map_err(storage)?;
            audit(&tx, &entry)?;
            tx.commit().map_err(storage)?;
            Ok(())
        })
        .await
    }
    async fn audit_namespace(
        &self,
        namespace: &str,
        after: u64,
        limit: usize,
    ) -> Result<Vec<CredentialAudit>> {
        let namespace = namespace.to_owned();
        self.run(move|db|{let mut statement=db.prepare("SELECT sequence,body FROM credential_audit WHERE sequence>? AND json_extract(body,'$.reference.namespace')=? ORDER BY sequence LIMIT ?").map_err(storage)?;let rows=statement.query_map(params![after,namespace,limit.min(1000)],|r|Ok((r.get::<_,u64>(0)?,r.get::<_,Vec<u8>>(1)?))).map_err(storage)?.collect::<std::result::Result<Vec<_>,_>>().map_err(storage)?;rows.into_iter().map(|(sequence,bytes)|{let mut item:CredentialAudit=parse(&bytes)?;item.sequence=sequence;Ok(item)}).collect()}).await
    }
    async fn append_audit(&self, entry: CredentialAudit) -> Result<()> {
        self.run(move |db| audit(db, &entry)).await
    }
}

impl SqliteRepository {
    fn full(db: &Connection) -> Result<CredentialRepositorySnapshot> {
        let mut total = 0u64;
        for (table, limit) in [
            ("credential_records", 10000u64),
            ("credential_operations", 100000),
            ("credential_audit", 100000),
        ] {
            let (count, bytes): (u64, u64) = db
                .query_row(
                    &format!("SELECT COUNT(*),COALESCE(SUM(LENGTH(body)),0) FROM {table}"),
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(storage)?;
            total = total.saturating_add(bytes);
            if count > limit || total > 64 * 1024 * 1024 {
                return Err(err(
                    ErrorCode::Unsupported,
                    "Credential checkpoint exceeds the bounded portable-state limit",
                ));
            }
        }
        fn bodies<T: serde::de::DeserializeOwned>(db: &Connection, sql: &str) -> Result<Vec<T>> {
            let mut s = db.prepare(sql).map_err(storage)?;
            let rows = s
                .query_map([], |r| r.get::<_, Vec<u8>>(0))
                .map_err(storage)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(storage)?;
            rows.iter().map(|row| parse(row)).collect()
        }
        let header = header(db)?.ok_or_else(|| {
            err(
                ErrorCode::Unavailable,
                "Credential vault is not initialized",
            )
        })?;
        let records = bodies(
            db,
            "SELECT body FROM credential_records ORDER BY namespace,id",
        )?;
        let operations = bodies(db, "SELECT body FROM credential_operations ORDER BY id")?;
        let mut audit_entries = Vec::new();
        let mut statement = db
            .prepare("SELECT sequence,body FROM credential_audit ORDER BY sequence")
            .map_err(storage)?;
        for value in statement
            .query_map([], |r| Ok((r.get::<_, u64>(0)?, r.get::<_, Vec<u8>>(1)?)))
            .map_err(storage)?
        {
            let (sequence, bytes) = value.map_err(storage)?;
            let mut entry: CredentialAudit = parse(&bytes)?;
            entry.sequence = sequence;
            audit_entries.push(entry);
        }
        Ok(CredentialRepositorySnapshot {
            header,
            records,
            operations,
            audit: audit_entries,
        })
    }
}
