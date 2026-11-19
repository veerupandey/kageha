//! Kageha adapter only: browser decision policy is frozen with each run.
mod chat;
use crate::{
    ui_settings, ui_store,
    webui::{self, Database},
};
use kageha_browser_decisions::{
    Advice, Audit, Guidance, Mode, Models, Observation, Policy, Purpose, Scope, Session,
};
use kageha_contracts::{
    execution::Control,
    policy::{ApprovalRequest, ReviewDecision, ReviewEvidence, ReviewPort},
};
use serde_json::{json, Value};
use std::{
    io,
    sync::{atomic::AtomicBool, Arc},
};
pub(crate) fn decode_policy(value: &Value) -> io::Result<Policy> {
    let mut value = value.clone();
    // UI store revisions are host metadata, not portable browser policy.
    if let Some(object) = value.as_object_mut() {
        object.remove("revision");
    }
    serde_json::from_value(value).map_err(io::Error::other)
}
pub(crate) fn capture(db: &Database) -> Value {
    db.lock()
        .ok()
        .and_then(|db| ui_store::get(&db, "setting", "decisions").ok().flatten())
        .unwrap_or(json!({"mode":"off"}))
}
// Persist only route metadata, never prompts, caller values or model responses.
// Each turn owns its own row; an old worker cannot overwrite the latest run.
fn store_route(db: &Database, key: &str, update: Value) -> io::Result<()> {
    let db = db.lock().map_err(|_| io::Error::other("Decision store unavailable"))?;
    update_route(&db, key, &update)
}

fn update_route(db: &rusqlite::Connection, key: &str, update: &Value) -> io::Result<()> {
    let current = ui_store::get(db, "browser-route", key).map_err(io::Error::other)?;
    let mut value = current.clone().unwrap_or(json!({}));
    for field in [
        "status", "mode", "model", "score_metric", "cutoff", "batch_cutoff", "minimum_score",
        "last_action", "execution", "reason", "elapsed_ms", "decision_id", "verified_actions",
    ] {
        if let Some(item) = update.get(field) {
            value[field] = item.clone();
        }
    }
    if current.as_ref() != Some(&value) {
        ui_store::put(db, "browser-route", key, value).map_err(io::Error::other)?;
    }
    Ok(())
}

