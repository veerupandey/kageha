//! Rust Kageha adapter for the independent computer runtime. No Python bridge.
use crate::{browser_client, ui_approvals, webui::Database};
use kageha_computer::client::{ClientOptions, ComputerClient};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    io,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
        Mutex, OnceLock,
    },
    time::{Duration, Instant},
};

struct Desktop {
    input_turn: Option<(String, String)>,
    client: ComputerClient,
    held: Option<Held>,
    preview: Option<(String, String, String)>,
    frames: std::collections::VecDeque<ManualFrame>,
    selected: Option<(String, Value, Value)>,
    interruption: Option<String>,
}
struct ManualFrame {
    chat: String,
    id: String,
    pid: Value,
    window: Value,
    width: f64,
    height: f64,
    at: Instant,
}
struct Held {
    chat: String,
    lease: String,
    until: Instant,
    observation: Option<Value>,
    target_in_use: bool,
}
static INPUT_OVERRIDE: AtomicU8 = AtomicU8::new(0);
static USER_CONTROL: AtomicBool = AtomicBool::new(false);
static CONTROL_REVISION: AtomicU64 = AtomicU64::new(1);
static DESKTOP: OnceLock<Mutex<Option<Desktop>>> = OnceLock::new();
fn state() -> &'static Mutex<Option<Desktop>> {
    DESKTOP.get_or_init(|| Mutex::new(None))
}
/// Helper launches and restarts so far (task 17.2). An observation taken in an earlier
/// generation is stale: the helper that issued it is gone.
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// Observation ids this host issued, with their generation. Bounded; the oldest go first.
static ISSUED: OnceLock<Mutex<std::collections::VecDeque<(String, u64)>>> = OnceLock::new();
const ISSUED_LIMIT: usize = 256;
fn issued() -> std::sync::MutexGuard<'static, std::collections::VecDeque<(String, u64)>> {
    ISSUED.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}
