//! Native settings and provider catalog. Credentials never appear in API replies.
use crate::{
    native_chat, server,
    ui_store::{self as store, db_error, ApiResult},
    webui::{self, Database},
};
use rusqlite::params;
#[derive(Clone)]
pub(crate) struct Provider {pub url:String,pub key:String,pub model:String,pub protocol:String,pub source:Option<crate::ui_credentials::ModelSource>,
    /// The model entry's `context_window` in tokens, when it has one.
    pub context_window:Option<u64>}
impl Provider {fn validate(&self)->std::io::Result<()>{kageha_models::Provider{url:self.url.clone(),key:self.key.clone(),model:self.model.clone(),protocol:self.protocol.clone()}.validate()}}
use serde_json::{json, Value};
use std::sync::{atomic::AtomicBool, OnceLock};
use tiny_http::{Method, Request};
static MODEL_ENV: OnceLock<kageha_models::catalog::Environment> = OnceLock::new();
pub(crate) fn model_env(name: &str) -> Option<String> {
    MODEL_ENV
        .get()
        .and_then(|env| env.get(name))
        .or_else(|| std::env::var(name).ok())
}
fn setting(db: &rusqlite::Connection, id: &str, default: Value) -> ApiResult<Value> {
    Ok(store::get(db, "setting", id)
        .map_err(db_error)?
        .unwrap_or(default))
}
pub(crate) fn secret(db: &rusqlite::Connection, id: &str) -> ApiResult<String> {
    crate::ui_credentials::secret(db,id)
}
pub(crate) fn save_secret(db: &rusqlite::Connection, id: &str, key: &str) -> ApiResult<()> {
    if key.len() > 17 * 1024 * 1024 || key.chars().any(char::is_control) {
        return Err((400, "Invalid credential".into()));
    }
    crate::ui_credentials::save(db,id,key)
}
fn valid_url(value: &str, key: &str) -> ApiResult<String> {
    let value = value.trim().trim_end_matches('/');
    if value.len() > 2048
        || value.chars().any(char::is_control)
        || !(value.starts_with("http://") || value.starts_with("https://"))
        || value
            .split("://")
            .nth(1)
            .is_none_or(|v| v.is_empty() || v.split('/').next().unwrap_or("").contains('@'))
    {
        return Err((
            400,
            "An HTTP(S) endpoint without embedded credentials is required".into(),
        ));
    }
    if !key.is_empty() && !value.starts_with("https://") && !value.strip_prefix("http://").is_some_and(|s|s.split('/').next().and_then(|h|h.strip_prefix("127.0.0.1:")).is_some_and(|port|port.parse::<u16>().is_ok_and(|p|p>0))) {
        return Err((400, "Provider credentials require HTTPS".into()));
    }
    Ok(value.into())
}
fn sync_environment_model(
    db: &rusqlite::Connection,
    model: &str,
    url: &str,
) -> rusqlite::Result<()> {
    let existing = store::get(db, "model", model)?;
    if existing
        .as_ref()
        .is_none_or(|value| value["provider"] == "environment")
    {
        store::put(
            db,
            "model",
            model,
            json!({"id":model,"provider":"environment","model":model,"base_url":url,"protocol":"openai_compat"}),
        )?;
    }
    Ok(())
}
pub(crate) fn initialize(db: &rusqlite::Connection) -> rusqlite::Result<()> {
    if let (Ok(model), Ok(url)) = (
        std::env::var("KAGEHA_MODEL"),
        std::env::var("KAGEHA_PROVIDER_URL"),
    ) {
        sync_environment_model(db, &model, &url)?;
    }
    Ok(())
}
pub(crate) fn initialize_workspace(
    db: &rusqlite::Connection,
    workspace: &std::path::Path,
) -> rusqlite::Result<()> {
    let env = kageha_models::catalog::Environment::load(
        &kageha_models::catalog::default_env_paths(workspace),
    )
    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    let _ = MODEL_ENV.set(env);
    let catalog = kageha_models::catalog::Catalog::load(
        &kageha_models::catalog::default_paths(workspace),
        model_env,
    )
    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    sync_catalog(db, &catalog)?;
    Ok(())
}
// Remember imported baselines so restarts refresh catalog entries without replacing UI edits.
fn sync_catalog(
    db: &rusqlite::Connection,
    catalog: &kageha_models::catalog::Catalog,
) -> rusqlite::Result<()> {
    let tx = db.unchecked_transaction()?;
    let db = &tx;
    let content = |value: &Value| {
        let mut value = value.clone();
        if let Some(fields) = value.as_object_mut() {
            fields.remove("revision");
        }
        value
    };
    for row in &catalog.models {
        let id = row["id"].as_str().unwrap();
        let existing = store::get(db, "model", id)?;
        let previous = store::get(db, "catalog-model", id)?;
        // An explicitly imported account-scoped endpoint remains usable when
        // the old dotenv source is no longer loaded after vault migration.
        // Never downgrade it to a literal unresolved environment placeholder.
        let mut imported=row.clone();
        if imported["base_url"].as_str().is_some_and(|url|url.contains('$')) {
            if let Some(base)=previous.as_ref().or(existing.as_ref()).filter(|old|old["protocol"]==imported["protocol"]).and_then(|old|old["base_url"].as_str()).filter(|base|!base.is_empty()&&!base.contains('$')) {
                imported["base_url"]=json!(base);
            }
        }
        let row=&imported;
        if existing.is_none()
            || previous.as_ref().is_some_and(|old| {
                existing
                    .as_ref()
                    .is_some_and(|v| content(v) == content(old))
            })
        {
            if existing.as_ref().is_none_or(|v| content(v) != *row) {
                store::put(db, "model", id, row.clone())?;
            }
        }
        if previous.as_ref().is_none_or(|v| content(v) != *row) {
            store::put(db, "catalog-model", id, row.clone())?;
        }
    }
    if store::get(db, "setting", "catalog-roles")?
        .as_ref()
        .is_none_or(|v| content(v) != catalog.roles)
    {
        store::put(db, "setting", "catalog-roles", catalog.roles.clone())?;
    }
    tx.commit()?;
    Ok(())
}
pub(crate) fn model_key(db: &rusqlite::Connection, model: &Value) -> ApiResult<String> {
    let provider = model["provider"].as_str().unwrap_or("environment");
    let saved = secret(db, provider)?;
    if !saved.is_empty() {
        return Ok(saved);
    }
    if crate::ui_credentials::managed(db,provider)? { return Ok(String::new()); }
    let name = if provider == "environment" {
        "KAGEHA_API_KEY"
    } else {
        model["api_key_env"].as_str().unwrap_or("")
    };
    Ok(kageha_models::catalog::credential(name, model_env))
}
fn codex_available(model: &Value) -> bool {
    model["provider"] == "openai-codex-app"
        && model["model"].as_str().is_some_and(kageha_models::codex::available)
}
fn available(db: &rusqlite::Connection, model: &Value) -> ApiResult<bool> {
    if model["protocol"] == "codex_app_server" {
        return Ok(codex_available(model));
    }
    if !kageha_models::catalog::supported(model["protocol"].as_str().unwrap_or("openai_compat"))
    {
        return Ok(false);
    }
    endpoint_ready(db, model)
}

