//! Replay of the live run of chat `4cc7ad0ecb63` (spec autonomous-agent-loop, task 31.8;
//! Requirements 20.1, 20.2, 20.3, 20.10): two ~2.9 MB photos and no text. Each test
//! serves the real chat API (`webui::handle`) with a scripted OpenAI-compatible model
//! endpoint, and checks that the images reach a model that can see them within at most
//! five model calls, for each of the three image routes.
use crate::webui::{self, Database};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const QUESTION: &str = "What would you like me to do with these two screenshots?";
const DESCRIPTION: &str = "Software Engineer, Acme, Berlin\nDescription: a job list";
/// The prompt `ui_vision` sends to a describing model.
const DESCRIBE: &str = "Transcribe all visible text in this image verbatim";
const NAMES: [&str; 2] = ["IMG_7776.jpeg", "IMG_7777.jpeg"];
/// About the sizes of the two photos of the live run.
const TARGET_BYTES: [usize; 2] = [2_857_818, 2_978_706];

/// Scripted model: describes images when asked to, otherwise answers with `QUESTION`.
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
                let answer = if body.contains(DESCRIBE) { DESCRIPTION } else { QUESTION };
                seen.lock().unwrap().push(serde_json::from_str(&body).unwrap_or(Value::Null));
                let reply = json!({"choices":[{"message":{"role":"assistant","content":answer}}]}).to_string();
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
    /// A fresh chat with the models `models` (id, vision flag); `fixture` is the executor
    /// and `planner` the planner.
    fn new(chat: &str, models: &[(&str, bool)]) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let project = directory.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let provider = Provider::start();
        let connection = Connection::open(directory.path().join("native-chats.sqlite")).unwrap();
        connection.execute_batch(
            "CREATE TABLE native_chat_turns(sequence INTEGER PRIMARY KEY AUTOINCREMENT,chat_id TEXT NOT NULL,
                 request_id TEXT NOT NULL,message TEXT NOT NULL,answer TEXT,status TEXT NOT NULL,UNIQUE(chat_id,request_id));
             CREATE UNIQUE INDEX native_chat_running ON native_chat_turns(chat_id) WHERE status IN ('running','waiting_for_unlock');",
        ).unwrap();
        webui::initialize(&connection).unwrap();
        connection.execute("INSERT INTO ui_sessions(id,title,created_at,updated_at) VALUES (?,?,0,0)", [chat, "Photos"]).unwrap();
        crate::ui_approvals::main_default(&connection, chat, "main").unwrap();
        for (id, vision) in models {
            crate::ui_store::put(&connection, "model", id, json!({"id":id,"provider":"environment",
                "model":id,"base_url":provider.url,"protocol":"openai_compat","vision":vision})).unwrap();
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

    /// Saves `bytes` as a JPEG upload, as `ui_uploads::upload` does, and returns its path.
    fn upload(&self, id: usize, name: &str, bytes: &[u8]) -> String {
        let relative = format!("inputs/{id:032x}-{name}");
        let root = crate::ui_uploads::root(&self.db, &self.chat).unwrap();
        std::fs::create_dir_all(root.join("inputs")).unwrap();
        std::fs::write(root.join(&relative), bytes).unwrap();
        self.db.lock().unwrap().execute(
            "INSERT INTO ui_uploads(chat_id,path,filename,mime,sha256,size) VALUES (?,?,?,?,?,?)",
            params![self.chat, relative, name, "image/jpeg", format!("{:x}", Sha256::digest(bytes)), bytes.len()],
        ).unwrap();
        relative
    }

    fn send(&self, request: &str, attachments: &[String]) -> (u16, Value) {
        let body = json!({"session_id":self.chat,"client_request_id":request,"message":"",
            // The chat UI sends role chains as `model_roles` (frontend/src/store/runTurn.ts).
            "attachments":attachments,"model":"fixture","model_roles":{"planner":["planner"]},
            "capability_preferences":{"vision":"auto"}}).to_string();
        let address = self.api.server_addr().to_ip().unwrap();
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(300))).unwrap();
        write!(stream, "POST /api/chat/send HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let (head, body) = response.split_once("\r\n\r\n").unwrap();
        (head.split_whitespace().nth(1).unwrap().parse().unwrap(), serde_json::from_str(body).unwrap_or(json!(body)))
    }

    /// Uploads the two photos, sends them without text and checks what every route shares.
    /// Returns the upload paths and the recorded model requests.
    fn replay(&self) -> (Vec<String>, Vec<Value>) {
        let paths: Vec<String> = NAMES.iter().zip(TARGET_BYTES).enumerate()
            .map(|(index, (name, size))| self.upload(index + 1, name, &noisy_jpeg(size, index as u32))).collect();
        let (status, reply) = self.send("request-photos", &paths);
        assert_eq!(status, 200, "{reply}");
        let turn = webui::turn_id(&self.chat, "request-photos");
        let payload: String = self.db.lock().unwrap().query_row(
            "SELECT payload FROM ui_events WHERE turn_id=? AND event='done'", [&turn], |row| row.get(0)).unwrap();
        let done: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(done["status"], "success", "{done}");
        // Requirement 20.1: in `workspace/inputs/`, not artifacts.
        let workspace = crate::ui_workspace::open_for_chat(&self.db, &self.chat).unwrap();
        for name in NAMES {
            assert!(workspace.root().join("inputs").join(name).is_file(), "workspace/inputs/{name}");
        }
        let artifacts: i64 = self.db.lock().unwrap()
            .query_row("SELECT count(*) FROM ui_turn_artifacts WHERE chat_id=?", [&self.chat], |row| row.get(0)).unwrap();
        assert_eq!(artifacts, 0);
        let requests = self.provider.requests.lock().unwrap().clone();
        let agent: Vec<&Value> = requests.iter().filter(|request| !request.to_string().contains(DESCRIBE)).collect();
        assert!(!agent.is_empty() && agent.len() <= 5, "{} model calls", agent.len());
        let first = agent[0].to_string();
        // Requirement 20.10: the attachment-only note and the guidance to look, then ask.
        assert!(first.contains(crate::ui_prompt::ATTACHMENT_ONLY_NOTE), "no attachment-only note");
        assert!(first.contains("When the user sends attachments without saying what to do"), "no guidance");
        for request in &agent {
            let text = request.to_string();
            for error in ["could not be analysed", "could not be decoded", "could not read"] {
                assert!(!text.contains(error), "the agent got `{error}`");
            }
            // The scripted model calls no tools, so no escalation or workaround happened.
            assert!(!text.contains("\"tool_calls\""), "{text}");
        }
        (paths, requests)
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

/// A JPEG of random pixels of about `target` bytes.
fn noisy_jpeg(target: usize, salt: u32) -> Vec<u8> {
    let encode = |width: u32, height: u32| {
        let mut seed = 0x2545_f491_u32 ^ salt.wrapping_mul(0x9e37_79b9);
        let image = image::RgbImage::from_fn(width, height, |_, _| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            image::Rgb([seed as u8, (seed >> 8) as u8, (seed >> 16) as u8])
        });
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80).encode_image(&image).unwrap();
        out
    };
    let probe = encode(400, 300);
    let scale = ((target as f64) / (probe.len() as f64)).sqrt();
    let bytes = encode((400.0 * scale) as u32, (300.0 * scale) as u32);
    assert!(bytes.len() > target * 9 / 10 && bytes.len() < target * 11 / 10, "{} bytes", bytes.len());
    bytes
}

