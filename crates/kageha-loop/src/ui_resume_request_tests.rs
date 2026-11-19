//! "Resume task" continues the latest request from its checkpoint (Requirement 13.5,
//! design section 12, task 16.4). Each test serves the real chat API (`webui::handle`)
//! from a fresh database whose first turn stopped with a checkpoint, and a model
//! endpoint that answers every request.
use crate::webui::{self, Database};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const CHAT: &str = "resume-request-chat";
const REQUEST: &str = "request-1";
const PROMPT: &str = "Summarize the notes";
const ANSWER: &str = "Summary finished after resuming.";
/// Model calls the request used before it stopped.
const USED_BEFORE: u32 = 3;

/// An OpenAI-compatible model endpoint that answers every request with `ANSWER` and
/// keeps the request bodies.
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
    fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
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
    /// The stopped first turn.
    turn: String,
    budget: String,
    provider: Provider,
    api: Arc<tiny_http::Server>,
}

impl Fixture {
    /// A chat whose first request stopped (`interrupted`) after `checkpoint` frames were
    /// recorded. Its request budget has used [`USED_BEFORE`] model calls, its
    /// RecoveryState holds evidence and its outcome contract holds one outcome.
    fn new(checkpoint: &[Value]) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let project = directory.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let provider = Provider::start();
        // File-backed like the server's; no credential vault exists in the directory.
        let connection = Connection::open(directory.path().join("native-chats.sqlite")).unwrap();
        // The table `server::run` creates.
        connection.execute_batch(
            "CREATE TABLE native_chat_turns(sequence INTEGER PRIMARY KEY AUTOINCREMENT,chat_id TEXT NOT NULL,
                 request_id TEXT NOT NULL,message TEXT NOT NULL,answer TEXT,status TEXT NOT NULL,UNIQUE(chat_id,request_id));
             CREATE UNIQUE INDEX native_chat_running ON native_chat_turns(chat_id) WHERE status IN ('running','waiting_for_unlock');",
        ).unwrap();
        webui::initialize(&connection).unwrap();
        connection.execute("INSERT INTO ui_sessions(id,title,created_at,updated_at) VALUES (?,?,0,0)", [CHAT, "Resume"]).unwrap();
        crate::ui_approvals::main_default(&connection, CHAT, "main").unwrap();
        crate::ui_store::put(&connection, "model", "fixture", json!({"id":"fixture","provider":"environment",
            "model":"fixture","base_url":provider.url,"protocol":"openai_compat"})).unwrap();
        let turn = webui::turn_id(CHAT, REQUEST);
        let mut configuration = json!({"agent_id":"main","agent_mode":"normal","model":"fixture","depth":0,
            "persona":{"id":"main","name":"Kageha"}});
        crate::ui_persistence::admit(&connection, &turn, &mut configuration).unwrap();
        let budget = configuration["request_budget_id"].as_str().expect("request persistence is on by default").to_owned();
        connection.execute("INSERT INTO native_chat_turns(chat_id,request_id,message,answer,status) VALUES (?,?,?,?,'interrupted')",
            params![CHAT, REQUEST, PROMPT, "The task stopped."]).unwrap();
        connection.execute("INSERT INTO ui_run_specs VALUES (?,?,?)", params![CHAT, REQUEST, configuration.to_string()]).unwrap();
        connection.execute("INSERT INTO ui_turn_uploads(chat_id,request_id,paths) VALUES (?,?,'[]')", params![CHAT, REQUEST]).unwrap();
        let spec = crate::runtime_host::spec(&project, CHAT, REQUEST, &turn, "main", PROMPT, 0, None, None,
            vec![json!({"role":"user","content":PROMPT})], configuration.clone(), 16);
        crate::runtime_host::prepare(&connection, &spec).unwrap();
        // The run ended; the runtime imports it as terminal and never runs it.
        connection.execute("UPDATE ui_runtime_runs SET state='cancelled' WHERE id=?", [&turn]).unwrap();
        connection.execute("UPDATE ui_request_budgets SET used_model_calls=? WHERE id=?", params![USED_BEFORE, budget]).unwrap();
        crate::ui_persistence::save_recovery(&connection, &configuration, CHAT,
            &json!({"version":2,"evidence":["pre-resume-evidence"]})).unwrap();
        let db = crate::agent_host::DatabaseState::open(connection).unwrap();
        let root = crate::ui_workspace::open_for_chat(&db, CHAT).unwrap().root().to_path_buf();
        crate::ui_tools::test_free_disk::set(&root, u64::MAX);
        // The outcome contract the first turn recorded.
        crate::ui_task_completion::record(&db, CHAT, &turn, &outcomes("summary"),
            &crate::ui_task_completion::provable_kinds([])).unwrap();
        for frame in checkpoint {
            let annotations = if frame["kind"] == "tools_dispatched" {
                let effects: serde_json::Map<String, Value> = frame["calls"].as_array().unwrap().iter()
                    .map(|call| (call["id"].as_str().unwrap().to_owned(), json!(crate::ui_turn_checkpoints::effect_name(
                        crate::ui_tools::capability(call["name"].as_str().unwrap()).effect))))
                    .collect();
                json!({"effects":effects})
            } else {
                json!({"recovery":null})
            };
            crate::ui_job_controls::record(&db, CHAT, &turn, frame, &annotations).unwrap();
        }
        // The chat API, as `server::handle` serves it after authentication.
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
        Self { _directory: directory, db, turn, budget, provider, api }
    }

    /// Submits `body` to `/api/chat/send`, which answers when the turn is done.
    fn send(&self, body: Value) -> (u16, Value) {
        let address = self.api.server_addr().to_ip().unwrap();
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
        let body = body.to_string();
        write!(stream, "POST /api/chat/send HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let (head, body) = response.split_once("\r\n\r\n").unwrap();
        let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        (status, serde_json::from_str(body).unwrap_or(json!(body)))
    }

    fn turns(&self) -> Vec<(String, String, String)> {
        let connection = self.db.lock().unwrap();
        let mut query = connection.prepare("SELECT request_id,message,status FROM native_chat_turns WHERE chat_id=? ORDER BY sequence").unwrap();
        let rows = query.query_map([CHAT], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap()
            .collect::<Result<_, _>>().unwrap();
        rows
    }

    /// The pinned configuration of the turn of `request`.
    fn configuration(&self, request: &str) -> Value {
        crate::ui_persistence::configuration_for_turn(&self.db, &webui::turn_id(CHAT, request)).unwrap().unwrap()
    }

    fn used_model_calls(&self) -> u32 {
        self.db.lock().unwrap()
            .query_row("SELECT used_model_calls FROM ui_request_budgets WHERE id=?", [&self.budget], |row| row.get(0)).unwrap()
    }

    fn events(&self, turn: &str, event: &str) -> Vec<Value> {
        let connection = self.db.lock().unwrap();
        let mut query = connection.prepare("SELECT payload FROM ui_events WHERE turn_id=? AND event=? ORDER BY sequence").unwrap();
        let rows = query.query_map([turn, event], |row| row.get::<_, String>(0)).unwrap()
            .map(|row| serde_json::from_str(&row.unwrap()).unwrap()).collect();
        rows
    }

    fn outcome_ids(&self) -> Vec<String> {
        let connection = self.db.lock().unwrap();
        let contract = crate::ui_task_completion::load(&connection, CHAT, &self.budget).unwrap();
        serde_json::to_value(&contract).unwrap()["outcomes"].as_array().unwrap().iter()
            .map(|outcome| outcome["id"].as_str().unwrap().to_owned()).collect()
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

fn outcomes(id: &str) -> Value {
    json!({"outcomes":[{"id":id,"description":"Answer the user","kind":"answer","verify":"Non-empty answer","origin":"agent"}]})
}

fn checkpoint_messages() -> Vec<Value> {
    vec![json!({"role":"system","content":"Saved turn context"}), json!({"role":"user","content":PROMPT})]
}

/// The first turn stopped while `record_required_outcomes` was running.
fn in_flight_outcomes() -> Vec<Value> {
    let calls = json!([{"id":"rec-1","name":"record_required_outcomes","arguments":outcomes("notes")}]);
    vec![
        json!({"kind":"model","messages":checkpoint_messages()}),
        json!({"kind":"tools","calls":calls}),
        json!({"kind":"tools_dispatched","calls":calls}),
    ]
}

fn resume_body(id: &str) -> Value {
    json!({"session_id":CHAT,"client_request_id":id,"message":"Resume task","resume":true,"model":"fixture"})
}

#[test]
fn resume_rehydrates_an_oversized_image_batch_from_the_last_tool_checkpoint() {
    let url = format!("data:image/png;base64,{}", "A".repeat(512 * 1024));
    let frames: Vec<Value> = (0..18).map(|i| json!({"role":"user","content":[
        {"type":"text","text":format!("frame {i}")},{"type":"image_url","image_url":{"url":url,"detail":"high"}}
    ]})).collect();
    assert!(serde_json::to_vec(&frames).unwrap().len() > 8 * 1024 * 1024);
    let calls = json!([{"id":"inspect","name":"view_image","arguments":{"path":"frame.png"}}]);
    let fixture = Fixture::new(&[
        json!({"kind":"model","messages":checkpoint_messages()}),
        json!({"kind":"tools","calls":calls}),
        json!({"kind":"tool_results","results":[{"role":"tool","tool_call_id":"inspect","content":"Frames extracted."}],"after":frames})
    ]);
    {
        let connection = fixture.db.lock().unwrap();
        let stored: String = connection.query_row("SELECT messages FROM ui_turn_checkpoints WHERE turn_id=?",
            [&fixture.turn], |r| r.get(0)).unwrap();
        assert!(stored.len() < 10_000);
        let count: usize = connection.query_row("SELECT COUNT(*) FROM ui_checkpoint_images WHERE chat_id=?",
            [CHAT], |r| r.get(0)).unwrap();
        assert_eq!(count, 1);
        let mut messages: Vec<Value> = serde_json::from_str(&stored).unwrap();
        assert!(crate::ui_turn_checkpoints::restore_images(&connection, "another-chat", &mut messages).is_err());
        crate::ui_turn_checkpoints::restore_images(&connection, CHAT, &mut messages).unwrap();
        assert!(messages.ends_with(&frames));
    }
    let (status, done) = fixture.send(resume_body("resume-images"));
    assert_eq!(status, 200, "{done}");
    assert_eq!(done["status"], "success", "{done}");
    let requests = fixture.provider.requests();
    let messages = requests[0]["messages"].as_array().unwrap();
    let images: Vec<_> = messages.iter().filter(|m| m["content"].as_array()
        .is_some_and(|parts| parts.iter().any(|part| part["type"] == "image_url"))).collect();
    assert!(!images.is_empty() && images.len() < frames.len());
    let retained = &frames[frames.len() - images.len()..];
    assert!(images.into_iter().zip(retained).all(|(actual, expected)| actual["content"] == expected["content"]));
    assert!(messages.iter().any(|message| message["content"].as_array().is_some_and(|parts|
        parts.iter().any(|part| part["text"].as_str().is_some_and(|s| s.starts_with("[Image omitted"))))));
}

#[test]
fn resume_accepts_inline_images_written_by_an_older_host() {
    let fixture = Fixture::new(&[json!({"kind":"model","messages":checkpoint_messages()})]);
    let mut messages = checkpoint_messages();
    let url = "data:image/png;base64,AA==";
    messages.push(json!({"role":"user","content":[{"type":"image_url","image_url":{"url":url}}]}));
    fixture.db.lock().unwrap().execute("UPDATE ui_turn_checkpoints SET messages=? WHERE turn_id=?",
        params![serde_json::to_string(&messages).unwrap(),fixture.turn]).unwrap();
    let (status, result) = fixture.send(resume_body("resume-old-image"));
    assert_eq!(status, 200);
    assert_eq!(result["status"], "success", "{result}");
    let requests = fixture.provider.requests();
    assert!(requests[0]["messages"].as_array().unwrap().iter()
        .any(|message| message["content"][0]["image_url"]["url"] == url));
}

#[test]
fn resume_continues_the_same_request_and_dispatches_in_flight_task_state_again() {
    let fixture = Fixture::new(&in_flight_outcomes());
    let (status, done) = fixture.send(resume_body("resume-1"));
    assert_eq!(status, 200, "{done}");
    assert_eq!(done["status"], "success", "{done}");

    // A new turn of the same logical request: the pinned configuration, with the same
    // request budget, continues from the stopped turn.
    let turns = fixture.turns();
    assert_eq!(turns.len(), 2, "{turns:?}");
    assert_eq!((turns[1].0.as_str(), turns[1].2.as_str()), ("resume-1", "completed"), "{turns:?}");
    let configuration = fixture.configuration("resume-1");
    assert_eq!(configuration["request_budget_id"], json!(fixture.budget));
    assert_eq!(configuration["chat_resume_source"], json!(fixture.turn));
    assert_eq!(configuration["resume_checkpoint"], true);

    // The model continued from the checkpoint, with the in-flight call resolved.
    let requests = fixture.provider.requests();
    assert!(!requests.is_empty());
    let messages = requests[0]["messages"].as_array().unwrap();
    assert_eq!(messages.iter().filter(|m| m["content"] == "Saved turn context").count(), 1, "{messages:?}");
    assert!(!messages.iter().any(|m| m["content"] == "Resume task"), "the button text is not a new objective");
    let result: Vec<&Value> = messages.iter().filter(|m| m["role"] == "tool" && m["tool_call_id"] == "rec-1").collect();
    assert_eq!(result.len(), 1, "{messages:?}");
    assert!(result[0]["content"].as_str().unwrap().contains("required_outcomes"), "{}", result[0]);
    assert!(messages.last().unwrap()["content"].as_str().unwrap().contains("asked to resume"));

    // record_required_outcomes ran again in the new turn, against the request's contract,
    // which kept the outcome recorded before the stop.
    let new_turn = webui::turn_id(CHAT, "resume-1");
    assert!(fixture.events(&new_turn, "event").iter().any(|event| event["kind"] == "recovery"
        && event["payload"]["tool_call_id"] == "rec-1" && event["payload"]["resolution"] == "HostState"));
    let mut ids = fixture.outcome_ids();
    ids.sort();
    assert_eq!(ids, ["notes", "summary"]);

    // Model calls keep counting against the same budget, and RecoveryState carries over.
    assert_eq!(fixture.used_model_calls(), USED_BEFORE + requests.len() as u32);
    let connection = fixture.db.lock().unwrap();
    let recovery = crate::ui_persistence::recovery(&connection, &configuration, CHAT).unwrap();
    assert!(recovery["evidence"].as_array().unwrap().contains(&json!("pre-resume-evidence")), "{recovery}");
}

#[test]
fn explicit_resume_reopens_a_bounded_stall_without_losing_original_evidence() {
    let fixture=Fixture::new(&in_flight_outcomes());
    let configuration=fixture.configuration(REQUEST);
    crate::ui_persistence::save_recovery(&fixture.db.lock().unwrap(),&configuration,CHAT,
        &json!({"version":2,"ladder":"conclude","stalled_batches":32,"conclude_batches":2,
            "evidence":["original-action-receipt"]})).unwrap();
    let (status,done)=fixture.send(resume_body("resume-stall"));
    assert_eq!(status,200,"{done}");
    assert_eq!(done["status"],"success","{done}");
    assert!(!fixture.provider.requests().is_empty(),"explicit continuation must be able to observe again");
    let resumed=fixture.configuration("resume-stall");
    let state=crate::ui_persistence::recovery(&fixture.db.lock().unwrap(),&resumed,CHAT).unwrap();
    assert_eq!(state["conclude_batches"],0);
    assert!(state["evidence"].as_array().unwrap().contains(&json!("original-action-receipt")));
    assert_eq!(resumed["request_budget_id"],configuration["request_budget_id"]);
}

#[test]
fn typed_resume_text_starts_a_new_request() {
    let fixture = Fixture::new(&in_flight_outcomes());
    let (status, done) = fixture.send(json!({"session_id":CHAT,"client_request_id":"typed-1","message":"Resume task","model":"fixture"}));
    assert_eq!(status, 200, "{done}");
    let configuration = fixture.configuration("typed-1");
    // A new request: its own budget, no continuation, and the text is the objective.
    assert_ne!(configuration["request_budget_id"], json!(fixture.budget));
    assert!(configuration["request_budget_id"].is_string());
    assert!(configuration["chat_resume_source"].is_null() && configuration["resume_checkpoint"].is_null(), "{configuration}");
    let requests = fixture.provider.requests();
    // The acknowledgment uses the same provider concurrently and may arrive
    // first. Check the actual executor request, not that diagnostic preamble.
    let messages = requests.iter().filter_map(|r|r["messages"].as_array()).find(|messages|
        messages.iter().rev().find(|m|m["role"]=="user").is_some_and(|m|m["content"]=="Resume task"))
        .expect("executor received the new objective");
    assert_eq!(messages.iter().rev().find(|m| m["role"] == "user").unwrap()["content"], "Resume task");
    assert!(!messages.iter().any(|m| m["role"] == "tool" && m["tool_call_id"] == "rec-1"));
    assert_eq!(fixture.used_model_calls(), USED_BEFORE, "the stopped request's budget is untouched");
}

#[test]
fn resume_with_an_exhausted_budget_is_refused_without_a_new_request() {
    let fixture = Fixture::new(&in_flight_outcomes());
    fixture.db.lock().unwrap()
        .execute("UPDATE ui_request_budgets SET used_model_calls=max_model_calls WHERE id=?", [&fixture.budget]).unwrap();
    let (status, refused) = fixture.send(resume_body("resume-1"));
    assert_eq!(status, 409, "{refused}");
    assert_eq!(refused["admission"], "rejected");
    let error = refused["error"].as_str().unwrap();
    assert!(error.contains("model calls") && error.contains("Start a new request"), "{error}");
    assert_eq!(fixture.turns().len(), 1, "no request was started");
    assert!(fixture.provider.requests().is_empty());
    // The session view shows the same reason instead of the button.
    let connection = fixture.db.lock().unwrap();
    let state = crate::ui_job_controls::chat_resume_state(&connection, &fixture.turn, "interrupted").unwrap();
    assert_eq!(state["allowed"], false);
    assert_eq!(state["reason"], error);
}

#[test]
fn resume_after_an_uncertain_stop_is_refused() {
    let fixture = Fixture::new(&in_flight_outcomes());
    fixture.db.lock().unwrap().execute("INSERT OR REPLACE INTO ui_runtime_results VALUES (?,'failed',?)",
        params![fixture.turn, json!({"uncertain":true,"failure_code":"tool_outcome_unconfirmed"}).to_string()]).unwrap();
    let (status, refused) = fixture.send(resume_body("resume-1"));
    assert_eq!(status, 409, "{refused}");
    assert!(refused["error"].as_str().unwrap().contains("original receipt"), "{refused}");
    assert_eq!(fixture.turns().len(), 1);
    assert!(fixture.provider.requests().is_empty());
}

#[test]
fn resume_without_a_resumable_checkpoint_is_refused() {
    // The stopped turn wrote no checkpoint.
    let fixture = Fixture::new(&[]);
    let (status, refused) = fixture.send(resume_body("resume-1"));
    assert_eq!(status, 409, "{refused}");
    assert!(refused["error"].as_str().unwrap().contains("no portable checkpoint"), "{refused}");
    assert_eq!(fixture.turns().len(), 1);
    assert!(fixture.provider.requests().is_empty());
}

#[test]
fn only_the_explicit_resume_fields_select_a_source() {
    use crate::ui_job_controls::resume_source;
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE native_chat_turns(sequence INTEGER PRIMARY KEY,chat_id TEXT,request_id TEXT,message TEXT,status TEXT);").unwrap();
    let message = |extra: Value| {
        let mut body = json!({"session_id":"c","client_request_id":"new","message":"Resume task"});
        body.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        body
    };
    // Nothing to resume: a resume is refused, plain text is a new request.
    assert_eq!(resume_source(&db, "c", "new", &message(json!({"resume":true}))).unwrap_err().0, 409);
    assert_eq!(resume_source(&db, "c", "new", &message(json!({}))).unwrap(), None);
    db.execute_batch("INSERT INTO native_chat_turns(chat_id,request_id,message,status) VALUES
        ('c','first','Do it','interrupted'),('c','latest','More','failed'),('other','x','Else','failed'),('c','new','Resume task','running');").unwrap();
    assert_eq!(resume_source(&db, "c", "new", &message(json!({}))).unwrap(), None, "text is never classified");
    assert_eq!(resume_source(&db, "c", "new", &message(json!({"resume":false}))).unwrap(), None);
    assert_eq!(resume_source(&db, "c", "new", &message(json!({"resume":true}))).unwrap(), Some(webui::turn_id("c", "latest")));
    assert_eq!(resume_source(&db, "c", "new", &message(json!({"resume":true,"resume_turn_id":"named"}))).unwrap(), Some("named".into()));
    assert_eq!(resume_source(&db, "c", "new", &message(json!({"resume":"yes"}))).unwrap_err().0, 400);
    assert_eq!(resume_source(&db, "c", "new", &message(json!({"resume_turn_id":7}))).unwrap_err().0, 400);
}
