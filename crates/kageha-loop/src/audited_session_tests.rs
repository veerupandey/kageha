//! Regression test for the audited session, chat `6a4177e69258` ("Help me prepare for
//! system design interview for seequent"). Spec: autonomous-agent-loop, task 8.1; design
//! "Testing Strategy" > "Regression test for the audited session"; Requirements 4.4, 6.5,
//! 7.5 and 8.4.
//!
//! A scripted `ModelPort` replays the model's decisions from that chat against the real
//! Host: `ui_tools::Tools` (catalog, specialist tool policy, `record_required_outcomes`,
//! `write_file`, `record_outcome_evidence`), the real Completion_Gate in
//! `ui_task_completion`, and `kageha_runtime::execution::execute_turn` with the request's
//! persistence policy enabled. Only the model is scripted.
//!
//! This lives inside the crate rather than in `tests/` because `Tools`, the gate and the
//! hosted `ToolPort` adapter are `pub(crate)`. The turn runs through
//! `native_chat::execute_hosted_with_model`, the same wiring as the local execution path
//! with the provider model replaced.
use crate::native_chat::{self, ToolHost};
use crate::turn::Start;
use crate::ui_tools::{ObservationCache, Tools};
use crate::webui::{self, Controls, Database};
use kageha_runtime::execution::{Control, ToolCall};
use kageha_runtime::lifecycle::RunStore;
use kageha_runtime::test_support::{text_reply, tool_call, tool_calls_reply, ScriptedModel};
use serde_json::{json, Value};
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

pub(crate) const CHAT: &str = "6a4177e69258";
pub(crate) const REQUEST: &str = "request-1";
const PROMPT: &str = "Help me prepare for system design interview for seequent (check my email)";
/// The runtime's generic stop text, used only when no candidate answer exists.
const GENERIC_STOP: &str = "I stopped because";

/// A ~10 KB interview-prep answer, like the 10–16 KB answers regenerated in the audit.
fn audited_answer() -> String {
    let mut answer = String::from("# Seequent system design interview prep\n\n");
    let mut topic = 0;
    while answer.len() < 10 * 1024 {
        topic += 1;
        answer.push_str(&format!(
            "## Topic {topic}: geoscience data pipeline\n\
             - Ingest borehole and survey data from many sources; discuss idempotent uploads and versioning.\n\
             - Model storage for large 3D meshes: object storage plus a metadata index, chunked by spatial tile.\n\
             - Practice question: design collaborative editing for a shared geological model with conflict resolution.\n\n"
        ));
    }
    answer
}

/// The Host's state for one chat turn: a file-backed WebUI database (the Task_Workspace
/// lives under the database's directory), the chat session, and a staged run whose
/// configuration has the request persistence policy enabled.
pub(crate) struct Fixture {
    _directory: tempfile::TempDir,
    chat: &'static str,
    pub(crate) db: Database,
    project: PathBuf,
    pub(crate) turn: String,
    controls: Controls,
    cancel: AtomicBool,
}

impl Fixture {
    pub(crate) fn new() -> Self {
        Self::for_chat(CHAT)
    }

    pub(crate) fn for_chat(chat: &'static str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let project = directory.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let connection = rusqlite::Connection::open(directory.path().join("native.sqlite")).unwrap();
        connection.execute_batch("CREATE TABLE native_chat_turns(sequence INTEGER PRIMARY KEY,chat_id TEXT,request_id TEXT,message TEXT,answer TEXT,status TEXT);").unwrap();
        webui::initialize(&connection).unwrap();
        connection.execute("INSERT INTO ui_sessions(id,title,created_at,updated_at) VALUES (?,?,0,0)", [chat, "Seequent prep"]).unwrap();
        crate::ui_approvals::main_default(&connection, chat, "main").unwrap();
        // Admission pins the default persistence policy and opens the request budget.
        let turn = webui::turn_id(chat, REQUEST);
        let mut configuration = json!({});
        crate::ui_persistence::admit(&connection, &turn, &mut configuration).unwrap();
        assert_eq!(configuration["persistence"]["enabled"], true);
        let spec = crate::runtime_host::spec(&project, chat, REQUEST, &turn, "main", PROMPT, 0, None, None,
            vec![json!({"role":"user","content":PROMPT})], configuration, 64);
        assert_eq!(spec.turn, turn);
        crate::runtime_host::prepare(&connection, &spec).unwrap();
        let db = crate::agent_host::DatabaseState::open(connection).unwrap();
        crate::runtime_host::store(&db).unwrap().stage(&spec, 64).unwrap();
        // The write path pauses below 5 GiB free (Requirement 3.7); this test is about the
        // gate, so it must not depend on the test machine's free space.
        let root = crate::ui_workspace::open_for_chat(&db, chat).unwrap().root().to_path_buf();
        crate::ui_tools::test_free_disk::set(&root, u64::MAX);
        Self { _directory: directory, chat, db, project, turn, controls: Controls::default(), cancel: AtomicBool::new(false) }
    }

