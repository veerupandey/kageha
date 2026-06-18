//! Explicit read-only sources. There is no fallback chain from a managed vault.
use crate::{
    crypto::{self, check, err},
    *,
};
use hmac::{Hmac, Mac};
use kageha_contracts::{execution::Control, ErrorCode, Result};
use sha2::Sha256;
use std::{collections::BTreeMap, io::Read, path::PathBuf, sync::Mutex};
pub enum Source {
    Environment(String),
    MountedFile(PathBuf),
}
pub struct SourceBinding {
    pub reference: CredentialRef,
    pub access: CredentialAccess,
    pub source: Source,
}
pub struct ReadOnlySources {
    bindings: Vec<SourceBinding>,
    key: SecretBytes,
    versions: Mutex<BTreeMap<CredentialRef, (Vec<u8>, u64)>>,
    generation: u64,
}
impl ReadOnlySources {
    pub fn new(bindings: Vec<SourceBinding>) -> Result<Self> {
        let mut refs = std::collections::BTreeSet::new();
        for binding in &bindings {
            binding.reference.validate()?;
            if !refs.insert(binding.reference.clone())
                || [
                    &binding.access.principal,
                    &binding.access.purpose,
                    &binding.access.destination,
                ]
                .iter()
                .any(|s| s.is_empty())
            {
                return Err(err(
                    ErrorCode::Invalid,
                    "Explicit credential sources require unique references and scopes",
                ));
            }
        }
        Ok(Self {
            bindings,
            key: SecretBytes::new(crypto::random::<32>()?.to_vec()),
            versions: Mutex::new(BTreeMap::new()),
            generation: u64::from_le_bytes(crypto::random::<8>()?),
        })
    }
    fn read(source: &Source) -> Result<SecretBytes> {
        match source {
            Source::Environment(name) => {
                let value = std::env::var(name).map_err(|_| {
                    err(
                        ErrorCode::Unavailable,
                        "Explicit credential environment source unavailable",
                    )
                })?;
                if value.len() > 65536 {
                    return Err(err(ErrorCode::Invalid, "Credential source exceeds limit"));
                }
                Ok(SecretBytes::new(value.into_bytes()))
            }
            Source::MountedFile(path) => {
                use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
                let file = std::fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                    .open(path)
                    .map_err(|_| {
                        err(
                            ErrorCode::Unavailable,
                            "Explicit mounted credential unavailable",
                        )
                    })?;
                let meta = file.metadata().map_err(|_| {
                    err(
                        ErrorCode::Unavailable,
                        "Mounted credential metadata unavailable",
                    )
                })?;
                if !meta.is_file()
                    || meta.uid() != unsafe { libc::geteuid() }
                    || meta.mode() & 0o077 != 0
                    || meta.len() > 65536
                {
                    return Err(err(
                        ErrorCode::Unauthorized,
                        "Mounted credential must be a private bounded regular file",
                    ));
                }
                let mut bytes = zeroize::Zeroizing::new(Vec::new());
                file.take(65537)
                    .read_to_end(&mut bytes)
                    .map_err(|_| err(ErrorCode::Unavailable, "Mounted credential read failed"))?;
                if bytes.len() > 65536 {
                    return Err(err(ErrorCode::Invalid, "Credential source exceeds limit"));
                }
                Ok(SecretBytes::new(std::mem::take(&mut *bytes)))
            }
        }
    }
}
#[async_trait::async_trait]
impl CredentialResolver for ReadOnlySources {
    async fn resolve(
        &self,
        a: &CredentialAccess,
        r: &CredentialRef,
        c: &Control<'_>,
    ) -> Result<SecretLease> {
        check(c)?;
        let binding = self
            .bindings
            .iter()
            .find(|b| {
                b.reference == *r
                    && b.access.principal == a.principal
                    && b.access.purpose == a.purpose
                    && b.access.destination == a.destination
            })
            .ok_or_else(|| err(ErrorCode::Unauthorized, "Credential source access denied"))?;
        let secret = Self::read(&binding.source)?;
        let mut mac = Hmac::<Sha256>::new_from_slice(self.key.expose())
            .map_err(|_| err(ErrorCode::Invalid, "Credential source detector unavailable"))?;
        mac.update(secret.expose());
        let digest = mac.finalize().into_bytes().to_vec();
        let mut versions = self.versions.lock().map_err(|_| {
            err(
                ErrorCode::Unavailable,
                "Credential source state unavailable",
            )
        })?;
        let row = versions.entry(r.clone()).or_insert((digest.clone(), 1));
        if row.0 != digest {
            row.0 = digest;
            row.1 += 1;
        }
        check(c)?;
        Ok(SecretLease {
            secret,
            metadata: CredentialMetadata {
                reference: r.clone(),
                kind: "explicit_read_only_source".into(),
                revision: row.1,
                authority_generation: self.generation,
                expires_at_ms: None,
                state: CredentialState::Active,
                refreshable: false,
            },
            valid_until_ms: (crypto::now_ms() + 60000).min(
                c.deadline
                    .map(|d| {
                        crypto::now_ms()
                            + d.saturating_duration_since(std::time::Instant::now())
                                .as_millis() as u64
                    })
                    .unwrap_or(u64::MAX),
            ),
        })
    }
    async fn validate_lease(
        &self,
        a: &CredentialAccess,
        l: &SecretLease,
        c: &Control<'_>,
    ) -> Result<()> {
        let current = self.resolve(a, &l.metadata.reference, c).await?;
        if l.valid_until_ms <= crypto::now_ms()
            || l.valid_until_ms > crypto::now_ms() + 60000
            || l.metadata.revision != current.metadata.revision
            || l.metadata.authority_generation != current.metadata.authority_generation
        {
            return Err(err(
                ErrorCode::StaleGeneration,
                "Credential source changed before dispatch",
            ));
        }
        Ok(())
    }
}