/// Marks every observation issued so far as stale. Runs at each helper launch and when the
/// Supervisor restarts the helper.
pub(crate) fn mark_observations_stale() {
    GENERATION.fetch_add(1, Ordering::SeqCst);
}
fn remember_observation(id: &str) {
    let mut issued = issued();
    if issued.len() == ISSUED_LIMIT {
        issued.pop_front();
    }
    issued.push_back((id.to_owned(), GENERATION.load(Ordering::SeqCst)));
}
/// `stale_observation` when `observation` came from a helper that has since restarted.
fn stale_observation(observation: &str) -> Option<io::Error> {
    let current = GENERATION.load(Ordering::SeqCst);
    let generation = issued().iter().rev().find(|(id, _)| id == observation).map(|(_, generation)| *generation)?;
    (generation < current).then(|| io::Error::other(json!({"error_code":"stale_observation","retryable":false,
        "component":"computer","observation_id":observation,"execution_phase":"not_dispatched",
        "summary":"The computer helper restarted after this observation was taken, so it is stale and no input was sent. Call computer_observe again and act on the new observation_id. For input that was in flight before the restart, use computer_receipt; do not repeat it."}).to_string()))
}
fn config() -> io::Result<(PathBuf, PathBuf)> {
    let user_home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("User home unavailable"))?;
    let binary = std::env::var_os("KAGEHA_COMPUTER_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            user_home.join(
                ".local/share/kageha/computer/Kageha Computer.app/Contents/MacOS/kageha-computer",
            )
        });
    let root = std::env::var_os("KAGEHA_COMPUTER_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| user_home.join(".kageha/runtime/computer-rust"));
    if binary.is_file() && !root.exists() && std::env::var_os("KAGEHA_COMPUTER_STATE_DIR").is_none()
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&root)?;
    }
    if !binary.is_absolute() || !binary.is_file() || !root.is_absolute() || !root.is_dir() {
        return Err(io::Error::other(
            "Computer helper and state directory must be absolute",
        ));
    }
    browser_client::private(&root)?;
    Ok((binary.canonicalize()?, root.canonicalize()?))
}
pub(crate) fn input_enabled() -> bool {
    match INPUT_OVERRIDE.load(Ordering::SeqCst) {
        1 => true,
        2 => false,
        _ => std::env::var("KAGEHA_COMPUTER_ALLOW_INPUT").as_deref() == Ok("1"),
    }
}
pub(crate) fn configured() -> bool {
    cfg!(target_os = "macos") && config().is_ok()
}
fn launch_desktop(resume: bool, selected: Option<(String, Value, Value)>, input_turn: Option<(String, String)>) -> io::Result<Desktop> {
    let (binary, root) = config()?;
    mark_observations_stale();
    let workspace = format!(
        "kageha-{:x}",
        Sha256::digest(root.to_string_lossy().as_bytes())
    );
    let client = ComputerClient::launch_with_options(
        &binary,
        &root.join("actions.sqlite"),
        &workspace,
        "rust-host",
        ClientOptions {
            allow_input: input_enabled() || input_turn.is_some(),
            resume_control: resume,
            ..Default::default()
        },
    )
    .map_err(io::Error::other)?;
    let interruption = client.info()["control"]["reason"]
        .as_str()
        .map(str::to_owned);
    if interruption.is_some() {
        USER_CONTROL.store(true, Ordering::SeqCst);
    }
    Ok(Desktop {
        input_turn,
        client,
        held: None,
        preview: None,
        frames: Default::default(),
        selected,
        interruption,
    })
}
fn with_desktop<T>(run: impl FnOnce(&mut Desktop) -> io::Result<T>) -> io::Result<T> {
    let mut slot = state()
        .lock()
        .map_err(|_| io::Error::other("Computer controller is unavailable"))?;
    if slot
        .as_ref()
        .is_none_or(|desktop| desktop.client.is_closed())
    {
        let selected = slot.as_ref().and_then(|d| d.selected.clone());
        *slot = Some(launch_desktop(false, selected, None)?);
    }
    let outcome = run(slot.as_mut().unwrap());
    if let Some(desktop) = slot.as_mut().filter(|d| d.client.is_closed()) {
        desktop.held = None;
        desktop.preview = None;
        desktop.frames.clear();
        desktop.interruption = Some("helper_disconnected".into());
        USER_CONTROL.store(true, Ordering::SeqCst);
    }
    outcome
}
/// Supervisor probe: whether the helper this host launched is down (it exited, or a failed
/// request closed it) and has not been launched again. Never waits: a controller that is
/// busy with a request has a live helper. `false` before the first launch.
pub(crate) fn helper_down() -> bool {
    let Ok(mut slot) = state().try_lock() else {
        return false;
    };
    let Some(desktop) = slot.as_mut() else {
        return false;
    };
    if desktop.client.is_closed() {
        return true;
    }
    if desktop.client.exited() {
        // The same state as a helper that disconnected during a request (`with_desktop`).
        desktop.held = None;
        desktop.preview = None;
        desktop.frames.clear();
        desktop.interruption = Some("helper_disconnected".into());
        USER_CONTROL.store(true, Ordering::SeqCst);
        return true;
    }
    false
}
/// Supervisor restart: launches the helper again when it is down, keeping the selected
/// window. Does nothing when it was never launched (it starts on first use).
pub(crate) fn relaunch_helper() -> io::Result<()> {
    let mut slot = state()
        .lock()
        .map_err(|_| io::Error::other("Computer controller is unavailable"))?;
    if let Some(desktop) = slot.as_mut().filter(|desktop| desktop.client.is_closed()) {
        let selected = desktop.selected.clone();
        *slot = Some(launch_desktop(false, selected, None)?);
    }
    Ok(())
}
fn call(desktop: &mut Desktop, op: &str, args: Value, request: &str) -> io::Result<Value> {
    let reply = desktop
        .client
        .call(op, args, request)
        .map_err(io::Error::other)?;
    if reply["control"]["owner"] == "user" {
        USER_CONTROL.store(true, Ordering::SeqCst);
        desktop.held = None;
        desktop.frames.clear();
        desktop.interruption = reply["control"]["reason"].as_str().map(str::to_owned);
    } else if let Some(held) = desktop.held.as_mut() {
        if reply["error"]["code"] == "background_target_in_use" || reply["background_target_in_use"] == true {
            held.target_in_use = true;
            held.observation = None;
        } else if op == "observe" && reply["status"] == "ok" {
            held.target_in_use = false;
        }
    }
    Ok(reply)
}
fn remember_window(desktop: &mut Desktop, chat: &str, value: &Value) {
    if value["status"] != "ok" {
        return;
    }
    if value["result"]["pid"].is_u64() && value["result"]["window_id"].is_u64() {
        desktop.selected = Some((
            chat.into(),
            value["result"]["pid"].clone(),
            value["result"]["window_id"].clone(),
        ));
    }
    if let Some(token) = value["preview_id"].as_str() {
        desktop.preview = Some((
            chat.into(),
            token.into(),
            value["result"]["window_title"]
                .as_str()
                .unwrap_or("")
                .into(),
        ));
    }
}
fn successful(value: Value) -> io::Result<Value> {
    if value["status"] == "ok" {
        Ok(value)
    } else {
        Err(io::Error::other(value.to_string()))
    }
}
fn request_id(chat: &str, turn: &str, id: &str) -> String {
    format!(
        "input-{:x}",
        Sha256::digest(json!([chat, turn, id]).to_string().as_bytes())
    )
}
fn active(desktop: &Desktop, chat: &str, observation: &str) -> io::Result<Value> {
    if let Some(stale) = stale_observation(observation) {
        return Err(stale);
    }
    if USER_CONTROL.load(Ordering::Relaxed) {
        return Err(io::Error::other("User has computer control"));
    }
    let held = desktop.held.as_ref().filter(|held| held.chat == chat && held.until > Instant::now()).ok_or_else(|| io::Error::other("Observe this exact computer window first; its lease has expired or belongs to another chat"))?;
    let value = held
        .observation
        .as_ref()
        .filter(|value| value["observation_id"] == observation)
        .ok_or_else(|| {
            io::Error::other("Computer observation was replaced or consumed; observe again")
        })?;
    Ok(
        json!({"epoch":desktop.client.service_epoch(),"lease_id":held.lease,"observation_id":observation,"pid":value["result"]["pid"],"window_id":value["result"]["window_id"],"window_title":value["result"]["window_title"]}),
    )
}

fn turn_input_allowed(
    grant: Option<&(String, String)>,
    global: bool,
    chat: &str,
    turn: &str,
) -> bool {
    global || grant.is_some_and(|(owner, request)| owner == chat && request == turn)
}

fn desktop_available_for(desktop: Option<&Desktop>, chat: &str, turn: &str) -> io::Result<()> {
    if INPUT_OVERRIDE.load(Ordering::SeqCst) == 2 {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "Desktop input was revoked. Ask the user to enable agent control in the Computer panel."));
    }
    if USER_CONTROL.load(Ordering::SeqCst) || desktop.is_some_and(|d| d.interruption.is_some()) {
        let reason = desktop.and_then(|d| d.interruption.as_deref());
        let message = match reason {
            Some("human_input") => "Kageha paused native Mac input after detecting physical mouse or keyboard activity. Ask the user to return control in the Computer panel.",
            Some("monitor_unavailable") => "Kageha paused native Mac input because it could not monitor physical input. Ask the user to check the Computer panel.",
            Some("helper_disconnected" | "helper_restarted") => "Kageha paused native Mac input after the computer helper disconnected or restarted. Ask the user to return control in the Computer panel.",
            _ => "The user has desktop control. Ask them to return control in the Computer panel.",
        };
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, message));
    }
    if desktop.and_then(|d| d.input_turn.as_ref()).is_some_and(|(owner, active_turn)| owner != chat || active_turn != turn) {
        return Err(io::Error::other("Another turn has desktop control"));
    }
    if desktop
        .and_then(|d| d.held.as_ref())
        .is_some_and(|h| h.chat != chat && h.until > Instant::now())
    {
        return Err(io::Error::other("Another chat owns the physical desktop"));
    }
    Ok(())
}

