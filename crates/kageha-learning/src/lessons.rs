//! Lessons from reflection (design section 15 "Reflection"; Requirements 15.2–15.5).
//!
//! The Host's reflection worker (`kageha-loop::ui_reflection`) claims a closed request
//! from `learn_reflection_queue` ([`SqliteLearning::claim_reflection`]), asks the
//! planner model for up to [`MAX_LESSONS`] lessons and hands the parsed drafts to
//! [`SqliteLearning::store_lessons`]. Everything after the model call happens here:
//!
//! - **Queue.** States are `queued` → `running` → `done` or `failed`. A claim takes a
//!   lease of [`REFLECTION_LEASE_MS`] with a random lease id. A `running` row whose lease
//!   expired (the worker crashed or hung) is claimed again, at most
//!   [`MAX_REFLECTION_ATTEMPTS`] times in total; after that it is marked `failed`. Only
//!   the holder of the current lease can finish or fail a row, so a worker that lost
//!   its lease cannot overwrite the result of the worker that took over.
//! - **Validation.** Each draft needs a task type slug ([`valid_task_type`]), a
//!   non-empty trigger and action whose lengths add up to fewer than
//!   [`MAX_LESSON_CHARS`] characters, and 1 to [`MAX_EVIDENCE`] evidence references,
//!   each the `seq` of an entry in the request's Run_Journal. Invalid drafts are
//!   dropped with a reason, and only the first [`MAX_LESSONS`] valid drafts are kept.
//!   [`sanitize_lesson`] runs last: a lesson holding a credential vault value (from the
//!   [`SecretSource`] passed to `store_lessons`) or a secret pattern
//!   ([`crate::secrets`]) is dropped with the reason `secret:<kind>`, which never holds
//!   the value. A source that cannot be read fails the call and stores nothing.
//! - **Status.** A new lesson is `active` when its request had a verified outcome or a
//!   `repair` entry in its journal ([`run_verified`]), else `candidate`.
//! - **Merging.** A lesson merges into an existing one of the same agent and task type
//!   whose `trigger + " " + action` is similar: cosine ≥ [`COSINE_MERGE`] when a
//!   [`TextEmbedder`] is given and succeeds, else token Jaccard ≥ [`JACCARD_MERGE`].
//!   Merging adds the request to `support` and its evidence to `evidence`; the stored
//!   text is kept. A candidate becomes active when the merged run is verified or when
//!   `support` holds 2 distinct requests (Property 15). A `retired` lesson stays
//!   retired, so a lesson that was retired for doing harm is not learned again.
//! - **Scope.** A lesson belongs to the request's agent. `main` lessons are the global
//!   ones; the learned-context ranking (task 25.1) reads them for every agent.
//!
//! Additions to the design's tables, made by [`migrate`]: `learn_reflection_queue`
//! gains `lease_id`, `lease_until` (Unix ms), `attempts` and `reason`;
//! `learn_lessons` gains `evidence` (JSON `{request id: [seq, ...]}`).
use crate::secrets::{self, Finding, SecretSource};
use crate::store::{now_ms, sql, valid_task_type, SqliteLearning};
use crate::{JournalKind, RequestSummary};
use zeroize::Zeroizing;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    sync::atomic::{AtomicU64, Ordering},
};

/// Most lessons kept from one reflection.
pub const MAX_LESSONS: usize = 5;
/// `trigger` and `action` together must have fewer characters than this.
pub const MAX_LESSON_CHARS: usize = 600;
/// Most evidence references per lesson.
pub const MAX_EVIDENCE: usize = 32;
/// Maximum journal entries inspected by one explicit lesson save.
const MAX_AGENT_EVIDENCE_EVENTS: usize = 10_000;
/// How long a claimed reflection may run before another claim may take it over.
pub const REFLECTION_LEASE_MS: u64 = 15 * 60 * 1000;
/// Claims of one queued request, including takeovers after an expired lease.
pub const MAX_REFLECTION_ATTEMPTS: u32 = 3;
/// Token Jaccard similarity at which two lessons merge.
pub const JACCARD_MERGE: f64 = 0.8;
/// Embedding cosine similarity at which two lessons merge.
pub const COSINE_MERGE: f64 = 0.9;
/// Existing lessons of one agent and task type compared with a new one, newest first.
const MERGE_CANDIDATES: usize = 200;
/// Texts sent in one embedding call (the Host's embedding adapter accepts 32).
const EMBED_BATCH: usize = 32;
const MAX_REASON_CHARS: usize = 512;

