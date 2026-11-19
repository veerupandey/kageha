//! Device-local password adapter over the independent encrypted credential library.
//! Only opaque references leave this module; browser authority checks remain at the caller.
use crate::cdp::Result;
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    XChaCha20Poly1305, XNonce,
};
use kageha_contracts::{execution::Control, ErrorCode};
#[cfg(target_os = "macos")]
use kageha_credentials::CredentialRepository;
use kageha_credentials::{
    keys, repository::SqliteRepository, CredentialAccess, CredentialRef, CredentialWrite, Grant,
    KeyProtector, Permission, SecretBytes, Vault as SharedVault,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    future::Future,
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{atomic::AtomicBool, Arc, OnceLock},
};
use zeroize::Zeroizing;

// A user-authorized Keychain read stays in this daemon only, never in RPC replies.
// This also supports macOS's one-time authorization, which is process-scoped.
fn authorized_keys() -> &'static std::sync::Mutex<BTreeMap<PathBuf, SecretBytes>> {
    static KEYS: OnceLock<std::sync::Mutex<BTreeMap<PathBuf, SecretBytes>>> = OnceLock::new();
    KEYS.get_or_init(Default::default)
}
fn cached_key(root: &Path) -> Result<Option<SecretBytes>> {
    Ok(authorized_keys().lock().map_err(|_| "credential_store_unavailable")?
        .get(root).map(|key| SecretBytes::new(key.expose().to_vec())))
}
fn browser_key_id(root: &Path) -> String {
    format!("browser-{}", data_encoding::HEXLOWER.encode(&Sha256::digest(root.to_string_lossy().as_bytes())))
}
#[cfg(target_os = "macos")]
fn background_keychain_keys(repository: &SqliteRepository) -> Result<Arc<dyn KeyProtector>> {
    // Provisioning can display OS UI. Only the explicit settings action may
    // initialize a new browser vault or create its Keychain entry.
    if run(repository.header())?.is_none() { return Err("credential_store_locked"); }
    Ok(Arc::new(keys::MacKeychain { service: "com.kageha.credentials.browser".into() }))
}
fn external_credentials_configured() -> bool {
    std::env::var_os("KAGEHA_BROWSER_CREDENTIAL_SERVICE").is_some()
        || std::env::var_os("KAGEHA_BROWSER_CREDENTIAL_KEY_FILE").is_some()
        || std::env::var_os("KAGEHA_CREDENTIAL_KEY_FILE").is_some()
}

/// Verify a key against the encrypted vault header before retaining it. This
/// function may initialize a new vault and is called only after user action.
fn retain_authorized_key(root: &Path, key: SecretBytes) -> Result<()> {
    let key_id = browser_key_id(root);
    let repository = Arc::new(SqliteRepository::open(&root.join("shared-vault.sqlite"))
        .map_err(|_| "credential_store_unavailable")?);
    let keys: Arc<dyn KeyProtector> = Arc::new(keys::SuppliedKey::new(&key_id, SecretBytes::new(key.expose().to_vec())));
    let cancelled = AtomicBool::new(false);
    run(SharedVault::open(repository, keys, &key_id, vec![], &control(&cancelled)))?;
    authorized_keys().lock().map_err(|_| "credential_store_unavailable")?.insert(root.to_owned(), key);
    Ok(())
}

