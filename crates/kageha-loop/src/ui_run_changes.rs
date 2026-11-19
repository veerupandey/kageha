//! User-approved changes to mode or budget, admitted from a confirmed checkpoint.
use crate::{
    runtime_host, ui_goals, ui_job_controls,
    ui_store::{db_error, ApiResult},
    webui,
};
use kageha_runtime::lifecycle::{RunRecord, RunSpec, RunState};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::io;

pub(crate) fn initialize(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS ui_run_changes(
            chat TEXT PRIMARY KEY, turn TEXT NOT NULL, state TEXT NOT NULL,
            proposal TEXT NOT NULL, successor TEXT);
         CREATE TABLE IF NOT EXISTS ui_run_change_evidence(
            chat TEXT NOT NULL, digest TEXT NOT NULL, PRIMARY KEY(chat,digest));",
    )
}

pub(crate) fn current(db: &Connection, chat: &str) -> rusqlite::Result<Option<Value>> {
    let row: Option<(String, String, String, Option<String>)> = db
        .query_row(
            "SELECT turn,state,proposal,successor FROM ui_run_changes WHERE chat=?",
            [chat],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    row.map(|(turn, state, body, successor)| {
        let mut proposal: Value = serde_json::from_str(&body)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        proposal["turn"] = json!(turn);
        proposal["state"] = json!(state);
        proposal["successor"] = json!(successor);
        Ok(proposal)
    })
    .transpose()
}

pub(crate) fn supersede(db: &Connection, chat: &str) -> rusqlite::Result<()> {
    db.execute("UPDATE ui_run_changes SET state='superseded' WHERE chat=? AND state IN ('offered','pending')", [chat])?;
    Ok(())
}

pub(crate) fn offered(db: &Connection, chat: &str, turn: &str) -> rusqlite::Result<bool> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM ui_run_changes WHERE chat=? AND turn=? AND state='offered')",
        params![chat, turn],
        |row| row.get(0),
    )
}

fn conflict(message: &str) -> (u16, String) {
    (409, message.into())
}

pub(crate) fn propose(
    db: &webui::Database,
    chat: &str,
    turn: &str,
    args: &Value,
) -> io::Result<Value> {
    propose_inner(db, chat, turn, args).map_err(|(_, message)| io::Error::other(message))
}

