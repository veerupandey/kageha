//! Turn checkpoints for chat turns (Requirement 13.1, design section 12).
//!
//! The turn observer calls [`observe`] with every runtime frame. It adds the Host data
//! a frame does not carry (the RecoveryState snapshot before a model call, the
//! Capability effect of each dispatched call) and writes one `ui_turn_checkpoints`
//! upsert per step through [`crate::ui_job_controls::record`]:
//!
//! | `step` | frame | written |
//! |---|---|---|
//! | `model` | `model` | before each model call |
//! | `tools_dispatched` | `tools_dispatched` | after admission, before dispatch |
//! | `tools_done` | `tool_results` | after the results are recorded |
//! | `gate` | `gate` | after each Completion_Gate decision |
//!
//! A turn does not continue without durability: a failed write ends it with
//! [`CheckpointFailed`].
use crate::native_chat::ToolHost;
use crate::ui_tools::{Effect, Tools};
use crate::webui::Database;
use serde_json::{json, Map, Value};
use std::{fmt, io};

/// A turn checkpoint could not be written; the turn stops.
#[derive(Debug)]
pub(crate) struct CheckpointFailed {
    step: String,
    cause: String,
}
impl fmt::Display for CheckpointFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Turn checkpoint `{}` could not be saved: {}", self.step, self.cause)
    }
}
impl std::error::Error for CheckpointFailed {}

pub(crate) fn is_failure(error: &io::Error) -> bool {
    error.get_ref().is_some_and(|cause| cause.is::<CheckpointFailed>())
}

fn failed(step: &str, error: io::Error) -> io::Error {
    if is_failure(&error) {
        return error;
    }
    io::Error::other(CheckpointFailed { step: step.to_owned(), cause: error.to_string() })
}

pub(crate) fn effect_name(effect: Effect) -> &'static str {
    match effect {
        Effect::Read => "read",
        Effect::Workspace => "workspace",
        Effect::Action => "action",
        Effect::Validation => "validation",
    }
}

// Blob lookups are always scoped to the chat pinned by the host. A reference
// cannot designate a filesystem path, URL, or another chat's attachment.
const IMAGE_PREFIX: &str = "kageha-checkpoint-image:";

pub(crate) fn store_images(db: &rusqlite::Connection, chat: &str, messages: &mut [Value]) -> io::Result<()> {
    use sha2::{Digest, Sha256};
    for url in kageha_runtime::turn::image_urls_mut(messages) {
        let Some(data) = url.as_str().filter(|url| url.starts_with("data:image/")) else { continue };
        let digest = format!("{:x}", Sha256::digest(data.as_bytes()));
        let exists: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM ui_checkpoint_images WHERE chat_id=? AND digest=?)",
            rusqlite::params![chat, digest], |r| r.get(0)
        ).map_err(io::Error::other)?;
        if !exists {
            db.execute("INSERT INTO ui_checkpoint_images(chat_id,digest,data) VALUES (?,?,?)",
                rusqlite::params![chat,digest,data]).map_err(io::Error::other)?;
        }
        *url = json!(format!("{IMAGE_PREFIX}{digest}"));
    }
    Ok(())
}

pub(crate) fn restore_images(db: &rusqlite::Connection, chat: &str, messages: &mut [Value]) -> io::Result<()> {
    for url in kageha_runtime::turn::image_urls_mut(messages) {
        let Some(digest) = url.as_str().and_then(|url| url.strip_prefix(IMAGE_PREFIX)) else { continue };
        let data: String = db.query_row("SELECT data FROM ui_checkpoint_images WHERE chat_id=? AND digest=?",
            rusqlite::params![chat,digest], |row| row.get(0)).map_err(io::Error::other)?;
        *url = Value::String(data);
    }
    Ok(())
}

