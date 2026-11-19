//! Durable foreground controls against Kageha's existing SQLite journal.
use crate::{reduce, Event, Phase, Snapshot};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Deserialize)]
pub struct ControlRequest {
    version: u32,
    database: PathBuf,
    session_id: String,
    turn_id: String,
    #[serde(flatten)]
    action: Action,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum Action {
    AdmitTurn(Admission),
    Cancel,
    Pulse {
        owner_json: String,
    },
    RevokeStale {
        owner_json: String,
        heartbeat: f64,
        timeout: f64,
    },
    RevokeOverdue {
        owner_json: String,
        grace: f64,
    },
    CompleteCleanup {
        generation: i64,
    },
    PublishResult {
        result: Value,
        reply: Option<Value>,
    },
    CloseInputs {
        through_input_id: i64,
    },
    AcknowledgeInput {
        input_id: i64,
    },
    Steer {
        message: String,
    },
    PrepareDispatch {
        token: String,
        resumed: bool,
    },
    AcceptDispatch {
        token: String,
        owner_json: String,
    },
    AdoptResume {
        owner_json: String,
        previous_owner_json: String,
        generation: i64,
        request_json: String,
        reset_dispatch: bool,
    },
    ClaimRecovery {
        owner_json: String,
        previous_owner_json: String,
        generation: i64,
        recoveries: i64,
        max_recoveries: i64,
        exhausted: bool,
    },
}

#[derive(Deserialize)]
struct Admission {
    idempotency_key: String,
    previous_metadata: Option<String>,
    previous_agent: Option<String>,
    metadata_json: Option<String>,
    objective: String,
    now: f64,
    event_id: String,
    payload_json: String,
    event_at: f64,
    snapshot_json: String,
    execution: Option<(String, String)>,
}

fn admit_turn(
    tx: Transaction<'_>,
    session_id: &str,
    turn_id: &str,
    input: &Admission,
) -> Result<Value, Box<dyn std::error::Error>> {
    let duplicate: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM events WHERE idempotency_key=?)",
        [&input.idempotency_key],
        |row| row.get(0),
    )?;
    if duplicate {
        return Ok(json!({"version": 1, "applied": false, "existing": true}));
    }
    let metadata: Option<String> = tx.query_row(
        "SELECT metadata_json FROM sessions WHERE id=?", [session_id], |row| row.get(0),
    ).optional()?;
    let first_payload: Option<String> = tx.query_row(
        "SELECT payload_json FROM events WHERE session_id=? AND kind='accepted' ORDER BY created_at LIMIT 1",
        [session_id], |row| row.get(0),
    ).optional()?;
    let first_agent = first_payload
        .and_then(|payload| serde_json::from_str::<Value>(&payload).ok())
        .and_then(|payload| payload["metadata"]["agent_id"].as_str().filter(|id| !id.is_empty()).map(str::to_owned));
    if metadata != input.previous_metadata || first_agent != input.previous_agent {
        return Ok(json!({"version": 1, "applied": false}));
    }

    if metadata.is_none() {
        let initial_metadata = input.metadata_json.as_ref().ok_or("missing initial session metadata")?;
        tx.execute(
            "INSERT INTO sessions(id,status,created_at,updated_at,metadata_json) VALUES (?,'active',?,?,?)",
            params![session_id, input.now, input.now, initial_metadata],
        )?;
    } else {
        tx.execute(
            "UPDATE sessions SET updated_at=?,metadata_json=COALESCE(?,metadata_json) WHERE id=?",
            params![input.now, input.metadata_json, session_id],
        )?;
    }
    let snapshot: Value = serde_json::from_str(&input.snapshot_json)?;
    let status = snapshot["status"].as_str().ok_or("missing initial status")?;
    let phase = snapshot["phase"].as_str().ok_or("missing initial phase")?;
    tx.execute(
        "INSERT INTO turns(id,session_id,objective,status,phase,snapshot_json,created_at,updated_at)
         VALUES (?,?,?,?,?,?,?,?)",
        params![turn_id, session_id, input.objective, status, phase, input.snapshot_json, input.now, input.now],
    )?;
    tx.execute(
        "INSERT INTO events(id,session_id,turn_id,sequence,kind,payload_json,created_at,idempotency_key)
         VALUES (?,?,?,1,'accepted',?,?,?)",
        params![input.event_id, session_id, turn_id, input.payload_json, input.event_at, input.idempotency_key],
    )?;
    if let Some((request, owner)) = &input.execution {
        tx.execute(
            "INSERT INTO foreground_executions(turn_id,request_json,owner_json) VALUES (?,?,?)",
            params![turn_id, request, owner],
        )?;
        tx.execute("INSERT INTO foreground_controls(turn_id) VALUES (?)", [turn_id])?;
    }
    let hash = format!("{:x}", Sha256::digest(format!("{turn_id}:1")));
    tx.execute(
        "INSERT INTO checkpoints(id,session_id,turn_id,sequence,snapshot_json,artifact_path,created_at)
         VALUES (?,?,?,1,?,'',?)",
        params![&hash[..32], session_id, turn_id, input.snapshot_json, input.event_at],
    )?;
    tx.commit()?;
    Ok(json!({"version": 1, "applied": true}))
}

