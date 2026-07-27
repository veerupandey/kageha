use kageha_contracts::execution::*;
use kageha_tools::browser::{BrowserTools, Config};
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::{fs::PermissionsExt, net::UnixListener},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
struct Fixture {
    dir: tempfile::TempDir,
    stop: Arc<AtomicBool>,
    changed: Arc<AtomicBool>,
    drop_fill: Arc<AtomicBool>,
    calls: Arc<Mutex<Vec<Value>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let root = dir.path().canonicalize().unwrap();
        for(name,body) in [("auth.token","0".repeat(32)),("daemon.json",json!({"protocol_version":1,"state_root":root,"instance_id":"fixture","application_id":"host","service_epoch":"epoch"}).to_string())]{std::fs::write(root.join(name),body).unwrap();std::fs::set_permissions(root.join(name),std::fs::Permissions::from_mode(0o600)).unwrap();}
        let listener = UnixListener::bind(root.join("control.sock")).unwrap();
        std::fs::set_permissions(
            root.join("control.sock"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let changed = Arc::new(AtomicBool::new(false));
        let drop_fill = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(Mutex::new(vec![]));
        let (s, c, d, log) = (
            stop.clone(),
            changed.clone(),
            drop_fill.clone(),
            calls.clone(),
        );
        let thread = std::thread::spawn(move || {
            let mut workers = vec![];
            while !s.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let (s, c, d, log) = (s.clone(), c.clone(), d.clone(), log.clone());
                        workers.push(std::thread::spawn(move||{stream.set_read_timeout(Some(Duration::from_millis(50))).unwrap();let mut stream=BufReader::new(stream);while !s.load(Ordering::Acquire){let mut line=String::new();match stream.read_line(&mut line){Ok(0)=>break,Err(e)if matches!(e.kind(),std::io::ErrorKind::WouldBlock|std::io::ErrorKind::TimedOut)=>continue,Err(_)=>break,_=>{}}let r:Value=serde_json::from_str(&line).unwrap();log.lock().unwrap().push(r.clone());let method=r["method"].as_str().unwrap();if method=="fill_observed"&&d.load(Ordering::Acquire){break;} if method=="stall" {let _=stream.get_mut().write_all(b"{");while !s.load(Ordering::Acquire){std::thread::sleep(Duration::from_millis(5));}break;}
   let result=match method{"handshake"=>json!({"action_backend":"standalone"}),"browser_selection"=>json!({"binding_id":if c.load(Ordering::Acquire){"new"}else{"original"},"browser_id":"browser","target_id":"tab","profile":"private","tabs":[{"target_id":"tab","url":"https://fixture.test/form"}]}),"observe_tab"=>json!({"observation_id":"observed","controls":[{"ref":"e1","role":"textbox"}]}),"claim_control"=>json!({"authority":{"generation":1,"grant_id":"grant"}}),_=>json!({"verified":true})};
   let mut reply=r.clone();reply["status"]=json!("ok");reply["service_epoch"]=json!("epoch");reply["result"]=result;reply["execution_phase"]=json!("not_dispatched");let mut bytes=serde_json::to_vec(&reply).unwrap();bytes.push(b'\n');if stream.get_mut().write_all(&bytes).is_err(){break;}
  }}));
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(2)),
                }
            }
            for worker in workers {
                worker.join().unwrap();
            }
        });
        Self {
            dir,
            stop,
            changed,
            drop_fill,
            calls,
            thread: Some(thread),
        }
    }
    fn tools(&self) -> BrowserTools {
        BrowserTools::new(Config {
            state_dir: self.dir.path().into(),
        })
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.thread.take().unwrap().join().unwrap();
    }
}
fn context() -> ToolContext {
    ToolContext {
        session: "chat".into(),
        turn: "turn".into(),
    }
}

