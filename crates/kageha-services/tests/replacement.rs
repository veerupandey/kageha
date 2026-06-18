use kageha_contracts::*;
use kageha_memory::{http::Call, sqlite::SqliteMemory};
use kageha_services::Registry;
use std::{
    collections::BTreeSet,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
};
use tokio::sync::{Notify, Semaphore};
fn access(actor: &str) -> MemoryAccess {
    MemoryAccess {
        namespace: "workspace-a".into(),
        actor: actor.into(),
    }
}
fn put(id: &str, body: &str) -> PutMemory {
    PutMemory {
        request_id: format!("{id}-{body}"),
        access: access("agent-a"),
        id: id.into(),
        kind: "note".into(),
        body: body.into(),
        metadata: Default::default(),
        acl: Default::default(),
        expected_revision: None,
    }
}
fn database(root: &tempfile::TempDir, name: &str) -> Arc<dyn MemoryPort> {
    Arc::new(SqliteMemory::open(&root.path().join(name)).unwrap())
}
async fn setup(
    root: &tempfile::TempDir,
    a: Arc<dyn MemoryPort>,
    b: Arc<dyn MemoryPort>,
) -> Arc<Registry> {
    let r = Arc::new(Registry::open(&root.path().join("registry.sqlite")).unwrap());
    r.register("a".into(), a).await.unwrap();
    r.register("b".into(), b).await.unwrap();
    r.initialize("a").await.unwrap();
    r
}
async fn write(r: &Registry, request: PutMemory) {
    r.call(None, Call::Put { request }).await.unwrap();
}

