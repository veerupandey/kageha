//! Compatibility boundary only: encrypted storage is owned by kageha-credentials.
//! SQL transactions publish references, never credential values. Unpublished vault changes fail closed.
use crate::ui_store::{db_error, ApiResult};
use kageha_contracts::{execution::Control, ErrorCode};
use kageha_credentials::{keys::FileKeys, repository::SqliteRepository, *};
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    path::PathBuf,
    sync::{atomic::AtomicBool, Arc, Mutex, OnceLock},
};
struct Engine {
    vault: Arc<registry::Registry>,
    owner: String,
    artifacts: PathBuf,
}
static ENGINES: OnceLock<Mutex<BTreeMap<String, Arc<Engine>>>> = OnceLock::new();
struct Binding {
    reference: CredentialRef,
    revision: u64,
    revoked: bool,
}
fn error(error: kageha_contracts::ServiceError) -> (u16, String) {
    (
        match error.code {
            ErrorCode::NotFound => 404,
            ErrorCode::Unauthorized => 403,
            ErrorCode::Conflict | ErrorCode::StaleGeneration => 409,
            _ => 503,
        },
        error.message,
    )
}
fn runtime() -> ApiResult<&'static tokio::runtime::Runtime> {
    static RUNTIME: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();
    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(|_| "Credential runtime unavailable".into())
        })
        .as_ref()
        .map_err(|e| (503, e.clone()))
}
fn run<T: Send, F: Future<Output = kageha_contracts::Result<T>> + Send>(future: F) -> ApiResult<T> {
    let runtime = runtime()?;
    if tokio::runtime::Handle::try_current().is_ok() {
        std::thread::scope(|s| s.spawn(|| runtime.block_on(future)).join())
            .map_err(|_| (503, "Credential worker unavailable".into()))?
            .map_err(error)
    } else {
        runtime.block_on(future).map_err(error)
    }
}
fn control(flag: &AtomicBool) -> Control<'_> {
    Control {
        cancelled: flag,
        deadline: Some(std::time::Instant::now() + std::time::Duration::from_secs(20)),
    }
}
fn component(name: &str) -> &str {
    if name.starts_with("oauth:") {
        "oauth"
    } else if name.starts_with("mcp:") {
        "mcp"
    } else if name.starts_with("media:") {
        "media"
    } else if name.starts_with("channel:") {
        "channels"
    } else {
        "models"
    }
}
fn access(name: &str) -> CredentialAccess {
    CredentialAccess {
        principal: component(name).into(),
        purpose: "compat".into(),
        destination: "local".into(),
    }
}
fn grants(owner: &str, destinations: &BTreeSet<String>) -> Vec<Grant> {
    let mut grants: Vec<Grant> = ["models", "oauth", "mcp", "media", "channels"]
        .into_iter()
        .map(|part| Grant {
            principal: part.into(),
            namespace: format!("{owner}/{part}"),
            ids: BTreeSet::from(["*".into()]),
            purposes: BTreeSet::from(["compat".into()]),
            destinations: BTreeSet::from(["local".into()]),
            permissions: [
                Permission::Metadata,
                Permission::Resolve,
                Permission::Write,
                Permission::Revoke,
            ]
            .into_iter()
            .collect(),
        })
        .collect();
    for grant in &mut grants {
        if grant.principal == "oauth" {
            grant.permissions.insert(Permission::Export);
        }
    }
    grants.push(Grant {
        principal: "migration".into(),
        namespace: format!("{owner}/oauth"),
        ids: BTreeSet::from(["*".into()]),
        purposes: BTreeSet::from(["migration".into()]),
        destinations: BTreeSet::from(["local".into()]),
        permissions: BTreeSet::from([Permission::Export]),
    });
    for namespace in ["models", "oauth", "mcp", "media", "channels"]
        .map(|p| format!("{owner}/{p}"))
        .into_iter()
        .chain(["system".into()])
    {
        grants.push(Grant {
            principal: "replacement".into(),
            namespace,
            ids: BTreeSet::from(["*".into()]),
            purposes: BTreeSet::from(["replacement".into()]),
            destinations: destinations.clone(),
            permissions: BTreeSet::from([Permission::Admin, Permission::Write, Permission::Export]),
        });
    }
    grants
}
pub(crate) fn initialize(db: &Connection) -> ApiResult<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS ui_credential_owner(singleton INTEGER PRIMARY KEY CHECK(singleton=1),owner TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS ui_credential_refs(name TEXT PRIMARY KEY,namespace TEXT NOT NULL,credential_id TEXT NOT NULL,revision INTEGER NOT NULL,revoked INTEGER NOT NULL);
    CREATE TABLE IF NOT EXISTS ui_voice_artifacts(name TEXT PRIMARY KEY,path TEXT NOT NULL,sha256 TEXT NOT NULL); CREATE TABLE IF NOT EXISTS ui_credential_migrations(id TEXT PRIMARY KEY);").map_err(db_error)?;
    db.execute(
        "INSERT OR IGNORE INTO ui_credential_owner VALUES(1,?)",
        [format!("app:{}", uuid::Uuid::new_v4())],
    )
    .map_err(db_error)?;
    Ok(())
}
fn engine(db: &Connection) -> ApiResult<Arc<Engine>> {
    engine_with_key(db, None)
}
#[cfg(test)]
pub(crate) fn use_disposable_key(db: &Connection) -> ApiResult<()> {
    let key = kageha_credentials::crypto::random::<32>().map_err(error)?;
    engine_with_key(db, Some(SecretBytes::new(key.to_vec())))?;
    Ok(())
}
fn engine_with_key(db: &Connection, authorized_key: Option<SecretBytes>) -> ApiResult<Arc<Engine>> {
    initialize(db)?;
    let owner: String = db
        .query_row(
            "SELECT owner FROM ui_credential_owner WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .map_err(db_error)?;
    let file = db.path().unwrap_or("").to_owned();
    let cache_key = format!("{owner}:{file}");
    let mut cache = ENGINES
        .get_or_init(Default::default)
        .lock()
        .map_err(db_error)?;
    if let Some(engine) = cache.get(&cache_key) {
        return Ok(engine.clone());
    }
    let flag = AtomicBool::new(false);
    let c = control(&flag);
    let (repository, keys, artifacts, key_id): (
        Arc<dyn CredentialRepository>,
        Arc<dyn KeyProtector>,
        PathBuf,
        String,
    ) = if file.is_empty() {
        #[cfg(test)]
        {
            let repo = Arc::new(SqliteRepository::in_memory().map_err(error)?);
            let key = SecretBytes::new(
                kageha_credentials::crypto::random::<32>()
                    .map_err(error)?
                    .to_vec(),
            );
            (
                repo,
                Arc::new(keys::SuppliedKey::new("test", key)),
                std::env::temp_dir().join(format!("kageha-voice-{}", uuid::Uuid::new_v4())),
                "test".into(),
            )
        }
        #[cfg(not(test))]
        {
            return Err((
                503,
                "Persistent credential storage requires a persistent host database".into(),
            ));
        }
    } else {
        use std::os::unix::fs::DirBuilderExt;
        let parent = std::path::Path::new(&file)
            .parent()
            .ok_or((503, "Credential state directory unavailable".into()))?;
        let root = parent.join("credentials");
        let mut directory = std::fs::DirBuilder::new();
        directory.recursive(true).mode(0o700);
        directory.create(&root).map_err(db_error)?;
        let repo = Arc::new(SqliteRepository::open(&root.join("vault.sqlite")).map_err(error)?);
        let key_id = format!("master-{:x}", Sha256::digest(owner.as_bytes()));
        let keys: Arc<dyn KeyProtector> =
            if let Some(key) = authorized_key {
                Arc::new(keys::SuppliedKey::new(&key_id, key))
            } else if let Some(path) = std::env::var_os("KAGEHA_CREDENTIAL_KEY_FILE") {
                Arc::new(FileKeys(BTreeMap::from([(
                    key_id.clone(),
                    PathBuf::from(path),
                )])))
            } else {
                #[cfg(target_os = "macos")]
                {
                    let protector = keys::MacKeychain {
                        service: "com.kageha.credentials".into(),
                    };
                    if run(repo.header())?.is_none() {
                        run(protector.provision(&key_id))?;
                    }
                    Arc::new(protector)
                }
                #[cfg(not(target_os = "macos"))]
                {
                    return Err((
                        503,
                        "Mount a private credential key and set KAGEHA_CREDENTIAL_KEY_FILE".into(),
                    ));
                }
            };
        (repo, keys, root.join("voice-artifacts"), key_id)
    };
    let remotes: Vec<config::RemoteBackend> =
        if let Some(path) = std::env::var_os("KAGEHA_CREDENTIAL_BACKENDS") {
            let bytes =
                config::protected_input(std::path::Path::new(&path), 1024 * 1024).map_err(error)?;
            serde_json::from_slice(bytes.expose())
                .map_err(|_| (400, "Credential backend configuration is invalid".into()))?
        } else {
            Vec::new()
        };
    let destinations: BTreeSet<String> = std::iter::once("embedded".to_owned())
        .chain(remotes.iter().map(|b| b.id.clone()))
        .collect();
    let embedded = Arc::new(run(Vault::open(
        repository,
        keys,
        &key_id,
        grants(&owner, &destinations),
        &c,
    ))?);
    let mut backends = vec![registry::Backend {
        id: "embedded".into(),
        device_id: owner.clone(),
        store: embedded,
    }];
    for remote in remotes {
        backends.push(remote.connect("replacement").map_err(error)?);
    }
    let authority = CredentialAccess {
        principal: "replacement".into(),
        purpose: "replacement".into(),
        destination: "embedded".into(),
    };
    let vault = if file.is_empty() {
        run(registry::Registry::in_memory(
            "embedded", backends, &authority, &c,
        ))?
    } else {
        run(registry::Registry::open(
            &artifacts.parent().unwrap().join("registry.sqlite"),
            "embedded",
            backends,
            &authority,
            &c,
        ))?
    };
    let engine = Arc::new(Engine {
        vault,
        owner,
        artifacts,
    });
    cache.insert(cache_key, engine.clone());
    Ok(engine)
}
fn binding(db: &Connection, name: &str) -> ApiResult<Option<Binding>> {
    initialize(db)?;
    db.query_row(
        "SELECT namespace,credential_id,revision,revoked FROM ui_credential_refs WHERE name=?",
        [name],
        |r| {
            Ok(Binding {
                reference: CredentialRef {
                    namespace: r.get(0)?,
                    id: r.get(1)?,
                },
                revision: r.get(2)?,
                revoked: r.get(3)?,
            })
        },
    )
    .optional()
    .map_err(db_error)
}
fn engine_exists(db: &Connection) -> bool {
    let owner = db
        .query_row(
            "SELECT owner FROM ui_credential_owner WHERE singleton=1",
            [],
            |r| r.get::<_, String>(0),
        )
        .ok();
    if let (Some(owner), Some(cache)) = (owner, ENGINES.get()) {
        if cache
            .lock()
            .is_ok_and(|c| c.contains_key(&format!("{owner}:{}", db.path().unwrap_or(""))))
        {
            return true;
        }
    }
    db.path()
        .filter(|p| !p.is_empty())
        .and_then(|p| std::path::Path::new(p).parent())
        .is_some_and(|p| p.join("credentials/vault.sqlite").is_file())
}
pub(crate) fn managed(db: &Connection, name: &str) -> ApiResult<bool> {
    if binding(db, name)?.is_some() {
        return Ok(true);
    }
    if engine_exists(db) {
        let engine = engine(db)?;
        let flag = AtomicBool::new(false);
        return Ok(current(&engine, name, &control(&flag))?.is_some());
    }
    Ok(false)
}
fn reference(engine: &Engine, name: &str) -> CredentialRef {
    CredentialRef {
        namespace: format!("{}/{}", engine.owner, component(name)),
        id: name.into(),
    }
}
fn publish(db: &Connection, name: &str, meta: &CredentialMetadata, revoked: bool) -> ApiResult<()> {
    let changed=db.execute("INSERT INTO ui_credential_refs VALUES(?,?,?,?,?) ON CONFLICT(name) DO UPDATE SET namespace=excluded.namespace,credential_id=excluded.credential_id,revision=excluded.revision,revoked=excluded.revoked WHERE excluded.revision>=ui_credential_refs.revision",params![name,meta.reference.namespace,meta.reference.id,meta.revision,revoked]).map_err(db_error)?;
    if changed == 0 {
        return Err((
            409,
            "An older credential receipt cannot replace a newer settings projection".into(),
        ));
    }
    Ok(())
}
fn current(engine: &Engine, name: &str, c: &Control<'_>) -> ApiResult<Option<CredentialMetadata>> {
    match run(engine
        .vault
        .metadata(&access(name), &reference(engine, name), c))
    {
        Ok(v) => Ok(Some(v)),
        Err((404, _)) => Ok(None),
        Err(e) => Err(e),
    }
}
pub(crate) fn secret(db: &Connection, name: &str) -> ApiResult<String> {
    if let Some((id, field)) = oauth_alias(name) {
        if oauth_metadata(db, id)?.is_some() {
            return oauth_field(db, id, field);
        }
    }
    secret_basic(db, name)
}
fn secret_basic(db: &Connection, name: &str) -> ApiResult<String> {
    if name.starts_with("voice:") {
        return voice_read(db, name);
    }
    let Some(binding) = binding(db, name)? else {
        if engine_exists(db) {
            let engine = engine(db)?;
            let flag = AtomicBool::new(false);
            if let Some(meta) = current(&engine, name, &control(&flag))? {
                if !matches!(
                    meta.state,
                    CredentialState::Revoked | CredentialState::Deleted
                ) {
                    return Err((409, "Credential settings publication is incomplete".into()));
                }
            }
        }
        return Ok(String::new());
    };
    if binding.revoked {
        return Ok(String::new());
    }
    let engine = engine(db)?;
    let flag = AtomicBool::new(false);
    let c = control(&flag);
    let meta = run(engine.vault.metadata(&access(name), &binding.reference, &c))?;
    if matches!(
        meta.state,
        CredentialState::Revoked | CredentialState::Deleted | CredentialState::RefreshUncertain
    ) {
        return Ok(String::new());
    }
    if meta.revision != binding.revision {
        return Err((
            409,
            "Credential update is durable but its settings publication needs reconciliation".into(),
        ));
    }
    let lease = run(engine.vault.resolve(&access(name), &binding.reference, &c))?;
    let value = std::str::from_utf8(lease.secret.expose())
        .map_err(|_| (503, "Credential encoding is invalid".into()))?
        .to_owned();
    Ok(value)
}
/// A number that changes whenever a published credential is added, changed, revoked or
/// removed, for caching [`vault_values`]. Reads only the references, never a value.
pub(crate) fn vault_generation(db: &Connection) -> ApiResult<u64> {
    use std::hash::{Hash, Hasher};
    initialize(db)?;
    let mut query = db
        .prepare("SELECT name,revision,revoked FROM ui_credential_refs ORDER BY name")
        .map_err(db_error)?;
    let rows = query
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, bool>(2)?)))
        .map_err(db_error)?;
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    for row in rows {
        row.map_err(db_error)?.hash(&mut hash);
    }
    Ok(hash.finish())
}
/// Every value held in the credential vault, for the learning pipeline's secret scan
/// (`ui_learning_secrets`; design section 15 "Safety"): the value of each published,
/// active and unexpired credential, or the string fields of a compound one (OAuth
/// grants, private settings, account details). Values of fewer than
/// `kageha_learning::secrets::MIN_VAULT_VALUE_CHARS` characters are left out. The values
/// stay in memory and are zeroized when dropped; errors never hold a value.
pub(crate) fn vault_values(db: &Connection) -> ApiResult<Vec<zeroize::Zeroizing<String>>> {
    use zeroize::Zeroizing;
    fn collect(value: &serde_json::Value, out: &mut Vec<Zeroizing<String>>) {
        match value {
            serde_json::Value::String(text) => out.push(Zeroizing::new(text.clone())),
            serde_json::Value::Array(items) => items.iter().for_each(|item| collect(item, out)),
            serde_json::Value::Object(map) => map.values().for_each(|item| collect(item, out)),
            _ => {}
        }
    }
    initialize(db)?;
    let references: Vec<(String, CredentialRef)> = {
        let mut query = db
            .prepare("SELECT name,namespace,credential_id FROM ui_credential_refs WHERE revoked=0 ORDER BY name")
            .map_err(db_error)?;
        let rows = query
            .query_map([], |r| {
                Ok((r.get(0)?, CredentialRef { namespace: r.get(1)?, id: r.get(2)? }))
            })
            .map_err(db_error)?;
        rows.collect::<Result<_, _>>().map_err(db_error)?
    };
    if references.is_empty() {
        return Ok(Vec::new());
    }
    let engine = engine(db)?;
    let mut values = Vec::new();
    for (name, reference) in references {
        let flag = AtomicBool::new(false);
        let c = control(&flag);
        let meta = match run(engine.vault.metadata(&access(&name), &reference, &c)) {
            Ok(meta) => meta,
            Err((404, _)) => continue,
            Err(error) => return Err(error),
        };
        if meta.state != CredentialState::Active
            || meta.expires_at_ms.is_some_and(|v| v <= kageha_credentials::crypto::now_ms())
        {
            continue;
        }
        let lease = run(engine.vault.resolve(&access(&name), &reference, &c))?;
        let bytes = lease.secret.expose();
        if matches!(bytes.iter().find(|b| !b.is_ascii_whitespace()), Some(b'{' | b'[')) {
            if let Ok(json) = SecretJson::parse(bytes) {
                collect(&json, &mut values);
                continue;
            }
        }
        if let Ok(text) = std::str::from_utf8(bytes) {
            values.push(Zeroizing::new(text.to_owned()));
        }
    }
    values.retain(|value| value.chars().count() >= kageha_learning::secrets::MIN_VAULT_VALUE_CHARS);
    Ok(values)
}
pub(crate) fn save(db: &Connection, name: &str, value: &str) -> ApiResult<()> {
    if name.starts_with("voice:") {
        return voice_save(db, name, value);
    }
    if value.len() > 65536 || value.chars().any(char::is_control) {
        return Err((400, "Credential input exceeds supported limits".into()));
    }
    if value.is_empty() {
        return delete(db, name);
    }
    let engine = engine(db)?;
    let flag = AtomicBool::new(false);
    let c = control(&flag);
    let previous = current(&engine, name, &c)?;
    if let Some(meta) = previous
        .as_ref()
        .filter(|m| m.state == CredentialState::Active)
    {
        let lease = run(engine.vault.resolve(&access(name), &meta.reference, &c))?;
        if lease.secret.expose() == value.as_bytes() {
            return publish(db, name, meta, false);
        }
    }
    let expected = previous.map(|v| v.revision).unwrap_or(0);
    let receipt = run(engine.vault.put(
        &access(name),
        CredentialWrite {
            operation_id: format!("ui:{}", uuid::Uuid::new_v4()),
            reference: reference(&engine, name),
            expected_revision: expected,
            kind: component(name).into(),
            expires_at_ms: None,
            secret: SecretBytes::new(value.as_bytes().to_vec()),
        },
        &c,
    ))?;
    publish(db, name, &receipt.metadata, false)
}
pub(crate) fn delete(db: &Connection, name: &str) -> ApiResult<()> {
    if let Some((id, _)) = oauth_alias(name) {
        if oauth_metadata(db, id)?.is_some() {
            return delete_basic(db, &grant_name(id));
        }
    }
    delete_basic(db, name)
}
fn delete_basic(db: &Connection, name: &str) -> ApiResult<()> {
    if name.starts_with("voice:") {
        db.execute("DELETE FROM ui_voice_artifacts WHERE name=?", [name])
            .map_err(db_error)?;
        return Ok(());
    }
    let engine = engine(db)?;
    let flag = AtomicBool::new(false);
    let c = control(&flag);
    let old = current(&engine, name, &c)?;
    if let Some(meta) = old {
        let metadata = if matches!(
            meta.state,
            CredentialState::Revoked | CredentialState::Deleted
        ) {
            meta
        } else {
            run(engine.vault.revoke(
                &access(name),
                &meta.reference,
                meta.revision,
                &format!("ui-revoke:{}", uuid::Uuid::new_v4()),
                true,
                &c,
            ))?
            .metadata
        };
        publish(db, name, &metadata, true)?;
    } else {
        let created = run(engine.vault.put(
            &access(name),
            CredentialWrite {
                operation_id: format!("ui-empty:{}", uuid::Uuid::new_v4()),
                reference: reference(&engine, name),
                expected_revision: 0,
                kind: component(name).into(),
                expires_at_ms: None,
                secret: SecretBytes::new(Vec::new()),
            },
            &c,
        ))?;
        let deleted = run(engine.vault.revoke(
            &access(name),
            &created.metadata.reference,
            created.metadata.revision,
            &format!("ui-revoke:{}", uuid::Uuid::new_v4()),
            true,
            &c,
        ))?;
        publish(db, name, &deleted.metadata, true)?;
    }
    Ok(())
}
fn voice_save(db: &Connection, name: &str, encoded: &str) -> ApiResult<()> {
    use std::{
        io::Write,
        os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    };
    let bytes = data_encoding::BASE64
        .decode(encoded.as_bytes())
        .map_err(|_| (400, "Voice reference is not valid base64".into()))?;
    if bytes.len() > 12 * 1024 * 1024 {
        return Err((400, "Voice reference exceeds artifact limit".into()));
    }
    let engine = engine(db)?;
    let mut directory = std::fs::DirBuilder::new();
    directory.recursive(true).mode(0o700);
    directory.create(&engine.artifacts).map_err(db_error)?;
    let path = engine.artifacts.join(uuid::Uuid::new_v4().to_string());
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
        .map_err(db_error)?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(db_error)?;
    let hash = format!("{:x}", Sha256::digest(&bytes));
    db.execute("INSERT INTO ui_voice_artifacts VALUES(?,?,?) ON CONFLICT(name) DO UPDATE SET path=excluded.path,sha256=excluded.sha256",params![name,path.to_string_lossy(),hash]).map_err(db_error)?;
    voice_read(db, name)?;
    Ok(())
}
fn voice_read(db: &Connection, name: &str) -> ApiResult<String> {
    initialize(db)?;
    let row: Option<(String, String)> = db
        .query_row(
            "SELECT path,sha256 FROM ui_voice_artifacts WHERE name=?",
            [name],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(db_error)?;
    let Some((path, hash)) = row else {
        return Ok(String::new());
    };
    let flag = AtomicBool::new(false);
    let snapshot =
        kageha_workspace::file_snapshot(std::path::Path::new(&path), Some(&hash), &control(&flag))
            .map_err(db_error)?;
    Ok(data_encoding::BASE64.encode(&snapshot.bytes))
}
/// Startup migration runs before admitting turns. A failed import keeps its source rows intact.
pub(crate) fn migrate(db: &Connection) -> ApiResult<()> {
    initialize(db)?;
    let rows = {
        let mut s = db
            .prepare("SELECT provider,value FROM ui_secrets ORDER BY provider")
            .map_err(db_error)?;
        let rows = s
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    zeroize::Zeroizing::new(r.get::<_, String>(1)?),
                ))
            })
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)?;
        rows
    };
    let atomic_oauth: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM ui_credential_migrations WHERE id='oauth_atomic_v1')",
            [],
            |r| r.get(0),
        )
        .map_err(db_error)?;
    let pending = {
        let mut statement = db
            .prepare("SELECT id,body FROM ui_documents WHERE kind='oauth-pending'")
            .map_err(db_error)?;
        let rows = statement
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    zeroize::Zeroizing::new(r.get::<_, String>(1)?),
                ))
            })
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)?;
        rows
    };
    let tx = db.unchecked_transaction().map_err(db_error)?;
    if !atomic_oauth || rows.iter().any(|(name, _)| oauth_alias(name).is_some()) {
        migrate_oauth(&tx, &rows)?;
    }
    for (name, value) in &rows {
        if oauth_alias(name).is_some() {
            tx.execute("DELETE FROM ui_secrets WHERE provider=?", [name])
                .map_err(db_error)?;
            continue;
        }
        if name.starts_with("voice:") {
            voice_save(&tx, name, value)?;
        } else {
            let engine = engine(&tx)?;
            let flag = AtomicBool::new(false);
            let c = control(&flag);
            let reference = reference(&engine, name);
            let receipt = run(engine.vault.put(
                &access(name),
                CredentialWrite {
                    operation_id: format!("legacy:{}:{name}", engine.owner),
                    reference: reference.clone(),
                    expected_revision: 0,
                    kind: component(name).into(),
                    expires_at_ms: None,
                    secret: SecretBytes::new(value.as_bytes().to_vec()),
                },
                &c,
            ))?;
            let lease = run(engine.vault.resolve(&access(name), &reference, &c))?;
            if lease.secret.expose() != value.as_bytes() {
                return Err((
                    409,
                    "Encrypted credential migration verification failed".into(),
                ));
            }
            publish(&tx, name, &receipt.metadata, false)?;
        }
        if secret(&tx, name)?.as_bytes() != value.as_bytes() {
            return Err((409, "Credential migration verification failed".into()));
        }
        tx.execute("DELETE FROM ui_secrets WHERE provider=?", [name])
            .map_err(db_error)?;
    }
    for (id, body) in pending {
        let mut value: serde_json::Value = serde_json::from_str(&body)
            .map_err(|_| (503, "Legacy OAuth pending state is invalid".into()))?;
        let encrypted = value["credential_ref"].is_string();
        if encrypted {
            if atomic_oauth {
                continue;
            }
            value = pending_get(&tx, &id)?
                .ok_or((409, "Encrypted OAuth pending state is unavailable".into()))?;
        }
        if !atomic_oauth {
            let previous = crate::ui_store::get(&tx, "oauth-generation", &id)
                .map_err(db_error)?
                .and_then(|v| v["value"].as_u64())
                .unwrap_or(0);
            if value["generation"].as_u64() != Some(previous) {
                value["claimed"] = serde_json::json!(true);
            }
            let current = if oauth_metadata(&tx, &id)?.is_some() {
                oauth_generation(&tx, &id)?
            } else {
                oauth_bump(&tx, &id)?
            };
            value["generation"] = serde_json::json!(current);
        }
        pending_put(&tx, &id, value)?;
    }
    // Historical pending flows are not credentials to reactivate. Current flows are verified above;
    // remove their old plaintext verifier/client-secret snapshots, retaining sanitized projections.
    tx.execute("DELETE FROM ui_document_history WHERE kind='oauth-pending' AND json_extract(body,'$.credential_ref') IS NULL",[]).map_err(db_error)?;
    tx.execute(
        "INSERT OR IGNORE INTO ui_credential_migrations VALUES('oauth_atomic_v1')",
        [],
    )
    .map_err(db_error)?;
    migrate_account_metadata(&tx)?;
    migrate_mcp_private_metadata(&tx)?;
    {
        let rows = {
            let mut q = tx
                .prepare("SELECT id,body FROM ui_documents WHERE kind='integration'")
                .map_err(db_error)?;
            let rows = q
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map_err(db_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(db_error)?;
            rows
        };
        for (id, body) in rows {
            let mut info: serde_json::Value = serde_json::from_str(&body).map_err(db_error)?;
            if info["id"].is_null() {
                info["id"] = serde_json::json!(id);
            }
            if info["authorization_id"].is_null() {
                if let Some(authority) = oauth_authority(&tx, &id)? {
                    info["authorization_id"] = serde_json::json!(authority);
                }
            }
            tx.execute(
                "UPDATE ui_documents SET body=? WHERE kind='integration' AND id=?",
                params![info.to_string(), id],
            )
            .map_err(db_error)?;
        }
    }

    tx.commit().map_err(db_error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn database() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        crate::ui_store::initialize(&db).unwrap();
        initialize(&db).unwrap();
        db
    }
    #[test]
    fn verified_migration_removes_plaintext_and_is_restartable() {
        let db = database();
        db.execute(
            "INSERT INTO ui_secrets VALUES(?,?)",
            params!["provider", "MIGRATION-CANARY"],
        )
        .unwrap();
        migrate(&db).unwrap();
        assert_eq!(secret(&db, "provider").unwrap(), "MIGRATION-CANARY");
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM ui_secrets", [], |r| r
                .get::<_, u64>(0))
                .unwrap(),
            0
        );
        let first = binding(&db, "provider").unwrap().unwrap().revision;
        migrate(&db).unwrap();
        assert_eq!(binding(&db, "provider").unwrap().unwrap().revision, first);
        delete(&db, "provider").unwrap();
        assert!(managed(&db, "provider").unwrap());
        assert_eq!(secret(&db, "provider").unwrap(), "");
    }
    #[test]
    fn failed_import_retains_sources_and_reuses_confirmed_import() {
        let db = database();
        db.execute(
            "INSERT INTO ui_secrets VALUES(?,?)",
            params!["a", "first-secret"],
        )
        .unwrap();
        db.execute(
            "INSERT INTO ui_secrets VALUES(?,?)",
            params!["b", "x".repeat(65537)],
        )
        .unwrap();
        assert!(migrate(&db).is_err());
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM ui_secrets", [], |r| r
                .get::<_, u64>(0))
                .unwrap(),
            2
        );
        db.execute(
            "UPDATE ui_secrets SET value='second-secret' WHERE provider='b'",
            [],
        )
        .unwrap();
        migrate(&db).unwrap();
        assert_eq!(secret(&db, "a").unwrap(), "first-secret");
        assert_eq!(secret(&db, "b").unwrap(), "second-secret");
    }
    #[test]
    fn publication_rollback_never_uses_a_different_key() {
        let mut db = database();
        save(&db, "provider", "first-key").unwrap();
        {
            let tx = db.transaction().unwrap();
            save(&tx, "provider", "second-key").unwrap();
            tx.rollback().unwrap();
        }
        assert_eq!(secret(&db, "provider").unwrap_err().0, 409);
        save(&db, "provider", "second-key").unwrap();
        assert_eq!(secret(&db, "provider").unwrap(), "second-key");
    }
    #[test]
    fn voice_references_are_artifacts_and_not_vault_records() {
        let db = database();
        let encoded = data_encoding::BASE64.encode(b"disposable-voice-reference");
        db.execute(
            "INSERT INTO ui_secrets VALUES(?,?)",
            params!["voice:sample", encoded],
        )
        .unwrap();
        migrate(&db).unwrap();
        assert_eq!(secret(&db, "voice:sample").unwrap(), encoded);
        assert!(binding(&db, "voice:sample").unwrap().is_none());
        let engine = engine(&db).unwrap();
        let flag = AtomicBool::new(false);
        assert!(current(&engine, "voice:sample", &control(&flag))
            .unwrap()
            .is_none());
        std::fs::remove_dir_all(&engine.artifacts).unwrap();
    }
    #[test]
    fn vault_values_lists_active_values_and_compound_fields() {
        let db = database();
        assert!(vault_values(&db).unwrap().is_empty());
        let empty = vault_generation(&db).unwrap();
        save(&db, "provider-a", "first-provider-value").unwrap();
        save(&db, "provider-b", "short").unwrap();
        save(&db, "mcp:server", r#"{"headers":{"X-Api":"compound-field-value"},"port":8080}"#).unwrap();
        let saved = vault_generation(&db).unwrap();
        assert_ne!(empty, saved);
        let values = |db: &Connection| -> Vec<String> {
            let mut values: Vec<String> = vault_values(db).unwrap().iter().map(|v| v.to_string()).collect();
            values.sort();
            values
        };
        assert_eq!(values(&db), ["compound-field-value", "first-provider-value"]);
        delete(&db, "provider-a").unwrap();
        assert_ne!(vault_generation(&db).unwrap(), saved);
        assert_eq!(values(&db), ["compound-field-value"]);
    }
}

