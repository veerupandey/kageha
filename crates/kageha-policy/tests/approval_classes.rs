// Feature: autonomous-agent-loop, Property 18: Approval classes
//
// For any Host action call, the call dispatches without a prompt iff its action class
// defaults to allow, or an allowlist entry matches both its class and its scope.
// Delegated workers can't create allowlist entries.
//
// **Validates: Requirements 2.6, 19.3, 19.4, 19.5**
//
// The oracle below restates the design rules (design section 14) from the generated
// structure of each call: it knows which hosts, words and shell operators went into a
// URL or command, so it doesn't parse them back the way `classes.rs` does.
use kageha_policy::classes::{
    add_allow, classify, decide, ActionClass, AllowEntry, Allowlist, ClassifyContext, Decision, Scope,
    ALLOWLIST_FILE, SECTION, SECTION_VERSION,
};
use proptest::prelude::*;
use proptest::sample::select;
use serde_json::{json, Map, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    net::IpAddr,
};
use ActionClass::*;

// ---------------------------------------------------------------------------------------
// Known Host tools (the Capability_Registry's `Read` tools and the design table)
// ---------------------------------------------------------------------------------------

const READ_TOOLS: &[&str] = &[
    "read_file", "list_workspace_files", "find_past_files", "search_past_chats", "discover_capabilities",
    "load_skill", "skill_read", "knowledge_search", "knowledge_list", "memory_get", "memory_search",
    "web_search", "news_search", "web_fetch", "integration_read", "integration_check", "remote_receipts",
    "computer_info", "computer_permissions", "computer_apps", "computer_windows", "computer_activity",
    "computer_inspect", "computer_receipt", "computer_observe", "browser_open", "browser_tabs",
    "browser_snapshot", "browser_validate", "browser_inspect", "browser_receipts", "browser_reconcile",
    "browser_preview_status", "transcribe_audio", "agent_status", "agent_wait", "agent_wait_any",
];
const WORKSPACE_TOOLS: &[&str] = &[
    "write_file", "edit_file", "memory_put", "memory_delete", "text_to_speech", "generate_image", "ask_human",
    "agent_spawn", "agent_cancel", "computer_release", "record_required_outcomes", "amend_required_outcome",
    "record_outcome_evidence", "report_blocker", "task_plan_update", "task_plan_revise", "propose_run_change",
    "goal_update",
];
const MESSAGE_TOOLS: &[&str] = &["channel_reply", "send_email", "email_send", "send_message", "message_send"];
const SHELL_TOOLS: &[&str] = &["shell", "bash", "shell_job"];
const BROWSER_INPUT: &[&str] = &[
    "browser_click", "browser_press", "browser_fill", "browser_fill_credential", "browser_select",
    "browser_check", "browser_upload",
];
/// Browser input that stays a draft on the page until something submits it.
const BROWSER_DRAFT: &[&str] = &["browser_fill", "browser_fill_credential", "browser_select", "browser_check"];
const OTHER_TOOLS: &[&str] = &["remote_exec", "remote_upload", "browser_credential", "mcp_call"];

fn known(tool: &str) -> bool {
    [READ_TOOLS, WORKSPACE_TOOLS, MESSAGE_TOOLS, SHELL_TOOLS, BROWSER_INPUT, OTHER_TOOLS]
        .iter()
        .any(|list| list.contains(&tool))
}

// ---------------------------------------------------------------------------------------
// Generated inputs
// ---------------------------------------------------------------------------------------

const WORDS: &[&str] = &[
    "git", "push", "origin", "main", "ls", "-la", "python", "run.py", "uv", "pip", "install", "npm", "test",
    "gitk", "rm", "-rf", "FOO=1", "build_2", "Push", "status",
];
const OPERATORS: &[&str] = &["&&", ";", "|", "||", "$(", "`", ">", "&"];
const LABELS: &[&str] = &["example", "com", "mail", "shop", "evil", "net", "badexample", "co", "uk", "10", "0", "1"];
const IP_HOSTS: &[&str] = &["10.0.0.1", "1.10.0.0.1", "192.168.0.1", "0.0.1", "0.1"];
const CONNECTORS: &[&str] = &["slack", "slack-2", "gmail", "email"];
const MCP_SCOPES: &[&str] = &["gmail/send", "gmail/send_all", "stripe/charge", "mcp_gmail_send", "mcp_blog_post"];
const TOOL_SCOPES: &[&str] = &[
    "brand_new_tool", "send_email", "browser_click", "browser_fill", "mcp_call", "shell", "remote_upload",
    "remote_exec", "wordpress_post",
];
const TAGS: &[&str] = &["send", "payment", "publish", "read", "misc"];