    /// The real Host tool set for this turn, with `persona`'s specialist tool policy.
    pub(crate) fn tools(&self, persona: Value) -> Tools<'_> {
        Tools {
            db: &self.db,
            chat: self.chat,
            turn: &self.turn,
            workspace: &self.project,
            cancel: &self.cancel,
            acknowledgment: None,
            news_synthesis: AtomicBool::new(false),
            capability_discoveries: Default::default(),
            observations: std::sync::Mutex::new(ObservationCache::default()),
            unavailable_sources: Default::default(),
            read_blockers: Default::default(),
            controls: &self.controls,
            persona,
            knowledge_scope: Value::Null,
            providers: Vec::new(),
            role_providers: Default::default(),
            depth: 0,
            remote: Value::Null,
            machine: None,
            media_preferences: Value::Null,
            browser_constraints: Value::Null,
            workflow: Value::Null,
            resume_receipts: Vec::new(),
            browser_decisions: crate::ui_browser_decisions::BrowserDecisions::new(&self.db, self.chat, &self.turn, &Value::Null),
            spilled_outputs: Default::default(),
            attachments: Default::default(),
        }
    }

    /// Runs one turn of the user's prompt and returns the final message and every frame.
    pub(crate) fn run(&self, model: &ScriptedModel, host: &dyn ToolHost) -> (String, Vec<Value>) {
        let mut frames = Vec::new();
        let start = Start { version: 1, max_model_calls: 64, messages: vec![json!({"role":"user","content":PROMPT})] };
        let answer = native_chat::execute_hosted_with_model(&self.project, start, model, host, &self.cancel, |frame| {
            // As in `webui::execute`; journaling is off unless a test installs a learning
            // store. The gate decision's event comes from the runtime's frame.
            crate::ui_learning::observe(&self.db, &self.turn, frame);
            if frame["kind"] == "completion_check" {
                webui::record_gate_decision(&self.db, self.chat, &self.turn, frame)?;
            }
            frames.push(frame.clone());
            Ok(())
        })
        .unwrap();
        (answer, frames)
    }

    fn rejected_answers(&self) -> Vec<(i64, String, Option<String>)> {
        let connection = self.db.lock().unwrap();
        let mut query = connection
            .prepare("SELECT n,content,draft_path FROM ui_rejected_answers WHERE chat_id=? ORDER BY n")
            .unwrap();
        query
            .query_map([self.chat], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    /// Payloads of the Host `event` rows of one kind, in order.
    pub(crate) fn events(&self, kind: &str) -> Vec<Value> {
        let connection = self.db.lock().unwrap();
        let mut query = connection
            .prepare("SELECT payload FROM ui_events WHERE chat_id=? AND event='event' AND json_extract(payload,'$.kind')=? ORDER BY sequence")
            .unwrap();
        query
            .query_map([self.chat, kind], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|text| serde_json::from_str(&text.unwrap()).unwrap())
            .collect()
    }

    fn workspace_root(&self) -> PathBuf {
        crate::ui_workspace::open_for_chat(&self.db, self.chat).unwrap().root().to_path_buf()
    }
}

pub(crate) fn main_persona(tool_policy: Value) -> Value {
    json!({"id":"main","name":"Kageha","tool_policy":tool_policy})
}

/// The tool result the runtime returned to the model for `call_id`.
fn tool_result(frames: &[Value], call_id: &str) -> Value {
    frames
        .iter()
        .filter(|frame| frame["kind"] == "tool_results")
        .flat_map(|frame| frame["results"].as_array().cloned().unwrap_or_default())
        .find(|result| result["tool_call_id"] == call_id)
        .unwrap_or_else(|| panic!("no tool result for {call_id}"))
}

fn tool_json(frames: &[Value], call_id: &str) -> Value {
    let result = tool_result(frames, call_id);
    let content = result["content"].as_str().unwrap();
    serde_json::from_str(content).unwrap_or_else(|_| panic!("{call_id} returned non-JSON: {content}"))
}

fn task_status(frames: &[Value]) -> Value {
    frames
        .iter()
        .rev()
        .find(|frame| frame["kind"] == "task_completion")
        .map(|frame| frame["status"].clone())
        .unwrap_or(Value::Null)
}

fn gate_decisions(frames: &[Value]) -> Vec<String> {
    frames
        .iter()
        .filter(|frame| frame["kind"] == "completion_check")
        .map(|frame| frame["status"].as_str().unwrap_or("").to_owned())
        .collect()
}

/// Gate rejections: candidates the Completion_Gate sent back with `continue`.
fn rejections(frames: &[Value]) -> usize {
    gate_decisions(frames).iter().filter(|status| *status == "continue").count()
}

/// The three deliverables the audited model declared as `artifact` outcomes.
fn audited_outcomes() -> Value {
    json!({"task_type":"interview-prep","outcomes":[
        {"id":"prep-brief","description":"Interview prep brief for the Seequent system design interview","kind":"artifact","verify":"outputs/prep-brief.md exists"},
        {"id":"question-bank","description":"Bank of likely system design questions with answer outlines","kind":"artifact","verify":"outputs/question-bank.md exists"},
        {"id":"design-diagram","description":"Architecture diagram for a geoscience data platform","kind":"artifact","verify":"outputs/design-diagram.md exists"}]})
}

/// Scenario 1, the audited turn: no tool can write a file, so the declared artifact
/// outcomes are retained as blocked with `outcome_unsatisfiable`. The model can
/// return its partial answer once without the missing deliverables disappearing.
#[test]
fn without_file_tools_artifact_outcomes_are_unsatisfiable_and_the_answer_is_delivered() {
    let fixture = Fixture::new();
    let host = fixture.tools(main_persona(json!({"deny":["write_file","edit_file","generate_image","text_to_speech","shell"]})));
    let offered: Vec<String> = host.specs().iter().filter_map(|spec| spec["function"]["name"].as_str().map(str::to_owned)).collect();
    for denied in ["write_file", "edit_file", "generate_image", "text_to_speech"] {
        assert!(!offered.iter().any(|name| name == denied), "{denied} must not be offered: {offered:?}");
    }
    for unavailable in ["agent_handoff_preview", "agent_handoff_apply"] {
        assert!(!offered.iter().any(|name| name == unavailable), "{unavailable} requires file access: {offered:?}");
    }
    assert!(offered.iter().any(|name| name == "record_required_outcomes"));
    let answer = audited_answer();
    let model = ScriptedModel::new([tool_calls_reply([tool_call("record-1", "record_required_outcomes", audited_outcomes())])]);
    for _ in 0..4 {
        model.push_text(answer.clone());
    }

    let (message, frames) = fixture.run(&model, &host);

    // Every artifact outcome is rejected at declaration with the missing capability.
    let recorded = tool_json(&frames, "record-1");
    assert_eq!(recorded["accepted"], json!([]), "{recorded}");
    let rejected: Vec<_> = recorded["rejected"].as_array().unwrap().iter()
        .map(|item| (item["id"].as_str().unwrap().to_owned(), item["code"].as_str().unwrap().to_owned())).collect();
    assert_eq!(rejected, [("prep-brief", "outcome_unsatisfiable"), ("question-bank", "outcome_unsatisfiable"), ("design-diagram", "outcome_unsatisfiable")]
        .map(|(id, code)| (id.to_owned(), code.to_owned())));
    for item in recorded["rejected"].as_array().unwrap() {
        assert!(!item["missing_capability"].as_str().unwrap_or("").is_empty(), "{item}");
        let expected = if offered.iter().any(|name| name == "computer_execute") {
            json!(["answer", "action"])
        } else {
            json!(["answer"])
        };
        assert_eq!(item["alternatives"], expected, "{item}");
    }
    assert_eq!(recorded["required_outcomes"].as_array().unwrap().len(), 3);
    assert_eq!(fixture.events("outcome_rejected").len(), 3);
    // Unprovable outcomes remain visible as blocked, without a fabricated failed call.
    let rejections = rejections(&frames);
    assert!(rejections <= 2, "gate rejected {rejections} times: {:?}", gate_decisions(&frames));
    assert_eq!(fixture.rejected_answers().len(), 1, "the partial answer is retained with the blocked decision");
    let answers_used = 4 - model.remaining();
    assert!((1..=3).contains(&answers_used), "the model answered {answers_used} times");
    assert_eq!(model.requests().len(), 1 + answers_used);
    assert!(message.contains(answer.trim()), "the final message must contain the answer");
    assert!(!message.contains(GENERIC_STOP), "{message}");
    assert_eq!(answers_used, 1);
    assert!(message.starts_with(answer.trim()) && message.contains("Still unmet:") && message.contains("prep-brief"));
    assert_eq!(task_status(&frames), "blocked");
}

/// A repeated final answer with the same unmet artifact stops without
/// burning the remaining request budget or generating another draft.
#[test]
fn duplicate_rejected_answer_preserves_the_unmet_artifact() {
    let fixture = Fixture::new();
    let host = fixture.tools(main_persona(Value::Null));
    let answer = audited_answer();
    let model = ScriptedModel::new([tool_calls_reply([tool_call("record-1", "record_required_outcomes",
        json!({"outcomes":[audited_outcomes()["outcomes"][0].clone()]}))])]);
    for _ in 0..4 {
        model.push_text(answer.clone());
    }

    let (message, frames) = fixture.run(&model, &host);

    assert_eq!(tool_json(&frames, "record-1")["accepted"], json!(["prep-brief"]));
    assert_eq!(gate_decisions(&frames), ["continue", "continue"]);
    assert_eq!(model.remaining(), 2, "the model answered {} times", 4 - model.remaining());
    // The second request carries the rejected answer before the rejection instruction (8.1).
    let second = &model.requests()[2].messages;
    let kept = second.iter().position(|m| m["role"] == "assistant" && m["content"] == answer.as_str()).expect("rejected answer kept");
    assert!(second[kept + 1..].iter().any(|m| m["content"].as_str().is_some_and(|text| text.contains("report_blocker"))));
    assert!(frames.iter().any(|frame| frame["kind"] == "completion_check" && frame["stop_reason"] == "duplicate_candidate"));
    assert!(message.starts_with(answer.trim()), "the final message must start with the answer");
    assert!(!message.contains(GENERIC_STOP), "{message}");
    let unmet = &message[answer.trim().len()..];
    assert!(unmet.contains("Still unmet:") && unmet.contains("prep-brief"), "{unmet}");
    assert_eq!(task_status(&frames), "blocked");
    // Both rejected answers were kept; the long ones also as workspace drafts (8.3).
    let kept = fixture.rejected_answers();
    assert_eq!(kept.len(), 2);
    assert!(kept.iter().all(|(_, content, draft)| content == &answer && draft.as_deref() == Some("workspace/drafts/answer-1.md")));
    assert!(fixture.workspace_root().join("drafts/answer-1.md").is_file());
}

/// Scenario 2: with file tools the model saves the deliverable under `outputs/`,
/// attaches the returned `artifact_path` as evidence, and the gate returns Ready.
#[test]
fn with_file_tools_a_written_and_attached_artifact_makes_the_gate_ready() {
    let fixture = Fixture::new();
    let host = fixture.tools(main_persona(Value::Null));
    let answer = audited_answer();
    let model = ScriptedModel::new([
        tool_calls_reply([tool_call("record-1", "record_required_outcomes",
            json!({"task_type":"interview-prep","outcomes":[audited_outcomes()["outcomes"][0].clone()]}))]),
        tool_calls_reply([tool_call("write-1", "write_file", json!({"path":"outputs/prep-brief.md","content":answer}))]),
        tool_calls_reply([tool_call("evidence-1", "record_outcome_evidence",
            json!({"outcome_id":"prep-brief","reference":"workspace/outputs/prep-brief.md"}))]),
        text_reply(format!("{}\n\nSaved as outputs/prep-brief.md.", answer.trim())),
    ]);

    let (message, frames) = fixture.run(&model, &host);

    assert_eq!(tool_json(&frames, "record-1")["accepted"], json!(["prep-brief"]));
    let written = tool_json(&frames, "write-1");
    // The script attached exactly the artifact_path write_file returned.
    assert_eq!(written["artifact_path"], "workspace/outputs/prep-brief.md", "{written}");
    assert_eq!(std::fs::read_to_string(fixture.workspace_root().join("outputs/prep-brief.md")).unwrap(), answer);
    assert_eq!(tool_json(&frames, "evidence-1")["verified_reference"], "workspace/outputs/prep-brief.md");
    assert_eq!(model.remaining(), 0);
    assert_eq!(rejections(&frames), 0);
    assert!(fixture.rejected_answers().is_empty());
    let checks = fixture.events("completion_check");
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0]["payload"]["status"], "ready");
    assert_eq!(checks[0]["payload"]["evidence"], json!(["workspace/outputs/prep-brief.md"]));
    assert_eq!(task_status(&frames), "verified");
    assert!(message.starts_with(answer.trim()) && !message.contains("Still unmet"), "{message}");
}