pub(crate) fn erase(db: &rusqlite::Connection, chat: &str) -> rusqlite::Result<()> {
    let prefix = format!("{chat}:");
    for table in ["ui_documents", "ui_document_history"] {
        db.execute(&format!("DELETE FROM {table} WHERE kind IN ('browser-route','browser-decision-state','browser-decision-dispatch') AND substr(id,1,length(?1))=?1"), [&prefix])?;
    }
    Ok(())
}
pub(crate) fn current_route(db: &rusqlite::Connection, chat: &str) -> io::Result<Value> {
    use rusqlite::OptionalExtension;
    let request: Option<String> = db.query_row(
        "SELECT request_id FROM native_chat_turns WHERE chat_id=? ORDER BY sequence DESC LIMIT 1",
        [chat], |row| row.get(0)).optional().map_err(io::Error::other)?;
    let Some(request) = request else { return Ok(Value::Null); };
    let turn = webui::turn_id(chat, &request);
    let mut route = ui_store::get(db, "browser-route", &format!("{chat}:{turn}"))
        .map_err(io::Error::other)?.unwrap_or(Value::Null);
    if route.is_object() { route["turn_id"] = turn.into(); }
    Ok(route)
}
struct Events(Database);
impl Audit for Events {
    fn record(&self, scope: &Scope, purpose: Purpose, event: &Value) -> io::Result<()> {
        if purpose == Purpose::Action && event["status"] == "eligible" {
            store_route(&self.0, &format!("{}:{}", scope.workspace, scope.run), json!({
                "status":"eligible","reason":null,"elapsed_ms":null,"minimum_score":null,"decision_id":null
            }))?;
        }
        webui::emit(
            &self.0,
            &scope.workspace,
            &scope.run,
            "event",
            json!({"kind":"browser_decision","payload":{"purpose":purpose,"decision":event}}),
        )
    }
}
pub(crate) struct BrowserDecisions {
    session: Option<Session>,
    intent: String,
    mode: Mode,
    db: Database,
    state_key: String,
}
impl BrowserDecisions {
    pub fn new(db: &Database, chat: &str, turn: &str, policy: &Value) -> Self {
        match Self::build(db, chat, turn, policy) {
            Ok(value) => value,
            Err(_) => {
                let mode = match policy["overrides"]["browser_review"]["mode"].as_str() {
                    Some("active") => Mode::Active,
                    Some("shadow") => Mode::Shadow,
                    _ => Mode::Off,
                };
                let _ = webui::emit(
                    db,
                    chat,
                    turn,
                    "event",
                    json!({"kind":"browser_decision","payload":{"status":"invalid_configuration","review_requires_approval":mode==Mode::Active}}),
                );
                let _ = store_route(db, &format!("{chat}:{turn}"), json!({"status":"invalid_configuration"}));
                Self {
                    session: None,
                    intent: String::new(),
                    mode,
                    db: db.clone(),
                    state_key: format!("{chat}:{turn}"),
                }
            }
        }
    }
    fn build(db: &Database, chat: &str, turn: &str, policy: &Value) -> io::Result<Self> {
        let policy: Policy = if policy.is_null() {
            Policy::default()
        } else {
            decode_policy(policy)?
        };
        policy.validate()?;
        let mut models = Models::new();
        let mut backends = kageha_browser_decisions::typed::Backends::new();
        for purpose in [
            Purpose::Guidance,
            Purpose::Recovery,
            Purpose::Review,
            Purpose::Action,
        ] {
            let effective = policy.effective(purpose);
            if effective.mode == Mode::Off {
                continue;
            }
            for alias in std::iter::once(&effective.model).chain(policy.fallback_models.iter()) {
                if alias.is_empty() || models.contains_key(alias) || backends.contains_key(alias) {
                    continue;
                }
                if let Ok(Some(backend)) = typed_backend(db, alias) {
                    backends.insert(alias.clone(), backend);
                    continue;
                }
                if purpose == Purpose::Action {
                    continue;
                }
                if let Ok(provider) = ui_settings::configured(db, alias, "default") {
                    if let Ok(model) = crate::ui_credentials::model(vec![provider]) {
                        models.insert(alias.clone(), Arc::new(model));
                    }
                }
            }
        }
        use kageha_runtime::lifecycle::RunStore;
        let intent = crate::runtime_host::store(db)
            .ok()
            .and_then(|store| {
                store.find_turn(turn).ok().flatten().and_then(|run| {
                    kageha_runtime::review_context::user_intent(store.as_ref(), &run).ok()
                })
            })
            .unwrap_or_default();
        let session = Session::new(
            Scope {
                workspace: chat.into(),
                run: turn.into(),
                client: "kageha-host".into(),
            },
            policy,
            models,
        )?
        .with_decision_backends(backends)
        .with_audit(Arc::new(Events(db.clone())));
        let state_key = format!("{chat}:{turn}");
        if let Some(checkpoint) = ui_store::get(
            &*db.lock()
                .map_err(|_| io::Error::other("Decision store unavailable"))?,
            "browser-decision-state",
            &state_key,
        )
        .map_err(io::Error::other)?
        {
            session.restore(&checkpoint["checkpoint"])?;
        }
        let mode = session.mode(Purpose::Review);
        let settings = session.action_settings();
        // Restored runs keep their last route; policy is frozen in the run spec.
        let has_route = ui_store::get(&*db.lock().map_err(|_| io::Error::other("Decision store unavailable"))?, "browser-route", &state_key)
            .map_err(io::Error::other)?.is_some();
        if !has_route {
            store_route(db, &state_key, json!({"status":match settings.mode {Mode::Off => "off", Mode::Shadow => "compare_ready", Mode::Active => "ready"},
                "mode":settings.mode,
                "model":settings.model,"score_metric":if session.action_confidence_cutoff().is_some() {"provider_confidence"} else {"selected_probability"},
                "cutoff":session.action_confidence_cutoff().unwrap_or(settings.threshold)}))?;
        }
        Ok(Self {
            session: Some(session),
            intent,
            mode,
            db: db.clone(),
            state_key,
        })
    }
    fn record_dispatch(&self, decision_id: &str, model: Option<&str>,
        calls: &[kageha_contracts::execution::ToolCall]) -> io::Result<()> {
        let (chat, turn) = self.state_key.split_once(':')
            .ok_or_else(|| io::Error::other("Browser decision scope unavailable"))?;
        let mut connection = self.db.lock().map_err(|_| io::Error::other("Decision store unavailable"))?;
        let tx = connection.transaction().map_err(io::Error::other)?;
        let key = format!("{}:{decision_id}", self.state_key);
        let calls = calls.iter().map(|call| (call.id.as_str(), json!({"action":call.name})))
            .collect::<std::collections::BTreeMap<_, _>>();
        ui_store::put(&tx, "browser-decision-dispatch", &key, json!({"calls":calls, "model":model}))
            .map_err(io::Error::other)?;
        webui::insert_event(&tx, chat, turn, "event", json!({"kind":"browser_decision",
            "payload":{"purpose":Purpose::Action,"decision":{"status":"sent_to_executor",
                "decision_id":decision_id,"model":model,"main_model_skipped":true,"calls":calls}}}))
            .map_err(io::Error::other)?;
        update_route(&tx, &self.state_key, &json!({"status":"sent_to_executor","decision_id":decision_id}))?;
        tx.commit().map_err(io::Error::other)?;
        drop(connection);
        crate::ui_session_events::notify();
        Ok(())
    }

