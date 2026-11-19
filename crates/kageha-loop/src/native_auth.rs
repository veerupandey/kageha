//! Native OAuth 2 authorization-code/PKCE and integration credentials.
use crate::{
    native_io, server, ui_settings,
    ui_store::{self as store, db_error, ApiResult},
    webui::{self, Database},
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    io::{self, Read},
    sync::atomic::AtomicBool,
    time::Duration,
};
use tiny_http::{Method, Request};
pub(crate) fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
fn nonce() -> io::Result<String> {
    let mut bytes = [0; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(data_encoding::BASE64URL_NOPAD.encode(&bytes))
}
fn prefix(id: &str) -> String {
    format!("KAGEHA_OAUTH_{}", id.to_ascii_uppercase().replace('-', "_"))
}
fn endpoints(id: &str) -> Value {
    let defaults = match id {
        "gmail" => (
            "https://accounts.google.com/o/oauth2/v2/auth",
            "https://oauth2.googleapis.com/token",
            "https://gmail.googleapis.com/gmail/v1/users/me/profile",
            "https://gmail.googleapis.com/gmail/v1/users/me",
            "https://www.googleapis.com/auth/gmail.readonly",
        ),
        "google-calendar" => (
            "https://accounts.google.com/o/oauth2/v2/auth",
            "https://oauth2.googleapis.com/token",
            "https://www.googleapis.com/oauth2/v3/userinfo",
            "https://www.googleapis.com/calendar/v3",
            "openid email https://www.googleapis.com/auth/calendar.readonly",
        ),
        "google-drive" => (
            "https://accounts.google.com/o/oauth2/v2/auth",
            "https://oauth2.googleapis.com/token",
            "https://www.googleapis.com/oauth2/v3/userinfo",
            "https://www.googleapis.com/drive/v3",
            "openid email https://www.googleapis.com/auth/drive.metadata.readonly",
        ),
        "linkedin" => (
            "https://www.linkedin.com/oauth/v2/authorization",
            "https://www.linkedin.com/oauth/v2/accessToken",
            "https://api.linkedin.com/v2/userinfo",
            "https://api.linkedin.com/v2",
            "openid profile email",
        ),
        "github" => (
            "https://github.com/login/oauth/authorize",
            "https://github.com/login/oauth/access_token",
            "https://api.github.com/user",
            "https://api.github.com",
            "read:user repo",
        ),
        _ => ("", "", "", "", ""),
    };
    let p = prefix(id);
    let env = |key: &str, fallback: &str| {
        std::env::var(format!("{p}_{key}")).unwrap_or_else(|_| fallback.into())
    };
    json!({"authorization":env("AUTHORIZE_URL",defaults.0),"token":env("TOKEN_URL",defaults.1),"profile":env("PROFILE_URL",defaults.2),"api":env("API_URL",defaults.3),"scope":env("SCOPES",defaults.4),"model_base_url":env("MODEL_BASE_URL",""),"device":env("DEVICE_URL",if id=="github"{"https://github.com/login/device/code"}else{""})})
}
fn configuration(db: &Database, id: &str) -> ApiResult<Value> {
    let connection = db.lock().map_err(db_error)?;
    let row = store::get(&connection, "oauth-config", id)
        .map_err(db_error)?
        .unwrap_or(json!({}));
    let legacy = if id == "gmail" {
        "KAGEHA_GOOGLE".to_owned()
    } else {
        format!("KAGEHA_{}", id.to_ascii_uppercase().replace('-', "_"))
    };
    let p = prefix(id);
    let env = |key: &str| {
        std::env::var(format!("{p}_{key}"))
            .or_else(|_| std::env::var(format!("{legacy}_{key}")))
            .unwrap_or_default()
    };
    let client = row["client_id"].as_str().unwrap_or("");
    let client = if client.is_empty() {
        env("CLIENT_ID")
    } else {
        client.into()
    };
    let redirect = row["redirect_uri"].as_str().unwrap_or("");
    let redirect = if redirect.is_empty() {
        env("REDIRECT_URI")
    } else {
        redirect.into()
    };
    let secret=client_credential(&connection,id,||env("CLIENT_SECRET"))?;
    Ok(
        json!({"client_id":client,"redirect_uri":redirect,"client_secret":secret,"revision":row["revision"]}),
    )
}
fn public_config(config: &Value) -> Value {
    json!({"client_id":config["client_id"],"redirect_uri":config["redirect_uri"],"secret_configured":config["client_secret"].as_str().is_some_and(|v|!v.is_empty()),"configured":config["client_id"].as_str().is_some_and(|v|!v.is_empty())&&config["redirect_uri"].as_str().is_some_and(|v|!v.is_empty()),"source":"native"})
}
fn effective_agents(info: &mut Value) {
    let mut agents = info["agents"].as_array().cloned().unwrap_or_default();
    if !agents.iter().any(|id| id == "main") {
        agents.push(json!("main"));
    }
    info["agents"] = json!(agents);
}
fn row(db: &Database, id: &str) -> ApiResult<Value> {
    let mut info = store::get(&*db.lock().map_err(db_error)?, "integration", id)
        .map_err(db_error)?
        .unwrap_or(json!({"id":id,"agents":[],"write_agents":[],"repositories":[]}));
    effective_agents(&mut info);
    Ok(info)
}
fn status(db: &Database, id: &str) -> ApiResult<Value> {
    let mut value = row(db, id)?;
    let cfg = configuration(db, id)?;
    let connection = db.lock().map_err(db_error)?;
    let scope=crate::ui_credentials::oauth_authority(&connection,id)?;
    value["connected"]=json!(crate::ui_credentials::oauth_ready(&connection,id)?&&(id.starts_with("provider-")||scope.as_deref().is_some_and(|v|value["authorization_id"].as_str()==Some(v))));
    value["needs_verification"] = json!(scope.is_some() && value["connected"] != true);
    if let Some(metadata)=crate::ui_credentials::oauth_metadata(&connection,id)?{value["refreshable"]=json!(metadata.refreshable);value["expires_at"]=metadata.expires_at_ms.map(|v|json!(v as f64/1000.0)).unwrap_or(Value::Null);}
    let account=crate::ui_credentials::account_metadata(&connection,id)?;for key in ["account_id","login","email","name"]{value[key]=account[key].clone();}
    value["configured"] = public_config(&cfg)["configured"].clone();
    value["browser_auth_available"] =
        json!(id == "github" && cfg["client_id"].as_str().is_some_and(|v| !v.is_empty()));
    value["read_only"] = json!(true);
    value["runtime_available"] = json!(true);
    value["redirect_uri"] = cfg["redirect_uri"].clone();
    let pending = store::get(&connection, "oauth-pending", id)
        .map_err(db_error)?
        .unwrap_or(json!({}));
    value["pending"] = json!(
        pending["expires"]
            .as_f64()
            .is_some_and(|v| v > store::now())
            && pending["claimed"] != true
    );
    Ok(value)
}
// Provider error descriptions are untrusted and may echo credentials. Keep only
// recognized protocol categories and fixed guidance; never include the raw body.
fn oauth_exchange_error(status: u16, body: &[u8], refreshing: bool) -> String {
    let category = if body.len() <= 16 * 1024 {
        serde_json::from_slice::<Value>(body).ok().and_then(|value|
            match value["error"].as_str() {
                Some("invalid_grant") => Some("invalid_grant"),
                Some("invalid_client") => Some("invalid_client"),
                Some("unauthorized_client") => Some("unauthorized_client"),
                Some("invalid_scope") => Some("invalid_scope"),
                Some("invalid_request") => Some("invalid_request"),
                Some("unsupported_grant_type") => Some("unsupported_grant_type"),
                Some("access_denied") => Some("access_denied"),
                Some("temporarily_unavailable") => Some("temporarily_unavailable"),
                Some("server_error") => Some("server_error"),
                _ => None,
            })
    } else { None };
    let guidance = match category {
        Some("invalid_grant") if refreshing => "The provider rejected the refresh credential. Reconnect this integration; repeating the refresh will not repair it.",
        Some("invalid_grant") => "The authorization grant was rejected. Start a new authorization flow.",
        Some("invalid_client" | "unauthorized_client") => "Check this integration's client configuration before authorizing again.",
        Some("invalid_scope") => "Check the configured OAuth scopes.",
        Some("invalid_request" | "unsupported_grant_type") => "Check the OAuth request and provider configuration.",
        Some("access_denied") => "The provider denied authorization. Review account access before reconnecting.",
        Some("temporarily_unavailable" | "server_error") => "The provider is temporarily unavailable. Try again later.",
        _ => "",
    };
    match category {
        Some(category) => format!("OAuth exchange failed (HTTP {status}; provider_error={category}). {guidance}"),
        None => format!("OAuth exchange failed (HTTP {status})"),
    }
}

fn form(url: &str, fields: &Value) -> ApiResult<Value> {
    let pairs = fields
        .as_object()
        .ok_or((400, "Invalid OAuth form".into()))?
        .iter()
        .filter_map(|(k, v)| {
            v.as_str()
                .filter(|s| !s.is_empty())
                .map(|v| format!("{}={}", encode(k), encode(v)))
        })
        .collect::<Vec<_>>()
        .join("&");
    if !url.starts_with("https://") {
        return Err((400, "OAuth token endpoints require HTTPS".into()));
    }
    let result = native_io::http(
        url,
        "POST",
        &[
            (
                "Content-Type".into(),
                "application/x-www-form-urlencoded".into(),
            ),
            ("Accept".into(), "application/json".into()),
        ],
        pairs.into_bytes(),
        &AtomicBool::new(false),
        Duration::from_secs(25),
        1024 * 1024,
    )
    .map_err(|_| (502, "OAuth endpoint could not be reached".into()))?;
    if !(200..300).contains(&result.status) {
        return Err((
            502,
            oauth_exchange_error(result.status, &result.bytes, fields["grant_type"] == "refresh_token"),
        ));
    }
    serde_json::from_slice(&result.bytes).map_err(|_| (502, "OAuth response invalid".into()))
}
fn generation(connection:&rusqlite::Connection,id:&str)->ApiResult<u64>{crate::ui_credentials::oauth_generation(connection,id)}
fn bump(connection:&rusqlite::Connection,id:&str)->ApiResult<u64>{crate::ui_credentials::oauth_bump(connection,id)}
fn project_grant(connection:&rusqlite::Connection,id:&str,metadata:&kageha_contracts::credentials::CredentialMetadata)->ApiResult<()> {
    let mut info=store::get(connection,"integration",id).map_err(db_error)?.unwrap_or(json!({"id":id,"agents":[],"repositories":[],"write_agents":[]}));
    info["refreshable"]=json!(metadata.refreshable);info["expires_at"]=metadata.expires_at_ms.map(|v|json!(v as f64/1000.0)).unwrap_or(Value::Null);
    store::put(connection,"integration",id,info).map_err(db_error)?;Ok(())
}
fn store_token(db:&Database,id:&str,token:Value,fresh:bool,expected:u64)->ApiResult<()> {
    let token=crate::ui_credentials::SecretJson::new(token);
    store::text(&token,"access_token",16384)?;
    let mut connection=db.lock().map_err(db_error)?;let tx=connection.transaction().map_err(db_error)?;
    let metadata=crate::ui_credentials::oauth_store(&tx,id,&token,fresh,expected)?;project_grant(&tx,id,&metadata)?;tx.commit().map_err(db_error)?;Ok(())
}
pub(crate) fn bearer(db:&Database,id:&str)->ApiResult<String> {
    static LOCKS:std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String,std::sync::Arc<std::sync::Mutex<()>>>>>=std::sync::OnceLock::new();
    let lock=LOCKS.get_or_init(Default::default).lock().map_err(db_error)?.entry(id.to_owned()).or_default().clone();let _guard=lock.lock().map_err(db_error)?;
    let config=configuration(db,id)?;
    let metadata={let connection=db.lock().map_err(db_error)?;crate::ui_credentials::oauth_metadata(&connection,id)?.ok_or((409,"Connect this integration first".into()))?};
    if metadata.state!=kageha_contracts::credentials::CredentialState::Active {return Err((409,"This OAuth grant needs reconnection or explicit recovery".into()));}
    if metadata.expires_at_ms.is_some_and(|v|v<=kageha_credentials::crypto::now_ms()+30000)&&metadata.refreshable {
        let refresh={let connection=db.lock().map_err(db_error)?;crate::ui_credentials::oauth_refresh_begin(&connection,id)?};
        let reply=form(endpoints(id)["token"].as_str().unwrap_or(""),&json!({"grant_type":"refresh_token","refresh_token":refresh.refresh_token(),"client_id":config["client_id"],"client_secret":config["client_secret"]}));
        let token=match reply {Ok(value)=>crate::ui_credentials::SecretJson::new(value),Err(error)=>{let connection=db.lock().map_err(db_error)?;let _=crate::ui_credentials::oauth_refresh_failed(&connection,&refresh);return Err(error);}};
        let connection=db.lock().map_err(db_error)?;
        let metadata=match crate::ui_credentials::oauth_refresh_finish(&connection,&refresh,&token){Ok(value)=>value,Err(error)=>{let _=crate::ui_credentials::oauth_refresh_failed(&connection,&refresh);return Err(error);}};
        project_grant(&connection,id,&metadata)?;
        return ui_settings::secret(&connection,&format!("oauth:{id}:token"));
    }
    let connection=db.lock().map_err(db_error)?;let token=ui_settings::secret(&connection,&format!("oauth:{id}:token"))?;
    if token.is_empty(){return Err((409,"Connect or refresh this integration first".into()));}Ok(token)
}
fn check(db: &Database, id: &str) -> ApiResult<Value> {
    let refresh_token=kageha_credentials::SecretBytes::new(bearer(db,id)?.into_bytes());drop(refresh_token);
    let (expected_generation,token)={let c=db.lock().map_err(db_error)?;crate::ui_credentials::oauth_access_with_revision(&c,id)?};
    let endpoint = endpoints(id);
    let url = endpoint["profile"].as_str().unwrap_or("");
    let profile = native_io::json(
        url,
        "GET",
        &[
            ("Authorization".into(), format!("Bearer {token}")),
            ("Accept".into(), "application/json".into()),
        ],
        &json!({}),
        &AtomicBool::new(false),
    )
    .map_err(|_| (502, "Integration account check failed".into()))?;
    let mut connection = db.lock().map_err(db_error)?;
    let tx = connection.transaction().map_err(db_error)?;
    if generation(&tx,id)?!=expected_generation{return Err((409,"OAuth grant changed while checking its account".into()));}
    let old_account=crate::ui_credentials::account_metadata(&tx,id)?;
    let mut info = store::get(&tx, "integration", id)
        .map_err(db_error)?
        .unwrap_or(json!({"id":id}));
    let identity = profile["id"]
        .as_str()
        .or_else(|| profile["emailAddress"].as_str())
        .or_else(|| profile["sub"].as_str())
        .or_else(|| profile["login"].as_str())
        .unwrap_or("");
    if identity.is_empty(){return Err((502,"Integration account identity is unavailable".into()));}
    if old_account["account_id"]
        .as_str()
        .is_some_and(|old| old != identity)
    {
        info["agents"] = json!([]);
        info["write_agents"] = json!([]);
        info["repositories"] = json!([]);
    }
    let account=json!({"account_id":identity,"login":profile["login"],"email":profile.get("emailAddress").or_else(||profile.get("email")).cloned().unwrap_or(Value::Null),"name":profile["name"]});
    let revision=info["revision"].as_u64().unwrap_or(0)+1;let reference=crate::ui_credentials::save_account_metadata(&tx,id,&account,revision)?;for key in ["account_id","login","email","name"]{if let Some(v)=info.as_object_mut(){v.remove(key);}}info["account_ref"]=json!(reference);
    info["authorization_id"]=json!(crate::ui_credentials::oauth_authority(&tx,id)?);
    store::put(&tx, "integration", id, info).map_err(db_error)?;
    tx.commit().map_err(db_error)?;
    drop(connection);
    status(db, id)
}
fn start(db: &Database, id: &str) -> ApiResult<Value> {
    let config = configuration(db, id)?;
    if public_config(&config)["configured"] != true {
        return Err((
            409,
            "Configure the OAuth client and redirect URI first".into(),
        ));
    }
    let endpoints = endpoints(id);
    let auth = endpoints["authorization"].as_str().unwrap_or("");
    if !auth.starts_with("https://") {
        return Err((
            409,
            "Configure an HTTPS OAuth authorization endpoint on the server".into(),
        ));
    }
    let state = nonce().map_err(db_error)?;
    let verifier = nonce().map_err(db_error)?;
    let challenge = data_encoding::BASE64URL_NOPAD.encode(&Sha256::digest(verifier.as_bytes()));
    let epoch = {
        let connection = db.lock().map_err(db_error)?;
        bump(&connection, id)?
    };
    let pending = json!({"state":state,"verifier":verifier,"generation":epoch,"expires":store::now()+600.0,"config":config,"claimed":false});
    crate::ui_credentials::pending_put(&*db.lock().map_err(db_error)?,id,pending)?;
    let fields = json!({"client_id":config["client_id"],"redirect_uri":config["redirect_uri"],"response_type":"code","scope":endpoints["scope"],"access_type":"offline","state":state,"code_challenge":challenge,"code_challenge_method":"S256"});
    let query = fields
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| format!("{}={}", encode(k), encode(v.as_str().unwrap_or(""))))
        .collect::<Vec<_>>()
        .join("&");
    Ok(json!({"ok":true,"authorization_url":format!("{auth}?{query}")}))
}
pub(crate) fn callback(db: &Database, id: &str, url: &str) -> ApiResult<Value> {
    let state = webui::form_decode(&webui::query(url, "state")).unwrap_or_default();
    let code = webui::form_decode(&webui::query(url, "code")).unwrap_or_default();
    let error = webui::query(url, "error");
    let pending = {
        let mut c = db.lock().map_err(db_error)?;
        let tx = c.transaction().map_err(db_error)?;
        let mut pending = crate::ui_credentials::pending_get(&tx,id)?
            .ok_or((400, "OAuth flow missing or already consumed".into()))?;
        if state.is_empty()
            || pending["state"] != state
            || pending["expires"].as_f64().is_none_or(|v| v < store::now())
            || pending["claimed"] == true
        {
            return Err((400, "OAuth state invalid, expired or already used".into()));
        }
        pending["claimed"] = json!(true);
        crate::ui_credentials::pending_put(&tx,id,pending.clone())?;
        tx.commit().map_err(db_error)?;
        pending
    };
    if !error.is_empty() || code.is_empty() {
        return Err((400, "Authorization was cancelled or denied".into()));
    }
    let config = configuration(db, id)?;
    if config != pending["config"] {
        return Err((409, "OAuth configuration changed; start again".into()));
    }
    let token = form(
        endpoints(id)["token"].as_str().unwrap_or(""),
        &json!({"grant_type":"authorization_code","code":code,"client_id":config["client_id"],"client_secret":config["client_secret"],"redirect_uri":config["redirect_uri"],"code_verifier":pending["verifier"]}),
    )?;
    store_token(
        db,
        id,
        token,
        true,
        pending["generation"].as_u64().unwrap_or(0),
    )?;
    if let Some(provider) = id.strip_prefix("provider-") {
        let base = endpoints(id)["model_base_url"]
            .as_str()
            .unwrap_or("")
            .to_owned();
        native_io::valid_url(&base)
            .map_err(|_| (400, "Model OAuth endpoint is not configured".into()))?;
        let model = std::env::var(format!("{}_MODEL", prefix(id)))
            .map_err(|_| (409, "Configure the OAuth model ID on the server".into()))?;
        store::put(&*db.lock().map_err(db_error)?,"model",&format!("{provider}/{model}"),json!({"id":format!("{provider}/{model}"),"provider":provider,"model":model,"base_url":base,"protocol":"openai_compat","oauth_id":id})).map_err(db_error)?;
    } else {
        check(db,id)?;
    }
    Ok(json!({"ok":true,"message":"Connected. Return to Kageha Settings; you can close this tab."}))
}
pub(crate) fn callback_id(url: &str) -> Option<&str> {
    let path = url.split('?').next()?;
    let id = path
        .strip_prefix("/api/integrations/")?
        .strip_suffix("/callback")?;
    server::identifier(id).then_some(id)
}
pub(crate) fn handle(mut request: Request, db: &Database) -> io::Result<()> {
    let url = request.url().to_owned();
    let path = url.split('?').next().unwrap_or("");
    let rest = path.trim_start_matches("/api/integrations/");
    let (id, action) = rest.split_once('/').unwrap_or((rest, ""));
    let method = request.method().clone();
    let body = if matches!(method, Method::Post | Method::Put) {
        webui::body(&mut request).map_err(|_| (400, "Invalid JSON".into()))
    } else {
        Ok(json!({}))
    };
    let result = body.and_then(|p| operation(db, id, action, &url, &method, p));
    match result {
        Ok(value) => server::response(request, 200, value),
        Err((code, error)) => server::response(request, code, json!({"error":error})),
    }
    Ok(())
}
fn operation(
    db: &Database,
    id: &str,
    action: &str,
    url: &str,
    method: &Method,
    payload: Value,
) -> ApiResult<Value> {
    if !server::identifier(id) {
        return Err((400, "Invalid integration ID".into()));
    }
    match (action, method) {
        ("setup", &Method::Get) => Ok(public_config(&configuration(db, id)?)),
        ("setup", &Method::Put) => {
            let old = configuration(db, id)?;
            let client = store::text(&payload, "client_id", 512)?;
            let redirect = store::text(&payload, "redirect_uri", 2048)?;
            native_io::valid_url(redirect).map_err(|_| (400, "Invalid redirect URI".into()))?;
            if !redirect.ends_with(&format!("/api/integrations/{id}/callback"))
                || redirect.contains('#')
                || redirect.contains('?')
                || !redirect.starts_with("https://")
                    && !(redirect.starts_with("http://localhost:")
                        || redirect.starts_with("http://127.0.0.1:"))
            {
                return Err((
                    400,
                    "Use HTTPS or a loopback callback ending in this integration's callback path"
                        .into(),
                ));
            }
            let mut c = db.lock().map_err(db_error)?;
            let tx = c.transaction().map_err(db_error)?;
            let secret = payload["client_secret"].as_str().unwrap_or("");
            if !secret.is_empty() {
                ui_settings::save_secret(&tx, &format!("oauth:{id}:client"), secret)?;
            } else if old["client_id"] != client {
                ui_settings::save_secret(&tx, &format!("oauth:{id}:client"), "")?;
            }
            if old["client_id"] != client {
                crate::ui_credentials::delete(&tx,&format!("oauth:{id}:token"))?;
                crate::ui_credentials::delete(&tx,&format!("oauth:{id}:refresh"))?;
            }
            store::put(
                &tx,
                "oauth-config",
                id,
                json!({"client_id":client,"redirect_uri":redirect}),
            )
            .map_err(db_error)?;
            crate::ui_credentials::pending_delete(&tx,id)?;
            bump(&tx, id)?;
            tx.commit().map_err(db_error)?;
            drop(c);
            Ok(public_config(&configuration(db, id)?))
        }
        ("", &Method::Get) => status(db, id),
        ("connect", &Method::Post) => {
            if let Some(token) = payload["token"].as_str().filter(|s| !s.is_empty()) {
                let epoch = {
                    let c = db.lock().map_err(db_error)?;
                    generation(&c, id)?
                };
                let url = endpoints(id)["profile"].as_str().unwrap_or("").to_owned();
                native_io::json(
                    &url,
                    "GET",
                    &[
                        ("Authorization".into(), format!("Bearer {token}")),
                        ("User-Agent".into(), "Kageha-Rust".into()),
                    ],
                    &json!({}),
                    &AtomicBool::new(false),
                )
                .map_err(|_| (502, "Integration credential validation failed".into()))?;
                store_token(db, id, json!({"access_token":token}), true, epoch)?;
                check(db, id)
            } else {
                start(db, id)
            }
        }
        ("device", &Method::Post) if id == "github" => device_start(db, id),
        ("device/poll", &Method::Post) if id == "github" => device_poll(db, id),
        ("check", &Method::Post) => check(db, id),
        ("callback", &Method::Get) => callback(db, id, url),
        ("cancel", &Method::Post) => {
            {
                let c = db.lock().map_err(db_error)?;
                bump(&c, id)?;
                crate::ui_credentials::pending_delete(&c,id)?;
            }
            status(db, id)
        }
        ("access", &Method::Put) => {
            let connection = db.lock().map_err(db_error)?;
            let mut info = store::get(&connection, "integration", id).map_err(db_error)?
                .unwrap_or(json!({"id":id,"agents":[],"write_agents":[],"repositories":[]}));
            effective_agents(&mut info);
            if payload.get("expected_agents").is_some_and(|expected| expected != &info["agents"]) {
                return Err((409, "Agent access changed. Reload Gmail settings and review the current assignments.".into()));
            }
            for key in ["agents", "write_agents", "repositories"] {
                if let Some(value) = payload.get(key) {
                    let rows = value
                        .as_array()
                        .filter(|v| v.len() <= 256)
                        .ok_or((400, "Invalid integration access list".into()))?;
                    if !rows.iter().all(|v| {
                        v.as_str()
                            .is_some_and(|s| s.len() <= 256 && !s.chars().any(char::is_control))
                    }) {
                        return Err((400, "Invalid access identity".into()));
                    }
                    info[key] = value.clone();
                }
            }
            effective_agents(&mut info);
            store::put(&connection, "integration", id, info)
                .map_err(db_error)?;
            drop(connection);
            status(db, id)
        }
        ("", &Method::Delete) => {
            let c = db.lock().map_err(db_error)?;
            bump(&c, id)?;
            crate::ui_credentials::delete(&c,&format!("oauth:{id}:token"))?;
            crate::ui_credentials::delete(&c,&format!("oauth:{id}:refresh"))?;
            crate::ui_credentials::pending_delete(&c,id)?;
            c.execute(
                "DELETE FROM ui_documents WHERE id=? AND kind IN ('integration','oauth-pending')",
                [id],
            )
            .map_err(db_error)?;
            drop(c);
            status(db, id)
        }
        ("repositories", &Method::Get) if id == "github" => {
            let token = bearer(db, id)?;
            let page = webui::query(url, "page")
                .parse::<u32>()
                .unwrap_or(1)
                .clamp(1, 100);
            let result = native_io::json(
                &format!(
                    "{}/user/repos?per_page=100&page={page}",
                    endpoints(id)["api"].as_str().unwrap()
                ),
                "GET",
                &[
                    ("Authorization".into(), format!("Bearer {token}")),
                    ("User-Agent".into(), "Kageha-Rust".into()),
                ],
                &json!({}),
                &AtomicBool::new(false),
            )
            .map_err(|_| (502, "Repository request failed".into()))?;
            Ok(json!({"repositories":result}))
        }
        _ => Err((404, "Integration operation not available".into())),
    }
}