pub fn control(request: ControlRequest) -> Result<Value, Box<dyn std::error::Error>> {
    if request.version != 1 {
        return Err("unsupported session control version".into());
    }
    let mut connection =
        Connection::open_with_flags(&request.database, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    connection.busy_timeout(Duration::from_secs(5))?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Action::AdmitTurn(input) = &request.action {
        return admit_turn(tx, &request.session_id, &request.turn_id, input);
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs_f64();
    let changed = match &request.action {
        Action::Pulse { owner_json } => Some(tx.execute(
            "UPDATE foreground_supervision SET heartbeat=? WHERE turn_id=? AND isolated=1
             AND EXISTS (SELECT 1 FROM foreground_executions e WHERE e.turn_id=? AND e.owner_json=?)",
            params![now, request.turn_id, request.turn_id, owner_json],
        )?),
        Action::RevokeStale { owner_json, heartbeat, timeout } => Some(tx.execute(
            "UPDATE foreground_supervision SET isolated=2 WHERE turn_id=?
             AND isolated=1 AND heartbeat=? AND heartbeat<? AND EXISTS (
                 SELECT 1 FROM foreground_executions e WHERE e.turn_id=? AND e.owner_json=?)",
            params![request.turn_id, heartbeat, now - timeout, request.turn_id, owner_json],
        )?),
        Action::RevokeOverdue { owner_json, grace } => Some(tx.execute(
            "UPDATE foreground_supervision SET isolated=2 WHERE turn_id=? AND isolated=1
             AND EXISTS (SELECT 1 FROM foreground_executions e WHERE e.turn_id=? AND e.owner_json=?)
             AND EXISTS (SELECT 1 FROM tool_attempts a WHERE a.turn_id=?
                 AND a.state='in_progress' AND a.deadline_at IS NOT NULL AND a.deadline_at<?)",
            params![request.turn_id, request.turn_id, owner_json, request.turn_id, now - grace],
        )?),
        Action::CompleteCleanup { generation } => Some(tx.execute(
            "UPDATE foreground_supervision SET cleanup_pending=0 WHERE turn_id=?
             AND EXISTS (SELECT 1 FROM foreground_executions e WHERE e.turn_id=? AND e.generation=?)",
            params![request.turn_id, request.turn_id, generation],
        )?),
        Action::CloseInputs { through_input_id } => Some(tx.execute(
            "UPDATE foreground_controls SET accepting=0 WHERE turn_id=?
             AND NOT EXISTS (SELECT 1 FROM foreground_inputs
                 WHERE turn_id=? AND delivered=0 AND id>?)",
            params![request.turn_id, request.turn_id, through_input_id],
        )?),
        Action::AcknowledgeInput { input_id } => Some(tx.execute(
            "UPDATE foreground_inputs SET delivered=1 WHERE id=?",
            [input_id],
        )?),
        Action::PublishResult { result, reply } => {
            let inserted = tx.execute(
                "INSERT OR IGNORE INTO foreground_results(turn_id,result_json) VALUES (?,?)",
                params![request.turn_id, serde_json::to_string(result)?],
            )?;
            if let Some(reply) = reply {
                let id = reply["job_reply_id"].as_str().ok_or("missing reply ID")?;
                tx.execute(
                    "INSERT OR IGNORE INTO job_reply_outbox(id,session_id,job_id,payload_json,created_at)
                     VALUES (?,?,?,?,?)",
                    params![id, request.session_id, format!("foreground:{}", request.turn_id),
                            serde_json::to_string(reply)?, now],
                )?;
            }
            tx.execute("UPDATE foreground_supervision SET isolated=0 WHERE turn_id=?", [&request.turn_id])?;
            Some(inserted)
        }
        _ => None,
    };
    if let Some(changed) = changed {
        tx.commit()?;
        return Ok(json!({"version": 1, "applied": changed == 1}));
    }

    let row: Option<(String, String)> = tx.query_row(
        "SELECT t.phase,t.snapshot_json FROM turns t JOIN foreground_executions e ON e.turn_id=t.id
         WHERE t.id=? AND (? OR t.session_id=?)",
        params![
            request.turn_id,
            matches!(&request.action, Action::PrepareDispatch { .. } | Action::AcceptDispatch { .. }),
            request.session_id
        ],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?;

    let Some((phase, snapshot_json)) = row else {
        return Ok(json!({"version": 1, "applied": false}));
    };
    let phase: Phase = serde_json::from_value(json!(phase))?;
    if phase.terminal() {
        return Ok(json!({"version": 1, "applied": false}));
    }

    let reply = match request.action {
        Action::PrepareDispatch { token, resumed } => {
            let changed = tx.execute(
                "INSERT INTO foreground_supervision(turn_id,token,heartbeat,resumed,cleanup_pending)
                 VALUES (?,?,?,?,1) ON CONFLICT(turn_id) DO UPDATE SET
                 token=excluded.token,heartbeat=excluded.heartbeat,resumed=excluded.resumed,cleanup_pending=1
                 WHERE foreground_supervision.isolated=0 AND foreground_supervision.cleanup_pending=0",
                params![request.turn_id, token, now, resumed],
            )?;
            if changed == 1 {
                tx.execute(
                    "DELETE FROM foreground_results WHERE turn_id=?",
                    [&request.turn_id],
                )?;
            }
            json!({"version": 1, "applied": changed == 1})
        }
        Action::AcceptDispatch { token, owner_json } => {
            let resumed: Option<bool> = tx
                .query_row(
                    "UPDATE foreground_supervision SET isolated=1,heartbeat=?
                 WHERE turn_id=? AND token=? AND isolated=0 RETURNING resumed",
                    params![now, request.turn_id, token],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(resumed) = resumed else {
                return Ok(json!({"version": 1, "applied": false}));
            };
            let request_json: String = tx.query_row(
                "UPDATE foreground_executions SET owner_json=? WHERE turn_id=? RETURNING request_json",
                params![owner_json, request.turn_id],
                |row| row.get(0),
            )?;
            json!({"version": 1, "applied": true, "request_json": request_json, "resumed": resumed})
        }
        Action::AdoptResume {
            owner_json,
            previous_owner_json,
            generation,
            request_json,
            reset_dispatch,
        } => {
            let bump = i64::from(owner_json != previous_owner_json);
            let changed = tx.execute(
                "UPDATE foreground_executions SET owner_json=?,request_json=?,generation=generation+?
                 WHERE turn_id=? AND owner_json=? AND generation=?
                 AND NOT EXISTS (SELECT 1 FROM durable_jobs j
                     WHERE json_extract(j.spec_json,'$.session_id')=?
                     AND (j.parent_turn=? OR j.root_id=?)
                     AND j.state NOT IN ('succeeded','failed','cancelled'))
                 AND NOT EXISTS (SELECT 1 FROM job_subscriptions s WHERE s.session_id=?
                     AND (json_extract(s.spec_json,'$.parent_turn')=? OR s.root_id=?)
                     AND s.state IN ('waiting','dispatched'))",
                params![owner_json, request_json, bump, request.turn_id, previous_owner_json,
                        generation, request.session_id, request.turn_id, request.turn_id,
                        request.session_id, request.turn_id, request.turn_id],
            )?;
            if changed == 1 && reset_dispatch && bump == 1 {
                tx.execute(
                    "DELETE FROM foreground_supervision WHERE turn_id=?",
                    [&request.turn_id],
                )?;
            }
            json!({"version": 1, "applied": changed == 1})
        }
        Action::ClaimRecovery {
            owner_json,
            previous_owner_json,
            generation,
            recoveries,
            max_recoveries,
            exhausted,
        } => {
            if (recoveries >= max_recoveries) != exhausted {
                return Ok(json!({"version": 1, "applied": false}));
            }
            let saved: Option<String> = tx.query_row(
                "UPDATE foreground_executions SET owner_json=?,recoveries=recoveries+?,generation=generation+1
                 WHERE turn_id=? AND owner_json=? AND recoveries=? AND generation=?
                 AND NOT EXISTS (SELECT 1 FROM durable_jobs j
                     WHERE json_extract(j.spec_json,'$.session_id')=?
                     AND (j.parent_turn=? OR j.root_id=?)
                     AND j.state NOT IN ('succeeded','failed','cancelled'))
                 AND NOT EXISTS (SELECT 1 FROM job_subscriptions s WHERE s.session_id=?
                     AND (json_extract(s.spec_json,'$.parent_turn')=? OR s.root_id=?)
                     AND s.state IN ('waiting','dispatched'))
                 RETURNING request_json",
                params![owner_json, i64::from(!exhausted), request.turn_id, previous_owner_json,
                        recoveries, generation, request.session_id, request.turn_id, request.turn_id,
                        request.session_id, request.turn_id, request.turn_id],
                |row| row.get(0),
            ).optional()?;
            let Some(request_json) = saved else {
                return Ok(json!({"version": 1, "applied": false}));
            };
            tx.execute(
                "UPDATE foreground_controls SET accepting=1 WHERE turn_id=?",
                [&request.turn_id],
            )?;
            tx.execute(
                "INSERT INTO foreground_supervision(turn_id,token,heartbeat,isolated,resumed,cleanup_pending)
                 VALUES (?,'recovered',?,1,1,1) ON CONFLICT(turn_id) DO UPDATE SET
                 token=excluded.token,heartbeat=excluded.heartbeat,isolated=1,resumed=1,cleanup_pending=1",
                params![request.turn_id, now],
            )?;
            json!({"version": 1, "applied": true, "request_json": request_json})
        }
        Action::Steer { message } => {
            if message.trim().is_empty() {
                return Err("message is required".into());
            }
            let count = tx.execute(
                "INSERT INTO foreground_inputs(turn_id,message)
                 SELECT turn_id,? FROM foreground_controls WHERE turn_id=? AND accepting=1",
                params![message, request.turn_id],
            )?;
            json!({"version": 1, "applied": count == 1, "input_id": (count == 1).then(|| tx.last_insert_rowid())})
        }
        Action::Cancel => {
            let terminal_key = format!("terminal:{}", request.turn_id);
            if tx
                .query_row(
                    "SELECT 1 FROM events WHERE idempotency_key=?",
                    [&terminal_key],
                    |_| Ok(()),
                )
                .optional()?
                .is_some()
            {
                return Ok(json!({"version": 1, "applied": false}));
            }
            let mut snapshot: Snapshot = serde_json::from_str(&snapshot_json)?;
            snapshot.session_id = request.session_id.clone();
            snapshot.turn_id = request.turn_id.clone();
            let sequence = snapshot
                .last_sequence
                .checked_add(1)
                .ok_or("journal sequence exhausted")?;
            let event = Event {
                session_id: request.session_id.clone(),
                turn_id: request.turn_id.clone(),
                sequence,
                kind: "cancelled".into(),
                payload: serde_json::from_value(json!({"status": "cancelled"}))?,
                created_at: now,
            };
            let mut state = Some(snapshot);
            reduce(&mut state, &event, now)?;
            let mut updated = state.ok_or("cancellation produced no snapshot")?;
            let progress = updated
                .metadata
                .entry("agent_progress")
                .or_insert(json!({}));
            if !progress.is_object() {
                *progress = json!({});
            }
            let progress = progress.as_object_mut().ok_or("invalid agent progress")?;
            for key in ["retry_at", "elapsed_s", "deadline_s", "wait_reason"] {
                progress.remove(key);
            }
            progress.extend([
                ("activity".into(), json!("cancelled")),
                ("summary".into(), json!("Work was stopped")),
                ("updated_at".into(), json!(now)),
            ]);
            let saved = serde_json::to_string(&updated)?;
            let hash = format!(
                "{:x}",
                Sha256::digest(format!("{}:{sequence}", request.turn_id))
            );
            tx.execute(
                "INSERT INTO events(id,session_id,turn_id,sequence,kind,payload_json,created_at,idempotency_key)
                 VALUES(lower(hex(randomblob(16))),?,?,?,?,?,?,?)",
                params![request.session_id, request.turn_id, sequence, event.kind,
                        serde_json::to_string(&event.payload)?, now, terminal_key],
            )?;
            tx.execute(
                "INSERT OR REPLACE INTO checkpoints(id,session_id,turn_id,sequence,snapshot_json,artifact_path,created_at)
                 VALUES(?,?,?,?,?,'',?)",
                params![&hash[..32], request.session_id, request.turn_id, sequence, saved, now],
            )?;
            tx.execute(
                "UPDATE turns SET status='cancelled',phase='cancelled',snapshot_json=?,updated_at=? WHERE id=? AND session_id=?",
                params![saved, now, request.turn_id, request.session_id],
            )?;
            tx.execute(
                "UPDATE sessions SET status='complete',updated_at=? WHERE id=?",
                params![now, request.session_id],
            )?;
            json!({"version": 1, "applied": true})
        }
        _ => unreachable!("supervision actions return before phase checks"),
    };
    tx.commit()?;
    Ok(reply)
}
