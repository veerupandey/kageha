//! Turn failures from the first live run (spec autonomous-agent-loop, task 31.1,
//! Requirement 20.11). Each test serves the real chat API (`webui::handle`) from a fresh
//! database, as right after a server start, with a model endpoint that answers every
//! request.
//!
//! Turn 151 of chat `4cc7ad0ecb63` sent two photos of 2.9 MB each and no text. The
//! turn failed after 212 ms without a model call: `ui_attachments::decorate` refused the
//! images because together they exceed the 4 MiB that one message may inline, and the
//! failure path reported only "The agent stopped before completing this turn".
use crate::webui::{self, Database};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const ANSWER: &str = "These are two screenshots.";
/// The sizes of `IMG_7776.jpeg` and `IMG_7777.jpeg` in the live run.
const PHOTO_BYTES: [usize; 2] = [2_857_818, 2_978_706];

/// An OpenAI-compatible model endpoint that answers every request with `ANSWER`.
struct Provider {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    server: Arc<tiny_http::Server>,
}
impl Provider {
    fn start() -> Self {
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
        let url = format!("http://{}/v1/chat/completions", server.server_addr());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (incoming, seen) = (server.clone(), requests.clone());
        std::thread::spawn(move || {
            for mut request in incoming.incoming_requests() {
                let mut body = String::new();
                let _ = request.as_reader().read_to_string(&mut body);
                seen.lock().unwrap().push(serde_json::from_str(&body).unwrap_or(Value::Null));
                let reply = json!({"choices":[{"message":{"role":"assistant","content":ANSWER}}]}).to_string();
                let header = tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap();
                let _ = request.respond(tiny_http::Response::from_string(reply).with_header(header));
            }
        });
        Self { url, requests, server }
    }
}
impl Drop for Provider {
    fn drop(&mut self) {
        self.server.unblock();
    }
}

struct Fixture {
    _directory: tempfile::TempDir,
    db: Database,
    chat: String,
    provider: Provider,
    api: Arc<tiny_http::Server>,
}

impl Fixture {
    /// A fresh chat `chat` with an executor model `fixture` and a planner model `planner`.
    fn new(chat: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let project = directory.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let provider = Provider::start();
        let connection = Connection::open(directory.path().join("native-chats.sqlite")).unwrap();
        // The table `server::run` creates.
        connection.execute_batch(
            "CREATE TABLE native_chat_turns(sequence INTEGER PRIMARY KEY AUTOINCREMENT,chat_id TEXT NOT NULL,
                 request_id TEXT NOT NULL,message TEXT NOT NULL,answer TEXT,status TEXT NOT NULL,UNIQUE(chat_id,request_id));
             CREATE UNIQUE INDEX native_chat_running ON native_chat_turns(chat_id) WHERE status IN ('running','waiting_for_unlock');",
        ).unwrap();
        webui::initialize(&connection).unwrap();
        connection.execute("INSERT INTO ui_sessions(id,title,created_at,updated_at) VALUES (?,?,0,0)", [chat, "Photos"]).unwrap();
        crate::ui_approvals::main_default(&connection, chat, "main").unwrap();
        for id in ["fixture", "planner"] {
            crate::ui_store::put(&connection, "model", id, json!({"id":id,"provider":"environment",
                "model":id,"base_url":provider.url,"protocol":"openai_compat",
                // An image-capable executor, so attached photos are inlined (Requirement 20.2).
                "vision":id == "fixture"})).unwrap();
        }
        let db = crate::agent_host::DatabaseState::open(connection).unwrap();
        let root = crate::ui_workspace::open_for_chat(&db, chat).unwrap().root().to_path_buf();
        crate::ui_tools::test_free_disk::set(&root, u64::MAX);
        let api = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
        let (incoming, served) = (api.clone(), db.clone());
        let controls: webui::Controls = Arc::new(Mutex::new(Default::default()));
        std::thread::spawn(move || {
            for request in incoming.incoming_requests() {
                let (db, controls, project) = (served.clone(), controls.clone(), project.clone());
                std::thread::spawn(move || {
                    let _ = webui::handle(request, db, controls, &project);
                });
            }
        });
        Self { _directory: directory, db, chat: chat.into(), provider, api }
    }

    /// Saves a JPEG upload of `size` bytes, as `ui_uploads::upload` does, and returns its path.
    fn upload(&self, name: &str, size: usize) -> String {
        let mut bytes = vec![0u8; size];
        bytes[..4].copy_from_slice(b"\xff\xd8\xff\xe0");
        let relative = format!("inputs/{:032x}-{name}", size);
        let root = crate::ui_uploads::root(&self.db, &self.chat).unwrap();
        std::fs::create_dir_all(root.join("inputs")).unwrap();
        std::fs::write(root.join(&relative), &bytes).unwrap();
        self.db.lock().unwrap().execute(
            "INSERT INTO ui_uploads(chat_id,path,filename,mime,sha256,size) VALUES (?,?,?,?,?,?)",
            params![self.chat, relative, name, "image/jpeg", format!("{:x}", Sha256::digest(&bytes)), size],
        ).unwrap();
        relative
    }

    /// Submits a message to `/api/chat/send`, which answers when the turn is done.
    fn send(&self, request: &str, message: &str, attachments: &[String]) -> (u16, Value) {
        let body = json!({"session_id":self.chat,"client_request_id":request,"message":message,
            "attachments":attachments,"model":"fixture","planner_model":"planner",
            "capability_preferences":{"vision":"auto"}}).to_string();
        let address = self.api.server_addr().to_ip().unwrap();
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(120))).unwrap();
        write!(stream, "POST /api/chat/send HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let (head, body) = response.split_once("\r\n\r\n").unwrap();
        let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        (status, serde_json::from_str(body).unwrap_or(json!(body)))
    }

    fn done(&self, turn: &str) -> Value {
        let payload: String = self.db.lock().unwrap().query_row(
            "SELECT payload FROM ui_events WHERE turn_id=? AND event='done'", [turn], |row| row.get(0)).unwrap();
        serde_json::from_str(&payload).unwrap()
    }

    fn result(&self, turn: &str) -> Value {
        let result: String = self.db.lock().unwrap().query_row(
            "SELECT result FROM ui_runtime_results WHERE turn=?", [turn], |row| row.get(0)).unwrap();
        serde_json::from_str(&result).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.api.unblock();
        if let Ok(host) = crate::runtime_host::get(&self.db) {
            let _ = host.supervisor.shutdown();
        }
    }
}

