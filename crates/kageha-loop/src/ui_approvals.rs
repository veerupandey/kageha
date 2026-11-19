//! Compatibility adapter for component-owned policy persistence and the existing WebUI.
use crate::{
    server,
    webui::{self, Database},
};
use kageha_contracts::{execution::Control, policy::*};
use kageha_policy::journal::Journal;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::{atomic::AtomicBool, Arc, Mutex, OnceLock},
};
use tiny_http::{Method, Request};
fn sql(e: impl std::fmt::Display) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(io::Error::other(e.to_string())))
}
static JOURNALS: OnceLock<Mutex<HashMap<PathBuf, Arc<Journal>>>> = OnceLock::new();
pub(crate) fn owned(db: &Connection) -> io::Result<Arc<Journal>> {
    let identity: String = db
        .query_row(
            "SELECT source FROM ui_policy_identity WHERE id=1",
            [],
            |r| r.get(0),
        )
        .map_err(io::Error::other)?;
    let path = if let Some(path) = db.path().filter(|p| !p.is_empty()) {
        Path::new(path)
            .parent()
            .unwrap_or(Path::new("."))
            .join("components/policy.sqlite")
    } else {
        std::env::temp_dir().join(format!(
            "kageha-policy-{}-{identity}/policy.sqlite",
            std::process::id()
        ))
    };
    let mut journals = JOURNALS
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| io::Error::other("Policy cache unavailable"))?;
    if let Some(journal) = journals.get(&path) {
        return Ok(journal.clone());
    }
    let journal = Arc::new(Journal::open(&path).map_err(io::Error::other)?);
    journals.insert(path, journal.clone());
    Ok(journal)
}
fn class(action: &str) -> ActionClass {
    match action {
        "browser_fill" => ActionClass::RoutineFill,
        "browser_upload" | "remote_upload" => ActionClass::Upload,
        "approve_plan" => ActionClass::Plan,
        _ => ActionClass::Mutation,
    }
}
fn approval(
    chat: &str,
    turn: &str,
    action: &str,
    detail: &str,
    context: &Value,
) -> ApprovalRequest {
    ApprovalRequest {
        authority: Authority {
            session: chat.into(),
            turn: turn.into(),
            binding: context["selection"]["binding_id"]
                .as_str()
                .or_else(|| context["binding_id"].as_str())
                .unwrap_or("workspace")
                .into(),
            generation: context["selection"]["generation"]
                .as_u64()
                .or_else(|| context["authority_generation"].as_u64())
                .unwrap_or(1),
            resource: context["url"]
                .as_str()
                .or_else(|| context["host"].as_str())
                .or_else(|| context["resource"].as_str())
                .unwrap_or(action)
                .into(),
            input_digest: kageha_policy::input_digest(context),
        },
        action: action.into(),
        class: class(action),
        detail: detail.into(),
    }
}
pub(crate) fn initialize(db: &Connection) -> rusqlite::Result<()> {
    // Retained source tables are an import source only, never a shared journal.
    db.execute_batch("CREATE TABLE IF NOT EXISTS ui_approvals(id TEXT PRIMARY KEY,chat_id TEXT NOT NULL,turn_id TEXT NOT NULL,action TEXT NOT NULL,detail TEXT NOT NULL,context TEXT NOT NULL,decision TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS ui_permissions(id INTEGER PRIMARY KEY CHECK(id=1),scope TEXT NOT NULL); INSERT OR IGNORE INTO ui_permissions VALUES (1,'ask');
      CREATE TABLE IF NOT EXISTS ui_policy_identity(id INTEGER PRIMARY KEY CHECK(id=1),source TEXT NOT NULL);")?;
    db.execute(
        "INSERT OR IGNORE INTO ui_policy_identity VALUES (1,?)",
        [uuid::Uuid::new_v4().to_string()],
    )?;
    let journal = owned(db).map_err(sql)?;
    let source: String = db.query_row(
        "SELECT source FROM ui_policy_identity WHERE id=1",
        [],
        |r| r.get(0),
    )?;
    let legacy_mode: String =
        db.query_row("SELECT scope FROM ui_permissions WHERE id=1", [], |r| {
            r.get(0)
        })?;
    let mode = if legacy_mode == "full" {
        PermissionMode::Full
    } else {
        PermissionMode::Ask
    };
    loop {
        let cursor = journal.import_cursor(&source).map_err(sql)?.unwrap_or(0);
        let mut q=db.prepare("SELECT rowid,id,chat_id,turn_id,action,detail,context,decision FROM ui_approvals WHERE rowid>? ORDER BY rowid LIMIT 256")?;
        let rows = q
            .query_map([cursor], |r| {
                Ok((
                    r.get::<_, u64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, String>(7)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let more = rows.len() == 256;
        let mut records = Vec::new();
        for (seq, id, chat, turn, action, detail, context, state) in rows {
            let context: Value = serde_json::from_str(&context).map_err(sql)?;
            let (state, decision) = match state.as_str() {
                "approved" => (ApprovalState::Approved, Some(true)),
                "denied" => (ApprovalState::Denied, Some(false)),
                "cancelled" => (ApprovalState::Cancelled, None),
                "pending" => (ApprovalState::Pending, None),
                _ => (ApprovalState::Interrupted, None),
            };
            records.push((
                seq,
                ApprovalRecord {
                    id,
                    request: approval(&chat, &turn, &action, &detail, &context),
                    state,
                    revision: 1,
                    decision,
                    scope: GrantScope::Once,
                    automatic: false,
                },
            ));
        }
        journal.import_batch(&source, mode, &records).map_err(sql)?;
        if !more {
            break;
        }
    }
    journal.recover().map_err(sql)
}
pub(crate) fn main_default(db: &Connection, chat: &str, agent: &str) -> rusqlite::Result<()> {
    if agent != "main" || crate::ui_store::get(db, "main-access-initialized", chat)?.is_some() {
        return Ok(());
    }
    let journal = owned(db).map_err(sql)?;
    // Delegated chats inherit their owning chat's profile; they cannot seed an
    // independent default, including when imported sessions lack a persona row.
    if journal.owner(chat).map_err(sql)? != chat { return Ok(()); }
    journal.set_profile(chat, PermissionProfile::Full).map_err(sql)?;
    crate::ui_store::put(db, "main-access-initialized", chat, json!({"version":1}))?;
    Ok(())
}

fn risk(request: &ApprovalRequest) -> &str {
    if request.class == ActionClass::Restricted { return "specialist"; }
    let action = request.action.as_str();
    if action == "approve_plan" {
        "plan"
    } else if matches!(action, "host_import_files" | "attach_past_file") {
        "host_files"
    } else if action.starts_with("computer_") {
        "computer"
    } else {
        "browser"
    }
}
/// Reuse a standing decision only for the same concrete action, inputs, resource,
/// and binding generation. The identity of an individual approval stays turn-scoped.
fn standing_key(request: &ApprovalRequest) -> String {
    let authority = &request.authority;
    kageha_policy::input_digest(&json!([request.action, request.detail, authority.binding,
        authority.generation, authority.resource, authority.input_digest]))
}
pub(crate) fn pending(db: &Connection, chat: &str) -> rusqlite::Result<Option<Value>> {
    if let Some(question) = crate::ui_questions::pending(db, chat)? { return Ok(Some(question)); }
    let rows = owned(db).map_err(sql)?.pending_family(chat).map_err(sql)?;
    let profile_scoped = owned(db)
        .map_err(sql)?
        .profile(chat)
        .map_err(sql)?
        .is_some();
    let delegated = owned(db).map_err(sql)?.owner(chat).map_err(sql)? != chat;
    Ok(rows.last().map(|r|json!({"approval_id":r.id,"action":r.request.action,"standing_key":standing_key(&r.request),"detail":r.request.detail,"turn_id":r.request.authority.turn,"risk_class":risk(&r.request),"session_id":chat,"profile_scoped":profile_scoped || r.request.authority.session!=chat || delegated})))
}
/// Closing a session has its own scope; stale questions or approvals from
/// earlier turns must not hide the one actionable SessionEnd approval.
pub(crate) fn pending_for_turn(db:&Connection,chat:&str,turn:&str)->rusqlite::Result<Option<Value>> {
    let journal=owned(db).map_err(sql)?;
    let profile_scoped=journal.profile(chat).map_err(sql)?.is_some();
    let delegated=journal.owner(chat).map_err(sql)?!=chat;
    Ok(journal.pending_family(chat).map_err(sql)?.into_iter().rev().find(|r|r.request.authority.turn==turn).map(|r|
        json!({"approval_id":r.id,"action":r.request.action,"standing_key":standing_key(&r.request),"detail":r.request.detail,"turn_id":r.request.authority.turn,"risk_class":risk(&r.request),"session_id":chat,"profile_scoped":profile_scoped || r.request.authority.session!=chat || delegated})))
}
struct UiApproval<'a> {
    db: &'a Database,
    journal: Arc<Journal>,
    automatic: bool,
    /// The id of the record `authorize_once` created, if it ran.
    created: Mutex<Option<String>>,
}
impl ApprovalPort for UiApproval<'_> {
    fn authorize_once(&self, request: &ApprovalRequest, control: &Control<'_>) -> io::Result<bool> {
        control.check()?;
        let record = self
            .journal
            .create(&uuid::Uuid::new_v4().to_string(), request, self.automatic)
            .map_err(io::Error::other)?;
        if let Ok(mut created) = self.created.lock() {
            *created = Some(record.id.clone());
        }
        if !self.automatic {
            webui::emit(
                self.db,
                &request.authority.session,
                &request.authority.turn,
                "event",
                json!({"kind":"approval_required","payload":{"approval_id":record.id,"action":request.action,"detail":request.detail,"risk_class":risk(request)}}),
            )?;
        }
        let allowed = if self.automatic {self.journal.wait(&record.id, &request.authority, control)?}
            else {kageha_contracts::request_budget::waiting(||self.journal.wait(&record.id, &request.authority, control))?};
        if !self.automatic {
            let reason = if allowed {
                "approved"
            } else {
                match self
                    .journal
                    .get(&record.id)
                    .map_err(io::Error::other)?
                    .map(|r| r.state)
                {
                    Some(ApprovalState::Denied) => "denied",
                    Some(ApprovalState::Cancelled) => "cancelled",
                    Some(ApprovalState::Interrupted) => "interrupted",
                    Some(ApprovalState::Consumed) => "consumed",
                    _ => "unavailable",
                }
            };
            webui::emit(
                self.db,
                &request.authority.session,
                &request.authority.turn,
                "event",
                json!({"kind":"approval_resolved","payload":{"approval_id":record.id,"approved":allowed,"reason":reason}}),
            )?;
        }
        Ok(allowed)
    }
}
/// A raised specialist risk always needs an exact, one-use operator decision.
/// It cannot be satisfied by Full access, routine grants or automatic review.
pub(crate) fn require_specialist(
    db: &Database,
    chat: &str,
    turn: &str,
    action: &str,
    context: &Value,
    cancel: &AtomicBool,
) -> io::Result<bool> {
    require_operator(db, chat, turn, action,
        &format!("Specialist policy requires approval for {action}"), context, cancel)
}

/// One operator decision bound to the actual resource and input.
pub(crate) fn require_operator(
    db: &Database, chat: &str, turn: &str, action: &str, detail: &str,
    context: &Value, cancel: &AtomicBool,
) -> io::Result<bool> {
    let journal = {
        let connection = db.lock().map_err(|_| io::Error::other("Database unavailable"))?;
        owned(&connection)?
    };
    let mut request = approval(chat, turn, action, detail, context);
    request.class = ActionClass::Restricted;
    let adapter = UiApproval { db, journal, automatic: false, created: Mutex::new(None) };
    adapter.authorize_once(&request, &Control { cancelled: cancel, deadline: None })
}
pub(crate) fn require(
    db: &Database,
    chat: &str,
    turn: &str,
    action: &str,
    detail: &str,
    context: &Value,
    cancel: &AtomicBool,
) -> io::Result<bool> {
    require_reviewed(db,chat,turn,action,detail,context,cancel,None)
}
pub(crate) fn require_reviewed(
    db:&Database,chat:&str,turn:&str,action:&str,detail:&str,context:&Value,cancel:&AtomicBool,
    browser_reviewer:Option<(&dyn ReviewPort,bool)>,
) -> io::Result<bool> {
    let approval = require_recorded(db,chat,turn,action,detail,context,cancel,browser_reviewer)?;
    if let Some(rationale) = approval.review_denial {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied,format!("Automatic approval review denied this action: {rationale}. Do not retry through another tool or workaround. Continue only with a materially safer alternative or ask the user.")));
    }
    Ok(approval.allowed)
}
/// The outcome of one approval request, with the journal record it created.
pub(crate) struct Approval {
    pub allowed: bool,
    /// The approval record's id. `None` when policy allowed the action without one.
    pub id: Option<String>,
    /// The record's final state.
    pub state: Option<ApprovalState>,
    /// Set when the chat's automatic reviewer denied the action: its rationale.
    pub review_denial: Option<String>,
}
/// Like [`require_reviewed`], but returns the approval record's id and state, so callers
/// can report a denial by id (`report_blocker` accepts denied approval ids). An
/// automatic review denial is returned in `review_denial` instead of as an error.
#[allow(clippy::too_many_arguments)]
pub(crate) fn require_recorded(
    db:&Database,chat:&str,turn:&str,action:&str,detail:&str,context:&Value,cancel:&AtomicBool,
    browser_reviewer:Option<(&dyn ReviewPort,bool)>,
) -> io::Result<Approval> {
    require_recorded_control(db,chat,turn,action,detail,context,&Control{cancelled:cancel,deadline:None},browser_reviewer)
}
/// Hook approvals observe the same caller deadline as the subsequent dispatch.
pub(crate) fn require_control(db:&Database,chat:&str,turn:&str,action:&str,detail:&str,context:&Value,control:&Control<'_>)->io::Result<bool> {
    let approval=require_recorded_control(db,chat,turn,action,detail,context,control,None)?;
    if let Some(rationale)=approval.review_denial {return Err(io::Error::new(io::ErrorKind::PermissionDenied,format!("Automatic approval review denied this action: {rationale}. Do not retry through another tool or workaround. Continue only with a materially safer alternative or ask the user.")));}
    Ok(approval.allowed)
}
#[allow(clippy::too_many_arguments)]
fn require_recorded_control(db:&Database,chat:&str,turn:&str,action:&str,detail:&str,context:&Value,control:&Control<'_>,browser_reviewer:Option<(&dyn ReviewPort,bool)>)->io::Result<Approval> {
    let journal = {
        let c = db
            .lock()
            .map_err(|_| io::Error::other("Database unavailable"))?;
        owned(&c)?
    };
    let request = approval(chat, turn, action, detail, context);
    journal.check_request(&request)?;
    if journal.profile(chat).map_err(io::Error::other)?.is_some() {
        let model = crate::ui_settings::configured(db, "", "reviewer")
            .ok()
            .and_then(|p| crate::ui_credentials::model(vec![p]).ok());
        use kageha_runtime::lifecycle::RunStore;
        let runtime = crate::runtime_host::store(db).map_err(|(_, e)| io::Error::other(e))?;
        let run = runtime.find_turn(turn).map_err(io::Error::other)?;
        let intent = run
            .as_ref()
            .filter(|r| r.spec.session == chat)
            .and_then(|r| kageha_runtime::review_context::user_intent(runtime.as_ref(), r).ok());
        let evidence: Vec<ReviewEvidence> = context["review_evidence"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .take(16)
            .collect();
        let reviewer = model
            .as_ref()
            .zip(intent.as_deref())
            .map(|(model, intent)| kageha_models::reviewer::ModelReviewer {
                model,
                intent,
                evidence: &evidence,
            });
        if journal.profile(chat).map_err(io::Error::other)?==Some(PermissionProfile::AutoReview) {
            if let Some((shadow,false))=browser_reviewer {let _=shadow.review(&request,&control);}
        }
        let selected_reviewer=browser_reviewer.filter(|(_,active)|*active).map(|(reviewer,_)|reviewer)
            .or_else(||reviewer.as_ref().map(|r|r as &dyn ReviewPort));
        let record = journal.prepare_profile(
            &request,
            selected_reviewer,
            &control,
        )?;
        if record.state == ApprovalState::Pending {
            webui::emit(
                db,
                chat,
                turn,
                "event",
                json!({"kind":"approval_required","payload":{"approval_id":record.id,"action":action,"detail":detail,"risk_class":risk(&request),"profile_scoped":true}}),
            )?;
        }
        let allowed = if record.state==ApprovalState::Pending {
            kageha_contracts::request_budget::waiting(||journal.wait(&record.id, &request.authority, &control))?
        } else {journal.wait(&record.id, &request.authority, &control)?};
        webui::emit(
            db,
            chat,
            turn,
            "event",
            json!({"kind":"approval_resolved","payload":{"approval_id":record.id,"approved":allowed}}),
        )?;
        let state = journal.get(&record.id).map_err(io::Error::other)?.map(|r| r.state);
        let review_denial = if record.state == ApprovalState::Denied {
            Some(journal.last_review(&record.id).map_err(io::Error::other)?.map(|r|r.rationale).unwrap_or_else(||"Not approved".into()))
        } else { None };
        return Ok(Approval { allowed, id: Some(record.id), state, review_denial });
    }
    let mode = journal.mode().map_err(io::Error::other)?;
    let automatic = !journal.requires_manual(action)
        && kageha_policy::DefaultPolicy.evaluate(&request, mode)? == Authorization::Allow;
    let adapter = UiApproval {
        db,
        journal: journal.clone(),
        automatic,
        created: Mutex::new(None),
    };
    let allowed = if journal.requires_manual(action) {
        adapter.authorize_once(&request, &control)?
    } else if automatic {
        let allowed = adapter.authorize_once(&request, &control)?;
        control.check()?;
        allowed
    } else {
        kageha_policy::authorize(
            &kageha_policy::DefaultPolicy,
            Some(&adapter),
            &request,
            mode,
            &control,
        )?
    };
    let id = adapter.created.lock().ok().and_then(|created| created.clone());
    let state = match &id {
        Some(id) => journal.get(id).map_err(io::Error::other)?.map(|r| r.state),
        None => None,
    };
    Ok(Approval { allowed, id, state, review_denial: None })
}
pub(crate) fn decide(db: &Database, payload: &Value) -> crate::ui_store::ApiResult<Value> {
    use crate::ui_store::db_error;
    let scope = match payload["scope"].as_str().unwrap_or("once") {
        "once" => GrantScope::Once,
        "session" => GrantScope::Session,
        "full" => GrantScope::Full,
        _ => return Err((400, "Invalid approval scope".into())),
    };
    let approved = payload["approved"]
        .as_bool()
        .ok_or((400, "approved must be a boolean".into()))?;
    let id = payload["approval_id"].as_str().unwrap_or("");
    let connection = db.lock().map_err(db_error)?;
    let journal = owned(&connection).map_err(db_error)?;
    let record = journal
        .get(id)
        .map_err(db_error)?
        .ok_or((404, "Approval not found".into()))?;
    let mut q = connection
        .prepare("SELECT request_id FROM native_chat_turns WHERE chat_id=? AND status='running'")
        .map_err(db_error)?;
    let ids = q
        .query_map([&record.request.authority.session], |r| {
            r.get::<_, String>(0)
        })
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    let active = ids.iter().any(|id| {
        webui::turn_id(&record.request.authority.session, id) == record.request.authority.turn
    }) || crate::ui_store::get(&connection,"session-lifecycle",&record.request.authority.session).map_err(db_error)?
        .is_some_and(|state|state["status"]=="closing" && state["turn_id"]==record.request.authority.turn);
    if record.state != ApprovalState::Pending || !active {
        return Ok(
            json!({"ok":true,"approval_id":id,"approved":record.decision==Some(true),"scope":"once","expired":!active}),
        );
    }
    let decided = journal
        .decide(
            id,
            &ApprovalDecision {
                expected_revision: record.revision,
                authority: record.request.authority.clone(),
                approved,
                scope,
            },
        )
        .map_err(|e| (409, e.message))?;
    // "Full" on an escalated shell prompt is its "Always allow": the command prefix joins
    // the Approval_Policy allowlist. Delegated workers can't add entries; their approval
    // still counts once.
    let mut allowlisted = false;
    if approved && scope == GrantScope::Full {
        if let Some(entry) = allow_entry(&record.request) {
            allowlisted = always_allow(&connection, &record.request.authority.session, entry).is_ok();
        }
    }
    Ok(
        json!({"ok":true,"approval_id":id,"approved":approved,"allowlisted":allowlisted,"scope":match decided.scope{GrantScope::Once=>"once",GrantScope::Session=>"session",GrantScope::Full=>"full"}}),
    )
}
/// Prefix of [`ApprovalRequest`] resources for escalated shell commands: `command:<scope>`,
/// where `<scope>` is the command's allowlist scope (`kageha_policy::classes::command_scope`).
pub(crate) const COMMAND_RESOURCE: &str = "command:";
/// The allowlist entry an "Always allow" on `request` adds, if it has one. Only escalated
/// shell requests with a storable command prefix do.
fn allow_entry(request: &ApprovalRequest) -> Option<kageha_policy::classes::AllowEntry> {
    use kageha_policy::classes::{ActionClass, AllowEntry, Scope};
    let prefix = request.authority.resource.strip_prefix(COMMAND_RESOURCE)?;
    let scope = Scope::Command(prefix.to_owned());
    (request.action == "shell" && scope.storable()).then_some(AllowEntry { class: ActionClass::ExternalWrite, scope })
}
pub(crate) fn cancel_turn(db: &Connection, chat: &str, turn: &str) -> io::Result<()> {
    owned(db)?.cancel_turn(chat, turn).map_err(io::Error::other)
}
pub(crate) fn handle(mut request: Request, db: &Database) -> io::Result<()> {
    if request.method() != &Method::Post {
        server::response(request, 405, json!({"error":"POST required"}));
        return Ok(());
    }
    let result = webui::body(&mut request)
        .map_err(|_| (400, "Invalid JSON".into()))
        .and_then(|p| decide(db, &p));
    match result {
        Ok(v) => server::response(request, 200, v),
        Err((code, e)) => server::response(request, code, json!({"error":e})),
    }
    Ok(())
}
pub(crate) fn override_request(mut request: Request, db: &Database) -> io::Result<()> {
    if request.method() != &Method::Post {
        server::response(request, 405, json!({"error":"POST required"}));
        return Ok(());
    }
    let payload = webui::body(&mut request)?;
    let result = (|| -> io::Result<()> {
        let id = payload["approval_id"]
            .as_str()
            .ok_or_else(|| io::Error::other("Approval ID required"))?;
        let authority: Authority = serde_json::from_value(payload["authority"].clone())?;
        let revision = payload["expected_revision"]
            .as_u64()
            .ok_or_else(|| io::Error::other("Revision required"))?;
        let c = db
            .lock()
            .map_err(|_| io::Error::other("Database unavailable"))?;
        owned(&c)?
            .request_override(id, &authority, revision)
            .map_err(io::Error::other)
    })();
    match result {
        Ok(()) => server::response(
            request,
            200,
            json!({"retry_requested":true,"input_dispatched":false}),
        ),
        Err(e) => server::response(request, 409, json!({"error":e.to_string()})),
    }
    Ok(())
}
pub(crate) fn permissions(mut request: Request, db: &Database) -> io::Result<()> {
    let journal = {
        let c = db
            .lock()
            .map_err(|_| io::Error::other("Database unavailable"))?;
        owned(&c)?
    };
    let session = request
        .url()
        .split('?')
        .nth(1)
        .unwrap_or("")
        .split('&')
        .find_map(|p| p.strip_prefix("session_id="))
        .filter(|s| crate::server::identifier(s))
        .map(str::to_owned);
    let mut session = session;
    if request.method() == &Method::Post {
        let payload = webui::body(&mut request)?;
        if payload.get("profile").is_some() {
            let profile: PermissionProfile =
                match serde_json::from_value(payload["profile"].clone()) {
                    Ok(p) => p,
                    Err(_) => {
                        server::response(
                            request,
                            400,
                            json!({"error":"Unknown permission profile"}),
                        );
                        return Ok(());
                    }
                };
            session = payload["session_id"]
                .as_str()
                .filter(|s| crate::server::identifier(s))
                .map(str::to_owned);
            let Some(ref chat) = session else {
                server::response(
                    request,
                    400,
                    json!({"error":"Select a chat before changing permissions"}),
                );
                return Ok(());
            };
            if profile == PermissionProfile::Full && payload["confirm_full"] != true {
                server::response(
                    request,
                    400,
                    json!({"error":"Full access requires explicit confirmation"}),
                );
                return Ok(());
            }
            if profile == PermissionProfile::AutoReview
                && crate::ui_settings::configured(db, "", "reviewer").is_err()
                && !crate::ui_browser_decisions::reviewer_ready(db)
            {
                server::response(
                    request,
                    409,
                    json!({"error":"Configure a model before enabling automatic review"}),
                );
                return Ok(());
            }
            journal
                .set_profile(chat, profile)
                .map_err(io::Error::other)?;
        } else {
            let mode = match payload["mode"].as_str() {
                Some("ask") => PermissionMode::Ask,
                Some("auto" | "session") => PermissionMode::Routine,
                Some("full") => PermissionMode::Full,
                _ => {
                    server::response(request, 400, json!({"error":"Unknown permission mode"}));
                    return Ok(());
                }
            };
            if let Err(e) = journal.set_mode(mode) {
                server::response(request, 403, json!({"error":e.message}));
                return Ok(());
            }
        }
    } else if request.method() != &Method::Get {
        server::response(request, 405, json!({"error":"GET or POST required"}));
        return Ok(());
    }
    let scope = match journal.mode().map_err(io::Error::other)? {
        PermissionMode::Ask => "ask",
        PermissionMode::Routine => "session",
        PermissionMode::Full => "full",
    };
    let explicit_profile = session
        .as_deref()
        .map(|chat| journal.profile(chat))
        .transpose()
        .map_err(io::Error::other)?
        .flatten();
    let profile = explicit_profile.unwrap_or(PermissionProfile::Ask);
    let owner_session = session
        .as_deref()
        .map(|s| journal.owner(s))
        .transpose()
        .map_err(io::Error::other)?;
    let reviewer_available = crate::ui_settings::configured(db, "", "reviewer").is_ok() || crate::ui_browser_decisions::reviewer_ready(db);
    let denials = session
        .as_deref()
        .map(|s| journal.denials(s))
        .transpose()
        .map_err(io::Error::other)?
        .unwrap_or_default()
        .into_iter()
        .map(|r| {
            let review = journal.last_review(&r.id).ok().flatten();
            json!({"record":r,"review":review})
        })
        .collect::<Vec<_>>();
    server::response(
        request,
        200,
        json!({"denials":denials,"ok":true,"mode":if scope=="ask"{"ask"}else{"auto"},"scope":scope,"auto_approve":scope!="ask","profile":profile,"legacy_routine":explicit_profile.is_none() && scope!="ask","session_id":session,"reviewer_available":reviewer_available,"chat_scoped_profiles":true,"owner_session":owner_session,"requirements":journal.requirements,"sandbox":"tool-boundaries","os_sandbox":false}),
    );
    Ok(())
}
/// Whether `chat` is a delegated worker. Workers inherit their owner's policy and allowlist.
fn delegated(db: &Connection, chat: &str) -> io::Result<bool> {
    Ok(owned(db)?.owner(chat).map_err(io::Error::other)? != chat)
}
/// The Approval_Policy action class, scope and decision of one Host call
/// (design section 14), using the allowlist at `allowlist` (`None`: the default
/// `approvals_allowlist.json`). Consulted by escalated `shell` calls; other existing
/// approval prompts are unchanged until the dispatch path adopts this decision.
pub(crate) fn action_class(
    db: &Connection,
    chat: &str,
    tool: &str,
    args: &Value,
    context: &kageha_policy::classes::ClassifyContext,
    allowlist: Option<&Path>,
) -> io::Result<(kageha_policy::classes::ActionClass, kageha_policy::classes::Scope, kageha_policy::classes::Decision)> {
    use kageha_policy::classes;
    let worker = delegated(db, chat)?;
    let (class, scope) = classes::classify(tool, args, context);
    // A missing or unreadable allowlist grants nothing.
    let allowlist = allowlist
        .map(Path::to_path_buf)
        .or_else(classes::default_allowlist_path)
        .and_then(|path| classes::Allowlist::load(&path).ok())
        .unwrap_or_default();
    let decision = classes::decide(class, &scope, &allowlist, worker);
    Ok((class, scope, decision))
}
/// "Always allow" on an approval prompt: adds `{class, scope}` to
/// `approvals_allowlist.json`. Delegated workers are refused. The WebUI decision endpoint
/// only knows once/session/full grants; a `full` grant on an escalated shell prompt
/// calls this (see [`decide`]).
pub(crate) fn always_allow(
    db: &Connection,
    chat: &str,
    entry: kageha_policy::classes::AllowEntry,
) -> io::Result<bool> {
    let path = kageha_policy::classes::default_allowlist_path()
        .ok_or_else(|| io::Error::other("Kageha home is not configured"))?;
    always_allow_at(db, chat, &path, entry)
}
fn always_allow_at(
    db: &Connection,
    chat: &str,
    path: &Path,
    entry: kageha_policy::classes::AllowEntry,
) -> io::Result<bool> {
    kageha_policy::classes::add_allow(path, entry, delegated(db, chat)?)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn always_allow_is_refused_for_delegated_workers() {
        use kageha_policy::classes::{ActionClass, AllowEntry, Allowlist, Scope};
        let db = Connection::open_in_memory().unwrap();
        initialize(&db).unwrap();
        owned(&db).unwrap().delegate("owner-chat", "worker-chat").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("approvals_allowlist.json");
        let entry = AllowEntry { class: ActionClass::MessageSend, scope: Scope::Connector("slack".into()) };
        let error = always_allow_at(&db, "worker-chat", &path, entry.clone()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(always_allow_at(&db, "owner-chat", &path, entry).unwrap());
        assert!(Allowlist::load(&path).unwrap().allows(ActionClass::MessageSend, &Scope::Connector("slack".into())));
    }
    #[test]
    fn a_full_grant_allowlists_only_simple_escalated_shell_prefixes() {
        use kageha_policy::classes::{ActionClass as Class, Scope};
        let shell = |resource: &str| approval("chat", "turn", "shell", "Run", &json!({"command":"x","resource":resource}));
        let entry = allow_entry(&shell("command:git push")).unwrap();
        assert_eq!((entry.class, entry.scope), (Class::ExternalWrite, Scope::Command("git push".into())));
        assert_eq!(shell("command:git push").authority.resource, "command:git push");
        // Compound commands keep their full text as scope and are never stored.
        assert!(allow_entry(&shell("command:printf x > /etc/y")).is_none());
        assert!(allow_entry(&shell("shell")).is_none());
        let remote = approval("chat", "turn", "remote_exec", "Run", &json!({"resource":"command:ls"}));
        assert!(allow_entry(&remote).is_none());
    }
    #[test]
    fn main_default_is_seeded_once_and_explicit_changes_survive() {
        let db = Connection::open_in_memory().unwrap();
        crate::ui_store::initialize(&db).unwrap();
        initialize(&db).unwrap();
        main_default(&db, "main-chat", "main").unwrap();
        let journal = owned(&db).unwrap();
        assert_eq!(journal.profile("main-chat").unwrap(), Some(PermissionProfile::Full));
        journal.set_profile("main-chat", PermissionProfile::Ask).unwrap();
        main_default(&db, "main-chat", "main").unwrap();
        main_default(&db, "specialist-chat", "specialist").unwrap();
        journal.delegate("main-chat", "worker-chat").unwrap();
        main_default(&db, "worker-chat", "main").unwrap();
        assert_eq!(journal.profile("main-chat").unwrap(), Some(PermissionProfile::Ask));
        assert_eq!(journal.profile("specialist-chat").unwrap(), None);
        assert_eq!(journal.profile("worker-chat").unwrap(), Some(PermissionProfile::Ask));
    }

    #[test]
    fn legacy_import_preserves_source_ids_and_never_reuses_approved_inputs() {
        let db = Connection::open_in_memory().unwrap();
        initialize(&db).unwrap();
        for (id, state) in [
            ("waiting", "pending"),
            ("accepted", "approved"),
            ("refused", "denied"),
        ] {
            db.execute("INSERT INTO ui_approvals VALUES (?,'chat','turn','browser_upload','fixture','{}',?)",rusqlite::params![id,state]).unwrap();
        }
        initialize(&db).unwrap();
        let journal = owned(&db).unwrap();
        assert_eq!(
            journal.get("waiting").unwrap().unwrap().state,
            ApprovalState::Interrupted
        );
        assert_eq!(
            journal.get("accepted").unwrap().unwrap().state,
            ApprovalState::Consumed
        );
        assert_eq!(
            journal.get("refused").unwrap().unwrap().state,
            ApprovalState::Denied
        );
        assert_eq!(
            db.query_row(
                "SELECT decision FROM ui_approvals WHERE id='waiting'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "pending"
        );
        journal.set_mode(PermissionMode::Full).unwrap();
        initialize(&db).unwrap();
        assert_eq!(journal.mode().unwrap(), PermissionMode::Full);
        journal.set_mode(PermissionMode::Routine).unwrap();
        initialize(&db).unwrap();
        assert_eq!(journal.mode().unwrap(), PermissionMode::Ask);
    }
}

#[cfg(test)]
mod owned_tests {
    use super::*;
    #[test]
    fn automatic_ui_approvals_are_consumed_in_the_component_not_source_tables() {
        let connection = Connection::open_in_memory().unwrap();
        initialize(&connection).unwrap();
        let journal = owned(&connection).unwrap();
        journal.set_mode(PermissionMode::Routine).unwrap();
        let db = crate::agent_host::DatabaseState::open(connection).unwrap();
        assert!(require(
            &db,
            "chat",
            "turn",
            "browser_fill",
            "Fill exact field",
            &json!({"url":"https://example.test","arguments_sha256":"exact"}),
            &AtomicBool::new(false)
        )
        .unwrap());
        assert_eq!(
            db.lock()
                .unwrap()
                .query_row("SELECT count(*) FROM ui_approvals", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        let rows = journal.list(Some("chat"), None, false, 0, 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1.state, ApprovalState::Consumed);
    }
}
