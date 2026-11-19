//! Explicit operator migration. Secret values cross only the authenticated local
//! settings API; catalog repair preserves IDs, history and user edits.
use crate::ui_store as store;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{collections::BTreeMap, io, path::Path, time::Duration};

fn failure(message: &str) -> io::Error {
    io::Error::other(message)
}
// This administrative bridge is intentionally restricted to an explicit IPv4
// loopback socket; it cannot follow redirects, proxies or DNS to another host.
fn local_import(endpoint: &str, token: &str, payload: &Value) -> io::Result<u16> {
    use std::io::{Read, Write};
    let port = endpoint
        .rsplit(':')
        .next()
        .and_then(|p| p.parse::<u16>().ok())
        .ok_or_else(|| failure("Invalid local API port"))?;
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let mut socket = std::net::TcpStream::connect_timeout(&address, Duration::from_secs(5))?;
    socket.set_read_timeout(Some(Duration::from_secs(30)))?;
    socket.set_write_timeout(Some(Duration::from_secs(30)))?;
    if token.len() < 32 || token.chars().any(char::is_control) {
        return Err(failure("Invalid protected API token"));
    }
    let body = zeroize::Zeroizing::new(
        serde_json::to_vec(payload).map_err(|_| failure("Credential import encoding failed"))?,
    );
    let header=zeroize::Zeroizing::new(format!("POST /api/provider-setup/key HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()));
    socket.write_all(header.as_bytes())?;
    socket.write_all(&body)?;
    let mut response = zeroize::Zeroizing::new(Vec::new());
    socket.take(65537).read_to_end(&mut response)?;
    if response.len() > 65536 {
        return Err(failure("Local API response exceeded its limit"));
    }
    std::str::from_utf8(&response)
        .ok()
        .and_then(|s| s.lines().next())
        .and_then(|s| s.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| failure("Invalid local API response"))
}
fn content(mut row: Value) -> Value {
    if let Some(v) = row.as_object_mut() {
        v.remove("revision");
    }
    row
}
fn incomplete(row: &Value) -> bool {
    row["base_url"]
        .as_str()
        .is_none_or(|url| url.is_empty() || url.contains('$'))
        || row["protocol"].as_str().is_none_or(str::is_empty)
}
fn public_update(
    current: Option<Value>,
    baseline: Option<Value>,
    canonical: &Value,
) -> Option<Value> {
    match current {
        None => Some(canonical.clone()),
        Some(mut row) => {
            if baseline.is_some_and(|old| content(old) == content(row.clone())) {
                return (content(row) != *canonical).then(|| canonical.clone());
            }
            if incomplete(&row) {
                for name in ["base_url", "protocol", "api_key_env"] {
                    if row[name].is_null()
                        || row[name] == ""
                        || name == "base_url"
                            && row[name].as_str().is_some_and(|url| url.contains('$'))
                    {
                        row[name] = canonical[name].clone();
                    }
                }
                return Some(row);
            }
            None
        }
    }
}
fn orphan_repair(row: &Value, catalog: &kageha_models::catalog::Catalog) -> Option<Value> {
    if !incomplete(row) || catalog.models.iter().any(|r| r["id"] == row["id"]) {
        return None;
    }
    let connection = catalog.models.iter().find(|r| {
        r["provider"] == row["provider"]
            && kageha_models::catalog::supported(r["protocol"].as_str().unwrap_or(""))
            && !incomplete(r)
    })?;
    let mut connection = connection.clone();
    if row["protocol"] == "clef" {
        let prefix = connection["base_url"].as_str()?.split_once("/ai/")?.0;
        connection["base_url"] = json!(format!("{prefix}/ai/run/{}", row["model"].as_str()?));
    }
    public_update(Some(row.clone()), None, &connection)
}
pub fn run(
    workspace: &Path,
    state: &Path,
    source: &Path,
    endpoint: &str,
    token_file: &Path,
    apply: bool,
    repair_only: bool,
) -> io::Result<Value> {
    let authority = endpoint
        .strip_prefix("http://127.0.0.1:")
        .or_else(|| endpoint.strip_prefix("http://localhost:"));
    if authority.is_none_or(|s| !s.parse::<u16>().is_ok_and(|p| p > 0)) {
        return Err(failure(
            "Migration requires an explicit local WebUI endpoint",
        ));
    }
    let environment = kageha_models::catalog::Environment::load(&[source.to_path_buf()])?;
    let lookup = |name: &str| environment.file_value(name).ok().flatten();
    let catalog = kageha_models::catalog::Catalog::load(
        &kageha_models::catalog::default_paths(workspace),
        lookup,
    )?;
    if catalog.models.is_empty() {
        return Err(failure(
            "No model catalog was found; select resources or an explicit catalog",
        ));
    }
    let mut db = Connection::open_with_flags(
        state.join("native-chats.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
    )
    .map_err(|_| failure("Existing application database unavailable"))?;
    db.busy_timeout(Duration::from_secs(5))
        .map_err(|_| failure("Database unavailable"))?;
    let mut sources = BTreeMap::new();
    for row in &catalog.models {
        if !kageha_models::catalog::supported(row["protocol"].as_str().unwrap_or("")) {
            continue;
        }
        if let (Some(provider), Some(name)) = (
            row["provider"].as_str(),
            row["api_key_env"].as_str().filter(|s| !s.is_empty()),
        ) {
            sources
                .entry(provider.to_owned())
                .or_insert(name.to_owned());
        }
    }
    for (provider, name) in [
        ("siliconflow", "SILICONFLOW_API_KEY"),
        ("opencode-go", "OPENCODE_API_KEY"),
        ("azure", "AZURE_OPENAI_API_KEY"),
    ] {
        if lookup(name).is_some_and(|v| !v.is_empty()) {
            sources.entry(provider.into()).or_insert(name.into());
        }
    }
    let mut keys = Vec::new();
    let mut planned = store::list(&db, "model")
        .map_err(|_| failure("Model inventory unavailable"))?
        .iter()
        .filter(|r| orphan_repair(r, &catalog).is_some())
        .count();
    for row in &catalog.models {
        let id = row["id"].as_str().unwrap();
        if public_update(
            store::get(&db, "model", id).map_err(|_| failure("Model inventory unavailable"))?,
            store::get(&db, "catalog-model", id)
                .map_err(|_| failure("Model baseline unavailable"))?,
            row,
        )
        .is_some()
        {
            planned += 1;
        }
    }
    let unsupported: Vec<_> = catalog
        .models
        .iter()
        .filter(|r| !kageha_models::catalog::supported(r["protocol"].as_str().unwrap_or("")))
        .map(|r| json!({"id":r["id"],"protocol":r["protocol"]}))
        .collect();
    if apply && !repair_only {
        db.execute_batch("CREATE TABLE IF NOT EXISTS ui_model_key_imports(provider TEXT PRIMARY KEY,source TEXT NOT NULL,state TEXT NOT NULL);").map_err(|_|failure("Migration journal unavailable"))?;
    }
    let token = if apply && !repair_only {
        use std::io::Read;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(token_file)
            .map_err(|_| failure("Protected API token unavailable"))?;
        let m = file.metadata()?;
        if !m.is_file()
            || m.mode() & 0o077 != 0
            || m.uid() != unsafe { libc::geteuid() }
            || m.len() > 1024
        {
            return Err(failure(
                "API token must be a bounded owner-only regular file",
            ));
        }
        let mut value = zeroize::Zeroizing::new(String::new());
        file.take(1025).read_to_string(&mut value)?;
        Some(value)
    } else {
        None
    };
    for (provider, name) in sources {
        let saved: Option<(u64, bool)> = db
            .query_row(
                "SELECT revision,revoked FROM ui_credential_refs WHERE name=?",
                [&provider],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|_| failure("Credential inventory unavailable"))?;
        let key = zeroize::Zeroizing::new(kageha_models::catalog::credential(&name, lookup));
        let status = if let Some((_, revoked)) = saved {
            if revoked {
                "revoked_preserved"
            } else {
                "existing_preserved"
            }
        } else if key.is_empty() {
            "source_missing"
        } else if !apply || repair_only {
            "would_import"
        } else {
            let previous: Option<String> = db
                .query_row(
                    "SELECT state FROM ui_model_key_imports WHERE provider=?",
                    [&provider],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|_| failure("Migration journal unavailable"))?;
            if previous.is_some_and(|s| s == "dispatching" || s == "uncertain") {
                return Err(failure("A previous key import has an unknown outcome; inspect its settings projection before retrying"));
            }
            let admitted=db.execute("INSERT INTO ui_model_key_imports VALUES(?,?,'dispatching') ON CONFLICT(provider) DO UPDATE SET state='dispatching' WHERE ui_model_key_imports.state='rejected'",params![provider,name]).map_err(|_|failure("Migration journal unavailable"))?;
            if admitted != 1 {
                return Err(failure(
                    "Another key import owns this provider; inspect its durable status",
                ));
            }
            let payload = crate::ui_credentials::SecretJson::new(
                json!({"provider":provider,"api_key":key.as_str()}),
            );
            let reply = local_import(endpoint, token.as_ref().unwrap().trim(), &payload);
            match reply {
                Ok(200) => {}
                Ok(400 | 401 | 403 | 404 | 422) => {
                    db.execute(
                        "UPDATE ui_model_key_imports SET state='rejected' WHERE provider=?",
                        [&provider],
                    )
                    .ok();
                    return Err(failure("The running credential vault rejected an import; unlock it before continuing"));
                }
                Ok(_) | Err(_) => {
                    db.execute(
                        "UPDATE ui_model_key_imports SET state='uncertain' WHERE provider=?",
                        [&provider],
                    )
                    .ok();
                    return Err(failure("Credential publication outcome is unknown; inspect the settings projection before retrying"));
                }
            }
            let verified:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM ui_credential_refs WHERE name=? AND revision>0 AND revoked=0)",[&provider],|r|r.get(0)).map_err(|_|failure("Credential verification unavailable"))?;
            if !verified {
                return Err(failure("Encrypted credential reference was not published"));
            }
            db.execute(
                "UPDATE ui_model_key_imports SET state='verified' WHERE provider=?",
                [&provider],
            )
            .map_err(|_| failure("Migration journal unavailable"))?;
            "imported"
        };
        keys.push(json!({"provider":provider,"source":name,"status":status}));
    }
    if apply {
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|_| failure("Model migration transaction unavailable"))?;
        for row in &catalog.models {
            let id = row["id"].as_str().unwrap();
            let current =
                store::get(&tx, "model", id).map_err(|_| failure("Model inventory unavailable"))?;
            let baseline = store::get(&tx, "catalog-model", id)
                .map_err(|_| failure("Model inventory unavailable"))?;
            if let Some(updated) = public_update(current, baseline, row) {
                store::put(&tx, "model", id, updated)
                    .map_err(|_| failure("Model repair failed"))?;
            }
            if store::get(&tx, "catalog-model", id)
                .map_err(|_| failure("Model inventory unavailable"))?
                .is_none_or(|v| content(v) != *row)
            {
                store::put(&tx, "catalog-model", id, row.clone())
                    .map_err(|_| failure("Catalog publication failed"))?;
            }
        }
        for old in store::list(&tx, "model").map_err(|_| failure("Model inventory unavailable"))? {
            if let Some(repaired) = orphan_repair(&old, &catalog) {
                store::put(&tx, "model", old["id"].as_str().unwrap(), repaired)
                    .map_err(|_| failure("Model alias repair failed"))?;
            }
        }
        tx.commit()
            .map_err(|_| failure("Model migration commit failed"))?;
    }
    Ok(
        json!({"applied":apply,"credentials_applied":apply&&!repair_only,"catalog_models":catalog.models.len(),"planned_model_repairs":planned,"credentials":keys,"unsupported":unsupported,"source_file_preserved":true}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn typed_alias_repair_preserves_its_protocol_and_model_route() {
        let catalog = kageha_models::catalog::Catalog {
            models: vec![
                json!({"id":"chat","provider":"cloudflare","base_url":"https://provider.example/accounts/account/ai/v1","protocol":"openai_compat","api_key_env":"TOKEN"}),
            ],
            roles: json!({}),
        };
        let repaired=orphan_repair(&json!({"id":"typed","provider":"cloudflare","model":"@cf/cloudflare/clef","protocol":"clef","base_url":"https://provider.example/accounts/${ACCOUNT}/ai/run/@cf/cloudflare/clef"}),&catalog).unwrap();
        assert_eq!(repaired["protocol"], "clef");
        assert_eq!(
            repaired["base_url"],
            "https://provider.example/accounts/account/ai/run/@cf/cloudflare/clef"
        );
    }
    #[test]
    fn repair_preserves_user_metadata_and_does_not_replace_valid_custom_endpoints() {
        let canonical = json!({"id":"m","base_url":"https://provider.example/v1","protocol":"openai_compat","api_key_env":"KEY"});
        let broken =
            json!({"id":"m","base_url":"","protocol":null,"roles":["worker"],"context_window":123});
        let repaired = public_update(Some(broken), None, &canonical).unwrap();
        assert_eq!(repaired["context_window"], 123);
        assert_eq!(repaired["roles"], json!(["worker"]));
        assert_eq!(repaired["base_url"], canonical["base_url"]);
        assert!(public_update(
            Some(
                json!({"id":"m","base_url":"https://custom.example/v1","protocol":"openai_compat"})
            ),
            None,
            &canonical
        )
        .is_none());
    }
}
