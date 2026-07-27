//! Skill use tracking and degradation (design section 15 "Tracking use" and
//! "Degrading"; Requirements 16.4 and 16.5; task 24.2).
//!
//! - **Recording.** The Host records each successful `skill_read` (and its alias
//!   `load_skill`) in `learn_skill_uses` with [`SqliteLearning::record_skill_use`]: the
//!   skill, the registry revision active at read time, the request id and the read time.
//!   `success` stays `NULL` until the request ends. The table's key is
//!   `(skill, request_id)`, so the first read of a skill in a request is its use. The
//!   Host also journals a `skill_used` entry `{skill, revision, call_id, tool}` whose
//!   `seq` is the read's position in the request ([`skill_read_positions`]).
//! - **Deciding.** When the request closes, each use is decided with [`use_succeeded`]: a
//!   use succeeded when an outcome whose evidence was produced after the read is
//!   verified. Evidence positions come from the journal ([`evidence_positions`]). A
//!   request without outcomes counts as a success when its final status is `verified`
//!   or `not_required`. [`SqliteLearning::decide_skill_use`] writes 1 or 0.
//! - **Degrading.** [`should_degrade`] is the rule: a skill is degraded when it has at
//!   least [`DEGRADE_WINDOW`] decided uses and fewer than [`DEGRADE_MIN_SUCCESSES`] of
//!   the last [`DEGRADE_WINDOW`] succeeded. With fewer decided uses there is no "last
//!   10" yet, so the skill is not degraded. The window counts only the uses of the
//!   current package ([`SqliteLearning::recent_skill_results`] with the registry's
//!   `content_revision`), so a newly installed package starts with a clean record.
//!   The degraded flag and the restored revision live in the `kageha-skills` registry
//!   (`Registry::degrade`), next to the revisions it restores.
use crate::store::{sql, SqliteLearning};
use crate::{JournalKind, StoredEvent};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, io};

/// Uses in the degradation window.
pub const DEGRADE_WINDOW: usize = 10;
/// Successes in the window below which a skill is degraded (60% of 10).
pub const DEGRADE_MIN_SUCCESSES: usize = 6;
/// Request statuses that count a use as a success when the request had no outcomes.
pub const SUCCESS_STATUSES: [&str; 2] = ["verified", "not_required"];
/// Tools whose reads are skill uses.
pub const SKILL_READ_TOOLS: [&str; 2] = ["skill_read", "load_skill"];
const MAX_NAME_BYTES: usize = 128;

/// One row of `learn_skill_uses`. `at` is Unix ms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillUse {
    pub skill: String,
    pub revision: u64,
    pub request_id: String,
    /// `None` until the request is decided.
    pub success: Option<bool>,
    pub at: u64,
}

/// Whether a skill whose decided uses are `recent` (newest first) is degraded: at least
/// [`DEGRADE_WINDOW`] uses, and fewer than [`DEGRADE_MIN_SUCCESSES`] successes among the
/// newest [`DEGRADE_WINDOW`]. Older uses do not count.
pub fn should_degrade(recent: &[bool]) -> bool {
    recent.len() >= DEGRADE_WINDOW
        && recent.iter().take(DEGRADE_WINDOW).filter(|success| **success).count()
            < DEGRADE_MIN_SUCCESSES
}

/// One required outcome of a request, for deciding a skill use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeEvidence {
    pub verified: bool,
    /// The journal position (`seq`) at which each piece of the outcome's evidence was
    /// produced. `None` is evidence whose producing entry is not in the journal; it
    /// counts as produced after any read, since the journal cannot place it earlier.
    /// An outcome without evidence (an answer, proven by the final reply) was produced at
    /// the end of the request.
    pub produced: Vec<Option<u64>>,
}

/// Whether a skill use read at journal position `read` succeeded (see the module
/// documentation). `read` is `None` when the read's `skill_used` entry is missing; then
/// all evidence counts as produced after it.
pub fn use_succeeded(read: Option<u64>, outcomes: &[OutcomeEvidence], final_status: &str) -> bool {
    if outcomes.is_empty() {
        return SUCCESS_STATUSES.contains(&final_status);
    }
    let after = |produced: &Option<u64>| match (produced, read) {
        (Some(at), Some(read)) => *at > read,
        _ => true,
    };
    outcomes
        .iter()
        .any(|outcome| outcome.verified && (outcome.produced.is_empty() || outcome.produced.iter().any(after)))
}

/// The journal position of the first `skill_read` of each skill: `skill_used` entries
/// written for a read tool ([`SKILL_READ_TOOLS`]). Entries for skills only shown in the
/// learned context are not reads.
pub fn skill_read_positions<'a>(events: impl IntoIterator<Item = &'a StoredEvent>) -> BTreeMap<String, u64> {
    let mut positions = BTreeMap::new();
    for event in events {
        if event.kind != JournalKind::SkillUsed
            || !event.body["tool"].as_str().is_some_and(|tool| SKILL_READ_TOOLS.contains(&tool))
        {
            continue;
        }
        if let Some(skill) = event.body["skill"].as_str() {
            positions.entry(skill.to_owned()).or_insert(event.seq);
        }
    }
    positions
}

