//! Durable refresh admission. A possibly consumed refresh token is never automatically replayed.
use crate::{
    crypto::{self, check, err, json, parse},
    *,
};
use kageha_contracts::{execution::Control, ErrorCode, Result};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Prepared {
    owner: String,
    purpose: String,
    destination: String,
    lease_id: String,
    authority_generation: u64,
    expires_at_ms: u64,
    original_expiry: Option<u64>,
    grant_base64: String,
}
impl Drop for Prepared {
    fn drop(&mut self) {
        self.grant_base64.zeroize();
    }
}
impl Prepared {
    fn ticket(&self, metadata: &CredentialMetadata) -> RefreshTicket {
        RefreshTicket {
            reference: metadata.reference.clone(),
            lease_id: self.lease_id.clone(),
            revision: metadata.revision,
            authority_generation: self.authority_generation,
            expires_at_ms: self.expires_at_ms,
        }
    }
    fn owner(
        &self,
        access: &CredentialAccess,
        ticket: &RefreshTicket,
        generation: u64,
    ) -> Result<()> {
        if self.owner != access.principal
            || self.purpose != access.purpose
            || self.destination != access.destination
            || self.lease_id != ticket.lease_id
            || ticket.authority_generation != generation
            || ticket.authority_generation != self.authority_generation
            || self.expires_at_ms <= crypto::now_ms()
        {
            return Err(err(
                ErrorCode::StaleGeneration,
                "OAuth refresh lease is no longer authorized",
            ));
        }
        Ok(())
    }
    fn grant(&self) -> Result<SecretBytes> {
        data_encoding::BASE64
            .decode(self.grant_base64.as_bytes())
            .map(SecretBytes::new)
            .map_err(|_| {
                err(
                    ErrorCode::VerificationFailed,
                    "Invalid encrypted OAuth grant",
                )
            })
    }
}
impl Vault {
    pub async fn fail_refresh(
        &self,
        access: &CredentialAccess,
        ticket: &RefreshTicket,
        operation_id: &str,
        control: &Control<'_>,
    ) -> Result<CredentialOperation> {
        let (h, row, secret) = self
            .refresh_record(access, &ticket.reference, ticket.revision, control)
            .await?;
        if row.metadata.state != CredentialState::RefreshDispatching {
            return Err(err(
                ErrorCode::Conflict,
                "OAuth refresh is not awaiting a response",
            ));
        }
        let prepared: Prepared = parse(secret.expose())?;
        if prepared.owner != access.principal || prepared.lease_id != ticket.lease_id {
            return Err(err(ErrorCode::Unauthorized, "OAuth refresh owner mismatch"));
        }
        let _ = h;
        self.write(
            access,
            CredentialWrite {
                operation_id: operation_id.into(),
                reference: ticket.reference.clone(),
                expected_revision: ticket.revision,
                kind: "oauth_grant".into(),
                expires_at_ms: None,
                secret: SecretBytes::new(Vec::new()),
            },
            CredentialState::RefreshUncertain,
            "oauth_refresh_uncertain",
            control,
        )
        .await
    }
    async fn refresh_record(
        &self,
        access: &CredentialAccess,
        reference: &CredentialRef,
        expected: u64,
        control: &Control<'_>,
    ) -> Result<(VaultHeader, CredentialEnvelope, SecretBytes)> {
        check(control)?;
        self.authorize(access, reference, Permission::Write, control)
            .await?;
        let (h, _, _) = self.material(control).await?;
        let row = self
            .repository
            .record(reference)
            .await?
            .ok_or_else(|| err(ErrorCode::NotFound, "OAuth grant not found"))?;
        if row.metadata.reference != *reference
            || row.metadata.kind != "oauth_grant"
            || row.metadata.revision != expected
        {
            return Err(err(
                ErrorCode::Conflict,
                "OAuth grant revision or kind changed",
            ));
        }
        let secret = self.decrypt(&h, &row, control).await?;
        Ok((h, row, secret))
    }
}
#[async_trait::async_trait]
impl CredentialRefresh for Vault {
    async fn begin_refresh(
        &self,
        access: &CredentialAccess,
        reference: &CredentialRef,
        expected_revision: u64,
        operation_id: &str,
        control: &Control<'_>,
    ) -> Result<RefreshTicket> {
        if let Some(previous) = self
            .operation(access, reference, operation_id, control)
            .await?
        {
            if previous.metadata.revision != expected_revision + 1
                || previous.metadata.state != CredentialState::RefreshPrepared
            {
                return Err(err(
                    ErrorCode::Conflict,
                    "OAuth refresh operation ID already used",
                ));
            }
            let (h, row, secret) = self
                .refresh_record(access, reference, previous.metadata.revision, control)
                .await?;
            if row.metadata.state != CredentialState::RefreshPrepared {
                return Err(err(
                    ErrorCode::Conflict,
                    "OAuth refresh has advanced; inspect its original receipt",
                ));
            }
            let prepared: Prepared = parse(secret.expose())?;
            let ticket = prepared.ticket(&row.metadata);
            prepared.owner(access, &ticket, h.generation)?;
            return Ok(ticket);
        }
        let (h, row, grant) = self
            .refresh_record(access, reference, expected_revision, control)
            .await?;
        if row.metadata.state != CredentialState::Active {
            return Err(err(
                ErrorCode::Conflict,
                "OAuth refresh is already pending or needs explicit recovery",
            ));
        }
        let remaining = control
            .deadline
            .map(|deadline| {
                deadline
                    .saturating_duration_since(std::time::Instant::now())
                    .as_millis() as u64
            })
            .unwrap_or(30000)
            .min(30000);
        let prepared = Prepared {
            owner: access.principal.clone(),
            purpose: access.purpose.clone(),
            destination: access.destination.clone(),
            lease_id: uuid::Uuid::new_v4().to_string(),
            authority_generation: h.generation,
            expires_at_ms: crypto::now_ms() + remaining,
            original_expiry: row.metadata.expires_at_ms,
            grant_base64: data_encoding::BASE64.encode(grant.expose()),
        };
        let result = self
            .write(
                access,
                CredentialWrite {
                    operation_id: operation_id.into(),
                    reference: reference.clone(),
                    expected_revision,
                    kind: "oauth_grant".into(),
                    expires_at_ms: None,
                    secret: SecretBytes::new(json(&prepared)?),
                },
                CredentialState::RefreshPrepared,
                "oauth_refresh_prepared",
                control,
            )
            .await?;
        Ok(prepared.ticket(&result.metadata))
    }
    async fn dispatch_refresh(
        &self,
        access: &CredentialAccess,
        ticket: &RefreshTicket,
        operation_id: &str,
        control: &Control<'_>,
    ) -> Result<(RefreshTicket, SecretBytes)> {
        let (h, row, secret) = self
            .refresh_record(access, &ticket.reference, ticket.revision, control)
            .await?;
        if row.metadata.state != CredentialState::RefreshPrepared {
            return Err(err(
                ErrorCode::Conflict,
                "OAuth refresh input may already have been dispatched",
            ));
        }
        let prepared: Prepared = parse(secret.expose())?;
        prepared.owner(access, ticket, h.generation)?;
        let result = self
            .write(
                access,
                CredentialWrite {
                    operation_id: operation_id.into(),
                    reference: ticket.reference.clone(),
                    expected_revision: ticket.revision,
                    kind: "oauth_grant".into(),
                    expires_at_ms: None,
                    secret: SecretBytes::new(json(&prepared)?),
                },
                CredentialState::RefreshDispatching,
                "oauth_refresh_dispatch",
                control,
            )
            .await?;
        Ok((prepared.ticket(&result.metadata), prepared.grant()?))
    }
    async fn complete_refresh(
        &self,
        access: &CredentialAccess,
        ticket: &RefreshTicket,
        operation_id: &str,
        grant: SecretBytes,
        expires_at_ms: Option<u64>,
        control: &Control<'_>,
    ) -> Result<CredentialOperation> {
        let (h, row, secret) = self
            .refresh_record(access, &ticket.reference, ticket.revision, control)
            .await?;
        if row.metadata.state != CredentialState::RefreshDispatching {
            return Err(err(
                ErrorCode::Conflict,
                "OAuth refresh is not awaiting a response",
            ));
        }
        let prepared: Prepared = parse(secret.expose())?;
        prepared.owner(access, ticket, h.generation)?;
        self.write(
            access,
            CredentialWrite {
                operation_id: operation_id.into(),
                reference: ticket.reference.clone(),
                expected_revision: ticket.revision,
                kind: "oauth_grant".into(),
                expires_at_ms,
                secret: grant,
            },
            CredentialState::Active,
            "oauth_refresh_complete",
            control,
        )
        .await
    }
    async fn recover_refresh(
        &self,
        access: &CredentialAccess,
        reference: &CredentialRef,
        expected_revision: u64,
        operation_id: &str,
        control: &Control<'_>,
    ) -> Result<CredentialOperation> {
        let (h, row, secret) = self
            .refresh_record(access, reference, expected_revision, control)
            .await?;
        if !matches!(
            row.metadata.state,
            CredentialState::RefreshPrepared | CredentialState::RefreshDispatching
        ) {
            return Err(err(
                ErrorCode::Conflict,
                "OAuth grant has no pending refresh",
            ));
        }
        let prepared: Prepared = parse(secret.expose())?;
        if prepared.expires_at_ms > crypto::now_ms()
            && prepared.authority_generation == h.generation
        {
            return Err(err(
                ErrorCode::Conflict,
                "OAuth refresh lease is still active",
            ));
        }
        let not_dispatched = row.metadata.state == CredentialState::RefreshPrepared;
        self.write(
            access,
            CredentialWrite {
                operation_id: operation_id.into(),
                reference: reference.clone(),
                expected_revision,
                kind: "oauth_grant".into(),
                expires_at_ms: if not_dispatched {
                    prepared.original_expiry
                } else {
                    None
                },
                secret: if not_dispatched {
                    prepared.grant()?
                } else {
                    SecretBytes::new(Vec::new())
                },
            },
            if not_dispatched {
                CredentialState::Active
            } else {
                CredentialState::RefreshUncertain
            },
            if not_dispatched {
                "oauth_refresh_not_dispatched"
            } else {
                "oauth_refresh_uncertain"
            },
            control,
        )
        .await
    }
}
