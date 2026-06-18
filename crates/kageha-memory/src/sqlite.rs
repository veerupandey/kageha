use fs2::FileExt;
use kageha_contracts::*;
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};
#[derive(Clone)]
pub struct SqliteMemory {
    db: std::sync::Arc<Mutex<Connection>>,
    _lock: std::sync::Arc<std::fs::File>,
    instance: String,
    fence: Option<MemoryFence>,
}
fn fail(e: impl std::fmt::Display) -> ServiceError {
    ServiceError::new(ErrorCode::Unavailable, format!("Memory storage: {e}"))
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn bounded(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control)
}
fn validate_record(r: &MemoryRecord) -> Result<()> {
    if !bounded(&r.id, 128)
        || !bounded(&r.namespace, 512)
        || !bounded(&r.owner, 128)
        || !bounded(&r.kind, 64)
        || r.body.len() > 65536
        || r.acl.len() > 256
        || r.acl.iter().any(|s| !bounded(s, 128))
        || serde_json::to_vec(&r.metadata).map_err(fail)?.len() > 65536
        || r.revision == 0
        || r.provenance.as_ref().is_some_and(|p| {
            !bounded(&p.actor, 128) || !bounded(&p.origin, 128) || !bounded(&p.source_id, 512)
        })
    {
        return Err(ServiceError::new(
            ErrorCode::Invalid,
            "Invalid memory record or size limit",
        ));
    }
    Ok(())
}
impl SqliteMemory {
    pub fn open(path: &Path) -> Result<Self> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .map_err(fail)?;
        let mut opts = std::fs::OpenOptions::new();
        opts.read(true).write(true).create(true).mode(0o600);
        if std::fs::symlink_metadata(path.with_extension("lock"))
            .is_ok_and(|m| m.file_type().is_symlink())
        {
            return Err(ServiceError::new(
                ErrorCode::Invalid,
                "Memory lock cannot be a symlink",
            ));
        }
        let lock = opts.open(path.with_extension("lock")).map_err(fail)?;
        lock.try_lock_exclusive().map_err(|_| {
            ServiceError::new(ErrorCode::Conflict, "Memory database already has an owner")
        })?;
        if path.exists()
            && std::fs::symlink_metadata(path)
                .map_err(fail)?
                .file_type()
                .is_symlink()
        {
            return Err(ServiceError::new(
                ErrorCode::Invalid,
                "Memory database cannot be a symlink",
            ));
        }
        drop(opts.open(path).map_err(fail)?);
        let db = Connection::open(path).map_err(fail)?;
        db.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(fail)?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS memory_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL); CREATE TABLE IF NOT EXISTS memory_records(namespace TEXT,id TEXT,body TEXT NOT NULL,PRIMARY KEY(namespace,id)); CREATE TABLE IF NOT EXISTS memory_operations(namespace TEXT,actor TEXT,request_id TEXT,digest TEXT NOT NULL,result TEXT NOT NULL,PRIMARY KEY(namespace,actor,request_id)); INSERT OR IGNORE INTO memory_meta VALUES ('sequence','0');").map_err(fail)?;
        db.execute(
            "INSERT OR IGNORE INTO memory_meta VALUES ('instance',?)",
            [uuid::Uuid::new_v4().to_string()],
        )
        .map_err(fail)?;
        let instance = db
            .query_row(
                "SELECT value FROM memory_meta WHERE key='instance'",
                [],
                |r| r.get(0),
            )
            .map_err(fail)?;
        Ok(Self {
            db: std::sync::Arc::new(Mutex::new(db)),
            _lock: std::sync::Arc::new(lock),
            instance,
            fence: None,
        })
    }
    fn read_fence(db: &Connection) -> Result<Option<MemoryFence>> {
        let value: Option<String> = db
            .query_row(
                "SELECT value FROM memory_meta WHERE key='writer_fence'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(fail)?;
        value
            .map(|v| serde_json::from_str(&v).map_err(fail))
            .transpose()
    }
    fn authorize_write(&self, db: &Connection) -> Result<()> {
        let current = Self::read_fence(db)?;
        match (current.as_ref(), self.fence.as_ref()) {
            (None, None) => Ok(()),
            (Some(current), Some(supplied)) if current == supplied && !current.retired => Ok(()),
            _ => Err(ServiceError::new(
                ErrorCode::StaleGeneration,
                "Writer authority changed or was retired; no mutation was dispatched",
            )),
        }
    }
    fn record(db: &Connection, namespace: &str, id: &str) -> Result<Option<MemoryRecord>> {
        let value: Option<String> = db
            .query_row(
                "SELECT body FROM memory_records WHERE namespace=? AND id=?",
                params![namespace, id],
                |r| r.get(0),
            )
            .optional()
            .map_err(fail)?;
        value
            .map(|s| serde_json::from_str(&s).map_err(fail))
            .transpose()
    }
    fn repeat(
        db: &Connection,
        access: &MemoryAccess,
        id: &str,
        digest: &str,
    ) -> Result<Option<MemoryRecord>> {
        if !bounded(id, 128) {
            return Err(ServiceError::new(
                ErrorCode::Invalid,
                "A request_id of at most 128 bytes is required",
            ));
        }
        let previous:Option<(String,String)>=db.query_row("SELECT digest,result FROM memory_operations WHERE namespace=? AND actor=? AND request_id=?",params![access.namespace,access.actor,id],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(fail)?;
        match previous {
            None => Ok(None),
            Some((hash, result)) if hash == digest => {
                Ok(Some(serde_json::from_str(&result).map_err(fail)?))
            }
            Some(_) => Err(ServiceError::new(
                ErrorCode::Conflict,
                "Memory request_id reused with different arguments",
            )),
        }
    }
    fn commit(
        db: &Connection,
        access: &MemoryAccess,
        request_id: &str,
        digest: &str,
        record: &MemoryRecord,
    ) -> Result<()> {
        let body = serde_json::to_string(record).map_err(fail)?;
        db.execute("INSERT INTO memory_records VALUES (?,?,?) ON CONFLICT(namespace,id) DO UPDATE SET body=excluded.body",params![record.namespace,record.id,body]).map_err(fail)?;
        db.execute(
            "INSERT INTO memory_operations VALUES (?,?,?,?,?)",
            params![access.namespace, access.actor, request_id, digest, body],
        )
        .map_err(fail)?;
        db.execute(
            "UPDATE memory_meta SET value=? WHERE key='sequence'",
            [record.revision.to_string()],
        )
        .map_err(fail)?;
        Ok(())
    }
    fn sequence(db: &Connection) -> Result<u64> {
        let value: String = db
            .query_row(
                "SELECT value FROM memory_meta WHERE key='sequence'",
                [],
                |r| r.get(0),
            )
            .map_err(fail)?;
        value.parse().map_err(fail)
    }
    fn rows(db: &Connection, access: &MemoryAccess) -> Result<Vec<MemoryRecord>> {
        let mut q = db
            .prepare("SELECT body FROM memory_records WHERE namespace=? ORDER BY id")
            .map_err(fail)?;
        let rows = q
            .query_map([&access.namespace], |r| r.get::<_, String>(0))
            .map_err(fail)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(fail)?;
        rows.into_iter()
            .map(|r| serde_json::from_str::<MemoryRecord>(&r).map_err(fail))
            .filter(|r| r.as_ref().is_err() || r.as_ref().is_ok_and(|v| v.visible(access)))
            .collect()
    }
}
#[async_trait::async_trait]
impl MemoryPort for SqliteMemory {
    async fn fence_status(&self) -> Result<Option<MemoryFence>> {
        Self::read_fence(&*self.db.lock().map_err(fail)?)
    }
    async fn claim_fence(
        &self,
        owner: &str,
        expected_epoch: u64,
        request_id: &str,
    ) -> Result<MemoryFence> {
        if !bounded(owner, 128) || !bounded(request_id, 128) {
            return Err(ServiceError::new(
                ErrorCode::Invalid,
                "Invalid writer identity",
            ));
        }
        let mut db = self.db.lock().map_err(fail)?;
        let tx = db.transaction().map_err(fail)?;
        let old = Self::read_fence(&tx)?;
        if let Some(fence) = &old {
            if fence.owner == owner
                && fence.claim_id == request_id
                && expected_epoch.checked_add(1) == Some(fence.epoch)
            {
                return Ok(fence.clone());
            }
        }
        if old.as_ref().map_or(0, |v| v.epoch) != expected_epoch {
            return Err(ServiceError::new(
                ErrorCode::StaleGeneration,
                "Writer epoch changed",
            ));
        }
        let fence = MemoryFence {
            owner: owner.into(),
            epoch: expected_epoch
                .checked_add(1)
                .ok_or_else(|| ServiceError::new(ErrorCode::Conflict, "Writer epoch exhausted"))?,
            retired: false,
            claim_id: request_id.into(),
        };
        tx.execute("INSERT INTO memory_meta VALUES ('writer_fence',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[serde_json::to_string(&fence).map_err(fail)?]).map_err(fail)?;
        tx.commit().map_err(fail)?;
        Ok(fence)
    }
    async fn retire_fence(&self, fence: &MemoryFence) -> Result<()> {
        let mut db = self.db.lock().map_err(fail)?;
        let tx = db.transaction().map_err(fail)?;
        let current = Self::read_fence(&tx)?;
        if current.as_ref().is_none_or(|v| {
            v.owner != fence.owner || v.epoch != fence.epoch || v.claim_id != fence.claim_id
        }) {
            return Err(ServiceError::new(
                ErrorCode::StaleGeneration,
                "Writer epoch changed",
            ));
        }
        let mut retired = fence.clone();
        retired.retired = true;
        tx.execute(
            "UPDATE memory_meta SET value=? WHERE key='writer_fence'",
            [serde_json::to_string(&retired).map_err(fail)?],
        )
        .map_err(fail)?;
        tx.commit().map_err(fail)?;
        Ok(())
    }
    fn with_fence(&self, fence: MemoryFence) -> Result<std::sync::Arc<dyn MemoryPort>> {
        let mut scoped = self.clone();
        scoped.fence = Some(fence);
        Ok(std::sync::Arc::new(scoped))
    }
    async fn descriptor(&self) -> Result<Descriptor> {
        Ok(Descriptor {
            protocol_version: VERSION,
            instance_id: self.instance.clone(),
            component: "memory".into(),
            adapter: "sqlite-records".into(),
            capabilities: memory_capabilities(),
        })
    }
    async fn put(&self, r: PutMemory) -> Result<MemoryRecord> {
        r.access.validate()?;
        let digest = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&r).map_err(fail)?)
        );
        let mut db = self.db.lock().map_err(fail)?;
        let tx = db.transaction().map_err(fail)?;
        self.authorize_write(&tx)?;
        if let Some(record) = Self::repeat(&tx, &r.access, &r.request_id, &digest)? {
            return Ok(record);
        }
        let old = Self::record(&tx, &r.access.namespace, &r.id)?;
        if old.as_ref().is_some_and(|v| v.owner != r.access.actor) {
            return Err(ServiceError::new(
                ErrorCode::Unauthorized,
                "Only the record owner can change memory",
            ));
        }
        if r.expected_revision
            .is_some_and(|v| v != old.as_ref().map(|v| v.revision).unwrap_or(0))
        {
            return Err(ServiceError::new(
                ErrorCode::Conflict,
                "Memory revision changed",
            ));
        }
        let record = MemoryRecord {
            id: r.id,
            namespace: r.access.namespace.clone(),
            owner: r.access.actor.clone(),
            kind: r.kind,
            body: r.body,
            metadata: r.metadata,
            acl: r.acl,
            revision: Self::sequence(&tx)? + 1,
            created_ms: old.as_ref().map(|v| v.created_ms).unwrap_or_else(now),
            updated_ms: now(),
            provenance: old.as_ref().and_then(|v| v.provenance.clone()).or_else(|| {
                Some(MemoryProvenance {
                    actor: r.access.actor.clone(),
                    origin: "memory-operation".into(),
                    source_id: r.request_id.clone(),
                })
            }),
            tombstone: false,
        };
        validate_record(&record)?;
        Self::commit(&tx, &r.access, &r.request_id, &digest, &record)?;
        tx.commit().map_err(fail)?;
        Ok(record)
    }
    async fn get(&self, access: &MemoryAccess, id: &str) -> Result<Option<MemoryRecord>> {
        access.validate()?;
        Ok(
            Self::record(&*self.db.lock().map_err(fail)?, &access.namespace, id)?
                .filter(|r| r.visible(access)),
        )
    }
    async fn delete(&self, r: DeleteMemory) -> Result<MemoryRecord> {
        r.access.validate()?;
        let digest = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&r).map_err(fail)?)
        );
        let mut db = self.db.lock().map_err(fail)?;
        let tx = db.transaction().map_err(fail)?;
        self.authorize_write(&tx)?;
        if let Some(record) = Self::repeat(&tx, &r.access, &r.request_id, &digest)? {
            return Ok(record);
        }
        let mut record = Self::record(&tx, &r.access.namespace, &r.id)?
            .filter(|v| v.owner == r.access.actor)
            .ok_or_else(|| {
                ServiceError::new(
                    ErrorCode::Unauthorized,
                    "Memory is unavailable to this writer",
                )
            })?;
        if r.expected_revision.is_some_and(|v| v != record.revision) {
            return Err(ServiceError::new(
                ErrorCode::Conflict,
                "Memory revision changed",
            ));
        }
        record.revision = Self::sequence(&tx)? + 1;
        record.updated_ms = now();
        record.tombstone = true;
        record.body.clear();
        record.metadata.clear();
        Self::commit(&tx, &r.access, &r.request_id, &digest, &record)?;
        // Preserve operation identity/digests for reconciliation without carrying
        // deleted content into a new destination through historical receipts.
        tx.execute("UPDATE memory_operations SET result=json_set(result,'$.body','', '$.metadata',json('{}'), '$.tombstone',json('true')) WHERE namespace=? AND json_extract(result,'$.id')=?", params![r.access.namespace, r.id]).map_err(fail)?;
        tx.commit().map_err(fail)?;
        Ok(record)
    }
    async fn search(
        &self,
        access: &MemoryAccess,
        query: &str,
        limit: usize,
    ) -> Result<Vec<MemoryRecord>> {
        access.validate()?;
        if query.len() > 4096 || limit == 0 || limit > 100 {
            return Err(ServiceError::new(
                ErrorCode::Invalid,
                "Search exceeds limits",
            ));
        }
        let records = Self::rows(&*self.db.lock().map_err(fail)?, access)?;
        #[cfg(feature = "zvec")]
        if crate::semantic::enabled() {
            let mut recent: Vec<_> = records.iter().collect();
            recent.sort_by_key(|record| std::cmp::Reverse(record.updated_ms));
            recent.truncate(crate::semantic::MAX_CANDIDATES);
            let bodies: Vec<_> = recent.iter().map(|record| record.body.clone()).collect();
            if let Ok(ranks) = crate::semantic::rank(query, &bodies, limit).await {
                let current = Self::rows(&*self.db.lock().map_err(fail)?, access)?;
                return Ok(ranks.into_iter().filter_map(|index| {
                    let original = recent.get(index)?;
                    current.iter().find(|record| record.id == original.id &&
                        record.revision == original.revision).cloned()
                }).collect());
            }
        }
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return Ok(records.into_iter().take(limit).collect());
        }
        let mut terms: Vec<_> = query
            .split(|c: char| !c.is_alphanumeric())
            .filter(|term| !term.is_empty())
            .filter(|term| {
                !matches!(
                    *term,
                    "a" | "an"
                        | "and"
                        | "as"
                        | "at"
                        | "by"
                        | "for"
                        | "from"
                        | "i"
                        | "in"
                        | "is"
                        | "me"
                        | "my"
                        | "of"
                        | "on"
                        | "or"
                        | "the"
                        | "to"
                        | "what"
                        | "where"
                        | "with"
                        | "you"
                        | "your"
                )
            })
            .collect();
        terms.sort_unstable();
        terms.dedup();
        // A query containing only punctuation or common words still retains
        // its previous literal-search behavior, rather than matching all notes.
        if terms.is_empty() {
            terms.push(&query);
        }
        let min_matches = terms.len().div_ceil(2);
        let postal_query = terms.contains(&"postal") && terms.contains(&"code");
        let mut scored: Vec<_> = records
            .into_iter()
            .filter_map(|record| {
                let id = record.id.to_lowercase();
                let body = record.body.to_lowercase();
                let actual = terms
                    .iter()
                    .filter(|term| id.contains(**term) || body.contains(**term))
                    .count();
                // Profile notes may include a postal code in a residential
                // address without labelling it. Return such notes as candidates,
                // never as proof that a postal code is actually present.
                let words: Vec<_> = body
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|word| !word.is_empty())
                    .collect();
                let residential = postal_query && words.windows(2).any(|pair| {
                    pair[1] == "address"
                        && (matches!(pair[0], "home" | "street" | "mailing")
                            || pair[0].starts_with("residen"))
                });
                let matched = terms
                    .iter()
                    .filter(|term| {
                        id.contains(**term)
                            || body.contains(**term)
                            || residential && matches!(**term, "postal" | "code")
                    })
                    .count();
                (matched >= min_matches).then(|| {
                    // Full literal matches rank first, followed by all-term and
                    // partial actual matches before the residential fallback.
                    let literal = id.contains(&query) || body.contains(&query);
                    (literal, actual == terms.len(), actual, matched, record)
                })
            })
            .collect();
        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| b.1.cmp(&a.1))
                .then_with(|| b.2.cmp(&a.2))
                .then_with(|| b.3.cmp(&a.3))
                .then_with(|| b.4.updated_ms.cmp(&a.4.updated_ms))
                .then_with(|| a.4.id.cmp(&b.4.id))
        });
        Ok(scored
            .into_iter()
            .take(limit)
            .map(|(_, _, _, _, record)| record)
            .collect())
    }
    async fn list(
        &self,
        access: &MemoryAccess,
        after: Option<String>,
        limit: usize,
    ) -> Result<MemoryPage> {
        access.validate()?;
        if limit == 0 || limit > 100 {
            return Err(ServiceError::new(
                ErrorCode::Invalid,
                "Page size must be 1 to 100",
            ));
        }
        let mut rows: Vec<_> = Self::rows(&*self.db.lock().map_err(fail)?, access)?
            .into_iter()
            .filter(|r| after.as_ref().is_none_or(|id| r.id > *id))
            .take(limit + 1)
            .collect();
        let next = if rows.len() > limit {
            rows.pop();
            rows.last().map(|v| v.id.clone())
        } else {
            None
        };
        Ok(MemoryPage {
            records: rows,
            next,
        })
    }
    async fn snapshot(&self) -> Result<MemorySnapshot> {
        let db = self.db.lock().map_err(fail)?;
        let mut q = db
            .prepare("SELECT body FROM memory_records ORDER BY namespace,id")
            .map_err(fail)?;
        let rows = q
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(fail)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(fail)?;
        let records = rows
            .into_iter()
            .map(|r| serde_json::from_str(&r).map_err(fail))
            .collect::<Result<Vec<_>>>()?;
        let mut q=db.prepare("SELECT namespace,actor,request_id,digest,result FROM memory_operations ORDER BY namespace,actor,request_id").map_err(fail)?;
        let rows = q
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })
            .map_err(fail)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(fail)?;
        let receipts = rows
            .into_iter()
            .map(|(namespace, actor, request_id, digest, result)| {
                Ok(MemoryReceipt {
                    namespace,
                    actor,
                    request_id,
                    digest,
                    record: serde_json::from_str(&result).map_err(fail)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let snapshot = MemorySnapshot {
            schema_version: 1,
            watermark: Self::sequence(&db)?,
            records,
            receipts,
        };
        if serde_json::to_vec(&snapshot).map_err(fail)?.len() > 32 * 1024 * 1024 {
            return Err(ServiceError::new(
                ErrorCode::Invalid,
                "Snapshot exceeds 32 MiB; paged migration is required",
            ));
        }
        Ok(snapshot)
    }
    async fn import(&self, snapshot: MemorySnapshot) -> Result<()> {
        validate_snapshot(&snapshot)?;
        let mut db = self.db.lock().map_err(fail)?;
        let tx = db.transaction().map_err(fail)?;
        self.authorize_write(&tx)?;
        tx.execute("DELETE FROM memory_records", []).map_err(fail)?;
        tx.execute("DELETE FROM memory_operations", [])
            .map_err(fail)?;
        for record in &snapshot.records {
            tx.execute(
                "INSERT INTO memory_records VALUES (?,?,?)",
                params![
                    record.namespace,
                    record.id,
                    serde_json::to_string(record).map_err(fail)?
                ],
            )
            .map_err(fail)?;
        }
        for receipt in &snapshot.receipts {
            tx.execute(
                "INSERT INTO memory_operations VALUES (?,?,?,?,?)",
                params![
                    receipt.namespace,
                    receipt.actor,
                    receipt.request_id,
                    receipt.digest,
                    serde_json::to_string(&receipt.record).map_err(fail)?
                ],
            )
            .map_err(fail)?;
        }
        tx.execute(
            "UPDATE memory_meta SET value=? WHERE key='sequence'",
            [snapshot.watermark.to_string()],
        )
        .map_err(fail)?;
        tx.commit().map_err(fail)?;
        Ok(())
    }
}
pub fn validate_snapshot(snapshot: &MemorySnapshot) -> Result<()> {
    if snapshot.schema_version != 1
        || snapshot.records.len() > 100000
        || serde_json::to_vec(snapshot).map_err(fail)?.len() > 32 * 1024 * 1024
    {
        return Err(ServiceError::new(
            ErrorCode::Invalid,
            "Invalid snapshot version or size",
        ));
    }
    let mut ids = std::collections::BTreeSet::new();
    for record in &snapshot.records {
        validate_record(record)?;
        if record.revision > snapshot.watermark || !ids.insert((&record.namespace, &record.id)) {
            return Err(ServiceError::new(
                ErrorCode::Invalid,
                "Snapshot revisions or IDs are invalid",
            ));
        }
    }
    let mut ops = std::collections::BTreeSet::new();
    for r in &snapshot.receipts {
        validate_record(&r.record)?;
        if r.record.revision > snapshot.watermark
            || r.namespace != r.record.namespace
            || r.actor != r.record.owner
            || !bounded(&r.request_id, 128)
            || r.digest.len() != 64
            || !r.digest.bytes().all(|b| b.is_ascii_hexdigit())
            || !ops.insert((&r.namespace, &r.actor, &r.request_id))
        {
            return Err(ServiceError::new(
                ErrorCode::Invalid,
                "Snapshot receipt is invalid",
            ));
        }
    }
    Ok(())
}
