//! Translate the existing browser-panel API into standalone browser RPCs.
use crate::{
    browser_client::{self, Client},
    server,
    webui::Database,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::hash::{Hash, Hasher};
use std::io::{self, Read};
use std::time::Duration;
use tiny_http::{Header, Method, Request, Response, StatusCode};

pub(crate) fn initialize(db: &rusqlite::Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS ui_browser_control(chat_id TEXT PRIMARY KEY,
        epoch TEXT NOT NULL,owner TEXT NOT NULL,authority TEXT NOT NULL,expires_ms INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS ui_browser_preview(chat_id TEXT PRIMARY KEY,
        binding_id TEXT NOT NULL,painted_ms INTEGER NOT NULL);")
}

/// Binding recovery for a chat whose selected profile has no running browser (first use,
/// or after the browser service restarted): relaunches the selected managed profile with
/// `switch_profile`, which reopens it with its saved state. Sends no page input. Returns
/// the new selection, or `None` when a browser is already running. A signed-in personal
/// profile can only be reconnected by the user in the browser panel.
pub(crate) fn reopen_profile(client: &mut Client, selection: &Value) -> io::Result<Option<Value>> {
    if !selection["browser_id"].as_str().unwrap_or("").is_empty() {
        return Ok(None);
    }
    let profile = selection["profile"].as_str().unwrap_or("private");
    if !matches!(profile, "private" | "alternate") {
        return Err(io::Error::other(
            "The selected signed-in profile is disconnected. Ask the user to reconnect it in the browser panel; do not substitute a private profile.",
        ));
    }
    client.call(
        "switch_profile",
        json!({"profile":profile,"binding_id":selection["binding_id"]}),
        None,
    )?;
    client.selection().map(Some)
}

/// `open_tab` needs a connected browser. Without one the daemon can only answer
/// `invalid_params`, which gives the agent nothing to act on and invites retries.
pub(crate) fn require_connected(selection: &Value) -> io::Result<()> {
    if selection["browser_id"].as_str().is_some_and(|id| !id.is_empty()) {
        return Ok(());
    }
    let profile = selection["profile"].as_str().unwrap_or("private");
    // A signed-in profile can only be reconnected by the user in the Browser panel.
    let (code, summary) = if matches!(profile, "chrome" | "comet") {
        ("browser_profile_disconnected", format!("The {profile} browser is not connected, so no page was opened and nothing was sent. Do not call browser_open for {profile} again. Ask the user to reconnect {profile} in the Browser panel, or open the page with profile private if signing in is not needed."))
    } else {
        ("browser_not_connected", format!("The {profile} browser is not connected, so no page was opened and nothing was sent. Take a browser_tabs reading, then try browser_open once more; if it fails again, tell the user the browser is unavailable."))
    };
    Err(crate::ui_tools::tool_error(io::ErrorKind::Other, code, false, &summary))
}

pub(crate) fn preview_status(db: &Database, chat: &str, selection: &Value) -> io::Result<Value> {
    let painted = db.lock().map_err(|_| io::Error::other("database unavailable"))?
        .query_row(
            "SELECT painted_ms FROM ui_browser_preview WHERE chat_id=? AND binding_id=?",
            params![chat, selection["binding_id"].as_str().unwrap_or("")],
            |row| row.get::<_, u64>(0),
        ).optional().map_err(io::Error::other)?;
    Ok(json!({
        "tab_handle": selection["target_id"],
        "binding_id": selection["binding_id"],
        "displayed": painted.is_some_and(|at| browser_client::milliseconds().checked_sub(at).is_some_and(|age| age < 15000)),
    }))
}

pub(crate) fn control(
    db: &Database,
    chat: &str,
    epoch: &str,
) -> io::Result<Option<(String, Value)>> {
    let row=db.lock().map_err(|_|io::Error::other("database unavailable"))?.query_row(
        "SELECT owner,authority FROM ui_browser_control WHERE chat_id=? AND epoch=? AND expires_ms>?",
        params![chat,epoch,browser_client::milliseconds()],|row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?)))
        .optional().map_err(io::Error::other)?;
    row.map(|(owner, authority)| Ok((owner, serde_json::from_str(&authority)?)))
        .transpose()
}

pub(crate) fn connect(db: &Database, chat: &str, actor: &str) -> io::Result<Client> {
    connect_with_start(db, chat, actor, actor == "agent")
}

fn connect_with_start(db: &Database, chat: &str, actor: &str, start: bool) -> io::Result<Client> {
    let _guard = crate::docker_desktop::lock(chat);
    let machine = {
        let connection = db.lock().map_err(|_| io::Error::other("database unavailable"))?;
        crate::docker_desktop::binding(&connection, chat).map_err(io::Error::other)?
    };
    let remote = crate::ui_remote::selection(db, chat).map_err(|(_, error)| io::Error::other(error))?;
    let hosted = if let Some(machine) = machine {
        crate::docker_desktop::validate(db, chat, &machine)?;
        crate::machine_io::register(db, chat, &machine)?;
        true
    } else if let Some(session) = crate::linux_session::for_chat(db, chat, &remote, start)? {
        let running = session.running(start, &std::sync::atomic::AtomicBool::new(false))
            .map_err(|_| io::Error::other("Linux Chromium is unavailable. Check the Linux session helper and reconnect."))?;
        crate::machine_io::register_linux(db, chat, &session, &running)?;
        if session.mode == "desktop" {
            let connection = db.lock().map_err(|_|io::Error::other("Database unavailable"))?;
            crate::docker_desktop::remember_display(&connection,chat,&session.key)?;
        }
        true
    } else if !remote.is_null() {
        return Err(io::Error::other("Browser control is unavailable on the selected SSH destination"));
    } else { false };
    let mut client = Client::connect(chat, actor)?;
    if hosted {
        let selection = client.selection()?;
        if selection["profile"] != "machine" || selection["browser_id"].as_str().is_none_or(str::is_empty) {
            client.call("switch_profile", json!({"profile":"machine","binding_id":selection["binding_id"]}), None)
                .map_err(|error| io::Error::other(format!("Linux Chromium is unavailable. Reconnect the selected environment and retry. {error}")))?;
        }
    }
    Ok(client)
}

