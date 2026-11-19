//! Map known official app references to independently configured native integrations.
//! A host-specific connector ID is not itself a credential or an access grant.
use serde_json::{json, Value};
use std::io;
use sha2::{Digest, Sha256};
use crate::webui::Database;

const BRIDGES: &str = "plugin-app-bridges";
fn err(message: &str) -> io::Error { io::Error::new(io::ErrorKind::InvalidInput, message) }
fn key(plugin: &str, app: &str, connector: &str) -> String {
    format!("{:x}",Sha256::digest(json!([plugin,app,connector]).to_string().as_bytes()))
}
fn fingerprint(config: &Value) -> String {
    let mut relevant=config.clone();
    if let Some(fields)=relevant.as_object_mut() {
        for field in ["revision","enabled","connected","tools","connection_error"] { fields.remove(field); }
    }
    format!("{:x}",Sha256::digest(relevant.to_string().as_bytes()))
}
fn identity<'a>(payload: &'a Value) -> io::Result<(&'a str,&'a str,&'a str)> {
    let plugin=payload["plugin"].as_str().filter(|s|crate::server::identifier(s)).ok_or_else(||err("Invalid plugin name"))?;
    let app=payload["app"].as_str().filter(|s| !s.is_empty() && s.len()<=80 && s.bytes().all(|b|b.is_ascii_alphanumeric() || matches!(b,b'-'|b'_'|b'.'))).ok_or_else(||err("Invalid app name"))?;
    let connector=payload["connector_id"].as_str().filter(|s|!s.is_empty() && s.len()<=160 && !s.chars().any(char::is_control)).ok_or_else(||err("Invalid connector identity"))?;
    Ok((plugin,app,connector))
}
fn installed_app<'a>(installed: &'a Value, app: &str, connector: &str) -> Option<&'a Value> {
    installed["apps"].as_array()?.iter().find(|item|item["name"]==app && item["connector_id"]==connector && item["integration"].is_null())
}
/// Configure a user-selected binding. No tools, credentials, or grants are created.
pub(crate) fn set_bridge(db: &Database, payload: &Value) -> io::Result<Value> {
    set_bridge_inner(db,payload,false)
}
/// Agent proposals may create an unbound app mapping, but never replace an operator's mapping.
pub(crate) fn set_new_bridge(db: &Database, payload: &Value) -> io::Result<Value> {
    set_bridge_inner(db,payload,true)
}
fn set_bridge_inner(db: &Database, payload: &Value, only_new: bool) -> io::Result<Value> {
    let (plugin,app,connector)=identity(payload)?;
    let server=payload["server"].as_str().filter(|s|crate::server::identifier(s)).ok_or_else(||err("Invalid MCP server name"))?;
    let expected=payload["expected_server_revision"].as_u64().ok_or_else(||err("Review the MCP server revision"))?;
    let package=payload["package_sha256"].as_str().filter(|s|!s.is_empty()).ok_or_else(||err("Review the installed plugin package"))?;
    let tools=payload["tools"].as_array().filter(|v|!v.is_empty() && v.len()<=64).ok_or_else(||err("Select at least one MCP tool"))?;
    let mut selected=std::collections::BTreeSet::new();
    for tool in tools {
        let name=tool.as_str().filter(|s|!s.is_empty() && s.len()<=256 && !s.chars().any(char::is_control)).ok_or_else(||err("Invalid MCP tool name"))?;
        if !selected.insert(name.to_owned()) {return Err(err("Duplicate MCP tool"));}
    }
    let mut connection=db.lock().map_err(|_|io::Error::other("Plugin state unavailable"))?;
    let tx=connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(io::Error::other)?;
    if only_new && crate::ui_store::get(&tx,BRIDGES,&key(plugin,app,connector)).map_err(io::Error::other)?.is_some() {
        return Err(err("An app bridge already exists; review or change it in Settings"));
    }
    let installed=crate::ui_store::get(&tx,"plugin-installed",plugin).map_err(io::Error::other)?.filter(|v|v["name"]==plugin && v["status"]=="installed" && v["package_sha256"]==package)
        .ok_or_else(||err("Installed plugin changed or is unavailable"))?;
    let app_row=installed_app(&installed,app,connector).ok_or_else(||err("Installed plugin app identity does not match"))?;
    let config=crate::ui_store::get(&tx,"config:mcp:servers",server).map_err(io::Error::other)?.filter(|v|v["name"]==server && v["revision"].as_u64()==Some(expected))
        .ok_or_else(||err("MCP server changed or is unavailable"))?;
    if selected.iter().any(|name|!config["tools"].as_array().is_some_and(|tools|tools.iter().any(|v|v["name"]==*name))) {
        return Err(err("Selected tools are not in the reviewed server inventory"));
    }
    let record=json!({"plugin":plugin,"app":app,"connector_id":connector,"server":server,"tools":selected.into_iter().collect::<Vec<_>>(),
        "package_sha256":package,"server_revision":expected,"config_sha256":fingerprint(&config)});
    crate::ui_store::put(&tx,BRIDGES,&key(plugin,app,connector),record).map_err(io::Error::other)?;
    let result=bridge_status(&tx,&installed,app_row)?;
    tx.commit().map_err(io::Error::other)?;
    Ok(result)
}
pub(crate) fn remove_bridge(db: &Database, payload: &Value) -> io::Result<Value> {
    let (plugin,app,connector)=identity(payload)?;
    let connection=db.lock().map_err(|_|io::Error::other("Plugin state unavailable"))?;
    connection.execute("DELETE FROM ui_documents WHERE kind=? AND id=?",rusqlite::params![BRIDGES,key(plugin,app,connector)]).map_err(io::Error::other)?;
    Ok(json!({"ok":true}))
}
/// Saved status only. Callers must additionally apply live MCP and agent policy gates.
pub(crate) fn bridge_status(connection: &rusqlite::Connection, installed: &Value, app: &Value) -> io::Result<Value> {
    let plugin=installed["name"].as_str().ok_or_else(||err("Invalid installed plugin"))?;
    let name=app["name"].as_str().ok_or_else(||err("Invalid installed app"))?;
    let connector=app["connector_id"].as_str().ok_or_else(||err("Invalid installed app identity"))?;
    let row=crate::ui_store::get(connection,BRIDGES,&key(plugin,name,connector)).map_err(io::Error::other)?;
    let Some(row)=row else {return Ok(json!({"status":"needs_setup"}));};
    let config=crate::ui_store::get(connection,"config:mcp:servers",row["server"].as_str().unwrap_or("")).map_err(io::Error::other)?;
    let stale=installed["status"]!="installed" || installed["package_sha256"]!=row["package_sha256"]
        || installed_app(installed,name,connector).is_none() || row["plugin"]!=plugin || row["app"]!=name || row["connector_id"]!=connector
        || config.as_ref().is_none_or(|v|row["server_revision"]!=v["revision"] || row["config_sha256"]!=fingerprint(v));
    let active=config.as_ref().is_some_and(|v|v["enabled"]==true && v["connected"]==true
        && row["tools"].as_array().is_some_and(|chosen|chosen.iter().all(|name|v["tools"].as_array().is_some_and(|tools|tools.iter().any(|tool|tool["name"]==*name)))));
    Ok(json!({"plugin":plugin,"app":name,"connector_id":connector,"server":row["server"],"tools":row["tools"],
        "status":if stale {"stale"} else if active {"configured"} else {"needs_setup"}}))
}

