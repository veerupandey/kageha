//! The Activity panel's routes (task 27.3), served by `server::handle` with its token check.
use crate::audited_session_tests::{Fixture, CHAT};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const TOKEN: &str = "workspace-api-test-token";

struct Api {
    server: Arc<tiny_http::Server>,
    _workspace: tempfile::TempDir,
}

struct Reply {
    status: u16,
    headers: String,
    body: Vec<u8>,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
    fn header(&self, name: &str) -> Option<String> {
        self.headers.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name).then(|| value.trim().to_owned())
        })
    }
}

impl Api {
    fn start(fixture: &Fixture) -> Self {
        let workspace = tempfile::tempdir().unwrap();
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
        let (incoming, db, root) = (server.clone(), fixture.db.clone(), workspace.path().to_path_buf());
        let controls: crate::webui::Controls = Arc::new(Mutex::new(Default::default()));
        std::thread::spawn(move || {
            for request in incoming.incoming_requests() {
                if let Err(error) = crate::server::handle(request, &db, &controls, &root, TOKEN) {eprintln!("test server error: {error}");}
            }
        });
        Self { server, _workspace: workspace }
    }

    fn send(&self, method: &str, path: &str, token: Option<&str>, body: &str) -> Reply {
        self.send_bytes(method, path, token, "application/json", body.as_bytes())
    }

    fn send_bytes(&self, method: &str, path: &str, token: Option<&str>, content_type: &str, body: &[u8]) -> Reply {
        let address = self.server.server_addr().to_ip().unwrap();
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let authorization = token.map(|token| format!("Authorization: Bearer {token}\r\n")).unwrap_or_default();
        write!(stream, "{method} {path} HTTP/1.1\r\nHost: localhost\r\n{authorization}Content-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
        stream.write_all(body).unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        let split = response.windows(4).position(|window| window == b"\r\n\r\n").unwrap();
        let head = String::from_utf8(response[..split].to_vec()).unwrap();
        let status = head.split(' ').nth(1).unwrap().parse().unwrap();
        let mut body = response[split + 4..].to_vec();
        if head.to_ascii_lowercase().contains("transfer-encoding: chunked") {
            body = dechunk(&body);
        }
        Reply { status, headers: head, body }
    }

    fn get(&self, path: &str) -> Reply {
        self.send("GET", path, Some(TOKEN), "")
    }
}

fn dechunk(mut body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let line = body.windows(2).position(|w| w == b"\r\n").unwrap();
        let size = usize::from_str_radix(std::str::from_utf8(&body[..line]).unwrap().trim(), 16).unwrap();
        if size == 0 {
            return out;
        }
        out.extend_from_slice(&body[line + 2..line + 2 + size]);
        body = &body[line + 4 + size..];
    }
}

impl Drop for Api {
    fn drop(&mut self) {
        self.server.unblock();
    }
}

fn route(operation: &str) -> String {
    format!("/api/sessions/{CHAT}/workspace/{operation}")
}

fn workspace(fixture: &Fixture) -> kageha_workspace::task::TaskWorkspace {
    crate::ui_workspace::open_for_chat(&fixture.db, CHAT).unwrap()
}