#[tokio::test]
async fn replacement_rollback_restart_and_generation_fencing() {
    let root = tempfile::tempdir().unwrap();
    let a = database(&root, "a.sqlite");
    let b = database(&root, "b.sqlite");
    let instance = b.descriptor().await.unwrap().instance_id;
    let r = setup(&root, a.clone(), b.clone()).await;
    write(&r, put("one", "original")).await;
    write(&r, put("deleted", "private")).await;
    r.call(
        None,
        Call::Delete {
            request: DeleteMemory {
                request_id: "delete".into(),
                access: access("agent-a"),
                id: "deleted".into(),
                expected_revision: None,
            },
        },
    )
    .await
    .unwrap();
    let switched = r.replace("switch", "b", &instance, 1).await.unwrap();
    assert_eq!(switched.stage, ReplacementStage::Retired);
    assert_eq!(a.snapshot().await.unwrap(), b.snapshot().await.unwrap());
    assert!(matches!(
        r.call(
            Some(1),
            Call::Get {
                access: access("agent-a"),
                id: "one".into()
            }
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::StaleGeneration
    ));
    write(&r, put("one", "post cutover")).await;
    let rolled = r.rollback("rollback", "switch", 2).await.unwrap();
    assert_eq!(rolled.stage, ReplacementStage::RolledBack);
    assert_eq!(a.snapshot().await.unwrap(), b.snapshot().await.unwrap());
    assert!(a
        .get(&access("agent-a"), "deleted")
        .await
        .unwrap()
        .is_none());
    drop(r);
    let r = Registry::open(&root.path().join("registry.sqlite")).unwrap();
    r.register("a".into(), a).await.unwrap();
    r.register("b".into(), b).await.unwrap();
    assert_eq!(r.binding().unwrap().unwrap().generation, 3);
    assert_eq!(
        r.events(0).unwrap().last().unwrap().1.stage,
        ReplacementStage::RolledBack
    );
}
#[derive(Clone)]
struct Controlled {
    inner: Arc<dyn MemoryPort>,
    pause_import: Arc<AtomicBool>,
    pause_snapshot: Arc<AtomicBool>,
    pause_descriptor: Arc<AtomicBool>,
    pause_retire: Arc<AtomicBool>,
    late_claim_error: Arc<AtomicBool>,
    imports: Arc<AtomicUsize>,
    pause_import_at: Arc<AtomicUsize>,
    pause_put: Arc<AtomicBool>,
    late_error: Arc<AtomicBool>,
    reverse_snapshot: Arc<AtomicBool>,
    corrupt_snapshot: Arc<AtomicBool>,
    fail_import: Arc<AtomicBool>,
    entered: Arc<Notify>,
    release: Arc<Semaphore>,
}
impl Controlled {
    fn new(inner: Arc<dyn MemoryPort>, import: bool, put: bool) -> Self {
        Self {
            inner,
            pause_import: Arc::new(AtomicBool::new(import)),
            pause_snapshot: Arc::new(AtomicBool::new(false)),
            pause_descriptor: Arc::new(AtomicBool::new(false)),
            pause_retire: Arc::new(AtomicBool::new(false)),
            late_claim_error: Arc::new(AtomicBool::new(false)),
            imports: Arc::new(AtomicUsize::new(0)),
            pause_import_at: Arc::new(AtomicUsize::new(0)),
            pause_put: Arc::new(AtomicBool::new(put)),
            late_error: Arc::new(AtomicBool::new(false)),
            reverse_snapshot: Arc::new(AtomicBool::new(false)),
            corrupt_snapshot: Arc::new(AtomicBool::new(false)),
            fail_import: Arc::new(AtomicBool::new(false)),
            entered: Arc::new(Notify::new()),
            release: Arc::new(Semaphore::new(0)),
        }
    }
}
#[async_trait::async_trait]
impl MemoryPort for Controlled {
    async fn fence_status(&self) -> Result<Option<MemoryFence>> {
        self.inner.fence_status().await
    }
    async fn claim_fence(
        &self,
        owner: &str,
        expected_epoch: u64,
        request_id: &str,
    ) -> Result<MemoryFence> {
        {
            let fence = self
                .inner
                .claim_fence(owner, expected_epoch, request_id)
                .await?;
            if self.late_claim_error.swap(false, Ordering::SeqCst) {
                return Err(ServiceError::new(
                    ErrorCode::Unavailable,
                    "Writer claim response was lost",
                ));
            }
            Ok(fence)
        }
    }
    async fn retire_fence(&self, fence: &MemoryFence) -> Result<()> {
        {
            self.inner.retire_fence(fence).await?;
            if self.pause_retire.swap(false, Ordering::SeqCst) {
                self.entered.notify_one();
                let _permit = self.release.acquire().await.unwrap();
            }
            Ok(())
        }
    }
    fn with_fence(&self, fence: MemoryFence) -> Result<Arc<dyn MemoryPort>> {
        let mut port = self.clone();
        port.inner = self.inner.with_fence(fence)?;
        Ok(Arc::new(port))
    }
    async fn descriptor(&self) -> Result<Descriptor> {
        if self.pause_descriptor.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            let _permit = self.release.acquire().await.unwrap();
        }
        self.inner.descriptor().await
    }
    async fn put(&self, r: PutMemory) -> Result<MemoryRecord> {
        let value = self.inner.put(r).await?;
        if self.pause_put.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            let _permit = self.release.acquire().await.unwrap();
            if self.late_error.load(Ordering::SeqCst) {
                return Err(ServiceError::new(
                    ErrorCode::Unavailable,
                    "Late transport failure",
                ));
            }
        }
        Ok(value)
    }
    async fn get(&self, a: &MemoryAccess, id: &str) -> Result<Option<MemoryRecord>> {
        self.inner.get(a, id).await
    }
    async fn delete(&self, r: DeleteMemory) -> Result<MemoryRecord> {
        self.inner.delete(r).await
    }
    async fn list(
        &self,
        a: &MemoryAccess,
        after: Option<String>,
        limit: usize,
    ) -> Result<MemoryPage> {
        self.inner.list(a, after, limit).await
    }
    async fn search(&self, a: &MemoryAccess, q: &str, n: usize) -> Result<Vec<MemoryRecord>> {
        self.inner.search(a, q, n).await
    }
    async fn snapshot(&self) -> Result<MemorySnapshot> {
        let mut snapshot = self.inner.snapshot().await?;
        if self.pause_snapshot.swap(false, Ordering::SeqCst) {
            self.entered.notify_one();
            let _permit = self.release.acquire().await.unwrap();
        }
        if self.reverse_snapshot.load(Ordering::SeqCst) {
            snapshot.records.reverse();
            snapshot.receipts.reverse();
        }
        if self.corrupt_snapshot.load(Ordering::SeqCst) {
            if let Some(record) = snapshot.records.first_mut() {
                record.body.push_str(" corrupt");
            }
        }
        Ok(snapshot)
    }
    async fn import(&self, s: MemorySnapshot) -> Result<()> {
        if self.fail_import.load(Ordering::SeqCst) {
            return Err(ServiceError::new(ErrorCode::Unavailable, "Import failed"));
        }
        self.inner.import(s).await?;
        let number = self.imports.fetch_add(1, Ordering::SeqCst) + 1;
        if self.pause_import.swap(false, Ordering::SeqCst)
            || self.pause_import_at.load(Ordering::SeqCst) == number
        {
            self.entered.notify_one();
            let _permit = self.release.acquire().await.unwrap();
        }
        Ok(())
    }
}
#[tokio::test]
async fn catches_up_writes_and_tombstones_during_transfer() {
    let root = tempfile::tempdir().unwrap();
    let a = database(&root, "a.sqlite");
    let b = Arc::new(Controlled::new(database(&root, "b.sqlite"), true, false));
    let instance = b.descriptor().await.unwrap().instance_id;
    let r = setup(&root, a.clone(), b.clone()).await;
    write(&r, put("old", "delete during transfer")).await;
    let running = {
        let r = r.clone();
        tokio::spawn(async move { r.replace("concurrent", "b", &instance, 1).await.unwrap() })
    };
    b.entered.notified().await;
    for n in 0..25 {
        write(&r, put(&format!("record-{n}"), "confirmed during transfer")).await;
    }
    r.call(
        None,
        Call::Delete {
            request: DeleteMemory {
                request_id: "concurrent-delete".into(),
                access: access("agent-a"),
                id: "old".into(),
                expected_revision: None,
            },
        },
    )
    .await
    .unwrap();
    b.release.add_permits(1);
    assert_eq!(running.await.unwrap().stage, ReplacementStage::Retired);
    assert_eq!(a.snapshot().await.unwrap(), b.snapshot().await.unwrap());
    assert_eq!(r.binding().unwrap().unwrap().generation, 2);
}
#[tokio::test]
async fn cancelled_dispatched_write_blocks_switch_until_same_request_reconciles() {
    let root = tempfile::tempdir().unwrap();
    let a = Arc::new(Controlled::new(database(&root, "a.sqlite"), false, true));
    let b = database(&root, "b.sqlite");
    let instance = b.descriptor().await.unwrap().instance_id;
    let r = setup(&root, a.clone(), b.clone()).await;
    let request = put("uncertain", "possibly committed");
    let running = {
        let r = r.clone();
        let request = request.clone();
        tokio::spawn(async move { r.call(None, Call::Put { request }).await })
    };
    a.entered.notified().await;
    running.abort();
    let _ = running.await;
    drop(r);
    let r = setup(&root, a.clone(), b.clone()).await;
    let job = r.replace("blocked", "b", &instance, 1).await.unwrap();
    assert_eq!(job.stage, ReplacementStage::Blocked);
    assert!(matches!(job.error.unwrap().code, ErrorCode::Uncertain));
    assert_eq!(r.binding().unwrap().unwrap().generation, 1);
    write(&r, request).await;
    assert_eq!(a.snapshot().await.unwrap().watermark, 1); // idempotent reconciliation, no duplicate write
                                                          // The blocked import must not be reused blindly. A fresh empty destination is explicit.
    let c = database(&root, "c.sqlite");
    let id = c.descriptor().await.unwrap().instance_id;
    r.register("c".into(), c).await.unwrap();
    assert_eq!(
        r.replace("reconciled", "c", &id, 1).await.unwrap().stage,
        ReplacementStage::Retired
    );
}
#[tokio::test]
async fn destination_identity_and_nonempty_state_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    let a = database(&root, "a.sqlite");
    let b = database(&root, "b.sqlite");
    b.put(put("foreign", "must not overwrite")).await.unwrap();
    let before = b.snapshot().await.unwrap();
    let instance = b.descriptor().await.unwrap().instance_id;
    let r = setup(&root, a, b.clone()).await;
    assert_eq!(
        r.replace("identity", "b", "wrong-instance", 1)
            .await
            .unwrap()
            .stage,
        ReplacementStage::Blocked
    );
    assert_eq!(
        r.replace("occupied", "b", &instance, 1)
            .await
            .unwrap()
            .stage,
        ReplacementStage::Blocked
    );
    assert_eq!(before, b.snapshot().await.unwrap());
    assert_eq!(r.binding().unwrap().unwrap().generation, 1);
}
#[tokio::test]
async fn legacy_jobs_without_checkpoints_block_without_replaying() {
    for stage in [
        ReplacementStage::Requested,
        ReplacementStage::Validated,
        ReplacementStage::Preparing,
        ReplacementStage::Transferring,
        ReplacementStage::Checkpointed,
    ] {
        let root = tempfile::tempdir().unwrap();
        let a = database(&root, "a.sqlite");
        let b = database(&root, "b.sqlite");
        let r = setup(&root, a.clone(), b.clone()).await;
        write(&r, put("legacy", "retained")).await;
        let job = Replacement {
            id: "legacy".into(),
            binding: "memory.default".into(),
            source: "a".into(),
            destination: "b".into(),
            expected_generation: 1,
            stage,
            error: None,
            generation: None,
        };
        drop(r);
        let connection = rusqlite::Connection::open(root.path().join("registry.sqlite")).unwrap();
        connection
            .execute(
                "INSERT INTO replacements VALUES (?,?)",
                rusqlite::params![job.id, serde_json::to_string(&job).unwrap()],
            )
            .unwrap();
        drop(connection);
        let before_a = a.snapshot().await.unwrap();
        let before_b = b.snapshot().await.unwrap();
        let r = Registry::open(&root.path().join("registry.sqlite")).unwrap();
        let recovered = r.recover_pending().await.unwrap();
        assert_eq!(recovered[0].stage, ReplacementStage::Blocked);
        assert!(matches!(
            recovered[0].error.as_ref().unwrap().code,
            ErrorCode::Unsupported
        ));
        assert_eq!(r.binding().unwrap().unwrap().generation, 1);
        assert_eq!(a.snapshot().await.unwrap(), before_a);
        assert_eq!(b.snapshot().await.unwrap(), before_b);
    }
}
#[tokio::test]
async fn provider_identity_cannot_change_on_restart() {
    let root = tempfile::tempdir().unwrap();
    let r = Registry::open(&root.path().join("registry.sqlite")).unwrap();
    r.register("stable".into(), database(&root, "old.sqlite"))
        .await
        .unwrap();
    drop(r);
    let r = Registry::open(&root.path().join("registry.sqlite")).unwrap();
    assert!(matches!(
        r.register("stable".into(), database(&root, "different.sqlite"))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    ));
}
#[tokio::test]
async fn registry_service_resumes_events_and_requires_destination_decision() {
    let root = tempfile::tempdir().unwrap();
    let a = database(&root, "a.sqlite");
    let b = database(&root, "b.sqlite");
    let instance = b.descriptor().await.unwrap().instance_id;
    let r = setup(&root, a, b).await;
    let token = "registry-operator-token-with-at-least-32-bytes";
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let router = kageha_services::http::router(r.clone(), token.into()).unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = reqwest::Client::new();
    assert_eq!(
        client
            .get(format!("{endpoint}/v1/bindings"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let rejected=client.post(format!("{endpoint}/v1/replacements")).bearer_auth(token).json(&serde_json::json!({"request_id":"reject","expected_generation":1,"decision":{"destination_id":"b","instance_id":instance,"allow_data_transfer":false}})).send().await.unwrap();
    assert_eq!(rejected.status(), 403);
    assert!(r.job("reject").unwrap().is_none());
    let response=client.post(format!("{endpoint}/v1/replacements")).bearer_auth(token).json(&serde_json::json!({"request_id":"sse-switch","expected_generation":1,"decision":{"destination_id":"b","instance_id":instance,"allow_data_transfer":true}})).send().await.unwrap();
    assert!(response.status().is_success());
    let all = r.events(0).unwrap();
    let cursor = all[all.len() - 2].0;
    let mut stream = client
        .get(format!("{endpoint}/v1/events"))
        .bearer_auth(token)
        .header("Last-Event-ID", cursor.to_string())
        .send()
        .await
        .unwrap();
    let chunk = tokio::time::timeout(std::time::Duration::from_secs(2), stream.chunk())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let text = String::from_utf8_lossy(&chunk);
    assert!(text.contains("retired"), "{text}");
    assert!(text.contains(&format!("id: {}", cursor + 1)), "{text}");
    assert!(
        !text.contains("verified"),
        "Resumed stream must skip earlier events"
    );
    server.abort();
}
#[test]
fn missing_required_capabilities_cannot_activate() {
    let descriptor = Descriptor {
        protocol_version: VERSION,
        instance_id: "incomplete".into(),
        component: "memory".into(),
        adapter: "incomplete".into(),
        capabilities: BTreeSet::from(["memory.records".into()]),
    };
    assert!(matches!(
        descriptor.require(&memory_capabilities()).unwrap_err().code,
        ErrorCode::Unsupported
    ));
}
#[tokio::test]
async fn late_failure_cannot_demote_a_confirmed_idempotent_receipt() {
    let root = tempfile::tempdir().unwrap();
    let a = Arc::new(Controlled::new(database(&root, "a.sqlite"), false, true));
    a.late_error.store(true, Ordering::SeqCst);
    let b = database(&root, "b.sqlite");
    let instance = b.descriptor().await.unwrap().instance_id;
    let r = setup(&root, a.clone(), b).await;
    let request = put("same-request", "confirmed");
    let late = {
        let r = r.clone();
        let request = request.clone();
        tokio::spawn(async move { r.call(None, Call::Put { request }).await })
    };
    a.entered.notified().await;
    write(&r, request).await;
    a.release.add_permits(1);
    assert!(late.await.unwrap().is_err());
    assert_eq!(
        r.replace("late-safe", "b", &instance, 1)
            .await
            .unwrap()
            .stage,
        ReplacementStage::Retired
    );
}
#[tokio::test]
async fn export_order_differences_do_not_block_compatible_implementations() {
    let root = tempfile::tempdir().unwrap();
    let a = database(&root, "a.sqlite");
    let b = Arc::new(Controlled::new(database(&root, "b.sqlite"), false, false));
    b.reverse_snapshot.store(true, Ordering::SeqCst);
    let instance = b.descriptor().await.unwrap().instance_id;
    let r = setup(&root, a, b.clone()).await;
    write(&r, put("one", "first")).await;
    write(&r, put("two", "second")).await;
    assert_eq!(
        r.replace("ordering", "b", &instance, 1)
            .await
            .unwrap()
            .stage,
        ReplacementStage::Retired
    );
}
#[tokio::test]
async fn import_and_verification_failures_keep_the_original_binding() {
    for verification in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let a = database(&root, "a.sqlite");
        let b = Arc::new(Controlled::new(database(&root, "b.sqlite"), false, false));
        b.fail_import.store(!verification, Ordering::SeqCst);
        b.corrupt_snapshot.store(verification, Ordering::SeqCst);
        let instance = b.descriptor().await.unwrap().instance_id;
        let r = setup(&root, a.clone(), b).await;
        write(&r, put("one", "retained confirmed record")).await;
        let before = a.snapshot().await.unwrap();
        let job = r.replace("failure", "b", &instance, 1).await.unwrap();
        assert_eq!(job.stage, ReplacementStage::Blocked);
        assert_eq!(r.binding().unwrap().unwrap().generation, 1);
        assert_eq!(a.snapshot().await.unwrap(), before);
        if verification {
            assert!(matches!(
                job.error.unwrap().code,
                ErrorCode::VerificationFailed
            ));
        }
    }
}

#[tokio::test]
async fn lost_writer_claim_response_is_reconciled_after_coordinator_restart() {
    let root = tempfile::tempdir().unwrap();
    let a = Arc::new(Controlled::new(database(&root, "a.sqlite"), false, false));
    a.late_claim_error.store(true, Ordering::SeqCst);
    let r = Registry::open(&root.path().join("registry.sqlite")).unwrap();
    r.register("a".into(), a.clone()).await.unwrap();
    assert!(r.initialize("a").await.is_err());
    let fence = a.fence_status().await.unwrap().unwrap();
    assert_eq!(fence.epoch, 1);
    drop(r);
    let r = Registry::open(&root.path().join("registry.sqlite")).unwrap();
    r.register("a".into(), a.clone()).await.unwrap();
    r.initialize("a").await.unwrap();
    assert!(a.fence_status().await.unwrap().unwrap().epoch > fence.epoch);
    write(&r, put("after", "reconciled without a new writer epoch")).await;
}
#[tokio::test]
async fn real_interrupted_transfers_resume_from_durable_checkpoints() {
    for stage in [
        ReplacementStage::Requested,
        ReplacementStage::Validated,
        ReplacementStage::Preparing,
        ReplacementStage::Transferring,
        ReplacementStage::Checkpointed,
    ] {
        let root = tempfile::tempdir().unwrap();
        let a = Arc::new(Controlled::new(database(&root, "a.sqlite"), false, false));
        let b = Arc::new(Controlled::new(database(&root, "b.sqlite"), false, false));
        let instance = b.descriptor().await.unwrap().instance_id;
        let r = setup(&root, a.clone(), b.clone()).await;
        write(&r, put("before", "preserved")).await;
        let paused = match stage {
            ReplacementStage::Requested => {
                b.pause_descriptor.store(true, Ordering::SeqCst);
                b.clone()
            }
            ReplacementStage::Validated => {
                b.pause_snapshot.store(true, Ordering::SeqCst);
                b.clone()
            }
            ReplacementStage::Preparing => {
                a.pause_snapshot.store(true, Ordering::SeqCst);
                a.clone()
            }
            ReplacementStage::Transferring => {
                b.pause_import.store(true, Ordering::SeqCst);
                b.clone()
            }
            _ => {
                a.pause_retire.store(true, Ordering::SeqCst);
                a.clone()
            }
        };
        let transfer = {
            let r = r.clone();
            tokio::spawn(async move { r.replace("interrupted", "b", &instance, 1).await.unwrap() })
        };
        tokio::time::timeout(std::time::Duration::from_secs(2), paused.entered.notified())
            .await
            .unwrap();
        assert_eq!(r.job("interrupted").unwrap().unwrap().stage, stage);
        transfer.abort();
        let _ = transfer.await;
        drop(r);
        let recovered = setup(&root, a.clone(), b.clone()).await;
        write(
            &recovered,
            put("during-recovery", "confirmed after interruption"),
        )
        .await;
        let jobs = recovered.recover_pending().await.unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].stage, ReplacementStage::Retired, "{:?}", jobs[0]);
        assert_eq!(recovered.binding().unwrap().unwrap().generation, 2);
        assert_eq!(a.snapshot().await.unwrap(), b.snapshot().await.unwrap());
        assert_eq!(b.snapshot().await.unwrap().records.len(), 2);
    }
}
fn rewind_stage(root: &tempfile::TempDir, id: &str, stage: ReplacementStage) {
    let db = rusqlite::Connection::open(root.path().join("registry.sqlite")).unwrap();
    let body: String = db
        .query_row("SELECT body FROM replacements WHERE id=?", [id], |r| {
            r.get(0)
        })
        .unwrap();
    let mut job: Replacement = serde_json::from_str(&body).unwrap();
    job.stage = stage;
    db.execute(
        "UPDATE replacements SET body=? WHERE id=?",
        rusqlite::params![serde_json::to_string(&job).unwrap(), id],
    )
    .unwrap();
}
#[tokio::test]
async fn committed_recovery_preserves_post_cutover_writes_instead_of_reimporting_old_state() {
    for stage in [ReplacementStage::Committed, ReplacementStage::Verified] {
        let root = tempfile::tempdir().unwrap();
        let a = database(&root, "a.sqlite");
        let b = database(&root, "b.sqlite");
        let instance = b.descriptor().await.unwrap().instance_id;
        let r = setup(&root, a.clone(), b.clone()).await;
        write(&r, put("before", "original")).await;
        r.replace("committed", "b", &instance, 1).await.unwrap();
        write(&r, put("before", "confirmed new version")).await;
        write(&r, put("after", "confirmed new record")).await;
        let before = b.snapshot().await.unwrap();
        rewind_stage(&root, "committed", stage);
        drop(r);
        let recovered = setup(&root, a, b.clone()).await;
        let jobs = recovered.recover_pending().await.unwrap();
        assert_eq!(jobs[0].stage, ReplacementStage::Retired);
        assert_eq!(b.snapshot().await.unwrap(), before);
        assert_eq!(recovered.binding().unwrap().unwrap().generation, 2);
    }
}
#[tokio::test]
async fn another_control_plane_cannot_silently_adopt_an_owned_backend() {
    let root = tempfile::tempdir().unwrap();
    let a = database(&root, "a.sqlite");
    let first = Registry::open(&root.path().join("first.sqlite")).unwrap();
    first.register("a".into(), a.clone()).await.unwrap();
    first.initialize("a").await.unwrap();
    let second = Registry::open(&root.path().join("second.sqlite")).unwrap();
    second.register("a".into(), a.clone()).await.unwrap();
    assert!(matches!(
        second.initialize("a").await.unwrap_err().code,
        ErrorCode::Conflict
    ));
    write(&first, put("owner", "first control plane remains active")).await;
}
#[tokio::test]
async fn recovery_blocks_corrupted_committed_state_until_repaired_without_overwriting_it() {
    let root = tempfile::tempdir().unwrap();
    let a = database(&root, "a.sqlite");
    let b = Arc::new(Controlled::new(database(&root, "b.sqlite"), false, false));
    let instance = b.descriptor().await.unwrap().instance_id;
    let r = setup(&root, a.clone(), b.clone()).await;
    write(&r, put("before", "verified record")).await;
    r.replace("corrupt", "b", &instance, 1).await.unwrap();
    rewind_stage(&root, "corrupt", ReplacementStage::Committed);
    b.corrupt_snapshot.store(true, Ordering::SeqCst);
    drop(r);
    let recovered = setup(&root, a, b.clone()).await;
    let jobs = recovered.recover_pending().await.unwrap();
    assert_eq!(jobs[0].stage, ReplacementStage::Blocked);
    assert!(matches!(
        recovered
            .call(
                None,
                Call::Get {
                    access: access("agent-a"),
                    id: "before".into()
                }
            )
            .await
            .unwrap_err()
            .code,
        ErrorCode::VerificationFailed
    ));
    let c = database(&root, "c.sqlite");
    let identity = c.descriptor().await.unwrap().instance_id;
    recovered.register("c".into(), c.clone()).await.unwrap();
    assert!(matches!(
        recovered
            .replace("bypass-corruption", "c", &identity, 2)
            .await
            .unwrap_err()
            .code,
        ErrorCode::VerificationFailed
    ));
    assert!(matches!(
        recovered
            .rollback("unsafe-rollback", "corrupt", 2)
            .await
            .unwrap_err()
            .code,
        ErrorCode::VerificationFailed
    ));
    assert_eq!(c.snapshot().await.unwrap().watermark, 0);
    b.corrupt_snapshot.store(false, Ordering::SeqCst);
    assert_eq!(
        recovered.resume("corrupt").await.unwrap().stage,
        ReplacementStage::Retired
    );
    write(&recovered, put("after", "resumed safely")).await;
}
#[tokio::test]
async fn blocked_transfer_resumes_on_the_same_destination_after_receipt_reconciliation() {
    let root = tempfile::tempdir().unwrap();
    let a = Arc::new(Controlled::new(database(&root, "a.sqlite"), false, true));
    let b = database(&root, "b.sqlite");
    let instance = b.descriptor().await.unwrap().instance_id;
    let r = setup(&root, a.clone(), b).await;
    let request = put("receipt", "confirmed once");
    let call = {
        let r = r.clone();
        let request = request.clone();
        tokio::spawn(async move { r.call(None, Call::Put { request }).await })
    };
    a.entered.notified().await;
    call.abort();
    let _ = call.await;
    assert_eq!(
        r.replace("blocked-same", "b", &instance, 1)
            .await
            .unwrap()
            .stage,
        ReplacementStage::Blocked
    );
    assert_eq!(
        r.resume("blocked-same").await.unwrap().stage,
        ReplacementStage::Blocked
    );
    write(&r, request).await;
    assert_eq!(
        r.resume("blocked-same").await.unwrap().stage,
        ReplacementStage::Retired
    );
    assert_eq!(a.snapshot().await.unwrap().watermark, 1);
}
#[tokio::test]
async fn rollback_can_resume_after_the_source_writer_was_retired() {
    let root = tempfile::tempdir().unwrap();
    let a = Arc::new(Controlled::new(database(&root, "a.sqlite"), false, false));
    let b = Arc::new(Controlled::new(database(&root, "b.sqlite"), false, false));
    let instance = b.descriptor().await.unwrap().instance_id;
    let r = setup(&root, a.clone(), b.clone()).await;
    write(&r, put("before", "old")).await;
    r.replace("forward", "b", &instance, 1).await.unwrap();
    write(&r, put("new", "confirmed after cutover")).await;
    b.pause_retire.store(true, Ordering::SeqCst);
    let rollback = {
        let r = r.clone();
        tokio::spawn(async move {
            r.rollback("interrupted-rollback", "forward", 2)
                .await
                .unwrap()
        })
    };
    b.entered.notified().await;
    rollback.abort();
    let _ = rollback.await;
    drop(r);
    let recovered = setup(&root, a.clone(), b.clone()).await;
    write(
        &recovered,
        put("later", "confirmed before rollback resumes"),
    )
    .await;
    let jobs = recovered.recover_pending().await.unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].stage, ReplacementStage::RolledBack);
    assert_eq!(recovered.binding().unwrap().unwrap().generation, 3);
    assert_eq!(a.snapshot().await.unwrap(), b.snapshot().await.unwrap());
    assert_eq!(a.snapshot().await.unwrap().records.len(), 3);
}

