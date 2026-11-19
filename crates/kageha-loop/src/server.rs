//! Authenticated native chat service with a durable local conversation store.
use crate::native_chat;
use fs2::FileExt;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::Deserialize;
use serde_json::{json, Value};
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tiny_http::{Header, Method, Request, Response, Server};

#[derive(Deserialize)]
struct TurnInput {
    request_id: String,
    message: String,
}

/// Whether a client disconnect was already logged by this process.
static DISCONNECT_LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether a write failed because the client closed its connection (a closed tab or a
/// dropped network). Such a failure ends only that HTTP response.
pub(crate) fn client_disconnected(error: &io::Error) -> bool {
    matches!(error.kind(), io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
        | io::ErrorKind::ConnectionAborted | io::ErrorKind::WriteZero)
}

pub(crate) fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

pub(crate) fn response(request: Request, status: u16, value: Value) {
    let reply = Response::from_string(value.to_string())
        .with_status_code(status)
        .with_header(Header::from_bytes("Content-Type", "application/json").unwrap())
        .with_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
    let _ = request.respond(reply);
}

pub(crate) fn signed_session(token:&str)->String {
    use hmac::{Hmac,Mac};use sha2::Sha256;
    let mut mac=Hmac::<Sha256>::new_from_slice(token.as_bytes()).expect("HMAC accepts every key length");
    mac.update(b"kageha-webui-session-v1");
    data_encoding::HEXLOWER.encode(&mac.finalize().into_bytes())
}
pub(crate) fn cookie_authorized(cookie:&str,token:&str)->bool {
    use hmac::{Hmac,Mac};use sha2::Sha256;
    cookie.split(';').filter_map(|item|item.trim().split_once('=')).any(|(name,value)|{
        if name=="kageha_native_session" {
            value.len()==token.len()&&value.bytes().zip(token.bytes()).fold(0,|d,(a,b)|d|(a^b))==0
        } else if name=="kageha_auth" {
            let Ok(bytes)=data_encoding::HEXLOWER.decode(value.as_bytes()) else{return false};
            let Ok(mut mac)=Hmac::<Sha256>::new_from_slice(token.as_bytes()) else{return false};
            mac.update(b"kageha-webui-session-v1");mac.verify_slice(&bytes).is_ok()
        } else {false}
    })
}
fn authorized(request: &Request, token: &str) -> bool {
    let supplied = request
        .headers()
        .iter()
        .find(|header| header.field.equiv("Authorization"))
        .map(|header| header.value.as_str())
        .unwrap_or("");
    let cookie = request
        .headers()
        .iter()
        .find(|header| header.field.equiv("Cookie"))
        .map(|header| header.value.as_str())
        .unwrap_or("");
    if cookie_authorized(cookie,token) { return true; }
    let expected = format!("Bearer {token}");
    let difference = supplied
        .bytes()
        .zip(expected.bytes())
        .fold(0, |bits, (a, b)| bits | (a ^ b));
    supplied.len() == expected.len() && difference == 0
}

pub(crate) fn handle(
    request: Request,
    db: &crate::webui::Database,
    controls: &crate::webui::Controls,
    workspace: &Path,
    token: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut request = match crate::webui::public(request, token)? {
        Some(request) => request,
        None => return Ok(()),
    };
    if request.method() == &tiny_http::Method::Get {
        if let Some(id) = crate::native_auth::callback_id(request.url()) {
            let result = crate::native_auth::callback(db, id, request.url());
            match result {
                Ok(value) => response(request, 200, value),
                Err((code, error)) => response(request, code, json!({"error":error})),
            }
            return Ok(());
        }
    }
    if !authorized(&request, token) {
        response(request, 401, json!({"error":"auth_required"}));
        return Ok(());
    }
    let path = request.url().split('?').next().unwrap_or("").to_owned();
    if path.starts_with("/api/") {
        return crate::webui::handle(request, Arc::clone(db), Arc::clone(controls), workspace);
    }
    if request.method() == &Method::Get && path == "/health" {
        let agent_runtime=match kageha_runtime_service::bridge_client::Client::from_env() {
            Ok(Some(client))=>match client.info(){Ok(info)=>json!({"mode":"service","connected":true,"service":info}),Err(_)=>json!({"mode":"service","connected":false})},
            Ok(None)=>json!({"mode":"embedded","connected":true}),Err(_)=>json!({"mode":"service","connected":false}),
        };
        response(request, 200, json!({"runtime":"rust","status":if agent_runtime["connected"]==true{"ready"}else{"degraded"},"agent_runtime":agent_runtime,"components":crate::supervisor::snapshot()}));
        return Ok(());
    }
    let parts: Vec<_> = path.split('/').collect();
    if parts.len() != 4 || parts[1] != "chats" || !identifier(parts[2]) {
        response(request, 404, json!({"error":"route_not_found"}));
        return Ok(());
    }
    let chat = parts[2];
    if request.method() == &Method::Get && parts[3] == "history" {
        let db = db.lock().map_err(|_| "conversation database unavailable")?;
        let mut query = db.prepare(
            "SELECT request_id,message,answer,status FROM (
                 SELECT sequence,request_id,message,answer,status FROM native_chat_turns
                 WHERE chat_id=? ORDER BY sequence DESC LIMIT 100) ORDER BY sequence",
        )?;
        let history: Vec<Value> = query.query_map([chat], |row| {
            Ok(json!({"request_id":row.get::<_, String>(0)?, "message":row.get::<_, String>(1)?,
                      "answer":row.get::<_, Option<String>>(2)?, "status":row.get::<_, String>(3)?}))
        })?.collect::<Result<_, _>>()?;
        drop(query);
        drop(db);
        response(request, 200, json!({"chat_id":chat,"turns":history}));
        return Ok(());
    }
    if request.method() != &Method::Post || parts[3] != "turns" {
        response(request, 404, json!({"error":"route_not_found"}));
        return Ok(());
    }
    let mut body = Vec::new();
    request.as_reader().take(262_145).read_to_end(&mut body)?;
    let input: TurnInput = match serde_json::from_slice(&body) {
        Ok(input) if body.len() <= 262_144 => input,
        _ => {
            response(request, 400, json!({"error":"invalid_request"}));
            return Ok(());
        }
    };
    if !identifier(&input.request_id) || input.message.trim().is_empty() {
        response(request, 400, json!({"error":"invalid_request"}));
        return Ok(());
    }
    let mut messages = Vec::new();
    {
        let mut db = db.lock().map_err(|_| "conversation database unavailable")?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let duplicate = tx.query_row(
            "SELECT message,answer,status FROM native_chat_turns WHERE chat_id=? AND request_id=?",
            params![chat, input.request_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, String>(2)?)),
        ).optional()?;
        if let Some((message, answer, status)) = duplicate {
            let code = if message == input.message && status == "completed" {
                200
            } else {
                409
            };
            let reply = if message == input.message {
                json!({"request_id":input.request_id,"status":status,"answer":answer})
            } else {
                json!({"error":"request_id_conflict"})
            };
            drop(tx);
            drop(db);
            response(request, code, reply);
            return Ok(());
        }
        let running: bool = tx.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM native_chat_turns WHERE chat_id=? AND status IN {})", crate::ui_turn_resume::LIVE_STATUSES),
            [chat],
            |row| row.get(0),
        )?;
        if running {
            drop(tx);
            drop(db);
            response(request, 409, json!({"error":"chat_busy"}));
            return Ok(());
        }
        // Bound history to the newest sixteen completed exchanges.
        {
            let mut query = tx.prepare(
                "SELECT message,answer FROM (SELECT sequence,message,answer FROM native_chat_turns
                 WHERE chat_id=? AND status='completed' ORDER BY sequence DESC LIMIT 16) ORDER BY sequence",
            )?;
            for item in query.query_map([chat], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })? {
                let (message, answer) = item?;
                messages.push(json!({"role":"user","content":message}));
                messages.push(json!({"role":"assistant","content":answer}));
            }
        }
        tx.execute(
            "INSERT INTO native_chat_turns(chat_id,request_id,message,status) VALUES (?,?,?,'running')",
            params![chat, input.request_id, input.message],
        )?;
        tx.commit()?;
    }
    messages.push(json!({"role":"user","content":input.message}));
    let result = native_chat::converse(workspace, messages);
    let (status, answer, code) = match result {
        Ok(answer) => ("completed", Some(answer), 200),
        Err(_) => ("failed", None, 502),
    };
    db.lock().map_err(|_| "conversation database unavailable")?.execute(
        "UPDATE native_chat_turns SET status=?,answer=? WHERE chat_id=? AND request_id=? AND status='running'",
        params![status, answer, chat, input.request_id],
    )?;
    response(
        request,
        code,
        json!({"request_id":input.request_id,"status":status,"answer":answer}),
    );
    Ok(())
}

