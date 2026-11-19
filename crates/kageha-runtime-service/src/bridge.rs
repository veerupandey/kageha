//! Standalone turn hosting with client-supplied model/tool ports. No UI database.
use axum::{
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use kageha_contracts::{execution::*, ErrorCode, Result, ServiceError};
use kageha_runtime::{execution::execute_turn, lifecycle::*, turn::Start};
use kageha_runtime_store::SqliteRunStore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io,
    path::Path as FsPath,
    sync::{atomic::AtomicBool, Arc, Condvar, Mutex},
    time::{Duration, Instant},
};
#[derive(Clone, Serialize, Deserialize)]
pub struct Command {
    pub id: String,
    pub kind: String,
    pub payload: Value,
}
struct Pending {
    command: Command,
    claimed: bool,
    reply: Option<Value>,
}
struct Channel {
    pending: Mutex<Vec<Pending>>,
    wake: Condvar,
    last_contact: Mutex<Instant>,
}
impl Channel {
    fn new() -> Self {
        Self {
            pending: Mutex::new(Vec::new()),
            wake: Condvar::new(),
            last_contact: Mutex::new(Instant::now()),
        }
    }
}
struct Ports {
    channel: Arc<Channel>,
    store: Arc<SqliteRunStore>,
    run: RunRecord,
    cancel: Arc<AtomicBool>,
    deadline: Option<Instant>,
}
fn failure(message: impl Into<String>) -> ServiceError {
    ServiceError::new(ErrorCode::Unavailable, message)
}
impl Ports {
    fn call(&self, kind: &str, payload: Value, control: &Control<'_>) -> io::Result<Value> {
        control.check()?;
        let id = uuid::Uuid::new_v4().to_string();
        let mut pending = self
            .channel
            .pending
            .lock()
            .map_err(|_| io::Error::other("Port queue unavailable"))?;
        if pending.len() >= 64000 {
            return Err(io::Error::other("Port command limit reached"));
        }
        pending.push(Pending {
            command: Command {
                id: id.clone(),
                kind: kind.into(),
                payload,
            },
            claimed: false,
            reply: None,
        });
        self.channel.wake.notify_all();
        loop {
            if let Err(error) = control.check() {
                if error.kind() == io::ErrorKind::TimedOut && kind == "tool"
                    && pending.iter().any(|item| {
                        item.command.id == id && item.claimed && item.reply.is_none()
                    })
                {
                    return Err(uncertain_stop(
                        "Run deadline elapsed with a tool in flight; inspect its original receipt",
                    ));
                }
                return Err(error);
            }
            if self
                .channel
                .last_contact
                .lock()
                .map_err(|_| io::Error::other("Port lease unavailable"))?
                .elapsed()
                > Duration::from_secs(15)
            {
                return Err(uncertain_stop(
                    "Execution host disconnected; inspect original receipts before retrying",
                ));
            }
            let item = pending
                .iter()
                .find(|p| p.command.id == id)
                .ok_or_else(|| io::Error::other("Port command missing"))?;
            if let Some(reply) = &item.reply {
                let reply = reply.clone();
                pending.retain(|p| p.command.id != id);
                return decode(reply);
            }
            pending = self
                .channel
                .wake
                .wait_timeout(pending, Duration::from_millis(20))
                .map_err(|_| io::Error::other("Port queue unavailable"))?
                .0;
        }
    }
}
impl ModelPort for Ports {
    fn accounts_invocations(&self) -> bool { true }
    /// The application's model context window. An application that predates the
    /// request reports none, and the runtime assumes its default.
    fn context_window(&self) -> Option<u64> {
        if self.run.spec.input["recovery_protocol"] != 1 { return None; }
        self.call("context_window", Value::Null, &self.control()).ok()?.as_u64()
    }
    fn complete(
        &self,
        request: ModelRequest,
        control: &Control<'_>,
    ) -> io::Result<AssistantMessage> {
        let message: AssistantMessage =
            serde_json::from_value(self.call("model", serde_json::to_value(request)?, control)?)?;
        // Malformed tool calls pass through to the runtime, which answers them
        // with `invalid_tool_call` without dispatching them.
        message.validate_envelope()?;
        Ok(message)
    }
}
impl ToolPort for Ports {
    fn on_compaction(&self,summary:&CompactionSummary)->io::Result<()> {
        if self.run.spec.input["recovery_protocol"] != 1 { return Ok(()); }
        self.call("on_compaction",serde_json::to_value(summary)?,&self.control()).map(|_|())
    }
    fn plan_snapshot(&self)->io::Result<Option<String>> {
        if self.run.spec.input["recovery_protocol"] != 1 { return Ok(None); }
        serde_json::from_value(self.call("plan_snapshot",Value::Null,&self.control())?).map_err(io::Error::other)
    }
    fn take_attachments(&self)->io::Result<Vec<Value>> {
        if self.run.spec.input["recovery_protocol"] != 1 { return Ok(Vec::new()); }
        serde_json::from_value(self.call("take_attachments",Value::Null,&self.control())?).map_err(io::Error::other)
    }
    fn recovery_revision(&self,call:&ToolCall)->io::Result<Option<String>> {
        if self.run.spec.input["recovery_protocol"]!=1 { return Ok(None); }
        serde_json::from_value(self.call("recovery_revision",serde_json::to_value(call)?,&self.control())?).map_err(io::Error::other)
    }
    fn persistence_policy(&self) -> io::Result<Option<PersistencePolicy>> {
        if self.run.spec.input["recovery_protocol"] != 1 { return Ok(None); }
        serde_json::from_value(self.call("persistence_policy",Value::Null,&self.control())?).map_err(io::Error::other)
    }
    fn recovery_checkpoint(&self) -> io::Result<Value> {
        if self.run.spec.input["recovery_protocol"] != 1 { return Ok(Value::Null); }
        self.call("recovery_checkpoint",Value::Null,&self.control())
    }
    fn save_recovery(&self,state:&Value) -> io::Result<()> {
        if self.run.spec.input["recovery_protocol"] != 1 { return Ok(()); }
        self.call("save_recovery",state.clone(),&self.control()).map(|_|())
    }
    fn completion_check(&self,candidate:&str,control:&Control<'_>) -> io::Result<CompletionCheck> {
        if self.run.spec.input["recovery_protocol"] != 1 { return Ok(CompletionCheck::default()); }
        serde_json::from_value(self.call("completion_check",json!({"candidate":candidate}),control)?).map_err(io::Error::other)
    }
    fn discover(&self) -> io::Result<Vec<Value>> {
        serde_json::from_value(self.call("discover", Value::Null, &self.control())?)
            .map_err(io::Error::other)
    }
    fn checkpoint(&self) -> io::Result<Vec<String>> {
        let mut steering = self
            .store
            .steering(&self.run.spec.id)
            .map_err(io::Error::other)?
            .into_iter()
            .map(|s| s.body)
            .collect::<Vec<_>>();
        steering.extend(serde_json::from_value::<Vec<String>>(self.call(
            "checkpoint",
            Value::Null,
            &self.control(),
        )?)?);
        Ok(steering)
    }
    fn dispatch_access(&self, call: &ToolCall) -> io::Result<DispatchAccess> {
        serde_json::from_value(self.call("access", serde_json::to_value(call)?, &self.control())?)
            .map_err(io::Error::other)
    }
    fn execute_batch(
        &self,
        calls: &[ToolCall],
        control: &Control<'_>,
    ) -> io::Result<Vec<ToolReply>> {
        kageha_runtime::fanout::execute_parallel(self, calls, control, 4)
    }
    fn execute(&self, call: &ToolCall, control: &Control<'_>) -> io::Result<ToolReply> {
        serde_json::from_value(self.call("tool", serde_json::to_value(call)?, control)?)
            .map_err(io::Error::other)
    }
}
// Each adapter call gets the same cancellation/deadline as its owning turn.
impl Ports {
    fn control(&self) -> Control<'_> {
        Control {
            cancelled: &self.cancel,
            deadline: self.deadline,
        }
    }
}

