//! Re-establishing state after a supervised helper restarts, and telling the running turns
//! (spec autonomous-agent-loop, task 17.2; design section 13; Requirements 10.2 and 10.6).
//!
//! The Supervisor passes every `component_restarted` event to the listener [`install`] sets.
//! On a thread of its own, [`handle`] then:
//!
//! 1. Re-establishes the component's state.
//!    - `shell`: re-adopts the background jobs `ui_shell_jobs` records as running, by PID and
//!      process start time (`ui_shell::readopt_jobs`). Sessions restore themselves from
//!      `.kageha/shell-env` when the chat's next command opens one
//!      (`kageha_workspace::shell::state`).
//!    - `browser`: for each running turn that used browser tools, runs the binding recovery
//!      (`web_browser::reopen_profile`): the selected managed profile is relaunched with its
//!      saved state. No page input is sent and nothing is replayed; an action that was in
//!      flight is resolved through its receipt (`browser_receipts`); navigation and
//!      closing require inspecting the original tab instead of inventing a receipt.
//!    - `computer`: the Supervisor already relaunched the helper, which marks every earlier
//!      observation stale (`computer_host`); `computer_execute` with one returns
//!      `stale_observation`.
//!    - `runtime`: re-attached on the next request; nothing to restore.
//! 2. Queues the note "Component `<name>` restarted at `<time>`; observations before this
//!    are stale", with what the model should do next, for every running turn in
//!    `ui_component_notes`. `ui_tools::Tools::steering` takes the turn's notes at its next
//!    model checkpoint and the runtime appends them as steering messages, so the context
//!    only grows and each note reaches the model once. A later restart of the same
//!    component replaces a note that was not delivered yet.
//! 3. Emits `component_restarted` and a `recovery` event (cause, action, result) to each
//!    running turn (Requirement 10.6; the Run_Journal of task 21.2 records them).
//!
//! At Host startup [`install`] also re-adopts the jobs of an earlier server process.
use crate::{
    browser_client::Client,
    supervisor::SupervisorEvent,
    ui_shell::{self, Readopted},
    web_browser,
    webui::{self, Database},
};
use kageha_workspace::shell::JobState;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::{io, sync::Arc, time::Duration};

const NOTES_TABLE: &str = "CREATE TABLE IF NOT EXISTS ui_component_notes(turn_id TEXT NOT NULL,
    component TEXT NOT NULL,note TEXT NOT NULL,at_ms INTEGER NOT NULL,PRIMARY KEY(turn_id,component))";

/// How long the startup pass waits for a shell service process to answer.
const SHELL_SERVICE_WAIT: Duration = Duration::from_secs(30);

/// Connects the Host to the Supervisor's restart events, and re-adopts the shell jobs of an
/// earlier server process. Call once after `supervisor::start`.
pub(crate) fn install(db: &Database) {
    let startup = Arc::clone(db);
    let spawned = std::thread::Builder::new().name("shell-readopt".into()).spawn(move || {
        // In process mode the service may still be starting.
        if let Some(service) = crate::supervisor::shell_backend() {
            let deadline = std::time::Instant::now() + SHELL_SERVICE_WAIT;
            while !service.reachable(Duration::from_secs(1)) && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(250));
            }
        }
        match ui_shell::readopt_jobs(&startup) {
            Ok(jobs) if !jobs.is_empty() => eprintln!("shell: re-adopted {} of {} recorded jobs at startup",
                jobs.iter().filter(|job| job.state == JobState::Running).count(), jobs.len()),
            Ok(_) => {}
            Err(error) => eprintln!("shell: startup job re-adoption failed: {error}"),
        }
    });
    if let Err(error) = spawned {
        eprintln!("shell: startup job re-adoption not started: {error}");
    }
    let Some(supervisor) = crate::supervisor::global() else { return };
    let db = Arc::clone(db);
    supervisor.set_listener(Arc::new(move |event: &SupervisorEvent| {
        if event.kind == "component_degraded" {
            let (db, event) = (Arc::clone(&db), event.clone());
            // Advisory: running turns see the component as unavailable until it recovers.
            let _ = std::thread::Builder::new()
                .name(format!("degraded-{}", event.component))
                .spawn(move || {
                    if let Err(error) = degraded(&db, &event) {
                        eprintln!("supervisor: publishing that {} is degraded failed: {error}", event.component);
                    }
                });
            return;
        }
        if event.kind != "component_restarted" {
            return;
        }
        let (db, event) = (Arc::clone(&db), event.clone());
        // The listener runs on the component's monitor thread: do the work elsewhere.
        let spawned = std::thread::Builder::new()
            .name(format!("restarted-{}", event.component))
            .spawn(move || {
                if let Err(error) = handle(&db, &event) {
                    eprintln!("supervisor: recovery after the {} restart failed: {error}", event.component);
                }
            });
        if let Err(error) = spawned {
            eprintln!("supervisor: recovery after a restart not started: {error}");
        }
    }));
}