fn propose_inner(db: &webui::Database, chat: &str, turn: &str, args: &Value) -> ApiResult<Value> {
    let text = |key| crate::ui_store::text(args, key, 1500);
    let kind = text("kind")?;
    let reason = text("reason")?;
    let next_action = text("next_action")?;
    let completion = if kind == "goal" {
        Some(text("completion_condition")?)
    } else {
        None
    };
    let evidence = args["evidence"]
        .as_array()
        .filter(|items| items.len() <= 12)
        .ok_or((400, "Provide up to twelve evidence IDs.".into()))?;
    if kind == "extend" && evidence.is_empty() {
        return Err((400, "An extension requires new verified evidence.".into()));
    }
    let mut connection = db.lock().map_err(db_error)?;
    let tx = connection.transaction().map_err(db_error)?;
    let run = source_run(&tx, chat, turn)?;
    let config = &run.input["configuration"];
    let mode = config["agent_mode"].as_str().unwrap_or("normal");
    if run.depth != 0
        || !config["workflow"].is_null()
        || !matches!(
            (mode, kind),
            ("normal", "plan" | "goal") | ("normal" | "plan" | "goal", "extend")
        )
    {
        return Err((
            400,
            "This change is unavailable in the current mode or workflow.".into(),
        ));
    }
    if current(&tx, chat)
        .map_err(db_error)?
        .is_some_and(|value| value["turn"] == turn)
    {
        return Err(conflict("This turn already proposed a change."));
    }
    if ui_goals::state(&tx, chat)
        .map_err(db_error)?
        .is_some_and(|goal| {
            goal["turn"] == turn && (goal["state"] != "active" || goal["decision"] != "")
        })
    {
        return Err(conflict("This goal already has an outcome or is paused."));
    }
    let checkpoint: String = tx.query_row(
        "SELECT messages FROM ui_turn_checkpoints WHERE turn_id=? AND phase IN ('ready','dispatching')",
        [turn], |row| row.get(0),
    ).optional().map_err(db_error)?.ok_or_else(|| conflict("Inspect the original action receipt before proposing a change."))?;
    if kind == "extend" {
        let messages: Vec<Value> = serde_json::from_str(&checkpoint).map_err(db_error)?;
        // Image observations and plan reminders can follow the results.
        let last_batch = messages.rsplit(|message| message["role"] == "assistant"
            && message["tool_calls"].as_array().is_some_and(|calls| !calls.is_empty()))
            .next().unwrap_or_default();
        let latest_results = last_batch.iter().filter(|message| message["role"] == "tool");
        for result in latest_results {
            let status = result["content"]
                .as_str()
                .and_then(|body| serde_json::from_str::<Value>(body).ok());
            if result["is_error"] == true
                || status.as_ref().is_some_and(|value| {
                    matches!(
                        value["status"].as_str(),
                        Some(
                            "disconnected"
                                | "needs_verification"
                                | "agent_not_assigned"
                                | "blocked"
                        )
                    )
                })
            {
                return Err(conflict(
                    "Resolve the latest tool blocker before proposing more budget.",
                ));
            }
        }
    }
    let mut digests = Vec::new();
    for id in evidence {
        let id = id.as_str().ok_or((400, "Use a tool call ID.".into()))?;
        let digest = ui_goals::evidence_digest(&tx, chat, turn, id)
            .map_err(|error| (400, error.to_string()))?;
        digests.push(digest);
    }
    if kind == "extend" && !fresh(&tx, chat, &digests)? {
        return Err(conflict(
            "This evidence already supported a budget extension.",
        ));
    }
    if kind == "extend" && config["persistence"]["enabled"] == true {
        let id = config["request_budget_id"].as_str().ok_or_else(|| conflict("The request budget is missing."))?;
        let budget = crate::ui_persistence::snapshot(&tx, id).map_err(db_error)?;
        let remaining = budget["remaining_model_calls"].as_u64().unwrap_or(0);
        if remaining < 2 || budget["remaining_duration_ms"].as_u64().unwrap_or(0) < 1000 {
            return Ok(json!({"status":"budget_limited","remaining_model_calls":remaining,
                "instruction":"Report verified progress and remaining work. A new request is needed for more budget."}));
        }
        for digest in &digests {
            tx.execute("INSERT OR IGNORE INTO ui_run_change_evidence VALUES (?,?)",
                params![chat, digest]).map_err(db_error)?;
        }
        webui::insert_event(&tx, chat, turn, "status", json!({
            "label":"Continuing within the request budget", "reason":reason,
            "next_action":next_action, "evidence":evidence, "remaining_model_calls":remaining
        })).map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        return Ok(json!({"status":"continuing","next_action":next_action,"remaining_model_calls":remaining,
            "instruction":"Continue with the next action in this turn."}));
    }
    let budget = match kind {
        "plan" => 256,
        "goal" => 512,
        _ => run.input["max_model_calls"].as_u64().unwrap_or(128),
    };
    let proposal = json!({"kind":kind,"reason":reason,"next_action":next_action,
        "completion_condition":completion,"evidence":evidence,"digests":digests,
        "model_calls":budget});
    tx.execute("INSERT INTO ui_run_changes VALUES (?,?,'offered',?,NULL)
        ON CONFLICT(chat) DO UPDATE SET turn=excluded.turn,state='offered',proposal=excluded.proposal,successor=NULL",
        params![chat,turn,proposal.to_string()]).map_err(db_error)?;
    tx.commit().map_err(db_error)?;
    Ok(
        json!({"status":"offered","instruction":"Conclude the turn. The user can review the proposed change."}),
    )
}

fn fresh(db: &Connection, chat: &str, digests: &[String]) -> ApiResult<bool> {
    for digest in digests {
        let used: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM ui_run_change_evidence WHERE chat=? AND digest=?)",
                params![chat, digest],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        if !used {
            return Ok(true);
        }
    }
    Ok(false)
}

