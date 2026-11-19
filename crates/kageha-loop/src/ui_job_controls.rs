//! Worker attempts keep logical identity while continuation starts only at a saved safe boundary.
use crate::{
    ui_store::{self as store, db_error, ApiResult},
    webui::{self, Controls, Database},
};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{io, path::Path};

pub(crate) fn initialize(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS ui_job_current(job_id TEXT PRIMARY KEY,run_id TEXT NOT NULL,request_id TEXT NOT NULL,turn_id TEXT NOT NULL); CREATE TABLE IF NOT EXISTS ui_job_resumes(job_id TEXT NOT NULL,command_id TEXT NOT NULL,run_id TEXT NOT NULL,turn_id TEXT NOT NULL,PRIMARY KEY(job_id,command_id)); CREATE TABLE IF NOT EXISTS ui_turn_checkpoints(turn_id TEXT PRIMARY KEY,messages TEXT NOT NULL,calls TEXT NOT NULL,phase TEXT NOT NULL,updated REAL NOT NULL); CREATE TABLE IF NOT EXISTS ui_job_times(job_id TEXT PRIMARY KEY,started REAL,ended REAL);")?;
    db.execute_batch("CREATE TABLE IF NOT EXISTS ui_checkpoint_images(
        chat_id TEXT NOT NULL,digest TEXT NOT NULL,data TEXT NOT NULL,PRIMARY KEY(chat_id,digest)) WITHOUT ROWID;")?;
    // `phase` is the resume-safety state (`ready`, `dispatching`, `blocked_dispatching`,
    // `unverified`). `step` is the turn step of the last write (`model`,
    // `tools_dispatched`, `tools_done`, `gate`; design section 12) and `state` its data.
    let columns: Vec<String> = db
        .prepare("PRAGMA table_info(ui_turn_checkpoints)")?
        .query_map([], |r| r.get(1))?
        .collect::<Result<_, _>>()?;
    if !columns.iter().any(|name| name == "step") {
        db.execute_batch("ALTER TABLE ui_turn_checkpoints ADD COLUMN step TEXT NOT NULL DEFAULT '';
            ALTER TABLE ui_turn_checkpoints ADD COLUMN state TEXT NOT NULL DEFAULT '{}';")?;
    }
    Ok(())
}
pub(crate) fn attempt(
    db: &Connection,
    id: &str,
    child: &str,
) -> rusqlite::Result<(String, String, String)> {
    db.query_row(
        "SELECT run_id,request_id,turn_id FROM ui_job_current WHERE job_id=?",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .optional()
    .map(|r| r.unwrap_or_else(|| (id.into(), "job".into(), webui::turn_id(child, "job"))))
}
/// [`record`] without annotations. The turn observer calls [`record`]; tests use this.
#[cfg(test)]
pub(crate) fn checkpoint(db: &Database, turn: &str, frame: &Value) -> io::Result<()> {
    record(db, "test-chat", turn, frame, &Value::Null)
}
/// The previous checkpoint row of a turn.
struct Previous {
    messages: String,
    calls: String,
    phase: String,
    step: String,
    state: String,
}
/// Writes the turn checkpoint for one runtime frame, one upsert per step (Requirement
/// 13.1, design section 12). `annotations` is Host data the frame does not carry:
/// `recovery` (the RecoveryState snapshot) for `model`, and `effects` (call id to
/// Capability effect) for `tools_dispatched`. The `tools` frame, which precedes
/// admission, marks the batch `dispatching` and keeps the previous step.
pub(crate) fn record(db: &Database, chat: &str, turn: &str, frame: &Value, annotations: &Value) -> io::Result<()> {
    let kind = frame["kind"].as_str().unwrap_or("");
    if !matches!(kind, "model" | "tools" | "tools_dispatched" | "tool_results" | "gate") {
        return Ok(());
    }
    let mut connection = db.lock().map_err(|_| io::Error::other("Checkpoint store unavailable"))?;
    let db = connection.transaction().map_err(io::Error::other)?;
    let previous: Option<Previous> = db
        .query_row(
            "SELECT messages,calls,phase,step,state FROM ui_turn_checkpoints WHERE turn_id=?",
            [turn],
            |r| Ok(Previous { messages: r.get(0)?, calls: r.get(1)?, phase: r.get(2)?, step: r.get(3)?, state: r.get(4)? }),
        )
        .optional()
        .map_err(io::Error::other)?;
    let (messages, calls, phase, step, state): (String, String, &str, String, String) = match kind {
        "model" => (
            {
                let mut messages: Vec<Value> = serde_json::from_value(frame["messages"].clone())?;
                crate::ui_turn_checkpoints::store_images(&db, chat, &mut messages)?;
                serde_json::to_string(&messages)?
            },
            "[]".into(),
            if previous
                .as_ref()
                .is_some_and(|p| p.phase == "unverified" || p.phase == "blocked_dispatching")
            {
                "unverified"
            } else {
                "ready"
            },
            "model".into(),
            json!({"recovery":annotations["recovery"],
                "best_candidate":annotations["recovery"]["best_candidate"]}).to_string(),
        ),
        "tools" => {
            let Some(previous) = previous else {
                return Err(io::Error::other("Missing pre-dispatch checkpoint"));
            };
            let phase = if previous.phase == "unverified" {
                "blocked_dispatching"
            } else {
                "dispatching"
            };
            (previous.messages, frame["calls"].to_string(), phase, previous.step, previous.state)
        }
        "tools_dispatched" => {
            let Some(previous) = previous else {
                return Err(io::Error::other("Missing tool checkpoint"));
            };
            if !matches!(previous.phase.as_str(), "dispatching" | "blocked_dispatching") {
                return Err(io::Error::other("Invalid checkpoint transition"));
            }
            let dispatched: Vec<Value> = frame["calls"]
                .as_array()
                .ok_or_else(|| io::Error::other("Invalid tool checkpoint"))?
                .iter()
                .map(|call| json!({"id":call["id"],"name":call["name"],
                    "effect":annotations["effects"][call["id"].as_str().unwrap_or("")]}))
                .collect();
            let phase = if previous.phase == "blocked_dispatching" { "blocked_dispatching" } else { "dispatching" };
            (
                previous.messages,
                previous.calls,
                phase,
                "tools_dispatched".into(),
                json!({"dispatched":dispatched}).to_string(),
            )
        }
        "gate" => {
            let Some(previous) = previous else {
                return Err(io::Error::other("Missing pre-gate checkpoint"));
            };
            let phase = match previous.phase.as_str() {
                "ready" => "ready",
                "unverified" => "unverified",
                _ => return Err(io::Error::other("Invalid checkpoint transition")),
            };
            (
                previous.messages,
                previous.calls,
                phase,
                "gate".into(),
                json!({"gate":{"status":frame["status"],"rejections":frame["rejections"]}}).to_string(),
            )
        }
        _ => {
            let Some(Previous { messages, calls, phase, .. }) = previous else {
                return Err(io::Error::other("Missing tool checkpoint"));
            };
            if !matches!(phase.as_str(), "dispatching" | "blocked_dispatching") {
                return Err(io::Error::other("Invalid checkpoint transition"));
            }
            let mut messages: Vec<Value> = serde_json::from_str(&messages)?;
            let calls: Value = serde_json::from_str(&calls)?;
            let results = frame["results"]
                .as_array()
                .ok_or_else(|| io::Error::other("Invalid tool checkpoint"))?;
            let assistant = frame.get("assistant").filter(|message| {
                message["role"] == "assistant" && message["tool_calls"] == calls
            });
            messages.push(assistant.cloned().unwrap_or_else(|| {
                json!({"role":"assistant","content":null,"tool_calls":calls})
            }));
            messages.extend(results.iter().cloned());
            if let Some(after) = frame["after"].as_array() {
                messages.extend(after.iter().cloned());
            }
            crate::ui_turn_checkpoints::store_images(&db, chat, &mut messages)?;
            let uncertain = results.iter().any(|r| {
                r["is_error"] == true
                    && [
                        "receipt_pending",
                        "uncertain",
                        "not_confirmed",
                        "outcome_unknown",
                    ]
                    .iter()
                    .any(|needle| r["content"].as_str().unwrap_or("").contains(needle))
            });
            kageha_runtime::turn::compact_tool_output(&mut messages);
            let ids: Vec<&Value> = results.iter().map(|result| &result["tool_call_id"]).collect();
            (
                json!(messages).to_string(),
                "[]".into(),
                if uncertain || phase == "blocked_dispatching" {
                    "unverified"
                } else {
                    "ready"
                },
                "tools_done".into(),
                json!({"tool_call_ids":ids}).to_string(),
            )
        }
    };
    if messages.len() > 8 * 1024 * 1024 {
        return Err(io::Error::other("Checkpoint exceeds limit"));
    }
    db.execute("INSERT INTO ui_turn_checkpoints(turn_id,messages,calls,phase,updated,step,state) VALUES (?,?,?,?,?,?,?) ON CONFLICT(turn_id) DO UPDATE SET messages=excluded.messages,calls=excluded.calls,phase=excluded.phase,updated=excluded.updated,step=excluded.step,state=excluded.state",params![turn,messages,calls,phase,store::now(),step,state]).map_err(io::Error::other)?;
    db.commit().map_err(io::Error::other)
}
pub(crate) fn resume_state(db: &Connection, turn: &str, state: &str) -> rusqlite::Result<Value> {
    let phase: Option<String> = db
        .query_row(
            "SELECT phase FROM ui_turn_checkpoints WHERE turn_id=?",
            [turn],
            |r| r.get(0),
        )
        .optional()?;
    let reason = if !matches!(state, "failed" | "cancelled" | "interrupted" | "unverified") {
        Some("Only a stopped agent can continue.")
    } else {
        match phase.as_deref() {
            Some("ready") => None,
            Some("dispatching" | "blocked_dispatching" | "unverified") => {
                Some("A prior action has no confirmed checkpoint. Inspect its original receipt before continuing.")
            }
            _ => Some("This older run has no portable checkpoint. Its history is preserved; start a new assignment explicitly."),
        }
    };
    Ok(json!({"allowed":reason.is_none(),"reason":reason,"requires_confirmation":true}))
}

/// Whether the "Resume task" button can continue the chat turn `turn` that ended with
/// `status` (Requirement 13.5). Unlike a worker's [`resume_state`], a chat turn can also
/// continue from a batch that was in flight when it stopped: [`chat_resume`] hands the
/// batch to `ui_turn_resume`, which resolves it by effect. The request budget must also
/// allow another model call.
pub(crate) fn chat_resume_state(db: &Connection, turn: &str, status: &str) -> rusqlite::Result<Value> {
    let reason = match chat_resume_blocker(db, turn, status)? {
        Some(reason) => Some(reason),
        None => exhausted_budget(db, turn)?,
    };
    Ok(json!({"allowed":reason.is_none(),"reason":reason,"requires_confirmation":true}))
}

/// The checkpoint reason a stopped chat turn cannot continue, if any. A `ready`
/// checkpoint continues from its messages; a `dispatching` one written by the turn
/// observer (non-empty `step`) continues after its batch is resolved. A checkpoint after
/// an unconfirmed action (`unverified`, `blocked_dispatching`) does not continue.
fn chat_resume_blocker(db: &Connection, turn: &str, status: &str) -> rusqlite::Result<Option<String>> {
    if !matches!(status, "failed" | "cancelled" | "interrupted" | "unverified") {
        return Ok(Some("Only a stopped agent can continue.".into()));
    }
    // A turn that stopped as uncertain (for example an in-flight action without a
    // receipt after a restart) would stop again at once; its receipt decides.
    let result: Option<String> = db
        .query_row("SELECT result FROM ui_runtime_results WHERE turn=?", [turn], |r| r.get(0))
        .optional()?;
    if result
        .and_then(|result| serde_json::from_str::<Value>(&result).ok())
        .is_some_and(|result| result["uncertain"] == true || result["delivery_status"] == "uncertain")
    {
        return Ok(Some("An action of this task has an unconfirmed outcome. Inspect its original receipt before continuing.".into()));
    }
    let checkpoint: Option<(String, String)> = db
        .query_row("SELECT phase,step FROM ui_turn_checkpoints WHERE turn_id=?", [turn], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    Ok(match checkpoint {
        Some((phase, _)) if phase == "ready" => None,
        Some((phase, step)) if phase == "dispatching" && !step.is_empty() => None,
        Some((phase, _)) if matches!(phase.as_str(), "dispatching" | "blocked_dispatching" | "unverified") => {
            Some("A prior action has no confirmed checkpoint. Inspect its original receipt before continuing.".into())
        }
        _ => Some("This older run has no portable checkpoint. Its history is preserved; start a new assignment explicitly.".into()),
    })
}

/// The reason the request of chat turn `turn` cannot make another model call, if its
/// pinned request budget (`ui_persistence`) has no model calls or active time left. A
/// request without a budget (persistence disabled) has only its per-turn limit.
pub(crate) fn exhausted_budget(db: &Connection, turn: &str) -> rusqlite::Result<Option<String>> {
    let budget: Option<String> = db
        .query_row(
            "SELECT json_extract(spec,'$.input.configuration.request_budget_id') FROM ui_runtime_runs WHERE id=?",
            [turn],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let Some(budget) = budget else { return Ok(None) };
    let row: Option<(i64, i64, i64, i64)> = db
        .query_row(
            "SELECT used_model_calls,max_model_calls,active_ms,max_duration_ms FROM ui_request_budgets WHERE id=?",
            [&budget],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    Ok(match row {
        None => Some("The budget of this request is missing, so it cannot continue. Start a new request instead.".into()),
        Some((used, max, _, _)) if used >= max => Some(format!(
            "This request has used all {max} of its model calls, so it cannot continue. Start a new request instead."
        )),
        Some((_, _, active, max_ms)) if active >= max_ms => Some(format!(
            "This request has used all {} minutes of its active time, so it cannot continue. Start a new request instead.",
            (max_ms + 59_999) / 60_000
        )),
        _ => None,
    })
}

/// The chat turn the submission `payload` continues, or `None` for a new request
/// (Requirement 13.5, design section 12). Only explicit fields resume: `resume: true`
/// (sent by the UI "Resume task" button) continues the latest turn of `chat` before
/// `request`, and `resume_turn_id` names that turn. The message text is never
/// classified, so a typed "Resume task" starts a new request. A `resume: true` without a
/// saved turn is refused rather than started as a new request.
pub(crate) fn resume_source(db: &Connection, chat: &str, request: &str, payload: &Value) -> ApiResult<Option<String>> {
    let resume = match &payload["resume"] {
        Value::Null => false,
        Value::Bool(resume) => *resume,
        _ => return Err((400, "resume must be true or false".into())),
    };
    if let Some(source) = payload.get("resume_turn_id").filter(|value| !value.is_null()) {
        let source = source.as_str().filter(|source| !source.is_empty())
            .ok_or_else(|| (400, "resume_turn_id must be a turn id".to_owned()))?;
        return Ok(Some(source.to_owned()));
    }
    if !resume {
        return Ok(None);
    }
    let latest: Option<String> = db
        .query_row(
            "SELECT request_id FROM native_chat_turns WHERE chat_id=? AND request_id!=? ORDER BY sequence DESC LIMIT 1",
            params![chat, request],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_error)?;
    latest
        .map(|latest| Some(webui::turn_id(chat, &latest)))
        .ok_or_else(|| (409, "This chat has no saved task to resume. Start a new request instead.".into()))
}

/// Appended to the checkpoint messages of a chat turn continued from a `ready` checkpoint.
pub(crate) const CHAT_RESUME_NOTE: &str = "Continue from the saved checkpoint. Completed actions already happened. Reobserve the original browser or device before sending input. Verify any external submission with its original receipt or affirmative site confirmation.";

/// Continues the stopped chat turn `source` as the new turn `next` of the same logical
/// request (Requirement 13.5): `next` inherits the pinned configuration, so the request
/// budget (`request_budget_id`), RecoveryState and outcome contract carry over, and its
/// messages are the checkpoint's. A batch in flight in the checkpoint is handed to
/// `ui_turn_resume`, which resolves it by effect before the first model call.
pub(crate) fn chat_resume(
    db: &Connection,
    next: &mut kageha_runtime::lifecycle::RunSpec,
    source: &str,
    recovery: &Value,
) -> ApiResult<()> {
    let conflict = |message: &str| (409, message.to_owned());
    let chat = &next.session;
    let (request, status): (String, String) = db.query_row(
        "SELECT request_id,status FROM native_chat_turns WHERE chat_id=? AND request_id!=? ORDER BY sequence DESC LIMIT 1",
        params![chat,next.request], |row| Ok((row.get(0)?,row.get(1)?)),
    ).optional().map_err(db_error)?.ok_or_else(|| conflict("No saved turn to resume."))?;
    if webui::turn_id(chat, &request) != source {
        return Err(conflict("This chat has a newer turn. Refresh before resuming."));
    }
    // Every continuation calls the model again, so a used-up budget refuses it.
    if let Some(reason) = exhausted_budget(db, source).map_err(db_error)? {
        return Err(conflict(&reason));
    }
    let blocker = chat_resume_blocker(db, source, &status).map_err(db_error)?;
    let review = matches!(recovery["recovery_mode"].as_str(), Some("recheck" | "continue"));
    if let Some(reason) = blocker.filter(|_| !(status == "completed" && review)) {
        return Err(conflict(&reason));
    }
    let checkpoint: Option<(String, String)> = db.query_row(
        "SELECT messages,phase FROM ui_turn_checkpoints WHERE turn_id=? AND (phase='ready' OR (phase='dispatching' AND step!=''))",
        [source], |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional().map_err(db_error)?;
    let (messages, phase) = checkpoint.ok_or_else(||
        conflict("There is no confirmed checkpoint. Inspect the original action receipt before continuing.")
    )?;
    // A batch in flight is resolved by the new turn before its first model call; its
    // results and the continuation note follow the checkpoint messages.
    let in_flight = phase != "ready";
    let mut messages: Vec<Value> = serde_json::from_str(&messages).map_err(db_error)?;
    crate::ui_turn_checkpoints::store_images(db, chat, &mut messages).map_err(db_error)?;
    let spec: String = db.query_row("SELECT spec FROM ui_runtime_runs WHERE id=?", [source], |row| row.get(0))
        .optional().map_err(db_error)?.ok_or_else(|| conflict("Original runtime record unavailable."))?;
    let prior: kageha_runtime::lifecycle::RunSpec = serde_json::from_str(&spec).map_err(db_error)?;
    let current = &next.input["configuration"];
    let mut config = prior.input["configuration"].clone();
    if prior.session != *chat || prior.agent != next.agent || prior.depth != 0
        || config["remote"] != current["remote"] || config["machine"] != current["machine"]
        || prior.input["workspace"] != next.input["workspace"]
        || !config["workflow"].is_null()
    {
        return Err(conflict("The original workspace, agent, or execution target cannot be resumed here."));
    }
    config["browser_decisions"] = current["browser_decisions"].clone();
    config["resume_checkpoint"] = json!(true);
    config["resumed_from_turn"] = json!(source);
    config["plan_transition"] = json!(false);
    config["resume_receipts"] = json!(completed_receipts(&messages));
    if config["agent_mode"] == "plan" {
        let changed = db.execute(
            "UPDATE ui_plans SET turn_id=? WHERE chat_id=? AND turn_id=? AND approved=1",
            params![next.turn,chat,source],
        ).map_err(db_error)?;
        config["plan_transition"] = json!(changed == 0);
    } else if config["agent_mode"] == "goal" {
        let changed = db.execute(
            "UPDATE ui_goals SET turn=?1,state='active',decision='',reason='' WHERE chat=?2 AND turn=?3 AND state!='completed'",
            params![next.turn,chat,source],
        ).map_err(db_error)?;
        if changed == 0 { return Err(conflict("The original goal is no longer available to resume.")); }
    }
    if in_flight {
        crate::ui_turn_resume::hand_over(db, source, &next.turn, &next.id).map_err(db_error)?;
    } else {
        messages.push(json!({"role":"user","content":CHAT_RESUME_NOTE}));
    }
    // A user explicitly chose to continue after the recovery stop. Reset only
    // its observation/wrap-up allowance, atomically with admission; receipts,
    // action failures and the aggregate request budget are retained.
    let state=crate::ui_persistence::recovery(db,&config,chat).map_err(db_error)?;
    if !state.is_null() {
        let mut state=kageha_runtime::recovery::RecoveryState::load(state).map_err(db_error)?;
        if state.resume_after_stall() {
            crate::ui_persistence::save_recovery(db,&config,chat,&serde_json::to_value(state).map_err(db_error)?).map_err(db_error)?;
        }
    }
    next.objective = prior.objective;
    next.input["messages"] = json!(messages);
    next.input["max_model_calls"] = prior.input["max_model_calls"].clone();
    next.input["configuration"] = config;
    if let Some(task)=crate::ui_store::get(db,"task-scope",source).map_err(db_error)? {
        if task["chat"]!=*chat {return Err(conflict("The saved task belongs to another chat."));}
        crate::ui_store::put(db,"task-scope",&next.turn,task).map_err(db_error)?;
    }
    db.execute(
        "INSERT OR REPLACE INTO ui_turn_uploads SELECT chat_id,?1,paths FROM ui_turn_uploads WHERE chat_id=?2 AND request_id=?3",
        params![next.request,chat,request],
    ).map_err(db_error)?;
    Ok(())
}
pub(crate) fn expected(payload: &Value, row: &Value) -> ApiResult<()> {
    if let Some(turn) = payload["expected_turn_id"].as_str() {
        if row["progress"]["turn_id"] != turn {
            return Err((
                409,
                "Agent turn changed. Refresh before sending this command.".into(),
            ));
        }
    }
    if let Some(revision) = payload["expected_revision"].as_i64() {
        if row["revision"] != revision {
            return Err((
                409,
                "Agent state changed. Refresh before sending this command.".into(),
            ));
        }
    }
    Ok(())
}
pub(crate) fn resume(
    db: &Database,
    controls: &Controls,
    workspace: &Path,
    id: &str,
    payload: &Value,
) -> ApiResult<Value> {
    let command = store::text(payload, "client_request_id", 64)?;
    if !crate::server::identifier(command) || payload["confirmed"] != true {
        return Err((
            400,
            "Continuation requires a stable client_request_id and confirmed: true".into(),
        ));
    }
    let runtime = crate::runtime_host::get(db)?;
    let (run_id, already) = {
        let mut connection = db.lock().map_err(db_error)?;
        let tx = connection.transaction().map_err(db_error)?;
        let previous: Option<String> = tx
            .query_row(
                "SELECT run_id FROM ui_job_resumes WHERE job_id=? AND command_id=?",
                params![id, command],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?;
        if let Some(previous) = previous {
            (previous, true)
        } else {
            let row = crate::ui_jobs::job(&tx, id)
                .map_err(db_error)?
                .ok_or((404, "Worker not found".into()))?;
            expected(payload, &row)?;
            if row["resume"]["allowed"] != true {
                return Err((
                    409,
                    row["resume"]["reason"]
                        .as_str()
                        .unwrap_or("Agent cannot continue safely")
                        .into(),
                ));
            }
            let child = row["session_id"].as_str().unwrap();
            let (previous_run, _, previous_turn) = attempt(&tx, id, child).map_err(db_error)?;
            let record = runtime
                .supervisor
                .store
                .get(&previous_run)
                .map_err(|e| (409, e.message))?
                .ok_or((409, "Original runtime record unavailable".into()))?;
            if !record.state.terminal() {
                return Err((409, "Original execution is still active".into()));
            }
            if record.completion.as_ref().is_some_and(|c| {
                c.result["uncertain"] == true || c.result["delivery_status"] == "uncertain"
            }) {
                return Err((
                    409,
                    "Original execution outcome is uncertain; inspect its receipt".into(),
                ));
            }
            let messages: String = tx
                .query_row(
                    "SELECT messages FROM ui_turn_checkpoints WHERE turn_id=? AND phase='ready'",
                    [&previous_turn],
                    |r| r.get(0),
                )
                .map_err(db_error)?;
            let mut messages: Vec<Value> = serde_json::from_str(&messages).map_err(db_error)?;
            crate::ui_turn_checkpoints::store_images(&tx, child, &mut messages).map_err(db_error)?;
            let receipts = completed_receipts(&messages);
            messages.push(json!({"role":"user","content":"Continue the existing assignment from this saved checkpoint. Completed tool calls above already happened; do not repeat them. Reobserve live browser or device state before new input."}));
            let run_id = webui::turn_id(id, &format!("resume:{command}"));
            let request = format!("resume-{run_id}");
            let turn = webui::turn_id(child, &request);
            let mut configuration = record.spec.input["configuration"].clone();
            configuration["resume_receipts"] = json!(receipts);
            configuration["job_id"] = json!(id);
            configuration["resume_checkpoint"] = json!(true);
            configuration["resumed_from_turn"] = json!(previous_turn);
            let mut spec = record.spec.clone();
            spec.id = run_id.clone();
            spec.turn = turn.clone();
            spec.request = request.clone();
            spec.parent_run = None;
            spec.input["messages"] = json!(messages);
            spec.input["configuration"] = configuration.clone();
            tx.execute("INSERT INTO native_chat_turns(chat_id,request_id,message,status) VALUES (?,?,?,'running')",params![child,request,"Continue from saved checkpoint"]).map_err(db_error)?;
            tx.execute(
                "INSERT INTO ui_run_specs VALUES (?,?,?)",
                params![child, request, configuration.to_string()],
            )
            .map_err(db_error)?;
            tx.execute(
                "INSERT INTO ui_turn_uploads VALUES (?,?,'[]')",
                params![child, request],
            )
            .map_err(db_error)?;
            crate::runtime_host::prepare(&tx, &spec).map_err(db_error)?;
            tx.execute("INSERT INTO ui_turn_checkpoints(turn_id,messages,calls,phase,updated) VALUES (?,?,'[]','ready',?)",params![turn,spec.input["messages"].to_string(),store::now()]).map_err(db_error)?;
            tx.execute("INSERT INTO ui_job_current VALUES (?,?,?,?) ON CONFLICT(job_id) DO UPDATE SET run_id=excluded.run_id,request_id=excluded.request_id,turn_id=excluded.turn_id",params![id,run_id,request,turn]).map_err(db_error)?;
            tx.execute(
                "INSERT INTO ui_job_resumes VALUES (?,?,?,?)",
                params![id, command, run_id, turn],
            )
            .map_err(db_error)?;
            tx.execute(
                "UPDATE ui_jobs SET state='queued',revision=revision+1,result=NULL WHERE id=?",
                [id],
            )
            .map_err(db_error)?;
            tx.execute("UPDATE ui_job_times SET ended=NULL WHERE job_id=?", [id])
                .map_err(db_error)?;
            crate::ui_session_events::publish_worker(&tx, id, &turn).map_err(db_error)?;
            tx.commit().map_err(db_error)?;
            (run_id, false)
        }
    };
    // Activation is idempotent and repairs a crash after the atomic admission transaction.
    runtime.submit(&run_id)?;
    runtime.start()?;
    let _ = (controls, workspace, already);
    Ok(json!({"job":crate::ui_jobs::get(db,id)?,"continued":true,"run_id":run_id}))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn db() -> Database {
        let c = Connection::open_in_memory().unwrap();
        initialize(&c).unwrap();
        crate::agent_host::DatabaseState::open(c).unwrap()
    }
    #[test]
    fn checkpoint_continues_confirmed_tools_but_blocks_unknown_dispatch() {
        let db = db();
        let turn = "turn";
        checkpoint(
            &db,
            turn,
            &json!({"kind":"model","messages":[{"role":"user","content":"review"}]}),
        )
        .unwrap();
        checkpoint(&db,turn,&json!({"kind":"tools","calls":[{"id":"read","name":"read_file","arguments":{"path":"file"}}]})).unwrap();
        assert_eq!(
            resume_state(&db.lock().unwrap(), turn, "interrupted").unwrap()["allowed"],
            false
        );
        let assistant = json!({"role":"assistant","content":null,
            "tool_calls":[{"id":"read","name":"read_file","arguments":{"path":"file"}}],
            "continuation":{"route":"a".repeat(64),"tool_signatures":{"read":"opaque"}}});
        checkpoint(&db,turn,&json!({"kind":"tool_results","assistant":assistant,
            "results":[{"role":"tool","tool_call_id":"read","content":"proof","is_error":false}]})).unwrap();
        let c = db.lock().unwrap();
        assert_eq!(
            resume_state(&c, turn, "interrupted").unwrap()["allowed"],
            true
        );
        let raw: String = c
            .query_row(
                "SELECT messages FROM ui_turn_checkpoints WHERE turn_id=?",
                [turn],
                |r| r.get(0),
            )
            .unwrap();
        let messages: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(messages[1], assistant);
        assert_eq!(messages[2]["content"], "proof");
    }
    #[test]
    fn controls_reject_changed_turn_and_revision() {
        let row = json!({"revision":4,"progress":{"turn_id":"current"}});
        assert!(expected(&json!({"expected_turn_id":"old"}), &row).is_err());
        assert!(expected(&json!({"expected_revision":3}), &row).is_err());
        assert!(expected(
            &json!({"expected_turn_id":"current","expected_revision":4}),
            &row
        )
        .is_ok());
    }
    #[test]
    fn current_attempt_preserves_old_job_and_new_turn() {
        let c = Connection::open_in_memory().unwrap();
        initialize(&c).unwrap();
        assert_eq!(attempt(&c, "worker", "child").unwrap().0, "worker");
        c.execute(
            "INSERT INTO ui_job_current VALUES ('worker','new-run','new-request','new-turn')",
            [],
        )
        .unwrap();
        assert_eq!(
            attempt(&c, "worker", "child").unwrap(),
            ("new-run".into(), "new-request".into(), "new-turn".into())
        );
    }
    #[test]
    fn uncertain_reply_is_not_a_resumable_checkpoint() {
        let db = db();
        checkpoint(&db, "t", &json!({"kind":"model","messages":[]})).unwrap();
        checkpoint(&db, "t", &json!({"kind":"tools","calls":[]})).unwrap();
        checkpoint(&db,"t",&json!({"kind":"tool_results","results":[{"is_error":true,"content":"browser_receipt_pending"}]})).unwrap();
        assert_eq!(
            resume_state(&db.lock().unwrap(), "t", "failed").unwrap()["allowed"],
            false
        );
    }
}

pub(crate) fn completed_receipts(messages: &[Value]) -> Vec<Value> {
    let mut calls = std::collections::HashMap::new();
    let mut receipts = Vec::new();
    for message in messages {
        if let Some(batch) = message["tool_calls"].as_array() {
            for call in batch {
                if let Some(id) = call["id"].as_str() {
                    calls.insert(id.to_owned(), call.clone());
                }
            }
        }
        if message["role"] == "tool" && message["is_error"] != true {
            if let Some(call) = message["tool_call_id"]
                .as_str()
                .and_then(|id| calls.get(id))
            {
                receipts.push(json!({"call":call,"content":message["content"]}));
            }
        }
    }
    receipts
}
pub(crate) fn completed_receipt(receipts: &[Value], call: &Value) -> Option<String> {
    let name = call["name"].as_str()?;
    // Reads and checks must observe current state after continuation. Use the
    // registry so new read tools cannot accidentally inherit mutation deduplication.
    if matches!(crate::ui_tools::capability(name).effect,
        crate::ui_tools::Effect::Read | crate::ui_tools::Effect::Validation)
        || matches!(name, "browser_open" | "computer_observe" | "propose_run_change" | "goal_update"
            | "task_plan_update" | "task_plan_revise" | "ask_human")
    {
        return None;
    }
    receipts.iter().find(|saved|saved["call"]["name"]==call["name"]&&saved["call"]["arguments"]==call["arguments"]).map(|saved|json!({"recovered_receipt":true,"original_tool_call_id":saved["call"]["id"],"message":"This action completed before continuation. Input was not repeated.","result":saved["content"]}).to_string())
}

#[cfg(test)]
mod receipt_tests {
    use super::*;
    #[test]
    fn continuation_never_substitutes_old_reads_or_validation() {
        for name in ["browser_validate", "browser_inspect", "browser_open", "browser_reconcile", "web_fetch",
            "web_search", "list_workspace_files", "shell_job", "computer_permissions"] {
            let call = json!({"id":"old","name":name,"arguments":{}});
            let receipts = vec![json!({"call":call,"content":"old result"})];
            assert!(completed_receipt(&receipts, &call).is_none(), "{name}");
        }
    }
    #[test]
    fn continuation_recovers_mutation_receipt_even_if_model_changes_call_id() {
        let call = json!({"id":"original","name":"remote_upload","arguments":{"path":"proof","destination":"data"}});
        let receipts = completed_receipts(&[
            json!({"role":"assistant","tool_calls":[call.clone()]}),
            json!({"role":"tool","tool_call_id":"original","content":"confirmed upload","is_error":false}),
        ]);
        let mut repeated = call;
        repeated["id"] = json!("new-id");
        let result: Value =
            serde_json::from_str(&completed_receipt(&receipts, &repeated).unwrap()).unwrap();
        assert_eq!(result["original_tool_call_id"], "original");
        assert_eq!(result["recovered_receipt"], true);
    }
    #[test]
    fn failed_actions_are_not_recovered_as_success_and_reads_are_fresh() {
        let receipts = completed_receipts(&[
            json!({"role":"assistant","tool_calls":[{"id":"one","name":"read_file","arguments":{"path":"file"}}]}),
            json!({"role":"tool","tool_call_id":"one","content":"old","is_error":false}),
        ]);
        assert!(completed_receipt(
            &receipts,
            &json!({"id":"again","name":"read_file","arguments":{"path":"file"}})
        )
        .is_none());
    }
}

#[cfg(test)]
mod sticky_tests {
    use super::*;
    #[test]
    fn another_model_or_read_does_not_clear_an_unverified_mutation() {
        let c = Connection::open_in_memory().unwrap();
        initialize(&c).unwrap();
        let db = crate::agent_host::DatabaseState::open(c).unwrap();
        checkpoint(&db, "t", &json!({"kind":"model","messages":[]})).unwrap();
        checkpoint(&db, "t", &json!({"kind":"tools","calls":[]})).unwrap();
        checkpoint(&db,"t",&json!({"kind":"tool_results","results":[{"is_error":true,"content":"uncertain outcome"}]})).unwrap();
        checkpoint(&db, "t", &json!({"kind":"model","messages":[]})).unwrap();
        checkpoint(&db, "t", &json!({"kind":"tools","calls":[]})).unwrap();
        checkpoint(&db, "t", &json!({"kind":"tool_results","results":[]})).unwrap();
        assert_eq!(
            resume_state(&db.lock().unwrap(), "t", "failed").unwrap()["allowed"],
            false
        );
    }
}
