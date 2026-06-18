//! Private authenticated-transport DTOs. Secret domain types deliberately have no serde implementation.
use crate::{crypto::err, *};
use kageha_contracts::{ErrorCode, Result};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Record {
    metadata: CredentialMetadata,
    secret_base64: String,
}
impl Drop for Record {
    fn drop(&mut self) {
        self.secret_base64.zeroize();
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Checkpoint {
    access_grants: Vec<Grant>,
    source_vault_id: String,
    source_generation: u64,
    receipt_key_base64: String,
    records: Vec<Record>,
    operations: Vec<StoredOperation>,
    audit: Vec<CredentialAudit>,
}
impl Drop for Checkpoint {
    fn drop(&mut self) {
        self.receipt_key_base64.zeroize();
    }
}
impl Checkpoint {
    pub fn encode(value: CredentialCheckpoint) -> Self {
        Self {
            access_grants: value.access_grants,
            source_vault_id: value.source_vault_id,
            source_generation: value.source_generation,
            receipt_key_base64: data_encoding::BASE64.encode(value.receipt_key.expose()),
            records: value
                .records
                .into_iter()
                .map(|row| Record {
                    metadata: row.metadata,
                    secret_base64: data_encoding::BASE64.encode(row.secret.expose()),
                })
                .collect(),
            operations: value.operations,
            audit: value.audit,
        }
    }
    pub fn decode(mut self) -> Result<CredentialCheckpoint> {
        if self.records.len() > 10000 || self.operations.len() > 100000 || self.audit.len() > 100000
        {
            return Err(err(
                ErrorCode::Invalid,
                "Credential checkpoint exceeds limits",
            ));
        }
        let key = data_encoding::BASE64
            .decode(self.receipt_key_base64.as_bytes())
            .map(SecretBytes::new)
            .map_err(|_| err(ErrorCode::Invalid, "Invalid credential checkpoint key"))?;
        self.receipt_key_base64.zeroize();
        if key.expose().len() != 32 {
            return Err(err(ErrorCode::Invalid, "Invalid credential checkpoint key"));
        }
        let mut rows = Vec::new();
        let mut size = 0usize;
        for row in std::mem::take(&mut self.records) {
            let secret = data_encoding::BASE64
                .decode(row.secret_base64.as_bytes())
                .map(SecretBytes::new)
                .map_err(|_| err(ErrorCode::Invalid, "Invalid credential checkpoint encoding"))?;
            size += secret.expose().len();
            if secret.expose().len() > 65536 || size > 32 * 1024 * 1024 {
                return Err(err(
                    ErrorCode::Invalid,
                    "Credential checkpoint exceeds limits",
                ));
            }
            rows.push(CredentialTransferRecord {
                metadata: row.metadata.clone(),
                secret,
            });
        }
        Ok(CredentialCheckpoint {
            access_grants: std::mem::take(&mut self.access_grants),
            source_vault_id: std::mem::take(&mut self.source_vault_id),
            source_generation: self.source_generation,
            receipt_key: key,
            records: rows,
            operations: std::mem::take(&mut self.operations),
            audit: std::mem::take(&mut self.audit),
        })
    }
}
#[cfg(feature = "client")]
pub(crate) fn clear(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(v) => v.zeroize(),
        serde_json::Value::Array(v) => v.iter_mut().for_each(clear),
        serde_json::Value::Object(v) => v.values_mut().for_each(clear),
        _ => {}
    }
}