pub(crate) fn claim(
    db: &Database,
    chat: &str,
    actor: &str,
    force: bool,
) -> io::Result<(Client, Value)> {
    let client = connect(db, chat, actor)?;
    claim_client(db, chat, actor, force, client)
}

fn claim_client(
    db: &Database,
    chat: &str,
    actor: &str,
    force: bool,
    mut client: Client,
) -> io::Result<(Client, Value)> {
    let connection = db
        .lock()
        .map_err(|_| io::Error::other("database unavailable"))?;
    if let Some(user) = crate::docker_desktop::user_driving(&connection, chat).map_err(io::Error::other)? {
        if user != (actor == "user") {
            return Err(io::Error::other("The other driver controls the Linux desktop"));
        }
    }
    let prior = connection
        .query_row(
            "SELECT epoch,owner,authority,expires_ms FROM ui_browser_control WHERE chat_id=?",
            [chat],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, u64>(3)?,
                ))
            },
        )
        .optional()
        .map_err(io::Error::other)?;
    if let Some((epoch, owner, authority, expires)) = prior {
        if epoch == client.epoch && expires > browser_client::milliseconds() && owner != actor {
            if !force {
                return Err(io::Error::other("Browser is controlled by another actor"));
            }
            let mut previous = Client::connect(chat, &owner)?;
            previous.call(
                "release_control",
                json!({}),
                Some(&serde_json::from_str::<Value>(&authority)?),
            )?;
        }
    }
    let result = client.call("claim_control", json!({"ttl_ms":60000}), None)?;
    connection.execute("INSERT INTO ui_browser_control(chat_id,epoch,owner,authority,expires_ms) VALUES (?,?,?,?,?)
        ON CONFLICT(chat_id) DO UPDATE SET epoch=excluded.epoch,owner=excluded.owner,authority=excluded.authority,expires_ms=excluded.expires_ms",
        params![chat,client.epoch,actor,result["authority"].to_string(),result["expires_ms"].as_u64()])
        .map_err(io::Error::other)?;
    Ok((client, result["authority"].clone()))
}

pub(crate) fn release(db: &Database, chat: &str, actor: &str, authority: &Value) -> io::Result<()> {
    let mut client = connect(db, chat, actor)?;
    client.call("release_control", json!({}), Some(authority))?;
    db.lock()
        .map_err(|_| io::Error::other("database unavailable"))?
        .execute(
            "DELETE FROM ui_browser_control WHERE chat_id=? AND owner=? AND authority=?",
            params![chat, actor, authority.to_string()],
        )
        .map_err(io::Error::other)?;
    Ok(())
}

pub(crate) fn clear_control(connection: &Connection, chat: &str) -> io::Result<()> {
    let owner: Option<(String, String)> = connection.query_row(
        "SELECT owner,authority FROM ui_browser_control WHERE chat_id=?", [chat],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional().map_err(io::Error::other)?;
    if let Some((owner, authority)) = owner {
        if let (Ok(mut client), Ok(authority)) = (Client::connect(chat, &owner), serde_json::from_str::<Value>(&authority)) {
            let _ = client.call("release_control", json!({}), Some(&authority));
        }
        connection.execute("DELETE FROM ui_browser_control WHERE chat_id=?", [chat]).map_err(io::Error::other)?;
    }
    connection.execute("DELETE FROM ui_browser_preview WHERE chat_id=?", [chat]).map_err(io::Error::other)?;
    Ok(())
}

pub(crate) fn machine_user(db: &Database, chat: &str) -> io::Result<Option<bool>> {
    let connection = db.lock().map_err(|_| io::Error::other("database unavailable"))?;
    crate::docker_desktop::user_driving(&connection, chat).map_err(io::Error::other)
}

pub(crate) fn release_user_control(connection: &Connection, chat: &str) -> io::Result<()> {
    let prior = connection
        .query_row(
            "SELECT epoch,owner,authority,expires_ms FROM ui_browser_control WHERE chat_id=?",
            [chat],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, u64>(3)?,
                ))
            },
        )
        .optional()
        .map_err(io::Error::other)?;
    if let Some((_epoch, owner, authority, expires)) = prior {
        if owner.as_str() == "user" && expires > browser_client::milliseconds() {
            if let Ok(mut previous) = Client::connect(chat, &owner) {
                if let Ok(auth) = serde_json::from_str::<Value>(&authority) {
                    let _ = previous.call("release_control", json!({}), Some(&auth));
                }
            }
            connection
                .execute(
                    "DELETE FROM ui_browser_control WHERE chat_id=? AND owner='user'",
                    [chat],
                )
                .map_err(io::Error::other)?;
        }
    }
    Ok(())
}

fn base_binding(s: &str) -> &str {
    s.rsplit_once(':').map(|(base, _)| base).unwrap_or(s)
}

