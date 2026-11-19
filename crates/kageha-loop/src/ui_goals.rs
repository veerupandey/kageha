//! Durable objectives and bounded continuation for WebUI Goal mode.
use crate::{runtime_host, webui};
use kageha_runtime::lifecycle::{RunRecord, RunState};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{io, path::Path};

const MAX_TURNS: u32 = 8;

pub(crate) fn initialize(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS ui_goals(
            chat TEXT PRIMARY KEY, id TEXT NOT NULL, objective TEXT NOT NULL,
            turn TEXT NOT NULL, state TEXT NOT NULL, reason TEXT NOT NULL,
            turns INTEGER NOT NULL, decision TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS ui_goal_evidence(
            chat TEXT NOT NULL, goal TEXT NOT NULL, digest TEXT NOT NULL,
            PRIMARY KEY(chat,goal,digest));
         CREATE TABLE IF NOT EXISTS ui_goal_extensions(
            chat TEXT NOT NULL, goal TEXT NOT NULL, turns INTEGER NOT NULL,
            PRIMARY KEY(chat,goal));",
    )
}

pub(crate) fn begin(
    db: &Connection,
    chat: &str,
    turn: &str,
    objective: &str,
) -> rusqlite::Result<()> {
    db.execute(
        "INSERT INTO ui_goals VALUES (?1,?2,?3,?2,'active','',1,'')
         ON CONFLICT(chat) DO UPDATE SET
            id=CASE WHEN state='completed' THEN excluded.id ELSE id END,
            objective=CASE WHEN state='completed' THEN excluded.objective ELSE objective END,
            turns=CASE WHEN state='completed' THEN 1 ELSE turns+1 END,
            turn=excluded.turn,state='active',reason='',decision=''",
        params![chat, turn, objective],
    )?;
    Ok(())
}

pub(crate) fn pause(db: &Connection, chat: &str) -> rusqlite::Result<()> {
    db.execute(
        "UPDATE ui_runtime_runs SET state='cancelled' WHERE state='prepared'
         AND id IN (SELECT turn FROM ui_goals WHERE chat=? AND state='active')",
        [chat],
    )?;
    db.execute(
        "UPDATE ui_goals SET state='paused',reason='Paused by user' WHERE chat=? AND state='active'",
        [chat],
    )?;
    Ok(())
}

pub(crate) fn state(db: &Connection, chat: &str) -> rusqlite::Result<Option<Value>> {
    db.query_row(
        "SELECT id,objective,turn,state,reason,turns,decision FROM ui_goals WHERE chat=?",
        [chat],
        |row| {
            Ok(json!({
                "id":row.get::<_,String>(0)?, "objective":row.get::<_,String>(1)?,
                "turn":row.get::<_,String>(2)?, "state":row.get::<_,String>(3)?,
                "reason":row.get::<_,String>(4)?, "turns":row.get::<_,u32>(5)?,
                "decision":row.get::<_,String>(6)?
            }))
        },
    )
    .optional()
}

pub(crate) fn update(
    db: &webui::Database,
    chat: &str,
    turn: &str,
    args: &Value,
) -> io::Result<Value> {
    let invalid = |message: &str| io::Error::new(io::ErrorKind::InvalidInput, message.to_owned());
    let decision = args["status"]
        .as_str()
        .filter(|value| matches!(*value, "progress" | "blocked" | "completed"))
        .ok_or_else(|| invalid("Choose progress, blocked, or completed."))?;
    let reason = args["reason"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= 1500)
        .ok_or_else(|| invalid("Give a short reason and the next step."))?;
    let mut connection = db
        .lock()
        .map_err(|_| io::Error::other("Goal state unavailable"))?;
    let tx = connection.transaction().map_err(io::Error::other)?;
    let goal = state(&tx, chat)
        .map_err(io::Error::other)?
        .filter(|goal| goal["state"] == "active" && goal["turn"] == turn)
        .ok_or_else(|| invalid("This turn has no active goal."))?;
    if goal["decision"] != "" {
        return Err(invalid(
            "This turn already recorded its goal outcome. Conclude the turn.",
        ));
    }
    if decision != "blocked" {
        let evidence = args["evidence"]
            .as_array()
            .filter(|items| !items.is_empty() && items.len() <= 12)
            .ok_or_else(|| invalid("Provide successful tool call IDs from this turn."))?;
        let mut added = 0;
        for id in evidence {
            let id = id.as_str().ok_or_else(|| invalid("Use a tool call ID."))?;
            let digest = evidence_digest(&tx, chat, turn, id)?;
            added += tx
                .execute(
                    "INSERT OR IGNORE INTO ui_goal_evidence VALUES (?,?,?)",
                    params![chat, goal["id"].as_str(), digest],
                )
                .map_err(io::Error::other)?;
        }
        if added == 0 {
            return Err(invalid(
                "This evidence was already recorded. Identify a new result or report the blocker.",
            ));
        }
    }
    tx.execute(
        "UPDATE ui_goals SET decision=?,reason=? WHERE chat=?",
        params![decision, reason, chat],
    )
    .map_err(io::Error::other)?;
    tx.commit().map_err(io::Error::other)?;
    Ok(json!({
        "status":decision,
        "evidence": if decision == "blocked" { json!([]) } else { args["evidence"].clone() },
        "instruction":"Conclude this turn with the current result."
    }))
}

