#[cfg(not(unix))]
compile_error!("The isolated browser daemon currently supports Unix only");

mod action_journal;
mod journal_store;
mod numeric;
mod confirmation;
mod actions;
mod application_journal;
mod batch;
mod browser;
mod cdp;
mod credentials;
mod decisions;
mod dialog;
mod download;
mod http_api;
mod input_evidence;
mod keyboard;
mod mcp;
mod observe;
mod panel;
mod protocol;
mod research;
mod script;
mod script_artifact;
mod script_sandbox;
mod state;
mod upload;
mod workstation_transfer;
mod watchdog;

use application_journal::ActionBackend;
use protocol::{limit, Reply, Request};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{watch, Semaphore};
use tokio::task::JoinSet;

async fn read_message(
    reader: &mut (impl tokio::io::AsyncBufRead + Unpin),
) -> io::Result<Option<Vec<u8>>> {
    read_message_limit(reader, limit("message_bytes")).await
}

async fn read_message_limit(
    reader: &mut (impl tokio::io::AsyncBufRead + Unpin),
    maximum: usize,
) -> io::Result<Option<Vec<u8>>> {
    let mut message = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if message.is_empty() {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "truncated_message",
                ))
            };
        }
        let end = available.iter().position(|b| *b == b'\n');
        let size = end.map(|i| i + 1).unwrap_or(available.len());
        if message.len() + size > maximum {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "message_too_large",
            ));
        }
        message.extend_from_slice(&available[..size]);
        reader.consume(size);
        if end.is_some() {
            return Ok(Some(message));
        }
    }
}

fn constant_time_equal(left: &str, right: &str) -> bool {
    left.len() == right.len()
        && left
            .bytes()
            .zip(right.bytes())
            .fold(0, |diff, (a, b)| diff | (a ^ b))
            == 0
}

struct Service {
    state: state::State,
    seen: Mutex<HashSet<(String, String, String)>>,
    active_reads: Mutex<HashSet<(String, String, String)>>,
    shutdown: watch::Sender<bool>,
    browsers: Option<browser::Browsers>,
    canary_workspace: Option<String>,
    decisions: Option<Arc<decisions::Runtime>>,
}

struct ActiveRead<'a> {
    reads: &'a Mutex<HashSet<(String, String, String)>>,
    key: (String, String, String),
}

impl Drop for ActiveRead<'_> {
    fn drop(&mut self) {
        self.reads.lock().unwrap().remove(&self.key);
    }
}

impl Service {
    fn reply(&self, request: Option<&Request>) -> Reply {
        Reply::new(
            request,
            &self.state.instance_id,
            &self.state.application_id,
            &self.state.epoch,
        )
    }

