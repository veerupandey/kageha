//! Restart continuation for chat turns (Requirements 13.2 and 13.4, design section 12).
//!
//! When the server starts, every `native_chat_turns` row with status `running` goes
//! through one resume pass:
//!
//! - A turn without a runtime run (the legacy `/chats/{id}/turns` route) has no
//!   checkpoint: [`startup`] marks it `interrupted`.
//! - A runtime-managed turn reaches [`restart`] from the runtime supervisor's recovery,
//!   before any worker starts. Without a resumable checkpoint it is recovered as before
//!   (`interrupted`, unless a committed result exists).
//! - With the credential vault locked, the turn becomes `waiting_for_unlock` and its
//!   run stays held. [`unlocked`], called by the vault unlock handler, resumes every
//!   waiting turn.
//! - Otherwise the run is requeued. Its worker calls the same execute path as a new
//!   turn, with the same request id and pinned configuration, so the request budget,
//!   RecoveryState and outcome contract carry over. [`take`] swaps the initial
//!   messages for the checkpoint messages.
//!
//! A checkpoint in phase `ready` (between steps, nothing in flight) continues from its
//! messages. A checkpoint in phase `dispatching` or `blocked_dispatching` has a tool
//! batch that was admitted when the process stopped (Requirement 13.3, task 16.3). The
//! batch's calls are resolved by effect before the model is called again, see
//! [`resolve`]: no call is sent twice, and an action without a verified receipt stops
//! the turn as uncertain (Requirement 19.2). Host task-state tools ([`HOST_STATE_TOOLS`])
//! are dispatched again. A checkpoint in phase `unverified` has an earlier uncertain
//! action result and is interrupted, as before.
//!
//! "Resume task" (Requirement 13.5): the UI button sends `{"resume": true}`, and the
//! Host continues the latest request as a new turn with the same pinned configuration
//! (`ui_job_controls::chat_resume`). From a checkpoint with a batch in flight,
//! [`hand_over`] gives the batch to the new turn, which resolves it the same way, with
//! the receipts of the turn that dispatched it.
//! Delegated workers (depth above 0) keep their own resume command.
use crate::{
    ui_store::{db_error, ApiResult},
    ui_tools::{Effect, Tools},
    webui::{self, Database},
};
use kageha_runtime::execution::{ToolCall, ToolReply};
use kageha_runtime::lifecycle::{Completion, Restart, RunRecord, RunState, RunStore};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::io;

/// Native status of a turn held until the credential vault is unlocked.
pub(crate) const WAITING_FOR_UNLOCK: &str = "waiting_for_unlock";
/// SQL list of native statuses of a turn that has not finished. Such a turn blocks a
/// new turn in its chat.
pub(crate) const LIVE_STATUSES: &str = "('running','waiting_for_unlock')";

const RESTART_NOTE: &str = "The server restarted. Continue this task from the saved checkpoint. Tool calls that have results above already happened; do not repeat them. Reobserve the browser or device before sending input.";
/// Appended after the results of an in-flight batch resolved by [`resolve`].
const IN_FLIGHT_NOTE: &str = "The server restarted while the last tool batch was running. Its results above were resolved after the restart: reads ran again, file and shell results show the current state, and actions show their saved receipts. None of these calls was sent twice. Continue the task from here. Reobserve the browser or device before sending input.";
/// Appended after the results of an in-flight batch of an earlier turn that "Resume task"
/// continues ([`hand_over`]).
const RESUMED_IN_FLIGHT_NOTE: &str = "The task stopped while the last tool batch was running, and the user asked to resume it. Its results above were resolved now: reads and task-state updates ran again, file and shell results show the current state, and actions show their saved receipts. No action was sent twice. Continue the task from here. Reobserve the browser or device before sending input.";
/// Checkpoint phases a restart continues from.
const RESUMABLE_PHASES: [&str; 3] = ["ready", "dispatching", "blocked_dispatching"];

pub(crate) fn initialize(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS ui_turn_restarts(turn_id TEXT PRIMARY KEY,run_id TEXT NOT NULL,
             state TEXT NOT NULL,created REAL NOT NULL);",
    )?;
    migrate_running_index(db)
}

/// The partial unique index allows one live turn per chat. A turn waiting for unlock
/// is live, so the index covers it. Rebuilding is safe: before this version no row
/// had the new status, so the indexed rows are the same.
fn migrate_running_index(db: &Connection) -> rusqlite::Result<()> {
    let sql: Option<String> = db
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='index' AND name='native_chat_running'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if sql.is_some_and(|sql| !sql.contains(WAITING_FOR_UNLOCK)) {
        db.execute_batch(&format!(
            "SAVEPOINT native_chat_running_migration;
             DROP INDEX native_chat_running;
             CREATE UNIQUE INDEX native_chat_running ON native_chat_turns(chat_id) WHERE status IN {LIVE_STATUSES};
             RELEASE native_chat_running_migration;"
        ))?;
    }
    Ok(())
}

/// Server start: a running turn without a runtime run cannot continue, because only
/// runtime-managed turns write checkpoints. Runtime-managed turns are decided by
/// [`restart`].
pub(crate) fn startup(db: &Connection) -> rusqlite::Result<usize> {
    db.execute(
        "UPDATE native_chat_turns SET status='interrupted' WHERE status='running' AND NOT EXISTS(SELECT 1 FROM ui_runtime_runs r WHERE json_extract(r.spec,'$.session')=native_chat_turns.chat_id AND json_extract(r.spec,'$.request')=native_chat_turns.request_id)",
        [],
    )
}

#[cfg(test)]
pub(crate) mod test_vault {
    //! Locks the credential vault of one test database without a real Keychain.
    use rusqlite::Connection;
    pub(crate) fn set(db: &Connection, locked: bool) {
        if locked {
            crate::ui_store::put(db, "test", "vault-locked", serde_json::json!({"locked":true})).unwrap();
        } else {
            db.execute("DELETE FROM ui_documents WHERE kind='test' AND id='vault-locked'", []).unwrap();
        }
    }
    pub(crate) fn locked(db: &Connection) -> bool {
        crate::ui_store::get(db, "test", "vault-locked").ok().flatten().is_some()
    }
}

/// The same check as the "Saved credentials are locked" readiness status. It never
/// shows an OS prompt.
fn vault_locked(db: &Connection) -> bool {
    #[cfg(test)]
    if test_vault::locked(db) {
        return true;
    }
    crate::ui_credentials::keychain_status(db)["locked"] == true
}