pub(crate) fn evidence_digest(
    db: &Connection,
    chat: &str,
    turn: &str,
    id: &str,
) -> io::Result<String> {
    let invalid = |message: &str| io::Error::new(io::ErrorKind::InvalidInput, message.to_owned());
    let result: Option<(String, String)> = db
        .query_row(
            "SELECT json_extract(payload,'$.name'),json_extract(payload,'$.result_preview')
                 FROM ui_events WHERE chat_id=?1 AND turn_id=?2 AND event='tool_card'
                 AND json_extract(payload,'$.id')=?3 AND json_extract(payload,'$.status')='done'
                 AND json_type(payload,'$.result_preview')='text' ORDER BY sequence DESC LIMIT 1",
            params![chat, turn, id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(io::Error::other)?;
    let Some((name, result)) = result else {
        return Err(invalid(
            "A cited tool call has no successful result in this turn.",
        ));
    };
    let status = serde_json::from_str::<Value>(&result)
        .ok()
        .and_then(|data| data["status"].as_str().map(str::to_owned));
    if matches!(
        name.as_str(),
        "goal_update"
            | "propose_run_change"
            | "task_plan_update"
            | "task_plan_revise"
            | "ask_human"
            | "discover_capabilities"
            | "agent_status"
    ) || result.trim().is_empty()
        || (name == "integration_read" && status.as_deref().is_some_and(|value| value != "ok"))
    {
        return Err(invalid(
            "Cite task evidence, not a control call or access status.",
        ));
    }
    Ok(format!(
        "{:x}",
        Sha256::digest(json!([name, result]).to_string().as_bytes())
    ))
}

pub(crate) fn extend(
    db: &Connection,
    chat: &str,
    turn: &str,
    successor: &str,
) -> rusqlite::Result<()> {
    let changed = db.execute(
        "UPDATE ui_goals SET turn=?1,turns=turns+1,state='active',decision='',reason=''
        WHERE chat=?2 AND turn=?3 AND state='paused'",
        params![successor, chat, turn],
    )?;
    if changed != 1 {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    db.execute(
        "INSERT INTO ui_goal_extensions SELECT chat,id,1 FROM ui_goals WHERE chat=?
        ON CONFLICT(chat,goal) DO UPDATE SET turns=turns+1",
        [chat],
    )?;
    Ok(())
}

/// Called inside terminal publication. Persist the next turn with the current
/// result so restart can submit it through the existing prepared-run journal.
pub(crate) fn settle(db: &Connection, run: &RunRecord) -> rusqlite::Result<()> {
    let chat = &run.spec.session;
    let turn = &run.spec.turn;
    let Some(goal) =
        state(db, chat)?.filter(|goal| goal["state"] == "active" && goal["turn"] == *turn)
    else {
        return Ok(());
    };
    let phase: Option<String> = db
        .query_row(
            "SELECT phase FROM ui_turn_checkpoints WHERE turn_id=?",
            [turn],
            |row| row.get(0),
        )
        .optional()?;
    let safe = phase.as_deref() == Some("ready")
        && run.state == RunState::Succeeded
        && run
            .completion
            .as_ref()
            .is_some_and(|receipt| receipt.result["uncertain"] != true);
    let decision = goal["decision"].as_str().unwrap_or("");
    let reason = goal["reason"].as_str().unwrap_or("");
    let extra: u64 = db
        .query_row(
            "SELECT turns FROM ui_goal_extensions WHERE chat=? AND goal=?",
            params![chat, goal["id"].as_str()],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0);
    let (state, detail) = if !safe {
        (
            "paused",
            "The turn ended without a confirmed checkpoint. Review activity before resuming.",
        )
    } else if decision == "completed" {
        ("completed", reason)
    } else if decision == "blocked" {
        ("blocked", reason)
    } else if decision != "progress" {
        ("blocked", "The turn ended without new verified progress.")
    } else if goal["turns"].as_u64().unwrap_or(0) >= u64::from(MAX_TURNS) + extra {
        (
            "budget_limited",
            "The goal reached its automatic continuation limit.",
        )
    } else if !run.spec.input["configuration"]["workflow"].is_null() {
        ("paused", "Review the workflow before continuing the goal.")
    } else {
        ("active", reason)
    };
    db.execute(
        "UPDATE ui_goals SET state=?,reason=? WHERE chat=?",
        params![state, detail, chat],
    )?;
    if state != "active" {
        return webui::insert_event(
            db,
            chat,
            turn,
            "status",
            json!({"label":format!("Goal {state}"),"detail":detail}),
        );
    }
    let busy: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM native_chat_turns WHERE chat_id=?1 AND status='running'
             UNION ALL SELECT 1 FROM ui_pending_admissions WHERE chat_id=?1
             UNION ALL SELECT 1 FROM ui_session_flags WHERE chat_id=?1 AND archived=1)",
        [chat],
        |row| row.get(0),
    )?;
    if busy {
        db.execute("UPDATE ui_goals SET state='paused',reason='Another chat action is pending.' WHERE chat=?", [chat])?;
        return Ok(());
    }
    let request = format!("goal-{}", uuid::Uuid::new_v4().simple());
    let next_turn = webui::turn_id(chat, &request);
    let mut configuration = run.spec.input["configuration"].clone();
    configuration["goal_continuation"] = json!(true);
    let objective = goal["objective"].as_str().unwrap_or("");
    let workspace = Path::new(run.spec.input["workspace"].as_str().unwrap_or("."));
    let mut messages = run.spec.input["messages"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if let Some(answer) = run
        .completion
        .as_ref()
        .and_then(|receipt| receipt.result["message"].as_str())
    {
        messages.push(json!({"role":"assistant","content":answer}));
    }
    messages.push(json!({"role":"user","content":format!("Continue the active goal from the verified checkpoint. Latest progress and next step: {reason}. Preserve the original scope and do not repeat completed actions.")}));
    let next = runtime_host::spec(
        workspace,
        chat,
        &request,
        &next_turn,
        &run.spec.agent,
        objective,
        run.spec.depth,
        None,
        None,
        messages,
        configuration.clone(),
        run.spec.input["max_model_calls"].as_u64().unwrap_or(128) as u32,
    );
    db.execute(
        "INSERT INTO native_chat_turns(chat_id,request_id,message,status) VALUES (?,?,?,'running')",
        params![chat, request, "Continue active goal"],
    )?;
    db.execute(
        "INSERT INTO ui_run_specs VALUES (?,?,?)",
        params![chat, request, configuration.to_string()],
    )?;
    db.execute("INSERT INTO ui_turn_uploads SELECT chat_id,?1,paths FROM ui_turn_uploads WHERE chat_id=?2 AND request_id=?3",
        params![request,chat,run.spec.request])?;
    runtime_host::prepare(db, &next)?;
    db.execute(
        "UPDATE ui_goals SET turn=?,turns=turns+1,decision='' WHERE chat=?",
        params![next_turn, chat],
    )?;
    webui::insert_event(
        db,
        chat,
        &next_turn,
        "status",
        json!({"label":"Continuing goal…","detail":detail}),
    )
}
