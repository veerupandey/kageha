//! Explicit recovery exports. Passphrases are protected input, never command-line arguments.
use crate::{
    crypto::{self, check, err, json, parse},
    *,
};
use kageha_contracts::{execution::Control, ErrorCode, Result};
use serde::{Deserialize, Serialize};
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Bundle {
    version: u32,
    memory_kib: u32,
    iterations: u32,
    lanes: u32,
    salt: Vec<u8>,
    ciphertext: Vec<u8>,
}
fn derive(passphrase: &SecretBytes, salt: &[u8], m: u32, t: u32, p: u32) -> Result<SecretBytes> {
    if passphrase.expose().len() < 16
        || passphrase.expose().len() > 1024
        || salt.len() != 16
        || !(19456..=262144).contains(&m)
        || !(2..=6).contains(&t)
        || p != 1
    {
        return Err(err(
            ErrorCode::Invalid,
            "Invalid recovery passphrase or bounded KDF parameters",
        ));
    }
    let mut key = zeroize::Zeroizing::new(vec![0u8; 32]);
    let params = argon2::Params::new(m, t, p, Some(32))
        .map_err(|_| err(ErrorCode::Invalid, "Invalid recovery KDF parameters"))?;
    argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params)
        .hash_password_into(passphrase.expose(), salt, &mut key)
        .map_err(|_| err(ErrorCode::Unavailable, "Recovery key derivation failed"))?;
    Ok(SecretBytes::new(std::mem::take(&mut *key)))
}
impl Vault {
    pub async fn export_recovery(
        &self,
        access: &CredentialAccess,
        namespace: &str,
        passphrase: SecretBytes,
        control: &Control<'_>,
    ) -> Result<Vec<u8>> {
        self.authorize(
            access,
            &CredentialRef {
                namespace: namespace.into(),
                id: "vault".into(),
            },
            Permission::Export,
            control,
        )
        .await?;
        check(control)?;
        let (h, _, receipt_key) = self.material(control).await?;
        let snapshot = self.repository.snapshot_full().await?;
        if h.generation != snapshot.header.generation {
            return Err(err(ErrorCode::Conflict, "Credential snapshot changed"));
        }
        // Export is namespace-scoped, including tombstones. Other namespaces are never silently exported.
        let mut values = Vec::new();
        for row in snapshot
            .records
            .into_iter()
            .filter(|r| r.metadata.reference.namespace == namespace)
        {
            let secret = self.decrypt(&h, &row, control).await?;
            values.push((row.metadata, data_encoding::BASE64.encode(secret.expose())));
        }
        // Secret strings exist only in this private zeroizing serialization buffer, not Debug/API DTOs.
        let operations: Vec<_> = snapshot
            .operations
            .into_iter()
            .filter(|o| o.operation.metadata.reference.namespace == namespace)
            .collect();
        for o in &operations {
            Self::verify_operation(&receipt_key, o)?;
        }
        let audit: Vec<_> = snapshot
            .audit
            .into_iter()
            .filter(|a| {
                a.reference
                    .as_ref()
                    .is_some_and(|r| r.namespace == namespace)
            })
            .collect();
        let plaintext = SecretBytes::new(json(&(
            h.vault_id,
            h.generation,
            h.authentication_generation,
            data_encoding::BASE64.encode(receipt_key.expose()),
            &values,
            &operations,
            &audit,
        ))?);
        use zeroize::Zeroize;
        for (_, value) in &mut values {
            value.zeroize();
        }
        let salt = crypto::random::<16>()?.to_vec();
        let salt_copy = salt.clone();
        let key = tokio::task::spawn_blocking(move || derive(&passphrase, &salt_copy, 65536, 3, 1))
            .await
            .map_err(|_| err(ErrorCode::Unavailable, "Recovery worker unavailable"))??;
        check(control)?;
        let aad = json(&(2, 65536, 3, 1, &salt))?;
        let bundle = Bundle {
            version: 2,
            memory_kib: 65536,
            iterations: 3,
            lanes: 1,
            salt,
            ciphertext: crypto::seal(key.expose(), &aad, plaintext.expose())?,
        };
        self.repository
            .append_audit(CredentialAudit {
                sequence: 0,
                at_ms: crypto::now_ms(),
                principal: access.principal.clone(),
                action: "export_recovery".into(),
                reference: Some(CredentialRef {
                    namespace: namespace.into(),
                    id: "vault".into(),
                }),
                outcome: "encrypted".into(),
            })
            .await?;
        json(&bundle)
    }
}
pub struct RecoveryRecord {
    pub metadata: CredentialMetadata,
    pub secret: SecretBytes,
}
struct RecoveryImage {
    vault_id: String,
    generation: u64,
    authentication_generation: u64,
    receipt_key: SecretBytes,
    records: Vec<RecoveryRecord>,
    operations: Vec<StoredOperation>,
    audit: Vec<CredentialAudit>,
}
async fn decode_recovery(
    bytes: &[u8],
    passphrase: SecretBytes,
    control: &Control<'_>,
) -> Result<RecoveryImage> {
    check(control)?;
    if bytes.len() > 64 * 1024 * 1024 {
        return Err(err(ErrorCode::Invalid, "Recovery bundle exceeds limit"));
    }
    let bundle: Bundle = parse(bytes)?;
    if bundle.version != 1 && bundle.version != 2 {
        return Err(err(
            ErrorCode::Unsupported,
            "Unsupported recovery bundle version",
        ));
    }
    let salt = bundle.salt.clone();
    let (m, t, p) = (bundle.memory_kib, bundle.iterations, bundle.lanes);
    let key = tokio::task::spawn_blocking(move || derive(&passphrase, &salt, m, t, p))
        .await
        .map_err(|_| err(ErrorCode::Unavailable, "Recovery worker unavailable"))??;
    check(control)?;
    let aad = json(&(bundle.version, m, t, p, &bundle.salt))?;
    let plaintext = crypto::open(key.expose(), &aad, &bundle.ciphertext)?;
    let (
        vault_id,
        generation,
        authentication_generation,
        mut receipt,
        mut values,
        operations,
        audit,
    ): (
        String,
        u64,
        u64,
        String,
        Vec<(CredentialMetadata, String)>,
        Vec<StoredOperation>,
        Vec<CredentialAudit>,
    ) = if bundle.version == 2 {
        parse(plaintext.expose())?
    } else {
        let (v, g, a, r, values) = parse(plaintext.expose())?;
        (v, g, a, r, values, Vec::new(), Vec::new())
    };
    use zeroize::Zeroize;
    let receipt_key = data_encoding::BASE64
        .decode(receipt.as_bytes())
        .map(SecretBytes::new);
    receipt.zeroize();
    let receipt_key = receipt_key.map_err(|_| {
        err(
            ErrorCode::VerificationFailed,
            "Invalid recovery receipt key",
        )
    })?;
    if receipt_key.expose().len() != 32
        || values.len() > 10000
        || operations.len() > 100000
        || audit.len() > 100000
    {
        return Err(err(ErrorCode::Invalid, "Recovery structure limit exceeded"));
    }
    let records = values
        .iter_mut()
        .map(|(metadata, value)| {
            let decoded = data_encoding::BASE64
                .decode(value.as_bytes())
                .map_err(|_| err(ErrorCode::VerificationFailed, "Invalid recovery record"));
            value.zeroize();
            decoded.map(|v| RecoveryRecord {
                metadata: metadata.clone(),
                secret: SecretBytes::new(v),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(RecoveryImage {
        vault_id,
        generation,
        authentication_generation,
        receipt_key,
        records,
        operations,
        audit,
    })
}
pub async fn open_recovery(
    bytes: &[u8],
    passphrase: SecretBytes,
    control: &Control<'_>,
) -> Result<Vec<RecoveryRecord>> {
    Ok(decode_recovery(bytes, passphrase, control).await?.records)
}
impl Vault {
    pub async fn restore_recovery(
        &self,
        access: &CredentialAccess,
        bytes: &[u8],
        passphrase: SecretBytes,
        control: &Control<'_>,
    ) -> Result<u64> {
        self.authorize(
            access,
            &CredentialRef {
                namespace: "system".into(),
                id: "vault".into(),
            },
            Permission::Admin,
            control,
        )
        .await?;
        let image = decode_recovery(bytes, passphrase, control).await?;
        let (mut h, master, _) = self.material(control).await?;
        let expected = h.generation;
        h.vault_id = image.vault_id;
        h.generation = image
            .generation
            .checked_add(1)
            .ok_or_else(|| err(ErrorCode::Invalid, "Recovery generation overflow"))?
            .max(expected + 1);
        h.authentication_generation = image
            .authentication_generation
            .checked_add(1)
            .ok_or_else(|| err(ErrorCode::Invalid, "Recovery authority overflow"))?
            .max(h.authentication_generation + 1);
        let mut rows = Vec::new();
        let mut ids = std::collections::BTreeSet::new();
        for mut record in image.records {
            record.metadata.reference.validate()?;
            if !ids.insert(record.metadata.reference.clone())
                || record.metadata.revision == 0
                || record.metadata.revision > i64::MAX as u64
                || record.secret.expose().len() > 65536
            {
                return Err(err(
                    ErrorCode::Invalid,
                    "Invalid or duplicate recovery record",
                ));
            }
            record.metadata.authority_generation = h.generation;
            rows.push(Self::envelope(
                &h,
                &master,
                record.metadata,
                record.secret.expose(),
            )?);
        }
        h.encrypted_check = crypto::seal(
            master.expose(),
            &Self::header_aad(&h)?,
            image.receipt_key.expose(),
        )?;
        check(control)?;
        for operation in &image.operations {
            Self::verify_operation(&image.receipt_key, operation)?;
            if !ids.contains(&operation.operation.metadata.reference) {
                return Err(err(
                    ErrorCode::VerificationFailed,
                    "Recovery receipt references a missing record",
                ));
            }
        }
        self.repository
            .restore_empty(
                expected,
                CredentialRepositorySnapshot {
                    header: h.clone(),
                    records: rows,
                    operations: image.operations,
                    audit: image.audit,
                },
                CredentialAudit {
                    sequence: 0,
                    at_ms: crypto::now_ms(),
                    principal: access.principal.clone(),
                    action: "restore".into(),
                    reference: Some(CredentialRef {
                        namespace: "system".into(),
                        id: "vault".into(),
                    }),
                    outcome: "committed_fresh_client_authority_required".into(),
                },
            )
            .await?;
        Ok(h.authentication_generation)
    }
}