    async fn handle(
        &self,
        request: &Request,
        authenticated: &mut Option<(String, String)>,
    ) -> (Reply, bool) {
        let reply = self.reply(Some(request));
        if let Err(code) = request.validate() {
            return (reply.reject(code), false);
        }
        if request.instance_id != self.state.instance_id
            || request.application_id != self.state.application_id
            || request.state_root != self.state.root.to_string_lossy()
        {
            return (reply.reject("instance_mismatch"), false);
        }
        if self
            .canary_workspace
            .as_ref()
            .is_some_and(|workspace| workspace != &request.workspace_id)
        {
            return (reply.reject("browser_workspace_mismatch"), false);
        }
        if request.method == "handshake" {
            if authenticated.is_some() {
                return (reply.reject("already_authenticated"), false);
            }
            let params = request.params.as_object().unwrap();
            if params.len() != 1
                || !constant_time_equal(
                    params.get("token").and_then(Value::as_str).unwrap_or(""),
                    &self.state.token,
                )
            {
                return (reply.reject("authentication_failed"), false);
            }
            if request.service_epoch.is_some() {
                return (reply.reject("invalid_request"), false);
            }
        } else {
            if authenticated.as_ref()
                != Some(&(request.client_id.clone(), request.workspace_id.clone()))
            {
                return (reply.reject("authentication_required"), false);
            }
            if request.service_epoch.as_deref() != Some(self.state.epoch.as_str()) {
                return (reply.reject("stale_service_epoch"), false);
            }
        }
        let _read = if matches!(request.method.as_str(),"read_page"|"submission_context"|"confirm_submission") {
            let mut reads = self.active_reads.lock().unwrap();
            let key = (request.client_id.clone(), request.workspace_id.clone(), request.request_id.clone());
            if reads.contains(&key) {
                return (reply.reject("duplicate_request"), false);
            }
            if reads.len() >= limit("remembered_requests") {
                return (reply.reject("request_capacity_exceeded"), false);
            }
            reads.insert(key.clone());
            Some(ActiveRead { reads: &self.active_reads, key })
        } else {
            None
        };
        let journaled_action = actions::DISPATCH_METHODS.contains(&request.method.as_str())
            && self.browsers.as_ref().is_some_and(|browser| matches!(
                browser.action_backend, ActionBackend::Application | ActionBackend::Standalone
            ));
        if !journaled_action && !matches!(
            request.method.as_str(),
            "handshake"
                | "health"
                | "cancel"
                | "shutdown"
                | "browser_selection"
                | "browser_adapters"
                | "list_tabs"
                | "list_user_tabs"
                | "observe_tab"
                | "query_tab"
                | "submission_context"
                | "confirm_submission"
                | "preview_tab"
                | "capture_tab"
                | "action_receipt"
                | "pending_actions"
                | "read_page"
                | "batch_status"
                | "browser_decide"
                | "browser_decision_info"
                | "credential_vault_status"
        ) {
            let mut seen = self.seen.lock().unwrap();
            let key = (
                request.client_id.clone(),
                request.workspace_id.clone(),
                request.request_id.clone(),
            );
            if seen.contains(&key) {
                return (reply.reject("duplicate_request"), false);
            }
            if seen.len() >= limit("remembered_requests") {
                return (reply.reject("request_capacity_exceeded"), false);
            }
            seen.insert(key);
        }
        match request.method.as_str() {
            "credential_vault_status" | "credential_vault_authorize" => {
                if request.params != json!({}) { return (reply.reject("invalid_params"), false); }
                // The host exposes authorization only through an authenticated,
                // user-initiated settings action. Agent and script clients may
                // never trigger a macOS Keychain prompt.
                if request.method == "credential_vault_authorize"
                    && (request.workspace_id != "settings" || request.client_id != "native-user-settings") {
                    return (reply.reject("credential_authorization_requires_user"), false);
                }
                let authorization = request.method == "credential_vault_authorize";
                let result = tokio::task::spawn_blocking(move || if authorization {
                    credentials::credential_vault_authorize()
                } else {
                    credentials::credential_vault_status()
                }).await;
                match result {
                    Ok(Ok(result)) => { let mut reply = reply; reply.result = result; (reply, false) }
                    Ok(Err(code)) => (reply.reject(code), false),
                    Err(_) => (reply.reject("credential_store_unavailable"), false),
                }
            }
            "browser_decision_info" => {
                let mut reply=reply;
                reply.result=self.decisions.as_ref().map(|runtime|runtime.info(request)).unwrap_or(json!({"version":1,"configured":false,"owner":"host","model_execution_has_tools":false}));
                (reply,false)
            }
            "browser_decide" => {
                let (Some(runtime),Some(browsers))=(&self.decisions,&self.browsers)else{return (reply.reject("browser_decisions_not_configured"),false);};
                match runtime.decide(request,browsers).await {Ok(result)=>{let mut reply=reply;reply.result=result;(reply,false)},Err(code)=>(reply.reject(code),false)}
            }
            "handshake" => {
                *authenticated = Some((request.client_id.clone(), request.workspace_id.clone()));
                let mut reply = reply;
                let mut methods = vec!["handshake", "health", "cancel", "shutdown"];
                methods.extend_from_slice(&["credential_vault_status", "credential_vault_authorize"]);
                methods.push("browser_decision_info");
                if self.decisions.is_some(){methods.push("browser_decide");}
                if let Some(browsers) = &self.browsers {
                    methods.extend_from_slice(browser::METHODS);
                    if browsers.action_backend != ActionBackend::Disabled {
                        methods.extend_from_slice(actions::METHODS);
                    }
                    if matches!(browsers.action_backend,ActionBackend::Standalone|ActionBackend::Fixture) {
                        methods.extend_from_slice(action_journal::CONTROL_METHODS);
                    }
                    if browsers.scripts_available() {
                        methods.push("run_script");
                    }
                }
                reply.result = json!({"state_root": self.state.root, "browser_execution": false,
                    "methods": methods, "limits": {
                        "message_bytes": limit("message_bytes"), "connections": limit("connections") }});
                if let Some(backend) = self.browsers.as_ref().and_then(|b| b.action_backend.name())
                {
                    reply.result["action_backend"] = json!(backend);
                }
                (reply, false)
            }
            "health" | "shutdown" => {
                if request.params != json!({}) {
                    return (reply.reject("invalid_params"), false);
                }
                let mut reply = reply;
                reply.result = json!({"ready": true, "browser_execution": false,
                    "research_http_transport": self.browsers.as_ref().and_then(browser::Browsers::research_http_transport),
                    "shutting_down": request.method == "shutdown"});
                (reply, request.method == "shutdown")
            }
            "cancel" => {
                if request.params.as_object().unwrap().len() != 1
                    || !request
                        .params
                        .get("request_id")
                        .and_then(Value::as_str)
                        .is_some_and(|id| !id.is_empty() && id.len() <= limit("id_bytes"))
                {
                    return (reply.reject("invalid_params"), false);
                }
                let id = request.params["request_id"].as_str().unwrap();
                if self
                    .decisions.as_ref().is_some_and(|runtime|runtime.cancel(request,id)) || self
                    .browsers
                    .as_ref()
                    .is_some_and(|b| b.scripts.cancel(request, id))
                {
                    let mut reply = reply;
                    reply.result = json!({"cancellation_requested": true});
                    (reply, false)
                } else {
                    (reply.reject("cancellation_target_unknown"), false)
                }
            }
            "preview_tab" | "capture_tab" => {
                let Some(browsers) = &self.browsers else {
                    return (reply.reject("browser_driver_unavailable"), false);
                };
                match browsers.preview(request).await {
                    Ok((result, binary)) => {
                        let mut reply = reply;
                        reply.result = result;
                        reply.binary = binary;
                        (reply, false)
                    }
                    Err(code) => (reply.reject(code), false),
                }
            }
            method
                if browser::METHODS.contains(&method)
                    || actions::METHODS.contains(&method)
                    || method == "run_script"
                    || action_journal::CONTROL_METHODS.contains(&method) =>
            {
                let Some(browsers) = &self.browsers else {
                    return (reply.reject("browser_driver_unavailable"), false);
                };
                if let Some(runtime)=&self.decisions {if let Err(code)=runtime.review(request,browsers).await{return (reply.reject(code),false);}}
                match browsers.dispatch(request).await {
                    Ok(result) => {
                        let mut reply = reply;
                        reply.result = result;
                        if matches!(request.method.as_str(), "panel_action" | "run_script")
                            || actions::DISPATCH_METHODS.contains(&request.method.as_str())
                        {
                            reply.execution_phase = if (request.method == "batch_observed"
                                && reply.result["status"] == "stopped")
                                || (request.method == "run_script"
                                    && reply.result["status"] != "completed")
                            {
                                "partial"
                            } else {
                                "verified"
                            }
                            .into();
                        }
                        // Retained public text travels only to its host, which
                        // publishes a small window to the model. Other JSON
                        // replies and all requests retain the normal IPC cap.
                        let response_limit = if request.method == "read_page" && request.params["retain_text"] == true {
                            limit("frame_bytes")
                        } else { limit("message_bytes") };
                        if serde_json::to_vec(&reply)
                            .is_ok_and(|bytes| bytes.len() < response_limit)
                        {
                            (reply, false)
                        } else {
                            reply.result = json!({});
                            (reply.reject("observation_too_large"), false)
                        }
                    }
                    Err(code) => {
                        let mut reply = reply.reject(code);
                        if code == "action_outcome_uncertain" {
                            reply.execution_phase = "uncertain".into();
                            if request.method == "close_tab" {
                                reply.error.as_mut().unwrap()["message"] = json!("Tab closure could not be confirmed. List managed tabs and check the exact target. Do not repeat the close automatically or open a replacement.");
                            }
                        }
                        (reply, false)
                    }
                }
            }
            _ => (reply.reject("unsupported_method"), false),
        }
    }
}

