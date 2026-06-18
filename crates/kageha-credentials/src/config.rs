//! Explicit operator-owned service connections, reusable by any component.
use crate::{
    client::{Client, RoutedClients},
    crypto::err,
    *,
};
use kageha_contracts::{ErrorCode, Result};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};
#[derive(Clone, Deserialize)]
#[serde(tag = "transport", rename_all = "snake_case", deny_unknown_fields)]
pub enum Connection {
    Unix {
        socket: PathBuf,
        token_file: PathBuf,
    },
    Https {
        endpoint: String,
        identity_file: PathBuf,
        ca_file: PathBuf,
    },
}
pub fn protected_input(path: &Path, limit: usize) -> Result<SecretBytes> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| {
            err(
                ErrorCode::Unavailable,
                "Protected credential input unavailable",
            )
        })?;
    let meta = file.metadata().map_err(|_| {
        err(
            ErrorCode::Unavailable,
            "Protected input metadata unavailable",
        )
    })?;
    if !meta.is_file()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o077 != 0
        || meta.len() > limit as u64
    {
        return Err(err(
            ErrorCode::Unauthorized,
            "Credential input must be an owner-only bounded regular file",
        ));
    }
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| {
            err(
                ErrorCode::Unavailable,
                "Protected credential input unavailable",
            )
        })?;
    if bytes.len() > limit {
        return Err(err(ErrorCode::Invalid, "Protected input exceeds limit"));
    }
    Ok(SecretBytes::new(std::mem::take(&mut *bytes)))
}
impl Connection {
    pub fn connect(&self) -> Result<Client> {
        match self {
            Self::Unix { socket, token_file } => {
                let bytes = protected_input(token_file, 4096)?;
                let token = std::str::from_utf8(bytes.expose())
                    .map_err(|_| err(ErrorCode::Invalid, "Invalid credential service token"))?
                    .trim();
                if token.len() < 32 || token.chars().any(char::is_control) {
                    return Err(err(
                        ErrorCode::Invalid,
                        "Credential service token must contain at least 32 printable characters",
                    ));
                }
                Client::unix(socket, SecretBytes::new(token.as_bytes().to_vec()))
            }
            Self::Https {
                endpoint,
                identity_file,
                ca_file,
            } => {
                let identity = protected_input(identity_file, 1024 * 1024)?;
                let ca = protected_input(ca_file, 1024 * 1024)?;
                Client::hosted(endpoint, identity.expose(), ca.expose())
            }
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteBackend {
    pub id: String,
    pub device_id: String,
    pub workloads: BTreeMap<String, Connection>,
}
impl RemoteBackend {
    pub fn connect(&self, management_principal: &str) -> Result<registry::Backend> {
        if !self.workloads.contains_key(management_principal) {
            return Err(err(
                ErrorCode::Invalid,
                "Credential backend requires a scoped management identity",
            ));
        }
        let clients = self
            .workloads
            .iter()
            .map(|(principal, connection)| Ok((principal.clone(), Arc::new(connection.connect()?))))
            .collect::<Result<_>>()?;
        Ok(registry::Backend {
            id: self.id.clone(),
            device_id: self.device_id.clone(),
            store: Arc::new(RoutedClients {
                workloads: clients,
                management_principal: management_principal.into(),
            }),
        })
    }
}
