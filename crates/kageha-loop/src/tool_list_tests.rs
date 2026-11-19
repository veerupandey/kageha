//! The fixed tool list (spec autonomous-agent-loop, task 12.2; design section 4;
//! Requirements 1.1 and 11.4): MCP tools are reached through `mcp_call`, skills through
//! `skill_read`, and discovery only reports how to call a capability.
//!
//! The MCP server is this test binary itself, started over stdio as the ignored
//! `mcp_fixture_server` test, so `mcp_call` goes through the real `ui_mcp` pool,
//! approval and dispatch path.
use crate::native_chat::ToolHost;
use crate::ui_tools::{Effect, ObservationCache, Tools};
use crate::webui::{self, Controls, Database};
use kageha_runtime::execution::ToolCall;
use kageha_runtime::lifecycle::RunStore;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

const CHAT: &str = "tool-list-chat";
const REQUEST: &str = "request-1";
const PROMPT: &str = "Look up the release notes";

/// A chat turn on a file-backed WebUI database with an MCP server and a skill.
struct Fixture {
    _directory: tempfile::TempDir,
    db: Database,
    project: PathBuf,
    turn: String,
    controls: Controls,
    cancel: AtomicBool,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let project = directory.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let connection = rusqlite::Connection::open(directory.path().join("native.sqlite")).unwrap();
        connection.execute_batch("CREATE TABLE native_chat_turns(sequence INTEGER PRIMARY KEY,chat_id TEXT,request_id TEXT,message TEXT,answer TEXT,status TEXT);").unwrap();
        webui::initialize(&connection).unwrap();
        connection.execute("INSERT INTO ui_sessions(id,title,created_at,updated_at) VALUES (?,?,0,0)", [CHAT, "Tool list"]).unwrap();
        crate::ui_approvals::main_default(&connection, CHAT, "main").unwrap();
        let turn = webui::turn_id(CHAT, REQUEST);
        let mut configuration = json!({});
        crate::ui_persistence::admit(&connection, &turn, &mut configuration).unwrap();
        let spec = crate::runtime_host::spec(&project, CHAT, REQUEST, &turn, "main", PROMPT, 0, None, None,
            vec![json!({"role":"user","content":PROMPT})], configuration, 64);
        crate::runtime_host::prepare(&connection, &spec).unwrap();
        // The MCP server: this binary, serving `mcp_fixture_server`. `lookup` is tagged read-only.
        crate::ui_store::put(&connection, "config:mcp:servers", "fixture", json!({
            "id":"fixture","name":"fixture","enabled":true,"transport":"stdio",
            "command":std::env::current_exe().unwrap(),
            "args":["--ignored","--exact","tool_list_tests::mcp_fixture_server","--nocapture"],
            // With one test thread libtest prints the test name before the test runs,
            // on the line of the first response.
            "env":{"RUST_TEST_THREADS":"2"},
            "read_only_tools":["lookup"],
            "tools":fixture_tools(),
        })).unwrap();
        let db = crate::agent_host::DatabaseState::open(connection).unwrap();
        crate::runtime_host::store(&db).unwrap().stage(&spec, 64).unwrap();
        // A skill with one resource file, installed in the registry `ui_skills` reads.
        let mut registry = kageha_skills::registry::Registry::open(
            &directory.path().join("components/skills/registry.sqlite")).unwrap();
        let package = kageha_skills::Package::from_files("release-notes", BTreeMap::from([
            ("SKILL.md".to_owned(), b"---\nname: release-notes\ndescription: Summarize release notes\n---\nList breaking changes first.".to_vec()),
            ("references/format.md".to_owned(), b"Use one bullet per change.".to_vec()),
        ])).unwrap();
        registry.install(&package, "operator", Some(0)).unwrap();
        drop(registry);
        Self { _directory: directory, db, project, turn, controls: Controls::default(), cancel: AtomicBool::new(false) }
    }
    fn tools(&self) -> Tools<'_> {
        Tools {
            db: &self.db,
            chat: CHAT,
            turn: &self.turn,
            workspace: &self.project,
            cancel: &self.cancel,
            acknowledgment: None,
            news_synthesis: AtomicBool::new(false),
            capability_discoveries: Default::default(),
            observations: std::sync::Mutex::new(ObservationCache::default()),
            unavailable_sources: Default::default(),
            read_blockers: Default::default(),
            controls: &self.controls,
            persona: json!({"id":"main","name":"Kageha","tool_policy":{}}),
            knowledge_scope: Value::Null,
            providers: Vec::new(),
            role_providers: Default::default(),
            depth: 0,
            remote: Value::Null,
            machine: None,
            media_preferences: Value::Null,
            browser_constraints: Value::Null,
            workflow: Value::Null,
            resume_receipts: Vec::new(),
            browser_decisions: crate::ui_browser_decisions::BrowserDecisions::new(&self.db, CHAT, &self.turn, &Value::Null),
            spilled_outputs: Default::default(),
            attachments: Default::default(),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // Stop the fixture server process before the directory is removed.
        let connection = self.db.lock().unwrap();
        let _ = crate::ui_mcp::release_session(&connection, CHAT);
    }
}

