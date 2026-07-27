//! Independent integration transport. Domain adapters and their owned stores are separate ports.
use kageha_contracts::execution::Control;
use std::{
    io,
    sync::Arc,
    time::{Duration, Instant},
};
pub struct Config {
    pub ca_bundle_pem: Option<Vec<u8>>,
    pub proxies: Vec<ProxySetting>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            ca_bundle_pem: None,
            proxies: Vec::new(),
        }
    }
}
pub enum ProxyScope {
    Http,
    Https,
    All,
}
/// Operator-owned routing. May contain a proxy credential; deliberately has no Debug output.
pub struct ProxySetting {
    pub scope: ProxyScope,
    pub endpoint: String,
    pub bypass: Option<String>,
}
struct Transport {
    runtime: Option<tokio::runtime::Runtime>,
    client: reqwest::Client,
    allowed_origin: Option<String>,
}
impl Drop for Transport {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}
#[derive(Clone)]
pub struct HttpClient {
    transport: Arc<Transport>,
}
pub struct Response {
    pub status: u16,
    pub bytes: Vec<u8>,
    /// Authentication challenges only; other response headers can contain credentials.
    pub www_authenticate: Vec<String>,
}
impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.status)
            .field("byte_count", &self.bytes.len())
            .finish()
    }
}
pub fn valid_url(raw: &str) -> io::Result<reqwest::Url> {
    let url = reqwest::Url::parse(raw).map_err(|_| io::Error::other("Invalid HTTP(S) endpoint"))?;
    if raw.len() > 4096
        || raw.chars().any(char::is_control)
        || !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(io::Error::other("Invalid HTTP(S) endpoint"));
    }
    Ok(url)
}
pub fn validate_mutual_tls_identity(identity_pem: &[u8]) -> io::Result<()> {
    if identity_pem.is_empty() || identity_pem.len()>1024*1024 {
        return Err(io::Error::other("Invalid mutual TLS identity size"));
    }
    let identity=reqwest::Identity::from_pem(identity_pem).map_err(|_|io::Error::other("Invalid mutual TLS identity"))?;
    reqwest::Client::builder().no_proxy().identity(identity).build().map_err(|_|io::Error::other("Invalid mutual TLS identity"))?;
    Ok(())
}
impl HttpClient {
    /// Construct/use synchronous ports outside an async reactor, or in spawn_blocking.
    pub fn new(config: Config) -> io::Result<Self> {
        Self::build(config,None)
    }
    /// A fresh client scoped to one HTTPS origin. Never share an mTLS identity
    /// across integrations or redirects.
    pub fn new_mutual_tls(config: Config, identity_pem: &[u8], endpoint: &str) -> io::Result<Self> {
        if identity_pem.is_empty() || identity_pem.len()>1024*1024 || !config.proxies.is_empty() {
            return Err(io::Error::other("Invalid mutual TLS identity or proxy configuration"));
        }
        let url=valid_url(endpoint)?;
        if url.scheme()!="https" {return Err(io::Error::other("Mutual TLS requires HTTPS"));}
        let identity=reqwest::Identity::from_pem(identity_pem).map_err(|_|io::Error::other("Invalid mutual TLS identity"))?;
        Self::build(config,Some((identity,url.origin().ascii_serialization())))
    }
    fn build(config: Config, mutual_tls: Option<(reqwest::Identity,String)>) -> io::Result<Self> {
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .user_agent("kageha-rust/0.1")
            .connect_timeout(Duration::from_secs(10))
            .pool_max_idle_per_host(8);
        let allowed_origin=mutual_tls.as_ref().map(|(_,origin)|origin.clone());
        if let Some((identity,_))=mutual_tls {builder=builder.identity(identity);}
        if let Some(pem) = config.ca_bundle_pem {
            if pem.is_empty() || pem.len() > 1024 * 1024 {
                return Err(io::Error::other("Invalid integration CA bundle size"));
            }
            let certificates = reqwest::Certificate::from_pem_bundle(&pem)
                .map_err(|_| io::Error::other("Invalid integration CA bundle"))?;
            if certificates.is_empty() {
                return Err(io::Error::other("Empty integration CA bundle"));
            }
            builder = builder.tls_built_in_root_certs(false);
            for certificate in certificates {
                builder = builder.add_root_certificate(certificate);
            }
        }
        for setting in config.proxies {
            if setting.endpoint.len() > 4096 || setting.endpoint.chars().any(char::is_control) {
                return Err(io::Error::other("Invalid configured proxy"));
            }
            let proxy = match setting.scope {
                ProxyScope::Http => reqwest::Proxy::http(&setting.endpoint),
                ProxyScope::Https => reqwest::Proxy::https(&setting.endpoint),
                ProxyScope::All => reqwest::Proxy::all(&setting.endpoint),
            }
            .map_err(|_| io::Error::other("Invalid configured proxy"))?;
            builder = builder.proxy(
                proxy.no_proxy(
                    setting
                        .bypass
                        .as_deref()
                        .and_then(reqwest::NoProxy::from_string),
                ),
            );
        }
        let client = builder
            .build()
            .map_err(|_| io::Error::other("Integration client unavailable"))?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|_| io::Error::other("Integration I/O runtime unavailable"))?;
        Ok(Self {
            transport: Arc::new(Transport {
                runtime: Some(runtime),
                client,
                allowed_origin,
            }),
        })
    }
    pub async fn request_async(
        &self,
        url: &str,
        method: &str,
        headers: &[(String, String)],
        body: Vec<u8>,
        control: &Control<'_>,
        timeout: Duration,
        limit: usize,
    ) -> io::Result<Response> {
        control.check()?;
        let url = valid_url(url)?;
        if self.transport.allowed_origin.as_ref().is_some_and(|origin|url.scheme()!="https" || url.origin().ascii_serialization()!=*origin) {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied,"Mutual TLS request is outside its configured origin"));
        }
        if body.len() > 64 * 1024 * 1024
            || limit == 0
            || limit > 64 * 1024 * 1024
            || timeout.is_zero()
            || timeout > Duration::from_secs(120)
            || method.len() > 16
            || !method.bytes().all(|b| b.is_ascii_uppercase())
        {
            return Err(io::Error::other("Invalid integration request bounds"));
        }
        let method = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|_| io::Error::other("Invalid HTTP method"))?;
        let mut map = reqwest::header::HeaderMap::new();
        let mut header_bytes = 0;
        for (name, value) in headers {
            header_bytes += name.len() + value.len();
            if header_bytes > 32768
                || name.is_empty()
                || name
                    .bytes()
                    .any(|b| !b.is_ascii_alphanumeric() && b != b'-')
                || value.chars().any(char::is_control)
            {
                return Err(io::Error::other("Invalid HTTP header"));
            }
            if url.scheme() != "https"
                && matches!(
                    name.to_ascii_lowercase().as_str(),
                    "authorization" | "x-api-key" | "cookie" | "proxy-authorization"
                )
            {
                return Err(io::Error::other("Credentials require HTTPS"));
            }
            let mut header = reqwest::header::HeaderValue::from_str(value)
                .map_err(|_| io::Error::other("Invalid HTTP header"))?;
            header.set_sensitive(true);
            map.append(
                reqwest::header::HeaderName::from_bytes(name.as_bytes())
                    .map_err(|_| io::Error::other("Invalid HTTP header"))?,
                header,
            );
        }
        let deadline = (Instant::now() + timeout)
            .min(control.deadline.unwrap_or_else(|| Instant::now() + timeout));
        async {
            let operation=async{
                let mut response=self.transport.client.request(method,url).headers(map).body(body).send().await.map_err(|_|io::Error::other("Integration transport failed; outcome may be unknown"))?;
                if response.content_length().is_some_and(|n|n>limit as u64){return Err(io::Error::other("HTTP body exceeds limit"));}
                let status=response.status().as_u16();
                let mut challenges=Vec::new();let mut challenge_bytes=0usize;
                for value in response.headers().get_all(reqwest::header::WWW_AUTHENTICATE) {
                    let value=value.to_str().map_err(|_|io::Error::other("Invalid HTTP authentication challenge"))?;
                    challenge_bytes+=value.len();if challenge_bytes>8192 || challenges.len()>=16{return Err(io::Error::other("HTTP authentication challenge exceeds limit"));}
                    challenges.push(value.to_owned());
                }
                let mut bytes=Vec::new();while let Some(chunk)=response.chunk().await.map_err(|_|io::Error::other("Integration response was interrupted; outcome may be unknown"))?{if bytes.len()+chunk.len()>limit{return Err(io::Error::other("HTTP body exceeds limit"));}bytes.extend_from_slice(&chunk);}Ok(Response{status,bytes,www_authenticate:challenges})
            };
            tokio::select!{
                result=operation=>result,
                _=async{loop{if control.check().is_err(){break;}tokio::time::sleep(Duration::from_millis(10)).await;}}=>{control.check()?;Err(io::Error::new(io::ErrorKind::Interrupted,"Integration cancelled"))},
                _=tokio::time::sleep_until(tokio::time::Instant::from_std(deadline))=>Err(io::Error::new(io::ErrorKind::TimedOut,"Integration deadline exceeded; outcome may be unknown")),
            }
        }.await
    }
    /// Synchronous bridge for native threads or Tokio's multi-thread blocking pool.
    /// Async callers should use request_async; current-thread reactors are never blocked.
    pub fn request(
        &self,
        url: &str,
        method: &str,
        headers: &[(String, String)],
        body: Vec<u8>,
        control: &Control<'_>,
        timeout: Duration,
        limit: usize,
    ) -> io::Result<Response> {
        let invoke = || {
            self.transport
                .runtime
                .as_ref()
                .expect("live transport runtime")
                .block_on(self.request_async(url, method, headers, body, control, timeout, limit))
        };
        match tokio::runtime::Handle::try_current() {
            Ok(handle)
                if matches!(
                    handle.runtime_flavor(),
                    tokio::runtime::RuntimeFlavor::MultiThread
                ) =>
            {
                tokio::task::block_in_place(invoke)
            }
            Ok(_) => Err(io::Error::other(
                "Use request_async on a current-thread reactor",
            )),
            Err(_) => invoke(),
        }
    }
}