/// What was done to re-establish a component's state for one chat.
#[derive(Clone, Debug, PartialEq)]
struct Recovery {
    action: &'static str,
    /// `recovered`, `skipped` or `failed`.
    status: &'static str,
    result: Value,
}

/// `<time>` of the note: RFC 3339 in UTC, to the second.
fn timestamp(at_ms: u64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(at_ms as i64)
        .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_else(|| at_ms.to_string())
}

/// The note appended to a running turn's context.
fn note(component: &str, at: &str, recovery: &Recovery) -> String {
    let next = match component {
        "browser" => "Read browser_tabs and take a new browser_snapshot before page input. For page input that was in flight, check browser_receipts and don't repeat it. For uncertain navigation inspect the original tab's URL; for uncertain tab closing check whether that exact target remains in browser_tabs. Navigation and closing RPC IDs are not input receipts. Never automatically repeat those actions.",
        "computer" => "Call computer_observe again before computer_execute. For input that was in flight, use computer_receipt and don't repeat it.",
        "shell" => "The shell session continues in its saved directory with its saved non-secret exported variables; shell functions and unexported variables are gone. Background jobs were re-adopted; check them with shell_job.",
        _ => "Continue from the recorded results.",
    };
    let mut text = format!("Component `{component}` restarted at `{at}`; observations before this are stale. {next}");
    if recovery.status == "failed" {
        let error = recovery.result["error"].as_str().unwrap_or("unknown error");
        text.push_str(&format!(" Automatic recovery did not complete: {error}."));
    }
    text
}

/// `(chat, turn)` of every running chat turn.
fn running_turns(db: &Database) -> io::Result<Vec<(String, String)>> {
    let connection = db.lock().map_err(|_| io::Error::other("database unavailable"))?;
    let mut query = connection
        .prepare("SELECT chat_id,request_id FROM native_chat_turns WHERE status='running' ORDER BY sequence")
        .map_err(io::Error::other)?;
    let rows = query
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
        .map_err(io::Error::other)?;
    let rows: Vec<(String, String)> = rows.collect::<rusqlite::Result<_>>().map_err(io::Error::other)?;
    Ok(rows.into_iter().map(|(chat, request)| { let turn = webui::turn_id(&chat, &request); (chat, turn) }).collect())
}

/// Whether `turn` called a browser tool.
fn used_browser(db: &Database, chat: &str, turn: &str) -> bool {
    db.lock().ok().and_then(|connection| connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM ui_events WHERE chat_id=? AND turn_id=? AND event='tool_card'
            AND json_extract(payload,'$.name') LIKE 'browser\\_%' ESCAPE '\\')",
        [chat, turn], |row| row.get::<_, bool>(0)).ok()).unwrap_or(false)
}

/// The browser binding recovery for one chat.
fn browser_recovery(db: &Database, chat: &str, turn: &str) -> Recovery {
    if !used_browser(db, chat, turn) {
        return Recovery { action: "none", status: "skipped", result: json!({"reason":"this turn has not used the browser"}) };
    }
    let reopened = (|| -> io::Result<(Value, bool)> {
        let mut client = Client::connect(chat, "agent")?;
        let selection = client.selection()?;
        Ok(match web_browser::reopen_profile(&mut client, &selection)? {
            Some(reopened) => (reopened, true),
            None => (selection, false),
        })
    })();
    match reopened {
        Ok((selection, reopened)) => Recovery {
            action: if reopened { "reopen_profile" } else { "rebind" },
            status: "recovered",
            result: json!({"profile":selection["profile"],"browser_id":selection["browser_id"],"reopened":reopened,
                "managed_tabs":selection["tabs"].as_array().map_or(0, Vec::len)}),
        },
        Err(error) => Recovery { action: "reopen_profile", status: "failed", result: json!({"error":error.to_string()}) },
    }
}