async fn connection(
    stream: UnixStream,
    service: Arc<Service>,
    mut stop: watch::Receiver<bool>,
) -> io::Result<()> {
    if stream.peer_cred()?.uid() != state::uid() {
        return Ok(());
    }
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut authenticated = None;
    for _ in 0..limit("requests_per_connection") {
        let read = tokio::select! {
            _ = stop.changed() => break,
            value = tokio::time::timeout(Duration::from_millis(limit("idle_ms") as u64), read_message(&mut reader)) => value,
        };
        let (reply, shutdown, close) = match read {
            Ok(Ok(Some(bytes))) => match serde_json::from_slice::<Request>(&bytes) {
                Ok(request) => {
                    let (reply, shutdown) = service.handle(&request, &mut authenticated).await;
                    (reply, shutdown, false)
                }
                Err(_) => (service.reply(None).reject("invalid_request"), false, true),
            },
            Ok(Ok(None)) => break,
            Ok(Err(error)) => (service.reply(None).reject(&error.to_string()), false, true),
            Err(_) => (
                service.reply(None).reject("connection_timeout"),
                false,
                true,
            ),
        };
        let mut bytes = serde_json::to_vec(&reply)?;
        bytes.push(b'\n');
        tokio::time::timeout(Duration::from_secs(1), async {
            write.write_all(&bytes).await?;
            write.write_all(&reply.binary).await
        })
        .await??;
        if shutdown {
            let _ = service.shutdown.send(true);
            break;
        }
        if close {
            break;
        }
    }
    Ok(())
}