pub(crate) fn pending_get(db: &Connection, id: &str) -> ApiResult<Option<serde_json::Value>> {
    if crate::ui_store::get(db, "oauth-pending", id)
        .map_err(db_error)?
        .is_none()
    {
        return Ok(None);
    }
    let value = secret(db, &format!("oauth:{id}:pending"))?;
    if value.is_empty() {
        return Ok(None);
    }
    serde_json::from_str(&value)
        .map(Some)
        .map_err(|_| (503, "Encrypted OAuth pending state is invalid".into()))
}
pub(crate) fn pending_put(db: &Connection, id: &str, value: serde_json::Value) -> ApiResult<()> {
    let name = format!("oauth:{id}:pending");
    save(db, &name, &value.to_string())?;
    crate::ui_store::put(db,"oauth-pending",id,serde_json::json!({"credential_ref":name,"kind":value["kind"],"generation":value["generation"],"expires":value["expires"],"claimed":value["claimed"]})).map_err(db_error)?;
    Ok(())
}
pub(crate) fn pending_delete(db: &Connection, id: &str) -> ApiResult<()> {
    delete(db, &format!("oauth:{id}:pending"))?;
    db.execute(
        "DELETE FROM ui_documents WHERE kind='oauth-pending' AND id=?",
        [id],
    )
    .map_err(db_error)?;
    Ok(())
}