/// Startup decision for a runtime run that was `running` when the previous process
/// stopped. Called by the runtime supervisor's recovery before workers start.
pub(crate) fn restart(db: &Database, run: &RunRecord) -> io::Result<Restart> {
    if run.spec.depth != 0 || run.spec.parent_run.is_some() {
        return Ok(Restart::Inspect);
    }
    // A standalone runtime service owns execution; recovery reconciles with it.
    if !matches!(kageha_runtime_service::bridge_client::Client::from_env(), Ok(None)) {
        return Ok(Restart::Inspect);
    }
    let (chat, request, turn) = (&run.spec.session, &run.spec.request, &run.spec.turn);
    let mut connection = db.lock().map_err(|_| io::Error::other("database unavailable"))?;
    let tx = connection.transaction().map_err(io::Error::other)?;
    let status: Option<String> = tx
        .query_row(
            "SELECT status FROM native_chat_turns WHERE chat_id=? AND request_id=?",
            params![chat, request],
            |row| row.get(0),
        )
        .optional()
        .map_err(io::Error::other)?;
    if !matches!(status.as_deref(), Some("running" | WAITING_FOR_UNLOCK)) {
        return Ok(Restart::Inspect);
    }
    let committed: bool = tx
        .query_row("SELECT EXISTS(SELECT 1 FROM ui_runtime_results WHERE turn=?)", [turn], |row| row.get(0))
        .map_err(io::Error::other)?;
    let checkpoint: Option<(String, String)> = tx
        .query_row(
            "SELECT phase,step FROM ui_turn_checkpoints WHERE turn_id=?",
            [turn],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(io::Error::other)?;
    // `step` is empty for rows not written by the turn observer; their messages are
    // not the model input. An in-flight batch is resolved by [`resolve`].
    let resumable = checkpoint
        .is_some_and(|(phase, step)| RESUMABLE_PHASES.contains(&phase.as_str()) && !step.is_empty());
    if committed || !resumable {
        if status.as_deref() == Some(WAITING_FOR_UNLOCK) {
            set_status(&tx, chat, request, "running")?;
        }
        tx.execute("DELETE FROM ui_turn_restarts WHERE turn_id=?", [turn]).map_err(io::Error::other)?;
        tx.commit().map_err(io::Error::other)?;
        return Ok(Restart::Inspect);
    }
    let decision = if vault_locked(&tx) {
        mark(&tx, turn, &run.spec.id, "waiting")?;
        set_status(&tx, chat, request, WAITING_FOR_UNLOCK)?;
        webui::insert_event(&tx, chat, turn, "status", json!({"label":"Waiting for credential unlock…"}))
            .map_err(io::Error::other)?;
        webui::insert_event(&tx, chat, turn, "event", json!({"kind":WAITING_FOR_UNLOCK,"interesting":true,
            "payload":{"message":"Saved credentials are locked. Open Settings > Models and choose Unlock saved credentials. This task continues after unlock."}}))
            .map_err(io::Error::other)?;
        Restart::Hold
    } else {
        mark(&tx, turn, &run.spec.id, "resuming")?;
        set_status(&tx, chat, request, "running")?;
        continuing(&tx, chat, turn)?;
        Restart::Resume
    };
    tx.commit().map_err(io::Error::other)?;
    crate::ui_session_events::notify();
    Ok(decision)
}

fn mark(db: &Connection, turn: &str, run: &str, state: &str) -> io::Result<()> {
    db.execute(
        "INSERT INTO ui_turn_restarts VALUES (?,?,?,?) ON CONFLICT(turn_id) DO UPDATE SET run_id=excluded.run_id,state=excluded.state,created=excluded.created",
        params![turn, run, state, crate::ui_store::now()],
    )
    .map_err(io::Error::other)?;
    Ok(())
}

fn set_status(db: &Connection, chat: &str, request: &str, status: &str) -> io::Result<()> {
    db.execute(
        &format!("UPDATE native_chat_turns SET status=? WHERE chat_id=? AND request_id=? AND status IN {LIVE_STATUSES}"),
        params![status, chat, request],
    )
    .map_err(io::Error::other)?;
    Ok(())
}

fn continuing(db: &Connection, chat: &str, turn: &str) -> io::Result<()> {
    webui::insert_event(db, chat, turn, "status", json!({"label":"Continuing after restart…"})).map_err(io::Error::other)?;
    webui::insert_event(db, chat, turn, "event", json!({"kind":"turn_resumed","interesting":true,
        "payload":{"message":"Continuing from the saved checkpoint after a restart."}}))
        .map_err(io::Error::other)?;
    Ok(())
}

/// What a turn that [`restart`] requeued, or that [`hand_over`] received, continues from.
#[derive(Debug)]
pub(crate) struct Saved {
    /// The checkpoint messages. Without an in-flight batch they end with the restart note.
    pub messages: Vec<Value>,
    /// The batch admitted when the process stopped. [`resolve_from`] produces its results.
    pub in_flight: Option<InFlight>,
    /// The earlier turn that dispatched the batch, when [`hand_over`] gave it to this
    /// turn ("Resume task"). Its receipts are looked up under that turn. `None` when the
    /// batch is this turn's own.
    pub receipt_turn: Option<String>,
}

/// "Resume task" from a checkpoint with a batch in flight (Requirement 13.5): the new
/// turn `turn` (run `run`) of the same request receives the checkpoint of the stopped
/// turn `source`, marked like a requeued turn, so its worker resolves the batch with
/// [`resolve_from`] before the first model call. The copy records `source`, because the
/// receipts of the batch belong to that turn. Called in the admission transaction.
pub(crate) fn hand_over(db: &Connection, source: &str, turn: &str, run: &str) -> rusqlite::Result<()> {
    let (messages, calls, phase, step, state): (String, String, String, String, String) = db.query_row(
        "SELECT messages,calls,phase,step,state FROM ui_turn_checkpoints WHERE turn_id=?",
        [source],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
    )?;
    let mut state: Value = serde_json::from_str(&state).unwrap_or_else(|_| json!({}));
    if !state.is_object() {
        state = json!({});
    }
    state["receipt_turn"] = json!(source);
    db.execute(
        "INSERT INTO ui_turn_checkpoints(turn_id,messages,calls,phase,updated,step,state) VALUES (?,?,?,?,?,?,?)
         ON CONFLICT(turn_id) DO UPDATE SET messages=excluded.messages,calls=excluded.calls,phase=excluded.phase,updated=excluded.updated,step=excluded.step,state=excluded.state",
        params![turn, messages, calls, phase, crate::ui_store::now(), step, state.to_string()],
    )?;
    db.execute(
        "INSERT INTO ui_turn_restarts VALUES (?,?,'resuming',?) ON CONFLICT(turn_id) DO UPDATE SET run_id=excluded.run_id,state=excluded.state,created=excluded.created",
        params![turn, run, crate::ui_store::now()],
    )?;
    Ok(())
}

/// A tool batch that was admitted but has no recorded results.
#[derive(Debug, Clone)]
pub(crate) struct InFlight {
    /// The batch as the model returned it (`{id, name, arguments}`, the `calls` column).
    pub calls: Vec<Value>,
    /// `{id, name, effect}` of the calls that were dispatched (`state.dispatched` of the
    /// `tools_dispatched` step). Empty when the process stopped before dispatch.
    pub dispatched: Vec<Value>,
}

/// The saved state of a turn that [`restart`] requeued. Consumes the marker, so a later
/// start of the same turn is not affected. `None` for a turn that was not requeued.
pub(crate) fn take(db: &Database, turn: &str) -> io::Result<Option<Saved>> {
    let mut connection = db.lock().map_err(|_| io::Error::other("database unavailable"))?;
    let tx = connection.transaction().map_err(io::Error::other)?;
    let marked: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM ui_turn_restarts WHERE turn_id=? AND state='resuming')",
            [turn],
            |row| row.get(0),
        )
        .map_err(io::Error::other)?;
    if !marked {
        return Ok(None);
    }
    let row: Option<(String, String, String, String, String)> = tx
        .query_row(
            "SELECT messages,calls,phase,step,state FROM ui_turn_checkpoints WHERE turn_id=?",
            [turn],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .optional()
        .map_err(io::Error::other)?;
    let (messages, calls, phase, step, state) = row
        .filter(|(_, _, phase, _, _)| RESUMABLE_PHASES.contains(&phase.as_str()))
        .ok_or_else(|| io::Error::other("The saved checkpoint of this turn is no longer resumable"))?;
    let mut messages: Vec<Value> = serde_json::from_str(&messages)?;
    let mut receipt_turn = None;
    let in_flight = if phase == "ready" {
        messages.push(json!({"role":"user","content":RESTART_NOTE}));
        None
    } else {
        let calls: Vec<Value> = serde_json::from_str(&calls)?;
        // The dispatch record must be readable: without it, sent calls would look unsent.
        let state: Value = if step == "tools_dispatched" {
            serde_json::from_str(&state)?
        } else {
            serde_json::from_str(&state).unwrap_or(Value::Null)
        };
        receipt_turn = state["receipt_turn"].as_str().filter(|source| *source != turn).map(str::to_owned);
        // Only the `tools_dispatched` step records which calls were sent. Before it, the
        // batch stopped at admission and nothing was dispatched.
        let dispatched = if step == "tools_dispatched" {
            state["dispatched"].as_array().cloned().unwrap_or_default()
        } else {
            Vec::new()
        };
        Some(InFlight { calls, dispatched })
    };
    tx.execute("DELETE FROM ui_turn_restarts WHERE turn_id=?", [turn]).map_err(io::Error::other)?;
    tx.commit().map_err(io::Error::other)?;
    Ok(Some(Saved { messages, in_flight, receipt_turn }))
}

/// How one call of an in-flight batch is resolved after a restart (design section 12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Resolution {
    /// Not dispatched before the restart (refused or suppressed at admission, or the
    /// batch stopped before dispatch). Reported as not run.
    NotDispatched,
    /// A read, or a read-only validation: dispatched again. The read cache makes a
    /// repeated read cheap.
    Redispatch,
    /// A workspace file tool (`write_file`, `edit_file`): not run again. The manifest is
    /// compared with disk and the actual file state is the result.
    FileState,
    /// `shell`: not run again. A command recorded as a background job is reported with
    /// its job status; any other command has an unknown outcome, reported as a
    /// recoverable result that tells the agent to inspect the workspace.
    Shell,
    /// An action, a validation that changes state, an unknown effect, or a call the
    /// checkpoint records as a read that the current registry does not: only the owning
    /// adapter's saved receipt is used. Without a verified receipt the turn stops as
    /// uncertain. Never sent again.
    Receipt,
    /// A Host task-state tool ([`HOST_STATE_TOOLS`]): dispatched again through the
    /// normal Host path. These tools change only Host-owned state, in one transaction,
    /// and are idempotent or revision-checked, so a second dispatch cannot apply a change
    /// twice. They have no external receipt. `ask_human` asks the question again.
    HostState,
}

/// Validation tools that only read state, so they can be dispatched again.
const READ_ONLY_VALIDATIONS: &[&str] = &["browser_validate"];

/// Host-owned task-state tools with the `Action` effect that are safe to dispatch again
/// after a restart or "Resume task" ([`Resolution::HostState`]). External actions are not
/// in this list: they use only receipts.
pub(crate) const HOST_STATE_TOOLS: &[&str] = &[
    "record_required_outcomes",
    "record_outcome_evidence",
    "amend_required_outcome",
    "report_blocker",
    "task_plan_update",
    "task_plan_revise",
    "memory_put",
    "memory_delete",
    "ask_human",
];

/// The current Capability_Registry effect of a call `(name, arguments)`. On the Host it
/// is `Tools::call_capability`, which knows the operator's read-only MCP tagging.
pub(crate) type Catalog<'a> = &'a dyn Fn(&str, &Value) -> Effect;

/// The resolution of each dispatched call `(id, effect, name, arguments)`, in order.
/// `None` is an effect the checkpoint does not name; it is treated as an action.
///
/// A call is dispatched again as a read only when the checkpoint and the current
/// `catalog` both say it reads (or it is a read-only validation). A checkpoint that
/// records `read` for a tool the registry does not treat as a read (a damaged record, or
/// an MCP target no longer tagged read-only) is resolved by its receipt only.
pub(crate) fn resolve_plan(dispatched: &[(&str, Option<Effect>, &str, &Value)], catalog: Catalog<'_>) -> Vec<Resolution> {
    dispatched
        .iter()
        .map(|(_, effect, name, arguments)| match effect {
            Some(Effect::Read) if READ_ONLY_VALIDATIONS.contains(name) || catalog(name, arguments) == Effect::Read => {
                Resolution::Redispatch
            }
            Some(Effect::Read) => Resolution::Receipt,
            Some(Effect::Validation) if READ_ONLY_VALIDATIONS.contains(name) => Resolution::Redispatch,
            Some(Effect::Workspace) if *name == "shell" => Resolution::Shell,
            Some(Effect::Workspace) => Resolution::FileState,
            Some(Effect::Action) if HOST_STATE_TOOLS.contains(name) => Resolution::HostState,
            Some(Effect::Validation | Effect::Action) | None => Resolution::Receipt,
        })
        .collect()
}

/// The effect a checkpoint names (`ui_turn_checkpoints::effect_name`).
fn parse_effect(name: &Value) -> Option<Effect> {
    match name.as_str()? {
        "read" => Some(Effect::Read),
        "workspace" => Some(Effect::Workspace),
        "action" => Some(Effect::Action),
        "validation" => Some(Effect::Validation),
        _ => None,
    }
}

/// The resolution of every call of `batch`. A call missing from `dispatched` was never
/// sent. The arguments of a dispatched call come from the batch, so `catalog` sees the
/// target of an `mcp_call`. Pure given `catalog`, so the "never re-send" rule can be
/// checked without a Host.
pub(crate) fn plan_batch<'a>(batch: &'a InFlight, catalog: Catalog<'_>) -> Vec<(&'a Value, Resolution)> {
    let sent: Vec<(&str, Option<Effect>, &str, &Value)> = batch
        .dispatched
        .iter()
        .map(|call| {
            let id = call["id"].as_str().unwrap_or("");
            let arguments = batch
                .calls
                .iter()
                .find(|candidate| !id.is_empty() && candidate["id"] == id)
                .map_or(&Value::Null, |candidate| &candidate["arguments"]);
            (id, parse_effect(&call["effect"]), call["name"].as_str().unwrap_or(""), arguments)
        })
        .collect();
    let plans = resolve_plan(&sent, catalog);
    batch
        .calls
        .iter()
        .map(|call| {
            let id = call["id"].as_str().unwrap_or("");
            let plan = sent
                .iter()
                .position(|(sent, _, _, _)| !id.is_empty() && *sent == id)
                .map_or(Resolution::NotDispatched, |index| plans[index]);
            (call, plan)
        })
        .collect()
}