async fn serve(
    state: state::State,
    chromium: Option<PathBuf>,
    action_backend: ActionBackend,
    adapters: browser::Adapters,
    canary_workspace: Option<String>,
    initial_profile: &str,
    http_config: Option<http_api::Config>,
    mcp_scope: Option<(String, String)>,
    decision_config: Option<&str>,
) -> io::Result<()> {
    let decisions=decision_config.map(|path|decisions::Runtime::load(std::path::Path::new(path),&state.root).map(Arc::new)).transpose()?;
    let managed = canary_workspace
        .as_ref()
        .map(|_| {
            state::managed_profile(&state.root, initial_profile)
                .map(|profile| (state.epoch.clone(), profile))
        })
        .transpose()?;
    if matches!(action_backend, ActionBackend::Standalone | ActionBackend::Fixture) {
        if let Err(code) = action_journal::Journal::lock(&state.root) {
            eprintln!("browser input unavailable cause={code}");
        }
    }
    let listener = UnixListener::bind(&state.socket)?;
    state.publish()?;
    {
        let root = state.root.clone();
        // Scanning the state root touches many files; keep it off the request path.
        tokio::task::spawn_blocking(move || {
            let Some(processes) = state::running_commands() else {
                return;
            };
            let removed = state::sweep_stale_profiles(&root, &processes, std::time::SystemTime::now());
            if removed > 0 {
                eprintln!("removed {removed} stale browser profile director{}", if removed == 1 { "y" } else { "ies" });
            }
        });
    }
    let (shutdown, mut stop) = watch::channel(false);
    let service = Arc::new(Service {
        browsers: (chromium.is_some() || action_backend == ActionBackend::Standalone).then(|| {
            browser::Browsers::new(
                chromium,
                state.root.clone(),
                action_backend,
                adapters,
                managed,
            )
        }),
        state,
        seen: Mutex::new(HashSet::new()),
        active_reads: Mutex::new(HashSet::new()),
        shutdown,
        canary_workspace,
        decisions,
    });
    let permits = Arc::new(Semaphore::new(limit("connections")));
    let mut tasks = JoinSet::new();
    if let Some((workspace, client)) = mcp_scope {
        let service = service.clone();
        tasks.spawn(async move {
            if let Err(error) = mcp::serve(service.clone(), workspace, client).await {
                eprintln!("kageha-browser MCP: {error}");
            }
            let _ = service.shutdown.send(true);
        });
    }
    if let Some(config) = http_config {
        let (http_listener, router) = http_api::bind(config, service.clone()).await?;
        let mut http_stop = stop.clone();
        tasks.spawn(async move {
            if let Err(error) = axum::serve(http_listener, router)
                .with_graceful_shutdown(async move {
                    let _ = http_stop.changed().await;
                })
                .await
            {
                eprintln!("kageha-browser HTTP: {error}");
            }
        });
    }
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let result = loop {
        tokio::select! {
            _ = stop.changed() => break Ok(()),
            _ = terminate.recv() => break Ok(()),
            _ = tokio::signal::ctrl_c() => break Ok(()),
            Some(_) = tasks.join_next(), if !tasks.is_empty() => (),
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _)) => {
                        if let Ok(permit) = permits.clone().try_acquire_owned() {
                            let service = service.clone();
                            let stop = stop.clone();
                            tasks.spawn(async move { let _permit = permit; let _ = connection(stream, service, stop).await; });
                        }
                        // A saturated instance drops new clients immediately;
                        // it does not create an unbounded queue of sockets.
                    }
                    Err(error) => break Err(error),
                }
            }
        }
    };
    let _ = service.shutdown.send(true);
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    if let Some(browsers) = &service.browsers {
        browsers.shutdown().await;
    }
    drop(listener);
    result
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--script-worker") {
        std::process::exit(script::worker());
    }
    run();
}