pub(crate) fn model_start(db: &Database, provider: &str) -> ApiResult<Value> {
    let configured: Vec<String> = std::env::var("KAGEHA_OAUTH_MODEL_PROVIDERS")
        .ok()
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default();
    if !configured.iter().any(|v| v == provider) {
        return Err((409,"Register this provider's OAuth client and endpoints in the native server configuration first".into()));
    }
    start(db, &format!("provider-{provider}"))
}
pub(crate) fn model_inventory() -> Vec<String> {
    std::env::var("KAGEHA_OAUTH_MODEL_PROVIDERS")
        .ok()
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default()
}

pub(crate) fn readable(db: &Database, agent: &str) -> Vec<String> {
    let c = match db.lock() {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    store::list(&c, "integration")
        .unwrap_or_default()
        .into_iter()
        .filter(|v| {
            agent == "main" || v["agents"]
                .as_array()
                .is_some_and(|a| a.iter().any(|a| a == agent))
        })
        .filter_map(|v| {
            let id = v["id"].as_str()?;
            if !matches!(id, "gmail" | "github" | "linkedin" | "google-calendar" | "google-drive")
                || (id == "github" && v["repositories"].as_array().is_none_or(Vec::is_empty))
                || !crate::ui_credentials::oauth_ready(&c, id).unwrap_or(false)
            {
                return None;
            }
            let authority = crate::ui_credentials::oauth_authority(&c, id).ok().flatten()?;
            (v["authorization_id"].as_str() == Some(authority.as_str())).then(|| id.to_owned())
        })
        .collect()
}
pub(crate) fn gmail_access(db: &Database, agent: &str) -> ApiResult<Value> {
    let info = row(db, "gmail")?;
    let connection = db.lock().map_err(db_error)?;
    let authority = crate::ui_credentials::oauth_authority(&connection, "gmail")?;
    let verified = authority.as_deref().is_some_and(|id| info["authorization_id"].as_str() == Some(id));
    let assigned = info["agents"].as_array().is_some_and(|agents| agents.iter().any(|id| id == agent));
    let (status, next_step) = if authority.is_none() {
        ("disconnected", "Connect Gmail in Settings → Integrations → Gmail.")
    } else if !crate::ui_credentials::oauth_ready(&connection, "gmail")? || !verified {
        ("needs_verification", "Check or reconnect Gmail in Settings → Integrations → Gmail.")
    } else if !assigned {
        ("agent_not_assigned", "Select this agent and save its access in Settings → Integrations → Gmail.")
    } else {
        ("ready", "Search the connected mailbox with operation search.")
    };
    Ok(json!({"status":status,"next_step":next_step,"revision":generation(&connection, "gmail")?,"agents":info["agents"]}))
}

fn gmail_resource(args: &Value) -> ApiResult<Option<String>> {
    match args["operation"].as_str() {
        Some("status") => Ok(None),
        Some("search") => {
            let query = store::text(args, "query", 512)?;
            Ok(Some(format!("messages?q={}&maxResults=10", encode(query))))
        }
        Some(operation @ ("message" | "thread")) => {
            let id = store::text(args, "id", 128)?;
            if !id.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')) {
                return Err((400, "Use the message or thread ID returned by Gmail search.".into()));
            }
            let collection = if operation == "message" { "messages" } else { "threads" };
            Ok(Some(format!("{collection}/{id}")))
        }
        _ => Err((400, "Use status, search, message, or thread.".into())),
    }
}

