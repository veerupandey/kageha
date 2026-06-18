//! Portable replacement checkpoints and recovery. Never replay application mutations.
use super::*;
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct Checkpoint {
    pub instance: String,
    pub rollback: bool,
    pub prepared: bool,
    pub snapshot: Option<MemorySnapshot>,
}
impl Registry {
    pub(super) fn checkpoint(&self, id: &str) -> Result<Option<Checkpoint>> {
        let body: Option<String> = self
            .db
            .lock()
            .map_err(storage)?
            .query_row(
                "SELECT body FROM replacement_checkpoints WHERE id=?",
                [id],
                |r| r.get(0),
            )
            .optional()
            .map_err(storage)?;
        body.map(|s| serde_json::from_str(&s).map_err(storage))
            .transpose()
    }
    pub(super) fn save_checkpoint(&self, id: &str, checkpoint: &Checkpoint) -> Result<()> {
        self.db.lock().map_err(storage)?.execute("INSERT INTO replacement_checkpoints VALUES (?,?) ON CONFLICT(id) DO UPDATE SET body=excluded.body",params![id,json(checkpoint)?]).map_err(storage)?;
        Ok(())
    }
    pub(super) fn begin(&self, job: &Replacement, instance: &str, rollback: bool) -> Result<()> {
        let checkpoint = Checkpoint {
            instance: instance.into(),
            rollback,
            prepared: false,
            snapshot: None,
        };
        let mut db = self.db.lock().map_err(storage)?;
        let tx = db.transaction().map_err(storage)?;
        tx.execute(
            "INSERT INTO replacement_checkpoints VALUES (?,?)",
            params![job.id, json(&checkpoint)?],
        )
        .map_err(storage)?;
        tx.execute(
            "INSERT INTO replacements VALUES (?,?)",
            params![job.id, json(job)?],
        )
        .map_err(storage)?;
        tx.execute("INSERT INTO events(body) VALUES (?)", [json(job)?])
            .map_err(storage)?;
        tx.commit().map_err(storage)?;
        Ok(())
    }
    fn block_binding(&self, error: Option<&ServiceError>) -> Result<()> {
        let db = self.db.lock().map_err(storage)?;
        if let Some(error) = error {
            db.execute("INSERT INTO binding_blocks VALUES ('memory.default',?) ON CONFLICT(name) DO UPDATE SET body=excluded.body",[json(error)?]).map_err(storage)?;
        } else {
            db.execute("DELETE FROM binding_blocks WHERE name='memory.default'", [])
                .map_err(storage)?;
        }
        Ok(())
    }
    pub(super) fn binding_block(&self) -> Result<Option<ServiceError>> {
        let body: Option<String> = self
            .db
            .lock()
            .map_err(storage)?
            .query_row(
                "SELECT body FROM binding_blocks WHERE name='memory.default'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(storage)?;
        body.map(|s| serde_json::from_str(&s).map_err(storage))
            .transpose()
    }
    pub(super) async fn restore_source(&self, job: &Replacement) -> Result<()> {
        if self
            .binding()?
            .is_some_and(|b| b.provider_id == job.source && b.generation == job.expected_generation)
        {
            self.own_provider(&job.source, true, false).await?;
        }
        Ok(())
    }
    /// Resume durable data checkpoints and authority. Unknown application receipts stay blocked.
    pub async fn resume(&self, id: &str) -> Result<Replacement> {
        let _serial = self.replacement.lock().await;
        let mut job = self
            .job(id)?
            .ok_or_else(|| conflict("Unknown replacement"))?;
        if matches!(
            job.stage,
            ReplacementStage::Retired | ReplacementStage::RolledBack
        ) {
            return Ok(job);
        }
        let checkpoint = self.checkpoint(id)?.ok_or_else(|| {
            ServiceError::new(
                ErrorCode::Unsupported,
                "Legacy replacement has no portable checkpoint; inspect it manually",
            )
        })?;
        job.error = None;
        let result = tokio::time::timeout(std::time::Duration::from_secs(120), async {
            if job.generation.is_some() {
                self.finish_committed(&mut job, &checkpoint).await
            } else {
                self.transfer(&mut job, &checkpoint.instance, checkpoint.rollback)
                    .await
            }
        })
        .await
        .unwrap_or_else(|_| {
            Err(ServiceError::new(
                ErrorCode::Deadline,
                "Replacement recovery deadline exceeded",
            ))
        });
        if let Err(error) = result {
            if job.generation.is_none() {
                if let Err(restoration) = self.restore_source(&job).await {
                    job.error = Some(restoration);
                }
            } else if self.binding()?.is_some_and(|b| {
                Some(b.generation) == job.generation && b.provider_id == job.destination
            }) {
                self.block_binding(Some(&error))?;
            }
            job.stage = ReplacementStage::Blocked;
            if job.error.is_none() {
                job.error = Some(error);
            }
            self.save(&job)?;
        }
        Ok(job)
    }
    async fn finish_committed(&self, job: &mut Replacement, checkpoint: &Checkpoint) -> Result<()> {
        let _guard = self.gate.write().await;
        let binding = self.binding()?.ok_or_else(|| conflict("Binding missing"))?;
        if Some(binding.generation) != job.generation || binding.provider_id != job.destination {
            return Err(conflict(
                "Committed replacement is no longer the active binding",
            ));
        }
        let port = self.own_provider(&job.destination, true, false).await?;
        if port.descriptor().await?.instance_id != checkpoint.instance {
            return Err(conflict("Committed destination identity changed"));
        }
        let expected = checkpoint
            .snapshot
            .as_ref()
            .ok_or_else(|| conflict("Committed replacement has no verified state checkpoint"))?;
        verify_successor(expected, &port.snapshot().await?)?;
        self.block_binding(None)?;
        job.stage = ReplacementStage::Verified;
        self.save(job)?;
        job.stage = if checkpoint.rollback {
            ReplacementStage::RolledBack
        } else {
            ReplacementStage::Retired
        };
        self.save(job)
    }
    pub async fn recover_pending(&self) -> Result<Vec<Replacement>> {
        let jobs = {
            let db = self.db.lock().map_err(storage)?;
            let mut q = db
                .prepare("SELECT body FROM replacements ORDER BY rowid")
                .map_err(storage)?;
            let rows = q
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(storage)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(storage)?;
            rows
        };
        let mut results = Vec::new();
        for body in jobs {
            let mut job: Replacement = serde_json::from_str(&body).map_err(storage)?;
            if matches!(
                job.stage,
                ReplacementStage::Requested
                    | ReplacementStage::Validated
                    | ReplacementStage::Preparing
                    | ReplacementStage::Transferring
                    | ReplacementStage::Checkpointed
                    | ReplacementStage::Committed
                    | ReplacementStage::Verified
            ) {
                if self.checkpoint(&job.id)?.is_none() {
                    job.stage = ReplacementStage::Blocked;
                    job.error = Some(ServiceError::new(
                        ErrorCode::Unsupported,
                        "Legacy replacement has no portable checkpoint",
                    ));
                    self.save(&job)?;
                    results.push(job);
                } else {
                    results.push(self.resume(&job.id).await?);
                }
            }
        }
        Ok(results)
    }
}
fn verify_successor(expected: &MemorySnapshot, current: &MemorySnapshot) -> Result<()> {
    let invalid = || {
        ServiceError::new(
            ErrorCode::VerificationFailed,
            "Committed memory no longer preserves its verified checkpoint",
        )
    };
    if current.schema_version != expected.schema_version || current.watermark < expected.watermark {
        return Err(invalid());
    }
    let records: BTreeMap<_, _> = current
        .records
        .iter()
        .map(|r| ((&r.namespace, &r.id), r))
        .collect();
    if records.len() != current.records.len() {
        return Err(invalid());
    }
    for old in &expected.records {
        let record = records
            .get(&(&old.namespace, &old.id))
            .ok_or_else(invalid)?;
        if record.revision < old.revision
            || record.owner != old.owner
            || record.provenance != old.provenance
            || record.revision == old.revision && *record != old
        {
            return Err(invalid());
        }
    }
    let receipts: BTreeMap<_, _> = current
        .receipts
        .iter()
        .map(|r| ((&r.namespace, &r.actor, &r.request_id), r))
        .collect();
    for old in &expected.receipts {
        let receipt = receipts
            .get(&(&old.namespace, &old.actor, &old.request_id))
            .ok_or_else(invalid)?;
        if receipt.digest != old.digest
            || receipt.record.id != old.record.id
            || receipt.record.revision != old.record.revision
        {
            return Err(invalid());
        }
    }
    Ok(())
}