/// Noninteractive status: no directory, key, or vault is created.
pub fn credential_vault_status() -> Result<serde_json::Value> {
    use serde_json::json;
    let Some(root) = std::env::var_os("KAGEHA_BROWSER_CREDENTIAL_DIR") else {
        return Ok(json!({"configured":false,"can_authorize":false,"state":"not_configured"}));
    };
    if external_credentials_configured() {
        return Ok(json!({"configured":true,"can_authorize":false,"state":"external"}));
    }
    #[cfg(not(target_os = "macos"))]
    { let _ = root; return Ok(json!({"configured":true,"can_authorize":false,"state":"external"})); }
    #[cfg(target_os = "macos")]
    {
        let root = PathBuf::from(root);
        if !root.is_absolute() { return Err("credential_store_not_private"); }
        if !root.exists() { return Ok(json!({"configured":true,"can_authorize":true,"state":"not_initialized"})); }
        private(&root, true)?;
        let root = root.canonicalize().map_err(|_| "credential_store_unavailable")?;
        let path = root.join("shared-vault.sqlite");
        if !path.exists() { return Ok(json!({"configured":true,"can_authorize":true,"state":"not_initialized"})); }
        let repository = Arc::new(SqliteRepository::open(&path).map_err(|_| "credential_store_unavailable")?);
        if run(repository.header())?.is_none() {
            return Ok(json!({"configured":true,"can_authorize":true,"state":"not_initialized"}));
        }
        let key_id = browser_key_id(&root);
        let cancelled = AtomicBool::new(false);
        let key = match cached_key(&root)? {
            Some(key) => Ok(key),
            None => run(keys::MacKeychain { service: "com.kageha.credentials.browser".into() }.unlock(&key_id, &control(&cancelled))),
        };
        let ready = key.and_then(|key| {
            let keys: Arc<dyn KeyProtector> = Arc::new(keys::SuppliedKey::new(&key_id, key));
            run(SharedVault::open(repository, keys, &key_id, vec![], &control(&cancelled))).map(|_| ())
        }).is_ok();
        Ok(json!({"configured":true,"can_authorize":true,"state":if ready {"ready"} else {"locked"}}))
    }
}