/// Wraps the real Host tools and answers Gmail reads itself, counting how many reach
/// the connector. The read cache (task 13.1) sits above `invoke` in the hosted
/// `ToolPort`, so cache hits never reach this count.
struct CountingGmail<'a> {
    tools: &'a Tools<'a>,
    dispatches: AtomicUsize,
}

impl ToolHost for CountingGmail<'_> {
    fn persistence_policy(&self) -> io::Result<Option<kageha_contracts::execution::PersistencePolicy>> { self.tools.persistence_policy() }
    fn recovery_checkpoint(&self) -> io::Result<Value> { self.tools.recovery_checkpoint() }
    fn save_recovery(&self, state: &Value) -> io::Result<()> { self.tools.save_recovery(state) }
    fn progress_inputs(&self) -> io::Result<kageha_contracts::execution::ProgressInputs> { self.tools.progress_inputs() }
    fn plan_snapshot(&self) -> io::Result<Option<String>> { self.tools.plan_snapshot() }
    fn completion_check(&self, candidate: &str, control: &Control<'_>) -> io::Result<kageha_contracts::execution::CompletionCheck> {
        self.tools.completion_check(candidate, control)
    }
    fn before_tool(&self, call: &ToolCall, control: &Control<'_>) -> io::Result<()> { self.tools.before_tool(call, control) }
    fn recovery_revision(&self, call: &ToolCall) -> io::Result<Option<String>> { self.tools.recovery_revision(call) }
    fn read_cache(&self, call: &ToolCall) -> crate::ui_read_cache::Plan { self.tools.read_cache(call) }
    fn dispatch_access(&self, call: &ToolCall) -> io::Result<kageha_runtime::execution::DispatchAccess> { self.tools.dispatch_access(call) }
    fn specs(&self) -> Vec<Value> { self.tools.specs() }
    fn steering(&self) -> io::Result<Vec<String>> { self.tools.steering() }
    fn invoke(&self, call: &Value) -> io::Result<String> {
        if call["name"] == "integration_read" && call["arguments"]["integration"] == "gmail" {
            self.dispatches.fetch_add(1, Ordering::SeqCst);
            return Ok(json!({"id":"m-seequent","subject":"Seequent interview: system design round","from":"recruiting@seequent.example",
                "body":"Your system design interview is on Thursday. Expect a 60 minute design of a geoscience data platform."}).to_string());
        }
        self.tools.invoke(call)
    }
}