/// Adds the lease and evidence columns to a store created before them.
pub(crate) fn migrate(db: &Connection) -> rusqlite::Result<()> {
    let columns = |table: &str| -> rusqlite::Result<BTreeSet<String>> {
        let mut query = db.prepare(&format!("PRAGMA table_info({table})"))?;
        let names = query.query_map([], |r| r.get::<_, String>(1))?;
        names.collect()
    };
    db.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        let queue = columns("learn_reflection_queue")?;
        for (name, declaration) in [
            ("lease_id", "TEXT"),
            ("lease_until", "INTEGER"),
            ("attempts", "INTEGER NOT NULL DEFAULT 0"),
            ("reason", "TEXT"),
        ] {
            if !queue.contains(name) {
                db.execute_batch(&format!(
                    "ALTER TABLE learn_reflection_queue ADD COLUMN {name} {declaration}"
                ))?;
            }
        }
        if !columns("learn_lessons")?.contains("evidence") {
            db.execute_batch(
                "ALTER TABLE learn_lessons ADD COLUMN evidence TEXT NOT NULL DEFAULT '{}'",
            )?;
        }
        if !columns("learn_lessons")?.contains("retirement_reason") {
            db.execute_batch("ALTER TABLE learn_lessons ADD COLUMN retirement_reason TEXT")?;
        }
        db.execute_batch(
            "CREATE INDEX IF NOT EXISTS learn_lessons_type ON learn_lessons(agent,task_type,id);
             CREATE TABLE IF NOT EXISTS learn_reference_key (id INTEGER PRIMARY KEY CHECK(id=1), key BLOB NOT NULL)",
        )
    })();
    match result {
        Ok(()) => db.execute_batch("COMMIT"),
        Err(error) => {
            let _ = db.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

/// A lesson as returned by the planner, before validation. Unknown or missing fields
/// parse to empty values, which validation then rejects.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LessonDraft {
    #[serde(default)]
    pub task_type: String,
    #[serde(default)]
    pub trigger: String,
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub evidence: Vec<u64>,
}

/// Lifecycle state of a stored lesson.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LessonStatus {
    Candidate,
    Active,
    Retired,
}

impl LessonStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::Active => "active",
            Self::Retired => "retired",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        [Self::Candidate, Self::Active, Self::Retired]
            .into_iter()
            .find(|status| status.as_str() == value)
    }
}

/// One row of `learn_lessons`. Times are Unix ms.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lesson {
    pub id: i64,
    pub agent: String,
    pub task_type: Option<String>,
    pub trigger: String,
    pub action: String,
    pub status: LessonStatus,
    /// Distinct supporting request ids, in the order they were added.
    pub support: Vec<String>,
    /// Evidence `seq`s per supporting request.
    pub evidence: BTreeMap<String, Vec<u64>>,
    pub shown: u64,
    pub created: u64,
    pub retired_at: Option<u64>,
}

/// A claimed reflection. `lease` identifies this claim; `attempt` counts claims of the
/// request since it was last queued.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflectionClaim {
    pub request: String,
    pub lease: String,
    pub attempt: u32,
}

/// A row of `learn_reflection_queue`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflectionRow {
    pub state: String,
    pub queued_at: u64,
    pub attempts: u32,
    pub lease_until: Option<u64>,
    pub reason: Option<String>,
}

/// How lesson similarity was measured for one reflection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SimilarityMethod {
    Cosine,
    Jaccard,
}

/// A lesson written by [`SqliteLearning::store_lessons`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredLesson {
    pub id: i64,
    pub status: LessonStatus,
    /// Whether the draft merged into an existing lesson instead of adding one.
    pub merged: bool,
}

/// A draft that was not stored, by its index in the planner's list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DroppedLesson {
    pub index: usize,
    pub reason: String,
}

/// What one reflection stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReflectionReport {
    pub verified: bool,
    pub similarity: SimilarityMethod,
    pub stored: Vec<StoredLesson>,
    pub dropped: Vec<DroppedLesson>,
}

/// Optional embeddings for lesson merging. Each returned vector belongs to the input
/// text at the same index.
pub trait TextEmbedder {
    fn embed(&self, texts: &[&str]) -> io::Result<Vec<Vec<f64>>>;
}

/// The secret scan applied to each validated lesson before it is stored (design section
/// 15 "Safety"): a lesson whose task type, trigger or action holds one of `vault_values`
/// or matches a secret pattern is dropped, not redacted. The error names the kind of
/// match and never holds the matched text.
pub fn sanitize_lesson(draft: LessonDraft, vault_values: &[Zeroizing<String>]) -> Result<LessonDraft, Finding> {
    for text in [&draft.task_type, &draft.trigger, &draft.action] {
        if let Some(finding) = secrets::scan(text, vault_values) {
            return Err(finding);
        }
    }
    Ok(draft)
}