fn matches_base(selection: &Value, binding: &str) -> bool {
    let current = selection["binding_id"].as_str().unwrap_or("");
    current == binding || (!current.is_empty() && base_binding(current) == base_binding(binding))
}

fn matches(selection: &Value, binding: &Value) -> io::Result<()> {
    let Some(binding_str) = binding.as_str() else {
        return Err(io::Error::other("browser_binding_changed"));
    };
    if !matches_base(selection, binding_str) {
        return Err(io::Error::other("browser_binding_changed"));
    }
    Ok(())
}

pub(crate) fn view(
    db: &Database,
    chat: &str,
    client: &mut Client,
    selection: Value,
) -> io::Result<Value> {
    let current = control(db, chat, &client.epoch)?;
    let owner = current.as_ref().map(|(owner, _)| owner.as_str());
    let generation = current
        .as_ref()
        .filter(|(owner, _)| owner == "user")
        .map(|(_, authority)| authority["generation"].clone());
    let tabs: Vec<Value> = selection["tabs"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .enumerate()
        .map(|(index, tab)| {
            json!({
                "index":index,"tabHandle":tab["target_id"],"url":tab["url"],"title":tab["title"],
                "active":tab["target_id"]==selection["target_id"]
            })
        })
        .collect();
    let active = tabs
        .iter()
        .find(|tab| tab["active"] == true)
        .cloned()
        .unwrap_or(json!({}));
    let binding = selection["binding_id"].as_str().unwrap_or("");
    let profile = selection["profile"].as_str().unwrap_or("private");
    let profiles = available_profiles(&selection);
    let frame = if active["url"]
        .as_str()
        .is_some_and(|url| !url.is_empty() && url != "about:blank")
    {
        json!({"url":format!("/api/sessions/{chat}/browser/frame?binding={binding}"),"bindingId":binding,
            "version":browser_client::milliseconds()})
    } else {
        Value::Null
    };
    let browser_sessions:Vec<Value>=selection["profiles"].as_array().unwrap_or(&Vec::new()).iter().map(|item|json!({
        "id":item["name"],"backend":"rust","mode":if matches!(item["name"].as_str(),Some("private"|"alternate"|"machine")) {"private"}else{"personal"},
        "connected":item["connected"],"selected":item["selected"],"title":item["name"]
    })).collect();
    let layout = if selection["layout"].is_object() {
        json!({"mode":"panel","width":selection["layout"]["width"],"height":selection["layout"]["height"]})
    } else {
        json!({"mode":"native"})
    };
    let decision_route = crate::ui_browser_decisions::current_route(
        &*db.lock().map_err(|_| io::Error::other("database unavailable"))?, chat).unwrap_or(Value::Null);
    let connected = selection["browser_id"]
        .as_str()
        .is_some_and(|id| !id.is_empty());
    Ok(
        json!({"sessionId":chat,"bindingId":binding,"revision":selection["revision"],
        "decisionRoute":decision_route,"backend":"rust","selectedBackend":"rust","selectedProfile":profile,"availableProfiles":profiles,
        "canClaimUserTabs":matches!(profile,"chrome"|"comet"),"previewOnly":false,
        "inputStatus":selection["input_status"],"connectionStatus":if connected {"connected"}else{"disconnected"},"connected":connected,
        "owned":matches!(profile,"private"|"alternate"|"machine"),"url":active["url"].as_str().unwrap_or(""),
        "title":active["title"].as_str().unwrap_or(""),"tabs":tabs,
        "loginAccess":if connected && matches!(profile,"chrome"|"comet"){json!({"status":"active"})}else{Value::Null},
        "lockedBy":if owner==Some("user"){Some("user")}else{owner.map(|_|"agent")},
        "agentLocked":owner.is_some_and(|owner|owner!="user"),"canControl":owner.is_none() || owner==Some("user"),
        "controlGeneration":generation,"viewport":selection["viewport"],"layout":layout,
        "screencast":{"available":!frame.is_null() && !selection["viewport"].is_null()},
        "previewAcknowledgment":true,
        "targetIdentity":format!("rust:{}:{}",selection["browser_id"].as_str().unwrap_or(""),selection["target_id"].as_str().unwrap_or("")),
        "frameVersion":browser_client::milliseconds(),"frame":frame,"browserSessions":browser_sessions}),
    )
}

fn available_profiles(selection: &Value) -> Vec<String> {
    // Personal profiles are supported choices even when not yet connected.
    // A missing endpoint is explained during attachment rather than hidden.
    if selection["profile"] == "machine" {
        return vec!["machine".to_owned()];
    }
    let mut profiles = vec!["private".to_owned(), "alternate".to_owned(),"chrome".to_owned(),"comet".to_owned()];
    for profile in selection["available_profiles"]
        .as_array()
        .unwrap_or(&Vec::new())
    {
        if let Some(profile) = profile.as_str().filter(|value| *value == "lightpanda") {
            profiles.push(profile.into())
        }
    }
    profiles
}

/// Configured and locally enabled endpoints are discovered by the host. Model
/// and page content cannot supply an attachment endpoint.
fn personal_endpoint(profile: &str) -> Option<String> {
    let key = match profile {
        "chrome" => "KAGEHA_CHROME_CDP_ENDPOINT",
        "comet" => "KAGEHA_COMET_CDP_ENDPOINT",
        _ => return None,
    };
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
        .or_else(|| browser_client::profiles::discover().remove(profile))
}

/// Names only. Cookies and unmanaged tabs stay inside each browser.
pub(crate) fn eligible_personal_profiles() -> Vec<&'static str> {
    ["chrome", "comet"].into_iter().filter(|profile| personal_endpoint(profile).is_some()).collect()
}

