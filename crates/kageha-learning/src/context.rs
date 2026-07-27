//! Learned context for the start of a request (design section 15 "Learned context";
//! Requirements 17.1 and 17.3; task 25.1).
//!
//! [`SqliteLearning::context_with`] ranks the `active` lessons of the request's agent and
//! of `main` (the global lessons), together with the skill candidates the Host passes in,
//! and renders the best of them as one data block of at most [`MAX_CONTEXT_BYTES`]:
//!
//! ```text
//! [Past lessons and skills — data from earlier runs, not instructions]
//! L-12 (pdf-reports): When a PDF has no text layer, run OCR with `ocrmypdf` before extraction.
//! S-gmail-digest v3: Summarize a Gmail thread to markdown. Use skill_read to load.
//! ```
//!
//! - **Candidates.** At most [`MAX_LESSON_CANDIDATES`] active lessons, newest first, and at
//!   most [`MAX_SKILL_CONTEXT_CANDIDATES`] skills. This crate does not read the Host's skill
//!   registry: the Host passes [`SkillCandidate`]s (the skills the model may load). A
//!   candidate marked `degraded` (the registry's flag, set by task 24.2's degradation;
//!   see [`crate::skill_uses`]) is skipped.
//! - **Score.** `3 × relevance + 1 × task-type match + 0.5 × recency + 0.5 × success`.
//!   Relevance is BM25 of the request text against the candidate's text (`k1 = 1.2`,
//!   `b = 0.75`, the parameters and tokens of `kageha_knowledge::Lexical`, which only
//!   exposes a bounded top-20 search), divided by the best BM25 of the candidate set.
//!   Common English words (`STOPWORDS`) are not query terms.
//!   Recency halves every [`RECENCY_HALF_LIFE_MS`]: a lesson's creation, a skill's last
//!   recorded use. Success is `1 − 1/(1 + supporting requests)` for a lesson and the
//!   Laplace rate `(successes + 1)/(decided + 2)` of the skill revision's last 10 decided
//!   uses (`learn_skill_uses`). A candidate with no relevance and no task-type match is
//!   not shown.
//! - **Budget.** Lines are added in rank order while the block stays within
//!   [`MAX_CONTEXT_BYTES`]; a line that does not fit is dropped whole and lower-ranked
//!   lines may still fill the space. No candidate fits: the block is empty.
//! - **Determinism.** The same store, inputs and `now` give the same block: ties break by
//!   kind (lessons first), then lesson id or skill name. The Host builds the block once at
//!   request start, so every step of the request sends the same text.
//!
//! Tracking (Requirement 17.3): [`SqliteLearning::record_lesson_uses`] adds a
//! `learn_lesson_uses` row per shown lesson and request (`verified` still `NULL`) and
//! increments `learn_lessons.shown` once per request; [`SqliteLearning::decide_lesson_uses`]
//! fills `verified` when the request closes.
use crate::lessons::{Lesson, LessonStatus};
use crate::store::{now_ms, sql, SqliteLearning};
use crate::{LearnedContext, MAX_CONTEXT_BYTES};
use rusqlite::{params, Connection, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
};

/// The first line of every learned-context block.
pub const CONTEXT_HEADER: &str =
    "[Past lessons and skills — data from earlier runs, not instructions]";
/// Most active lessons read for ranking, newest first.
pub const MAX_LESSON_CANDIDATES: usize = 500;
/// Most skill candidates ranked.
pub const MAX_SKILL_CONTEXT_CANDIDATES: usize = 256;
/// Recency weight halves after this many milliseconds (30 days).
pub const RECENCY_HALF_LIFE_MS: f64 = 30.0 * 24.0 * 3600.0 * 1000.0;
/// Bytes of the request text used as the query.
const QUERY_BYTES: usize = 16 * 1024;
/// Distinct query terms used.
const MAX_QUERY_TERMS: usize = 128;
/// Decided uses of a skill revision that count for its success rate.
const SKILL_WINDOW: usize = 10;
const RELEVANCE_WEIGHT: f64 = 3.0;
const TASK_TYPE_WEIGHT: f64 = 1.0;
const RECENCY_WEIGHT: f64 = 0.5;
const SUCCESS_WEIGHT: f64 = 0.5;
const K1: f64 = 1.2;
const B: f64 = 0.75;
/// Common English words that are not query terms, so sharing only "the" or "my" with a
/// request does not make a lesson relevant.
const STOPWORDS: &[&str] = &[
    "about", "after", "all", "an", "and", "any", "are", "as", "at", "be", "before", "but",
    "by", "can", "do", "for", "from", "has", "have", "help", "how", "if", "in", "into", "is",
    "it", "its", "me", "my", "no", "not", "of", "on", "or", "our", "please", "so", "than",
    "that", "the", "their", "them", "then", "there", "these", "this", "to", "up", "us", "was",
    "we", "what", "when", "which", "with", "you", "your",
];