/// Whether a run supports an `active` lesson: it had a verified outcome or a `repair`
/// entry (a failure followed by a success of the same operation) in its journal.
pub fn run_verified(summary: &RequestSummary, journal_kinds: &[JournalKind]) -> bool {
    summary
        .outcomes
        .iter()
        .any(|outcome| outcome.status == "verified")
        || journal_kinds.contains(&JournalKind::Repair)
}

/// `text` with whitespace runs collapsed to one space and trimmed.
fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Checks one draft against the request's journal `seqs`. Returns the normalized
/// lesson (whitespace collapsed, evidence sorted and deduplicated) or the reason it is
/// dropped.
pub fn validate_lesson(draft: &LessonDraft, seqs: &BTreeSet<u64>) -> Result<LessonDraft, String> {
    let task_type = draft.task_type.trim();
    if !valid_task_type(task_type) {
        return Err("invalid_task_type".into());
    }
    let (trigger, action) = (collapse(&draft.trigger), collapse(&draft.action));
    if trigger.is_empty() || action.is_empty() {
        return Err("empty_text".into());
    }
    if trigger.chars().count() + action.chars().count() >= MAX_LESSON_CHARS
        || [&trigger, &action]
            .iter()
            .any(|text| text.chars().any(char::is_control))
    {
        return Err("invalid_text".into());
    }
    let evidence: BTreeSet<u64> = draft.evidence.iter().copied().collect();
    if evidence.is_empty() || evidence.len() > MAX_EVIDENCE {
        return Err("invalid_evidence".into());
    }
    if let Some(missing) = evidence.iter().find(|seq| !seqs.contains(seq)) {
        return Err(format!("unknown_evidence:{missing}"));
    }
    Ok(LessonDraft {
        task_type: task_type.to_owned(),
        trigger,
        action,
        evidence: evidence.into_iter().collect(),
    })
}

