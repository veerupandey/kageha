//! Policy and approval ports shared by embedded and independent hosts.
use crate::execution::Control;
use serde::{Deserialize, Serialize};
use std::io;
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Authority {
    pub session: String,
    pub turn: String,
    pub binding: String,
    pub generation: u64,
    pub resource: String,
    pub input_digest: String,
}
impl Authority {
    pub fn validate(&self) -> io::Result<()> {
        if self.generation == 0
            || [&self.session, &self.turn, &self.binding]
                .iter()
                .any(|s| s.is_empty() || s.len() > 4096 || s.chars().any(char::is_control))
            || self.resource.is_empty()
            || self.resource.len() > 65536
            || self.resource.chars().any(char::is_control)
            || self.input_digest.len() != 64
            || !self.input_digest.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Invalid resource authority",
            ));
        }
        Ok(())
    }
    /// Call after approval and before input; approval is not a replacement lease.
    pub fn verify_current(&self, current: &Self) -> io::Result<()> {
        self.validate()?;
        current.validate()?;
        if self != current {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Resource authority changed after authorization",
            ));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionClass {
    /// Specialist policy raised risk: requires a one-use operator decision.
    Restricted,
    ReadOnly,
    RoutineFill,
    Mutation,
    Upload,
    Plan,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    Ask,
    Routine,
    Full,
}
/// Chat-scoped user choice. Legacy routine grants remain separate from review.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PermissionProfile {
    Ask,
    AutoReview,
    Full,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewVerdict {
    Allow,
    Deny,
    Ask,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewDecision {
    pub input_digest: String,
    pub verdict: ReviewVerdict,
    pub rationale: String,
}
/// A separate invocation, with no tool execution or authority to change policy.
pub trait ReviewPort {
    /// An exact user retry still requires this independent review.
    fn review_retry(
        &self,
        request: &ApprovalRequest,
        control: &Control<'_>,
    ) -> io::Result<ReviewDecision> {
        self.review(request, control)
    }
    fn review(
        &self,
        request: &ApprovalRequest,
        control: &Control<'_>,
    ) -> io::Result<ReviewDecision>;
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GrantScope {
    Once,
    Session,
    Full,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovalRequest {
    pub authority: Authority,
    pub action: String,
    pub class: ActionClass,
    /// User-facing description; never include passwords or raw file contents.
    pub detail: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Authorization {
    Allow,
    RequireApproval,
    Deny,
}
pub trait PolicyPort {
    /// Classifications and permission mode come from trusted host configuration.
    fn evaluate(
        &self,
        request: &ApprovalRequest,
        mode: PermissionMode,
    ) -> io::Result<Authorization>;
}
pub trait ApprovalPort {
    /// Persist the exact request, await a decision and authorize this invocation once.
    /// Implementations must honor cancellation/deadlines, fence late decisions and
    /// must not turn a routine grant into approval for another destination/input.
    fn authorize_once(&self, request: &ApprovalRequest, control: &Control<'_>) -> io::Result<bool>;
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalState {
    Pending,
    Approved,
    Denied,
    Consumed,
    Cancelled,
    Interrupted,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovalRecord {
    pub id: String,
    pub request: ApprovalRequest,
    pub state: ApprovalState,
    pub revision: u64,
    pub decision: Option<bool>,
    pub scope: GrantScope,
    pub automatic: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalDecision {
    pub expected_revision: u64,
    pub authority: Authority,
    pub approved: bool,
    pub scope: GrantScope,
}

/// A policy stop is terminal for this turn, not a tool error the agent can route around.
#[derive(Debug)]
pub struct PolicyStop(pub String);
impl std::fmt::Display for PolicyStop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for PolicyStop {}
pub fn stop(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, PolicyStop(message.into()))
}
pub fn is_stop(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|e| e.is::<PolicyStop>() || e.is::<crate::execution::UncertainStop>())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewEvidence {
    pub source: String,
    pub digest: String,
    pub summary: String,
}