fn release_chat_lease(desktop: &mut Desktop, chat: &str) -> io::Result<()> {
    if let Some(held) = desktop
        .held
        .as_ref()
        .filter(|h| h.chat == chat && h.until > Instant::now())
    {
        let lease = held.lease.clone();
        successful(call(
            desktop,
            "release",
            json!({"lease_id":lease}),
            &browser_client::unique_id(),
        )?)?;
        desktop.held = None;
    }
    Ok(())
}

pub(crate) fn finish_turn(chat: &str, turn: &str) {
    let Ok(mut slot) = state().lock() else { return };
    let Some(desktop) = slot
        .as_mut()
        .filter(|d| turn_input_allowed(d.input_turn.as_ref(), false, chat, turn))
    else {
        return;
    };
    desktop.input_turn = None;
    let released = call(desktop, "activity", json!({}), &browser_client::unique_id())
        .and_then(successful)
        .and_then(|_| release_chat_lease(desktop, chat));
    if let Err(error) = released {
        eprintln!("kageha: desktop lease release failed: {error}");
    }
}

fn installed_bundle_id(args: &Value) -> io::Result<&str> {
    args["bundle_id"]
        .as_str()
        .filter(|id| {
            id.len() <= 255
                && id.contains('.')
                && id.split('.').all(|part| {
                    !part.is_empty()
                        && part.as_bytes()[0].is_ascii_alphanumeric()
                        && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                })
        })
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Supply an installed app bundle ID such as com.apple.Terminal",
            )
        })
}

fn request_desktop_access(
    db: &Database,
    chat: &str,
    turn: &str,
    request: &Value,
    cancel: &AtomicBool,
    launch_app: impl FnOnce(&str, Option<&str>) -> io::Result<bool>,
) -> io::Result<String> {
    let name = request["name"].as_str().unwrap_or("");
    let args = &request["arguments"];
    let reason = args["reason"]
        .as_str()
        .map(str::trim)
        .filter(|r| !r.is_empty() && r.len() <= 1024)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Give a reason for desktop access",
            )
        })?;
    let bundle = if name == "computer_launch_app" {
        Some(installed_bundle_id(args)?)
    } else {
        None
    };
    let reveal = match args.get("reveal_path") {
        None => None,
        Some(value) => Some(value.as_str().filter(|path|
            bundle == Some("com.apple.finder") && std::path::Path::new(path).is_absolute()
                && path.len() <= 4096 && !path.chars().any(char::is_control)
        ).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Use an absolute Finder path with com.apple.finder"))?),
    };
    let revision = CONTROL_REVISION.load(Ordering::SeqCst);
    {
        let slot = state()
            .lock()
            .map_err(|_| io::Error::other("Computer unavailable"))?;
        desktop_available_for(slot.as_ref(), chat, turn)?;
    }
    let detail = match (bundle, reveal) {
        (_, Some(path)) => format!("Reveal {path} in Finder. {reason}"),
        (Some(id), None) => format!("Open installed application {id} for this task. {reason}"),
        (None, None) => format!("Enable background Accessibility actions for this chat turn. {reason}"),
    };
    let mut context = json!({"resource":bundle.unwrap_or("desktop-input"),"reason":reason,"turn":turn,"authority_generation":revision});
    if let Some(path) = reveal { context["reveal_path"] = json!(path); }
    if !ui_approvals::require(db, chat, turn, name, &detail, &context, cancel)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Desktop access declined",
        ));
    }
    let mut slot = state()
        .lock()
        .map_err(|_| io::Error::other("Computer unavailable"))?;
    desktop_available_for(slot.as_ref(), chat, turn)?;
    // Recheck the native monitor after any approval wait. A physical takeover can
    // occur without a web-panel request changing the host's revision counter.
    if let Some(desktop) = slot.as_mut() {
        successful(call(
            desktop,
            "activity",
            json!({}),
            &browser_client::unique_id(),
        )?)?;
        desktop_available_for(Some(desktop), chat, turn)?;
    }
    if cancel.load(Ordering::SeqCst) || revision != CONTROL_REVISION.load(Ordering::SeqCst) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Desktop authority changed before dispatch",
        ));
    }
    if let Some(id) = bundle {
        let outcome = crate::ui_effects::run(db, turn, request, &context, || {
            let mut result = json!({"launched":launch_app(id, reveal)?,"bundle_id":id});
            if let Some(path) = reveal { result["reveal_requested"] = json!(path); }
            Ok(result)
        })?;
        if outcome["launched"] != true {
            return Err(io::Error::other(format!("macOS could not open {id}")));
        }
        return Ok(outcome.to_string());
    }
    if slot.as_ref().is_none_or(|d| {
        d.client.is_closed()
            || !turn_input_allowed(d.input_turn.as_ref(), input_enabled(), chat, turn)
    }) {
        if let Some(desktop) = slot.as_mut() {
            release_chat_lease(desktop, chat)?;
            desktop_available_for(Some(desktop), chat, turn)?;
        }
        let selected = slot.take().and_then(|d| d.selected.clone());
        *slot = Some(launch_desktop(
            false,
            selected,
            Some((chat.to_owned(), turn.to_owned())),
        )?);
    }
    let desktop = slot.as_ref().unwrap();
    desktop_available_for(Some(desktop), chat, turn)?;
    Ok(json!({"input_authorized":desktop.client.info()["input_authorized"],"next":"Check computer_permissions. If macOS requires consent, ask the user to grant it in the Computer panel. Observe the exact target window before input."}).to_string())
}