/// The user message of the first model request.
fn first_user_message(fixture: &Fixture) -> Value {
    let requests = fixture.provider.requests.lock().unwrap();
    let first = requests.iter().find(|r| r["tools"].as_array().is_some_and(|tools| !tools.is_empty())).expect("the executor was called");
    first["messages"].as_array().unwrap().iter().rev().find(|m| m["role"] == "user").unwrap().clone()
}

#[test]
fn a_first_turn_with_two_large_photos_and_no_text_reaches_the_model() {
    let fixture = Fixture::new("photos-chat");
    let paths: Vec<String> = ["IMG_7776.jpeg", "IMG_7777.jpeg"].iter().zip(PHOTO_BYTES)
        .map(|(name, size)| fixture.upload(name, size)).collect();
    let (status, done) = fixture.send("request-photos", "", &paths);
    assert_eq!(status, 200, "{done}");
    let turn = webui::turn_id(&fixture.chat, "request-photos");
    let done = fixture.done(&turn);
    assert_eq!(done["status"], "success", "{done}");
    assert!(done["failure_code"].is_null() && done["failure_detail"].is_null(), "{done}");
    // The first photo fits the 4 MiB inline limit; the second stays a file, and the
    // message says so.
    let user = first_user_message(&fixture);
    let blocks = user["content"].as_array().expect("image content parts");
    assert_eq!(blocks.iter().filter(|block| block["type"] == "image_url").count(), 1, "{}", blocks[0]);
    let text = blocks[0]["text"].as_str().unwrap();
    assert!(text.contains(&format!("- `{}`", paths[0])) && text.contains(&format!("- `{}`", paths[1])), "{text}");
    assert!(text.contains(&format!("[Host note: the images `{}` are not shown inline", paths[1])), "{text}");
}

#[test]
fn an_unrecognized_setup_error_is_logged_and_its_redacted_detail_reaches_the_result_and_done_event() {
    let fixture = Fixture::new("failing-chat");
    let turn = webui::turn_id(&fixture.chat, "request-fails");
    let token = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    crate::ui_turn_failure::test_hooks::inject(&turn, "Model routing setup failed",
        &format!("provider answered 401 for token={token}"));
    let (status, reply) = fixture.send("request-fails", "Summarize my notes", &[]);
    assert_eq!(status, 200, "{reply}");
    let done = fixture.done(&turn);
    assert_eq!((done["status"].as_str(), done["failure_code"].as_str()), (Some("error"), Some("execution_failed")), "{done}");
    let detail = done["failure_detail"].as_str().expect("failure_detail");
    assert!(detail.starts_with("Model routing setup failed: provider answered 401 for token="), "{detail}");
    assert!(detail.contains("[redacted]") && !detail.contains(token), "{detail}");
    let result = fixture.result(&turn);
    assert_eq!(result["failure_detail"], done["failure_detail"], "{result}");
    assert_eq!(result["failure_id"], done["failure_id"], "{result}");
    // One log line names the failure id, the chat and the turn.
    let lines = crate::ui_turn_failure::test_hooks::logged(&turn);
    assert_eq!(lines.len(), 1, "{lines:?}");
    let expected = format!("turn failed failure_id={} chat={} turn={turn} error=Model routing setup failed: provider answered 401 for token=",
        result["failure_id"].as_str().unwrap(), fixture.chat);
    assert!(lines[0].starts_with(&expected) && !lines[0].contains(token), "{}", lines[0]);
    // The model was never called.
    assert!(fixture.provider.requests.lock().unwrap().is_empty());
}

