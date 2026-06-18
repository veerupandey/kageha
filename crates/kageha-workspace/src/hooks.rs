//! Declarative lifecycle hooks. Hosts provide approved, journaled execution ports.
use crate::ReadWorkspace;
use kageha_contracts::execution::Control;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    io,
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
const EVENTS: &[&str] = &[
    "preToolUse",
    "postToolUse",
    "beforeShell",
    "afterFileEdit",
    "preFileWrite",
    "postFileWrite",
    "postFileCreate",
    "postFileDelete",
    "preCommit",
    "postCommit",
    "sessionStart",
    "sessionEnd",
    "planApproved",
    "specStageComplete",
    "agentStuck",
    "budgetWarning",
    "contextOverflow",
    "stop",
    "preCompact",
    "subagentStart",
    "subagentStop",
];
fn invalid(s: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, s)
}
pub fn normalize(event: &str) -> Option<&'static str> {
    if event.eq_ignore_ascii_case("BeforeShellExecution") {
        return Some("beforeShell");
    }
    EVENTS
        .iter()
        .copied()
        .find(|e| e.eq_ignore_ascii_case(event))
}
fn digest(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Hook {
    pub event: String,
    pub command: String,
    pub http: String,
    pub deny_message: String,
    pub timeout_s: f64,
    pub matcher: String,
    pub config_path: PathBuf,
    pub config_index: usize,
    pub config_sha256: String,
    /// Immutable, previewed plugin files; local project hooks leave this empty.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub plugin_files: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_message: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub plugin_hook: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_matcher_regex: Option<String>,
}
fn is_false(value:&bool)->bool {!*value}
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Set {
    pub hooks: Vec<Hook>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Outcome {
    pub allowed: bool,
    pub message: String,
    pub extra_context: String,
    pub ran: usize,
    pub warning: Option<String>,
}
impl Default for Outcome {
    fn default() -> Self {
        Self {
            allowed: true,
            message: String::new(),
            extra_context: String::new(),
            ran: 0,
            warning: None,
        }
    }
}
/// An executor authorizes first, applies Hook::control at dispatch, journals
/// intent before effects, and preserves unknown outcomes without replay.
pub trait Executor {
    fn execute(&self, hook: &Hook, payload: &Value, control: &Control<'_>) -> io::Result<Outcome>;
}
pub fn validate_plugin_matcher(pattern:&str)->bool {
    pattern.len()<=256 && regex::RegexBuilder::new(pattern).size_limit(1024*1024).nest_limit(50).build().is_ok()
}
impl Hook {
    pub fn timeout_limit(&self)->f64 {
        if self.plugin_hook && self.config_path.starts_with("/plugin/") && self.event=="stop" {900.0} else {120.0}
    }
    pub fn matches(&self, event: &str, tool: &str) -> bool {
        if self.event!=event {return false;}
        if let Some(pattern)=self.plugin_matcher_regex.as_deref() {
            if !self.plugin_hook || !self.config_path.starts_with("/plugin/") {return false;}
            let Ok(regex)=regex::RegexBuilder::new(pattern).size_limit(1024*1024).nest_limit(50).build() else {return false};
            let alias=match tool {"write_file"=>"Write","edit_file"=>"Edit","read_file"=>"Read","shell"=>"Bash",other=>other};
            return regex.is_match(tool) || regex.is_match(alias);
        }
        self.matcher.is_empty() || tool.is_empty() || tool.contains(&self.matcher)
            || glob::Pattern::new(&self.matcher).is_ok_and(|p|p.matches(tool))
    }
    pub fn resource(&self) -> String {
        digest(format!("project-hook-source\0{}", self.config_path.display()).as_bytes())
    }
    pub fn fingerprint(&self) -> String {
        digest(&serde_json::to_vec(self).expect("typed hook serialization"))
    }
    pub fn control<'a>(&self, parent: &Control<'a>) -> Control<'a> {
        let deadline = Instant::now() + Duration::from_secs_f64(self.timeout_s);
        Control {
            cancelled: parent.cancelled,
            deadline: Some(parent.deadline.unwrap_or(deadline).min(deadline)),
        }
    }
}
impl Set {
    pub fn parse(raw: &Value, path: &Path, sha: &str) -> io::Result<Self> {
        let mut rows = Vec::new();
        match raw {
            Value::Array(items) => rows = items.clone(),
            Value::Object(object) => match object.get("hooks") {
                Some(Value::Array(items)) => rows = items.clone(),
                Some(Value::Object(events)) => {
                    for (event, items) in events {
                        let canonical = normalize(event)
                            .ok_or_else(|| invalid("Unknown lifecycle hook event"))?;
                        for item in items
                            .as_array()
                            .ok_or_else(|| invalid("Hook event handlers must be lists"))?
                        {
                            let mut item = if let Some(command) = item.as_str() {
                                json!({"command":command})
                            } else {
                                item.clone()
                            };
                            let object = item.as_object_mut().ok_or_else(|| {
                                invalid("Hook definition must be an object or command")
                            })?;
                            object.insert("event".into(), json!(canonical));
                            rows.push(item);
                            if rows.len() > 256 {
                                return Err(invalid("Too many lifecycle hooks"));
                            }
                        }
                    }
                }
                _ => {
                    return Err(invalid(
                        "Hook configuration must contain a list or event map",
                    ))
                }
            },
            _ => return Err(invalid("Hook configuration must be a list or object")),
        };
        if rows.len() > 256 || serde_json::to_vec(raw)?.len() > 256 * 1024 {
            return Err(invalid("Hook configuration exceeds limits"));
        }
        let mut hooks = Vec::new();
        for (index, row) in rows.into_iter().enumerate() {
            let event = normalize(
                row["event"]
                    .as_str()
                    .ok_or_else(|| invalid("Hook event is required"))?,
            )
            .ok_or_else(|| invalid("Unknown lifecycle hook event"))?;
            let field = |names: &[&str], limit: usize| -> io::Result<String> {
                let value = names.iter().find_map(|name| {
                    row.get(*name)
                        .filter(|v| !v.is_null() && v.as_str() != Some(""))
                });
                let value = match value {
                    None => "",
                    Some(v) => v
                        .as_str()
                        .ok_or_else(|| invalid("Hook values must be text"))?,
                };
                if value.len() > limit || value.contains('\0') {
                    return Err(invalid("Hook value exceeds limits"));
                }
                Ok(value.into())
            };
            let command = field(&["command", "script"], 16384)?;
            let http = field(&["http", "url"], 4096)?;
            let deny_message = field(&["deny_message", "deny"], 4000)?;
            let matcher = field(&["matcher", "tool"], 256)?;
            let timeout_s = match row.get("timeout_s").or_else(|| row.get("timeout")) {
                None => 15.0,
                Some(v) => v
                    .as_f64()
                    .filter(|n| n.is_finite() && *n > 0.0 && *n <= 120.0)
                    .ok_or_else(|| {
                        invalid("Hook timeout must be positive and at most 120 seconds")
                    })?,
            };
            if command.is_empty() && http.is_empty() && deny_message.is_empty() {
                return Err(invalid(
                    "Hook requires a command, HTTP destination or denial",
                ));
            }
            if !http.is_empty()
                && (!http.starts_with("https://") && !http.starts_with("http://")
                    || http.chars().any(char::is_control))
            {
                return Err(invalid("Hook HTTP destination must use HTTP(S)"));
            }
            hooks.push(Hook {
                event: event.into(),
                command,
                http,
                deny_message,
                timeout_s,
                matcher,
                config_path: path.into(),
                config_index: index,
                config_sha256: sha.into(),
                plugin_files: BTreeMap::new(),
                status_message: None,
                plugin_hook: false,
                plugin_matcher_regex: None,
            });
        }
        Ok(Self { hooks })
    }
    pub fn load(paths: &[PathBuf], control: &Control<'_>) -> io::Result<Self> {
        let mut result = Self::default();
        for path in paths {
            control.check()?;
            let Some(parent) = path.parent() else {
                return Err(invalid("Hook configuration directory is required"));
            };
            if !parent.exists() {
                continue;
            }
            let reader = ReadWorkspace::new(parent)?;
            let name = path
                .file_name()
                .and_then(|v| v.to_str())
                .ok_or_else(|| invalid("Invalid hook configuration path"))?;
            let bytes = match reader.snapshot(name, 256 * 1024, control) {
                Ok(b) => b.bytes,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            let raw = serde_json::from_slice(&bytes)
                .map_err(|_| invalid("Invalid lifecycle hook JSON"))?;
            let loaded = Self::parse(&raw, &parent.canonicalize()?.join(name), &digest(&bytes))?;
            result.hooks.extend(loaded.hooks);
            if result.hooks.len() > 256 {
                return Err(invalid("Too many combined lifecycle hooks"));
            }
        }
        Ok(result)
    }
    pub fn run(
        &self,
        event: &str,
        tool: &str,
        payload: &Value,
        executor: &dyn Executor,
        control: &Control<'_>,
    ) -> io::Result<Outcome> {
        let event = normalize(event).ok_or_else(|| invalid("Unknown hook invocation event"))?;
        if !payload.is_object() || serde_json::to_vec(payload)?.len() > 100000 {
            return Err(invalid("Hook payload exceeds limits"));
        }
        if self.hooks.len() > 256
            || self.hooks.iter().any(|h| {
                !h.timeout_s.is_finite()
                    || h.timeout_s <= 0.0
                    || h.timeout_s > h.timeout_limit()
                    || h.command.len() > 16384
                    || h.http.len() > 4096
                    || h.matcher.len() > 256
                    || normalize(&h.event).is_none()
            })
        {
            return Err(invalid("Invalid stored hook configuration"));
        }
        let mut result = Outcome::default();
        for hook in self.hooks.iter().filter(|h| h.matches(event, tool)) {
            control.check()?;
            if hook.command.is_empty() && hook.http.is_empty() {
                if !hook.deny_message.is_empty() {
                    result.allowed = false;
                    result.message = hook.deny_message.clone();
                    return Ok(result);
                }
                continue;
            }
            let mut out = executor.execute(hook, payload, control)?;
            out.ran = 1;
            if !out.extra_context.is_empty() {
                let separator = usize::from(!result.extra_context.is_empty());
                let remaining = 16384usize.saturating_sub(result.extra_context.len() + separator);
                if remaining > 0 {
                    if !result.extra_context.is_empty() {
                        result.extra_context.push('\n');
                    }
                    let mut end = remaining.min(out.extra_context.len());
                    while !out.extra_context.is_char_boundary(end) {
                        end -= 1;
                    }
                    result.extra_context.push_str(&out.extra_context[..end]);
                }
            }
            result.ran += 1;
            if !out.message.is_empty() {
                result.message = out.message;
            }
            if out.warning.is_some() {
                result.warning = out.warning;
            }
            if !out.allowed {
                result.allowed = false;
                if result.message.is_empty() {
                    result.message = hook.deny_message.clone();
                }
                return Ok(result);
            }
        }
        Ok(result)
    }
}
/// Decode bounded command/webhook output without losing hard exit-2 denial.
pub fn output(bytes: &[u8], exit: Option<i32>) -> io::Result<Outcome> {
    if bytes.len() > 65536 {
        return Err(invalid("Hook response exceeds limit"));
    }
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim();
    let mut out = Outcome::default();
    out.ran = 1;
    if exit.is_some_and(|v| v != 0) {
        out.warning = Some(format!("Hook command exited with status {}", exit.unwrap()));
    }
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        if value.is_object() {
            out.allowed = value["allowed"] != false && value["decision"] != "block";
            out.message = value["message"].as_str().or_else(||value["reason"].as_str())

                .unwrap_or("")
                .chars()
                .take(4000)
                .collect();
            out.extra_context = value["extra_context"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(4000)
                .collect();
        } else {
            out.extra_context = text.chars().take(4000).collect();
        }
    } else {
        out.extra_context = text.chars().take(4000).collect();
    }
    if exit == Some(2) {
        out.allowed = false;
        if out.message.is_empty() {
            out.message = text.chars().take(4000).collect();
        }
    }
    Ok(out)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    struct Fake(AtomicUsize);
    impl Executor for Fake {
        fn execute(&self, _: &Hook, _: &Value, _: &Control<'_>) -> io::Result<Outcome> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(Outcome {
                extra_context: "context".into(),
                ..Outcome::default()
            })
        }
    }
    #[test]
    fn retained_shapes_aliases_matchers_and_static_denials() {
        let set=Set::parse(&json!({"hooks":{"PreToolUse":[{"matcher":"browser_*","deny":"excluded"}],"BeforeShellExecution":["echo safe"]}}),Path::new("/fixture/hooks.json"),"digest").unwrap();
        let flag = AtomicBool::new(false);
        let control = Control {
            cancelled: &flag,
            deadline: None,
        };
        let fake = Fake(AtomicUsize::new(0));
        assert!(
            !set.run("preToolUse", "browser_click", &json!({}), &fake, &control)
                .unwrap()
                .allowed
        );
        assert_eq!(fake.0.load(Ordering::Relaxed), 0);
        assert_eq!(
            set.run("beforeShell", "bash", &json!({}), &fake, &control)
                .unwrap()
                .extra_context,
            "context"
        );
        assert!(Set::parse(
            &json!([{"event":"unknown","command":"x"}]),
            Path::new("hooks.json"),
            "digest"
        )
        .is_err());
    }
    #[test]
    fn command_results_keep_hard_denials_and_soft_warnings() {
        assert!(!output(br#"{"allowed":true}"#, Some(2)).unwrap().allowed);
        let soft = output(b"Useful context", Some(1)).unwrap();
        assert!(soft.allowed);
        assert!(soft.warning.is_some());
        assert_eq!(soft.extra_context, "Useful context");
        assert!(
            !output(br#"{"allowed":false,"message":"no"}"#, None)
                .unwrap()
                .allowed
        );
    }
    #[test]
    fn empty_aliases_fall_through_and_combined_context_stays_bounded() {
        struct Large;
        impl Executor for Large {
            fn execute(&self, _: &Hook, _: &Value, _: &Control<'_>) -> io::Result<Outcome> {
                Ok(Outcome {
                    extra_context: "é".repeat(4000),
                    ..Outcome::default()
                })
            }
        }
        let row = json!({"event":"preToolUse","command":"","script":"approved command"});
        let set = Set::parse(
            &json!([row.clone(), row.clone(), row]),
            Path::new("hooks.json"),
            "digest",
        )
        .unwrap();
        assert_eq!(set.hooks[0].command, "approved command");
        let flag = AtomicBool::new(false);
        let out = set
            .run(
                "preToolUse",
                "test",
                &json!({}),
                &Large,
                &Control {
                    cancelled: &flag,
                    deadline: None,
                },
            )
            .unwrap();
        assert!(out.extra_context.len() <= 16384);
        assert_eq!(out.ran, 3);
    }
}