/// The references each journal entry produced, mapped to the position of the last
/// entry that produced them: the `call_id` of `tool` and `shell_command` entries, the
/// `files_changed` paths of `shell_command` entries, and the `produced` references
/// (artifact paths, receipt ids) of `tool` entries. A later entry that rewrites a file
/// moves its position.
pub fn evidence_positions<'a>(events: impl IntoIterator<Item = &'a StoredEvent>) -> BTreeMap<String, u64> {
    let mut positions = BTreeMap::new();
    for event in events {
        if !matches!(event.kind, JournalKind::Tool | JournalKind::ShellCommand) {
            continue;
        }
        let body = &event.body;
        // A failed call produced nothing that verifies.
        if body["status"] == "failed" {
            continue;
        }
        let mut add = |reference: &str| {
            if !reference.is_empty() {
                positions.insert(reference.to_owned(), event.seq);
            }
        };
        if let Some(id) = body["call_id"].as_str() {
            add(id);
        }
        for key in ["files_changed", "produced"] {
            for item in body[key].as_array().into_iter().flatten() {
                if let Some(reference) = item.as_str().or_else(|| item["artifact_path"].as_str()) {
                    add(reference);
                }
            }
        }
    }
    positions
}

fn check(value: &str, what: &str) -> io::Result<()> {
    if value.is_empty() || value.len() > MAX_NAME_BYTES * 2 || value.chars().any(char::is_control) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("Invalid {what}")));
    }
    Ok(())
}

fn to_i64(value: u64) -> io::Result<i64> {
    i64::try_from(value).map_err(sql)
}

impl SqliteLearning {
    /// Records a read of `skill` at `revision` in `request`, undecided. Returns `false`
    /// when the request already used the skill; the first read is kept.
    pub fn record_skill_use(&self, skill: &str, revision: u64, request: &str, at: u64) -> io::Result<bool> {
        check(skill, "skill name")?;
        check(request, "request id")?;
        let inserted = self
            .lock()?
            .execute(
                "INSERT OR IGNORE INTO learn_skill_uses(skill,revision,request_id,success,at) VALUES(?,?,?,NULL,?)",
                params![skill, to_i64(revision)?, request, to_i64(at)?],
            )
            .map_err(sql)?;
        Ok(inserted > 0)
    }

    /// The skill uses of `request`, by skill.
    pub fn skill_uses(&self, request: &str) -> io::Result<Vec<SkillUse>> {
        let db = self.lock()?;
        let mut query = db
            .prepare(
                "SELECT skill,revision,success,at FROM learn_skill_uses WHERE request_id=? ORDER BY skill LIMIT 1024",
            )
            .map_err(sql)?;
        let rows = query
            .query_map([request], |r| {
                Ok(SkillUse {
                    skill: r.get(0)?,
                    revision: r.get::<_, i64>(1)?.max(0) as u64,
                    request_id: request.to_owned(),
                    success: r.get::<_, Option<i64>>(2)?.map(|success| success != 0),
                    at: r.get::<_, i64>(3)?.max(0) as u64,
                })
            })
            .map_err(sql)?;
        rows.collect::<Result<_, _>>().map_err(sql)
    }

    /// Writes whether the use of `skill` in `request` succeeded. A request closed again
    /// (after a resume) is decided again. Returns `false` when there is no such use.
    pub fn decide_skill_use(&self, skill: &str, request: &str, success: bool) -> io::Result<bool> {
        let updated = self
            .lock()?
            .execute(
                "UPDATE learn_skill_uses SET success=? WHERE skill=? AND request_id=?",
                params![i64::from(success), skill, request],
            )
            .map_err(sql)?;
        Ok(updated > 0)
    }

    /// Results of the newest decided uses of `skill` recorded at `min_revision` or later,
    /// newest first, at most `limit`. Undecided uses are skipped.
    pub fn recent_skill_results(&self, skill: &str, min_revision: u64, limit: usize) -> io::Result<Vec<bool>> {
        let db = self.lock()?;
        let mut query = db
            .prepare(
                "SELECT success FROM learn_skill_uses WHERE skill=? AND revision>=? AND success IS NOT NULL
                 ORDER BY at DESC, rowid DESC LIMIT ?",
            )
            .map_err(sql)?;
        let rows = query
            .query_map(params![skill, to_i64(min_revision)?, limit.min(i64::MAX as usize) as i64], |r| {
                r.get::<_, i64>(0)
            })
            .map_err(sql)?;
        rows.map(|row| row.map(|success| success != 0).map_err(sql)).collect()
    }

    /// The use of `skill` in `request`, if recorded.
    pub fn skill_use(&self, skill: &str, request: &str) -> io::Result<Option<SkillUse>> {
        let db = self.lock()?;
        db.query_row(
            "SELECT revision,success,at FROM learn_skill_uses WHERE skill=? AND request_id=?",
            [skill, request],
            |r| {
                Ok(SkillUse {
                    skill: skill.to_owned(),
                    revision: r.get::<_, i64>(0)?.max(0) as u64,
                    request_id: request.to_owned(),
                    success: r.get::<_, Option<i64>>(1)?.map(|success| success != 0),
                    at: r.get::<_, i64>(2)?.max(0) as u64,
                })
            },
        )
        .optional()
        .map_err(sql)
    }
}

/// The `skill_used` journal body of a read.
pub fn skill_used_body(skill: &str, revision: u64, call_id: &str, tool: &str) -> Value {
    serde_json::json!({"skill":skill,"revision":revision,"call_id":call_id,"tool":tool})
}

#[cfg(test)]
#[path = "skill_uses_tests.rs"]
mod tests;