/// The shell recovery for one chat, from the jobs re-adopted for every chat.
fn shell_recovery(readopted: &io::Result<Vec<Readopted>>, chat: &str) -> Recovery {
    match readopted {
        Ok(jobs) => {
            let mine: Vec<&Readopted> = jobs.iter().filter(|job| job.chat == chat).collect();
            Recovery { action: "readopt_jobs", status: "recovered", result: json!({
                "jobs_running": mine.iter().filter(|job| job.state == JobState::Running).map(|job| &job.job_id).collect::<Vec<_>>(),
                "jobs_ended": mine.iter().filter(|job| job.state != JobState::Running).map(|job| &job.job_id).collect::<Vec<_>>(),
                "session":"restored from .kageha/shell-env on the next shell call"}) }
        }
        Err(error) => Recovery { action: "readopt_jobs", status: "failed", result: json!({"error":error.to_string()}) },
    }
}

fn queue(connection: &Connection, turn: &str, component: &str, note: &str, at_ms: u64) -> rusqlite::Result<()> {
    connection.execute_batch(NOTES_TABLE)?;
    connection.execute("INSERT OR REPLACE INTO ui_component_notes(turn_id,component,note,at_ms) VALUES (?,?,?,?)",
        params![turn, component, note, at_ms as i64])?;
    Ok(())
}

