//! Native SSH host catalog and immutable per-turn execution destinations.
use crate::{
    server,
    ui_store::{self as store, db_error, ApiResult},
    webui::{self, Database},
};
use rusqlite::params;
use serde_json::{json, Value};
use std::{io::{self, Write}, path::Path, sync::atomic::AtomicBool};
use tiny_http::{Method, Request};
const HOSTS: &str = "config:remote:hosts";

pub(crate) fn saved_host(db: &rusqlite::Connection, id: &str) -> rusqlite::Result<Option<Value>> {
    store::get(db, HOSTS, id)
}

pub(crate) fn desktop_configured(db: &Database) -> bool {
    db.lock().ok().and_then(|db| store::list(&db, HOSTS).ok())
        .is_some_and(|hosts| hosts.iter().any(|host|host["enable_desktop"]==true))
}
fn token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && !value.starts_with('-')
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-:".contains(&b))
}
fn validate(mut value: Value) -> ApiResult<Value> {
    let host = store::text(&value, "host", 253)?;
    if !token(host) {
        return Err((400, "Invalid SSH host".into()));
    }
    let user = value["user"].as_str().unwrap_or("");
    if !user.is_empty() && !token(user) {
        return Err((400, "Invalid SSH user".into()));
    }
    let port = value["port"].as_u64().unwrap_or(22);
    if !(1..=65535).contains(&port) {
        return Err((400, "Invalid SSH port".into()));
    }
    let id = value["id"]
        .as_str()
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| webui::turn_id(host, &format!("{user}:{port}"))[..16].to_owned());
    if !server::identifier(&id) {
        return Err((400, "Invalid host identity".into()));
    }
    for key in ["identity", "workdir", "init"] {
        if value[key]
            .as_str()
            .is_some_and(|v| v.len() > 4096 || v.chars().any(char::is_control))
        {
            return Err((400, format!("Invalid {key}")));
        }
    }
    if value["identity"]
        .as_str()
        .is_some_and(|v| !v.is_empty() && !std::path::Path::new(v).is_absolute())
    {
        return Err((400, "Identity must be an absolute private-key path".into()));
    }
    value["id"] = json!(id);
    value["port"] = json!(port);
    value["runtime_available"] = json!(true);
    if !matches!(value["risk_tier"].as_str(), Some("normal" | "production")) {
        value["risk_tier"] = json!("normal");
    }
    kageha_execution::remote::Config::from_legacy(&value).map_err(|e| (400, e.to_string()))?;
    Ok(value)
}

fn supplied_host_key(host: &Value) -> ApiResult<Option<(String, String)>> {
    use sha2::{Digest, Sha256};
    let Some(key) = host["host_public_key"].as_str().filter(|key| !key.trim().is_empty()) else { return Ok(None); };
    let invalid = || (400, "Enter the trusted server's ssh-ed25519 host public key".into());
    if key.len() > 1024 || key.contains(['\r', '\n']) { return Err(invalid()); }
    let mut parts = key.split_ascii_whitespace();
    if parts.next() != Some("ssh-ed25519") { return Err(invalid()); }
    let base64 = parts.next().ok_or_else(invalid)?;
    let decoded = data_encoding::BASE64.decode(base64.as_bytes()).map_err(|_| invalid())?;
    if decoded.len() != 51 || !decoded.starts_with(b"\0\0\0\x0bssh-ed25519\0\0\0\x20") {
        return Err(invalid());
    }
    let address = host["host"].as_str().unwrap_or_default().to_ascii_lowercase();
    let port = host["port"].as_u64().unwrap_or(22);
    let endpoint = if port == 22 { address } else { format!("[{address}]:{port}") };
    let fingerprint = data_encoding::BASE64_NOPAD.encode(&Sha256::digest(&decoded));
    Ok(Some((format!("{endpoint} ssh-ed25519 {base64}\n"), format!("SHA256:{fingerprint}"))))
}

fn host_in_use(db: &rusqlite::Connection, id: &str) -> ApiResult<bool> {
    db.query_row("SELECT EXISTS(SELECT 1 FROM ui_documents WHERE kind='remote-session' AND json_extract(body,'$.active')=1 AND json_extract(body,'$.host_id')=?)", [id], |row| row.get(0)).map_err(db_error)
}