#[tokio::main]
async fn run() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let [mode, executable, root] = args.as_slice() {
        if mode == "--watch-owned-chromium" {
            let result =
                watchdog::watch(std::path::Path::new(executable), std::path::Path::new(root)).await;
            // Tokio stdin is backed by a blocking read. All owned resources
            // have been dropped by watch before this worker exits.
            std::process::exit(if result.is_ok() { 0 } else { 1 });
        }
    }
    if args == ["--schema"] {
        println!("{}", protocol::SCHEMA);
        return;
    }
    if args == ["--help"] {
        println!("kageha-browser --state-dir ABSOLUTE_PRIVATE_DIR --instance-id ID --application-id ID [--chromium ABSOLUTE_EXECUTABLE] [--lightpanda ABSOLUTE_EXECUTABLE] [--curl ABSOLUTE_EXECUTABLE] [--adapter-config ABSOLUTE_JSON] [--standalone on | --fixture-actions on | --application-bridge on]\nStandalone HTTP: --http-listen IP:PORT --http-token-file ABSOLUTE_PRIVATE_FILE\nStandalone MCP: --mcp on --mcp-workspace ID --mcp-client ID\nUse --canary-workspace ID with application mode to restrict the daemon to one workspace and private Chromium. --schema prints the contract.");
        return;
    }
    let mut root = None;
    let mut instance = None;
    let mut application = None;
    let mut chromium = None;
    let mut lightpanda = None;
    let mut curl = None;
    let mut adapter_config = None;
    let mut fixture_actions = None;
    let mut application_bridge = None;
    let mut standalone = None;
    let mut canary_workspace = None;
    let mut initial_profile = None;
    let mut http_listen = None;
    let mut http_token_file = None;
    let mut mcp_mode = None;
    let mut mcp_workspace = None;
    let mut mcp_client = None;
    let mut decision_config = None;
    for pair in args.chunks(2) {
        if pair.len() != 2 {
            eprintln!("expected explicit state, instance and application flags");
            std::process::exit(2);
        }
        let slot = match pair[0].as_str() {
            "--state-dir" => &mut root,
            "--instance-id" => &mut instance,
            "--application-id" => &mut application,
            "--chromium" => &mut chromium,
            "--lightpanda" => &mut lightpanda,
            "--curl" => &mut curl,
            "--adapter-config" => &mut adapter_config,
            "--fixture-actions" => &mut fixture_actions,
            "--application-bridge" => &mut application_bridge,
            "--standalone" => &mut standalone,
            "--canary-workspace" => &mut canary_workspace,
            "--initial-profile" => &mut initial_profile,
            "--http-listen" => &mut http_listen,
            "--http-token-file" => &mut http_token_file,
            "--mcp" => &mut mcp_mode,
            "--mcp-workspace" => &mut mcp_workspace,
            "--mcp-client" => &mut mcp_client,
            "--decision-config" => &mut decision_config,
            _ => {
                eprintln!("unsupported argument");
                std::process::exit(2);
            }
        };
        if slot.replace(pair[1].clone()).is_some() {
            eprintln!("duplicate argument");
            std::process::exit(2);
        }
    }
    let chromium = chromium.map(PathBuf::from);
    let lightpanda = lightpanda.map(PathBuf::from);
    let curl = curl.map(PathBuf::from);
    for (flag, path) in [
        ("--chromium", &chromium),
        ("--lightpanda", &lightpanda),
        ("--curl", &curl),
    ] {
        if path
            .as_ref()
            .is_some_and(|path| !path.is_absolute() || !path.is_file())
        {
            eprintln!("{flag} requires an absolute executable path");
            std::process::exit(2);
        }
    }
    let hosted = match adapter_config
        .as_deref()
        .map(browser::HostedAdapters::load)
        .transpose()
    {
        Ok(config) => config,
        Err(error) => {
            eprintln!("--adapter-config: {error}");
            std::process::exit(2);
        }
    };
    let adapters = browser::Adapters {
        lightpanda,
        curl,
        hosted,
    };
    let action_backend = match (
        fixture_actions.as_deref(),
        application_bridge.as_deref(),
        standalone.as_deref(),
    ) {
        (None, None, None) => ActionBackend::Disabled,
        (Some("on"), None, None) if chromium.is_some() => ActionBackend::Fixture,
        (None, Some("on"), None) if chromium.is_some() => ActionBackend::Application,
        (None, None, Some("on")) => ActionBackend::Standalone,
        _ => {
            eprintln!("select one action backend with --chromium");
            std::process::exit(2);
        }
    };
    if canary_workspace.as_ref().is_some_and(|workspace| {
        action_backend != ActionBackend::Application
            || adapters.hosted.is_some()
            || adapters.lightpanda.is_some()
            || workspace.is_empty()
            || workspace.len() > limit("id_bytes")
            || workspace.chars().any(char::is_control)
    }) {
        eprintln!("--canary-workspace requires application mode without hosted adapters");
        std::process::exit(2);
    }
    if initial_profile.as_deref().is_some_and(|profile| {
        canary_workspace.is_none() || !matches!(profile, "private" | "chrome" | "comet")
    }) {
        eprintln!("--initial-profile requires --canary-workspace and private, chrome, or comet");
        std::process::exit(2);
    }
    let http_config = match (http_listen.as_deref(), http_token_file.as_deref()) {
        (None, None) => None,
        (Some(address), Some(file))
            if action_backend == ActionBackend::Standalone && canary_workspace.is_none() =>
        {
            match http_api::Config::load(address, std::path::Path::new(file)) {
                Ok(config) => Some(config),
                Err(error) => {
                    eprintln!("kageha-browser HTTP: {error}");
                    std::process::exit(2);
                }
            }
        }
        _ => {
            eprintln!("HTTP requires --standalone on, --http-listen and --http-token-file");
            std::process::exit(2);
        }
    };
    let mcp_scope = match (mcp_mode.as_deref(), mcp_workspace, mcp_client) {
        (None, None, None) => None,
        (Some("on"), Some(workspace), Some(client))
            if action_backend == ActionBackend::Standalone
                && [&workspace, &client].iter().all(|id| {
                    !id.is_empty()
                        && id.len() <= limit("id_bytes")
                        && !id.chars().any(char::is_control)
                }) =>
        {
            Some((workspace, client))
        }
        _ => {
            eprintln!(
                "MCP requires --standalone on, --mcp on, --mcp-workspace ID and --mcp-client ID"
            );
            std::process::exit(2);
        }
    };
    let mcp_enabled = mcp_scope.is_some();
    let result = match (root, instance, application) {
        (Some(root), Some(instance), Some(application)) => {
            match state::State::open(&PathBuf::from(root), instance, application) {
                Ok(state) => {
                    let backend = if canary_workspace.is_some() {
                        Some("private-canary")
                    } else {
                        action_backend.name()
                    };
                    if let Some(backend) = backend {
                        if let Err(error) = state.claim_action_backend(backend) {
                            eprintln!("kageha-browser: {error}");
                            std::process::exit(1);
                        }
                    }
                    serve(
                        state,
                        chromium,
                        action_backend,
                        adapters,
                        canary_workspace,
                        initial_profile.as_deref().unwrap_or("private"),
                        http_config,
                        mcp_scope,
                        decision_config.as_deref(),
                    )
                    .await
                }
                Err(error) => Err(error),
            }
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "explicit --state-dir, --instance-id and --application-id are required",
        )),
    };
    if let Err(error) = result {
        eprintln!("kageha-browser: {error}");
        std::process::exit(1);
    }
    if mcp_enabled {
        // Resource cleanup completed in serve. Tokio stdin may still have a
        // blocking read pending after a signal; it must not hold process exit.
        std::process::exit(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn credential_authorization_requires_the_settings_user_client() {
        let root = PathBuf::from("/tmp").join(format!("kbr-auth-{}", uuid::Uuid::new_v4().simple()));
        let state = state::State::open(&root, "auth-test".into(), "test-app".into()).unwrap();
        let (shutdown, _) = watch::channel(false);
        let service = Service { state, seen: Mutex::new(HashSet::new()), active_reads: Mutex::new(HashSet::new()), shutdown,
            browsers: None, canary_workspace: None, decisions: None };
        let fixtures: Value = serde_json::from_str(include_str!("../protocol/fixtures.json")).unwrap();
        let mut request: Request = serde_json::from_value(fixtures["Request"][0].clone()).unwrap();
        request.instance_id = service.state.instance_id.clone();
        request.application_id = service.state.application_id.clone();
        request.state_root = service.state.root.to_string_lossy().to_string();
        request.deadline_ms = protocol::now_ms() + 10000;
        request.client_id = "native-agent-chat".into();
        request.workspace_id = "chat".into();
        request.params = json!({"token": service.state.token});
        let mut authenticated = None;
        assert_eq!(service.handle(&request, &mut authenticated).await.0.status, "ok");
        request.method = "credential_vault_authorize".into();
        request.request_id = "authorize".into();
        request.service_epoch = Some(service.state.epoch.clone());
        request.params = json!({});
        let response = service.handle(&request, &mut authenticated).await.0;
        assert_eq!(response.error.unwrap()["code"], "credential_authorization_requires_user");
        drop(service);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn polling_and_recovery_remain_available_when_mutation_capacity_is_full() {
        let root = PathBuf::from("/tmp").join(format!("kbr-{}", uuid::Uuid::new_v4().simple()));
        let state = state::State::open(&root, "quota-test".into(), "test-app".into()).unwrap();
        let (shutdown, _) = watch::channel(false);
        let service = Service {
            state,
            seen: Mutex::new(HashSet::new()),
            active_reads: Mutex::new(HashSet::new()),
            shutdown,
            browsers: None,
            canary_workspace: None,
            decisions: None,
        };
        let fixtures: Value =
            serde_json::from_str(include_str!("../protocol/fixtures.json")).unwrap();
        let mut request: Request = serde_json::from_value(fixtures["Request"][0].clone()).unwrap();
        request.instance_id = service.state.instance_id.clone();
        request.application_id = service.state.application_id.clone();
        request.state_root = service.state.root.to_string_lossy().to_string();
        request.deadline_ms = protocol::now_ms() + 10000;
        request.request_id = "handshake".into();
        request.params = json!({"token": service.state.token});
        let mut authenticated = None;
        assert_eq!(
            service.handle(&request, &mut authenticated).await.0.status,
            "ok"
        );
        request.method = "health".into();
        request.params = json!({});
        request.service_epoch = Some(service.state.epoch.clone());
        for index in 0..limit("remembered_requests") + 1 {
            request.request_id = format!("request-{index}");
            assert_eq!(
                service.handle(&request, &mut authenticated).await.0.status,
                "ok"
            );
        }
        assert!(service.seen.lock().unwrap().is_empty());
        {
            let mut seen = service.seen.lock().unwrap();
            for index in 0..limit("remembered_requests") {
                seen.insert((
                    request.client_id.clone(),
                    request.workspace_id.clone(),
                    format!("mutation-{index}"),
                ));
            }
        }
        request.request_id = "healthy-at-capacity".into();
        assert_eq!(
            service.handle(&request, &mut authenticated).await.0.status,
            "ok"
        );
        request.method = "read_page".into();
        for index in 0..limit("remembered_requests") + 1 {
            request.request_id = format!("read-{index}");
            let (reply, _) = service.handle(&request, &mut authenticated).await;
            assert_eq!(reply.error.unwrap()["code"], "browser_driver_unavailable");
        }
        assert!(service.active_reads.lock().unwrap().is_empty());
        request.method = "open_tab".into();
        request.request_id = "mutation-0".into();
        assert_eq!(
            service
                .handle(&request, &mut authenticated)
                .await
                .0
                .error
                .unwrap()["code"],
            "duplicate_request"
        );
        request.request_id = "over-capacity".into();
        let (reply, shutdown) = service.handle(&request, &mut authenticated).await;
        assert_eq!(reply.error.unwrap()["code"], "request_capacity_exceeded");
        assert_eq!(reply.execution_phase, "not_dispatched");
        assert!(!shutdown);
        assert_eq!(
            service.seen.lock().unwrap().len(),
            limit("remembered_requests")
        );
        request.method = "cancel".into();
        request.params = json!({"request_id": "unknown-action"});
        assert_eq!(
            service
                .handle(&request, &mut authenticated)
                .await
                .0
                .error
                .unwrap()["code"],
            "cancellation_target_unknown"
        );
        request.method = "shutdown".into();
        request.params = json!({});
        let (reply, shutdown) = service.handle(&request, &mut authenticated).await;
        assert_eq!(reply.status, "ok");
        assert!(shutdown);
        drop(service);
        std::fs::remove_dir_all(root).unwrap();
    }
}