    pub(crate) fn record_execution(&self, id: &str, name: &str, arguments: &Value, result: Result<&str, &io::Error>) {
        let Some((decision_id, _)) = id.strip_prefix("browser-decision-").and_then(|id| id.rsplit_once('-')) else {
            return;
        };
        let (status, reason) = match result {
            Ok(content) => {
                let value: Value = serde_json::from_str(content).unwrap_or(Value::Null);
                if value["receipt"]["state"] == "verified" || (name == "browser_tabs"
                    && value["target_id"].is_string() && value["target_id"] == arguments["tab_handle"]) {
                    ("verified", None)
                } else {
                    ("unconfirmed", Some("receipt_unverified"))
                }
            }
            Err(error) if kageha_contracts::execution::is_uncertain(error) => {
                ("unconfirmed", Some("receipt_unverified"))
            }
            Err(error) => {
                let detail = serde_json::from_str::<Value>(&error.to_string()).unwrap_or(Value::Null);
                if detail["execution_phase"] == "uncertain" {
                    ("unconfirmed", Some("receipt_unverified"))
                } else if detail["execution_phase"] == "not_dispatched"
                    || error.kind() == io::ErrorKind::PermissionDenied
                    || kageha_contracts::policy::is_stop(error) {
                    ("blocked", Some("executor_rejected"))
                } else {
                    ("failed", Some("executor_error"))
                }
            }
        };
        let _ = self.record_execution_status(decision_id, id, name, status, reason);
    }

    fn record_execution_status(&self, decision_id: &str, id: &str, name: &str,
        status: &str, reason: Option<&str>) -> io::Result<()> {
        let (chat, turn) = self.state_key.split_once(':')
            .ok_or_else(|| io::Error::other("Browser decision scope unavailable"))?;
        let key = format!("{}:{decision_id}", self.state_key);
        let mut connection = self.db.lock().map_err(|_| io::Error::other("Decision store unavailable"))?;
        let tx = connection.transaction().map_err(io::Error::other)?;
        let Some(mut dispatch) = ui_store::get(&tx, "browser-decision-dispatch", &key)
            .map_err(io::Error::other)? else { return Ok(()); };
        if dispatch["calls"][id]["action"] != name || dispatch["calls"][id]["status"].is_string() {
            return Ok(());
        }
        dispatch["calls"][id]["status"] = json!(status);
        ui_store::put(&tx, "browser-decision-dispatch", &key, dispatch.clone()).map_err(io::Error::other)?;
        webui::insert_event(&tx, chat, turn, "event", json!({"kind":"browser_decision",
            "payload":{"purpose":Purpose::Action,"decision":{"status":status,"reason":reason,
              "decision_id":decision_id,"call_id":id,"action":name,"model":dispatch["model"]}}}))
            .map_err(io::Error::other)?;
        let current = ui_store::get(&tx, "browser-route", &self.state_key).map_err(io::Error::other)?
            .unwrap_or(Value::Null);
        let count = current["verified_actions"].as_u64().unwrap_or(0) + u64::from(status == "verified");
        let mut route = json!({"last_action":name,"execution":status,"verified_actions":count});
        if current["decision_id"] == decision_id {
            let calls = dispatch["calls"].as_object().expect("recorded dispatch has calls");
            let has = |status: &str| calls.values().any(|call| call["status"] == status);
            let batch_status = if has("unconfirmed") { "unconfirmed" }
                else if has("failed") { "failed" }
                else if has("blocked") { "blocked" }
                else if calls.values().any(|call| call["status"].is_null()) { "sent_to_executor" }
                else { "verified" };
            route["status"] = json!(batch_status);
            route["reason"] = json!(match batch_status {
                "unconfirmed" => Some("receipt_unverified"),
                "failed" => Some("executor_error"),
                "blocked" => Some("executor_rejected"),
                _ => None,
            });
        }
        update_route(&tx, &self.state_key, &route)?;
        tx.commit().map_err(io::Error::other)?;
        drop(connection);
        crate::ui_session_events::notify();
        Ok(())
    }
    pub fn mode(&self) -> Mode {
        self.mode
    }
    fn persist(&self) -> io::Result<()> {
        if let Some(session) = &self.session {
            ui_store::put(
                &*self
                    .db
                    .lock()
                    .map_err(|_| io::Error::other("Decision store unavailable"))?,
                "browser-decision-state",
                &self.state_key,
                json!({"checkpoint":session.checkpoint()?}),
            )
            .map_err(io::Error::other)?;
        }
        Ok(())
    }
    pub fn annotate(&self, snapshot: &mut Value, selection: &Value, cancel: &AtomicBool) {
        self.annotate_for(snapshot, selection, cancel, Purpose::Guidance)
    }
    pub fn annotate_for(
        &self,
        snapshot: &mut Value,
        selection: &Value,
        cancel: &AtomicBool,
        purpose: Purpose,
    ) {
        let Some(session) = &self.session else {
            return;
        };
        if snapshot["browser_access"].is_object() || session.mode(purpose) == Mode::Off || session.mode(Purpose::Action) == Mode::Active {
            return;
        }
        let main = snapshot["main_frame_id"].as_str().unwrap_or("");
        let request = Guidance {
            version: 1,
            purpose,
            observation: Observation {
                resource: snapshot["target_id"].as_str().unwrap_or("").into(),
                binding: selection["binding_id"].as_str().unwrap_or("").into(),
                document: snapshot["frame_scopes"][main]["document_id"]
                    .as_str()
                    .unwrap_or("")
                    .into(),
                service_epoch: snapshot["service_epoch"]
                    .as_str()
                    .unwrap_or("native-browser")
                    .into(),
                evidence: kageha_browser_decisions::page_evidence(snapshot),
            },
            allowed: [
                Advice::UseObservedControls,
                Advice::ReadContent,
                Advice::Wait,
                Advice::InspectBlocker,
                Advice::Reconcile,
            ]
            .into_iter()
            .collect(),
        };
        if let Ok(outcome) = session.guide(
            &self.intent,
            &request,
            &Control {
                cancelled: cancel,
                deadline: None,
            },
        ) {
            if self.persist().is_err() {
                return;
            }
            if let Some(advice) = outcome.applied {
                snapshot["browser_guidance"] = json!({"purpose":purpose,"instruction":advice.instruction(),"confidence":outcome.confidence,"cached":outcome.cached});
            }
        }
    }
    pub fn reviewer<'a>(&'a self, evidence: &'a [ReviewEvidence]) -> BrowserReviewer<'a> {
        BrowserReviewer {
            owner: self,
            evidence,
        }
    }
}
pub(crate) fn reviewer_ready(db: &Database) -> bool {
    let Ok(policy) = decode_policy(&capture(db)) else {
        return false;
    };
    let e = policy.effective(Purpose::Review);
    e.mode == Mode::Active
        && !e.model.is_empty()
        && (typed_backend(db, &e.model).ok().flatten().is_some()
            || ui_settings::configured(db, &e.model, "default").is_ok())
}