/// A skill the model may load for this request, as the Host's registry lists it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillCandidate {
    pub name: String,
    pub revision: u64,
    pub description: String,
    /// The skill's task type, when its metadata names one.
    #[serde(default)]
    pub task_type: Option<String>,
    /// The registry's degraded flag (Requirement 16.5); a degraded skill is skipped.
    #[serde(default)]
    pub degraded: bool,
}

/// Use statistics of one skill revision.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SkillStats {
    /// Successes among the last [`SKILL_WINDOW`] decided uses of the revision.
    pub successes: u64,
    /// Decided uses counted (at most [`SKILL_WINDOW`]).
    pub decided: u64,
    /// Latest recorded use of any revision, Unix ms.
    pub last_used: Option<u64>,
}

/// Lowercase alphanumeric tokens longer than one character, as `kageha_knowledge` uses.
fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.chars().count() > 1)
        .map(str::to_lowercase)
        .collect()
}

/// `text` on one line: control characters become spaces, whitespace runs collapse.
fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// `text` ending in sentence punctuation.
fn sentence(text: &str) -> String {
    let text = one_line(text);
    if text.is_empty() || text.ends_with(['.', '!', '?']) {
        text
    } else {
        format!("{text}.")
    }
}

/// A lesson as one sentence: `When <trigger>, <action>`.
fn lesson_sentence(trigger: &str, action: &str) -> String {
    let trigger = one_line(trigger);
    let trigger = trigger.trim_end_matches(['.', ',', ';', ':']);
    let lower = trigger.to_lowercase();
    let trigger = if lower.starts_with("when ") || lower.starts_with("if ") || trigger.is_empty() {
        trigger.to_owned()
    } else {
        // "A PDF has ..." reads "When a PDF has ...", but "PDF ..." keeps its case.
        let mut chars = trigger.chars();
        let first = chars.next().unwrap_or_default();
        let lowered = match chars.clone().next() {
            Some(second) if second.is_lowercase() || second == ' ' => first.to_lowercase().collect(),
            _ => first.to_string(),
        };
        format!("When {lowered}{}", chars.as_str())
    };
    if trigger.is_empty() {
        sentence(action)
    } else {
        sentence(&format!("{trigger}, {action}", action = one_line(action)))
    }
}

/// Which candidate a ranked line shows.
#[derive(Debug, Clone, PartialEq)]
enum Item {
    Lesson(i64),
    Skill(String),
}

struct Ranked {
    item: Item,
    line: String,
    score: f64,
}

/// The candidate text, task type, recency time and success value used for ranking.
struct Scored<'a> {
    item: Item,
    line: String,
    text: String,
    task_type: Option<&'a str>,
    time: Option<u64>,
    success: f64,
}

/// Ranks `lessons` and `skills` for `request_text` and renders the block. Pure: the same
/// arguments give the same result. Lessons that are not `active` and degraded skills are
/// skipped.
pub fn rank(
    lessons: &[Lesson],
    skills: &[SkillCandidate],
    stats: &BTreeMap<String, SkillStats>,
    request_text: &str,
    task_type: Option<&str>,
    now: u64,
) -> LearnedContext {
    rank_visible(lessons.iter().filter(|lesson| lesson.status == LessonStatus::Active),
        skills, stats, request_text, task_type, now)
}