pub(crate) fn tool(
    db: &Database,
    chat: &str,
    turn: &str,
    request: &Value,
    cancel: &AtomicBool,
) -> io::Result<String> {
    let name = request["name"].as_str().unwrap_or("");
    let args = &request["arguments"];
    if cancel.load(Ordering::Relaxed) {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "turn cancelled"));
    }
    let id = request_id(chat, turn, request["id"].as_str().unwrap_or(""));
    if matches!(name, "computer_request_control" | "computer_launch_app") {
        // Read the helper's persisted interruption state before considering an app launch.
        with_desktop(|desktop| desktop_available_for(Some(desktop), chat, turn))?;
        return request_desktop_access(db, chat, turn, request, cancel, |id, reveal| {
            let mut open = std::process::Command::new("/usr/bin/open");
            open.arg("-g");
            if let Some(path) = reveal { open.args(["-R", path]); } else { open.args(["-b", id]); }
            open.status().map(|s| s.success())
        });
    }
    if name == "computer_execute" {
        let observation = args["observation_id"]
            .as_str()
            .ok_or_else(|| io::Error::other("observation_id required"))?;
        let method = args["method"]
            .as_str()
            .ok_or_else(|| io::Error::other("native action method required"))?;
        kageha_computer::control::validate_action(method, &args["args"])
            .map_err(io::Error::other)?;
        kageha_computer::control::validate_background_action(method, &args["args"])
            .map_err(io::Error::other)?;
        let action = json!({"method":method,"args":args["args"]});
        let original = with_desktop(|desktop| {
            if desktop.client.info()["input_authorized"] != true || !turn_input_allowed(desktop.input_turn.as_ref(),input_enabled(),chat,turn) {
                return Err(io::Error::other("Call computer_request_control with a reason for this task, then observe the window again."));
            }
            active(desktop, chat, observation)
        })?;
        let mut context = original.clone();
        context["action_sha256"] = json!(format!(
            "{:x}",
            Sha256::digest(action.to_string().as_bytes())
        ));
        if !ui_approvals::require(
            db,
            chat,
            turn,
            name,
            &format!(
                "Use {} in process {}, window {} ({})",
                args["method"].as_str().unwrap_or("native input"),
                original["pid"],
                original["window_id"],
                original["window_title"].as_str().unwrap_or("")
            ),
            &context,
            cancel,
        )? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Computer input declined",
            ));
        }
        let value = with_desktop(|desktop| {
            if cancel.load(Ordering::Relaxed) { return Err(io::Error::new(io::ErrorKind::Interrupted,"turn cancelled before computer dispatch")); }
            if !turn_input_allowed(desktop.input_turn.as_ref(), input_enabled(), chat, turn) || active(desktop, chat, observation)? != original { return Err(io::Error::other("Computer authority changed during approval; no input sent")); }
            let connection = db.lock().map_err(|_| io::Error::other("database unavailable"))?;
            connection.execute_batch("CREATE TABLE IF NOT EXISTS ui_computer_receipts(request_id TEXT PRIMARY KEY,chat_id TEXT NOT NULL)").map_err(io::Error::other)?;
            connection.execute("INSERT OR IGNORE INTO ui_computer_receipts VALUES(?,?)", [&id, chat]).map_err(io::Error::other)?;
            drop(connection);
            let result = call(desktop,"execute",json!({"lease_id":original["lease_id"],"observation_id":observation,"action":action}),&id);
            if let Some(held) = desktop.held.as_mut() { held.observation = None; }
            result
        }).map_err(|error| io::Error::new(error.kind(),format!("Computer input request {id}: {error}. Recover using computer_receipt; do not replay input.")))?;
        let reply = json!({"request_id":id,"outcome":value});
        return if reply["outcome"]["status"] == "ok" {
            Ok(reply.to_string())
        } else {
            Err(io::Error::other(reply.to_string()))
        };
    }
    if name == "computer_observe" && !input_enabled() {
        let mut slot = state()
            .lock()
            .map_err(|_| io::Error::other("Computer unavailable"))?;
        if slot.as_ref().is_some_and(|d| {
            d.input_turn.is_some() && !turn_input_allowed(d.input_turn.as_ref(), false, chat, turn)
        }) {
            return Err(io::Error::other(
                "Another turn has desktop input. Use computer_inspect for a read-only view.",
            ));
        }
        if slot
            .as_ref()
            .is_some_and(|d| d.input_turn.is_none() && d.client.info()["input_authorized"] == true)
        {
            desktop_available_for(slot.as_ref(), chat, turn)?;
            let selected = slot.take().and_then(|d| d.selected.clone());
            *slot = Some(launch_desktop(false, selected, None)?);
        }
    }
    with_desktop(|desktop| match name {
        "computer_info" => {
            let mut info = desktop.client.info().clone();
            info["input_authorized"] = json!(info["input_authorized"] == true && turn_input_allowed(desktop.input_turn.as_ref(),input_enabled(),chat,turn));
            Ok(info)
        },
        "computer_activity" => call(desktop,"activity",json!({}),&id),
        "computer_inspect" => { let result = call(desktop,"inspect",args.clone(),&id)?; remember_window(desktop,chat,&result); Ok(result) }
        "computer_permissions" => call(desktop,"permissions",json!({}),&id),
        "computer_apps" => call(desktop,"list_apps",json!({}),&id),
        "computer_windows" => call(desktop,"list_windows",json!({"pid":args["pid"]}),&id),
        "computer_observe" => {
            if desktop.client.info()["background_accessibility"] != true {
                return Err(io::Error::other("Update the Mac computer helper for background Accessibility input. Use computer_inspect to read the target window."));
            }
            if desktop.client.info()["input_authorized"] != true {
                return Err(io::Error::other("Call computer_request_control for input authorization, or use computer_inspect for a read-only view."));
            }
            if USER_CONTROL.load(Ordering::Relaxed) { return Err(io::Error::other("User has computer control")); }
            if desktop.held.as_ref().is_some_and(|held| held.until <= Instant::now()) { desktop.held = None; }
            if desktop.held.as_ref().is_some_and(|held| held.chat != chat) { return Err(io::Error::other("Another chat owns the physical desktop")); }
            let lease_deadline = Instant::now()+Duration::from_secs(59);
            if let Some(held) = desktop.held.as_ref() {
                let lease = held.lease.clone();
                successful(call(desktop,"renew",json!({"lease_id":lease,"ttl_ms":60000}),&id)?)?;
            } else {
                let acquired = successful(call(desktop,"acquire",json!({"ttl_ms":60000,"background":true}),&id)?)?;
                desktop.held = Some(Held { chat:chat.into(), lease:acquired["lease_id"].as_str().ok_or_else(||io::Error::other("lease missing"))?.into(), until:Instant::now()+Duration::from_secs(60), observation:None, target_in_use:false });
            }
            let lease = desktop.held.as_ref().unwrap().lease.clone();
            let mut native = args.clone(); native["lease_id"] = json!(lease);
            let result = call(desktop,"observe",native,&id)?;
            remember_window(desktop,chat,&result);
            if let Some(held) = desktop.held.as_mut() { held.until = lease_deadline; held.observation = (result["status"] == "ok").then(|| result.clone()); }
            if let Some(observation) = result["observation_id"].as_str().filter(|_| result["status"] == "ok") { remember_observation(observation); }
            Ok(result)
        }
        "computer_receipt" => {
            let original = args["request_id"].as_str().ok_or_else(||io::Error::other("request_id required"))?;
            let connection = db.lock().map_err(|_|io::Error::other("database unavailable"))?;
            connection.execute_batch("CREATE TABLE IF NOT EXISTS ui_computer_receipts(request_id TEXT PRIMARY KEY,chat_id TEXT NOT NULL)").map_err(io::Error::other)?;
            let owned = connection.query_row("SELECT COUNT(*) FROM ui_computer_receipts WHERE request_id=? AND chat_id=?",[original,chat],|row|row.get::<_,i64>(0)).map_err(io::Error::other)?;
            drop(connection);
            if owned != 1 { return Err(io::Error::other("Receipt does not belong to this chat")); }
            call(desktop,"receipt",json!({"request_id":original}),&id)
        }
        "computer_release" => {
            if let Some(held) = desktop.held.as_ref().filter(|held|held.chat == chat) {
                let lease = held.lease.clone(); let result = call(desktop,"release",json!({"lease_id":lease}),&id)?;
                desktop.held = None; Ok(result)
            } else { Ok(json!({"status":"ok","released":false})) }
        }
        _ => Err(io::Error::other("Unsupported computer tool")),
    }).and_then(tool_reply)
}
fn tool_reply(value: Value) -> io::Result<String> {
    if value["status"] == "error" {
        Err(io::Error::other(value.to_string()))
    } else {
        Ok(value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn desktop_input_grant_belongs_to_the_requesting_chat_turn() {
        let grant = Some(("first".to_owned(), "turn1".to_owned()));
        assert!(turn_input_allowed(grant.as_ref(), false, "first", "turn1"));
        assert!(!turn_input_allowed(grant.as_ref(), false, "first", "turn2"));
        assert!(!turn_input_allowed(grant.as_ref(), false, "other", "turn1"));
        assert!(!turn_input_allowed(None, false, "first", "turn1"));
    }

    #[test]
    fn app_launch_is_bound_to_one_installed_bundle_and_reuses_its_receipt() {
        use crate::audited_session_tests::{Fixture, CHAT};
        let f = Fixture::new();
        let cancel = AtomicBool::new(false);
        let request = json!({"id":"terminal-launch","name":"computer_launch_app",
            "arguments":{"bundle_id":"com.apple.Terminal","reason":"Open the requested terminal"}});
        let first = request_desktop_access(&f.db, CHAT, &f.turn, &request, &cancel, |id, path| {
            assert!(path.is_none());
            assert_eq!(id, "com.apple.Terminal");
            Ok(true)
        })
        .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&first).unwrap()["launched"],
            true
        );
        let replay = request_desktop_access(&f.db, CHAT, &f.turn, &request, &cancel, |_, _| {
            panic!("duplicate dispatch")
        })
        .unwrap();
        assert_eq!(first, replay);
        let path = "/Users/example/Downloads/report with spaces.pdf";
        let mut reveal = json!({"id":"reveal","name":"computer_launch_app","arguments":{
            "bundle_id":"com.apple.finder","reveal_path":path,"reason":"Show the requested report"}});
        let result = request_desktop_access(&f.db, CHAT, &f.turn, &reveal, &cancel, |id, target| {
            assert_eq!((id,target), ("com.apple.finder", Some(path)));
            Ok(true)
        }).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&result).unwrap()["reveal_requested"], path);
        for (bundle, target) in [("com.apple.Terminal", path), ("com.apple.finder", "-a"), ("com.apple.finder","https://example.org")] {
            reveal["arguments"]["bundle_id"] = json!(bundle);
            reveal["arguments"]["reveal_path"] = json!(target);
            assert!(request_desktop_access(&f.db, CHAT, &f.turn, &reveal, &cancel,
                |_, _| panic!("invalid reveal dispatched")).is_err());
        }
        for invalid in [
            "/Applications/Terminal.app",
            "-b",
            "file:///tmp/a",
            "com.apple.Terminal -e",
            "com..apple",
        ] {
            assert!(installed_bundle_id(&json!({"bundle_id":invalid})).is_err());
        }
    }

    #[test]
    fn refused_read_tools_stream_as_failures_without_flattening_receipt_queries() {
        assert!(
            tool_reply(json!({"status":"error","error":{"code":"human_intervention"}})).is_err()
        );
        let unknown = json!({"status":"unknown","receipt":{"state":"unknown"}});
        assert_eq!(
            serde_json::from_str::<Value>(&tool_reply(unknown.clone()).unwrap()).unwrap(),
            unknown
        );
    }
    #[test]
    fn manual_coordinates_are_window_local_bounded_and_foreground_explicit() {
        let frame = ManualFrame {
            chat: "a".into(),
            id: "issued".into(),
            pid: json!(1),
            window: json!(2),
            width: 1000.0,
            height: 500.0,
            at: Instant::now(),
        };
        let plan =
            manual_plan(&json!({"action":"click","x":500,"y":250,"pid":999}), &frame).unwrap();
        assert_eq!(
            plan,
            json!({"method":"click","args":{"x":500.0,"y":250.0,"button":"left","delivery_mode":"foreground"}})
        );
        for payload in [
            json!({"action":"click","x":-1,"y":0}),
            json!({"action":"click","x":1000,"y":0}),
            json!({"action":"drag","from_x":0,"from_y":0,"to_x":50,"to_y":500}),
            json!({"action":"scroll","direction":"invalid"}),
            json!({"action":"type","text":42}),
        ] {
            assert!(manual_plan(&payload, &frame).is_err());
        }
    }
    #[test]
    fn observations_from_before_a_helper_relaunch_are_stale() {
        let before = format!("obs-before-{}", browser_client::unique_id());
        remember_observation(&before);
        assert!(stale_observation(&before).is_none());
        // What a relaunch (`relaunch_helper` → `launch_desktop`) does.
        mark_observations_stale();
        let error: Value = serde_json::from_str(&stale_observation(&before).unwrap().to_string()).unwrap();
        assert_eq!(error["error_code"], "stale_observation");
        assert_eq!((error["observation_id"].as_str(), error["execution_phase"].as_str()), (Some(before.as_str()), Some("not_dispatched")));
        // An observation from the new helper is not stale; unknown ids are left to the
        // lease check.
        let after = format!("obs-after-{}", browser_client::unique_id());
        remember_observation(&after);
        assert!(stale_observation(&after).is_none());
        assert!(stale_observation("never-issued").is_none());
    }
    #[test]
    fn durable_input_ids_are_bound_to_chat_turn_and_call() {
        assert_eq!(
            request_id("a", "turn", "call"),
            request_id("a", "turn", "call")
        );
        assert_ne!(request_id("a:b", "c", "d"), request_id("a", "b:c", "d"));
        assert_ne!(
            request_id("a", "turn", "call"),
            request_id("b", "turn", "call")
        );
    }
}

