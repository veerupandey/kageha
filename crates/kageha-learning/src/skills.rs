//! Skill promotion from verified script runs (design section 15 "Skill promotion";
//! Requirements 16.1–16.3; task 24.1).
//!
//! The Host's reflection worker (`kageha-loop::ui_reflection`) promotes scripts that did
//! a verified step of a request. This module holds the parts that need no Host state:
//!
//! - **Candidates.** [`script_runs`] reads the request's `shell_command` journal entries
//!   and keeps commands that exited 0 and ran a workspace script
//!   ([`script_invocation`]: `python|python3|node|bash|sh <path>` or
//!   `uv run [python|python3] <path>`, one simple command without shell operators),
//!   when one of the files the command changed, or the command's call id, is verified
//!   evidence of the request. At most [`MAX_SKILL_CANDIDATES`] distinct scripts are
//!   kept, the most recently run first.
//! - **Drafts.** The planner answers with a JSON object that [`parse_skill_draft`] reads:
//!   `name`, `description`, `instructions` (when to use, how to run), `script` (the
//!   script, possibly generalized; `null` keeps the original) and `test_sh`. An invalid
//!   script draft keeps the original script.
//! - **Packages.** [`build_skill`] lays the draft out as a `kageha-skills` package:
//!   `SKILL.md` (frontmatter written here, with `metadata.source: learned`), the script
//!   under `scripts/` and `test.sh` at the package root, and validates it.
//! - **Records.** Each attempt is recorded in `learn_skill_promotions`, keyed by the
//!   reflection queue's request id and the script path, with the learned skill and
//!   revision or the reason it was dropped ([`SqliteLearning::record_promotion`]). A
//!   reflection that runs again skips scripts already recorded.
use crate::store::{now_ms, sql, SqliteLearning};
use crate::{JournalKind, StoredEvent};
use kageha_skills::{valid_name, Package};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
};

/// Most scripts promoted from one request.
pub const MAX_SKILL_CANDIDATES: usize = 2;
/// Largest script that is promoted. It is sent to the planner in full.
pub const MAX_SCRIPT_BYTES: usize = 64 * 1024;
/// Largest `test.sh`.
pub const MAX_TEST_BYTES: usize = 64 * 1024;
/// Largest instruction text of `SKILL.md`.
pub const MAX_INSTRUCTIONS_BYTES: usize = 16 * 1024;
/// Most characters of a skill description.
pub const MAX_DESCRIPTION_CHARS: usize = 300;
/// The registry source of learned skills.
pub const LEARNED_SOURCE: &str = "learned";
/// Suffix of a learned skill whose name an operator-installed skill already uses.
pub const LEARNED_SUFFIX: &str = "-learned";
/// The package path of the test.
pub const TEST_PATH: &str = "test.sh";
const MAX_COMMAND_BYTES: usize = 4096;
const MAX_SCRIPT_PATH_BYTES: usize = 255;
const MAX_REASON_CHARS: usize = 512;
/// Prefix of the artifact path of a Task_Workspace file.
const WORKSPACE_PREFIX: &str = "workspace/";

/// Creates `learn_skill_promotions`.
pub(crate) fn migrate(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS learn_skill_promotions(request_id TEXT NOT NULL,script TEXT NOT NULL,
            state TEXT NOT NULL,skill TEXT,revision INTEGER,reason TEXT,at INTEGER NOT NULL,
            PRIMARY KEY(request_id,script));",
    )
}

/// A workspace script run by a shell command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptInvocation {
    /// `python`, `python3`, `node`, `bash`, `sh`, `uv run` or `uv run python`.
    pub program: String,
    /// The script, relative to the Task_Workspace root, without a leading `./`.
    pub script: String,
    pub args: Vec<String>,
}

impl ScriptInvocation {
    /// The script's file name.
    pub fn file_name(&self) -> &str {
        self.script.rsplit('/').next().unwrap_or(&self.script)
    }
    /// The script's path in a skill package: `scripts/<file name>`.
    pub fn package_path(&self) -> String {
        format!("scripts/{}", self.file_name())
    }
    /// The script's artifact path, as `files_changed` and artifact evidence name it.
    pub fn artifact_path(&self) -> String {
        format!("{WORKSPACE_PREFIX}{}", self.script)
    }
}