fn image_parts(message: &Value) -> usize {
    message["content"].as_array().map_or(0, |parts| parts.iter().filter(|part| part["type"] == "image_url").count())
}
fn user_message(request: &Value) -> Value {
    request["messages"].as_array().unwrap().iter().rev().find(|m| m["role"] == "user").unwrap().clone()
}
fn has_images(request: &Value) -> bool {
    request["messages"].as_array().unwrap().iter().any(|message| image_parts(message) > 0)
}

/// Both photos as image parts, resized under the inline limit, or the overflow note.
fn assert_images_inline(request: &Value, paths: &[String]) {
    let user = user_message(request);
    let parts = image_parts(&user);
    let text = user["content"][0]["text"].as_str().unwrap_or_default();
    assert!(parts == 2 || (parts == 1 && text.contains("are not shown inline")), "{parts} image parts: {text}");
    let mut total = 0;
    for part in user["content"].as_array().unwrap().iter().filter(|part| part["type"] == "image_url") {
        let url = part["image_url"]["url"].as_str().unwrap();
        let encoded = url.split_once(";base64,").unwrap().1;
        let bytes = data_encoding::BASE64.decode(encoded.as_bytes()).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert!(decoded.width().max(decoded.height()) <= crate::ui_vision::MAX_LONG_SIDE);
        total += bytes.len();
    }
    assert!(total <= crate::ui_attachments::MAX_INLINE_IMAGE_BYTES, "{total} bytes inline");
    for path in paths {
        assert!(text.contains(path.as_str()), "{path} not listed: {text}");
    }
}