fn requested_profile<'a>(intent: &'a str, selected: &'a str, available: &[&'a str]) -> io::Result<&'a str> {
    match intent {
        "selected" => Ok(selected),
        "private" if selected == "alternate" => Ok(selected),
        "private" | "chrome" | "comet" => Ok(intent),
        "personal" if matches!(selected, "chrome" | "comet") => Ok(selected),
        "personal" => match available {
            [only] => Ok(only),
            [] => Err(io::Error::other("Select or connect the desired signed-in profile in the Browser panel, then return control.")),
            _ => Err(io::Error::other("Multiple personal browsers are available. Use the one named in chat or ask the user to select one in the Browser panel.")),
        },
        _ => Err(io::Error::other("Unknown browser profile choice")),
    }
}

pub(crate) fn select_for_open(
    host: &crate::ui_tools::Tools<'_>,
    client: &mut Client,
    selection: &Value,
    intent: &str,
    url: &str,
) -> io::Result<Value> {
    let current = selection["profile"].as_str().unwrap_or("private");
    if host.machine.is_some() {
        if current != "machine" || !matches!(intent, "selected" | "private") {
            return Err(io::Error::other("This turn uses Chromium in the Docker workstation"));
        }
        require_connected(selection)?;
        return Ok(selection.clone());
    }

    let personal = |profile: &str| matches!(profile, "chrome" | "comet");
    let check_control = || -> io::Result<()> {
        if control(host.db, host.chat, &client.epoch)?.is_some_and(|(owner, _)| owner == "user") {
            return Err(io::Error::other("The user has browser control. Continue after they return it."));
        }
        Ok(())
    };
    check_control()?;
    let choices: Vec<_> = if intent == "personal" && !personal(current) {
        eligible_personal_profiles()
    } else {
        Vec::new()
    };
    let profile = requested_profile(intent, current, &choices)?;
    if profile == current && (!personal(profile) || selection["browser_id"].as_str().is_some_and(|id| !id.is_empty())) {
        return Ok(reopen_profile(client, selection)?.unwrap_or_else(|| selection.clone()));
    }

    let mut params = json!({"profile":profile,"binding_id":selection["binding_id"]});
    if personal(profile) {
        let endpoint = personal_endpoint(profile).ok_or_else(|| io::Error::other(
            format!("Connect {profile} for browser control in the Browser panel, then return control.")))?;
        let destination = url::Url::parse(url).ok()
            .filter(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())
            .ok_or_else(|| io::Error::other("Use an HTTP or HTTPS site for a personal profile"))?;
        let origin = destination.origin().ascii_serialization();
        let scope = json!({"binding_id":selection["binding_id"],"profile":profile,"url":url,
            "endpoint_digest":kageha_policy::input_digest(&json!(endpoint))});
        let detail = format!("Share the {profile} browser profile with this chat and open {origin}?");
        if !crate::ui_approvals::require(host.db, host.chat, host.turn, "browser_open", &detail, &scope, host.cancel)? {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "Sharing the personal browser profile was declined."));
        }
        check_control()?;
        params["endpoint"] = json!(endpoint);
        params["consent"] = json!(true);
    }
    client.call("switch_profile", params, None)?;
    let updated = client.selection()?;
    if updated["profile"] != profile {
        return Err(io::Error::other("Browser selection changed. Inspect browser_tabs before continuing."));
    }
    Ok(updated)
}

/// A login link in the navigation bar is common on public pages. Classify the
/// destination and visible interstitial together before suggesting a handoff.
pub(crate) fn access_hint(page: &Value, selection: &Value) -> Option<Value> {
    let tab = selection["tabs"].as_array().and_then(|tabs| tabs.iter()
        .find(|tab| tab["target_id"] == page["target_id"] && tab["url"] == page["url"]));
    let title = page["title"].as_str().or_else(|| tab.and_then(|tab| tab["title"].as_str()))
        .unwrap_or("").trim().to_ascii_lowercase();
    let challenge = page["text"].as_array().into_iter().flatten().filter_map(Value::as_str)
        .take(64).any(|line| matches!(line.trim().trim_end_matches('.').to_ascii_lowercase().as_str(),
            "performing security verification" | "verify you are human" | "checking your browser"));
    if challenge && (title.starts_with("just a moment") || title.contains("security verification") || title.is_empty()) {
        return Some(json!({"kind":"security_check","next":"Hand the interactive check to the user and resume after they return control."}));
    }
    let url = url::Url::parse(page["url"].as_str()?).ok()?;
    let last = url.path_segments()?.rfind(|part| !part.is_empty())?;
    if !matches!(last.to_ascii_lowercase().as_str(), "login" | "signin" | "sign-in" | "authwall") {
        return None;
    }
    if matches!(selection["profile"].as_str(), Some("chrome" | "comet" | "cdp")) {
        return Some(json!({"kind":"sign_in","next":"The selected profile reached sign-in. Request a user handoff if authentication is needed."}));
    }
    Some(json!({"kind":"sign_in","eligible_personal_profiles":eligible_personal_profiles(),
        "next":"For requested account data, call browser_open with profile=personal and the original task URL, or use an integration that provides the data. If the user chose private, keep that choice and offer handoff. This page contains no account result."}))
}