#[test]
fn dedicated_acknowledgment_starts_before_executor_routing_finishes() {
    let fixture = Fixture::new("ack-routing");
    let endpoint = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let spec = json!({"interactive":true});
    {
        let conn = fixture.db.lock().unwrap();
        crate::ui_store::put(&conn, "model", "brief", json!({
            "provider":"lmstudio","model":"brief","protocol":"openai_compat",
            "base_url":format!("http://{}/v1", endpoint.server_addr())
        })).unwrap();
        crate::ui_store::put(&conn, "setting", "roles", json!({"acknowledgment":"brief"})).unwrap();
        conn.execute("INSERT INTO ui_run_specs VALUES (?,?,?)", params![fixture.chat, "request", spec.to_string()]).unwrap();
    }
    let (routing, executor) = std::sync::mpsc::channel();
    let worker = crate::ui_acknowledgment::start(&fixture.db, &fixture.chat, "request", "turn",
        &spec, &[json!({"role":"user","content":"What did we decide?"})], executor, &Arc::default()).unwrap();
    // Keep the sender alive without providing an executor. Waiting for routing here
    // would prevent the independent model from ever reaching the HTTP endpoint.
    let request = endpoint.recv_timeout(Duration::from_secs(3)).unwrap().expect("ack should already be running");
    worker.stop();
    let _ = request.respond(tiny_http::Response::from_string(r#"{"choices":[{"message":{"role":"assistant","content":"Received"}}]}"#));
    drop(routing);
}

#[test]
fn acknowledgment_streams_while_the_executor_reads_and_replay_does_not_charge_again() {
    use std::sync::mpsc;
    let fixture = Fixture::new("ack-overlap");
    let main = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let ack = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let turn = webui::turn_id(&fixture.chat, "overlap");
    let db = fixture.db.clone();
    {
        let conn = db.lock().unwrap();
        for (name, address) in [("fixture", main.server_addr().to_string()), ("brief", ack.local_addr().unwrap().to_string())] {
            crate::ui_store::put(&conn, "model", name, json!({"id":name,"provider":"environment","model":name,
                "base_url":format!("http://{address}/v1/chat/completions"),"protocol":"openai_compat"})).unwrap();
        }
        crate::ui_store::put(&conn,"setting","roles",json!({"acknowledgment":"brief"})).unwrap();
    }
    let root = crate::ui_workspace::open_for_chat(&db, &fixture.chat).unwrap();
    std::fs::write(root.root().join("fixture.txt"), "violet").unwrap();
    let (read_finished, release) = mpsc::channel();
    let (ack_done, completed) = mpsc::channel();
    let timeout = Duration::from_secs(5);
    let ack_thread = std::thread::spawn(move || {
        use std::io::{BufRead, BufReader};
        let (mut stream, _) = ack.accept().unwrap();
        stream.set_read_timeout(Some(timeout)).unwrap();
        let mut reader = BufReader::new(&stream);
        let mut len = 0;
        loop {
            let mut line = String::new();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            if line.trim().is_empty() { break; }
            if let Some(raw) = line.to_ascii_lowercase().strip_prefix("content-length:") { len = raw.trim().parse().unwrap(); }
        }
        let mut bytes = vec![0; len];
        reader.read_exact(&mut bytes).unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["model"], "brief");
        assert_eq!(body["max_tokens"], 48);
        assert!(body["tools"].as_array().is_none_or(Vec::is_empty));
        assert_eq!(body["messages"].as_array().unwrap().len(), 2);
        write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {}\n\n",
            json!({"choices":[{"delta":{"content":"I'll check"},"finish_reason":null}]})).unwrap();
        release.recv_timeout(timeout).expect("file read must complete before acknowledgment finishes");
        write!(stream,"data: {}\n\ndata: [DONE]\n\n",
            json!({"choices":[{"delta":{"content":" the file."},"finish_reason":"stop"}],"usage":{
                "prompt_tokens":80,"completion_tokens":6,"prompt_tokens_details":{"cached_tokens":12}}})).unwrap();
        drop(stream);
        ack_done.send(()).unwrap();
    });
    let executor = std::thread::spawn(move || {
        let respond = |request: tiny_http::Request, message: Value| {
            request.respond(tiny_http::Response::from_string(json!({"choices":[{"message":message}]}).to_string())).unwrap();
        };
        let first = main.recv_timeout(timeout).unwrap().expect("executor starts concurrently");
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let visible: bool = db.lock().unwrap().query_row("SELECT EXISTS(SELECT 1 FROM ui_events WHERE turn_id=? AND json_extract(payload,'$.payload.text')=?)",
                params![turn, "I'll check"], |r|r.get(0)).unwrap();
            if visible { break; }
            assert!(std::time::Instant::now() < deadline, "partial acknowledgment should be visible before the tool is selected");
            std::thread::sleep(Duration::from_millis(10));
        }
        respond(first, json!({"role":"assistant","content":null,"tool_calls":[{"id":"read","type":"function",
            "function":{"name":"read_file","arguments":"{\"path\":\"fixture.txt\"}"}}]}));
        let mut second = main.recv_timeout(timeout).unwrap().expect("tool finished during acknowledgment");
        let mut body = String::new();
        second.as_reader().read_to_string(&mut body).unwrap();
        assert!(body.contains("violet"), "executor should receive the file contents");
        assert!(!body.contains("I'll check"));
        read_finished.send(()).unwrap();
        completed.recv_timeout(timeout).unwrap();
        let deadline = std::time::Instant::now() + timeout;
        while !db.lock().unwrap().query_row("SELECT EXISTS(SELECT 1 FROM ui_events WHERE turn_id=? AND json_extract(payload,'$.payload.text')=?)",
            params![turn,"I'll check the file."], |r|r.get::<_,bool>(0)).unwrap() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        respond(second, json!({"role":"assistant","content":"violet"}));
    });
    let message = "Read the file fixture.txt and tell me the color.";
    let (_, result) = fixture.send("overlap", message, &[]);
    assert_eq!(result["message"], "violet", "{result}");
    ack_thread.join().unwrap();
    executor.join().unwrap();
    let (_, replay) = fixture.send("overlap", message, &[]);
    assert_eq!(replay["message"], "violet");
    let conn = fixture.db.lock().unwrap();
    let (calls, cached): (u32,u32) = conn.query_row("SELECT used_model_calls,cached_tokens FROM ui_request_budgets WHERE id=?",
        [webui::turn_id(&fixture.chat, "overlap")], |r| Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!((calls,cached),(3,12));
    let usage: String = conn.query_row("SELECT json_extract(spec,'$.acknowledgment.usage') FROM ui_run_specs WHERE request_id='overlap'", [], |r|r.get(0)).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&usage).unwrap(),json!({"input_tokens":80,"output_tokens":6,"cached_tokens":12}));
}

