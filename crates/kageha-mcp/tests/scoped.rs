use kageha_contracts::execution::*;
use kageha_mcp::{Config, Pool};
use serde_json::{json, Value};
use std::{
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};
fn config() -> Config {
    serde_json::from_value(json!({"id":"fixture","command":std::env::current_exe().unwrap(),"args":["--ignored","--exact","fixture_loop","--nocapture"]})).unwrap()
}
fn scope(s: &str, t: &str) -> ToolContext {
    ToolContext {
        session: s.into(),
        turn: t.into(),
    }
}
fn call(
    pool: &Pool,
    config: &Config,
    scope: &ToolContext,
    id: &str,
    name: &str,
) -> std::io::Result<Value> {
    pool.exchange(
        config,
        scope,
        "tools/call",
        json!({"name":name,"arguments":{}}),
        Some(id),
        &Control {
            cancelled: &AtomicBool::new(false),
            deadline: None,
        },
    )
}
#[test]
fn protected_mail_requires_explicit_operator_configuration_and_does_not_journal_response() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.sqlite");
    let pool = Pool::open(&path).unwrap();
    let mut cfg = config();
    let cancel = AtomicBool::new(false);
    let control = Control { cancelled: &cancel, deadline: None };
    assert!(pool.verification_mail_read(&cfg, &scope("a", "1"), json!({}), &control).is_err());
    cfg.verification_mail_tool = Some("private_mail".into());
    assert!(cfg.validate().is_err());
    cfg.read_only_tools.push("private_mail".into());
    let response = pool.verification_mail_read(&cfg, &scope("a", "1"), json!({}), &control).unwrap();
    assert_eq!(response["structuredContent"]["messages"][0]["text"], "CODE-CANARY-482927");
    assert!(pool.receipts("a").unwrap().is_empty());
    for file in [path.clone(), path.with_extension("sqlite-wal")] {
        if file.exists() { assert!(!std::fs::read(file).unwrap().windows(18).any(|v| v == b"CODE-CANARY-482927")); }
    }
}
#[test]
fn persistent_connections_scoped_receipts_and_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.sqlite");
    let pool = Pool::open(&path).unwrap();
    let config = config();
    assert_eq!(
        call(&pool, &config, &scope("a", "1"), "one", "count").unwrap()["content"][0]["text"],
        "1"
    );
    assert_eq!(
        call(&pool, &config, &scope("a", "2"), "two", "count").unwrap()["content"][0]["text"],
        "2"
    );
    assert_eq!(
        call(&pool, &config, &scope("b", "1"), "one", "count").unwrap()["content"][0]["text"],
        "1"
    );
    assert_eq!(
        call(&pool, &config, &scope("a", "1"), "one", "count").unwrap()["content"][0]["text"],
        "1"
    );
    assert!(call(&pool, &config, &scope("a", "1"), "one", "failure").is_err());
    assert!(is_uncertain(
        &call(&pool, &config, &scope("a", "3"), "crash", "crash").unwrap_err()
    ));
    assert!(is_uncertain(
        &call(&pool, &config, &scope("a", "4"), "retry", "count").unwrap_err()
    ));
    assert_eq!(
        call(&pool, &config, &scope("b", "2"), "two", "count").unwrap()["content"][0]["text"],
        "2"
    );
    pool.release("a").unwrap();
    drop(pool);
    let pool = Pool::open(&path).unwrap();
    assert!(is_uncertain(
        &call(&pool, &config, &scope("a", "5"), "after-restart", "count").unwrap_err()
    ));
    let mut other = config.clone();
    other.id = "other".into();
    assert!(call(&pool, &other, &scope("a", "5"), "other", "count").is_ok());
    assert!(pool
        .receipts("a")
        .unwrap()
        .iter()
        .any(|r| r["state"] == "uncertain"));
    assert!(pool.receipts("unknown").unwrap().is_empty());
    pool.erase("a").unwrap();
    assert!(pool.receipts("a").unwrap().is_empty());
    assert!(call(&pool, &config, &scope("a", "6"), "erased", "count").is_err());
}
#[test]
fn deadline_leaves_uncertain_receipt_and_does_not_hang() {
    let dir = tempfile::tempdir().unwrap();
    let pool = Pool::open(&dir.path().join("mcp.sqlite")).unwrap();
    pool.exchange(
        &config(),
        &scope("a", "warmup"),
        "tools/list",
        json!({}),
        None,
        &Control {
            cancelled: &AtomicBool::new(false),
            deadline: None,
        },
    )
    .unwrap();
    let started = Instant::now();
    assert!(pool
        .exchange(
            &config(),
            &scope("a", "1"),
            "tools/call",
            json!({"name":"hold","arguments":{}}),
            Some("hold"),
            &Control {
                cancelled: &AtomicBool::new(false),
                deadline: Some(Instant::now() + Duration::from_millis(200))
            }
        )
        .is_err());
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(pool.receipts("a").unwrap()[0]["state"], "uncertain");
}
#[test]
fn acknowledged_negative_is_not_transport_uncertainty() {
    let dir = tempfile::tempdir().unwrap();
    let pool = Pool::open(&dir.path().join("mcp.sqlite")).unwrap();
    assert_eq!(
        call(&pool, &config(), &scope("a", "1"), "no", "failure").unwrap()["isError"],
        true
    );
    assert!(call(&pool, &config(), &scope("a", "2"), "yes", "count").is_ok());
}
#[test]
fn configuration_and_database_validation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.sqlite");
    let _pool = Pool::open(&path).unwrap();
    assert!(Pool::open(&path).is_err());
    let mut cfg = config();
    cfg.transport = "http".into();
    cfg.url = "http://127.0.0.1/".into();
    cfg.headers
        .insert("Authorization".into(), "Bearer secret".into());
    assert!(cfg.validate().is_err());
    cfg.url = "https://example.com".into();
    assert!(cfg.validate().is_ok());
    cfg.headers.insert("MCP-Session-Id".into(), "forged".into());
    assert!(cfg.validate().is_err());
}

