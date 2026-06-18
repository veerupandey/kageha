#![cfg(all(feature = "service", feature = "client"))]
use kageha_contracts::execution::Control;
use kageha_credentials::{
    keys::SuppliedKey,
    registry::{Backend, Registry},
    repository::SqliteRepository,
    *,
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    sync::{atomic::AtomicBool, Arc},
};
fn access(d: &str) -> CredentialAccess {
    CredentialAccess {
        principal: "adapter".into(),
        purpose: "test".into(),
        destination: d.into(),
    }
}
async fn vault(c: &Control<'_>) -> Arc<Vault> {
    Arc::new(
        Vault::open(
            Arc::new(SqliteRepository::in_memory().unwrap()),
            Arc::new(SuppliedKey::new("key", SecretBytes::new(vec![7; 32]))),
            "key",
            ["system", "models"]
                .into_iter()
                .map(|n| Grant {
                    principal: "adapter".into(),
                    namespace: n.into(),
                    ids: BTreeSet::from(["*".into()]),
                    purposes: BTreeSet::from(["test".into()]),
                    destinations: ["embedded", "remote", "provider"]
                        .into_iter()
                        .map(str::to_owned)
                        .collect(),
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
#[tokio::test]
async fn same_port_switches_embedded_to_real_ipc_without_restarting_consumers() {
    use std::os::unix::fs::PermissionsExt;
    let f = AtomicBool::new(false);
    let c = Control {
        cancelled: &f,
        deadline: None,
    };
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = dir.path().join("remote.sock");
    let token = b"test-management-token-1234567890123456789";
    let target = vault(&c).await;
    let router = service::router(
        target,
        vec![service::ClientIdentity {
            principal: "adapter".into(),
            authentication_generation: 1,
            management: true,
            uid: Some(unsafe { libc::geteuid() }),
            local_token_sha256: Some(format!("{:x}", Sha256::digest(token))),
            certificate_sha256: None,
        }],
    )
    .unwrap();
    let path = socket.clone();
    let server = tokio::spawn(async move { service::serve_unix(&path, router).await });
    for _ in 0..100 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let remote = Arc::new(client::Client::unix(&socket, SecretBytes::new(token.to_vec())).unwrap());
    let registry = Registry::in_memory(
        "embedded",
        vec![
            Backend {
                id: "embedded".into(),
                device_id: "local".into(),
                store: vault(&c).await,
            },
            Backend {
                id: "remote".into(),
                device_id: "local".into(),
                store: remote,
            },
        ],
        &access("embedded"),
        &c,
    )
    .await
    .unwrap();
    let front = dir.path().join("registry.sock");
    let path = front.clone();
    let router = service::router_with_registry(
        registry,
        vec![service::ClientIdentity {
            principal: "adapter".into(),
            authentication_generation: 1,
            management: true,
            uid: Some(unsafe { libc::geteuid() }),
            local_token_sha256: Some(format!("{:x}", Sha256::digest(token))),
            certificate_sha256: None,
        }],
    )
    .unwrap();
    let host = tokio::spawn(async move { service::serve_unix(&path, router).await });
    for _ in 0..100 {
        if front.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let client = client::Client::unix(&front, SecretBytes::new(token.to_vec())).unwrap();
    let r = CredentialRef {
        namespace: "models".into(),
        id: "primary".into(),
    };
    client
        .put(
            &access("provider"),
            CredentialWrite {
                operation_id: "create".into(),
                reference: r.clone(),
                expected_revision: 0,
                kind: "api_key".into(),
                expires_at_ms: None,
                secret: SecretBytes::new(b"IPC-SWITCH-CANARY".to_vec()),
            },
            &c,
        )
        .await
        .unwrap();
    client
        .prepare(&access("remote"), "remote", "switch", &c)
        .await
        .unwrap();
    let status = client
        .replacement(
            &access("remote"),
            serde_json::json!({"action":"resume","operation_id":"switch"}),
            &c,
        )
        .await
        .unwrap();
    assert_eq!(status["stage"], "verified", "{status}");
    assert_eq!(
        client
            .resolve(&access("provider"), &r, &c)
            .await
            .unwrap()
            .secret
            .expose(),
        b"IPC-SWITCH-CANARY"
    );
    client
        .revoke(&access("provider"), &r, 1, "revoke", false, &c)
        .await
        .unwrap();
    let status=client.replacement(&access("embedded"),serde_json::json!({"action":"rollback","operation_id":"rollback","previous_operation_id":"switch","decision":{"id":"approved-return","destination":"embedded"}}),&c).await.unwrap();
    assert_eq!(status["stage"], "verified", "{status}");
    assert!(client.resolve(&access("provider"), &r, &c).await.is_err());
    host.abort();
    server.abort();
}