#[test]
fn past_attachment_uses_chat_permissions_and_returns_a_readable_input() {
    use kageha_contracts::policy::PermissionProfile;
    use sha2::{Digest, Sha256};

    let fixture = Fixture::new();
    let source = "prior-chat";
    let filename = "resume.txt";
    let content = b"Test resume from earlier chat";
    let version = format!("{:x}", Sha256::digest(content));
    let policy = {
        let db = fixture.db.lock().unwrap();
        db.execute("INSERT INTO native_chat_turns(chat_id,request_id,message,status) VALUES (?,?,'Bring my resume from an earlier chat','running')", [CHAT, REQUEST]).unwrap();
        db.execute("INSERT INTO ui_sessions VALUES (?,'Prior chat',0,0)", [source]).unwrap();
        db.execute("INSERT INTO ui_uploads VALUES (?,'inputs/resume.txt',?,'text/plain',?,?)",
            rusqlite::params![source, filename, version, content.len()]).unwrap();
        crate::ui_approvals::owned(&db).unwrap()
    };
    let inputs = crate::ui_uploads::root(&fixture.db, source).unwrap().join("inputs");
    std::fs::create_dir_all(&inputs).unwrap();
    std::fs::write(inputs.join(filename), content).unwrap();
    let found: Value = serde_json::from_str(&fixture.tools().invoke(&json!({
        "id":"search","name":"find_past_files","arguments":{"query":"resume"}
    })).unwrap()).unwrap();
    let file = &found["files"][0];
    assert_eq!(file["version"], version);
    let call = json!({"id":"attach","name":"attach_past_file","arguments":{"filename":file["filename"],"version":file["version"]}});
    policy.set_profile(CHAT, PermissionProfile::Ask).unwrap();
    std::thread::scope(|scope| {
        let pending = scope.spawn(|| fixture.tools().invoke(&call));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let request = { let db = fixture.db.lock().unwrap(); crate::ui_approvals::pending(&db, CHAT).unwrap() };
            if let Some(request) = request {
                assert_eq!(request["action"], "attach_past_file");
                crate::ui_approvals::decide(&fixture.db, &json!({"approval_id":request["approval_id"], "approved":false})).unwrap();
                break;
            }
            if std::time::Instant::now() >= deadline {
                fixture.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                panic!("past attachment did not request approval");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(pending.join().unwrap().is_err());
    });
    assert_eq!(fixture.db.lock().unwrap().query_row("SELECT count(*) FROM ui_uploads WHERE chat_id=?", [CHAT], |row| row.get::<_, i64>(0)).unwrap(), 0);
    policy.set_profile(CHAT, PermissionProfile::Full).unwrap();
    let mut approved_call = call;
    approved_call["id"] = json!("allowed-attach");
    let saved: Value = serde_json::from_str(&fixture.tools().invoke(&approved_call).unwrap()).unwrap();
    let path = format!("workspace/{}", saved["workspace_path"].as_str().unwrap());
    let read = fixture.tools().invoke(&json!({"id":"read","name":"read_file","arguments":{"path":path}})).unwrap();
    assert!(read.contains("Test resume from earlier chat"), "{read}");
}

fn fixture_tools() -> Value {
    json!([
        {"name":"echo","description":"Echo text back","inputSchema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}},
        {"name":"lookup","description":"Look up a release by version","inputSchema":{"type":"object","properties":{"version":{"type":"string"}}}},
        {"name":"search_mail","description":"Search connected email messages by query","inputSchema":{"type":"object","properties":{"query":{"type":"string"}},"required":["query"]}},
    ])
}

/// A stdio MCP server for the tests above; started by them, never run directly.
#[test]
#[ignore = "launched as an isolated stdio MCP server by the tool list tests"]
fn mcp_fixture_server() {
    for line in io::stdin().lock().lines() {
        let request: Value = serde_json::from_str(&line.unwrap()).unwrap();
        if request.get("id").is_none() { continue; }
        let result = match request["method"].as_str().unwrap_or("") {
            "initialize" => json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}),
            "tools/list" => json!({"tools":fixture_tools()}),
            "tools/call" => {
                let params = &request["params"];
                let text = format!("{}:{}", params["name"].as_str().unwrap_or(""), params["arguments"]);
                json!({"content":[{"type":"text","text":text}]})
            }
            _ => json!({}),
        };
        println!("{}", json!({"jsonrpc":"2.0","id":request["id"],"result":result}));
        io::stdout().flush().unwrap();
    }
}

fn names(specs: &[Value]) -> Vec<&str> {
    specs.iter().filter_map(|spec| spec["function"]["name"].as_str()).collect()
}
fn mcp_call(id: &str, server: &str, tool: &str, arguments: Value) -> Value {
    json!({"id":id,"name":"mcp_call","arguments":{"server":server,"tool":tool,"arguments":arguments}})
}
fn error_code(error: &io::Error) -> String {
    serde_json::from_str::<Value>(&error.to_string()).map(|value| value["error_code"].as_str().unwrap_or("").to_owned()).unwrap_or_default()
}

#[test]
fn agent_discovers_and_links_an_existing_mcp_app_under_policy() {
    let fixture=Fixture::new();
    {
        let db=fixture.db.lock().unwrap();
        crate::ui_store::put(&db,"plugin-installed","fixture-app",json!({"name":"fixture-app","status":"installed","package_sha256":"package-hash","mcp":[],"skills":[],"apps":[{"name":"app","connector_id":"opaque-host-app","integration":null}]})).unwrap();
        let mut server=crate::ui_store::get(&db,"config:mcp:servers","fixture").unwrap().unwrap();
        server["connected"]=json!(true);
        crate::ui_store::put(&db,"config:mcp:servers","fixture",server).unwrap();
    }
    let tools=fixture.tools();
    assert!(names(&tools.specs()).contains(&"plugin_app_bridge"));
    let discover=json!({"id":"discover-bridge","name":"plugin_app_bridge","arguments":{"plugin":"fixture-app","app":"app","action":"discover"}});
    let result:Value=serde_json::from_str(&tools.invoke(&discover).unwrap()).unwrap();
    assert_eq!(result["bridge"]["status"],"needs_setup");
    assert_eq!(result["candidates"][0]["server"],"fixture");
    assert_eq!(result["candidates"][0]["source"],"independent");
    assert!(result["note"].as_str().unwrap().contains("not proof of compatibility"));
    let link=json!({"id":"link-bridge","name":"plugin_app_bridge","arguments":{"plugin":"fixture-app","app":"app","action":"link","server":"fixture","tools":["lookup"],"reason":"The server can look up release data for this app task"}});
    let mut denied=fixture.tools();denied.persona=json!({"id":"main","tool_policy":{"deny":["plugin_app_bridge"]}});
    assert!(denied.invoke(&link).is_err());
    let mut denied_mcp=fixture.tools();denied_mcp.persona=json!({"id":"main","tool_policy":{"deny":["mcp_call"]}});
    assert!(denied_mcp.invoke(&link).is_err());
    let mut denied_tool=fixture.tools();
    denied_tool.persona=json!({"id":"main","tool_policy":{"deny":[kageha_mcp::tool_id("fixture","lookup")]}});
    let limited:Value=serde_json::from_str(&denied_tool.invoke(&discover).unwrap()).unwrap();
    assert!(!limited["candidates"][0]["tools"].as_array().unwrap().iter().any(|tool|tool["name"]=="lookup"));
    assert!(denied_tool.invoke(&link).is_err());
    let mut invalid=link.clone();invalid["arguments"]["tools"]=json!(["not-a-tool"]);
    assert!(tools.invoke(&invalid).is_err());
    let linked:Value=serde_json::from_str(&tools.invoke(&link).unwrap()).unwrap();
    assert_eq!(linked["status"],"configured");
    assert_eq!(linked["tools"],json!(["lookup"]));
    assert!(tools.invoke(&link).is_err(), "agent must never overwrite an existing mapping");
    let found:Value=serde_json::from_str(&tools.invoke(&json!({"id":"linked-find","name":"discover_capabilities","arguments":{"query":"fixture-app","kind":"plugin"}})).unwrap()).unwrap();
    let app=&found["results"][0]["apps"][0];
    assert_eq!(app["status"],"ready");
    assert_eq!(app["tools"],json!(["lookup"]));
    let read=tools.invoke(&mcp_call("linked-read","fixture","lookup",json!({"version":"1"}))).unwrap();
    assert!(read.contains("lookup"), "{read}");
}