pub(crate) fn endpoint_ready(db: &rusqlite::Connection, model: &Value) -> ApiResult<bool> {
    if model["base_url"].as_str().unwrap_or("").contains('$') {
        return Ok(false);
    }
    if let Some(id) = model["oauth_id"].as_str() {
        return Ok(crate::ui_credentials::oauth_ready(db,id).unwrap_or(false));
    }
    let base = model["base_url"].as_str().unwrap_or("");
    let Ok(url) = kageha_models::catalog::completion_url(
        base,
        model["protocol"].as_str().unwrap_or("openai_compat"),
    ) else {
        return Ok(false);
    };
    let key = match model_key(db, model) {Ok(key)=>key,Err(_)=>return Ok(false)};
    if key.is_empty() && crate::ui_credentials::managed(db,model["provider"].as_str().unwrap_or("environment"))? {return Ok(false);}
    if (Provider {
        url,
        key: key.clone(),
        model: model["model"].as_str().unwrap_or("unknown").into(),
        protocol: model["protocol"].as_str().unwrap_or("openai_compat").into(),
        source: None,
        context_window: None,
    })
    .validate()
    .is_err()
    {
        return Ok(false);
    }
    // Anonymous proxies omit api_key_env. An explicitly configured credential
    // remains required even on loopback, matching dispatch/revocation checks.
    Ok(model["api_key_env"].as_str().is_none_or(str::is_empty) || !key.is_empty())
}
fn selected_id(db: &rusqlite::Connection, requested: &str, role: &str) -> ApiResult<String> {
    if !requested.is_empty() {
        return Ok(requested.into());
    }
    let roles = setting(db, "roles", json!({}))?;
    if let Some(id) = roles[role]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| roles["default"].as_str().filter(|s| !s.is_empty()))
    {
        return Ok(id.into());
    }
    if let Ok(id) = std::env::var("KAGEHA_MODEL") {
        if !id.is_empty() {
            return Ok(id);
        }
    }
    let ladders = setting(db, "catalog-roles", json!({}))?;
    for id in ladders
        .get(role)
        .or_else(|| ladders.get("default"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if let Some(model) = store::get(db, "model", id).map_err(db_error)? {
            if available(db, &model)? {
                return Ok(id.into());
            }
        }
    }
    Err((
        400,
        "Configure a ready model or select a default model".into(),
    ))
}
pub(crate) fn models(db: &Database) -> ApiResult<Vec<Value>> {
    let mut rows = store::list(&*db.lock().map_err(db_error)?, "model").map_err(db_error)?;
    let mut readiness = std::collections::HashMap::new();
    for row in &mut rows {
        // Availability varies by connection and endpoint. Model names only
        // affect readiness when they fail validation.
        let model = row["model"].as_str().unwrap_or("unknown");
        let valid_name = !model.trim().is_empty() && model.len() <= 256
            && !model.chars().any(char::is_control);
        let key = (["provider", "protocol", "base_url", "api_key_env", "oauth_id"]
            .map(|field| row[field].as_str().map(str::to_owned)), valid_name);
        let ready = if row["protocol"] == "codex_app_server" {
            // Probing the external app server can take seconds. Keep chat admission
            // and token publication independent of this catalog refresh.
            codex_available(row)
        } else {
            match readiness.entry(key) {
                std::collections::hash_map::Entry::Occupied(entry) => *entry.get(),
                std::collections::hash_map::Entry::Vacant(entry) => {
                    *entry.insert(available(&*db.lock().map_err(db_error)?, row)?)
                }
            }
        };
        row["ready"] = json!(ready);
        row["supported"] = json!(kageha_models::catalog::supported(
            row["protocol"].as_str().unwrap_or("openai_compat")
        ));
    }
    Ok(rows)
}
pub(crate) fn configured(db: &Database, requested: &str, role: &str) -> ApiResult<Provider> {
    let connection = db.lock().map_err(db_error)?;
    let id = selected_id(&connection, requested, role)?;
    let id = id.as_str();
    let model = store::get(&connection, "model", id)
        .map_err(db_error)?
        .ok_or((400, "The selected model is not configured".into()))?;
    if model["protocol"] == "codex_app_server" {
        if !available(&connection, &model)? {
            return Err((400, "This model is unavailable in the local Codex account".into()));
        }
        return Ok(Provider {
            url: "codex://local".into(), key: String::new(),
            model: model["model"].as_str().unwrap_or_default().into(),
            protocol: "codex_app_server".into(), source: None,
            context_window: model_context_window(&model),
        });
    }
    // Preserve the concrete vault error instead of masking it as a missing adapter.
    if model["oauth_id"].as_str().is_none() && kageha_models::catalog::supported(model["protocol"].as_str().unwrap_or("openai_compat")) {
        model_key(&connection, &model)?;
    }
    if !available(&connection, &model)? {
        return Err((
            400,
            "Selected model needs credentials, endpoint configuration or a supported adapter"
                .into(),
        ));
    }
    let key = model_key(&connection, &model)?;
    let base = model["base_url"].as_str().unwrap_or("");
    let protocol = model["protocol"].as_str().unwrap_or("openai_compat");
    let base = valid_url(base, &key)?;
    let url = kageha_models::catalog::completion_url(&base, protocol)
        .map_err(|e| (400, e.to_string()))?;
    Ok(Provider {
        source:Some(crate::ui_credentials::ModelSource{db:db.clone(),id:id.into(),provider:model["provider"].as_str().unwrap_or("environment").into(),model:model["model"].as_str().unwrap_or(id).into(),protocol:protocol.into(),url:url.clone(),destination:crate::ui_credentials::model_origin(&url)?}),
        url,
        key:String::new(),
        model: model["model"].as_str().unwrap_or(id).into(),
        protocol: protocol.into(),
        context_window: model_context_window(&model),
    })
}
/// The `context_window` of a configured model, from its `models.yaml` entry. `None` when the
/// entry has none; the runtime then assumes a conservative default.
fn model_context_window(model: &Value) -> Option<u64> {
    model["context_window"].as_u64().filter(|tokens| *tokens > 0)
}
fn list_models(db: &Database) -> ApiResult<Value> {
    let rows = models(db)?;
    let (default, credentials_locked) = {
        let connection = db.lock().map_err(db_error)?;
        (selected_id(&connection, "", "default").unwrap_or_default(),
         crate::ui_credentials::keychain_status(&connection)["locked"] == true)
    };
    Ok(
        json!({"models":rows.iter().map(|v|v["id"].clone()).collect::<Vec<_>>(),"all":rows.into_iter().map(public_model).collect::<Vec<_>>(),"default":default,"default_model":default,"credentials_locked":credentials_locked}),
    )
}
fn public_model(mut row: Value) -> Value {
    if let Some(fields) = row.as_object_mut() {
        for key in ["base_url", "api_key_env", "oauth_id"] {
            fields.remove(key);
        }
    }
    row
}
fn search_status(db: &rusqlite::Connection) -> ApiResult<Value> {
    setting(db, "search", json!({"backend":"none","order":[],"engines":[],"available":false}))
}