pub(crate) fn status(chat: &str) -> Value {
    let slot = state().lock().ok();
    let desktop = slot.as_ref().and_then(|slot| slot.as_ref());
    let held = desktop
        .and_then(|desktop| desktop.held.as_ref())
        .filter(|held| held.chat == chat && held.until > Instant::now());
    json!({"sessionId":chat,"provider":"local_mac","runtime":"rust","native_adapter":"swift",
        "available":configured(),"connected":desktop.is_some_and(|d| !d.client.is_closed()),"serviceEpoch":desktop.map(|d| d.client.service_epoch()),"interruptionReason":desktop.and_then(|d| d.interruption.as_ref()),"canControl":input_enabled(),"canTakeControl":configured(),"inputAuthorized":input_enabled() || desktop.and_then(|d| d.input_turn.as_ref()).is_some_and(|(owner,_)|owner == chat),
        "backgroundTargetInUse":held.is_some_and(|held| held.target_in_use),
        "agentBound":held.is_some(),"lockedBy":if USER_CONTROL.load(Ordering::Relaxed) {json!("user")}else if held.is_some(){json!("agent")}else{Value::Null},
        "app":held.and_then(|held|held.observation.as_ref()).map(|v|v["result"]["window_title"].clone()).or_else(||desktop.and_then(|d|d.preview.as_ref()).filter(|(owner,_,_)|owner==chat).map(|(_,_,title)|json!(title))).unwrap_or(Value::Null),
        "previewAvailable":configured(),"previewError":null})
}