#[derive(Debug, Clone)]
enum Tok {
    Word(&'static str),
    Op(&'static str),
    /// An operator written without spaces, such as `push;rm`.
    Glued(&'static str, &'static str, &'static str),
}

impl Tok {
    fn text(&self) -> String {
        match self {
            Tok::Word(w) | Tok::Op(w) => (*w).to_owned(),
            Tok::Glued(a, op, b) => format!("{a}{op}{b}"),
        }
    }
}

#[derive(Debug, Clone)]
struct Cmd {
    toks: Vec<Tok>,
    gaps: Vec<usize>,
    lead: bool,
}

impl Cmd {
    fn text(&self) -> String {
        let mut out = if self.lead { " ".to_owned() } else { String::new() };
        for (i, tok) in self.toks.iter().enumerate() {
            if i > 0 {
                out.push_str(&" ".repeat(self.gaps[i]));
            }
            out.push_str(&tok.text());
        }
        out
    }
    fn words(&self) -> Vec<String> {
        self.toks.iter().map(Tok::text).collect()
    }
    /// A plain command: words only, and no leading environment assignment.
    fn simple(&self) -> bool {
        !self.toks.is_empty()
            && self.toks.iter().all(|t| matches!(t, Tok::Word(_)))
            && !matches!(self.toks.first(), Some(Tok::Word(w)) if w.contains('='))
    }
}

#[derive(Debug, Clone)]
struct Host {
    labels: Vec<&'static str>,
    upper: bool,
    trailing_dot: bool,
}

impl Host {
    fn text(&self) -> String {
        let host = self.labels.join(".");
        let host = if self.upper { host.to_ascii_uppercase() } else { host };
        if self.trailing_dot { format!("{host}.") } else { host }
    }
    fn expected(&self) -> String {
        self.labels.join(".")
    }
}

#[derive(Debug, Clone)]
struct Url {
    scheme: bool,
    userinfo: Option<Host>,
    host: Host,
    port: Option<u16>,
    path: bool,
}

impl Url {
    fn text(&self) -> String {
        let mut out = String::new();
        if self.scheme {
            out.push_str("https://");
        }
        if let Some(user) = &self.userinfo {
            out.push_str(&user.text());
            out.push('@');
        }
        out.push_str(&self.host.text());
        if let Some(port) = self.port {
            out.push_str(&format!(":{port}"));
        }
        if self.path {
            out.push_str("/a/b?c=d#e");
        }
        out
    }
}

#[derive(Debug, Clone)]
struct ArgsSpec {
    command: Option<Cmd>,
    escalate: Option<Value>,
    submission: Option<Value>,
    url: Option<Url>,
    connector: Option<&'static str>,
    server: Option<&'static str>,
    tool: Option<&'static str>,
}

impl ArgsSpec {
    fn value(&self) -> Value {
        let mut args = Map::new();
        if let Some(cmd) = &self.command {
            args.insert("command".into(), cmd.text().into());
        }
        if let Some(v) = &self.escalate {
            args.insert("escalate".into(), v.clone());
        }
        if let Some(v) = &self.submission {
            args.insert("application_submission".into(), v.clone());
        }
        if let Some(url) = &self.url {
            args.insert("url".into(), url.text().into());
        }
        if let Some(c) = self.connector {
            args.insert("connector".into(), c.into());
        }
        if let Some(s) = self.server {
            args.insert("server".into(), s.into());
        }
        if let Some(t) = self.tool {
            args.insert("tool".into(), t.into());
        }
        Value::Object(args)
    }
}

#[derive(Debug, Clone)]
struct CtxSpec {
    url: Option<Url>,
    outside: bool,
    submit: bool,
    checkout: bool,
    settings: bool,
    tags: BTreeSet<&'static str>,
    connector: Option<&'static str>,
    remote_host: Option<Host>,
}

impl CtxSpec {
    fn context(&self) -> ClassifyContext {
        ClassifyContext {
            url: self.url.as_ref().map(Url::text),
            outside_workspace: self.outside,
            submit_target: self.submit,
            checkout_page: self.checkout,
            settings_page: self.settings,
            tags: self.tags.iter().map(|t| (*t).to_owned()).collect(),
            connector: self.connector.map(str::to_owned),
            remote_host: self.remote_host.as_ref().map(Host::text),
        }
    }
}

/// An allowlist entry and whether the oracle considers it storable.
#[derive(Debug, Clone)]
struct EntrySpec {
    entry: AllowEntry,
    storable: bool,
}

fn tok() -> impl Strategy<Value = Tok> {
    prop_oneof![
        6 => select(WORDS).prop_map(Tok::Word),
        1 => select(OPERATORS).prop_map(Tok::Op),
        1 => (select(WORDS), select(OPERATORS), select(WORDS)).prop_map(|(a, op, b)| Tok::Glued(a, op, b)),
    ]
}

fn cmd(len: std::ops::Range<usize>) -> impl Strategy<Value = Cmd> {
    (prop::collection::vec(tok(), len), prop::collection::vec(1usize..=3, 6), any::<bool>())
        .prop_map(|(toks, gaps, lead)| Cmd { toks, gaps, lead })
}

fn host() -> impl Strategy<Value = Host> {
    let labels = prop_oneof![
        2 => prop::collection::vec(select(LABELS), 1..5),
        1 => select(IP_HOSTS).prop_map(|h: &'static str| h.split('.').collect::<Vec<&'static str>>()),
    ];
    (labels, prop::bool::weighted(0.2), prop::bool::weighted(0.2))
        .prop_map(|(labels, upper, trailing_dot)| Host { labels, upper, trailing_dot })
}

fn url() -> impl Strategy<Value = Url> {
    (any::<bool>(), prop::option::weighted(0.15, host()), host(), prop::option::weighted(0.3, 1u16..9000), any::<bool>())
        .prop_map(|(scheme, userinfo, host, port, path)| Url { scheme, userinfo, host, port, path })
}

fn flag() -> impl Strategy<Value = Option<Value>> {
    prop::option::of(select(vec![json!(true), json!(false), json!("true"), json!(1)]))
}

fn args() -> impl Strategy<Value = ArgsSpec> {
    (
        prop::option::weighted(0.7, cmd(0..6)),
        flag(),
        flag(),
        prop::option::of(url()),
        prop::option::of(select(CONNECTORS)),
        prop::option::of(select(&["gmail", "stripe"][..])),
        prop::option::of(select(&["send", "send_all", "charge"][..])),
    )
        .prop_map(|(command, escalate, submission, url, connector, server, tool)| ArgsSpec {
            command,
            escalate,
            submission,
            url,
            connector,
            server,
            tool,
        })
}

fn ctx() -> impl Strategy<Value = CtxSpec> {
    (
        prop::option::of(url()),
        prop::bool::weighted(0.25),
        prop::bool::weighted(0.25),
        prop::bool::weighted(0.2),
        prop::bool::weighted(0.2),
        prop::collection::btree_set(select(TAGS), 0..3),
        prop::option::of(select(CONNECTORS)),
        prop::option::of(host()),
    )
        .prop_map(|(url, outside, submit, checkout, settings, tags, connector, remote_host)| CtxSpec {
            url,
            outside,
            submit,
            checkout,
            settings,
            tags,
            connector,
            remote_host,
        })
}

fn tool() -> impl Strategy<Value = String> {
    let all: Vec<&'static str> = [READ_TOOLS, WORKSPACE_TOOLS, MESSAGE_TOOLS, SHELL_TOOLS, BROWSER_INPUT, OTHER_TOOLS]
        .concat();
    prop_oneof![
        4 => select(all).prop_map(str::to_owned),
        1 => "[a-z_]{0,14}",
        1 => "mcp_[a-z_]{1,10}",
        1 => select(&["mcp_gmail_send", "mcp_blog_post", "brand_new_tool", "wordpress_post"][..]).prop_map(str::to_owned),
    ]
}

fn entry() -> impl Strategy<Value = EntrySpec> {
    let scope = prop_oneof![
        host().prop_map(|h| {
            let text = h.text();
            let storable = text == h.expected();
            (Scope::Domain(text), storable)
        }),
        cmd(1..4).prop_map(|c| (Scope::Command(c.text()), c.simple())),
        select(CONNECTORS).prop_map(|c| (Scope::Connector(c.into()), true)),
        select(MCP_SCOPES).prop_map(|m| (Scope::McpTool(m.into()), true)),
        select(TOOL_SCOPES).prop_map(|t| (Scope::Tool(t.into()), true)),
    ];
    (select(ActionClass::ALL.to_vec()), scope)
        .prop_map(|(class, (scope, storable))| EntrySpec { entry: AllowEntry { class, scope }, storable })
}

// ---------------------------------------------------------------------------------------
// Oracle
// ---------------------------------------------------------------------------------------

fn oracle_default_allows(class: ActionClass) -> bool {
    // Requirement 19.3: workspace work, package installs and reads. Everything else asks (19.4).
    matches!(class, Workspace | Read)
}

fn subcommand(word: &str) -> bool {
    word.len() <= 32
        && word.starts_with(|c: char| c.is_ascii_lowercase())
        && word.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// The scope of a shell-like call and whether its command is compound.
fn oracle_command_scope(tool: &str, command: Option<&Cmd>) -> (Scope, bool) {
    match command.filter(|c| !c.text().is_empty()) {
        None => (Scope::Tool(tool.into()), false),
        Some(c) if !c.simple() => (Scope::Command(c.words().join(" ")), true),
        Some(c) => {
            let words = c.words();
            let prefix = match words.get(1) {
                Some(sub) if subcommand(sub) => format!("{} {sub}", words[0]),
                _ => words[0].clone(),
            };
            (Scope::Command(prefix), false)
        }
    }
}

/// The class and scope the design gives a call, plus whether its command is compound.
fn oracle_classify(tool: &str, a: &ArgsSpec, c: &CtxSpec) -> (ActionClass, Scope, bool) {
    let is_true = |v: &Option<Value>| *v == Some(Value::Bool(true));
    let tool_scope = Scope::Tool(tool.into());
    // Host-owned page URL first; the model's `url` argument only when the Host has none.
    let page = c
        .url
        .as_ref()
        .or(a.url.as_ref())
        .map(|u| Scope::Domain(u.host.expected()))
        .unwrap_or_else(|| tool_scope.clone());
    let connector = c.connector.map(|x| Scope::Connector(x.into())).unwrap_or_else(|| tool_scope.clone());
    let tag = if c.tags.contains("payment") {
        Some(Payment)
    } else if c.tags.contains("publish") {
        Some(Publish)
    } else if c.tags.contains("send") {
        Some(MessageSend)
    } else {
        None
    };
    let mcp_class = tag.unwrap_or(if c.tags.contains("read") { Read } else { ExternalWrite });
    if SHELL_TOOLS.contains(&tool) {
        let class = if is_true(&a.escalate) || c.outside { ExternalWrite } else { Workspace };
        let (scope, compound) = oracle_command_scope(tool, a.command.as_ref());
        return (class, scope, compound);
    }
    if matches!(tool, "write_file" | "edit_file") && c.outside {
        return (ExternalWrite, tool_scope, false);
    }
    if matches!(tool, "web_fetch" | "browser_open") {
        return (Read, page, false);
    }
    if matches!(tool, "integration_read" | "integration_check") {
        let scope = a.connector.or(c.connector).map(|x| Scope::Connector(x.into())).unwrap_or(tool_scope);
        return (Read, scope, false);
    }
    if READ_TOOLS.contains(&tool) {
        return (Read, tool_scope, false);
    }
    if WORKSPACE_TOOLS.contains(&tool) {
        return (Workspace, tool_scope, false);
    }
    if MESSAGE_TOOLS.contains(&tool) {
        return (MessageSend, connector, false);
    }
    match tool {
        "remote_exec" => {
            let (scope, compound) = oracle_command_scope(tool, a.command.as_ref());
            (ExternalWrite, scope, compound)
        }
        "remote_upload" => (
            ExternalWrite,
            c.remote_host.as_ref().map(|h| Scope::Domain(h.expected())).unwrap_or(tool_scope),
            false,
        ),
        "browser_credential" => (AccountChange, page, false),
        t if BROWSER_INPUT.contains(&t) => {
            let class = if c.checkout {
                Payment
            } else if c.settings {
                AccountChange
            } else if is_true(&a.submission) || c.submit {
                FormSubmit
            } else if BROWSER_DRAFT.contains(&t) {
                Workspace
            } else {
                ExternalWrite
            };
            (class, page, false)
        }
        "mcp_call" => {
            let scope = match (a.server, a.tool) {
                (Some(s), Some(t)) => Scope::McpTool(format!("{s}/{t}")),
                _ => tool_scope,
            };
            (mcp_class, scope, false)
        }
        t if t.starts_with("mcp_") => (mcp_class, Scope::McpTool(t.into()), false),
        _ => (tag.unwrap_or(ExternalWrite), connector, false),
    }
}

/// Whether an entry scope covers a call scope under the design's matching rules.
fn oracle_covers(entry: &Scope, call: &Scope, call_compound: bool) -> bool {
    match (entry, call) {
        (Scope::Domain(e), Scope::Domain(h)) => {
            let (e, h) = (e.to_ascii_lowercase(), h.to_ascii_lowercase());
            if e.is_empty() || h.is_empty() {
                return false;
            }
            // An IP address has no subdomains and is no one's subdomain.
            if e.parse::<IpAddr>().is_ok() || h.parse::<IpAddr>().is_ok() {
                return e == h;
            }
            let el: Vec<&str> = e.split('.').collect();
            let hl: Vec<&str> = h.split('.').collect();
            hl.len() >= el.len() && hl[hl.len() - el.len()..] == el[..]
        }
        (Scope::Command(e), Scope::Command(c)) => {
            let ew: Vec<&str> = e.split_whitespace().collect();
            let cw: Vec<&str> = c.split_whitespace().collect();
            !call_compound && !ew.is_empty() && cw.starts_with(&ew)
        }
        (Scope::Connector(a), Scope::Connector(b))
        | (Scope::McpTool(a), Scope::McpTool(b))
        | (Scope::Tool(a), Scope::Tool(b)) => !a.is_empty() && a == b,
        _ => false,
    }
}

fn oracle_decide(class: ActionClass, scope: &Scope, compound: bool, entries: &[AllowEntry]) -> Decision {
    if oracle_default_allows(class) || entries.iter().any(|e| e.class == class && oracle_covers(&e.scope, scope, compound)) {
        Decision::Allow
    } else {
        Decision::Ask
    }
}

/// Inserts every generated entry, checking each result against the oracle, and returns
/// the allowlist with the entries the oracle expects it to hold.
fn build(entries: &[EntrySpec]) -> Result<(Allowlist, Vec<AllowEntry>), TestCaseError> {
    let mut list = Allowlist::default();
    let mut expected: Vec<AllowEntry> = Vec::new();
    for spec in entries {
        let result = list.insert(spec.entry.clone());
        if !spec.storable {
            prop_assert!(result.is_err(), "unstorable entry accepted: {:?}", spec.entry);
        } else if oracle_default_allows(spec.entry.class) || expected.contains(&spec.entry) {
            prop_assert_eq!(result.ok(), Some(false), "redundant entry {:?}", spec.entry);
        } else {
            prop_assert_eq!(result.ok(), Some(true), "entry {:?}", spec.entry);
            expected.push(spec.entry.clone());
        }
    }
    prop_assert_eq!(list.entries(), &expected[..]);
    Ok((list, expected))
}

fn legacy_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        "[a-z_]{1,8}\\|[a-z_]{1,10}\\|[ -~\n]{0,12}".prop_map(Value::String),
        any::<i64>().prop_map(|n| json!(n)),
        Just(json!({"action": "bash", "risk": "safe"})),
    ]
}

fn other_keys() -> impl Strategy<Value = BTreeMap<String, Value>> {
    prop::collection::btree_map(
        "x_[a-z]{1,6}",
        prop_oneof!["[ -~]{0,10}".prop_map(Value::String), any::<i64>().prop_map(|n| json!(n)), any::<bool>().prop_map(Value::Bool)],
        0..3,
    )
}

fn object_file(legacy: &[Value], other: &BTreeMap<String, Value>, existing: &[AllowEntry]) -> Value {
    let mut object = Map::new();
    object.insert("entries".into(), Value::Array(legacy.to_vec()));
    for (k, v) in other {
        object.insert(k.clone(), v.clone());
    }
    let entries: Vec<Value> = existing.iter().map(|e| serde_json::to_value(e).unwrap()).collect();
    object.insert(SECTION.into(), json!({"version": SECTION_VERSION, "entries": entries}));
    Value::Object(object)
}

// ---------------------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    // Feature: autonomous-agent-loop, Property 18: Approval classes
    /// `decide(classify(..))` allows exactly the calls the design allows.
    #[test]
    fn decisions_match_the_design_oracle(
        tool in tool(),
        args in args(),
        ctx in ctx(),
        entries in prop::collection::vec(entry(), 0..6),
        delegated in any::<bool>(),
    ) {
        let (list, expected_entries) = build(&entries)?;
        let (class, scope) = classify(&tool, &args.value(), &ctx.context());
        let (o_class, o_scope, compound) = oracle_classify(&tool, &args, &ctx);
        prop_assert_eq!(class, o_class);
        prop_assert_eq!(&scope, &o_scope);
        // Delegated workers inherit the parent's policy and allowlist.
        prop_assert_eq!(
            decide(class, &scope, &list, delegated),
            oracle_decide(o_class, &o_scope, compound, &expected_entries)
        );
    }

    // Feature: autonomous-agent-loop, Property 18: Approval classes
    /// Adding an entry for class C and scope S only ever newly allows calls of class C
    /// whose scope S covers.
    #[test]
    fn adding_an_entry_never_over_grants(
        // Bias toward tools whose calls ask and carry a domain, command, connector or MCP scope.
        tool in prop_oneof![
            tool(),
            select(&[
                "browser_click", "browser_upload", "browser_credential", "remote_upload", "remote_exec", "shell",
                "mcp_call", "channel_reply",
            ][..]).prop_map(str::to_owned),
        ],
        args in args(),
        ctx in ctx(),
        base in prop::collection::vec(entry(), 0..4),
        extra in entry(),
        target in 0u8..5,
        other_class in select(ActionClass::ALL.to_vec()),
    ) {
        let (list, _) = build(&base)?;
        let (class, scope) = classify(&tool, &args.value(), &ctx.context());
        let (_, _, compound) = oracle_classify(&tool, &args, &ctx);
        // Aim most entries at the call: its own scope with its class or another one, a
        // parent of its scope (`0.0.1` for the IP `10.0.0.1`, `git` for `git push`), or a
        // lookalike that is a plain string suffix or prefix (`xample.com`, `gi`).
        let near = |scope: &Scope, parent: bool| match scope {
            Scope::Domain(h) if parent => Scope::Domain(h.split_once('.').map_or(h.as_str(), |(_, rest)| rest).to_owned()),
            Scope::Domain(h) => Scope::Domain(h.get(1..).unwrap_or_default().to_owned()),
            Scope::Command(c) if parent => Scope::Command(c.split_whitespace().next().unwrap_or_default().to_owned()),
            Scope::Command(c) => Scope::Command(c.get(..c.len().saturating_sub(1)).unwrap_or_default().to_owned()),
            other => other.clone(),
        };
        let extra = match target {
            0 => extra.entry,
            1 => AllowEntry { class, scope: scope.clone() },
            2 => AllowEntry { class: other_class, scope: scope.clone() },
            3 => AllowEntry { class, scope: near(&scope, true) },
            _ => AllowEntry { class, scope: near(&scope, false) },
        };
        let before = decide(class, &scope, &list, false);
        let mut after_list = list.clone();
        let inserted = after_list.insert(extra.clone());
        let after = decide(class, &scope, &after_list, false);
        prop_assert!(before == Decision::Ask || after == Decision::Allow, "adding an entry revoked an allow");
        if compound {
            prop_assert!(!(target == 1 && inserted.is_ok()), "compound command scope {:?} was storable", scope);
        }
        if before == Decision::Ask && after == Decision::Allow {
            prop_assert_eq!(extra.class, class, "entry {:?} granted another class", extra);
            prop_assert!(!compound, "entry {:?} granted compound command {:?}", extra, scope);
            prop_assert!(oracle_covers(&extra.scope, &scope, compound), "entry {:?} over-granted {:?}", extra, scope);
        }
    }

    // Feature: autonomous-agent-loop, Property 18: Approval classes
    /// Tools outside the Host's catalog never run without a prompt by default.
    #[test]
    fn unknown_tools_are_never_allowed_by_default(
        name in prop_oneof!["[a-z][a-z0-9_]{0,15}", "mcp_[a-z0-9_]{1,12}"],
        args in args(),
        ctx in ctx(),
        delegated in any::<bool>(),
    ) {
        prop_assume!(!known(&name));
        let (class, scope) = classify(&name, &args.value(), &ctx.context());
        let decision = decide(class, &scope, &Allowlist::default(), delegated);
        // Only a Host-configured `read` tag on an MCP tool (with no write tag) relaxes it.
        let host_read_tag = name.starts_with("mcp_")
            && ctx.tags.contains("read")
            && !["payment", "publish", "send"].iter().any(|t| ctx.tags.contains(t));
        prop_assert_eq!(decision, if host_read_tag { Decision::Allow } else { Decision::Ask });
        if !host_read_tag {
            prop_assert!(!oracle_default_allows(class), "{} classified as {:?}", name, class);
        }
    }

    // Feature: autonomous-agent-loop, Property 18: Approval classes
    /// `add_allow` from a delegated worker always fails and leaves the file untouched.
    #[test]
    fn delegated_workers_cannot_add_entries(
        state in 0u8..4,
        legacy in prop::collection::vec(legacy_value(), 0..4),
        other in other_keys(),
        existing in prop::collection::vec(entry(), 0..4),
        new in entry(),
    ) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(ALLOWLIST_FILE);
        let existing: Vec<AllowEntry> = existing.into_iter().filter(|e| e.storable).map(|e| e.entry).collect();
        match state {
            0 => {}
            1 => fs::write(&path, Value::Array(legacy).to_string()).unwrap(),
            2 => fs::write(&path, object_file(&legacy, &other, &existing).to_string()).unwrap(),
            _ => fs::write(&path, "{not json").unwrap(),
        }
        let before = fs::read(&path).ok();
        let error = add_allow(&path, new.entry, true).unwrap_err();
        prop_assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        prop_assert_eq!(fs::read(&path).ok(), before.clone());
        let files = fs::read_dir(dir.path()).unwrap().count();
        prop_assert_eq!(files, usize::from(before.is_some()), "a delegated add_allow left files behind");
    }