#[derive(Clone)]
pub(crate) struct ModelSource {
    pub db: crate::webui::Database,
    pub id: String,
    pub provider: String,
    pub model: String,
    pub protocol: String,
    pub url: String,
    pub destination: String,
}
struct ModelSources(Vec<ModelSource>);
pub(crate) fn model_origin(url: &str) -> ApiResult<String> {
    let parsed = std::str::from_utf8(url.as_bytes()).map_err(db_error)?;
    let url = kageha_models::catalog::completion_url(parsed, "openai_compat").map_err(db_error)?;
    let scheme = url.split("://").next().unwrap_or("");
    let authority = url
        .split("://")
        .nth(1)
        .unwrap_or("")
        .split('/')
        .next()
        .unwrap_or("");
    Ok(format!("{scheme}://{authority}"))
}
impl ModelSource {
    pub(crate) fn binding(&self) -> kageha_models::CredentialBinding {
        kageha_models::CredentialBinding {
            allow_empty: true,
            reference: CredentialRef {
                namespace: "kageha.models".into(),
                id: self.id.clone(),
            },
            access: CredentialAccess {
                principal: "models".into(),
                purpose: "model".into(),
                destination: self.destination.clone(),
            },
        }
    }
    fn resolve(&self) -> ApiResult<SecretLease> {
        use hmac::{Hmac, Mac};
        let (model, key, revision) = {
            let connection = self.db.lock().map_err(db_error)?;
            let row = crate::ui_store::get(&connection, "model", &self.id)
                .map_err(db_error)?
                .ok_or((409, "Model configuration no longer exists".into()))?;
            if row["provider"].as_str().unwrap_or("environment") != self.provider
                || row["model"].as_str().unwrap_or(&self.id) != self.model
                || row["protocol"].as_str().unwrap_or("openai_compat") != self.protocol
                || kageha_models::catalog::completion_url(
                    row["base_url"].as_str().unwrap_or(""),
                    &self.protocol,
                )
                .map_err(db_error)?
                    != self.url
                || model_origin(row["base_url"].as_str().unwrap_or(""))? != self.destination
            {
                return Err((
                    409,
                    "Model configuration changed before credential dispatch".into(),
                ));
            }
            let key = if row["oauth_id"].as_str().is_some() {
                None
            } else {
                Some(crate::ui_settings::model_key(&connection, &row)?)
            };
            let revision = row["revision"].as_u64().unwrap_or(0);
            (row, key, revision)
        };
        let key = if let Some(key) = key {
            key
        } else {
            crate::native_auth::bearer(&self.db, model["oauth_id"].as_str().unwrap())?
        };
        if key.is_empty() {
            let connection = self.db.lock().map_err(db_error)?;
            if binding(&connection, &self.provider)?.is_some()
                || model["oauth_id"].is_string()
                || model["api_key_env"].as_str().is_some_and(|v| !v.is_empty())
            {
                return Err((403, "Model credential is unavailable or revoked".into()));
            }
        }
        let secret = SecretBytes::new(key.into_bytes());
        // Only a keyed change detector leaves secret memory; public versions are counters, never key hashes.
        struct Versions {
            key: SecretBytes,
            epoch: u64,
            values: BTreeMap<String, (Vec<u8>, u64)>,
        }
        static VERSIONS: OnceLock<kageha_contracts::Result<Mutex<Versions>>> = OnceLock::new();
        let versions = VERSIONS
            .get_or_init(|| {
                Ok(Mutex::new(Versions {
                    key: SecretBytes::new(kageha_credentials::crypto::random::<32>()?.to_vec()),
                    epoch: u64::from_le_bytes(kageha_credentials::crypto::random::<8>()?),
                    values: BTreeMap::new(),
                }))
            })
            .as_ref()
            .map_err(|_| (503, "Credential change detector unavailable".into()))?;
        let mut versions = versions.lock().map_err(db_error)?;
        let mut mac = Hmac::<Sha256>::new_from_slice(versions.key.expose()).map_err(db_error)?;
        mac.update(secret.expose());
        mac.update(&revision.to_le_bytes());
        let generation = {
            let connection = self.db.lock().map_err(db_error)?;
            if engine_exists(&connection) {
                let engine = engine(&connection)?;
                let flag = AtomicBool::new(false);
                run(engine.vault.binding_generation(&control(&flag)))?
            } else {
                0
            }
        };
        mac.update(&generation.to_le_bytes());
        mac.update(self.destination.as_bytes());
        let digest = mac.finalize().into_bytes().to_vec();
        let namespace = {
            let connection = self.db.lock().map_err(db_error)?;
            connection
                .query_row(
                    "SELECT owner FROM ui_credential_owner WHERE singleton=1",
                    [],
                    |r| r.get::<_, String>(0),
                )
                .map_err(db_error)?
        };
        let key = format!("{namespace}/{}", self.id);
        let entry = versions.values.entry(key).or_insert((digest.clone(), 1));
        if entry.0 != digest {
            *entry = (digest, entry.1 + 1);
        }
        let version = entry.1;
        Ok(SecretLease {
            secret,
            metadata: CredentialMetadata {
                reference: self.binding().reference,
                kind: "model_binding".into(),
                revision: version,
                authority_generation: versions.epoch,
                expires_at_ms: None,
                state: CredentialState::Active,
                refreshable: false,
            },
            valid_until_ms: kageha_credentials::crypto::now_ms() + 60000,
        })
    }
}
#[async_trait::async_trait]
impl CredentialResolver for ModelSources {
    async fn resolve(
        &self,
        access: &CredentialAccess,
        reference: &CredentialRef,
        control: &Control<'_>,
    ) -> kageha_contracts::Result<SecretLease> {
        control.check().map_err(|_| {
            kageha_contracts::ServiceError::new(
                ErrorCode::Cancelled,
                "Model credential resolution interrupted",
            )
        })?;
        let source = self
            .0
            .iter()
            .find(|source| {
                source.binding().reference == *reference
                    && source.destination == access.destination
                    && access.principal == "models"
                    && access.purpose == "model"
            })
            .cloned()
            .ok_or_else(|| {
                kageha_contracts::ServiceError::new(
                    ErrorCode::Unauthorized,
                    "Model credential scope mismatch",
                )
            })?;
        tokio::task::spawn_blocking(move || source.resolve())
            .await
            .map_err(|_| {
                kageha_contracts::ServiceError::new(
                    ErrorCode::Unavailable,
                    "Model credential worker unavailable",
                )
            })?
            .map_err(|(status, message)| {
                kageha_contracts::ServiceError::new(
                    if status == 409 {
                        ErrorCode::StaleGeneration
                    } else {
                        ErrorCode::Unauthorized
                    },
                    message,
                )
            })
    }
    async fn validate_lease(
        &self,
        access: &CredentialAccess,
        lease: &SecretLease,
        control: &Control<'_>,
    ) -> kageha_contracts::Result<()> {
        let current = self
            .resolve(access, &lease.metadata.reference, control)
            .await?;
        if lease.valid_until_ms <= kageha_credentials::crypto::now_ms()
            || current.metadata.revision != lease.metadata.revision
            || current.metadata.authority_generation != lease.metadata.authority_generation
        {
            return Err(kageha_contracts::ServiceError::new(
                ErrorCode::StaleGeneration,
                "Model credential changed before dispatch",
            ));
        }
        Ok(())
    }
}
pub(crate) fn model(
    providers: Vec<crate::ui_settings::Provider>,
) -> std::io::Result<kageha_models::HttpModel> {
    let sources = providers.iter().filter_map(|p| p.source.clone()).collect();
    // Any model of the chain can serve a request, so the smallest window applies.
    let context_window = providers.iter().filter_map(|p| p.context_window).min();
    let configured = providers
        .into_iter()
        .map(|p| {
            let binding = p.source.as_ref().map(ModelSource::binding);
            (
                kageha_models::Provider {
                    url: p.url,
                    key: p.key,
                    model: p.model,
                    protocol: p.protocol,
                },
                binding,
            )
        })
        .collect();
    Ok(kageha_models::HttpModel::with_optional_credentials(configured, Arc::new(ModelSources(sources)))?
        .with_context_window(context_window))
}