/// The words of a simple command, with `'...'` and `"..."` quoting. `None` for
/// unclosed quotes or any shell operator, expansion or escape outside quotes.
fn words(command: &str) -> Option<Vec<String>> {
    const SPECIAL: &[char] = &[
        ';', '&', '|', '<', '>', '$', '`', '\\', '(', ')', '{', '}', '*', '?', '!', '~', '#',
        '[', ']',
    ];
    let mut words = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    for c in command.chars() {
        match quote {
            Some(open) if c == open => quote = None,
            Some(_) => current.push(c),
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                started = true;
            }
            None if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            None if SPECIAL.contains(&c) => return None,
            None => {
                current.push(c);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return None;
    }
    if started {
        words.push(current);
    }
    Some(words)
}

/// A plain relative path inside the workspace: `[A-Za-z0-9._-]` segments separated by
/// `/`, no `.`, `..` or empty segment, not under `.kageha/`.
fn valid_script_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= MAX_SCRIPT_PATH_BYTES
        && !path.starts_with('-')
        && path.split('/').next() != Some(".kageha")
        && path.split('/').all(|segment| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && segment
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        })
}

/// The workspace script `command` runs, if it is one simple command
/// `python|python3|node|bash|sh <path> [args]` or `uv run [python|python3] <path> [args]`
/// whose path is relative and has the interpreter's extension (`.py`, `.js`/`.mjs`/`.cjs`,
/// `.sh`). Options before the path, environment assignments, `cd`, pipes, redirections
/// and other shell syntax are not promoted.
pub fn script_invocation(command: &str) -> Option<ScriptInvocation> {
    if command.len() > MAX_COMMAND_BYTES || command.chars().any(char::is_control) {
        return None;
    }
    let words = words(command)?;
    let (program, rest): (&str, &[String]) = match words.first()?.as_str() {
        "uv" => {
            if words.get(1)? != "run" {
                return None;
            }
            match words.get(2).map(String::as_str) {
                Some("python" | "python3") => ("uv run python", &words[3..]),
                _ => ("uv run", &words[2..]),
            }
        }
        program @ ("python" | "python3" | "node" | "bash" | "sh") => (program, &words[1..]),
        _ => return None,
    };
    let path = rest.first()?;
    let path = path.strip_prefix("./").unwrap_or(path);
    if !valid_script_path(path) {
        return None;
    }
    let extension = path.rsplit_once('.').map(|(_, extension)| extension)?;
    let matches = match program {
        "node" => matches!(extension, "js" | "mjs" | "cjs"),
        "bash" | "sh" => extension == "sh",
        _ => extension == "py",
    };
    matches.then(|| ScriptInvocation {
        program: program.into(),
        script: path.into(),
        args: rest[1..].to_vec(),
    })
}

/// A verified script run: a candidate for promotion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptRun {
    /// The journal entry of the run.
    pub seq: u64,
    pub call_id: Option<String>,
    pub command: String,
    pub invocation: ScriptInvocation,
    /// The verified evidence the run produced: artifact paths from its `files_changed`,
    /// and its call id when that is validation evidence.
    pub evidence: Vec<String>,
}

/// The candidates among a request's journal entries (see the module documentation).
/// `verified` holds the references of the request's verified evidence: artifact paths
/// (`workspace/outputs/report.md`) and call ids. A script that rewrote only itself
/// does not count.
pub fn script_runs<'a>(
    events: impl IntoIterator<Item = &'a StoredEvent>,
    verified: &BTreeSet<String>,
) -> Vec<ScriptRun> {
    let mut by_script: BTreeMap<String, ScriptRun> = BTreeMap::new();
    for event in events {
        let body = &event.body;
        if event.kind != JournalKind::ShellCommand
            || body["exit_code"].as_i64() != Some(0)
            || body["background"] == true
        {
            continue;
        }
        let Some(command) = body["command"].as_str() else {
            continue;
        };
        let Some(invocation) = script_invocation(command) else {
            continue;
        };
        let own = invocation.artifact_path();
        let mut evidence: Vec<String> = body["files_changed"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|path| *path != own && verified.contains(*path))
            .map(str::to_owned)
            .collect();
        let call_id = body["call_id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .map(str::to_owned);
        if let Some(id) = call_id.as_ref().filter(|id| verified.contains(id.as_str())) {
            evidence.push(id.clone());
        }
        if evidence.is_empty() {
            continue;
        }
        by_script.insert(
            invocation.script.clone(),
            ScriptRun {
                seq: event.seq,
                call_id,
                command: command.into(),
                invocation,
                evidence,
            },
        );
    }
    let mut runs: Vec<ScriptRun> = by_script.into_values().collect();
    runs.sort_by_key(|run| std::cmp::Reverse(run.seq));
    runs.truncate(MAX_SKILL_CANDIDATES);
    runs
}

/// A skill as the planner drafted it, after parsing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillDraft {
    pub name: String,
    pub description: String,
    /// The body of `SKILL.md`: when to use the skill and how to run it.
    pub instructions: String,
    /// The drafted script; `None` keeps the original (also when the draft was invalid).
    pub script: Option<String>,
    pub test_sh: String,
}