fn source_run(db: &Connection, chat: &str, turn: &str) -> ApiResult<RunSpec> {
    let spec: String = db
        .query_row(
            "SELECT spec FROM ui_runtime_runs WHERE id=? AND json_extract(spec,'$.session')=?",
            params![turn, chat],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_error)?
        .ok_or((404, "Source run unavailable.".into()))?;
    serde_json::from_str(&spec).map_err(db_error)
}

fn safe_checkpoint(db: &Connection, turn: &str) -> rusqlite::Result<Option<String>> {
    db.query_row(
        "SELECT messages FROM ui_turn_checkpoints WHERE turn_id=? AND phase='ready'",
        [turn],
        |row| row.get(0),
    )
    .optional()
}

/// Publish a proposal only after its source turn has concluded safely.
pub(crate) fn settle(db: &Connection, run: &RunRecord) -> rusqlite::Result<bool> {
    let chat = &run.spec.session;
    let turn = &run.spec.turn;
    if !offered(db, chat, turn)? {
        return Ok(false);
    }
    let ready = run.state == RunState::Succeeded
        && safe_checkpoint(db, turn)?.is_some()
        && run
            .completion
            .as_ref()
            .is_some_and(|receipt| receipt.result["uncertain"] != true);
    let state = if ready { "pending" } else { "superseded" };
    db.execute(
        "UPDATE ui_run_changes SET state=? WHERE chat=?",
        params![state, chat],
    )?;
    if ready {
        db.execute(
            "UPDATE ui_goals SET state='paused',reason='A run change is awaiting your decision.'
            WHERE chat=? AND turn=? AND state='active'",
            params![chat, turn],
        )?;
        webui::insert_event(
            db,
            chat,
            turn,
            "status",
            json!({"label":"Run change ready to review"}),
        )?;
    }
    Ok(ready)
}

pub(crate) fn decide(db: &webui::Database, chat: &str, args: &Value) -> ApiResult<Value> {
    let turn = crate::ui_store::text(args, "turn", 128)?;
    let action = crate::ui_store::text(args, "action", 16)?;
    if !matches!(action, "accept" | "dismiss") {
        return Err((400, "Choose accept or dismiss.".into()));
    }
    let mut connection = db.lock().map_err(db_error)?;
    let tx = connection.transaction().map_err(db_error)?;
    let proposal = current(&tx, chat)
        .map_err(db_error)?
        .filter(|proposal| proposal["turn"] == turn)
        .ok_or_else(|| conflict("This proposal was superseded."))?;
    if (proposal["state"] == "accepted" && action == "accept")
        || (proposal["state"] == "dismissed" && action == "dismiss")
    {
        return Ok(proposal);
    }
    if proposal["state"] != "pending" {
        return Err(conflict("This proposal is no longer pending."));
    }
    if action == "dismiss" {
        tx.execute(
            "UPDATE ui_run_changes SET state='dismissed' WHERE chat=?",
            [chat],
        )
        .map_err(db_error)?;
    } else {
        admit(&tx, chat, turn, &proposal)?;
    }
    let result = current(&tx, chat).map_err(db_error)?.unwrap_or(Value::Null);
    tx.commit().map_err(db_error)?;
    crate::ui_session_events::notify();
    Ok(result)
}