/// Only map exact, reviewed plugin/app/connector triples. Unknown references must
/// remain visible as unsupported; the native integration has narrower read-only
/// capabilities than the corresponding host app.
pub(crate) fn parse(plugin: &str, document: &Value) -> io::Result<(Vec<Value>, Vec<String>)> {
    let apps = document["apps"].as_object().filter(|apps| apps.len() <= 32)
        .ok_or_else(|| io::Error::other("Invalid plugin app manifest"))?;
    let mut supported = Vec::new();
    let mut unsupported = Vec::new();
    for (name, descriptor) in apps {
        if name.is_empty() || name.len()>80 || !name.bytes().all(|b|b.is_ascii_alphanumeric() || matches!(b,b'-'|b'_'|b'.')) {
            return Err(io::Error::other("Invalid plugin app name"));
        }
        let id = descriptor["id"].as_str().filter(|id| !id.is_empty() && id.len() <= 160)
            .ok_or_else(|| io::Error::other("Invalid plugin app identity"))?;
        if id.chars().any(char::is_control) { return Err(io::Error::other("Invalid plugin app identity")); }
        let integration = match (plugin, name.as_str(), id) {
            ("gmail", "gmail", "connector_2128aebfecb84f64a069897515042a44") => Some("gmail"),
            ("github", "github", "connector_76869538009648d5b282a4bb21c3d157") => Some("github"),
            ("google-calendar", "google-calendar", "connector_947e0d954944416db111db556030eea6") => Some("google-calendar"),
            ("google-drive", "google-drive", "connector_5f3c8c41a1e54ad7a76272c89e2554fa") => Some("google-drive"),
            _ => None,
        };
        if let Some(integration) = integration {
            supported.push(json!({"name":name,"connector_id":id,"integration":integration,
                "capabilities":if integration == "google-drive" {json!(["metadata_read"])} else {json!(["read"])},"setup_required":true}));
        } else {
            let optional = descriptor["required"] == false;
            unsupported.push(format!("{} app {name} has no native adapter; an operator can configure a compatible MCP bridge", if optional {"Optional"} else {"Required"}));
            supported.push(json!({"name":name,"connector_id":id,"integration":null,
                "capabilities":[],"setup_required":true,"status":"needs_setup"}));
        }
    }
    Ok((supported, unsupported))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_known_official_app_identities_map_to_native_read_only_integrations() {
        let app = json!({"apps":{"gmail":{"id":"connector_2128aebfecb84f64a069897515042a44"}}});
        let (mapped, unsupported) = parse("gmail", &app).unwrap();
        assert_eq!(mapped[0]["integration"], "gmail");
        assert_eq!(mapped[0]["capabilities"], json!(["read"]));
        assert!(unsupported.is_empty());
        assert!(parse("another-plugin", &app).unwrap().0[0]["integration"].is_null());
        let changed = json!({"apps":{"gmail":{"id":"connector_modified"}}});
        assert!(parse("gmail", &changed).unwrap().0[0]["integration"].is_null());
        assert!(parse("gmail", &json!({"apps":[]})).is_err());
        assert!(parse("gmail", &json!({"apps":{"bad\nname":{"id":"x"}}})).is_err());
        let drive = json!({"apps":{"google-drive":{"id":"connector_5f3c8c41a1e54ad7a76272c89e2554fa"}}});
        let (mapped, unsupported) = parse("google-drive", &drive).unwrap();
        assert_eq!(mapped[0]["integration"], "google-drive");
        assert_eq!(mapped[0]["capabilities"], json!(["metadata_read"]));
        assert!(unsupported.is_empty());
        assert!(parse("another-plugin", &drive).unwrap().0[0]["integration"].is_null());
    }
    #[test]
    fn bridge_requires_reviewed_identity_and_server_and_goes_stale_on_change() {
        let connection=rusqlite::Connection::open_in_memory().unwrap();
        crate::ui_store::initialize(&connection).unwrap();
        let manifest=json!({"apps":{"slack":{"id":"asdk_app_official"}}});
        let apps=parse("slack",&manifest).unwrap().0;
        let plugin=crate::ui_store::put(&connection,"plugin-installed","slack",json!({"name":"slack","status":"installed","package_sha256":"package","apps":apps})).unwrap();
        let server=crate::ui_store::put(&connection,"config:mcp:servers","workspace",json!({"name":"workspace","transport":"http","url":"https://example.test/mcp","enabled":true,"connected":true,"tools":[{"name":"search"},{"name":"send"}]})).unwrap();
        let db=crate::agent_host::DatabaseState::open(connection).unwrap();
        let mut request=json!({"plugin":"slack","app":"slack","connector_id":"asdk_app_official","package_sha256":"package","server":"workspace","expected_server_revision":server["revision"],"tools":["search"]});
        let result=set_bridge(&db,&request).unwrap();
        assert_eq!(result["status"],"configured");
        assert_eq!(result["tools"],json!(["search"]));
        let mut replacement=request.clone(); replacement["tools"]=json!(["send"]);
        assert!(set_new_bridge(&db,&replacement).is_err(), "an agent must not replace an existing operator mapping");
        let connection=db.lock().unwrap();
        assert_eq!(bridge_status(&connection,&plugin,&apps[0]).unwrap()["tools"],json!(["search"]));
        drop(connection);
        request["connector_id"]=json!("forged"); assert!(set_bridge(&db,&request).is_err()); request["connector_id"]=json!("asdk_app_official");
        request["plugin"]=json!("other"); assert!(set_bridge(&db,&request).is_err()); request["plugin"]=json!("slack");
        request["package_sha256"]=json!("old-package"); assert!(set_bridge(&db,&request).is_err()); request["package_sha256"]=json!("package");
        request["tools"]=json!(["private"]); assert!(set_bridge(&db,&request).is_err()); request["tools"]=json!(["search"]);
        request["expected_server_revision"]=json!(server["revision"].as_u64().unwrap()+1); assert!(set_bridge(&db,&request).is_err());
        let connection=db.lock().unwrap();
        let mut changed=server.clone();changed["url"]=json!("https://other.example/mcp");
        crate::ui_store::put(&connection,"config:mcp:servers","workspace",changed).unwrap();
        assert_eq!(bridge_status(&connection,&plugin,&apps[0]).unwrap()["status"],"stale");
        let mut changed_plugin=plugin.clone(); changed_plugin["package_sha256"]=json!("new-package");
        assert_eq!(bridge_status(&connection,&changed_plugin,&apps[0]).unwrap()["status"],"stale");
        connection.execute("DELETE FROM ui_documents WHERE kind='config:mcp:servers' AND id='workspace'",[]).unwrap();
        assert_eq!(bridge_status(&connection,&plugin,&apps[0]).unwrap()["status"],"stale");
        drop(connection);
        remove_bridge(&db,&request).unwrap();
        let connection=db.lock().unwrap();
        assert_eq!(bridge_status(&connection,&plugin,&apps[0]).unwrap()["status"],"needs_setup");
    }
}