pub(crate) struct BrowserReviewer<'a> {
    owner: &'a BrowserDecisions,
    evidence: &'a [ReviewEvidence],
}
impl ReviewPort for BrowserReviewer<'_> {
    fn review(
        &self,
        request: &ApprovalRequest,
        control: &Control<'_>,
    ) -> io::Result<ReviewDecision> {
        let result = self
            .owner
            .session
            .as_ref()
            .ok_or_else(|| {
                io::Error::other("Browser decision model unavailable; approval required")
            })?
            .review(request, &self.owner.intent, self.evidence, control);
        self.owner.persist()?;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn luna_decisions_are_separate_from_chat_and_use_the_same_configured_origin() {
        let db=rusqlite::Connection::open_in_memory().unwrap();ui_store::initialize(&db).unwrap();
        let original=json!({"id":"openai/chat","provider":"openai","model":"gpt-6-luna","protocol":"openai_compat","base_url":"https://proxy.example/v1/responses","api_key_env":"PRIVATE_OPENAI_KEY"});
        ui_store::put(&db,"model","openai/chat",original.clone()).unwrap();
        register_typed(&db).unwrap();
        let row=ui_store::get(&db,"model","openai-decisions/gpt-6-luna").unwrap().unwrap();
        assert_eq!(row["protocol"],"openai_decisions");assert_eq!(row["api_key_env"],"PRIVATE_OPENAI_KEY");
        assert_eq!(kageha_models::catalog::completion_url(row["base_url"].as_str().unwrap(),"openai_decisions").unwrap(),"https://proxy.example/v1/decisions");
        assert!(!kageha_models::catalog::supported("openai_decisions"));
        assert_eq!(ui_store::get(&db,"model","openai/chat").unwrap().unwrap()["model"],original["model"]);
    }
    #[test]
    fn panel_route_is_private_to_the_latest_chat_turn_and_contains_no_facts() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        ui_store::initialize(&connection).unwrap();
        connection.execute_batch("CREATE TABLE native_chat_turns(sequence INTEGER PRIMARY KEY,chat_id TEXT,request_id TEXT);
            INSERT INTO native_chat_turns VALUES(1,'a','first'),(2,'b','other'),(3,'a','second');").unwrap();
        let db = crate::agent_host::DatabaseState::open(connection).unwrap();
        let first = format!("a:{}",webui::turn_id("a","first"));
        let second = format!("a:{}",webui::turn_id("a","second"));
        store_route(&db,&first,json!({"status":"routed","model":"old"})).unwrap();
        assert!(current_route(&db.lock().unwrap(),"a").unwrap().is_null());
        store_route(&db,&second,json!({"status":"fallback","model":"clef","cutoff":0.9,"values":{"secret":"never"}})).unwrap();
        store_route(&db,&first,json!({"status":"routed","model":"late-old-worker"})).unwrap();
        let current = current_route(&db.lock().unwrap(),"a").unwrap();
        assert_eq!(current["model"],"clef");
        assert_eq!(current["status"],"fallback");
        assert!(current.get("values").is_none());
        assert!(current_route(&db.lock().unwrap(),"b").unwrap().is_null());
        let other = format!("b:{}",webui::turn_id("b","other"));
        store_route(&db,&other,json!({"status":"routed"})).unwrap();
        erase(&db.lock().unwrap(),"a").unwrap();
        assert!(current_route(&db.lock().unwrap(),"a").unwrap().is_null());
        assert_eq!(current_route(&db.lock().unwrap(),"b").unwrap()["status"],"routed");
    }
    #[test]
    fn host_revision_is_not_a_browser_policy_field() {
        let policy = decode_policy(&json!({"mode":"active","revision":4})).unwrap();
        assert_eq!(policy.mode, Mode::Active);
        assert!(decode_policy(&json!({"mode":"active","unexpected":true})).is_err());
    }

    #[test]
    fn decision_receipts_track_only_recorded_calls_and_survive_fallback() {
        use kageha_contracts::execution::ToolCall;

        let connection = rusqlite::Connection::open_in_memory().unwrap();
        ui_store::initialize(&connection).unwrap();
        connection.execute_batch(
            "CREATE TABLE ui_events(sequence INTEGER PRIMARY KEY,chat_id TEXT,turn_id TEXT,event TEXT,payload TEXT);
             CREATE TABLE ui_jobs(id TEXT,parent_chat TEXT,child_chat TEXT,created REAL);
             CREATE TABLE native_chat_turns(sequence INTEGER PRIMARY KEY,chat_id TEXT,request_id TEXT);
             INSERT INTO native_chat_turns VALUES(1,'chat','request');",
        ).unwrap();
        let db = crate::agent_host::DatabaseState::open(connection).unwrap();
        let state_key = format!("chat:{}", webui::turn_id("chat", "request"));
        let decisions = BrowserDecisions {
            session: None,
            intent: String::new(),
            mode: Mode::Active,
            db: db.clone(),
            state_key,
        };
        let fill = ToolCall {
            id: "browser-decision-example-0".into(),
            name: "browser_fill".into(),
            arguments: json!({"text":"fixture value"}),
        };
        let click = ToolCall {
            id: "browser-decision-example-1".into(),
            name: "browser_click".into(),
            arguments: json!({}),
        };
        decisions.record_dispatch("example", Some("fixture"), &[fill.clone(), click.clone()]).unwrap();

        let verified = r#"{"receipt":{"state":"verified"}}"#;
        decisions.record_execution("browser-decision-example-2", &fill.name, &fill.arguments, Ok(verified));
        decisions.record_execution(&fill.id, &fill.name, &fill.arguments, Ok(verified));
        decisions.record_execution(&fill.id, &fill.name, &fill.arguments, Ok(verified));
        let error = io::Error::other(r#"{"execution_phase":"not_dispatched"}"#);
        decisions.record_execution(&click.id, &click.name, &click.arguments, Err(&error));

        let route = current_route(&db.lock().unwrap(), "chat").unwrap();
        assert_eq!(route["status"], "blocked");
        assert_eq!(route["reason"], "executor_rejected");
        assert_eq!(route["verified_actions"], 1);
        store_route(&db, &decisions.state_key, json!({"status":"fallback","decision_id":null})).unwrap();
        assert_eq!(current_route(&db.lock().unwrap(), "chat").unwrap()["verified_actions"], 1);

        let connection = db.lock().unwrap();
        let mut statement = connection.prepare("SELECT payload FROM ui_events ORDER BY sequence").unwrap();
        let events: Vec<Value> = statement.query_map([], |row| row.get::<_, String>(0)).unwrap()
            .map(|row| serde_json::from_str(&row.unwrap()).unwrap()).collect();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0]["payload"]["decision"]["main_model_skipped"], true);
        assert_eq!(events[1]["payload"]["decision"]["call_id"], fill.id);
        assert_eq!(events[2]["payload"]["decision"]["call_id"], click.id);
        assert!(!Value::Array(events).to_string().contains("fixture value"));
    }
}