fn setup(db: &Database) -> ApiResult<Value> {
    let rows = models(db)?;
    let connection = db.lock().map_err(db_error)?;
    let mut providers = std::collections::BTreeSet::<String>::new();
    for row in &rows {
        providers.insert(row["provider"].as_str().unwrap_or("").into());
    }
    let mut connections: Vec<_> = providers
        .iter()
        .map(|id| json!({"id":id,"label":id,"kind":"custom","ready":rows.iter().any(|v|v["provider"]==id.as_str()&&v["ready"]==true),"detected":true}))
        .collect();
    let oauth = crate::native_auth::model_inventory();
    connections.retain(|row| {
        row["id"]
            .as_str()
            .is_none_or(|id| !oauth.iter().any(|value| value == id))
    });
    for id in oauth {
        connections.push(json!({"id":id,"label":id,"kind":"oauth","auth_flow":"pkce","ready":rows.iter().any(|v|v["provider"]==id&&v["ready"]==true)}));
    }
    let default = selected_id(&connection, "", "default").unwrap_or_default();
    let mut roles = setting(
        &connection,
        "roles",
        json!({"default":default,"planner":default,"worker":default}),
    )?;
    if roles["default"].as_str().is_none_or(str::is_empty) {
        roles["default"] = json!(default);
    }
    Ok(
        json!({"ok":true,"credentials":crate::ui_credentials::keychain_status(&connection),"connections":connections,"models":rows.into_iter().map(public_model).collect::<Vec<_>>(),"default_model":roles["default"],"roles":roles,"tools":{},"search":search_status(&connection)?}),
    )
}
pub(crate) fn handles(path: &str) -> bool {
    [
        "/api/settings/persistence",
        "/api/provider-setup",
        "/api/models",
        "/api/capabilities",
        "/api/model",
        "/api/audio8-settings",
        "/api/mcp",
        "/api/extensions",
    ]
    .iter()
    .any(|p| path == *p || path.starts_with(&format!("{p}/")))
}
fn operation(db: &Database, path: &str, method: &Method, payload: Value) -> ApiResult<Value> {
    if path == "/api/provider-setup/browser-credentials" {
        if method != &Method::Get { return Err((405, "Method not allowed".into())); }
        let mut client = crate::browser_client::Client::connect("settings", "user")
            .map_err(|_| (503, "Browser credential service is unavailable".into()))?;
        return client.call("credential_vault_status", json!({}), None)
            .map_err(|_| (503, "Browser credential status is unavailable".into()));
    }
    if path == "/api/provider-setup/browser-credentials/authorize" {
        if method != &Method::Post { return Err((405, "Method not allowed".into())); }
        let mut client = crate::browser_client::Client::connect("settings", "user")
            .map_err(|_| (503, "Browser credential service is unavailable".into()))?;
        return client.call_user_interactive("credential_vault_authorize", json!({}), None)
            .map_err(|_| (503, "Could not authorize browser credentials. Check the macOS Keychain prompt and try again.".into()));
    }
    if path == "/api/provider-setup/unlock" && method == &Method::Post {
        crate::ui_credentials::authorize_keychain(db)?;
        // Turns held after a restart continue now (Requirement 13.4). The unlock
        // itself succeeded even if a turn cannot continue.
        let resumed = crate::ui_turn_resume::unlocked(db).unwrap_or_else(|(_, error)| {
            eprintln!("Waiting turns were not resumed after unlock: {error}");
            Vec::new()
        });
        return Ok(json!({"ok":true,"resumed_turns":resumed}));
    }
    if matches!(path, "/api/models" | "/api/model") && method == &Method::Get {
        return list_models(db);
    }
    if path == "/api/capabilities" && method == &Method::Get {
        let rows = models(db)?;
        let mut providers = std::collections::BTreeSet::new();
        for model in &rows {
            if let Some(provider) = model["provider"].as_str() {
                providers.insert(provider.to_owned());
            }
        }
        return Ok(
            json!({"harness":{"recovery_version":kageha_runtime::recovery::RECOVERY_VERSION,
                "features":["bounded_semantic_reads","terminal_submission_recovery","observed_reference_membership","click_delivery_is_not_progress"]},
                "providers":providers.into_iter().map(|id| { let ready = rows.iter().any(|v|v["provider"]==id && v["ready"]==true); json!({"id":id,"ready":ready,"capabilities":if ready {vec!["tool_calling"]} else {vec![]}}) }).collect::<Vec<_>>()}),
        );
    }
    if path == "/api/provider-setup/search" && method == &Method::Get {
        let connection = db.lock().map_err(db_error)?;
        return Ok(json!({"search":search_status(&connection)?}));
    }
    if path == "/api/provider-setup" && method == &Method::Get {
        return setup(db);
    }
    if path == "/api/mcp/oauth/mtls-credential" && method == &Method::Post {
        let payload=crate::ui_credentials::SecretJson::new(payload);
        return crate::ui_mcp_mtls::provision(db, &payload);
    }
    if path == "/api/mcp/oauth/assertion-credential" && method == &Method::Post {
        let payload=crate::ui_credentials::SecretJson::new(payload);
        return crate::ui_mcp_oauth::provision_assertion(db, &payload);
    }
    if path == "/api/mcp/oauth/start" && method == &Method::Post {
        let name = store::text(&payload, "name", 64)?;
        return crate::ui_mcp_oauth::start(db, name);
    }
    if path == "/api/mcp/oauth/status" && method == &Method::Post {
        let name = store::text(&payload, "name", 64)?;
        return crate::ui_mcp_oauth::status(db, name);
    }
    if path == "/api/mcp/oauth/cancel" && method == &Method::Post {
        let name = store::text(&payload, "name", 64)?;
        return crate::ui_mcp_oauth::cancel(db, name);
    }
    if path == "/api/mcp/oauth/disconnect" && method == &Method::Post {
        let name = store::text(&payload, "name", 64)?;
        return crate::ui_mcp_oauth::disconnect(db, name);
    }
    if path == "/api/mcp" && matches!(method, Method::Post | Method::Put) {
        return crate::ui_mcp::save(db, &payload, false);
    }
    if path == "/api/mcp/test" {
        return crate::ui_mcp::save(db, &payload, true);
    }
    if path == "/api/provider-setup/test" {
        let id = payload["model_id"].as_str().unwrap_or("");
        let cfg = configured(db, id, "default")?;
        let message = native_chat::complete_with(
            &cfg,
            vec![json!({"role":"user","content":"Reply with OK."})],
            &AtomicBool::new(false),
            vec![],
        )
        .map_err(|_| (502, "Model connection failed".into()))?;
        return Ok(json!({"ok":true,"model_id":id,"message":message["message"]["content"]}));
    }
    if path == "/api/provider-setup/discover" {
        let base = valid_url(
            payload["base_url"].as_str().unwrap_or(""),
            payload["api_key"].as_str().unwrap_or(""),
        )?;
        let rows = kageha_models::discover_models(
            &base,
            payload["api_key"].as_str().unwrap_or(""),
            payload["protocol"].as_str().unwrap_or("openai_compat"),
            &kageha_contracts::execution::Control {
                cancelled: &AtomicBool::new(false),
                deadline: None,
            },
        )
        .map_err(|e| (502, e.to_string()))?;
        return Ok(json!({"ok":true,"models":rows}));
    }
    if path == "/api/provider-setup/connect" || path == "/api/provider-setup/model" {
        let provider = payload["provider_name"]
            .as_str()
            .or_else(|| payload["provider"].as_str())
            .unwrap_or("custom");
        let provider_id = provider.trim().to_ascii_lowercase().replace(' ', "-");
        let provider = provider_id.as_str();
        if !server::identifier(provider) {
            return Err((400, "Invalid provider id".into()));
        }
        let protocol = payload["protocol"].as_str().unwrap_or("openai_compat");
        if !matches!(protocol, "openai_compat" | "anthropic_compat") && !kageha_models::decisions::is_typed_protocol(protocol) {
            return Err((
                400,
                "Choose a supported chat or decision protocol".into(),
            ));
        }
        let key = payload["api_key"].as_str().unwrap_or("");
        let base = valid_url(payload["base_url"].as_str().unwrap_or(""), key)?;
        let selected = payload["models"]
            .as_array()
            .cloned()
            .unwrap_or_else(|| vec![payload["model"].clone()]);
        if selected.is_empty() || selected.len() > 256 {
            return Err((400, "Choose one to 256 models".into()));
        }
        let mut definitions = Vec::new();
        for item in selected {
            let model = item
                .as_str()
                .or_else(|| item["id"].as_str())
                .filter(|s| !s.is_empty() && s.len() < 256 && !s.chars().any(char::is_control))
                .ok_or((400, "Invalid model id".into()))?;
            let id = payload["model_id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("{provider}/{model}"));
            definitions.push(json!({"id":id,"provider":provider,"model":model,"base_url":base,"protocol":protocol,"api_key_env":payload["api_key_env"]}));
        }
        {
            let mut connection = db.lock().map_err(db_error)?;
            let tx = connection.transaction().map_err(db_error)?;
            if !key.is_empty() {
                save_secret(&tx, provider, key)?;
            }
            for definition in &definitions {
                let id=definition["id"].as_str().unwrap();
                let mut saved=store::get(&tx,"model",id).map_err(db_error)?.unwrap_or(json!({}));
                if let Some(expected)=payload["expected_revision"].as_u64() {
                    if definitions.len()!=1 || saved["revision"].as_u64().unwrap_or(0)!=expected {
                        return Err((409,"Model configuration changed; refresh before applying migration".into()));
                    }
                }
                for (field,value) in definition.as_object().unwrap() {
                    if field=="api_key_env" && value.is_null() {continue;}
                    saved[field]=value.clone();
                }
                store::put(
                    &tx,
                    "model",
                    id,
                    saved,
                )
                .map_err(db_error)?;
            }
            tx.commit().map_err(db_error)?;
        }
        return Ok(
            json!({"ok":true,"provider":provider,"id":definitions[0]["id"],"models":definitions.iter().map(|v|v["id"].clone()).collect::<Vec<_>>()}),
        );
    }
    if path == "/api/provider-setup/key" {
        let provider = store::text(&payload, "provider", 64)?;
        let key = store::text(&payload, "api_key", 16384)?;
        if !server::identifier(provider) {
            return Err((400, "Invalid provider".into()));
        }
        save_secret(&*db.lock().map_err(db_error)?, provider, key)?;
        return Ok(json!({"ok":true,"provider":provider,"ready":true}));
    }
    if path == "/api/provider-setup/oauth" {
        return crate::native_auth::model_start(db, payload["provider"].as_str().unwrap_or(""));
    }
    if path == "/api/provider-setup/roles" || path == "/api/provider-setup/search" {
        let key = if path.ends_with("roles") {
            "roles"
        } else {
            "search"
        };
        let mut connection = db.lock().map_err(db_error)?;
        let tx = connection.transaction().map_err(db_error)?;
        let mut value = setting(&tx, key, json!({}))?;
        for (k, v) in payload
            .as_object()
            .ok_or((400, "An object is required".into()))?
        {
            if key == "roles"
                && !v.as_str().is_some_and(|id| {
                    id.is_empty() || store::get(&tx, "model", id).ok().flatten().is_some()
                })
            {
                return Err((400, "Unknown role model".into()));
            }
            value[k] = v.clone();
        }
        let saved = store::put(&tx, "setting", key, value).map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        return Ok(json!({"ok":true,key:saved,"message":"Settings saved. Applies to new runs."}));
    }
    if path == "/api/settings/persistence" {
        let connection=db.lock().map_err(db_error)?;
        let policy=if method==&Method::Post {
            let policy=crate::ui_persistence::Policy::parse(&payload).map_err(|error|(400,error.to_string()))?;
            store::put(&connection,"setting","persistence",json!(policy)).map_err(db_error)?;
            policy
        } else {crate::ui_persistence::settings(&connection).map_err(db_error)?};
        return Ok(json!({"policy":policy,"message":"Applies to new requests. Model calls are shared across all workers; the limit counts active execution and excludes waiting for input and time offline."}));
    }
    if path == "/api/models/decisions/test" {
        if method != &Method::Post {
            return Err((405, "Method not allowed".into()));
        }
        let id = store::text(&payload, "model_id", 256)?;
        let metric = match payload["score_metric"].as_str() {
            Some(metric @ ("selected_probability" | "provider_confidence")) => metric,
            _ => return Err((400, "Invalid decision score type".into())),
        };
        return Ok(crate::ui_browser_decisions::probe(db, id, metric));
    }
    if matches!(path, "/api/models/fallback" | "/api/models/decisions") {
        let decisions = path.ends_with("decisions");
        let key = if decisions { "decisions" } else { "fallback" };
        let default = if decisions {
            json!({"mode":"off","model":"","fallback_models":[],"judge_fallback":false,"threshold":0.9,"timeout_s":3.0,"overrides":{}})
        } else {
            json!({"enabled":true,"providers":{},"models":{}})
        };
        let connection = db.lock().map_err(db_error)?;
        if decisions {crate::ui_browser_decisions::register_typed(&connection).map_err(db_error)?;}
        let policy = if method == &Method::Post {
            if decisions {
                if !matches!(payload["mode"].as_str(), Some("off" | "shadow" | "active"))
                    || !payload["threshold"]
                        .as_f64()
                        .is_some_and(|v| (0.5..=1.0).contains(&v))
                    || !payload["timeout_s"]
                        .as_f64()
                        .is_some_and(|v| (0.1..=15.0).contains(&v))
                {
                    return Err((400, "Invalid decision policy limits".into()));
                }
            } else if !payload["enabled"].is_boolean()
                || !payload["providers"].is_object()
                || !payload["models"].is_object()
            {
                return Err((400, "Invalid fallback policy".into()));
            }
            validate_policy(&payload, decisions)?;
            if decisions {
                let configured=crate::ui_browser_decisions::decode_policy(&payload).map_err(db_error)?.effective(kageha_browser_decisions::Purpose::Action);
                if configured.mode!=kageha_browser_decisions::Mode::Off {
                    let row=store::get(&connection,"model",&configured.model).map_err(db_error)?;
                    let Some(row) = row.filter(|row| kageha_models::decisions::is_typed_protocol(
                        row["protocol"].as_str().unwrap_or(""),
                    )) else {
                        return Err((400, "Browser actions require a typed decision model".into()));
                    };
                    if !endpoint_ready(&connection, &row)? {
                        return Err((400, "The browser decision model connection needs setup".into()));
                    }
                }
            }
            store::put(&connection, "setting", key, payload).map_err(db_error)?
        } else {
            setting(&connection, key, default)?
        };
        let rows = store::list(&connection, "model").map_err(db_error)?;
        return Ok(
            json!({"policy":policy,"browser_action_inheritance":decisions,"purposes":["model_routing","browser_guidance","recovery","browser_review","browser_action"],"providers":rows.iter().map(|v|v["provider"].clone()).collect::<Vec<_>>(),"models":crate::ui_browser_decisions::decision_inventory(&connection, &rows, decisions),"codex":{"supported":false,"reason":"Use a configured HTTP endpoint"},"message":"Settings saved. Applies to new runs."}),
        );
    }
    // Settings for optional adapters are persisted independently of whether that
    // adapter is installed. Never claim a remote integration has connected.
    let parts: Vec<_> = path.trim_start_matches("/api/").split('/').collect();
    let collection = parts[0];
    let key = match collection {
        "mcp" => "servers",
        "channels" => "channels",
        "remote" => "hosts",
        "voice-profiles" => "profiles",
        "extensions" => parts.get(1).copied().unwrap_or("plugins"),
        "integrations" => "integrations",
        _ => "settings",
    };
    if path.ends_with("/test")
        || path.ends_with("/check")
        || path.ends_with("/qr")
        || path.ends_with("/calls")
        || path.ends_with("/transcript")
        || path.ends_with("/connect")
    {
        return Err((409,"This optional adapter is not installed in the native runtime. Its configuration can be managed without Python.".into()));
    }
    let connection = db.lock().map_err(db_error)?;
    if path == "/api/audio8-settings" {
        let value = if method == &Method::Post {
            if payload["enabled"] == true {
                return Err((409, "No native Audio8 adapter is installed".into()));
            }
            store::put(&connection, "setting", "audio8", payload).map_err(db_error)?
        } else {
            setting(
                &connection,
                "audio8",
                json!({"enabled":false,"device":"cpu"}),
            )?
        };
        return Ok(
            json!({"enabled":value["enabled"],"device":value["device"],"installed":false,"model":"Audio8"}),
        );
    }
    let namespace = format!("config:{collection}:{key}");
    if method == &Method::Get {
        let rows = store::list(&connection, &namespace).map_err(db_error)?;
        if parts.len() > 1 && !matches!(collection, "extensions" | "remote") {
            return Ok(rows.into_iter().find(|v|v["id"]==parts[1]||v["name"]==parts[1]).unwrap_or(json!({"id":parts[1],"enabled":false,"connected":false,"configured":false,"runtime_available":false})));
        }
        return Ok(
            json!({key:rows,"enabled":true,"runtime_available":false,"default_host_id":setting(&connection,"remote-default",json!({}))?["id"]}),
        );
    }
    if path == "/api/remote/default" {
        let id = payload["host_id"].as_str().unwrap_or("");
        store::put(&connection, "setting", "remote-default", json!({"id":id})).map_err(db_error)?;
        return Ok(json!({"ok":true,"default_host_id":id}));
    }
    let id = parts
        .get(2)
        .filter(|_| collection == "remote")
        .or_else(|| {
            parts
                .get(1)
                .filter(|_| collection != "extensions" && collection != "remote")
        })
        .copied()
        .filter(|s| !s.is_empty())
        .or_else(|| payload["name"].as_str())
        .or_else(|| payload["id"].as_str())
        .unwrap_or("default");
    if !server::identifier(id) {
        return Err((400, "Invalid settings identity".into()));
    }
    if method == &Method::Delete {
        connection
            .execute(
                "DELETE FROM ui_documents WHERE kind=? AND id=?",
                params![namespace, id],
            )
            .map_err(db_error)?;
    } else if matches!(method, Method::Post | Method::Put | Method::Patch) {
        if payload["enabled"] == true {
            return Err((
                409,
                "Install the native adapter before enabling this integration".into(),
            ));
        }
        let mut value = store::get(&connection, &namespace, id)
            .map_err(db_error)?
            .unwrap_or(json!({"id":id,"name":id}));
        for (k, v) in payload
            .as_object()
            .ok_or((400, "Expected settings object".into()))?
        {
            if {
                let field = k.to_ascii_lowercase();
                field.contains("token")
                    || field.contains("secret")
                    || field.contains("password")
                    || matches!(
                        field.as_str(),
                        "api_key"
                            | "private_key"
                            | "credentials"
                            | "headers"
                            | "env"
                            | "authorization"
                    )
            } {
                save_secret(
                    &connection,
                    &format!("{namespace}:{id}:{k}"),
                    &v.to_string(),
                )?;
            } else {
                value[k] = v.clone();
            }
        }
        value["connected"] = json!(false);
        value["runtime_available"] = json!(false);
        value["editable"] = json!(true);
        value["origin"] = json!("standalone");
        store::put(&connection, &namespace, id, value).map_err(db_error)?;
    } else {
        return Err((405, "Unsupported settings operation".into()));
    }
    let rows = store::list(&connection, &namespace).map_err(db_error)?;
    Ok(json!({"ok":true,key:rows,"message":"Configuration saved; adapter is not installed."}))
}
pub(crate) fn handle(mut request: Request, db: &Database) -> std::io::Result<()> {
    let path = request.url().split('?').next().unwrap_or("").to_owned();
    let method = request.method().clone();
    let input = if matches!(method, Method::Post | Method::Put | Method::Patch) {
        webui::body(&mut request).map_err(|_| (400, "Invalid JSON".into()))
    } else {
        Ok(json!({}))
    };
    match input.and_then(|p| operation(db, &path, &method, p)) {
        Ok(v) => server::response(request, 200, v),
        Err((code, error)) => server::response(request, code, json!({"error":error})),
    }
    Ok(())
}

pub(crate) fn candidates(db: &Database, requested: &str, role: &str) -> ApiResult<Vec<Provider>> {
    let first = configured(db, requested, role)?;
    let rows = models(db)?;
    let connection = db.lock().map_err(db_error)?;
    let policy = setting(
        &connection,
        "fallback",
        json!({"enabled":true,"providers":{},"models":{}}),
    )?;
    drop(connection);
    let selected = selected_id(&*db.lock().map_err(db_error)?, requested, role)?;
    let primary = rows.iter().find(|v| v["id"] == selected);
    let provider = primary
        .and_then(|v| v["provider"].as_str())
        .unwrap_or("environment");
    let identity = primary.and_then(|v| v["id"].as_str()).unwrap_or(requested);
    let outgoing = policy["models"][identity]["allow_fallback"]
        .as_bool()
        .or_else(|| policy["providers"][provider]["allow_fallback"].as_bool())
        .unwrap_or(true);
    let mut candidates = vec![first];
    if policy["enabled"] == false || !outgoing || candidates[0].protocol == "codex_app_server" {
        return Ok(candidates);
    }
    let mut eligible: Vec<_> = rows
        .iter()
        .filter(|v| v["id"] != identity)
        .filter(|v| v["ready"] == true && v["protocol"] != "codex_app_server")
        .filter_map(|v| {
            let mode = policy["models"][v["id"].as_str().unwrap_or("")]["mode"]
                .as_str()
                .or_else(|| {
                    policy["providers"][v["provider"].as_str().unwrap_or("")]["mode"].as_str()
                })
                .unwrap_or("normal");
            if mode == "never" {
                None
            } else {
                Some((mode == "last", v["id"].as_str().unwrap_or("").to_owned()))
            }
        })
        .collect();
    eligible.sort();
    for (_, id) in eligible.into_iter().take(7) {
        if let Ok(config) = configured(db, &id, role) {
            candidates.push(config)
        }
    }
    Ok(candidates)
}

pub(crate) fn advise(
    db: &Database,
    chat: &str,
    turn: &str,
    messages: &[Value],
    explicit: bool,
    cancel: &AtomicBool,
) -> Option<String> {
    let policy = {
        let connection = db.lock().ok()?;
        setting(&connection, "decisions", json!({"mode":"off"})).ok()?
    };
    let purpose = "model_routing";
    let override_ = &policy["overrides"][purpose];
    let mode = override_["mode"]
        .as_str()
        .or_else(|| policy["mode"].as_str())
        .unwrap_or("off");
    if mode == "off" || explicit {
        return None;
    }
    let model = override_["model"]
        .as_str()
        .or_else(|| policy["model"].as_str())
        .unwrap_or("");
    let config = configured(db, model, "default").ok()?;
    let catalog = models(db).ok()?;
    let ids: Vec<_> = catalog.iter().filter_map(|v| v["id"].as_str()).collect();
    let objective = messages
        .iter()
        .rev()
        .find(|v| v["role"] == "user")
        .map(|v| v["content"].clone())
        .unwrap_or(Value::Null);
    let prompt = vec![
        json!({"role":"system","content":"Choose an execution model from the eligible ids. Return only JSON with model_id and confidence (0 to 1). This advice cannot approve tools or verify task completion."}),
        json!({"role":"user","content":json!({"objective":objective,"eligible_models":ids}).to_string()}),
    ];
    let timeout = override_["timeout_s"]
        .as_f64()
        .or_else(|| policy["timeout_s"].as_f64())
        .unwrap_or(3.0);
    let reply = native_chat::complete_timed(&config, prompt, cancel, timeout).ok()?;
    let value: Value = serde_json::from_str(reply["message"]["content"].as_str()?).ok()?;
    let candidate = value["model_id"].as_str()?.to_owned();
    let confidence = value["confidence"].as_f64()?;
    let threshold = override_["threshold"]
        .as_f64()
        .or_else(|| policy["threshold"].as_f64())
        .unwrap_or(0.9);
    let applied = mode == "active"
        && confidence.is_finite()
        && confidence >= threshold
        && confidence <= 1.0
        && ids.contains(&candidate.as_str());
    let _ = webui::emit(
        db,
        chat,
        turn,
        "event",
        json!({"kind":"decision","payload":{"purpose":purpose,"mode":mode,"model_id":model,"candidate":candidate,"confidence":confidence,"applied":applied}}),
    );
    applied.then_some(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capabilities_identify_the_compiled_harness_guards() {
        let db=crate::agent_host::DatabaseState::open(rusqlite::Connection::open_in_memory().unwrap()).unwrap();
        let value=operation(&db,"/api/capabilities",&Method::Get,json!({})).unwrap();
        assert_eq!(value["harness"]["recovery_version"],kageha_runtime::recovery::RECOVERY_VERSION);
        assert!(value["harness"]["features"].as_array().unwrap().contains(&json!("click_delivery_is_not_progress")));
    }
    #[test]
    fn browser_vault_authorization_requires_an_explicit_post() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        let db = crate::agent_host::DatabaseState::open(connection).unwrap();
        let error = operation(&db, "/api/provider-setup/browser-credentials/authorize", &Method::Get, json!({})).unwrap_err();
        assert_eq!(error.0, 405);
        let error = operation(&db, "/api/provider-setup/browser-credentials", &Method::Post, json!({})).unwrap_err();
        assert_eq!(error.0, 405);
    }
    #[test]
    fn a_missing_explicit_local_credential_is_not_advertised_ready() {
        let db=rusqlite::Connection::open_in_memory().unwrap();store::initialize(&db).unwrap();
        let mut model=json!({"provider":"fixture","model":"local","protocol":"openai_compat","base_url":"http://127.0.0.1:9999/v1","api_key_env":"KAGEHA_TEST_UNCONFIGURED_CATALOG_CREDENTIAL"});
        assert!(!available(&db,&model).unwrap());model.as_object_mut().unwrap().remove("api_key_env");assert!(available(&db,&model).unwrap());
    }
    #[test]
    fn catalog_refresh_preserves_ui_overrides_and_model_metadata() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        store::initialize(&db).unwrap();
        let mut catalog = kageha_models::catalog::Catalog {
            models: vec![
                json!({"id":"fixture","provider":"local","model":"fixture","base_url":"http://127.0.0.1:1111/v1","protocol":"openai_compat","context_window":12345,"capabilities":["vision"]}),
            ],
            roles: json!({"default":["fixture"]}),
        };
        sync_catalog(&db, &catalog).unwrap();
        assert_eq!(selected_id(&db, "", "default").unwrap(), "fixture");
        catalog.models[0]["base_url"] = json!("http://127.0.0.1:2222/v1");
        sync_catalog(&db, &catalog).unwrap();
        assert_eq!(
            store::get(&db, "model", "fixture").unwrap().unwrap()["base_url"],
            catalog.models[0]["base_url"]
        );
        let mut edited = catalog.models[0].clone();
        edited["model"] = json!("user-override");
        store::put(&db, "model", "fixture", edited.clone()).unwrap();
        catalog.models[0]["model"] = json!("new-catalog-model");
        sync_catalog(&db, &catalog).unwrap();
        sync_catalog(&db, &catalog).unwrap();
        let mut saved = store::get(&db, "model", "fixture").unwrap().unwrap();
        saved.as_object_mut().unwrap().remove("revision");
        assert_eq!(saved, edited);
        store::put(&db, "setting", "roles", json!({"default":"user-chosen"})).unwrap();
        assert_eq!(selected_id(&db, "", "default").unwrap(), "user-chosen");
        assert!(!available(&db, &json!({"protocol":"gemini"})).unwrap());
        assert!(!available(
            &db,
            &json!({"protocol":"openai_compat","base_url":"https://api.test/${ACCOUNT}"})
        )
        .unwrap());
    }
    #[test]
    fn environment_endpoint_refreshes_after_restart_without_overwriting_user_provider() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        store::initialize(&db).unwrap();
        sync_environment_model(&db, "test-model", "http://127.0.0.1:1111/v1").unwrap();
        sync_environment_model(&db, "test-model", "http://127.0.0.1:2222/v1").unwrap();
        assert_eq!(
            store::get(&db, "model", "test-model").unwrap().unwrap()["base_url"],
            "http://127.0.0.1:2222/v1"
        );
        store::put(
            &db,
            "model",
            "test-model",
            json!({"id":"test-model","provider":"user","base_url":"https://user.example/v1"}),
        )
        .unwrap();
        sync_environment_model(&db, "test-model", "http://127.0.0.1:3333/v1").unwrap();
        assert_eq!(
            store::get(&db, "model", "test-model").unwrap().unwrap()["base_url"],
            "https://user.example/v1"
        );
    }
    #[test]
    fn provider_credentials_never_use_cleartext_or_embedded_auth_urls() {
        assert!(valid_url("http://example.test/v1", "secret").is_err());
        assert!(valid_url("https://user:secret@example.test/v1", "").is_err());
        assert!(valid_url("https://example.test/\nheader=x", "").is_err());
        assert_eq!(
            valid_url("http://127.0.0.1:1234/v1/", "").unwrap(),
            "http://127.0.0.1:1234/v1"
        );
    }
}