/// The resolved in-flight batch.
#[derive(Debug)]
pub(crate) struct Resolved {
    /// The assistant message with the batch, one tool result per call (paired by the
    /// original `tool_call_id`, in batch order) and the restart note.
    pub messages: Vec<Value>,
    /// `{call, content}` of each action resolved by a verified receipt, for
    /// `Tools::resume_receipts`, so a repeated call returns the receipt instead of
    /// sending the input again.
    pub receipts: Vec<Value>,
}

/// One resolved result.
struct Answer {
    content: String,
    is_error: bool,
    outcome: Value,
    /// The original-format result of an action resolved by its receipt.
    receipt: Option<String>,
}

impl Answer {
    fn ok(content: Value) -> Self {
        Self { content: content.to_string(), is_error: false, outcome: Value::Null, receipt: None }
    }
    /// A result the agent can act on by observing state first.
    fn recoverable(content: Value, code: &str, resource: &str) -> Self {
        Self {
            content: content.to_string(),
            is_error: true,
            outcome: json!({"kind":"recoverable","code":code,"resource":resource,"revision":"",
                "recovery":"refresh_observation","evidence":[]}),
            receipt: None,
        }
    }
}

/// Resolves the in-flight batch of a turn continuing after a restart (Requirement 13.3,
/// design section 12). Calls that send nothing are resolved first: receipts, shell state
/// and file state. If an action has no verified receipt, the turn stops as uncertain
/// before any call is dispatched. Reads, read-only validations and Host task-state tools
/// are then dispatched again through the normal Host path. No other call is dispatched.
pub(crate) fn resolve(host: &Tools<'_>, batch: &InFlight) -> io::Result<Resolved> {
    resolve_with(host, batch, &mut |call| redispatch(host, call))
}

/// [`resolve`] for a batch that the earlier turn `source` dispatched ("Resume task",
/// [`hand_over`]): receipts, journal entries and shell records are looked up under
/// `source`, and results are recorded for the current turn.
pub(crate) fn resolve_from(host: &Tools<'_>, batch: &InFlight, source: &str) -> io::Result<Resolved> {
    resolve_in(host, batch, source, &mut |call| redispatch(host, call))
}

/// [`resolve`] with the dispatch of a call that is sent again passed in. Every call the
/// resolution sends goes through `dispatch`, so a test can record them.
fn resolve_with(
    host: &Tools<'_>,
    batch: &InFlight,
    dispatch: &mut dyn FnMut(&Value) -> io::Result<Answer>,
) -> io::Result<Resolved> {
    resolve_in(host, batch, host.turn, dispatch)
}

/// [`resolve_with`] with the turn whose records hold the batch's receipts.
fn resolve_in(
    host: &Tools<'_>,
    batch: &InFlight,
    source: &str,
    dispatch: &mut dyn FnMut(&Value) -> io::Result<Answer>,
) -> io::Result<Resolved> {
    let plan = plan_batch(batch, &|name, arguments| host.call_capability(name, arguments).effect);
    let mut answers: Vec<Option<Answer>> = Vec::with_capacity(plan.len());
    for (call, resolution) in &plan {
        answers.push(match resolution {
            Resolution::Redispatch | Resolution::HostState => None,
            Resolution::NotDispatched => Some(not_dispatched(call)),
            Resolution::FileState => Some(file_state(host, call)?),
            Resolution::Shell => Some(shell_state(host, source, call)?),
            Resolution::Receipt => Some(match action_receipt(host, source, call)? {
                Some(answer) => answer,
                None => return Err(uncertain(host, call)),
            }),
        });
    }
    for ((call, _), answer) in plan.iter().zip(answers.iter_mut()) {
        if answer.is_none() {
            *answer = Some(dispatch(call)?);
        }
    }
    let mut messages = vec![json!({"role":"assistant","content":null,"tool_calls":batch.calls})];
    let mut receipts = Vec::new();
    for ((call, resolution), answer) in plan.iter().zip(answers) {
        let answer = answer.expect("every call is resolved");
        let id = &call["id"];
        let card_content = id.as_str().and_then(|id| host.take_full_output(id)).unwrap_or_else(|| answer.content.clone());
        webui::emit(host.db, host.chat, host.turn, "tool_card", json!({"id":id,"name":call["name"],
            "status":if answer.is_error {"failed"} else {"done"},"result_preview":card_content,
            "resolved_after_restart":true}))?;
        webui::emit(host.db, host.chat, host.turn, "event", json!({"kind":"recovery","interesting":true,
            "payload":{"phase":"restart","status":"resolved","tool_call_id":id,"tool":call["name"],
                "resolution":format!("{resolution:?}")}}))?;
        if let Some(receipt) = answer.receipt {
            receipts.push(json!({"call":call,"content":receipt}));
        }
        let mut result = json!({"role":"tool","tool_call_id":id,"content":answer.content,"is_error":answer.is_error});
        if !answer.outcome.is_null() {
            result["outcome"] = answer.outcome;
        }
        messages.push(result);
    }
    messages.push(json!({"role":"user","content":if source == host.turn {IN_FLIGHT_NOTE} else {RESUMED_IN_FLIGHT_NOTE}}));
    Ok(Resolved { messages, receipts })
}

fn name(call: &Value) -> &str {
    call["name"].as_str().unwrap_or("")
}

fn not_dispatched(call: &Value) -> Answer {
    Answer::recoverable(
        json!({"resolved_after_restart":true,"dispatched":false,"error_code":"not_dispatched",
            "summary":"This call was not run: the server restarted before it was sent. Call it again if it is still needed."}),
        "not_dispatched",
        name(call),
    )
}

/// Dispatches a read again. A terminal outcome (uncertain or fatal) stops the turn, as
/// in a normal batch.
fn redispatch(host: &Tools<'_>, call: &Value) -> io::Result<Answer> {
    let call: ToolCall = serde_json::from_value(call.clone())?;
    let reply: ToolReply = crate::native_chat::dispatch_one(host.workspace, host, &call, host.cancel)?;
    if let Some(error) = reply.terminal_error() {
        return Err(error);
    }
    Ok(Answer {
        content: reply.content,
        is_error: reply.is_error,
        outcome: reply.outcome.map(|outcome| json!(outcome)).unwrap_or(Value::Null),
        receipt: None,
    })
}

/// SHA-256 and size of the regular file at `path`, or `None` when there is none.
fn file_digest(path: &std::path::Path) -> io::Result<Option<(String, u64)>> {
    use sha2::{Digest, Sha256};
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if matches!(error.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => return Ok(None),
        Err(error) => return Err(error),
    };
    if !file.metadata()?.is_file() {
        return Ok(None);
    }
    let mut hasher = Sha256::new();
    let size = io::copy(&mut file, &mut hasher)?;
    Ok(Some((format!("{:x}", hasher.finalize()), size)))
}

/// The files changed since the manifest was last updated, registered as artifacts of the
/// turn, as after a finished workspace call.
fn register_changes(host: &Tools<'_>, workspace: &kageha_workspace::task::TaskWorkspace) -> io::Result<Vec<String>> {
    let changes = workspace.scan_changes().map_err(crate::ui_tools::workspace_failure)?;
    crate::ui_workspace::register_changes(host.db, host.chat, host.turn, &changes)
}

/// `write_file` or `edit_file` in flight: the call is not run again. The result is the
/// file as it is on disk, and for `write_file` whether it holds the intended content.
fn file_state(host: &Tools<'_>, call: &Value) -> io::Result<Answer> {
    use sha2::{Digest, Sha256};
    let workspace = crate::ui_workspace::open_for_chat(host.db, host.chat)?;
    register_changes(host, &workspace)?;
    let tool = name(call);
    let args = &call["arguments"];
    let path = crate::ui_tools::workspace_relative(args["path"].as_str().unwrap_or(""));
    let digest = match workspace.resolve(path) {
        Ok(target) => file_digest(&target)?,
        Err(_) => None,
    };
    let mut state = json!({"resolved_after_restart":true,"tool":tool,"path":path,"exists":digest.is_some()});
    if let Some((sha256, size)) = &digest {
        state["sha256"] = json!(sha256);
        state["size"] = json!(size);
        state["artifact_path"] = json!(crate::ui_workspace::artifact_path(path));
    }
    let summary = if tool == "write_file" {
        let intended = args["content"].as_str().map(|content| format!("{:x}", Sha256::digest(content.as_bytes())));
        let matches = intended.is_some() && digest.as_ref().map(|(sha, _)| sha) == intended.as_ref();
        state["intended_sha256"] = json!(intended);
        state["matches_intended"] = json!(matches);
        if matches {
            "The server restarted while this write was running. The file has the intended content, so the write took effect. It was not run again."
        } else {
            "The server restarted while this write was running. The file does not have the intended content, so the write did not take effect or the file changed later. It was not run again. Write the file again if it is still needed."
        }
    } else {
        "The server restarted while this change was running. The current file state is shown. It was not run again. Read the file to check whether the change was applied before changing it again."
    };
    state["summary"] = json!(summary);
    Ok(Answer::ok(state))
}

