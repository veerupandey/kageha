//! Read cache (spec autonomous-agent-loop, design section 11, Requirement 12).
//!
//! A successful read-only call is stored per request, keyed on
//! `sha256(request_scope, tool, canonical arguments, revision)`. `request_scope` is the
//! request's `request_budget_id` (see `ui_task_completion::scope`), so resumed turns and
//! delegated workers of one request share entries. `revision` is the connector revision
//! for `integration_read`, the `source_id` for `web_fetch` and the Host's
//! `recovery_revision()` otherwise. A changed revision changes the key, so stale entries
//! are never matched (12.3). `refresh: true` dispatches the call and overwrites the entry
//! (12.4). A hit returns the stored result marked `cached` (12.1). Only tools whose
//! `CapabilitySpec.cacheable` is true reach this module (12.2).
//!
//! Entries are deleted when the request ends, and entries older than [`TTL_SECONDS`] are
//! dropped at startup. The local connector API (`ui_connector_api`) uses the same cache.
use crate::webui::Database;
use kageha_runtime::recovery::CACHED_NOTE_PREFIX;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::io;

/// Entries older than this are dropped at startup, whether or not their request ended.
pub(crate) const TTL_SECONDS: f64 = 24.0 * 60.0 * 60.0;
/// Larger results are not stored.
const MAX_CONTENT_BYTES: usize = 4 * 1024 * 1024;
/// Arguments that select cache behavior or are runtime bookkeeping, not part of the read.
const IGNORED_ARGUMENTS: [&str; 2] = ["refresh", "_runtime_revision"];

pub(crate) fn initialize(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS ui_read_cache(key TEXT PRIMARY KEY,scope TEXT NOT NULL,tool TEXT NOT NULL,
            revision TEXT NOT NULL,content TEXT NOT NULL,created REAL NOT NULL);
         CREATE INDEX IF NOT EXISTS ui_read_cache_scope ON ui_read_cache(scope,tool);",
    )?;
    db.execute("DELETE FROM ui_read_cache WHERE created<?", [crate::ui_store::now() - TTL_SECONDS])?;
    Ok(())
}

/// `value` with object keys sorted at every level.
fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut keys: Vec<_> = object.keys().collect();
            keys.sort();
            Value::Object(keys.into_iter().map(|key| (key.clone(), canonical(&object[key]))).collect::<Map<_, _>>())
        }
        Value::Array(values) => Value::Array(values.iter().map(canonical).collect()),
        other => other.clone(),
    }
}

/// The arguments sent to the tool: `refresh` and `_runtime_revision` removed.
pub(crate) fn dispatch_arguments(arguments: &Value) -> Value {
    let mut arguments = arguments.clone();
    if let Some(object) = arguments.as_object_mut() {
        for key in IGNORED_ARGUMENTS {
            object.remove(key);
        }
    }
    arguments
}

/// The cache key of one read.
pub(crate) fn key(scope: &str, tool: &str, arguments: &Value, revision: &str) -> String {
    let arguments = canonical(&dispatch_arguments(arguments)).to_string();
    format!("{:x}", Sha256::digest(json!([scope, tool, arguments, revision]).to_string().as_bytes()))
}

/// Whether `refresh: true` was passed.
pub(crate) fn refresh_requested(arguments: &Value) -> bool {
    arguments["refresh"] == true
}

/// Whether a successful result may be stored. Results that report a failure or a blocked
/// connector (disconnected, needs verification, agent not assigned) are read again.
pub(crate) fn storable(content: &str) -> bool {
    if content.len() > MAX_CONTENT_BYTES {
        return false;
    }
    let Ok(value) = serde_json::from_str::<Value>(content) else { return true };
    let blocked = |state: &Value| matches!(state.as_str(), Some("disconnected" | "needs_verification" | "agent_not_assigned"));
    let connector = &value["connector_outcome"];
    !(value["ok"] == false
        || value["error_code"].is_string()
        || value["error"].is_object()
        || blocked(&connector["state"])
        || connector["recovery"] == "manage_access"
        || blocked(&value["status"])
        || matches!(value["status"].as_str(), Some("error" | "failed" | "stopped")))
}