#[test]
fn plugin_connection_checks_authorization_policy_and_durable_receipt() {
    let fixture=Fixture::new();
    let (plugin_rev,server_rev)={
        let db=fixture.db.lock().unwrap();
        let plugin=crate::ui_store::put(&db,"plugin-installed","bundle",json!({"name":"bundle","status":"installed","mcp":[{"name":"plugin-server"}]})).unwrap();
        let oauth=json!({"client_id":"fixture-client","callback_url":"http://127.0.0.1:12799/callback"});
        crate::ui_credentials::oauth_store(&db,"mcp-plugin-server",&json!({"access_token":"private-canary","expires_in":3600}),true,0).unwrap();
        let authority=crate::ui_credentials::oauth_authority(&db,"mcp-plugin-server").unwrap().unwrap();
        let config=crate::ui_store::put(&db,"config:mcp:servers","plugin-server",json!({"name":"plugin-server","origin":"plugin","plugin":"bundle","transport":"http","url":"https://api.githubcopilot.com/mcp","enabled":false,"connected":false,"headers":{"private":"secret-canary"},"oauth":oauth,"oauth_binding":{"resource":"https://api.githubcopilot.com/mcp","client_id":"fixture-client","issuer":"https://github.com/login/oauth","token_endpoint":"https://github.com/login/oauth/access_token","oauth":oauth,"authorization_id":authority}})).unwrap();
        (plugin["revision"].as_u64().unwrap(),config["revision"].as_u64().unwrap())
    };
    let tools=fixture.tools();
    assert!(names(&tools.specs()).contains(&"plugin_connection"));
    let call=json!({"id":"status","name":"plugin_connection","arguments":{"plugin":"bundle","server":"plugin-server"}});
    let status=tools.invoke(&call).unwrap();
    let value:Value=serde_json::from_str(&status).unwrap();
    assert_eq!(value["authorization"],"ready");
    assert!(!status.contains("private-canary") && !status.contains("secret-canary") && !status.contains("https://"));
    let mut denied=fixture.tools(); denied.persona=json!({"id":"main","tool_policy":{"deny":["plugin_connection"]}});
    assert!(denied.invoke(&call).is_err());
    let activate=json!({"id":"activate","name":"plugin_connection","arguments":{"plugin":"bundle","server":"plugin-server","action":"activate"}});
    {
        let db=fixture.db.lock().unwrap();
        crate::ui_credentials::pending_put(&db,"mcp-plugin-server",json!({"expires":crate::ui_store::now()+300.0})).unwrap();
    }
    let pending:Value=serde_json::from_str(&tools.invoke(&call).unwrap()).unwrap();
    assert_eq!(pending["authorization"],"needs_authorization");
    assert!(tools.invoke(&activate).is_err());
    assert!(crate::ui_tools::reserve_plugin_activation(&fixture.db,"bundle","plugin-server",plugin_rev,server_rev,"a").is_err());
    {let db=fixture.db.lock().unwrap();crate::ui_credentials::pending_delete(&db,"mcp-plugin-server").unwrap();}
    assert!(crate::ui_tools::reserve_plugin_activation(&fixture.db,"bundle","plugin-server",plugin_rev,server_rev+1,"a").is_err());
    crate::ui_tools::reserve_plugin_activation(&fixture.db,"bundle","plugin-server",plugin_rev,server_rev,"a").unwrap();
    let uncertain:Value=serde_json::from_str(&tools.invoke(&call).unwrap()).unwrap();
    assert_eq!(uncertain["activation"],"uncertain");
    assert!(uncertain["next_step"].as_str().unwrap().contains("manual"));
    assert!(tools.invoke(&activate).is_err());
    assert!(crate::ui_tools::reserve_plugin_activation(&fixture.db,"bundle","plugin-server",plugin_rev,server_rev,"b").is_err());
    assert!(crate::ui_tools::complete_plugin_activation(&fixture.db,"bundle","plugin-server","b").is_err());
    crate::ui_tools::complete_plugin_activation(&fixture.db,"bundle","plugin-server","a").unwrap();
    let cleared:Value=serde_json::from_str(&tools.invoke(&call).unwrap()).unwrap();
    assert_eq!(cleared["activation"],"none");
    let wrong=json!({"id":"wrong","name":"plugin_connection","arguments":{"plugin":"other","server":"plugin-server"}});
    assert!(tools.invoke(&wrong).is_err());
}

#[test]
fn discovery_returns_how_to_call_and_leaves_the_tool_list_unchanged() {
    let fixture = Fixture::new();
    let fresh = fixture.tools().specs();
    let tools = fixture.tools();
    let before = tools.specs();
    assert_eq!(before, fresh);
    let listed = names(&before);
    for name in ["mcp_call", "mcp_receipts", "skill_read", "discover_capabilities"] {
        assert!(listed.contains(&name), "{name} must be listed: {listed:?}");
    }
    // Individual MCP tools and the old skill tool name are never listed.
    assert!(!listed.iter().any(|name| name.starts_with("mcp_") && !matches!(*name, "mcp_call" | "mcp_receipts")), "{listed:?}");
    assert!(!listed.contains(&"load_skill"));
    assert!(before.iter().all(|spec| spec.get("discovery_alias").is_none() && spec.get("mcp_server").is_none()));

    let found: Value = serde_json::from_str(&tools.invoke(&json!({"id":"d1","name":"discover_capabilities","arguments":{"query":"fixture::echo"}})).unwrap()).unwrap();
    let echo = &found["results"][0];
    assert_eq!((&echo["name"], &echo["server"], &echo["tool"]), (&json!("fixture::echo"), &json!("fixture"), &json!("echo")), "{found}");
    assert_eq!(echo["parameters"]["required"], json!(["text"]));
    assert!(echo["call"].as_str().unwrap().contains("mcp_call"), "{echo}");
    let skill: Value = serde_json::from_str(&tools.invoke(&json!({"id":"d2","name":"discover_capabilities","arguments":{"query":"release-notes","kind":"skill"}})).unwrap()).unwrap();
    assert!(skill["results"][0]["call"].as_str().unwrap().contains("skill_read"), "{skill}");

    assert_eq!(tools.specs(), before, "discovery must not change the tool list");
}