pub(crate) fn gmail_read(db: &Database, agent: &str, args: &Value, cancel: &AtomicBool) -> ApiResult<Value> {
    let resource = gmail_resource(args)?;
    let access = gmail_access(db, agent)?;
    let mut result = match resource {
        Some(resource) if access["status"] == "ready" => read(db, agent, "gmail", &resource, cancel)
            .map(|data| json!({"status":"ok","data":data})),
        _ => Ok(access.clone()),
    }?;
    let blocked = matches!(result["status"].as_str(), Some("disconnected" | "needs_verification" | "agent_not_assigned"));
    result["connector_outcome"] = json!({"connector":"gmail","state":result["status"],"revision":access["revision"],"agents":access["agents"],"operation":args["operation"],
        "recovery":blocked.then_some("manage_access")});
    if blocked {
        result["guidance"] = json!("No mailbox data was read. Chat already shows how to restore access. Continue independent work using the user's request. Do not look for this mailbox data in workspace files, past chats, desktop apps, or a browser unless the user asks for that source.");
    }
    Ok(result)
}

pub(crate) fn read(
    db: &Database,
    agent: &str,
    id: &str,
    path: &str,
    cancel: &AtomicBool,
) -> ApiResult<Value> {
    let info = row(db, id)?;
    if !info["agents"]
        .as_array()
        .is_some_and(|a| a.iter().any(|v| v == agent))
    {
        return Err((403, "This agent has no access to the integration".into()));
    }
    if path.contains("..")
        || path.contains('#')
        || path.contains("\\")
        || path.contains("://")
        || path.chars().any(char::is_control)
        || path.len() > 4096
    {
        return Err((400, "Invalid integration resource path".into()));
    }
    let resource = path.trim_start_matches('/');
    if resource.split('?').next().unwrap_or("").contains('%') {
        return Err((400, "Encoded resource paths are not accepted".into()));
    }
    let accepted = match id {
        "google-calendar" => calendar_resource(resource),
        "google-drive" => drive_resource(resource),
        "gmail" => matches!(
            resource.split(['?', '/']).next(),
            Some("profile" | "messages" | "threads" | "labels")
        ),
        "linkedin" => resource == "userinfo",
        "github" => {
            let parts: Vec<_> = resource.split('/').collect();
            parts.len() >= 3
                && parts[0] == "repos"
                && info["repositories"].as_array().is_some_and(|repos| {
                    repos
                        .iter()
                        .any(|v| v == &format!("{}/{}", parts[1], parts[2]))
                })
        }
        _ => false,
    };
    if !accepted {
        return Err((
            403,
            "Resource is outside this integration's read-only access grant".into(),
        ));
    }
    let token = bearer(db, id)?;
    {let c=db.lock().map_err(db_error)?;let scope=crate::ui_credentials::oauth_authority(&c,id)?;if !scope.as_deref().is_some_and(|v|info["authorization_id"].as_str()==Some(v)){return Err((403,"Verify the current OAuth account before reusing integration access grants".into()));}}
    native_io::json(
        &format!(
            "{}/{}",
            endpoints(id)["api"].as_str().unwrap_or(""),
            resource
        ),
        "GET",
        &[
            ("Authorization".into(), format!("Bearer {token}")),
            ("User-Agent".into(), "Kageha-Rust".into()),
        ],
        &json!({}),
        cancel,
    )
    // Keep the transport's own message ("Provider returned HTTP 503", "Integration transport
    // failed", a deadline) so the Host can tell a transient failure from a refusal
    // (ui_tool_errors). The messages carry no token or response body.
    .map_err(|error| (502, format!("Integration read failed: {error}")))
}