/// Reads the planner's draft `{name, description, instructions, script, test_sh}`.
/// `description`, `instructions` and `test_sh` are required; a missing or invalid
/// `name` becomes empty (then [`skill_name`] derives one from the script); a `script`
/// that is not a non-empty string of at most [`MAX_SCRIPT_BYTES`] without NUL bytes is
/// dropped, so the original script is kept.
pub fn parse_skill_draft(reply: &Value) -> Result<SkillDraft, String> {
    let object = reply
        .as_object()
        .ok_or_else(|| "The skill draft is not a JSON object".to_owned())?;
    let text = |key: &str, max: usize| -> Result<String, String> {
        let value = object
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("The skill draft has no {key}"))?;
        if value.len() > max || value.contains('\0') {
            return Err(format!("The skill draft's {key} exceeds {max} bytes"));
        }
        Ok(value.to_owned())
    };
    let description = text("description", 4 * MAX_DESCRIPTION_CHARS)?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if description.chars().count() > MAX_DESCRIPTION_CHARS {
        return Err(format!(
            "The skill draft's description exceeds {MAX_DESCRIPTION_CHARS} characters"
        ));
    }
    let instructions = text("instructions", MAX_INSTRUCTIONS_BYTES)?;
    let test_sh = text("test_sh", MAX_TEST_BYTES)?;
    let script = object
        .get("script")
        .and_then(Value::as_str)
        .filter(|script| {
            !script.trim().is_empty() && script.len() <= MAX_SCRIPT_BYTES && !script.contains('\0')
        })
        .map(str::to_owned);
    Ok(SkillDraft {
        name: object
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        description,
        instructions: instructions.trim().to_owned(),
        script,
        test_sh,
    })
}

/// `text` as a skill name: lowercase, runs of other characters as one `-`, trimmed,
/// at most 64 bytes.
fn slug(text: &str) -> String {
    let mut slug = String::new();
    for c in text.trim().chars().flat_map(char::to_lowercase) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let mut slug = slug.trim_matches(['-', '_']).to_owned();
    slug.truncate(64);
    slug.trim_end_matches(['-', '_']).to_owned()
}

/// The skill name for a draft: the drafted name as a slug, else the script's file stem
/// as a slug, else `learned-script`.
pub fn skill_name(drafted: &str, invocation: &ScriptInvocation) -> String {
    let file = invocation.file_name();
    let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
    [slug(drafted), slug(stem)]
        .into_iter()
        .find(|name| valid_name(name))
        .unwrap_or_else(|| "learned-script".into())
}

/// `name` with [`LEARNED_SUFFIX`], shortened to stay within 64 bytes.
pub fn learned_name(name: &str) -> String {
    let mut base = name.to_owned();
    base.truncate(64 - LEARNED_SUFFIX.len());
    let base = base.trim_end_matches(['-', '_']);
    format!("{base}{LEARNED_SUFFIX}")
}

/// A validated learned skill package.
#[derive(Debug, Clone)]
pub struct BuiltSkill {
    pub package: Package,
    /// The script's package path, `scripts/<file name>`.
    pub script_path: String,
    /// Whether the package holds the planner's version of the script.
    pub generalized: bool,
}

fn quoted(text: &str) -> String {
    // A JSON string is a valid YAML double-quoted scalar.
    serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into())
}