#[path = "../examples/fixture.rs"]
mod fixture;
#[test]
#[ignore = "launched as an isolated stdio server by the adapter tests"]
fn fixture_loop() {
    fixture::main();
}
#[test]
fn cancellation_terminates_pending_io_and_preserves_intent() {
    let dir = tempfile::tempdir().unwrap();
    let pool = Pool::open(&dir.path().join("mcp.sqlite")).unwrap();
    pool.exchange(
        &config(),
        &scope("a", "warmup"),
        "tools/list",
        json!({}),
        None,
        &Control {
            cancelled: &AtomicBool::new(false),
            deadline: None,
        },
    )
    .unwrap();
    let cancel = AtomicBool::new(false);
    std::thread::scope(|threads| {
        threads.spawn(|| {
            while pool.receipts("a").unwrap().is_empty() {
                std::thread::sleep(Duration::from_millis(5));
            }
            cancel.store(true, std::sync::atomic::Ordering::Release);
        });
        let start = Instant::now();
        assert!(pool
            .exchange(
                &config(),
                &scope("a", "1"),
                "tools/call",
                json!({"name":"hold","arguments":{}}),
                Some("cancel"),
                &Control {
                    cancelled: &cancel,
                    deadline: Some(Instant::now() + Duration::from_secs(3))
                }
            )
            .is_err());
        assert!(start.elapsed() < Duration::from_secs(2));
    });
    assert_eq!(pool.receipts("a").unwrap()[0]["state"], "uncertain");
}

#[test]
fn adapter_catalog_authority_and_receipt_scope_are_not_model_selected() {
    use kageha_contracts::policy::ActionClass;
    use kageha_mcp::{tool_id, McpTools};
    use std::sync::Arc;
    let dir = tempfile::tempdir().unwrap();
    let pool = Arc::new(Pool::open(&dir.path().join("mcp.sqlite")).unwrap());
    let control = Control {
        cancelled: &AtomicBool::new(false),
        deadline: None,
    };
    let context = scope("chat", "turn");
    let adapter = McpTools::connect(pool.clone(), vec![config()], &control).unwrap();
    let mut call = ToolCall {
        id: "one".into(),
        name: tool_id("fixture", "count"),
        arguments: json!({}),
    };
    assert_eq!(
        adapter.binding(&context, &call).unwrap().class,
        ActionClass::Mutation
    );
    assert_eq!(
        adapter.dispatch_access_in(&context, &call).unwrap(),
        DispatchAccess::Resource("mcp:chat:fixture".into())
    );
    assert!(
        !adapter
            .execute_in(&context, &call, &control)
            .unwrap()
            .is_error
    );
    assert!(adapter.execute(&call, &control).is_err());
    call.name = "mcp_receipts".into();
    call.arguments = json!({"session":"other"});
    assert!(adapter.execute_in(&context, &call, &control).is_err());
    call.arguments = json!({});
    assert!(adapter
        .execute_in(&scope("other", "turn"), &call, &control)
        .unwrap()
        .content
        .contains("[]"));
    let mut readonly = config();
    readonly.read_only_tools = vec!["count".into()];
    let adapter = McpTools::connect(pool, vec![readonly], &control).unwrap();
    call.name = tool_id("fixture", "count");
    assert_eq!(
        adapter.binding(&context, &call).unwrap().class,
        ActionClass::ReadOnly
    );
}

#[test]
fn stdio_mcp_works_inside_the_process_sandbox() {
    if !kageha_sandbox::available(){return;}
    let dir=tempfile::tempdir().unwrap();let pool=Pool::open(&dir.path().join("mcp.sqlite")).unwrap();let mut config=config();config.sandbox=Some(kageha_sandbox::ProcessSandbox::default());
    assert_eq!(call(&pool,&config,&scope("sandbox-chat","turn"),"one","count").unwrap()["content"][0]["text"],"1");
    config.inherit_env=true;assert!(config.validate().is_err());
}
