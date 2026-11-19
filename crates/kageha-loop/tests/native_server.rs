#![cfg(unix)]

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const TOKEN: &str = "fixture-server-authentication-token";

struct Process(Child);

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn launch(root: &Path, provider: &str) -> (Process, String) {
    let mut process = Command::new(env!("CARGO_BIN_EXE_kageha-loop"))
        .args(["serve"])
        .arg(root.join("workspace"))
        .arg(root.join("state"))
        .arg("127.0.0.1:0")
        .env_clear()
        .env("PATH", root.join("bin"))
        .env("KAGEHA_SERVER_TOKEN", TOKEN)
        .env("KAGEHA_MODEL", "fixture")
        .env("KAGEHA_PROVIDER_URL", provider)
        // The learning store and reflection worker live under KAGEHA_HOME; keep them
        // inside the fixture root instead of the user's ~/.kageha.
        .env("KAGEHA_HOME", root.join("home"))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut reader = BufReader::new(process.stderr.take().unwrap());
    // Startup diagnostics may precede the listening line, as launchers that scan the
    // log (scripts/start_webui.sh) allow.
    let mut startup = String::new();
    let address = loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap() == 0 {
            panic!("server exited before listening:\n{startup}");
        }
        if let Some(address) = line.trim().strip_prefix("Rust chat service listening on ") {
            break address.to_owned();
        }
        startup.push_str(&line);
    };
    std::thread::spawn(move || {
        let mut line = String::new();
        while reader.read_line(&mut line).unwrap_or(0) > 0 {
            line.clear();
        }
    });
    (Process(process), format!("http://{address}"))
}

fn reply(socket: &mut TcpStream, content: &str) {
    let reply = json!({"choices":[{"message":{"role":"assistant","content":content}}]}).to_string();
    write!(
        socket,
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
        reply.len()
    )
    .unwrap();
}
/// Accepts the next turn's model call and returns its socket and payload. The Host's
/// background reflection (run after each request) calls the same provider; those calls
/// are answered with no lessons and skipped.
fn accept_turn_call(listener: &TcpListener) -> (TcpStream, Value) {
    loop {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(&mut socket);
        let mut length = 0;
        loop {
            let mut header = String::new();
            assert_ne!(reader.read_line(&mut header).unwrap(), 0);
            if header == "\r\n" {
                break;
            }
            if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse().unwrap();
            }
        }
        let mut bytes = vec![0; length];
        reader.read_exact(&mut bytes).unwrap();
        let payload: Value = serde_json::from_slice(&bytes).unwrap();
        let reflection = payload["messages"][0]["content"]
            .as_str()
            .is_some_and(|text| text.starts_with("You review the journal of a request"));
        if reflection {
            reply(&mut socket, r#"{"lessons":[]}"#);
            continue;
        }
        return (socket, payload);
    }
}
fn request(url: &str, token: &str, body: Option<Value>) -> (u16, Value) {
    request_method(
        url,
        token,
        if body.is_some() { "POST" } else { "GET" },
        body,
    )
}

fn request_method(url: &str, token: &str, method: &str, body: Option<Value>) -> (u16, Value) {
    let mut command = Command::new("curl");
    command.args(["--request", method]);
    command.args([
        "--disable",
        "--silent",
        "--max-time",
        "5",
        "--write-out",
        "\n%{http_code}",
        "--header",
        &format!("Authorization: Bearer {token}"),
    ]);
    if let Some(body) = body {
        command.args([
            "--header",
            "Content-Type: application/json",
            "--data",
            &body.to_string(),
        ]);
    }
    let output = command.arg(url).output().unwrap();
    assert!(
        output.status.success(),
        "request failed: {:?}",
        output.status
    );
    let output = String::from_utf8(output.stdout).unwrap();
    let (body, status) = output.rsplit_once('\n').unwrap();
    (status.parse().unwrap(), serde_json::from_str(body).unwrap())
}