fn notify_panel_choice(db: &Database, chat: &str, profile: &Value, action: &str) -> io::Result<()> {
    use kageha_runtime::lifecycle::{RunStore, Steering};
    let profile = match profile.as_str() {
        Some(p @ ("chrome" | "comet" | "private" | "alternate" | "lightpanda" | "embedded" | "machine" | "docker")) => p,
        _ => "selected",
    };
    let turn: Option<String> = db.lock().map_err(|_| io::Error::other("Database unavailable"))?
        .query_row(&format!("SELECT request_id FROM native_chat_turns WHERE chat_id=? AND status IN {}", crate::ui_turn_resume::LIVE_STATUSES),
            [chat], |row| row.get(0)).optional().map_err(io::Error::other)?;
    let Some(request) = turn else { return Ok(()); };
    let turn = crate::webui::turn_id(chat, &request);
    let store = crate::runtime_host::store(db).map_err(|(_, e)| io::Error::other(e))?;
    if let Some(run) = store.find_turn(&turn).map_err(io::Error::other)?.filter(|r| r.spec.session == chat && !r.state.terminal() && !r.cancel_requested) {
        // No tab title or URL enters the trusted steering channel.
        let note = format!("Browser panel update: {action}. The selected profile is {profile}. Refresh browser_tabs and observe the selected tab before continuing. Respect user control.");
        if let Err(error) = store.steer(&run.spec.id, &Steering { id: uuid::Uuid::new_v4().to_string(), body: note }) {
            if store.get(&run.spec.id).map_err(io::Error::other)?.is_some_and(|r| !r.state.terminal() && !r.cancel_requested) {
                return Err(io::Error::other(error));
            }
        }
    }
    Ok(())
}

pub(crate) fn state(db: &Database, chat: &str) -> io::Result<Value> {
    let mut client = connect(db, chat, "user")?;
    let selection = client.selection()?;
    if selection["profile"] == "machine" && machine_user(db, chat)? == Some(true)
        && control(db, chat, &client.epoch)?.is_none() {
        let _guard = crate::docker_desktop::lock(chat);
        (client, _) = claim_client(db, chat, "user", true, client)?;
    }
    view(db, chat, &mut client, selection)
}

