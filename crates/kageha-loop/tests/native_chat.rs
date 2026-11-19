#![cfg(unix)]

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::Duration;

#[test]
fn completes_a_tool_turn_without_python() {
    let root = std::env::temp_dir().join(format!("kageha-native-chat-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let result = std::panic::catch_unwind(|| {
        let bin = root.join("bin");
        let workspace = root.join("workspace");
        std::fs::create_dir(&bin).unwrap();
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(workspace.join("fixture.txt"), "workspace fixture").unwrap();
        std::fs::write(root.join("private.txt"), "outside workspace").unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let provider = std::thread::spawn(move || {
            for step in 0..2 {
                let (mut socket, _) = listener.accept().unwrap();
                socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut reader = BufReader::new(&mut socket);
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let payload: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(payload["model"], "fixture");
                let message = if step == 0 {
                    assert_eq!(payload["messages"][0]["content"], "Read both files.");
                    json!({"role":"assistant","content":null,"tool_calls":[
                        {"id":"ok","function":{"name":"read_file","arguments":"{\"path\":\"fixture.txt\"}"}},
                        {"id":"blocked","function":{"name":"read_file","arguments":"{\"path\":\"../private.txt\"}"}}
                    ]})
                } else {
                    assert_eq!(payload["messages"][2]["content"], "workspace fixture");
                    assert_eq!(payload["messages"][3]["content"],
                               "Read failed: expected a file inside the workspace");
                    json!({"role":"assistant","content":"Fixture read."})
                };
                let body = json!({"choices":[{"message":message}]}).to_string();
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                       body.len()).unwrap();
            }
        });
        let mut child = Command::new(env!("CARGO_BIN_EXE_kageha-loop"))
            .arg("chat").arg("--offline").arg(workspace)
            .env_clear()
            .env("PATH", bin)
            .env("KAGEHA_PROVIDER_URL", format!("http://{address}/v1/chat/completions"))
            .env("KAGEHA_MODEL", "fixture")
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
            .spawn().unwrap();
        child.stdin.take().unwrap().write_all(b"Read both files.").unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert_eq!(output.stdout, b"Fixture read.\n");
        provider.join().unwrap();
    });
    std::fs::remove_dir_all(root).unwrap();
    if let Err(error) = result {
        std::panic::resume_unwind(error);
    }
}