pub fn encode(result: io::Result<Value>) -> Value {
    match result {
        Ok(value) => json!({"ok":true,"value":value}),
        Err(error) => {
            if let Some(failure)=model_failure(&error) {
                return json!({"ok":false,"error":{"code":"model_failure","message":failure.message,"model_failure":failure}});
            }
            if kageha_contracts::request_budget::is_exceeded(&error) {
                return json!({"ok":false,"error":{"code":"budget_exceeded","message":error.to_string()}});
            }
            json!({"ok":false,"error":{"message":error.to_string(),"code":if kageha_contracts::policy::is_stop(&error)&&is_uncertain(&error){"uncertain_stop"}else if kageha_contracts::policy::is_stop(&error){"stop"}else if is_uncertain(&error){"uncertain"}else{match error.kind(){io::ErrorKind::Interrupted=>"cancelled",io::ErrorKind::TimedOut=>"deadline",io::ErrorKind::PermissionDenied=>"authorization",io::ErrorKind::ConnectionAborted|io::ErrorKind::ConnectionReset|io::ErrorKind::BrokenPipe=>"transport",_=>"error"}}}})
        }
    }
}
pub fn decode(reply: Value) -> io::Result<Value> {
    if reply["ok"] == true {
        return Ok(reply["value"].clone());
    }
    let message = reply["error"]["message"]
        .as_str()
        .unwrap_or("Remote port failed");
    Err(match reply["error"]["code"].as_str() {
        Some("model_failure") => match serde_json::from_value::<ModelFailure>(reply["error"]["model_failure"].clone()) {
            Ok(failure)=>io::Error::other(failure),
            Err(_)=>io::Error::other(ModelFailure::new(ModelFailureKind::InvalidResponse,"Invalid typed model failure from runtime port")),
        },
        Some("budget_exceeded") => io::Error::other(kageha_contracts::request_budget::BudgetExceeded::new(message)),
        Some("uncertain_stop") => uncertain_stop(message),
        Some("stop") => kageha_contracts::policy::stop(message),
        Some("uncertain") => io::Error::other(ServiceError::new(ErrorCode::Uncertain, message)),
        Some("cancelled") => io::Error::new(io::ErrorKind::Interrupted, message),
        Some("deadline") => io::Error::new(io::ErrorKind::TimedOut, message),
        Some("authorization") => io::Error::new(io::ErrorKind::PermissionDenied, message),
        Some("transport") => io::Error::new(io::ErrorKind::ConnectionAborted, message),
        _ => io::Error::other(message.to_owned()),
    })
}
#[cfg(test)]
mod reliability_tests {
    use super::*;
    #[test]
    fn port_failures_retain_typed_causes_and_legacy_payloads_decode() {
        for kind in [io::ErrorKind::PermissionDenied, io::ErrorKind::TimedOut, io::ErrorKind::Interrupted, io::ErrorKind::ConnectionAborted] {
            assert_eq!(decode(encode(Err(io::Error::new(kind, "sanitized failure")))).unwrap_err().kind(), kind);
        }
        assert!(is_uncertain(&decode(encode(Err(uncertain_stop("receipt unknown")))).unwrap_err()));
        assert_eq!(decode(json!({"ok":false,"error":{"message":"old failure","code":"error"}})).unwrap_err().to_string(), "old failure");
        let mut cause=ModelFailure::new(ModelFailureKind::InterruptedStream,"stream interrupted");
        cause.partial_tool_calls=true;
        let error=decode(encode(Err(io::Error::other(cause)))).unwrap_err();
        let typed=model_failure(&error).unwrap();
        assert_eq!(typed.kind,ModelFailureKind::InterruptedStream);
        assert!(typed.partial_tool_calls);
        assert!(kageha_contracts::request_budget::is_exceeded(&decode(encode(Err(io::Error::other(kageha_contracts::request_budget::BudgetExceeded::new("ceiling"))))).unwrap_err()));
    }
}
struct Executor {
    store: Arc<SqliteRunStore>,
    channels: Arc<Mutex<HashMap<String, Arc<Channel>>>>,
    turn_timeout: Duration,
    explicit_turn_timeout: bool,
}
impl Executor {
    fn channel(&self, id: &str) -> Result<Arc<Channel>> {
        let mut channels = self
            .channels
            .lock()
            .map_err(|_| failure("Port registry unavailable"))?;
        if let Some(channel) = channels.get(id) {
            return Ok(channel.clone());
        }
        if channels.len() >= 128 {
            channels.retain(|id, _| {
                self.store
                    .get(id)
                    .ok()
                    .flatten()
                    .is_some_and(|r| !r.state.terminal())
            });
        }
        if channels.len() >= 128 {
            return Err(ServiceError::new(
                ErrorCode::Conflict,
                "Port host capacity reached",
            ));
        }
        let channel = Arc::new(Channel::new());
        channels.insert(id.into(), channel.clone());
        Ok(channel)
    }
}
impl RunExecutor for Executor {
    fn execute(&self, run: &RunRecord, cancel: Arc<AtomicBool>) -> Result<Completion> {
        let start: Start = serde_json::from_value(run.spec.input["start"].clone())
            .map_err(|_| failure("Invalid turn input"))?;
        let digest = format!(
            "{:x}",
            <sha2::Sha256 as sha2::Digest>::digest(
                serde_json::to_vec(&run.spec).map_err(|_| failure("Invalid turn"))?
            )
        );
        if let Some(saved) =
            self.store
                .effect_begin(&run.spec.turn, "bridge:completion", &digest)?
        {
            return serde_json::from_value(saved)
                .map_err(|_| failure("Invalid completion receipt"));
        }
        let ports = Ports {
            channel: self.channel(&run.spec.id)?,
            store: self.store.clone(),
            run: run.clone(),
            cancel: cancel.clone(),
            // New clients enforce the durable active request clock. Retain
            // explicit operator wall limits and legacy-client deadlines.
            deadline: (self.explicit_turn_timeout || run.spec.input["recovery_protocol"]!=1)
                .then(||Instant::now()+self.turn_timeout),
        };
        let control = ports.control();
        let result = execute_turn(start, &ports, &ports, &control, |frame| {
            ports.call("observe", frame.clone(), &control).map(|_| ())
        });
        let completion = match result {
            Ok(message) => Completion {
                state: RunState::Succeeded,
                result: json!({"message":message}),
            },
            Err(error) => Completion {
                state: if error.kind() == io::ErrorKind::Interrupted {
                    RunState::Cancelled
                } else {
                    RunState::Failed
                },
                result: json!({"message":error.to_string(),"uncertain":is_uncertain(&error),
                    "deadline_exceeded":error.kind() == io::ErrorKind::TimedOut,
                    "failure_code":match error.kind() {
                        io::ErrorKind::TimedOut => "deadline_exceeded",
                        io::ErrorKind::PermissionDenied => "authorization_required",
                        io::ErrorKind::Interrupted => "cancelled",
                        io::ErrorKind::ConnectionAborted | io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe => "runtime_disconnected",
                        _ => "execution_failed",
                    }}),
            },
        };
        self.store.effect_finish(
            &run.spec.turn,
            "bridge:completion",
            &digest,
            serde_json::to_value(&completion).map_err(|_| failure("Invalid completion"))?,
        )?;
        if let Ok(mut pending) = ports.channel.pending.lock() {
            pending.clear();
        }
        Ok(completion)
    }
    fn inspect(&self, run: &RunRecord) -> Result<Option<Completion>> {
        self.store
            .effect_get(&run.spec.turn, "bridge:completion")?
            .and_then(|e| e.receipt)
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| failure("Invalid completion receipt"))
    }
    fn publish(&self, _: &RunRecord) -> Result<()> {
        Ok(())
    }
}
#[derive(Clone)]
struct App {
    executor: Arc<Executor>,
    supervisor: Arc<Supervisor>,
    token: Arc<String>,
}
fn authorized(headers: &HeaderMap, app: &App) -> bool {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|s| {
            let expected = format!("Bearer {}", app.token);
            s.len() == expected.len()
                && s.bytes()
                    .zip(expected.bytes())
                    .fold(0u8, |a, (b, c)| a | (b ^ c))
                    == 0
        })
}
type Reply = std::result::Result<Json<Value>, (StatusCode, Json<Value>)>;
fn api(error: impl std::fmt::Display) -> (StatusCode, Json<Value>) {
    (
        StatusCode::CONFLICT,
        Json(json!({"error":error.to_string()})),
    )
}
fn auth(headers: &HeaderMap, app: &App) -> std::result::Result<(), (StatusCode, Json<Value>)> {
    if authorized(headers, app) {
        Ok(())
    } else {
        Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Unauthorized"})),
        ))
    }
}
fn owned(
    app: &App,
    headers: &HeaderMap,
    id: &str,
) -> std::result::Result<RunRecord, (StatusCode, Json<Value>)> {
    auth(headers, app)?;
    let run = app
        .executor
        .store
        .get(id)
        .map_err(api)?
        .ok_or_else(|| api("Turn not found"))?;
    if headers
        .get("x-kageha-port-owner")
        .and_then(|h| h.to_str().ok())
        != run.spec.input["owner"].as_str()
    {
        return Err(api("Port host scope changed"));
    }
    Ok(run)
}
async fn info(State(app): State<App>, headers: HeaderMap) -> Reply {
    auth(&headers, &app)?;
    Ok(Json(
        json!({"runtime":"rust","mode":"standalone-client-ports","protocol_version":1,
            "pid":std::process::id(),"turn_timeout_seconds":app.executor.turn_timeout.as_secs()}),
    ))
}
async fn submit(State(app): State<App>, headers: HeaderMap, Json(spec): Json<RunSpec>) -> Reply {
    auth(&headers, &app)?;
    spec.validate().map_err(api)?;
    if [&spec.id, &spec.turn].iter().any(|id| {
        !id.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b':'))
    }) {
        return Err(api("Turn identity is not path-safe"));
    }
    if spec
        .input
        .as_object()
        .is_none_or(|v| !v.contains_key("start") || !v.contains_key("owner")
            || v.keys().any(|key|!matches!(key.as_str(),"start"|"owner"|"recovery_protocol"))
            || v.get("recovery_protocol").is_some_and(|version|version!=1))
        || spec.input["owner"]
            .as_str()
            .is_none_or(|s| s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(api("Invalid client-port turn input"));
    }
    let start: Start = serde_json::from_value(spec.input["start"].clone()).map_err(api)?;
    if start.version != 1
        || !(1..=1000).contains(&start.max_model_calls)
        || start.messages.is_empty()
    {
        return Err(api("Invalid turn budget"));
    }
    if headers
        .get("x-kageha-port-owner")
        .and_then(|h| h.to_str().ok())
        != spec.input["owner"].as_str()
    {
        return Err(api("Port host identity missing"));
    }
    app.executor.channel(&spec.id).map_err(api)?;
    let run = app.supervisor.stage(&spec).map_err(api)?;
    let run = if run.state == RunState::Staged {
        app.supervisor.activate(&spec.id).map_err(api)?
    } else {
        run
    };
    Ok(Json(serde_json::to_value(run).map_err(api)?))
}
async fn status(State(app): State<App>, headers: HeaderMap, Path(id): Path<String>) -> Reply {
    let run = owned(&app, &headers, &id)?;
    Ok(Json(serde_json::to_value(run).map_err(api)?))
}
async fn cancel(State(app): State<App>, headers: HeaderMap, Path(id): Path<String>) -> Reply {
    owned(&app, &headers, &id)?;
    Ok(Json(
        serde_json::to_value(app.supervisor.cancel(&id).map_err(api)?).map_err(api)?,
    ))
}
async fn commands(State(app): State<App>, headers: HeaderMap, Path(id): Path<String>) -> Reply {
    let run = owned(&app, &headers, &id)?;
    if run.state.terminal() {
        return Ok(Json(json!({"commands":[],"run":run})));
    }
    let channel = app.executor.channel(&id).map_err(api)?;
    let commands = tokio::task::spawn_blocking(move || -> Result<Vec<Command>> {
        *channel
            .last_contact
            .lock()
            .map_err(|_| failure("Port lease unavailable"))? = Instant::now();
        let mut pending = channel
            .pending
            .lock()
            .map_err(|_| failure("Port queue unavailable"))?;
        if !pending.iter().any(|p| !p.claimed) {
            pending = channel
                .wake
                .wait_timeout(pending, Duration::from_millis(250))
                .map_err(|_| failure("Port queue unavailable"))?
                .0;
        }
        Ok(pending
            .iter()
            .filter(|p| !p.claimed)
            .take(16)
            .map(|p| p.command.clone())
            .collect())
    })
    .await
    .map_err(api)?
    .map_err(api)?;
    Ok(Json(
        json!({"commands":commands,"run":app.executor.store.get(&id).map_err(api)?.map(|r|json!({"state":r.state,"completion":r.completion}))}),
    ))
}
async fn claim(
    State(app): State<App>,
    headers: HeaderMap,
    Path((id, command_id)): Path<(String, String)>,
) -> Reply {
    let run = owned(&app, &headers, &id)?;
    if run.state != RunState::Running {
        return Err(api("Turn is not running"));
    }
    let channel = app.executor.channel(&id).map_err(api)?;
    let mut pending = channel
        .pending
        .lock()
        .map_err(|_| api("Port queue unavailable"))?;
    let item = pending
        .iter_mut()
        .find(|p| p.command.id == command_id)
        .ok_or_else(|| api("Command not found"))?;
    if item.claimed {
        return Ok(Json(json!({"claimed":false})));
    }
    let digest = format!(
        "{:x}",
        <sha2::Sha256 as sha2::Digest>::digest(serde_json::to_vec(&item.command).map_err(api)?)
    );
    app.executor
        .store
        .effect_begin(&run.spec.turn, &format!("port:{command_id}"), &digest)
        .map_err(api)?;
    item.claimed = true;
    item.command.payload = Value::Null;
    Ok(Json(json!({"claimed":true})))
}
async fn reply(
    State(app): State<App>,
    headers: HeaderMap,
    Path((id, command_id)): Path<(String, String)>,
    Json(value): Json<Value>,
) -> Reply {
    let run = owned(&app, &headers, &id)?;
    let effect = app
        .executor
        .store
        .effect_get(&run.spec.turn, &format!("port:{command_id}"))
        .map_err(api)?
        .ok_or_else(|| api("Unclaimed port command"))?;
    if let Some(saved) = effect.receipt {
        return if saved == value {
            Ok(Json(json!({"accepted":true})))
        } else {
            Err(api("Port response changed"))
        };
    }
    // Late replies remain journaled under their original scope; never reactivate a turn.
    app.executor
        .store
        .effect_finish(
            &run.spec.turn,
            &format!("port:{command_id}"),
            &effect.digest,
            value.clone(),
        )
        .map_err(api)?;
    let channel = app.executor.channel(&id).map_err(api)?;
    let mut pending = channel
        .pending
        .lock()
        .map_err(|_| api("Port queue unavailable"))?;
    if let Some(item) = pending.iter_mut().find(|p| p.command.id == command_id) {
        item.reply = Some(value);
        channel.wake.notify_all();
    }
    Ok(Json(json!({"accepted":true})))
}
/// Start an independently deployable runtime. UI/framework clients supply ports;
/// the daemon owns turn budgets, tool ordering, receipts, cancellation and recovery.
fn turn_timeout(value: Option<&str>) -> io::Result<Duration> {
    let seconds = match value {
        None => 7_200,
        Some(value) => value.parse::<u64>().ok()
            .filter(|seconds| (1..=43_200).contains(seconds))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput,
                "KAGEHA_BRIDGE_TURN_TIMEOUT_SECONDS must be between 1 and 43200"))?,
    };
    Ok(Duration::from_secs(seconds))
}