/// `shell` in flight. The command is not run again, because a workspace command is
/// low-risk but not idempotent:
///
/// - An escalated command was journaled with `ui_effects::run`: its saved result is the
///   result, and a dispatch without one stops the turn as uncertain (it may have written
///   outside the workspace).
/// - A command the Run_Journal records as a background job (started with `background`,
///   or a foreground command that outlived its timeout) is reported with the job's
///   status from `ui_shell_jobs`. Running jobs were re-adopted at startup, so
///   `shell_job` reaches them.
/// - A command the Run_Journal records as finished is reported with its exit code and
///   changed files; its output was not saved.
/// - Otherwise the outcome is unknown. The files changed since the last scan are
///   registered and listed, and the result tells the agent to inspect the workspace
///   before deciding to run the command again.
///
/// `source` is the turn that dispatched the command.
fn shell_state(host: &Tools<'_>, source: &str, call: &Value) -> io::Result<Answer> {
    let id = crate::ui_shell::file_call_id(call["id"].as_str().unwrap_or(""));
    let store = crate::runtime_host::store(host.db).map_err(|(_, error)| io::Error::other(error))?;
    if let Some(effect) = store.effect_get(source, &id).map_err(io::Error::other)? {
        let Some(receipt) = effect.receipt else { return Err(uncertain(host, call)) };
        return Ok(receipt_answer(host, call, "effect_journal", receipt)?);
    }
    let journal: Option<String> = host
        .db
        .lock()
        .map_err(|_| io::Error::other("database unavailable"))?
        .query_row(
            "SELECT payload FROM ui_events WHERE turn_id=? AND event='event' AND json_extract(payload,'$.kind')='shell_command'
             AND json_extract(payload,'$.payload.call_id')=? ORDER BY sequence DESC LIMIT 1",
            params![source, id],
            |row| row.get(0),
        )
        .optional()
        .map_err(io::Error::other)?;
    let journal: Value = journal.map(|text| serde_json::from_str(&text)).transpose()?.unwrap_or(Value::Null);
    let entry = &journal["payload"];
    let command = &call["arguments"]["command"];
    if let Some(job_id) = entry["job_id"].as_str() {
        let job: Option<(String, Option<i64>, String)> = host
            .db
            .lock()
            .map_err(|_| io::Error::other("database unavailable"))?
            .query_row(
                "SELECT state,exit_code,log_path FROM ui_shell_jobs WHERE job_id=? AND chat_id=?",
                params![job_id, host.chat],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(io::Error::other)?;
        if let Some((state, exit_code, log_path)) = job {
            return Ok(Answer::ok(json!({"resolved_after_restart":true,"outcome":"background_job","command":command,
                "job_id":job_id,"state":state,"exit_code":exit_code,"log_path":log_path,
                "summary":format!("The server restarted while this command was running. It continues, or ran, as background job {job_id}. It was not run again. Check it with shell_job (action status or output).")})));
        }
    }
    if let Some(exit_code) = entry["exit_code"].as_i64() {
        let content = json!({"resolved_after_restart":true,"outcome":"finished","command":command,"exit_code":exit_code,
            "files_changed":entry["files_changed"],
            "summary":format!("The command finished with exit code {exit_code} before the server restarted, but its output was not saved. It was not run again. Read the files it changed or run a read-only check if you need its output.")});
        return Ok(if exit_code == 0 { Answer::ok(content) } else { Answer::recoverable(content, "shell_exit_nonzero", "shell") });
    }
    let workspace = crate::ui_workspace::open_for_chat(host.db, host.chat)?;
    let changed = register_changes(host, &workspace)?;
    Ok(Answer::recoverable(
        json!({"resolved_after_restart":true,"outcome":"unknown","command":command,"files_changed":changed,
            "error_code":"shell_outcome_unknown",
            "summary":"The server restarted while this command was running, and no result was saved. It was not run again because it may have partly run. Inspect the workspace (list_workspace_files, read_file) before deciding whether to run it again."}),
        "shell_outcome_unknown",
        "shell",
    ))
}

/// The verified receipt of an in-flight action from the adapter that owns it, without
/// sending anything:
///
/// - `ui_tool_evidence`: the verified browser and computer input receipts (and other
///   Host-verified results) the Host saved when the call returned.
/// - The runtime effect journal (`ui_effects::run`): remote commands and uploads, media
///   and channel sends. A dispatch without a saved result is uncertain.
/// - The MCP journal: an `mcp_*` call with a completed result. One without is uncertain.
///
/// `None` when no verified receipt exists; the caller stops the turn as uncertain.
/// `source` is the turn that dispatched the action; its records hold the receipt.
fn action_receipt(host: &Tools<'_>, source: &str, call: &Value) -> io::Result<Option<Answer>> {
    let id = call["id"].as_str().unwrap_or("");
    let tool = name(call);
    if id.is_empty() {
        return Ok(None);
    }
    let evidence: Option<String> = host
        .db
        .lock()
        .map_err(|_| io::Error::other("database unavailable"))?
        .query_row(
            "SELECT body FROM ui_tool_evidence WHERE chat_id=? AND turn_id=? AND call_id=? AND name=?",
            params![host.chat, source, id, tool],
            |row| row.get(0),
        )
        .optional()
        .map_err(io::Error::other)?;
    if let Some(body) = evidence {
        return Ok(Some(receipt_answer(host, call, "tool_evidence", serde_json::from_str(&body)?)?));
    }
    let store = crate::runtime_host::store(host.db).map_err(|(_, error)| io::Error::other(error))?;
    if let Some(effect) = store.effect_get(source, id).map_err(io::Error::other)? {
        return effect.receipt.map(|receipt| receipt_answer(host, call, "effect_journal", receipt)).transpose();
    }
    if tool.starts_with("mcp_") {
        // A journal that can't be read gives no receipt, which stops the turn.
        let receipts = crate::ui_mcp::receipts(host.db, host.chat).unwrap_or(Value::Null);
        let receipt = receipts
            .as_array()
            .into_iter()
            .flatten()
            .find(|receipt| receipt["id"] == id && receipt["turn"] == source && receipt["state"] == "completed");
        if let Some(receipt) = receipt {
            return Ok(Some(receipt_answer(host, call, "mcp_journal", receipt["result"].clone())?));
        }
    }
    Ok(None)
}

/// The result of an in-flight action from its saved receipt. The receipt is also saved
/// as the call's evidence (a no-op when it already is), as when the call returns.
fn receipt_answer(host: &Tools<'_>, call: &Value, source: &str, receipt: Value) -> io::Result<Answer> {
    let raw = receipt.to_string();
    crate::ui_task_completion::remember_tool(host.db, host.chat, host.turn, call, &raw)?;
    // The adapters report these as failed calls (`ui_tools`): a remote command that
    // exited unsuccessfully and an MCP tool that reported an error.
    let is_error = (name(call) == "remote_exec" && receipt["ok"] != true) || receipt["isError"] == true;
    Ok(Answer {
        content: json!({"resolved_after_restart":true,"receipt_source":source,
            "summary":"This call was running when the server restarted. Its saved receipt is below. It was not sent again.",
            "result":receipt}).to_string(),
        is_error,
        outcome: Value::Null,
        receipt: (!is_error).then_some(raw),
    })
}

/// The uncertain stop for an in-flight action without a verified receipt. The turn ends
/// with the uncertain-outcome message; nothing is sent again (Requirement 19.2).
fn uncertain(host: &Tools<'_>, call: &Value) -> io::Error {
    let summary = format!(
        "`{}` (call {}) was running when the server restarted and has no verified receipt. It was not sent again. Inspect its original receipt before continuing.",
        name(call),
        call["id"].as_str().unwrap_or("")
    );
    let _ = webui::emit(host.db, host.chat, host.turn, "event", json!({"kind":"recovery","interesting":true,
        "payload":{"phase":"restart","status":"uncertain","tool_call_id":call["id"],"tool":call["name"],"summary":summary}}));
    kageha_contracts::execution::uncertain_stop(summary)
}

/// The run id of a turn held for unlock.
pub(crate) fn held(db: &Connection, turn: &str) -> rusqlite::Result<Option<String>> {
    db.query_row(
        "SELECT run_id FROM ui_turn_restarts WHERE turn_id=? AND state='waiting'",
        [turn],
        |row| row.get(0),
    )
    .optional()
}

/// Stop requested for a turn held for unlock: it has no worker, so it ends here.
pub(crate) fn cancel_held(db: &Database, supervisor: &kageha_runtime::lifecycle::Supervisor, turn: &str) -> ApiResult<bool> {
    let run = {
        let connection = db.lock().map_err(db_error)?;
        let Some(run) = held(&connection, turn).map_err(db_error)? else { return Ok(false) };
        connection.execute("DELETE FROM ui_turn_restarts WHERE turn_id=?", [turn]).map_err(db_error)?;
        run
    };
    supervisor
        .settle_held(&run, Completion {
            state: RunState::Cancelled,
            result: json!({"message":"Stopped while waiting for credential unlock.","not_dispatched":true}),
        })
        .map_err(|error| (409, error.message))?;
    Ok(true)
}

/// Called by the vault unlock handler: resumes every turn waiting for unlock. Returns
/// the resumed turn ids.
pub(crate) fn unlocked(db: &Database) -> ApiResult<Vec<String>> {
    let waiting: Vec<(String, String)> = {
        let mut connection = db.lock().map_err(db_error)?;
        if vault_locked(&connection) {
            return Ok(Vec::new());
        }
        let tx = connection.transaction().map_err(db_error)?;
        let rows: Vec<(String, String, String, String)> = {
            let mut query = tx
                .prepare(
                    "SELECT t.turn_id,t.run_id,json_extract(r.spec,'$.session'),json_extract(r.spec,'$.request')
                     FROM ui_turn_restarts t JOIN ui_runtime_runs r ON r.id=t.run_id WHERE t.state='waiting' ORDER BY t.created",
                )
                .map_err(db_error)?;
            let rows = query
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))
                .map_err(db_error)?
                .collect::<rusqlite::Result<_>>()
                .map_err(db_error)?;
            rows
        };
        for (turn, run, chat, request) in &rows {
            mark(&tx, turn, run, "resuming").map_err(db_error)?;
            set_status(&tx, chat, request, "running").map_err(db_error)?;
            continuing(&tx, chat, turn).map_err(db_error)?;
        }
        tx.commit().map_err(db_error)?;
        rows.into_iter().map(|(turn, run, _, _)| (turn, run)).collect()
    };
    if waiting.is_empty() {
        return Ok(Vec::new());
    }
    crate::ui_session_events::notify();
    let runtime = crate::runtime_host::get(db)?;
    let mut resumed = Vec::new();
    for (turn, run) in waiting {
        match runtime.supervisor.resume(&run) {
            Ok(_) => resumed.push(turn),
            // Stopped or finished meanwhile: nothing to continue.
            Err(error) => {
                db.lock().map_err(db_error)?.execute("DELETE FROM ui_turn_restarts WHERE turn_id=?", [&turn]).map_err(db_error)?;
                eprintln!("Turn {turn} was not resumed after unlock: {:?}", error.code);
            }
        }
    }
    Ok(resumed)
}

#[cfg(test)]
mod tests {
    //! Each fixture is a fresh database whose runtime journal holds the chat turn as
    //! `running` with the lease of a process that no longer exists. The first
    //! `runtime_host::get` for it is the server start: recovery runs [`restart`].
    use super::*;
    use crate::webui::{self, Database};
    use kageha_runtime::lifecycle::RunStore;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    const CHAT: &str = "resume-chat";
    const REQUEST: &str = "request-1";
    const PROMPT: &str = "Summarize the notes";
    const ANSWER: &str = "Summary finished after the restart.";

