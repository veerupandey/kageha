use kageha_contracts::execution::Control;
use kageha_credentials::{
    keys::SuppliedKey,
    registry::{Backend, Decision, Registry},
    repository::SqliteRepository,
    *,
};
use std::{
    collections::BTreeSet,
    sync::{atomic::AtomicBool, Arc},
};
fn set(v: &[&str]) -> BTreeSet<String> {
    v.iter().map(|s| s.to_string()).collect()
}
fn access(destination: &str) -> CredentialAccess {
    CredentialAccess {
        principal: "adapter".into(),
        purpose: "test".into(),
        destination: destination.into(),
    }
}
fn reference(id: &str) -> CredentialRef {
    CredentialRef {
        namespace: "models".into(),
        id: id.into(),
    }
}
fn write(id: &str, revision: u64, value: &[u8], op: &str) -> CredentialWrite {
    CredentialWrite {
        operation_id: op.into(),
        reference: reference(id),
        expected_revision: revision,
        kind: "api_key".into(),
        expires_at_ms: None,
        secret: SecretBytes::new(value.to_vec()),
    }
}
async fn vault(c: &Control<'_>) -> Arc<Vault> {
    Arc::new(
        Vault::open(
            Arc::new(SqliteRepository::in_memory().unwrap()),
            Arc::new(SuppliedKey::new("key", SecretBytes::new(vec![7; 32]))),
            "key",
            ["models", "system"]
                .into_iter()
                .map(|n| Grant {
                    principal: "adapter".into(),
                    namespace: n.into(),
                    ids: set(&["*"]),
                    purposes: set(&["test"]),
                    destinations: set(&["a", "b", "provider"]),
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
                .collect(),
            c,
        )
        .await
        .unwrap(),
    )
}
fn backends(a: Arc<Vault>, b: Arc<Vault>) -> Vec<Backend> {
    vec![
        Backend {
            id: "a".into(),
            device_id: "device".into(),
            store: a,
        },
        Backend {
            id: "b".into(),
            device_id: "device".into(),
            store: b,
        },
    ]
}
fn decision(dest: &str) -> Decision {
    Decision {
        id: format!("operator-approved-{dest}"),
        destination: dest.into(),
        allow_browser_transfer: false,
    }
}
#[tokio::test]
async fn switch_and_rollback_catch_up_writes_tombstones_and_receipts() {
    let f = AtomicBool::new(false);
    let c = Control {
        cancelled: &f,
        deadline: None,
    };
    let a = vault(&c).await;
    let b = vault(&c).await;
    let registry = Registry::in_memory("a", backends(a.clone(), b.clone()), &access("a"), &c)
        .await
        .unwrap();
    registry
        .put(
            &access("provider"),
            write("key", 0, b"first-secret", "create"),
            &c,
        )
        .await
        .unwrap();
    registry
        .put(
            &access("provider"),
            write("deleted", 0, b"must-disappear", "create-deleted"),
            &c,
        )
        .await
        .unwrap();
    registry
        .revoke(
            &access("provider"),
            &reference("deleted"),
            1,
            "delete",
            true,
            &c,
        )
        .await
        .unwrap();
    let old = registry
        .resolve(&access("provider"), &reference("key"), &c)
        .await
        .unwrap();
    registry
        .request(&access("b"), decision("b"), "switch", &c)
        .await
        .unwrap();
    let status = registry.resume(&access("b"), "switch", &c).await.unwrap();
    assert_eq!(
        status.stage,
        MigrationStage::Verified,
        "{:?}",
        status.blocker
    );
    assert_eq!(registry.binding().await.unwrap().backend, "b");
    assert!(registry
        .validate_lease(&access("provider"), &old, &c)
        .await
        .is_err());
    assert!(a
        .put(
            &access("provider"),
            write("key", 1, b"old-writer", "late"),
            &c
        )
        .await
        .is_err());
    assert_eq!(
        registry
            .put(
                &access("provider"),
                write("key", 0, b"first-secret", "create"),
                &c
            )
            .await
            .unwrap()
            .metadata
            .revision,
        1
    );
    registry
        .put(
            &access("provider"),
            write("key", 1, b"after-switch", "update"),
            &c,
        )
        .await
        .unwrap();
    registry
        .put(
            &access("provider"),
            write("new", 0, b"new-after-switch", "new"),
            &c,
        )
        .await
        .unwrap();
    registry
        .revoke(
            &access("provider"),
            &reference("key"),
            2,
            "revoke",
            false,
            &c,
        )
        .await
        .unwrap();
    let result = registry
        .rollback(&access("a"), "switch", decision("a"), "rollback", &c)
        .await
        .unwrap();
    assert_eq!(
        result.stage,
        MigrationStage::Verified,
        "{:?}",
        result.blocker
    );
    assert!(registry
        .resolve(&access("provider"), &reference("key"), &c)
        .await
        .is_err());
    assert!(registry
        .resolve(&access("provider"), &reference("deleted"), &c)
        .await
        .is_err());
    assert_eq!(
        registry
            .resolve(&access("provider"), &reference("new"), &c)
            .await
            .unwrap()
            .secret
            .expose(),
        b"new-after-switch"
    );
    assert!(registry
        .operation(&access("provider"), &reference("key"), "revoke", &c)
        .await
        .unwrap()
        .is_some());
    assert!(b
        .resolve(&access("provider"), &reference("new"), &c)
        .await
        .is_err());
}
#[tokio::test]
async fn replacement_restart_and_refresh_blocker_are_recoverable() {
    use std::os::unix::fs::PermissionsExt;
    let f = AtomicBool::new(false);
    let c = Control {
        cancelled: &f,
        deadline: None,
    };
    let a = vault(&c).await;
    let b = vault(&c).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = dir.path().join("registry.sqlite");
    let registry = Registry::open(&path, "a", backends(a.clone(), b.clone()), &access("a"), &c)
        .await
        .unwrap();
    let mut w = write("oauth", 0, b"grant", "grant");
    w.kind = "oauth_grant".into();
    registry.put(&access("provider"), w, &c).await.unwrap();
    let ticket = registry
        .begin_refresh(&access("provider"), &reference("oauth"), 1, "begin", &c)
        .await
        .unwrap();
    registry
        .request(&access("b"), decision("b"), "switch", &c)
        .await
        .unwrap();
    let status = registry.resume(&access("b"), "switch", &c).await.unwrap();
    assert_eq!(status.stage, MigrationStage::Blocked);
    assert_eq!(
        a.backend_state(&access("a"), &c).await.unwrap().admission,
        VaultAdmission::Active
    );
    // Disconnection resolves the outstanding mutation; no refresh exchange is replayed.
    registry
        .revoke(
            &access("provider"),
            &reference("oauth"),
            ticket.revision,
            "disconnect",
            false,
            &c,
        )
        .await
        .unwrap();
    drop(registry);
    let restarted = Registry::open(&path, "a", backends(a, b), &access("a"), &c)
        .await
        .unwrap();
    let status = restarted.resume(&access("b"), "switch", &c).await.unwrap();
    assert_eq!(
        status.stage,
        MigrationStage::Verified,
        "{:?}",
        status.blocker
    );
    assert!(restarted
        .resolve(&access("provider"), &reference("oauth"), &c)
        .await
        .is_err());
}
#[tokio::test]
async fn a_failed_prepare_retains_source_and_requires_explicit_destination() {
    let f = AtomicBool::new(false);
    let c = Control {
        cancelled: &f,
        deadline: None,
    };
    let a = vault(&c).await;
    let b = vault(&c).await;
    b.put(
        &access("provider"),
        write("existing", 0, b"other-owner-data", "other"),
        &c,
    )
    .await
    .unwrap();
    let registry = Registry::in_memory("a", backends(a, b), &access("a"), &c)
        .await
        .unwrap();
    registry
        .put(
            &access("provider"),
            write("source", 0, b"still-working", "source"),
            &c,
        )
        .await
        .unwrap();
    assert!(registry
        .request(&access("b"), decision("b"), "must-not-overwrite", &c)
        .await
        .is_err());
    assert!(registry
        .request(&access("b"), decision("a"), "wrong-destination", &c)
        .await
        .is_err());
    assert_eq!(
        registry
            .resolve(&access("provider"), &reference("source"), &c)
            .await
            .unwrap()
            .secret
            .expose(),
        b"still-working"
    );
}
#[tokio::test]
async fn cancelled_precommit_replacement_reopens_only_its_source() {
    let f = AtomicBool::new(false);
    let c = Control {
        cancelled: &f,
        deadline: None,
    };
    let a = vault(&c).await;
    let b = vault(&c).await;
    let registry = Registry::in_memory("a", backends(a.clone(), b), &access("a"), &c)
        .await
        .unwrap();
    registry
        .put(
            &access("provider"),
            write("source", 0, b"still-live", "source"),
            &c,
        )
        .await
        .unwrap();
    registry
        .request(&access("b"), decision("b"), "cancel-me", &c)
        .await
        .unwrap();
    let state = a.backend_state(&access("a"), &c).await.unwrap();
    a.freeze(&access("b"), state.generation, "cancel-me", &c)
        .await
        .unwrap();
    assert!(registry
        .resolve(&access("provider"), &reference("source"), &c)
        .await
        .is_err());
    assert_eq!(
        registry
            .cancel(&access("b"), "cancel-me", &c)
            .await
            .unwrap()
            .stage,
        MigrationStage::Cancelled
    );
    assert_eq!(
        registry
            .resolve(&access("provider"), &reference("source"), &c)
            .await
            .unwrap()
            .secret
            .expose(),
        b"still-live"
    );
    registry
        .request(&access("b"), decision("b"), "next", &c)
        .await
        .unwrap();
    assert_eq!(
        registry
            .resume(&access("b"), "next", &c)
            .await
            .unwrap()
            .stage,
        MigrationStage::Verified
    );
    assert!(registry.cancel(&access("b"), "next", &c).await.is_err());
}
#[tokio::test]
async fn missing_lifecycle_capabilities_are_rejected_before_cutover() {
    let f = AtomicBool::new(false);
    let c = Control {
        cancelled: &f,
        deadline: None,
    };
    let a = vault(&c).await;
    let b = vault(&c).await;
    let nested = Registry::in_memory(
        "b",
        vec![Backend {
            id: "b".into(),
            device_id: "device".into(),
            store: b,
        }],
        &access("b"),
        &c,
    )
    .await
    .unwrap();
    let outer = Registry::in_memory(
        "a",
        vec![
            Backend {
                id: "a".into(),
                device_id: "device".into(),
                store: a,
            },
            Backend {
                id: "b".into(),
                device_id: "device".into(),
                store: nested,
            },
        ],
        &access("a"),
        &c,
    )
    .await
    .unwrap();
    assert!(outer
        .request(&access("b"), decision("b"), "unsupported", &c)
        .await
        .is_err());
    assert_eq!(outer.binding().await.unwrap().backend, "a");
}
#[tokio::test]
async fn browser_password_transfer_requires_explicit_destination_decision() {
    let f = AtomicBool::new(false);
    let c = Control {
        cancelled: &f,
        deadline: None,
    };
    let a = vault(&c).await;
    let b = vault(&c).await;
    let mut w = write("browser", 0, b"DISPOSABLE-BROWSER-PASSWORD", "browser");
    w.kind = "browser_password".into();
    a.put(&access("provider"), w, &c).await.unwrap();
    let registry = Registry::in_memory(
        "a",
        vec![
            Backend {
                id: "a".into(),
                device_id: "user-device".into(),
                store: a.clone(),
            },
            Backend {
                id: "b".into(),
                device_id: "hosted-device".into(),
                store: b,
            },
        ],
        &access("a"),
        &c,
    )
    .await
    .unwrap();
    registry
        .request(&access("b"), decision("b"), "not-approved", &c)
        .await
        .unwrap();
    let blocked = registry
        .resume(&access("b"), "not-approved", &c)
        .await
        .unwrap();
    assert_eq!(blocked.stage, MigrationStage::Blocked);
    assert_eq!(
        a.backend_state(&access("a"), &c).await.unwrap().admission,
        VaultAdmission::Active
    );
    registry
        .cancel(&access("b"), "not-approved", &c)
        .await
        .unwrap();
    let mut approved = decision("b");
    approved.allow_browser_transfer = true;
    registry
        .request(&access("b"), approved, "explicit-browser-approval", &c)
        .await
        .unwrap();
    assert_eq!(
        registry
            .resume(&access("b"), "explicit-browser-approval", &c)
            .await
            .unwrap()
            .stage,
        MigrationStage::Verified
    );
}
