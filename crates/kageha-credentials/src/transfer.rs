//! Authenticated state transfer. Backends remain fenced until their logical binding commits.
use crate::{
    crypto::{self, check, err},
    *,
};
use kageha_contracts::{execution::Control, ErrorCode, Result};
use std::collections::BTreeSet;
impl Vault {
    async fn transfer_authority(
        &self,
        access: &CredentialAccess,
        control: &Control<'_>,
    ) -> Result<()> {
        check(control)?;
        self.authorize(
            access,
            &CredentialRef {
                namespace: "system".into(),
                id: "vault".into(),
            },
            Permission::Admin,
            control,
        )
        .await
    }
    async fn transfer_state(&self, control: &Control<'_>) -> Result<CredentialBackendState> {
        let (h, _, _) = self.material_unchecked(control).await?;
        let snapshot = self.repository.snapshot().await?;
        Ok(CredentialBackendState {
            vault_id: h.vault_id,
            generation: h.generation,
            admission: h.admission,
            admission_owner: h.admission_owner,
            records: snapshot.1.len() as u64,
            capabilities: [
                "checkpoint.v1",
                "authority.fence",
                "receipts.portable",
                "audit.portable",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        })
    }
    fn transfer_audit(access: &CredentialAccess, action: &str) -> CredentialAudit {
        CredentialAudit {
            sequence: 0,
            at_ms: crypto::now_ms(),
            principal: access.principal.clone(),
            action: action.into(),
            reference: Some(CredentialRef {
                namespace: "system".into(),
                id: "vault".into(),
            }),
            outcome: "committed".into(),
        }
    }
}
#[async_trait::async_trait]
impl CredentialControl for Vault {
    async fn revoke_workload(
        &self,
        a: &CredentialAccess,
        principal: &str,
        expected: u64,
        id: &str,
        c: &Control<'_>,
    ) -> Result<CredentialOperation> {
        self.transfer_authority(a, c).await?;
        if principal.is_empty() || principal.len() > 128 || principal.chars().any(char::is_control)
        {
            return Err(err(ErrorCode::Invalid, "Invalid credential workload"));
        }
        self.write(
            a,
            CredentialWrite {
                operation_id: id.into(),
                reference: CredentialRef {
                    namespace: "system".into(),
                    id: format!("workload:{principal}"),
                },
                expected_revision: expected,
                kind: "workload_authority".into(),
                expires_at_ms: None,
                secret: SecretBytes::new(Vec::new()),
            },
            CredentialState::Revoked,
            "revoke_workload",
            c,
        )
        .await
    }

