//! Framework-independent service contracts. No transport, database or application imports.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeSet;
pub const VERSION: u32 = 1;
pub mod agents;
pub mod request_budget;
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    NotFound,
    Invalid,
    Unauthorized,
    Conflict,
    Unsupported,
    Unavailable,
    Deadline,
    Cancelled,
    StaleGeneration,
    Uncertain,
    VerificationFailed,
}
#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
#[error("{message}")]
pub struct ServiceError {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
}
impl ServiceError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: false,
        }
    }
}
pub type Result<T> = std::result::Result<T, ServiceError>;
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Descriptor {
    pub protocol_version: u32,
    pub instance_id: String,
    pub component: String,
    pub adapter: String,
    pub capabilities: BTreeSet<String>,
}
impl Descriptor {
    pub fn require(&self, capabilities: &BTreeSet<String>) -> Result<()> {
        if self.protocol_version != VERSION {
            return Err(ServiceError::new(
                ErrorCode::Unsupported,
                "Service protocol version mismatch",
            ));
        }
        if !capabilities.is_subset(&self.capabilities) {
            return Err(ServiceError::new(
                ErrorCode::Unsupported,
                "Replacement lacks required capabilities",
            ));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryAccess {
    pub namespace: String,
    pub actor: String,
}
impl MemoryAccess {
    pub fn validate(&self) -> Result<()> {
        if self.namespace.is_empty()
            || self.actor.is_empty()
            || self.namespace.len() > 512
            || self.actor.len() > 128
            || self
                .namespace
                .chars()
                .chain(self.actor.chars())
                .any(char::is_control)
        {
            return Err(ServiceError::new(
                ErrorCode::Invalid,
                "A bounded namespace and authenticated actor are required",
            ));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryProvenance {
    pub actor: String,
    pub origin: String,
    pub source_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryRecord {
    pub id: String,
    pub namespace: String,
    pub owner: String,
    pub kind: String,
    pub body: String,
    #[serde(default)]
    pub metadata: Map<String, Value>,
    #[serde(default)]
    pub acl: BTreeSet<String>,
    pub revision: u64,
    pub created_ms: u64,
    pub updated_ms: u64,
    /// Immutable creation provenance survives updates, deletion and migration.
    #[serde(default)]
    pub provenance: Option<MemoryProvenance>,
    #[serde(default)]
    pub tombstone: bool,
}
impl MemoryRecord {
    pub fn visible(&self, access: &MemoryAccess) -> bool {
        self.namespace == access.namespace
            && (self.owner == access.actor || self.acl.contains(&access.actor))
            && !self.tombstone
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PutMemory {
    pub request_id: String,
    pub access: MemoryAccess,
    pub id: String,
    pub kind: String,
    pub body: String,
    #[serde(default)]
    pub metadata: Map<String, Value>,
    #[serde(default)]
    pub acl: BTreeSet<String>,
    pub expected_revision: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteMemory {
    pub request_id: String,
    pub access: MemoryAccess,
    pub id: String,
    pub expected_revision: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemorySnapshot {
    pub schema_version: u32,
    pub watermark: u64,
    pub records: Vec<MemoryRecord>,
    #[serde(default)]
    pub receipts: Vec<MemoryReceipt>,
}
impl MemorySnapshot {
    /// Portable export order is not an implementation identity or a search ranking.
    pub fn canonicalize(&mut self) {
        self.records
            .sort_by(|a, b| (&a.namespace, &a.id).cmp(&(&b.namespace, &b.id)));
        self.receipts.sort_by(|a, b| {
            (&a.namespace, &a.actor, &a.request_id).cmp(&(&b.namespace, &b.actor, &b.request_id))
        });
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryReceipt {
    pub namespace: String,
    pub actor: String,
    pub request_id: String,
    pub digest: String,
    pub record: MemoryRecord,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryPage {
    pub records: Vec<MemoryRecord>,
    pub next: Option<String>,
}
#[async_trait::async_trait]
pub trait MemoryPort: Send + Sync {
    /// Backend-enforced writer authority. Epochs never move backwards.
    async fn fence_status(&self) -> Result<Option<MemoryFence>> {
        Err(ServiceError::new(
            ErrorCode::Unsupported,
            "Writer fencing is unavailable",
        ))
    }
    async fn claim_fence(
        &self,
        owner: &str,
        expected_epoch: u64,
        request_id: &str,
    ) -> Result<MemoryFence> {
        let _ = (owner, expected_epoch, request_id);
        Err(ServiceError::new(
            ErrorCode::Unsupported,
            "Writer fencing is unavailable",
        ))
    }
    async fn retire_fence(&self, fence: &MemoryFence) -> Result<()> {
        let _ = fence;
        Err(ServiceError::new(
            ErrorCode::Unsupported,
            "Writer fencing is unavailable",
        ))
    }
    fn with_fence(&self, fence: MemoryFence) -> Result<std::sync::Arc<dyn MemoryPort>> {
        let _ = fence;
        Err(ServiceError::new(
            ErrorCode::Unsupported,
            "Writer fencing is unavailable",
        ))
    }
    async fn descriptor(&self) -> Result<Descriptor>;
    async fn put(&self, request: PutMemory) -> Result<MemoryRecord>;
    async fn get(&self, access: &MemoryAccess, id: &str) -> Result<Option<MemoryRecord>>;
    async fn delete(&self, request: DeleteMemory) -> Result<MemoryRecord>;
    async fn search(
        &self,
        access: &MemoryAccess,
        query: &str,
        limit: usize,
    ) -> Result<Vec<MemoryRecord>>;
    async fn list(
        &self,
        access: &MemoryAccess,
        after: Option<String>,
        limit: usize,
    ) -> Result<MemoryPage>;
    /// Controller-only portable migration operations; never exposed as agent tools.
    async fn snapshot(&self) -> Result<MemorySnapshot>;
    async fn import(&self, snapshot: MemorySnapshot) -> Result<()>;
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryFence {
    pub owner: String,
    pub epoch: u64,
    pub retired: bool,
    pub claim_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReplacementStage {
    Requested,
    Validated,
    Preparing,
    Transferring,
    Checkpointed,
    Committed,
    Verified,
    Retired,
    Blocked,
    RolledBack,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Binding {
    pub name: String,
    pub provider_id: String,
    pub generation: u64,
    pub required: BTreeSet<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Replacement {
    pub id: String,
    pub binding: String,
    pub source: String,
    pub destination: String,
    pub expected_generation: u64,
    pub stage: ReplacementStage,
    pub error: Option<ServiceError>,
    pub generation: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Routed<T> {
    pub binding: String,
    pub provider_id: String,
    pub generation: u64,
    pub value: T,
}
pub fn memory_capabilities() -> BTreeSet<String> {
    [
        "memory.records",
        "memory.search.lexical",
        "memory.pagination",
        "memory.snapshot.v1",
        "memory.tombstones",
        "memory.revisions",
        "memory.idempotency",
        "memory.fencing.v1",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum MemoryCall {
    Put {
        request: PutMemory,
    },
    Get {
        access: MemoryAccess,
        id: String,
    },
    Delete {
        request: DeleteMemory,
    },
    Search {
        access: MemoryAccess,
        query: String,
        limit: usize,
    },
    List {
        access: MemoryAccess,
        after: Option<String>,
        limit: usize,
    },
    Snapshot,
    Import {
        snapshot: MemorySnapshot,
    },
}

pub mod execution;

pub mod policy;
pub mod credentials;

/// A logical memory binding. Consumers never retain a permanent backend client.
#[async_trait::async_trait]
pub trait LogicalMemoryPort: Send + Sync {
    fn binding(&self) -> Result<Binding>;
    async fn call(
        &self,
        expected_generation: Option<u64>,
        call: MemoryCall,
    ) -> Result<Routed<Value>>;
}

pub mod knowledge;
