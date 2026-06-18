//! The transport authenticates identity before invoking the same embedded ports.
use crate::{
    crypto::{err, now_ms},
    *,
};
use axum::{
    extract::{DefaultBodyLimit, Request, State},
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Extension, Json, Router,
};
use kageha_contracts::{ErrorCode, ServiceError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    sync::{atomic::AtomicBool, Arc},
    time::{Duration, Instant},
};
use zeroize::Zeroize;
#[derive(Clone, Debug)]
pub enum Peer {
    Local(u32),
    Certificate(String),
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientIdentity {
    pub principal: String,
    pub authentication_generation: u64,
    #[serde(default)]
    pub management: bool,
    pub local_token_sha256: Option<String>,
    pub uid: Option<u32>,
    pub certificate_sha256: Option<String>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceOptions {
    #[serde(default = "default_rate")]
    pub requests_per_minute: u32,
}
fn default_rate() -> u32 {
    6000
}
impl Default for ServiceOptions {
    fn default() -> Self {
        Self {
            requests_per_minute: default_rate(),
        }
    }
}
struct Host {
    options: ServiceOptions,
    registry: Option<Arc<registry::Registry>>,
    vault: Arc<dyn CredentialStore>,
    identities: Vec<ClientIdentity>,
    slots: Arc<tokio::sync::Semaphore>,
    transfer_slots: Arc<tokio::sync::Semaphore>,
    rate: std::sync::Mutex<BTreeMap<String, (u64, u32)>>,
}
fn failure(e: ServiceError) -> Response {
    let status = match e.code {
        ErrorCode::NotFound => StatusCode::NOT_FOUND,
        ErrorCode::Unauthorized => StatusCode::FORBIDDEN,
        ErrorCode::Invalid => StatusCode::BAD_REQUEST,
        ErrorCode::Conflict | ErrorCode::StaleGeneration => StatusCode::CONFLICT,
        ErrorCode::Deadline => StatusCode::GATEWAY_TIMEOUT,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    };
    (status, Json(e)).into_response()
}
fn equal(a: &str, b: &str) -> bool {
    let mut diff = a.len() ^ b.len();
    for (i, v) in a.bytes().enumerate() {
        diff |= (v ^ b.as_bytes().get(i).copied().unwrap_or(0)) as usize;
    }
    diff == 0
}
async fn authentication(State(host): State<Arc<Host>>, mut req: Request, next: Next) -> Response {
    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    let digest = format!("{:x}", Sha256::digest(token.as_bytes()));
    let identity = host
        .identities
        .iter()
        .find(|id| match req.extensions().get::<Peer>() {
            Some(Peer::Local(uid)) => {
                id.uid == Some(*uid)
                    && id
                        .local_token_sha256
                        .as_ref()
                        .is_some_and(|v| equal(v, &digest))
                    && token.len() >= 32
                    && token.len() <= 4096
            }
            Some(Peer::Certificate(fp)) => {
                id.certificate_sha256.as_ref().is_some_and(|v| equal(v, fp))
            }
            None => false,
        });
    let Some(id) = identity else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let transfer_permit = if matches!(req.uri().path(), "/v1/transfer" | "/v1/replacements") {
        if !id.management {
            return StatusCode::FORBIDDEN.into_response();
        }
        match host.transfer_slots.clone().try_acquire_owned() {
            Ok(v) => Some(v),
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        }
    } else {
        None
    };
    let cancelled = AtomicBool::new(false);
    match host
        .vault
        .authentication_generation(&control(&cancelled))
        .await
    {
        Ok(generation) if generation == id.authentication_generation => {}
        Ok(_) => return StatusCode::UNAUTHORIZED.into_response(),
        Err(_) => {
            return failure(err(
                ErrorCode::Unavailable,
                "Credential vault is locked or unavailable",
            ))
        }
    }
    let principal = id.principal.clone();
    {
        let mut rates = match host.rate.lock() {
            Ok(v) => v,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
        let minute = now_ms() / 60_000;
        let entry = rates.entry(principal.clone()).or_insert((minute, 0));
        if entry.0 != minute {
            *entry = (minute, 0);
        }
        if entry.1 >= host.options.requests_per_minute {
            return StatusCode::TOO_MANY_REQUESTS.into_response();
        }
        entry.1 += 1;
    }
    let permit = match host.slots.clone().try_acquire_owned() {
        Ok(v) => v,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    req.extensions_mut().insert(Principal(principal));
    let mut response = match tokio::time::timeout(Duration::from_secs(30), next.run(req)).await {
        Ok(v) => v,
        Err(_) => StatusCode::GATEWAY_TIMEOUT.into_response(),
    };
    drop(permit);
    drop(transfer_permit);
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}
#[derive(Clone)]
struct Principal(String);
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Query {
    pub reference: CredentialRef,
    pub purpose: String,
    pub destination: String,
}
impl Query {
    fn access(&self, p: &Principal) -> CredentialAccess {
        CredentialAccess {
            principal: p.0.clone(),
            purpose: self.purpose.clone(),
            destination: self.destination.clone(),
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Put {
    reference: CredentialRef,
    purpose: String,
    destination: String,
    operation_id: String,
    expected_revision: u64,
    kind: String,
    expires_at_ms: Option<u64>,
    secret_base64: String,
}
impl Drop for Put {
    fn drop(&mut self) {
        self.secret_base64.zeroize();
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Revoke {
    reference: CredentialRef,
    purpose: String,
    destination: String,
    operation_id: String,
    expected_revision: u64,
    delete: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Operation {
    reference: CredentialRef,
    purpose: String,
    destination: String,
    operation_id: String,
}
#[derive(Serialize, Deserialize)]
pub(crate) struct WireLease {
    pub secret_base64: String,
    pub metadata: CredentialMetadata,
    pub valid_until_ms: u64,
}
impl Drop for WireLease {
    fn drop(&mut self) {
        self.secret_base64.zeroize();
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Validate {
    reference: CredentialRef,
    purpose: String,
    destination: String,
    metadata: CredentialMetadata,
    valid_until_ms: u64,
}
fn control(cancel: &AtomicBool) -> kageha_contracts::execution::Control<'_> {
    kageha_contracts::execution::Control {
        cancelled: cancel,
        deadline: Some(Instant::now() + Duration::from_secs(25)),
    }
}
async fn info(State(h): State<Arc<Host>>, Extension(p): Extension<Principal>) -> Response {
    let budgets = if h
        .identities
        .iter()
        .any(|id| id.principal == p.0 && id.management)
    {
        h.rate.lock().ok().map(|rates| rates.clone())
    } else {
        None
    };
    let c = AtomicBool::new(false);
    match h.vault.binding_generation(&control(&c)).await{Ok(generation)=>Json(serde_json::json!({"component":"credentials","protocol_version":1,"workload":p.0,"replacement_jobs":h.registry.is_some(),"requests_per_minute":h.options.requests_per_minute,"request_budgets":budgets,"generation":generation,"authentication_generation":h.vault.authentication_generation(&control(&c)).await.ok(),"capabilities":["metadata","resolve","write.cas","revoke","operation.status","oauth.refresh","audit","checkpoint.v1"]})).into_response(),Err(e)=>failure(e)}
}
async fn metadata(
    State(h): State<Arc<Host>>,
    Extension(p): Extension<Principal>,
    Json(q): Json<Query>,
) -> Response {
    let c = AtomicBool::new(false);
    match h
        .vault
        .metadata(&q.access(&p), &q.reference, &control(&c))
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => failure(e),
    }
}
async fn resolve(
    State(h): State<Arc<Host>>,
    Extension(p): Extension<Principal>,
    Json(q): Json<Query>,
) -> Response {
    let c = AtomicBool::new(false);
    match h
        .vault
        .resolve(&q.access(&p), &q.reference, &control(&c))
        .await
    {
        Ok(v) => Json(WireLease {
            secret_base64: data_encoding::BASE64.encode(v.secret.expose()),
            metadata: v.metadata,
            valid_until_ms: v.valid_until_ms,
        })
        .into_response(),
        Err(e) => failure(e),
    }
}
async fn put(
    State(h): State<Arc<Host>>,
    Extension(p): Extension<Principal>,
    Json(q): Json<Put>,
) -> Response {
    let secret = match data_encoding::BASE64.decode(q.secret_base64.as_bytes()) {
        Ok(v) => SecretBytes::new(v),
        Err(_) => return failure(err(ErrorCode::Invalid, "Invalid credential encoding")),
    };
    let c = AtomicBool::new(false);
    let access = CredentialAccess {
        principal: p.0,
        purpose: q.purpose.clone(),
        destination: q.destination.clone(),
    };
    match h
        .vault
        .put(
            &access,
            CredentialWrite {
                operation_id: q.operation_id.clone(),
                reference: q.reference.clone(),
                expected_revision: q.expected_revision,
                kind: q.kind.clone(),
                expires_at_ms: q.expires_at_ms,
                secret,
            },
            &control(&c),
        )
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => failure(e),
    }
}
async fn revoke(
    State(h): State<Arc<Host>>,
    Extension(p): Extension<Principal>,
    Json(q): Json<Revoke>,
) -> Response {
    let c = AtomicBool::new(false);
    let access = CredentialAccess {
        principal: p.0,
        purpose: q.purpose,
        destination: q.destination,
    };
    match h
        .vault
        .revoke(
            &access,
            &q.reference,
            q.expected_revision,
            &q.operation_id,
            q.delete,
            &control(&c),
        )
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => failure(e),
    }
}
async fn operation(
    State(h): State<Arc<Host>>,
    Extension(p): Extension<Principal>,
    Json(q): Json<Operation>,
) -> Response {
    let c = AtomicBool::new(false);
    let access = CredentialAccess {
        principal: p.0,
        purpose: q.purpose,
        destination: q.destination,
    };
    match h
        .vault
        .operation(&access, &q.reference, &q.operation_id, &control(&c))
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(e) => failure(e),
    }
}
async fn validate(
    State(h): State<Arc<Host>>,
    Extension(p): Extension<Principal>,
    Json(q): Json<Validate>,
) -> Response {
    let c = AtomicBool::new(false);
    if q.reference != q.metadata.reference {
        return failure(err(
            ErrorCode::Invalid,
            "Credential lease reference mismatch",
        ));
    }
    let access = CredentialAccess {
        principal: p.0,
        purpose: q.purpose,
        destination: q.destination,
    };
    let lease = SecretLease {
        secret: SecretBytes::new(Vec::new()),
        metadata: q.metadata,
        valid_until_ms: q.valid_until_ms,
    };
    match h.vault.validate_lease(&access, &lease, &control(&c)).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => failure(e),
    }
}
pub fn router(
    vault: Arc<dyn CredentialStore>,
    identities: Vec<ClientIdentity>,
) -> kageha_contracts::Result<Router> {
    router_inner(vault, identities, None, ServiceOptions::default())
}
fn router_inner(
    vault: Arc<dyn CredentialStore>,
    identities: Vec<ClientIdentity>,
    registry: Option<Arc<registry::Registry>>,
    options: ServiceOptions,
) -> kageha_contracts::Result<Router> {
    if !(1..=60000).contains(&options.requests_per_minute) {
        return Err(err(
            ErrorCode::Invalid,
            "Credential rate limit must be between 1 and 60000 per minute",
        ));
    }
    if identities.is_empty()
        || identities.len() > 1000
        || identities.iter().any(|v| {
            v.principal.is_empty()
                || v.principal.len() > 256
                || v.local_token_sha256.as_ref().is_some_and(|s| s.len() != 64)
                || v.certificate_sha256.as_ref().is_some_and(|s| s.len() != 64)
        })
    {
        return Err(err(
            ErrorCode::Invalid,
            "Configure bounded authenticated credential workloads",
        ));
    }
    let h = Arc::new(Host {
        options,
        registry,
        vault,
        identities,
        slots: Arc::new(tokio::sync::Semaphore::new(32)),
        transfer_slots: Arc::new(tokio::sync::Semaphore::new(1)),
        rate: std::sync::Mutex::new(BTreeMap::new()),
    });
    let large = Router::new()
        .route("/v1/transfer", post(transfer))
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024));
    Ok(Router::new()
        .route("/v1/info", get(info).post(info))
        .route("/v1/replacements", post(jobs))
        .route("/v1/metadata", post(metadata))
        .route("/v1/resolve", post(resolve))
        .route("/v1/put", post(put))
        .route("/v1/revoke", post(revoke))
        .route("/v1/operation", post(operation))
        .route("/v1/check", post(validate))
        .route("/v1/oauth", post(oauth))
        .route("/v1/audit", post(audits))
        .route("/v1/control", post(controls))
        .layer(DefaultBodyLimit::max(128 * 1024))
        .merge(large)
        .layer(middleware::from_fn_with_state(h.clone(), authentication))
        .with_state(h)
        .layer(middleware::from_fn(no_store)))
}
/// Each accepted connection receives transport-derived identity; network headers cannot supply it.
pub async fn serve_unix(socket: &std::path::Path, router: Router) -> kageha_contracts::Result<()> {
    serve_unix_until(socket, router, std::future::pending()).await
}
pub async fn serve_unix_until(
    socket: &std::path::Path,
    router: Router,
    shutdown: impl std::future::Future<Output = ()>,
) -> kageha_contracts::Result<()> {
    tokio::pin!(shutdown);
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let parent = socket
        .parent()
        .ok_or_else(|| err(ErrorCode::Invalid, "Credential socket parent missing"))?;
    let meta = std::fs::metadata(parent).map_err(|_| {
        err(
            ErrorCode::Unavailable,
            "Credential socket directory unavailable",
        )
    })?;
    if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err(err(
            ErrorCode::Unauthorized,
            "Credential socket directory must be private",
        ));
    }
    use fs2::FileExt;
    use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(socket.with_extension("lock"))
        .map_err(|_| err(ErrorCode::Unavailable, "Credential socket lock unavailable"))?;
    let meta = lock
        .metadata()
        .map_err(|_| err(ErrorCode::Unavailable, "Credential socket lock unavailable"))?;
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err(err(
            ErrorCode::Unauthorized,
            "Credential socket lock must be private",
        ));
    }
    lock.try_lock_exclusive()
        .map_err(|_| err(ErrorCode::Conflict, "Credential service is already running"))?;
    if let Ok(meta) = std::fs::symlink_metadata(socket) {
        if !meta.file_type().is_socket()
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.mode() & 0o077 != 0
        {
            return Err(err(
                ErrorCode::Unauthorized,
                "Existing credential socket is not private",
            ));
        }
        match tokio::net::UnixStream::connect(socket).await {
            Ok(_) => return Err(err(ErrorCode::Conflict, "Credential socket is in use")),
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                std::fs::remove_file(socket).map_err(|_| {
                    err(
                        ErrorCode::Unavailable,
                        "Stale credential socket cannot be retired",
                    )
                })?;
            }
            Err(_) => {
                return Err(err(
                    ErrorCode::Unavailable,
                    "Credential socket state is unknown",
                ))
            }
        }
    }
    let listener = tokio::net::UnixListener::bind(socket)
        .map_err(|_| err(ErrorCode::Unavailable, "Credential socket unavailable"))?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600)).map_err(|_| {
        err(
            ErrorCode::Unavailable,
            "Credential socket permissions failed",
        )
    })?;
    let _socket_guard = SocketGuard {
        path: socket.into(),
        inode: std::fs::symlink_metadata(socket)
            .map_err(|_| err(ErrorCode::Unavailable, "Credential socket unavailable"))?
            .ino(),
        _lock: lock,
    };
    let (drain, draining) = tokio::sync::watch::channel(false);
    let slots = Arc::new(tokio::sync::Semaphore::new(64));
    loop {
        let permit = tokio::select! {_=&mut shutdown=>break,p=slots.clone().acquire_owned()=>p.map_err(|_|err(ErrorCode::Unavailable,"Credential service draining"))?};
        let (stream, _) = tokio::select! {_=&mut shutdown=>break,stream=listener.accept()=>stream.map_err(|_|err(ErrorCode::Unavailable,"Credential socket accept failed"))?};
        let uid = stream
            .peer_cred()
            .map_err(|_| {
                err(
                    ErrorCode::Unauthorized,
                    "Credential peer identity unavailable",
                )
            })?
            .uid();
        let service = router.clone().layer(Extension(Peer::Local(uid)));
        let end = draining.clone();
        tokio::spawn(async move {
            connection(stream, service, end).await;
            drop(permit);
        });
    }
    let _ = drain.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(36), slots.acquire_many_owned(64)).await;
    Ok(())
}
pub async fn serve_tls(
    bind: &str,
    router: Router,
    certificates: Vec<tokio_rustls::rustls::pki_types::CertificateDer<'static>>,
    key: tokio_rustls::rustls::pki_types::PrivateKeyDer<'static>,
    client_roots: tokio_rustls::rustls::RootCertStore,
) -> kageha_contracts::Result<()> {
    serve_tls_until(
        bind,
        router,
        certificates,
        key,
        client_roots,
        std::future::pending(),
    )
    .await
}
pub async fn serve_tls_until(
    bind: &str,
    router: Router,
    certificates: Vec<tokio_rustls::rustls::pki_types::CertificateDer<'static>>,
    key: tokio_rustls::rustls::pki_types::PrivateKeyDer<'static>,
    client_roots: tokio_rustls::rustls::RootCertStore,
    shutdown: impl std::future::Future<Output = ()>,
) -> kageha_contracts::Result<()> {
    tokio::pin!(shutdown);
    let verifier =
        tokio_rustls::rustls::server::WebPkiClientVerifier::builder(Arc::new(client_roots))
            .build()
            .map_err(|_| err(ErrorCode::Invalid, "Invalid credential client trust roots"))?;
    let config = tokio_rustls::rustls::ServerConfig::builder()
        .with_client_cert_verifier(verifier)
        .with_single_cert(certificates, key)
        .map_err(|_| err(ErrorCode::Invalid, "Invalid credential server TLS identity"))?;
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = tokio::net::TcpListener::bind(bind).await.map_err(|_| {
        err(
            ErrorCode::Unavailable,
            "Credential TLS listener unavailable",
        )
    })?;
    let (drain, draining) = tokio::sync::watch::channel(false);
    let slots = Arc::new(tokio::sync::Semaphore::new(64));
    loop {
        let permit = tokio::select! {_=&mut shutdown=>break,p=slots.clone().acquire_owned()=>p.map_err(|_|err(ErrorCode::Unavailable,"Credential service draining"))?};
        let (stream, _) = tokio::select! {_=&mut shutdown=>break,stream=listener.accept()=>stream.map_err(|_|err(ErrorCode::Unavailable,"Credential TLS accept failed"))?};
        let acceptor = acceptor.clone();
        let router = router.clone();
        let mut end = draining.clone();
        tokio::spawn(async move {
            let Ok(Ok(stream)) = tokio::select!(result=tokio::time::timeout(Duration::from_secs(10),acceptor.accept(stream))=>result,_=end.changed()=>return)
            else {
                return;
            };
            let Some(cert) = stream
                .get_ref()
                .1
                .peer_certificates()
                .and_then(|v| v.first())
            else {
                return;
            };
            let peer = Peer::Certificate(format!("{:x}", Sha256::digest(cert.as_ref())));
            connection(stream, router.layer(Extension(peer)), end).await;
            drop(permit);
        });
    }
    let _ = drain.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(36), slots.acquire_many_owned(64)).await;
    Ok(())
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum OAuthAction {
    Begin {
        expected_revision: u64,
        operation_id: String,
    },
    Dispatch {
        ticket: RefreshTicket,
        operation_id: String,
    },
    Complete {
        ticket: RefreshTicket,
        operation_id: String,
        grant_base64: String,
        expires_at_ms: Option<u64>,
    },
    Recover {
        expected_revision: u64,
        operation_id: String,
    },
}
impl Drop for OAuthAction {
    fn drop(&mut self) {
        if let Self::Complete { grant_base64, .. } = self {
            grant_base64.zeroize();
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OAuthRequest {
    query: Query,
    call: OAuthAction,
}
#[derive(Serialize)]
struct OAuthReply {
    ticket: Option<RefreshTicket>,
    operation: Option<CredentialOperation>,
    grant_base64: Option<String>,
}
impl Drop for OAuthReply {
    fn drop(&mut self) {
        if let Some(value) = &mut self.grant_base64 {
            value.zeroize();
        }
    }
}
async fn oauth(
    State(h): State<Arc<Host>>,
    Extension(p): Extension<Principal>,
    Json(q): Json<OAuthRequest>,
) -> Response {
    let c = AtomicBool::new(false);
    let control = control(&c);
    let access = q.query.access(&p);
    let reference = &q.query.reference;
    let result: kageha_contracts::Result<OAuthReply> = match &q.call {
        OAuthAction::Begin {
            expected_revision,
            operation_id,
        } => h
            .vault
            .begin_refresh(
                &access,
                reference,
                *expected_revision,
                operation_id,
                &control,
            )
            .await
            .map(|ticket| OAuthReply {
                ticket: Some(ticket),
                operation: None,
                grant_base64: None,
            }),
        OAuthAction::Dispatch {
            ticket,
            operation_id,
        } if ticket.reference == *reference => h
            .vault
            .dispatch_refresh(&access, ticket, operation_id, &control)
            .await
            .map(|(ticket, grant)| OAuthReply {
                ticket: Some(ticket),
                operation: None,
                grant_base64: Some(data_encoding::BASE64.encode(grant.expose())),
            }),
        OAuthAction::Complete {
            ticket,
            operation_id,
            grant_base64,
            expires_at_ms,
        } if ticket.reference == *reference => {
            match data_encoding::BASE64.decode(grant_base64.as_bytes()) {
                Ok(value) => h
                    .vault
                    .complete_refresh(
                        &access,
                        ticket,
                        operation_id,
                        SecretBytes::new(value),
                        *expires_at_ms,
                        &control,
                    )
                    .await
                    .map(|operation| OAuthReply {
                        ticket: None,
                        operation: Some(operation),
                        grant_base64: None,
                    }),
                Err(_) => Err(err(ErrorCode::Invalid, "Invalid OAuth grant encoding")),
            }
        }
        OAuthAction::Recover {
            expected_revision,
            operation_id,
        } => h
            .vault
            .recover_refresh(
                &access,
                reference,
                *expected_revision,
                operation_id,
                &control,
            )
            .await
            .map(|operation| OAuthReply {
                ticket: None,
                operation: Some(operation),
                grant_base64: None,
            }),
        _ => Err(err(ErrorCode::Invalid, "OAuth refresh reference mismatch")),
    };
    match result {
        Ok(reply) => Json(reply).into_response(),
        Err(e) => failure(e),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuditQuery {
    namespace: String,
    purpose: String,
    destination: String,
    after: u64,
}
async fn audits(
    State(h): State<Arc<Host>>,
    Extension(p): Extension<Principal>,
    Json(q): Json<AuditQuery>,
) -> Response {
    let cancelled = AtomicBool::new(false);
    let access = CredentialAccess {
        principal: p.0,
        purpose: q.purpose,
        destination: q.destination,
    };
    match h
        .vault
        .audits(&access, &q.namespace, q.after, &control(&cancelled))
        .await
    {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => failure(e),
    }
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum TransferAction {
    State,
    Export,
    Freeze {
        expected_generation: u64,
        owner: String,
    },
    Import {
        expected_generation: u64,
        owner: String,
        checkpoint: crate::wire::Checkpoint,
    },
    Activate {
        expected_generation: u64,
        owner: String,
    },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TransferRequest {
    query: Query,
    call: TransferAction,
}
async fn transfer(
    State(h): State<Arc<Host>>,
    Extension(p): Extension<Principal>,
    Json(q): Json<TransferRequest>,
) -> Response {
    let c = AtomicBool::new(false);
    let control = control(&c);
    let access = q.query.access(&p);
    if q.query.reference
        != (CredentialRef {
            namespace: "system".into(),
            id: "vault".into(),
        })
    {
        return failure(err(
            ErrorCode::Invalid,
            "Credential transfer requires the system vault reference",
        ));
    }
    match q.call {
        TransferAction::State => match h.vault.backend_state(&access, &control).await {
            Ok(state) => Json(state).into_response(),
            Err(e) => failure(e),
        },
        TransferAction::Export => match h.vault.export_checkpoint(&access, &control).await {
            Ok(checkpoint) => Json(crate::wire::Checkpoint::encode(checkpoint)).into_response(),
            Err(e) => failure(e),
        },
        TransferAction::Freeze {
            expected_generation,
            owner,
        } => match h
            .vault
            .freeze(&access, expected_generation, &owner, &control)
            .await
        {
            Ok(state) => Json(state).into_response(),
            Err(e) => failure(e),
        },
        TransferAction::Import {
            expected_generation,
            owner,
            checkpoint,
        } => match checkpoint.decode() {
            Ok(checkpoint) => match h
                .vault
                .import_checkpoint(&access, expected_generation, &owner, checkpoint, &control)
                .await
            {
                Ok(state) => Json(state).into_response(),
                Err(e) => failure(e),
            },
            Err(e) => failure(e),
        },
        TransferAction::Activate {
            expected_generation,
            owner,
        } => match h
            .vault
            .activate(&access, expected_generation, &owner, &control)
            .await
        {
            Ok(state) => Json(state).into_response(),
            Err(e) => failure(e),
        },
    }
}
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum ControlAction {
    RevokeWorkload {
        principal: String,
        expected_revision: u64,
        operation_id: String,
    },
    Fence {
        expected_revision: u64,
        operation_id: String,
    },
    FailRefresh {
        ticket: RefreshTicket,
        operation_id: String,
    },
    ExportRecord,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ControlRequest {
    query: Query,
    call: ControlAction,
}
#[derive(Serialize)]
struct ExportRecordReply {
    metadata: CredentialMetadata,
    secret_base64: String,
}
impl Drop for ExportRecordReply {
    fn drop(&mut self) {
        self.secret_base64.zeroize();
    }
}
async fn controls(
    State(h): State<Arc<Host>>,
    Extension(p): Extension<Principal>,
    Json(q): Json<ControlRequest>,
) -> Response {
    let c = AtomicBool::new(false);
    let control = control(&c);
    let access = q.query.access(&p);
    match q.call {
        ControlAction::Fence {
            expected_revision,
            operation_id,
        } => match h
            .vault
            .fence(
                &access,
                &q.query.reference,
                expected_revision,
                &operation_id,
                &control,
            )
            .await
        {
            Ok(op) => Json(op).into_response(),
            Err(e) => failure(e),
        },
        ControlAction::FailRefresh {
            ticket,
            operation_id,
        } => {
            if ticket.reference != q.query.reference {
                return failure(err(ErrorCode::Invalid, "OAuth refresh reference mismatch"));
            }
            match h
                .vault
                .fail_refresh(&access, &ticket, &operation_id, &control)
                .await
            {
                Ok(op) => Json(op).into_response(),
                Err(e) => failure(e),
            }
        }
        ControlAction::RevokeWorkload {
            principal,
            expected_revision,
            operation_id,
        } => match h
            .vault
            .revoke_workload(
                &access,
                &principal,
                expected_revision,
                &operation_id,
                &control,
            )
            .await
        {
            Ok(v) => Json(v).into_response(),
            Err(e) => failure(e),
        },
        ControlAction::ExportRecord => match h
            .vault
            .export_record(&access, &q.query.reference, &control)
            .await
        {
            Ok((metadata, secret)) => Json(ExportRecordReply {
                metadata,
                secret_base64: data_encoding::BASE64.encode(secret.expose()),
            })
            .into_response(),
            Err(e) => failure(e),
        },
    }
}

#[derive(Deserialize)]
struct JobRequest {
    purpose: String,
    destination: String,
    #[serde(flatten)]
    call: JobAction,
}
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum JobAction {
    Cancel {
        operation_id: String,
    },
    Binding,
    Request {
        operation_id: String,
        decision: registry::Decision,
    },
    Status {
        operation_id: String,
    },
    Resume {
        operation_id: String,
    },
    Rollback {
        operation_id: String,
        previous_operation_id: String,
        decision: registry::Decision,
    },
    Retire {
        operation_id: String,
    },
}
async fn jobs(
    State(h): State<Arc<Host>>,
    Extension(p): Extension<Principal>,
    Json(q): Json<JobRequest>,
) -> Response {
    let Some(registry) = &h.registry else {
        return failure(err(
            ErrorCode::Unsupported,
            "Credential registry is not configured",
        ));
    };
    let flag = AtomicBool::new(false);
    let c = control(&flag);
    let access = CredentialAccess {
        principal: p.0,
        purpose: q.purpose,
        destination: q.destination,
    };
    let result: kageha_contracts::Result<serde_json::Value> = match q.call {
        JobAction::Binding => match registry.backend_state(&access, &c).await {
            Ok(_) => registry.binding().await.and_then(|v| {
                serde_json::to_value(v)
                    .map_err(|_| err(ErrorCode::Unavailable, "Credential binding unavailable"))
            }),
            Err(e) => Err(e),
        },
        JobAction::Request {
            operation_id,
            decision,
        } => registry
            .request(&access, decision, &operation_id, &c)
            .await
            .and_then(|v| {
                serde_json::to_value(v)
                    .map_err(|_| err(ErrorCode::Unavailable, "Credential replacement unavailable"))
            }),
        JobAction::Status { operation_id } => registry
            .status(&access, &operation_id, &c)
            .await
            .and_then(|v| {
                serde_json::to_value(v)
                    .map_err(|_| err(ErrorCode::Unavailable, "Credential replacement unavailable"))
            }),
        JobAction::Resume { operation_id } => registry
            .resume(&access, &operation_id, &c)
            .await
            .and_then(|v| {
                serde_json::to_value(v)
                    .map_err(|_| err(ErrorCode::Unavailable, "Credential replacement unavailable"))
            }),
        JobAction::Rollback {
            operation_id,
            previous_operation_id,
            decision,
        } => registry
            .rollback(&access, &previous_operation_id, decision, &operation_id, &c)
            .await
            .and_then(|v| {
                serde_json::to_value(v)
                    .map_err(|_| err(ErrorCode::Unavailable, "Credential replacement unavailable"))
            }),
        JobAction::Cancel { operation_id } => registry
            .cancel(&access, &operation_id, &c)
            .await
            .and_then(|v| {
                serde_json::to_value(v).map_err(|_| {
                    err(
                        ErrorCode::Unavailable,
                        "Credential cancellation unavailable",
                    )
                })
            }),
        JobAction::Retire { operation_id } => registry
            .retire(&access, &operation_id, &c)
            .await
            .and_then(|v| {
                serde_json::to_value(v)
                    .map_err(|_| err(ErrorCode::Unavailable, "Credential replacement unavailable"))
            }),
    };
    match result {
        Ok(v) => Json(v).into_response(),
        Err(e) => failure(e),
    }
}
/// Host the same registry consumers use, with separately authorized management APIs.
pub fn router_with_registry(
    registry: Arc<registry::Registry>,
    identities: Vec<ClientIdentity>,
) -> kageha_contracts::Result<Router> {
    router_inner(
        registry.clone(),
        identities,
        Some(registry),
        ServiceOptions::default(),
    )
}

async fn no_store(req: Request, next: Next) -> Response {
    let mut r = next.run(req).await;
    if matches!(
        r.status(),
        StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY | StatusCode::PAYLOAD_TOO_LARGE
    ) {
        let status = r.status();
        r = (
            status,
            Json(err(
                ErrorCode::Invalid,
                "Invalid bounded credential request",
            )),
        )
            .into_response();
    }
    r.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    r
}

struct SocketGuard {
    path: std::path::PathBuf,
    inode: u64,
    _lock: std::fs::File,
}
impl Drop for SocketGuard {
    fn drop(&mut self) {
        use std::os::unix::fs::MetadataExt;
        if std::fs::symlink_metadata(&self.path).is_ok_and(|m| m.ino() == self.inode) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

async fn connection<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    stream: S,
    router: Router,
    mut drain: tokio::sync::watch::Receiver<bool>,
) {
    let service = hyper_util::service::TowerToHyperService::new(router);
    let conn = hyper::server::conn::http1::Builder::new()
        .serve_connection(hyper_util::rt::TokioIo::new(stream), service);
    tokio::pin!(conn);
    tokio::select! {_=tokio::time::sleep(Duration::from_secs(35))=>{},_=&mut conn=>{},_=drain.changed()=>{conn.as_mut().graceful_shutdown();let _=tokio::time::timeout(Duration::from_secs(30),&mut conn).await;}}
}

pub fn router_with_options(
    vault: Arc<dyn CredentialStore>,
    identities: Vec<ClientIdentity>,
    options: ServiceOptions,
) -> kageha_contracts::Result<Router> {
    router_inner(vault, identities, None, options)
}
pub fn registry_router_with_options(
    registry: Arc<registry::Registry>,
    identities: Vec<ClientIdentity>,
    options: ServiceOptions,
) -> kageha_contracts::Result<Router> {
    router_inner(registry.clone(), identities, Some(registry), options)
}