/// Drive exposes only file listings and file metadata, never downloads or writes.
fn drive_resource(resource: &str) -> bool {
    let (path, query) = resource.split_once('?').unwrap_or((resource, ""));
    let listing = path == "files";
    let item = path.strip_prefix("files/").is_some_and(|id| !id.is_empty() && id.len() <= 256
        && id.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')));
    if !listing && !item { return false; }
    let mut names = std::collections::BTreeSet::new();
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        if !names.insert(key.to_string()) || value.len() > 1024 || value.chars().any(char::is_control) { return false; }
        let valid = match key.as_ref() {
            "fields" => !value.is_empty(),
            "q" | "orderBy" | "pageToken" => listing && !value.is_empty(),
            "pageSize" => listing && value.parse::<u32>().is_ok_and(|n| (1..=100).contains(&n)),
            _ => false,
        };
        if !valid { return false; }
    }
    true
}

/// Limit Calendar access to listing calendars and events in the signed-in
/// account's primary calendar. The integration grants no write operations.
fn calendar_resource(resource: &str) -> bool {
    let (path, query) = resource.split_once('?').unwrap_or((resource, ""));
    if !matches!(path, "users/me/calendarList" | "calendars/primary/events") { return false; }
    let mut names = std::collections::BTreeSet::new();
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        if !names.insert(key.to_string()) || value.len() > 1024 || value.chars().any(char::is_control) { return false; }
        let valid = match key.as_ref() {
            "maxResults" => value.parse::<u32>().is_ok_and(|n| (1..=250).contains(&n)),
            "timeMin" | "timeMax" => path == "calendars/primary/events" && !value.is_empty() && value.len() <= 64,
            "singleEvents" | "showDeleted" => matches!(value.as_ref(), "true" | "false"),
            "orderBy" => path == "calendars/primary/events" && matches!(value.as_ref(), "startTime" | "updated"),
            "pageToken" => !value.is_empty(),
            _ => false,
        };
        if !valid { return false; }
    }
    true
}