#[test]
fn discovery_finds_a_connected_mail_operation_and_its_call_schema() {
    let fixture = Fixture::new();
    let tools = fixture.tools();
    let found: Value = serde_json::from_str(&tools.invoke(&json!({"id":"d-mail","name":"discover_capabilities","arguments":{"query":"search email messages","kind":"tool"}})).unwrap()).unwrap();
    let mail = found["results"].as_array().unwrap().iter()
        .find(|result| result["server"] == "fixture" && result["tool"] == "search_mail")
        .expect("a matching connected mail tool should be discoverable");
    assert_eq!(mail["parameters"]["required"], json!(["query"]));
    assert!(mail["call"].as_str().unwrap().contains("mcp_call"));
}

#[test]
fn installed_plugin_discovery_respects_skill_scope_without_a_network_catalog() {
    let fixture = Fixture::new();
    {
        let db = fixture.db.lock().unwrap();
        crate::ui_store::put(&db, "plugin-installed", "fixture-plugin", json!({
            "name":"fixture-plugin", "status":"installed",
            "skills":[{"name":"release-notes","revision":1,"sha256":"fixture"}],
            "mcp":[{"name":"fixture-plugin-mcp","enabled":false}]
        })).unwrap();
    }
    let tools = fixture.tools();
    let found: Value = serde_json::from_str(&tools.invoke(&json!({"id":"plugin-local","name":"discover_capabilities",
        "arguments":{"query":"fixture-plugin"}})).unwrap()).unwrap();
    let result = found["results"].as_array().unwrap().iter().find(|item| item["id"] == "plugin:fixture-plugin").unwrap();
    assert_eq!(result["status"], "installed");
    assert_eq!(result["runnable"], true);
    assert_eq!(result["skills"], json!(["release-notes"]));
    assert!(result["next_step"].as_str().unwrap().contains("Disabled MCP"));
    let mut denied = fixture.tools();
    denied.persona = json!({"id":"main","tool_policy":{"deny":["skill_read", "load_skill"]}});
    let absent: Value = serde_json::from_str(&denied.invoke(&json!({"id":"plugin-denied","name":"discover_capabilities",
        "arguments":{"query":"fixture-plugin"}})).unwrap()).unwrap();
    assert!(!absent["results"].as_array().unwrap().iter().any(|item| item["id"] == "plugin:fixture-plugin"));
}

#[test]
fn installed_app_plugin_discovery_distinguishes_missing_access() {
    let fixture = Fixture::new();
    {
        let db = fixture.db.lock().unwrap();
        crate::ui_store::put(&db, "plugin-installed", "calendar-bundle", json!({
            "name":"calendar-bundle", "status":"installed", "skills":[], "mcp":[],
            "apps":[{"name":"calendar","integration":"google-calendar","capabilities":["read"]}]
        })).unwrap();
    }
    let tools = fixture.tools();
    let found: Value = serde_json::from_str(&tools.invoke(&json!({"id":"app-installed","name":"discover_capabilities",
        "arguments":{"query":"calendar-bundle"}})).unwrap()).unwrap();
    let plugin = found["results"].as_array().unwrap().iter().find(|item| item["id"] == "plugin:calendar-bundle").unwrap();
    assert_eq!(plugin["status"], "installed");
    assert_eq!(plugin["runnable"], false);
    assert_eq!(plugin["apps"][0]["status"], "needs_setup");
    assert!(plugin["next_step"].as_str().unwrap().contains("Settings"));
    let mut denied = fixture.tools();
    denied.persona = json!({"id":"main","tool_policy":{"deny":["integration_read"]}});
    if let Ok(value) = denied.invoke(&json!({"id":"app-denied","name":"discover_capabilities","arguments":{"query":"calendar-bundle"}})) {
        let result: Value = serde_json::from_str(&value).unwrap();
        assert!(!result["results"].as_array().unwrap().iter().any(|item| item["id"] == "plugin:calendar-bundle"));
    }
}

#[test]
fn bridged_app_discovery_only_reports_selected_eligible_tools() {
    let fixture=Fixture::new();
    let revision={
        let db=fixture.db.lock().unwrap();
        crate::ui_store::put(&db,"plugin-installed","bridge-test",json!({"name":"bridge-test","status":"installed","package_sha256":"approved",
            "apps":[{"name":"mail-app","connector_id":"connector-test","integration":null}],"skills":[],"mcp":[]})).unwrap();
        let mut server=crate::ui_store::get(&db,"config:mcp:servers","fixture").unwrap().unwrap();
        server["connected"]=json!(true);
        crate::ui_store::put(&db,"config:mcp:servers","fixture",server).unwrap()["revision"].as_u64().unwrap()
    };
    crate::ui_plugin_apps::set_bridge(&fixture.db,&json!({"plugin":"bridge-test","app":"mail-app","connector_id":"connector-test",
        "package_sha256":"approved","server":"fixture","expected_server_revision":revision,"tools":["lookup"]})).unwrap();
    let tools=fixture.tools();
    let found:Value=serde_json::from_str(&tools.invoke(&json!({"id":"bridge-search","name":"discover_capabilities","arguments":{"query":"bridge-test"}})).unwrap()).unwrap();
    let app=&found["results"].as_array().unwrap().iter().find(|item|item["id"]=="plugin:bridge-test").unwrap()["apps"][0];
    assert_eq!(app["status"],"ready"); assert_eq!(app["tools"],json!(["lookup"])); assert_eq!(app["server"],"fixture");
    let mut denied=fixture.tools();
    denied.persona=json!({"id":"main","tool_policy":{"deny":[kageha_mcp::tool_id("fixture","lookup")]}});
    let found:Value=serde_json::from_str(&denied.invoke(&json!({"id":"bridge-denied","name":"discover_capabilities","arguments":{"query":"bridge-test"}})).unwrap()).unwrap();
    let app=&found["results"].as_array().unwrap().iter().find(|item|item["id"]=="plugin:bridge-test").unwrap()["apps"][0];
    assert_eq!(app["status"],"needs_setup"); assert_eq!(app["tools"],json!([]));
    { let db=fixture.db.lock().unwrap(); let mut server=crate::ui_store::get(&db,"config:mcp:servers","fixture").unwrap().unwrap();
      server["enabled"]=json!(false);crate::ui_store::put(&db,"config:mcp:servers","fixture",server).unwrap(); }
    let found:Value=serde_json::from_str(&tools.invoke(&json!({"id":"bridge-stale","name":"discover_capabilities","arguments":{"query":"bridge-test"}})).unwrap()).unwrap();
    let app=&found["results"].as_array().unwrap().iter().find(|item|item["id"]=="plugin:bridge-test").unwrap()["apps"][0];
    assert_eq!(app["status"],"needs_setup"); assert_eq!(app["tools"],json!([]));
}

