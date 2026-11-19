//! WebUI configuration compatibility for the independent MCP adapter.
use crate::{
    ui_store::{self as store, db_error, ApiResult},
    webui::Database,
};
use kageha_contracts::execution::{Control, ToolContext};
use rusqlite::Connection;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::{atomic::AtomicBool, Arc, Mutex, OnceLock},
};
const KIND: &str = "config:mcp:servers";
static POOLS: OnceLock<Mutex<HashMap<PathBuf, Arc<kageha_mcp::Pool>>>> = OnceLock::new();
fn pool_owned(db: &Connection) -> io::Result<Arc<kageha_mcp::Pool>> {
    let path = if let Some(path) = db.path().filter(|p| !p.is_empty()) {
        Path::new(path)
            .parent()
            .unwrap_or(Path::new("."))
            .join("components/mcp.sqlite")
    } else {
        let identity: String = db
            .query_row(
                "SELECT source FROM ui_policy_identity WHERE id=1",
                [],
                |r| r.get(0),
            )
            .map_err(io::Error::other)?;
        std::env::temp_dir().join(format!(
            "kageha-mcp-{}-{identity}/mcp.sqlite",
            std::process::id()
        ))
    };
    let mut pools = POOLS
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| io::Error::other("MCP cache unavailable"))?;
    if let Some(pool) = pools.get(&path) {
        return Ok(pool.clone());
    }
    let pool = Arc::new(kageha_mcp::Pool::open(&path)?);
    pools.insert(path, pool.clone());
    Ok(pool)
}
fn pool(db: &Database) -> io::Result<Arc<kageha_mcp::Pool>> {
    {
        let db = db
            .lock()
            .map_err(|_| io::Error::other("Database unavailable"))?;
        pool_owned(&db)
    }
}
pub(crate) fn release_session(db: &Connection, session: &str) -> io::Result<()> {
    pool_owned(db)?.release(session)
}
pub(crate) fn erase_session(db: &Connection, session: &str) -> io::Result<()> {
    pool_owned(db)?.erase(session)
}
pub(crate) fn receipts(db: &Database, session: &str) -> io::Result<Value> {
    Ok(json!(pool(db)?.receipts(session)?))
}
fn runtime_config(db:&Database, value:&Value) -> ApiResult<kageha_mcp::Config> {
    let config=kageha_mcp::Config::from_legacy(value).map_err(|_|(400,"Invalid MCP configuration".into()))?;
    if matches!(value["oauth"]["client_auth_method"].as_str(),Some("tls_client_auth"|"self_signed_tls_client_auth")) {
        let name=value["name"].as_str().ok_or((400,"MCP server identity is unavailable".into()))?;
        let pem=crate::ui_mcp_mtls::identity(db,name,value)?;
        return config.with_mutual_tls_identity(pem.as_str()).map_err(|_|(400,"Invalid mutual TLS identity".into()));
    }
    Ok(config)
}
pub(crate) fn invoke(
    db: &Database,
    session: &str,
    turn: &str,
    id: &str,
    config: &Value,
    tool: &str,
    args: Value,
    cancel: &AtomicBool,
) -> io::Result<Value> {
    let config = runtime_config(db,config).map_err(|(_,message)|io::Error::other(message))?;
    pool(db)?.exchange(
        &config,
        &ToolContext {
            session: session.into(),
            turn: turn.into(),
        },
        "tools/call",
        json!({"name":tool,"arguments":args}),
        Some(id),
        &Control {
            cancelled: cancel,
            deadline: None,
        },
    )
}