/// Route a standalone navigation command through the usual browser tool, including
/// profile consent and policy checks. Compound tasks and ambiguous destinations
/// stay with the model. Only user turns can supply the destination.
fn explicit_navigation(messages: &[Value]) -> Option<Value> {
    let last = messages.iter().rposition(|m| m["role"] == "user")?;
    if messages[last + 1..].iter().any(|m| m["role"] != "system") {
        return None; // A call or reply already happened after this request.
    }
    let mut command = messages[last]["content"].as_str()?.trim();
    if command.trim_end_matches('.').eq_ignore_ascii_case("use browser tools") {
        let previous = messages[..last].iter().rev().find(|m| m["role"] == "user")?;
        command = previous["content"].as_str()?.trim();
    }
    let words: Vec<_> = command.split_whitespace().collect();
    let words = if words.first()?.eq_ignore_ascii_case("please") { &words[1..] } else { &words };
    if !words.first()?.eq_ignore_ascii_case("open") { return None; }
    let destination = *words.get(1)?;
    let suffix = words[2..].join(" ").to_ascii_lowercase();
    let profile = match suffix.trim_end_matches('.') {
        "" => "selected",
        "in comet" | "in my comet browser" | "in comet browser" => "comet",
        "in chrome" | "in my chrome browser" | "in chrome browser" => "chrome",
        "in my browser" | "in my signed-in browser" => "personal",
        "in the private browser" => "private",
        _ => return None,
    };
    let url = if destination.eq_ignore_ascii_case("linkedin") {
        url::Url::parse("https://www.linkedin.com/").ok()?
    } else if destination.starts_with("https://") || destination.starts_with("http://") {
        url::Url::parse(destination).ok()?
    } else {
        // Bare hostnames only. Leave filenames, unknown site names and local paths alone.
        if !destination.contains('.') || destination.contains(['/', '@', ':', '?', '#'])
            || !destination.to_ascii_lowercase().ends_with(".com") { return None; }
        url::Url::parse(&format!("https://{destination}")).ok()?
    };
    if url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() { return None; }
    Some(json!({"url":url.as_str(),"profile":profile}))
}

