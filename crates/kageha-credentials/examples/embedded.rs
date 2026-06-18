//! Disposable example; a real host supplies a KeyProtector rather than an in-memory test key.
use kageha_contracts::execution::Control;
use kageha_credentials::{keys::SuppliedKey, repository::SqliteRepository, *};
use std::{
    collections::BTreeSet,
    sync::{atomic::AtomicBool, Arc},
};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let scope = CredentialRef {
        namespace: "example".into(),
        id: "model".into(),
    };
    let access = CredentialAccess {
        principal: "my-framework".into(),
        purpose: "model".into(),
        destination: "https://model.example".into(),
    };
    let strings = |v: &str| BTreeSet::from([v.to_owned()]);
    let grant = Grant {
        principal: access.principal.clone(),
        namespace: scope.namespace.clone(),
        ids: strings(&scope.id),
        purposes: strings(&access.purpose),
        destinations: strings(&access.destination),
        permissions: BTreeSet::from([Permission::Write, Permission::Resolve]),
    };
    let cancelled = AtomicBool::new(false);
    let control = Control {
        cancelled: &cancelled,
        deadline: None,
    };
    let vault = Vault::open(
        Arc::new(SqliteRepository::in_memory()?),
        Arc::new(SuppliedKey::new(
            "test-only",
            SecretBytes::new(crypto::random::<32>()?.to_vec()),
        )),
        "test-only",
        vec![grant],
        &control,
    )
    .await?;
    #[cfg(feature = "client")]
    let service = std::env::var_os("KAGEHA_CREDENTIAL_EXAMPLE_CONNECTION")
        .map(|p| -> kageha_contracts::Result<_> {
            let data = config::protected_input(std::path::Path::new(&p), 65536)?;
            let connection: config::Connection =
                serde_json::from_slice(data.expose()).map_err(|_| {
                    kageha_contracts::ServiceError::new(
                        kageha_contracts::ErrorCode::Invalid,
                        "Invalid example connection",
                    )
                })?;
            connection.connect()
        })
        .transpose()?;
    #[cfg(feature = "client")]
    let vault: &dyn CredentialStore = service
        .as_ref()
        .map(|c| c as &dyn CredentialStore)
        .unwrap_or(&vault);
    vault
        .put(
            &access,
            CredentialWrite {
                operation_id: "example-create".into(),
                reference: scope.clone(),
                expected_revision: 0,
                kind: "api_key".into(),
                expires_at_ms: None,
                secret: SecretBytes::new(b"disposable-example-value".to_vec()),
            },
            &control,
        )
        .await?;
    let lease = vault.resolve(&access, &scope, &control).await?;
    vault.validate_lease(&access, &lease, &control).await?;
    // Supply lease.secret.expose() to a trusted adapter; never print its value.
    println!(
        "Credential resolved at revision {}",
        lease.metadata.revision
    );
    Ok(())
}