/// The package of a learned skill named `name`: `SKILL.md` with frontmatter (`name`,
/// `description`, and `metadata` with `source: learned`, the request, the script, the
/// test, the command and the script as a required resource) and the draft's
/// instructions; the drafted script, else `original_script`, at
/// [`ScriptInvocation::package_path`]; and `test.sh`.
pub fn build_skill(
    name: &str,
    draft: &SkillDraft,
    original_script: &[u8],
    invocation: &ScriptInvocation,
    request: &str,
) -> io::Result<BuiltSkill> {
    let script_path = invocation.package_path();
    let command = format!("{} {script_path}", invocation.program);
    let text = format!(
        "---\nname: {name}\ndescription: {}\nmetadata:\n  source: {LEARNED_SOURCE}\n  learned_from: {}\n  script: {}\n  test: {TEST_PATH}\n  command: {}\n  required_resources: [{}]\n---\n{}\n",
        quoted(&draft.description),
        quoted(request),
        quoted(&script_path),
        quoted(&command),
        quoted(&script_path),
        draft.instructions,
    );
    let script = draft
        .script
        .as_ref()
        .map_or(original_script, |script| script.as_bytes());
    let generalized = script != original_script;
    let files = BTreeMap::from([
        ("SKILL.md".to_owned(), text.into_bytes()),
        (script_path.clone(), script.to_vec()),
        (TEST_PATH.to_owned(), draft.test_sh.as_bytes().to_vec()),
    ]);
    let package = Package::from_files(name, files)?;
    if package.skill.name != name {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "The learned skill's name changed",
        ));
    }
    Ok(BuiltSkill {
        package,
        script_path,
        generalized,
    })
}

/// How a promotion attempt ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromotionState {
    /// The test passed and a revision was added to the skill registry.
    Learned,
    /// Nothing was stored; `reason` says why.
    Dropped,
}

impl PromotionState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Learned => "learned",
            Self::Dropped => "dropped",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        [Self::Learned, Self::Dropped]
            .into_iter()
            .find(|state| state.as_str() == value)
    }
}

/// One row of `learn_skill_promotions`. `at` is Unix ms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillPromotion {
    /// The script, relative to the Task_Workspace root.
    pub script: String,
    pub state: PromotionState,
    pub skill: Option<String>,
    pub revision: Option<u64>,
    pub reason: Option<String>,
    pub at: u64,
}

impl SkillPromotion {
    pub fn learned(script: &str, skill: &str, revision: u64) -> Self {
        Self {
            script: script.into(),
            state: PromotionState::Learned,
            skill: Some(skill.into()),
            revision: Some(revision),
            reason: None,
            at: now_ms(),
        }
    }
    pub fn dropped(script: &str, skill: Option<&str>, reason: &str) -> Self {
        Self {
            script: script.into(),
            state: PromotionState::Dropped,
            skill: skill.map(str::to_owned),
            revision: None,
            reason: Some(reason.chars().take(MAX_REASON_CHARS).collect()),
            at: now_ms(),
        }
    }
}

impl SqliteLearning {
    /// Records the result of promoting `promotion.script` from `request`, replacing an
    /// earlier record of the same script.
    pub fn record_promotion(&self, request: &str, promotion: &SkillPromotion) -> io::Result<()> {
        let revision = promotion
            .revision
            .map(i64::try_from)
            .transpose()
            .map_err(sql)?;
        let at = i64::try_from(promotion.at).map_err(sql)?;
        let reason: Option<String> = promotion
            .reason
            .as_ref()
            .map(|reason| reason.chars().take(MAX_REASON_CHARS).collect());
        self.lock()?
            .execute(
                "INSERT OR REPLACE INTO learn_skill_promotions(request_id,script,state,skill,revision,reason,at)
                 VALUES(?,?,?,?,?,?,?)",
                params![
                    request,
                    promotion.script,
                    promotion.state.as_str(),
                    promotion.skill,
                    revision,
                    reason,
                    at
                ],
            )
            .map_err(sql)?;
        Ok(())
    }

    /// The promotion records of `request`, by script.
    pub fn promotions(&self, request: &str) -> io::Result<Vec<SkillPromotion>> {
        let db = self.lock()?;
        let mut query = db
            .prepare(
                "SELECT script,state,skill,revision,reason,at FROM learn_skill_promotions
                 WHERE request_id=? ORDER BY script LIMIT 64",
            )
            .map_err(sql)?;
        let rows = query
            .query_map([request], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, i64>(5)?,
                ))
            })
            .map_err(sql)?;
        let mut promotions = Vec::new();
        for row in rows {
            let (script, state, skill, revision, reason, at) = row.map_err(sql)?;
            promotions.push(SkillPromotion {
                script,
                state: PromotionState::parse(&state)
                    .ok_or_else(|| sql(format!("Unknown promotion state {state}")))?,
                skill,
                revision: revision.map(|revision| revision.max(0) as u64),
                reason,
                at: at.max(0) as u64,
            });
        }
        Ok(promotions)
    }
}

#[cfg(test)]
#[path = "skills_tests.rs"]
mod tests;