fn oauth_alias(name: &str) -> Option<(&str, &str)> {
    let (id, field) = name.strip_prefix("oauth:")?.rsplit_once(':')?;
    matches!(field, "token" | "refresh").then_some((id, field))
}
fn grant_name(id: &str) -> String {
    format!("oauth:{id}:grant")
}
pub(crate) fn oauth_metadata(db: &Connection, id: &str) -> ApiResult<Option<CredentialMetadata>> {
    if binding(db, &grant_name(id))?.is_none() && !engine_exists(db) {
        return Ok(None);
    }
    let engine = engine(db)?;
    let name = grant_name(id);
    let flag = AtomicBool::new(false);
    let meta = current(&engine, &name, &control(&flag))?;
    if let Some(meta) = &meta {
        publish(
            db,
            &name,
            meta,
            matches!(
                meta.state,
                CredentialState::Revoked | CredentialState::Deleted
            ),
        )?;
    }
    Ok(meta)
}
pub(crate) fn oauth_ready(db: &Connection, id: &str) -> ApiResult<bool> {
    let Some(meta) = oauth_metadata(db, id)? else {
        return Ok(false);
    };
    if meta.state != CredentialState::Active {
        return Ok(false);
    }
    if meta
        .expires_at_ms
        .is_none_or(|v| v > kageha_credentials::crypto::now_ms())
    {
        Ok(!oauth_field(db, id, "token")?.is_empty())
    } else {
        Ok(meta.refreshable)
    }
}
pub(crate) fn oauth_generation(db: &Connection, id: &str) -> ApiResult<u64> {
    Ok(oauth_metadata(db, id)?.map(|m| m.revision).unwrap_or(0))
}
pub(crate) fn oauth_bump(db: &Connection, id: &str) -> ApiResult<u64> {
    let engine = engine(db)?;
    let name = grant_name(id);
    let flag = AtomicBool::new(false);
    let c = control(&flag);
    let meta = current(&engine, &name, &c)?;
    let result = if let Some(meta) = meta {
        run(engine.vault.fence(
            &access(&name),
            &meta.reference,
            meta.revision,
            &format!("oauth-fence:{}", uuid::Uuid::new_v4()),
            &c,
        ))?
    } else {
        let created = run(engine.vault.put(
            &access(&name),
            CredentialWrite {
                operation_id: format!("oauth-empty:{}", uuid::Uuid::new_v4()),
                reference: reference(&engine, &name),
                expected_revision: 0,
                kind: "oauth_grant".into(),
                expires_at_ms: None,
                secret: SecretBytes::new(Vec::new()),
            },
            &c,
        ))?;
        run(engine.vault.revoke(
            &access(&name),
            &created.metadata.reference,
            created.metadata.revision,
            &format!("oauth-inactive:{}", uuid::Uuid::new_v4()),
            false,
            &c,
        ))?
    };
    publish(
        db,
        &name,
        &result.metadata,
        result.metadata.state != CredentialState::Active,
    )?;
    Ok(result.metadata.revision)
}
fn token_bundle(
    token: &serde_json::Value,
    old: Option<&serde_json::Value>,
    fresh: bool,
) -> ApiResult<(SecretBytes, Option<u64>)> {
    let access_token = token["access_token"]
        .as_str()
        .filter(|v| !v.is_empty() && v.len() <= 16384 && !v.chars().any(char::is_control))
        .ok_or((
            400,
            "OAuth response did not contain a valid access token".into(),
        ))?;
    let refresh = token["refresh_token"]
        .as_str()
        .filter(|v| !v.is_empty())
        .or_else(|| {
            if fresh {
                None
            } else {
                old.and_then(|v| v["refresh_token"].as_str())
            }
        })
        .unwrap_or("");
    if refresh.len() > 16384 || refresh.chars().any(char::is_control) {
        return Err((
            400,
            "OAuth response contained an invalid refresh token".into(),
        ));
    }
    let authorization = if fresh {
        serde_json::json!(uuid::Uuid::new_v4().to_string())
    } else {
        old.map(|v| v["authorization_id"].clone())
            .unwrap_or(serde_json::Value::Null)
    };
    let bundle = SecretJson::new(
        serde_json::json!({"access_token":access_token,"refresh_token":refresh,"authorization_id":authorization}),
    );
    let expires = token["expires_in"]
        .as_f64()
        .filter(|v| v.is_finite())
        .map(|v| {
            kageha_credentials::crypto::now_ms()
                .saturating_add((v.clamp(0.0, 31536000.0) * 1000.0) as u64)
        });
    Ok((SecretBytes::new(bundle.to_string().into_bytes()), expires))
}
pub(crate) fn oauth_store(
    db: &Connection,
    id: &str,
    token: &serde_json::Value,
    fresh: bool,
    expected: u64,
) -> ApiResult<CredentialMetadata> {
    let engine = engine(db)?;
    let name = grant_name(id);
    let flag = AtomicBool::new(false);
    let c = control(&flag);
    let previous = current(&engine, &name, &c)?;
    if previous.as_ref().map(|m| m.revision).unwrap_or(0) != expected {
        return Err((
            409,
            "Credentials changed during authorization; retry with the current connection".into(),
        ));
    }
    let old = if !fresh {
        if let Some(meta) = &previous {
            if meta.state == CredentialState::Active {
                let lease = run(engine.vault.resolve(&access(&name), &meta.reference, &c))?;
                Some(SecretJson::parse(lease.secret.expose())?)
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };
    let (secret, expiry) = token_bundle(token, old.as_ref().map(|v| &v.0), fresh)?;
    let operation = run(engine.vault.put(
        &access(&name),
        CredentialWrite {
            operation_id: format!("oauth-token:{}", uuid::Uuid::new_v4()),
            reference: reference(&engine, &name),
            expected_revision: expected,
            kind: "oauth_grant".into(),
            expires_at_ms: expiry,
            secret,
        },
        &c,
    ))?;
    publish(db, &name, &operation.metadata, false)?;
    Ok(operation.metadata)
}
fn oauth_field(db: &Connection, id: &str, field: &str) -> ApiResult<String> {
    let engine = engine(db)?;
    let name = grant_name(id);
    let flag = AtomicBool::new(false);
    let c = control(&flag);
    let Some(meta) = current(&engine, &name, &c)? else {
        return Ok(String::new());
    };
    if meta.state != CredentialState::Active
        || meta
            .expires_at_ms
            .is_some_and(|v| v <= kageha_credentials::crypto::now_ms())
    {
        return Ok(String::new());
    }
    let lease = run(engine.vault.resolve(&access(&name), &meta.reference, &c))?;
    let value = SecretJson::parse(lease.secret.expose())?;
    Ok(value[if field == "token" {
        "access_token"
    } else {
        "refresh_token"
    }]
    .as_str()
    .unwrap_or("")
    .to_owned())
}
pub(crate) struct OAuthRefresh {
    engine: Arc<Engine>,
    name: String,
    ticket: RefreshTicket,
    grant: SecretJson,
}
pub(crate) fn oauth_refresh_begin(db: &Connection, id: &str) -> ApiResult<OAuthRefresh> {
    let engine = engine(db)?;
    let name = grant_name(id);
    let flag = AtomicBool::new(false);
    let c = control(&flag);
    let meta =
        current(&engine, &name, &c)?.ok_or((409, "Connect this integration first".into()))?;
    let prefix = format!("oauth-refresh:{}:{}", id, meta.revision);
    let prepared = run(engine.vault.begin_refresh(
        &access(&name),
        &meta.reference,
        meta.revision,
        &format!("{prefix}:prepare"),
        &c,
    ))?;
    let (ticket, grant) = run(engine.vault.dispatch_refresh(
        &access(&name),
        &prepared,
        &format!("{prefix}:dispatch"),
        &c,
    ))?;
    let grant = SecretJson::parse(grant.expose())?;
    publish(
        db,
        &name,
        &CredentialMetadata {
            reference: ticket.reference.clone(),
            kind: "oauth_grant".into(),
            revision: ticket.revision,
            authority_generation: ticket.authority_generation,
            expires_at_ms: None,
            state: CredentialState::RefreshDispatching,
            refreshable: false,
        },
        false,
    )?;
    Ok(OAuthRefresh {
        engine,
        name,
        ticket,
        grant,
    })
}
impl OAuthRefresh {
    pub(crate) fn refresh_token(&self) -> &str {
        self.grant["refresh_token"].as_str().unwrap_or("")
    }
}
pub(crate) fn oauth_refresh_finish(
    db: &Connection,
    handle: &OAuthRefresh,
    token: &serde_json::Value,
) -> ApiResult<CredentialMetadata> {
    let (secret, expiry) = token_bundle(token, Some(&handle.grant), false)?;
    let flag = AtomicBool::new(false);
    let result = run(handle.engine.vault.complete_refresh(
        &access(&handle.name),
        &handle.ticket,
        &format!("oauth-complete:{}", handle.ticket.lease_id),
        secret,
        expiry,
        &control(&flag),
    ))?;
    publish(db, &handle.name, &result.metadata, false)?;
    Ok(result.metadata)
}
pub(crate) fn oauth_refresh_failed(db: &Connection, handle: &OAuthRefresh) -> ApiResult<()> {
    let flag = AtomicBool::new(false);
    let result = run(handle.engine.vault.fail_refresh(
        &access(&handle.name),
        &handle.ticket,
        &format!("oauth-uncertain:{}", handle.ticket.lease_id),
        &control(&flag),
    ))?;
    publish(db, &handle.name, &result.metadata, true)
}

fn migrate_oauth(db: &Connection, rows: &[(String, zeroize::Zeroizing<String>)]) -> ApiResult<()> {
    type Tokens = (
        Option<zeroize::Zeroizing<String>>,
        Option<zeroize::Zeroizing<String>>,
    );
    let mut groups: BTreeMap<String, Tokens> = BTreeMap::new();
    for (name, value) in rows {
        if let Some((id, field)) = oauth_alias(name) {
            let entry = groups.entry(id.into()).or_default();
            let value = Some(zeroize::Zeroizing::new(value.to_string()));
            if field == "token" {
                entry.0 = value;
            } else {
                entry.1 = value;
            }
        }
    }
    let names = {
        let mut statement = db
            .prepare("SELECT name FROM ui_credential_refs WHERE name LIKE 'oauth:%'")
            .map_err(db_error)?;
        let names = statement
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)?;
        names
    };
    for name in &names {
        if let Some((id, field)) = oauth_alias(name) {
            let entry = groups.entry(id.into()).or_default();
            if binding(db, name)?.is_some_and(|b| b.revoked) {
                continue;
            }
            if if field == "token" {
                entry.0.is_none()
            } else {
                entry.1.is_none()
            } {
                let value = Some(zeroize::Zeroizing::new(secret_basic(db, name)?));
                if field == "token" {
                    entry.0 = value;
                } else {
                    entry.1 = value;
                }
            }
        }
    }
    for (id, (token, refresh)) in groups {
        let engine = engine(db)?;
        let name = grant_name(&id);
        let flag = AtomicBool::new(false);
        let c = control(&flag);
        let destination = reference(&engine, &name);
        let bundle = serde_json::json!({"access_token":token.as_ref().map(|v|v.as_str()).unwrap_or(""),"refresh_token":refresh.as_ref().map(|v|v.as_str()).unwrap_or("")});
        let mut meta = current(&engine, &name, &c)?;
        if meta.is_none() {
            let info = crate::ui_store::get(db, "integration", &id)
                .map_err(db_error)?
                .unwrap_or_default();
            let expiry = info["expires_at"]
                .as_f64()
                .filter(|v| v.is_finite() && *v >= 0.0)
                .map(|v| (v * 1000.0) as u64);
            let receipt = run(engine.vault.put(
                &access(&name),
                CredentialWrite {
                    operation_id: format!("legacy-grant:{}:{id}", engine.owner),
                    reference: destination.clone(),
                    expected_revision: 0,
                    kind: "oauth_grant".into(),
                    expires_at_ms: expiry,
                    secret: SecretBytes::new(bundle.to_string().into_bytes()),
                },
                &c,
            ))?;
            meta = Some(receipt.metadata);
        }
        let meta = meta.unwrap();
        if meta.kind != "oauth_grant" {
            return Err((409, "OAuth grant migration kind mismatch".into()));
        }
        if meta.state == CredentialState::Active {
            let migration = CredentialAccess {
                principal: "migration".into(),
                purpose: "migration".into(),
                destination: "local".into(),
            };
            let (_, secret) = run(engine.vault.export_record(&migration, &destination, &c))?;
            let value: serde_json::Value = serde_json::from_slice(secret.expose())
                .map_err(|_| (503, "Encrypted OAuth grant is invalid".into()))?;
            if token
                .as_ref()
                .is_some_and(|v| v.as_str() != value["access_token"].as_str().unwrap_or(""))
                || refresh
                    .as_ref()
                    .is_some_and(|v| v.as_str() != value["refresh_token"].as_str().unwrap_or(""))
            {
                return Err((
                    409,
                    "OAuth grant changed during migration; original sources retained".into(),
                ));
            }
        }
        publish(
            db,
            &name,
            &meta,
            matches!(
                meta.state,
                CredentialState::Revoked | CredentialState::Deleted
            ),
        )?;
        for field in ["token", "refresh"] {
            let old = format!("oauth:{id}:{field}");
            if binding(db, &old)?.is_some() {
                delete_basic(db, &old)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod oauth_migration_tests {
    use super::*;
    #[test]
    fn access_and_refresh_are_one_encrypted_authoritative_grant() {
        let db = Connection::open_in_memory().unwrap();
        crate::ui_store::initialize(&db).unwrap();
        initialize(&db).unwrap();
        db.execute(
            "INSERT INTO ui_secrets VALUES('oauth:fixture:token','access-canary')",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO ui_secrets VALUES('oauth:fixture:refresh','refresh-canary')",
            [],
        )
        .unwrap();
        crate::ui_store::put(
            &db,
            "integration",
            "fixture",
            serde_json::json!({"expires_at":1,"refreshable":true}),
        )
        .unwrap();
        migrate(&db).unwrap();
        let meta = oauth_metadata(&db, "fixture").unwrap().unwrap();
        assert_eq!(meta.kind, "oauth_grant");
        assert!(meta.refreshable);
        assert!(oauth_ready(&db, "fixture").unwrap());
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM ui_secrets", [], |r| r
                .get::<_, u64>(0))
                .unwrap(),
            0
        );
        let refresh = oauth_refresh_begin(&db, "fixture").unwrap();
        assert_eq!(refresh.refresh_token(), "refresh-canary");
        let meta = oauth_refresh_finish(
            &db,
            &refresh,
            &serde_json::json!({"access_token":"new-access","expires_in":3600}),
        )
        .unwrap();
        assert!(meta.refreshable);
        assert_eq!(secret(&db, "oauth:fixture:token").unwrap(), "new-access");
        assert_eq!(
            secret(&db, "oauth:fixture:refresh").unwrap(),
            "refresh-canary"
        );
        delete(&db, "oauth:fixture:token").unwrap();
        assert!(!oauth_ready(&db, "fixture").unwrap());
        assert!(secret(&db, "oauth:fixture:refresh").unwrap().is_empty());
    }
    #[test]
    fn pending_verifiers_and_client_secrets_leave_plaintext_history() {
        let db = Connection::open_in_memory().unwrap();
        crate::ui_store::initialize(&db).unwrap();
        initialize(&db).unwrap();
        let pending = serde_json::json!({"state":"csrf-nonce","verifier":"VERIFIER-CANARY","config":{"client_secret":"CLIENT-SECRET-CANARY"},"generation":5,"expires":99999999999.0});
        crate::ui_store::put(
            &db,
            "oauth-generation",
            "fixture",
            serde_json::json!({"value":5}),
        )
        .unwrap();
        crate::ui_store::put(&db, "oauth-pending", "fixture", pending).unwrap();
        migrate(&db).unwrap();
        let recovered = pending_get(&db, "fixture").unwrap().unwrap();
        assert_eq!(recovered["verifier"], "VERIFIER-CANARY");
        let body: String = db
            .query_row(
                "SELECT body FROM ui_documents WHERE kind='oauth-pending'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!body.contains("VERIFIER-CANARY") && !body.contains("CLIENT-SECRET-CANARY"));
        let count:u64=db.query_row("SELECT COUNT(*) FROM ui_document_history WHERE body LIKE '%VERIFIER-CANARY%' OR body LIKE '%CLIENT-SECRET-CANARY%'",[],|r|r.get(0)).unwrap();
        assert_eq!(count, 0);
        assert_eq!(
            recovered["generation"],
            oauth_generation(&db, "fixture").unwrap()
        );
    }
}

/// Existing authenticated settings host invokes only registered, destination-scoped operations.
pub(crate) fn replacement(
    db: &crate::webui::Database,
    value: &serde_json::Value,
) -> ApiResult<serde_json::Value> {
    let engine = {
        let db = db.lock().map_err(db_error)?;
        engine(&db)?
    };
    let destination = value["destination"].as_str().unwrap_or("embedded");
    let access = CredentialAccess {
        principal: "replacement".into(),
        purpose: "replacement".into(),
        destination: destination.into(),
    };
    let id = value["operation_id"].as_str().unwrap_or("");
    let flag = AtomicBool::new(false);
    let c = control(&flag);
    let decision = || {
        serde_json::from_value::<registry::Decision>(value["decision"].clone()).map_err(|_| {
            (
                400,
                "An explicit credential destination decision is required".into(),
            )
        })
    };
    let status = match value["action"].as_str().unwrap_or("binding") {
        "binding" => return serde_json::to_value(run(engine.vault.binding())?).map_err(db_error),
        "request" => run(engine.vault.request(&access, decision()?, id, &c))?,
        "status" => run(engine.vault.status(&access, id, &c))?,
        "resume" => run(engine.vault.resume(&access, id, &c))?,
        "rollback" => run(engine.vault.rollback(
            &access,
            value["previous_operation_id"].as_str().unwrap_or(""),
            decision()?,
            id,
            &c,
        ))?,
        "cancel" => run(engine.vault.cancel(&access, id, &c))?,
        "retire" => run(engine.vault.retire(&access, id, &c))?,
        _ => return Err((400, "Unknown credential replacement action".into())),
    };
    serde_json::to_value(status).map_err(db_error)
}

pub(crate) fn revoke_setting(
    db: &crate::webui::Database,
    value: &serde_json::Value,
) -> ApiResult<serde_json::Value> {
    let name = value["name"]
        .as_str()
        .filter(|n| {
            !n.is_empty()
                && n.len() <= 256
                && !n.chars().any(char::is_control)
                && !n.starts_with("voice:")
        })
        .ok_or((400, "A valid credential setting name is required".into()))?;
    let db = db.lock().map_err(db_error)?;
    delete(&db, name)?;
    Ok(serde_json::json!({"name":name,"status":"revoked"}))
}
#[cfg(test)]
mod dispatch_tests {
    use super::*;
    #[test]
    fn anonymous_catalog_models_resolve_without_opening_a_secret_vault() {
        let connection=Connection::open_in_memory().unwrap();crate::ui_store::initialize(&connection).unwrap();initialize(&connection).unwrap();
        crate::ui_store::put(&connection,"model","anonymous",serde_json::json!({"id":"anonymous","provider":"local","model":"local-model","base_url":"http://127.0.0.1:19999/v1","protocol":"openai_compat"})).unwrap();
        let db=crate::agent_host::DatabaseState::open(connection).unwrap();
        let source=ModelSource{db:db.clone(),id:"anonymous".into(),provider:"local".into(),model:"local-model".into(),protocol:"openai_compat".into(),url:"http://127.0.0.1:19999/v1/chat/completions".into(),destination:"http://127.0.0.1:19999".into()};
        let lease=source.resolve().unwrap();assert!(lease.secret.expose().is_empty());assert!(!engine_exists(&db.lock().unwrap()));
    }
    #[test]
    fn a_revoked_model_key_cannot_become_an_anonymous_request() {
        let db = Connection::open_in_memory().unwrap();
        crate::ui_store::initialize(&db).unwrap();
        initialize(&db).unwrap();
        crate::ui_store::put(&db,"model","fixture",serde_json::json!({"id":"fixture","model":"fixture","provider":"fixture-provider","base_url":"https://provider.example/v1","protocol":"openai_compat"})).unwrap();
        save(&db, "fixture-provider", "DISPOSABLE-MODEL-CANARY").unwrap();
        let db = crate::agent_host::DatabaseState::open(db).unwrap();
        let source = ModelSource {
            db: db.clone(),
            id: "fixture".into(),
            provider: "fixture-provider".into(),
            model: "fixture".into(),
            protocol: "openai_compat".into(),
            url: "https://provider.example/v1/chat/completions".into(),
            destination: "https://provider.example".into(),
        };
        let lease = source.resolve().unwrap();
        assert_eq!(lease.secret.expose(), b"DISPOSABLE-MODEL-CANARY");
        delete(&db.lock().unwrap(), "fixture-provider").unwrap();
        assert!(source.resolve().is_err());
    }
}

#[cfg(test)]
mod projection_tests {
    use super::*;
    #[test]
    fn old_receipts_cannot_undo_a_newer_revocation() {
        let db = Connection::open_in_memory().unwrap();
        crate::ui_store::initialize(&db).unwrap();
        initialize(&db).unwrap();
        save(&db, "fixture", "CANARY").unwrap();
        let engine = engine(&db).unwrap();
        let flag = AtomicBool::new(false);
        let old = current(&engine, "fixture", &control(&flag))
            .unwrap()
            .unwrap();
        delete(&db, "fixture").unwrap();
        assert!(publish(&db, "fixture", &old, false).is_err());
        assert!(binding(&db, "fixture").unwrap().unwrap().revoked);
        assert!(secret(&db, "fixture").unwrap().is_empty());
    }
}

const ACCOUNT_FIELDS: [&str; 4] = ["account_id", "login", "email", "name"];
pub(crate) fn account_metadata(db: &Connection, id: &str) -> ApiResult<serde_json::Value> {
    let raw = crate::ui_store::get(db, "integration", id)
        .map_err(db_error)?
        .unwrap_or_default();
    let Some(reference) = raw["account_ref"].as_str() else {
        return Ok(serde_json::json!({}));
    };
    if !reference.starts_with(&format!("oauth:{id}:account:")) {
        return Err((
            409,
            "Integration account reference does not match its connection".into(),
        ));
    }
    let value = secret_basic(db, reference)?;
    if value.is_empty() {
        return Ok(serde_json::json!({}));
    }
    serde_json::from_str(&value).map_err(|_| (503, "Encrypted account metadata is invalid".into()))
}
pub(crate) fn save_account_metadata(
    db: &Connection,
    id: &str,
    fields: &serde_json::Value,
    revision: u64,
) -> ApiResult<String> {
    let name = format!("oauth:{id}:account:{revision}");
    save(db, &name, &fields.to_string())?;
    let verified = secret_basic(db, &name)?;
    if serde_json::from_str::<serde_json::Value>(&verified).map_err(db_error)? != *fields {
        return Err((409, "Encrypted account metadata verification failed".into()));
    }
    Ok(name)
}
fn migrate_account_metadata(db: &Connection) -> ApiResult<()> {
    // Current documents and historical snapshots keep their IDs/revisions; private fields become encrypted references.
    for history in [false, true] {
        let rows = {
            let sql = if history {
                "SELECT id,revision,body FROM ui_document_history WHERE kind='integration'"
            } else {
                "SELECT id,0,body FROM ui_documents WHERE kind='integration'"
            };
            let mut q = db.prepare(sql).map_err(db_error)?;
            let rows = q
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, u64>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })
                .map_err(db_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(db_error)?;
            rows
        };
        for (id, revision, body) in rows {
            let mut value: serde_json::Value = serde_json::from_str(&body).map_err(db_error)?;
            let mut private = serde_json::Map::new();
            for field in ACCOUNT_FIELDS {
                if let Some(v) = value.as_object_mut().and_then(|v| v.remove(field)) {
                    if !v.is_null() {
                        private.insert(field.into(), v);
                    }
                }
            }
            if private.is_empty() {
                continue;
            }
            let private = serde_json::Value::Object(private);
            let name = format!(
                "oauth:{id}:account:legacy-{}-{revision}",
                if history { "history" } else { "current" }
            );
            let engine = engine(db)?;
            let flag = AtomicBool::new(false);
            let c = control(&flag);
            let meta = current(&engine, &name, &c)?;
            if let Some(meta) = meta {
                if meta.state == CredentialState::Active {
                    let lease = run(engine.vault.resolve(&access(&name), &meta.reference, &c))?;
                    if serde_json::from_slice::<serde_json::Value>(lease.secret.expose())
                        .map_err(db_error)?
                        != private
                    {
                        return Err((
                            409,
                            "Legacy account metadata conflicts with the encrypted record".into(),
                        ));
                    }
                    publish(db, &name, &meta, false)?;
                }
            } else {
                let operation = run(engine.vault.put(
                    &access(&name),
                    CredentialWrite {
                        operation_id: format!("legacy-account:{}:{name}", engine.owner),
                        reference: reference(&engine, &name),
                        expected_revision: 0,
                        kind: "oauth_account".into(),
                        expires_at_ms: None,
                        secret: SecretBytes::new(private.to_string().into_bytes()),
                    },
                    &c,
                ))?;
                let lease =
                    run(engine
                        .vault
                        .resolve(&access(&name), &operation.metadata.reference, &c))?;
                if serde_json::from_slice::<serde_json::Value>(lease.secret.expose())
                    .map_err(db_error)?
                    != private
                {
                    return Err((409, "Legacy account encryption verification failed".into()));
                }
                publish(db, &name, &operation.metadata, false)?;
            }
            value["account_ref"] = serde_json::json!(name);
            if history {
                db.execute("UPDATE ui_document_history SET body=? WHERE kind='integration' AND id=? AND revision=?",params![value.to_string(),id,revision]).map_err(db_error)?;
            } else {
                db.execute(
                    "UPDATE ui_documents SET body=? WHERE kind='integration' AND id=?",
                    params![value.to_string(), id],
                )
                .map_err(db_error)?;
            }
        }
    }
    Ok(())
}
#[cfg(test)]
mod account_metadata_tests {
    use super::*;
    #[test]
    fn current_and_historical_account_details_are_encrypted_without_losing_revisions() {
        let db = Connection::open_in_memory().unwrap();
        crate::ui_store::initialize(&db).unwrap();
        initialize(&db).unwrap();
        crate::ui_store::put(&db,"integration","gmail",serde_json::json!({"account_id":"ACCOUNT-CANARY","email":"private-canary@example.test","agents":["main"]})).unwrap();
        let before: u64 = db
            .query_row("SELECT COUNT(*) FROM ui_document_history", [], |r| r.get(0))
            .unwrap();
        migrate(&db).unwrap();
        let bodies:String=db.query_row("SELECT group_concat(body) FROM (SELECT body FROM ui_documents UNION ALL SELECT body FROM ui_document_history)",[],|r|r.get(0)).unwrap();
        assert!(!bodies.contains("ACCOUNT-CANARY"));
        assert!(!bodies.contains("private-canary@example.test"));
        assert_eq!(
            account_metadata(&db, "gmail").unwrap()["account_id"],
            "ACCOUNT-CANARY"
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM ui_document_history", [], |r| r
                .get::<_, u64>(0))
                .unwrap(),
            before
        );
        migrate(&db).unwrap();
    }
}

pub(crate) fn oauth_access_with_revision(db: &Connection, id: &str) -> ApiResult<(u64, String)> {
    let engine = engine(db)?;
    let name = grant_name(id);
    let flag = AtomicBool::new(false);
    let lease =
        run(engine
            .vault
            .resolve(&access(&name), &reference(&engine, &name), &control(&flag)))?;
    let value = SecretJson::parse(lease.secret.expose())?;
    let token = value["access_token"]
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or((409, "OAuth access token is unavailable".into()))?
        .to_owned();
    Ok((lease.metadata.revision, token))
}

/// Opaque authorization identity changes on reconnect, and survives refresh/key/storage rotation.
pub(crate) fn oauth_authority(db: &Connection, id: &str) -> ApiResult<Option<String>> {
    if binding(db, &grant_name(id))?.is_none() && !engine_exists(db) {
        return Ok(None);
    }
    let engine = engine(db)?;
    let name = grant_name(id);
    let flag = AtomicBool::new(false);
    let c = control(&flag);
    let Some(meta) = current(&engine, &name, &c)? else {
        return Ok(None);
    };
    if meta.state != CredentialState::Active {
        return Ok(None);
    }
    let (_, secret) = run(engine
        .vault
        .export_record(&access(&name), &meta.reference, &c))?;
    let value = SecretJson::parse(secret.expose())?;
    Ok(Some(
        value["authorization_id"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("legacy:{}:{id}", engine.owner)),
    ))
}

/// Private compound credential data: redacted/non-serializable and scrubbed on drop.
pub(crate) struct SecretJson(serde_json::Value);
impl SecretJson {
    pub(crate) fn new(value: serde_json::Value) -> Self {
        Self(value)
    }
    fn parse(bytes: &[u8]) -> ApiResult<Self> {
        serde_json::from_slice(bytes)
            .map(Self)
            .map_err(|_| (503, "Credential payload is invalid".into()))
    }
}
impl std::ops::Deref for SecretJson {
    type Target = serde_json::Value;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl Drop for SecretJson {
    fn drop(&mut self) {
        fn clear(v: &mut serde_json::Value) {
            match v {
                serde_json::Value::String(s) => {
                    use zeroize::Zeroize;
                    s.zeroize();
                }
                serde_json::Value::Object(v) => v.values_mut().for_each(clear),
                serde_json::Value::Array(v) => v.iter_mut().for_each(clear),
                _ => {}
            }
        }
        clear(&mut self.0);
    }
}

pub(crate) fn usable_or_anonymous(db: &Connection, name: &str) -> bool {
    let result = (|| -> ApiResult<bool> {
        let published = binding(db, name)?;
        if let Some(published) = &published {
            if published.revoked {
                return Ok(false);
            }
        }
        if published.is_none() && !engine_exists(db) {
            return Ok(true);
        }
        let engine = engine(db)?;
        let flag = AtomicBool::new(false);
        let meta = current(&engine, name, &control(&flag))?;
        Ok(match (published, meta) {
            (None, None) => true,
            (Some(reference), Some(meta)) => {
                reference.revision == meta.revision
                    && meta.state == CredentialState::Active
                    && meta
                        .expires_at_ms
                        .is_none_or(|v| v > kageha_credentials::crypto::now_ms())
            }
            _ => false,
        })
    })();
    result.unwrap_or(false)
}
fn import_private_value(
    db: &Connection,
    name: &str,
    kind: &str,
    value: &serde_json::Value,
) -> ApiResult<()> {
    let engine = engine(db)?;
    let flag = AtomicBool::new(false);
    let c = control(&flag);
    if let Some(meta) = current(&engine, name, &c)? {
        if matches!(
            meta.state,
            CredentialState::Revoked | CredentialState::Deleted
        ) {
            return Ok(());
        }
        if meta.state != CredentialState::Active {
            return Err((
                409,
                "Resolve the current credential operation before importing private settings".into(),
            ));
        }
        let secret = run(engine.vault.resolve(&access(name), &meta.reference, &c))?;
        let stored = SecretJson::parse(secret.secret.expose())?;
        if stored.0 != *value {
            return Err((
                409,
                "Legacy private settings conflict with encrypted state".into(),
            ));
        }
        return publish(db, name, &meta, false);
    }
    let operation = run(engine.vault.put(
        &access(name),
        CredentialWrite {
            operation_id: format!("legacy-private:{}:{name}", engine.owner),
            reference: reference(&engine, name),
            expected_revision: 0,
            kind: kind.into(),
            expires_at_ms: None,
            secret: SecretBytes::new(serde_json::to_vec(value).map_err(db_error)?),
        },
        &c,
    ))?;
    let stored = run(engine
        .vault
        .resolve(&access(name), &operation.metadata.reference, &c))?;
    if SecretJson::parse(stored.secret.expose())?.0 != *value {
        return Err((409, "Encrypted private settings verification failed".into()));
    }
    publish(db, name, &operation.metadata, false)
}
fn migrate_mcp_private_metadata(db: &Connection) -> ApiResult<()> {
    for history in [false, true] {
        let rows = {
            let sql = if history {
                "SELECT id,revision,body FROM ui_document_history WHERE kind='config:mcp:servers'"
            } else {
                "SELECT id,0,body FROM ui_documents WHERE kind='config:mcp:servers'"
            };
            let mut query = db.prepare(sql).map_err(db_error)?;
            let rows = query
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, u64>(1)?,
                        zeroize::Zeroizing::new(r.get::<_, String>(2)?),
                    ))
                })
                .map_err(db_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(db_error)?;
            rows
        };
        for (id, revision, body) in rows {
            let mut value: serde_json::Value = serde_json::from_str(&body).map_err(db_error)?;
            let name = value["name"].as_str().unwrap_or(&id).to_owned();
            let mut changed = false;
            for field in ["env", "headers"] {
                if value[field].is_null() {
                    continue;
                }
                let private = SecretJson::new(value[field].take());
                let reference = if history {
                    format!("mcp:{name}:{field}:history:{revision}")
                } else {
                    format!("mcp:{name}:{field}")
                };
                import_private_value(db, &reference, "mcp", &private)?;
                value["credential_refs"][field] = serde_json::json!(reference);
                changed = true;
            }
            if changed {
                if history {
                    db.execute("UPDATE ui_document_history SET body=? WHERE kind='config:mcp:servers' AND id=? AND revision=?",params![value.to_string(),id,revision]).map_err(db_error)?;
                } else {
                    db.execute(
                        "UPDATE ui_documents SET body=? WHERE kind='config:mcp:servers' AND id=?",
                        params![value.to_string(), id],
                    )
                    .map_err(db_error)?;
                }
            }
        }
    }
    Ok(())
}
#[cfg(test)]
mod mcp_migration_tests {
    use super::*;
    #[test]
    fn legacy_mcp_secret_fields_are_encrypted_but_server_configuration_stays_usable() {
        let db = Connection::open_in_memory().unwrap();
        crate::ui_store::initialize(&db).unwrap();
        initialize(&db).unwrap();
        crate::ui_store::put(&db,"config:mcp:servers","fixture",serde_json::json!({"id":"fixture","name":"fixture","command":"server","args":["--stdio"],"env":{"API_KEY":"MCP-CANARY"},"headers":{"Authorization":"Bearer HEADER-CANARY"}})).unwrap();
        migrate(&db).unwrap();
        let public = crate::ui_store::get(&db, "config:mcp:servers", "fixture")
            .unwrap()
            .unwrap();
        assert_eq!(public["command"], "server");
        assert!(public["env"].is_null());
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&secret(&db, "mcp:fixture:env").unwrap())
                .unwrap()["API_KEY"],
            "MCP-CANARY"
        );
        let text:String=db.query_row("SELECT group_concat(body) FROM (SELECT body FROM ui_documents UNION ALL SELECT body FROM ui_document_history)",[],|r|r.get(0)).unwrap();
        assert!(!text.contains("MCP-CANARY"));
        assert!(!text.contains("HEADER-CANARY"));
        migrate(&db).unwrap();
    }
}