fn rank_visible<'a>(
    lessons: impl Iterator<Item = &'a Lesson>,
    skills: &[SkillCandidate],
    stats: &BTreeMap<String, SkillStats>,
    request_text: &str,
    task_type: Option<&str>,
    now: u64,
) -> LearnedContext {
    let mut candidates: Vec<Scored<'_>> = Vec::new();
    for lesson in lessons {
        let label = match lesson.task_type.as_deref().filter(|t| !t.is_empty()) {
            Some(task) => format!("L-{} ({task})", lesson.id),
            None => format!("L-{}", lesson.id),
        };
        let status = if lesson.status == LessonStatus::Candidate { " [unverified candidate]" } else { "" };
        candidates.push(Scored {
            item: Item::Lesson(lesson.id),
            line: format!("{label}{status}: {}", lesson_sentence(&lesson.trigger, &lesson.action)),
            text: format!(
                "{} {} {}",
                lesson.task_type.as_deref().unwrap_or(""),
                lesson.trigger,
                lesson.action
            ),
            task_type: lesson.task_type.as_deref(),
            time: Some(lesson.created),
            success: 1.0 - 1.0 / (1.0 + lesson.support.len() as f64),
        });
    }
    let mut seen = BTreeSet::new();
    let mut usable: Vec<&SkillCandidate> = skills
        .iter()
        .filter(|s| !s.degraded && !s.name.is_empty())
        .collect();
    usable.sort_by(|a, b| a.name.cmp(&b.name).then(b.revision.cmp(&a.revision)));
    for skill in usable
        .into_iter()
        .filter(|s| seen.insert(s.name.clone()))
        .take(MAX_SKILL_CONTEXT_CANDIDATES)
    {
        let stat = stats.get(&skill.name).copied().unwrap_or_default();
        let description = sentence(&skill.description);
        let line = if description.is_empty() {
            format!("S-{} v{}: Use skill_read to load.", one_line(&skill.name), skill.revision)
        } else {
            format!(
                "S-{} v{}: {description} Use skill_read to load.",
                one_line(&skill.name),
                skill.revision
            )
        };
        candidates.push(Scored {
            item: Item::Skill(skill.name.clone()),
            line,
            text: format!(
                "{} {} {}",
                skill.name,
                skill.description,
                skill.task_type.as_deref().unwrap_or("")
            ),
            task_type: skill.task_type.as_deref(),
            time: stat.last_used,
            success: (stat.successes as f64 + 1.0) / (stat.decided as f64 + 2.0),
        });
    }
    if candidates.is_empty() {
        return LearnedContext::default();
    }

    // BM25 of the request text against each candidate.
    let mut query: Vec<String> = Vec::new();
    let mut cut = request_text.len().min(QUERY_BYTES);
    while !request_text.is_char_boundary(cut) {
        cut -= 1;
    }
    for term in tokens(&request_text[..cut]) {
        if query.len() == MAX_QUERY_TERMS {
            break;
        }
        if !STOPWORDS.contains(&term.as_str()) && !query.contains(&term) {
            query.push(term);
        }
    }
    let documents: Vec<Vec<String>> = candidates.iter().map(|c| tokens(&c.text)).collect();
    let n = documents.len() as f64;
    let average = documents.iter().map(|d| d.len().max(1)).sum::<usize>() as f64 / n;
    let frequency: BTreeMap<&str, f64> = query
        .iter()
        .map(|term| {
            let df = documents.iter().filter(|d| d.contains(term)).count() as f64;
            (term.as_str(), df)
        })
        .collect();
    let bm25: Vec<f64> = documents
        .iter()
        .map(|document| {
            let length = document.len().max(1) as f64;
            query
                .iter()
                .map(|term| {
                    let tf = document.iter().filter(|word| *word == term).count() as f64;
                    if tf == 0.0 {
                        return 0.0;
                    }
                    let df = frequency[term.as_str()];
                    let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
                    idf * tf * (K1 + 1.0) / (tf + K1 * (1.0 - B + B * length / average))
                })
                .sum()
        })
        .collect();
    let best = bm25.iter().copied().fold(0.0_f64, f64::max);

    let wanted = task_type.filter(|t| !t.is_empty());
    let mut ranked: Vec<Ranked> = candidates
        .into_iter()
        .zip(bm25)
        .filter_map(|(candidate, bm25)| {
            let relevance = if best > 0.0 { bm25 / best } else { 0.0 };
            let matches = wanted.is_some() && candidate.task_type == wanted;
            if relevance <= 0.0 && !matches {
                return None;
            }
            let recency = candidate.time.map_or(0.0, |time| {
                0.5_f64.powf(now.saturating_sub(time) as f64 / RECENCY_HALF_LIFE_MS)
            });
            let score = RELEVANCE_WEIGHT * relevance
                + if matches { TASK_TYPE_WEIGHT } else { 0.0 }
                + RECENCY_WEIGHT * recency
                + SUCCESS_WEIGHT * candidate.success;
            Some(Ranked {
                item: candidate.item,
                line: candidate.line,
                score,
            })
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.score.total_cmp(&a.score).then_with(|| match (&a.item, &b.item) {
            (Item::Lesson(x), Item::Lesson(y)) => x.cmp(y),
            (Item::Skill(x), Item::Skill(y)) => x.cmp(y),
            (Item::Lesson(_), Item::Skill(_)) => std::cmp::Ordering::Less,
            (Item::Skill(_), Item::Lesson(_)) => std::cmp::Ordering::Greater,
        })
    });

    let mut context = LearnedContext::default();
    let mut text = String::from(CONTEXT_HEADER);
    for entry in ranked {
        if text.len() + 1 + entry.line.len() > MAX_CONTEXT_BYTES {
            continue;
        }
        text.push('\n');
        text.push_str(&entry.line);
        match entry.item {
            Item::Lesson(id) => context.lesson_ids.push(id),
            Item::Skill(name) => context.skill_ids.push(name),
        }
    }
    if !context.lesson_ids.is_empty() || !context.skill_ids.is_empty() {
        context.text = text;
    }
    context
}

