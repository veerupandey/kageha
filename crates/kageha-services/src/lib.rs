//! Logical bindings and durable memory replacement. Independent of runtime and WebUI.
use fs2::FileExt;
use kageha_contracts::MemoryCall as Call;
use kageha_contracts::*;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::{Arc, Mutex},
};
use tokio::sync::RwLock;
fn storage(e: impl std::fmt::Display) -> ServiceError {
    ServiceError::new(ErrorCode::Unavailable, format!("Service registry: {e}"))
}
fn conflict(message: &str) -> ServiceError {
    ServiceError::new(ErrorCode::Conflict, message)
}
fn json<T: serde::Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(storage)
}
/// All callers retain this logical router, never a permanent vendor client.
pub struct Registry {
    db: Mutex<Connection>,
    owner: String,
    authority: tokio::sync::Mutex<()>,
    incarnation_bound: Mutex<BTreeSet<String>>,
    _lock: std::fs::File,
    providers: RwLock<BTreeMap<String, Arc<dyn MemoryPort>>>,
    tool_lanes: Mutex<BTreeMap<String, std::sync::Weak<tokio::sync::Mutex<()>>>>,
    gate: RwLock<()>,
    replacement: tokio::sync::Mutex<()>,
    admission: tokio::sync::Semaphore,
}
impl Registry {
    pub fn open(path: &Path) -> Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(storage)?;
        }
        for p in [path.to_path_buf(), path.with_extension("lock")] {
            if std::fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(conflict("Registry paths cannot be symlinks"));
            }
        }
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .open(path.with_extension("lock"))
            .map_err(storage)?;
        lock.try_lock_exclusive()
            .map_err(|_| conflict("Registry already has an owner"))?;
        drop(
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .mode(0o600)
                .open(path)
                .map_err(storage)?,
        );
        let db = Connection::open(path).map_err(storage)?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS bindings(name TEXT PRIMARY KEY,body TEXT NOT NULL); CREATE TABLE IF NOT EXISTS replacements(id TEXT PRIMARY KEY,body TEXT NOT NULL); CREATE TABLE IF NOT EXISTS events(cursor INTEGER PRIMARY KEY AUTOINCREMENT,body TEXT NOT NULL); CREATE TABLE IF NOT EXISTS calls(binding TEXT,generation INTEGER,request_id TEXT,digest TEXT,state TEXT,PRIMARY KEY(binding,generation,request_id)); CREATE TABLE IF NOT EXISTS identities(provider TEXT PRIMARY KEY,instance TEXT NOT NULL);").map_err(storage)?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS registry_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL); CREATE TABLE IF NOT EXISTS provider_fences(provider TEXT PRIMARY KEY,body TEXT NOT NULL); CREATE TABLE IF NOT EXISTS replacement_checkpoints(id TEXT PRIMARY KEY,body TEXT NOT NULL); CREATE TABLE IF NOT EXISTS binding_blocks(name TEXT PRIMARY KEY,body TEXT NOT NULL);").map_err(storage)?;
        db.execute(
            "INSERT OR IGNORE INTO registry_meta VALUES ('owner',?)",
            [uuid::Uuid::new_v4().to_string()],
        )
        .map_err(storage)?;
        let owner = db
            .query_row(
                "SELECT value FROM registry_meta WHERE key='owner'",
                [],
                |r| r.get(0),
            )
            .map_err(storage)?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS fence_claims(provider TEXT PRIMARY KEY,expected_epoch INTEGER,request_id TEXT NOT NULL);").map_err(storage)?;
        db.execute(
            "UPDATE calls SET state='uncertain' WHERE state='pending'",
            [],
        )
        .map_err(storage)?;
        Ok(Self {
            db: Mutex::new(db),
            owner,
            authority: tokio::sync::Mutex::new(()),
            incarnation_bound: Mutex::new(BTreeSet::new()),
            _lock: lock,
            providers: RwLock::new(BTreeMap::new()),
            gate: RwLock::new(()),
            replacement: tokio::sync::Mutex::new(()),
            admission: tokio::sync::Semaphore::new(64),
            tool_lanes: Mutex::new(BTreeMap::new()),
        })
    }
    pub async fn register(&self, id: String, port: Arc<dyn MemoryPort>) -> Result<Descriptor> {
        if id.is_empty() || id.len() > 128 {
            return Err(conflict("Invalid provider ID"));
        }
        let descriptor = port.descriptor().await?;
        descriptor.require(&memory_capabilities())?;
        if descriptor.component != "memory" {
            return Err(conflict("Expected a memory component"));
        }
        let mut providers = self.providers.write().await;
        if providers.contains_key(&id) {
            return Err(conflict("Provider already registered"));
        }
        let db = self.db.lock().map_err(storage)?;
        let known: Option<String> = db
            .query_row(
                "SELECT instance FROM identities WHERE provider=?",
                [&id],
                |r| r.get(0),
            )
            .optional()
            .map_err(storage)?;
        if known.is_some_and(|v| v != descriptor.instance_id) {
            return Err(conflict(
                "Provider identity changed; register a new destination ID",
            ));
        }
        db.execute(
            "INSERT OR IGNORE INTO identities VALUES (?,?)",
            params![id, descriptor.instance_id],
        )
        .map_err(storage)?;
        providers.insert(id, port);
        Ok(descriptor)
    }
    async fn provider(&self, id: &str) -> Result<Arc<dyn MemoryPort>> {
        self.providers.read().await.get(id).cloned().ok_or_else(|| {
            ServiceError::new(ErrorCode::Unavailable, "Binding provider is not connected")
        })
    }
    fn stored_fence(&self, id: &str) -> Result<Option<MemoryFence>> {
        let value: Option<String> = self
            .db
            .lock()
            .map_err(storage)?
            .query_row(
                "SELECT body FROM provider_fences WHERE provider=?",
                [id],
                |r| r.get(0),
            )
            .optional()
            .map_err(storage)?;
        value
            .map(|v| serde_json::from_str(&v).map_err(storage))
            .transpose()
    }
    async fn scoped_provider(&self, id: &str) -> Result<Arc<dyn MemoryPort>> {
        let fence = self
            .stored_fence(id)?
            .ok_or_else(|| conflict("Provider has no writer authority"))?;
        self.provider(id).await?.with_fence(fence)
    }
    async fn own_provider(
        &self,
        id: &str,
        reactivate: bool,
        renew: bool,
    ) -> Result<Arc<dyn MemoryPort>> {
        let _authority = self.authority.lock().await;
        // Every provider, including a staging destination, gets a fresh epoch
        // in this coordinator incarnation. An old migration worker may still
        // be alive after a journal is restored elsewhere.
        let renew = renew || !self.incarnation_bound.lock().map_err(storage)?.contains(id);
        let port = self.provider(id).await?;
        let current = port.fence_status().await?;
        if current.as_ref().is_some_and(|f| f.owner != self.owner) {
            return Err(conflict("Provider belongs to another control plane; explicit ownership transfer is required"));
        }
        let pending: Option<(u64, String)> = self
            .db
            .lock()
            .map_err(storage)?
            .query_row(
                "SELECT expected_epoch,request_id FROM fence_claims WHERE provider=?",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(storage)?;
        let reconciled =
            pending
                .as_ref()
                .zip(current.as_ref())
                .is_some_and(|((epoch, request), fence)| {
                    epoch.checked_add(1) == Some(fence.epoch)
                        && fence.claim_id == *request
                        && !fence.retired
                });
        if !renew
            && !reconciled
            && self
                .stored_fence(id)?
                .zip(current.clone())
                .is_some_and(|(old, current)| old.epoch != current.epoch)
        {
            return Err(ServiceError::new(
                ErrorCode::StaleGeneration,
                "Provider writer epoch changed outside this coordinator",
            ));
        }
        let fence = match current {
            Some(fence) if !renew && (reconciled || !fence.retired) => fence,
            Some(fence) if fence.retired && !reactivate => {
                return Err(ServiceError::new(
                    ErrorCode::StaleGeneration,
                    "Provider writer is retired",
                ))
            }
            current => {
                let expected_epoch = current.map_or(0, |v| v.epoch);
                let request_id = uuid::Uuid::new_v4().to_string();
                self.db.lock().map_err(storage)?.execute("INSERT INTO fence_claims VALUES (?,?,?) ON CONFLICT(provider) DO UPDATE SET expected_epoch=excluded.expected_epoch,request_id=excluded.request_id",params![id,expected_epoch,request_id]).map_err(storage)?;
                port.claim_fence(&self.owner, expected_epoch, &request_id)
                    .await?
            }
        };
        let mut db = self.db.lock().map_err(storage)?;
        let tx = db.transaction().map_err(storage)?;
        tx.execute("INSERT INTO provider_fences VALUES (?,?) ON CONFLICT(provider) DO UPDATE SET body=excluded.body",params![id,json(&fence)?]).map_err(storage)?;
        tx.execute("DELETE FROM fence_claims WHERE provider=?", [id])
            .map_err(storage)?;
        tx.commit().map_err(storage)?;
        self.incarnation_bound
            .lock()
            .map_err(storage)?
            .insert(id.into());
        port.with_fence(fence)
    }
    fn tool_lane(&self, access: &MemoryAccess) -> Result<Arc<tokio::sync::Mutex<()>>> {
        let key = json(access)?;
        let mut lanes = self.tool_lanes.lock().map_err(storage)?;
        if let Some(lane) = lanes.get(&key).and_then(std::sync::Weak::upgrade) {
            return Ok(lane);
        }
        lanes.retain(|_, lane| lane.strong_count() > 0);
        if lanes.len() >= 256 {
            return Err(ServiceError::new(
                ErrorCode::Unavailable,
                "Memory tool scope capacity reached",
            ));
        }
        let lane = Arc::new(tokio::sync::Mutex::new(()));
        lanes.insert(key, Arc::downgrade(&lane));
        Ok(lane)
    }
    /// Operator-visible identities needed for destination-scoped replacement decisions.
    pub async fn descriptors(&self) -> Result<BTreeMap<String, Descriptor>> {
        let providers = self
            .providers
            .read()
            .await
            .iter()
            .map(|(id, port)| (id.clone(), port.clone()))
            .collect::<Vec<_>>();
        let mut result = BTreeMap::new();
        for (id, port) in providers {
            result.insert(id, port.descriptor().await?);
        }
        Ok(result)
    }
    pub fn binding(&self) -> Result<Option<Binding>> {
        let db = self.db.lock().map_err(storage)?;
        let body: Option<String> = db
            .query_row(
                "SELECT body FROM bindings WHERE name='memory.default'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(storage)?;
        body.map(|s| serde_json::from_str(&s).map_err(storage))
            .transpose()
    }
    pub async fn initialize(&self, provider_id: &str) -> Result<Binding> {
        let _guard = self.gate.write().await;
        if let Some(binding) = self.binding()? {
            self.own_provider(&binding.provider_id, true, true).await?;
            return Ok(binding);
        }
        self.provider(provider_id)
            .await?
            .descriptor()
            .await?
            .require(&memory_capabilities())?;
        self.own_provider(provider_id, true, true).await?;
        let binding = Binding {
            name: "memory.default".into(),
            provider_id: provider_id.into(),
            generation: 1,
            required: memory_capabilities(),
        };
        self.db
            .lock()
            .map_err(storage)?
            .execute(
                "INSERT INTO bindings VALUES (?,?)",
                params![binding.name, json(&binding)?],
            )
            .map_err(storage)?;
        Ok(binding)
    }
    pub fn job(&self, id: &str) -> Result<Option<Replacement>> {
        let body: Option<String> = self
            .db
            .lock()
            .map_err(storage)?
            .query_row("SELECT body FROM replacements WHERE id=?", [id], |r| {
                r.get(0)
            })
            .optional()
            .map_err(storage)?;
        body.map(|s| serde_json::from_str(&s).map_err(storage))
            .transpose()
    }
    fn save(&self, job: &Replacement) -> Result<()> {
        let mut db = self.db.lock().map_err(storage)?;
        let tx = db.transaction().map_err(storage)?;
        let body = json(job)?;
        tx.execute("INSERT INTO replacements VALUES (?,?) ON CONFLICT(id) DO UPDATE SET body=excluded.body",params![job.id,body]).map_err(storage)?;
        tx.execute("INSERT INTO events(body) VALUES (?)", [&body])
            .map_err(storage)?;
        tx.commit().map_err(storage)?;
        Ok(())
    }
    pub fn events(&self, after: u64) -> Result<Vec<(u64, Replacement)>> {
        let db = self.db.lock().map_err(storage)?;
        let mut q = db
            .prepare("SELECT cursor,body FROM events WHERE cursor>? ORDER BY cursor LIMIT 100")
            .map_err(storage)?;
        let rows = q
            .query_map([after], |r| {
                Ok((r.get::<_, u64>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(storage)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(storage)?;
        rows.into_iter()
            .map(|(id, s)| Ok((id, serde_json::from_str(&s).map_err(storage)?)))
            .collect()
    }
    pub async fn call(
        &self,
        expected_generation: Option<u64>,
        call: Call,
    ) -> Result<Routed<Value>> {
        let _admission = self.admission.try_acquire().map_err(|_| {
            ServiceError::new(ErrorCode::Unavailable, "Memory admission queue is full")
        })?;
        let _guard = self.gate.read().await;
        let binding = self
            .binding()?
            .ok_or_else(|| conflict("No memory binding"))?;
        if expected_generation.is_some_and(|g| g != binding.generation) {
            return Err(ServiceError::new(
                ErrorCode::StaleGeneration,
                "Logical binding generation changed",
            ));
        }
        if let Some(error) = self.binding_block()? {
            return Err(error);
        }
        let provider = self.scoped_provider(&binding.provider_id).await?;
        let request = match &call {
            Call::Put { request } => Some((&request.request_id, &request.access)),
            Call::Delete { request } => Some((&request.request_id, &request.access)),
            Call::Snapshot | Call::Import { .. } => {
                return Err(ServiceError::new(
                    ErrorCode::Unauthorized,
                    "Migration requires the replacement controller",
                ))
            }
            _ => None,
        };
        let key = if let Some((request_id, access)) = request {
            access.validate()?;
            if request_id.is_empty() || request_id.len() > 128 {
                return Err(conflict("A bounded mutation request_id is required"));
            }
            let key = json(&(access, request_id))?;
            let digest = format!(
                "{:x}",
                Sha256::digest(serde_json::to_vec(&call).map_err(storage)?)
            );
            let db = self.db.lock().map_err(storage)?;
            let previous: Option<String> = db
                .query_row(
                    "SELECT digest FROM calls WHERE binding=? AND generation=? AND request_id=?",
                    params![binding.name, binding.generation, key],
                    |r| r.get(0),
                )
                .optional()
                .map_err(storage)?;
            if previous.is_some_and(|v| v != digest) {
                return Err(conflict(
                    "Mutation request_id was reused with different arguments",
                ));
            }
            db.execute("INSERT INTO calls VALUES (?,?,?,?,'pending') ON CONFLICT(binding,generation,request_id) DO NOTHING",params![binding.name,binding.generation,key,digest]).map_err(storage)?;
            Some(key)
        } else {
            None
        };
        // Dropping a cancelled future is not proof that its backend write stopped.
        let _receipt_guard = key.as_ref().map(|key| UncertainCall {
            db: &self.db,
            binding: binding.name.clone(),
            generation: binding.generation,
            key: key.clone(),
        });
        let value: Result<Value> =
            tokio::time::timeout(std::time::Duration::from_secs(35), async {
                Ok(match call {
                    Call::Put { request } => serde_json::to_value(provider.put(request).await?),
                    Call::Get { access, id } => {
                        serde_json::to_value(provider.get(&access, &id).await?)
                    }
                    Call::Delete { request } => {
                        serde_json::to_value(provider.delete(request).await?)
                    }
                    Call::Search {
                        access,
                        query,
                        limit,
                    } => serde_json::to_value(provider.search(&access, &query, limit).await?),
                    Call::List {
                        access,
                        after,
                        limit,
                    } => serde_json::to_value(provider.list(&access, after, limit).await?),
                    _ => unreachable!(),
                }
                .map_err(storage)?)
            })
            .await
            .unwrap_or_else(|_| {
                Err(ServiceError::new(
                    ErrorCode::Deadline,
                    "Memory call deadline exceeded; dispatched mutation outcome may be unknown",
                ))
            });
        if let Some(key) = key {
            let state = if value.is_ok()
                || value.as_ref().is_err_and(|e| {
                    matches!(
                        e.code,
                        ErrorCode::Invalid
                            | ErrorCode::Unauthorized
                            | ErrorCode::Conflict
                            | ErrorCode::StaleGeneration
                    )
                }) {
                "resolved"
            } else {
                "uncertain"
            };
            self.db
                .lock()
                .map_err(storage)?
                .execute(
                    "UPDATE calls SET state=CASE WHEN state='resolved' THEN state ELSE ? END WHERE binding=? AND generation=? AND request_id=?",
                    params![state, binding.name, binding.generation, key],
                )
                .map_err(storage)?;
        }
        Ok(Routed {
            binding: binding.name,
            provider_id: binding.provider_id,
            generation: binding.generation,
            value: value?,
        })
    }
    /// Destination identity constitutes an explicit operator decision. Composition owns credentials.
    pub async fn replace(
        &self,
        id: &str,
        destination: &str,
        instance: &str,
        expected_generation: u64,
    ) -> Result<Replacement> {
        let _serial = self.replacement.lock().await;
        if id.is_empty() || id.len() > 128 {
            return Err(conflict("Invalid replacement ID"));
        }
        if let Some(job) = self.job(id)? {
            if job.destination != destination || job.expected_generation != expected_generation {
                return Err(conflict("Replacement ID reused"));
            }
            return Ok(job);
        }
        if let Some(error) = self.binding_block()? {
            return Err(error);
        }
        let binding = self
            .binding()?
            .ok_or_else(|| conflict("No memory binding"))?;
        if binding.generation != expected_generation || binding.provider_id == destination {
            return Err(conflict(
                "Replacement requires the current generation and a different destination",
            ));
        }
        let mut job = Replacement {
            id: id.into(),
            binding: binding.name.clone(),
            source: binding.provider_id.clone(),
            destination: destination.into(),
            expected_generation,
            stage: ReplacementStage::Requested,
            error: None,
            generation: None,
        };
        self.begin(&job, instance, false)?;
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            self.transfer(&mut job, instance, false),
        )
        .await
        .unwrap_or_else(|_| {
            Err(ServiceError::new(
                ErrorCode::Deadline,
                "Replacement deadline exceeded; inspect the durable job and binding",
            ))
        });
        if let Err(e) = result {
            let e = self.restore_source(&job).await.err().unwrap_or(e);
            job.stage = ReplacementStage::Blocked;
            job.error = Some(e);
            self.save(&job)?;
        }
        Ok(job)
    }
    async fn transfer(&self, job: &mut Replacement, instance: &str, rollback: bool) -> Result<()> {
        let binding = self
            .binding()?
            .ok_or_else(|| conflict("No memory binding"))?;
        if binding.provider_id != job.source || binding.generation != job.expected_generation {
            return Err(conflict(
                "Binding changed; obsolete transfer cannot modify its destination",
            ));
        }
        let mut checkpoint = self
            .checkpoint(&job.id)?
            .ok_or_else(|| conflict("Replacement checkpoint is missing"))?;
        if checkpoint.instance != instance || checkpoint.rollback != rollback {
            return Err(conflict(
                "Replacement decision differs from its durable checkpoint",
            ));
        }
        let source = self.own_provider(&job.source, true, false).await?;
        let destination_raw = self.provider(&job.destination).await?;
        let destination = destination_raw.clone();
        let descriptor = destination.descriptor().await?;
        descriptor.require(&memory_capabilities())?;
        if descriptor.instance_id != instance
            || descriptor.instance_id == source.descriptor().await?.instance_id
        {
            return Err(conflict(
                "Destination identity does not match the replacement decision",
            ));
        }
        job.stage = ReplacementStage::Validated;
        self.save(job)?;
        if !rollback && !checkpoint.prepared && destination.snapshot().await?.watermark != 0 {
            return Err(conflict(
                "Destination contains data; use a dedicated empty migration destination",
            ));
        }
        let destination = self.own_provider(&job.destination, true, false).await?;
        checkpoint.prepared = true;
        self.save_checkpoint(&job.id, &checkpoint)?;
        job.stage = ReplacementStage::Preparing;
        self.save(job)?;
        let initial = source.snapshot().await?;
        job.stage = ReplacementStage::Transferring;
        self.save(job)?;
        destination.import(initial).await?;
        let _checkpoint = self.gate.write().await;
        let binding = self.binding()?.ok_or_else(|| conflict("Binding removed"))?;
        if binding.generation != job.expected_generation || binding.provider_id != job.source {
            return Err(conflict("Binding changed during preparation"));
        }
        let pending: u64 = self
            .db
            .lock()
            .map_err(storage)?
            .query_row(
                "SELECT count(*) FROM calls WHERE binding=? AND state!='resolved'",
                [&binding.name],
                |r| r.get(0),
            )
            .map_err(storage)?;
        if pending != 0 {
            return Err(ServiceError::new(ErrorCode::Uncertain,"A dispatched memory write has no confirmed outcome; reconcile the same request on its original provider"));
        }
        job.stage = ReplacementStage::Checkpointed;
        self.save(job)?;
        let mut final_state = source.snapshot().await?;
        final_state.canonicalize();
        checkpoint.snapshot = Some(final_state.clone());
        self.save_checkpoint(&job.id, &checkpoint)?;
        destination.import(final_state.clone()).await?;
        let mut imported = destination.snapshot().await?;
        imported.canonicalize();
        if imported != final_state {
            return Err(ServiceError::new(
                ErrorCode::VerificationFailed,
                "Imported memory records, receipts or tombstones differ",
            ));
        }
        let source_fence = self
            .stored_fence(&job.source)?
            .ok_or_else(|| conflict("Source writer authority missing"))?;
        self.provider(&job.source)
            .await?
            .retire_fence(&source_fence)
            .await?;
        let next = Binding {
            provider_id: job.destination.clone(),
            generation: binding
                .generation
                .checked_add(1)
                .ok_or_else(|| conflict("Generation exhausted"))?,
            ..binding
        };
        job.stage = ReplacementStage::Committed;
        job.generation = Some(next.generation);
        {
            let mut db = self.db.lock().map_err(storage)?;
            let tx = db.transaction().map_err(storage)?;
            tx.execute(
                "UPDATE bindings SET body=? WHERE name=?",
                params![json(&next)?, next.name],
            )
            .map_err(storage)?;
            tx.execute(
                "UPDATE replacements SET body=? WHERE id=?",
                params![json(job)?, job.id],
            )
            .map_err(storage)?;
            tx.execute("INSERT INTO events(body) VALUES (?)", [json(job)?])
                .map_err(storage)?;
            tx.commit().map_err(storage)?;
        }
        job.stage = ReplacementStage::Verified;
        self.save(job)?;
        job.stage = if rollback {
            ReplacementStage::RolledBack
        } else {
            ReplacementStage::Retired
        };
        self.save(job)?;
        Ok(())
    }
    /// Rollback transfers current confirmed writes and tombstones back; never restores a stale snapshot.
    pub async fn rollback(
        &self,
        id: &str,
        previous_job: &str,
        expected_generation: u64,
    ) -> Result<Replacement> {
        let _serial = self.replacement.lock().await;
        if id.is_empty() || id.len() > 128 || id == previous_job {
            return Err(conflict(
                "Rollback requires a distinct, bounded operation ID",
            ));
        }
        let previous = self
            .job(previous_job)?
            .ok_or_else(|| conflict("Unknown previous replacement"))?;
        if let Some(job) = self.job(id)? {
            if job.source != previous.destination
                || job.destination != previous.source
                || job.expected_generation != expected_generation
            {
                return Err(conflict("Rollback operation ID was reused"));
            }
            return Ok(job);
        }
        if let Some(error) = self.binding_block()? {
            return Err(error);
        }
        let binding = self
            .binding()?
            .ok_or_else(|| conflict("No memory binding"))?;
        if previous.destination != binding.provider_id
            || previous.generation != Some(binding.generation)
            || binding.generation != expected_generation
        {
            return Err(conflict(
                "Rollback requires the current replacement generation",
            ));
        }
        let descriptor = self.provider(&previous.source).await?.descriptor().await?;
        let mut job = Replacement {
            id: id.into(),
            binding: binding.name,
            source: binding.provider_id,
            destination: previous.source,
            expected_generation,
            stage: ReplacementStage::Requested,
            error: None,
            generation: None,
        };
        self.begin(&job, &descriptor.instance_id, true)?;
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            self.transfer(&mut job, &descriptor.instance_id, true),
        )
        .await
        .unwrap_or_else(|_| {
            Err(ServiceError::new(
                ErrorCode::Deadline,
                "Rollback deadline exceeded; inspect the durable job and binding",
            ))
        });
        if let Err(e) = result {
            let e = self.restore_source(&job).await.err().unwrap_or(e);
            job.stage = ReplacementStage::Blocked;
            job.error = Some(e);
            self.save(&job)?;
        }
        Ok(job)
    }
}

pub mod http;

mod recovery;

/// A dropped in-flight call remains a durable blocker, including cooperative cancellation.
struct UncertainCall<'a> {
    db: &'a Mutex<Connection>,
    binding: String,
    generation: u64,
    key: String,
}
impl Drop for UncertainCall<'_> {
    fn drop(&mut self) {
        if let Ok(db) = self.db.lock() {
            let _=db.execute("UPDATE calls SET state='uncertain' WHERE binding=? AND generation=? AND request_id=? AND state='pending'",params![self.binding,self.generation,self.key]);
        }
    }
}
#[async_trait::async_trait]
impl LogicalMemoryPort for Registry {
    fn binding(&self) -> Result<Binding> {
        Registry::binding(self)?.ok_or_else(|| {
            ServiceError::new(ErrorCode::Unavailable, "Memory binding is unavailable")
        })
    }
    async fn call(&self, generation: Option<u64>, call: MemoryCall) -> Result<Routed<Value>> {
        let access = match &call {
            MemoryCall::Put { request } => Some(&request.access),
            MemoryCall::Delete { request } => Some(&request.access),
            _ => None,
        };
        let _lane = if let Some(access) = access {
            access.validate()?;
            Some(self.tool_lane(access)?.lock_owned().await)
        } else {
            None
        };
        if let Some(access) = access {
            access.validate()?;
            let blocked:bool=self.db.lock().map_err(storage)?.query_row("SELECT EXISTS(SELECT 1 FROM calls WHERE binding='memory.default' AND state!='resolved' AND json_extract(request_id,'$[0].namespace')=? AND json_extract(request_id,'$[0].actor')=?)",params![access.namespace,access.actor],|r|r.get(0)).map_err(storage)?;
            if blocked {
                return Err(ServiceError::new(ErrorCode::Uncertain,"An earlier memory write in this private scope is uncertain. Inspect its original receipt before further writes; reads and unrelated scopes remain available."));
            }
        }
        Registry::call(self, generation, call).await
    }
}
