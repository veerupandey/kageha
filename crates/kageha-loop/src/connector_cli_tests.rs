//! End-to-end test of the `kageha` CLI shim (spec autonomous-agent-loop, task 11.2;
//! Requirement 1.4): a sandboxed shell session runs the shim installed on its `PATH`
//! against the real local connector API of a turn, with Gmail not connected.
use crate::{ui_connector_api, webui};
use kageha_workspace::shell::{cli_shim, ShellPaths, ShellSession};
use serde_json::{json, Value};
use std::{
    sync::{atomic::AtomicBool, Arc},
    time::Duration,
};

const CHAT: &str = "chatcli00001";

#[test]
fn the_kageha_shim_reads_through_the_connector_api_from_a_sandboxed_shell() {
    if !kageha_workspace::shell::sandbox_available() {
        eprintln!("skipping: OS process sandbox unavailable");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let connection = rusqlite::Connection::open(directory.path().join("native.sqlite")).unwrap();
    connection.execute_batch("CREATE TABLE native_chat_turns(sequence INTEGER PRIMARY KEY,chat_id TEXT,request_id TEXT,message TEXT,answer TEXT,status TEXT);").unwrap();
    webui::initialize(&connection).unwrap();
    connection.execute("INSERT INTO ui_sessions(id,title,created_at,updated_at) VALUES (?,?,0,0)", [CHAT, "CLI"]).unwrap();
    crate::ui_approvals::main_default(&connection, CHAT, "main").unwrap();
    let db = crate::agent_host::DatabaseState::open(connection).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let workspace = crate::ui_workspace::open_for_chat(&db, CHAT).unwrap();
    let root = workspace.root().to_path_buf();
    let mut api = ui_connector_api::start(workspace, ui_connector_api::Scope {
        db: db.clone(), chat: CHAT.into(), turn: format!("{CHAT}/request-1"), persona: json!({"id":"main"}), cancel: cancel.clone(),
    }).unwrap();
    let token = std::fs::read_to_string(root.join(kageha_workspace::task::API_TOKEN)).unwrap();
    let paths = ShellPaths::prepare(&root, &directory.path().join("toolchains")).unwrap();
    let mut shell = ShellSession::open(paths.clone()).unwrap();
    let t = Duration::from_secs(60);
    let read = |args: Value| -> Value {
        serde_json::from_str(&crate::ui_tools::integration_read(&db, "main", &args, &cancel).unwrap()).unwrap()
    };

    let which = shell.run("command -v kageha", "c", t).unwrap();
    assert_eq!(which.stdout.trim_end(), cli_shim::shim_path(&paths.toolchain_cache).to_str().unwrap());

    // Disconnected Gmail: the same envelope `integration_read` returns, exit 0, pretty-printed.
    let expected = read(json!({"integration":"gmail","operation":"status"}));
    assert_eq!(expected["status"], "disconnected");
    let status = shell.run("kageha gmail status", "c", t).unwrap();
    assert_eq!(status.exit_code, 0, "{status:?}");
    assert_eq!(serde_json::from_str::<Value>(&status.stdout).unwrap(), expected);
    assert!(status.stdout.contains("\n  \""), "pretty by default: {}", status.stdout);
    assert!(status.stderr.contains("gmail is not connected (status: disconnected)"), "{}", status.stderr);

    // `--json` is one compact line, usable from scripts.
    let expected = read(json!({"integration":"gmail","operation":"search","query":"Seequent interview"}));
    let search = shell.run(r#"kageha gmail search "Seequent interview" --json"#, "c", t).unwrap();
    assert_eq!(search.exit_code, 0, "{search:?}");
    assert_eq!(search.stdout.lines().count(), 1);
    assert_eq!(serde_json::from_str::<Value>(&search.stdout).unwrap(), expected);
    let thread = shell.run("kageha gmail thread t1 --json | wc -l", "c", t).unwrap();
    assert_eq!(thread.stdout.trim(), "1");

    // A read the API fails: exit 1 and `kageha: <code>: <message>` on stderr.
    let error = crate::ui_tools::integration_read(&db, "main", &json!({"integration":"github","path":"repos/o/r/issues"}), &cancel)
        .unwrap_err()
        .to_string();
    let failed = shell.run("kageha github path repos/o/r/issues", "c", t).unwrap();
    assert_eq!(failed.exit_code, 1, "{failed:?}");
    assert!(failed.stdout.is_empty());
    assert_eq!(failed.stderr, format!("kageha: read_failed: {error}\n"));
    // Write operations never reach the API.
    let send = shell.run("kageha gmail send m1", "c", t).unwrap();
    assert_eq!(send.exit_code, 2);

    for output in [&status, &search, &thread, &failed, &send] {
        assert!(!output.stdout.contains(&token) && !output.stderr.contains(&token), "the token is never printed");
    }

    // After the turn ends the shim reports that the API is gone.
    api.stop();
    let gone = shell.run("kageha gmail status", "c", t).unwrap();
    assert_eq!(gone.exit_code, 3, "{gone:?}");
    assert!(gone.stderr.starts_with("kageha: api_unavailable: The local connector API is not running at "), "{}", gone.stderr);
}
