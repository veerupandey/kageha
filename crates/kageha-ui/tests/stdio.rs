use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

#[test]
fn agent_presents_and_host_controls_a_panel() {
    let mut worker = Command::new(env!("CARGO_BIN_EXE_kageha-ui"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = worker.stdin.take().unwrap();
    let mut output = BufReader::new(worker.stdout.take().unwrap());
    let mut call = |request: Value| {
        writeln!(input, "{request}").unwrap();
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        serde_json::from_str::<Value>(&line).unwrap()
    };

    let initialized = call(json!({
        "source":"initialize", "version":1, "components":{"checklist":["item_selected"]}
    }));
    assert_eq!(initialized["state"]["revision"], 0);

    let presented = call(json!({
        "source":"agent", "command":{
            "method":"present", "revision":0, "component":"checklist", "props":{"title":"Review"}
        }
    }));
    assert_eq!(presented["state"]["view"]["props"]["title"], "Review");

    let event = call(json!({
        "source":"host", "command":{
            "method":"dispatch", "revision":1, "event":"item_selected", "payload":{"id":"draft"}
        }
    }));
    assert_eq!(event["event"]["payload"]["id"], "draft");
    assert_eq!(event["state"]["revision"], 2);

    let replay = call(json!({
        "source":"host", "command":{
            "method":"dispatch", "revision":1, "event":"item_selected", "payload":{"id":"draft"}
        }
    }));
    assert_eq!(replay["error"], "stale_revision");

    let taken = call(json!({
        "source":"host", "command":{"method":"set_owner", "revision":2, "owner":"user"}
    }));
    assert_eq!(taken["state"]["owner"], "user");
    let denied = call(json!({
        "source":"agent", "command":{"method":"close", "revision":3}
    }));
    assert_eq!(denied["error"], "user_has_control");
    assert_eq!(denied["state"]["view"]["component"], "checklist");

    let forged = call(json!({
        "source":"agent", "command":{"method":"set_owner", "revision":3, "owner":"agent"}
    }));
    assert_eq!(forged["error"], "invalid_request");

    call(json!({"source":"host", "command":{"method":"set_owner", "revision":3, "owner":"agent"}}));
    let unknown = call(json!({
        "source":"agent", "command":{"method":"present", "revision":4, "component":"arbitrary", "props":{}}
    }));
    assert_eq!(unknown["error"], "unknown_component");
    let closed = call(json!({"source":"agent", "command":{"method":"close", "revision":4}}));
    assert_eq!(closed["state"]["revision"], 5);
    assert!(closed["state"]["view"].is_null());

    drop(input);
    assert!(worker.wait().unwrap().success());
}
