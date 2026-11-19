//! Authenticated, bounded service API. Blocking ports never execute on reactor threads.
use crate::ExecutionHost;
use axum::{
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{
        sse::{Event, KeepAlive},
        IntoResponse, Response, Sse,
    },
    routing::{get, post},
    Json, Router,
};
use kageha_contracts::{policy::*, ErrorCode, Result, ServiceError};
use kageha_runtime::lifecycle::*;
use serde::Deserialize;
use serde_json::json;
use std::{convert::Infallible, sync::Arc, time::Duration};
#[derive(Clone)]
struct Host {
    runtime: Arc<ExecutionHost>,
    token: Arc<str>,
    admission: Arc<tokio::sync::Semaphore>,
}
type Failure = (StatusCode, Json<ServiceError>);
fn failure(e: ServiceError) -> Failure {
    let status = match e.code {
        ErrorCode::Invalid => StatusCode::BAD_REQUEST,
        ErrorCode::Conflict | ErrorCode::StaleGeneration => StatusCode::CONFLICT,
        ErrorCode::Unauthorized => StatusCode::FORBIDDEN,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    };
    (status, Json(e))
}
async fn auth(State(host): State<Host>, request: Request, next: Next) -> Response {
    let supplied = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    let mut diff = supplied.len() ^ host.token.len();
    for (i, b) in host.token.bytes().enumerate() {
        diff |= (b ^ supplied.as_bytes().get(i).copied().unwrap_or(0)) as usize;
    }
    if diff != 0 {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    next.run(request).await
}
async fn blocking<T: Send + 'static>(
    host: Host,
    call: impl FnOnce(Arc<ExecutionHost>) -> Result<T> + Send + 'static,
) -> std::result::Result<Json<T>, Failure> {
    let permit = host.admission.try_acquire_owned().map_err(|_| {
        (
            StatusCode::TOO_MANY_REQUESTS,
            Json(ServiceError::new(
                ErrorCode::Unavailable,
                "Runtime request queue is full",
            )),
        )
    })?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        call(host.runtime)
    })
    .await
    .map_err(|_| {
        failure(ServiceError::new(
            ErrorCode::Unavailable,
            "Runtime worker failed",
        ))
    })?
    .map(Json)
    .map_err(failure)
}
async fn info(State(host): State<Host>) -> Json<serde_json::Value> {
    let mut capabilities = vec![
        "runs.submit",
        "runs.status",
        "runs.cancel",
        "runs.steer",
        "runs.resume_confirmed",
        "events.resume",
        "receipts.inspect",
        "tools.discover",
        "tools.release_context",
        "recovery",
    ];
    if host.runtime.workers.is_some(){capabilities.extend(["workers.catalog","workers.profile_settings","workers.policy_patterns","workers.risk_escalation","workers.spawn","workers.status","workers.cancel","workers.wait_any","workspace.run_scoped","workspace.reconcile","workspace.cleanup_confirmed"]);}
    if host.runtime.planning.is_some(){capabilities.extend(["planning.review","planning.required_outcomes","planning.evidence","planning.revisions","planning.continuation"]);}
    if host.runtime.hooks.is_some(){capabilities.extend(["hooks.session_start","hooks.pre_tool","hooks.before_shell","hooks.post_tool","hooks.receipts","hooks.acknowledge_unknown"]);}
    if host.runtime.journal.is_some() {
        capabilities.extend([
            "approvals.list",
            "approvals.status",
            "approvals.decide",
            "permissions.profiles",
        ]);
    }
    Json(
        json!({"protocol_version":1,"component":"runtime","bindings":{"models.default":{"generation":1},"tools.workspace":{"generation":1}},"capabilities":capabilities,"live_replacement":false,"remote_approvals":host.runtime.journal.is_some()}),
    )
}