/// Resolve only an operator-configured protected mailbox adapter. The server's
/// advertised read-only annotation alone is deliberately not sufficient.
pub(crate) fn verification_mail_config(db: &Database, server: &str) -> io::Result<(Value, String)> {
    let row = catalog(db).map_err(|(_, e)| io::Error::other(e))?.into_iter()
        .find(|v| v["name"] == server && v["enabled"] == true && v["connected"] != false)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Protected mailbox is unavailable"))?;
    let tool = row["verification_mail_tool"].as_str().filter(|tool| read_only(&row, tool))
        .filter(|tool| row["tools"].as_array().is_some_and(|tools| tools.iter().any(|v| v["name"] == *tool)))
        .ok_or_else(|| io::Error::new(io::ErrorKind::PermissionDenied, "Protected mailbox adapter is not configured"))?;
    resolve(db, &tool_id(server, tool)).map_err(|(_, e)| io::Error::other(e))
}

pub(crate) fn verification_mail_read(db: &Database, session: &str, turn: &str, config: &Value,
    args: Value, cancel: &AtomicBool) -> io::Result<Value> {
    let config = runtime_config(db,config).map_err(|(_,message)|io::Error::other(message))?;
    pool(db)?.verification_mail_read(&config, &ToolContext { session: session.into(), turn: turn.into() },
        args, &Control { cancelled: cancel, deadline: None })
}
fn discover(db: &Database, config: &Value) -> ApiResult<Value> {
    let oauth = !config["oauth"].is_null();
    let config = runtime_config(db,config)?;
    let context = ToolContext {
        session: format!("catalog:{}", config.id),
        turn: "discovery".into(),
    };
    let pool = pool(db).map_err(db_error)?;
    let result = pool.exchange(
        &config,
        &context,
        "tools/list",
        json!({}),
        None,
        &Control {
            cancelled: &AtomicBool::new(false),
            deadline: None,
        },
    );
    let _ = pool.release(&context.session);
    result.map_err(|e| (502, if oauth { "MCP connection could not be verified".into() } else { e.to_string() }))
}
pub(crate) fn catalog(db: &Database) -> ApiResult<Vec<Value>> {
    let connection = db.lock().map_err(db_error)?;
    let mut rows = store::list(&connection, KIND).map_err(db_error)?;
    for row in &mut rows {
        let name = row["name"].as_str().unwrap_or("").to_owned();
        let ready = ["env", "headers"].iter().all(|field| {
            crate::ui_credentials::usable_or_anonymous(&connection, &format!("mcp:{name}:{field}"))
        });
        if !ready {
            row["connected"] = json!(false);
            row["connection_error"] = json!("Credentials are unavailable");
        }
        if !row["oauth"].is_null() {
            let grant = format!("mcp-{name}");
            let valid = crate::ui_mcp_oauth::trust_valid(row) && row["oauth_binding"]["resource"] == row["url"]
                && row["oauth_binding"]["client_id"] == row["oauth"]["client_id"]
                && row["oauth_binding"]["oauth"] == row["oauth"]
                && row["oauth_binding"]["authorization_id"].as_str().is_some_and(|value|
                    crate::ui_credentials::oauth_authority(&connection, &grant).ok().flatten().as_deref() == Some(value))
                && (!matches!(row["oauth"]["client_auth_method"].as_str(),Some("tls_client_auth"|"self_signed_tls_client_auth"))
                    || crate::ui_mcp_mtls::identity_available(&connection,&name,row))
                && crate::ui_credentials::oauth_ready(&connection, &grant).unwrap_or(false);
            if !valid {
                row["connected"] = json!(false);
                row["connection_error"] = json!("MCP authorization needs connection or recovery");
            }
        }
    }
    Ok(rows)
}
pub(crate) fn save(db: &Database, payload: &Value, test: bool) -> ApiResult<Value> {
    if !payload["identity_pem"].is_null() || !payload["mutual_tls_identity"].is_null()
        || !payload["oauth"]["identity_pem"].is_null() || !payload["oauth"]["mutual_tls_identity"].is_null() {
        return Err((400,"Provision mutual TLS identities through the dedicated credential endpoint".into()));
    }
    let name = store::text(payload, "name", 64)?.to_owned();
    let expected = match payload.get("expected_revision") {
        None => None,
        Some(value) => Some(value.as_u64().ok_or((400,"Expected revision must be a nonnegative integer".into()))?),
    };
    let mut config = {
        let connection = db.lock().map_err(db_error)?;
        store::get(&connection, KIND, &name)
            .map_err(db_error)?
            .unwrap_or(json!({}))
    };
    let plugin_owner = (config["origin"] == "plugin").then(|| config["plugin"].as_str().map(str::to_owned)).flatten();
    if expected.is_some_and(|revision| config["revision"].as_u64()!=Some(revision)) {
        return Err((409,"MCP configuration changed; review it again".into()));
    }
    for (key, value) in payload
        .as_object()
        .ok_or((400, "Expected MCP config".into()))?
    {
        if key == "expected_revision" {continue;}
        config[key] = value.clone();
    }
    for field in ["env", "headers"] {
        if config[field].is_null() {
            let connection = db.lock().map_err(db_error)?;
            let credential_name = format!("mcp:{name}:{field}");
            let saved = crate::ui_settings::secret(&connection, &credential_name)?;
            if saved.is_empty() && crate::ui_credentials::managed(&connection, &credential_name)? {
                return Err((403, "MCP credential is revoked or unavailable".into()));
            }
            if !saved.is_empty() {
                config[field] = serde_json::from_str(&saved).map_err(db_error)?;
            }
        }
    }
    if !crate::server::identifier(&name) {
        return Err((400, "Invalid MCP server name".into()));
    }
    if config.get("enabled").is_none() {
        config["enabled"] = json!(true)
    }
    let tools = if config["enabled"] == true || test {
        let mut authenticated = config.clone();
        crate::ui_mcp_oauth::authorize(db, &name, &mut authenticated)?;
        if let Some(revision)=expected {
            let connection=db.lock().map_err(db_error)?;
            if store::get(&connection,KIND,&name).map_err(db_error)?.and_then(|row|row["revision"].as_u64())!=Some(revision) {
                return Err((409,"MCP configuration changed before connection".into()));
            }
        }
        discover(db, &authenticated)?["tools"]
            .as_array()
            .cloned()
            .ok_or((502, "MCP tool list missing".into()))?
    } else {
        Vec::new()
    };
    if tools.len() > 128 {
        return Err((400, "MCP server exposes too many tools".into()));
    }
    let mut names = std::collections::BTreeSet::new();
    for tool in &tools {
        let name = tool["name"]
            .as_str()
            .filter(|n| !n.is_empty() && n.len() <= 256 && !n.chars().any(char::is_control))
            .ok_or((502, "Invalid MCP tool name".into()))?;
        if !names.insert(name) || !tool["inputSchema"].is_object() {
            return Err((502, "Duplicate MCP tool or invalid input schema".into()));
        }
    }
    config["id"] = json!(name);
    config["tools"] = json!(tools);
    // Preserve existing plugin ownership; submitted configuration cannot forge it.
    if let Some(owner) = plugin_owner {
        config["origin"] = json!("plugin"); config["plugin"] = json!(owner);
    } else {
        config["origin"] = json!("standalone"); config["plugin"] = Value::Null;
    }
    config["editable"] = json!(true);
    config["runtime_available"] = json!(true);
    config["connected"] = json!(config["enabled"] == true || test);
    if !test {
        let mut connection = db.lock().map_err(db_error)?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(db_error)?;
        if let Some(revision)=expected {
            if store::get(&tx,KIND,&name).map_err(db_error)?.and_then(|row|row["revision"].as_u64())!=Some(revision) {
                return Err((409,"MCP configuration changed during connection".into()));
            }
        }
        for field in ["env", "headers"] {
            if config[field].is_object() {
                crate::ui_settings::save_secret(
                    &tx,
                    &format!("mcp:{name}:{field}"),
                    &config[field].to_string(),
                )?;
                config[field] = Value::Null;
            }
        }
        store::put(&tx, KIND, &name, config.clone()).map_err(db_error)?;
        tx.commit().map_err(db_error)?;
    }
    Ok(
        json!({"ok":true,"tools":config["tools"],"servers":catalog(db)?,"message":"Native MCP connection verified"}),
    )
}
/// Activate only the precise configuration that was inspected before a handoff.
pub(crate) fn activate(db:&Database,name:&str,expected_revision:u64)->ApiResult<Value> {
    let current={let connection=db.lock().map_err(db_error)?;store::get(&connection,KIND,name).map_err(db_error)?.ok_or((404,"MCP server not found".into()))?};
    if !matches!(current["transport"].as_str(),Some("http"|"streamable_http")) || current["oauth"].is_null() {
        return Err((403,"Automatic activation requires an OAuth HTTP MCP server".into()));
    }
    save(db,&json!({"name":name,"enabled":true,"expected_revision":expected_revision}),false)
}
/// The model-facing definitions of every usable MCP tool, for discovery and for
/// resolving `mcp_call` targets. They are not added to the tool list: the model reaches
/// them through `mcp_call` (design section 4). Each carries Host-only fields that must
/// not be sent to the model: `discovery_alias` (`server::tool`), `mcp_server` and
/// `mcp_tool` (the `mcp_call` arguments), and `mcp_read_only` (the operator tagged the
/// tool read-only in the server's `read_only_tools`).
pub(crate) fn tool_specs(db: &Database) -> Vec<Value> {
    let mut specs = Vec::new();
    for server in catalog(db)
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s["enabled"] == true && s["connected"] != false)
    {
        let name = server["name"].as_str().unwrap_or("");
        if let Ok(tools) = kageha_mcp::specs(
            name,
            server["tools"].as_array().map(Vec::as_slice).unwrap_or(&[]),
        ) {
            for (mut spec, raw) in tools.into_iter().zip(server["tools"].as_array().into_iter().flatten()) {
                let tool = raw["name"].as_str().unwrap_or("");
                spec["discovery_alias"] = json!(format!("{name}::{tool}"));
                spec["mcp_server"] = json!(name);
                spec["mcp_tool"] = json!(tool);
                spec["mcp_read_only"] = json!(read_only(&server, tool));
                specs.push(spec);
            }
        }
    }
    specs
}
/// Whether the server's configuration tags `tool` read-only. Only the operator's
/// `read_only_tools` list counts; server-provided annotations are not trusted.
pub(crate) fn read_only(server: &Value, tool: &str) -> bool {
    server["read_only_tools"].as_array().is_some_and(|tools| tools.iter().any(|name| name == tool))
}
use kageha_mcp::tool_id;
pub(crate) fn resolve(db: &Database, name: &str) -> ApiResult<(Value, String)> {
    for server in catalog(db)?.into_iter().filter(|v| v["enabled"] == true) {
        for tool in server["tools"].as_array().into_iter().flatten() {
            let original = tool["name"].as_str().unwrap_or("");
            if tool_id(server["name"].as_str().unwrap_or(""), original) == name {
                let mut config = server.clone();
                let connection = db.lock().map_err(db_error)?;
                for field in ["env", "headers"] {
                    let credential_name =
                        format!("mcp:{}:{field}", server["name"].as_str().unwrap_or(""));
                    let saved = crate::ui_settings::secret(&connection, &credential_name)?;
                    if saved.is_empty()
                        && crate::ui_credentials::managed(&connection, &credential_name)?
                    {
                        return Err((403, "MCP credential is revoked or unavailable".into()));
                    }
                    if !saved.is_empty() {
                        config[field] = serde_json::from_str(&saved).map_err(db_error)?;
                    }
                }
                drop(connection);
                crate::ui_mcp_oauth::authorize(db, server["name"].as_str().unwrap_or(""), &mut config)?;
                return Ok((config, original.into()));
            }
        }
    }
    Err((404, "MCP tool is no longer available".into()))
}
#[cfg(test)]
mod credential_tests {
    use super::*;
    #[test]
    fn inline_mutual_tls_key_material_is_never_saved_as_config() {
        let connection=rusqlite::Connection::open_in_memory().unwrap();
        store::initialize(&connection).unwrap();crate::ui_credentials::initialize(&connection).unwrap();
        let db=crate::agent_host::DatabaseState::open(connection).unwrap();
        for payload in [json!({"name":"fixture","identity_pem":"PRIVATE-CANARY"}),
            json!({"name":"fixture","oauth":{"mutual_tls_identity":"PRIVATE-CANARY"}})] {
            assert!(save(&db,&payload,false).is_err());
        }
        let connection=db.lock().unwrap();
        assert!(store::get(&connection,KIND,"fixture").unwrap().is_none());
    }
    #[test]
    fn editing_plugin_mcp_preserves_real_owner_and_cannot_forge_one() {
        let connection=rusqlite::Connection::open_in_memory().unwrap();
        store::initialize(&connection).unwrap();crate::ui_credentials::initialize(&connection).unwrap();
        store::put(&connection,KIND,"plugin-example-server",json!({"name":"plugin-example-server","origin":"plugin",
            "plugin":"example","enabled":false,"transport":"http","url":"https://example.test/mcp","tools":[]})).unwrap();
        let db=crate::agent_host::DatabaseState::open(connection).unwrap();
        save(&db,&json!({"name":"plugin-example-server","enabled":false,"plugin":"attacker","origin":"standalone"}),false).unwrap();
        let item=catalog(&db).unwrap().into_iter().find(|item|item["name"]=="plugin-example-server").unwrap();
        assert_eq!(item["origin"],"plugin");assert_eq!(item["plugin"],"example");
        save(&db,&json!({"name":"unrelated","enabled":false,"plugin":"example","origin":"plugin","transport":"http","url":"https://example.test/mcp"}),false).unwrap();
        let item=catalog(&db).unwrap().into_iter().find(|item|item["name"]=="unrelated").unwrap();
        assert_eq!(item["origin"],"standalone");assert!(item["plugin"].is_null());
    }
    #[test]
    fn activation_rejects_stale_configuration_before_dispatch() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        store::initialize(&connection).unwrap();
        crate::ui_credentials::initialize(&connection).unwrap();
        store::put(&connection, KIND, "plugin-example-server", json!({"name":"plugin-example-server", "origin":"plugin",
            "plugin":"example", "enabled":false, "transport":"http", "url":"https://api.githubcopilot.com/mcp",
            "oauth":{"client_id":"client", "callback_url":"http://127.0.0.1:12799/callback"}})).unwrap();
        let revision = store::get(&connection, KIND, "plugin-example-server").unwrap().unwrap()["revision"].as_u64().unwrap();
        let db = crate::agent_host::DatabaseState::open(connection).unwrap();
        assert_eq!(activate(&db,"plugin-example-server",revision + 1).unwrap_err().0, 409);
        assert_eq!(catalog(&db).unwrap()[0]["enabled"], false);
    }
    #[test]
    fn revocation_blocks_mcp_resolution_before_process_dispatch() {
        let db = Connection::open_in_memory().unwrap();
        store::initialize(&db).unwrap();
        crate::ui_credentials::initialize(&db).unwrap();
        store::put(
            &db,
            KIND,
            "fixture",
            json!({"id":"fixture","name":"fixture","enabled":true,"tools":[{"name":"echo"}]}),
        )
        .unwrap();
        crate::ui_settings::save_secret(&db, "mcp:fixture:env", "{\"TOKEN\":\"CANARY\"}").unwrap();
        crate::ui_credentials::delete(&db, "mcp:fixture:env").unwrap();
        let db = crate::agent_host::DatabaseState::open(db).unwrap();
        assert_eq!(
            resolve(&db, &tool_id("fixture", "echo")).unwrap_err().0,
            403
        );
    }
}