#[test]
fn installed_mcp_only_plugin_discovery_obeys_server_and_tool_access() {
    let fixture = Fixture::new();
    {
        let db = fixture.db.lock().unwrap();
        crate::ui_store::put(&db, "plugin-installed", "integration-bundle", json!({
            "name":"integration-bundle", "status":"installed", "skills":[],
            "mcp":[{"name":"fixture","enabled":false}]
        })).unwrap();
    }
    let mut tools = fixture.tools();
    tools.persona = json!({"id":"main", "tool_policy":{"deny":["skill_read", "load_skill"]}});
    let found: Value = serde_json::from_str(&tools.invoke(&json!({"id":"mcp-installed","name":"discover_capabilities",
        "arguments":{"query":"integration-bundle"}})).unwrap()).unwrap();
    let plugin = found["results"].as_array().unwrap().iter().find(|item| item["id"] == "plugin:integration-bundle").unwrap();
    assert_eq!(plugin["status"], "installed");
    assert_eq!(plugin["mcp"], json!(["fixture"]));
    assert_eq!(plugin["runnable"], false, "configuration does not prove live connectivity");
    assert_eq!(plugin["mcp_status"], "configured");

    {
        let db = fixture.db.lock().unwrap();
        let mut server = crate::ui_store::get(&db, "config:mcp:servers", "fixture").unwrap().unwrap();
        server["enabled"] = json!(false);
        crate::ui_store::put(&db, "config:mcp:servers", "fixture", server).unwrap();
    }
    let after = tools.invoke(&json!({"id":"mcp-disabled","name":"discover_capabilities",
        "arguments":{"query":"integration-bundle"}}));
    if let Ok(value) = after {
        let result: Value = serde_json::from_str(&value).unwrap();
        assert!(!result["results"].as_array().unwrap().iter().any(|item| item["id"] == "plugin:integration-bundle"));
    }
}

#[test]
fn plugin_status_is_scoped_and_unsupported_preview_is_not_installed() {
    let fixture = Fixture::new();
    {
        let db = fixture.db.lock().unwrap();
        let summary = json!({"preview_id":"fixture-preview", "name":"unsupported", "installable":false,
            "skills":[], "mcp":[], "unsupported":["Host app connectors are not supported"]});
        crate::ui_store::put(&db, "plugin-preview", "fixture-preview", json!({
            "owner":CHAT, "summary":summary, "packages":[], "configs":[], "status":"preview"
        })).unwrap();
        crate::ui_store::put(&db, "plugin-preview", "other-preview", json!({
            "owner":"other-chat", "summary":{"preview_id":"other-preview"}, "status":"preview"
        })).unwrap();
    }
    let tools = fixture.tools();
    let status: Value = serde_json::from_str(&tools.invoke(&json!({"id":"status","name":"plugin_status",
        "arguments":{"preview_id":"fixture-preview"}})).unwrap()).unwrap();
    assert_eq!(status["preview"]["installable"], false);
    assert!(status["installation"].is_null());
    assert!(tools.invoke(&json!({"id":"install","name":"plugin_install",
        "arguments":{"preview_id":"fixture-preview"}})).unwrap_err().to_string().contains("no supported components"));
    assert!(tools.invoke(&json!({"id":"other","name":"plugin_status",
        "arguments":{"preview_id":"other-preview"}})).is_err());
    let db = fixture.db.lock().unwrap();
    assert!(crate::ui_store::get(&db, "plugin-installed", "unsupported").unwrap().is_none());
}

#[test]
fn agent_installs_approved_preview_and_reads_installed_skill() {
    let fixture = Fixture::new();
    let files = BTreeMap::from([("SKILL.md".to_owned(), b"---\nname: plugin-example\ndescription: Summarize example documents\n---\nList the key facts.\n".to_vec())]);
    let package = kageha_skills::Package::from_files("plugin-example", files.clone()).unwrap();
    let encoded: BTreeMap<_, _> = files.iter().map(|(name, bytes)| (name.clone(), data_encoding::BASE64.encode(bytes))).collect();
    let summary = json!({"preview_id":"agent-preview", "name":"example-plugin", "source":"openai/plugins",
        "commit":"0123456789012345678901234567890123456789", "package_sha256":"fixture-package",
        "skills":[{"name":"plugin-example","sha256":package.sha256}], "mcp":[],
        "unsupported":["Host app connectors are not supported"], "installable":true});
    {
        let db = fixture.db.lock().unwrap();
        crate::ui_store::put(&db, "plugin-preview", "agent-preview", json!({"owner":CHAT,
            "summary":summary, "packages":[{"name":"plugin-example","sha256":package.sha256,"files":encoded}],
            "configs":[], "status":"preview"})).unwrap();
    }
    let call = json!({"id":"install-plugin","name":"plugin_install","arguments":{"preview_id":"agent-preview"}});
    let mut denied = fixture.tools();
    denied.persona = json!({"id":"main","tool_policy":{"deny":["plugin_install"]}});
    assert!(denied.invoke(&call).is_err());
    {
        let db = fixture.db.lock().unwrap();
        assert!(crate::ui_store::get(&db, "plugin-installed", "example-plugin").unwrap().is_none());
    }
    let tools = fixture.tools();
    let installed: Value = serde_json::from_str(&tools.invoke(&call).unwrap()).unwrap();
    assert_eq!(installed["plugin"]["status"], "installed");
    assert_eq!(installed["plugin"]["unsupported"][0], "Host app connectors are not supported");
    let found: Value = serde_json::from_str(&tools.invoke(&json!({"id":"find-plugin","name":"discover_capabilities",
        "arguments":{"query":"example-plugin"}})).unwrap()).unwrap();
    let plugin = found["results"].as_array().unwrap().iter().find(|item| item["id"] == "plugin:example-plugin").unwrap();
    assert_eq!(plugin["status"], "installed");
    assert_eq!(plugin["skills"], json!(["plugin-example"]));
    let skill = tools.invoke(&json!({"id":"read-plugin-skill","name":"skill_read",
        "arguments":{"name":"plugin-example"}})).unwrap();
    assert!(skill.contains("List the key facts."));
    let status: Value = serde_json::from_str(&tools.invoke(&json!({"id":"plugin-status","name":"plugin_status",
        "arguments":{"preview_id":"agent-preview"}})).unwrap()).unwrap();
    assert_eq!(status["installation"]["status"], "installed");
}