#[tokio::test]
async fn restored_coordinator_fences_a_late_staging_worker_before_new_writes() {
    let root = tempfile::tempdir().unwrap();
    let a = Arc::new(Controlled::new(database(&root, "a.sqlite"), false, false));
    let b = Arc::new(Controlled::new(database(&root, "b.sqlite"), true, false));
    let instance = b.descriptor().await.unwrap().instance_id;
    let old = setup(&root, a.clone(), b.clone()).await;
    write(&old, put("before", "confirmed source")).await;
    let delayed = {
        let old = old.clone();
        tokio::spawn(async move { old.replace("in-flight", "b", &instance, 1).await })
    };
    b.entered.notified().await;
    let copy = root.path().join("restored.sqlite");
    let source = rusqlite::Connection::open(root.path().join("registry.sqlite")).unwrap();
    source
        .execute("VACUUM INTO ?", [copy.to_str().unwrap()])
        .unwrap();
    drop(source);
    let restored = Registry::open(&copy).unwrap();
    restored.register("a".into(), a.clone()).await.unwrap();
    restored.register("b".into(), b.clone()).await.unwrap();
    restored.initialize("a").await.unwrap();
    assert_eq!(
        restored.recover_pending().await.unwrap()[0].stage,
        ReplacementStage::Retired
    );
    write(
        &restored,
        put("after", "new destination write must survive"),
    )
    .await;
    let confirmed = b.snapshot().await.unwrap();
    b.release.add_permits(1);
    let result = delayed.await.unwrap().unwrap();
    assert_eq!(result.stage, ReplacementStage::Blocked);
    assert!(matches!(
        result.error.unwrap().code,
        ErrorCode::StaleGeneration
    ));
    assert_eq!(b.snapshot().await.unwrap(), confirmed);
    assert!(b.get(&access("agent-a"), "after").await.unwrap().is_some());
}

