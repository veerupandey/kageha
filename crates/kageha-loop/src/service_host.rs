//! Application composition bridge. The registry and memory never import WebUI storage.
use crate::{server, webui::Database};
use kageha_contracts::*;
use kageha_memory::{http::HttpMemory, sqlite::SqliteMemory};
use kageha_services::Registry;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};
use tiny_http::{Method, Request};
pub struct Host {
    pub registry: Arc<Registry>,
    root: PathBuf,
    runtime: tokio::runtime::Runtime,
    configuration: Mutex<Value>,
    connection_errors: Mutex<HashMap<String, String>>,
}
static HOSTS: OnceLock<Mutex<HashMap<PathBuf, Arc<Host>>>> = OnceLock::new();
fn err(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}
impl Host {
    fn open(root: PathBuf) -> io::Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&root)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let registry = Arc::new(Registry::open(&root.join("registry.sqlite")).map_err(err)?);
        let config = match std::fs::read(root.join("providers.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => json!({}),
            Err(e) => return Err(e),
        };
        let host = Self {
            registry,
            root,
            runtime,
            configuration: Mutex::new(config),
            connection_errors: Mutex::new(HashMap::new()),
        };
        host.runtime.block_on(async {
            host.registry
                .register(
                    "local".into(),
                    Arc::new(SqliteMemory::open(&host.root.join("local.sqlite")).map_err(err)?),
                )
                .await
                .map_err(err)?;
            let config = host.configuration.lock().map_err(err)?.clone();
            for (id, definition) in config
                .as_object()
                .ok_or_else(|| err("Invalid provider configuration"))?
            {
                let result = match host.port(id, definition) {
                    Ok(port) => host.registry.register(id.clone(), port).await.map_err(err),
                    Err(error) => Err(error),
                };
                if let Err(error) = result {
                    host.connection_errors
                        .lock()
                        .map_err(err)?
                        .insert(id.clone(), error.to_string());
                }
            }
            host.registry.initialize("local").await.map_err(err)?;
            host.registry.recover_pending().await.map_err(err)?;
            Ok::<_, io::Error>(())
        })?;
        Ok(host)
    }
    fn port(&self, id: &str, definition: &Value) -> io::Result<Arc<dyn MemoryPort>> {
        if id == "local" || !crate::server::identifier(id) {
            return Err(err("Invalid destination ID"));
        }
        match definition["adapter"].as_str() {
            Some("sqlite") => Ok(Arc::new(
                SqliteMemory::open(&self.root.join(format!("{id}.sqlite"))).map_err(err)?,
            )),
            Some("memory-http") => {
                let reference = definition["credentials_ref"]
                    .as_str()
                    .and_then(|v| v.strip_prefix("env:"))
                    .filter(|v| {
                        !v.is_empty()
                            && v.bytes()
                                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
                    })
                    .ok_or_else(|| err("Use an env:NAME credential reference"))?;
                let token = std::env::var(reference)
                    .map_err(|_| err("Destination credential reference is unavailable"))?;
                Ok(Arc::new(
                    HttpMemory::new(definition["endpoint"].as_str().unwrap_or(""), token)
                        .map_err(err)?,
                ))
            }
            _ => Err(err("Unsupported memory adapter")),
        }
    }
    fn register(&self, id: &str, definition: Value) -> io::Result<Descriptor> {
        let keys = definition
            .as_object()
            .ok_or_else(|| err("Provider configuration must be an object"))?;
        if keys
            .keys()
            .any(|key| !matches!(key.as_str(), "adapter" | "endpoint" | "credentials_ref"))
        {
            return Err(err("Configuration accepts adapter, endpoint and credentials_ref only; never inline credentials"));
        }
        let mut configuration = self.configuration.lock().map_err(err)?;
        let port = self.port(id, &definition)?;
        let descriptor = self
            .runtime
            .block_on(self.registry.register(id.into(), port))
            .map_err(err)?;
        let mut next = configuration.clone();
        next[id] = definition;
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let temporary = self.root.join(format!(
            "providers-{}.pending",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(err)?
                .as_nanos()
        ));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec(&next)?)?;
        file.sync_all()?;
        std::fs::rename(temporary, self.root.join("providers.json"))?;
        std::fs::File::open(&self.root)?.sync_all()?;
        *configuration = next;
        self.connection_errors.lock().map_err(err)?.remove(id);
        Ok(descriptor)
    }
}
pub fn get(db: &Database) -> io::Result<Arc<Host>> {
    let path = {
        let db = db.lock().map_err(err)?;
        Path::new(
            db.path()
                .ok_or_else(|| err("Persistent state is required for services"))?,
        )
        .parent()
        .ok_or_else(|| err("State directory unavailable"))?
        .join("components/memory")
    };
    let mut hosts = HOSTS.get_or_init(Default::default).lock().map_err(err)?;
    if let Some(host) = hosts.get(&path) {
        return Ok(host.clone());
    }
    let host = Arc::new(Host::open(path.clone())?);
    hosts.insert(path, host.clone());
    Ok(host)
}
pub fn tool(
    db: &Database,
    workspace: &Path,
    actor: &str,
    turn: &str,
    call: &Value,
    cancel: &std::sync::atomic::AtomicBool,
) -> io::Result<String> {
    use kageha_contracts::execution::{Control, ToolCall, ToolContext, ToolPort};
    // Durable notes are reusable across turns. Apply the same secret boundary as
    // learned lessons before dispatch, including both the searchable ID and body.
    // An empty/unconfigured vault returns an empty set; a configured vault that
    // cannot be inspected fails closed. Never include the rejected content in an error.
    if call["name"] == "memory_put" {
        check_memory_note(call, &crate::ui_learning_secrets::VaultSecrets::new(db))?;
    }
    let adapter = kageha_tools::memory::MemoryTools::fixed(
        get(db)?.registry.clone(),
        MemoryAccess {
            namespace: kageha_tools::memory::workspace_namespace(workspace),
            actor: actor.into(),
        },
    )?;
    let call: ToolCall = serde_json::from_value(call.clone())?;
    adapter
        .execute_in(
            &ToolContext {
                session: actor.into(),
                turn: turn.into(),
            },
            &call,
            &Control {
                cancelled: cancel,
                deadline: None,
            },
        )
        .map(|r| r.content)
}