#[test]
fn discovery_identifies_integration_availability_and_allowed_mcp_servers() {
    let fixture = Fixture::new();
    let tools = fixture.tools();
    let find = |query: &str| -> Value { serde_json::from_str(&tools.invoke(&json!({
        "id":"i", "name":"discover_capabilities", "arguments":{"query":query,"kind":"integration"}
    })).unwrap()).unwrap() };
    let github = find("github");
    assert_eq!(github["results"][0]["status"], "unavailable");
    assert!(github["results"][0]["call"].is_null());
    assert!(github["results"][0]["next_step"].as_str().unwrap().contains("Settings"));
    let mcp = find("fixture");
    assert_eq!(mcp["results"][0]["status"], "configured");
    assert_eq!(mcp["results"][0]["provider"], "mcp");
    assert!(mcp["results"][0]["tools"].as_array().unwrap().iter().any(|v| v == "search_mail"));

    let mut denied = fixture.tools();
    denied.persona = json!({"id":"main","tool_policy":{"deny":["integration_read", kageha_mcp::tool_id("fixture", "search_mail"), kageha_mcp::tool_id("fixture", "lookup"), kageha_mcp::tool_id("fixture", "echo")]}});
    let result = denied.invoke(&json!({"id":"d","name":"discover_capabilities","arguments":{"query":"github","kind":"integration"}})).unwrap_err();
    assert_eq!(error_code(&result), "capability_not_found");
    let result = denied.invoke(&json!({"id":"d2","name":"discover_capabilities","arguments":{"query":"fixture","kind":"integration"}})).unwrap_err();
    assert_eq!(error_code(&result), "capability_not_found");
}

#[test]
fn native_integration_discovery_requires_a_current_authority_and_agent_grant() {
    let fixture = Fixture::new();
    {
        let db = fixture.db.lock().unwrap();
        // Capability discovery uses a disposable vault, independent of the host Keychain.
        crate::ui_credentials::use_disposable_key(&db).unwrap();
        crate::ui_credentials::oauth_store(&db, "gmail", &json!({"access_token":"fixture-token"}), true, 0).unwrap();
        let authority = crate::ui_credentials::oauth_authority(&db, "gmail").unwrap();
        crate::ui_store::put(&db, "integration", "gmail", json!({"id":"gmail", "agents":["main"],
            "authorization_id":authority})).unwrap();
    }
    let find = |tools: &Tools<'_>| -> Value { serde_json::from_str(&tools.invoke(&json!({
        "id":"gmail", "name":"discover_capabilities", "arguments":{"query":"gmail", "kind":"integration"}
    })).unwrap()).unwrap() };
    let tools = fixture.tools();
    let ready = find(&tools);
    assert_eq!(ready["results"][0]["status"], "ready");
    assert!(ready["results"][0]["call"].as_str().unwrap().contains("integration_read"));
    let mut specialist = fixture.tools();
    specialist.persona = json!({"id":"specialist", "tool_policy":{}});
    let unassigned = find(&specialist);
    assert_eq!(unassigned["results"][0]["status"], "unavailable");
    assert!(unassigned["results"][0]["call"].is_null());
    {
        let db = fixture.db.lock().unwrap();
        crate::ui_credentials::delete(&db, "oauth:gmail:token").unwrap();
    }
    assert_eq!(find(&tools)["results"][0]["status"], "unavailable");
}

#[test]
#[ignore = "launched in an isolated process with a configured plugin catalog"]
fn configured_plugin_catalog_child() {
    let fixture = Fixture::new();
    let tools = fixture.tools();
    let value: Value = serde_json::from_str(&tools.invoke(&json!({"id":"p","name":"discover_capabilities",
        "arguments":{"query":"calendar","kind":"plugin"}})).unwrap()).unwrap();
    assert_eq!(value["plugin_catalog_status"], "available");
    assert_eq!(value["results"][0]["name"], "calendar-helper");
    assert_eq!(value["results"][0]["status"], "catalog_only");
    assert_eq!(value["results"][0]["runnable"], false);
    assert!(value["results"][0]["call"].is_null());
    let missing = tools.invoke(&json!({"id":"missing","name":"discover_capabilities",
        "arguments":{"query":"zznonexistentcatalogitemzz"}})).unwrap_err();
    let error: Value = serde_json::from_str(&missing.to_string()).unwrap();
    assert_eq!(error["plugin_catalog_status"], "available");
}

#[test]
fn discovery_exposes_configured_plugin_metadata_without_executing_it() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("catalog.json");
    std::fs::write(&path, json!({"plugins":[{"name":"calendar-helper","description":"Find calendar events",
        "source":"https://example.test/calendar","command":"never execute"}]}).to_string()).unwrap();
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "tool_list_tests::configured_plugin_catalog_child", "--nocapture"])
        .env("KAGEHA_PLUGIN_CATALOG_FILE", &path).output().unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
}

#[test]
fn discovery_can_recover_from_a_miss_and_find_multiple_task_capabilities() {
    let fixture = Fixture::new();
    let tools = fixture.tools();
    let find = |id: &str, query: &str, kind: &str| tools.invoke(&json!({
        "id":id,"name":"discover_capabilities","arguments":{"query":query,"kind":kind}
    }));
    let miss = find("missing", "zznonexistentcapabilityzz", "tool").unwrap_err();
    assert_eq!(error_code(&miss), "capability_not_found");
    assert!(miss.to_string().contains("Try a specific provider name"));
    for (id, query, kind, expected) in [
        ("mail", "mailbox message", "tool", "fixture::search_mail"),
        ("lookup", "fixture::lookup", "tool", "fixture::lookup"),
        ("echo", "fixture::echo", "tool", "fixture::echo"),
        ("skill", "release-notes", "skill", "release-notes"),
    ] {
        let value: Value = serde_json::from_str(&find(id, query, kind).unwrap()).unwrap();
        assert!(value["results"].as_array().unwrap().iter().any(|item| item["name"] == expected), "{value}");
    }
}