#[cfg(test)]
mod token_tests {
    use super::*;
    #[test]
    fn drive_resources_are_limited_to_metadata_reads() {
        assert!(drive_resource("files?pageSize=20&q=trashed%3Dfalse&fields=files%28id%2Cname%29"));
        assert!(drive_resource("files/Abc_123-x?fields=id%2Cname"));
        for rejected in ["files/../other", "files/a/export", "files/a?alt=media", "files?pageSize=101",
            "files/a?pageToken=abc", "files?pageSize=3&pageSize=2", "files?q=%0a", "changes"] {
            assert!(!drive_resource(rejected), "{rejected}");
        }
        assert!(endpoints("google-drive")["scope"].as_str().unwrap().contains("drive.metadata.readonly"));
    }
    #[test]
    fn drive_reads_require_agent_grant() {
        let conn=rusqlite::Connection::open_in_memory().unwrap();
        store::initialize(&conn).unwrap();
        crate::ui_credentials::initialize(&conn).unwrap();
        store::put(&conn,"integration","google-drive",json!({"id":"google-drive","agents":["other"]})).unwrap();
        let db=crate::agent_host::DatabaseState::open(conn).unwrap();
        assert_eq!(read(&db,"research","google-drive","files",&AtomicBool::new(false)).unwrap_err().0,403);
        assert!(!readable(&db,"research").contains(&"google-drive".to_owned()));
    }
    #[test]
    fn calendar_resources_are_strictly_scoped_to_read_only_paths() {
        assert!(calendar_resource("users/me/calendarList?maxResults=20"));
        assert!(calendar_resource("calendars/primary/events?timeMin=2026-01-01T00%3A00%3A00Z&singleEvents=true&maxResults=20"));
        for rejected in ["calendars/other/events", "calendars/primary", "users/me/calendarList?timeMin=now",
            "calendars/primary/events?maxResults=251", "calendars/primary/events?singleEvents=true&singleEvents=false",
            "calendars/primary/events?alt=media", "calendars/primary/events?timeMin=%0a"] {
            assert!(!calendar_resource(rejected), "{rejected}");
        }
    }
    #[test]
    fn calendar_reads_still_require_the_agent_grant() {
        let conn=rusqlite::Connection::open_in_memory().unwrap();
        store::initialize(&conn).unwrap();
        crate::ui_credentials::initialize(&conn).unwrap();
        store::put(&conn,"integration","google-calendar",json!({"id":"google-calendar","agents":["other"]})).unwrap();
        let db=crate::agent_host::DatabaseState::open(conn).unwrap();
        let error=read(&db,"research","google-calendar","calendars/primary/events",&AtomicBool::new(false)).unwrap_err();
        assert_eq!(error.0,403);
        assert!(!readable(&db,"research").contains(&"google-calendar".to_owned()));
    }
    #[test]
    fn gmail_status_distinguishes_connection_from_agent_access() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        store::initialize(&connection).unwrap();
        crate::ui_credentials::initialize(&connection).unwrap();
        crate::ui_credentials::oauth_store(
            &connection, "gmail", &json!({"access_token":"fixture-token"}), true, 0,
        ).unwrap();
        let authority = crate::ui_credentials::oauth_authority(&connection, "gmail").unwrap();
        store::put(&connection, "integration", "gmail", json!({
            "id":"gmail", "agents":[], "authorization_id":authority
        })).unwrap();
        let db = crate::agent_host::DatabaseState::open(connection).unwrap();
        let result = gmail_read(
            &db, "specialist", &json!({"operation":"search","query":"Seequent"}), &AtomicBool::new(false),
        ).unwrap();
        assert_eq!(gmail_access(&db, "main").unwrap()["status"], "ready");
        assert_eq!(result["status"], "agent_not_assigned");
        assert!(result["next_step"].as_str().unwrap().contains("save its access"));
    }

    #[test]
    fn gmail_tools_construct_only_supported_resources() {
        assert_eq!(gmail_resource(&json!({"operation":"status"})).unwrap(), None);
        assert_eq!(gmail_resource(&json!({"operation":"search","query":"Seequent & interview"})).unwrap(),
            Some("messages?q=Seequent%20%26%20interview&maxResults=10".into()));
        assert!(gmail_resource(&json!({"operation":"message","id":"../profile"})).is_err());
    }

    #[test]
    fn disconnected_credentials_cannot_be_restored_by_a_late_refresh() {
        let db = crate::agent_host::DatabaseState::open(
            rusqlite::Connection::open_in_memory().unwrap(),
        ).unwrap();
        store::initialize(&db.lock().unwrap()).unwrap();
        store_token(&db, "fixture", json!({"access_token":"first"}), true, 0).unwrap();
        {
            let c = db.lock().unwrap();
            bump(&c, "fixture").unwrap();
            crate::ui_credentials::delete(&c,"oauth:fixture:token").unwrap();
            crate::ui_credentials::delete(&c,"oauth:fixture:refresh").unwrap();
        }
        assert_eq!(
            store_token(&db, "fixture", json!({"access_token":"late"}), false, 1)
                .unwrap_err()
                .0,
            409
        );
        assert!(
            ui_settings::secret(&db.lock().unwrap(), "oauth:fixture:token")
                .unwrap()
                .is_empty()
        );
    }
}