#[test]
fn thread_menu_changes_are_persisted_and_reversible() {
    use kageha_runtime::lifecycle::RunStore;
    let fixture = Fixture::new();
    crate::runtime_host::store(&fixture.db).unwrap().cancel(&fixture.turn).unwrap();
    let api = Api::start(&fixture);
    let path = format!("/api/sessions/{CHAT}");
    let patch = |body: &str| api.send("PATCH", &path, Some(TOKEN), body);

    assert_eq!(patch(r#"{"title":"Renamed thread"}"#).status, 200);
    assert_eq!(api.get(&path).json()["title"], "Renamed thread");
    for pinned in [true, false] {
        assert_eq!(patch(&json!({"pinned":pinned}).to_string()).status, 200);
        assert_eq!(api.get(&path).json()["pinned"], pinned);
    }
    let projects = api.send("POST", "/api/projects", Some(TOKEN), r#"{"name":"Thread actions"}"#);
    let project = projects.json()["project"]["id"].as_str().unwrap().to_owned();
    let project_path = format!("{path}/project");
    assert_eq!(api.send("POST", &project_path, Some(TOKEN), &json!({"project":project}).to_string()).status, 200);
    assert_eq!(api.get(&path).json()["project_id"], project);
    assert_eq!(api.send("POST", &project_path, Some(TOKEN), r#"{"project":""}"#).status, 200);
    assert!(api.get(&path).json()["project_id"].is_null());

    let archive = patch(r#"{"archived":true}"#);
    assert_eq!(archive.status, 200, "{:?}", archive.json());
    let archived = poll_session(&api, |v| v["archived"] == true);
    assert_eq!(archived["lifecycle_status"], "closed");
    assert_eq!(patch(r#"{"archived":false,"title":123}"#).status, 400);
    assert_eq!(api.get(&path).json()["lifecycle_status"], "closed");
    assert_eq!(patch(r#"{"archived":false}"#).status, 200);
    let restored = api.get(&path).json();
    assert_eq!(restored["archived"], false);
    assert_eq!(restored["lifecycle_status"], "open");
}

#[test]
fn thread_transfer_preserves_history_and_owned_attachments() {
    let fixture = Fixture::new();
    let api = Api::start(&fixture);
    let path = format!("/api/sessions/{CHAT}");
    let upload = api.send_bytes(
        "POST", &format!("{path}/upload"), Some(TOKEN), "multipart/form-data; boundary=fixture",
        b"--fixture\r\nContent-Disposition: form-data; name=\"file\"; filename=\"notes.txt\"\r\n\r\nattached notes\r\n--fixture--\r\n",
    );
    let uploaded = upload.json();
    assert_eq!(upload.status, 200, "{uploaded}");
    {
        let conn = fixture.db.lock().unwrap();
        crate::ui_projects::assign(&conn, CHAT, "Fork project").unwrap();
        for i in 0..102 {
            conn.execute(
                "INSERT INTO native_chat_turns(chat_id,request_id,message,answer,status) VALUES (?,?,?,?,'completed')",
                rusqlite::params![CHAT, format!("history-{i}"), format!("Question {i}"), format!("Answer {i}")],
            ).unwrap();
        }
        conn.execute("INSERT INTO ui_turn_uploads VALUES (?,'history-0',?)",
            rusqlite::params![CHAT, json!([uploaded["path"]]).to_string()]).unwrap();
    }
    assert_eq!(api.get(&path).json()["messages"].as_array().unwrap().len(), 200);
    assert_eq!(api.send("GET", &format!("{path}/fork"), Some(TOKEN), "").status, 405);
    let fork = api.send("POST", &format!("{path}/fork"), Some(TOKEN), "");
    assert_eq!(fork.status, 200, "{:?}", fork.json());
    let forked_id = fork.json()["session_id"].as_str().unwrap().to_owned();
    assert_ne!(forked_id, CHAT);
    let check_copy = |api: &Api, id: &str| {
        let copy = api.get(&format!("/api/sessions/{id}")).json();
        assert_eq!(copy["messages"].as_array().unwrap().len(), 204);
        assert_eq!(copy["messages"][0]["text"], "Question 0");
        assert_eq!(copy["messages"][203]["text"], "Answer 101");
        assert_eq!(copy["active"], false);
        assert_eq!(copy["lifecycle_status"], "open");
        assert_eq!(copy["archived"], false);
        assert_eq!(copy["pinned"], false);
        let attachment = copy["messages"][0]["attachments"][0].as_str().unwrap();
        assert_eq!(api.get(&format!("/api/sessions/{id}/files/{attachment}")).body, b"attached notes");
        copy
    };
    let forked = check_copy(&api, &forked_id);
    assert_eq!(forked["title"], "Seequent prep (fork)");
    assert_eq!(forked["project_id"], api.get(&path).json()["project_id"]);
    assert_eq!(api.get(&path).json()["title"], "Seequent prep");

    let exported = api.get(&format!("{path}/export"));
    assert_eq!(exported.status, 200);
    assert_eq!(exported.header("Content-Type").as_deref(), Some("application/zip"));
    assert_eq!(exported.header("X-Kageha-Unavailable-Attachments").as_deref(), Some("0"));
    let other_instance = Fixture::for_chat("other-instance");
    let destination = Api::start(&other_instance);
    let import = |data: &[u8]| destination.send_bytes("POST", "/api/sessions/import", Some(TOKEN), "application/zip", data);
    assert_eq!(import(b"Invalid ZIP").status, 400);
    let imported = import(&exported.body);
    assert_eq!(imported.status, 200, "{:?}", imported.json());
    let imported_id = imported.json()["session_id"].as_str().unwrap().to_owned();
    assert_ne!(imported_id, CHAT);
    let imported = check_copy(&destination, &imported_id);
    assert_eq!(imported["title"], "Seequent prep");
    assert!(imported["project_id"].is_null());

    assert_eq!(api.get("/api/sessions/missing/export").status, 404);
    assert_eq!(api.send("GET", &format!("{path}/export"), None, "").status, 401);
    fixture.db.lock().unwrap().execute("UPDATE native_chat_turns SET status='running' WHERE request_id='history-101'", []).unwrap();
    assert_eq!(api.send("POST", &format!("{path}/fork"), Some(TOKEN), "").status, 409);
    assert_eq!(api.get(&format!("{path}/export")).status, 409);
}

#[test]
fn the_routes_require_the_token_and_get() {
    let fixture = Fixture::new();
    let api = Api::start(&fixture);
    for operation in ["tree", "plan", "file?path=todo.md"] {
        assert_eq!(api.send("GET", &route(operation), None, "").status, 401, "{operation}");
        assert_eq!(api.send("POST", &route(operation), Some(TOKEN), "{}").status, 405, "{operation}");
    }
    assert_eq!(api.get("/api/sessions/unknown-chat/workspace/tree").status, 404);
}

#[test]
fn the_tree_comes_from_the_manifest_without_host_state() {
    let fixture = Fixture::new();
    let workspace = workspace(&fixture);
    workspace.write("outputs/report.md", b"# Report\n").unwrap();
    workspace.write("notes/a b.txt", b"spaced").unwrap();
    // Host state, a delegated worker's nested workspace and an unscanned shell write.
    std::fs::write(workspace.root().join(".kageha/tool-output/call.txt"), b"spilled").unwrap();
    let worker = kageha_workspace::task::TaskWorkspace::open_nested(&workspace, "job-1").unwrap();
    worker.write("outputs/worker.md", b"worker").unwrap();
    std::fs::write(workspace.root().join("outputs/unscanned.txt"), b"later").unwrap();
    let api = Api::start(&fixture);

    let reply = api.get(&route("tree"));
    assert_eq!(reply.status, 200);
    let body = reply.json();
    assert_eq!((body["exists"].as_bool(), body["truncated"].as_bool()), (Some(true), Some(false)));
    let paths: Vec<_> = body["files"].as_array().unwrap().iter().map(|f| f["path"].as_str().unwrap().to_owned()).collect();
    assert_eq!(paths, ["notes/a b.txt", "outputs/report.md", "todo.md"], "{body}");
    let report = &body["files"][1];
    assert_eq!(report["size"], 9);
    assert_eq!(report["artifact_path"], "workspace/outputs/report.md");
    assert_eq!(report["sha256"].as_str().unwrap().len(), 64);
    // Reading the tree does not scan: the pending change is still reported to the turn.
    let pending: Vec<_> = workspace.scan_changes().unwrap().into_iter().map(|c| c.path).collect();
    assert_eq!(pending, ["outputs/unscanned.txt"]);
}

#[test]
fn the_tree_and_plan_do_not_create_a_workspace() {
    let fixture = Fixture::new();
    let root = workspace(&fixture).root().to_path_buf();
    std::fs::remove_dir_all(&root).unwrap();
    let api = Api::start(&fixture);
    assert_eq!(api.get(&route("tree")).json(), json!({"root":"workspace","exists":false,"files":[],"truncated":false}));
    assert_eq!(api.get(&route("plan")).json(), json!({"path":"todo.md","exists":false,"content":"","sha256":null}));
    assert_eq!(api.get(&route("file?path=todo.md")).status, 404);
    assert!(!root.exists());
}

#[test]
fn the_plan_has_its_hash_for_change_detection() {
    let fixture = Fixture::new();
    let workspace = workspace(&fixture);
    let api = Api::start(&fixture);
    let empty = api.get(&route("plan")).json();
    assert_eq!((empty["exists"].as_bool(), empty["content"].as_str()), (Some(true), Some("")));
    workspace.write("todo.md", b"- [x] draft\n- [ ] verify\n").unwrap();
    let plan = api.get(&route("plan")).json();
    assert_eq!(plan["content"], "- [x] draft\n- [ ] verify\n");
    assert_ne!(plan["sha256"], empty["sha256"]);
    use sha2::Digest;
    assert_eq!(plan["sha256"].as_str().unwrap(), format!("{:x}", sha2::Sha256::digest(b"- [x] draft\n- [ ] verify\n")));
}

#[test]
fn a_file_is_served_inline_or_as_a_download() {
    let fixture = Fixture::new();
    let workspace = workspace(&fixture);
    workspace.write("outputs/report.md", b"# Report\n").unwrap();
    workspace.write("outputs/r\u{e9}sum\u{e9} 1.pdf", b"%PDF-1.7").unwrap();
    // Not registered and not in the manifest yet: still served (the tree catches up).
    std::fs::write(workspace.root().join("outputs/page.html"), b"<script>alert(1)</script>").unwrap();
    let api = Api::start(&fixture);

    let inline = api.get(&route("file?path=outputs%2Freport.md"));
    assert_eq!(inline.status, 200);
    assert_eq!(inline.body, b"# Report\n");
    assert_eq!(inline.header("Content-Type").as_deref(), Some("text/plain; charset=utf-8"));
    assert_eq!(inline.header("Content-Disposition").as_deref(), Some("inline; filename=\"report.md\"; filename*=UTF-8''report.md"));
    assert_eq!(inline.header("X-Content-Type-Options").as_deref(), Some("nosniff"));
    // The artifact form names the same file.
    assert_eq!(api.get(&route("file?path=workspace/outputs/report.md")).body, b"# Report\n");

    let download = api.get(&route("file?path=outputs/r%C3%A9sum%C3%A9%201.pdf&download=1"));
    assert_eq!(download.status, 200);
    assert_eq!(download.header("Content-Type").as_deref(), Some("application/pdf"));
    assert_eq!(download.header("Content-Disposition").as_deref(),
        Some("attachment; filename=\"r_sum_ 1.pdf\"; filename*=UTF-8''r%C3%A9sum%C3%A9%201.pdf"));
    // HTML is never served as an active type in the app's origin.
    assert_eq!(api.get(&route("file?path=outputs/page.html")).header("Content-Type").as_deref(), Some("application/octet-stream"));
}

#[test]
fn a_large_file_is_streamed_whole() {
    let fixture = Fixture::new();
    let workspace = workspace(&fixture);
    // Above the 16 MiB upload cap, which workspace files don't have (Requirement 3.6).
    let bytes: Vec<u8> = (0..17 * 1024 * 1024u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(workspace.root().join("outputs/data.bin"), &bytes).unwrap();
    let api = Api::start(&fixture);
    let reply = api.get(&route("file?path=outputs/data.bin&download=1"));
    assert_eq!(reply.status, 200);
    assert_eq!(reply.body.len(), bytes.len());
    assert!(reply.body == bytes);
}

#[test]
fn file_paths_are_confined_and_host_state_is_refused() {
    let fixture = Fixture::new();
    let workspace = workspace(&fixture);
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("secret.txt"), b"secret").unwrap();
    std::os::unix::fs::symlink(outside.path().join("secret.txt"), workspace.root().join("outputs/link.txt")).unwrap();
    std::os::unix::fs::symlink(outside.path(), workspace.root().join("outside")).unwrap();
    std::fs::write(workspace.root().join(".kageha/api-token"), b"token").unwrap();
    let worker = kageha_workspace::task::TaskWorkspace::open_nested(&workspace, "job-1").unwrap();
    worker.write("outputs/worker.md", b"worker").unwrap();
    let api = Api::start(&fixture);
    for (path, status) in [
        ("..%2F..%2Fnative.sqlite", 403),
        ("../outside.txt", 403),
        ("outputs/../../x", 403),
        ("%2Fetc%2Fpasswd", 403),
        ("outputs/link.txt", 403),
        ("outside/secret.txt", 403),
        (".kageha/api-token", 403),
        (".kageha/manifest.sqlite", 403),
        ("outputs/../.kageha/api-token", 403),
        ("agents/job-1/outputs/worker.md", 403),
        ("agents/job-1/.kageha/manifest.sqlite", 403),
        ("outputs", 404),
        ("outputs/missing.md", 404),
        ("", 400),
        ("bad%ZZ", 400),
    ] {
        let reply = api.get(&route(&format!("file?path={path}")));
        assert_eq!(reply.status, status, "{path}: {}", String::from_utf8_lossy(&reply.body));
        assert!(!String::from_utf8_lossy(&reply.body).contains("secret"), "{path}");
    }
    // A missing `path` is the empty path.
    assert_eq!(api.get(&route("file")).status, 400);
}

#[test]
fn the_skill_list_shows_learned_and_degraded_skills_and_disable_and_delete_work() {
    use kageha_skills::Package;
    let fixture = Fixture::new();
    let package = |body: &str| Package::plain("digest", &format!("---\nname: digest\ndescription: Summarize the inbox.\n---\n{body}")).unwrap();
    {
        let mut registry = crate::ui_skills::owned(&fixture.db).unwrap();
        registry.install(&package("Version one."), "learned", None).unwrap();
        registry.install(&package("Version two."), "learned", None).unwrap();
        registry.degrade("digest", None).unwrap();
    }
    let api = Api::start(&fixture);
    let list = api.get("/api/extensions/skills").json();
    let skill = list["skills"].as_array().unwrap().iter().find(|s| s["name"] == "digest").unwrap().clone();
    assert_eq!((skill["source"].as_str(), skill["degraded"].as_bool()), (Some("learned"), Some(true)), "{skill}");
    assert_eq!(skill["restored_from"], 1);
    assert_eq!(skill["archived"], false);

    let change = |action: &str| api.send("POST", "/api/knowledge/skill", Some(TOKEN), &json!({"name":"digest","action":action}).to_string());
    let disabled = change("archive");
    assert_eq!(disabled.status, 200);
    let row = |body: &Value| body["skills"].as_array().unwrap().iter().find(|s| s["name"] == "digest").cloned();
    assert_eq!(row(&disabled.json()).unwrap()["archived"], true);
    assert_eq!(row(&change("restore").json()).unwrap()["archived"], false);
    let deleted = change("delete");
    assert_eq!(deleted.status, 200);
    assert!(row(&deleted.json()).is_none());
    assert!(row(&api.get("/api/extensions/skills").json()).is_none());
}

#[test]
fn plugin_hook_route_requires_operator_token_and_tracks_activation() {
    let fixture=Fixture::new();
    let commit="0123456789012345678901234567890123456789";
    let config=crate::ui_plugin_components::hook_definition("fixture",commit,&json!({"hooks":{"PreToolUse":[{"matcher":"browser_*","hooks":[{"type":"command","command":"printf test"}]}]}})).unwrap();
    {
        let db=fixture.db.lock().unwrap();
        crate::ui_store::put(&db,"plugin-installed","fixture",json!({"name":"fixture","status":"installed","commit":commit,"hooks_sha256":config["sha256"],"mcp":[]})).unwrap();
        crate::ui_store::put(&db,"plugin-hooks","fixture",config).unwrap();
    }
    let api=Api::start(&fixture);
    let enable=r#"{"name":"fixture","enabled":true}"#;
    assert_eq!(api.send("POST","/api/plugins/hooks",None,enable).status,401);
    assert!(crate::ui_plugin_components::active_hooks(&fixture.db).unwrap().is_empty());
    assert_eq!(api.send("POST","/api/plugins/hooks",Some(TOKEN),r#"{"name":"fixture"}"#).status,400);
    assert_eq!(api.send("POST","/api/plugins/hooks",Some(TOKEN),r#"{"name":"other","enabled":true}"#).status,400);
    let response=api.send("POST","/api/plugins/hooks",Some(TOKEN),enable);
    assert_eq!(response.status,200); assert_eq!(response.json(),json!({"plugin":"fixture","enabled":true}));
    let active=crate::ui_plugin_components::active_hooks(&fixture.db).unwrap();
    assert_eq!(active.len(),1);
    assert_eq!(api.get("/api/plugins").json()["plugins"][0]["hooks_enabled"],true);
    let disable=r#"{"name":"fixture","enabled":false}"#;
    assert_eq!(api.send("POST","/api/plugins/hooks",Some(TOKEN),disable).status,200);
    assert!(crate::ui_plugin_components::verify_active_hook(&fixture.db,&active[0]).is_err());
    let mut installed=crate::ui_store::get(&fixture.db.lock().unwrap(),"plugin-installed","fixture").unwrap().unwrap();
    installed["commit"]=json!("ffffffffffffffffffffffffffffffffffffffff");
    crate::ui_store::put(&fixture.db.lock().unwrap(),"plugin-installed","fixture",installed).unwrap();
    assert_eq!(api.send("POST","/api/plugins/hooks",Some(TOKEN),enable).status,400);
    assert!(crate::ui_plugin_components::active_hooks(&fixture.db).unwrap().is_empty());
}

fn install_session_end_hook(fixture:&Fixture) {
    use kageha_runtime::lifecycle::RunStore;
    let store=crate::runtime_host::store(&fixture.db).unwrap();
    if store.find_turn(&fixture.turn).unwrap().is_some_and(|run|!run.state.terminal()) {store.cancel(&fixture.turn).unwrap();}
    let commit="0123456789012345678901234567890123456789";
    let config=crate::ui_plugin_components::hook_definition("fixture",commit,&json!({"hooks":{"SessionEnd":[{"matcher":"other","hooks":[{"type":"command","command":"printf x >> close-count","async":true}]}]}})).unwrap();
    {let conn=fixture.db.lock().unwrap();
        crate::ui_store::put(&conn,"plugin-installed","fixture",json!({"name":"fixture","status":"installed","commit":commit,"hooks_sha256":config["sha256"]})).unwrap();
        crate::ui_store::put(&conn,"plugin-hooks","fixture",config).unwrap();
        crate::ui_approvals::owned(&conn).unwrap().set_profile(CHAT,kageha_contracts::policy::PermissionProfile::Ask).unwrap();
    }
    crate::ui_plugin_components::set_hooks_enabled(&fixture.db,"fixture",true).unwrap();
}
fn poll_session(api:&Api,predicate:impl Fn(&Value)->bool)->Value {
    let deadline=std::time::Instant::now()+Duration::from_secs(5);
    loop {
        let value=api.get(&format!("/api/sessions/{CHAT}")).json();
        if predicate(&value){return value;}
        assert!(std::time::Instant::now()<deadline,"session state timed out: {value}");
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn close_session_runs_hook_once_after_manual_approval_and_can_reopen() {
    let fixture=Fixture::new();install_session_end_hook(&fixture);let api=Api::start(&fixture);
    let path=format!("/api/sessions/{CHAT}/close");
    let first=api.send("POST",&path,Some(TOKEN),"{}");assert_eq!(first.status,200);let turn=first.json()["turn_id"].clone();
    let state=poll_session(&api,|v|v["pending_approval"]["approval_id"].is_string());
    assert_eq!(state["lifecycle_status"],"closing");assert_eq!(state["lifecycle"]["turn_id"],turn);
    assert!(!api._workspace.path().join("close-count").exists());
    assert_eq!(api.send("POST",&path,Some(TOKEN),"{}").json()["turn_id"],turn);
    assert_eq!(api.send("POST",&format!("/api/sessions/{CHAT}/reopen"),Some(TOKEN),"{}").status,409);
    let blocked=api.send("POST","/api/chat/send",Some(TOKEN),&json!({"session_id":CHAT,"client_request_id":"while-closing","message":"hello"}).to_string());
    assert_eq!(blocked.status,409);
    let decision=json!({"approval_id":state["pending_approval"]["approval_id"],"approved":true});
    let approved=api.send("POST","/api/approvals",Some(TOKEN),&decision.to_string());
    assert_eq!(approved.status,200);assert_eq!(approved.json()["approved"],true);
    poll_session(&api,|v|v["lifecycle_status"]=="closed");
    assert_eq!(std::fs::read_to_string(api._workspace.path().join("close-count")).unwrap(),"x");
    assert_eq!(api.send("POST",&path,Some(TOKEN),"{}").json()["status"],"closed");
    assert_eq!(std::fs::read_to_string(api._workspace.path().join("close-count")).unwrap(),"x");
    let receipts=api.get(&format!("/api/sessions/{CHAT}/hooks/receipts")).json();
    assert_eq!(receipts["receipts"].as_array().unwrap().len(),1);
    assert_eq!(api.send("POST",&format!("/api/sessions/{CHAT}/reopen"),Some(TOKEN),"{}").json()["status"],"open");
    assert_eq!(api.get(&format!("/api/sessions/{CHAT}")).json()["lifecycle_status"],"open");
}
#[test]
fn revoking_session_end_during_approval_prevents_dispatch() {
    let fixture=Fixture::new();install_session_end_hook(&fixture);let api=Api::start(&fixture);
    assert_eq!(api.send("POST",&format!("/api/sessions/{CHAT}/close"),Some(TOKEN),"{}").status,200);
    let pending=poll_session(&api,|v|v["pending_approval"]["approval_id"].is_string());
    crate::ui_plugin_components::set_hooks_enabled(&fixture.db,"fixture",false).unwrap();
    let decision=json!({"approval_id":pending["pending_approval"]["approval_id"],"approved":true});
    assert_eq!(api.send("POST","/api/approvals",Some(TOKEN),&decision.to_string()).status,200);
    poll_session(&api,|v|v["lifecycle_status"]=="failed");
    assert!(!api._workspace.path().join("close-count").exists());
}

#[test]
fn interrupted_session_close_reuses_durable_hook_receipt() {
    let fixture=Fixture::new();install_session_end_hook(&fixture);
    let conn=fixture.db.lock().unwrap();
    crate::ui_approvals::owned(&conn).unwrap().set_profile(CHAT,kageha_contracts::policy::PermissionProfile::Full).unwrap();
    drop(conn);
    let api=Api::start(&fixture);
    let workspace=api._workspace.path();
    let hooks=crate::ui_hooks::capture(&fixture.db,workspace).unwrap();
    let set:kageha_workspace::hooks::Set=serde_json::from_value(hooks.clone()).unwrap();
    let turn=crate::webui::turn_id(CHAT,"session-close-recovery");
    crate::ui_hooks::run(&fixture.db,CHAT,&turn,workspace,&set,"sessionEnd",
        &json!({"id":"lifecycle-sessionEnd","name":"session","arguments":{},"reason":"other","end_trigger":"user_close"}),&std::sync::atomic::AtomicBool::new(false)).unwrap();
    crate::ui_store::put(&fixture.db.lock().unwrap(),"session-lifecycle",CHAT,
        json!({"status":"closing","turn_id":turn,"hooks":hooks,"workspace":workspace,"error":null})).unwrap();
    assert_eq!(api.get(&format!("/api/sessions/{CHAT}")).json()["lifecycle_status"],"interrupted");
    let resumed=api.send("POST",&format!("/api/sessions/{CHAT}/close"),Some(TOKEN),"{}");
    assert_eq!(resumed.status,200);assert_eq!(resumed.json()["turn_id"],turn);
    poll_session(&api,|v|v["lifecycle_status"]=="closed");
    assert_eq!(std::fs::read_to_string(workspace.join("close-count")).unwrap(),"x");
    assert_eq!(api.get(&format!("/api/sessions/{CHAT}/hooks/receipts")).json()["receipts"].as_array().unwrap().len(),1);
}

#[test]
fn archive_waits_for_session_end_and_unarchive_reopens() {
    let fixture=Fixture::new();install_session_end_hook(&fixture);let api=Api::start(&fixture);
    let path=format!("/api/sessions/{CHAT}");
    let response=api.send("PATCH",&path,Some(TOKEN),r#"{"archived":true}"#);
    assert_eq!(response.status,200);assert_eq!(response.json()["archived"],false);assert_eq!(response.json()["lifecycle"]["archive"],true);
    let pending=poll_session(&api,|v|v["pending_approval"]["approval_id"].is_string());
    assert_eq!(pending["lifecycle_status"],"closing");assert_eq!(pending["archived"],false);assert_eq!(pending["lifecycle"]["archive"],true);
    assert_eq!(api.send("PATCH",&path,Some(TOKEN),r#"{"archived":false}"#).status,409);
    let decision=json!({"approval_id":pending["pending_approval"]["approval_id"],"approved":true});
    assert_eq!(api.send("POST","/api/approvals",Some(TOKEN),&decision.to_string()).status,200);
    let closed=poll_session(&api,|v|v["lifecycle_status"]=="closed");assert_eq!(closed["archived"],true);
    assert_eq!(std::fs::read_to_string(api._workspace.path().join("close-count")).unwrap(),"x");
    let reopened=api.send("PATCH",&path,Some(TOKEN),r#"{"archived":false}"#);
    assert_eq!(reopened.status,200);assert_eq!(reopened.json()["archived"],false);
    assert_eq!(api.get(&path).json()["lifecycle_status"],"open");
}
#[test]
fn failed_archive_keeps_session_unarchived() {
    let fixture=Fixture::new();install_session_end_hook(&fixture);let api=Api::start(&fixture);
    let path=format!("/api/sessions/{CHAT}");
    assert_eq!(api.send("PATCH",&path,Some(TOKEN),r#"{"archived":true}"#).status,200);
    let pending=poll_session(&api,|v|v["pending_approval"]["approval_id"].is_string());
    crate::ui_plugin_components::set_hooks_enabled(&fixture.db,"fixture",false).unwrap();
    let decision=json!({"approval_id":pending["pending_approval"]["approval_id"],"approved":true});
    assert_eq!(api.send("POST","/api/approvals",Some(TOKEN),&decision.to_string()).status,200);
    let failed=poll_session(&api,|v|v["lifecycle_status"]=="failed");assert_eq!(failed["archived"],false);assert_eq!(failed["lifecycle"]["archive"],true);
    assert!(!api._workspace.path().join("close-count").exists());
}

#[test]
fn session_close_uses_admitted_workspace_for_logical_projects_and_rejects_remote() {
    let fixture=Fixture::new();install_session_end_hook(&fixture);let api=Api::start(&fixture);
    {let conn=fixture.db.lock().unwrap();crate::ui_approvals::owned(&conn).unwrap().set_profile(CHAT,kageha_contracts::policy::PermissionProfile::Full).unwrap();}
    assert_eq!(api.send("POST","/api/projects",Some(TOKEN),r#"{"name":"Logical project"}"#).status,200);
    assert_eq!(api.send("POST",&format!("/api/sessions/{CHAT}/project"),Some(TOKEN),r#"{"project":"Logical project"}"#).status,200);
    let path=format!("/api/sessions/{CHAT}/close");
    assert_eq!(api.send("POST",&path,Some(TOKEN),"{}").status,200);
    poll_session(&api,|v|v["lifecycle_status"]=="closed");
    assert_eq!(std::fs::read_to_string(api._workspace.path().join("close-count")).unwrap(),"x");
    assert_eq!(api.send("POST",&format!("/api/sessions/{CHAT}/reopen"),Some(TOKEN),"{}").status,200);
    crate::ui_store::put(&fixture.db.lock().unwrap(),"remote-session",CHAT,json!({"active":true,"host_id":"remote"})).unwrap();
    assert_eq!(api.send("POST",&path,Some(TOKEN),"{}").status,409);
    assert_eq!(std::fs::read_to_string(api._workspace.path().join("close-count")).unwrap(),"x");
}
#[test]
fn deletion_requires_session_end_completion() {
    use kageha_runtime::lifecycle::RunStore;
    let fixture=Fixture::new();
    crate::runtime_host::store(&fixture.db).unwrap().cancel(&fixture.turn).unwrap();
    install_session_end_hook(&fixture);let api=Api::start(&fixture);
    let path=format!("/api/sessions/{CHAT}");
    let deleted=api.send("DELETE",&path,Some(TOKEN),"");assert_eq!(deleted.status,409);
    assert!(deleted.json()["error"].as_str().unwrap().contains("Close the session"));
    let close=api.send("POST",&format!("{path}/close"),Some(TOKEN),"{}");assert_eq!(close.status,200);
    let pending=poll_session(&api,|v|v["pending_approval"]["approval_id"].is_string());
    assert_eq!(api.send("DELETE",&path,Some(TOKEN),"").status,409);
    let decision=json!({"approval_id":pending["pending_approval"]["approval_id"],"approved":true});
    assert_eq!(api.send("POST","/api/approvals",Some(TOKEN),&decision.to_string()).status,200);
    poll_session(&api,|v|v["lifecycle_status"]=="closed");
    let deleted=api.send("DELETE",&path,Some(TOKEN),"");assert_eq!(deleted.status,200,"{}",String::from_utf8_lossy(&deleted.body));
    assert_eq!(api.get(&path).status,404);
}

#[test]
fn interrupted_archive_reuses_receipt_and_only_then_archives() {
    let fixture=Fixture::new();install_session_end_hook(&fixture);
    {let db=fixture.db.lock().unwrap();crate::ui_approvals::owned(&db).unwrap().set_profile(CHAT,kageha_contracts::policy::PermissionProfile::Full).unwrap();}
    let api=Api::start(&fixture);let workspace=api._workspace.path();
    let hooks=crate::ui_hooks::capture(&fixture.db,workspace).unwrap();
    let set:kageha_workspace::hooks::Set=serde_json::from_value(hooks.clone()).unwrap();
    let turn=crate::webui::turn_id(CHAT,"archive-recovery");
    crate::ui_hooks::run(&fixture.db,CHAT,&turn,workspace,&set,"sessionEnd",
        &json!({"id":"lifecycle-sessionEnd","name":"session","arguments":{},"reason":"other","end_trigger":"archive"}),&std::sync::atomic::AtomicBool::new(false)).unwrap();
    crate::ui_store::put(&fixture.db.lock().unwrap(),"session-lifecycle",CHAT,
        json!({"status":"closing","turn_id":turn,"hooks":hooks,"workspace":workspace,"archive":true,"error":null})).unwrap();
    let path=format!("/api/sessions/{CHAT}");
    assert_eq!(api.get(&path).json()["lifecycle_status"],"interrupted");
    let resumed=api.send("PATCH",&path,Some(TOKEN),r#"{"archived":true}"#);
    assert_eq!(resumed.status,200);assert_eq!(resumed.json()["lifecycle"]["turn_id"],turn);
    let finished=poll_session(&api,|v|v["lifecycle_status"]=="closed");assert_eq!(finished["archived"],true);
    assert_eq!(std::fs::read_to_string(workspace.join("close-count")).unwrap(),"x");
}
#[test]
fn archive_rejects_active_turns_and_close_cancel_stops_approval() {
    let fixture=Fixture::new();install_session_end_hook(&fixture);let api=Api::start(&fixture);
    let path=format!("/api/sessions/{CHAT}");
    fixture.db.lock().unwrap().execute("INSERT INTO native_chat_turns(chat_id,request_id,message,status) VALUES (?,?,?,'running')",[CHAT,"active","work"]).unwrap();
    assert_eq!(api.send("PATCH",&path,Some(TOKEN),r#"{"archived":true}"#).status,409);
    fixture.db.lock().unwrap().execute("UPDATE native_chat_turns SET status='completed' WHERE chat_id=? AND request_id='active'",[CHAT]).unwrap();
    assert_eq!(api.send("POST",&format!("{path}/close"),Some(TOKEN),"{}").status,200);
    poll_session(&api,|v|v["pending_approval"]["approval_id"].is_string());
    assert_eq!(api.send("POST",&format!("{path}/close/cancel"),Some(TOKEN),"{}").json()["cancelled"],true);
    poll_session(&api,|v|v["lifecycle_status"]=="failed");
    assert!(!api._workspace.path().join("close-count").exists());
}

#[test]
fn remote_session_without_active_end_hooks_can_archive() {
    let fixture=Fixture::new();install_session_end_hook(&fixture);
    crate::ui_plugin_components::set_hooks_enabled(&fixture.db,"fixture",false).unwrap();
    crate::ui_store::put(&fixture.db.lock().unwrap(),"remote-session",CHAT,json!({"active":true,"host_id":"remote"})).unwrap();
    let api=Api::start(&fixture);let path=format!("/api/sessions/{CHAT}");
    assert_eq!(api.send("PATCH",&path,Some(TOKEN),r#"{"archived":true}"#).status,200);
    assert_eq!(poll_session(&api,|v|v["lifecycle_status"]=="closed")["archived"],true);
    assert!(!api._workspace.path().join("close-count").exists());
}

#[test]
fn uncertain_session_end_is_not_replayed_after_cancellation() {
    let fixture=Fixture::new();install_session_end_hook(&fixture);
    {let conn=fixture.db.lock().unwrap();crate::ui_approvals::owned(&conn).unwrap().set_profile(CHAT,kageha_contracts::policy::PermissionProfile::Full).unwrap();}
    let commit="0123456789012345678901234567890123456789";
    let config=crate::ui_plugin_components::hook_definition("fixture",commit,&json!({"hooks":{"SessionEnd":[{"matcher":"other","hooks":[{"type":"command","command":"printf x >> close-count; sleep 10"}]}]}})).unwrap();
    {let conn=fixture.db.lock().unwrap();let mut installed=crate::ui_store::get(&conn,"plugin-installed","fixture").unwrap().unwrap();installed["hooks_sha256"]=config["sha256"].clone();
        crate::ui_store::put(&conn,"plugin-installed","fixture",installed).unwrap();crate::ui_store::put(&conn,"plugin-hooks","fixture",config).unwrap();}
    crate::ui_plugin_components::set_hooks_enabled(&fixture.db,"fixture",true).unwrap();
    let api=Api::start(&fixture);let path=format!("/api/sessions/{CHAT}/close");
    let first=api.send("POST",&path,Some(TOKEN),"{}");assert_eq!(first.status,200);let turn=first.json()["turn_id"].clone();
    let deadline=std::time::Instant::now()+Duration::from_secs(3);
    // Redirection creates the file before printf writes. Cancel only after the
    // effect whose replay the test measures is observable.
    let count = api._workspace.path().join("close-count");
    while std::fs::read(&count).unwrap_or_default() != b"x" {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(api.send("POST",&format!("{path}/cancel"),Some(TOKEN),"{}").json()["cancelled"],true);
    poll_session(&api,|v|v["lifecycle_status"]=="failed");
    let retry=api.send("POST",&path,Some(TOKEN),"{}");assert_eq!(retry.status,200);assert_eq!(retry.json()["turn_id"],turn);
    poll_session(&api,|v|v["lifecycle_status"]=="failed");
    assert_eq!(std::fs::read_to_string(api._workspace.path().join("close-count")).unwrap(),"x");
}