#[test]
fn native_select_normalizes_integer_options_without_changing_strings() {
    for (option, expected) in [
        (json!(39), json!("39")),
        (json!("39"), json!("39")),
        (json!("039"), json!("039")),
        (json!([39, "004"]), json!(["39", "004"])),
    ] {
        let fixture = Fixture::new();
        let tools = fixture.tools();
        let selection = call("browser_select", json!({"target":"e1", "observation_id":"observed", "option":option}));
        let authority = tools.binding(&context(), &selection).unwrap();
        let cancelled = AtomicBool::new(false);
        let control = Control { cancelled: &cancelled, deadline: Some(Instant::now() + Duration::from_secs(2)) };
        assert!(!tools.execute_authorized_in(&context(), &selection, &authority, &control).unwrap().is_error);
        let calls = fixture.calls.lock().unwrap();
        let sent = calls.iter().find(|request| request["method"] == "select_observed").unwrap();
        assert_eq!(sent["params"]["option"], expected);
    }
    for option in [json!(-1), json!(1.5), json!(9_007_199_254_740_992u64), json!(["39", 39]), json!(true)] {
        let result = kageha_tools::browser::observed_action("browser_select", &json!({"option":option}),
            &json!({"browser_id":"b", "target_id":"t"}), "fresh", "e1", None);
        assert!(result.is_err());
    }
}
fn call(name: &str, args: Value) -> ToolCall {
    ToolCall {
        id: "attempt".into(),
        name: name.into(),
        arguments: args,
    }
}
#[test]
fn scoped_reads_and_exact_mutation_authority() {
    let f = Fixture::new();
    let tools = f.tools();
    let c = context();
    let cancel = AtomicBool::new(false);
    let control = Control {
        cancelled: &cancel,
        deadline: Some(Instant::now() + Duration::from_secs(2)),
    };
    let read = call("browser_snapshot", json!({}));
    assert!(tools.execute(&read, &control).is_err());
    assert!(!tools.execute_in(&c, &read, &control).unwrap().is_error);
    let fill = call(
        "browser_fill",
        json!({"target":"e1","observation_id":"observed","text":"test"}),
    );
    assert!(tools.execute_in(&c, &fill, &control).is_err());
    assert!(!f
        .calls
        .lock()
        .unwrap()
        .iter()
        .any(|v| v["method"] == "fill_observed"));
    let authority = tools.binding(&c, &fill).unwrap();
    let reply = tools
        .execute_authorized_in(&c, &fill, &authority, &control)
        .unwrap();
    assert!(!reply.is_error);
    let calls = f.calls.lock().unwrap();
    let input = calls
        .iter()
        .find(|v| v["method"] == "fill_observed")
        .unwrap();
    assert_eq!(input["authority"]["grant_id"], "grant");
    assert!(input["workspace_id"]
        .as_str()
        .unwrap()
        .starts_with("runtime-"));
    assert!(calls.iter().any(|v| v["method"] == "release_control"));
}
#[test]
fn changed_selection_cannot_use_pending_approval() {
    let f = Fixture::new();
    let tools = f.tools();
    let c = context();
    let fill = call(
        "browser_fill",
        json!({"target":"e1","observation_id":"observed","text":"test"}),
    );
    let authority = tools.binding(&c, &fill).unwrap();
    f.changed.store(true, Ordering::Release);
    assert!(tools
        .execute_authorized_in(
            &c,
            &fill,
            &authority,
            &Control {
                cancelled: &AtomicBool::new(false),
                deadline: None
            }
        )
        .is_err());
    assert!(!f
        .calls
        .lock()
        .unwrap()
        .iter()
        .any(|v| v["method"] == "claim_control"));
}
#[test]
fn dropped_mutation_is_uncertain_never_replayed() {
    let f = Fixture::new();
    let tools = f.tools();
    let c = context();
    let fill = call(
        "browser_fill",
        json!({"target":"e1","observation_id":"observed","text":"test"}),
    );
    let authority = tools.binding(&c, &fill).unwrap();
    f.drop_fill.store(true, Ordering::Release);
    let error = tools
        .execute_authorized_in(
            &c,
            &fill,
            &authority,
            &Control {
                cancelled: &AtomicBool::new(false),
                deadline: Some(Instant::now() + Duration::from_secs(1)),
            },
        )
        .unwrap_err();
    assert!(is_uncertain(&error));
    assert_eq!(
        f.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|v| v["method"] == "fill_observed")
            .count(),
        1
    );
}
#[test]
fn model_cannot_inject_transport_authority_or_filesystem_paths() {
    let f = Fixture::new();
    let tools = f.tools();
    for call in [
        call("browser_open", json!({"url":"file:///etc/passwd"})),
        call(
            "browser_fill",
            json!({"target":"e1","observation_id":"observed","text":"test","authority":{"grant_id":"forged"}}),
        ),
        call("browser_upload", json!({"path":"/etc/passwd"})),
    ] {
        assert!(tools.binding(&context(), &call).is_err());
    }
    assert!(f.calls.lock().unwrap().is_empty());
}
#[test]
fn controlled_transport_can_cancel_a_partial_reply_and_honor_deadline() {
    for cancel_early in [true, false] {
        let f = Fixture::new();
        let mut client = kageha_browser_client::Client::connect_to(
            "cancel-fixture",
            "agent",
            f.dir.path().into(),
            false,
        )
        .unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let signal = std::thread::spawn(move || {
            if cancel_early {
                std::thread::sleep(Duration::from_millis(60));
                flag.store(true, Ordering::Release);
            }
        });
        let start = Instant::now();
        let error = client
            .call_controlled(
                "stall",
                json!({}),
                None,
                "partial-reply",
                &Control {
                    cancelled: cancelled.as_ref(),
                    deadline: Some(start + Duration::from_millis(200)),
                },
            )
            .unwrap_err();
        signal.join().unwrap();
        assert!(start.elapsed() < Duration::from_secs(1));
        assert_eq!(
            error.kind(),
            if cancel_early {
                std::io::ErrorKind::Interrupted
            } else {
                std::io::ErrorKind::TimedOut
            }
        );
    }
}
#[test]
fn authorization_pins_arguments_and_native_key_projection_is_strict() {
    let f = Fixture::new();
    let tools = f.tools();
    let mut fill = call(
        "browser_fill",
        json!({"target":"e1","observation_id":"observed","text":"approved"}),
    );
    let authority = tools.binding(&context(), &fill).unwrap();
    assert!(authority.generation <= 9_007_199_254_740_991);
    fill.arguments["text"] = json!("changed");
    assert!(tools
        .execute_authorized_in(
            &context(),
            &fill,
            &authority,
            &Control {
                cancelled: &AtomicBool::new(false),
                deadline: None
            }
        )
        .is_err());
    assert!(!f
        .calls
        .lock()
        .unwrap()
        .iter()
        .any(|v| v["method"] == "claim_control"));
    let (method, params) = kageha_tools::browser::observed_action(
        "browser_press",
        &json!({"key":"Tab"}),
        &json!({"browser_id":"b","target_id":"t"}),
        "fresh",
        "e1",
        Some("frame"),
    )
    .unwrap();
    assert_eq!(method, "press_observed");
    assert!(params.get("target").is_none());
    assert_eq!(params["frame_id"], "frame");
}

#[test]
fn text_only_click_expectation_is_rejected_before_binding() {
    let fixture = Fixture::new();
    let tools = fixture.tools();
    let click = call(
        "browser_click",
        json!({"target":"e1","observation_id":"observed","expected_text":"Review application"}),
    );
    let error = tools.binding(&context(), &click).unwrap_err();
    assert!(error.to_string().contains("expected_text requires expected_selector"));
    assert!(fixture.calls.lock().unwrap().is_empty());
}