/// Scenario 3: the audit re-read the same Gmail message about 37 times. With the read
/// cache, at most 4 of 37 identical reads (one per parallel dispatch slot) reach the
/// connector and the rest are cache hits (Requirement 12.1).
#[test]
fn identical_gmail_reads_are_served_from_the_read_cache() {
    let fixture = Fixture::new();
    let tools = fixture.tools(main_persona(Value::Null));
    let host = CountingGmail { tools: &tools, dispatches: AtomicUsize::new(0) };
    let read = json!({"integration":"gmail","operation":"message","id":"m-seequent"});
    // Batches of at most 16 calls (the runtime's batch limit): 16 + 16 + 5 = 37 reads.
    let mut replies = Vec::new();
    let mut next = 0;
    for size in [16, 16, 5] {
        replies.push(tool_calls_reply((0..size).map(|_| {
            next += 1;
            tool_call(format!("gmail-{next}"), "integration_read", read.clone())
        })));
    }
    replies.push(text_reply(audited_answer()));
    let model = ScriptedModel::new(replies);

    let (message, frames) = fixture.run(&model, &host);

    let results: usize = frames.iter().filter(|frame| frame["kind"] == "tool_results")
        .map(|frame| frame["results"].as_array().map_or(0, Vec::len)).sum();
    assert_eq!(results, 37);
    let dispatches = host.dispatches.load(Ordering::SeqCst);
    assert!(dispatches <= 4, "{dispatches} of 37 identical Gmail reads reached the connector");
    // Hits carry the original result plus the cached marker.
    let hit = tool_json(&frames, "gmail-37");
    assert_eq!((hit["cached"].clone(), hit["subject"].clone()), (json!(true), json!("Seequent interview: system design round")), "{hit}");
    assert_eq!(message, audited_answer());
}