#[test]
fn plugin_endpoints_require_a_scoped_review_before_installation() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("workspace")).unwrap();
    std::fs::create_dir_all(root.path().join("bin")).unwrap();
    let (_server, base) = launch(root.path(), "http://127.0.0.1:1");
    let (status, list) = request(&format!("{base}/api/plugins"), TOKEN, None);
    assert_eq!(status, 200);
    assert_eq!(list["plugins"], json!([]));
    let (status, _) = request(&format!("{base}/api/plugins/preview"), TOKEN, Some(json!({"name":"../invalid"})));
    assert_eq!(status, 409);
    let (status, _) = request(&format!("{base}/api/plugins/install"), TOKEN, Some(json!({"preview_id":"unknown","approved_summary":{}})));
    assert_eq!(status, 409);
    let (status, _) = request(&format!("{base}/api/plugins/preview"), TOKEN, None);
    assert_eq!(status, 400);
    assert_eq!(request(&format!("{base}/api/plugins"), TOKEN, None).1["plugins"], json!([]));
    let bridge=json!({"plugin":"fixture","app":"fixture","connector_id":"connector","server":"workspace","tools":["search"],"package_sha256":"package","expected_server_revision":1});
    assert_eq!(request(&format!("{base}/api/plugins/app-bridge"), "wrong", Some(bridge.clone())).0, 401);
    assert_eq!(request_method(&format!("{base}/api/plugins/app-bridge"), "wrong", "DELETE", Some(bridge)).0, 401);
    assert_eq!(request(&format!("{base}/api/plugins"), TOKEN, None).1["plugins"], json!([]));
}

#[test]
fn native_service_restarts_and_reuses_completed_requests_without_python() {
    let root = std::env::temp_dir().join(format!("kageha-native-server-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let result = std::panic::catch_unwind(|| {
        std::fs::create_dir(root.join("bin")).unwrap();
        std::fs::create_dir(root.join("workspace")).unwrap();
        let curl = Command::new("which").arg("curl").output().unwrap();
        symlink(
            String::from_utf8(curl.stdout).unwrap().trim(),
            root.join("bin/curl"),
        )
        .unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let provider_url = format!(
            "http://{}/v1/chat/completions",
            listener.local_addr().unwrap()
        );
        let (observed, receiving) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let provider = std::thread::spawn(move || {
            for step in 0..2 {
                let (mut socket, body) = accept_turn_call(&listener);
                let messages = body["messages"].as_array().unwrap();
                if step == 0 {
                    assert_eq!(messages.len(), 1);
                    assert_eq!(messages[0]["content"], "Remember red");
                    observed.send(()).unwrap();
                    released.recv_timeout(Duration::from_secs(5)).unwrap();
                } else {
                    assert_eq!(messages.len(), 3);
                    assert_eq!(messages[0]["content"], "Remember red");
                    assert_eq!(messages[1]["content"], "Red remembered.");
                    assert_eq!(messages[2]["content"], "What color?");
                }
                reply(&mut socket, if step == 0 { "Red remembered." } else { "Red." });
            }
        });
        let (server, url) = launch(&root, &provider_url);
        assert_eq!(request(&format!("{url}/health"), "wrong", None).0, 401);
        let body = json!({"request_id":"first","message":"Remember red"});
        let first_url = format!("{url}/chats/chat-one/turns");
        let first_body = body.clone();
        let first_request =
            std::thread::spawn(move || request(&first_url, TOKEN, Some(first_body)));
        receiving.recv_timeout(Duration::from_secs(5)).unwrap();
        let busy = json!({"request_id":"busy","message":"Concurrent work"});
        let denied = request(&format!("{url}/chats/chat-one/turns"), TOKEN, Some(busy));
        assert_eq!(denied.0, 409);
        assert_eq!(denied.1["error"], "chat_busy");
        assert_eq!(
            request(
                &format!("{url}/chats/chat-one/turns"),
                TOKEN,
                Some(body.clone())
            )
            .0,
            409
        );
        release.send(()).unwrap();
        let first = first_request.join().unwrap();
        assert_eq!(first.0, 200);
        assert_eq!(first.1["answer"], "Red remembered.");
        drop(server);
        {
            let db = rusqlite::Connection::open(root.join("state/native-chats.sqlite")).unwrap();
            db.execute(
                "INSERT INTO native_chat_turns(chat_id,request_id,message,status)
                 VALUES ('interrupted-chat','interrupted-request','Earlier work','running')",
                [],
            )
            .unwrap();
        }

        let (_server, url) = launch(&root, &provider_url);
        assert_eq!(
            request(&format!("{url}/chats/chat-one/turns"), TOKEN, Some(body)),
            first
        );
        let interrupted = json!({"request_id":"interrupted-request","message":"Earlier work"});
        let reply = request(
            &format!("{url}/chats/interrupted-chat/turns"),
            TOKEN,
            Some(interrupted),
        );
        assert_eq!(reply.0, 409);
        assert_eq!(reply.1["status"], "interrupted");
        let conflict = json!({"request_id":"first","message":"Different"});
        assert_eq!(
            request(
                &format!("{url}/chats/chat-one/turns"),
                TOKEN,
                Some(conflict)
            )
            .0,
            409
        );
        let second = json!({"request_id":"second","message":"What color?"});
        let reply = request(&format!("{url}/chats/chat-one/turns"), TOKEN, Some(second));
        assert_eq!(reply.0, 200);
        assert_eq!(reply.1["answer"], "Red.");
        let history = request(&format!("{url}/chats/chat-one/history"), TOKEN, None);
        assert_eq!(history.0, 200);
        assert_eq!(history.1["turns"].as_array().unwrap().len(), 2);
        provider.join().unwrap();
    });
    std::fs::remove_dir_all(root).unwrap();
    if let Err(error) = result {
        std::panic::resume_unwind(error);
    }
}

