//! Real protocol-2 executable smoke: read-only, no input or permission prompt.
#![cfg(unix)]
use kageha_computer::client::ComputerClient;
use serde_json::json;
use std::{fs, os::unix::fs::DirBuilderExt, path::Path};

static LIVE_NATIVE: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn real_helper_handshake_permissions_and_reconnect_receipt_lookup() {
    let _native = LIVE_NATIVE.lock().unwrap();
    let root = std::env::temp_dir().join(format!(
        "kg-cli-client-{}",
        kageha_computer::instance_nonce()
    ));
    fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(root.clone());
    let journal = root.join("journal.sqlite");
    let binary = Path::new(env!("CARGO_BIN_EXE_kageha-computer"));
    let mut first = ComputerClient::launch(binary, &journal, "integration", "agent").unwrap();
    let epoch = first.service_epoch().to_owned();
    assert_eq!(first.info()["protocol"], 2);
    assert_eq!(first.info()["input_authorized"], false);
    let permissions = first
        .call("permissions", json!({}), "check-permissions")
        .unwrap();
    // Non-macOS builds report an unavailable adapter; neither path prompts.
    #[cfg(all(target_os = "macos", feature = "native-macos"))]
    {
        assert_eq!(permissions["status"], "ok");
        assert!(permissions["result"]["accessibility"].is_boolean());
        assert!(permissions["result"]["screen_recording"].is_boolean());
    }
    #[cfg(not(all(target_os = "macos", feature = "native-macos")))]
    assert_eq!(permissions["error"]["code"], "observation_failed");
    let missing = first
        .call(
            "receipt",
            json!({"request_id":"never-dispatched"}),
            "lookup-1",
        )
        .unwrap();
    assert_eq!(missing["error"]["code"], "receipt_not_found");
    first.close();
    let mut second = ComputerClient::launch(binary, &journal, "integration", "agent").unwrap();
    assert_ne!(second.service_epoch(), epoch);
    let missing = second
        .call(
            "receipt",
            json!({"request_id":"never-dispatched"}),
            "lookup-2",
        )
        .unwrap();
    assert_eq!(missing["error"]["code"], "receipt_not_found");
    // No input operation was admitted, so inspection cannot create a receipt.
    second.close();
}

#[test]
fn real_mcp_helper_lifecycle_has_protocol_only_stdout() {
    use std::{
        io::{BufRead, BufReader, Write},
        process::{Command, Stdio},
    };
    let _native = LIVE_NATIVE.lock().unwrap();
    let root =
        std::env::temp_dir().join(format!("kg-real-mcp-{}", kageha_computer::instance_nonce()));
    fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
    struct Cleanup {
        child: std::process::Child,
        root: std::path::PathBuf,
    }
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    let child = Command::new(env!("CARGO_BIN_EXE_kageha-computer"))
        .args(["control", "--journal"])
        .arg(root.join("journal.sqlite"))
        .args([
            "--mcp",
            "--workspace",
            "mcp-workspace",
            "--client",
            "mcp-agent",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut cleanup = Cleanup { child, root };
    let mut input = cleanup.child.stdin.take().unwrap();
    let mut output = BufReader::new(cleanup.child.stdout.take().unwrap());
    writeln!(input, "{}", json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"rust-integration","version":"1"}}})).unwrap();
    input.flush().unwrap();
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    let initialized: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(initialized["result"]["protocolVersion"], "2024-11-05");
    writeln!(
        input,
        "{}",
        json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    )
    .unwrap();
    writeln!(input, "{}", json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"computer_info","arguments":{}}})).unwrap();
    input.flush().unwrap();
    line.clear();
    output.read_line(&mut line).unwrap();
    let reply: serde_json::Value = serde_json::from_str(&line).unwrap();
    let info: serde_json::Value =
        serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(info["protocol"], 2);
    assert_eq!(info["input_authorized"], false);
    writeln!(input, "{}", json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"computer_call","arguments":{"request_id":"permissions","service_epoch":info["service_epoch"],"operation":"permissions","args":{}}}})).unwrap();
    input.flush().unwrap();
    line.clear();
    output.read_line(&mut line).unwrap();
    let reply: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(reply["id"], 3);
    assert!(reply["result"]["content"][0]["text"].is_string());
    drop(input);
    assert!(cleanup.child.wait().unwrap().success());
}