fn validate_policy(value: &Value, decisions: bool) -> ApiResult<()> {
    if decisions {
        crate::ui_browser_decisions::decode_policy(value).map_err(|_|(400,"Invalid browser decision policy configuration".into()))?.validate().map_err(|_|(400,"Invalid browser decision budgets or limits".into()))?;
        if !value["fallback_models"]
            .as_array()
            .is_some_and(|a| a.len() <= 8 && a.iter().all(Value::is_string))
            || !value["overrides"].is_object()
        {
            return Err((400, "Invalid decision fallback or overrides".into()));
        }
        for (_, item) in value["overrides"].as_object().unwrap() {
            if item["threshold"]
                .as_f64()
                .is_some_and(|v| !(0.5..=1.0).contains(&v))
                || item["timeout_s"]
                    .as_f64()
                    .is_some_and(|v| !(0.1..=15.0).contains(&v))
                || item
                    .get("mode")
                    .is_some_and(|v| !matches!(v.as_str(), Some("off" | "shadow" | "active")))
            {
                return Err((400, "Invalid decision override".into()));
            }
        }
    } else {
        for key in ["models", "providers"] {
            for (_, rule) in value[key].as_object().unwrap() {
                if !rule.is_object()
                    || rule
                        .get("mode")
                        .is_some_and(|v| !matches!(v.as_str(), Some("normal" | "last" | "never")))
                    || rule.get("allow_fallback").is_some_and(|v| !v.is_boolean())
                {
                    return Err((400, "Invalid fallback rule".into()));
                }
            }
        }
    }
    Ok(())
}