fn check_request(request: &str) -> io::Result<()> {
    if request.is_empty() || request.len() > 256 || request.chars().any(char::is_control) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "Invalid request id"));
    }
    Ok(())
}

impl SqliteLearning {
    /// The learned-context block for `agent` with the Host's skill candidates, at the
    /// current time. See the module documentation.
    pub fn context_with(
        &self,
        agent: &str,
        request_text: &str,
        task_type: Option<&str>,
        skills: &[SkillCandidate],
    ) -> io::Result<LearnedContext> {
        self.context_at(agent, request_text, task_type, skills, now_ms())
    }

    /// [`Self::context_with`] at time `now` (Unix ms).
    pub fn context_at(
        &self,
        agent: &str,
        request_text: &str,
        task_type: Option<&str>,
        skills: &[SkillCandidate],
        now: u64,
    ) -> io::Result<LearnedContext> {
        let lessons = self.visible_lessons(agent, false)?;
        let db = self.lock()?;
        let mut stats = BTreeMap::new();
        // The first candidate of a name is ranked; see `rank`.
        let mut ordered: Vec<&SkillCandidate> = skills.iter().collect();
        ordered.sort_by(|a, b| a.name.cmp(&b.name).then(b.revision.cmp(&a.revision)));
        for skill in ordered {
            if !stats.contains_key(&skill.name) {
                stats.insert(skill.name.clone(), skill_stats(&db, &skill.name, skill.revision)?);
            }
        }
        drop(db);
        Ok(rank(&lessons, skills, &stats, request_text, task_type, now))
    }

    /// Explicit recall also shows the requesting agent's relevant unverified candidates.
    pub fn search_lessons(&self, agent: &str, query: &str) -> io::Result<LearnedContext> {
        let lessons = self.visible_lessons(agent, true)?;
        Ok(rank_visible(lessons.iter(), &[], &BTreeMap::new(), query, None, now_ms()))
    }

