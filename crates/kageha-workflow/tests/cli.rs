use serde_json::json;
use std::{
    io::Write,
    process::{Command, Stdio},
};
#[test]
fn ndjson_adapter_is_usable_without_linking_an_agent_framework() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_kageha-workflow"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let request = json!({"spec":{"version":1,"id":"run","namespace":"external-framework","resource":"tab","origin":"https://example.test","goals":{"review":"Review visible; do not submit"}},"previous":null,"observation":{"resource":"tab","origin":"https://example.test","progress_token":"review","satisfied":["review"],"pending":[],"blocker":null},"checkpoint":"candidate_final"});
    writeln!(child.stdin.take().unwrap(), "{request}").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let reply: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(reply["result"]["decision"], "verified");
    assert_eq!(reply["state"]["phase"], "verified");
}