/// Lowercase alphanumeric tokens of `text`.
pub fn tokens(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Token Jaccard similarity; 0 when both texts have no tokens.
pub fn jaccard(a: &str, b: &str) -> f64 {
    let (a, b) = (tokens(a), tokens(b));
    let union = a.union(&b).count();
    if union == 0 {
        return 0.0;
    }
    a.intersection(&b).count() as f64 / union as f64
}

/// Cosine similarity; 0 for vectors of different lengths or zero length.
pub fn cosine(a: &[f64], b: &[f64]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f64 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm = |v: &[f64]| v.iter().map(|x| x * x).sum::<f64>().sqrt();
    let denominator = norm(a) * norm(b);
    if !denominator.is_finite() || denominator == 0.0 {
        return 0.0;
    }
    dot / denominator
}

fn lesson_text(trigger: &str, action: &str) -> String {
    format!("{trigger} {action}")
}

fn bounded_reason(reason: &str) -> String {
    let reason = collapse(reason);
    match reason.char_indices().nth(MAX_REASON_CHARS) {
        Some((end, _)) => format!("{}…", &reason[..end]),
        None => reason,
    }
}

fn lease_id(request: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seed = format!(
        "{request}\0{nanos}\0{}\0{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    Sha256::digest(seed.as_bytes())[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn lost_lease() -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        "Reflection lease expired; another claim owns the request",
    )
}

fn to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// An existing lesson considered for a merge.
struct Known {
    id: i64,
    task_type: String,
    text: String,
}

impl SqliteLearning {
    /// Match references without persisting their values. The private key remains in
    /// the learning database and the turn is part of the HMAC domain.
    pub fn lesson_reference(&self, turn: &str, reference: &str) -> io::Result<String> {
        use hmac::{Hmac, Mac};
        let db = self.lock()?;
        let mut key: Option<Vec<u8>> = db.query_row("SELECT key FROM learn_reference_key WHERE id=1", [], |r| r.get(0)).optional().map_err(sql)?;
        if key.is_none() {
            let mut generated = Zeroizing::new([0u8; 32]);
            getrandom::getrandom(&mut *generated).map_err(|e| io::Error::other(format!("Reference key unavailable: {e}")))?;
            db.execute("INSERT OR IGNORE INTO learn_reference_key(id,key) VALUES(1,?)", [generated.as_slice()]).map_err(sql)?;
            key = Some(db.query_row("SELECT key FROM learn_reference_key WHERE id=1", [], |r| r.get(0)).map_err(sql)?);
        }
        let key = Zeroizing::new(key.unwrap_or_default());
        let mut mac = Hmac::<Sha256>::new_from_slice(&key).map_err(|_| io::Error::other("Invalid reference key"))?;
        mac.update(turn.as_bytes());
        mac.update(&[0]);
        mac.update(reference.as_bytes());
        Ok(format!("{:x}", mac.finalize().into_bytes()))
    }

    /// Save a lesson from the current turn, resolving evidence to journal entries.
    /// Only host-recorded call ids and produced receipt references are accepted.
    pub fn save_agent_lesson(&self, agent: &str, request: &str, turn: &str, draft: &LessonDraft,
        references: &[String], supersedes: Option<i64>, secrets: &dyn SecretSource) -> io::Result<StoredLesson> {
        if agent.is_empty() || agent.len() > 128 || request.is_empty() || request.len() > 256 || turn.is_empty() || turn.len() > 256
            || draft.task_type.len() > 64 || draft.trigger.len().saturating_add(draft.action.len()) > MAX_LESSON_CHARS * 4
            || references.is_empty() || references.len() > MAX_EVIDENCE
            || references.iter().any(|s| s.is_empty() || s.len() > 1024) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Invalid lesson scope or evidence"));
        }
        let characters = collapse(&draft.trigger).chars().count() + collapse(&draft.action).chars().count();
        if characters >= MAX_LESSON_CHARS {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("Shorten trigger and action to fewer than {MAX_LESSON_CHARS} characters combined (currently {characters}).")));
        }
        let values = secrets.values()?;
        let mut after = 0;
        let mut scanned = 0;
        let mut evidence = BTreeSet::new();
        let mut matched = BTreeSet::new();
        let mut repaired = false;
        let origin = lesson_origin(turn);
        let digests: Vec<_> = references.iter().map(|reference| {
            let receipt = if reference.starts_with("receipt:") { reference.clone() } else { format!("receipt:{reference}") };
            Ok((reference, self.lesson_reference(turn, reference)?, self.lesson_reference(turn, &receipt)?))
        }).collect::<io::Result<_>>()?;
        let needed = references.iter().collect::<BTreeSet<_>>().len();
        loop {
            let remaining = MAX_AGENT_EVIDENCE_EVENTS - scanned;
            if remaining == 0 { break; }
            let events = self.events(request, after, remaining.min(1000))?;
            if events.is_empty() { break; }
            scanned += events.len();
            for event in &events {
                after = event.seq;
                if event.body["origin_turn"].as_str() != Some(origin.as_str()) { continue; }
                if event.kind == JournalKind::Repair {
                    let failed = event.body["failed_ref"].as_str();
                    let fixed = event.body["repair_ref"].as_str();
                    repaired |= failed.is_some_and(|id| digests.iter().any(|(_, hash, _)| hash.as_str() == id))
                        && fixed.is_some_and(|id| digests.iter().any(|(_, hash, _)| hash.as_str() == id));
                }
                if event.kind != JournalKind::Tool { continue; }
                for (reference, call_ref, receipt_ref) in &digests {
                    let call = event.body["call_ref"].as_str() == Some(call_ref.as_str());
                    let produced = event.body["receipt_refs"].as_array().is_some_and(|items|
                        items.iter().any(|item| item.as_str() == Some(receipt_ref.as_str())));
                    if call || produced { matched.insert((*reference).clone()); evidence.insert(event.seq); }
                }
            }
            if matched.len() == needed || events.len() < remaining.min(1000) { break; }
        }
        if matched.len() != needed {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "Evidence must refer to recorded calls or receipts from this turn"));
        }
        let mut resolved = draft.clone();
        resolved.evidence = evidence.iter().copied().collect();
        let resolved = validate_lesson(&resolved, &evidence).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let resolved = sanitize_lesson(resolved, &values).map_err(|finding| io::Error::new(io::ErrorKind::InvalidInput, format!("secret:{finding}")))?;
        let mut db = self.lock()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        if let Some(id) = supersedes {
            let owned: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM learn_lessons WHERE id=? AND agent=?)", params![id, agent], |r| r.get(0)).map_err(sql)?;
            if !owned { return Err(io::Error::new(io::ErrorKind::NotFound, "Superseded lesson not owned by this agent")); }
        }
        let target = {
            let mut q = tx.prepare("SELECT id,\"trigger\",action,status FROM learn_lessons WHERE agent=? AND task_type=? ORDER BY id DESC LIMIT ?").map_err(sql)?;
            let rows = q.query_map(params![agent, resolved.task_type, MERGE_CANDIDATES as i64], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?))).map_err(sql)?;
            let text = lesson_text(&resolved.trigger, &resolved.action);
            let mut best: Option<(i64, f64)> = None;
            let mut retired_match = false;
            for row in rows { let (id, trigger, action, status) = row.map_err(sql)?; if Some(id) == supersedes { continue; }
                let score = jaccard(&text, &lesson_text(&trigger, &action));
                if score >= JACCARD_MERGE {
                    if status == "retired" { retired_match = true; }
                    else if best.is_none_or(|(_, old)| score > old) { best = Some((id, score)); }
                }
            }
            if best.is_none() && retired_match { return Err(io::Error::new(io::ErrorKind::InvalidInput, "A similar lesson is retired")); }
            best.map(|(id, _)| id)
        };
        let stored = if let Some(id) = target {
            let status = merge(&tx, id, request, &resolved.evidence, repaired)?.ok_or_else(|| io::Error::other("Lesson disappeared"))?;
            StoredLesson { id, status, merged: true }
        } else {
            let status = if repaired { LessonStatus::Active } else { LessonStatus::Candidate };
            tx.execute("INSERT INTO learn_lessons(agent,task_type,\"trigger\",action,status,support,evidence,shown,created) VALUES(?,?,?,?,?,?,?,0,?)",
                params![agent, resolved.task_type, resolved.trigger, resolved.action, status.as_str(), json!([request]).to_string(), json!({(request): resolved.evidence}).to_string(), to_i64(now_ms())]).map_err(sql)?;
            StoredLesson { id: tx.last_insert_rowid(), status, merged: false }
        };
        if let Some(id) = supersedes {
            tx.execute("UPDATE learn_lessons SET status='retired',retired_at=?,retirement_reason='superseded' WHERE id=? AND agent=? AND status<>'retired'", params![to_i64(now_ms()), id, agent]).map_err(sql)?;
        }
        tx.commit().map_err(sql)?;
        Ok(stored)
    }

    /// Retire a lesson belonging to the caller's agent.
    pub fn retire_agent_lesson(&self, agent: &str, id: i64, reason: &str, secrets: &dyn SecretSource) -> io::Result<()> {
        if reason.trim().is_empty() || reason.len() > 512 { return Err(io::Error::new(io::ErrorKind::InvalidInput, "A short retirement reason is required")); }
        if secrets::scan(reason, &secrets.values()?).is_some() { return Err(io::Error::new(io::ErrorKind::InvalidInput, "Retirement reason contains a secret")); }
        let db = self.lock()?;
        let owned: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM learn_lessons WHERE id=? AND agent=?)", params![id, agent], |r| r.get(0)).map_err(sql)?;
        if !owned { return Err(io::Error::new(io::ErrorKind::NotFound, "Lesson not owned by this agent")); }
        db.execute("UPDATE learn_lessons SET status='retired',retired_at=?,retirement_reason=? WHERE id=? AND agent=? AND status<>'retired'", params![to_i64(now_ms()), reason.trim(), id, agent]).map_err(sql)?;
        Ok(())
    }

    /// Activate candidate lessons supported by this now-verified request.
    pub fn verify_agent_lessons(&self, request: &str) -> io::Result<usize> {
        let db = self.lock()?;
        db.execute("UPDATE learn_lessons SET status='active' WHERE status='candidate' AND EXISTS (SELECT 1 FROM json_each(learn_lessons.support) WHERE value=?)", [request]).map_err(sql)
    }
    /// Claims the oldest queued reflection, or a running one whose lease expired.
    pub fn claim_reflection(&self) -> io::Result<Option<ReflectionClaim>> {
        self.claim_reflection_at(now_ms())
    }

    /// [`Self::claim_reflection`] at time `now` (Unix ms).
    pub fn claim_reflection_at(&self, now: u64) -> io::Result<Option<ReflectionClaim>> {
        let now = to_i64(now);
        let mut db = self.lock()?;
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql)?;
        tx.execute(
            "UPDATE learn_reflection_queue SET state='failed',lease_id=NULL,lease_until=NULL,
             reason='Reflection did not finish after '||attempts||' attempts'
             WHERE state='running' AND COALESCE(lease_until,0)<=? AND attempts>=?",
            params![now, MAX_REFLECTION_ATTEMPTS],
        )
        .map_err(sql)?;
        let next: Option<(String, String, u32)> = tx
            .query_row(
                "SELECT request_id,state,attempts FROM learn_reflection_queue
                 WHERE state='queued' OR (state='running' AND COALESCE(lease_until,0)<=?)
                 ORDER BY queued_at,request_id LIMIT 1",
                [now],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(sql)?;
        let Some((request, state, attempts)) = next else {
            tx.commit().map_err(sql)?;
            return Ok(None);
        };
        let attempt = if state == "queued" { 1 } else { attempts + 1 };
        let lease = lease_id(&request);
        tx.execute(
            "UPDATE learn_reflection_queue SET state='running',lease_id=?,lease_until=?,attempts=?,reason=NULL
             WHERE request_id=?",
            params![
                lease,
                now.saturating_add(REFLECTION_LEASE_MS as i64),
                attempt,
                request
            ],
        )
        .map_err(sql)?;
        tx.commit().map_err(sql)?;
        Ok(Some(ReflectionClaim {
            request,
            lease,
            attempt,
        }))
    }

    /// Marks a claimed reflection `failed` with `reason`. Returns `false` when the claim
    /// no longer holds the lease (nothing is changed then).
    pub fn fail_reflection(&self, claim: &ReflectionClaim, reason: &str) -> io::Result<bool> {
        let db = self.lock()?;
        let changed = db
            .execute(
                "UPDATE learn_reflection_queue SET state='failed',reason=?,lease_id=NULL,lease_until=NULL
                 WHERE request_id=? AND state='running' AND lease_id=?",
                params![bounded_reason(reason), claim.request, claim.lease],
            )
            .map_err(sql)?;
        Ok(changed == 1)
    }

    /// The queue row of a request, with its lease and failure reason.
    pub fn reflection_row(&self, request: &str) -> io::Result<Option<ReflectionRow>> {
        let db = self.lock()?;
        db.query_row(
            "SELECT state,queued_at,attempts,lease_until,reason FROM learn_reflection_queue WHERE request_id=?",
            [request],
            |r| {
                Ok(ReflectionRow {
                    state: r.get(0)?,
                    queued_at: r.get::<_, i64>(1)?.max(0) as u64,
                    attempts: r.get(2)?,
                    lease_until: r.get::<_, Option<i64>>(3)?.map(|at| at.max(0) as u64),
                    reason: r.get(4)?,
                })
            },
        )
        .optional()
        .map_err(sql)
    }

    /// Kinds of the request's journal entries by `seq`.
    pub fn journal_kinds(&self, request: &str) -> io::Result<BTreeMap<u64, JournalKind>> {
        let db = self.lock()?;
        let mut query = db
            .prepare("SELECT seq,kind FROM learn_events WHERE request_id=? ORDER BY seq")
            .map_err(sql)?;
        let rows = query
            .query_map([request], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(sql)?;
        rows.map(|row| {
            let (seq, kind) = row.map_err(sql)?;
            let kind = JournalKind::parse(&kind).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "Unknown journal kind")
            })?;
            Ok((seq.max(0) as u64, kind))
        })
        .collect()
    }

    /// Lessons of `agent`, oldest first.
    pub fn lessons(&self, agent: &str) -> io::Result<Vec<Lesson>> {
        let db = self.lock()?;
        let mut query = db
            .prepare(
                "SELECT id,agent,task_type,\"trigger\",action,status,support,evidence,shown,created,retired_at
                 FROM learn_lessons WHERE agent=? ORDER BY id",
            )
            .map_err(sql)?;
        let rows = query
            .query_map([agent], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, String>(7)?,
                    r.get::<_, i64>(8)?,
                    r.get::<_, i64>(9)?,
                    r.get::<_, Option<i64>>(10)?,
                ))
            })
            .map_err(sql)?;
        rows.map(|row| {
            let (
                id,
                agent,
                task_type,
                trigger,
                action,
                status,
                support,
                evidence,
                shown,
                created,
                retired,
            ) = row.map_err(sql)?;
            Ok(Lesson {
                id,
                agent,
                task_type,
                trigger,
                action,
                status: LessonStatus::parse(&status).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "Unknown lesson status")
                })?,
                support: serde_json::from_str(&support)?,
                evidence: serde_json::from_str(&evidence)?,
                shown: shown.max(0) as u64,
                created: created.max(0) as u64,
                retired_at: retired.map(|at| at.max(0) as u64),
            })
        })
        .collect()
    }

    /// Validates `drafts` against the claimed request's journal, stores the valid ones
    /// (merging similar lessons) and marks the reflection `done`, in one transaction.
    /// Fails with `ErrorKind::TimedOut` when the claim lost its lease, and fails when
    /// `secrets` cannot be read; nothing is stored then. See the module documentation
    /// for the rules.
    pub fn store_lessons(
        &self,
        claim: &ReflectionClaim,
        drafts: &[LessonDraft],
        embedder: Option<&dyn TextEmbedder>,
        secrets: &dyn SecretSource,
    ) -> io::Result<ReflectionReport> {
        // Read once for all drafts; the values are zeroized when this call returns.
        let vault_values = secrets.values().map_err(|error| {
            io::Error::new(error.kind(), format!("The secret scan could not read the credential vault: {error}"))
        })?;
        let summary = self
            .run(&claim.request)?
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Request has no run row"))?;
        let journal = self.journal_kinds(&claim.request)?;
        let kinds: Vec<JournalKind> = journal.values().copied().collect();
        let verified = run_verified(&summary, &kinds);
        let seqs: BTreeSet<u64> = journal.keys().copied().collect();

        let mut dropped = Vec::new();
        let mut valid = Vec::new();
        for (index, draft) in drafts.iter().enumerate() {
            if valid.len() == MAX_LESSONS {
                dropped.push(DroppedLesson {
                    index,
                    reason: "over_limit".into(),
                });
                continue;
            }
            match validate_lesson(draft, &seqs).map(|lesson| sanitize_lesson(lesson, &vault_values)) {
                Ok(Ok(lesson)) => valid.push(lesson),
                Ok(Err(finding)) => dropped.push(DroppedLesson {
                    index,
                    reason: format!("secret:{finding}"),
                }),
                Err(reason) => dropped.push(DroppedLesson { index, reason }),
            }
        }

        let task_types: BTreeSet<&str> = valid.iter().map(|d| d.task_type.as_str()).collect();
        let known = self.merge_candidates(&summary.agent, &task_types)?;
        // Known lessons, then the earlier drafts of this batch, so duplicates in one
        // reply merge too.
        let mut texts: Vec<String> = known.iter().map(|k| k.text.clone()).collect();
        texts.extend(valid.iter().map(|d| lesson_text(&d.trigger, &d.action)));
        let vectors = embedder.and_then(|embedder| embed_all(embedder, &texts));
        let similarity = if vectors.is_some() {
            SimilarityMethod::Cosine
        } else {
            SimilarityMethod::Jaccard
        };
        // Similarity of texts `a` and `b` when it reaches the merge threshold.
        let similar = |a: usize, b: usize| {
            let (score, threshold) = match &vectors {
                Some(vectors) => (cosine(&vectors[a], &vectors[b]), COSINE_MERGE),
                None => (jaccard(&texts[a], &texts[b]), JACCARD_MERGE),
            };
            (score >= threshold).then_some(score)
        };
        // Each draft merges into the most similar known lesson or earlier draft of the
        // same task type (an index into `texts`).
        let mut targets: Vec<Option<usize>> = Vec::with_capacity(valid.len());
        for (index, draft) in valid.iter().enumerate() {
            let me = known.len() + index;
            let task_type = |other: usize| {
                if other < known.len() {
                    known[other].task_type.as_str()
                } else {
                    valid[other - known.len()].task_type.as_str()
                }
            };
            let best = (0..me)
                .filter(|&other| task_type(other) == draft.task_type)
                .filter_map(|other| similar(me, other).map(|score| (other, score)))
                .fold(None::<(usize, f64)>, |best, item| match best {
                    Some((_, score)) if score >= item.1 => best,
                    _ => Some(item),
                });
            targets.push(best.map(|(other, _)| other));
        }

        let now = to_i64(now_ms());
        let mut db = self.lock()?;
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql)?;
        let held: Option<String> = tx
            .query_row(
                "SELECT lease_id FROM learn_reflection_queue WHERE request_id=? AND state='running'",
                [&claim.request],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql)?
            .flatten();
        if held.as_deref() != Some(claim.lease.as_str()) {
            return Err(lost_lease());
        }
        let mut stored: Vec<StoredLesson> = Vec::with_capacity(valid.len());
        for (index, draft) in valid.iter().enumerate() {
            // A draft matching an earlier draft merges into that draft's lesson.
            let target = targets[index].map(|other| {
                if other < known.len() {
                    known[other].id
                } else {
                    stored[other - known.len()].id
                }
            });
            let merged = match target {
                Some(id) => merge(&tx, id, &claim.request, &draft.evidence, verified)?,
                None => None,
            };
            let lesson = match merged {
                Some(status) => StoredLesson {
                    id: target.unwrap_or_default(),
                    status,
                    merged: true,
                },
                None => {
                    let status = if verified {
                        LessonStatus::Active
                    } else {
                        LessonStatus::Candidate
                    };
                    tx.execute(
                        "INSERT INTO learn_lessons(agent,task_type,\"trigger\",action,status,support,evidence,shown,created)
                         VALUES(?,?,?,?,?,?,?,0,?)",
                        params![
                            summary.agent,
                            draft.task_type,
                            draft.trigger,
                            draft.action,
                            status.as_str(),
                            json!([claim.request]).to_string(),
                            json!({ &claim.request: draft.evidence }).to_string(),
                            now,
                        ],
                    )
                    .map_err(sql)?;
                    StoredLesson {
                        id: tx.last_insert_rowid(),
                        status,
                        merged: false,
                    }
                }
            };
            stored.push(lesson);
        }
        tx.execute(
            "UPDATE learn_reflection_queue SET state='done',lease_id=NULL,lease_until=NULL,reason=NULL
             WHERE request_id=? AND lease_id=?",
            params![claim.request, claim.lease],
        )
        .map_err(sql)?;
        tx.commit().map_err(sql)?;
        Ok(ReflectionReport {
            verified,
            similarity,
            stored,
            dropped,
        })
    }

    /// The newest [`MERGE_CANDIDATES`] lessons of `agent` for each task type.
    fn merge_candidates(&self, agent: &str, task_types: &BTreeSet<&str>) -> io::Result<Vec<Known>> {
        let db = self.lock()?;
        let mut query = db
            .prepare(
                "SELECT id,\"trigger\",action FROM learn_lessons WHERE agent=? AND task_type=? ORDER BY id DESC LIMIT ?",
            )
            .map_err(sql)?;
        let mut known = Vec::new();
        for task_type in task_types {
            let rows = query
                .query_map(params![agent, task_type, MERGE_CANDIDATES as i64], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })
                .map_err(sql)?;
            for row in rows {
                let (id, trigger, action) = row.map_err(sql)?;
                known.push(Known {
                    id,
                    task_type: (*task_type).to_owned(),
                    text: lesson_text(&trigger, &action),
                });
            }
        }
        Ok(known)
    }
}

