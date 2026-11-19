//! Rust-owned plan review, immutable approval and evidence-backed progress.
use crate::{
    native_chat, ui_approvals,
    ui_settings::Provider,
    ui_store::{db_error, ApiResult},
    webui::{self, Database},
};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{
    io,
    sync::atomic::{AtomicBool, Ordering},
};
use tiny_http::{Header, Method, Request, Response};
pub(crate) fn initialize(db: &rusqlite::Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS ui_plans(chat_id TEXT PRIMARY KEY,turn_id TEXT NOT NULL,document TEXT NOT NULL,revision INTEGER NOT NULL,approved INTEGER NOT NULL DEFAULT 0);")
}
fn document(db: &Database, chat: &str) -> ApiResult<Option<Value>> {
    db.lock()
        .map_err(db_error)?
        .query_row(
            "SELECT document,revision,approved,turn_id FROM ui_plans WHERE chat_id=?",
            [chat],
            |r| {
                let body: String = r.get(0)?;
                let mut value: Value = serde_json::from_str(&body).unwrap_or(json!({}));
                value["revision"] = r.get::<_, i64>(1)?.into();
                value["approved"] = r.get::<_, bool>(2)?.into();
                value["turn_id"] = r.get::<_, String>(3)?.into();
                Ok(value)
            },
        )
        .optional()
        .map_err(db_error)
}
pub(crate) fn board(value: &Value) -> Value {
    kageha_workflow::planning::board(value)
}
pub(crate) fn historical_context(db: &Database, chat: &str) -> ApiResult<Option<Value>> {
    Ok(document(db,chat)?.filter(|v|v["migration"]["interrupted"]==true).map(|v|json!({"objective":v["objective"],"required_outcomes":v["outcomes"],"constraints":v["constraints"],"historical_budget":v["historical_budget"],"historical_answers":v["historical_answers"],"review_required":true})))
}
pub(crate) fn markdown(value: &Value) -> String {
    kageha_workflow::planning::markdown(value)
}
fn plan_error(e: kageha_workflow::planning::Error) -> (u16, String) {
    use kageha_workflow::planning::Code;
    let status = match e.code {
        Code::Invalid => 400,
        Code::Conflict => 409,
        Code::NotFound => 404,
        Code::Unavailable => 503,
    };
    (status, e.message)
}
fn validate(value: &Value) -> ApiResult<()> {
    kageha_workflow::planning::validate(value).map_err(plan_error)
}
fn preserve_outcomes(old: &Value, next: &Value) -> ApiResult<()> {
    kageha_workflow::planning::preserve_outcomes(old, next).map_err(plan_error)
}
struct PlanEvidence<'a>(&'a Database);
const EVIDENCE_FILTER: &str = "chat_id=?1 AND turn_id=?2 AND event='tool_card'
    AND json_extract(payload,'$.status')='done'
    AND json_extract(payload,'$.name') NOT IN ('task_plan_update','task_plan_revise','propose_run_change','goal_update','agent_spawn','agent_status','agent_wait')";