fn operation(db: &Database, chat: &str, name: &str, payload: &Value) -> io::Result<Value> {
    let mut client = connect_with_start(db, chat, "user", name == "connect")?;
    let mut selected = client.selection()?;
    // Returning/renewing an existing lease is fenced by owner, epoch and
    // generation below. A navigation may change the page binding meanwhile;
    // it must not prevent its owner from relinquishing control.
    if !(name=="lease" && matches!(payload["action"].as_str(),Some("return"|"renew")))
        && !(name == "connect" && selected["profile"] == "machine") {
        matches(&selected, &payload["bindingId"])?;
    }
    if !matches!(name, "lease")
        && control(db, chat, &client.epoch)?.is_some_and(|(owner, _)| owner != "user")
    {
        return Err(io::Error::other("The agent is using this browser"));
    }
    match name {
        "connect" | "promote" => {
            let profile = if name == "connect" {
                selected["profile"].as_str().unwrap_or("private")
            } else {
                payload["browserSession"]
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .or_else(|| {
                        payload["target"]
                            .as_str()
                            .and_then(|value| value.strip_prefix("profile:"))
                    })
                    .ok_or_else(|| io::Error::other("Choose a browser profile"))?
            };
            if !available_profiles(&selected)
                .iter()
                .any(|item| item == profile)
            {
                return Err(io::Error::other("browser_adapter_unavailable"));
            }
            let mut params = json!({"profile":profile,"binding_id":selected["binding_id"]});
            if matches!(profile, "chrome" | "comet") {
                if payload["consent"] != true {
                    return Err(io::Error::other("browser_profile_consent_required"));
                }
                let endpoint = personal_endpoint(profile).ok_or_else(||io::Error::other(
                    format!("{profile} is not enabled for browser control. Connect a CDP-enabled profile or configure its endpoint.")))?;
                params["consent"] = json!(true);
                params["endpoint"] = json!(endpoint);
            }
            client.call("switch_profile", params, None)?;
        }
        "revoke-login" => {
            client.call(
                "detach_profile",
                json!({"binding_id":selected["binding_id"]}),
                None,
            )?;
        }
        "lease" => {
            let action = payload["action"].as_str().unwrap_or("take");
            if selected["profile"] == "machine" && machine_user(db,chat)?.is_some() && matches!(action, "take" | "return") {
                let _guard = crate::docker_desktop::lock(chat);
                if action == "return" {
                    let active = control(db, chat, &client.epoch)?
                        .ok_or_else(|| io::Error::other("Browser control expired"))?;
                    if active.0 != "user" || payload["controlGeneration"] != active.1["generation"] {
                        return Err(io::Error::other("Browser control changed"));
                    }
                }
                {
                    let connection = db.lock().map_err(|_| io::Error::other("database unavailable"))?;
                    if crate::docker_desktop::user_driving(&connection, chat).map_err(io::Error::other)?.is_none() {
                        return Err(io::Error::other("The Linux desktop selection changed"));
                    }
                    crate::docker_desktop::change_driver(&connection, chat, action == "take")?;
                }
                if action == "take" {
                    (client, _) = claim_client(db, chat, "user", true, client)?;
                }
                return view(db, chat, &mut client, selected);
            }
            if action == "take" || action == "renew" {
                if action == "renew" {
                    let prior = control(db, chat, &client.epoch)?
                        .ok_or_else(|| io::Error::other("Browser control expired"))?;
                    if prior.0 != "user" || payload["controlGeneration"] != prior.1["generation"] {
                        return Err(io::Error::other("Browser control changed"));
                    }
                }
                claim(db, chat, "user", action == "take")?;
            } else if action == "return" {
                let prior = control(db, chat, &client.epoch)?
                    .ok_or_else(|| io::Error::other("Browser control expired"))?;
                if prior.0 != "user" || payload["controlGeneration"] != prior.1["generation"] {
                    return Err(io::Error::other("Browser control changed"));
                }
                release(db, chat, "user", &prior.1)?;
            } else {
                return Err(io::Error::other("Unsupported browser lease operation"));
            }
        }
        "action" | "layout" => {
            let action = payload["action"].as_str().unwrap_or("");
            if name == "action" && action == "tab_select" {
                client.call(
                    "select_tab",
                    json!({"binding_id":selected["binding_id"],"browser_id":selected["browser_id"],
                    "target_id":payload["tabHandle"]}),
                    None,
                )?;
            } else if name == "action" && matches!(action, "list_user_tabs" | "claim_user_tab") {
                let candidates = if action == "list_user_tabs" {
                    Some(client.call("list_user_tabs", json!({}), None)?)
                } else {
                    if payload["candidateBindingId"] != selected["binding_id"] {
                        return Err(io::Error::other("browser_binding_changed"));
                    }
                    client.call("claim_user_tab",json!({"binding_id":selected["binding_id"],"browser_id":selected["browser_id"],
                        "target_id":payload["tabHandle"],"url":payload["url"],"title":payload["title"]}),None)?;
                    None
                };
                let current = client.selection()?;
                if action == "claim_user_tab" { notify_panel_choice(db, chat, &current["profile"], "the user shared an open tab")?; }
                let mut result = view(db, chat, &mut client, current)?;
                if let Some(candidates) = candidates {
                    result["userTabCandidates"] = candidates["tabs"].clone()
                }
                return Ok(result);
            } else {
                let (authority, temporary) = if name == "layout" {
                    if let Some((owner, authority)) = control(db, chat, &client.epoch)? {
                        if owner != "user" {
                            return Err(io::Error::other("The agent is using this browser"));
                        }
                        (authority, false)
                    } else {
                        let (_, authority) = claim(db, chat, "user", false)?;
                        (authority, true)
                    }
                } else {
                    let (owner, authority) = control(db, chat, &client.epoch)?
                        .ok_or_else(|| io::Error::other("Take control before sending input"))?;
                    if owner != "user" || payload["controlGeneration"] != authority["generation"] {
                        return Err(io::Error::other("Browser control changed"));
                    }
                    (authority, false)
                };
                let command = if name == "layout" {
                    if !matches!(payload["mode"].as_str(), Some("panel" | "native")) {
                        return Err(io::Error::other("Unsupported layout mode"));
                    }
                    let dpr = payload["dpr"].as_f64().unwrap_or(1.0).clamp(1.0, 3.0);
                    json!({"action":"layout","size":if payload["mode"]=="panel" {json!({"width":payload["width"],"height":payload["height"],"dpr":dpr})}else{Value::Null}})
                } else {
                    let mut command = json!({"action":action});
                    let keys: &[&str] = match action {
                        "click" => &["x", "y", "button"],
                        "wheel" => &["x", "y", "deltaX", "deltaY"],
                        "type" => &["text"],
                        "press" => &["key"],
                        "navigate" => &["url"],
                        "back" | "forward" | "reload" | "focus" | "tab_new" | "tab_close" => &[],
                        _ => return Err(io::Error::other("Unsupported browser action")),
                    };
                    if action == "tab_close" && payload["tabHandle"] != selected["target_id"] {
                        return Err(io::Error::other("browser_binding_changed"));
                    }
                    for key in keys {
                        command[match *key {
                            "deltaX" => "delta_x",
                            "deltaY" => "delta_y",
                            key => key,
                        }] = payload[*key].clone();
                    }
                    if action == "navigate" {
                        if let Some(raw_url) = payload["url"].as_str() {
                            let trimmed = raw_url.trim();
                            let normalized = if !trimmed.is_empty()
                                && !trimmed.contains("://")
                                && !trimmed.starts_with("about:")
                                && !trimmed.starts_with("data:")
                                && !trimmed.starts_with("javascript:")
                            {
                                if trimmed.starts_with("localhost")
                                    || trimmed.starts_with("127.0.0.1")
                                    || trimmed.starts_with("[::1]")
                                {
                                    format!("http://{trimmed}")
                                } else {
                                    format!("https://{trimmed}")
                                }
                            } else {
                                trimmed.to_string()
                            };
                            command["url"] = serde_json::Value::String(normalized);
                        }
                    }
                    command
                };
                let result = client.call(
                    "panel_action",
                    json!({"selection":browser_client::target(&selected),
                    "viewport":selected["viewport"],"input":command}),
                    Some(&authority),
                );
                if temporary {
                    let _ = release(db, chat, "user", &authority);
                }
                result?;
            }
        }
        _ => return Err(io::Error::other("Unsupported browser operation")),
    }
    selected = client.selection()?;
    let change = match (name, payload["action"].as_str()) {
        ("promote", _) => Some("the user chose a browser profile"),
        ("revoke-login", _) => Some("the user detached the personal browser"),
        ("action", Some("tab_select")) => Some("the user selected a managed tab"),
        ("lease", Some("return")) => Some("the user returned browser control"),
        _ => None,
    };
    if let Some(change) = change { notify_panel_choice(db, chat, &selected["profile"], change)?; }
    view(db, chat, &mut client, selected)
}