    /// An OpenAI-compatible model endpoint that answers every request with `ANSWER`
    /// and keeps the request bodies.
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
        turn: String,
        budget: Option<String>,
        configuration: Value,
        provider: Provider,
        project: std::path::PathBuf,
    }

    impl Fixture {
        /// A chat turn admitted with a request budget, interrupted by a process exit
        /// after `checkpoint` frames were recorded.
        fn new(checkpoint: &[Value]) -> Self {
            let directory = tempfile::tempdir().unwrap();
            let project = directory.path().join("project");
            std::fs::create_dir(&project).unwrap();
            let provider = Provider::start();
            // File-backed like the server's; no credential vault exists in the directory,
            // so the Keychain is never read.
            let connection = Connection::open(directory.path().join("native-chats.sqlite")).unwrap();
            // The schema `server::run` created before this version.
            connection.execute_batch(
                "CREATE TABLE native_chat_turns(sequence INTEGER PRIMARY KEY AUTOINCREMENT,chat_id TEXT NOT NULL,
                     request_id TEXT NOT NULL,message TEXT NOT NULL,answer TEXT,status TEXT NOT NULL,UNIQUE(chat_id,request_id));
                 CREATE UNIQUE INDEX native_chat_running ON native_chat_turns(chat_id) WHERE status='running';",
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
            let budget = configuration["request_budget_id"].as_str().map(str::to_owned);
            connection.execute("INSERT INTO native_chat_turns(chat_id,request_id,message,status) VALUES (?,?,?,'running')",
                params![CHAT, REQUEST, PROMPT]).unwrap();
            connection.execute("INSERT INTO ui_run_specs VALUES (?,?,?)", params![CHAT, REQUEST, configuration.to_string()]).unwrap();
            let spec = crate::runtime_host::spec(&project, CHAT, REQUEST, &turn, "main", PROMPT, 0, None, None,
                vec![json!({"role":"user","content":PROMPT})], configuration.clone(), 16);
            crate::runtime_host::prepare(&connection, &spec).unwrap();
            connection.execute("UPDATE ui_runtime_runs SET state='submitted' WHERE id=?", [&turn]).unwrap();
            if let Some(budget) = &budget {
                // Work done before the restart.
                connection.execute("UPDATE ui_request_budgets SET used_model_calls=3 WHERE id=?", [budget]).unwrap();
                crate::ui_persistence::save_recovery(&connection, &configuration, CHAT,
                    &json!({"version":2,"evidence":["pre-restart-evidence"]})).unwrap();
            }
            let db = crate::agent_host::DatabaseState::open(connection).unwrap();
            let root = crate::ui_workspace::open_for_chat(&db, CHAT).unwrap().root().to_path_buf();
            crate::ui_tools::test_free_disk::set(&root, u64::MAX);
            // The previous process claimed the run and died while executing it.
            let store = crate::runtime_host::store(&db).unwrap();
            store.stage(&spec, 16).unwrap();
            store.activate(&turn).unwrap();
            assert_eq!(store.claim(0).unwrap().unwrap().state, RunState::Running);
            for frame in checkpoint {
                // The Capability effect of each dispatched call, as the turn observer adds it.
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
            Self { _directory: directory, db, turn, budget, configuration, provider, project }
        }

        /// The chat's Task_Workspace.
        fn workspace(&self) -> kageha_workspace::task::TaskWorkspace {
            crate::ui_workspace::open_for_chat(&self.db, CHAT).unwrap()
        }

        /// The tool result for `id` in the turn's messages after the restart (the latest
        /// checkpoint, which keeps `is_error`), with its content parsed when it is JSON.
        /// The first model request after the restart received the same content once.
        fn result(&self, id: &str) -> (Value, Value) {
            let saved: String = self.db.lock().unwrap()
                .query_row("SELECT messages FROM ui_turn_checkpoints WHERE turn_id=?", [&self.turn], |row| row.get(0)).unwrap();
            let saved: Vec<Value> = serde_json::from_str(&saved).unwrap();
            let results: Vec<&Value> = saved.iter().filter(|m| m["role"] == "tool" && m["tool_call_id"] == id).collect();
            assert_eq!(results.len(), 1, "one result for {id}: {saved:?}");
            let result = results[0].clone();
            let requests = self.provider.requests();
            let sent: Vec<&Value> = requests.first().expect("the model was called")["messages"].as_array().unwrap()
                .iter().filter(|m| m["role"] == "tool" && m["tool_call_id"] == id).collect();
            assert_eq!(sent.len(), 1, "one result for {id} sent to the model");
            assert_eq!(sent[0]["content"], result["content"]);
            let content = result["content"].as_str().unwrap_or("");
            let parsed = serde_json::from_str(content).unwrap_or_else(|_| json!(content));
            (result, parsed)
        }

        /// Tool cards published for `id`.
        fn cards(&self, id: &str) -> Vec<Value> {
            self.events("tool_card").into_iter().filter(|card| card["id"] == id).collect()
        }

        /// The server start: recovery decides, then workers run.
        fn start(&self) -> Arc<crate::runtime_host::Host> {
            let host = crate::runtime_host::get(&self.db).unwrap();
            host.start().unwrap();
            host
        }

        fn status(&self) -> (String, Option<String>) {
            self.db.lock().unwrap()
                .query_row("SELECT status,answer FROM native_chat_turns WHERE chat_id=? AND request_id=?",
                    params![CHAT, REQUEST], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
        }

        fn wait_for(&self, status: &str) {
            let end = Instant::now() + Duration::from_secs(30);
            while self.status().0 != status {
                assert!(Instant::now() < end, "turn status stayed {:?}, expected {status}", self.status());
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        fn run_state(&self) -> RunState {
            crate::runtime_host::store(&self.db).unwrap().get(&self.turn).unwrap().unwrap().state
        }

        fn events(&self, event: &str) -> Vec<Value> {
            let connection = self.db.lock().unwrap();
            let mut query = connection.prepare("SELECT payload FROM ui_events WHERE turn_id=? AND event=? ORDER BY sequence").unwrap();
            let rows = query.query_map([&self.turn, event], |row| row.get::<_, String>(0)).unwrap()
                .map(|row| serde_json::from_str(&row.unwrap()).unwrap()).collect();
            rows
        }
    }

    fn checkpoint_messages() -> Vec<Value> {
        vec![json!({"role":"system","content":"Saved turn context"}), json!({"role":"user","content":PROMPT})]
    }

    /// Between steps: the model is about to be called again.
    fn ready() -> Vec<Value> {
        vec![json!({"kind":"model","messages":checkpoint_messages()})]
    }

    /// A batch was admitted when the process stopped. With `dispatched`, its calls were
    /// also sent (`tools_dispatched`); without, the process stopped before dispatch.
    fn in_flight(calls: Value, dispatched: bool) -> Vec<Value> {
        let mut frames = vec![
            json!({"kind":"model","messages":checkpoint_messages()}),
            json!({"kind":"tools","calls":calls}),
        ];
        if dispatched {
            frames.push(json!({"kind":"tools_dispatched","calls":calls}));
        }
        frames
    }

    fn call(id: &str, name: &str, arguments: Value) -> Value {
        json!({"id":id,"name":name,"arguments":arguments})
    }

    fn click(id: &str) -> Value {
        call(id, "browser_click", json!({"target":"e1","observation_id":"obs-1"}))
    }

    #[test]
    fn a_turn_without_a_runtime_run_is_interrupted_at_startup() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE native_chat_turns(sequence INTEGER PRIMARY KEY,chat_id TEXT,request_id TEXT,message TEXT,answer TEXT,status TEXT);
             CREATE TABLE ui_runtime_runs(id TEXT PRIMARY KEY,spec TEXT NOT NULL,state TEXT NOT NULL,projected_revision INTEGER NOT NULL DEFAULT 0);
             INSERT INTO native_chat_turns(chat_id,request_id,message,status) VALUES ('legacy','r1','Earlier work','running'),('managed','r2','Runtime work','running');
             INSERT INTO ui_runtime_runs VALUES ('run','{\"session\":\"managed\",\"request\":\"r2\"}','submitted',0);",
        ).unwrap();
        assert_eq!(startup(&db).unwrap(), 1);
        let statuses: Vec<String> = db.prepare("SELECT status FROM native_chat_turns ORDER BY sequence").unwrap()
            .query_map([], |row| row.get(0)).unwrap().collect::<Result<_, _>>().unwrap();
        // The runtime-managed turn is decided by the runtime host's recovery.
        assert_eq!(statuses, ["interrupted", "running"]);
    }

    #[test]
    fn a_runtime_turn_without_a_checkpoint_is_interrupted() {
        let fixture = Fixture::new(&[]);
        let host = fixture.start();
        fixture.wait_for("interrupted");
        assert_eq!(fixture.run_state(), RunState::Interrupted);
        assert!(fixture.provider.requests().is_empty());
        host.supervisor.shutdown().unwrap();
    }

    #[test]
    fn a_ready_checkpoint_continues_the_same_request_and_finishes() {
        let fixture = Fixture::new(&ready());
        let host = fixture.start();
        fixture.wait_for("completed");
        assert_eq!(fixture.status().1.as_deref(), Some(ANSWER));
        assert_eq!(fixture.run_state(), RunState::Succeeded);

        // The model continued from the checkpoint messages, not from a new history.
        let requests = fixture.provider.requests();
        let messages = requests[0]["messages"].as_array().unwrap();
        let count = |text: &str| messages.iter().filter(|m| m["content"] == text).count();
        assert_eq!((count("Saved turn context"), count(PROMPT)), (1, 1), "{messages:?}");
        assert_eq!(messages.last().unwrap()["content"], RESTART_NOTE);
        assert!(fixture.events("status").iter().any(|e| e["label"] == "Continuing after restart…"));
        assert!(fixture.db.lock().unwrap()
            .query_row("SELECT NOT EXISTS(SELECT 1 FROM ui_turn_restarts)", [], |row| row.get::<_, bool>(0)).unwrap());

        // The request budget and RecoveryState of the request carry over.
        let budget = fixture.budget.as_deref().expect("request persistence is enabled by default");
        let connection = fixture.db.lock().unwrap();
        let used: u32 = connection.query_row("SELECT used_model_calls FROM ui_request_budgets WHERE id=?", [budget], |row| row.get(0)).unwrap();
        assert_eq!(used, 3 + requests.len() as u32);
        let recovery = crate::ui_persistence::recovery(&connection, &fixture.configuration, CHAT).unwrap();
        assert!(recovery["evidence"].as_array().unwrap().contains(&json!("pre-restart-evidence")), "{recovery}");
        drop(connection);
        host.supervisor.shutdown().unwrap();
    }

    #[test]
    fn a_batch_stopped_before_dispatch_is_reported_as_not_run() {
        let write = call("write-1", "write_file", json!({"path":"outputs/note.md","content":"note"}));
        let fixture = Fixture::new(&in_flight(json!([write]), false));
        let host = fixture.start();
        fixture.wait_for("completed");
        // The write was never sent, so it is reported as not run, and not run now.
        let (result, content) = fixture.result("write-1");
        assert_eq!(result["is_error"], true);
        assert_eq!((content["dispatched"].clone(), content["error_code"].clone()), (json!(false), json!("not_dispatched")));
        assert!(!fixture.workspace().root().join("outputs/note.md").exists());
        host.supervisor.shutdown().unwrap();
    }

    #[test]
    fn an_in_flight_read_is_dispatched_again_once() {
        let read = call("read-1", "read_file", json!({"path":"notes.md"}));
        let fixture = Fixture::new(&in_flight(json!([read]), true));
        std::fs::write(fixture.project.join("notes.md"), "alpha notes").unwrap();
        let host = fixture.start();
        fixture.wait_for("completed");
        assert_eq!(fixture.status().1.as_deref(), Some(ANSWER));
        let (result, content) = fixture.result("read-1");
        assert_eq!(result["is_error"], false, "{result}");
        assert!(content.to_string().contains("alpha notes"), "{content}");
        // One dispatch: the resolution's. The model's assistant message precedes it, then the note.
        assert_eq!(fixture.cards("read-1").len(), 1);
        let requests = fixture.provider.requests();
        let messages = requests[0]["messages"].as_array().unwrap();
        assert_eq!(messages.last().unwrap()["content"], IN_FLIGHT_NOTE);
        host.supervisor.shutdown().unwrap();
    }

    #[test]
    fn an_in_flight_write_is_not_run_again_and_reports_the_file_on_disk() {
        let calls = json!([
            call("write-1", "write_file", json!({"path":"outputs/done.md","content":"done"})),
            call("write-2", "write_file", json!({"path":"outputs/missing.md","content":"missing"})),
        ]);
        let fixture = Fixture::new(&in_flight(calls, true));
        // The first write reached disk before the process stopped; the second did not.
        std::fs::create_dir_all(fixture.workspace().root().join("outputs")).unwrap();
        std::fs::write(fixture.workspace().root().join("outputs/done.md"), "done").unwrap();
        let host = fixture.start();
        fixture.wait_for("completed");

        let (_, done) = fixture.result("write-1");
        assert_eq!((done["resolved_after_restart"].clone(), done["exists"].clone(), done["matches_intended"].clone()),
            (json!(true), json!(true), json!(true)), "{done}");
        assert_eq!(done["sha256"], done["intended_sha256"]);
        let (_, missing) = fixture.result("write-2");
        assert_eq!((missing["exists"].clone(), missing["matches_intended"].clone()), (json!(false), json!(false)), "{missing}");
        assert!(!fixture.workspace().root().join("outputs/missing.md").exists(), "the write was not run again");
        host.supervisor.shutdown().unwrap();
    }

    #[test]
    fn an_in_flight_action_with_a_verified_receipt_uses_the_receipt() {
        let fixture = Fixture::new(&in_flight(json!([click("click-1")]), true));
        // The browser adapter's receipt, saved when the click returned.
        fixture.db.lock().unwrap().execute(
            "INSERT INTO ui_tool_evidence(chat_id,turn_id,call_id,name,receipt_id,body) VALUES (?,?,?,?,?,?)",
            params![CHAT, fixture.turn, "click-1", "browser_click", "req-1",
                json!({"status":"ok","receipt":{"state":"verified","request_id":"req-1"}}).to_string()],
        ).unwrap();
        let host = fixture.start();
        fixture.wait_for("completed");
        let (result, content) = fixture.result("click-1");
        assert_eq!(result["is_error"], false, "{result}");
        assert_eq!(content["receipt_source"], "tool_evidence");
        assert_eq!(content["result"]["receipt"]["request_id"], "req-1");
        // The card is the resolution's; the browser was never called.
        let cards = fixture.cards("click-1");
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0]["resolved_after_restart"], true);
        host.supervisor.shutdown().unwrap();
    }

    #[test]
    fn an_in_flight_action_without_a_receipt_stops_the_turn_as_uncertain() {
        let calls = json!([call("read-1", "read_file", json!({"path":"notes.md"})), click("click-1")]);
        let fixture = Fixture::new(&in_flight(calls, true));
        std::fs::write(fixture.project.join("notes.md"), "alpha notes").unwrap();
        let host = fixture.start();
        fixture.wait_for("failed");
        // Nothing was dispatched: not the click, not even the read, and not the model.
        assert!(fixture.provider.requests().is_empty());
        assert!(fixture.cards("click-1").is_empty() && fixture.cards("read-1").is_empty());
        let result: String = fixture.db.lock().unwrap()
            .query_row("SELECT result FROM ui_runtime_results WHERE turn=?", [&fixture.turn], |row| row.get(0)).unwrap();
        let result: Value = serde_json::from_str(&result).unwrap();
        assert_eq!((result["uncertain"].clone(), result["failure_code"].clone()), (json!(true), json!("tool_outcome_unconfirmed")), "{result}");
        assert!(fixture.events("event").iter().any(|event| event["kind"] == "recovery"
            && event["payload"]["status"] == "uncertain" && event["payload"]["tool_call_id"] == "click-1"));
        host.supervisor.shutdown().unwrap();
    }

    #[test]
    fn an_in_flight_shell_command_is_not_run_again() {
        let calls = json!([
            call("shell-1", "shell", json!({"command":"touch outputs/ran.txt"})),
            call("shell-2", "shell", json!({"command":"sleep 100","background":true})),
        ]);
        let fixture = Fixture::new(&in_flight(calls, true));
        {
            // The second command was started as a background job before the process stopped.
            let connection = fixture.db.lock().unwrap();
            webui::insert_event(&connection, CHAT, &fixture.turn, "event", json!({"kind":"shell_command",
                "payload":{"call_id":"shell-2","command":"sleep 100","background":true,"job_id":"job-7","state":"running"}})).unwrap();
            connection.execute("INSERT INTO ui_shell_jobs(job_id,chat_id,command,pid,started_ns,state,exit_code,log_path) VALUES ('job-7',?,'sleep 100',1,0,'running',NULL,'.kageha/jobs/job-7.log')",
                [CHAT]).unwrap();
        }
        let host = fixture.start();
        fixture.wait_for("completed");
        let (result, unknown) = fixture.result("shell-1");
        assert_eq!(result["is_error"], true);
        assert_eq!((unknown["outcome"].clone(), unknown["error_code"].clone()), (json!("unknown"), json!("shell_outcome_unknown")), "{unknown}");
        assert!(!fixture.workspace().root().join("outputs/ran.txt").exists(), "the command was not run again");
        let (result, job) = fixture.result("shell-2");
        assert_eq!(result["is_error"], false);
        assert_eq!((job["outcome"].clone(), job["job_id"].clone(), job["state"].clone()),
            (json!("background_job"), json!("job-7"), json!("running")), "{job}");
        host.supervisor.shutdown().unwrap();
    }

    #[test]
    fn the_plan_dispatches_again_only_reads_and_read_only_validations() {
        let batch = InFlight {
            calls: ["read", "validate", "remote", "click", "write", "shell", "unknown", "refused"]
                .iter().map(|id| json!({"id":id})).collect(),
            dispatched: vec![
                json!({"id":"read","name":"read_file","effect":"read"}),
                json!({"id":"validate","name":"browser_validate","effect":"validation"}),
                json!({"id":"remote","name":"remote_exec","effect":"action"}),
                json!({"id":"click","name":"browser_click","effect":"action"}),
                json!({"id":"write","name":"write_file","effect":"workspace"}),
                json!({"id":"shell","name":"shell","effect":"workspace"}),
                json!({"id":"unknown","name":"mcp_call","effect":null}),
            ],
        };
        let plan: Vec<Resolution> = plan_batch(&batch, &registry).into_iter().map(|(_, plan)| plan).collect();
        use Resolution::*;
        assert_eq!(plan, [Redispatch, Redispatch, Receipt, Receipt, FileState, Shell, Receipt, NotDispatched]);
        // A validation that is not a known read goes to its receipt.
        assert_eq!(resolve_plan(&[("v", Some(Effect::Validation), "remote_validate", &Value::Null)], &registry), [Receipt]);
    }

    /// The static Capability_Registry: no MCP target is tagged read-only.
    fn registry(name: &str, _arguments: &Value) -> Effect {
        crate::ui_tools::capability(name).effect
    }

    #[test]
    fn a_read_record_the_registry_does_not_confirm_uses_the_receipt() {
        use Resolution::*;
        let none = &Value::Null;
        // The checkpoint says read, but the registry says action (or an unknown tool).
        for name in ["browser_click", "remote_exec", "mcp_call", "mcp_github__create_issue", "memory_put", "write_file"] {
            assert_eq!(resolve_plan(&[("c", Some(Effect::Read), name, none)], &registry), [Receipt], "{name}");
        }
        // Both agree, or the tool is a read-only validation.
        assert_eq!(resolve_plan(&[("c", Some(Effect::Read), "read_file", none)], &registry), [Redispatch]);
        assert_eq!(resolve_plan(&[("c", Some(Effect::Read), "browser_validate", none)], &registry), [Redispatch]);
        // An `mcp_call` recorded as a read follows the current tagging of its target.
        let tagged = |name: &str, arguments: &Value| {
            if name == "mcp_call" && arguments["tool"] == "lookup" { Effect::Read } else { registry(name, arguments) }
        };
        let batch = InFlight {
            calls: vec![
                json!({"id":"lookup","name":"mcp_call","arguments":{"server":"fixture","tool":"lookup"}}),
                json!({"id":"echo","name":"mcp_call","arguments":{"server":"fixture","tool":"echo"}}),
            ],
            dispatched: vec![
                json!({"id":"lookup","name":"mcp_call","effect":"read"}),
                json!({"id":"echo","name":"mcp_call","effect":"read"}),
            ],
        };
        let plan: Vec<Resolution> = plan_batch(&batch, &tagged).into_iter().map(|(_, plan)| plan).collect();
        assert_eq!(plan, [Redispatch, Receipt]);
    }

    #[test]
    fn host_task_state_tools_are_dispatched_again_and_external_actions_use_receipts() {
        let effect = |name: &str| crate::ui_tools::capability(name).effect;
        let none = &Value::Null;
        for name in HOST_STATE_TOOLS {
            assert_eq!(effect(name), Effect::Action, "{name} is registered as an action");
            assert_eq!(resolve_plan(&[("c", Some(effect(name)), name, none)], &registry), [Resolution::HostState], "{name}");
        }
        // External actions, other Host actions and unknown effects keep receipt-only resolution.
        for name in ["browser_click", "remote_exec", "mcp_call", "channel_send", "goal_update", "agent_spawn"] {
            assert_eq!(resolve_plan(&[("c", Some(Effect::Action), name, none)], &registry), [Resolution::Receipt], "{name}");
        }
        assert_eq!(resolve_plan(&[("c", None, "record_required_outcomes", none)], &registry), [Resolution::Receipt]);
    }

    #[test]
    fn a_locked_vault_holds_the_turn_until_unlock() {
        let fixture = Fixture::new(&ready());
        test_vault::set(&fixture.db.lock().unwrap(), true);
        let host = fixture.start();
        assert_eq!(fixture.status().0, WAITING_FOR_UNLOCK);
        assert_eq!(fixture.run_state(), RunState::Running);
        assert!(fixture.events("event").iter().any(|e| e["kind"] == WAITING_FOR_UNLOCK));
        {
            let connection = fixture.db.lock().unwrap();
            let session = webui::session(&connection, CHAT).unwrap().unwrap();
            assert_eq!(session["active"], true);
            assert_eq!(session["active_turn"]["status"], WAITING_FOR_UNLOCK);
            // The waiting turn still blocks a new turn in the chat.
            assert!(connection.execute("INSERT INTO native_chat_turns(chat_id,request_id,message,status) VALUES (?,'other','More','running')", [CHAT]).is_err());
            let index: String = connection.query_row("SELECT sql FROM sqlite_master WHERE name='native_chat_running'", [], |row| row.get(0)).unwrap();
            assert!(index.contains(WAITING_FOR_UNLOCK), "{index}");
        }
        std::thread::sleep(Duration::from_millis(100));
        assert!(fixture.provider.requests().is_empty(), "a held turn has no worker");
        // Still locked: nothing continues.
        assert!(unlocked(&fixture.db).unwrap().is_empty());
        assert_eq!(fixture.status().0, WAITING_FOR_UNLOCK);

        test_vault::set(&fixture.db.lock().unwrap(), false);
        assert_eq!(unlocked(&fixture.db).unwrap(), [fixture.turn.clone()]);
        fixture.wait_for("completed");
        assert_eq!(fixture.status().1.as_deref(), Some(ANSWER));
        let requests = fixture.provider.requests();
        assert!(requests[0]["messages"].as_array().unwrap().iter().any(|m| m["content"] == "Saved turn context"));
        host.supervisor.shutdown().unwrap();
    }

    #[test]
    fn stopping_a_turn_held_for_unlock_cancels_it() {
        let fixture = Fixture::new(&ready());
        test_vault::set(&fixture.db.lock().unwrap(), true);
        let host = fixture.start();
        assert_eq!(fixture.status().0, WAITING_FOR_UNLOCK);
        assert!(host.cancel_turn(CHAT, &fixture.turn).unwrap());
        fixture.wait_for("cancelled");
        assert_eq!(fixture.run_state(), RunState::Cancelled);
        assert!(fixture.events("done").iter().any(|e| e["status"] == "cancelled"));
        test_vault::set(&fixture.db.lock().unwrap(), false);
        assert!(unlocked(&fixture.db).unwrap().is_empty());
        assert!(fixture.provider.requests().is_empty());
        host.supervisor.shutdown().unwrap();
    }

}