/// Resolve public evidence handles to the originating successful tool call.
pub(crate) fn confirmed_call(connection: &Connection, chat: &str, turn: &str, reference: &str) -> rusqlite::Result<Option<String>> {
    let row = reference.strip_prefix("ev-").and_then(|id| id.parse::<i64>().ok());
    let mut statement = connection.prepare(&format!(
        "SELECT DISTINCT json_extract(payload,'$.id') FROM ui_events WHERE {EVIDENCE_FILTER}
         AND (rowid=?3 OR json_extract(payload,'$.id')=?4 OR
           CASE WHEN json_valid(json_extract(payload,'$.result_preview')) THEN
             CASE json_extract(payload,'$.name')
               WHEN 'web_fetch' THEN json_extract(payload,'$.result_preview') ->> 'source_id'=?4
                 AND json_extract(payload,'$.result_preview') ->> 'source_cached'=0
               WHEN 'browser_open' THEN json_extract(payload,'$.result_preview') ->> 'observation_id'=?4
               WHEN 'browser_snapshot' THEN json_extract(payload,'$.result_preview') ->> 'observation_id'=?4
             END END)
         LIMIT 2"
    ))?;
    let ids = statement.query_map(params![chat,turn,row,reference], |row| row.get::<_,String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(if ids.len() == 1 { ids.into_iter().next() } else { None })
}

pub(crate) fn recent_evidence(db: &Connection, chat: &str, turn: &str) -> rusqlite::Result<Vec<(String, String)>> {
    let mut statement = db.prepare(&format!(
        "SELECT 'ev-' || rowid,json_extract(payload,'$.name') FROM ui_events WHERE {EVIDENCE_FILTER} ORDER BY rowid DESC LIMIT 12"
    ))?;
    let rows = statement.query_map(params![chat,turn], |row| Ok((row.get(0)?,row.get(1)?)))?;
    rows.collect()
}
impl kageha_workflow::planning::EvidencePort for PlanEvidence<'_> {
    fn successful(
        &self,
        key: &kageha_workflow::planning::Key,
        id: &str,
    ) -> kageha_workflow::planning::Result<bool> {
        self.0.lock().map_err(|_|kageha_workflow::planning::failure(kageha_workflow::planning::Code::Unavailable,"Plan evidence unavailable"))?.query_row(&format!("SELECT EXISTS(SELECT 1 FROM ui_events WHERE {EVIDENCE_FILTER} AND json_extract(payload,'$.id')=?3)"),params![key.session,key.turn,id],|r|r.get(0)).map_err(|e|kageha_workflow::planning::failure(kageha_workflow::planning::Code::Unavailable,e.to_string()))
    }
}
pub(crate) fn prepare(
    db: &Database,
    chat: &str,
    turn: &str,
    messages: &[Value],
    provider: &Provider,
    cancel: &AtomicBool,
) -> io::Result<bool> {
    let mut prompts: Vec<_> = messages.iter()
        .filter(|message| message["role"] != "tool" && !message["tool_calls"].is_array())
        .cloned().collect();
    prompts.insert(0,json!({"role":"system","content":"Plan this task without executing it. Return only JSON with objective and steps. Every step has a unique id, title, depends_on array, and acceptance_criteria array. Preserve all requested outcomes. Maximum 64 steps. Do not call tools."}));
    prompts.push(json!({"role":"user","content":concat!(
        "Draft the plan now. Treat the preceding conversation as task context. ",
        "Use this exact JSON shape with task-specific text and IDs: ",
        r#"{"objective":"Task objective","steps":[{"id":"inspect","title":"Inspect the source","depends_on":[],"acceptance_criteria":["Source read"]},{"id":"verify","title":"Verify the result","depends_on":["inspect"],"acceptance_criteria":["Result confirmed"]}]}"#,
        " Return only the JSON object."
    )}));
    let reply = native_chat::complete_with(provider, prompts, cancel, vec![])?;
    let text = reply["message"]["content"]
        .as_str()
        .ok_or_else(|| io::Error::other("Planner returned no plan"))?;
    let text = text
        .trim()
        .strip_prefix("```json")
        .or_else(|| text.trim().strip_prefix("```"))
        .unwrap_or(text.trim())
        .trim()
        .trim_end_matches("```")
        .trim();
    let mut value: Value = serde_json::from_str(text)
        .map_err(|_| io::Error::other("Planner must return a JSON plan"))?;
    validate(&value).map_err(|(_, e)| io::Error::other(e))?;
    value = kageha_workflow::planning::draft(&value).map_err(io::Error::other)?;
    value["turn_id"] = json!(turn);
    {
        let connection = db
            .lock()
            .map_err(|_| io::Error::other("database unavailable"))?;
        connection.execute("INSERT INTO ui_plans(chat_id,turn_id,document,revision,approved) VALUES (?,?,?,1,0) ON CONFLICT(chat_id) DO UPDATE SET turn_id=excluded.turn_id,document=excluded.document,revision=ui_plans.revision+1,approved=0",params![chat,turn,value.to_string()]).map_err(io::Error::other)?;
    }
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "Cancelled"));
        }
        let current = document(db, chat)
            .map_err(|(_, e)| io::Error::other(e))?
            .unwrap();
        webui::emit(
            db,
            chat,
            turn,
            "event",
            json!({"kind":"todo_board","payload":board(&current)}),
        )?;
        let detail = markdown(&current);
        let approved = ui_approvals::require(
            db,
            chat,
            turn,
            "approve_plan",
            &detail,
            &json!({"revision":current["revision"],"plan":current}),
            cancel,
        )?;
        if !approved {
            return Ok(false);
        }
        let connection = db
            .lock()
            .map_err(|_| io::Error::other("database unavailable"))?;
        let updated = connection
            .execute(
                "UPDATE ui_plans SET approved=1 WHERE chat_id=? AND turn_id=? AND revision=?",
                params![chat, turn, current["revision"].as_i64()],
            )
            .map_err(io::Error::other)?;
        if updated == 1 {
            return Ok(true);
        }
    }
}
pub(crate) fn approved_context(db: &Database, chat: &str) -> ApiResult<Option<Value>> {
    Ok(document(db, chat)?.filter(|v| v["approved"] == true))
}
pub(crate) fn update(db: &Database, chat: &str, turn: &str, args: &Value) -> ApiResult<Value> {
    change(db, chat, turn, args, false)
}
pub(crate) fn revise(db: &Database, chat: &str, turn: &str, args: &Value) -> ApiResult<Value> {
    change(db, chat, turn, args, true)
}
fn change(db: &Database, chat: &str, turn: &str, args: &Value, revise: bool) -> ApiResult<Value> {
    let prior = document(db, chat)?
        .filter(|v| v["turn_id"] == turn && v["approved"] == true)
        .ok_or((409, "No approved plan for this turn".into()))?;
    if args
        .get("expected_revision")
        .is_some_and(|v| v.as_u64() != prior["revision"].as_u64())
    {
        return Err((409, "Plan changed; read the current revision".into()));
    }
    let key = kageha_workflow::planning::Key { session: chat.into(), turn: turn.into() };
    let mut args = args.clone();
    {
        let connection = db.lock().map_err(db_error)?;
        let resolve = |value: &mut Value| -> ApiResult<()> {
            let Some(reference) = value.as_str() else { return Ok(()); };
            let id = confirmed_call(&connection, chat, turn, reference).map_err(db_error)?;
            if let Some(id) = id { *value = json!(id); }
            else {
                let recent = recent_evidence(&connection, chat, turn).map_err(db_error)?;
                let hints = recent.into_iter().map(|(id, name)| format!("{id} ({name})")).collect::<Vec<_>>();
                let help = if hints.is_empty() {
                    "Run the tool that verifies the step first.".to_owned()
                } else {
                    format!("Choose the relevant confirmed call from this turn: {}.", hints.join(", "))
                };
                return Err((422, format!("Unrecognized or ambiguous plan evidence. {help}")));
            }
            Ok(())
        };
        if revise {
            if let Some(value) = args.get_mut("evidence_id") { resolve(value)?; }
        } else if args["status"] == "completed" {
            if let Some(values) = args.get_mut("evidence").and_then(Value::as_array_mut) {
            for value in values { resolve(value)?; }
            }
        }
    }
    let evidence = PlanEvidence(db);
    let current = if revise {
        kageha_workflow::planning::revise(&prior, &key, &args, &evidence)
    } else {
        kageha_workflow::planning::progress(&prior, &key, &args, &evidence)
    }.map_err(plan_error)?;
    let connection = db.lock().map_err(db_error)?;
    if revise && connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM ui_turn_checkpoints WHERE turn_id=? AND phase IN ('unverified','blocked_dispatching'))",
        [turn], |row| row.get::<_, bool>(0),
    ).map_err(db_error)? {
        return Err((409, "Resolve the original action receipt before revising the plan.".into()));
    }
    let changed=connection.execute("UPDATE ui_plans SET document=?,revision=revision+1 WHERE chat_id=? AND turn_id=? AND revision=? AND approved=1",params![current.to_string(),chat,turn,prior["revision"].as_i64()]).map_err(db_error)?;
    if changed == 0 {
        return Err((409, "Plan changed; read the current plan".into()));
    }
    drop(connection);
    let saved = document(db, chat)?.unwrap();
    webui::emit(
        db,
        chat,
        turn,
        "event",
        json!({"kind":"todo_board","payload":board(&saved)}),
    )
    .map_err(db_error)?;
    Ok(board(&saved))
}
pub(crate) fn handle(
    mut request: Request,
    db: &Database,
    chat: &str,
    file: bool,
) -> io::Result<()> {
    let method = request.method().clone();
    let result = (|| -> ApiResult<Value> {
        let current = document(db, chat)?;
        if file {
            return Ok(json!({"markdown":current.as_ref().map(markdown)}));
        }
        if matches!(method, Method::Patch | Method::Put) {
            let mut value = current.ok_or((404, "No plan has been created".into()))?;
            let expected_revision = value["revision"].clone();
            if value["approved"] == true {
                return Err((409, "Design is locked after Build".into()));
            }
            let payload = webui::body(&mut request).map_err(|_| (400, "Invalid JSON".into()))?;
            if let Some(plan) = payload.get("plan") {
                validate(plan)?;
                preserve_outcomes(&value, plan)?;
                let migration = value["migration"].clone();
                value = kageha_workflow::planning::draft(plan).map_err(plan_error)?;
                if !migration.is_null() {
                    value["migration"] = migration;
                }
            } else {
                let markdown = payload["files"]["plan.md"]
                    .as_str()
                    .or_else(|| payload["content"].as_str())
                    .or_else(|| payload["text"].as_str())
                    .ok_or((400, "plan.md content is required".into()))?;
                if markdown.len() > 131072 {
                    return Err((400, "Plan is too large".into()));
                }
                let old = value.clone();
                let mut steps = Vec::new();
                for (index, line) in markdown
                    .lines()
                    .filter_map(|line| {
                        line.trim()
                            .strip_prefix("- [ ] ")
                            .or_else(|| line.trim().strip_prefix("- [x] "))
                    })
                    .enumerate()
                {
                    let (id, title) = line
                        .split_once(" — ")
                        .map(|(id, title)| (id.to_owned(), title.to_owned()))
                        .unwrap_or((format!("step-{}", index + 1), line.to_owned()));
                    let mut step = old["steps"]
                        .as_array()
                        .and_then(|steps| steps.iter().find(|v| v["id"] == id))
                        .cloned()
                        .unwrap_or(json!({"id":id,"depends_on":[],"acceptance_criteria":[title]}));
                    step["title"] = json!(title);
                    step["status"] = json!("pending");
                    step["evidence"] = json!([]);
                    steps.push(step);
                }
                value["steps"] = json!(steps);
                validate(&value)?;
            }
            let connection = db.lock().map_err(db_error)?;
            let changed=connection.execute("UPDATE ui_plans SET document=?,revision=revision+1 WHERE chat_id=? AND revision=? AND approved=0",params![value.to_string(),chat,expected_revision.as_i64()]).map_err(db_error)?;
            if changed != 1 {
                return Err((409, "Plan is no longer editable".into()));
            }
            drop(connection);
            return Ok(json!({"ok":true}));
        }
        let value = current.unwrap_or(json!({}));
        Ok(
            json!({"session_id":chat,"agent_mode":if value.is_object()&&!value["steps"].is_null(){"plan"}else{"normal"},"phases":["explore","plan","build"],"files":if value["steps"].is_array(){json!({"plan.md":markdown(&value)})}else{json!({})},"editable":["plan.md"],"approved":value["approved"]==true,"awaiting_build":value["steps"].is_array()&&value["approved"]!=true,"awaiting_clarify":false,"revision":value["revision"],"todo_board":if value["steps"].is_array(){board(&value)}else{Value::Null}}),
        )
    })();
    match result {
        Ok(value) if file => {
            if let Some(text) = value["markdown"].as_str() {
                let _ = request.respond(Response::from_string(text).with_header(
                    Header::from_bytes("Content-Type", "text/markdown; charset=utf-8").unwrap(),
                ));
            } else {
                crate::server::response(request, 404, json!({"error":"No plan"}));
            }
        }
        Ok(value) => crate::server::response(request, 200, value),
        Err((code, error)) => crate::server::response(request, code, json!({"error":error})),
    }
    Ok(())
}