struct Frames {
    client: Client,
    binding: String,
    viewport: String,
    buffer: std::io::Cursor<Vec<u8>>,
    first: bool,
    last_frame_hash: u64,
    unchanged_polls: u32,
}
impl Read for Frames {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        loop {
            let count = self.buffer.read(out)?;
            if count > 0 {
                return Ok(count);
            }
            if !self.first {
                let sleep_ms = if self.unchanged_polls > 0 { 180 } else { 90 };
                std::thread::sleep(Duration::from_millis(sleep_ms));
            }
            self.first = false;
            let selection = self.client.selection()?;
            if !matches_base(&selection, &self.binding) {
                return Ok(0);
            }
            let geometry = format!(
                "{}x{}",
                selection["viewport"]["width"], selection["viewport"]["height"]
            );
            if !self.viewport.is_empty() && geometry != self.viewport {
                return Ok(0);
            }
            let bytes = self.client.frame(&selection)?;
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            bytes.hash(&mut hasher);
            let frame_hash = hasher.finish();

            if frame_hash == self.last_frame_hash && self.unchanged_polls < 25 {
                self.unchanged_polls += 1;
                continue;
            }
            self.last_frame_hash = frame_hash;
            self.unchanged_polls = 0;

            let mut frame = format!(
                "--kageha-frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
                bytes.len()
            )
            .into_bytes();
            frame.extend_from_slice(&bytes);
            frame.extend_from_slice(b"\r\n");
            self.buffer = std::io::Cursor::new(frame);
        }
    }
}

pub(crate) fn handle(
    mut request: Request,
    db: &Database,
    chat: &str,
    name: &str,
) -> io::Result<()> {
    if let Err(error) = crate::ui_uploads::root_path(db, chat) {
        server::response(request, 404, json!({"error":error.to_string()}));
        return Ok(());
    }
    let url = request.url().to_owned();
    let binding =
        super::webui::form_decode(&super::webui::query(&url, "binding")).unwrap_or_default();
    if request.method() == &Method::Post && name == "paint" {
        let payload = super::webui::body(&mut request)?;
        let result = (|| -> io::Result<Value> {
            let mut client = connect(db, chat, "user")?;
            let selection = client.selection()?;
            matches(&selection, &payload["bindingId"])?;
            if payload["displayed"] == false {
                db.lock().map_err(|_| io::Error::other("database unavailable"))?
                    .execute("DELETE FROM ui_browser_preview WHERE chat_id=? AND binding_id=?",
                        params![chat, payload["bindingId"].as_str().unwrap_or("")])
                    .map_err(io::Error::other)?;
                return preview_status(db, chat, &selection);
            }
            if payload["displayed"] != true {
                return Err(io::Error::other("A paint acknowledgment needs displayed=true or false"));
            }
            if selection["target_id"].as_str().is_none_or(str::is_empty) {
                return Err(io::Error::other("No tab is selected"));
            }
            db.lock().map_err(|_| io::Error::other("database unavailable"))?
                .execute(
                    "INSERT INTO ui_browser_preview(chat_id,binding_id,painted_ms) VALUES (?,?,?)
                     ON CONFLICT(chat_id) DO UPDATE SET binding_id=excluded.binding_id,painted_ms=excluded.painted_ms",
                    params![chat, payload["bindingId"].as_str().unwrap_or(""), browser_client::milliseconds()],
                ).map_err(io::Error::other)?;
            preview_status(db, chat, &selection)
        })();
        match result {
            Ok(value) => server::response(request, 200, value),
            Err(error) => server::response(request, 409, json!({"error":error.to_string()})),
        }
        return Ok(());
    }
    if request.method() == &Method::Get && matches!(name, "frame" | "stream") {
        let setup = (|| -> io::Result<_> {
            let mut client = connect(db, chat, "user")?;
            let selected = client.selection()?;
            matches(&selected, &json!(binding))?;
            Ok((client, selected))
        })();
        let (mut client, selected) = match setup {
            Ok(value) => value,
            Err(error) => {
                server::response(request, 409, json!({"error":error.to_string()}));
                return Ok(());
            }
        };
        if name == "frame" {
            let bytes = match client.frame(&selected) {
                Ok(bytes) => bytes,
                Err(error) => {
                    server::response(request, 409, json!({"error":error.to_string()}));
                    return Ok(());
                }
            };
            let _ = request.respond(
                Response::from_data(bytes)
                    .with_header(Header::from_bytes("Content-Type", "image/jpeg").unwrap())
                    .with_header(Header::from_bytes("Cache-Control", "no-store").unwrap()),
            );
        } else {
            let viewport = super::webui::query(&url, "viewport");
            let frames = Frames {
                client,
                binding,
                viewport,
                buffer: std::io::Cursor::new(Vec::new()),
                first: true,
                last_frame_hash: 0,
                unchanged_polls: 0,
            };
            let _ = request.respond(Response::new(
                StatusCode(200),
                vec![
                    Header::from_bytes(
                        "Content-Type",
                        "multipart/x-mixed-replace; boundary=kageha-frame",
                    )
                    .unwrap(),
                    Header::from_bytes("Cache-Control", "no-store").unwrap(),
                ],
                frames,
                None,
                None,
            ));
        }
        return Ok(());
    }
    let result = if request.method() == &Method::Get && name == "state" {
        state(db, chat)
    } else if request.method() == &Method::Post {
        let payload = super::webui::body(&mut request)?;
        operation(db, chat, name, &payload)
    } else {
        Err(io::Error::other("Unsupported browser operation"))
    };
    match result {
        Ok(value) => server::response(request, 200, value),
        Err(error) => server::response(request, 409, json!({"error":error.to_string()})),
    }
    Ok(())
}