fn pinned_path(dir: &Path, contents: &str) -> io::Result<std::path::PathBuf> {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    crate::browser_client::private(dir)?;
    let path = dir.join(format!("{:x}", Sha256::digest(contents.as_bytes())));
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    file.write_all(contents.as_bytes())?;
    file.as_file().sync_all()?;
    match file.persist_noclobber(&path) {
        Ok(_) => Ok(path),
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
            if std::fs::symlink_metadata(&path)?.is_file() && std::fs::read(&path)? == contents.as_bytes() { Ok(path) }
            else { Err(io::Error::other("Stored SSH host pin changed on disk")) }
        }
        Err(error) => Err(error.error),
    }
}
pub(crate) fn selection(db: &Database, chat: &str) -> ApiResult<Value> {
    let db = db.lock().map_err(db_error)?;
    let selected = store::get(&db, "remote-session", chat)
        .map_err(db_error)?
        .unwrap_or(json!({"active":false}));
    if selected["active"] != true {
        return Ok(Value::Null);
    }
    store::get(&db, HOSTS, selected["host_id"].as_str().unwrap_or(""))
        .map_err(db_error)?
        .ok_or((409, "Selected remote host was removed".into()))
}
pub(crate) fn read(host: &Value, path: &str, cancel: &AtomicBool) -> io::Result<String> {
    kageha_execution::remote::Remote::new(kageha_execution::remote::Config::from_legacy(host)?)?
        .read(
            path,
            &kageha_contracts::execution::Control {
                cancelled: cancel,
                deadline: None,
            },
        )
}

pub(crate) fn start_job(host: &crate::ui_tools::Tools<'_>, call: &Value) -> io::Result<Value> {
    let arguments = &call["arguments"];
    let command = arguments["command"].as_str().filter(|s| !s.trim().is_empty() && s.len() <= 32768 && !s.contains('\0'))
        .ok_or_else(||io::Error::other("A bounded job command is required"))?;
    let duration = arguments["timeout_s"].as_u64().unwrap_or(3600);
    if !(1..=86400).contains(&duration) { return Err(io::Error::other("Remote job timeout must be 1–86400 seconds")); }
    let id = crate::linux_session::identifier(&json!({"chat":host.chat,"turn":host.turn,"call":call["id"]}));
    let scope = json!({"remote":host.remote,"job_id":id,"command":command,"timeout_s":duration});
    if !crate::ui_approvals::require(host.db,host.chat,host.turn,"remote_job_start",&format!("Start job on {}: {command}",host.remote["host"].as_str().unwrap_or_default()),&scope,host.cancel)? {
        return Err(io::Error::other("Remote job was declined"));
    }
    let endpoint = crate::linux_session::remote_endpoint(&host.remote)?;
    crate::ui_effects::run(host.db,host.turn,call,&scope,|| {
        {
            let connection = host.db.lock().map_err(|_|io::Error::other("Database unavailable"))?;
            store::put(&connection,"remote-job",&id,json!({"chat":host.chat,"turn":host.turn,"host":host.remote,"job_id":id,"command":command})).map_err(io::Error::other)?;
        }
        endpoint.call(&["job-start",&id,&duration.to_string()],true,command.as_bytes().to_vec(),host.cancel,25,4096)
            .map_err(|_|kageha_contracts::execution::uncertain_stop(&format!("Remote job submission outcome is unknown. Check remote_job for {id} before continuing.")))?;
        Ok(json!({"ok":true,"job_id":id,"host_id":host.remote["id"],"state":"submitted"}))
    })
}