fn check_memory_note(call: &Value, source: &dyn kageha_learning::secrets::SecretSource) -> io::Result<()> {
    for field in ["id", "body"] {
        if let Some(text) = call["arguments"][field].as_str() {
            if kageha_learning::secrets::contains_secret(text, source).is_some() {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied,
                    "Memory was not saved because it may contain a secret or the credential vault is unavailable."));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod autonomous_memory_tests {
    use super::*;
    use crate::native_chat::ToolHost;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn memory_note_secret_checks_cover_ids_and_unavailable_vault() {
        use kageha_learning::secrets::SecretSource;
        use zeroize::Zeroizing;
        struct Vault(bool);
        impl SecretSource for Vault {
            fn values(&self) -> io::Result<Vec<Zeroizing<String>>> {
                if self.0 { Ok(vec![Zeroizing::new("private-vault-canary-7421".into())]) }
                else { Err(io::Error::other("locked")) }
            }
        }
        for args in [json!({"id":"private-vault-canary-7421","body":"ordinary"}),
            json!({"id":"normal","body":"private-vault-canary-7421"})] {
            let err = check_memory_note(&json!({"arguments":args}), &Vault(true)).unwrap_err();
            assert!(!err.to_string().contains("private-vault-canary-7421"));
        }
        assert!(check_memory_note(&json!({"arguments":{"id":"safe","body":"Use cargo test"}}), &Vault(false)).is_err());
        assert!(check_memory_note(&json!({"arguments":{"id":"safe","body":"Use cargo test"}}), &Vault(true)).is_ok());
    }

    #[test]
    fn notes_survive_turns_without_crossing_scopes_or_storing_secrets() {
        let fixture = crate::audited_session_tests::Fixture::new();
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("project");
        let other = root.path().join("other");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::create_dir(&other).unwrap();
        let cancel = AtomicBool::new(false);
        let call = |turn: &str, actor: &str, folder: &Path, name: &str, args: Value| {
            let request = json!({"id":format!("{turn}-{name}"),"name":name,"arguments":args});
            tool(&fixture.db, folder, actor, turn, &request, &cancel)
        };
        let saved = call("first", "main", &workspace, "memory_put",
            json!({"id":"build-preference","body":"The project uses cargo nextest for integration checks."})).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&saved).unwrap()["value"]["body"], "The project uses cargo nextest for integration checks.");
        let found: Value = serde_json::from_str(&call("second", "main", &workspace, "memory_search",
            json!({"query":"nextest"})).unwrap()).unwrap();
        assert_eq!(found["value"].as_array().unwrap().len(), 1);
        assert_eq!(found["value"][0]["id"], "build-preference");
        for (actor, folder) in [("other-agent", workspace.as_path()), ("main", other.as_path())] {
            let result: Value = serde_json::from_str(&call("second", actor, folder, "memory_search",
                json!({"query":"nextest"})).unwrap()).unwrap();
            assert!(result["value"].as_array().unwrap().is_empty());
        }
        let secret = "password = PRIVATE-MEMORY-CANARY-8675309";
        let error = call("secret", "main", &workspace, "memory_put", json!({"id":"bad","body":secret})).unwrap_err();
        assert!(!error.to_string().contains(secret));
        let denied = fixture.tools(json!({"id":"main","tool_policy":{"deny":["memory_put"]}}));
        assert!(denied.invoke(&json!({"id":"denied","name":"memory_put","arguments":{"id":"denied","body":"Do not persist this"}})).is_err());
        for id in ["bad", "denied"] {
            let result: Value = serde_json::from_str(&call("after", "main", &workspace, "memory_get", json!({"id":id})).unwrap()).unwrap();
            assert!(result["value"].is_null(), "{id} was stored");
        }
    }
}
pub fn handle(mut request: Request, db: &Database) -> io::Result<()> {
    let path = request.url().split('?').next().unwrap_or("").to_owned();
    let result = (|| -> io::Result<Value> {
        let host = get(db)?;
        if request.method() == &Method::Get {
            if path == "/api/services" {
                return Ok(
                    json!({"bindings":[host.registry.binding().map_err(err)?],"supported_replacement_components":["memory"],"connection_errors":*host.connection_errors.lock().map_err(err)?}),
                );
            }
            if path == "/api/services/events" {
                let after = crate::webui::query(request.url(), "after")
                    .parse()
                    .unwrap_or(0);
                return Ok(json!({"events":host.registry.events(after).map_err(err)?}));
            }
            if let Some(id) = path.strip_prefix("/api/services/replacements/") {
                return Ok(json!({"replacement":host.registry.job(id).map_err(err)?}));
            }
        }
        if request.method() != &Method::Post {
            return Err(err("Unsupported service operation"));
        }
        let body = crate::webui::body(&mut request)?;
        match path.as_str() {
            "/api/services/providers" => Ok(
                json!({"descriptor":host.register(body["id"].as_str().unwrap_or(""),body["configuration"].clone())?}),
            ),
            "/api/services/replacements" => {
                let decision = &body["decision"];
                if decision["allow_data_transfer"] != true {
                    return Err(err(
                        "Explicit destination-scoped data-transfer decision is required",
                    ));
                }
                let job = host
                    .runtime
                    .block_on(
                        host.registry.replace(
                            body["request_id"].as_str().unwrap_or(""),
                            decision["destination_id"].as_str().unwrap_or(""),
                            decision["instance_id"].as_str().unwrap_or(""),
                            body["expected_generation"]
                                .as_u64()
                                .ok_or_else(|| err("Expected generation is required"))?,
                        ),
                    )
                    .map_err(err)?;
                Ok(json!({"replacement":job}))
            }
            p if p.starts_with("/api/services/replacements/") && p.ends_with("/resume") => {
                let id = p
                    .trim_start_matches("/api/services/replacements/")
                    .trim_end_matches("/resume");
                Ok(
                    json!({"replacement":host.runtime.block_on(host.registry.resume(id)).map_err(err)?}),
                )
            }
            "/api/services/rollback" => Ok(
                json!({"replacement":host.runtime.block_on(host.registry.rollback(body["request_id"].as_str().unwrap_or(""),body["replacement_id"].as_str().unwrap_or(""),body["expected_generation"].as_u64().unwrap_or(0))).map_err(err)?}),
            ),
            _ => Err(err("Unknown service route")),
        }
    })();
    match result {
        Ok(value) => server::response(request, 200, value),
        Err(e) => server::response(request, 400, json!({"error":e.to_string()})),
    }
    Ok(())
}
