//! Durable logical binding and managed replacement. Consumers keep this port, never backend clients.
use crate::{
    crypto::{check, err, json, parse},
    *,
};
use fs2::FileExt;
use hmac::{Hmac, Mac};
use kageha_contracts::{execution::Control, ErrorCode, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex},
};
#[derive(Clone)]
pub struct Backend {
    pub id: String,
    pub device_id: String,
    pub store: Arc<dyn CredentialStore>,
}
fn initial_authentication_generation() -> u64 {
    1
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Binding {
    #[serde(default = "initial_authentication_generation")]
    pub authentication_generation: u64,
    pub backend: String,
    pub vault_id: String,
    pub generation: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Decision {
    pub id: String,
    pub destination: String,
    #[serde(default)]
    pub allow_browser_transfer: bool,
}
#[derive(Clone, Serialize, Deserialize)]
struct Proof {
    reference: CredentialRef,
    revision: u64,
    mac: Vec<u8>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Job {
    status: CredentialMigrationStatus,
    source: String,
    source_vault: String,
    target_vault: String,
    principal: String,
    purpose: String,
    decision: Decision,
    committed_generation: Option<u64>,
    proofs: Vec<Proof>,
    prior_owner: Option<String>,
    checkpoint_verified: bool,
}
pub struct Registry {
    #[cfg(test)]
    checkpoint: Mutex<Option<(MigrationStage, Arc<tokio::sync::Notify>)>>,
    db: Arc<Mutex<Connection>>,
    backends: BTreeMap<String, Backend>,
    admin_principal: String,
    admin_purpose: String,
    gate: Arc<tokio::sync::RwLock<()>>,
    jobs: tokio::sync::Mutex<()>,
    _writer: Option<std::fs::File>,
}
fn storage(_: impl std::fmt::Display) -> kageha_contracts::ServiceError {
    err(
        ErrorCode::Unavailable,
        "Credential registry journal unavailable",
    )
}
fn audit_mac(key: &SecretBytes, record: &CredentialTransferRecord) -> Result<Vec<u8>> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key.expose())
        .map_err(|_| err(ErrorCode::Invalid, "Invalid credential checkpoint key"))?;
    mac.update(&json(&(
        &record.metadata.reference,
        &record.metadata.kind,
        record.metadata.revision,
        record.metadata.expires_at_ms,
        &record.metadata.state,
    ))?);
    mac.update(record.secret.expose());
    Ok(mac.finalize().into_bytes().to_vec())
}
impl Registry {
    pub async fn open(
        path: &Path,
        primary: &str,
        backends: Vec<Backend>,
        admin: &CredentialAccess,
        control: &Control<'_>,
    ) -> Result<Arc<Self>> {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let db = crate::repository::private_connection(path)?;
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path.with_extension("lock"))
            .map_err(storage)?;
        let metadata = lock.metadata().map_err(storage)?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
        {
            return Err(err(
                ErrorCode::Unauthorized,
                "Credential registry lock must be private and owned by this user",
            ));
        }
        lock.try_lock_exclusive().map_err(|_| {
            err(
                ErrorCode::Conflict,
                "Credential registry already has a control-plane writer",
            )
        })?;
        Self::compose(db, Some(lock), primary, backends, admin, control).await
    }
    pub async fn in_memory(
        primary: &str,
        backends: Vec<Backend>,
        admin: &CredentialAccess,
        control: &Control<'_>,
    ) -> Result<Arc<Self>> {
        Self::compose(
            Connection::open_in_memory().map_err(storage)?,
            None,
            primary,
            backends,
            admin,
            control,
        )
        .await
    }
    async fn compose(
        db: Connection,
        writer: Option<std::fs::File>,
        primary: &str,
        backends: Vec<Backend>,
        admin: &CredentialAccess,
        control: &Control<'_>,
    ) -> Result<Arc<Self>> {
        check(control)?;
        db.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(storage)?;
        db.execute_batch("PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS credential_binding(singleton INTEGER PRIMARY KEY CHECK(singleton=1),body BLOB NOT NULL); CREATE TABLE IF NOT EXISTS credential_replacements(id TEXT PRIMARY KEY,body BLOB NOT NULL);").map_err(storage)?;
        let mut map = BTreeMap::new();
        for backend in backends {
            if backend.id.is_empty()
                || backend.id.len() > 128
                || backend.device_id.is_empty()
                || backend.id.chars().any(char::is_control)
                || map.insert(backend.id.clone(), backend).is_some()
            {
                return Err(err(
                    ErrorCode::Invalid,
                    "Invalid or duplicate credential backend",
                ));
            }
        }
        let registry = Arc::new(Self {
            #[cfg(test)]
            checkpoint: Mutex::new(None),
            db: Arc::new(Mutex::new(db)),
            backends: map,
            admin_principal: admin.principal.clone(),
            admin_purpose: admin.purpose.clone(),
            gate: Arc::new(tokio::sync::RwLock::new(())),
            jobs: tokio::sync::Mutex::new(()),
            _writer: writer,
        });
        let existing = registry.binding_optional().await?;
        if existing.is_none() {
            let endpoint = registry.backend(primary)?;
            let state = endpoint
                .store
                .backend_state(&registry.authority(primary), control)
                .await?;
            Self::capabilities(&state)?;
            let binding = Binding {
                authentication_generation: endpoint
                    .store
                    .authentication_generation(control)
                    .await?,
                backend: primary.into(),
                vault_id: state.vault_id,
                generation: 1,
            };
            registry
                .run(move |db| {
                    db.execute(
                        "INSERT INTO credential_binding VALUES(1,?)",
                        [json(&binding)?],
                    )
                    .map_err(storage)?;
                    Ok(())
                })
                .await?;
        }
        let binding = registry.binding().await?;
        let backend = registry.backend(&binding.backend)?;
        let state = backend
            .store
            .backend_state(&registry.authority(&binding.backend), control)
            .await?;
        if state.vault_id != binding.vault_id {
            return Err(err(
                ErrorCode::VerificationFailed,
                "Registered credential backend identity changed",
            ));
        }
        Ok(registry)
    }
    fn authority(&self, destination: &str) -> CredentialAccess {
        CredentialAccess {
            principal: self.admin_principal.clone(),
            purpose: self.admin_purpose.clone(),
            destination: destination.into(),
        }
    }
    fn backend(&self, id: &str) -> Result<&Backend> {
        self.backends.get(id).ok_or_else(|| {
            err(
                ErrorCode::Unavailable,
                "Credential backend is not registered",
            )
        })
    }
    fn capabilities(state: &CredentialBackendState) -> Result<()> {
        for capability in [
            "checkpoint.v1",
            "authority.fence",
            "receipts.portable",
            "audit.portable",
        ] {
            if !state.capabilities.contains(capability) {
                return Err(err(
                    ErrorCode::Unsupported,
                    "Credential replacement lacks a required capability",
                ));
            }
        }
        Ok(())
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
    async fn binding_optional(&self) -> Result<Option<Binding>> {
        self.run(|db| {
            db.query_row(
                "SELECT body FROM credential_binding WHERE singleton=1",
                [],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .optional()
            .map_err(storage)?
            .map(|b| parse(&b))
            .transpose()
        })
        .await
    }
    pub async fn binding(&self) -> Result<Binding> {
        self.binding_optional().await?.ok_or_else(|| {
            err(
                ErrorCode::Unavailable,
                "Credential binding is not initialized",
            )
        })
    }
    async fn load(&self, id: &str) -> Result<Job> {
        let id = id.to_owned();
        self.run(move |db| {
            db.query_row(
                "SELECT body FROM credential_replacements WHERE id=?",
                [id],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .optional()
            .map_err(storage)?
            .ok_or_else(|| err(ErrorCode::NotFound, "Credential replacement not found"))
            .and_then(|b| parse(&b))
        })
        .await
    }
    async fn save(&self, job: &Job) -> Result<()> {
        let job = job.clone();
        self.run(move|db|{db.execute("INSERT INTO credential_replacements VALUES(?,?) ON CONFLICT(id) DO UPDATE SET body=excluded.body",params![job.status.id,json(&job)?]).map_err(storage)?;Ok(())}).await
    }
    fn access(&self, access: &CredentialAccess, job: &Job) -> Result<()> {
        if access.principal != job.principal
            || access.purpose != job.purpose
            || access.destination != job.status.destination
        {
            return Err(err(
                ErrorCode::Unauthorized,
                "Replacement decision scope changed",
            ));
        }
        Ok(())
    }
    async fn admit(
        &self,
        control: &Control<'_>,
    ) -> Result<(
        tokio::sync::OwnedRwLockReadGuard<()>,
        Arc<dyn CredentialStore>,
    )> {
        let guard = tokio::select! {guard=self.gate.clone().read_owned()=>guard,_=async{loop{if control.check().is_err(){break;}tokio::time::sleep(std::time::Duration::from_millis(10)).await;}}=>{check(control)?;return Err(err(ErrorCode::Cancelled,"Credential admission cancelled"));}};
        let binding = self.binding().await?;
        let backend = self.backend(&binding.backend)?;
        let state = backend
            .store
            .backend_state(&self.authority(&binding.backend), control)
            .await?;
        if state.vault_id != binding.vault_id {
            return Err(err(
                ErrorCode::VerificationFailed,
                "Credential backend identity changed",
            ));
        }
        Ok((guard, backend.store.clone()))
    }
    pub async fn request(
        &self,
        access: &CredentialAccess,
        decision: Decision,
        operation_id: &str,
        control: &Control<'_>,
    ) -> Result<CredentialMigrationStatus> {
        self.request_inner(access, decision, operation_id, None, control)
            .await
    }
    async fn request_inner(
        &self,
        access: &CredentialAccess,
        decision: Decision,
        id: &str,
        prior_owner: Option<String>,
        control: &Control<'_>,
    ) -> Result<CredentialMigrationStatus> {
        check(control)?;
        let _serial =
            crate::crypto::controlled(control, async { Ok(self.jobs.lock().await) }).await?;
        if id.is_empty()
            || id.len() > 128
            || decision.id.is_empty()
            || decision.id.len() > 256
            || decision.destination != access.destination
            || access.principal != self.admin_principal
            || access.purpose != self.admin_purpose
        {
            return Err(err(
                ErrorCode::Unauthorized,
                "An explicit destination-scoped replacement decision is required",
            ));
        }
        match self.load(id).await {
            Ok(job) => {
                self.access(access, &job)?;
                if job.decision.id != decision.id
                    || job.decision.allow_browser_transfer != decision.allow_browser_transfer
                {
                    return Err(err(
                        ErrorCode::Conflict,
                        "Replacement operation ID already has a different decision",
                    ));
                }
                return Ok(job.status);
            }
            Err(e) if e.code == ErrorCode::NotFound => {}
            Err(e) => return Err(e),
        }
        let pending = self
            .run(|db| {
                let mut statement = db
                    .prepare("SELECT body FROM credential_replacements")
                    .map_err(storage)?;
                let rows = statement
                    .query_map([], |r| r.get::<_, Vec<u8>>(0))
                    .map_err(storage)?
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(storage)?;
                for row in rows {
                    let job: Job = parse(&row)?;
                    if !matches!(
                        job.status.stage,
                        MigrationStage::Verified
                            | MigrationStage::Retired
                            | MigrationStage::Cancelled
                    ) {
                        return Ok(true);
                    }
                }
                Ok(false)
            })
            .await?;
        if pending {
            return Err(err(
                ErrorCode::Conflict,
                "Finish or recover the current credential replacement first",
            ));
        }
        let binding = self.binding().await?;
        if binding.backend == decision.destination {
            return Err(err(
                ErrorCode::Invalid,
                "Credential destination is already active",
            ));
        }
        let source = self.backend(&binding.backend)?;
        let destination = self.backend(&decision.destination)?;
        let source_state = source.store.backend_state(access, control).await?;
        let target_state = destination.store.backend_state(access, control).await?;
        Self::capabilities(&source_state)?;
        Self::capabilities(&target_state)?;
        if source_state.vault_id == target_state.vault_id {
            return Err(err(
                ErrorCode::Invalid,
                "Credential backends must own independent vaults",
            ));
        }
        if target_state.records > 0
            && (prior_owner.as_deref() != target_state.admission_owner.as_deref()
                || target_state.admission != VaultAdmission::Fenced)
        {
            return Err(err(ErrorCode::Conflict,"Credential replacement requires an empty destination or the verified rollback backend"));
        }
        let job = Job {
            status: CredentialMigrationStatus {
                id: id.into(),
                stage: MigrationStage::Requested,
                source_generation: binding.generation,
                destination: decision.destination.clone(),
                blocker: None,
            },
            source: binding.backend,
            source_vault: source_state.vault_id,
            target_vault: target_state.vault_id,
            principal: access.principal.clone(),
            purpose: access.purpose.clone(),
            decision,
            committed_generation: None,
            proofs: Vec::new(),
            prior_owner,
            checkpoint_verified: false,
        };
        self.save(&job).await?;
        Ok(job.status)
    }
    async fn stage(&self, job: &mut Job, stage: MigrationStage) -> Result<()> {
        job.status.stage = stage;
        job.status.blocker = None;
        self.save(job).await?;
        #[cfg(test)]
        self.test_checkpoint(&job.status.stage).await;
        Ok(())
    }
    #[cfg(test)]
    async fn test_checkpoint(&self, stage: &MigrationStage) {
        let selected = self
            .checkpoint
            .lock()
            .unwrap()
            .as_ref()
            .filter(|(s, _)| s == stage)
            .map(|(_, n)| n.clone());
        if let Some(notify) = selected {
            notify.notify_one();
            std::future::pending::<()>().await;
        }
    }
    fn proofs(checkpoint: &CredentialCheckpoint) -> Result<Vec<Proof>> {
        checkpoint
            .records
            .iter()
            .map(|record| {
                Ok(Proof {
                    reference: record.metadata.reference.clone(),
                    revision: record.metadata.revision,
                    mac: audit_mac(&checkpoint.receipt_key, record)?,
                })
            })
            .collect()
    }
    fn verify(checkpoint: &CredentialCheckpoint, proofs: &[Proof]) -> Result<()> {
        let expected: BTreeMap<_, _> = proofs.iter().map(|p| (&p.reference, p)).collect();
        let records: BTreeMap<_, _> = checkpoint
            .records
            .iter()
            .map(|r| (&r.metadata.reference, r))
            .collect();
        for proof in proofs {
            let row = records.get(&proof.reference).ok_or_else(|| {
                err(
                    ErrorCode::VerificationFailed,
                    "Credential checkpoint lost a record or tombstone",
                )
            })?;
            if row.metadata.revision < proof.revision {
                return Err(err(
                    ErrorCode::VerificationFailed,
                    "Credential checkpoint lost confirmed revisions",
                ));
            }
            if row.metadata.revision == proof.revision
                && audit_mac(&checkpoint.receipt_key, row)? != proof.mac
            {
                return Err(err(
                    ErrorCode::VerificationFailed,
                    "Credential checkpoint value or access semantics changed",
                ));
            }
        }
        for row in &checkpoint.records {
            if expected
                .get(&row.metadata.reference)
                .is_none_or(|proof| row.metadata.revision > proof.revision)
            {
                let confirmed = checkpoint.operations.iter().any(|op| {
                    op.operation.metadata.reference == row.metadata.reference
                        && op.operation.metadata.revision == row.metadata.revision
                        && op.operation.metadata.state == row.metadata.state
                });
                if !confirmed {
                    return Err(err(
                        ErrorCode::VerificationFailed,
                        "Credential destination has an unconfirmed change",
                    ));
                }
            }
        }
        Ok(())
    }
    async fn work(
        &self,
        access: &CredentialAccess,
        job: &mut Job,
        control: &Control<'_>,
    ) -> Result<()> {
        self.access(access, job)?;
        let source = self.backend(&job.source)?;
        let target = self.backend(&job.status.destination)?;
        let source_state = source.store.backend_state(access, control).await?;
        let target_state = target.store.backend_state(access, control).await?;
        if source_state.vault_id != job.source_vault || target_state.vault_id != job.target_vault {
            return Err(err(
                ErrorCode::VerificationFailed,
                "Credential replacement backend identity changed",
            ));
        }
        Self::capabilities(&source_state)?;
        Self::capabilities(&target_state)?;
        let binding = self.binding().await?;
        if binding.backend == job.status.destination {
            if binding.generation != job.status.source_generation + 1
                || binding.vault_id != job.target_vault
            {
                return Err(err(
                    ErrorCode::StaleGeneration,
                    "Credential binding advanced beyond this replacement",
                ));
            }
            let _gate = crate::crypto::controlled(control, async {
                Ok(self.gate.clone().write_owned().await)
            })
            .await?;
            target
                .store
                .activate(access, target_state.generation, &job.status.id, control)
                .await?;
            let checkpoint = target.store.export_checkpoint(access, control).await?;
            Self::verify(&checkpoint, &job.proofs)?;
            job.committed_generation = Some(binding.generation);
            self.stage(job, MigrationStage::Verified).await?;
            return Ok(());
        }
        if binding.backend != job.source || binding.generation != job.status.source_generation {
            return Err(err(
                ErrorCode::StaleGeneration,
                "Credential source binding changed",
            ));
        }
        self.stage(job, MigrationStage::Validated).await?;
        let checkpoint = source.store.export_checkpoint(access, control).await?;
        if source.device_id != target.device_id
            && !job.decision.allow_browser_transfer
            && checkpoint
                .records
                .iter()
                .any(|r| r.metadata.kind == "browser_password")
        {
            return Err(err(ErrorCode::Unauthorized,"Browser credentials remain device-local unless this destination is explicitly approved"));
        }
        self.stage(job, MigrationStage::Preparing).await?;
        if target_state.admission == VaultAdmission::Fenced
            && target_state.admission_owner.as_deref() != Some(&job.status.id)
            && target_state.admission_owner != job.prior_owner
        {
            return Err(err(
                ErrorCode::Conflict,
                "Credential destination is owned by another replacement",
            ));
        }
        let target_state = target
            .store
            .freeze(access, target_state.generation, &job.status.id, control)
            .await?;
        self.stage(job, MigrationStage::Transferring).await?;
        // Initial copy happens while ordinary source operations remain available.
        target
            .store
            .import_checkpoint(
                access,
                target_state.generation,
                &job.status.id,
                checkpoint,
                control,
            )
            .await?;
        let _gate = self.gate.clone().write_owned().await;
        let source_state = source.store.backend_state(access, control).await?;
        source
            .store
            .freeze(access, source_state.generation, &job.status.id, control)
            .await?;
        // Freeze atomically rejects outstanding refresh receipts and fences all old writers.
        let checkpoint = source.store.export_checkpoint(access, control).await?;
        job.proofs = Self::proofs(&checkpoint)?;
        self.save(job).await?;
        let target_state = target.store.backend_state(access, control).await?;
        target
            .store
            .import_checkpoint(
                access,
                target_state.generation,
                &job.status.id,
                checkpoint,
                control,
            )
            .await?;
        let imported = target.store.export_checkpoint(access, control).await?;
        Self::verify(&imported, &job.proofs)?;
        job.checkpoint_verified = true;
        self.stage(job, MigrationStage::Checkpointed).await?;
        let mut committed = job.clone();
        committed.status.stage = MigrationStage::Committed;
        committed.committed_generation = Some(binding.generation + 1);
        let next = Binding {
            authentication_generation: source
                .store
                .authentication_generation(control)
                .await?
                .max(binding.authentication_generation),
            backend: job.status.destination.clone(),
            vault_id: job.target_vault.clone(),
            generation: binding.generation + 1,
        };
        let source_id = job.source.clone();
        let expected = binding.generation;
        self.run(move |db| {
            let tx = db
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(storage)?;
            let bytes: Vec<u8> = tx
                .query_row(
                    "SELECT body FROM credential_binding WHERE singleton=1",
                    [],
                    |r| r.get(0),
                )
                .map_err(storage)?;
            let current: Binding = parse(&bytes)?;
            if current.backend != source_id || current.generation != expected {
                return Err(err(
                    ErrorCode::StaleGeneration,
                    "Credential binding changed before commit",
                ));
            }
            tx.execute(
                "UPDATE credential_binding SET body=? WHERE singleton=1",
                [json(&next)?],
            )
            .map_err(storage)?;
            tx.execute(
                "UPDATE credential_replacements SET body=? WHERE id=?",
                params![json(&committed)?, committed.status.id],
            )
            .map_err(storage)?;
            tx.commit().map_err(storage)?;
            Ok(())
        })
        .await?;
        job.status.stage = MigrationStage::Committed;
        job.committed_generation = Some(binding.generation + 1);
        #[cfg(test)]
        self.test_checkpoint(&MigrationStage::Committed).await;
        let state = target.store.backend_state(access, control).await?;
        target
            .store
            .activate(access, state.generation, &job.status.id, control)
            .await?;
        let checkpoint = target.store.export_checkpoint(access, control).await?;
        Self::verify(&checkpoint, &job.proofs)?;
        self.stage(job, MigrationStage::Verified).await?;
        Ok(())
    }
    pub async fn resume(
        &self,
        access: &CredentialAccess,
        id: &str,
        control: &Control<'_>,
    ) -> Result<CredentialMigrationStatus> {
        let _serial = self.jobs.lock().await;
        let mut job = self.load(id).await?;
        self.access(access, &job)?;
        if matches!(
            job.status.stage,
            MigrationStage::Verified | MigrationStage::Retired | MigrationStage::Cancelled
        ) {
            return Ok(job.status);
        }
        if let Err(error) = self.work(access, &mut job, control).await {
            job.status.stage = MigrationStage::Blocked;
            job.status.blocker = Some(error.message);
            self.save(&job).await?;
        }
        Ok(job.status)
    }
    pub async fn rollback(
        &self,
        access: &CredentialAccess,
        verified_job: &str,
        decision: Decision,
        id: &str,
        control: &Control<'_>,
    ) -> Result<CredentialMigrationStatus> {
        let previous = self.load(verified_job).await?;
        if previous.status.stage != MigrationStage::Verified
            || decision.destination != previous.source
        {
            return Err(err(
                ErrorCode::Invalid,
                "Rollback requires the retained verified previous binding",
            ));
        }
        let binding = self.binding().await?;
        if binding.backend != previous.status.destination
            || Some(binding.generation) != previous.committed_generation
        {
            return Err(err(
                ErrorCode::StaleGeneration,
                "Another replacement changed this binding",
            ));
        }
        self.request_inner(access, decision, id, Some(previous.status.id), control)
            .await?;
        self.resume(access, id, control).await
    }
    /// Cancellation before cutover resumes only the original fenced source, never a stale replica.
    pub async fn cancel(
        &self,
        access: &CredentialAccess,
        id: &str,
        control: &Control<'_>,
    ) -> Result<CredentialMigrationStatus> {
        let _serial =
            crate::crypto::controlled(control, async { Ok(self.jobs.lock().await) }).await?;
        let mut job = self.load(id).await?;
        self.access(access, &job)?;
        if job.status.stage == MigrationStage::Cancelled {
            return Ok(job.status);
        }
        let _gate =
            crate::crypto::controlled(control, async { Ok(self.gate.clone().write_owned().await) })
                .await?;
        let binding = self.binding().await?;
        if binding.backend != job.source || binding.generation != job.status.source_generation {
            return Err(err(
                ErrorCode::Conflict,
                "A committed credential replacement requires rollback",
            ));
        }
        let source = self.backend(&job.source)?;
        let state = source.store.backend_state(access, control).await?;
        if state.admission == VaultAdmission::Fenced {
            if state.admission_owner.as_deref() != Some(id) {
                return Err(err(
                    ErrorCode::Conflict,
                    "Credential source is fenced by another operation",
                ));
            }
            source
                .store
                .activate(access, state.generation, id, control)
                .await?;
        }
        self.stage(&mut job, MigrationStage::Cancelled).await?;
        Ok(job.status)
    }
    pub async fn retire(
        &self,
        access: &CredentialAccess,
        id: &str,
        control: &Control<'_>,
    ) -> Result<CredentialMigrationStatus> {
        check(control)?;
        let _serial = self.jobs.lock().await;
        let mut job = self.load(id).await?;
        self.access(access, &job)?;
        if job.status.stage != MigrationStage::Verified {
            return Err(err(
                ErrorCode::Conflict,
                "Only verified credential replacements can retire",
            ));
        }
        self.backend(&job.status.destination)?
            .store
            .backend_state(access, control)
            .await?;
        self.stage(&mut job, MigrationStage::Retired).await?;
        Ok(job.status)
    }
}

#[async_trait::async_trait]
impl CredentialResolver for Registry {
    async fn resolve(
        &self,
        a: &CredentialAccess,
        r: &CredentialRef,
        c: &Control<'_>,
    ) -> Result<SecretLease> {
        let (_guard, s) = self.admit(c).await?;
        s.resolve(a, r, c).await
    }
    async fn validate_lease(
        &self,
        a: &CredentialAccess,
        l: &SecretLease,
        c: &Control<'_>,
    ) -> Result<()> {
        let (_guard, s) = self.admit(c).await?;
        s.validate_lease(a, l, c).await
    }
}
#[async_trait::async_trait]
impl CredentialAdmin for Registry {
    async fn metadata(
        &self,
        a: &CredentialAccess,
        r: &CredentialRef,
        c: &Control<'_>,
    ) -> Result<CredentialMetadata> {
        let (_guard, s) = self.admit(c).await?;
        s.metadata(a, r, c).await
    }
    async fn put(
        &self,
        a: &CredentialAccess,
        w: CredentialWrite,
        c: &Control<'_>,
    ) -> Result<CredentialOperation> {
        let (_guard, s) = self.admit(c).await?;
        s.put(a, w, c).await
    }
    async fn revoke(
        &self,
        a: &CredentialAccess,
        r: &CredentialRef,
        v: u64,
        id: &str,
        delete: bool,
        c: &Control<'_>,
    ) -> Result<CredentialOperation> {
        let (_guard, s) = self.admit(c).await?;
        s.revoke(a, r, v, id, delete, c).await
    }
    async fn operation(
        &self,
        a: &CredentialAccess,
        r: &CredentialRef,
        id: &str,
        c: &Control<'_>,
    ) -> Result<Option<CredentialOperation>> {
        let (_guard, s) = self.admit(c).await?;
        s.operation(a, r, id, c).await
    }
}
#[async_trait::async_trait]
impl CredentialRefresh for Registry {
    async fn begin_refresh(
        &self,
        a: &CredentialAccess,
        r: &CredentialRef,
        v: u64,
        id: &str,
        c: &Control<'_>,
    ) -> Result<RefreshTicket> {
        let (_guard, s) = self.admit(c).await?;
        s.begin_refresh(a, r, v, id, c).await
    }
    async fn dispatch_refresh(
        &self,
        a: &CredentialAccess,
        t: &RefreshTicket,
        id: &str,
        c: &Control<'_>,
    ) -> Result<(RefreshTicket, SecretBytes)> {
        let (_guard, s) = self.admit(c).await?;
        s.dispatch_refresh(a, t, id, c).await
    }
    async fn complete_refresh(
        &self,
        a: &CredentialAccess,
        t: &RefreshTicket,
        id: &str,
        g: SecretBytes,
        expiry: Option<u64>,
        c: &Control<'_>,
    ) -> Result<CredentialOperation> {
        let (_guard, s) = self.admit(c).await?;
        s.complete_refresh(a, t, id, g, expiry, c).await
    }
    async fn recover_refresh(
        &self,
        a: &CredentialAccess,
        r: &CredentialRef,
        v: u64,
        id: &str,
        c: &Control<'_>,
    ) -> Result<CredentialOperation> {
        let (_guard, s) = self.admit(c).await?;
        s.recover_refresh(a, r, v, id, c).await
    }
}
#[async_trait::async_trait]
impl CredentialControl for Registry {
    async fn revoke_workload(
        &self,
        a: &CredentialAccess,
        p: &str,
        v: u64,
        id: &str,
        c: &Control<'_>,
    ) -> Result<CredentialOperation> {
        let (_g, s) = self.admit(c).await?;
        s.revoke_workload(a, p, v, id, c).await
    }
    async fn binding_generation(&self, c: &Control<'_>) -> Result<u64> {
        let (_guard, s) = self.admit(c).await?;
        s.binding_generation(c).await
    }
    async fn authentication_generation(&self, c: &Control<'_>) -> Result<u64> {
        check(c)?;
        let binding = self.binding().await?;
        let store = &self.backend(&binding.backend)?.store;
        Ok(store
            .authentication_generation(c)
            .await?
            .max(binding.authentication_generation))
    }
    async fn fence(
        &self,
        a: &CredentialAccess,
        r: &CredentialRef,
        v: u64,
        id: &str,
        c: &Control<'_>,
    ) -> Result<CredentialOperation> {
        let (_guard, s) = self.admit(c).await?;
        s.fence(a, r, v, id, c).await
    }
    async fn fail_refresh(
        &self,
        a: &CredentialAccess,
        t: &RefreshTicket,
        id: &str,
        c: &Control<'_>,
    ) -> Result<CredentialOperation> {
        let (_guard, s) = self.admit(c).await?;
        s.fail_refresh(a, t, id, c).await
    }
    async fn export_record(
        &self,
        a: &CredentialAccess,
        r: &CredentialRef,
        c: &Control<'_>,
    ) -> Result<(CredentialMetadata, SecretBytes)> {
        let (_guard, s) = self.admit(c).await?;
        s.export_record(a, r, c).await
    }
    async fn audits(
        &self,
        a: &CredentialAccess,
        n: &str,
        after: u64,
        c: &Control<'_>,
    ) -> Result<Vec<CredentialAudit>> {
        let (_guard, s) = self.admit(c).await?;
        s.audits(a, n, after, c).await
    }
}
// Nested registries must use their own lifecycle API. Export is allowed, but an outer
// coordinator cannot silently bypass the inner registry's durable binding journal.
#[async_trait::async_trait]
impl CredentialTransfer for Registry {
    async fn backend_state(
        &self,
        a: &CredentialAccess,
        c: &Control<'_>,
    ) -> Result<CredentialBackendState> {
        let (_guard, s) = self.admit(c).await?;
        let mut state = s.backend_state(a, c).await?;
        state.capabilities.remove("authority.fence");
        Ok(state)
    }
    async fn export_checkpoint(
        &self,
        a: &CredentialAccess,
        c: &Control<'_>,
    ) -> Result<CredentialCheckpoint> {
        let (_guard, s) = self.admit(c).await?;
        s.export_checkpoint(a, c).await
    }
    async fn freeze(
        &self,
        _: &CredentialAccess,
        _: u64,
        _: &str,
        _: &Control<'_>,
    ) -> Result<CredentialBackendState> {
        Err(err(
            ErrorCode::Unsupported,
            "Use the credential registry replacement API",
        ))
    }
    async fn import_checkpoint(
        &self,
        _: &CredentialAccess,
        _: u64,
        _: &str,
        _: CredentialCheckpoint,
        _: &Control<'_>,
    ) -> Result<CredentialBackendState> {
        Err(err(
            ErrorCode::Unsupported,
            "Use the credential registry replacement API",
        ))
    }
    async fn activate(
        &self,
        _: &CredentialAccess,
        _: u64,
        _: &str,
        _: &Control<'_>,
    ) -> Result<CredentialBackendState> {
        Err(err(
            ErrorCode::Unsupported,
            "Use the credential registry replacement API",
        ))
    }
}
#[async_trait::async_trait]
impl CredentialMigration for Registry {
    async fn prepare(
        &self,
        a: &CredentialAccess,
        destination: &str,
        id: &str,
        c: &Control<'_>,
    ) -> Result<CredentialMigrationStatus> {
        self.request(
            a,
            Decision {
                id: id.into(),
                destination: destination.into(),
                allow_browser_transfer: false,
            },
            id,
            c,
        )
        .await
    }
    async fn status(
        &self,
        a: &CredentialAccess,
        id: &str,
        c: &Control<'_>,
    ) -> Result<CredentialMigrationStatus> {
        check(c)?;
        let job = self.load(id).await?;
        self.access(a, &job)?;
        Ok(job.status)
    }
}

#[cfg(test)]
mod crash_tests {
    use super::*;
    use crate::{keys::SuppliedKey, repository::SqliteRepository};
    use std::{collections::BTreeSet, sync::atomic::AtomicBool};
    fn access(d: &str) -> CredentialAccess {
        CredentialAccess {
            principal: "operator".into(),
            purpose: "replacement".into(),
            destination: d.into(),
        }
    }
    async fn vault(c: &Control<'_>) -> Arc<Vault> {
        Arc::new(
            Vault::open(
                Arc::new(SqliteRepository::in_memory().unwrap()),
                Arc::new(SuppliedKey::new("key", SecretBytes::new(vec![7; 32]))),
                "key",
                ["system", "records"]
                    .map(|n| Grant {
                        principal: "operator".into(),
                        namespace: n.into(),
                        ids: BTreeSet::from(["*".into()]),
                        purposes: BTreeSet::from(["replacement".into()]),
                        destinations: BTreeSet::from(["a".into(), "b".into()]),
                        permissions: [
                            Permission::Metadata,
                            Permission::Resolve,
                            Permission::Write,
                            Permission::Revoke,
                            Permission::Admin,
                            Permission::Export,
                        ]
                        .into_iter()
                        .collect(),
                    })
                    .to_vec(),
                c,
            )
            .await
            .unwrap(),
        )
    }
    #[tokio::test]
    async fn restart_during_every_durable_stage_never_replays_or_loses_state() {
        use std::os::unix::fs::PermissionsExt;
        for stage in [
            MigrationStage::Requested,
            MigrationStage::Validated,
            MigrationStage::Preparing,
            MigrationStage::Transferring,
            MigrationStage::Checkpointed,
            MigrationStage::Committed,
            MigrationStage::Verified,
        ] {
            let flag = AtomicBool::new(false);
            let c = Control {
                cancelled: &flag,
                deadline: None,
            };
            let a = vault(&c).await;
            let b = vault(&c).await;
            let backends = || {
                vec![
                    Backend {
                        id: "a".into(),
                        device_id: "device".into(),
                        store: a.clone(),
                    },
                    Backend {
                        id: "b".into(),
                        device_id: "device".into(),
                        store: b.clone(),
                    },
                ]
            };
            let dir = tempfile::tempdir().unwrap();
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
            let path = dir.path().join("registry.sqlite");
            let registry = Registry::open(&path, "a", backends(), &access("a"), &c)
                .await
                .unwrap();
            let reference = CredentialRef {
                namespace: "records".into(),
                id: "stable".into(),
            };
            registry
                .put(
                    &access("a"),
                    CredentialWrite {
                        operation_id: "confirmed-write".into(),
                        reference: reference.clone(),
                        expected_revision: 0,
                        kind: "key".into(),
                        expires_at_ms: None,
                        secret: SecretBytes::new(b"crash-canary".to_vec()),
                    },
                    &c,
                )
                .await
                .unwrap();
            registry
                .request(
                    &access("b"),
                    Decision {
                        id: "explicit-decision".into(),
                        destination: "b".into(),
                        allow_browser_transfer: false,
                    },
                    "switch",
                    &c,
                )
                .await
                .unwrap();
            if stage != MigrationStage::Requested {
                let notify = Arc::new(tokio::sync::Notify::new());
                *registry.checkpoint.lock().unwrap() = Some((stage.clone(), notify.clone()));
                let worker = registry.clone();
                let task = tokio::spawn(async move {
                    let f = AtomicBool::new(false);
                    worker
                        .resume(
                            &access("b"),
                            "switch",
                            &Control {
                                cancelled: &f,
                                deadline: None,
                            },
                        )
                        .await
                });
                tokio::time::timeout(std::time::Duration::from_secs(5), notify.notified())
                    .await
                    .unwrap();
                task.abort();
                let _ = task.await;
            }
            drop(registry);
            let recovered = Registry::open(&path, "a", backends(), &access("a"), &c)
                .await
                .unwrap();
            let status = recovered.resume(&access("b"), "switch", &c).await.unwrap();
            assert_eq!(
                status.stage,
                MigrationStage::Verified,
                "stage {stage:?}: {:?}",
                status.blocker
            );
            assert_eq!(
                recovered
                    .resolve(&access("a"), &reference, &c)
                    .await
                    .unwrap()
                    .secret
                    .expose(),
                b"crash-canary"
            );
            assert_eq!(
                recovered
                    .operation(&access("a"), &reference, "confirmed-write", &c)
                    .await
                    .unwrap()
                    .unwrap()
                    .metadata
                    .revision,
                1
            );
            assert!(a.resolve(&access("a"), &reference, &c).await.is_err());
        }
    }
}
