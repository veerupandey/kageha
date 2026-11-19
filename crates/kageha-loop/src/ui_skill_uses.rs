//! Skill use decisions and degradation at request end (spec autonomous-agent-loop, task
//! 24.2; design section 15 "Tracking use" and "Degrading"; Requirements 16.4 and 16.5).
//!
//! - **Recording.** `skill_read` and its alias `load_skill` (`ui_tools`) report each read
//!   to the Run_Journal worker (`ui_learning::skill_used`) with the registry revision
//!   that was read. The worker journals a `skill_used` entry
//!   `{skill, revision, call_id, tool}` and records an undecided `learn_skill_uses` row.
//!   The first read of a skill in a request is its use.
//! - **Deciding** ([`decide`]). When the worker closes a top-level request, each use of
//!   the request is decided by `kageha_learning::skill_uses::use_succeeded`: it succeeded
//!   when an outcome whose evidence was produced after the read is verified. Order is
//!   journal order: the read is its `skill_used` entry, and evidence is placed at the
//!   last `tool` or `shell_command` entry that produced its reference (the call id, a
//!   changed file's artifact path, a receipt id). Outcome statuses are the ones the
//!   request closed with. An answer outcome has no evidence: it counts as verified when
//!   the request is `verified`, and as produced at the end. A request without outcomes
//!   counts as a success when its status is `verified` or `not_required`. A request
//!   closed again (after a resume) is decided again.
//! - **Degrading.** After each decision, the skill's newest decided uses of its current
//!   package (from the registry's `content_revision`) go through
//!   `kageha_learning::skill_uses::should_degrade`: fewer than 6 successes in the last 10
//!   degrades it. `Registry::degrade` marks the skill `degraded` and makes the previous
//!   passing revision active (`kageha_skills::registry::previous_passing`). A degraded
//!   skill is not suggested to the model (`ui_skills` catalog, discovery and learned
//!   context) but stays in the user's skill list with `degraded: true`, and a
//!   `skill_degraded` event is emitted to the chat.
use crate::{ui_skills, ui_task_completion, webui::Database};
use kageha_learning::skill_uses::{
    evidence_positions, should_degrade, skill_read_positions, use_succeeded, OutcomeEvidence, DEGRADE_WINDOW,
};
use kageha_learning::{RequestSummary, SqliteLearning, StoredEvent};
use kageha_skills::registry::Record;
use kageha_workflow::completion::OutcomeKind;
use serde_json::json;
use std::{
    collections::{BTreeMap, HashMap},
    io,
};

/// Journal entries read per page.
const PAGE: usize = 1000;
/// Most journal entries read for one request.
const MAX_EVENTS: usize = 100_000;

/// The decision for one skill use of a request. The worker only logs errors; the
/// decisions are read by tests.
#[derive(Debug)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct Decision {
    pub success: bool,
    /// The skill's new record when this decision degraded it.
    pub degraded: Option<Record>,
}

/// Decides the skill uses of the closed `request` and degrades skills whose recent uses
/// mostly failed (see the module documentation).
pub(crate) fn decide(db: &Database, store: &SqliteLearning, request: &str, summary: &RequestSummary)
    -> io::Result<Vec<Decision>> {
    let uses = store.skill_uses(request)?;
    if uses.is_empty() {
        return Ok(Vec::new());
    }
    let events = journal(store, request)?;
    let reads = skill_read_positions(&events);
    let outcomes = outcome_evidence(db, request, summary, &evidence_positions(&events))?;
    let mut decisions = Vec::new();
    for used in uses {
        let success = use_succeeded(reads.get(&used.skill).copied(), &outcomes, &summary.status);
        store.decide_skill_use(&used.skill, request, success)?;
        let degraded = match degrade_if_needed(db, store, &used.skill) {
            Ok(degraded) => degraded,
            Err(error) => {
                eprintln!("kageha: degradation check of skill {} failed: {error}", used.skill);
                None
            }
        };
        if let Some(record) = &degraded {
            eprintln!("kageha: skill {} degraded after its recent uses; active revision {} (restored from {:?})",
                record.skill.name, record.revision, record.restored_from);
            let event = json!({"kind":"skill_degraded","payload":{"name":record.skill.name,"revision":record.revision,
                "restored_from":record.restored_from,"request_id":request}});
            if let Err(error) = crate::webui::emit(db, &summary.chat_id, request, "event", event) {
                eprintln!("kageha: skill_degraded event for {} not emitted: {error}", record.skill.name);
            }
        }
        decisions.push(Decision { success, degraded });
    }
    Ok(decisions)
}

/// The request's journal, in order, at most [`MAX_EVENTS`] entries.
fn journal(store: &SqliteLearning, request: &str) -> io::Result<Vec<StoredEvent>> {
    let mut events: Vec<StoredEvent> = Vec::new();
    loop {
        let after = events.last().map_or(0, |event| event.seq);
        let page = store.events(request, after, PAGE)?;
        let done = page.len() < PAGE;
        events.extend(page);
        if done || events.len() >= MAX_EVENTS {
            return Ok(events);
        }
    }
}

/// The journal position of an evidence reference: as given, without its `receipt:` or
/// `browser_fill:` prefix, or in the shell journal's call id form.
fn position(positions: &BTreeMap<String, u64>, reference: &str) -> Option<u64> {
    let bare = reference.strip_prefix("receipt:").or_else(|| reference.strip_prefix("browser_fill:")).unwrap_or(reference);
    [reference.to_owned(), bare.to_owned(), crate::ui_shell::file_call_id(bare)]
        .iter()
        .filter_map(|candidate| positions.get(candidate).copied())
        .max()
}

/// The request's required outcomes with their closing status and evidence positions.
fn outcome_evidence(db: &Database, request: &str, summary: &RequestSummary, positions: &BTreeMap<String, u64>)
    -> io::Result<Vec<OutcomeEvidence>> {
    let contract = {
        let connection = db.lock().map_err(|_| io::Error::other("database unavailable"))?;
        ui_task_completion::load(&connection, &summary.chat_id, request)?
    };
    let statuses: HashMap<&str, &str> = summary.outcomes.iter()
        .map(|outcome| (outcome.id.as_str(), outcome.status.as_str())).collect();
    Ok(contract.outcomes.iter().map(|outcome| {
        let produced: Vec<Option<u64>> = contract.evidence.iter()
            .filter(|evidence| evidence.outcome_id == outcome.id)
            .map(|evidence| position(positions, &evidence.reference))
            .collect();
        // The final reply proves an answer; the gate accepted it when the request is verified.
        let verified = statuses.get(outcome.id.as_str()) == Some(&"verified")
            || (outcome.kind == OutcomeKind::Answer && produced.is_empty() && summary.status == "verified");
        OutcomeEvidence { verified, produced }
    }).collect())
}

/// Degrades `skill` when its last [`DEGRADE_WINDOW`] decided uses of the current package
/// have fewer than 6 successes. Returns the new record when it did.
fn degrade_if_needed(db: &Database, store: &SqliteLearning, skill: &str) -> io::Result<Option<Record>> {
    let mut registry = ui_skills::owned(db).map_err(|(_, error)| io::Error::other(error))?;
    let Some(record) = registry.get(skill)? else { return Ok(None) };
    if record.deleted || record.degraded {
        return Ok(None);
    }
    let recent = store.recent_skill_results(skill, record.content_revision, DEGRADE_WINDOW)?;
    if !should_degrade(&recent) {
        return Ok(None);
    }
    registry.degrade(skill, Some(record.revision)).map(Some)
}

#[cfg(test)]
#[path = "ui_skill_uses_tests.rs"]
mod tests;