// Feature: autonomous-agent-loop, Property 13: Resume never re-sends actions
#[cfg(test)]
mod property_13 {
    //! Property 13 (design "Correctness Properties"): for any checkpoint, resuming
    //! dispatches no tool call id whose result is in the checkpoint, and every `Action`
    //! call that was in flight is resolved only by receipt lookup, giving a verified
    //! result or an uncertain stop. It is never dispatched again.
    //!
    //! **Validates: Requirements 13.3, 19.2, 19.7**
    //!
    //! Layer 1 checks the pure plan ([`plan_batch`], [`resolve_plan`]) over generated
    //! checkpoints. Layer 2 runs [`resolve_with`] against the real Host (`Tools` of the
    //! audited-session fixture, real receipts in `ui_tool_evidence` and the runtime
    //! effect journal) with a dispatcher that records every call it sends and forwards
    //! it to the real dispatch.
    use super::*;
    use crate::ui_tools::{capability, CAPABILITIES};
    use crate::ui_turn_checkpoints::effect_name;
    use proptest::prelude::*;
    use std::collections::HashSet;

    /// Host task-state tools. They change Host-internal state only (no external input),
    /// so this property does not constrain how they are resolved. Task 16.4 dispatches
    /// [`HOST_STATE_TOOLS`] (which include the memory tools and `ask_human`) again.
    const HOST_STATE: &[&str] = &[
        "record_required_outcomes",
        "amend_required_outcome",
        "record_outcome_evidence",
        "report_blocker",
        "task_plan_update",
        "task_plan_revise",
        "propose_run_change",
        "goal_update",
        "memory_put",
        "memory_delete",
        "ask_human",
    ];