/// Writes the checkpoint for `frame` of `turn`. Frames without a checkpoint step are
/// ignored.
pub(crate) fn observe(db: &Database, turn: &str, host: &Tools<'_>, frame: &Value) -> io::Result<()> {
    let kind = frame["kind"].as_str().unwrap_or("");
    let annotations = match kind {
        "model" => json!({"recovery": host.recovery_checkpoint().map_err(|error| failed(kind, error))?}),
        "tools_dispatched" => {
            let effects: Map<String, Value> = frame["calls"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|call| {
                    let id = call["id"].as_str()?;
                    let capability = host.call_capability(call["name"].as_str().unwrap_or(""), &call["arguments"]);
                    Some((id.to_owned(), json!(effect_name(capability.effect))))
                })
                .collect();
            json!({"effects": effects})
        }
        _ => Value::Null,
    };
    crate::ui_job_controls::record(db, host.chat, turn, frame, &annotations).map_err(|error| failed(kind, error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_chat;
    use crate::turn::Start;
    use crate::ui_tools::ObservationCache;
    use crate::webui::{self, Controls};
    use kageha_runtime::lifecycle::RunStore;
    use kageha_runtime::test_support::{text_reply, tool_call, tool_calls_reply, ScriptedModel};
    use rusqlite::OptionalExtension;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;

    const CHAT: &str = "checkpoint-chat";
    const REQUEST: &str = "request-1";
    const PROMPT: &str = "Save a short note";

    /// A file-backed WebUI database with one chat and a staged run whose request
    /// persistence policy is enabled, so RecoveryState is saved.
    struct Fixture {
        _directory: tempfile::TempDir,
        db: Database,
        project: PathBuf,
        turn: String,
        controls: Controls,
        cancel: AtomicBool,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let project = directory.path().join("project");
            std::fs::create_dir(&project).unwrap();
            let connection = rusqlite::Connection::open(directory.path().join("native.sqlite")).unwrap();
            connection.execute_batch("CREATE TABLE native_chat_turns(sequence INTEGER PRIMARY KEY,chat_id TEXT,request_id TEXT,message TEXT,answer TEXT,status TEXT);").unwrap();
            webui::initialize(&connection).unwrap();
            connection.execute("INSERT INTO ui_sessions(id,title,created_at,updated_at) VALUES (?,?,0,0)", [CHAT, "Checkpoints"]).unwrap();
            crate::ui_approvals::main_default(&connection, CHAT, "main").unwrap();
            let turn = webui::turn_id(CHAT, REQUEST);
            let mut configuration = json!({});
            crate::ui_persistence::admit(&connection, &turn, &mut configuration).unwrap();
            let spec = crate::runtime_host::spec(&project, CHAT, REQUEST, &turn, "main", PROMPT, 0, None, None,
                vec![json!({"role":"user","content":PROMPT})], configuration, 16);
            crate::runtime_host::prepare(&connection, &spec).unwrap();
            let db = crate::agent_host::DatabaseState::open(connection).unwrap();
            crate::runtime_host::store(&db).unwrap().stage(&spec, 16).unwrap();
            let root = crate::ui_workspace::open_for_chat(&db, CHAT).unwrap().root().to_path_buf();
            crate::ui_tools::test_free_disk::set(&root, u64::MAX);
            Self { _directory: directory, db, project, turn, controls: Controls::default(), cancel: AtomicBool::new(false) }
        }

        fn tools(&self) -> Tools<'_> {
            Tools {
                db: &self.db,
                chat: CHAT,
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
                persona: json!({"id":"main","name":"Kageha"}),
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
                browser_decisions: crate::ui_browser_decisions::BrowserDecisions::new(&self.db, CHAT, &self.turn, &Value::Null),
                spilled_outputs: Default::default(),
                attachments: Default::default(),
            }
        }

        fn row(&self) -> Option<(String, String, String, Value)> {
            let connection = self.db.lock().unwrap();
            connection
                .query_row(
                    "SELECT step,phase,messages,state FROM ui_turn_checkpoints WHERE turn_id=?",
                    [&self.turn],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get::<_, String>(3)?)),
                )
                .optional()
                .unwrap()
                .map(|(step, phase, messages, state)| (step, phase, messages, serde_json::from_str(&state).unwrap()))
        }

        /// Runs the turn with the WebUI checkpoint observer. Returns the result and each
        /// new checkpoint step with its saved state, in write order.
        fn run(&self, model: &ScriptedModel, host: &Tools<'_>) -> (io::Result<String>, Vec<(String, Value)>) {
            let mut steps: Vec<(String, Value)> = Vec::new();
            let start = Start { version: 1, max_model_calls: 16, messages: vec![json!({"role":"user","content":PROMPT})] };
            let result = native_chat::execute_hosted_with_model(&self.project, start, model, host, &self.cancel, |frame| {
                observe(&self.db, &self.turn, host, frame)?;
                if let Some((step, _, _, state)) = self.row() {
                    if steps.last().map(|(last, _)| last != &step).unwrap_or(true) {
                        steps.push((step, state));
                    }
                }
                Ok(())
            });
            (result, steps)
        }
    }

    fn script() -> ScriptedModel {
        ScriptedModel::new([
            tool_calls_reply([
                tool_call("write-1", "write_file", json!({"path":"outputs/note.md","content":"note"})),
                tool_call("list-1", "list_workspace_files", json!({})),
            ]),
            text_reply("Saved outputs/note.md."),
        ])
    }

    #[test]
    fn a_chat_turn_checkpoints_each_step_in_order_with_dispatched_effects() {
        let fixture = Fixture::new();
        let host = fixture.tools();
        let (result, steps) = fixture.run(&script(), &host);
        assert_eq!(result.unwrap(), "Saved outputs/note.md.");
        let names: Vec<&str> = steps.iter().map(|(step, _)| step.as_str()).collect();
        assert_eq!(names, ["model", "tools_dispatched", "tools_done", "model", "gate"]);

        // A model step holds the saved RecoveryState and best candidate (none before
        // the first save).
        assert!(steps[0].1.get("best_candidate").is_some());
        assert_eq!(steps[3].1["recovery"]["version"], kageha_runtime::recovery::RECOVERY_VERSION, "{}", steps[3].1);
        assert!(steps[3].1.get("best_candidate").is_some());
        // The dispatched calls with their ids and Capability effects.
        let dispatched = &steps[1].1["dispatched"];
        assert_eq!(dispatched, &json!([
            {"id":"write-1","name":"write_file","effect":"workspace"},
            {"id":"list-1","name":"list_workspace_files","effect":"read"},
        ]));
        assert_eq!(steps[2].1["tool_call_ids"], json!(["write-1", "list-1"]));
        assert_eq!(steps[4].1["gate"], json!({"status":"ready","rejections":0}));

        // The final row is resumable and its messages hold the recorded results.
        let (step, phase, messages, _) = fixture.row().unwrap();
        assert_eq!((step.as_str(), phase.as_str()), ("gate", "ready"));
        let messages: Vec<Value> = serde_json::from_str(&messages).unwrap();
        assert!(messages.iter().any(|m| m["role"] == "tool" && m["tool_call_id"] == "write-1"));
        let resume = crate::ui_job_controls::resume_state(&fixture.db.lock().unwrap(), &fixture.turn, "interrupted").unwrap();
        assert_eq!(resume["allowed"], true);
    }

    #[test]
    fn an_existing_checkpoint_table_gains_step_and_state() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE ui_turn_checkpoints(turn_id TEXT PRIMARY KEY,messages TEXT NOT NULL,calls TEXT NOT NULL,phase TEXT NOT NULL,updated REAL NOT NULL);
            INSERT INTO ui_turn_checkpoints VALUES ('old','[]','[]','ready',0);").unwrap();
        crate::ui_job_controls::initialize(&connection).unwrap();
        crate::ui_job_controls::initialize(&connection).unwrap();
        let db = crate::agent_host::DatabaseState::open(connection).unwrap();
        crate::ui_job_controls::record(&db, "chat", "old", &json!({"kind":"gate","status":"continue","rejections":1}), &Value::Null).unwrap();
        let (step, state): (String, String) = db.lock().unwrap()
            .query_row("SELECT step,state FROM ui_turn_checkpoints WHERE turn_id='old'", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!(step, "gate");
        assert_eq!(serde_json::from_str::<Value>(&state).unwrap()["gate"], json!({"status":"continue","rejections":1}));
    }

    #[test]
    fn a_failed_checkpoint_write_stops_the_turn_before_dispatch() {
        let fixture = Fixture::new();
        fixture.db.lock().unwrap().execute_batch(
            "CREATE TRIGGER fail_dispatch_checkpoint BEFORE UPDATE ON ui_turn_checkpoints
             WHEN NEW.step='tools_dispatched' BEGIN SELECT RAISE(ABORT,'database or disk is full'); END;",
        ).unwrap();
        let host = fixture.tools();
        let model = script();
        let (result, steps) = fixture.run(&model, &host);
        let error = result.unwrap_err();
        assert!(is_failure(&error), "{error}");
        assert!(error.to_string().contains("tools_dispatched") && error.to_string().contains("disk is full"), "{error}");
        assert_eq!(steps.iter().map(|(step, _)| step.as_str()).collect::<Vec<_>>(), ["model"]);
        // Nothing was dispatched and the model was not called again.
        let root = crate::ui_workspace::open_for_chat(&fixture.db, CHAT).unwrap().root().to_path_buf();
        assert!(!root.join("outputs/note.md").exists());
        assert_eq!(model.remaining(), 1);
        // The last durable checkpoint marks the batch as possibly in flight.
        assert_eq!(fixture.row().unwrap().1, "dispatching");
    }
}
