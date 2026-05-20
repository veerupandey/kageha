//! Credential ports contain no application, database, vendor, or transport implementation.
use crate::{execution::Control, Result};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, fmt};
use zeroize::Zeroizing;

/// Explicitly exposed only by authorized adapters; never serde-serializable.
pub struct SecretBytes(Zeroizing<Vec<u8>>);
impl SecretBytes {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(Zeroizing::new(bytes))
    }
    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}
impl fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretBytes([REDACTED])")
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct CredentialRef {
    pub namespace: String,
    pub id: String,
}
impl CredentialRef {
    pub fn validate(&self) -> Result<()> {
        for value in [&self.namespace, &self.id] {
            if value.is_empty() || value.len() > 256 || value.chars().any(|c| c.is_control()) {
                return Err(crate::ServiceError::new(
                    crate::ErrorCode::Invalid,
                    "Invalid credential reference",
                ));
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    Metadata,
    Resolve,
    Write,
    Revoke,
    Export,
    Admin,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    pub principal: String,
    pub namespace: String,
    pub ids: BTreeSet<String>,
    pub purposes: BTreeSet<String>,
    pub destinations: BTreeSet<String>,
    pub permissions: BTreeSet<Permission>,
}
/// Trusted composition constructs this after authentication. A service never deserializes the principal from a body.
#[derive(Clone, Debug)]
pub struct CredentialAccess {
    pub principal: String,
    pub purpose: String,
    pub destination: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CredentialState {
    Active,
    Revoked,
    Deleted,
    RefreshPrepared,
    RefreshDispatching,
    RefreshUncertain,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CredentialMetadata {
    pub reference: CredentialRef,
    pub kind: String,
    pub revision: u64,
    pub authority_generation: u64,
    pub expires_at_ms: Option<u64>,
    pub state: CredentialState,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub refreshable: bool,
}
#[derive(Debug)]
pub struct SecretLease {
    pub secret: SecretBytes,
    pub metadata: CredentialMetadata,
    pub valid_until_ms: u64,
}
pub struct CredentialWrite {
    pub operation_id: String,
    pub reference: CredentialRef,
    pub expected_revision: u64,
    pub kind: String,
    pub expires_at_ms: Option<u64>,
    pub secret: SecretBytes,
}
impl fmt::Debug for CredentialWrite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CredentialWrite")
            .field("operation_id", &self.operation_id)
            .field("reference", &self.reference)
            .field("secret", &"[REDACTED]")
            .finish()
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CredentialOperation {
    pub operation_id: String,
    pub metadata: CredentialMetadata,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CredentialAudit {
    pub sequence: u64,
    pub at_ms: u64,
    pub principal: String,
    pub action: String,
    pub reference: Option<CredentialRef>,
    pub outcome: String,
}
#[async_trait::async_trait]
pub trait CredentialResolver: Send + Sync {
    async fn resolve(
        &self,
        access: &CredentialAccess,
        reference: &CredentialRef,
        control: &Control<'_>,
    ) -> Result<SecretLease>;
    async fn validate_lease(
        &self,
        access: &CredentialAccess,
        lease: &SecretLease,
        control: &Control<'_>,
    ) -> Result<()>;
}
#[async_trait::async_trait]
pub trait CredentialAdmin: Send + Sync {
    async fn metadata(
        &self,
        access: &CredentialAccess,
        reference: &CredentialRef,
        control: &Control<'_>,
    ) -> Result<CredentialMetadata>;
    async fn put(
        &self,
        access: &CredentialAccess,
        write: CredentialWrite,
        control: &Control<'_>,
    ) -> Result<CredentialOperation>;
    async fn revoke(
        &self,
        access: &CredentialAccess,
        reference: &CredentialRef,
        expected_revision: u64,
        operation_id: &str,
        delete: bool,
        control: &Control<'_>,
    ) -> Result<CredentialOperation>;
    async fn operation(
        &self,
        access: &CredentialAccess,
        reference: &CredentialRef,
        operation_id: &str,
        control: &Control<'_>,
    ) -> Result<Option<CredentialOperation>>;
}
#[async_trait::async_trait]
pub trait KeyProtector: Send + Sync {
    async fn unlock(&self, key_id: &str, control: &Control<'_>) -> Result<SecretBytes>;
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RefreshTicket {
    pub reference: CredentialRef,
    pub lease_id: String,
    pub revision: u64,
    pub authority_generation: u64,
    pub expires_at_ms: u64,
}
#[async_trait::async_trait]
pub trait CredentialRefresh: Send + Sync {
    async fn begin_refresh(
        &self,
        access: &CredentialAccess,
        reference: &CredentialRef,
        expected_revision: u64,
        operation_id: &str,
        control: &Control<'_>,
    ) -> Result<RefreshTicket>;
    async fn dispatch_refresh(
        &self,
        access: &CredentialAccess,
        ticket: &RefreshTicket,
        operation_id: &str,
        control: &Control<'_>,
    ) -> Result<(RefreshTicket, SecretBytes)>;
    async fn complete_refresh(
        &self,
        access: &CredentialAccess,
        ticket: &RefreshTicket,
        operation_id: &str,
        grant: SecretBytes,
        expires_at_ms: Option<u64>,
        control: &Control<'_>,
    ) -> Result<CredentialOperation>;
    async fn recover_refresh(
        &self,
        access: &CredentialAccess,
        reference: &CredentialRef,
        expected_revision: u64,
        operation_id: &str,
        control: &Control<'_>,
    ) -> Result<CredentialOperation>;
}
/// The repository persists encrypted envelopes only. CAS and audit publication share one transaction.
#[derive(Clone, Serialize, Deserialize)]
pub struct CredentialEnvelope {
    pub metadata: CredentialMetadata,
    pub key_id: String,
    pub wrapped_key: Vec<u8>,
    pub ciphertext: Vec<u8>,
}
impl fmt::Debug for CredentialEnvelope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CredentialEnvelope")
            .field("metadata", &self.metadata)
            .finish_non_exhaustive()
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VaultAdmission {
    #[default]
    Active,
    Fenced,
}
fn legacy_header_version() -> u32 {
    1
}
#[derive(Clone, Serialize, Deserialize)]
pub struct VaultHeader {
    #[serde(default = "legacy_header_version")]
    pub format_version: u32,
    pub vault_id: String,
    pub key_id: String,
    pub generation: u64,
    pub authentication_generation: u64,
    #[serde(default)]
    pub admission: VaultAdmission,
    #[serde(default)]
    pub admission_owner: Option<String>,
    pub encrypted_check: Vec<u8>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct StoredOperation {
    pub operation: CredentialOperation,
    pub fingerprint: Vec<u8>,
    pub authentication: Vec<u8>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct CredentialRepositorySnapshot {
    pub header: VaultHeader,
    pub records: Vec<CredentialEnvelope>,
    pub operations: Vec<StoredOperation>,
    pub audit: Vec<CredentialAudit>,
}
pub struct CredentialTransferRecord {
    pub metadata: CredentialMetadata,
    pub secret: SecretBytes,
}
/// Canonical state is private memory. Only authenticated transport adapters may encode it.
pub struct CredentialCheckpoint {
    pub access_grants: Vec<Grant>,
    pub source_vault_id: String,
    pub source_generation: u64,
    pub receipt_key: SecretBytes,
    pub records: Vec<CredentialTransferRecord>,
    pub operations: Vec<StoredOperation>,
    pub audit: Vec<CredentialAudit>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CredentialBackendState {
    pub vault_id: String,
    pub generation: u64,
    pub admission: VaultAdmission,
    pub admission_owner: Option<String>,
    pub records: u64,
    pub capabilities: BTreeSet<String>,
}
#[async_trait::async_trait]
pub trait CredentialRepository: Send + Sync {
    async fn header(&self) -> Result<Option<VaultHeader>>;
    async fn initialize(&self, header: VaultHeader) -> Result<VaultHeader>;
    async fn record(&self, reference: &CredentialRef) -> Result<Option<CredentialEnvelope>>;
    async fn operation(&self, id: &str) -> Result<Option<StoredOperation>>;
    async fn commit(
        &self,
        expected_generation: u64,
        expected_revision: u64,
        envelope: CredentialEnvelope,
        operation: StoredOperation,
        audit: CredentialAudit,
    ) -> Result<StoredOperation>;
    async fn snapshot(&self) -> Result<(VaultHeader, Vec<CredentialEnvelope>)>;
    async fn replace(
        &self,
        expected_generation: u64,
        header: VaultHeader,
        records: Vec<CredentialEnvelope>,
        audit: CredentialAudit,
    ) -> Result<()>;
    async fn restore_empty(
        &self,
        expected_generation: u64,
        snapshot: CredentialRepositorySnapshot,
        audit: CredentialAudit,
    ) -> Result<()>;
    async fn snapshot_full(&self) -> Result<CredentialRepositorySnapshot>;
    async fn update_header(
        &self,
        expected_generation: u64,
        header: VaultHeader,
        require_quiescent: bool,
        audit: CredentialAudit,
    ) -> Result<()>;
    async fn replace_all(
        &self,
        expected_generation: u64,
        snapshot: CredentialRepositorySnapshot,
        audit: CredentialAudit,
    ) -> Result<()>;
    async fn audit(&self, after: u64, limit: usize) -> Result<Vec<CredentialAudit>>;
    async fn audit_namespace(
        &self,
        namespace: &str,
        after: u64,
        limit: usize,
    ) -> Result<Vec<CredentialAudit>>;
    async fn append_audit(&self, audit: CredentialAudit) -> Result<()>;
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MigrationStage {
    Requested,
    Validated,
    Preparing,
    Transferring,
    Checkpointed,
    Committed,
    Verified,
    Retired,
    Blocked,
    Cancelled,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CredentialMigrationStatus {
    pub id: String,
    pub stage: MigrationStage,
    pub source_generation: u64,
    pub destination: String,
    pub blocker: Option<String>,
}
#[async_trait::async_trait]
pub trait CredentialMigration: Send + Sync {
    async fn prepare(
        &self,
        access: &CredentialAccess,
        destination: &str,
        operation_id: &str,
        control: &Control<'_>,
    ) -> Result<CredentialMigrationStatus>;
    async fn status(
        &self,
        access: &CredentialAccess,
        operation_id: &str,
        control: &Control<'_>,
    ) -> Result<CredentialMigrationStatus>;
}
#[async_trait::async_trait]
pub trait CredentialTransfer: Send + Sync {
    async fn backend_state(
        &self,
        access: &CredentialAccess,
        control: &Control<'_>,
    ) -> Result<CredentialBackendState>;
    async fn export_checkpoint(
        &self,
        access: &CredentialAccess,
        control: &Control<'_>,
    ) -> Result<CredentialCheckpoint>;
    async fn freeze(
        &self,
        access: &CredentialAccess,
        expected_generation: u64,
        owner: &str,
        control: &Control<'_>,
    ) -> Result<CredentialBackendState>;
    async fn import_checkpoint(
        &self,
        access: &CredentialAccess,
        expected_generation: u64,
        owner: &str,
        checkpoint: CredentialCheckpoint,
        control: &Control<'_>,
    ) -> Result<CredentialBackendState>;
    async fn activate(
        &self,
        access: &CredentialAccess,
        expected_generation: u64,
        owner: &str,
        control: &Control<'_>,
    ) -> Result<CredentialBackendState>;
}
#[async_trait::async_trait]
pub trait CredentialControl: Send + Sync {
    async fn revoke_workload(
        &self,
        _access: &CredentialAccess,
        _principal: &str,
        _expected_revision: u64,
        _operation_id: &str,
        _control: &Control<'_>,
    ) -> Result<CredentialOperation> {
        Err(crate::ServiceError::new(
            crate::ErrorCode::Unsupported,
            "Workload revocation is unsupported",
        ))
    }
    async fn binding_generation(&self, control: &Control<'_>) -> Result<u64>;
    async fn authentication_generation(&self, control: &Control<'_>) -> Result<u64>;
    async fn fence(
        &self,
        access: &CredentialAccess,
        reference: &CredentialRef,
        expected_revision: u64,
        operation_id: &str,
        control: &Control<'_>,
    ) -> Result<CredentialOperation>;
    async fn fail_refresh(
        &self,
        access: &CredentialAccess,
        ticket: &RefreshTicket,
        operation_id: &str,
        control: &Control<'_>,
    ) -> Result<CredentialOperation>;
    async fn export_record(
        &self,
        access: &CredentialAccess,
        reference: &CredentialRef,
        control: &Control<'_>,
    ) -> Result<(CredentialMetadata, SecretBytes)>;
    async fn audits(
        &self,
        access: &CredentialAccess,
        namespace: &str,
        after: u64,
        control: &Control<'_>,
    ) -> Result<Vec<CredentialAudit>>;
}
pub trait CredentialStore:
    CredentialResolver + CredentialAdmin + CredentialRefresh + CredentialControl + CredentialTransfer
{
}
impl<
        T: CredentialResolver
            + CredentialAdmin
            + CredentialRefresh
            + CredentialControl
            + CredentialTransfer,
    > CredentialStore for T
{
}