#[test]
fn discovery_never_exposes_denied_connector_tools() {
    let fixture = Fixture::new();
    let mut tools = fixture.tools();
    tools.persona = json!({"id":"main","tool_policy":{"deny":[kageha_mcp::tool_id("fixture", "search_mail")]}});
    let value: Value = serde_json::from_str(&tools.invoke(&json!({
        "id":"search", "name":"discover_capabilities", "arguments":{"query":"mailbox message","kind":"tool"}
    })).unwrap()).unwrap();
    assert!(!value["results"].as_array().unwrap().iter().any(|item| item["server"] == "fixture" && item["tool"] == "search_mail"));
}

#[test]
fn discovery_respects_the_tool_and_skill_budget() {
    let fixture = Fixture::new();
    let tools = fixture.tools();
    let call = |id: &str| tools.invoke(&json!({"id":id,"name":"discover_capabilities","arguments":{"query":"fixture::echo"}}));
    for _ in 0..12 { assert!(call("discover").is_ok()); }
    let exhausted = call("too-many").unwrap_err();
    assert_eq!(error_code(&exhausted), "capability_discovery_exhausted");
    let fresh = fixture.tools();
    assert!(fresh.invoke(&json!({"id":"invalid","name":"discover_capabilities","arguments":{"query":"mail","kind":42}})).is_err());
}