pub(crate) fn job(host: &crate::ui_tools::Tools<'_>, call: &Value) -> io::Result<Value> {
    let args = &call["arguments"];
    let id = args["job_id"].as_str().filter(|s| s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit())).ok_or_else(||io::Error::other("A valid remote job ID is required"))?;
    let stored = {
        let connection = host.db.lock().map_err(|_|io::Error::other("Database unavailable"))?;
        store::get(&connection,"remote-job",id).map_err(io::Error::other)?
    }.filter(|value| value["chat"] == host.chat && value["host"] == host.remote)
        .ok_or_else(||io::Error::other("Remote job does not belong to the selected destination and chat"))?;
    let endpoint = crate::linux_session::remote_endpoint(&host.remote)?;
    match args["action"].as_str() {
        Some("status") => {
            let bytes = endpoint.call(&["job-status",id],false,vec![],host.cancel,12,1024)?;
            let status = std::str::from_utf8(&bytes).map_err(io::Error::other)?.trim_end();
            let state = status.split(' ').next().unwrap_or_default();
            let code = status.strip_prefix("exited ").and_then(|code| code.parse::<u8>().ok());
            if !matches!(state,"running" | "stopped" | "interrupted") && code.is_none() { return Err(io::Error::other("Invalid remote job status")); }
            Ok(json!({"state":state,"exit_code":code,"job_id":id,"host_id":host.remote["id"],"command":stored["command"],"started_turn":stored["turn"]}))
        }
        Some("output") => {
            let offset = args["offset"].as_u64().unwrap_or(0);
            if offset > 999_999_999 { return Err(io::Error::other("Invalid log offset")); }
            let bytes = endpoint.call(&["job-output",id,&offset.to_string()],false,vec![],host.cancel,12,24576)?;
            Ok(json!({"job_id":id,"offset":offset,"next_offset":offset+bytes.len() as u64,"output":String::from_utf8_lossy(&bytes)}))
        }
        Some("stop") => {
            let scope = json!({"remote":host.remote,"job_id":id});
            if !crate::ui_approvals::require(host.db,host.chat,host.turn,"remote_job_stop",&format!("Stop remote job {id}"),&scope,host.cancel)? {
                return Err(io::Error::other("Stopping the remote job was declined"));
            }
            crate::ui_effects::run(host.db,host.turn,call,&scope,|| {
                endpoint.call(&["job-stop",id],false,vec![],host.cancel,25,4096)
                    .map_err(|_|kageha_contracts::execution::uncertain_stop("Remote stop outcome is unknown. Check the job status."))?;
                Ok(json!({"ok":true,"job_id":id}))
            })
        }
        _=>Err(io::Error::other("Unknown remote job operation")),
    }
}

pub(crate) fn files(host: &Value, operation: &str, path: Option<&str>, cancel: &AtomicBool) -> io::Result<Value> {
    let remote = kageha_execution::remote::Remote::new(kageha_execution::remote::Config::from_legacy(host)?)?;
    let control = kageha_contracts::execution::Control { cancelled: cancel, deadline: None };
    if operation == "remote_list_files" { return remote.list(path, &control); }
    let path = path.ok_or_else(|| io::Error::other("A workspace-relative SSH path is required"))?;
    let file = remote.snapshot(path, kageha_workspace::DOCUMENT_LIMIT, &control)?;
    Ok(json!({"ok":true,"host_id":remote.config.id,"path":path,"sha256":file.sha256,"bytes":file.bytes.len()}))
}

pub(crate) fn download(db: &Database, chat: &str, turn: &str, host: &Value, path: &str, cancel: &AtomicBool) -> io::Result<Value> {
    let remote = kageha_execution::remote::Remote::new(kageha_execution::remote::Config::from_legacy(host)?)?;
    let control = kageha_contracts::execution::Control { cancelled: cancel, deadline: None };
    let file = remote.snapshot(path, kageha_workspace::DOCUMENT_LIMIT, &control)?;
    control.check()?;
    let filename = crate::ui_uploads::safe_filename(path.rsplit('/').next().unwrap_or_default());
    let mut artifact = crate::native_media::artifact(db, chat, turn, &file.bytes, &filename)?;
    artifact["artifact_path"] = artifact["path"].clone();
    artifact["remote_path"] = json!(path);
    artifact["host_id"] = json!(remote.config.id);
    Ok(artifact)
}
static JOURNALS: std::sync::OnceLock<
    std::sync::Mutex<
        std::collections::HashMap<
            std::path::PathBuf,
            std::sync::Arc<kageha_execution::journal::Journal>,
        >,
    >,