// Operator input is separate from model approval. The request selects only a
// controller-issued recent frame, never an arbitrary process or window.
fn manual_plan(payload: &Value, frame: &ManualFrame) -> io::Result<Value> {
    let number = |key: &str, limit: f64| {
        payload[key]
            .as_f64()
            .filter(|n| n.is_finite() && *n >= 0.0 && *n < limit)
            .ok_or_else(|| io::Error::other(format!("{key} outside the displayed frame")))
    };
    let (method, mut args) = match payload["action"].as_str().unwrap_or("") {
        "click" => (
            "click",
            json!({"x":number("x",frame.width)?,"y":number("y",frame.height)?,"button":payload.get("button").cloned().unwrap_or(json!("left"))}),
        ),
        "type" => ("type_text", json!({"text":payload["text"]})),
        "key" => ("press_key", json!({"key":payload["key"]})),
        "scroll" => (
            "scroll",
            json!({"direction":payload["direction"],"amount":payload.get("amount").cloned().unwrap_or(json!(3))}),
        ),
        "drag" => (
            "drag",
            json!({"from_x":number("from_x",frame.width)?,"from_y":number("from_y",frame.height)?,"to_x":number("to_x",frame.width)?,"to_y":number("to_y",frame.height)?}),
        ),
        "focus" => ("focus_window", json!({})),
        _ => return Err(io::Error::other("Unsupported manual action")),
    };
    if ["click", "scroll", "drag", "press_key"].contains(&method) {
        args["delivery_mode"] = json!("foreground");
    }
    kageha_computer::control::validate_action(method, &args).map_err(io::Error::other)?;
    Ok(json!({"method":method,"args":args}))
}
fn manual_action(db: &Database, chat: &str, payload: &Value) -> io::Result<Value> {
    let client_id = payload["request_id"]
        .as_str()
        .filter(|id| !id.is_empty() && id.len() <= 128)
        .ok_or_else(|| {
            io::Error::other("A bounded request_id is required; never replay unknown input")
        })?;
    let id = request_id(chat, "manual", client_id);
    let digest = format!("{:x}", Sha256::digest(payload.to_string().as_bytes()));
    let revision = CONTROL_REVISION.load(Ordering::SeqCst);
    with_desktop(|desktop| {
        let connection = db
            .lock()
            .map_err(|_| io::Error::other("database unavailable"))?;
        connection.execute_batch("CREATE TABLE IF NOT EXISTS ui_computer_manual(request_id TEXT PRIMARY KEY,digest TEXT NOT NULL,response TEXT)").map_err(io::Error::other)?;
        let prior = connection.query_row(
            "SELECT digest,response FROM ui_computer_manual WHERE request_id=?",
            [&id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
        );
        match prior {
            Ok((old, response)) => {
                return if old != digest {
                    Err(io::Error::other(
                        "Manual request_id conflict; no input sent",
                    ))
                } else {
                    Ok(response.and_then(|text|serde_json::from_str(&text).ok()).unwrap_or(json!({"status":"unknown","request_id":id,"summary":"Earlier manual request has no recorded result. Inspect current state; do not replay input."})))
                }
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {}
            Err(error) => return Err(io::Error::other(error)),
        }
        if !USER_CONTROL.load(Ordering::SeqCst)
            || !input_enabled()
            || desktop.client.info()["input_authorized"] != true
        {
            return Err(io::Error::other(
                "Take control and enable input first; no input sent",
            ));
        }
        let frame = desktop
            .frames
            .iter()
            .find(|frame| {
                frame.chat == chat
                    && payload["frame_id"] == frame.id
                    && frame.at.elapsed() < Duration::from_secs(15)
            })
            .ok_or_else(|| {
                io::Error::other(
                    "Displayed frame expired or belongs to another chat; refresh before input",
                )
            })?;
        let action = manual_plan(payload, frame)?;
        let (pid, window, width, height) = (
            frame.pid.clone(),
            frame.window.clone(),
            frame.width,
            frame.height,
        );
        connection
            .execute(
                "INSERT INTO ui_computer_manual(request_id,digest) VALUES(?,?)",
                [&id, &digest],
            )
            .map_err(io::Error::other)?;
        drop(connection);
        // This reservation is never retried, including after a crash. Native
        // execution has its own durable receipt in addition to this host row.
        let mut dispatch_started = false;
        let result = (|| -> io::Result<Value> {
            if let Some(held) = desktop.held.take() {
                successful(call(
                    desktop,
                    "release",
                    json!({"lease_id":held.lease}),
                    &browser_client::unique_id(),
                )?)?;
            }
            let acquired = successful(call(
                desktop,
                "acquire",
                json!({"ttl_ms":60000}),
                &browser_client::unique_id(),
            )?)?;
            let lease = acquired["lease_id"].clone();
            let observed = successful(call(
                desktop,
                "observe",
                json!({"lease_id":lease,"pid":pid,"window_id":window,"include_screenshot":true,"max_image_dimension":1280}),
                &browser_client::unique_id(),
            )?)?;
            if let Some(token) = observed["preview_id"].as_str() {
                desktop.preview = Some((
                    chat.into(),
                    token.into(),
                    observed["result"]["window_title"]
                        .as_str()
                        .unwrap_or("")
                        .into(),
                ));
            }
            desktop.frames.clear();
            let outcome = if observed["result"]["screenshot_width"].as_f64() != Some(width)
                || observed["result"]["screenshot_height"].as_f64() != Some(height)
            {
                Err(io::Error::other(
                    "Window capture dimensions changed; refresh before input",
                ))
            } else if revision != CONTROL_REVISION.load(Ordering::SeqCst)
                || !USER_CONTROL.load(Ordering::SeqCst)
                || !input_enabled()
            {
                Err(io::Error::other(
                    "Control changed before dispatch; no input sent",
                ))
            } else {
                dispatch_started = true;
                call(
                    desktop,
                    "execute",
                    json!({"lease_id":lease,"observation_id":observed["observation_id"],"action":action}),
                    &id,
                )
            };
            let _ = call(
                desktop,
                "release",
                json!({"lease_id":lease}),
                &browser_client::unique_id(),
            );
            outcome
        })();
        let reply = match result {
            Ok(value) => json!({"request_id":id,"outcome":value,"status":value["status"]}),
            Err(error) => {
                json!({"status":if dispatch_started {"unknown"}else{"error"},"execution_phase":if dispatch_started {"dispatch_started"}else{"not_dispatched"},"request_id":id,"summary":format!("{error}. Inspect state; this request will not be replayed.")})
            }
        };
        let saved = db
            .lock()
            .map_err(|_| io::Error::other("database unavailable"))
            .and_then(|db| {
                db.execute(
                    "UPDATE ui_computer_manual SET response=? WHERE request_id=?",
                    [reply.to_string(), id.clone()],
                )
                .map_err(io::Error::other)
            });
        if saved.is_err() {
            return Ok(
                json!({"status":"unknown","request_id":id,"summary":"Manual result could not be persisted. Inspect the app; do not repeat the input."}),
            );
        }
        Ok(reply)
    })
}

pub(crate) fn handle(
    mut request: tiny_http::Request,
    db: &Database,
    chat: &str,
    operation: &str,
) -> io::Result<()> {
    crate::ui_uploads::root(db, chat)?; // Require an existing authenticated chat.
    if request.method() == &tiny_http::Method::Get && operation == "state" {
        respond(request, 200, status(chat));
        return Ok(());
    }
    if request.method() == &tiny_http::Method::Post && operation == "access" {
        let payload = crate::webui::body(&mut request)?;
        let Some(allowed) = payload["allow"].as_bool() else {
            respond(request, 400, json!({"error":"allow must be boolean"}));
            return Ok(());
        };
        CONTROL_REVISION.fetch_add(1, Ordering::SeqCst);
        if !allowed {
            USER_CONTROL.store(true, Ordering::SeqCst);
        }
        INPUT_OVERRIDE.store(if allowed { 1 } else { 2 }, Ordering::SeqCst);
        // Input authorization is fixed at child launch. Reconnect only after a
        // deliberate operator change, never to retry a model action.
        *state()
            .lock()
            .map_err(|_| io::Error::other("computer unavailable"))? = None;
        respond(request, 200, status(chat));
        return Ok(());
    }
    if request.method() == &tiny_http::Method::Post && operation == "refresh" {
        let result = with_desktop(|desktop| {
            if !USER_CONTROL.load(Ordering::SeqCst) {
                return Err(io::Error::other(
                    "Take control before refreshing the manual view",
                ));
            }
            let (_, pid, window) = desktop
                .selected
                .as_ref()
                .filter(|(owner, _, _)| owner == chat)
                .cloned()
                .ok_or_else(|| io::Error::other("No selected window for this chat"))?;
            let observed = successful(call(
                desktop,
                "inspect",
                json!({"pid":pid,"window_id":window}),
                &browser_client::unique_id(),
            )?)?;
            remember_window(desktop, chat, &observed);
            desktop.frames.clear();
            Ok(())
        });
        match result {
            Ok(()) => respond(request, 200, status(chat)),
            Err(error) => respond(request, 409, json!({"error":error.to_string()})),
        };
        return Ok(());
    }
    if request.method() == &tiny_http::Method::Post && operation == "action" {
        let payload = crate::webui::body(&mut request)?;
        match manual_action(db, chat, &payload) {
            Ok(reply) => respond(request, 200, reply),
            Err(error) => respond(
                request,
                409,
                json!({"status":"error","error":error.to_string()}),
            ),
        };
        return Ok(());
    }
    if request.method() == &tiny_http::Method::Post && operation == "permission" {
        let payload = crate::webui::body(&mut request)?;
        let option = match payload["kind"].as_str() {
            Some("screen_recording") => "--request-screen-recording",
            Some("input_monitoring") => "--request-input-monitoring",
            Some("accessibility") => "--request-accessibility",
            _ => {
                respond(request, 400, json!({"error":"Unsupported permission"}));
                return Ok(());
            }
        };
        let (binary, _) = config()?;
        let output = std::process::Command::new(binary)
            .args(["permissions", option])
            .output()?;
        let value = serde_json::from_slice::<Value>(&output.stdout)
            .unwrap_or_else(|_| json!({"error":"Permission request failed; check macOS Settings"}));
        respond(
            request,
            if output.status.success() { 200 } else { 503 },
            value,
        );
        return Ok(());
    }
    if request.method() == &tiny_http::Method::Get && operation == "preview" {
        let value = with_desktop(|desktop| {
            let token = desktop
                .preview
                .as_ref()
                .filter(|(owner, _, _)| owner == chat)
                .map(|(_, token, _)| token.clone())
                .ok_or_else(|| {
                    io::Error::other(
                        "No preview grant for this chat; ask the agent to observe a window",
                    )
                })?;
            let mut reply = call(
                desktop,
                "preview",
                json!({"preview_id":token}),
                &browser_client::unique_id(),
            )?;
            if reply["status"] == "ok" {
                let id = browser_client::unique_id();
                desktop.frames.push_back(ManualFrame {
                    chat: chat.into(),
                    id: id.clone(),
                    pid: reply["result"]["pid"].clone(),
                    window: reply["result"]["window_id"].clone(),
                    width: reply["result"]["width"].as_f64().unwrap_or(0.0),
                    height: reply["result"]["height"].as_f64().unwrap_or(0.0),
                    at: Instant::now(),
                });
                while desktop.frames.len() > 4 {
                    desktop.frames.pop_front();
                }
                reply["frame_id"] = json!(id);
            }
            Ok(reply)
        });
        match value {
            Ok(reply) => respond(request, 200, reply),
            Err(error) => respond(
                request,
                200,
                json!({"status":"error","error":{"code":"preview_unavailable","message":error.to_string()}}),
            ),
        }
        return Ok(());
    }
    if request.method() == &tiny_http::Method::Post && operation == "lease" {
        let payload = crate::webui::body(&mut request)?;
        let action = payload["action"].as_str().unwrap_or("");
        if !["take", "return"].contains(&action) {
            respond(
                request,
                400,
                json!({"error":"action must be take or return"}),
            );
            return Ok(());
        }
        CONTROL_REVISION.fetch_add(1, Ordering::SeqCst);
        USER_CONTROL.store(true, Ordering::SeqCst);
        let result = (|| -> io::Result<()> {
            let mut slot = state()
                .lock()
                .map_err(|_| io::Error::other("Computer controller unavailable"))?;
            // A healthy connection can transfer ownership without releasing the
            // physical desktop lock. Only an interrupted connection needs restart.
            let mut restart = slot
                .as_ref()
                .is_none_or(|d| d.client.is_closed() || d.interruption.is_some());
            if !restart {
                let desktop = slot.as_mut().unwrap();
                let activity = call(desktop, "activity", json!({}), &browser_client::unique_id());
                restart = activity.is_err() || desktop.interruption.is_some();
                if !restart {
                    if let Some(held) = desktop.held.take() {
                        successful(call(
                            desktop,
                            "release",
                            json!({"lease_id":held.lease}),
                            &browser_client::unique_id(),
                        )?)?;
                    }
                    desktop.frames.clear();
                }
            }
            if restart {
                let selected = slot.as_ref().and_then(|d| d.selected.clone());
                let mut previous = slot.take();
                if let Some(old) = previous.as_mut() {
                    old.client.close();
                    old.held = None;
                    old.frames.clear();
                    old.preview = None;
                    old.interruption = Some("helper_disconnected".into());
                }
                std::thread::sleep(Duration::from_millis(100));
                match launch_desktop(input_enabled(), selected, None) {
                    Ok(desktop) => *slot = Some(desktop),
                    Err(error) => {
                        *slot = previous;
                        return Err(error);
                    }
                }
            }
            let desktop = slot.as_mut().unwrap();
            if action == "take" {
                if let Some((_, pid, window)) = desktop
                    .selected
                    .clone()
                    .filter(|(owner, _, _)| owner == chat)
                {
                    let view = successful(call(
                        desktop,
                        "inspect",
                        json!({"pid":pid,"window_id":window}),
                        &browser_client::unique_id(),
                    )?)?;
                    remember_window(desktop, chat, &view);
                }
            }
            USER_CONTROL.store(
                action == "take" || desktop.interruption.is_some(),
                Ordering::SeqCst,
            );
            Ok(())
        })();
        match result {
            Ok(()) => respond(request, 200, status(chat)),
            Err(error) => respond(
                request,
                409,
                json!({"error":format!("Control remains with you: {error}. No input was replayed.")}),
            ),
        }
        return Ok(());
    }
    respond(
        request,
        501,
        json!({"error":"Native agent tools and ownership are available; manual desktop streaming controls have not been migrated yet"}),
    );
    Ok(())
}

fn respond(request: tiny_http::Request, status: u16, value: Value) {
    let response = tiny_http::Response::from_string(value.to_string())
        .with_status_code(status)
        .with_header(tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap());
    let _ = request.respond(response);
}