    /// The MCP target the operator tagged read-only in the generated checkpoints.
    const TAGGED_TOOL: &str = "lookup";

    /// The current Capability_Registry, as `Tools::call_capability` answers it: an
    /// `mcp_call` to [`TAGGED_TOOL`] is a read, any other call has its registered effect.
    fn catalog(name: &str, arguments: &Value) -> Effect {
        if name == "mcp_call" && arguments["tool"] == TAGGED_TOOL {
            Effect::Read
        } else {
            capability(name).effect
        }
    }

    /// Whether a call sends input outside the Host when the registry's current effect
    /// for it is `current`: an action or state-changing validation, or a tool the Host
    /// does not own (MCP servers, connectors, channel sends), which is an action.
    fn external_action(name: &str, current: Effect) -> bool {
        !HOST_STATE.contains(&name)
            && (current == Effect::Action || (current == Effect::Validation && !READ_ONLY_VALIDATIONS.contains(&name)))
    }

    /// The `effect` a checkpoint holds for a dispatched call.
    #[derive(Debug, Clone)]
    enum Recorded {
        /// As the turn observer writes it: the call's current capability effect.
        Faithful,
        /// `mcp_call` recorded as a read when it was dispatched. Its target may no longer
        /// be tagged read-only.
        ReadOnlyMcp,
        /// `read` whatever the tool, as a damaged or stale checkpoint could hold. For an
        /// action the registry disagrees, so the call must use its receipt.
        ForgedRead,
        Null,
        Missing,
        Garbage(String),
    }

    #[derive(Debug, Clone)]
    struct GenCall {
        name: String,
        empty_id: bool,
        dispatched: bool,
        recorded: Recorded,
        /// For `mcp_call`: the target is [`TAGGED_TOOL`].
        tagged: bool,
    }

    fn arguments_of(call: &GenCall) -> Value {
        if call.name == "mcp_call" {
            json!({"server":"fixture","tool":if call.tagged {TAGGED_TOOL} else {"create_issue"},"arguments":{}})
        } else {
            json!({})
        }
    }

    fn tool_name() -> impl Strategy<Value = String> {
        let registered: Vec<String> = CAPABILITIES.iter().map(|spec| spec.name.to_owned()).collect();
        prop_oneof![
            6 => proptest::sample::select(registered),
            1 => proptest::sample::select(vec!["mcp_github__create_issue".to_owned(), "mcp_slack__post_message".to_owned(),
                "channel_send".to_owned(), "mcp_call".to_owned()]),
            1 => "[a-z_]{1,16}",
        ]
    }

    fn recorded() -> impl Strategy<Value = Recorded> {
        prop_oneof![
            6 => Just(Recorded::Faithful),
            1 => Just(Recorded::ReadOnlyMcp),
            2 => Just(Recorded::ForgedRead),
            1 => Just(Recorded::Null),
            1 => Just(Recorded::Missing),
            // An effect name the Host does not know (a valid name is `Faithful`).
            1 => "[a-z]{0,10}".prop_map(|text| Recorded::Garbage(
                if matches!(text.as_str(), "read" | "workspace" | "action" | "validation") { format!("{text}x") } else { text }
            )),
        ]
    }

    fn gen_call() -> impl Strategy<Value = GenCall> {
        (tool_name(), proptest::bool::weighted(0.05), proptest::bool::weighted(0.7), recorded(), proptest::bool::ANY).prop_map(
            |(name, empty_id, dispatched, recorded, tagged)| {
                // Only `mcp_call` can be recorded as a read-only target.
                let recorded = match recorded {
                    Recorded::ReadOnlyMcp if name != "mcp_call" => Recorded::Faithful,
                    other => other,
                };
                GenCall { name, empty_id, dispatched, recorded, tagged }
            },
        )
    }

    fn effect_field(name: &str, arguments: &Value, recorded: &Recorded) -> Option<Value> {
        match recorded {
            Recorded::Faithful => Some(json!(effect_name(catalog(name, arguments)))),
            Recorded::ReadOnlyMcp | Recorded::ForgedRead => Some(json!("read")),
            Recorded::Null => Some(Value::Null),
            Recorded::Missing => None,
            Recorded::Garbage(text) => Some(json!(text)),
        }
    }

    fn dispatched_entry(id: &str, name: &str, effect: Option<Value>) -> Value {
        let mut entry = json!({"id":id,"name":name});
        if let Some(effect) = effect {
            entry["effect"] = effect;
        }
        entry
    }