/// Drops notes of turns that are no longer running (they ended before their next step).
fn prune(connection: &Connection, running: &[(String, String)]) -> rusqlite::Result<()> {
    connection.execute_batch(NOTES_TABLE)?;
    let turns: Vec<String> = {
        let mut query = connection.prepare("SELECT DISTINCT turn_id FROM ui_component_notes")?;
        let rows = query.query_map([], |row| row.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for turn in turns.iter().filter(|turn| !running.iter().any(|(_, running)| running == *turn)) {
        connection.execute("DELETE FROM ui_component_notes WHERE turn_id=?", [turn])?;
    }
    Ok(())
}

/// Re-establishes the state of `event.component` and notifies every running turn. Returns
/// the `recovery` payloads it emitted, one per running turn.
pub(crate) fn handle(db: &Database, event: &SupervisorEvent) -> io::Result<Vec<Value>> {
    let component = event.component.as_str();
    let at = timestamp(event.at_ms);
    let readopted = (component == "shell").then(|| ui_shell::readopt_jobs(db));
    let running = running_turns(db)?;
    prune(&*db.lock().map_err(|_| io::Error::other("database unavailable"))?, &running).map_err(io::Error::other)?;
    let mut payloads = Vec::new();
    for (chat, turn) in &running {
        let recovery = match component {
            "shell" => shell_recovery(readopted.as_ref().expect("shell re-adoption ran"), chat),
            "browser" => browser_recovery(db, chat, turn),
            "computer" => Recovery { action: "relaunch_helper", status: "recovered",
                result: json!({"observations":"stale; observe again before the next action"}) },
            _ => Recovery { action: "reattach", status: "recovered", result: json!({}) },
        };
        let text = note(component, &at, &recovery);
        queue(&*db.lock().map_err(|_| io::Error::other("database unavailable"))?, turn, component, &text, event.at_ms)
            .map_err(io::Error::other)?;
        // Advisory publication: the queued note is what the turn relies on.
        let _ = webui::emit(db, chat, turn, "event", json!({"kind":"component_restarted","interesting":true,
            "payload":{"component":component,"at":at,"at_ms":event.at_ms,"reason":event.reason,
                "restarts_in_window":event.restarts_in_window}}));
        let payload = json!({"kind":"recovery","phase":"component","component":component,"status":recovery.status,
            "cause":format!("component_restarted: {}", event.reason),"action":recovery.action,"result":recovery.result,"at":at});
        let _ = webui::emit(db, chat, turn, "event", json!({"kind":"recovery","interesting":true,"payload":payload}));
        payloads.push(payload);
    }
    eprintln!("supervisor: {component} restarted at {at}; notified {} running turn(s)", running.len());
    Ok(payloads)
}

/// Emits `component_degraded {component, at, reason, retry_after}` to every running turn
/// (design section 16). Returns the number of turns told.
pub(crate) fn degraded(db: &Database, event: &SupervisorEvent) -> io::Result<usize> {
    let running = running_turns(db)?;
    let at = timestamp(event.at_ms);
    for (chat, turn) in &running {
        webui::emit(db, chat, turn, "event", json!({"kind":"component_degraded","interesting":true,
            "payload":{"component":event.component,"at":at,"at_ms":event.at_ms,"reason":event.reason,
                "restarts_in_window":event.restarts_in_window,"retry_after":event.retry_after}}))?;
    }
    Ok(running.len())
}
/// Takes the notes queued for `turn`, oldest first. Each note is returned once.
pub(crate) fn take_notes(db: &Database, turn: &str) -> io::Result<Vec<String>> {
    let mut connection = db.lock().map_err(|_| io::Error::other("database unavailable"))?;
    let tx = connection.transaction().map_err(io::Error::other)?;
    tx.execute_batch(NOTES_TABLE).map_err(io::Error::other)?;
    let notes: Vec<String> = {
        let mut query = tx.prepare("SELECT note FROM ui_component_notes WHERE turn_id=? ORDER BY at_ms,rowid")
            .map_err(io::Error::other)?;
        let rows = query.query_map([turn], |row| row.get(0)).map_err(io::Error::other)?;
        rows.collect::<rusqlite::Result<_>>().map_err(io::Error::other)?
    };
    if !notes.is_empty() {
        tx.execute("DELETE FROM ui_component_notes WHERE turn_id=?", [turn]).map_err(io::Error::other)?;
    }
    tx.commit().map_err(io::Error::other)?;
    Ok(notes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audited_session_tests::{main_persona, Fixture, CHAT, REQUEST};
    use crate::native_chat::ToolHost;
    use crate::ui_tools::Tools;
    use kageha_runtime::execution::{Control, ToolCall};
    use kageha_runtime::test_support::{text_reply, tool_call, tool_calls_reply, ScriptedModel};
    use std::sync::atomic::{AtomicBool, Ordering};

    fn restarted(component: &str) -> SupervisorEvent {
        SupervisorEvent { seq: 1, kind: "component_restarted", component: component.into(), at_ms: 1_762_970_405_000,
            reason: "process exited".into(), restarts_in_window: 1, retry_after: None }
    }

    /// The real Host tools; the first dispatch of `trigger` happens while the runtime
    /// service restarts.
    struct RestartDuringTool<'a> {
        tools: &'a Tools<'a>,
        db: &'a Database,
        trigger: &'static str,
        fired: AtomicBool,
    }

    impl ToolHost for RestartDuringTool<'_> {
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
            if call["id"] == self.trigger && !self.fired.swap(true, Ordering::SeqCst) {
                let notified = handle(self.db, &restarted("runtime")).unwrap();
                assert_eq!(notified.len(), 1, "{notified:?}");
            }
            self.tools.invoke(call)
        }
    }

    fn mentions(message: &Value, text: &str) -> bool {
        message["content"].as_str().is_some_and(|content| content.contains(text))
    }

    #[test]
    fn a_running_turn_gets_the_restart_note_once_on_its_next_model_step() {
        let fixture = Fixture::new();
        fixture.db.lock().unwrap().execute(
            "INSERT INTO native_chat_turns(chat_id,request_id,message,status) VALUES (?,?,'prompt','running')",
            [CHAT, REQUEST]).unwrap();
        let tools = fixture.tools(main_persona(Value::Null));
        let host = RestartDuringTool { tools: &tools, db: &fixture.db, trigger: "probe-1", fired: AtomicBool::new(false) };
        let model = ScriptedModel::new([
            tool_calls_reply([tool_call("probe-1", "list_workspace_files", json!({}))]),
            tool_calls_reply([tool_call("probe-2", "list_workspace_files", json!({}))]),
            text_reply("The workspace is empty."),
        ]);
        for _ in 0..3 {
            model.push_text("The workspace is empty.");
        }

        fixture.run(&model, &host);

        let expected = "Component `runtime` restarted at `2025-11-12T18:00:05Z`; observations before this are stale.";
        let requests = model.requests();
        // The first request that carries probe-1's result is the turn's next step.
        let next = requests.iter().position(|request| request.messages.iter()
            .any(|message| message["role"] == "tool" && message["tool_call_id"] == "probe-1"))
            .expect("a model step after the restart");
        assert!(requests.len() > next + 1, "the turn continued after the note");
        for (index, request) in requests.iter().enumerate() {
            let notes: Vec<&Value> = request.messages.iter().filter(|message| mentions(message, expected)).collect();
            assert_eq!(notes.len(), usize::from(index >= next), "request {index}: {:?}", request.messages);
            if index == next {
                // Appended after the tool result, never inserted earlier in the context.
                assert!(mentions(request.messages.last().unwrap(), expected), "{:?}", request.messages.last());
            }
        }
        assert!(take_notes(&fixture.db, &fixture.turn).unwrap().is_empty());
        // The chat got the restart and the recovery record.
        let restarts = fixture.events("component_restarted");
        assert_eq!(restarts.len(), 1);
        assert_eq!(restarts[0]["payload"]["component"], "runtime");
        let recoveries: Vec<Value> = fixture.events("recovery").into_iter()
            .filter(|event| event["payload"]["phase"] == "component").collect();
        assert_eq!(recoveries.len(), 1, "{recoveries:?}");
        let recovery = &recoveries[0]["payload"];
        assert_eq!((recovery["cause"].as_str(), recovery["action"].as_str(), recovery["status"].as_str()),
            (Some("component_restarted: process exited"), Some("reattach"), Some("recovered")));
    }

    #[test]
    fn a_degraded_component_is_reported_to_running_turns_only() {
        let fixture = Fixture::new();
        fixture.db.lock().unwrap().execute_batch(&format!(
            "INSERT INTO native_chat_turns(chat_id,request_id,message,status) VALUES ('{CHAT}','{REQUEST}','p','running');
             INSERT INTO native_chat_turns(chat_id,request_id,message,status) VALUES ('{CHAT}','old','p','completed');")).unwrap();
        let event = SupervisorEvent { kind: "component_degraded", retry_after: Some(60), ..restarted("browser") };
        assert_eq!(degraded(&fixture.db, &event).unwrap(), 1);
        let events = fixture.events("component_degraded");
        assert_eq!(events.len(), 1);
        let payload = &events[0]["payload"];
        assert_eq!((payload["component"].as_str(), payload["retry_after"].as_u64()), (Some("browser"), Some(60)));
        assert_eq!(events[0]["label"], "Component unavailable");
    }
    #[test]
    fn notes_reach_only_running_turns_and_a_later_restart_replaces_an_undelivered_one() {
        let fixture = Fixture::new();
        fixture.db.lock().unwrap().execute_batch(&format!(
            "INSERT INTO native_chat_turns(chat_id,request_id,message,status) VALUES ('{CHAT}','{REQUEST}','p','running');
             INSERT INTO native_chat_turns(chat_id,request_id,message,status) VALUES ('{CHAT}','old','p','completed');")).unwrap();
        let done = webui::turn_id(CHAT, "old");
        let first = handle(&fixture.db, &restarted("computer")).unwrap();
        assert_eq!(first[0]["action"], "relaunch_helper");
        let mut later = restarted("computer");
        later.at_ms += 60_000;
        handle(&fixture.db, &later).unwrap();
        handle(&fixture.db, &restarted("browser")).unwrap();
        let notes = take_notes(&fixture.db, &fixture.turn).unwrap();
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes[0].starts_with("Component `browser` restarted at `2025-11-12T18:00:05Z`"), "{}", notes[0]);
        // The browser was not used in this turn, so nothing was reopened.
        assert!(notes[0].contains("browser_receipts"));
        assert!(notes[1].starts_with("Component `computer` restarted at `2025-11-12T18:01:05Z`"), "{}", notes[1]);
        assert!(notes[1].contains("computer_observe"));
        assert!(take_notes(&fixture.db, &fixture.turn).unwrap().is_empty());
        assert!(take_notes(&fixture.db, &done).unwrap().is_empty());
        let skipped: Vec<Value> = fixture.events("recovery").into_iter()
            .filter(|event| event["payload"]["component"] == "browser").collect();
        assert_eq!(skipped[0]["payload"]["status"], "skipped");
    }
}
