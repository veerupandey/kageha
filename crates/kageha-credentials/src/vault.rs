use crate::{
    crypto::{self, check, err, json},
    *,
};
use hmac::{Hmac, Mac};
use kageha_contracts::{execution::Control, ErrorCode, Result};
use sha2::Sha256;
use std::sync::Arc;

pub struct Vault {
    pub(crate) repository: Arc<dyn CredentialRepository>,
    pub(crate) keys: Arc<dyn KeyProtector>,
    pub(crate) grants: Vec<Grant>,
}
impl Vault {
    pub async fn open(
        repository: Arc<dyn CredentialRepository>,
        keys: Arc<dyn KeyProtector>,
        initial_key_id: &str,
        grants: Vec<Grant>,
        control: &Control<'_>,
    ) -> Result<Self> {
        check(control)?;
        if initial_key_id.is_empty() || initial_key_id.len() > 128 {
            return Err(err(ErrorCode::Invalid, "Invalid vault key ID"));
        }
        for grant in &grants {
            if grant.principal.is_empty()
                || grant.namespace.is_empty()
                || grant.ids.is_empty()
                || grant.purposes.is_empty()
                || grant.destinations.is_empty()
            {
                return Err(err(ErrorCode::Invalid,"Credential grants require explicit identity, references, purposes and destinations"));
            }
        }
        if repository.header().await?.is_none() {
            let key = keys.unlock(initial_key_id, control).await?;
            let mut header = VaultHeader {
                format_version: 2,
                admission: VaultAdmission::Active,
                admission_owner: None,
                vault_id: uuid::Uuid::new_v4().to_string(),
                key_id: initial_key_id.into(),
                generation: 1,
                authentication_generation: 1,
                encrypted_check: Vec::new(),
            };
            header.encrypted_check = crypto::seal(
                key.expose(),
                &Self::header_aad(&header)?,
                &crypto::random::<32>()?,
            )?;
            repository.initialize(header).await?;
        }
        let vault = Self {
            repository,
            keys,
            grants,
        };
        vault.material_unchecked(control).await?;
        Ok(vault)
    }
    pub(crate) fn header_aad(h: &VaultHeader) -> Result<Vec<u8>> {
        match h.format_version {
            1 => json(&(
                1,
                &h.vault_id,
                &h.key_id,
                h.generation,
                h.authentication_generation,
            )),
            2 => json(&(
                2,
                &h.vault_id,
                &h.key_id,
                h.generation,
                h.authentication_generation,
                &h.admission,
                &h.admission_owner,
            )),
            _ => Err(err(
                ErrorCode::Unsupported,
                "Unsupported credential header format",
            )),
        }
    }
    pub async fn authentication_generation(&self, control: &Control<'_>) -> Result<u64> {
        Ok(self
            .material_unchecked(control)
            .await?
            .0
            .authentication_generation)
    }
    pub async fn export_record(
        &self,
        access: &CredentialAccess,
        reference: &CredentialRef,
        control: &Control<'_>,
    ) -> Result<(CredentialMetadata, SecretBytes)> {
        self.authorize(access, reference, Permission::Export, control)
            .await?;
        let (h, _, _) = self.material(control).await?;
        let row = self
            .repository
            .record(reference)
            .await?
            .ok_or_else(|| err(ErrorCode::NotFound, "Credential not found"))?;
        if row.metadata.reference != *reference {
            return Err(err(
                ErrorCode::VerificationFailed,
                "Credential reference mismatch",
            ));
        }
        let secret = self.decrypt(&h, &row, control).await?;
        self.repository
            .append_audit(Self::audit(
                access,
                Some(reference),
                "export_record",
                "authorized",
            ))
            .await?;
        Ok((row.metadata, secret))
    }
    pub(crate) async fn material_unchecked(
        &self,
        control: &Control<'_>,
    ) -> Result<(VaultHeader, SecretBytes, SecretBytes)> {
        check(control)?;
        let h = crypto::controlled(control, self.repository.header())
            .await?
            .ok_or_else(|| err(ErrorCode::Unavailable, "Credential vault not initialized"))?;
        let key = self.keys.unlock(&h.key_id, control).await?;
        let receipt_key = crypto::open(key.expose(), &Self::header_aad(&h)?, &h.encrypted_check)?;
        if receipt_key.expose().len() != 32 {
            return Err(err(
                ErrorCode::VerificationFailed,
                "Invalid credential vault header",
            ));
        }
        check(control)?;
        Ok((h, key, receipt_key))
    }
    pub(crate) async fn material(
        &self,
        control: &Control<'_>,
    ) -> Result<(VaultHeader, SecretBytes, SecretBytes)> {
        let result = self.material_unchecked(control).await?;
        if result.0.admission != VaultAdmission::Active {
            return Err(err(
                ErrorCode::Unavailable,
                "Credential binding is paused for replacement",
            ));
        }
        Ok(result)
    }
    fn audit(
        access: &CredentialAccess,
        reference: Option<&CredentialRef>,
        action: &str,
        outcome: &str,
    ) -> CredentialAudit {
        CredentialAudit {
            sequence: 0,
            at_ms: crypto::now_ms(),
            principal: access.principal.clone(),
            action: action.into(),
            reference: reference.cloned(),
            outcome: outcome.into(),
        }
    }
    pub(crate) async fn authorize(
        &self,
        access: &CredentialAccess,
        reference: &CredentialRef,
        permission: Permission,
        control: &Control<'_>,
    ) -> Result<()> {
        check(control)?;
        reference.validate()?;
        if access.principal.len() > 256
            || access.purpose.len() > 256
            || access.destination.len() > 2048
            || [&access.principal, &access.purpose, &access.destination]
                .iter()
                .any(|v| v.is_empty() || v.chars().any(char::is_control))
        {
            return Err(err(
                ErrorCode::Unauthorized,
                "Invalid credential access context",
            ));
        }
        let workload = CredentialRef {
            namespace: "system".into(),
            id: format!("workload:{}", access.principal),
        };
        if let Some(record) = crypto::controlled(control, self.repository.record(&workload)).await?
        {
            let (h, _, _) = self.material_unchecked(control).await?;
            self.decrypt(&h, &record, control).await?;
            if record.metadata.state != CredentialState::Active {
                self.repository
                    .append_audit(Self::audit(
                        access,
                        Some(reference),
                        "authorize",
                        "workload_revoked",
                    ))
                    .await?;
                return Err(err(
                    ErrorCode::Unauthorized,
                    "Credential workload is revoked",
                ));
            }
        }
        let allowed = self.grants.iter().any(|g| {
            g.principal == access.principal
                && g.namespace == reference.namespace
                && (g.ids.contains(&reference.id) || g.ids.contains("*"))
                && g.purposes.contains(&access.purpose)
                && g.destinations.contains(&access.destination)
                && g.permissions.contains(&permission)
        });
        if !allowed {
            self.repository
                .append_audit(Self::audit(access, Some(reference), "authorize", "denied"))
                .await?;
            return Err(err(ErrorCode::Unauthorized, "Credential access denied"));
        }
        Ok(())
    }
    fn payload_aad(h: &VaultHeader, meta: &CredentialMetadata) -> Result<Vec<u8>> {
        json(&(
            1,
            &h.vault_id,
            &meta.reference,
            &meta.kind,
            meta.revision,
            meta.expires_at_ms,
            &meta.state,
        ))
    }
    fn wrap_aad(h: &VaultHeader, row: &CredentialEnvelope) -> Result<Vec<u8>> {
        json(&(
            1,
            &h.vault_id,
            &row.metadata.reference,
            row.metadata.revision,
            &row.key_id,
        ))
    }
    pub(crate) async fn decrypt(
        &self,
        h: &VaultHeader,
        row: &CredentialEnvelope,
        control: &Control<'_>,
    ) -> Result<SecretBytes> {
        let master = self.keys.unlock(&row.key_id, control).await?;
        let data_key = crypto::open(master.expose(), &Self::wrap_aad(h, row)?, &row.wrapped_key)?;
        crypto::open(
            data_key.expose(),
            &Self::payload_aad(h, &row.metadata)?,
            &row.ciphertext,
        )
    }
    pub(crate) fn envelope(
        h: &VaultHeader,
        master: &SecretBytes,
        meta: CredentialMetadata,
        secret: &[u8],
    ) -> Result<CredentialEnvelope> {
        let data_key = SecretBytes::new(crypto::random::<32>()?.to_vec());
        let mut row = CredentialEnvelope {
            metadata: meta,
            key_id: h.key_id.clone(),
            wrapped_key: Vec::new(),
            ciphertext: Vec::new(),
        };
        row.wrapped_key = crypto::seal(
            master.expose(),
            &Self::wrap_aad(h, &row)?,
            data_key.expose(),
        )?;
        row.ciphertext = crypto::seal(
            data_key.expose(),
            &Self::payload_aad(h, &row.metadata)?,
            secret,
        )?;
        Ok(row)
    }
    fn fingerprint(
        receipt_key: &SecretBytes,
        access: &CredentialAccess,
        action: &str,
        write: &CredentialWrite,
    ) -> Result<Vec<u8>> {
        let mut mac = Hmac::<Sha256>::new_from_slice(receipt_key.expose())
            .map_err(|_| err(ErrorCode::Invalid, "Invalid credential receipt key"))?;
        mac.update(&json(&(
            &access.principal,
            &access.purpose,
            &access.destination,
            action,
            &write.reference,
            write.expected_revision,
            &write.kind,
            write.expires_at_ms,
        ))?);
        mac.update(write.secret.expose());
        Ok(mac.finalize().into_bytes().to_vec())
    }
    fn operation_id(id: &str) -> Result<()> {
        if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
            return Err(err(ErrorCode::Invalid, "Invalid credential operation ID"));
        }
        Ok(())
    }
    fn operation_auth(
        key: &SecretBytes,
        operation: &CredentialOperation,
        fingerprint: &[u8],
    ) -> Result<Vec<u8>> {
        let mut mac = Hmac::<Sha256>::new_from_slice(key.expose())
            .map_err(|_| err(ErrorCode::Invalid, "Invalid credential receipt key"))?;
        mac.update(&json(&(operation, fingerprint))?);
        Ok(mac.finalize().into_bytes().to_vec())
    }
    pub(crate) fn verify_operation(key: &SecretBytes, stored: &StoredOperation) -> Result<()> {
        let mut mac = Hmac::<Sha256>::new_from_slice(key.expose())
            .map_err(|_| err(ErrorCode::Invalid, "Invalid credential receipt key"))?;
        mac.update(&json(&(&stored.operation, &stored.fingerprint))?);
        mac.verify_slice(&stored.authentication).map_err(|_| {
            err(
                ErrorCode::VerificationFailed,
                "Credential receipt authentication failed",
            )
        })
    }
    pub(crate) async fn write(
        &self,
        access: &CredentialAccess,
        write: CredentialWrite,
        state: CredentialState,
        action: &str,
        control: &Control<'_>,
    ) -> Result<CredentialOperation> {
        check(control)?;
        Self::operation_id(&write.operation_id)?;
        write.reference.validate()?;
        if write.secret.expose().len() > 64 * 1024
            || write.kind.is_empty()
            || write.kind.len() > 128
            || write.kind.chars().any(char::is_control)
            || write.expected_revision >= i64::MAX as u64
        {
            return Err(err(ErrorCode::Invalid, "Credential input exceeds limits"));
        }
        let (h, master, receipt_key) = self.material(control).await?;
        let fingerprint = Self::fingerprint(&receipt_key, access, action, &write)?;
        if let Some(old) = self.repository.operation(&write.operation_id).await? {
            Self::verify_operation(&receipt_key, &old)?;
            if old.fingerprint != fingerprint {
                return Err(err(
                    ErrorCode::Conflict,
                    "Credential operation ID already has different input",
                ));
            }
            return Ok(old.operation);
        }
        let refreshable = Self::refreshable(&write.kind, &state, write.secret.expose());
        let metadata = CredentialMetadata {
            reference: write.reference.clone(),
            kind: write.kind.clone(),
            revision: write.expected_revision + 1,
            authority_generation: h.generation,
            expires_at_ms: write.expires_at_ms,
            state,
            refreshable,
        };
        let row = Self::envelope(&h, &master, metadata.clone(), write.secret.expose())?;
        let mut operation = StoredOperation {
            operation: CredentialOperation {
                operation_id: write.operation_id.clone(),
                metadata,
            },
            fingerprint,
            authentication: Vec::new(),
        };
        operation.authentication =
            Self::operation_auth(&receipt_key, &operation.operation, &operation.fingerprint)?;
        check(control)?;
        // If publication is cancelled, callers query this operation ID rather than invent another mutation.
        let saved=crypto::controlled(control,self.repository.commit(h.generation,write.expected_revision,row,operation,Self::audit(access,Some(&write.reference),action,"committed"))).await.map_err(|e|if matches!(e.code,ErrorCode::Cancelled|ErrorCode::Deadline){err(ErrorCode::Uncertain,"Credential commit may have completed; query the original operation ID before retrying")}else{e})?;
        Self::verify_operation(&receipt_key, &saved)?;
        Ok(saved.operation)
    }
    pub async fn audits(
        &self,
        access: &CredentialAccess,
        namespace: &str,
        after: u64,
        control: &Control<'_>,
    ) -> Result<Vec<CredentialAudit>> {
        check(control)?;
        self.authorize(
            access,
            &CredentialRef {
                namespace: namespace.into(),
                id: "audit".into(),
            },
            Permission::Admin,
            control,
        )
        .await?;
        self.repository
            .audit_namespace(namespace, after, 1000)
            .await
    }
    pub(crate) fn refreshable(kind: &str, state: &CredentialState, bytes: &[u8]) -> bool {
        if kind != "oauth_grant" || *state != CredentialState::Active {
            return false;
        }
        let value: serde_json::Value = match serde_json::from_slice(bytes) {
            Ok(v) => v,
            Err(_) => return false,
        };
        let result = value["refresh_token"]
            .as_str()
            .is_some_and(|v| !v.is_empty());
        let mut value = value;
        fn clear(value: &mut serde_json::Value) {
            match value {
                serde_json::Value::String(v) => {
                    use zeroize::Zeroize;
                    v.zeroize();
                }
                serde_json::Value::Object(v) => v.values_mut().for_each(clear),
                serde_json::Value::Array(v) => v.iter_mut().for_each(clear),
                _ => {}
            }
        }
        clear(&mut value);
        result
    }
    /// Fence outstanding domain operations without exposing or replacing the current secret.
    pub async fn fence(
        &self,
        access: &CredentialAccess,
        reference: &CredentialRef,
        expected: u64,
        id: &str,
        control: &Control<'_>,
    ) -> Result<CredentialOperation> {
        self.authorize(access, reference, Permission::Write, control)
            .await?;
        let (h, _, _) = self.material(control).await?;
        let row = self
            .repository
            .record(reference)
            .await?
            .ok_or_else(|| err(ErrorCode::NotFound, "Credential not found"))?;
        if row.metadata.revision != expected {
            return Err(err(ErrorCode::Conflict, "Credential revision changed"));
        }
        let secret = self.decrypt(&h, &row, control).await?;
        self.write(
            access,
            CredentialWrite {
                operation_id: id.into(),
                reference: reference.clone(),
                expected_revision: expected,
                kind: row.metadata.kind,
                expires_at_ms: row.metadata.expires_at_ms,
                secret,
            },
            row.metadata.state,
            "fence",
            control,
        )
        .await
    }
    pub async fn rotate_key(
        &self,
        access: &CredentialAccess,
        namespace: &str,
        next_id: &str,
        control: &Control<'_>,
    ) -> Result<u64> {
        if namespace != "system" {
            return Err(err(
                ErrorCode::Unauthorized,
                "Vault key rotation requires system administration",
            ));
        }
        self.authorize(
            access,
            &CredentialRef {
                namespace: namespace.into(),
                id: "vault".into(),
            },
            Permission::Admin,
            control,
        )
        .await?;
        // One atomic replacement is restart-safe: a crash exposes the old complete generation or the new complete generation.
        let (mut h, _, receipt_key) = self.material(control).await?;
        let (snapshot, mut rows) = self.repository.snapshot().await?;
        if snapshot.generation != h.generation {
            return Err(err(
                ErrorCode::StaleGeneration,
                "Credential authority changed",
            ));
        }
        if next_id == h.key_id {
            return Ok(h.generation);
        }
        let next = self.keys.unlock(next_id, control).await?;
        for row in &mut rows {
            check(control)?;
            let old = self.keys.unlock(&row.key_id, control).await?;
            let data_key = crypto::open(old.expose(), &Self::wrap_aad(&h, row)?, &row.wrapped_key)?;
            row.key_id = next_id.into();
            row.wrapped_key =
                crypto::seal(next.expose(), &Self::wrap_aad(&h, row)?, data_key.expose())?;
        }
        let expected = h.generation;
        h.generation += 1;
        h.key_id = next_id.into();
        h.encrypted_check =
            crypto::seal(next.expose(), &Self::header_aad(&h)?, receipt_key.expose())?;
        for row in &mut rows {
            row.metadata.authority_generation = h.generation;
        }
        self.repository
            .replace(
                expected,
                h.clone(),
                rows,
                Self::audit(
                    access,
                    Some(&CredentialRef {
                        namespace: namespace.into(),
                        id: "vault".into(),
                    }),
                    "rotate_key",
                    "committed",
                ),
            )
            .await?;
        Ok(h.generation)
    }
}
#[async_trait::async_trait]
impl CredentialResolver for Vault {
    async fn resolve(
        &self,
        access: &CredentialAccess,
        reference: &CredentialRef,
        control: &Control<'_>,
    ) -> Result<SecretLease> {
        check(control)?;
        self.authorize(access, reference, Permission::Resolve, control)
            .await?;
        let (h, _, _) = self.material(control).await?;
        let row = self
            .repository
            .record(reference)
            .await?
            .ok_or_else(|| err(ErrorCode::NotFound, "Credential not found"))?;
        if &row.metadata.reference != reference {
            return Err(err(
                ErrorCode::VerificationFailed,
                "Credential reference mismatch",
            ));
        }
        if row.metadata.state != CredentialState::Active
            || row
                .metadata
                .expires_at_ms
                .is_some_and(|v| v <= crypto::now_ms())
        {
            return Err(err(
                ErrorCode::Unauthorized,
                "Credential is revoked, expired, deleted or requires recovery",
            ));
        }
        let secret = self.decrypt(&h, &row, control).await?;
        let valid_until_ms =
            (crypto::now_ms() + 60_000).min(row.metadata.expires_at_ms.unwrap_or(u64::MAX));
        let mut metadata = row.metadata;
        metadata.authority_generation = h.generation;
        metadata.refreshable = Self::refreshable(&metadata.kind, &metadata.state, secret.expose());
        let lease = SecretLease {
            secret,
            metadata,
            valid_until_ms,
        };
        self.validate_lease(access, &lease, control).await?;
        self.repository
            .append_audit(Self::audit(access, Some(reference), "resolve", "allowed"))
            .await?;
        check(control)?;
        Ok(lease)
    }
    async fn validate_lease(
        &self,
        access: &CredentialAccess,
        lease: &SecretLease,
        control: &Control<'_>,
    ) -> Result<()> {
        check(control)?;
        self.authorize(
            access,
            &lease.metadata.reference,
            Permission::Resolve,
            control,
        )
        .await?;
        let (h, _, _) = self.material(control).await?;
        let current = self
            .repository
            .record(&lease.metadata.reference)
            .await?
            .ok_or_else(|| err(ErrorCode::NotFound, "Credential not found"))?;
        self.decrypt(&h, &current, control).await?;
        if lease.valid_until_ms <= crypto::now_ms()
            || lease.valid_until_ms > crypto::now_ms() + 60000
            || current
                .metadata
                .expires_at_ms
                .is_some_and(|v| v <= crypto::now_ms() || lease.valid_until_ms > v)
            || lease.metadata.authority_generation != h.generation
            || current.metadata.state != CredentialState::Active
            || current.metadata.revision != lease.metadata.revision
        {
            return Err(err(
                ErrorCode::StaleGeneration,
                "Credential lease is no longer authorized",
            ));
        }
        Ok(())
    }
}
#[async_trait::async_trait]
impl CredentialAdmin for Vault {
    async fn metadata(
        &self,
        access: &CredentialAccess,
        reference: &CredentialRef,
        control: &Control<'_>,
    ) -> Result<CredentialMetadata> {
        check(control)?;
        self.authorize(access, reference, Permission::Metadata, control)
            .await?;
        let (h, _, _) = self.material(control).await?;
        let mut row = self
            .repository
            .record(reference)
            .await?
            .ok_or_else(|| err(ErrorCode::NotFound, "Credential not found"))?;
        if &row.metadata.reference != reference {
            return Err(err(
                ErrorCode::VerificationFailed,
                "Credential reference mismatch",
            ));
        }
        let secret = self.decrypt(&h, &row, control).await?;
        row.metadata.refreshable =
            Self::refreshable(&row.metadata.kind, &row.metadata.state, secret.expose());
        row.metadata.authority_generation = h.generation;
        Ok(row.metadata)
    }
    async fn put(
        &self,
        access: &CredentialAccess,
        write: CredentialWrite,
        control: &Control<'_>,
    ) -> Result<CredentialOperation> {
        self.authorize(access, &write.reference, Permission::Write, control)
            .await?;
        self.write(access, write, CredentialState::Active, "put", control)
            .await
    }
    async fn revoke(
        &self,
        access: &CredentialAccess,
        reference: &CredentialRef,
        expected_revision: u64,
        operation_id: &str,
        delete: bool,
        control: &Control<'_>,
    ) -> Result<CredentialOperation> {
        self.authorize(access, reference, Permission::Revoke, control)
            .await?;
        let kind = self
            .repository
            .record(reference)
            .await?
            .ok_or_else(|| err(ErrorCode::NotFound, "Credential not found"))?
            .metadata
            .kind;
        self.write(
            access,
            CredentialWrite {
                operation_id: operation_id.into(),
                reference: reference.clone(),
                expected_revision,
                kind,
                expires_at_ms: None,
                secret: SecretBytes::new(Vec::new()),
            },
            if delete {
                CredentialState::Deleted
            } else {
                CredentialState::Revoked
            },
            if delete { "delete" } else { "revoke" },
            control,
        )
        .await
    }
    async fn operation(
        &self,
        access: &CredentialAccess,
        reference: &CredentialRef,
        id: &str,
        control: &Control<'_>,
    ) -> Result<Option<CredentialOperation>> {
        check(control)?;
        self.authorize(access, reference, Permission::Metadata, control)
            .await?;
        Self::operation_id(id)?;
        let (_, _, receipt_key) = self.material(control).await?;
        let old = self.repository.operation(id).await?;
        if let Some(ref value) = old {
            Self::verify_operation(&receipt_key, value)?;
        }
        if old
            .as_ref()
            .is_some_and(|v| v.operation.metadata.reference != *reference)
        {
            return Err(err(
                ErrorCode::Unauthorized,
                "Credential operation does not belong to this reference",
            ));
        }
        Ok(old.map(|v| v.operation))
    }
}
