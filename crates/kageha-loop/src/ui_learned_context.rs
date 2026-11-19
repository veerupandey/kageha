//! The learned-context block at request start (spec autonomous-agent-loop, task 25.1;
//! design section 15 "Learned context"; Requirements 17.1 and 17.3).
//!
//! `webui::execute` calls [`message`] once, while it builds a new turn's system context
//! right after the `ui_prompt` guidance. A resumed checkpoint keeps the copy saved in its
//! messages, so every step of a turn, and every step after a restart, sends the same
//! block at the same position, which keeps the prompt prefix cacheable.
//!
//! - **Inputs.** The persona's agent id (`main` by default), the text of the last user
//!   message, the request's task type when its outcome contract already has one, and the
//!   skills the model may load (`ui_skills::model_catalog_for`, which leaves out skills that
//!   are disabled, archived, out of scope or degraded) with their registry revisions.
//! - **Ranking.** `SqliteLearning::context_with` (`kageha_learning::context`).
//! - **Tracking.** The shown lessons get `learn_lesson_uses` rows and `shown + 1` for the
//!   request (`SqliteLearning::record_lesson_uses`); each shown lesson is journaled as
//!   `lesson_used {lesson_id, via: "context"}` and each skill as
//!   `skill_used {skill, revision, via: "context"}`. A `skill_used` entry from here says
//!   only that the skill was offered; loading it with `skill_read` is a separate use
//!   (task 24.2). The worker fills `verified` on the lesson uses when the request closes
//!   (`ui_learning`, `Worker::decide_lesson_uses`).
//!
//! Without an installed learning store there is no block. Store errors are logged and
//! also give no block; the turn is never affected.
use crate::{ui_tools::Tools, webui::Database};
use kageha_learning::{JournalKind, SkillCandidate};
use serde_json::{json, Value};

/// The text of the last user message: a string, or the `text` parts of a list.
pub(crate) fn request_text(messages: &[Value]) -> String {
    let Some(content) = messages.iter().rev().find(|m| m["role"] == "user").map(|m| &m["content"]) else {
        return String::new();
    };
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// The skills the model may load for `persona`, with their revisions and task types.
/// `shell` is whether the turn lists the `shell` tool (`Tools::shell_listed`).
fn skill_candidates(db: &Database, persona: &Value, shell: bool) -> Vec<SkillCandidate> {
    let Ok(catalog) = crate::ui_skills::model_catalog_for(db, persona, shell) else { return Vec::new() };
    if catalog.is_empty() {
        return Vec::new();
    }
    let Ok(records) = crate::ui_skills::owned(db).and_then(|registry| {
        registry.list().map_err(|error| (500, error.to_string()))
    }) else {
        return Vec::new();
    };
    catalog
        .into_iter()
        .filter_map(|(name, description)| {
            let record = records.iter().find(|record| record.skill.name == name)?;
            Some(SkillCandidate {
                task_type: record.skill.metadata["task_type"].as_str()
                    .filter(|slug| kageha_learning::valid_task_type(slug))
                    .map(str::to_owned),
                revision: record.revision,
                name,
                description,
                // The catalog already leaves degraded skills out (task 24.2).
                degraded: record.degraded,
            })
        })
        .collect()
}

/// The learned-context system message for the request of `host`'s turn, or `None` when
/// nothing qualifies or the learning store is unavailable. Records what it shows.
pub(crate) fn message(host: &Tools<'_>, messages: &[Value]) -> Option<Value> {
    let (db, chat, turn, persona) = (host.db, host.chat, host.turn, &host.persona);
    let store = crate::ui_learning::store(db)?;
    let agent = persona["id"].as_str().filter(|id| !id.is_empty()).unwrap_or("main");
    let key = crate::ui_task_completion::scope(db, turn).unwrap_or_else(|_| turn.to_owned());
    let task_type = db.lock().ok().and_then(|connection| {
        crate::ui_task_completion::load(&connection, chat, &key).ok()?.task_type
    });
    let skills = skill_candidates(db, persona, host.shell_listed());
    let context = match store.context_with(agent, &request_text(messages), task_type.as_deref(), &skills) {
        Ok(context) => context,
        Err(error) => {
            eprintln!("kageha: learned context unavailable: {error}");
            return None;
        }
    };
    if context.text.is_empty() {
        return None;
    }
    if let Err(error) = store.record_lesson_uses(&key, &context.lesson_ids) {
        eprintln!("kageha: learned context uses not recorded: {error}");
    }
    for id in &context.lesson_ids {
        crate::ui_learning::journal(db, turn, JournalKind::LessonUsed, json!({"lesson_id":id,"via":"context"}));
    }
    for name in &context.skill_ids {
        let revision = skills.iter().find(|skill| &skill.name == name).map(|skill| skill.revision);
        crate::ui_learning::journal(db, turn, JournalKind::SkillUsed,
            json!({"skill":name,"revision":revision,"via":"context"}));
    }
    Some(json!({"role":"system","content":context.text}))
}

#[cfg(test)]
#[path = "ui_learned_context_tests.rs"]
mod tests;