async fn release_tools(
    State(host): State<Host>,
    Path(session): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, Failure> {
    blocking(host, move |h| {
        if h.supervisor
            .store
            .active()?
            .iter()
            .any(|run| run.spec.session == session)
        {
            return Err(ServiceError::new(
                ErrorCode::Conflict,
                "Stop active runs before releasing tool connections",
            ));
        }
        h.tools
            .release_context(&session)
            .map_err(|e| ServiceError::new(ErrorCode::Unavailable, e.to_string()))?;
        Ok(json!({"released":true,"receipts_retained":true}))
    })
    .await
}
async fn tool_catalog(
    State(host): State<Host>,
) -> std::result::Result<Json<serde_json::Value>, Failure> {
    blocking(host,move|h|{let mut tools=h.tools.discover().map_err(|e|ServiceError::new(ErrorCode::Unavailable,e.to_string()))?;if h.workers.is_some(){tools.extend(crate::workers::specs());}if h.planning.is_some(){tools.extend(crate::planning::specs());}Ok(json!({"tools":tools}))}).await
}
fn plan_failure(e:kageha_workflow::planning::Error)->ServiceError{use kageha_workflow::planning::Code;ServiceError::new(match e.code{Code::Invalid=>ErrorCode::Invalid,Code::Conflict=>ErrorCode::Conflict,Code::NotFound=>ErrorCode::Invalid,Code::Unavailable=>ErrorCode::Unavailable},e.message)}
async fn plan(State(host):State<Host>,Path(id):Path<String>)->std::result::Result<Json<serde_json::Value>,Failure>{blocking(host,move|h|{
    let plans=h.planning.as_ref().ok_or_else(||ServiceError::new(ErrorCode::Unsupported,"Planning composition unavailable"))?;
    let run=h.supervisor.store.get(&id)?.ok_or_else(||ServiceError::new(ErrorCode::Invalid,"Unknown run"))?;
    let key=plans.key(&run,h.supervisor.store.as_ref()).map_err(|e|ServiceError::new(ErrorCode::Conflict,e.to_string()))?;
    let record=plans.service.load(&key).map_err(plan_failure)?;
    Ok(json!({"plan":record.as_ref().map(|r|r.value()),"complete":record.as_ref().is_some_and(|r|r.complete()),"plan_key":key}))
}).await}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanEdit {command_id:String,expected_revision:u64,document:serde_json::Value}
async fn edit_plan(State(host):State<Host>,Path(id):Path<String>,Json(input):Json<PlanEdit>)->std::result::Result<Json<serde_json::Value>,Failure>{blocking(host,move|h|{
    if input.command_id.is_empty()||input.command_id.len()>256||input.command_id.chars().any(char::is_control){return Err(ServiceError::new(ErrorCode::Invalid,"Invalid plan edit identity"));}
    let plans=h.planning.as_ref().ok_or_else(||ServiceError::new(ErrorCode::Unsupported,"Planning composition unavailable"))?;
    let run=h.supervisor.store.get(&id)?.ok_or_else(||ServiceError::new(ErrorCode::Invalid,"Unknown run"))?;
    if run.spec.input.get("planning").is_none(){return Err(ServiceError::new(ErrorCode::Conflict,"No planning assignment for this run"));}
    let key=plans.key(&run,h.supervisor.store.as_ref()).map_err(|e|ServiceError::new(ErrorCode::Conflict,e.to_string()))?;
    let record=plans.service.propose(&key,&kageha_workflow::planning::digest(&json!(["operator-edit",input.command_id])),Some(input.expected_revision),&input.document).map_err(plan_failure)?;
    Ok(json!({"plan":record.value(),"complete":record.complete()}))
}).await}
async fn hook_receipts(State(host):State<Host>,Path(id):Path<String>)->std::result::Result<Json<serde_json::Value>,Failure>{blocking(host,move|h|{
    let hooks=h.hooks.as_ref().ok_or_else(||ServiceError::new(ErrorCode::Unsupported,"Hook composition unavailable"))?;
    let run=h.supervisor.store.get(&id)?.ok_or_else(||ServiceError::new(ErrorCode::Invalid,"Unknown run"))?;
    let rows=hooks.journal.receipts(Some(&run.spec.session)).map_err(|e|ServiceError::new(ErrorCode::Unavailable,e.to_string()))?;
    Ok(json!({"receipts":rows.into_iter().filter(|r|r["turn"]==run.spec.turn).collect::<Vec<_>>()}))
}).await}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HookAck {receipt:String,target:String,confirmed:bool}
async fn acknowledge_hook(State(host):State<Host>,Path(id):Path<String>,Json(input):Json<HookAck>)->std::result::Result<Json<serde_json::Value>,Failure>{blocking(host,move|h|{
    let hooks=h.hooks.as_ref().ok_or_else(||ServiceError::new(ErrorCode::Unsupported,"Hook composition unavailable"))?;
    let run=h.supervisor.store.get(&id)?.ok_or_else(||ServiceError::new(ErrorCode::Invalid,"Unknown run"))?;
    if !input.confirmed||!run.state.terminal(){return Err(ServiceError::new(ErrorCode::Conflict,"Stop the run and explicitly acknowledge the unknown hook outcome"));}
    let rows=hooks.journal.receipts(Some(&run.spec.session)).map_err(|e|ServiceError::new(ErrorCode::Unavailable,e.to_string()))?;
    if !rows.iter().any(|r|r["turn"]==run.spec.turn&&r["receipt"]==input.receipt&&r["target"]==input.target){return Err(ServiceError::new(ErrorCode::Unauthorized,"Hook receipt is outside this run"));}
    hooks.journal.acknowledge(&input.receipt,&input.target).map_err(|e|ServiceError::new(ErrorCode::Conflict,e.to_string()))?;
    Ok(json!({"acknowledged":true,"outcome":"unknown","replayed":false}))
}).await}
async fn worker_catalog(State(host):State<Host>)->std::result::Result<Json<serde_json::Value>,Failure>{blocking(host,|h|{let workers=h.workers.as_ref().ok_or_else(||ServiceError::new(ErrorCode::Unsupported,"Worker composition unavailable"))?;Ok(json!({"specialists":workers.catalog_entries().map_err(|e|ServiceError::new(ErrorCode::Unavailable,e.to_string()))?,"profiles":workers.profile_settings().map_err(|e|ServiceError::new(ErrorCode::Unavailable,e.to_string()))?}))}).await}
async fn workspace(State(host):State<Host>,Path(id):Path<String>)->std::result::Result<Json<serde_json::Value>,Failure>{blocking(host,move|h|{let workers=h.workers.as_ref().ok_or_else(||ServiceError::new(ErrorCode::Unsupported,"Worker composition unavailable"))?;let run=h.supervisor.store.get(&id)?.ok_or_else(||ServiceError::new(ErrorCode::Invalid,"Unknown run"))?;workers.workspace(&run,h.supervisor.store.as_ref())}).await}
async fn reconcile_workspace(State(host):State<Host>,Path(id):Path<String>)->std::result::Result<Json<serde_json::Value>,Failure>{blocking(host,move|h|{let w=h.workers.as_ref().ok_or_else(||ServiceError::new(ErrorCode::Unsupported,"Worker composition unavailable"))?;let r=h.supervisor.store.get(&id)?.ok_or_else(||ServiceError::new(ErrorCode::Invalid,"Unknown run"))?;w.reconcile_workspace(&r,h.supervisor.store.as_ref())}).await}
#[derive(Deserialize)]#[serde(deny_unknown_fields)]struct Cleanup{confirmed:bool}
async fn cleanup_workspace(State(host):State<Host>,Path(id):Path<String>,Json(q):Json<Cleanup>)->std::result::Result<Json<serde_json::Value>,Failure>{blocking(host,move|h|{let w=h.workers.as_ref().ok_or_else(||ServiceError::new(ErrorCode::Unsupported,"Worker composition unavailable"))?;let r=h.supervisor.store.get(&id)?.ok_or_else(||ServiceError::new(ErrorCode::Invalid,"Unknown run"))?;w.cleanup_workspace(&r,h.supervisor.store.as_ref(),q.confirmed)}).await}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Resume{command_id:String,confirmed:bool}
async fn resume(State(host):State<Host>,Path(id):Path<String>,Json(request):Json<Resume>)->std::result::Result<Json<RunRecord>,Failure>{blocking(host,move|h|h.resume(&id,&request.command_id,request.confirmed)).await}
async fn submit(
    State(host): State<Host>,
    Json(spec): Json<RunSpec>,
) -> std::result::Result<Json<RunRecord>, Failure> {
    blocking(host, move |h| h.submit(&spec)).await
}
async fn status(
    State(host): State<Host>,
    Path(id): Path<String>,
) -> std::result::Result<Json<RunRecord>, Failure> {
    blocking(host, move |h| {
        h.supervisor
            .store
            .get(&id)?
            .ok_or_else(|| ServiceError::new(ErrorCode::Invalid, "Unknown run"))
    })
    .await
}
async fn cancel(
    State(host): State<Host>,
    Path(id): Path<String>,
) -> std::result::Result<Json<RunRecord>, Failure> {
    blocking(host, move |h| h.supervisor.cancel(&id)).await
}
async fn steer(
    State(host): State<Host>,
    Path(id): Path<String>,
    Json(message): Json<Steering>,
) -> std::result::Result<Json<serde_json::Value>, Failure> {
    blocking(host, move |h| {
        h.supervisor.store.steer(&id, &message)?;
        Ok(json!({"queued":true,"id":message.id}))
    })
    .await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileChange {
    session: String,
    profile: kageha_contracts::policy::PermissionProfile,
    #[serde(default)]
    confirm_full: bool,
}
#[derive(Deserialize)]
struct ProfileQuery {
    session: String,
}
async fn permission_status(
    State(host): State<Host>,
    Query(q): Query<ProfileQuery>,
) -> std::result::Result<Json<serde_json::Value>, Failure> {
    blocking(host,move|h|{let j=h.journal.as_ref().ok_or_else(||ServiceError::new(ErrorCode::Unsupported,"No policy journal configured"))?;
        Ok(json!({"owner_session":j.owner(&q.session)?,"session":q.session,"profile":j.profile(&q.session)?.unwrap_or(kageha_contracts::policy::PermissionProfile::Ask),"sandbox":"tool-boundaries","os_sandbox":false,"requirements":j.requirements}))}).await
}
async fn permission_change(
    State(host): State<Host>,
    Json(q): Json<ProfileChange>,
) -> std::result::Result<Json<serde_json::Value>, Failure> {
    blocking(host, move |h| {
        let j = h.journal.as_ref().ok_or_else(|| {
            ServiceError::new(ErrorCode::Unsupported, "No policy journal configured")
        })?;
        if q.profile == kageha_contracts::policy::PermissionProfile::Full && !q.confirm_full {
            return Err(ServiceError::new(
                ErrorCode::Invalid,
                "Full access requires explicit confirmation",
            ));
        }
        j.set_profile(&q.session, q.profile)?;
        Ok(json!({"session":q.session,"profile":q.profile}))
    })
    .await
}
#[derive(Deserialize)]
struct ApprovalPage {
    session: Option<String>,
    turn: Option<String>,
    after: Option<u64>,
    pending: Option<bool>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OverrideRequest {
    approval_id: String,
    authority: Authority,
    expected_revision: u64,
}
async fn denials(
    State(host): State<Host>,
    Query(q): Query<ProfileQuery>,
) -> std::result::Result<Json<serde_json::Value>, Failure> {
    blocking(host, move |h| {
        let j = h
            .journal
            .as_ref()
            .ok_or_else(|| ServiceError::new(ErrorCode::Unsupported, "No policy journal"))?;
        let rows = j.denials(&q.session)?;
        let data = rows
            .into_iter()
            .map(|r| {
                let review = j.last_review(&r.id).ok().flatten();
                json!({"record":r,"review":review})
            })
            .collect::<Vec<_>>();
        Ok(json!({"denials":data}))
    })
    .await
}
async fn review_override(
    State(host): State<Host>,
    Json(q): Json<OverrideRequest>,
) -> std::result::Result<Json<serde_json::Value>, Failure> {
    blocking(host, move |h| {
        h.journal
            .as_ref()
            .ok_or_else(|| ServiceError::new(ErrorCode::Unsupported, "No policy journal"))?
            .request_override(&q.approval_id, &q.authority, q.expected_revision)?;
        Ok(json!({"retry_requested":true,"input_dispatched":false}))
    })
    .await
}
async fn approvals(
    State(host): State<Host>,
    Query(page): Query<ApprovalPage>,
) -> std::result::Result<Json<serde_json::Value>, Failure> {
    blocking(host,move|h|{
        let journal=h.journal.as_ref().ok_or_else(||ServiceError::new(ErrorCode::Unsupported,"No policy journal configured"))?;
        let rows=journal.list(page.session.as_deref(),page.turn.as_deref(),page.pending.unwrap_or(true),page.after.unwrap_or(0),128)?;
        Ok(json!({"approvals":rows.iter().map(|(_,r)|r).collect::<Vec<_>>(),"next_cursor":rows.last().map(|(n,_)|n)}))
    }).await
}
async fn approval_status(
    State(host): State<Host>,
    Path(id): Path<String>,
) -> std::result::Result<Json<ApprovalRecord>, Failure> {
    blocking(host, move |h| {
        h.journal
            .as_ref()
            .ok_or_else(|| {
                ServiceError::new(ErrorCode::Unsupported, "No policy journal configured")
            })?
            .get(&id)?
            .ok_or_else(|| ServiceError::new(ErrorCode::Invalid, "Approval not found"))
    })
    .await
}
async fn approval_decision(
    State(host): State<Host>,
    Path(id): Path<String>,
    Json(decision): Json<ApprovalDecision>,
) -> std::result::Result<Json<ApprovalRecord>, Failure> {
    blocking(host, move |h| h.decide_approval(&id, &decision)).await
}
async fn receipts(
    State(host): State<Host>,
    Path(id): Path<String>,
) -> std::result::Result<Json<serde_json::Value>, Failure> {
    blocking(host, move |h| {
        Ok(json!({"late":h.supervisor.store.late_receipts(&id)?}))
    })
    .await
}
#[derive(Deserialize)]
struct Cursor {
    after: Option<u64>,
}
async fn events(
    State(host): State<Host>,
    Path(id): Path<String>,
    Query(query): Query<Cursor>,
    headers: HeaderMap,
) -> std::result::Result<impl IntoResponse, Failure> {
    let after = if let Some(cursor) = query.after {
        cursor
    } else if let Some(value) = headers.get("last-event-id") {
        value
            .to_str()
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| {
                failure(ServiceError::new(
                    ErrorCode::Invalid,
                    "Invalid event cursor",
                ))
            })?
    } else {
        0
    };
    if after > i64::MAX as u64 {
        return Err(failure(ServiceError::new(
            ErrorCode::Invalid,
            "Invalid event cursor",
        )));
    }
    // A live SSE connection owns capacity until it disconnects.
    let permit = host.admission.clone().try_acquire_owned().map_err(|_| {
        failure(ServiceError::new(
            ErrorCode::Unavailable,
            "Too many runtime streams",
        ))
    })?;
    let store = host.runtime.supervisor.store.clone();
    let run = id.clone();
    let initial = tokio::task::spawn_blocking(move || store.get(&run))
        .await
        .map_err(|_| {
            failure(ServiceError::new(
                ErrorCode::Unavailable,
                "Runtime unavailable",
            ))
        })?
        .map_err(failure)?
        .ok_or_else(|| failure(ServiceError::new(ErrorCode::Invalid, "Unknown run")))?;
    let stream = futures_util::stream::unfold(
        (
            host.runtime,
            id,
            after,
            false,
            permit,
            initial.state.terminal(),
        ),
        |(runtime, id, mut after, closed, permit, terminal)| async move {
            if closed {
                return None;
            }
            loop {
                let store = runtime.supervisor.store.clone();
                let run = id.clone();
                let result = tokio::task::spawn_blocking(move || {
                    let rows = store.events(&run, after, 1)?;
                    let finished = store.get(&run)?.is_none_or(|r| r.state.terminal());
                    Ok::<_, ServiceError>((rows, finished))
                })
                .await;
                match result {
                    Ok(Ok((mut rows, _))) if !rows.is_empty() => {
                        let event = rows.remove(0);
                        after = event.sequence;
                        let terminal = event.record.state.terminal();
                        let frame = Event::default()
                            .id(after.to_string())
                            .event("run")
                            .json_data(event)
                            .unwrap_or_else(|_| {
                                Event::default().event("error").data("Encoding failed")
                            });
                        return Some((
                            Ok::<_, Infallible>(frame),
                            (runtime, id, after, terminal, permit, terminal),
                        ));
                    }
                    Ok(Ok((_, true))) => return None,
                    Ok(Ok(_)) => tokio::time::sleep(Duration::from_millis(100)).await,
                    _ => {
                        return Some((
                            Ok(Event::default()
                                .event("error")
                                .data("Runtime journal unavailable")),
                            (runtime, id, after, true, permit, terminal),
                        ))
                    }
                }
            }
        },
    );
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(10))))
}
pub fn router(runtime: Arc<ExecutionHost>, token: String) -> Result<Router> {
    if token.len() < 24 || token.chars().any(char::is_control) {
        return Err(ServiceError::new(
            ErrorCode::Invalid,
            "Use a strong runtime token of at least 24 characters",
        ));
    }
    let host = Host {
        runtime,
        token: token.into(),
        admission: Arc::new(tokio::sync::Semaphore::new(64)),
    };
    Ok(Router::new()
        .route("/health", get(info))
        .route("/v1/info", get(info))
        .route("/v1/workers/catalog",get(worker_catalog))
        .route("/v1/runs/{id}/plan",get(plan).post(edit_plan))
        .route("/v1/runs/{id}/hooks/receipts",get(hook_receipts))
        .route("/v1/runs/{id}/hooks/acknowledge",post(acknowledge_hook))
        .route("/v1/runs/{id}/workspace",get(workspace))
        .route("/v1/runs/{id}/workspace/reconcile",post(reconcile_workspace))
        .route("/v1/runs/{id}/workspace/cleanup",post(cleanup_workspace))
        .route("/v1/runs/{id}/resume",post(resume))
        .route("/v1/runs", post(submit))
        .route("/v1/tools", get(tool_catalog))
        .route("/v1/sessions/{session}/tools/release", post(release_tools))
        .route(
            "/v1/permissions",
            get(permission_status).post(permission_change),
        )
        .route("/v1/approvals", get(approvals))
        .route("/v1/review-denials", get(denials))
        .route("/v1/review-overrides", post(review_override))
        .route("/v1/approvals/{id}", get(approval_status))
        .route("/v1/approvals/{id}/decision", post(approval_decision))
        .route("/v1/runs/{id}", get(status))
        .route("/v1/runs/{id}/cancel", post(cancel))
        .route("/v1/runs/{id}/steer", post(steer))
        .route("/v1/runs/{id}/receipts", get(receipts))
        .route("/v1/runs/{id}/events", get(events))
        .layer(DefaultBodyLimit::max(8 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(host.clone(), auth))
        .with_state(host))
}