/// Non-sensitive provenance used to bind a journal result to its originating turn.
pub fn lesson_origin(turn: &str) -> String {
    format!("{:x}", Sha256::digest(turn.as_bytes()))
}

/// Embeds `texts` in batches; `None` when any batch fails or returns the wrong shape,
/// so the caller falls back to Jaccard similarity for the whole reflection.
fn embed_all(embedder: &dyn TextEmbedder, texts: &[String]) -> Option<Vec<Vec<f64>>> {
    let mut vectors = Vec::with_capacity(texts.len());
    for batch in texts.chunks(EMBED_BATCH) {
        let inputs: Vec<&str> = batch.iter().map(String::as_str).collect();
        let result = embedder.embed(&inputs).ok()?;
        if result.len() != inputs.len() {
            return None;
        }
        vectors.extend(result);
    }
    Some(vectors)
}

/// Adds `request` and its evidence to lesson `id` and updates its status. Returns the
/// new status, or `None` when the lesson no longer exists.
fn merge(
    tx: &rusqlite::Transaction<'_>,
    id: i64,
    request: &str,
    evidence: &[u64],
    verified: bool,
) -> io::Result<Option<LessonStatus>> {
    let row: Option<(String, String, String)> = tx
        .query_row(
            "SELECT status,support,evidence FROM learn_lessons WHERE id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(sql)?;
    let Some((status, support, stored_evidence)) = row else {
        return Ok(None);
    };
    let status = LessonStatus::parse(&status)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Unknown lesson status"))?;
    let mut support: Vec<String> = serde_json::from_str(&support)?;
    if !support.iter().any(|known| known == request) {
        support.push(request.to_owned());
    }
    let mut all_evidence: BTreeMap<String, Vec<u64>> =
        serde_json::from_str::<Value>(&stored_evidence)
            .ok()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default();
    let seqs = all_evidence.entry(request.to_owned()).or_default();
    seqs.extend_from_slice(evidence);
    seqs.sort_unstable();
    seqs.dedup();
    let status = match status {
        LessonStatus::Retired => LessonStatus::Retired,
        LessonStatus::Active => LessonStatus::Active,
        LessonStatus::Candidate if verified || support.len() >= 2 => LessonStatus::Active,
        LessonStatus::Candidate => LessonStatus::Candidate,
    };
    tx.execute(
        "UPDATE learn_lessons SET status=?,support=?,evidence=? WHERE id=?",
        params![
            status.as_str(),
            serde_json::to_string(&support)?,
            serde_json::to_string(&all_evidence)?,
            id
        ],
    )
    .map_err(sql)?;
    Ok(Some(status))
}

#[cfg(test)]
#[path = "lessons_tests.rs"]
mod tests;
