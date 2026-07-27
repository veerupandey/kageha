//! Run journal, failure index, lessons and skill promotion (Requirements 15–17).
//!
//! The Host talks to this crate only through [`LearningPort`], so the agent
//! runtime never depends on the learning store. Storage lives in
//! `learning.sqlite` (see [`store`]). Lessons and skills produced here are
//! data for the model context; nothing in this crate can change approval
//! policy, tool permissions or the system prompt's safety rules. Lessons and skill files
//! that hold a credential vault value or a secret pattern are dropped before they are
//! stored ([`secrets`]).
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io;

pub mod context;
pub mod failures;
pub mod lessons;
pub mod metrics;
pub mod retirement;
pub mod secrets;
pub mod skill_uses;
pub mod skills;
pub mod store;
pub use context::{SkillCandidate, CONTEXT_HEADER};
pub use failures::{fix_hash, normalize, StoredFix, MAX_FIX_STEPS, MAX_STEP_BYTES};
pub use lessons::{
    sanitize_lesson, LessonDraft, LessonStatus, ReflectionClaim, ReflectionReport, TextEmbedder,
    MAX_LESSONS,
};
pub use metrics::{iso_week, WeeklyMetrics, WeeklySpeedMetrics, DEFAULT_METRICS_WEEKS, MAX_METRICS_WEEKS};
pub use retirement::should_retire;
pub use secrets::{contains_secret, Finding, NoSecrets, SecretSource, SecretValues};
pub use store::{
    default_path, valid_task_type, SqliteLearning, StoredEvent, MAX_EVENTS_PER_REQUEST,
    MAX_EVENT_BODY_BYTES,
};

/// Maximum size of the rendered [`LearnedContext::text`] block.
pub const MAX_CONTEXT_BYTES: usize = 4 * 1024;

/// Entry points the Host calls. Implementations must be safe to share
/// between request tasks.
pub trait LearningPort: Send + Sync {
    /// Appends one ordered entry to the request's Run_Journal.
    fn journal(&self, request: &str, entry: JournalEntry) -> io::Result<()>;
    /// The best recorded fix for a failure signature, if one qualifies.
    fn known_fix(&self, signature: &FailureSignature) -> io::Result<Option<KnownFix>>;
    /// The learned-context block for the start of a request, at most
    /// [`MAX_CONTEXT_BYTES`].
    fn context_for(
        &self,
        agent: &str,
        request_text: &str,
        task_type: Option<&str>,
    ) -> io::Result<LearnedContext>;
    /// Records the request's final summary and queues reflection.
    fn close_request(&self, request: &str, summary: RequestSummary) -> io::Result<()>;
}

/// The kind of a Run_Journal entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalKind {
    Tool,
    ShellCommand,
    Failure,
    Repair,
    Escalation,
    Recovery,
    Gate,
    Outcome,
    LessonUsed,
    SkillUsed,
}

impl JournalKind {
    pub const ALL: [JournalKind; 10] = [
        Self::Tool,
        Self::ShellCommand,
        Self::Failure,
        Self::Repair,
        Self::Escalation,
        Self::Recovery,
        Self::Gate,
        Self::Outcome,
        Self::LessonUsed,
        Self::SkillUsed,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tool => "tool",
            Self::ShellCommand => "shell_command",
            Self::Failure => "failure",
            Self::Repair => "repair",
            Self::Escalation => "escalation",
            Self::Recovery => "recovery",
            Self::Gate => "gate",
            Self::Outcome => "outcome",
            Self::LessonUsed => "lesson_used",
            Self::SkillUsed => "skill_used",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == value)
    }
}

/// One Run_Journal entry. `at` is Unix time in milliseconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub kind: JournalKind,
    pub body: Value,
    pub at: u64,
}

/// Inputs to the failure signature
/// `sha256(tool, error_code, normalize(message), os, arch)`.
/// Build it with [`FailureSignature::from_failure`]; see [`failures`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FailureSignature {
    pub tool: String,
    pub error_code: String,
    pub message: String,
    pub os: String,
    pub arch: String,
}

impl FailureSignature {
    /// Hex sha256 over the length-prefixed fields, with `message` passed
    /// through [`normalize`], so field boundaries cannot be shifted to forge
    /// a collision.
    pub fn digest(&self) -> String {
        let message = normalize(&self.message);
        let mut hash = Sha256::new();
        for field in [
            &self.tool,
            &self.error_code,
            &message,
            &self.os,
            &self.arch,
        ] {
            hash.update((field.len() as u64).to_be_bytes());
            hash.update(field.as_bytes());
        }
        hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// A fix learned from a failure-then-success sequence. `last_seen` is Unix ms.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnownFix {
    pub steps: Vec<Value>,
    pub successes: u64,
    pub last_seen: u64,
}

/// The rendered learned-context block and the ids it shows.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LearnedContext {
    pub text: String,
    pub lesson_ids: Vec<i64>,
    pub skill_ids: Vec<String>,
}

/// Final status of one required outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeStatus {
    pub id: String,
    pub status: String,
}

/// Per-request summary written to `learn_runs`. Times are Unix ms.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestSummary {
    pub agent: String,
    pub chat_id: String,
    #[serde(default)]
    pub task_type: Option<String>,
    pub status: String,
    pub model_calls: u64,
    pub cost_usd: f64,
    pub cached_tokens: u64,
    #[serde(default)]
    pub outcomes: Vec<OutcomeStatus>,
    pub started: u64,
    pub ended: u64,
}