/// Interactive Keychain authorization, invoked only from the authenticated UI.
pub fn credential_vault_authorize() -> Result<serde_json::Value> {
    #[cfg(not(target_os = "macos"))]
    { return Err("credential_authorization_unavailable"); }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::DirBuilderExt;
        if external_credentials_configured() {
            return Err("credential_authorization_unavailable");
        }
        let root = PathBuf::from(std::env::var_os("KAGEHA_BROWSER_CREDENTIAL_DIR").ok_or("credential_store_not_configured")?);
        if !root.is_absolute() { return Err("credential_store_not_private"); }
        if !root.exists() { fs::DirBuilder::new().mode(0o700).create(&root).map_err(|_| "credential_store_unavailable")?; }
        private(&root, true)?;
        let root = root.canonicalize().map_err(|_| "credential_store_unavailable")?;
        let key_id = browser_key_id(&root);
        let repository = SqliteRepository::open(&root.join("shared-vault.sqlite")).map_err(|_| "credential_store_unavailable")?;
        let initialized = run(repository.header())?.is_some();
        let keychain = keys::MacKeychain { service: "com.kageha.credentials.browser".into() };
        if !initialized { run(keychain.provision(&key_id))?; }
        let key = run(keychain.authorize(&key_id))?;
        retain_authorized_key(&root, key)?;
        Ok(serde_json::json!({"authorized":true}))
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    workspace: String,
    origin: String,
    account: String,
    password: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rotation: Option<Rotation>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Rotation {
    candidate: String,
    base_revision: u64,
}
impl Drop for Entry {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.password.zeroize();
    }
}
pub fn origin(value: &str) -> Result<String> {
    let url = url::Url::parse(value).map_err(|_| "credential_origin_invalid")?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("credential_origin_invalid");
    }
    Ok(url.origin().ascii_serialization())
}
fn private(path: &Path, directory: bool) -> Result<()> {
    let meta = fs::symlink_metadata(path).map_err(|_| "credential_store_unavailable")?;
    if meta.uid() != crate::state::uid()
        || meta.mode() & 0o077 != 0
        || if directory {
            !meta.is_dir()
        } else {
            !meta.is_file()
        }
    {
        return Err("credential_store_not_private");
    }
    Ok(())
}
fn read(path: &Path) -> Result<Zeroizing<Vec<u8>>> {
    private(path, false)?;
    let mut bytes = Zeroizing::new(Vec::new());
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .and_then(|f| f.take(16385).read_to_end(&mut bytes))
        .map_err(|_| "credential_store_unavailable")?;
    if bytes.len() > 16384 {
        return Err("credential_store_invalid");
    }
    Ok(bytes)
}
fn run<T: Send, F: Future<Output = kageha_contracts::Result<T>> + Send>(future: F) -> Result<T> {
    static RUNTIME: OnceLock<std::result::Result<tokio::runtime::Runtime, ()>> = OnceLock::new();
    let runtime = RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(|_| ())
        })
        .as_ref()
        .map_err(|_| "credential_store_unavailable")?;
    let result = if tokio::runtime::Handle::try_current().is_ok() {
        std::thread::scope(|scope| scope.spawn(|| runtime.block_on(future)).join())
            .map_err(|_| "credential_store_unavailable")?
    } else {
        runtime.block_on(future)
    };
    result.map_err(|error| match error.code {
        ErrorCode::Unauthorized | ErrorCode::VerificationFailed => "credential_scope_mismatch",
        ErrorCode::Conflict | ErrorCode::StaleGeneration => "credential_store_changed",
        ErrorCode::NotFound => "credential_reference_invalid",
        _ => "credential_store_locked",
    })
}
fn control(flag: &AtomicBool) -> Control<'_> {
    Control {
        cancelled: flag,
        deadline: Some(std::time::Instant::now() + std::time::Duration::from_secs(20)),
    }
}
enum Storage {
    Embedded {
        repository: Arc<SqliteRepository>,
        keys: Arc<dyn KeyProtector>,
        key_id: String,
    },
    Service(Arc<dyn kageha_credentials::CredentialStore>),
}
pub struct Vault {
    root: PathBuf,
    storage: Storage,
}
impl Vault {
    pub fn configured() -> Result<Self> {
        let root = std::env::var_os("KAGEHA_BROWSER_CREDENTIAL_DIR")
            .ok_or("credential_store_not_configured")?;
        Self::open(Path::new(&root))
    }
    fn open(root: &Path) -> Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        if !root.is_absolute() {
            return Err("credential_store_not_private");
        }
        if !root.exists() {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(root)
                .map_err(|_| "credential_store_unavailable")?;
        }
        private(root, true)?;
        let root = root
            .canonicalize()
            .map_err(|_| "credential_store_unavailable")?;
        #[cfg(not(test))]
        if let Some(path) = std::env::var_os("KAGEHA_BROWSER_CREDENTIAL_SERVICE") {
            let bytes = kageha_credentials::config::protected_input(Path::new(&path), 65536)
                .map_err(|_| "credential_service_config_invalid")?;
            let connection: kageha_credentials::config::Connection =
                serde_json::from_slice(bytes.expose())
                    .map_err(|_| "credential_service_config_invalid")?;
            if !matches!(
                connection,
                kageha_credentials::config::Connection::Unix { .. }
            ) {
                return Err("credential_browser_service_must_be_device_local");
            }
            let client = connection
                .connect()
                .map_err(|_| "credential_service_unavailable")?;
            return Ok(Self {
                root,
                storage: Storage::Service(Arc::new(client)),
            });
        }
        let repository = Arc::new(
            SqliteRepository::open(&root.join("shared-vault.sqlite"))
                .map_err(|_| "credential_store_unavailable")?,
        );
        let key_id = browser_key_id(&root);
        let keys: Arc<dyn KeyProtector> = if let Some(path) =
            std::env::var_os("KAGEHA_BROWSER_CREDENTIAL_KEY_FILE")
                .or_else(|| std::env::var_os("KAGEHA_CREDENTIAL_KEY_FILE"))
        {
            Arc::new(keys::FileKeys(BTreeMap::from([(
                key_id.clone(),
                PathBuf::from(path),
            )])))
        } else if let Some(key) = cached_key(&root)? {
            Arc::new(keys::SuppliedKey::new(&key_id, key))
        } else {
            #[cfg(test)]
            {
                static KEYS: OnceLock<std::sync::Mutex<BTreeMap<PathBuf, Zeroizing<Vec<u8>>>>> =
                    OnceLock::new();
                let mut keys = KEYS
                    .get_or_init(Default::default)
                    .lock()
                    .map_err(|_| "credential_store_unavailable")?;
                let key = if let Some(key) = keys.get(&root) {
                    key.to_vec()
                } else {
                    let key = kageha_credentials::crypto::random::<32>()
                        .map_err(|_| "credential_random_unavailable")?
                        .to_vec();
                    keys.insert(root.clone(), Zeroizing::new(key.clone()));
                    key
                };
                Arc::new(keys::SuppliedKey::new(&key_id, SecretBytes::new(key)))
            }
            #[cfg(all(not(test), target_os = "macos"))]
            {
                background_keychain_keys(&repository)?
            }
            #[cfg(all(not(test), not(target_os = "macos")))]
            {
                return Err("credential_store_key_required");
            }
        };
        Ok(Self {
            root,
            storage: Storage::Embedded {
                repository,
                keys,
                key_id,
            },
        })
    }
    fn path(&self, handle: &str) -> Result<PathBuf> {
        if handle.len() != 69
            || !handle.starts_with("cred_")
            || !handle[5..].bytes().all(|v| v.is_ascii_hexdigit())
        {
            return Err("credential_reference_invalid");
        }
        Ok(self.root.join(handle))
    }
    fn namespace(workspace: &str, site: &str) -> String {
        let scope = serde_json::to_vec(&(workspace, site)).unwrap();
        format!(
            "browser:{}",
            data_encoding::HEXLOWER.encode(&Sha256::digest(scope))
        )
    }
    fn access(site: &str) -> CredentialAccess {
        CredentialAccess {
            principal: "browser-device".into(),
            purpose: "password_fill".into(),
            destination: site.into(),
        }
    }
    fn shared(
        &self,
        workspace: &str,
        site: &str,
    ) -> Result<Arc<dyn kageha_credentials::CredentialStore>> {
        let Storage::Embedded {
            repository,
            keys,
            key_id,
        } = &self.storage
        else {
            if let Storage::Service(store) = &self.storage {
                return Ok(store.clone());
            }
            unreachable!()
        };
        let namespace = Self::namespace(workspace, site);
        let grant = Grant {
            principal: "browser-device".into(),
            namespace,
            ids: BTreeSet::from(["*".into()]),
            purposes: BTreeSet::from(["password_fill".into()]),
            destinations: BTreeSet::from([site.into()]),
            permissions: BTreeSet::from([
                Permission::Metadata,
                Permission::Resolve,
                Permission::Write,
            ]),
        };
        let flag = AtomicBool::new(false);
        run(SharedVault::open(
            repository.clone(),
            keys.clone(),
            key_id,
            vec![grant],
            &control(&flag),
        ))
        .map(|v| Arc::new(v) as Arc<dyn kageha_credentials::CredentialStore>)
    }
    fn reference(workspace: &str, site: &str, handle: &str) -> CredentialRef {
        CredentialRef {
            namespace: Self::namespace(workspace, site),
            id: handle.into(),
        }
    }
    pub fn generate(&self, workspace: &str, site: &str, account: &str) -> Result<String> {
        let site = origin(site)?;
        if account.trim().is_empty() || account.len() > 320 || account.chars().any(char::is_control)
        {
            return Err("credential_account_invalid");
        }
        let scope = serde_json::to_vec(&(workspace, &site, account)).unwrap();
        let handle = format!(
            "cred_{}",
            data_encoding::HEXLOWER.encode(&Sha256::digest(scope))
        );
        self.path(&handle)?;
        if self.repository_has(workspace, &site, &handle)? || self.path(&handle)?.exists() {
            self.resolve(workspace, &site, &handle)?;
            return Ok(handle);
        }
        let password = format!(
            "Aa1!{}",
            data_encoding::BASE64URL_NOPAD.encode(
                &kageha_credentials::crypto::random::<24>()
                    .map_err(|_| "credential_random_unavailable")?
            )
        );
        let entry = Entry {
            workspace: workspace.into(),
            origin: site.clone(),
            account: account.into(),
            password,
            rotation: None,
        };
        let shared = self.shared(workspace, &site)?;
        let flag = AtomicBool::new(false);
        let c = control(&flag);
        let result = run(shared.put(
            &Self::access(&site),
            CredentialWrite {
                operation_id: format!("browser-generate:{handle}"),
                reference: Self::reference(workspace, &site, &handle),
                expected_revision: 0,
                kind: "browser_password".into(),
                expires_at_ms: None,
                secret: SecretBytes::new(
                    serde_json::to_vec(&entry).map_err(|_| "credential_store_invalid")?,
                ),
            },
            &c,
        ));
        match result {
            Ok(_) => {}
            Err("credential_store_changed") => {
                self.resolve(workspace, &site, &handle)?;
            }
            Err(error) => return Err(error),
        }
        Ok(handle)
    }
    /// Stage a new secret without replacing the active one. A reset can fail or have an
    /// uncertain outcome, so the candidate stays durable until explicitly committed.
    pub fn stage_rotation(&self, workspace: &str, site: &str, account: &str) -> Result<String> {
        let site = origin(site)?;
        if account.trim().is_empty() || account.len() > 320 || account.chars().any(char::is_control) {
            return Err("credential_account_invalid");
        }
        let active = format!("cred_{}", data_encoding::HEXLOWER.encode(&Sha256::digest(
            serde_json::to_vec(&(workspace, &site, account)).unwrap()
        )));
        if self.path(&active)?.exists() { self.import_legacy(workspace, &site, &active)?; }
        let shared = self.shared(workspace, &site)?;
        let flag = AtomicBool::new(false);
        let c = control(&flag);
        let base_revision = match run(shared.metadata(&Self::access(&site), &Self::reference(workspace, &site, &active), &c)) {
            Ok(metadata) => metadata.revision,
            Err("credential_reference_invalid") => 0,
            Err(error) => return Err(error),
        };
        let random = kageha_credentials::crypto::random::<32>().map_err(|_| "credential_random_unavailable")?;
        let candidate = format!("cred_{}", data_encoding::HEXLOWER.encode(&random));
        let password = format!("Aa1!{}", data_encoding::BASE64URL_NOPAD.encode(
            &kageha_credentials::crypto::random::<24>().map_err(|_| "credential_random_unavailable")?
        ));
        let entry = Entry { workspace: workspace.into(), origin: site.clone(), account: account.into(), password,
            rotation: Some(Rotation { candidate: candidate.clone(), base_revision }) };
        run(shared.put(&Self::access(&site), CredentialWrite {
            operation_id: format!("browser-stage:{candidate}"),
            reference: Self::reference(workspace, &site, &candidate), expected_revision: 0,
            kind: "browser_password_candidate".into(), expires_at_ms: None,
            secret: SecretBytes::new(serde_json::to_vec(&entry).map_err(|_| "credential_store_invalid")?),
        }, &c))?;
        Ok(candidate)
    }

    /// Promote only the candidate based on the active revision observed at staging.
    /// Repeated commits of the same candidate are idempotent in the credential store.
    pub fn commit_rotation(&self, workspace: &str, site: &str, account: &str, candidate: &str) -> Result<String> {
        let site = origin(site)?;
        self.path(candidate)?;
        let shared = self.shared(workspace, &site)?;
        let flag = AtomicBool::new(false);
        let c = control(&flag);
        let lease = run(shared.resolve(&Self::access(&site), &Self::reference(workspace, &site, candidate), &c))?;
        let mut entry: Entry = serde_json::from_slice(lease.secret.expose()).map_err(|_| "credential_store_invalid")?;
        let rotation = entry.rotation.take().ok_or("credential_reference_invalid")?;
        if rotation.candidate != candidate || entry.workspace != workspace || entry.origin != site || entry.account != account {
            return Err("credential_scope_mismatch");
        }
        let active = format!("cred_{}", data_encoding::HEXLOWER.encode(&Sha256::digest(
            serde_json::to_vec(&(workspace, &site, account)).unwrap()
        )));
        run(shared.put(&Self::access(&site), CredentialWrite {
            operation_id: format!("browser-commit:{candidate}"),
            reference: Self::reference(workspace, &site, &active), expected_revision: rotation.base_revision,
            kind: "browser_password".into(), expires_at_ms: None,
            secret: SecretBytes::new(serde_json::to_vec(&entry).map_err(|_| "credential_store_invalid")?),
        }, &c))?;
        // An idempotent retry may return an earlier receipt after a later rotation.
        // Report success only while this candidate is still the active password.
        let current = self.resolve(workspace, &site, &active)?;
        if *current != entry.password { return Err("credential_store_changed"); }
        Ok(active)
    }
    fn repository_has(&self, workspace: &str, site: &str, handle: &str) -> Result<bool> {
        let flag = AtomicBool::new(false);
        let shared = self.shared(workspace, site)?;
        match run(shared.metadata(
            &Self::access(site),
            &Self::reference(workspace, site, handle),
            &control(&flag),
        )) {
            Ok(_) => Ok(true),
            Err("credential_reference_invalid") => Ok(false),
            Err(error) => Err(error),
        }
    }
    fn import_legacy(&self, workspace: &str, site: &str, handle: &str) -> Result<()> {
        if matches!(self.storage, Storage::Service(_)) {
            return Err("credential_legacy_transfer_requires_destination_approval");
        }
        let source = self.path(handle)?;
        let bytes = read(&source)?;
        let key = read(&self.root.join("key"))?;
        if key.len() != 32 || bytes.len() < 40 {
            return Err("credential_store_invalid");
        }
        let cipher =
            XChaCha20Poly1305::new_from_slice(&key).map_err(|_| "credential_store_invalid")?;
        let aad = serde_json::to_vec(&(workspace, site, handle)).unwrap();
        let plaintext = SecretBytes::new(
            cipher
                .decrypt(
                    XNonce::from_slice(&bytes[..24]),
                    Payload {
                        msg: &bytes[24..],
                        aad: &aad,
                    },
                )
                .map_err(|_| "credential_scope_mismatch")?,
        );
        let entry: Entry =
            serde_json::from_slice(plaintext.expose()).map_err(|_| "credential_store_invalid")?;
        if entry.workspace != workspace || entry.origin != site {
            return Err("credential_scope_mismatch");
        }
        let shared = self.shared(workspace, site)?;
        let flag = AtomicBool::new(false);
        let c = control(&flag);
        let reference = Self::reference(workspace, site, handle);
        run(shared.put(
            &Self::access(site),
            CredentialWrite {
                operation_id: format!("legacy-browser:{handle}"),
                reference: reference.clone(),
                expected_revision: 0,
                kind: "browser_password".into(),
                expires_at_ms: None,
                secret: SecretBytes::new(plaintext.expose().to_vec()),
            },
            &c,
        ))?;
        let lease = run(shared.resolve(&Self::access(site), &reference, &c))?;
        if lease.secret.expose() != plaintext.expose() {
            return Err("credential_store_invalid");
        }
        fs::remove_file(source).map_err(|_| "credential_store_unavailable")?;
        self.retire_legacy_key()?;
        Ok(())
    }
    fn retire_legacy_key(&self) -> Result<()> {
        let key = self.root.join("key");
        if !key.exists() {
            return Ok(());
        }
        let remaining = fs::read_dir(&self.root)
            .map_err(|_| "credential_store_unavailable")?
            .any(|entry| {
                entry.is_ok_and(|entry| entry.file_name().to_string_lossy().starts_with("cred_"))
            });
        if !remaining {
            private(&key, false)?;
            fs::remove_file(key).map_err(|_| "credential_store_unavailable")?;
            std::fs::File::open(&self.root)
                .and_then(|f| f.sync_all())
                .map_err(|_| "credential_store_unavailable")?;
        }
        Ok(())
    }
    pub fn resolve(&self, workspace: &str, site: &str, handle: &str) -> Result<Zeroizing<String>> {
        let site = origin(site)?;
        self.path(handle)?;
        if self.path(handle)?.exists() {
            self.import_legacy(workspace, &site, handle)?;
        }
        let shared = self.shared(workspace, &site)?;
        let flag = AtomicBool::new(false);
        let lease = run(shared.resolve(
            &Self::access(&site),
            &Self::reference(workspace, &site, handle),
            &control(&flag),
        ))?;
        let mut entry: Entry = serde_json::from_slice(lease.secret.expose())
            .map_err(|_| "credential_store_invalid")?;
        let expected = format!(
            "cred_{}",
            data_encoding::HEXLOWER.encode(&Sha256::digest(
                serde_json::to_vec(&(workspace, &site, &entry.account))
                    .map_err(|_| "credential_store_invalid")?
            ))
        );
        let valid_handle = if let Some(rotation) = &entry.rotation {
            rotation.candidate == handle
        } else {
            expected == handle
        };
        if !valid_handle || entry.workspace != workspace || entry.origin != site {
            return Err("credential_scope_mismatch");
        }
        self.retire_legacy_key()?;
        Ok(Zeroizing::new(std::mem::take(&mut entry.password)))
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn background_keychain_open_does_not_initialize_an_empty_vault() {
        let temp = tempfile::tempdir().unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let repository = SqliteRepository::open(&temp.path().join("shared-vault.sqlite")).unwrap();
        assert!(run(repository.header()).unwrap().is_none());
        assert!(matches!(background_keychain_keys(&repository), Err("credential_store_locked")));
        assert!(run(repository.header()).unwrap().is_none());
    }

    #[test]
    fn authorization_validates_existing_vault_before_replacing_cached_key() {
        let temp = tempfile::tempdir().unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let root = temp.path().canonicalize().unwrap();
        let original = vec![41_u8; 32];
        retain_authorized_key(&root, SecretBytes::new(original.clone())).unwrap();
        let vault = Vault::open(&root).unwrap();
        let handle = vault.generate("chat", "https://example.com", "name@example.com").unwrap();
        let before = vault.resolve("chat", "https://example.com", &handle).unwrap();
        assert!(retain_authorized_key(&root, SecretBytes::new(vec![19_u8; 32])).is_err());
        assert_eq!(cached_key(&root).unwrap().unwrap().expose(), original.as_slice());
        let after = Vault::open(&root).unwrap().resolve("chat", "https://example.com", &handle).unwrap();
        assert_eq!(*before, *after);
        authorized_keys().lock().unwrap().remove(&root);
    }
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn encrypted_durable_scoped_and_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let handle;
        let secret;
        {
            let vault = Vault::open(temp.path()).unwrap();
            handle = vault
                .generate(
                    "chat",
                    "https://example.com/reset?secret=ignored",
                    "user@example.com",
                )
                .unwrap();
            secret = vault
                .resolve("chat", "https://example.com", &handle)
                .unwrap();
            assert_eq!(
                handle,
                vault
                    .generate("chat", "https://example.com", "user@example.com")
                    .unwrap()
            );
            assert!(vault
                .resolve("other", "https://example.com", &handle)
                .is_err());
            assert!(vault
                .resolve("chat", "https://evil.example", &handle)
                .is_err());
            assert!(!fs::read(vault.root.join("shared-vault.sqlite"))
                .unwrap()
                .windows(secret.len())
                .any(|s| s == secret.as_bytes()));
            assert!(!vault.root.join("key").exists());
        }
        let vault = Vault::open(temp.path()).unwrap();
        assert_eq!(
            *secret,
            *vault
                .resolve("chat", "https://example.com", &handle)
                .unwrap()
        );
        assert!(vault.path("../key").is_err());
    }
    #[test]
    fn rotation_stages_without_overwriting_and_commits_conditionally() {
        let temp = tempfile::tempdir().unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let vault = Vault::open(temp.path()).unwrap();
        let (workspace, site, account) = ("chat", "https://example.com", "user@example.com");
        let active = vault.generate(workspace, site, account).unwrap();
        let old = vault.resolve(workspace, site, &active).unwrap();
        let candidate = vault.stage_rotation(workspace, site, account).unwrap();
        let next = vault.resolve(workspace, site, &candidate).unwrap();
        assert_ne!(*old, *next);
        assert_eq!(*vault.resolve(workspace, site, &active).unwrap(), *old);
        assert!(vault.resolve("other", site, &candidate).is_err());
        assert!(vault.commit_rotation(workspace, site, "other@example.com", &candidate).is_err());
        let competing = vault.stage_rotation(workspace, site, account).unwrap();
        assert_eq!(vault.commit_rotation(workspace, site, account, &candidate).unwrap(), active);
        assert_eq!(*vault.resolve(workspace, site, &active).unwrap(), *next);
        assert_eq!(vault.commit_rotation(workspace, site, account, &candidate).unwrap(), active);
        assert_eq!(vault.commit_rotation(workspace, site, account, &competing), Err("credential_store_changed"));
        assert_eq!(*vault.resolve(workspace, site, &active).unwrap(), *next);
        let later = vault.stage_rotation(workspace, site, account).unwrap();
        vault.commit_rotation(workspace, site, account, &later).unwrap();
        assert_eq!(vault.commit_rotation(workspace, site, account, &candidate), Err("credential_store_changed"));
        assert_eq!(*vault.resolve(workspace, site, &active).unwrap(), *vault.resolve(workspace, site, &later).unwrap());
        assert!(!fs::read(vault.root.join("shared-vault.sqlite")).unwrap().windows(next.len()).any(|s| s == next.as_bytes()));
    }
    #[test]
    fn rotation_works_through_credential_store_interface() {
        let temp = tempfile::tempdir().unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let embedded = Vault::open(temp.path()).unwrap();
        let (workspace, site, account) = ("chat", "https://example.com", "user@example.com");
        let service = Vault { root: embedded.root.clone(), storage: Storage::Service(embedded.shared(workspace, site).unwrap()) };
        let active = service.generate(workspace, site, account).unwrap();
        let before = service.resolve(workspace, site, &active).unwrap();
        let candidate = service.stage_rotation(workspace, site, account).unwrap();
        assert_eq!(*service.resolve(workspace, site, &active).unwrap(), *before);
        service.commit_rotation(workspace, site, account, &candidate).unwrap();
        assert_eq!(*service.resolve(workspace, site, &active).unwrap(), *service.resolve(workspace, site, &candidate).unwrap());
    }
    #[test]
    fn rotation_can_recover_an_account_without_a_saved_password() {
        let temp = tempfile::tempdir().unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let vault = Vault::open(temp.path()).unwrap();
        let (workspace, site, account) = ("chat", "https://example.com", "user@example.com");
        let candidate = vault.stage_rotation(workspace, site, account).unwrap();
        let next = vault.resolve(workspace, site, &candidate).unwrap();
        let active = vault.commit_rotation(workspace, site, account, &candidate).unwrap();
        assert_eq!(*vault.resolve(workspace, site, &active).unwrap(), *next);
        assert_eq!(vault.generate(workspace, site, account).unwrap(), active);
    }
    #[test]
    fn rejects_insecure_storage_and_origins() {
        let temp = tempfile::tempdir().unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(Vault::open(temp.path()).is_err());
        for url in [
            "http://example.com",
            "https://user:pass@example.com",
            "file:///tmp/test",
            "about:blank",
        ] {
            assert!(origin(url).is_err());
        }
    }
    #[test]
    fn legacy_password_import_preserves_reference_and_retires_old_key() {
        let temp = tempfile::tempdir().unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let vault = Vault::open(temp.path()).unwrap();
        let workspace = "chat";
        let site = "https://example.com";
        let account = "user@example.com";
        let handle = format!(
            "cred_{}",
            data_encoding::HEXLOWER.encode(&Sha256::digest(
                serde_json::to_vec(&(workspace, site, account)).unwrap()
            ))
        );
        let entry = Entry {
            workspace: workspace.into(),
            origin: site.into(),
            account: account.into(),
            password: "LEGACY-PASSWORD-CANARY".into(),
            rotation: None,
        };
        let key = [42; 32];
        let nonce = [13; 24];
        let cipher = XChaCha20Poly1305::new_from_slice(&key).unwrap();
        let mut encrypted = nonce.to_vec();
        encrypted.extend(
            cipher
                .encrypt(
                    XNonce::from_slice(&nonce),
                    Payload {
                        msg: &serde_json::to_vec(&entry).unwrap(),
                        aad: &serde_json::to_vec(&(workspace, site, &handle)).unwrap(),
                    },
                )
                .unwrap(),
        );
        fs::write(vault.root.join("key"), key).unwrap();
        fs::set_permissions(vault.root.join("key"), fs::Permissions::from_mode(0o600)).unwrap();
        let source = vault.path(&handle).unwrap();
        fs::write(&source, encrypted).unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            &*vault.resolve(workspace, site, &handle).unwrap(),
            "LEGACY-PASSWORD-CANARY"
        );
        assert!(!source.exists());
        assert!(!vault.root.join("key").exists());
        assert_eq!(vault.generate(workspace, site, account).unwrap(), handle);
    }
}