    fn visible_lessons(&self, agent: &str, include_candidates: bool) -> io::Result<Vec<Lesson>> {
        let db = self.lock()?;
        let mut query = db
            .prepare(
                "SELECT id,agent,task_type,\"trigger\",action,support,shown,created,status FROM learn_lessons
                 WHERE (status='active' AND agent IN (?1,'main')) OR (status='candidate' AND agent=?1 AND ?3)
                 ORDER BY id DESC LIMIT ?2",
            )
            .map_err(sql)?;
        let rows = query
            .query_map(params![agent, MAX_LESSON_CANDIDATES as i64, include_candidates], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, i64>(7)?,
                    r.get::<_, String>(8)?,
                ))
            })
            .map_err(sql)?;
        rows.map(|row| {
            let (id, agent, task_type, trigger, action, support, shown, created, status) = row.map_err(sql)?;
            Ok(Lesson {
                id,
                agent,
                task_type,
                trigger,
                action,
                status: if status == "candidate" { LessonStatus::Candidate } else { LessonStatus::Active },
                support: serde_json::from_str(&support).unwrap_or_default(),
                evidence: BTreeMap::new(),
                shown: shown.max(0) as u64,
                created: created.max(0) as u64,
                retired_at: None,
            })
        })
        .collect()
    }

    /// Records that `lesson_ids` were shown in `request`: one `learn_lesson_uses` row per
    /// lesson (with `verified` unset) and `shown + 1` the first time a lesson is shown in
    /// the request. Returns how many lessons were newly recorded.
    pub fn record_lesson_uses(&self, request: &str, lesson_ids: &[i64]) -> io::Result<usize> {
        check_request(request)?;
        let mut db = self.lock()?;
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql)?;
        let mut recorded = 0;
        for id in lesson_ids.iter().collect::<BTreeSet<_>>() {
            let exists: bool = tx
                .query_row("SELECT EXISTS(SELECT 1 FROM learn_lessons WHERE id=?)", [id], |r| {
                    r.get(0)
                })
                .map_err(sql)?;
            if !exists {
                continue;
            }
            let added = tx
                .execute(
                    "INSERT OR IGNORE INTO learn_lesson_uses(lesson_id,request_id,verified) VALUES(?,?,NULL)",
                    params![id, request],
                )
                .map_err(sql)?;
            if added == 1 {
                tx.execute("UPDATE learn_lessons SET shown=shown+1 WHERE id=?", [id])
                    .map_err(sql)?;
                recorded += 1;
            }
        }
        tx.commit().map_err(sql)?;
        Ok(recorded)
    }

    /// Sets `verified` on every lesson use of `request` when it closes. A request closed
    /// again (after a resume) is decided again. Returns the rows updated.
    pub fn decide_lesson_uses(&self, request: &str, verified: bool) -> io::Result<usize> {
        check_request(request)?;
        let db = self.lock()?;
        db.execute(
            "UPDATE learn_lesson_uses SET verified=? WHERE request_id=?",
            params![verified, request],
        )
        .map_err(sql)
    }

    /// The lesson uses of `request` as `(lesson id, verified)`, by lesson id.
    pub fn lesson_uses(&self, request: &str) -> io::Result<Vec<(i64, Option<bool>)>> {
        let db = self.lock()?;
        let mut query = db
            .prepare(
                "SELECT lesson_id,verified FROM learn_lesson_uses WHERE request_id=? ORDER BY lesson_id",
            )
            .map_err(sql)?;
        let rows = query
            .query_map([request], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(sql)?;
        rows.collect::<rusqlite::Result<_>>().map_err(sql)
    }
}

/// Success counts of the last [`SKILL_WINDOW`] decided uses of a skill revision, and the
/// skill's latest use.
fn skill_stats(db: &Connection, skill: &str, revision: u64) -> io::Result<SkillStats> {
    let revision = i64::try_from(revision).unwrap_or(i64::MAX);
    let (successes, decided): (i64, i64) = db
        .query_row(
            "SELECT COALESCE(SUM(success),0),COUNT(*) FROM (SELECT success FROM learn_skill_uses
             WHERE skill=? AND revision=? AND success IS NOT NULL ORDER BY at DESC,request_id LIMIT ?)",
            params![skill, revision, SKILL_WINDOW as i64],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(sql)?;
    let last_used: Option<i64> = db
        .query_row("SELECT MAX(at) FROM learn_skill_uses WHERE skill=?", [skill], |r| r.get(0))
        .map_err(sql)?;
    Ok(SkillStats {
        successes: successes.max(0) as u64,
        decided: decided.max(0) as u64,
        last_used: last_used.map(|at| at.max(0) as u64),
    })
}

#[cfg(test)]
#[path = "context_tests.rs"]
mod tests;