fn device_start(db: &Database, id: &str) -> ApiResult<Value> {
    let config = configuration(db, id)?;
    if config["client_id"].as_str().is_none_or(|v| v.is_empty()) {
        return Err((409, "Configure the GitHub OAuth client ID first".into()));
    }
    let reply = form(
        endpoints(id)["device"].as_str().unwrap_or(""),
        &json!({"client_id":config["client_id"],"scope":endpoints(id)["scope"]}),
    )?;
    let code = store::text(&reply, "device_code", 4096)?;
    let user_code = store::text(&reply, "user_code", 128)?;
    let uri = store::text(&reply, "verification_uri", 2048)?;
    if !uri.starts_with("https://") {
        return Err((502, "Invalid verification URL".into()));
    }
    let expires = reply["expires_in"]
        .as_f64()
        .unwrap_or(900.0)
        .clamp(30.0, 1800.0);
    let interval = reply["interval"].as_f64().unwrap_or(5.0).clamp(1.0, 60.0);
    let mut c = db.lock().map_err(db_error)?;
    let tx = c.transaction().map_err(db_error)?;
    let epoch = bump(&tx, id)?;
    crate::ui_credentials::pending_put(&tx,id,json!({"kind":"device","code":code,"config":config,"expires":store::now()+expires,"interval":interval,"next_poll":0,"generation":epoch}))?;
    tx.commit().map_err(db_error)?;
    Ok(
        json!({"ok":true,"user_code":user_code,"verification_uri":uri,"expires_in":expires,"interval":interval}),
    )
}
fn device_poll(db: &Database, id: &str) -> ApiResult<Value> {
    let pending = {
        let mut c = db.lock().map_err(db_error)?;
        let tx = c.transaction().map_err(db_error)?;
        let mut pending = crate::ui_credentials::pending_get(&tx,id)?
            .filter(|v| v["kind"] == "device")
            .ok_or((409, "Start device sign-in first".into()))?;
        if pending["expires"].as_f64().is_none_or(|v| v < store::now()) {
            return Err((409, "Device sign-in expired".into()));
        }
        if pending["next_poll"]
            .as_f64()
            .is_some_and(|v| v > store::now())
        {
            return Ok(json!({"ok":false,"pending":true,"detail":"Waiting for authorization"}));
        }
        pending["next_poll"] = json!(store::now() + pending["interval"].as_f64().unwrap_or(5.0));
        crate::ui_credentials::pending_put(&tx,id,pending.clone())?;
        tx.commit().map_err(db_error)?;
        pending
    };
    let config = configuration(db, id)?;
    if config != pending["config"] {
        return Err((409, "OAuth client changed; restart sign-in".into()));
    }
    let reply = form(
        endpoints(id)["token"].as_str().unwrap_or(""),
        &json!({"client_id":config["client_id"],"device_code":pending["code"],"grant_type":"urn:ietf:params:oauth:grant-type:device_code"}),
    )?;
    if reply["error"] == "slow_down" {
        let connection = db.lock().map_err(db_error)?;
        if let Some(mut current) = crate::ui_credentials::pending_get(&connection,id)?
            .filter(|v| v["generation"] == pending["generation"])
        {
            let interval = (current["interval"].as_f64().unwrap_or(5.0) + 5.0).min(120.0);
            current["interval"] = json!(interval);
            current["next_poll"] = json!(store::now() + interval);
            crate::ui_credentials::pending_put(&connection,id,current)?;
        }
    }
    if matches!(
        reply["error"].as_str(),
        Some("authorization_pending" | "slow_down")
    ) {
        return Ok(json!({"ok":false,"pending":true,"detail":"Waiting for authorization"}));
    }
    if reply.get("error").is_some() {
        return Err((409, "Device sign-in was rejected or expired".into()));
    }
    store_token(
        db,
        id,
        reply,
        true,
        pending["generation"].as_u64().unwrap_or(0),
    )?;
    crate::ui_credentials::pending_delete(&*db.lock().map_err(db_error)?,id)?;
    check(db, id)
}