pub async fn serve(
    state: &FsPath,
    bind: &str,
    token: String,
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    if token.len() < 32 || token.chars().any(char::is_whitespace) {
        return Err("Invalid runtime service token".into());
    }
    let configured_timeout = match std::env::var("KAGEHA_BRIDGE_TURN_TIMEOUT_SECONDS") {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(error) => return Err(error.into()),
    };
    let turn_timeout = turn_timeout(configured_timeout.as_deref())?;
    let store = Arc::new(SqliteRunStore::open(&state.join("turns.sqlite"))?);
    let executor = Arc::new(Executor {
        store: store.clone(),
        channels: Arc::new(Mutex::new(HashMap::new())),
        turn_timeout,
        explicit_turn_timeout: configured_timeout.is_some(),
    });
    let supervisor = Supervisor::new(store, executor.clone(), Limits::default())?;
    supervisor.recover()?;
    supervisor.start()?;
    let app = App {
        executor,
        supervisor: supervisor.clone(),
        token: Arc::new(token),
    };
    let router = Router::new()
        .route("/v1/info", get(info))
        .route("/v1/runs", post(submit))
        .route("/v1/runs/{id}", get(status))
        .route("/v1/runs/{id}/cancel", post(cancel))
        .route("/v1/runs/{id}/commands", get(commands))
        .route("/v1/runs/{id}/commands/{command}/claim", post(claim))
        .route("/v1/runs/{id}/commands/{command}/reply", post(reply))
        .layer(DefaultBodyLimit::max(8 * 1024 * 1024))
        .with_state(app);
    let listener = tokio::net::TcpListener::bind(bind).await?;
    println!("runtime bridge listening on {}", listener.local_addr()?);
    axum::serve(listener, router)
        .with_graceful_shutdown(async {
            #[cfg(unix)]
            {
                let mut term =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .unwrap();
                tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}}
            }
            #[cfg(not(unix))]
            {
                let _ = tokio::signal::ctrl_c().await;
            }
        })
        .await?;
    supervisor.shutdown()?;
    tokio::task::spawn_blocking(move || supervisor.join(Duration::from_secs(5))).await??;
    Ok(())
}
