//! Compatibility adapter for the independent, pooled Rust integration transport.
use std::{io, sync::atomic::AtomicBool, time::Duration};
pub(crate) fn valid_url(url: &str) -> io::Result<()> {
    kageha_integrations::valid_url(url).map(|_| ())
}
pub(crate) type Http = kageha_integrations::Response;
fn configuration(mutual_tls: bool) -> io::Result<kageha_integrations::Config> {
    use std::io::Read;
    let path=std::env::var_os("KAGEHA_CA_BUNDLE").or_else(||std::env::var_os("CURL_CA_BUNDLE")).or_else(||std::env::var_os("SSL_CERT_FILE"));
    let ca_bundle_pem=if let Some(path)=path {
        let mut pem=Vec::new();
        std::fs::File::open(path).map_err(|_|io::Error::other("Configured integration CA bundle unavailable"))?
            .take(1024*1024+1).read_to_end(&mut pem).map_err(|_|io::Error::other("Cannot read integration CA bundle"))?;
        Some(pem)
    } else {None};
    let bypass=std::env::var("no_proxy").ok().or_else(||std::env::var("NO_PROXY").ok());
    let mut proxies=Vec::new();
    if !mutual_tls {
        for (scope,lower,upper) in [(kageha_integrations::ProxyScope::Http,"http_proxy",None),
            (kageha_integrations::ProxyScope::Https,"https_proxy",Some("HTTPS_PROXY")),
            (kageha_integrations::ProxyScope::All,"all_proxy",Some("ALL_PROXY"))] {
            if let Some(endpoint)=std::env::var(lower).ok().or_else(||upper.and_then(|name|std::env::var(name).ok())).filter(|value|!value.is_empty()) {
                proxies.push(kageha_integrations::ProxySetting {scope,endpoint,bypass:bypass.clone()});
            }
        }
    }
    Ok(kageha_integrations::Config {ca_bundle_pem,proxies})
}
fn client() -> io::Result<kageha_integrations::HttpClient> {
    use std::sync::OnceLock;
    static CLIENT: OnceLock<Result<kageha_integrations::HttpClient, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            kageha_integrations::HttpClient::new(configuration(false).map_err(|e|e.to_string())?)
            .map_err(|e| e.to_string())
        })
        .as_ref()
        .cloned()
        .map_err(|e| io::Error::other(e.clone()))
}
pub(crate) fn http_mutual_tls(
    url:&str,method:&str,headers:&[(String,String)],body:Vec<u8>,cancel:&AtomicBool,timeout:Duration,limit:usize,
    identity_pem:&[u8],allowed_endpoint:&str,
) -> io::Result<Http> {
    kageha_integrations::HttpClient::new_mutual_tls(configuration(true)?,identity_pem,allowed_endpoint)?.request(
        url,method,headers,body,&kageha_contracts::execution::Control{cancelled:cancel,deadline:None},timeout,limit)
}
pub(crate) fn http(
    url: &str,
    method: &str,
    headers: &[(String, String)],
    body: Vec<u8>,
    cancel: &AtomicBool,
    timeout: Duration,
    limit: usize,
) -> io::Result<Http> {
    client()?.request(
        url,
        method,
        headers,
        body,
        &kageha_contracts::execution::Control {
            cancelled: cancel,
            deadline: None,
        },
        timeout,
        limit,
    )
}
pub(crate) fn json(
    url: &str,
    method: &str,
    headers: &[(String, String)],
    body: &serde_json::Value,
    cancel: &AtomicBool,
) -> io::Result<serde_json::Value> {
    let mut headers = headers.to_vec();
    headers.push(("Content-Type".into(), "application/json".into()));
    let result = http(
        url,
        method,
        &headers,
        if method == "GET" {
            vec![]
        } else {
            serde_json::to_vec(body)?
        },
        cancel,
        Duration::from_secs(30),
        8 * 1024 * 1024,
    )?;
    if !(200..300).contains(&result.status) {
        return Err(io::Error::other(format!(
            "Provider returned HTTP {}",
            result.status
        )));
    }
    serde_json::from_slice(&result.bytes).map_err(io::Error::other)
}