/// A stored result as returned on a hit: a JSON object gets `"cached":true` and
/// `"cached_at":<created>`; any other result gets a one-line note in front.
pub(crate) fn mark(content: &str, created: f64) -> String {
    match serde_json::from_str::<Value>(content) {
        Ok(Value::Object(mut object)) => {
            object.insert("cached".into(), json!(true));
            object.insert("cached_at".into(), json!(created));
            Value::Object(object).to_string()
        }
        _ => format!("{CACHED_NOTE_PREFIX}from {created}; pass refresh: true to read again]\n{content}"),
    }
}

/// The connector revision of an integration: access status, OAuth generation and agent
/// assignment for Gmail (the same value as the Host's `recovery_revision`), and the OAuth
/// generation with the stored integration record for the others.
pub(crate) fn connector_revision(db: &Database, agent: &str, integration: &str) -> io::Result<String> {
    if integration == "gmail" {
        let access = crate::native_auth::gmail_access(db, agent).map_err(|(_, error)| io::Error::other(error))?;
        return Ok(json!([access["status"], access["revision"], access["agents"]]).to_string());
    }
    let connection = db.lock().map_err(|_| io::Error::other("Database unavailable"))?;
    let generation = crate::ui_credentials::oauth_generation(&connection, integration).map_err(|(_, error)| io::Error::other(error))?;
    let record = crate::ui_store::get(&connection, "integration", integration).map_err(io::Error::other)?;
    Ok(json!([generation, record]).to_string())
}

/// One cacheable read.
pub(crate) struct Lookup {
    db: Database,
    scope: String,
    tool: String,
    revision: String,
    key: String,
    refresh: bool,
}

impl Lookup {
    pub(crate) fn new(db: &Database, scope: &str, tool: &str, arguments: &Value, revision: &str) -> Self {
        Self {
            db: db.clone(),
            scope: scope.to_owned(),
            tool: tool.to_owned(),
            revision: revision.to_owned(),
            key: key(scope, tool, arguments, revision),
            refresh: refresh_requested(arguments),
        }
    }

    /// The stored result, marked as cached, or `None` on a miss or with `refresh: true`.
    /// A cache failure is a miss.
    pub(crate) fn hit(&self) -> Option<String> {
        if self.refresh {
            return None;
        }
        let found = (|| -> rusqlite::Result<Option<(String, f64)>> {
            let connection = self.db.lock().map_err(|_| rusqlite::Error::InvalidQuery)?;
            connection
                .query_row(
                    "SELECT content,created FROM ui_read_cache WHERE key=? AND scope=? AND created>=?",
                    params![self.key, self.scope, crate::ui_store::now() - TTL_SECONDS],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
        })();
        match found {
            Ok(found) => found.map(|(content, created)| mark(&content, created)),
            Err(error) => {
                eprintln!("kageha: read cache lookup failed: {error}");
                None
            }
        }
    }

    /// Stores a successful result, replacing any earlier entry. Failures are logged; the
    /// call's result is unchanged.
    pub(crate) fn store(&self, content: &str) {
        if !storable(content) {
            return;
        }
        let stored = self.db.lock().map_err(|_| rusqlite::Error::InvalidQuery).and_then(|connection| {
            connection.execute(
                "INSERT INTO ui_read_cache(key,scope,tool,revision,content,created) VALUES (?,?,?,?,?,?)
                 ON CONFLICT(key) DO UPDATE SET content=excluded.content,created=excluded.created",
                params![self.key, self.scope, self.tool, self.revision, content, crate::ui_store::now()],
            )
        });
        if let Err(error) = stored {
            eprintln!("kageha: read cache store failed: {error}");
        }
    }

    /// Serves the read from the cache, or dispatches it with `read` and stores a successful
    /// result.
    pub(crate) fn read(&self, read: impl FnOnce() -> io::Result<String>) -> io::Result<String> {
        if let Some(hit) = self.hit() {
            return Ok(hit);
        }
        let result = read();
        if let Ok(content) = &result {
            self.store(content);
        }
        result
    }
}

/// What the Host does with the read cache for one call.
pub(crate) enum Plan {
    /// Not a cacheable tool: dispatched unchanged.
    Dispatch,
    /// A cacheable tool whose call must be read live (for example `read_file` inside the
    /// Task_Workspace): dispatched without `refresh`, never stored.
    Live,
    /// A cacheable read.
    Read(Lookup),
    /// A call that changes what earlier reads of `tools` returned. After dispatch the
    /// request's entries of those tools are deleted.
    Invalidate { db: Database, scope: String, tools: &'static [&'static str] },
}