    async fn binding_generation(&self, control: &Control<'_>) -> Result<u64> {
        Ok(self.material_unchecked(control).await?.0.generation)
    }
    async fn authentication_generation(&self, control: &Control<'_>) -> Result<u64> {
        Vault::authentication_generation(self, control).await
    }
    async fn fence(
        &self,
        a: &CredentialAccess,
        r: &CredentialRef,
        revision: u64,
        id: &str,
        c: &Control<'_>,
    ) -> Result<CredentialOperation> {
        Vault::fence(self, a, r, revision, id, c).await
    }
    async fn fail_refresh(
        &self,
        a: &CredentialAccess,
        t: &RefreshTicket,
        id: &str,
        c: &Control<'_>,
    ) -> Result<CredentialOperation> {
        Vault::fail_refresh(self, a, t, id, c).await
    }
    async fn export_record(
        &self,
        a: &CredentialAccess,
        r: &CredentialRef,
        c: &Control<'_>,
    ) -> Result<(CredentialMetadata, SecretBytes)> {
        Vault::export_record(self, a, r, c).await
    }
    async fn audits(
        &self,
        a: &CredentialAccess,
        namespace: &str,
        after: u64,
        c: &Control<'_>,
    ) -> Result<Vec<CredentialAudit>> {
        Vault::audits(self, a, namespace, after, c).await
    }
}
#[async_trait::async_trait]
impl CredentialTransfer for Vault {
    async fn backend_state(
        &self,
        access: &CredentialAccess,
        control: &Control<'_>,
    ) -> Result<CredentialBackendState> {
        self.transfer_authority(access, control).await?;
        self.transfer_state(control).await
    }
    async fn export_checkpoint(
        &self,
        access: &CredentialAccess,
        control: &Control<'_>,
    ) -> Result<CredentialCheckpoint> {
        self.transfer_authority(access, control).await?;
        let (h, _, key) = self.material_unchecked(control).await?;
        let snapshot = self.repository.snapshot_full().await?;
        if snapshot.header.generation != h.generation {
            return Err(err(
                ErrorCode::StaleGeneration,
                "Credential checkpoint authority changed",
            ));
        }
        let namespaces: BTreeSet<_> = snapshot
            .records
            .iter()
            .map(|row| row.metadata.reference.namespace.clone())
            .collect();
        for namespace in namespaces {
            self.authorize(
                access,
                &CredentialRef {
                    namespace,
                    id: "vault".into(),
                },
                Permission::Export,
                control,
            )
            .await?;
        }
        let mut records = Vec::new();
        for row in snapshot.records {
            check(control)?;
            let secret = self.decrypt(&h, &row, control).await?;
            let mut metadata = row.metadata;
            metadata.refreshable =
                Self::refreshable(&metadata.kind, &metadata.state, secret.expose());
            records.push(CredentialTransferRecord { metadata, secret });
        }
        for operation in &snapshot.operations {
            Self::verify_operation(&key, operation)?;
        }
        let access_grants = self
            .grants
            .iter()
            .filter(|g| {
                g.principal != access.principal
                    && records
                        .iter()
                        .any(|r| r.metadata.reference.namespace == g.namespace)
            })
            .cloned()
            .collect();
        Ok(CredentialCheckpoint {
            access_grants,
            source_vault_id: h.vault_id,
            source_generation: h.generation,
            receipt_key: key,
            records,
            operations: snapshot.operations,
            audit: snapshot.audit,
        })
    }
    async fn freeze(
        &self,
        access: &CredentialAccess,
        expected: u64,
        owner: &str,
        control: &Control<'_>,
    ) -> Result<CredentialBackendState> {
        self.transfer_authority(access, control).await?;
        if owner.is_empty() || owner.len() > 256 {
            return Err(err(
                ErrorCode::Invalid,
                "Invalid credential replacement owner",
            ));
        }
        let (mut h, master, receipt) = self.material_unchecked(control).await?;
        if h.admission == VaultAdmission::Fenced && h.admission_owner.as_deref() == Some(owner) {
            return self.transfer_state(control).await;
        }
        if h.generation != expected {
            return Err(err(
                ErrorCode::StaleGeneration,
                "Credential authority changed",
            ));
        }
        h.format_version = 2;
        h.generation = h
            .generation
            .checked_add(1)
            .ok_or_else(|| err(ErrorCode::Invalid, "Credential generation overflow"))?;
        h.admission = VaultAdmission::Fenced;
        h.admission_owner = Some(owner.into());
        h.encrypted_check =
            crypto::seal(master.expose(), &Self::header_aad(&h)?, receipt.expose())?;
        self.repository
            .update_header(
                expected,
                h,
                true,
                Self::transfer_audit(access, "replacement_fence"),
            )
            .await?;
        self.transfer_state(control).await
    }
    async fn import_checkpoint(
        &self,
        access: &CredentialAccess,
        expected: u64,
        owner: &str,
        checkpoint: CredentialCheckpoint,
        control: &Control<'_>,
    ) -> Result<CredentialBackendState> {
        self.transfer_authority(access, control).await?;
        let (mut h, master, _) = self.material_unchecked(control).await?;
        if h.generation != expected
            || h.admission != VaultAdmission::Fenced
            || h.admission_owner.as_deref() != Some(owner)
        {
            return Err(err(
                ErrorCode::StaleGeneration,
                "Credential destination is not owned by this replacement",
            ));
        }
        if checkpoint.access_grants.len() > 10000
            || checkpoint.receipt_key.expose().len() != 32
            || checkpoint.records.len() > 10000
            || checkpoint.operations.len() > 100000
            || checkpoint.audit.len() > 100000
        {
            return Err(err(
                ErrorCode::Invalid,
                "Credential checkpoint exceeds limits",
            ));
        }
        let namespaces: BTreeSet<_> = checkpoint
            .records
            .iter()
            .map(|r| r.metadata.reference.namespace.clone())
            .collect();
        let source_grants = &checkpoint.access_grants;
        let target_grants: Vec<_> = self
            .grants
            .iter()
            .filter(|g| g.principal != access.principal && namespaces.contains(&g.namespace))
            .collect();
        if source_grants.len() != target_grants.len()
            || source_grants.iter().any(|g| !target_grants.contains(&g))
        {
            return Err(err(
                ErrorCode::Unauthorized,
                "Credential destination workload access scopes do not match the source",
            ));
        }
        if checkpoint.source_vault_id == h.vault_id {
            return Err(err(
                ErrorCode::Invalid,
                "Credential transfer requires an independent source vault",
            ));
        }
        // A retry or rollback cannot replace a staged revision with an older snapshot.
        let existing = self.repository.snapshot_full().await?;
        for row in &existing.records {
            let next = checkpoint
                .records
                .iter()
                .find(|r| r.metadata.reference == row.metadata.reference)
                .ok_or_else(|| {
                    err(
                        ErrorCode::VerificationFailed,
                        "Credential import would lose an existing record or tombstone",
                    )
                })?;
            if next.metadata.revision < row.metadata.revision {
                return Err(err(
                    ErrorCode::Conflict,
                    "Credential import would regress a revision",
                ));
            }
            if next.metadata.revision == row.metadata.revision {
                let old = self.decrypt(&h, row, control).await?;
                if old.expose() != next.secret.expose()
                    || row.metadata.state != next.metadata.state
                    || row.metadata.kind != next.metadata.kind
                    || row.metadata.expires_at_ms != next.metadata.expires_at_ms
                {
                    return Err(err(
                        ErrorCode::VerificationFailed,
                        "Credential import changed a confirmed revision",
                    ));
                }
            }
        }
        h.format_version = 2;
        h.generation = checkpoint
            .source_generation
            .max(h.generation)
            .checked_add(1)
            .ok_or_else(|| err(ErrorCode::Invalid, "Credential generation overflow"))?;
        let mut ids = BTreeSet::new();
        let mut rows = Vec::new();
        let mut size = 0usize;
        for mut record in checkpoint.records {
            check(control)?;
            record.metadata.reference.validate()?;
            self.authorize(
                access,
                &CredentialRef {
                    namespace: record.metadata.reference.namespace.clone(),
                    id: "vault".into(),
                },
                Permission::Write,
                control,
            )
            .await?;
            size += record.secret.expose().len();
            if size > 32 * 1024 * 1024
                || record.secret.expose().len() > 65536
                || record.metadata.revision == 0
                || record.metadata.revision > i64::MAX as u64
                || !ids.insert(record.metadata.reference.clone())
            {
                return Err(err(
                    ErrorCode::Invalid,
                    "Credential checkpoint contains invalid or duplicate records",
                ));
            }
            if matches!(
                record.metadata.state,
                CredentialState::RefreshPrepared | CredentialState::RefreshDispatching
            ) {
                return Err(err(
                    ErrorCode::Conflict,
                    "Outstanding OAuth refresh prevents credential replacement",
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
        for operation in &checkpoint.operations {
            Self::verify_operation(&checkpoint.receipt_key, operation)?;
            if !ids.contains(&operation.operation.metadata.reference) {
                return Err(err(
                    ErrorCode::VerificationFailed,
                    "Credential receipt references missing checkpoint state",
                ));
            }
        }
        h.encrypted_check = crypto::seal(
            master.expose(),
            &Self::header_aad(&h)?,
            checkpoint.receipt_key.expose(),
        )?;
        self.repository
            .replace_all(
                expected,
                CredentialRepositorySnapshot {
                    header: h,
                    records: rows,
                    operations: checkpoint.operations,
                    audit: checkpoint.audit,
                },
                Self::transfer_audit(access, "replacement_import"),
            )
            .await?;
        self.transfer_state(control).await
    }
    async fn activate(
        &self,
        access: &CredentialAccess,
        expected: u64,
        owner: &str,
        control: &Control<'_>,
    ) -> Result<CredentialBackendState> {
        self.transfer_authority(access, control).await?;
        let (mut h, master, receipt) = self.material_unchecked(control).await?;
        if h.admission == VaultAdmission::Active && h.admission_owner.as_deref() == Some(owner) {
            return self.transfer_state(control).await;
        }
        if h.generation != expected
            || h.admission != VaultAdmission::Fenced
            || h.admission_owner.as_deref() != Some(owner)
        {
            return Err(err(
                ErrorCode::StaleGeneration,
                "Credential activation authority changed",
            ));
        }
        h.format_version = 2;
        h.generation += 1;
        h.admission = VaultAdmission::Active;
        h.encrypted_check =
            crypto::seal(master.expose(), &Self::header_aad(&h)?, receipt.expose())?;
        self.repository
            .update_header(
                expected,
                h,
                false,
                Self::transfer_audit(access, "replacement_activate"),
            )
            .await?;
        self.transfer_state(control).await
    }
}