#[test]
fn mcp_call_and_legacy_tool_ids_reach_the_mcp_tool() {
    let fixture = Fixture::new();
    let tools = fixture.tools();
    let result: Value = serde_json::from_str(&tools.invoke(&mcp_call("m1", "fixture", "echo", json!({"text":"hi"}))).unwrap()).unwrap();
    assert_eq!(result["content"][0]["text"], r#"echo:{"text":"hi"}"#, "{result}");
    // A direct call by MCP tool id, as in old checkpoints and transcripts, still dispatches.
    let legacy = json!({"id":"m2","name":kageha_mcp::tool_id("fixture", "echo"),"arguments":{"text":"old"}});
    let result: Value = serde_json::from_str(&tools.invoke(&legacy).unwrap()).unwrap();
    assert_eq!(result["content"][0]["text"], r#"echo:{"text":"old"}"#, "{result}");
    // Unknown targets and malformed calls are refused before any dispatch.
    assert_eq!(error_code(&tools.invoke(&mcp_call("m3", "fixture", "missing", json!({}))).unwrap_err()), "mcp_tool_unavailable");
    assert_eq!(error_code(&tools.invoke(&json!({"id":"m4","name":"mcp_call","arguments":{"server":"fixture"}})).unwrap_err()), "invalid_arguments");
}

#[test]
fn mcp_call_is_an_uncached_action_unless_the_target_is_tagged_read() {
    let fixture = Fixture::new();
    let tools = fixture.tools();
    let echo = tools.call_capability("mcp_call", &json!({"server":"fixture","tool":"echo"}));
    assert_eq!((echo.effect, echo.cacheable), (Effect::Action, false));
    let lookup = tools.call_capability("mcp_call", &json!({"server":"fixture","tool":"lookup"}));
    assert_eq!((lookup.effect, lookup.cacheable), (Effect::Read, true));
    assert_eq!(tools.call_capability("read_file", &json!({})), crate::ui_tools::capability("read_file"));
}

#[test]
fn only_reads_are_retried_and_new_error_codes_are_recoverable_change_strategy() {
    use kageha_contracts::execution::{RecoveryAction, ToolOutcomeKind};
    let fixture = Fixture::new();
    let tools = fixture.tools();
    let call = |name: &str, arguments: Value| ToolCall { id: "e1".into(), name: name.into(), arguments };
    for (name, arguments, retried) in [
        ("web_search", json!({"query":"q"}), true),
        ("integration_read", json!({"integration":"gmail"}), true),
        ("mcp_call", json!({"server":"fixture","tool":"lookup"}), true),
        ("mcp_call", json!({"server":"fixture","tool":"echo"}), false),
        ("write_file", json!({"path":"a"}), false),
        ("browser_click", json!({}), false),
        ("agent_wait", json!({}), false),
    ] {
        assert_eq!(tools.retries_transient(&call(name, arguments.clone())), retried, "{name} {arguments}");
    }
    for code in ["tool_failed", "transient_upstream"] {
        let body = json!({"error_code":code,"retryable":code == "transient_upstream","summary":"s"}).to_string();
        let outcome = crate::ui_tool_outcomes::classify(&tools, &call("web_search", json!({"query":"q"})), &Err(io::Error::other(body))).unwrap();
        assert_eq!((outcome.kind, outcome.code.as_str(), outcome.recovery),
            (ToolOutcomeKind::Recoverable, code, Some(RecoveryAction::ChangeStrategy)));
    }
}

#[test]
fn skill_read_returns_the_instructions_and_files_and_load_skill_still_works() {
    let fixture = Fixture::new();
    let tools = fixture.tools();
    let text = tools.invoke(&json!({"id":"s1","name":"skill_read","arguments":{"name":"release-notes"}})).unwrap();
    assert!(text.contains("List breaking changes first."), "{text}");
    assert!(text.contains("skills/release-notes/references/format.md"), "{text}");
    let alias = tools.invoke(&json!({"id":"s2","name":"load_skill","arguments":{"name":"release-notes"}})).unwrap();
    assert_eq!(alias, text);
}

#[test]
fn an_offered_run_change_keeps_the_list_and_refuses_calls_as_unavailable_now() {
    let fixture = Fixture::new();
    let tools = fixture.tools();
    let before = tools.specs();
    fixture.db.lock().unwrap().execute(
        "INSERT INTO ui_run_changes(chat,turn,state,proposal) VALUES (?,?,'offered','{}')", [CHAT, fixture.turn.as_str()]).unwrap();
    assert_eq!(tools.specs(), before, "the tool list stays the same for the turn");
    let call = json!({"id":"r1","name":"list_workspace_files","arguments":{}});
    let error = tools.invoke(&call).unwrap_err();
    assert_eq!(error_code(&error), "tool_unavailable_now");
    let outcome = crate::ui_tool_outcomes::classify(&tools,
        &ToolCall { id: "r1".into(), name: "list_workspace_files".into(), arguments: json!({}) }, &Err(error)).unwrap();
    assert_eq!((outcome.kind, outcome.code.as_str()), (kageha_contracts::execution::ToolOutcomeKind::Blocked, "tool_unavailable_now"));
}

#[test]
fn shell_is_listed_dispatched_and_its_failures_are_classified() {
    use kageha_contracts::execution::{RecoveryAction, ToolOutcomeKind};
    let fixture = Fixture::new();
    let tools = fixture.tools();
    let names: Vec<String> = tools.specs().iter().filter_map(|spec| spec["function"]["name"].as_str().map(str::to_owned)).collect();
    let listed = crate::ui_shell::available();
    assert_eq!(names.iter().any(|name| name == "shell"), listed);
    assert_eq!(names.iter().any(|name| name == "shell_job"), listed);
    if listed {
        let result: Value = serde_json::from_str(&tools.invoke(&json!({"id":"s1","name":"shell","arguments":{"command":"echo hi > outputs/hi.txt; echo ok"}})).unwrap()).unwrap();
        assert_eq!((result["exit_code"].clone(), result["stdout"].clone()), (json!(0), json!("ok\n")));
        assert_eq!(result["files_changed"][0]["artifact_path"], "workspace/outputs/hi.txt");
    }
    let classify = |name: &str, body: Value| crate::ui_tool_outcomes::classify(&tools,
        &ToolCall { id: "s2".into(), name: name.into(), arguments: json!({"command":"sleep 5"}) },
        &Err(io::Error::other(body.to_string()))).unwrap();
    // Escalation needs a reason; without one nothing is asked or run.
    let escalate = tools.invoke(&json!({"id":"s2","name":"shell","arguments":{"command":"touch /etc/x","escalate":true}})).unwrap_err();
    assert_eq!(error_code(&escalate), "invalid_arguments");
    let outcome = crate::ui_tool_outcomes::classify(&tools,
        &ToolCall { id: "s2".into(), name: "shell".into(), arguments: json!({}) }, &Err(escalate)).unwrap();
    assert_eq!((outcome.kind, outcome.recovery), (ToolOutcomeKind::Recoverable, Some(RecoveryAction::ChangeStrategy)));
    let outcome = classify("shell", json!({"error_code":"policy_denied","retry_with":{"escalate":true}}));
    assert_eq!((outcome.kind, outcome.recovery), (ToolOutcomeKind::Blocked, Some(RecoveryAction::ChangeStrategy)));
    let outcome = classify("shell", json!({"error_code":"policy_denied","denied_paths":["/etc/x"]}));
    assert_eq!((outcome.kind, outcome.recovery), (ToolOutcomeKind::Blocked, Some(RecoveryAction::ChangeStrategy)));
    let outcome = classify("shell", json!({"error_code":"approval_denied","approval_id":"a1"}));
    assert_eq!((outcome.kind, outcome.code.as_str(), outcome.recovery),
        (ToolOutcomeKind::Blocked, "approval_denied", Some(RecoveryAction::ChangeStrategy)));
    let outcome = classify("shell", json!({"error_code":"command_timeout","job_id":"job-0000000000000000"}));
    assert_eq!((outcome.kind, outcome.code.as_str()), (ToolOutcomeKind::Recoverable, "command_timeout"));
    let outcome = classify("shell_job", json!({"error_code":"job_not_found"}));
    assert_eq!(outcome.kind, ToolOutcomeKind::Recoverable);
}

#[test]
fn unsuccessful_checks_are_recovery_failures_even_when_transport_succeeded() {
    use kageha_contracts::execution::{RecoveryAction, ToolOutcomeKind};
    let fixture=Fixture::new();
    let tools=fixture.tools();
    for (name,body) in [("browser_validate",json!({"validation":{"passed":false}})),
        ("shell",json!({"exit_code":1})), ("shell_job",json!({"state":"exited","exit_code":2}))] {
        let call=ToolCall{id:"c".into(),name:name.into(),arguments:json!({})};
        for content in [body.to_string(),json!({"tool_result":body.to_string(),"hook_context":"context"}).to_string()] {
            let outcome=crate::ui_tool_outcomes::classify(&tools,&call,&Ok(content)).unwrap();
            assert_eq!((outcome.kind,outcome.recovery),(ToolOutcomeKind::Recoverable,Some(RecoveryAction::ChangeStrategy)));
            assert!(outcome.evidence.is_empty());
        }
    }
}

#[test]
fn post_submission_hook_stop_preserves_original_receipt_and_evidence() {
    use kageha_workflow::completion::OutcomeKind;
    let fixture=Fixture::new();
    let tools=fixture.tools();
    crate::ui_task_completion::record(&fixture.db,CHAT,&fixture.turn,&json!({"outcomes":[{
        "id":"submit","description":"Submit application","kind":"action","verify":"Site confirmation"
    }]}),&std::collections::BTreeSet::from([OutcomeKind::Action])).unwrap();
    let args=json!({"application_submission":true,"outcome_id":"submit","expected_selector":"#done","expected_text":"Received"});
    crate::ui_task_completion::prepare_submission(&fixture.db,CHAT,&fixture.turn,&args,
        &json!({"target_id":"tab","document_id":"doc"}),"req").unwrap();
    let call=json!({"id":"click","name":"browser_click","arguments":args});
    let receipt=json!({"receipt":{"request_id":"req","state":"verified","action":"click",
        "target_id":"tab","document_id":"doc","expected":{"selector":"#done","text":"Received"}}}).to_string();
    let result=tools.record_tool_success(&call,&receipt).unwrap();
    let body:Value=serde_json::from_str(&result).unwrap();
    assert_eq!(body["submission_evidence"]["verified_reference"],"receipt:req");
    let error=tools.post_submission_error(&call,&result,"postToolUse",kageha_contracts::policy::stop("Hook denied"));
    assert!(kageha_contracts::policy::is_stop(&error));
    let body:Value=serde_json::from_str(&error.to_string()).unwrap();
    assert_eq!(body["hook_stage"],"postToolUse");
    assert_eq!(body["hook_error"],"Hook denied");
    assert_eq!(body["submission_result"]["receipt"]["request_id"],"req");
    assert_eq!(body["submission_result"]["submission_evidence"]["verified_reference"],"receipt:req");
    let root=crate::ui_task_completion::scope(&fixture.db,&fixture.turn).unwrap();
    let contract=crate::ui_task_completion::load(&fixture.db.lock().unwrap(),CHAT,&root).unwrap();
    assert!(contract.evidence.iter().any(|e|e.outcome_id=="submit" && e.reference=="receipt:req"));
}