#[cfg(test)]
mod preview_tests {
    use super::*;

    #[test]
    fn panel_switch_steers_only_the_current_run_in_the_same_chat() {
        use kageha_runtime::lifecycle::RunStore;
        let temp = tempfile::tempdir().unwrap();
        let connection = Connection::open(temp.path().join("app.sqlite")).unwrap();
        connection.execute_batch("CREATE TABLE native_chat_turns(chat_id,request_id,status);
            INSERT INTO native_chat_turns VALUES ('a','old','complete'),('a','current','running'),('b','other','running');").unwrap();
        let db = crate::agent_host::DatabaseState::open(connection).unwrap();
        let store = crate::runtime_host::store(&db).unwrap();
        for (chat, request) in [("a","old"), ("a","current"), ("b","other")] {
            let spec = crate::runtime_host::spec(temp.path(),chat,request,request,"main","Read the fixture",0,None,None,vec![],Value::Null,4);
            store.stage(&spec, 10).unwrap();
        }
        notify_panel_choice(&db, "a", &json!("comet"), "the user chose a browser profile").unwrap();
        let received = store.steering("current").unwrap();
        assert_eq!(received.len(), 1);
        assert!(received[0].body.contains("selected profile is comet"));
        assert!(store.steering("old").unwrap().is_empty());
        assert!(store.steering("other").unwrap().is_empty());
    }

    #[test]
    fn browser_choice_respects_selected_sessions_and_asks_on_ambiguity() {
        assert_eq!(requested_profile("selected", "private", &["chrome"]).unwrap(), "private");
        assert_eq!(requested_profile("personal", "comet", &["chrome"]).unwrap(), "comet");
        assert_eq!(requested_profile("personal", "private", &["chrome"]).unwrap(), "chrome");
        assert!(requested_profile("personal", "private", &["chrome", "comet"]).is_err());
        assert!(requested_profile("personal", "private", &[]).is_err());
        assert_eq!(requested_profile("private", "chrome", &[]).unwrap(), "private");
        assert_eq!(requested_profile("comet", "chrome", &[]).unwrap(), "comet");
    }

    #[test]
    fn distinguishes_a_public_page_a_login_redirect_and_a_security_interstitial() {
        let selection = json!({"profile":"private"});
        let mut page = json!({"url":"https://example.org/jobs?next=/login", "text":["Sign in"]});
        assert!(access_hint(&page, &selection).is_none());
        page["url"] = json!("https://example.org/uas/login/");
        assert_eq!(access_hint(&page, &selection).unwrap()["kind"], "sign_in");
        assert!(access_hint(&page, &json!({"profile":"comet"})).unwrap()["next"].as_str().unwrap().contains("handoff"));
        page["text"] = json!(["Performing security verification"]);
        assert_eq!(access_hint(&page, &selection).unwrap()["kind"], "security_check");
    }

    #[test]
    fn signed_in_choices_do_not_depend_on_launcher_environment() {
        let profiles=available_profiles(&json!({}));
        for name in ["private","alternate","chrome","comet"] {assert!(profiles.iter().any(|profile|profile==name));}
    }

    #[test]
    fn displayed_preview_requires_fresh_evidence_for_the_current_binding() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        initialize(&connection).unwrap();
        let db = crate::agent_host::DatabaseState::open(connection).unwrap();
        let selected = json!({"binding_id": "epoch:tab:1", "target_id": "original"});
        assert_eq!(preview_status(&db, "chat", &selected).unwrap()["displayed"], false);
        let now = browser_client::milliseconds();
        for (binding, at, expected) in [
            ("epoch:tab:1", now, true),
            ("epoch:other:2", now, false),
            ("epoch:tab:1", now - 16000, false),
            ("epoch:tab:1", now + 60000, false),
        ] {
            db.lock().unwrap().execute(
                "INSERT OR REPLACE INTO ui_browser_preview(chat_id,binding_id,painted_ms) VALUES (?,?,?)",
                params!["chat", binding, at],
            ).unwrap();
            assert_eq!(preview_status(&db, "chat", &selected).unwrap()["displayed"], expected);
            assert_eq!(preview_status(&db, "another-chat", &selected).unwrap()["displayed"], false);
        }
    }

    #[test]
    fn opening_a_page_without_a_connected_browser_fails_with_a_specific_non_retryable_error() {
        let detail = |selection: Value| -> Value {
            let error = require_connected(&selection).unwrap_err();
            serde_json::from_str(&error.to_string()).unwrap()
        };
        let comet = detail(json!({"profile":"comet","browser_id":null,"binding_id":"b:1"}));
        assert_eq!(comet["error_code"], "browser_profile_disconnected");
        assert_eq!(comet["retryable"], false);
        let text = comet["summary"].as_str().unwrap();
        assert!(text.contains("nothing was sent") && text.contains("Browser panel"), "{text}");
        // An empty id is the same as no browser.
        assert_eq!(detail(json!({"profile":"private","browser_id":""}))["error_code"], "browser_not_connected");
        assert!(detail(json!({"profile":"private"}))["summary"].as_str().unwrap().contains("browser_tabs"));
        // A connected browser passes.
        assert!(require_connected(&json!({"profile":"comet","browser_id":"d0d6"})).is_ok());
    }
}