fn admit(db: &Connection, chat: &str, turn: &str, proposal: &Value) -> ApiResult<()> {
    let mut run = source_run(db, chat, turn)?;
    let latest: Option<(String,String)> = db.query_row(
        "SELECT request_id,status FROM native_chat_turns WHERE chat_id=? ORDER BY sequence DESC LIMIT 1",
        [chat], |row| Ok((row.get(0)?,row.get(1)?)),
    ).optional().map_err(db_error)?;
    let busy: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM ui_pending_admissions WHERE chat_id=?1
         UNION ALL SELECT 1 FROM ui_session_flags WHERE chat_id=?1 AND archived=1)",
            [chat],
            |row| row.get(0),
        )
        .map_err(db_error)?;
    if busy
        || !latest.is_some_and(|(request, status)| request == run.request && status == "completed")
    {
        return Err(conflict("The chat changed since this proposal was made."));
    }
    let checkpoint = safe_checkpoint(db, turn)
        .map_err(db_error)?
        .ok_or_else(|| conflict("Inspect the original action receipt before continuing."))?;
    let mut messages: Vec<Value> = serde_json::from_str(&checkpoint).map_err(db_error)?;
    crate::ui_turn_checkpoints::store_images(db, chat, &mut messages).map_err(db_error)?;
    let config = &mut run.input["configuration"];
    let kind = proposal["kind"].as_str().unwrap_or("");
    let request = format!("change-{turn}");
    let successor = webui::turn_id(chat, &request);
    let label = match kind {
        "plan" => {
            config["agent_mode"] = json!("plan");
            config["plan_transition"] = json!(true);
            "Switch to Plan"
        }
        "goal" => {
            config["agent_mode"] = json!("goal");
            "Start Goal"
        }
        "extend" => "Extend execution budget",
        _ => return Err((400, "Unknown run change.".into())),
    };
    if config["agent_mode"] == "goal" {
        if kind == "goal" {
            let objective = format!(
                "{}\nCompletion condition: {}",
                run.objective,
                proposal["completion_condition"].as_str().unwrap_or("")
            );
            db.execute("DELETE FROM ui_goals WHERE chat=?", [chat])
                .map_err(db_error)?;
            ui_goals::begin(db, chat, &successor, &objective).map_err(db_error)?;
        } else {
            ui_goals::extend(db, chat, turn, &successor).map_err(db_error)?;
        }
    }
    if kind == "extend" && config["agent_mode"] == "plan" {
        let changed = db
            .execute(
                "UPDATE ui_plans SET turn_id=? WHERE chat_id=? AND turn_id=? AND approved=1",
                params![successor, chat, turn],
            )
            .map_err(db_error)?;
        if changed != 1 {
            return Err(conflict("The approved plan changed before continuation."));
        }
        config["plan_transition"] = json!(false);
    }
    if kind == "extend" {
        let digests: Vec<String> =
            serde_json::from_value(proposal["digests"].clone()).map_err(db_error)?;
        if !fresh(db, chat, &digests)? {
            return Err(conflict(
                "This evidence already supported a budget extension.",
            ));
        }
        for digest in digests {
            db.execute(
                "INSERT OR IGNORE INTO ui_run_change_evidence VALUES (?,?)",
                params![chat, digest],
            )
            .map_err(db_error)?;
        }
    }
    config["resume_checkpoint"] = json!(true);
    config["resumed_from_turn"] = json!(turn);
    config["run_change_continuation"] = json!(true);
    config["goal_continuation"] = json!(false);
    config["resume_receipts"] = json!(ui_job_controls::completed_receipts(&messages));
    messages.push(json!({"role":"user","content":format!(
        "I accepted {label}. Continue within the original scope from the saved checkpoint. Recheck current authority and reobserve live state before input. Completed actions already happened. Proposed next step: {}",
        proposal["next_action"].as_str().unwrap_or("")
    )}));
    let old_request = std::mem::replace(&mut run.request, request.clone());
    run.id = successor.clone();
    run.turn = successor.clone();
    let config = config.to_string();
    run.input["messages"] = json!(messages);
    run.input["max_model_calls"] = proposal["model_calls"].clone();
    db.execute(
        "INSERT INTO native_chat_turns(chat_id,request_id,message,status) VALUES (?,?,?,'running')",
        params![chat, request, label],
    )
    .map_err(db_error)?;
    db.execute(
        "INSERT INTO ui_run_specs VALUES (?,?,?)",
        params![chat, request, config],
    )
    .map_err(db_error)?;
    db.execute("INSERT INTO ui_turn_uploads SELECT chat_id,?1,paths FROM ui_turn_uploads WHERE chat_id=?2 AND request_id=?3",
        params![request,chat,old_request]).map_err(db_error)?;
    runtime_host::prepare(db, &run).map_err(db_error)?;
    db.execute(
        "UPDATE ui_run_changes SET state='accepted',successor=? WHERE chat=?",
        params![successor, chat],
    )
    .map_err(db_error)?;
    webui::insert_event(
        db,
        chat,
        &successor,
        "status",
        json!({"label":"Continuing from checkpoint…"}),
    )
    .map_err(db_error)?;
    Ok(())
}