/// Read-only readiness can fail closed without showing an OS prompt.
pub(crate) fn keychain_status(db: &Connection) -> serde_json::Value {
    let supported = cfg!(target_os="macos") && std::env::var_os("KAGEHA_CREDENTIAL_KEY_FILE").is_none();
    serde_json::json!({"can_authorize":supported,"locked":supported && engine_exists(db) && engine(db).is_err()})
}
pub(crate) fn authorize_keychain(db: &crate::webui::Database) -> ApiResult<()> {
    #[cfg(not(target_os="macos"))]
    { let _=db; Err((400,"Keychain authorization is only available on macOS".into())) }
    #[cfg(target_os="macos")]
    {
        if std::env::var_os("KAGEHA_CREDENTIAL_KEY_FILE").is_some() {return Err((400,"This deployment uses an operator-provided credential key".into()));}
        let owner: String = {
            let connection=db.lock().map_err(db_error)?;
            connection.query_row("SELECT owner FROM ui_credential_owner WHERE singleton=1",[],|row|row.get(0)).map_err(db_error)?
        };
        let key_id=format!("master-{:x}",Sha256::digest(owner.as_bytes()));
        // No database/cache mutex is held while the user answers macOS.
        let key=run(keys::MacKeychain{service:"com.kageha.credentials".into()}.authorize(&key_id))?;
        let connection=db.lock().map_err(db_error)?;
        let current:String=connection.query_row("SELECT owner FROM ui_credential_owner WHERE singleton=1",[],|row|row.get(0)).map_err(db_error)?;
        if current!=owner {return Err((409,"Credential owner changed during authorization".into()));}
        engine_with_key(&connection,Some(key))?;
        Ok(())
    }
}
