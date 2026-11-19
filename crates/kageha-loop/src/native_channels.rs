//! Native channel adapters, durable incoming messages and non-replayed delivery intents.
use crate::{
    native_io, server, ui_settings,
    ui_store::{self as store, db_error, ApiResult},
    webui::{self, Controls, Database},
};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use std::{io, path::Path, sync::atomic::AtomicBool, time::Duration};
use tiny_http::{Method, Request};
const KIND: &str = "channel";
pub(crate) fn initialize(db: &rusqlite::Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS ui_channel_state(id TEXT PRIMARY KEY,connected INTEGER NOT NULL,error TEXT NOT NULL,observed REAL NOT NULL);")
}
fn field(key: &str, label: &str, secret: bool, required: bool) -> Value {
    json!({"key":key,"label":label,"secret":secret,"required":required,"placeholder":"","help_text":""})
}
fn channel_process_is_running() -> bool {
    let path = if let Ok(val) = std::env::var("KAGEHA_HOME") {
        std::path::PathBuf::from(val).join("daemon/channels.lock")
    } else if let Ok(h) = std::env::var("HOME") {
        std::path::PathBuf::from(h).join(".kageha/daemon/channels.lock")
    } else {
        std::path::PathBuf::from(".kageha/daemon/channels.lock")
    };
    if let Ok(file) = std::fs::OpenOptions::new().read(true).write(true).open(&path) {
        use fs2::FileExt;
        if file.try_lock_exclusive().is_err() {
            return true;
        }
        let _ = file.unlock();
        return false;
    }
    false
}
fn qr_state_path() -> std::path::PathBuf {
    if let Ok(val) = std::env::var("KAGEHA_HOME") {
        std::path::PathBuf::from(val).join("platforms/whatsapp/qr_state.json")
    } else if let Ok(home) = std::env::var("HOME") {
        std::path::PathBuf::from(home).join(".kageha/platforms/whatsapp/qr_state.json")
    } else {
        std::path::PathBuf::from(".kageha/platforms/whatsapp/qr_state.json")
    }
}
fn read_qr_state() -> Value {
    let path = qr_state_path();
    if !path.is_file() {
        return json!({"status": "not_started"});
    }
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return json!({"status": "not_started"}),
    };
    let mut row: Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return json!({"status": "not_started"}),
    };
    let live_states = ["ready", "connecting", "qr", "reconnecting"];
    let status_str = row["status"].as_str().unwrap_or("");
    if live_states.contains(&status_str) {
        let updated_at = row["updated_at"].as_f64().unwrap_or(0.0);
        let now = store::now();
        if (now - updated_at) > 120.0 && !channel_process_is_running() {
            row["status"] = json!("not_started");
            row["stale"] = json!(true);
        }
    }
    row
}
fn ensure_channel_listener() {
    if channel_process_is_running() {
        return;
    }
    let binary = if std::path::Path::new("/opt/miniconda3/bin/kageha").exists() {
        "/opt/miniconda3/bin/kageha"
    } else {
        "kageha"
    };
    let _ = std::process::Command::new(binary)
        .args(["channels", "run", "--whatsapp-qr"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}
fn config(db: &Database, id: &str) -> ApiResult<Value> {
    let c = db.lock().map_err(db_error)?;
    let mut row = store::get(&c, KIND, id)
        .map_err(db_error)?
        .unwrap_or(json!({"id":id,"enabled":false}));
    let secret = if id == "whatsapp_qr" {
        String::new()
    } else {
        ui_settings::secret(&c, &format!("channel:{id}")).unwrap_or_default()
    };
    row["secret"] = json!(secret);
    Ok(row)
}
fn status(db: &Database, id: &str) -> ApiResult<Value> {
    let mut row = config(db, id)?;
    let secret = row
        .as_object_mut()
        .unwrap()
        .remove("secret")
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default();
    if id == "whatsapp_qr" {
        let running = channel_process_is_running();
        let qr = read_qr_state();
        let live = qr.get("status").and_then(Value::as_str).unwrap_or("not_started");
        let allow_all = row.get("allow_all_users").and_then(Value::as_bool).unwrap_or(false);
        let allowed = row.get("allowed_users").and_then(Value::as_str).unwrap_or("");
        let configured = row["enabled"] == true && (allow_all || !allowed.trim().is_empty());
        let connected = running && live == "ready";
        row["configured"] = json!(configured);
        row["connected"] = json!(connected);
        row["listener_running"] = json!(running);
        row["live_status"] = json!(live);
        row["dependencies_ready"] = json!(true);
        row["allow_all_users"] = json!(allow_all);
        row["runtime_available"] = json!(true);
        if let Some(err) = qr.get("error").and_then(Value::as_str) {
            row["error"] = json!(err);
        }
        return Ok(row);
    }
    row["configured"] = json!(!secret.is_empty() && row["enabled"] == true);
    row["bot_token_configured"] = json!(!secret.is_empty());
    row["runtime_available"] =
        json!(id == "telegram" || row["base_url"].as_str().is_some_and(|s| !s.is_empty()));
    row["listener_running"] = row["configured"].clone();
    let state: Option<(bool, String, f64)> = db
        .lock()
        .map_err(db_error)?
        .query_row(
            "SELECT connected,error,observed FROM ui_channel_state WHERE id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(db_error)?;
    row["connected"] = json!(
        row["configured"] == true
            && state
                .as_ref()
                .is_some_and(|v| v.0 && v.2 > store::now() - 90.0)
    );
    row["live_status"] = json!(if row["configured"] != true {
        "off"
    } else if row["connected"] == true {
        "connected"
    } else {
        "connecting"
    });
    if let Some((_, error, _)) = state {
        row["error"] = json!(error);
    }

    Ok(row)
}
fn call(cfg: &Value, action: &str, payload: &Value, cancel: &AtomicBool) -> ApiResult<Value> {
    let telegram = cfg["id"] == "telegram" && cfg["protocol"] != "gateway";
    let url = if telegram {
        format!(
            "{}/bot{}/{}",
            cfg["base_url"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or("https://api.telegram.org"),
            cfg["secret"].as_str().unwrap_or(""),
            action
        )
    } else {
        format!(
            "{}/{}",
            store::text(cfg, "base_url", 2048)?.trim_end_matches('/'),
            action
        )
    };
    let secret = cfg["secret"].as_str().unwrap_or("");
    if secret.is_empty(){return Err((403,"Channel credential is revoked or unavailable".into()));}
    if !secret.is_empty() && !url.starts_with("https://") {
        return Err((400, "Channel credentials require HTTPS".into()));
    }
    let headers = if telegram {
        vec![]
    } else {
        vec![("Authorization".into(), format!("Bearer {secret}"))]
    };
    let result = native_io::json(&url, "POST", &headers, payload, cancel).map_err(|_| {
        (
            502,
            "Channel request failed; a dispatched send may have an uncertain outcome".into(),
        )
    })?;
    if result["ok"] == false {
        return Err((502, "Channel rejected the request".into()));
    }
    Ok(result)
}
fn allowed(cfg: &Value, sender: &str) -> bool {
    cfg["allow_all_users"] == true
        || cfg["allowed_users"]
            .as_str()
            .unwrap_or("")
            .split([',', ';'])
            .any(|v| !v.trim().is_empty() && v.trim() == sender)
}
fn send(cfg: &Value, to: &str, text: &str, cancel: &AtomicBool) -> ApiResult<Value> {
    if to.is_empty() || to.len() > 256 || text.len() > 16000 {
        return Err((400, "Invalid channel destination or message".into()));
    }
    if cfg["id"] == "telegram" && cfg["protocol"] != "gateway" {
        call(
            cfg,
            "sendMessage",
            &json!({"chat_id":to,"text":text.chars().take(4000).collect::<String>()}),
            cancel,
        )
    } else {
        call(cfg, "send", &json!({"to":to,"text":text}), cancel)
    }
}
pub(crate) fn handle(mut request: Request, db: &Database) -> io::Result<()> {
    let url = request.url().to_owned();
    let path = url.split('?').next().unwrap_or("");
    if path.ends_with("/qr.svg") && request.method() == &Method::Get {
        let id = path
            .trim_start_matches("/api/channels/")
            .trim_end_matches("/qr.svg");
        let result: ApiResult<String> = (|| {
            if id == "whatsapp_qr" {
                let state = read_qr_state();
                let raw = state.get("qr").and_then(Value::as_str).unwrap_or("");
                if raw.is_empty() {
                    let mut found: Option<String> = None;
                    if let Ok(cfg) = config(db, id) {
                        if cfg["protocol"] == "gateway" && cfg["enabled"] == true {
                            if let Ok(value) = call(&cfg, "qr", &json!({}), &AtomicBool::new(false)) {
                                if let Ok(qr_str) = store::text(&value, "qr", 4096) {
                                    found = Some(qr_str.to_owned());
                                }
                            }
                        }
                    }
                    match found {
                        Some(qr_val) => {
                            let code = qrcode::QrCode::new(qr_val.as_bytes())
                                .map_err(|_| (400, "QR payload is invalid or too large".into()))?;
                            Ok(code
                                .render::<qrcode::render::svg::Color>()
                                .min_dimensions(256, 256)
                                .build())
                        }
                        None => Err((404, "No QR code available".into())),
                    }
                } else {
                    let code = qrcode::QrCode::new(raw.as_bytes())
                        .map_err(|_| (400, "QR payload is invalid or too large".into()))?;
                    Ok(code
                        .render::<qrcode::render::svg::Color>()
                        .min_dimensions(256, 256)
                        .build())
                }
            } else {
                let cfg = config(db, id)?;
                if cfg["enabled"] != true {
                    return Err((409, "Channel is disabled".into()));
                }
                let value = call(&cfg, "qr", &json!({}), &AtomicBool::new(false))?;
                let raw = store::text(&value, "qr", 4096)?;
                let code = qrcode::QrCode::new(raw.as_bytes())
                    .map_err(|_| (400, "QR payload is invalid or too large".into()))?;
                Ok(code
                    .render::<qrcode::render::svg::Color>()
                    .min_dimensions(256, 256)
                    .build())
            }
        })();
        match result {
            Ok(svg) => {
                let _ = request.respond(
                    tiny_http::Response::from_string(svg)
                        .with_header(
                            tiny_http::Header::from_bytes("Content-Type", "image/svg+xml").unwrap(),
                        )
                        .with_header(
                            tiny_http::Header::from_bytes("Cache-Control", "no-store").unwrap(),
                        ),
                );
            }
            Err((code, error)) => server::response(request, code, json!({"error":error})),
        }
        return Ok(());
    }
    let method = request.method().clone();
    let payload = if matches!(method, Method::Put | Method::Post) {
        webui::body(&mut request).map_err(|_| (400, "Invalid JSON".into()))
    } else {
        Ok(json!({}))
    };
    let result = payload.and_then(|p| operation(db, path, &url, &method, p));
    match result {
        Ok(v) => server::response(request, 200, v),
        Err((c, e)) => server::response(request, c, json!({"error":e})),
    }
    Ok(())
}
fn operation(
    db: &Database,
    path: &str,
    url: &str,
    method: &Method,
    payload: Value,
) -> ApiResult<Value> {
    if path == "/api/channels" && method == &Method::Get {
        let mut out = Vec::new();
        for (id, name) in [
            ("telegram", "Telegram"),
            ("whatsapp_qr", "WhatsApp"),
            ("imessage", "iMessage"),
            ("phone", "Phone"),
        ] {
            let fields = if id == "telegram" {
                vec![
                    field("bot_token", "Bot token", true, false),
                    field("allowed_users", "Allowed sender IDs", false, false),
                ]
            } else if id == "whatsapp_qr" {
                vec![
                    json!({
                        "key": "allowed_users",
                        "label": "Allowed WhatsApp numbers",
                        "secret": false,
                        "required": false,
                        "placeholder": "15551234567",
                        "help_text": "International format, digits only (no + or spaces), comma-separated. Deny-by-default: leave blank and enable 'allow everyone' only for a temporary test run."
                    })
                ]
            } else {
                vec![
                    field("base_url", "Native channel gateway URL", false, true),
                    field("token", "Gateway access token", true, false),
                    field("allowed_users", "Allowed sender IDs", false, false),
                ]
            };
            let tagline = if id == "telegram" {
                "Native Bot API transport"
            } else if id == "whatsapp_qr" {
                "Link a WhatsApp account as a companion device (personal use, experimental)"
            } else {
                "HTTP gateway transport; configure a gateway to connect this service"
            };
            let mut ch = json!({
                "id": id,
                "name": name,
                "tagline": tagline,
                "fields": fields,
                "supports_qr": id == "whatsapp_qr",
                "supports_check": true,
                "status": status(db, id)?
            });
            if id == "whatsapp_qr" {
                ch["setup_hint"] = json!("Save your allowed number below, then scan the QR code that appears here from WhatsApp on your phone: Settings → Linked devices → Link a device.");
                ch["docs_url"] = json!("https://kageha.dev/docs/channels#whatsapp-qr");
            }
            out.push(ch);
        }
        return Ok(json!({"channels": out}));
    }
    let rest = path
        .strip_prefix("/api/channels/")
        .ok_or((404, "Channel not found".into()))?;
    let (id, action) = rest.split_once('/').unwrap_or((rest, ""));
    if !matches!(id, "telegram" | "whatsapp_qr" | "imessage" | "phone") {
        return Err((404, "Channel not found".into()));
    }
    if method == &Method::Get && action.is_empty() {
        return status(db, id);
    }
    if method == &Method::Delete && action.is_empty() {
        let c = db.lock().map_err(db_error)?;
        crate::ui_credentials::delete(&c,&format!("channel:{id}"))?;
        c.execute(
            "DELETE FROM ui_documents WHERE kind=? AND id=?",
            params![KIND, id],
        )
        .map_err(db_error)?;
        drop(c);
        return status(db, id);
    }
    if method == &Method::Put && action.is_empty() {
        let mut cfg = config(db, id)?;
        cfg.as_object_mut().unwrap().remove("secret");
        let token = payload["bot_token"]
            .as_str()
            .or_else(|| payload["token"].as_str())
            .unwrap_or("");
        for (key, value) in payload
            .as_object()
            .ok_or((400, "Invalid channel settings".into()))?
        {
            if !matches!(
                key.as_str(),
                "bot_token" | "token" | "secret" | "configured" | "connected" | "listener_running"
            ) {
                cfg[key] = value.clone();
            }
        }
        if id != "telegram" && id != "whatsapp_qr" {
            native_io::valid_url(store::text(&cfg, "base_url", 2048)?)
                .map_err(|_| (400, "A valid channel gateway URL is required".into()))?;
            cfg["protocol"] = json!("gateway");
        } else if id == "whatsapp_qr" && cfg.get("base_url").and_then(Value::as_str).is_some_and(|s| !s.trim().is_empty()) {
            cfg["protocol"] = json!("gateway");
        }
        if cfg["allow_all_users"] != true
            && cfg["allowed_users"]
                .as_str()
                .is_none_or(|s| s.trim().is_empty())
        {
            return Err((
                400,
                "Specify allowed sender IDs; channels deny unknown senders by default".into(),
            ));
        }
        cfg["id"] = json!(id);
        cfg["enabled"] = payload.get("enabled").cloned().unwrap_or(json!(true));
        let mut c = db.lock().map_err(db_error)?;
        let tx = c.transaction().map_err(db_error)?;
        if !token.is_empty() {
            ui_settings::save_secret(&tx, &format!("channel:{id}"), token)?;
        }
        if id != "whatsapp_qr" && ui_settings::secret(&tx, &format!("channel:{id}")).unwrap_or_default().is_empty() {
            return Err((400, "A channel credential is required".into()));
        }
        store::put(&tx, KIND, id, cfg).map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        drop(c);
        if id == "whatsapp_qr" {
            ensure_channel_listener();
        }
        return status(db, id);
    }
    if id == "whatsapp_qr" && action == "qr" && method == &Method::Get {
        return Ok(read_qr_state());
    }
    let cfg = config(db, id)?;
    if id != "whatsapp_qr" && cfg["enabled"] != true {
        return Err((409, "Configure and enable this channel first".into()));
    }
    match (action, method) {
        ("check", &Method::Post) => {
            if id == "whatsapp_qr" && cfg["protocol"] != "gateway" {
                let qr_state = read_qr_state();
                let live_status = qr_state.get("status").and_then(Value::as_str).unwrap_or("not_started");
                let running = channel_process_is_running();
                if running && live_status == "ready" {
                    return Ok(json!({"ok": true, "detail": "WhatsApp bridge is linked and ready."}));
                } else if live_status == "qr" {
                    return Ok(json!({"ok": false, "detail": "WhatsApp QR code is waiting to be scanned on your phone."}));
                } else if running {
                    return Ok(json!({"ok": false, "detail": "WhatsApp listener is starting. Wait for the QR code."}));
                } else {
                    return Ok(json!({"ok": false, "detail": "WhatsApp listener is not running. Save changes to start it."}));
                }
            }
            let result = call(
                &cfg,
                if id == "telegram" { "getMe" } else { "health" },
                &json!({}),
                &AtomicBool::new(false),
            )?;
            Ok(json!({"ok":true,"detail":"Native transport connected","result":result}))
        }
        ("qr", &Method::Get) => {
            if id == "whatsapp_qr" && cfg["protocol"] != "gateway" {
                Ok(read_qr_state())
            } else {
                call(&cfg, "qr", &json!({}), &AtomicBool::new(false))
            }
        }
        ("calls", &Method::Get) => call(&cfg, "calls", &json!({}), &AtomicBool::new(false)),
        ("calls", &Method::Post) => call(&cfg, "calls", &payload, &AtomicBool::new(false)),
        ("transcript", &Method::Get) => call(
            &cfg,
            "transcript",
            &json!({"call_ref":webui::query(url,"call_ref")}),
            &AtomicBool::new(false),
        ),
        _ => Err((404, "Channel operation not supported".into())),
    }
}
pub(crate) fn start(db: Database, controls: Controls, workspace: &Path) {
    let workspace = workspace.to_owned();
    std::thread::spawn(move || loop {
        let ids = {
            match db.lock() {
                Ok(c) => store::list(&c, KIND)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|v| v["enabled"] == true)
                    .filter_map(|v| v["id"].as_str().map(str::to_owned))
                    .collect::<Vec<_>>(),
                // Channel polling is optional background work.  A transient
                // database failure must not kill its worker or the web host.
                Err(_) => Vec::new(),
            }
        };
        for id in ids {
            let result = poll(&db, &controls, &workspace, &id);
            if let Ok(c) = db.lock() {
                let _=c.execute("INSERT INTO ui_channel_state VALUES (?,?,?,?) ON CONFLICT(id) DO UPDATE SET connected=excluded.connected,error=excluded.error,observed=excluded.observed",params![id,result.is_ok(),result.err().map(|(_,e)|e).unwrap_or_default(),store::now()]);
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    });
}
fn poll(db: &Database, _controls: &Controls, workspace: &Path, id: &str) -> ApiResult<()> {
    let cfg = config(db, id)?;
    if cfg["enabled"] != true {
        return Ok(());
    }
    if id == "whatsapp_qr" && cfg["protocol"] != "gateway" {
        return Ok(());
    }
    let offset = {
        let c = db.lock().map_err(db_error)?;
        store::get(&c, "channel-offset", id)
            .map_err(db_error)?
            .unwrap_or(json!({"offset":0}))
    };
    let telegram = id == "telegram" && cfg["protocol"] != "gateway";
    let updates = call(
        &cfg,
        if telegram { "getUpdates" } else { "updates" },
        &json!({"offset":offset["offset"],"timeout":0,"limit":20}),
        &AtomicBool::new(false),
    )?;
    let rows = updates
        .get(if telegram { "result" } else { "updates" })
        .and_then(Value::as_array)
        .ok_or((502, "Invalid channel updates".into()))?;
    for update in rows.iter().take(20) {
        let number = update["update_id"]
            .as_u64()
            .ok_or((502, "Channel update needs a numeric update_id".into()))?;
        let sender = if telegram {
            update["message"]["from"]["id"].to_string()
        } else {
            update["sender"].as_str().unwrap_or("").into()
        };
        let destination = if telegram {
            update["message"]["chat"]["id"].to_string()
        } else {
            update["destination"].as_str().unwrap_or(&sender).into()
        };
        let text = if telegram {
            update["message"]["text"].as_str().unwrap_or("")
        } else {
            update["text"].as_str().unwrap_or("")
        };
        let request = format!("channel-{id}-{number}");
        let chat = format!("ch-{}", &webui::turn_id(id, &destination)[..24]);
        let turn = webui::turn_id(&chat, &request);
        let mut admitted = false;
        let mut messages = Vec::new();
        {
            let mut c = db.lock().map_err(db_error)?;
            let tx = c.transaction().map_err(db_error)?;
            let current = store::get(&tx, KIND, id)
                .map_err(db_error)?
                .unwrap_or(json!({}));
            if current["revision"] != cfg["revision"] || current["enabled"] != true {
                return Ok(());
            }
            let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM native_chat_turns WHERE chat_id=? AND request_id=?)",params![chat,request],|r|r.get(0)).map_err(db_error)?;
            let busy:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM native_chat_turns WHERE chat_id=? AND status='running')",[&chat],|r|r.get(0)).map_err(db_error)?;
            if allowed(&cfg, &sender) && !text.trim().is_empty() && text.len() <= 65536 && !exists {
                let count: i64 = tx
                    .query_row(
                        "SELECT COUNT(*) FROM native_chat_turns WHERE status='running'",
                        [],
                        |r| r.get(0),
                    )
                    .map_err(db_error)?;
                if busy || count >= 32 {
                    return Ok(());
                }
                let persona = db.profiles.get(cfg["agent_id"].as_str().unwrap_or("main"))
                    .map_err(db_error)?.map(|p|p.body().clone())
                    .ok_or((409, "Channel agent not found".into()))?;
                tx.execute("INSERT OR IGNORE INTO ui_sessions(id,title,created_at,updated_at) VALUES (?,?,?,?)",params![chat,format!("{id}: {destination}"),store::now(),store::now()]).map_err(db_error)?;
                {
                    let mut history=tx.prepare("SELECT message,COALESCE(answer,'The earlier run was interrupted; inspect current state before repeating it.') FROM (SELECT sequence,message,answer FROM native_chat_turns WHERE chat_id=? ORDER BY sequence DESC LIMIT 16) ORDER BY sequence").map_err(db_error)?;
                    let rows = history
                        .query_map([&chat], |r| {
                            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                        })
                        .map_err(db_error)?
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(db_error)?;
                    for (prompt, answer) in rows {
                        messages.push(json!({"role":"user","content":prompt}));
                        messages.push(json!({"role":"assistant","content":answer}));
                    }
                }
                messages.push(json!({"role":"user","content":text}));
                tx.execute("INSERT INTO native_chat_turns(chat_id,request_id,message,status) VALUES (?,?,?,'running')",params![chat,request,text]).map_err(db_error)?;
                let knowledge_scope=crate::ui_knowledge::resolve_connection(&tx,workspace,&persona,&Value::Null).map_err(db_error)?;
                tx.execute("INSERT INTO ui_run_specs VALUES (?,?,?)",params![chat,request,json!({"agent_id":persona["id"],"persona":persona,"knowledge_scope":knowledge_scope,"agent_mode":"normal","depth":0}).to_string()]).map_err(db_error)?;
                tx.execute(
                    "INSERT INTO ui_turn_uploads VALUES (?,?,'[]')",
                    params![chat, request],
                )
                .map_err(db_error)?;
                let configuration = json!({"agent_id":persona["id"],"persona":persona,"knowledge_scope":knowledge_scope,"agent_mode":"normal","depth":0});
                let mut run = crate::runtime_host::spec(
                    workspace,
                    &chat,
                    &request,
                    &turn,
                    persona["id"].as_str().unwrap_or("main"),
                    &text,
                    0,
                    None,
                    None,
                    messages.clone(),
                    configuration,
                    128,
                );
                run.input["delivery"] =
                    json!({"id":cfg["id"],"revision":cfg["revision"],"destination":destination});
                crate::runtime_host::prepare(&tx, &run).map_err(db_error)?;
                admitted = true;
            }
            store::put(&tx, "channel-offset", id, json!({"offset":number+1})).map_err(db_error)?;
            tx.commit().map_err(db_error)?;
        }
        if admitted {
            let runtime = crate::runtime_host::get(db)?;
            runtime.submit(&turn)?;
            runtime.start()?;
        }
    }
    Ok(())
}

pub(crate) fn deliver_run(
    db: &Database,
    run: &kageha_runtime::lifecycle::RunSpec,
    cancel: &AtomicBool,
) -> ApiResult<()> {
    let delivery = &run.input["delivery"];
    if delivery.is_null() {
        return Ok(());
    }
    let cfg = config(db, delivery["id"].as_str().unwrap_or(""))?;
    if cfg["revision"] != delivery["revision"] || cfg["enabled"] != true {
        return Err((409, "Channel changed before reply delivery".into()));
    }
    let row: Option<(String, String)> = {
        let c = db.lock().map_err(db_error)?;
        c.query_row(
            "SELECT status,result FROM ui_runtime_results WHERE turn=?",
            [&run.turn],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(db_error)?
    };
    let Some((status, result)) = row else {
        return Err((409, "Model reply receipt missing".into()));
    };
    if status != "completed" {
        return Ok(());
    }
    let result: Value = serde_json::from_str(&result).map_err(db_error)?;
    let answer = result["message"].as_str().unwrap_or("");
    let destination = delivery["destination"].as_str().unwrap_or("");
    let call = json!({"id":"channel-reply","name":"channel_reply","arguments":{"to":destination,"text":answer}});
    crate::ui_effects::run(
        db,
        &run.turn,
        &call,
        &json!({"channel":cfg["id"],"revision":cfg["revision"]}),
        || send(&cfg, destination, answer, cancel).map_err(|(_, e)| io::Error::other(e)),
    )
    .map_err(|e| (500, e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sender_allowlist_is_not_a_substring_match() {
        let cfg = json!({"allowed_users":"123, 456"});
        assert!(allowed(&cfg, "123"));
        assert!(!allowed(&cfg, "23"));
        assert!(!allowed(&cfg, ""));
        assert!(!allowed(&json!({}), "123"));
    }
    #[test]
    fn whatsapp_channel_metadata_and_fields() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        store::initialize(&conn).unwrap();
        initialize(&conn).unwrap();
        let db = crate::agent_host::DatabaseState::open(conn).unwrap();
        let channels = operation(&db, "/api/channels", "/api/channels", &Method::Get, json!({})).unwrap();
        let wa = channels["channels"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == "whatsapp_qr")
            .unwrap();
        assert_eq!(wa["name"], "WhatsApp");
        assert_eq!(wa["supports_qr"], true);
        assert_eq!(wa["supports_check"], true);
        assert_eq!(
            wa["tagline"],
            "Link a WhatsApp account as a companion device (personal use, experimental)"
        );
        let fields = wa["fields"].as_array().unwrap();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0]["key"], "allowed_users");
        assert_eq!(fields[0]["required"], false);
        assert_eq!(fields[0]["secret"], false);
    }
}
#[cfg(test)]mod credential_tests {use super::*;#[test]fn missing_channel_keys_are_blocked_before_network_dispatch(){let error=call(&json!({"id":"telegram","base_url":"https://example.test","secret":""}),"sendMessage",&json!({"text":"must not dispatch"}),&AtomicBool::new(false)).unwrap_err();assert_eq!(error.0,403);}}