#[cfg(test)]
#[test]
fn explicit_navigation_uses_only_a_current_user_command() {
    let user = |s| json!({"role":"user","content":s});
    let previous = user("Open LinkedIn in my comet browser");
    let refusal = json!({"role":"assistant","content":"I cannot do that."});
    let follow = user("Use browser tools");
    let expected = json!({"url":"https://www.linkedin.com/","profile":"comet"});
    assert_eq!(explicit_navigation(&[previous.clone()]), Some(expected.clone()));
    assert_eq!(explicit_navigation(&[previous.clone(), refusal.clone(), follow.clone()]), Some(expected));
    assert!(explicit_navigation(&[previous, json!({"role":"tool","content":"declined"})]).is_none());
    assert!(explicit_navigation(&[refusal, follow]).is_none());
    assert!(explicit_navigation(&[user("Open LinkedIn and send a message")]).is_none());
    assert_eq!(explicit_navigation(&[user("Open https://example.org/Path in Chrome")]),
        Some(json!({"url":"https://example.org/Path","profile":"chrome"})));
}

impl crate::ui_tools::Tools<'_> {
    pub(crate) fn decision_first(
        &self,
        request: &kageha_contracts::execution::ModelRequest,
        control: &Control<'_>,
    ) -> io::Result<Option<kageha_contracts::execution::AssistantMessage>> {
        if self.depth == 0 && self.workflow.is_null() && self.resume_receipts.is_empty()
            && request.tools.iter().any(|tool| tool["function"]["name"] == "browser_open")
        {
            if let Some(arguments) = explicit_navigation(&request.messages) {
                return Ok(Some(kageha_contracts::execution::AssistantMessage {
                    continuation: None,
                    content: Value::Null,
                    tool_calls: vec![kageha_contracts::execution::ToolCall {
                        id: format!("browser-open-{}", self.turn), name: "browser_open".into(), arguments,
                    }],
                }));
            }
        }
        let Some(session) = self.browser_decisions.session.as_ref() else {
            return Ok(None);
        };
        if session.mode(Purpose::Action) == Mode::Off {
            return Ok(None);
        }
        let workflow = self.workflow_config()?;
        if workflow.is_none() && (self.depth != 0 || !recent_browser_activity(&request.messages) || !request.tools.iter().any(|t| t["function"]["name"] == "browser_snapshot")) {
            return Ok(None);
        }
        let mut client = match crate::browser_client::Client::connect(self.chat, "agent") {
                Ok(client) => client,
                Err(_) => return Ok(None),
            };
        if crate::web_browser::control(self.db, self.chat, &client.epoch)?
                .is_some_and(|(owner, _)| owner == "user") {
                let _ = store_route(self.db, &self.browser_decisions.state_key, json!({"status":"user_control","minimum_score":null}));
                return Ok(None);
            }
        // A controller publishes this context after a fresh read. Never parse an assistant claim.
        let context = request
            .messages
            .iter()
            .rev()
            .filter(|m| m["role"] == "system")
            .find_map(|m| {
                m["content"]
                    .as_str()
                    .and_then(|text| {
                        text.find("\n{")
                            .and_then(|i| serde_json::from_str::<Value>(&text[i + 1..]).ok())
                    })
                    .filter(|v| v.get("fresh_browser_observation").is_some())
            });
        let mut chat_context = None;
        let (snapshot, mut values, instructions, pending) = if let Some(config) = workflow {
            let Some(context) = context else {
                return Ok(None);
            };
            let Some(id) = context["fresh_browser_observation"]["observation_id"].as_str() else {
                return Ok(None);
            };
            let snapshot = self
                .observations
                .lock()
                .map_err(|_| io::Error::other("Browser observation cache unavailable"))?
                .get(id)
                .cloned();
            let Some(snapshot) = snapshot else {
                return Ok(None);
            };
            (
                snapshot,
                config.known_values,
                config.known_context,
                context["pending_original_actions"]
                    .as_array()
                    .is_none_or(|v| !v.is_empty()),
            )
        } else {
            let Some((snapshot, facts, intent, selection)) = chat::context(self, &mut client, request) else {
                return Ok(None);
            };
            chat_context = Some(selection);
            (snapshot, facts, intent, false)
        };
        let Some(id) = snapshot["observation_id"].as_str() else {
            return Ok(None);
        };
        let mut controls = snapshot["controls"].as_array().cloned().unwrap_or_default();
        let mut trusted_click_targets: std::collections::BTreeSet<String> = chat_context.as_ref().map(|selection| chat::candidates(&instructions,&snapshot,selection,&mut controls,&mut values)).unwrap_or_default();
        if chat_context.is_some() && request.messages.iter().rev().take_while(|m| m["role"] != "user")
            .filter_map(|m| m["tool_calls"].as_array()).flatten()
            .any(|call| call["id"].as_str().is_some_and(|id| id.starts_with("browser-decision-"))
                && matches!(call["name"].as_str().or(call["function"]["name"].as_str()),Some("browser_click" | "browser_tabs"))) {
            // A singular navigation request cannot authorize clicking Next on every subsequent page.
            trusted_click_targets.clear();
        }
        if controls.len() > 256 {
            return Ok(None);
        }
        let main = snapshot["main_frame_id"].as_str().unwrap_or("");
        let allowed_actions = [
            ("click", "browser_click"),
            ("switch_tab", "browser_tabs"),
            ("fill", "browser_fill"),
            ("check", "browser_check"),
            ("select", "browser_select"),
            ("press", "browser_press"),
        ]
        .into_iter()
        .filter(|(_, name)| request.tools.iter().any(|t| t["function"]["name"] == *name))
        .map(|(kind, _)| kind.to_owned())
        .collect();
        let action_request = kageha_browser_decisions::routing::Request {
            observation_id: id.into(),
            observation: Observation {
                resource: snapshot["target_id"].as_str().unwrap_or("").into(),
                binding: snapshot["binding_id"].as_str().unwrap_or("").into(),
                document: snapshot["frame_scopes"][main]["document_id"]
                    .as_str()
                    .unwrap_or("")
                    .into(),
                service_epoch: snapshot["service_epoch"].as_str().unwrap_or("").into(),
                evidence: kageha_browser_decisions::page_evidence(&snapshot),
            },
            controls,
            values,
            allowed_actions,
            trusted_click_targets,
            pending_actions: pending,
        };
        let objective = if chat_context.is_some() { instructions } else { format!(
            "{}\nTrusted caller workflow instructions: {}",
            self.browser_decisions.intent, instructions
        ) };
        let outcome = session.route(&objective, &action_request, control);
        control.check()?;
        let Ok(outcome) = outcome else {
            let _ = store_route(self.db, &self.browser_decisions.state_key, json!({"status":"decision_unavailable","minimum_score":null}));
            return Ok(None);
        };
        let _ = store_route(self.db, &self.browser_decisions.state_key, json!({
            "status":outcome.status,"model":outcome.model.clone().unwrap_or_else(||session.action_settings().model),"score_metric":outcome.score_metric,
            "cutoff":outcome.cutoff,"batch_cutoff":outcome.batch_cutoff,"minimum_score":outcome.minimum_score,
            "reason":outcome.attempts.last().filter(|attempt| attempt.reason != "accepted").map(|attempt| &attempt.reason),
            "elapsed_ms":outcome.elapsed_ms,"decision_id":null
        }));
        if self.browser_decisions.persist().is_err() {
            let _ = store_route(self.db, &self.browser_decisions.state_key, json!({"status":"decision_unavailable","reason":"audit_unavailable"}));
            return Ok(None);
        }
        if outcome.actions.is_empty() {
            return Ok(None);
        }
        if let Some(selection) = &chat_context {
            if chat::operator_intent(self).as_ref() != Some(&objective)
                || !client.selection().ok().is_some_and(|current| crate::browser_client::target(&current) == crate::browser_client::target(selection))
                || crate::web_browser::control(self.db,self.chat,&client.epoch)?.is_some_and(|(owner,_)| owner == "user") {
                store_route(self.db,&self.browser_decisions.state_key,json!({"status":"context_changed","minimum_score":null}))?;
                return Ok(None);
            }
        }
        let mut calls = Vec::new();
        let decision_id = uuid::Uuid::new_v4().simple().to_string();
        for (index, action) in outcome.actions.into_iter().enumerate() {
            use kageha_browser_decisions::routing::Action;
            let mut arguments = json!({"target":action.target(),"observation_id":id});
            let name = match action {
                Action::SwitchTab { target } => {
                    let Some(handle) = target.strip_prefix("tab:") else { return Ok(None); };
                    let Some(tab) = chat_context.as_ref().and_then(|selection| selection["tabs"].as_array())
                        .and_then(|tabs| tabs.iter().find(|tab| tab["target_id"] == handle)) else { return Ok(None); };
                    arguments = json!({"tab_handle":handle,"observation_id":id,"expected_tab_url":tab["url"],"expected_tab_title":tab["title"]});
                    "browser_tabs"
                }
                Action::Click { ref target } => {
                    if let Some(url) = action_request.controls.iter().find(|c| c["ref"] == target.as_str()).and_then(|c| c["href"].as_str()) {
                        arguments["expected_url"] = json!(url);
                    }
                    "browser_click"
                },
                Action::Fill { value_id, .. } => {
                    arguments["text"] = action_request.values[&value_id].clone();
                    "browser_fill"
                }
                Action::Check { value_id, .. } => {
                    arguments["checked"] = action_request.values[&value_id].clone();
                    "browser_check"
                }
                Action::Select { value_id, .. } => {
                    arguments["option"] = action_request.values[&value_id].clone();
                    "browser_select"
                }
                Action::Press { key, .. } => {
                    arguments["key"] = json!(key);
                    "browser_press"
                }
            };
            calls.push(kageha_contracts::execution::ToolCall {
                id: format!("browser-decision-{decision_id}-{index}"),
                name: name.into(),
                arguments,
            });
        }
        if self.browser_decisions.record_dispatch(&decision_id, outcome.model.as_deref(), &calls).is_err() {
            let _ = store_route(self.db, &self.browser_decisions.state_key, json!({"status":"decision_unavailable","reason":"audit_unavailable"}));
            return Ok(None);
        }
        Ok(Some(kageha_contracts::execution::AssistantMessage {
            continuation: None,
            content: Value::Null,
            tool_calls: calls,
        }))
    }
}