/// Opens `method path` on the server with a raw socket and returns it unread, so a test
/// can drop the connection while the server still streams.
fn open_stream(url: &str, method: &str, path: &str, body: Option<&Value>) -> TcpStream {
    let address = url.strip_prefix("http://").unwrap();
    let mut socket = TcpStream::connect(address).unwrap();
    let body = body.map(Value::to_string).unwrap_or_default();
    write!(
        socket,
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {TOKEN}\r\nAccept: text/event-stream\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    socket.flush().unwrap();
    socket
}

fn sequences(page: &Value) -> Vec<i64> {
    page["events"].as_array().unwrap().iter().map(|event| event["sequence"].as_i64().unwrap()).collect()
}

#[test]
fn a_dropped_stream_leaves_the_turn_running_and_replay_returns_what_it_missed() {
    let root = std::env::temp_dir().join(format!("kageha-dropped-stream-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let result = std::panic::catch_unwind(|| {
        std::fs::create_dir(root.join("bin")).unwrap();
        std::fs::create_dir(root.join("workspace")).unwrap();
        let curl = Command::new("which").arg("curl").output().unwrap();
        symlink(String::from_utf8(curl.stdout).unwrap().trim(), root.join("bin/curl")).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let provider_url = format!("http://{}/v1/chat/completions", listener.local_addr().unwrap());
        let (called, calling) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let provider = std::thread::spawn(move || {
            let (mut socket, _) = accept_turn_call(&listener);
            called.send(()).unwrap();
            released.recv_timeout(Duration::from_secs(10)).unwrap();
            reply(&mut socket, "Finished after the stream dropped");
        });
        let (_server, url) = launch(&root, &provider_url);
        let (_, created) = request(&format!("{url}/api/sessions"), TOKEN, Some(json!({})));
        let chat = created["session_id"].as_str().unwrap().to_owned();

        // A turn streamed over SSE, and the chat's live feed, both mid-turn.
        let body = json!({"session_id":chat,"client_request_id":"dropped","message":"Keep going without me"});
        let turn_stream = open_stream(&url, "POST", "/api/chat/stream", Some(&body));
        calling.recv_timeout(Duration::from_secs(10)).unwrap();
        let mut live = open_stream(&url, "GET", &format!("/api/sessions/{chat}/live/stream?after=0"), None);
        live.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut head = [0u8; 256];
        let read = live.read(&mut head).unwrap();
        assert!(String::from_utf8_lossy(&head[..read]).starts_with("HTTP/1.1 200"));
        let seen = request(&format!("{url}/api/sessions/{chat}/events?after=0"), TOKEN, None).1;
        assert_eq!(seen["turn"]["running"], true, "{seen}");
        let cursor = seen["next_after"].as_i64().unwrap();
        assert!(cursor > 0);

        // Both clients go away; heartbeats to them now fail.
        drop(turn_stream);
        drop(live);
        std::thread::sleep(Duration::from_millis(600));
        let still = request(&format!("{url}/api/sessions/{chat}/events?after={cursor}"), TOKEN, None).1;
        assert_eq!(still["turn"]["running"], true, "a dropped stream must not stop the turn: {still}");
        release.send(()).unwrap();
        provider.join().unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let finished = loop {
            let page = request(&format!("{url}/api/sessions/{chat}/events?after=0&limit=2000"), TOKEN, None).1;
            if page["turn"]["running"] == false {
                break page;
            }
            assert!(std::time::Instant::now() < deadline, "the turn did not finish: {page}");
            std::thread::sleep(Duration::from_millis(100));
        };
        assert_eq!(finished["turn"]["status"], "success", "{finished}");
        assert!(finished["events"].as_array().unwrap().iter().any(|event| event["kind"] == "done"));
        let detail = request(&format!("{url}/api/sessions/{chat}"), TOKEN, None).1;
        assert!(detail["messages"].as_array().unwrap().iter()
            .any(|message| message["text"] == "Finished after the stream dropped"), "{detail}");

        // Replay after the cursor returns exactly the events the dropped stream missed.
        let missed = request(&format!("{url}/api/sessions/{chat}/events?after={cursor}"), TOKEN, None).1;
        let expected: Vec<i64> = sequences(&finished).into_iter().filter(|sequence| *sequence > cursor).collect();
        assert!(!expected.is_empty());
        assert_eq!(sequences(&missed), expected);
        assert_eq!(missed["next_after"].as_i64(), expected.last().copied());
        let paged = request(&format!("{url}/api/sessions/{chat}/events?after={cursor}&limit=1"), TOKEN, None).1;
        assert_eq!(sequences(&paged), expected[..1]);
        assert_eq!(paged["has_more"], expected.len() > 1);
        assert_eq!(request(&format!("{url}/api/sessions/{chat}/events?after=-1"), TOKEN, None).0, 400);
        assert_eq!(request(&format!("{url}/api/sessions/{chat}/events?limit=0"), TOKEN, None).0, 400);
    });
    std::fs::remove_dir_all(root).unwrap();
    if let Err(error) = result {
        std::panic::resume_unwind(error);
    }
}

fn stream_request(url: &str, body: Value) -> String {
    let output = Command::new("curl")
        .args([
            "--disable",
            "--silent",
            "--max-time",
            "10",
            "--header",
            &format!("Authorization: Bearer {TOKEN}"),
            "--header",
            "Content-Type: application/json",
            "--data",
            &body.to_string(),
            url,
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn webui_contract_streams_persists_and_cancels_without_python() {
    let root = std::env::temp_dir().join(format!("kageha-webui-api-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let result = std::panic::catch_unwind(|| {
        std::fs::create_dir(root.join("bin")).unwrap();
        std::fs::create_dir(root.join("workspace")).unwrap();
        let curl = Command::new("which").arg("curl").output().unwrap();
        symlink(
            String::from_utf8(curl.stdout).unwrap().trim(),
            root.join("bin/curl"),
        )
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let provider_url = format!(
            "http://{}/v1/chat/completions",
            listener.local_addr().unwrap()
        );
        let (started, pending) = std::sync::mpsc::channel();
        let provider = std::thread::spawn(move || {
            for step in 0..2 {
                let (mut socket, payload) = accept_turn_call(&listener);
                if step == 0 {
                    let user_msg = payload["messages"].as_array().unwrap().iter().find(|m| m["role"] == "user").unwrap();
                    assert_eq!(user_msg["content"], "First UI message");
                    reply(&mut socket, "Native UI answer");
                } else {
                    let msgs = payload["messages"].as_array().unwrap();
                    assert!(msgs.iter().any(|m| m["role"] == "assistant" && m["content"] == "Native UI answer"), "{payload:#}");
                    assert!(msgs.iter().any(|m| m["role"] == "user" && m["content"] == "Slow UI message"));
                    started.send(()).unwrap();
                    let mut tail = Vec::new();
                    let _ = socket.read_to_end(&mut tail);
                }
            }
        });
        let (server, url) = launch(&root, &provider_url);
        let (_, created) = request(&format!("{url}/api/sessions"), TOKEN, Some(json!({})));
        let chat = created["session_id"].as_str().unwrap().to_owned();
        let first =
            json!({"session_id":chat,"client_request_id":"ui-first","message":"First UI message"});
        let frames = stream_request(&format!("{url}/api/chat/stream"), first.clone());
        assert!(frames.contains("event: status\n"));
        assert!(frames.contains("event: message\n"));
        assert!(frames.contains("event: done\n"));
        assert!(frames.contains("Native UI answer"));
        let detail = request(&format!("{url}/api/sessions/{chat}"), TOKEN, None);
        assert_eq!(detail.1["messages"][1]["text"], "Native UI answer");
        let changed = request_method(
            &format!("{url}/api/sessions/{chat}"),
            TOKEN,
            "PATCH",
            Some(json!({"title":"Native conversation","pinned":true,"archived":false})),
        );
        assert_eq!(changed.0, 200);
        assert_eq!(changed.1["title"], "Native conversation");
        assert_eq!(changed.1["pinned"], true);
        assert_eq!(
            request(
                &format!("{url}/api/sessions/{chat}/requests/ui-first"),
                TOKEN,
                None
            )
            .1["receipt"]["state"],
            "finished"
        );
        drop(server);

        let (_server, url) = launch(&root, &provider_url);
        let repeated = stream_request(&format!("{url}/api/chat/stream"), first);
        assert!(repeated.contains("Native UI answer"));
        let restored = request(&format!("{url}/api/sessions/{chat}"), TOKEN, None).1;
        assert_eq!(restored["title"], "Native conversation");
        assert_eq!(restored["pinned"], true);
        assert!(
            request(&format!("{url}/api/sessions"), TOKEN, None).1["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|session| session["session_id"] == chat)
        );
        let slow =
            json!({"session_id":chat,"client_request_id":"ui-slow","message":"Slow UI message"});
        let streaming_url = format!("{url}/api/chat/stream");
        let worker = std::thread::spawn(move || stream_request(&streaming_url, slow));
        pending.recv_timeout(Duration::from_secs(5)).unwrap();
        let receipt = request(
            &format!("{url}/api/sessions/{chat}/requests/ui-slow"),
            TOKEN,
            None,
        )
        .1;
        let turn = receipt["receipt"]["turn_id"].as_str().unwrap();
        assert_eq!(
            request_method(&format!("{url}/api/sessions/{chat}"), TOKEN, "DELETE", None).0,
            409
        );
        let stopped = request(
            &format!("{url}/api/chat/cancel"),
            TOKEN,
            Some(json!({"session_id":chat,"turn_id":turn})),
        );
        assert_eq!(stopped.1["cancelled"], true);
        let frames = worker.join().unwrap();
        assert!(frames.contains("\"status\":\"cancelled\""));
        let receipt = request(
            &format!("{url}/api/sessions/{chat}/requests/ui-slow"),
            TOKEN,
            None,
        )
        .1;
        assert_eq!(receipt["receipt"]["result_status"], "cancelled");
        let deleted = request_method(&format!("{url}/api/sessions/{chat}"), TOKEN, "DELETE", None);
        assert_eq!(deleted.0, 200);
        assert_eq!(deleted.1["deleted"], true);
        assert_eq!(
            request(&format!("{url}/api/sessions/{chat}"), TOKEN, None).0,
            404
        );
        provider.join().unwrap();
    });
    std::fs::remove_dir_all(root).unwrap();
    if let Err(error) = result {
        std::panic::resume_unwind(error);
    }
}