pub fn run(workspace: &Path, state: &Path, bind: &str) -> Result<(), Box<dyn std::error::Error>> {
    let token = std::env::var("KAGEHA_SERVER_TOKEN")?;
    if !(32..=256).contains(&token.len())
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("set KAGEHA_SERVER_TOKEN to at least 32 characters".into());
    }
    let workspace = workspace.canonicalize()?;
    if !workspace.is_dir() {
        return Err("expected a workspace directory".into());
    }
    let mut directory = std::fs::DirBuilder::new();
    directory.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        directory.mode(0o700);
    }
    directory.create(state)?;
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(state.join("native-server.lock"))?;
    lock.try_lock_exclusive()?;
    let database = state.join("native-chats.sqlite");
    let file: File = options.open(&database)?;
    drop(file);
    let mut db = Connection::open(database)?;
    db.busy_timeout(Duration::from_secs(5))?;
    db.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE IF NOT EXISTS native_chat_turns(
             sequence INTEGER PRIMARY KEY AUTOINCREMENT,chat_id TEXT NOT NULL,
             request_id TEXT NOT NULL,message TEXT NOT NULL,answer TEXT,status TEXT NOT NULL,
             UNIQUE(chat_id,request_id));
         CREATE UNIQUE INDEX IF NOT EXISTS native_chat_running ON native_chat_turns(chat_id) WHERE status IN ('running','waiting_for_unlock');
         CREATE TABLE IF NOT EXISTS ui_runtime_runs(id TEXT PRIMARY KEY,spec TEXT NOT NULL,state TEXT NOT NULL,projected_revision INTEGER NOT NULL DEFAULT 0);",
    )?;
    // Resume pass (design section 12). Turns without a runtime run are interrupted
    // here; runtime-managed turns continue from their checkpoint, wait for credential
    // unlock, or are interrupted when the runtime host recovers below.
    crate::ui_turn_resume::startup(&db)?;
    crate::webui::initialize(&db)?;
    crate::ui_settings::initialize_workspace(&db, &workspace)?;
    crate::ui_legacy::import(&mut db)?;
    if let Some(archive)=std::env::var_os("KAGEHA_CHAT_IMPORT_DIR") {crate::chat_archive::import(&mut db,std::path::Path::new(&archive))?;}

    let server = Arc::new(Server::http(bind).map_err(|error| io::Error::other(error.to_string()))?);
    let db = crate::agent_host::DatabaseState::open(db)?;
    // Run_Journal store (task 21.2) and the background reflection worker (task 23.1),
    // before any turn can run. Failure leaves them off.
    crate::ui_learning::install_default(&db);
    crate::ui_reflection::install_default(&db);
    let runtime=crate::runtime_host::get(&db).map_err(|(_,e)|io::Error::other(e))?;
    let controls=runtime.controls.clone();
    runtime.start().map_err(|(_,e)|io::Error::other(e))?;
    crate::supervisor::start(state)?;
    crate::component_restarts::install(&db);
    eprintln!("Rust chat service listening on {}", server.server_addr());
    crate::native_channels::start(Arc::clone(&db), Arc::clone(&controls), &workspace);
    std::thread::scope(|scope| {
        // Streaming preview and chat responses must leave capacity for Stop and approvals.
        for _ in 0..16 {
            let server = Arc::clone(&server);
            let db = Arc::clone(&db);
            let controls = Arc::clone(&controls);
            let token = &token;
            let workspace = &workspace;
            scope.spawn(move || {
                while let Ok(request) = server.recv() {
                    let request_id = uuid::Uuid::new_v4();
                    let path = request.url().split('?').next().unwrap_or("");
                    let route = path.split('/').take(3).collect::<Vec<_>>().join("/");
                    let run_id = path.strip_prefix("/api/sessions/").and_then(|path| path.split('/').next())
                        .filter(|chat| identifier(chat)).and_then(|chat| {
                            let db = db.lock().ok()?;
                            let request: Option<String> = db.query_row("SELECT request_id FROM native_chat_turns WHERE chat_id=? AND status='running' ORDER BY sequence DESC LIMIT 1", [chat], |row| row.get(0)).optional().ok()?;
                            request.map(|request| crate::webui::turn_id(chat, &request))
                        });
                    if let Err(error) = handle(request, &db, &controls, workspace, token) {
                        // A client that closed its connection ends only its own response;
                        // the turn keeps running. It is noted once, not as a failure.
                        if error.downcast_ref::<io::Error>().is_some_and(client_disconnected) {
                            if !DISCONNECT_LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                                eprintln!("Rust chat client disconnected request_id={request_id} route={route:?} error={error} (later disconnects are not logged)");
                            }
                            continue;
                        }
                        let category = if let Some(error) = error.downcast_ref::<io::Error>() {
                            format!("io_{:?}", error.kind())
                        } else if error.is::<rusqlite::Error>() { "database".into() }
                        else if error.is::<serde_json::Error>() { "invalid_json".into() }
                        else { "handler".into() };
                        let failure_id = uuid::Uuid::new_v4();
                        eprintln!("Rust chat request failed request_id={request_id} failure_id={failure_id} run_id={} route={route:?} category={category} error={error:?}", run_id.as_deref().unwrap_or("none"));
                    }
                }
            });
        }
    });
    drop(lock);
    Ok(())
}

#[cfg(test)] mod cutover_auth_tests {use super::*;#[test] fn signed_sessions_survive_host_cutover_but_not_rotation(){let token="a-private-existing-token";let cookie=format!("other=x; kageha_auth={}",signed_session(token));assert!(cookie_authorized(&cookie,token));assert!(!cookie_authorized(&cookie,"rotated-token"));assert!(!cookie_authorized("kageha_auth=forged",token));assert!(!cookie_authorized("other=kageha_auth=forged",token));assert!(cookie_authorized(&format!("kageha_native_session={token}"),token));}}
