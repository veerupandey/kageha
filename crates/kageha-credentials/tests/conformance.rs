use kageha_contracts::{execution::Control, ErrorCode};
use kageha_credentials::{keys::SuppliedKey, repository::SqliteRepository, *};
use std::{
    collections::BTreeSet,
    sync::{atomic::AtomicBool, Arc},
};
fn set(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|v| v.to_string()).collect()
}
fn grants() -> Vec<Grant> {
    ["models", "system"]
        .into_iter()
        .map(|namespace| Grant {
            principal: "adapter".into(),
            namespace: namespace.into(),
            ids: set(&["*"]),
            purposes: set(&["model", "admin"]),
            destinations: set(&["https://provider.example", "local"]),
            permissions: [
                Permission::Metadata,
                Permission::Resolve,
                Permission::Write,
                Permission::Revoke,
                Permission::Export,
                Permission::Admin,
            ]
            .into_iter()
            .collect(),
        })
        .collect()
}
fn access() -> CredentialAccess {
    CredentialAccess {
        principal: "adapter".into(),
        purpose: "model".into(),
        destination: "https://provider.example".into(),
    }
}
fn reference() -> CredentialRef {
    CredentialRef {
        namespace: "models".into(),
        id: "primary".into(),
    }
}
fn write(id: &str, revision: u64, value: &[u8]) -> CredentialWrite {
    CredentialWrite {
        operation_id: id.into(),
        reference: reference(),
        expected_revision: revision,
        kind: "api_key".into(),
        expires_at_ms: None,
        secret: SecretBytes::new(value.to_vec()),
    }
}
fn control(flag: &AtomicBool) -> Control<'_> {
    Control {
        cancelled: flag,
        deadline: None,
    }
}
async fn fixture() -> (Arc<Vault>, Arc<SqliteRepository>) {
    let repo = Arc::new(SqliteRepository::in_memory().unwrap());
    let flag = AtomicBool::new(false);
    let v = Vault::open(
        repo.clone(),
        Arc::new(SuppliedKey::new("key", SecretBytes::new(vec![7; 32]))),
        "key",
        grants(),
        &control(&flag),
    )
    .await
    .unwrap();
    (Arc::new(v), repo)
}
#[tokio::test]
async fn cas_idempotency_and_revocation_fence() {
    let (v, _) = fixture().await;
    let f = AtomicBool::new(false);
    let c = control(&f);
    let a = access();
    let first = v
        .put(&a, write("one", 0, b"CANARY-SECRET"), &c)
        .await
        .unwrap();
    assert_eq!(first.metadata.revision, 1);
    let lease = v.resolve(&a, &reference(), &c).await.unwrap();
    assert_eq!(lease.secret.expose(), b"CANARY-SECRET");
    assert!(!format!("{lease:?}").contains("CANARY"));
    assert_eq!(
        v.put(&a, write("one", 0, b"CANARY-SECRET"), &c)
            .await
            .unwrap()
            .metadata
            .revision,
        1
    );
    assert_eq!(
        v.put(&a, write("one", 0, b"different"), &c)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(
        v.put(&a, write("stale", 0, b"different"), &c)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    v.revoke(&a, &reference(), 1, "revoke", true, &c)
        .await
        .unwrap();
    assert!(v.validate_lease(&a, &lease, &c).await.is_err());
    assert!(v.resolve(&a, &reference(), &c).await.is_err());
    assert_eq!(
        v.metadata(&a, &reference(), &c).await.unwrap().state,
        CredentialState::Deleted
    );
    assert_eq!(
        v.put(&a, write("one", 0, b"CANARY-SECRET"), &c)
            .await
            .unwrap()
            .metadata
            .revision,
        1
    );
    assert!(v.resolve(&a, &reference(), &c).await.is_err());
}
#[tokio::test]
async fn explicit_identity_scope_and_destination() {
    let (v, _) = fixture().await;
    let f = AtomicBool::new(false);
    let c = control(&f);
    v.put(&access(), write("one", 0, b"secret"), &c)
        .await
        .unwrap();
    for mut a in [access(), access(), access()] {
        if a.principal == "adapter" {
            a.principal = "agent".into();
        }
        assert_eq!(
            v.resolve(&a, &reference(), &c).await.unwrap_err().code,
            ErrorCode::Unauthorized
        );
    }
    let mut a = access();
    a.destination = "https://attacker.example".into();
    assert!(v.resolve(&a, &reference(), &c).await.is_err());
    a = access();
    a.purpose = "shell".into();
    assert!(v.resolve(&a, &reference(), &c).await.is_err());
    let r = CredentialRef {
        namespace: "other".into(),
        id: "primary".into(),
    };
    assert!(v.resolve(&access(), &r, &c).await.is_err());
    let audit = v.audits(&access(), "models", 0, &c).await.unwrap();
    assert!(audit.iter().any(|v| v.outcome == "denied"));
    assert!(!serde_json::to_string(&audit).unwrap().contains("secret"));
}
#[tokio::test]
async fn encrypted_database_tamper_and_wrong_key() {
    let dir = tempfile::tempdir().unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let db = dir.path().join("vault.sqlite");
    let repo = Arc::new(SqliteRepository::open(&db).unwrap());
    let f = AtomicBool::new(false);
    let c = control(&f);
    let v = Vault::open(
        repo.clone(),
        Arc::new(SuppliedKey::new("key", SecretBytes::new(vec![7; 32]))),
        "key",
        grants(),
        &c,
    )
    .await
    .unwrap();
    v.put(&access(), write("one", 0, b"UNIQUE-PLAINTEXT-CANARY"), &c)
        .await
        .unwrap();
    let bytes = std::fs::read(&db).unwrap();
    assert!(!bytes.windows(23).any(|b| b == b"UNIQUE-PLAINTEXT-CANARY"));
    assert!(Vault::open(
        repo.clone(),
        Arc::new(SuppliedKey::new("key", SecretBytes::new(vec![8; 32]))),
        "key",
        grants(),
        &c
    )
    .await
    .is_err());
    let mut row = repo.record(&reference()).await.unwrap().unwrap();
    row.ciphertext[25] ^= 1;
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute(
        "UPDATE credential_records SET body=?",
        [serde_json::to_vec(&row).unwrap()],
    )
    .unwrap();
    assert_eq!(
        v.resolve(&access(), &reference(), &c)
            .await
            .unwrap_err()
            .code,
        ErrorCode::VerificationFailed
    );
}
#[tokio::test]
async fn concurrent_updates_only_one_commit() {
    let (v, _) = fixture().await;
    let f = AtomicBool::new(false);
    let c = control(&f);
    let a = access();
    let (one, two) = tokio::join!(
        v.put(&a, write("one", 0, b"one"), &c),
        v.put(&a, write("two", 0, b"two"), &c)
    );
    assert_ne!(one.is_ok(), two.is_ok());
    assert_eq!(v.metadata(&a, &reference(), &c).await.unwrap().revision, 1);
}
#[tokio::test]
async fn cancellation_expiry_and_read_only_grants() {
    let (v, _) = fixture().await;
    let f = AtomicBool::new(false);
    let c = control(&f);
    let mut w = write("expired", 0, b"secret");
    w.expires_at_ms = Some(1);
    v.put(&access(), w, &c).await.unwrap();
    assert!(v.resolve(&access(), &reference(), &c).await.is_err());
    let cancelled = AtomicBool::new(true);
    assert_eq!(
        v.put(
            &access(),
            write("cancelled", 1, b"new"),
            &control(&cancelled)
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::Cancelled
    );
    assert!(v
        .operation(&access(), &reference(), "cancelled", &c)
        .await
        .unwrap()
        .is_none());
}
#[tokio::test]
async fn recovery_export_authentication_and_scope() {
    let (v, _) = fixture().await;
    let f = AtomicBool::new(false);
    let c = control(&f);
    v.put(&access(), write("one", 0, b"RECOVERY-CANARY"), &c)
        .await
        .unwrap();
    let bundle = v
        .export_recovery(
            &access(),
            "models",
            SecretBytes::new(b"a-long-recovery-passphrase".to_vec()),
            &c,
        )
        .await
        .unwrap();
    assert!(!String::from_utf8_lossy(&bundle).contains("RECOVERY-CANARY"));
    assert!(recovery::open_recovery(
        &bundle,
        SecretBytes::new(b"wrong-but-long-passphrase".to_vec()),
        &c
    )
    .await
    .is_err());
    let records = recovery::open_recovery(
        &bundle,
        SecretBytes::new(b"a-long-recovery-passphrase".to_vec()),
        &c,
    )
    .await
    .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].secret.expose(), b"RECOVERY-CANARY");
    assert_eq!(records[0].metadata.reference, reference());
}
#[tokio::test]
async fn private_key_files_and_atomic_rotation() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let old = dir.path().join("old");
    let new = dir.path().join("new");
    std::fs::write(&old, [7; 32]).unwrap();
    std::fs::write(&new, [8; 32]).unwrap();
    std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::set_permissions(&new, std::fs::Permissions::from_mode(0o600)).unwrap();
    let keys = Arc::new(keys::FileKeys(
        [("old".into(), old.clone()), ("new".into(), new)]
            .into_iter()
            .collect(),
    ));
    let f = AtomicBool::new(false);
    let c = control(&f);
    let repo = Arc::new(SqliteRepository::open(&dir.path().join("vault.sqlite")).unwrap());
    let v = Vault::open(repo.clone(), keys.clone(), "old", grants(), &c)
        .await
        .unwrap();
    v.put(&access(), write("one", 0, b"secret"), &c)
        .await
        .unwrap();
    let lease = v.resolve(&access(), &reference(), &c).await.unwrap();
    assert!(v.rotate_key(&access(), "models", "new", &c).await.is_err());
    assert_eq!(
        v.rotate_key(&access(), "system", "new", &c).await.unwrap(),
        2
    );
    assert!(v.validate_lease(&access(), &lease, &c).await.is_err());
    assert_eq!(
        v.resolve(&access(), &reference(), &c)
            .await
            .unwrap()
            .secret
            .expose(),
        b"secret"
    );
    assert_eq!(
        v.put(&access(), write("one", 0, b"secret"), &c)
            .await
            .unwrap()
            .metadata
            .revision,
        1
    );
    let restarted = Vault::open(repo, keys.clone(), "old", grants(), &c)
        .await
        .unwrap();
    assert_eq!(
        restarted
            .resolve(&access(), &reference(), &c)
            .await
            .unwrap()
            .secret
            .expose(),
        b"secret"
    );
    std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(keys.unlock("old", &c).await.is_err());
}
#[tokio::test]
async fn restored_refs_and_tombstones_require_new_client_authority() {
    let (v, _) = fixture().await;
    let f = AtomicBool::new(false);
    let c = control(&f);
    let a = access();
    v.put(&a, write("one", 0, b"secret"), &c).await.unwrap();
    v.revoke(&a, &reference(), 1, "delete", true, &c)
        .await
        .unwrap();
    let bundle = v
        .export_recovery(
            &a,
            "models",
            SecretBytes::new(b"a-long-recovery-passphrase".to_vec()),
            &c,
        )
        .await
        .unwrap();
    let (target, _) = fixture().await;
    assert_eq!(
        target
            .restore_recovery(
                &a,
                &bundle,
                SecretBytes::new(b"a-long-recovery-passphrase".to_vec()),
                &c
            )
            .await
            .unwrap(),
        2
    );
    let meta = target.metadata(&a, &reference(), &c).await.unwrap();
    assert_eq!(meta.revision, 2);
    assert_eq!(meta.state, CredentialState::Deleted);
    assert!(target.resolve(&a, &reference(), &c).await.is_err());
    assert!(target
        .restore_recovery(
            &a,
            &bundle,
            SecretBytes::new(b"a-long-recovery-passphrase".to_vec()),
            &c
        )
        .await
        .is_err());
}
#[tokio::test]
async fn oauth_grant_refresh_is_atomic_and_late_reply_cannot_restore_revocation() {
    let (v, _) = fixture().await;
    let f = AtomicBool::new(false);
    let c = control(&f);
    let a = access();
    let mut w = write("oauth-initial", 0, b"access-and-refresh-token-bundle");
    w.kind = "oauth_grant".into();
    v.put(&a, w, &c).await.unwrap();
    let ticket = v
        .begin_refresh(&a, &reference(), 1, "prepare", &c)
        .await
        .unwrap();
    assert!(v
        .begin_refresh(&a, &reference(), 1, "parallel", &c)
        .await
        .is_err());
    assert!(v.resolve(&a, &reference(), &c).await.is_err());
    let (dispatched, grant) = v
        .dispatch_refresh(&a, &ticket, "dispatch", &c)
        .await
        .unwrap();
    assert_eq!(grant.expose(), b"access-and-refresh-token-bundle");
    assert!(v.dispatch_refresh(&a, &ticket, "replay", &c).await.is_err());
    let result = v
        .complete_refresh(
            &a,
            &dispatched,
            "new-tokens",
            SecretBytes::new(b"rotated-access-and-refresh".to_vec()),
            None,
            &c,
        )
        .await
        .unwrap();
    assert_eq!(
        v.resolve(&a, &reference(), &c)
            .await
            .unwrap()
            .secret
            .expose(),
        b"rotated-access-and-refresh"
    );
    let next = v
        .begin_refresh(&a, &reference(), result.metadata.revision, "prepare-2", &c)
        .await
        .unwrap();
    let (next, _) = v
        .dispatch_refresh(&a, &next, "dispatch-2", &c)
        .await
        .unwrap();
    v.revoke(&a, &reference(), next.revision, "disconnect", false, &c)
        .await
        .unwrap();
    assert!(v
        .complete_refresh(
            &a,
            &next,
            "late-refresh",
            SecretBytes::new(b"must-not-restore".to_vec()),
            None,
            &c
        )
        .await
        .is_err());
    assert!(v.resolve(&a, &reference(), &c).await.is_err());
}
#[tokio::test]
async fn unknown_refresh_dispatch_pauses_grant_and_never_replays() {
    let (v, _) = fixture().await;
    let f = AtomicBool::new(false);
    let c = control(&f);
    let mut w = write("grant", 0, b"refresh-once");
    w.kind = "oauth_grant".into();
    v.put(&access(), w, &c).await.unwrap();
    let short = Control {
        cancelled: &f,
        deadline: Some(std::time::Instant::now() + std::time::Duration::from_millis(250)),
    };
    let ticket = v
        .begin_refresh(&access(), &reference(), 1, "prepared-short", &short)
        .await
        .unwrap();
    assert_eq!(
        v.begin_refresh(&access(), &reference(), 1, "prepared-short", &short)
            .await
            .unwrap()
            .lease_id,
        ticket.lease_id
    );
    let (dispatched, _) = v
        .dispatch_refresh(&access(), &ticket, "dispatched-short", &c)
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let result = v
        .recover_refresh(
            &access(),
            &reference(),
            dispatched.revision,
            "unknown-outcome",
            &c,
        )
        .await
        .unwrap();
    assert_eq!(result.metadata.state, CredentialState::RefreshUncertain);
    assert!(v
        .begin_refresh(
            &access(),
            &reference(),
            result.metadata.revision,
            "unsafe-retry",
            &c
        )
        .await
        .is_err());
    assert!(v.resolve(&access(), &reference(), &c).await.is_err());
}
#[tokio::test]
async fn explicit_mounted_source_has_no_fallback_and_detects_changes() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mounted");
    std::fs::write(&path, b"first").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let resolver = sources::ReadOnlySources::new(vec![sources::SourceBinding {
        reference: reference(),
        access: access(),
        source: sources::Source::MountedFile(path.clone()),
    }])
    .unwrap();
    let f = AtomicBool::new(false);
    let c = control(&f);
    let old = resolver.resolve(&access(), &reference(), &c).await.unwrap();
    std::fs::write(&path, b"second").unwrap();
    assert!(resolver.validate_lease(&access(), &old, &c).await.is_err());
    std::fs::remove_file(path).unwrap();
    assert!(resolver.resolve(&access(), &reference(), &c).await.is_err());
}
#[tokio::test]
async fn record_substitution_and_repeated_values_use_authenticated_distinct_nonces() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = dir.path().join("vault.sqlite");
    let repo = Arc::new(SqliteRepository::open(&path).unwrap());
    let f = AtomicBool::new(false);
    let c = control(&f);
    let v = Vault::open(
        repo.clone(),
        Arc::new(SuppliedKey::new("key", SecretBytes::new(vec![7; 32]))),
        "key",
        grants(),
        &c,
    )
    .await
    .unwrap();
    v.put(&access(), write("one", 0, b"secret"), &c)
        .await
        .unwrap();
    let one = repo.record(&reference()).await.unwrap().unwrap();
    v.put(&access(), write("two", 1, b"secret"), &c)
        .await
        .unwrap();
    let two = repo.record(&reference()).await.unwrap().unwrap();
    assert_ne!(&one.ciphertext[..24], &two.ciphertext[..24]);
    assert_ne!(&one.wrapped_key[..24], &two.wrapped_key[..24]);
    let mut substituted = two;
    substituted.metadata.reference.id = "different".into();
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute(
        "UPDATE credential_records SET id='different',body=?",
        [serde_json::to_vec(&substituted).unwrap()],
    )
    .unwrap();
    let mut r = reference();
    r.id = "different".into();
    assert_eq!(
        v.resolve(&access(), &r, &c).await.unwrap_err().code,
        ErrorCode::VerificationFailed
    );
}
#[tokio::test]
async fn revoked_workloads_cannot_resolve_or_reuse_a_lease() {
    let (v, repo) = fixture().await;
    let f = AtomicBool::new(false);
    let c = control(&f);
    v.put(&access(), write("create", 0, b"secret"), &c)
        .await
        .unwrap();
    let lease = v.resolve(&access(), &reference(), &c).await.unwrap();
    let admin = CredentialAccess {
        principal: "security-operator".into(),
        purpose: "admin".into(),
        destination: "local".into(),
    };
    let mut configured = grants();
    configured.push(Grant {
        principal: admin.principal.clone(),
        namespace: "system".into(),
        ids: set(&["*"]),
        purposes: set(&["admin"]),
        destinations: set(&["local"]),
        permissions: BTreeSet::from([Permission::Admin]),
    });
    let control_vault = Vault::open(
        repo,
        Arc::new(SuppliedKey::new("key", SecretBytes::new(vec![7; 32]))),
        "key",
        configured,
        &c,
    )
    .await
    .unwrap();
    control_vault
        .revoke_workload(&admin, "adapter", 0, "revoke-workload", &c)
        .await
        .unwrap();
    assert!(v.validate_lease(&access(), &lease, &c).await.is_err());
    assert!(v.resolve(&access(), &reference(), &c).await.is_err());
    assert!(v
        .put(&access(), write("late-write", 1, b"late"), &c)
        .await
        .is_err());
}
struct LockedProtector;
#[async_trait::async_trait]
impl KeyProtector for LockedProtector {
    async fn unlock(&self, _: &str, _: &Control<'_>) -> kageha_contracts::Result<SecretBytes> {
        Err(kageha_contracts::ServiceError::new(
            ErrorCode::Unavailable,
            "Key protector locked",
        ))
    }
}
#[tokio::test]
async fn locked_protector_never_creates_a_replacement_key() {
    let (_, repo) = fixture().await;
    let f = AtomicBool::new(false);
    let c = control(&f);
    let before = repo.header().await.unwrap().unwrap();
    assert!(
        Vault::open(repo.clone(), Arc::new(LockedProtector), "key", grants(), &c)
            .await
            .is_err()
    );
    let after = repo.header().await.unwrap().unwrap();
    assert_eq!(before.encrypted_check, after.encrypted_check);
    assert_eq!(before.key_id, after.key_id);
}
#[tokio::test]
async fn audit_pagination_cannot_be_starved_by_other_namespaces() {
    let (v, repo) = fixture().await;
    let f = AtomicBool::new(false);
    let c = control(&f);
    for _ in 0..1001 {
        repo.append_audit(CredentialAudit {
            sequence: 0,
            at_ms: 1,
            principal: "other".into(),
            action: "read".into(),
            reference: Some(CredentialRef {
                namespace: "other".into(),
                id: "x".into(),
            }),
            outcome: "allowed".into(),
        })
        .await
        .unwrap();
    }
    v.put(&access(), write("visible", 0, b"secret"), &c)
        .await
        .unwrap();
    let page = v.audits(&access(), "models", 0, &c).await.unwrap();
    assert_eq!(page.len(), 1);
    assert!(page[0].sequence > 1001);
}
#[tokio::test]
async fn recovery_corruption_and_unbounded_kdf_parameters_fail_closed() {
    let (v, _) = fixture().await;
    let f = AtomicBool::new(false);
    let c = control(&f);
    v.put(&access(), write("create", 0, b"secret"), &c)
        .await
        .unwrap();
    let pass = || SecretBytes::new(b"long-enough-disposable-passphrase".to_vec());
    let bundle = v
        .export_recovery(&access(), "models", pass(), &c)
        .await
        .unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&bundle).unwrap();
    value["memory_kib"] = serde_json::json!(u32::MAX);
    assert!(
        recovery::open_recovery(&serde_json::to_vec(&value).unwrap(), pass(), &c)
            .await
            .is_err()
    );
    let mut value: serde_json::Value = serde_json::from_slice(&bundle).unwrap();
    let byte = value["ciphertext"][25].as_u64().unwrap();
    value["ciphertext"][25] = serde_json::json!(byte ^ 1);
    assert!(
        recovery::open_recovery(&serde_json::to_vec(&value).unwrap(), pass(), &c)
            .await
            .is_err()
    );
}