impl Plan {
    /// The stored result for a [`Plan::Read`] hit.
    pub(crate) fn hit(&self) -> Option<String> {
        match self {
            Plan::Read(lookup) => lookup.hit(),
            _ => None,
        }
    }

    /// The call as dispatched to the tool: cacheable tools never see `refresh`.
    pub(crate) fn dispatch_call(&self, call: &Value) -> Value {
        match self {
            Plan::Dispatch => call.clone(),
            Plan::Live | Plan::Read(_) | Plan::Invalidate { .. } => {
                let mut call = call.clone();
                call["arguments"] = dispatch_arguments(&call["arguments"]);
                call
            }
        }
    }

    /// Stores a successful read, or deletes entries the call may have made stale.
    pub(crate) fn finish(&self, result: &io::Result<String>) {
        match self {
            Plan::Read(lookup) => {
                if let Ok(content) = result {
                    lookup.store(content);
                }
            }
            Plan::Invalidate { db, scope, tools } => {
                if let Err(error) = invalidate(db, scope, tools) {
                    eprintln!("kageha: read cache invalidation failed: {error}");
                }
            }
            Plan::Dispatch | Plan::Live => {}
        }
    }
}

/// Cacheable reads whose results a call to `tool` can change, within one request.
/// Browser actions open, close and change tabs, and `browser_tabs` with `tab_handle`
/// changes the selected tab, so all of them make an earlier tab listing stale.
pub(crate) fn invalidated_by(tool: &str, effect: crate::ui_tools::Effect, arguments: &Value) -> Option<&'static [&'static str]> {
    use crate::ui_tools::Effect;
    match tool {
        "memory_put" | "memory_delete" => Some(&["memory_get", "memory_search"]),
        "write_file" | "edit_file" | "shell" => Some(&["read_file"]),
        "browser_tabs" if !arguments["tab_handle"].is_null() => Some(&["browser_tabs"]),
        _ if tool.starts_with("browser_") && effect == Effect::Action => Some(&["browser_tabs"]),
        _ => None,
    }
}

/// Deletes the request's entries of `tools`.
pub(crate) fn invalidate(db: &Database, scope: &str, tools: &[&str]) -> io::Result<()> {
    let connection = db.lock().map_err(|_| io::Error::other("Database unavailable"))?;
    for tool in tools {
        connection
            .execute("DELETE FROM ui_read_cache WHERE scope=? AND tool=?", params![scope, tool])
            .map_err(io::Error::other)?;
    }
    Ok(())
}