fn client_credential(db:&rusqlite::Connection,id:&str,environment:impl FnOnce()->String)->ApiResult<String>{let name=format!("oauth:{id}:client");let secret=ui_settings::secret(db,&name)?;if secret.is_empty()&&!crate::ui_credentials::managed(db,&name)?{Ok(environment())}else{Ok(secret)}}
#[cfg(test)]mod credential_authority_tests {use super::*;#[test]fn revoked_client_secrets_never_select_an_old_environment_value(){let db=rusqlite::Connection::open_in_memory().unwrap();store::initialize(&db).unwrap();crate::ui_credentials::initialize(&db).unwrap();ui_settings::save_secret(&db,"oauth:fixture:client","new-client-secret").unwrap();crate::ui_credentials::delete(&db,"oauth:fixture:client").unwrap();assert!(client_credential(&db,"fixture",||panic!("revocation selected the environment")).unwrap().is_empty());assert_eq!(client_credential(&db,"explicit-source",||"read-only-environment".into()).unwrap(),"read-only-environment");}
#[test]fn new_authorization_cannot_inherit_unverified_account_access(){let db=rusqlite::Connection::open_in_memory().unwrap();store::initialize(&db).unwrap();crate::ui_credentials::initialize(&db).unwrap();crate::ui_credentials::oauth_store(&db,"gmail",&json!({"access_token":"first-token"}),true,0).unwrap();let previous=crate::ui_credentials::oauth_authority(&db,"gmail").unwrap().unwrap();store::put(&db,"integration","gmail",json!({"id":"gmail","agents":["main"],"authorization_id":previous})).unwrap();crate::ui_credentials::oauth_store(&db,"gmail",&json!({"access_token":"new-account-token"}),true,1).unwrap();let current=crate::ui_credentials::oauth_authority(&db,"gmail").unwrap().unwrap();assert_ne!(current,previous);crate::ui_credentials::oauth_store(&db,"gmail",&json!({"access_token":"refreshed-token"}),false,2).unwrap();assert_eq!(crate::ui_credentials::oauth_authority(&db,"gmail").unwrap().unwrap(),current);let db=crate::agent_host::DatabaseState::open(db).unwrap();let denied=read(&db,"main","gmail","profile",&AtomicBool::new(false)).unwrap_err();assert_eq!(denied.0,403);assert!(denied.1.contains("Verify the current OAuth account"));}}

