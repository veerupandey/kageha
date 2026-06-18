//! Thin credential host. Configuration holds references/paths and grants, never credential values.
use kageha_contracts::{execution::Control, ErrorCode};
use kageha_credentials::{keys::FileKeys, repository::SqliteRepository, *};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    sync::{atomic::AtomicBool, Arc},
};
fn err(code: ErrorCode, message: &str) -> kageha_contracts::ServiceError {
    kageha_contracts::ServiceError::new(code, message)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    vault: PathBuf,
    key_id: String,
    #[serde(default)]
    key_files: BTreeMap<String, PathBuf>,
    keychain_service: Option<String>,
    #[cfg(feature = "client")]
    registry: Option<RegistryConfig>,
    grants: Vec<Grant>,
    #[serde(default)]
    clients: Vec<service::ClientIdentity>,
    #[serde(default)]
    service_options: service::ServiceOptions,
    socket: Option<PathBuf>,
    bind: Option<String>,
    server_cert: Option<PathBuf>,
    server_key: Option<PathBuf>,
    client_ca: Option<PathBuf>,
}
#[cfg(feature = "client")]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryConfig {
    journal: PathBuf,
    embedded_id: String,
    device_id: String,
    admin_principal: String,
    admin_purpose: String,
    #[serde(default)]
    backends: Vec<config::RemoteBackend>,
}
fn read(path: &Path, limit: usize, private: bool) -> kageha_contracts::Result<Vec<u8>> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| err(ErrorCode::Unavailable, "Protected input unavailable"))?;
    let meta = file
        .metadata()
        .map_err(|_| err(ErrorCode::Unavailable, "Protected input unavailable"))?;
    if !meta.is_file()
        || meta.len() > limit as u64
        || private && (meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0)
    {
        return Err(err(
            ErrorCode::Unauthorized,
            "Protected input must be a bounded private regular file owned by this user",
        ));
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| err(ErrorCode::Unavailable, "Protected input read failed"))?;
    if bytes.len() > limit {
        return Err(err(ErrorCode::Invalid, "Protected input exceeds limit"));
    }
    Ok(bytes)
}
fn output(path: &Path, bytes: &[u8]) -> kageha_contracts::Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| err(ErrorCode::Conflict, "Output must be a new private file"))?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| err(ErrorCode::Unavailable, "Protected output write failed"))?;
    Ok(())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Import {
    operation_id: String,
    reference: CredentialRef,
    kind: String,
    expected_revision: u64,
    secret_base64: String,
    expires_at_ms: Option<u64>,
}
impl Drop for Import {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.secret_base64.zeroize();
    }
}
async fn run() -> kageha_contracts::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("help");
    if cmd == "help" {
        println!("kageha-credentials <serve|status|migrate|rotate-key|export-recovery|restore> --config PATH [--input PATH] [--output PATH] [--namespace NAME] [--passphrase-file PATH] [--next-key-id ID]");
        return Ok(());
    }
    let arg = |name: &str| -> kageha_contracts::Result<String> {
        args.iter()
            .position(|v| v == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
            .ok_or_else(|| {
                err(
                    ErrorCode::Invalid,
                    "Required credential command option missing",
                )
            })
    };
    let cfg: Config =
        serde_json::from_slice(&read(Path::new(&arg("--config")?), 1024 * 1024, true)?)
            .map_err(|_| err(ErrorCode::Invalid, "Invalid credential host configuration"))?;
    if !matches!(cmd, "serve" | "migrate" | "restore") && !cfg.vault.is_file() {
        return Err(err(
            ErrorCode::NotFound,
            "Credential vault is not initialized",
        ));
    }
    let repository = Arc::new(SqliteRepository::open(&cfg.vault)?);
    let existing = repository.header().await?;
    if existing.is_none() && !matches!(cmd, "serve" | "migrate" | "restore") {
        return Err(err(
            ErrorCode::NotFound,
            "Credential vault is not initialized",
        ));
    }
    let keys: Arc<dyn KeyProtector> = if !cfg.key_files.is_empty() {
        Arc::new(FileKeys(cfg.key_files))
    } else {
        #[cfg(target_os = "macos")]
        {
            let keychain = keys::MacKeychain {
                service: cfg
                    .keychain_service
                    .unwrap_or_else(|| "kageha.credentials".into()),
            };
            if existing.is_none() {
                keychain.provision(&cfg.key_id).await?;
            }
            Arc::new(keychain)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = cfg.keychain_service;
            return Err(err(
                ErrorCode::Unavailable,
                "Configure an explicitly mounted credential key file",
            ));
        }
    };
    let cancelled = AtomicBool::new(false);
    let control = Control {
        cancelled: &cancelled,
        deadline: None,
    };
    let vault =
        Arc::new(Vault::open(repository.clone(), keys, &cfg.key_id, cfg.grants, &control).await?);
    let access = CredentialAccess {
        principal: "operator".into(),
        purpose: "admin".into(),
        destination: "local".into(),
    };
    match cmd {
        "status" => {
            let h = repository.header().await?.unwrap();
            println!(
                "{}",
                serde_json::json!({"unlocked":true,"vault_id":h.vault_id,"generation":h.generation,"key_id":h.key_id,"admission":h.admission,"authentication_generation":h.authentication_generation})
            );
        }
        "serve" => {
            #[cfg(feature = "client")]
            let router = if let Some(configuration) = cfg.registry {
                let authority = CredentialAccess {
                    principal: configuration.admin_principal.clone(),
                    purpose: configuration.admin_purpose,
                    destination: configuration.embedded_id.clone(),
                };
                let mut backends = vec![registry::Backend {
                    id: configuration.embedded_id.clone(),
                    device_id: configuration.device_id,
                    store: vault,
                }];
                for backend in configuration.backends {
                    backends.push(backend.connect(&configuration.admin_principal)?);
                }
                let registry = registry::Registry::open(
                    &configuration.journal,
                    &configuration.embedded_id,
                    backends,
                    &authority,
                    &control,
                )
                .await?;
                service::registry_router_with_options(registry, cfg.clients, cfg.service_options)?
            } else {
                service::router_with_options(vault, cfg.clients, cfg.service_options)?
            };
            #[cfg(not(feature = "client"))]
            let router = service::router(vault, cfg.clients)?;
            if let Some(socket) = cfg.socket {
                if cfg.bind.is_some() {
                    return Err(err(
                        ErrorCode::Invalid,
                        "Select either local IPC or hosted TLS",
                    ));
                }
                service::serve_unix_until(&socket, router, shutdown()).await?;
            } else {
                let bind = cfg.bind.ok_or_else(|| {
                    err(
                        ErrorCode::Invalid,
                        "Credential socket or TLS listener required",
                    )
                })?;
                let certs = read(
                    cfg.server_cert
                        .as_deref()
                        .ok_or_else(|| err(ErrorCode::Invalid, "TLS certificate required"))?,
                    1024 * 1024,
                    false,
                )?;
                let key = SecretBytes::new(read(
                    cfg.server_key
                        .as_deref()
                        .ok_or_else(|| err(ErrorCode::Invalid, "TLS key required"))?,
                    1024 * 1024,
                    true,
                )?);
                let ca = read(
                    cfg.client_ca
                        .as_deref()
                        .ok_or_else(|| err(ErrorCode::Invalid, "Client trust roots required"))?,
                    1024 * 1024,
                    false,
                )?;
                let certificates = rustls_pemfile::certs(&mut certs.as_slice())
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|_| err(ErrorCode::Invalid, "Invalid TLS certificate"))?;
                let key = rustls_pemfile::private_key(&mut key.expose())
                    .map_err(|_| err(ErrorCode::Invalid, "Invalid TLS key"))?
                    .ok_or_else(|| err(ErrorCode::Invalid, "TLS key missing"))?;
                let mut roots = tokio_rustls::rustls::RootCertStore::empty();
                for cert in rustls_pemfile::certs(&mut ca.as_slice()) {
                    roots
                        .add(
                            cert.map_err(|_| err(ErrorCode::Invalid, "Invalid client trust root"))?,
                        )
                        .map_err(|_| err(ErrorCode::Invalid, "Invalid client trust root"))?;
                }
                service::serve_tls_until(&bind, router, certificates, key, roots, shutdown())
                    .await?;
            }
        }
        "migrate" => {
            let bytes = SecretBytes::new(read(Path::new(&arg("--input")?), 8 * 1024 * 1024, true)?);
            let imports: Vec<Import> = serde_json::from_slice(bytes.expose())
                .map_err(|_| err(ErrorCode::Invalid, "Invalid credential import"))?;
            if imports.len() > 1000 {
                return Err(err(ErrorCode::Invalid, "Credential import exceeds limit"));
            }
            let mut count = 0;
            for row in imports {
                let secret = SecretBytes::new(
                    data_encoding::BASE64
                        .decode(row.secret_base64.as_bytes())
                        .map_err(|_| {
                            err(ErrorCode::Invalid, "Invalid credential import encoding")
                        })?,
                );
                let op = vault
                    .put(
                        &access,
                        CredentialWrite {
                            operation_id: row.operation_id.clone(),
                            reference: row.reference.clone(),
                            expected_revision: row.expected_revision,
                            kind: row.kind.clone(),
                            expires_at_ms: row.expires_at_ms,
                            secret,
                        },
                        &control,
                    )
                    .await?;
                vault
                    .metadata(&access, &op.metadata.reference, &control)
                    .await?;
                count += 1;
            }
            println!(
                "{}",
                serde_json::json!({"verified_imports":count,"source_removed":false})
            );
        }
        "rotate-key" => {
            println!(
                "{}",
                serde_json::json!({"generation":vault.rotate_key(&access,"system",&arg("--next-key-id")?,&control).await?})
            );
        }
        "revoke" => {
            let reference = CredentialRef {
                namespace: arg("--namespace")?,
                id: arg("--id")?,
            };
            let revision = arg("--revision")?
                .parse::<u64>()
                .map_err(|_| err(ErrorCode::Invalid, "Invalid credential revision"))?;
            let receipt = vault
                .revoke(
                    &access,
                    &reference,
                    revision,
                    &arg("--operation-id")?,
                    false,
                    &control,
                )
                .await?;
            println!(
                "{}",
                serde_json::json!({"revision":receipt.metadata.revision,"state":receipt.metadata.state})
            );
        }
        "export-recovery" => {
            let passphrase =
                SecretBytes::new(read(Path::new(&arg("--passphrase-file")?), 1024, true)?);
            let bundle = vault
                .export_recovery(&access, &arg("--namespace")?, passphrase, &control)
                .await?;
            output(Path::new(&arg("--output")?), &bundle)?;
            println!("Encrypted recovery export written");
        }
        "restore" => {
            let bytes = read(Path::new(&arg("--input")?), 64 * 1024 * 1024, true)?;
            let passphrase =
                SecretBytes::new(read(Path::new(&arg("--passphrase-file")?), 1024, true)?);
            let generation = vault
                .restore_recovery(&access, &bytes, passphrase, &control)
                .await?;
            println!(
                "{}",
                serde_json::json!({"restored":true,"authentication_generation":generation,"fresh_client_authority_required":true})
            );
        }
        _ => return Err(err(ErrorCode::Invalid, "Unknown credential command")),
    }
    Ok(())
}
#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{}", error.message);
        std::process::exit(1);
    }
}

async fn shutdown() {
    #[cfg(unix)]
    {
        if let Ok(mut term) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}}
        } else {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