pub(crate) fn stored_board(
    db: &rusqlite::Connection,
    chat: &str,
) -> rusqlite::Result<Option<Value>> {
    db.query_row(
        "SELECT document,revision,turn_id FROM ui_plans WHERE chat_id=?",
        [chat],
        |r| {
            let text: String = r.get(0)?;
            let mut value: Value = serde_json::from_str(&text).unwrap_or(json!({}));
            value["revision"] = r.get::<_, i64>(1)?.into();
            value["turn_id"] = r.get::<_, String>(2)?.into();
            Ok(board(&value))
        },
    )
    .optional()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn imported_completion_is_history_and_required_outcomes_survive_review() {
        let old = json!({"steps":[{"id":"one","title":"Earlier work","status":"completed","depends_on":[]}],"outcomes":[{"id":"required","text":"Deliver the result","required":true,"done":false,"historical_passes":true}],"migration":{"interrupted":true}});
        assert_eq!(board(&old)["complete"], false);
        assert_eq!(board(&old)["verified"], 0);
        assert!(preserve_outcomes(&old, &json!({"steps":[]})).is_err());
        assert!(preserve_outcomes(&old, &old).is_ok());
        assert!(markdown(&old).contains("Deliver the result"));
        let native = json!({"steps":[{"id":"one","title":"Fresh work","status":"completed","depends_on":[]}]});
        assert_eq!(board(&native)["complete"], true);
    }
    #[test]
    fn plans_reject_missing_dependencies_duplicate_ids_and_cycles() {
        assert!(validate(&json!({"steps":[{"id":"a","title":"Read","depends_on":[]}]})).is_ok());
        for steps in [
            json!([]),
            json!([{"id":"a","title":"Read","depends_on":["missing"]}]),
            json!([{"id":"a","title":"Read","depends_on":["b"]},{"id":"b","title":"Write","depends_on":["a"]}]),
            json!([{"id":"a","title":"Read","depends_on":[]},{"id":"a","title":"Reuse","depends_on":[]}]),
        ] {
            assert!(validate(&json!({"steps":steps})).is_err());
        }
    }
    #[test]
    fn fetched_source_completes_step_using_the_original_call() {
        let connection = Connection::open_in_memory().unwrap();
        initialize(&connection).unwrap();
        connection.execute_batch(
            "CREATE TABLE ui_events(chat_id TEXT,turn_id TEXT,event TEXT,payload TEXT);
             CREATE TABLE ui_jobs(id TEXT,parent_chat TEXT,child_chat TEXT,created REAL);"
        ).unwrap();
        let plan = json!({"steps":[{"id":"inspect","title":"Inspect source","depends_on":[]}]});
        connection.execute("INSERT INTO ui_plans VALUES ('chat','turn',?,1,1)", [plan.to_string()]).unwrap();
        for (turn, id, source, status, cached) in [
            ("turn", "fetch", "source-one", "done", false),
            ("turn", "page-two", "source-one", "done", true),
            ("old", "old-call", "source-old", "done", false),
            ("turn", "failed-call", "source-failed", "failed", false),
        ] {
            let preview = json!({"source_id":source,"source_cached":cached});
            let card = json!({"id":id,"name":"web_fetch","status":status,"result_preview":preview.to_string()});
            connection.execute("INSERT INTO ui_events VALUES ('chat',?,'tool_card',?)", params![turn,card.to_string()]).unwrap();
        }
        let db = crate::agent_host::DatabaseState::open(connection).unwrap();
        let complete = |reference: &str| update(&db, "chat", "turn",
            &json!({"step_id":"inspect","status":"completed","evidence":[reference],"expected_revision":1}));
        for invalid in ["source-old", "source-failed", "invented"] {
            let (status, hint) = complete(invalid).unwrap_err();
            assert_eq!(status, 422);
            assert!(hint.contains("ev-1 (web_fetch)"));
        }
        complete("source-one").unwrap();
        let saved = document(&db, "chat").unwrap().unwrap();
        assert_eq!(saved["steps"][0]["evidence"], json!(["fetch"]));
        assert_eq!(saved["revision"], 2);
    }
    #[test]
    fn prose_alone_cannot_complete_a_plan_step() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        initialize(&connection).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE ui_events(chat_id TEXT,turn_id TEXT,event TEXT,payload TEXT);",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO ui_plans VALUES ('chat','turn',?,1,1)",
                [json!({"steps":[{"id":"read","title":"Read","depends_on":[]}]}).to_string()],
            )
            .unwrap();
        let db = crate::agent_host::DatabaseState::open(connection).unwrap();
        assert!(update(
            &db,
            "chat",
            "turn",
            &json!({"step_id":"read","status":"completed","evidence":[]})
        )
        .is_err());
        assert!(update(
            &db,
            "chat",
            "turn",
            &json!({"step_id":"read","status":"completed","evidence":["invented"]})
        )
        .is_err());
        assert!(update(
            &db,
            "chat",
            "other-turn",
            &json!({"step_id":"read","status":"completed","evidence":["invented"]})
        )
        .is_err());
    }
}