    /// A checkpoint: completed calls whose results are in the messages, and the
    /// in-flight batch. `stray` are `dispatched` records for completed ids, as a
    /// damaged checkpoint could hold; they are not part of the batch.
    fn checkpoint() -> impl Strategy<Value = (Vec<(String, String)>, Vec<GenCall>, Vec<(String, Recorded)>, Vec<usize>)> {
        (
            proptest::collection::vec(tool_name(), 0..4),
            proptest::collection::vec(gen_call(), 0..9),
            proptest::collection::vec((tool_name(), recorded()), 0..3),
        )
            .prop_flat_map(|(completed, batch, stray)| {
                let completed: Vec<(String, String)> =
                    completed.into_iter().enumerate().map(|(index, name)| (format!("done-{index}"), name)).collect();
                // The order of the `dispatched` records does not matter to the plan.
                let order = Just((0..batch.len() + stray.len()).collect::<Vec<_>>()).prop_shuffle();
                (Just(completed), Just(batch), Just(stray), order)
            })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2000))]

        // Feature: autonomous-agent-loop, Property 13: Resume never re-sends actions
        #[test]
        fn the_plan_never_sends_an_action_again((completed, batch, stray, order) in checkpoint()) {
            let ids: Vec<String> = batch.iter().enumerate()
                .map(|(index, call)| if call.empty_id { String::new() } else { format!("call-{index}") })
                .collect();
            let calls: Vec<Value> = batch.iter().zip(&ids)
                .map(|(call, id)| json!({"id":id,"name":call.name,"arguments":arguments_of(call)}))
                .collect();
            let mut records: Vec<Value> = batch.iter().zip(&ids)
                .filter(|(call, _)| call.dispatched)
                .map(|(call, id)| dispatched_entry(id, &call.name, effect_field(&call.name, &arguments_of(call), &call.recorded)))
                .collect();
            records.extend(stray.iter().zip(completed.iter().cycle())
                .map(|((name, recorded), (id, _))| dispatched_entry(id, name, effect_field(name, &Value::Null, recorded))));
            let mut dispatched: Vec<Value> = Vec::with_capacity(records.len());
            for index in order.iter().filter(|index| **index < records.len()) {
                dispatched.push(records[*index].clone());
            }
            let in_flight = InFlight { calls: calls.clone(), dispatched };
            let completed_ids: HashSet<&str> = completed.iter().map(|(id, _)| id.as_str()).collect();

            let plan = plan_batch(&in_flight, &catalog);
            // One resolution per call of the batch, in batch order.
            prop_assert_eq!(plan.len(), calls.len());
            for ((call, resolution), (expected, (generated, id))) in plan.iter().zip(in_flight.calls.iter().zip(batch.iter().zip(&ids))) {
                prop_assert!(std::ptr::eq(*call, expected));
                // A call with a result in the checkpoint is not in flight, so it has no
                // resolution and is never sent again.
                prop_assert!(!completed_ids.contains(id.as_str()));
                if !generated.dispatched || id.is_empty() {
                    prop_assert_eq!(*resolution, Resolution::NotDispatched, "{} was never sent", generated.name);
                    continue;
                }
                let name = generated.name.as_str();
                let current = catalog(name, &call["arguments"]);
                let recorded = effect_field(name, &call["arguments"], &generated.recorded).unwrap_or(Value::Null);
                let read_only_validation = READ_ONLY_VALIDATIONS.contains(&name);
                // The registry agrees that the call reads.
                let confirmed = current == Effect::Read || read_only_validation;
                let read = (recorded == "read" && confirmed) || (recorded == "validation" && read_only_validation);
                if read {
                    prop_assert_eq!(*resolution, Resolution::Redispatch);
                } else if !HOST_STATE.contains(&name) {
                    prop_assert_ne!(*resolution, Resolution::Redispatch, "{} ({}) is sent again", name, recorded);
                }
                // A read record the registry does not confirm uses the receipt only, also
                // for a Host task-state tool.
                if recorded == "read" && !confirmed {
                    prop_assert_eq!(*resolution, Resolution::Receipt, "{} recorded as a read is {:?} now", name, current);
                }
                if external_action(name, current) {
                    prop_assert_eq!(*resolution, Resolution::Receipt, "{} ({}) is not resolved by its receipt", name, recorded);
                }
            }
            // The same rule on the per-call function the plan uses.
            let sent: Vec<(&str, Option<Effect>, &str, &Value)> = in_flight.dispatched.iter()
                .map(|call| {
                    let id = call["id"].as_str().unwrap_or("");
                    let arguments = in_flight.calls.iter().find(|candidate| !id.is_empty() && candidate["id"] == id)
                        .map_or(&Value::Null, |candidate| &candidate["arguments"]);
                    (id, parse_effect(&call["effect"]), call["name"].as_str().unwrap_or(""), arguments)
                })
                .collect();
            for ((_, effect, name, arguments), resolution) in sent.iter().zip(resolve_plan(&sent, &catalog)) {
                if matches!(effect, Some(Effect::Action) | None) && !HOST_STATE.contains(name) {
                    prop_assert_eq!(resolution, Resolution::Receipt, "{} with effect {:?}", name, effect);
                }
                let confirmed = catalog(name, arguments) == Effect::Read || READ_ONLY_VALIDATIONS.contains(name);
                if *effect == Some(Effect::Read) && !confirmed {
                    prop_assert_eq!(resolution, Resolution::Receipt, "{} recorded as a read", name);
                }
            }
        }
    }

    // Layer 2: the real Host.

    /// The saved receipt state of a dispatched action (or shell command).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Saved {
        None,
        /// A verified receipt in `ui_tool_evidence`, as the Host saves it when the call returns.
        Evidence,
        /// Evidence under the same call id for another turn: not this call's receipt.
        ForeignEvidence,
        /// The effect journal holds the dispatch but no result.
        JournalBegun,
        /// The effect journal holds the dispatch and its result.
        JournalDone,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Kind {
        Read,
        Write,
        Shell,
        Action,
    }

    const ACTIONS: &[&str] = &["browser_click", "browser_fill", "browser_navigate", "browser_close", "computer_execute", "remote_exec", "remote_upload",
        "mcp_call", "generate_image", "text_to_speech", "channel_send", "mcp_github__create_issue"];

    fn host_call() -> impl Strategy<Value = (Kind, String, bool, Saved)> {
        let saved = prop_oneof![
            Just(Saved::None),
            Just(Saved::Evidence),
            Just(Saved::ForeignEvidence),
            Just(Saved::JournalBegun),
            Just(Saved::JournalDone),
        ];
        let call = prop_oneof![
            2 => proptest::sample::select(vec!["read_file", "list_workspace_files"]).prop_map(|name| (Kind::Read, name.to_owned())),
            1 => Just((Kind::Write, "write_file".to_owned())),
            1 => Just((Kind::Shell, "shell".to_owned())),
            4 => proptest::sample::select(ACTIONS.to_vec()).prop_map(|name| (Kind::Action, name.to_owned())),
        ];
        (call, proptest::bool::weighted(0.75), saved).prop_map(|((kind, name), dispatched, saved)| (kind, name, dispatched, saved))
    }

    fn arguments(kind: Kind, name: &str, id: &str) -> Value {
        match (kind, name) {
            (Kind::Read, "read_file") => json!({"path":"notes.md"}),
            (Kind::Read, _) => json!({}),
            (Kind::Write, _) => json!({"path":format!("outputs/{id}.md"),"content":"intended"}),
            (Kind::Shell, _) => json!({"command":format!("touch outputs/{id}.ran")}),
            (Kind::Action, "remote_exec") => json!({"command":"deploy"}),
            (Kind::Action, "mcp_call") => json!({"server":"github","tool":"create_issue","arguments":{}}),
            _ => json!({"target":"e1","observation_id":"obs-1"}),
        }
    }

    /// A verified receipt body for `name`.
    fn receipt(name: &str, id: &str) -> Value {
        match name {
            "remote_exec" => json!({"ok":true,"exit_code":0,"request_id":format!("req-{id}")}),
            _ => json!({"status":"ok","receipt":{"state":"verified","request_id":format!("req-{id}")}}),
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        // Feature: autonomous-agent-loop, Property 13: Resume never re-sends actions
        #[test]
        fn resolving_on_the_real_host_never_sends_an_action_again(batch in proptest::collection::vec(host_call(), 1..6)) {
            let fixture = crate::audited_session_tests::Fixture::new();
            let host = fixture.tools(json!({"id":"main","name":"Kageha"}));
            std::fs::write(host.workspace.join("notes.md"), "alpha notes").unwrap();
            let store = crate::runtime_host::store(&fixture.db).unwrap();
            let chat = crate::audited_session_tests::CHAT;

            let mut calls = Vec::new();
            let mut dispatched = Vec::new();
            let mut expected_sent = Vec::new();
            let mut uncertain = false;
            let mut receipts = 0;
            let mut action_ids = HashSet::new();
            for (index, (kind, name, sent, saved)) in batch.iter().enumerate() {
                let id = format!("call-{index}");
                let call = json!({"id":id,"name":name,"arguments":arguments(*kind, name, &id)});
                calls.push(call.clone());
                if matches!(kind, Kind::Action) {
                    action_ids.insert(id.clone());
                }
                if !sent {
                    continue;
                }
                dispatched.push(json!({"id":id,"name":name,"effect":effect_name(capability(name).effect)}));
                // Only actions and escalated shell commands have saved receipts.
                let saved = if matches!(kind, Kind::Action | Kind::Shell) { *saved } else { Saved::None };
                match saved {
                    Saved::None => {}
                    Saved::Evidence | Saved::ForeignEvidence => {
                        let turn = if saved == Saved::Evidence { fixture.turn.clone() } else { "another-turn".to_owned() };
                        fixture.db.lock().unwrap().execute(
                            "INSERT INTO ui_tool_evidence(chat_id,turn_id,call_id,name,receipt_id,body) VALUES (?,?,?,?,?,?)",
                            params![chat, turn, id, name, format!("req-{id}"), receipt(name, &id).to_string()],
                        ).unwrap();
                    }
                    Saved::JournalBegun | Saved::JournalDone => {
                        let journal_id = if *kind == Kind::Shell { crate::ui_shell::file_call_id(&id) } else { id.clone() };
                        store.effect_begin(&fixture.turn, &journal_id, "digest").unwrap();
                        if saved == Saved::JournalDone {
                            store.effect_finish(&fixture.turn, &journal_id, "digest", receipt(name, &id)).unwrap();
                        }
                    }
                }
                match kind {
                    Kind::Read => expected_sent.push(id.clone()),
                    Kind::Write => {}
                    // Without an escalation record the command's state is reported; with
                    // an unfinished one the outcome is uncertain.
                    Kind::Shell => match saved {
                        Saved::JournalBegun => uncertain = true,
                        Saved::JournalDone => receipts += 1,
                        _ => {}
                    },
                    Kind::Action => match saved {
                        Saved::Evidence | Saved::JournalDone => receipts += 1,
                        _ => uncertain = true,
                    },
                }
            }

            let in_flight = InFlight { calls: calls.clone(), dispatched };
            let mut sent: Vec<(String, String)> = Vec::new();
            let result = resolve_with(&host, &in_flight, &mut |call| {
                sent.push((call["id"].as_str().unwrap_or("").to_owned(), call["name"].as_str().unwrap_or("").to_owned()));
                redispatch(&host, call)
            });

            // No action, write or shell command is sent again, whatever the outcome.
            let resent: Vec<&(String, String)> = sent.iter().filter(|(id, _)| action_ids.contains(id)).collect();
            prop_assert!(resent.is_empty(), "actions sent again: {:?}", resent);
            prop_assert!(sent.iter().all(|(_, name)| capability(name).effect == Effect::Read), "{:?}", sent);
            match result {
                Err(error) => {
                    prop_assert!(uncertain, "unexpected failure: {}", error);
                    prop_assert!(kageha_contracts::execution::is_uncertain(&error), "not an uncertain stop: {}", error);
                    // The uncertain stop comes before any call is sent.
                    prop_assert!(sent.is_empty(), "{:?}", sent);
                }
                Ok(resolved) => {
                    prop_assert!(!uncertain, "an action without a verified receipt did not stop the turn");
                    let sent_ids: Vec<String> = sent.iter().map(|(id, _)| id.clone()).collect();
                    prop_assert_eq!(sent_ids, expected_sent);
                    prop_assert_eq!(resolved.receipts.len(), receipts);
                    // The assistant message, one result per call in batch order, and the note.
                    prop_assert_eq!(resolved.messages.len(), calls.len() + 2);
                    for (message, call) in resolved.messages[1..=calls.len()].iter().zip(&calls) {
                        prop_assert_eq!(&message["tool_call_id"], &call["id"]);
                    }
                    // No write or command in flight was run again in the Task_Workspace.
                    let root = crate::ui_workspace::open_for_chat(&fixture.db, chat).unwrap().root().to_path_buf();
                    for call in &calls {
                        let id = call["id"].as_str().unwrap();
                        let (ran, written) = (root.join(format!("outputs/{id}.ran")), root.join(format!("outputs/{id}.md")));
                        prop_assert!(!ran.exists() && !written.exists(), "{} ran again", id);
                    }
                }
            }
        }
    }
}