#[test]
fn stalled_acknowledgment_cannot_delay_or_follow_a_direct_answer() {
    let fixture = Fixture::new("ack-stall");
    let main = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let brief = tiny_http::Server::http("127.0.0.1:0").unwrap();
    {
        let conn = fixture.db.lock().unwrap();
        for (name, endpoint) in [("fixture", &main), ("brief", &brief)] {
            crate::ui_store::put(&conn, "model", name, json!({"id":name,"provider":"environment","model":name,
                "base_url":format!("http://{}/v1/chat/completions", endpoint.server_addr()),"protocol":"openai_compat"})).unwrap();
        }
        crate::ui_store::put(&conn,"setting","roles",json!({"acknowledgment":"brief"})).unwrap();
    }
    std::thread::scope(|scope| {
        let turn = scope.spawn(|| fixture.send("stalled", "Look up the color", &[]));
        let timeout = Duration::from_secs(3);
        let ack = brief.recv_timeout(timeout).unwrap().expect("ack started");
        let executor = main.recv_timeout(timeout).unwrap().expect("executor started independently");
        executor.respond(tiny_http::Response::from_string(json!({"choices":[{"message":{"role":"assistant","content":"Blue."}}]}).to_string())).unwrap();
        let start = std::time::Instant::now();
        assert_eq!(turn.join().unwrap().1["message"], "Blue.");
        assert!(start.elapsed() < timeout, "answer waited for the stalled acknowledgment");
        let _ = ack.respond(tiny_http::Response::from_string(json!({"choices":[{"message":{"role":"assistant","content":"Too late."}}]}).to_string()));
    });
    let turn = webui::turn_id(&fixture.chat, "stalled");
    let count: i64 = fixture.db.lock().unwrap().query_row("SELECT COUNT(*) FROM ui_events WHERE turn_id=? AND json_extract(payload,'$.payload.acknowledgment_id') IS NOT NULL", [turn], |r|r.get(0)).unwrap();
    assert_eq!(count, 0);
}