#[test]
fn an_image_capable_executor_sees_both_photos_in_its_first_request() {
    let fixture = Fixture::new("replay-executor", &[("fixture", true), ("planner", false)]);
    let (paths, requests) = fixture.replay();
    let summary: Vec<(Value, bool)> = requests.iter().map(|request| (request["model"].clone(), has_images(request))).collect();
    assert_eq!(requests[0]["model"], "fixture", "(model, has images): {summary:?}");
    assert_images_inline(&requests[0], &paths);
    assert!(!requests.iter().any(|request| request.to_string().contains(DESCRIBE)), "no descriptions");
}

#[test]
fn a_text_only_executor_hands_image_steps_to_the_vision_planner() {
    // The live setup: cf-deepseek-v4.1-flash executor (text-only), cf-glm-5.3 planner (vision).
    let fixture = Fixture::new("replay-planner", &[("fixture", false), ("planner", true)]);
    let (paths, requests) = fixture.replay();
    let summary: Vec<(Value, bool, bool)> = requests.iter()
        .map(|request| (request["model"].clone(), request.to_string().contains(DESCRIBE), has_images(request))).collect();
    assert!(!requests[0].to_string().contains(DESCRIBE), "(model, description request, has images): {summary:?}");
    assert_images_inline(&requests[0], &paths);
    for request in requests.iter().filter(|request| has_images(request)) {
        assert_eq!(request["model"], "planner", "a request with images went to {}", request["model"]);
    }
    assert!(!requests.iter().any(|request| request.to_string().contains(DESCRIBE)), "no descriptions");
}

#[test]
fn without_a_vision_planner_a_vision_model_describes_each_photo_once() {
    let fixture = Fixture::new("replay-describe", &[("fixture", false), ("planner", false), ("seer", true)]);
    let (paths, requests) = fixture.replay();
    let (described, agent): (Vec<&Value>, Vec<&Value>) =
        requests.iter().partition(|request| request.to_string().contains(DESCRIBE));
    assert_eq!(described.len(), 2, "one description per photo");
    assert!(described.iter().all(|request| request["model"] == "seer" && has_images(request)));
    for request in &agent {
        assert!(!has_images(request), "a text-only model got image parts");
    }
    let user = user_message(agent[0]);
    let text = user["content"].as_str().expect("text content");
    for path in &paths {
        let name = path.rsplit('/').next().unwrap();
        let found = text.lines().any(|line| line.starts_with("[Image ") && line.contains(name)
            && line.contains("— description by seer]: Software Engineer, Acme, Berlin"));
        assert!(found, "no description of {name}: {text}");
    }
}