    // Feature: autonomous-agent-loop, Property 18: Approval classes
    /// The allowlist file round-trips: legacy entries, other keys and class entries survive
    /// an "Always allow", and only a new, storable, non-default entry changes the file.
    #[test]
    fn allowlist_round_trips_through_the_file(
        bare_list in prop::bool::weighted(0.3),
        legacy in prop::collection::vec(legacy_value(), 0..4),
        other in other_keys(),
        existing in prop::collection::vec(entry(), 0..4),
        new in entry(),
    ) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(ALLOWLIST_FILE);
        let existing: Vec<AllowEntry> = if bare_list {
            Vec::new()
        } else {
            existing.into_iter().filter(|e| e.storable).map(|e| e.entry).collect()
        };
        let file = if bare_list { Value::Array(legacy.clone()) } else { object_file(&legacy, &other, &existing) };
        fs::write(&path, file.to_string()).unwrap();
        let before = fs::read(&path).unwrap();

        let loaded = Allowlist::load(&path).unwrap();
        prop_assert_eq!(loaded.legacy(), &legacy[..]);
        prop_assert_eq!(loaded.entries(), &existing[..]);
        prop_assert_eq!(Allowlist::parse(loaded.to_value()).unwrap().to_value(), loaded.to_value());

        let result = add_allow(&path, new.entry.clone(), false);
        let redundant = oracle_default_allows(new.entry.class) || existing.contains(&new.entry);
        if !new.storable {
            prop_assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
            prop_assert_eq!(fs::read(&path).unwrap(), before);
            return Ok(());
        }
        prop_assert_eq!(result.unwrap(), !redundant);
        if redundant {
            prop_assert_eq!(fs::read(&path).unwrap(), before);
            return Ok(());
        }
        let saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        prop_assert_eq!(&saved["entries"], &Value::Array(legacy.clone()));
        if !bare_list {
            for (k, v) in &other {
                prop_assert_eq!(&saved[k], v);
            }
        }
        prop_assert_eq!(&saved[SECTION]["version"], &json!(SECTION_VERSION));
        let reloaded = Allowlist::load(&path).unwrap();
        let mut expected = existing.clone();
        expected.push(new.entry.clone());
        prop_assert_eq!(reloaded.entries(), &expected[..]);
        prop_assert_eq!(reloaded.legacy(), &legacy[..]);
        prop_assert_eq!(Allowlist::parse(reloaded.to_value()).unwrap(), reloaded.clone());
        prop_assert_eq!(&reloaded.to_value(), &saved);
        prop_assert!(reloaded.allows(new.entry.class, &new.entry.scope));
    }
}