#[cfg(test)]
mod oauth_diagnostic_tests {
    use super::oauth_exchange_error;

    #[test]
    fn recognized_errors_explain_the_required_recovery_without_remote_text() {
        let body = br#"{"error":"invalid_grant","error_description":"secret-token and private@example.test","refresh_token":"secret-refresh"}"#;
        let refresh = oauth_exchange_error(400, body, true);
        assert!(refresh.contains("HTTP 400; provider_error=invalid_grant"));
        assert!(refresh.contains("Reconnect"));
        assert!(!refresh.contains("secret") && !refresh.contains("private@"));
        let authorization = oauth_exchange_error(400, body, false);
        assert!(authorization.contains("new authorization flow"));
        let client = oauth_exchange_error(401, br#"{"error":"invalid_client"}"#, true);
        assert!(client.contains("client configuration"));
    }

    #[test]
    fn unknown_malformed_oversized_and_non_json_bodies_are_never_exposed() {
        for body in [br#"{"error":"secret-token","error_description":"private"}"#.to_vec(),
            b"<html>secret-refresh</html>".to_vec(), b"{".to_vec(),
            vec![b'x'; 16 * 1024 + 1]] {
            assert_eq!(oauth_exchange_error(400, &body, true), "OAuth exchange failed (HTTP 400)");
        }
    }
}