fn recent_browser_activity(messages: &[Value]) -> bool {
    for message in messages.iter().rev() {
        if message["role"] == "user" {
            return false;
        }
        if message["role"] == "assistant" {
            let Some(calls) = message["tool_calls"].as_array().filter(|v| !v.is_empty()) else {
                return false;
            };
            return calls.iter().all(|call| {
                call["name"]
                    .as_str()
                    .or_else(|| call["function"]["name"].as_str())
                    .is_some_and(|name| name.starts_with("browser_"))
            });
        }
    }
    false
}
#[cfg(test)]
mod activity_tests {
    use super::*;
    #[test]
    fn routing_does_not_leak_into_unrelated_or_steered_chat() {
        let user = json!({"role":"user","content":"Open the browser"});
        let browser = json!({"role":"assistant","tool_calls":[{"name":"browser_snapshot"}]});
        assert!(recent_browser_activity(&[
            user.clone(),
            browser.clone(),
            json!({"role":"tool","content":"observed"})
        ]));
        assert!(!recent_browser_activity(&[browser.clone(), user]));
        assert!(!recent_browser_activity(&[
            browser,
            json!({"role":"assistant","tool_calls":[{"name":"read_file"}]})
        ]));
    }
}

mod catalog;
pub(crate) use catalog::{inventory as decision_inventory, register_typed};
/// Exercise the configured credential resolver and typed adapter using no user data.
pub(crate) fn probe(db: &Database, id: &str, metric: &str) -> Value {
    use kageha_browser_decisions::{decision_error_reason, typed};
    use std::time::{Duration, Instant};
    let started = Instant::now();
    let outcome = (|| {
        let backend = typed_backend(db, id).map_err(|error| decision_error_reason(&error))?
            .ok_or("provider_unavailable")?;
        let request = typed::Request {
            state: json!({"color":"blue"}),
            questions: std::collections::BTreeMap::from([("observed_color".into(), typed::Question {
                instructions: "Select the color named in the supplied state.".into(),
                criteria: std::collections::BTreeMap::from([
                    ("blue".into(), "The named color is blue.".into()),
                    ("red".into(), "The named color is red.".into()),
                ]),
            })]),
        };
        let control = Control {
            cancelled: &AtomicBool::new(false),
            deadline: Some(started + Duration::from_secs(5)),
        };
        let answer = backend.decide(&request, &control).map_err(|error| decision_error_reason(&error))?;
        answer.validate(&request).map_err(|_| "invalid_response")?;
        let answer = &answer.answers["observed_color"];
        if answer.choice != "blue" { return Err("incorrect_probe_answer"); }
        if metric == "provider_confidence" && answer.confidence.is_none() { return Err("missing_confidence"); }
        Ok(())
    })();
    json!({"ok":outcome.is_ok(),"model_id":id,"score_metric":metric,
        "reason":outcome.err(),"elapsed_ms":started.elapsed().as_millis()})
}