> = std::sync::OnceLock::new();
pub(crate) fn journal_owned(
    db: &rusqlite::Connection,
) -> io::Result<std::sync::Arc<kageha_execution::journal::Journal>> {
    let path = std::path::Path::new(
        db.path()
            .filter(|p| !p.is_empty())
            .ok_or_else(|| io::Error::other("Persistent execution state required"))?,
    )
    .parent()
    .ok_or_else(|| io::Error::other("State directory unavailable"))?
    .join("components/execution.sqlite");
    let mut journals = JOURNALS
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| io::Error::other("Execution journal unavailable"))?;
    if let Some(journal) = journals.get(&path) {
        return Ok(journal.clone());
    }
    let journal = std::sync::Arc::new(kageha_execution::journal::Journal::open(&path)?);
    journals.insert(path, journal.clone());
    Ok(journal)
}
pub(crate) fn receipts(db: &Database, session: &str) -> io::Result<Value> {
    let journal = {
        let db = db
            .lock()
            .map_err(|_| io::Error::other("Database unavailable"))?;
        journal_owned(&db)?
    };
    Ok(json!(journal.receipts(Some(session))?))
}
pub(crate) fn tool(
    db: &Database,
    session: &str,
    turn: &str,
    host: &Value,
    call: &Value,
    upload: Option<kageha_execution::PreparedUpload>,
    cancel: &AtomicBool,
) -> io::Result<Value> {
    use kageha_contracts::execution::ToolPort;
    let journal = {
        let db = db
            .lock()
            .map_err(|_| io::Error::other("Database unavailable"))?;
        journal_owned(&db)?
    };
    let source: std::sync::Arc<dyn kageha_execution::UploadSource> = if let Some(upload) = upload {
        std::sync::Arc::new(upload)
    } else {
        std::sync::Arc::new(())
    };
    let tools = kageha_execution::Tools::new(
        kageha_execution::remote::Config::from_legacy(host)?,
        source,
        journal,
    )?;
    let call = serde_json::from_value(call.clone())?;
    let reply = tools.execute_in(
        &kageha_contracts::execution::ToolContext {
            session: session.into(),
            turn: turn.into(),
        },
        &call,
        &kageha_contracts::execution::Control {
            cancelled: cancel,
            deadline: None,
        },
    )?;
    serde_json::from_str(&reply.content).map_err(io::Error::other)
}
pub(crate) fn handle(mut request: Request, db: &Database) -> io::Result<()> {
    let url = request.url().to_owned();
    let path = url.split('?').next().unwrap_or("");
    let method = request.method().clone();
    let body = if matches!(method, Method::Post | Method::Put) {
        webui::body(&mut request).map_err(|_| (400, "Invalid JSON".into()))
    } else {
        Ok(json!({}))
    };
    let result = body.and_then(|payload| operation(db, path, &url, &method, payload));
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
    mut payload: Value,
) -> ApiResult<Value> {
    if path == "/api/remote/receipts" && method == &Method::Get {
        let session = webui::query(url, "session_id");
        let journal = {
            let db = db.lock().map_err(db_error)?;
            journal_owned(&db).map_err(db_error)?
        };
        return Ok(
            json!({"receipts":journal.receipts(if session.is_empty(){None}else{Some(&session)}).map_err(db_error)?}),
        );
    }
    if let Some(id) = path
        .strip_prefix("/api/remote/receipts/")
        .and_then(|v| v.strip_suffix("/acknowledge"))
    {
        if method != &Method::Post {
            return Err((405, "POST required".into()));
        }
        if payload.as_object().is_none_or(|v| {
            v.keys()
                .any(|key| !matches!(key.as_str(), "target" | "allow_continuation_without_replay"))
        }) {
            return Err((400, "Unexpected continuation decision field".into()));
        }
        if payload["allow_continuation_without_replay"] != true {
            return Err((
                403,
                "Explicit continuation without replay decision required".into(),
            ));
        }
        let target = store::text(&payload, "target", 64)?;
        let journal = {
            let db = db.lock().map_err(db_error)?;
            journal_owned(&db).map_err(db_error)?
        };
        journal
            .acknowledge(id, target)
            .map_err(|e| (409, e.to_string()))?;
        return Ok(json!({"acknowledged":true,"historical_outcome":"unknown","replayed":false}));
    }
    let known_hosts = {
        let c = db.lock().map_err(db_error)?;
        std::path::Path::new(c.path().unwrap_or("."))
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join("ssh_known_hosts")
            .to_string_lossy()
            .to_string()
    };
    payload["known_hosts"] = json!(known_hosts);
    if path == "/api/remote/test" {
        if method != &Method::Post { return Err((405, "POST required".into())); }
        let mut host = if let Some(id) = payload["host_id"].as_str() {
            store::get(&*db.lock().map_err(db_error)?, HOSTS, id)
                .map_err(db_error)?
                .ok_or((404, "Host not found".into()))?
        } else {
            validate(payload)?
        };
        let key = supplied_host_key(&host)?;
        let mut temporary = None;
        if let Some((contents, fingerprint)) = key {
            let mut file = tempfile::NamedTempFile::new().map_err(db_error)?;
            file.write_all(contents.as_bytes()).map_err(db_error)?;
            host["known_hosts"] = json!(file.path());
            host["host_fingerprint"] = json!(fingerprint);
            temporary = Some(file);
        } else if let Some(id) = host["id"].as_str() {
            let stored = store::get(&*db.lock().map_err(db_error)?, HOSTS, id).map_err(db_error)?;
            if let Some(stored) = stored.filter(|stored| stored["host"] == host["host"] && stored["port"] == host["port"]) {
                host["known_hosts"] = stored["known_hosts"].clone();
                host["host_fingerprint"] = stored["host_fingerprint"].clone();
            }
        }
        let transport = kageha_execution::remote::Config::from_legacy(&host)
            .and_then(kageha_execution::remote::Remote::new);
        let cancel = AtomicBool::new(false);
        let probe = transport.and_then(|remote| remote.probe(&kageha_contracts::execution::Control {cancelled:&cancel,deadline:None}));
        let mut report = match probe {
            Ok(value) => value,
            Err(_) => return Ok(json!({"ok":false,"report":{"ok":false,"connect":"Failed","error":"SSH connection failed. Verify the server's host key, SSH user and client key on the Kageha host.","workdir_writable":null}})),
        };
        let ok = report["uname"] == "Linux" && matches!(report["workdir_state"].as_str(), Some("can-create" | "writable"));
        report["ok"] = json!(ok);
        report["connect"] = json!("Connected");
        report["host_fingerprint"] = host["host_fingerprint"].clone();
        let wants_helper = crate::linux_session::mode(&host).is_some() || host["enable_jobs"] == true;
        let capabilities = if wants_helper { crate::linux_session::remote_endpoint(&host).and_then(|endpoint| endpoint.capabilities()).unwrap_or_default() }
            else { vec![] };
        drop(temporary);
        let enabled = |flag: bool, capability: &str| {
            if !flag { "disabled" } else if capabilities.iter().any(|v| v == capability) { "available" } else { "setup required" }
        };
        report["browser"] = json!(enabled(crate::linux_session::mode(&host).is_some(), "browser"));
        report["desktop"] = json!(enabled(host["enable_desktop"] == true, "desktop"));
        report["jobs"] = json!(enabled(host["enable_jobs"] == true, "jobs"));
        report["error"] = json!(if ok { "" } else { "The SSH destination needs a Linux host and a writable workspace" });
        return Ok(json!({"ok":ok,"report":report}));
    }
    let _target_guard = if path == "/api/remote/activate" && method == &Method::Post {
        Some(crate::docker_desktop::lock(store::text(&payload, "session_id", 64)?))
    } else {
        None
    };
    let mut connection = db.lock().map_err(db_error)?;
    let tx = connection.transaction().map_err(db_error)?;
    if path == "/api/remote/hosts" && method == &Method::Post {
        let mut host = validate(payload)?;
        let id = host["id"].as_str().unwrap().to_owned();
        if host_in_use(&tx, &id)? {
            return Err((409, "Deactivate this host in its chats before editing it".into()));
        }
        host.as_object_mut().unwrap().remove("host_fingerprint");
        if let Some((contents, fingerprint)) = supplied_host_key(&host)? {
            let parent = Path::new(&known_hosts).parent().unwrap_or_else(|| Path::new("."));
            let path = pinned_path(&parent.join("ssh-host-keys"), &contents).map_err(db_error)?;
            host["known_hosts"] = json!(path);
            host["host_fingerprint"] = json!(fingerprint);
        } else if let Some(old) = store::get(&tx, HOSTS, &id).map_err(db_error)?
            .filter(|old| old["host"] == host["host"] && old["port"] == host["port"]) {
            host["known_hosts"] = old["known_hosts"].clone();
            host["host_fingerprint"] = old["host_fingerprint"].clone();
        }
        host.as_object_mut().unwrap().remove("host_public_key");
        store::put(&tx, HOSTS, &id, host).map_err(db_error)?;
    } else if let Some(id) = path
        .strip_prefix("/api/remote/hosts/")
        .filter(|_| method == &Method::Delete)
    {
        if host_in_use(&tx, id)? {
            return Err((
                409,
                "Deactivate this host in its chats before removing it".into(),
            ));
        }
        tx.execute(
            "DELETE FROM ui_documents WHERE kind=? AND id=?",
            params![HOSTS, id],
        )
        .map_err(db_error)?;
    } else if path == "/api/remote/default" && method == &Method::Post {
        let id = store::text(&payload, "host_id", 64)?;
        if store::get(&tx, HOSTS, id).map_err(db_error)?.is_none() {
            return Err((404, "Host not found".into()));
        }
        store::put(&tx, "setting", "remote-default", json!({"id":id})).map_err(db_error)?;
    } else if path == "/api/remote/activate" && method == &Method::Post {
        let chat = store::text(&payload, "session_id", 64)?;
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM ui_sessions WHERE id=?)",
                [chat],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        if !exists {
            return Err((404, "Chat not found".into()));
        }
        let busy = crate::docker_desktop::busy(&tx, &chat).map_err(db_error)?;
        if busy {
            return Err((
                409,
                "Stop the chat and specialist workers before changing the execution target".into(),
            ));
        }
        let active = payload["active"] == true;
        if active && crate::docker_desktop::binding(&tx, &chat).map_err(db_error)?.is_some() {
            return Err((409, "Stop this chat's Docker workstation before selecting SSH".into()));
        }

        let id = payload["host_id"].as_str().unwrap_or("");
        if active && store::get(&tx, HOSTS, id).map_err(db_error)?.is_none() {
            return Err((404, "Host not found".into()));
        }
        let prior = store::get(&tx,"remote-session",chat).map_err(db_error)?;
        if active && prior.as_ref().is_some_and(|prior| prior["active"]==true && prior["host_id"]==id) {
            return Ok(json!({"ok":true,"session_id":chat,"active":true,"host_id":id}));
        }
        let previous_host = match prior.as_ref().filter(|prior| prior["active"]==true).and_then(|prior|prior["host_id"].as_str()) {
            Some(previous) => saved_host(&tx,previous).map_err(db_error)?,
            None => None,
        };
        crate::web_browser::clear_control(&tx, chat).map_err(db_error)?;
        crate::machine_io::unregister(chat).map_err(db_error)?;
        tx.execute("DELETE FROM ui_x11_desktops WHERE chat=?", [chat]).map_err(db_error)?;
        store::put(
            &tx,
            "remote-session",
            chat,
            json!({"active":active,"host_id":id}),
        )
        .map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        drop(connection);
        let mut result = json!({"ok":true,"session_id":chat,"active":active,"host_id":id});
        if let Some(previous) = previous_host.filter(|host| crate::linux_session::mode(host).is_some()) {
            if crate::linux_session::for_chat(db,chat,&previous,false).and_then(|session| session.map_or(Ok(()),|session|session.stop())).is_err() {
                result["cleanup_warning"] = json!("The previous Linux session could not be reached to stop it. It will expire on the SSH host.");
            }
        }
        return Ok(result);
    } else if !(path == "/api/remote" && method == &Method::Get) {
        return Err((404, "Unknown remote operation".into()));
    }
    let hosts = store::list(&tx, HOSTS).map_err(db_error)?;
    let default = store::get(&tx, "setting", "remote-default")
        .map_err(db_error)?
        .unwrap_or(json!({}));
    let chat = webui::query(url, "session_id");
    let selected = store::get(&tx, "remote-session", &chat)
        .map_err(db_error)?
        .unwrap_or(json!({}));
    let result = json!({"ok":true,"enabled":true,"runtime_available":true,"hosts":hosts,"default_host_id":default["id"].as_str().unwrap_or(""),"active":selected["active"]==true,"host_id":selected["host_id"].as_str().unwrap_or("")});
    tx.commit().map_err(db_error)?;
    Ok(result)
}