/// Deletes every entry of a request when it ends. Returns the number deleted.
pub(crate) fn end_request(db: &Database, scope: &str) -> io::Result<usize> {
    let connection = db.lock().map_err(|_| io::Error::other("Database unavailable"))?;
    connection.execute("DELETE FROM ui_read_cache WHERE scope=?", [scope]).map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database() -> (tempfile::TempDir, Database) {
        let directory = tempfile::tempdir().unwrap();
        let connection = Connection::open(directory.path().join("native.sqlite")).unwrap();
        initialize(&connection).unwrap();
        (directory, crate::agent_host::DatabaseState::open(connection).unwrap())
    }

    fn entries(db: &Database) -> i64 {
        db.lock().unwrap().query_row("SELECT count(*) FROM ui_read_cache", [], |row| row.get(0)).unwrap()
    }

    #[test]
    fn keys_ignore_refresh_runtime_revision_and_key_order() {
        let base = key("r", "web_search", &json!({"query":"q","limit":3}), "");
        assert_eq!(base, key("r", "web_search", &json!({"limit":3,"query":"q","refresh":true,"_runtime_revision":"x"}), ""));
        assert_ne!(base, key("r", "web_search", &json!({"query":"other","limit":3}), ""));
        assert_ne!(base, key("other", "web_search", &json!({"query":"q","limit":3}), ""));
        assert_ne!(base, key("r", "news_search", &json!({"query":"q","limit":3}), ""));
        assert_ne!(base, key("r", "web_search", &json!({"query":"q","limit":3}), "2"));
    }

    #[test]
    fn a_hit_returns_the_original_content_with_the_cached_marker() {
        let (_directory, db) = database();
        let arguments = json!({"integration":"gmail","operation":"message","id":"m1"});
        let lookup = Lookup::new(&db, "request", "integration_read", &arguments, "rev-1");
        let original = json!({"status":"ok","data":{"id":"m1","subject":"Interview"}}).to_string();
        let mut dispatches = 0;
        assert_eq!(lookup.read(|| { dispatches += 1; Ok(original.clone()) }).unwrap(), original);
        let hit: Value = serde_json::from_str(&lookup.read(|| { dispatches += 1; Ok(original.clone()) }).unwrap()).unwrap();
        assert_eq!(dispatches, 1);
        assert_eq!(hit["cached"], true);
        assert!(hit["cached_at"].as_f64().unwrap() > 0.0);
        let mut unmarked = hit.clone();
        unmarked.as_object_mut().unwrap().retain(|key, _| key != "cached" && key != "cached_at");
        assert_eq!(unmarked, serde_json::from_str::<Value>(&original).unwrap());
        // Text results keep their bytes after a one-line note.
        let text = Lookup::new(&db, "request", "skill_read", &json!({"name":"s"}), "");
        text.store("# Skill\nbody");
        let hit = text.hit().unwrap();
        assert!(hit.starts_with(CACHED_NOTE_PREFIX) && hit.ends_with("\n# Skill\nbody"), "{hit}");
    }

    #[test]
    fn a_changed_revision_misses_and_refresh_dispatches_and_overwrites() {
        let (_directory, db) = database();
        let arguments = json!({"query":"seequent"});
        Lookup::new(&db, "request", "web_search", &arguments, "1").store(r#"{"results":["old"]}"#);
        assert!(Lookup::new(&db, "request", "web_search", &arguments, "2").hit().is_none());
        assert!(Lookup::new(&db, "other", "web_search", &arguments, "1").hit().is_none());
        let refresh = Lookup::new(&db, "request", "web_search", &json!({"query":"seequent","refresh":true}), "1");
        let mut dispatched = false;
        assert_eq!(refresh.read(|| { dispatched = true; Ok(r#"{"results":["new"]}"#.into()) }).unwrap(), r#"{"results":["new"]}"#);
        assert!(dispatched);
        let hit: Value = serde_json::from_str(&Lookup::new(&db, "request", "web_search", &arguments, "1").hit().unwrap()).unwrap();
        assert_eq!(hit["results"], json!(["new"]));
    }

    #[test]
    fn failures_and_blocked_connector_results_are_not_stored() {
        let (_directory, db) = database();
        let lookup = Lookup::new(&db, "request", "integration_read", &json!({"integration":"gmail","operation":"status"}), "");
        assert!(lookup.read(|| Err(io::Error::other("offline"))).is_err());
        for blocked in [
            json!({"status":"disconnected","connector_outcome":{"connector":"gmail","state":"disconnected","recovery":"manage_access"}}),
            json!({"ok":false,"error_code":"fetch_failed"}),
            json!({"status":"agent_not_assigned"}),
        ] {
            lookup.store(&blocked.to_string());
        }
        assert_eq!(entries(&db), 0);
        lookup.store(&json!({"status":"ok"}).to_string());
        assert_eq!(entries(&db), 1);
    }

    #[test]
    fn plans_strip_refresh_and_invalidate_or_end_the_request() {
        let (_directory, db) = database();
        let call = json!({"id":"c1","name":"memory_search","arguments":{"query":"q","refresh":true}});
        let read = Plan::Read(Lookup::new(&db, "request", "memory_search", &call["arguments"], ""));
        assert_eq!(read.dispatch_call(&call)["arguments"], json!({"query":"q"}));
        assert_eq!(Plan::Live.dispatch_call(&call)["arguments"], json!({"query":"q"}));
        assert_eq!(Plan::Dispatch.dispatch_call(&call), call);
        read.finish(&Ok("[\"note\"]".into()));
        Lookup::new(&db, "request", "web_search", &json!({"query":"q"}), "").store("{}");
        Lookup::new(&db, "other", "memory_search", &json!({"query":"q"}), "").store("{}");
        assert_eq!(entries(&db), 3);
        use crate::ui_tools::Effect;
        let tools = invalidated_by("memory_put", Effect::Action, &json!({})).unwrap();
        Plan::Invalidate { db: db.clone(), scope: "request".into(), tools }.finish(&Err(io::Error::other("failed")));
        assert_eq!(entries(&db), 2, "the request's memory reads are gone");
        assert!(invalidated_by("browser_click", Effect::Action, &json!({})).unwrap().contains(&"browser_tabs"));
        assert!(invalidated_by("browser_tabs", Effect::Read, &json!({"tab_handle":"t2"})).unwrap().contains(&"browser_tabs"));
        assert!(invalidated_by("browser_tabs", Effect::Read, &json!({})).is_none());
        assert!(invalidated_by("web_search", Effect::Read, &json!({})).is_none());
        assert_eq!(end_request(&db, "request").unwrap(), 1);
        assert_eq!(entries(&db), 1, "other requests keep their entries");
    }

    /// The Host's plans: a chat turn on a file-backed WebUI database with the request
    /// persistence policy, like the audited-session fixture.
    mod host {
        use super::*;
        use crate::native_chat::ToolHost;
        use crate::ui_tools::{Effect, ObservationCache, Tools, CAPABILITIES};
        use kageha_runtime::execution::ToolCall;
        use kageha_runtime::lifecycle::RunStore;
        use std::path::PathBuf;
        use std::sync::atomic::AtomicBool;

        const CHAT: &str = "readcache001";

        struct Fixture {
            _directory: tempfile::TempDir,
            db: Database,
            project: PathBuf,
            turn: String,
            controls: crate::webui::Controls,
            cancel: AtomicBool,
        }

        impl Fixture {
            fn new() -> Self {
                let directory = tempfile::tempdir().unwrap();
                let project = directory.path().join("project");
                std::fs::create_dir(&project).unwrap();
                std::fs::write(project.join("README.md"), "project notes").unwrap();
                let connection = Connection::open(directory.path().join("native.sqlite")).unwrap();
                connection.execute_batch("CREATE TABLE native_chat_turns(sequence INTEGER PRIMARY KEY,chat_id TEXT,request_id TEXT,message TEXT,answer TEXT,status TEXT);").unwrap();
                crate::webui::initialize(&connection).unwrap();
                connection.execute("INSERT INTO ui_sessions(id,title,created_at,updated_at) VALUES (?,?,0,0)", [CHAT, "Cache"]).unwrap();
                crate::ui_approvals::main_default(&connection, CHAT, "main").unwrap();
                let turn = crate::webui::turn_id(CHAT, "request-1");
                let mut configuration = json!({});
                crate::ui_persistence::admit(&connection, &turn, &mut configuration).unwrap();
                let spec = crate::runtime_host::spec(&project, CHAT, "request-1", &turn, "main", "read", 0, None, None,
                    vec![json!({"role":"user","content":"read"})], configuration, 64);
                crate::runtime_host::prepare(&connection, &spec).unwrap();
                let db = crate::agent_host::DatabaseState::open(connection).unwrap();
                crate::runtime_host::store(&db).unwrap().stage(&spec, 64).unwrap();
                Self { _directory: directory, db, project, turn, controls: Default::default(), cancel: AtomicBool::new(false) }
            }

            fn tools(&self) -> Tools<'_> {
                Tools {
                    db: &self.db, chat: CHAT, turn: &self.turn, workspace: &self.project, cancel: &self.cancel,
                    acknowledgment: None,
                    news_synthesis: AtomicBool::new(false), capability_discoveries: Default::default(),
                    observations: std::sync::Mutex::new(ObservationCache::default()),
                    unavailable_sources: Default::default(), read_blockers: Default::default(), controls: &self.controls,
                    persona: json!({"id":"main","name":"Kageha"}), knowledge_scope: Value::Null, providers: Vec::new(), role_providers: Default::default(),
                    depth: 0, remote: Value::Null, machine: None, media_preferences: Value::Null, browser_constraints: Value::Null,
                    workflow: Value::Null, resume_receipts: Vec::new(),
                    browser_decisions: crate::ui_browser_decisions::BrowserDecisions::new(&self.db, CHAT, &self.turn, &Value::Null),
                    spilled_outputs: Default::default(),
                    attachments: Default::default(),
                }
            }
        }

        fn call(name: &str, arguments: Value) -> ToolCall {
            ToolCall { id: "c1".into(), name: name.into(), arguments }
        }

        #[test]
        fn action_validation_and_workspace_tools_are_never_served_from_the_cache() {
            let fixture = Fixture::new();
            let tools = fixture.tools();
            let arguments = json!({"path":"README.md","query":"q","integration":"gmail","operation":"status","name":"s","id":"m1","url":"https://example.com"});
            for spec in CAPABILITIES {
                let plan = tools.read_cache(&call(spec.name, arguments.clone()));
                if spec.effect != Effect::Read || !spec.cacheable {
                    assert!(matches!(plan, Plan::Dispatch | Plan::Invalidate { .. }), "{} must not be cached", spec.name);
                } else {
                    assert!(matches!(plan, Plan::Read(_)), "{} should be a cacheable read", spec.name);
                }
            }
            assert!(matches!(tools.read_cache(&call("mcp_call", json!({"server":"s","tool":"t","arguments":{}}))), Plan::Dispatch));
        }

        #[test]
        fn read_file_inside_the_task_workspace_and_tab_selection_are_read_live() {
            let fixture = Fixture::new();
            let tools = fixture.tools();
            let task = crate::ui_workspace::open_for_chat(&fixture.db, CHAT).unwrap().root().to_path_buf();
            for path in ["workspace/outputs/a.md", ".kageha/tool-output/c1.txt", "../outside.md", task.join("outputs/a.md").to_str().unwrap()] {
                assert!(matches!(tools.read_cache(&call("read_file", json!({"path":path,"refresh":true}))), Plan::Live), "{path}");
            }
            for path in ["README.md", "skills/release-notes/SKILL.md", "inputs/cv.pdf"] {
                assert!(matches!(tools.read_cache(&call("read_file", json!({"path":path}))), Plan::Read(_)), "{path}");
            }
            // A user can change the selected profile or tabs outside this agent.
            assert!(matches!(tools.read_cache(&call("browser_tabs", json!({}))), Plan::Dispatch));
            assert!(matches!(tools.read_cache(&call("browser_tabs", json!({"tab_handle":"t2"}))), Plan::Invalidate { .. }));
        }

        #[test]
        fn a_changed_connector_revision_misses() {
            let fixture = Fixture::new();
            let tools = fixture.tools();
            let read = call("integration_read", json!({"integration":"gmail","operation":"message","id":"m1"}));
            let Plan::Read(lookup) = tools.read_cache(&read) else { panic!("integration_read is cacheable") };
            lookup.store(r#"{"status":"ok","data":{"id":"m1"}}"#);
            assert!(tools.read_cache(&read).hit().is_some());
            // Assigning another agent to Gmail changes the connector revision.
            crate::ui_store::put(&fixture.db.lock().unwrap(), "integration", "gmail", json!({"id":"gmail","agents":["main","research"]})).unwrap();
            assert!(tools.read_cache(&read).hit().is_none());
        }
    }

    #[test]
    fn startup_drops_entries_older_than_the_ttl() {
        let (_directory, db) = database();
        Lookup::new(&db, "request", "web_search", &json!({"query":"q"}), "").store("{}");
        Lookup::new(&db, "request", "web_search", &json!({"query":"old"}), "").store("{}");
        let connection = db.lock().unwrap();
        connection.execute("UPDATE ui_read_cache SET created=created-? WHERE content='{}' AND rowid=(SELECT min(rowid) FROM ui_read_cache)", [TTL_SECONDS + 60.0]).unwrap();
        initialize(&connection).unwrap();
        let left: i64 = connection.query_row("SELECT count(*) FROM ui_read_cache", [], |row| row.get(0)).unwrap();
        assert_eq!(left, 1);
    }
}