pub(crate) fn typed_backend(
    db: &Database,
    id: &str,
) -> io::Result<Option<Arc<dyn kageha_browser_decisions::typed::DecisionPort + Send + Sync>>> {
    let row = {
        let c = db
            .lock()
            .map_err(|_| io::Error::other("Decision catalog unavailable"))?;
        register_typed(&c)?;
        ui_store::get(&c, "model", id).map_err(io::Error::other)?
    };
    let Some(row) = row else { return Ok(None) };
    let protocol = row["protocol"].as_str().unwrap_or("");
    if !kageha_models::decisions::is_typed_protocol(protocol) {
        return Ok(None);
    }
    let url =
        kageha_models::catalog::completion_url(row["base_url"].as_str().unwrap_or(""), protocol)?;
    let model = row["model"]
        .as_str()
        .ok_or_else(|| io::Error::other("Decision model missing"))?;
    let provider = crate::ui_settings::Provider {
        key: String::new(),
        model: model.into(),
        protocol: protocol.into(),
        url: url.clone(),
        source: Some(crate::ui_credentials::ModelSource {
            db: db.clone(),
            id: id.into(),
            provider: row["provider"].as_str().unwrap_or("").into(),
            model: model.into(),
            protocol: protocol.into(),
            url: url.clone(),
            destination: crate::ui_credentials::model_origin(&url)
                .map_err(|(_, e)| io::Error::other(e))?,
        }),
        context_window: None,
    };
    Ok(Some(Arc::new(
        kageha_models::decisions::HttpDecisions::new(crate::ui_credentials::model(vec![
            provider,
        ])?)?,
    )))
}