#[tokio::test]
async fn logical_tool_writes_pause_after_cancel_but_reads_and_other_scopes_continue() {
    let root = tempfile::tempdir().unwrap();
    let a = Arc::new(Controlled::new(database(&root, "a.sqlite"), false, true));
    let b = database(&root, "b.sqlite");
    let registry = setup(&root, a.clone(), b.clone()).await;
    let request = put("ambiguous", "committed before disconnect");
    let pending = {
        let registry = registry.clone();
        let request = request.clone();
        tokio::spawn(async move {
            LogicalMemoryPort::call(registry.as_ref(), Some(1), Call::Put { request }).await
        })
    };
    a.entered.notified().await;
    let queued={let registry=registry.clone();tokio::spawn(async move{LogicalMemoryPort::call(registry.as_ref(),Some(1),Call::Put{request:put("queued","must not dispatch")}).await})};
    tokio::task::yield_now().await;
    assert!(!queued.is_finished());
    let mut independent=put("other","allowed");independent.access=access("agent-b");
    tokio::time::timeout(std::time::Duration::from_secs(1),LogicalMemoryPort::call(registry.as_ref(),Some(1),Call::Put{request:independent})).await.unwrap().unwrap();
    pending.abort();
    let _ = pending.await;
    assert_eq!(queued.await.unwrap().unwrap_err().code,ErrorCode::Uncertain);
    let error = LogicalMemoryPort::call(
        registry.as_ref(),
        Some(1),
        Call::Put {
            request: put("later", "must pause"),
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::Uncertain);
    let record = LogicalMemoryPort::call(
        registry.as_ref(),
        Some(1),
        Call::Get {
            access: access("agent-a"),
            id: "ambiguous".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(record.value["body"], "committed before disconnect");
    let mut unrelated = put("other", "allowed");
    unrelated.access = access("agent-b");
    LogicalMemoryPort::call(registry.as_ref(), Some(1), Call::Put { request: unrelated })
        .await
        .unwrap();
    drop(registry);
    let registry = setup(&root, a.clone(), b).await;
    assert_eq!(
        LogicalMemoryPort::call(
            registry.as_ref(),
            Some(1),
            Call::Put {
                request: put("later", "still blocked")
            }
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::Uncertain
    );
    // Explicit operator reconciliation reuses the original request; backend receipt deduplication proves no duplicate.
    registry.call(Some(1), Call::Put { request }).await.unwrap();
    LogicalMemoryPort::call(
        registry.as_ref(),
        Some(1),
        Call::Put {
            request: put("later", "resumed"),
        },
    )
    .await
    .unwrap();
    assert_eq!(a.snapshot().await.unwrap().watermark, 3);
}
