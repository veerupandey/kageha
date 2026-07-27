use kageha_contracts::execution::*;
use kageha_execution::{journal::Journal, remote::Config, Tools};
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    sync::{atomic::AtomicBool, Arc},
    time::{Duration, Instant},
};
fn config(root: &std::path::Path) -> Config {
    let fixture = root.join("ssh");
    fs::write(&fixture,"#!/bin/sh\nexport PATH=/usr/bin:/bin\nfor arg do script=\"$arg\"; done\n/bin/sh -c \"$script\"\n").unwrap();
    fs::set_permissions(&fixture, fs::Permissions::from_mode(0o700)).unwrap();
    Config {
        id: "test".into(),
        host: "fixture.example".into(),
        user: "user".into(),
        port: 22,
        identity: String::new(),
        known_hosts: String::new(),
        workdir: root.join("remote").to_str().unwrap().into(),
        ssh_program: Some(fixture),
        accept_new_host_keys: false,
        sandbox: None,
    }
}
fn context(turn: &str) -> ToolContext {
    ToolContext {
        session: "chat".into(),
        turn: turn.into(),
    }
}
fn call(id: &str, name: &str, args: Value) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: args,
    }
}
fn run(tools: &Tools, turn: &str, call: &ToolCall) -> std::io::Result<ToolReply> {
    tools.execute_in(
        &context(turn),
        call,
        &Control {
            cancelled: &AtomicBool::new(false),
            deadline: Some(Instant::now() + Duration::from_secs(3)),
        },
    )
}
#[test]
fn independent_exec_upload_read_and_exact_receipt_reuse() {
    let root = tempfile::tempdir().unwrap();
    let cfg = config(root.path());
    fs::create_dir(root.path().join("local")).unwrap();
    fs::write(root.path().join("local/source.txt"), "exact upload").unwrap();
    let journal = Arc::new(Journal::open(&root.path().join("journal.sqlite")).unwrap());
    let tools = Tools::workspace(cfg, &root.path().join("local"), journal.clone()).unwrap();
    let write = call(
        "write",
        "remote_exec",
        json!({"command":"printf x >> count; printf proof"}),
    );
    assert!(run(&tools, "one", &write)
        .unwrap()
        .content
        .contains("proof"));
    run(&tools, "one", &write).unwrap();
    assert_eq!(fs::read(root.path().join("remote/count")).unwrap(), b"x");
    let failed = run(
        &tools,
        "two",
        &call("fail", "remote_exec", json!({"command":"exit 7"})),
    )
    .unwrap();
    assert!(failed.is_error);
    assert_eq!(serde_json::from_str::<Value>(&failed.content).unwrap()["exit_code"], 7);
    assert!(
        !run(
            &tools,
            "three",
            &call(
                "upload",
                "remote_upload",
                json!({"path":"source.txt","destination":"nested/space ' file.txt"})
            )
        )
        .unwrap()
        .is_error
    );
    assert_eq!(
        run(
            &tools,
            "four",
            &call(
                "read",
                "read_file",
                json!({"path":"nested/space ' file.txt"})
            )
        )
        .unwrap()
        .content,
        "exact upload"
    );
    assert!(run(
        &tools,
        "five",
        &call(
            "bad",
            "remote_upload",
            json!({"path":"source.txt","destination":"../escape"})
        )
    )
    .is_err());
    assert!(run(
        &tools,
        "five",
        &call(
            "forge",
            "remote_exec",
            json!({"command":"true","host":"other"})
        )
    )
    .is_err());
    assert_eq!(journal.receipts(Some("other")).unwrap().len(), 0);
}
#[test]
fn approved_upload_cannot_change_its_bytes_or_destination() {
    let root = tempfile::tempdir().unwrap();
    let cfg = config(root.path());
    fs::create_dir(root.path().join("local")).unwrap();
    fs::write(root.path().join("local/source"), "before").unwrap();
    let journal = Arc::new(Journal::open(&root.path().join("journal.sqlite")).unwrap());
    let tools = Tools::workspace(cfg, &root.path().join("local"), journal.clone()).unwrap();
    let call = call(
        "upload",
        "remote_upload",
        json!({"path":"source","destination":"proof"}),
    );
    let binding = tools.binding(&context("one"), &call).unwrap();
    fs::write(root.path().join("local/source"), "after").unwrap();
    assert!(tools
        .execute_authorized_in(
            &context("one"),
            &call,
            &binding,
            &Control {
                cancelled: &AtomicBool::new(false),
                deadline: None
            }
        )
        .is_err());
    assert!(journal.receipts(None).unwrap().is_empty());
}
#[test]
fn uncertain_command_fences_account_across_labels_and_restart_without_replay() {
    let root = tempfile::tempdir().unwrap();
    let mut cfg = config(root.path());
    fs::create_dir(root.path().join("local")).unwrap();
    let path = root.path().join("journal.sqlite");
    let journal = Arc::new(Journal::open(&path).unwrap());
    let tools = Tools::workspace(cfg.clone(), &root.path().join("local"), journal.clone()).unwrap();
    let command = call(
        "lost",
        "remote_exec",
        json!({"command":"printf x >> counter; exit 255"}),
    );
    assert!(is_uncertain(&run(&tools, "one", &command).unwrap_err()));
    cfg.id = "renamed".into();
    cfg.workdir = root.path().join("other-directory").to_str().unwrap().into();
    let other = Tools::workspace(cfg.clone(), &root.path().join("local"), journal.clone()).unwrap();
    assert!(is_uncertain(
        &run(
            &other,
            "two",
            &call("fresh", "remote_exec", json!({"command":"touch forbidden"}))
        )
        .unwrap_err()
    ));
    assert!(!root.path().join("other-directory/forbidden").exists());
    assert_eq!(
        run(
            &tools,
            "read",
            &call("read", "read_file", json!({"path":"counter"}))
        )
        .unwrap()
        .content,
        "x"
    );
    drop(other);
    drop(tools);
    drop(journal);
    let journal = Arc::new(Journal::open(&path).unwrap());
    let receipt = journal.receipts(None).unwrap().remove(0);
    assert_eq!(receipt["state"], "uncertain");
    journal
        .acknowledge(
            receipt["receipt"].as_str().unwrap(),
            receipt["target"].as_str().unwrap(),
        )
        .unwrap();
    let tools = Tools::workspace(cfg, &root.path().join("local"), journal.clone()).unwrap();
    run(
        &tools,
        "three",
        &call("next", "remote_exec", json!({"command":"touch allowed"})),
    )
    .unwrap();
    assert!(root.path().join("other-directory/allowed").exists());
    assert_eq!(fs::read(root.path().join("remote/counter")).unwrap(), b"x");
    journal.erase("chat").unwrap();
    assert!(journal.receipts(Some("chat")).unwrap().is_empty());
    assert!(run(
        &tools,
        "erased-read",
        &call("read", "read_file", json!({"path":"counter"}))
    )
    .is_err());
    assert!(run(
        &tools,
        "four",
        &call("erased", "remote_exec", json!({"command":"touch erased"}))
    )
    .is_err());
}
#[test]
fn cancellation_preserves_unknown_intent() {
    let root = tempfile::tempdir().unwrap();
    let cfg = config(root.path());
    let journal = Arc::new(Journal::open(&root.path().join("journal.sqlite")).unwrap());
    let tools = Tools::workspace(cfg, root.path(), journal.clone()).unwrap();
    let start = Instant::now();
    let result = tools.execute_in(
        &context("one"),
        &call("sleep", "remote_exec", json!({"command":"sleep 30"})),
        &Control {
            cancelled: &AtomicBool::new(false),
            deadline: Some(Instant::now() + Duration::from_millis(100)),
        },
    );
    assert!(result.is_err());
    assert!(start.elapsed() < Duration::from_secs(2));
    assert_eq!(journal.receipts(None).unwrap()[0]["state"], "uncertain");
}
#[test]
fn changed_timeout_or_target_requires_new_authority() {
    let root = tempfile::tempdir().unwrap();
    let cfg = config(root.path());
    let journal = Arc::new(Journal::open(&root.path().join("journal.sqlite")).unwrap());
    let tools = Tools::workspace(cfg, root.path(), journal.clone()).unwrap();
    let mut call = call(
        "one",
        "remote_exec",
        json!({"command":"touch forbidden","timeout_s":1}),
    );
    let authority = tools.binding(&context("turn"), &call).unwrap();
    call.arguments["timeout_s"] = json!(120);
    assert!(tools
        .execute_authorized_in(
            &context("turn"),
            &call,
            &authority,
            &Control {
                cancelled: &AtomicBool::new(false),
                deadline: None
            }
        )
        .is_err());
    assert!(journal.receipts(None).unwrap().is_empty());
}
#[test]
fn erasing_history_does_not_clear_an_unverified_target() {
    let root = tempfile::tempdir().unwrap();
    let cfg = config(root.path());
    let journal = Arc::new(Journal::open(&root.path().join("journal.sqlite")).unwrap());
    let tools = Tools::workspace(cfg, root.path(), journal.clone()).unwrap();
    assert!(run(
        &tools,
        "one",
        &call("lost", "remote_exec", json!({"command":"exit 255"}))
    )
    .is_err());
    journal.erase("chat").unwrap();
    let scope = ToolContext {
        session: "other".into(),
        turn: "other".into(),
    };
    assert!(is_uncertain(
        &tools
            .execute_in(
                &scope,
                &call("next", "remote_exec", json!({"command":"touch forbidden"})),
                &Control {
                    cancelled: &AtomicBool::new(false),
                    deadline: None
                }
            )
            .unwrap_err()
    ));
}
#[test]
fn deleted_session_cannot_resurrect_output_or_clear_an_active_blocker() {
    use std::sync::{atomic::Ordering, mpsc};
    let root = tempfile::tempdir().unwrap();
    let journal = Arc::new(Journal::open(&root.path().join("journal.sqlite")).unwrap());
    let target = kageha_execution::hash(b"target");
    let digest = kageha_execution::hash(b"input");
    let release = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    let running = {
        let journal = journal.clone();
        let target = target.clone();
        let digest = digest.clone();
        let release = release.clone();
        std::thread::spawn(move || {
            journal.run(
                &context("turn"),
                "active",
                &target,
                &digest,
                &Control {
                    cancelled: &AtomicBool::new(false),
                    deadline: None,
                },
                || {
                    tx.send(()).unwrap();
                    while !release.load(Ordering::Acquire) {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Ok(ToolReply::success("PRIVATE OUTPUT".into()))
                },
            )
        })
    };
    rx.recv().unwrap();
    let receipt = journal.receipts(None).unwrap().remove(0);
    assert!(journal
        .acknowledge(receipt["receipt"].as_str().unwrap(), &target)
        .is_err());
    journal.erase("chat").unwrap();
    release.store(true, Ordering::Release);
    assert!(is_uncertain(&running.join().unwrap().unwrap_err()));
    assert!(journal.receipts(Some("chat")).unwrap().is_empty());
    assert!(!serde_json::to_string(&journal.receipts(None).unwrap())
        .unwrap()
        .contains("PRIVATE OUTPUT"));
    assert_eq!(journal.receipts(None).unwrap()[0]["state"], "uncertain");
}
#[test]
fn completed_receipt_lookup_checks_exact_scope_without_dispatch_or_unknown_replay() {
    let root=tempfile::tempdir().unwrap();let journal=Journal::open(&root.path().join("journal.sqlite")).unwrap();
    let target="a".repeat(64);let digest="b".repeat(64);let ctl=Control{cancelled:&AtomicBool::new(false),deadline:None};let context=context("lookup");
    assert!(journal.completed(&context,"call",&target,&digest).unwrap().is_none());
    journal.run(&context,"call",&target,&digest,&ctl,||Ok(ToolReply::success("verified".into()))).unwrap();
    assert_eq!(journal.completed(&context,"call",&target,&digest).unwrap().unwrap().content,"verified");
    assert!(journal.completed(&context,"call",&target,&"c".repeat(64)).is_err());
    assert!(journal.run(&context,"unknown",&target,&digest,&ctl,||Err(std::io::Error::other("transport interrupted"))).is_err());
    assert!(is_uncertain(&journal.completed(&context,"unknown",&target,&digest).unwrap_err()));
    let receipt=journal.receipts(Some("chat")).unwrap().into_iter().find(|v|v["call"]=="unknown").unwrap();journal.acknowledge(receipt["receipt"].as_str().unwrap(),&target).unwrap();
    assert!(is_uncertain(&journal.completed(&context,"unknown",&target,&digest).unwrap_err()));
}
#[test]
fn an_expired_child_deadline_after_dispatch_keeps_a_typed_unknown_outcome() {
    let root=tempfile::tempdir().unwrap();let journal=Journal::open(&root.path().join("journal.sqlite")).unwrap();let context=context("deadline");let target="a".repeat(64);let digest="b".repeat(64);
    let control=Control{cancelled:&AtomicBool::new(false),deadline:Some(Instant::now()+Duration::from_millis(200))};
    let error=journal.run(&context,"effect",&target,&digest,&control,||{std::thread::sleep(Duration::from_millis(250));Err(std::io::Error::new(std::io::ErrorKind::TimedOut,"command interrupted after dispatch"))}).unwrap_err();
    assert!(is_uncertain(&error));assert_eq!(journal.receipts(Some("chat")).unwrap()[0]["state"],"uncertain");
}
