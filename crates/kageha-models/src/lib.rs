//! Independent provider adapters implementing the runtime's model port.
//! Call synchronous ports from blocking execution threads, not an async reactor.
pub mod catalog;
pub mod decisions;
pub mod protocol;
mod chat_stream;
pub mod reviewer;
use kageha_contracts::credentials::{CredentialAccess, CredentialRef, CredentialResolver};
use kageha_contracts::execution::{AssistantMessage, Control, ModelPort, ModelRequest};
use serde_json::{json, Value};
use std::{
    io,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};
#[derive(Clone)]
pub struct Provider {
    pub url: String,
    pub key: String,
    pub model: String,
    pub protocol: String,
}
impl Provider {
    pub fn validate(&self) -> io::Result<()> {
        let url = reqwest::Url::parse(&self.url)
            .map_err(|_| io::Error::other("Invalid provider endpoint"))?;
        if self.url.len() > 2048
            || self.url.chars().any(char::is_control)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || !matches!(url.scheme(), "https" | "http")
            || self.key.chars().any(char::is_control)
            || !self.key.is_empty() && url.scheme() != "https" && !literal_loopback(&url)
            || self.model.trim().is_empty()
            || self.model.len() > 256
            || self.model.chars().any(char::is_control)
        {
            return Err(io::Error::other(
                "Provider credentials require HTTPS and valid endpoint configuration",
            ));
        }
        if !matches!(self.protocol.as_str(), "openai_compat" | "anthropic_compat" | "openai_embeddings" | "system_one" | "clef" | "openai_decisions") {
            return Err(io::Error::other("Unsupported provider protocol"));
        }
        Ok(())
    }
}
/// Local proxy tokens can cross literal loopback HTTP; remote secrets require TLS.
pub fn literal_loopback(url:&reqwest::Url)->bool {
    url.host_str().and_then(|host|host.trim_matches(['[',']']).parse::<std::net::IpAddr>().ok()).is_some_and(|ip|ip.is_loopback())
}
struct Transport {
    runtime: tokio::runtime::Runtime,
    client: reqwest::Client,
}
#[derive(Debug)]
struct TemporaryModelFailure {
    status: u16,
    after: Duration,
}
impl std::fmt::Display for TemporaryModelFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Model temporarily unavailable (HTTP {})", self.status)
    }
}
impl std::error::Error for TemporaryModelFailure {}
fn retry_delay(error: &io::Error) -> Option<Duration> {
    error
        .get_ref()?
        .downcast_ref::<TemporaryModelFailure>()
        .map(|e| e.after)
        .filter(|after| *after <= Duration::from_secs(60))
}
fn wait_for_model(delay: Duration, control: &Control<'_>) -> io::Result<()> {
    let end = Instant::now() + delay;
    while Instant::now() < end {
        control.check()?;
        std::thread::sleep(
            end.saturating_duration_since(Instant::now())
                .min(Duration::from_millis(50)),
        );
    }
    control.check()
}
/// Bounded model discovery through the same authenticated, pooled transport as completion.
pub fn discover_models(
    base: &str,
    key: &str,
    protocol: &str,
    control: &Control<'_>,
) -> io::Result<Vec<Value>> {
    control.check()?;
    if tokio::runtime::Handle::try_current().is_ok() {
        return Err(io::Error::other(
            "Invoke model ports on blocking execution threads",
        ));
    }
    if !catalog::supported(protocol){return Err(io::Error::other("Typed decision backends use a fixed question API, not chat model discovery"));}
    let completion = catalog::completion_url(base, protocol)?;
    let suffix = if protocol == "anthropic_compat" {
        "/messages"
    } else {
        "/chat/completions"
    };
    let url = format!("{}/models", completion.strip_suffix(suffix).unwrap());
    Provider {
        url: url.clone(),
        key: key.into(),
        model: "discovery".into(),
        protocol: protocol.into(),
    }
    .validate()?;
    let transport = transport()?;
    let deadline = control
        .deadline
        .unwrap_or(Instant::now() + Duration::from_secs(12))
        .min(Instant::now() + Duration::from_secs(12));
    transport.runtime.block_on(async {
        let operation = async {
            let mut builder = transport.client.get(&url);
            if protocol == "anthropic_compat" {
                builder = builder.header("anthropic-version", "2023-06-01");
                if !key.is_empty() { builder = builder.header("x-api-key", key); }
            } else if !key.is_empty() { builder = builder.bearer_auth(key); }
            let mut response = builder.send().await.map_err(|_|io::Error::other("Model discovery transport failed"))?;
            if !response.status().is_success() { return Err(io::Error::other(format!("Model discovery returned HTTP {}", response.status().as_u16()))); }
            let mut bytes=Vec::new();
            while let Some(chunk)=response.chunk().await.map_err(|_|io::Error::other("Model discovery interrupted"))? {
                if bytes.len()+chunk.len()>8*1024*1024 { return Err(io::Error::other("Model discovery exceeds limit")); }
                bytes.extend_from_slice(&chunk);
            }
            let value:Value=serde_json::from_slice(&bytes).map_err(|_|io::Error::other("Invalid model discovery JSON"))?;
            let rows=value["data"].as_array().filter(|v|v.len()<=10000).ok_or_else(||io::Error::other("A bounded model list was not returned"))?;
            Ok(rows.clone())
        };
        tokio::select! {
            result=operation => result,
            _=async {loop {if control.check().is_err() {break;} tokio::time::sleep(Duration::from_millis(10)).await;}} => {control.check()?;Err(io::Error::new(io::ErrorKind::Interrupted,"Model discovery cancelled"))},
            _=tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => Err(io::Error::new(io::ErrorKind::TimedOut,"Model discovery deadline exceeded")),
        }
    })
}
fn transport() -> io::Result<Arc<Transport>> {
    static SHARED: OnceLock<Result<Arc<Transport>, String>> = OnceLock::new();
    SHARED
        .get_or_init(|| {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map_err(|_| "Model I/O runtime unavailable".to_owned())?;
            let mut builder = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(10))
                .pool_max_idle_per_host(8);
            // Retain operator-configured trust roots when replacing curl transport.
            let ca = std::env::var_os("KAGEHA_CA_BUNDLE")
                .or_else(|| std::env::var_os("CURL_CA_BUNDLE"))
                .or_else(|| std::env::var_os("SSL_CERT_FILE"));
            if let Some(path) = ca {
                use std::io::Read;
                let mut pem = Vec::new();
                std::fs::File::open(path)
                    .map_err(|_| "Configured model CA bundle unavailable".to_owned())?
                    .take(1024 * 1024 + 1)
                    .read_to_end(&mut pem)
                    .map_err(|_| "Cannot read configured model CA bundle".to_owned())?;
                if pem.len() > 1024 * 1024 {
                    return Err("Model CA bundle exceeds limit".into());
                }
                let certs = reqwest::Certificate::from_pem_bundle(&pem)
                    .map_err(|_| "Invalid model CA bundle".to_owned())?;
                if certs.is_empty() {
                    return Err("Empty model CA bundle".into());
                }
                builder = builder.tls_built_in_root_certs(false);
                for cert in certs {
                    builder = builder.add_root_certificate(cert);
                }
            }
            let client = builder
                .build()
                .map_err(|_| "Model HTTP client unavailable".to_owned())?;
            Ok(Arc::new(Transport { runtime, client }))
        })
        .as_ref()
        .cloned()
        .map_err(|e| io::Error::other(e.clone()))
}
pub struct HttpModel {
    providers: Vec<Provider>,
    transport: Arc<Transport>,
    timeout: Duration,
    resolver: Option<Arc<dyn CredentialResolver>>,
    references: Vec<Option<CredentialBinding>>,
    activity: Option<ModelActivityObserver>,
}
pub type ModelActivityObserver = Arc<dyn Fn(ModelActivity) + Send + Sync>;

pub enum ModelActivity {
    Requesting { model: String, fallback: bool },
    Backoff { seconds: u64 },
    Text { id: String, offset: usize, text: String },
    DiscardText { id: String },
}
#[derive(Clone, Debug)]
pub struct CredentialBinding {
    pub reference: CredentialRef,
    pub access: CredentialAccess,
    /// Only an explicitly anonymous application source may authorize an empty lease.
    pub allow_empty: bool,
}
impl HttpModel {
    pub fn new(providers: Vec<Provider>) -> io::Result<Self> {
        if providers.is_empty() || providers.len() > 16 {
            return Err(io::Error::other("Configure one to sixteen model adapters"));
        }
        for provider in &providers {
            provider.validate()?;
        }
        Ok(Self {
            providers,
            transport: transport()?,
            timeout: Duration::from_secs(120),
            resolver: None,
            references: Vec::new(),
            activity: None,
        })
    }
    /// Resolve a scoped lease per call. No credential value is retained in provider configuration.
    pub fn with_credentials(
        providers: Vec<(Provider, CredentialBinding)>,
        resolver: Arc<dyn CredentialResolver>,
    ) -> io::Result<Self> {
        if providers.iter().any(|(p, _)| !p.key.is_empty()) {
            return Err(io::Error::other(
                "Referenced providers must not contain an inline credential",
            ));
        }
        for (provider, binding) in &providers {
            let url = reqwest::Url::parse(&provider.url)
                .map_err(|_| io::Error::other("Invalid model endpoint"))?;
            if binding.access.destination != url.origin().ascii_serialization()
                || url.query().is_some()
            {
                return Err(io::Error::other(
                    "Model credential destination must match the actual endpoint origin",
                ));
            }
            binding
                .reference
                .validate()
                .map_err(|_| io::Error::other("Invalid model credential reference"))?;
        }
        Self::with_optional_credentials(
            providers.into_iter().map(|(p, b)| (p, Some(b))).collect(),
            resolver,
        )
    }
    pub fn with_optional_credentials(
        providers: Vec<(Provider, Option<CredentialBinding>)>,
        resolver: Arc<dyn CredentialResolver>,
    ) -> io::Result<Self> {
        for (provider, binding) in &providers {
            if let Some(binding) = binding {
                if !provider.key.is_empty() {
                    return Err(io::Error::other(
                        "Referenced providers must not contain an inline credential",
                    ));
                }
                let url = reqwest::Url::parse(&provider.url)
                    .map_err(|_| io::Error::other("Invalid model endpoint"))?;
                if binding.access.destination != url.origin().ascii_serialization()
                    || url.query().is_some()
                {
                    return Err(io::Error::other(
                        "Model credential destination must match the actual endpoint origin",
                    ));
                }
                binding
                    .reference
                    .validate()
                    .map_err(|_| io::Error::other("Invalid model credential reference"))?;
            }
        }
        let (configs, references) = providers.into_iter().unzip();
        let mut model = Self::new(configs)?;
        model.resolver = Some(resolver);
        model.references = references;
        Ok(model)
    }
    /// Keep the configured transport and credential authority for one adapter.
    pub fn select_chat_adapter(&self, index: usize) -> Option<Self> {
        let provider = self.providers.get(index)?;
        if !matches!(provider.protocol.as_str(), "openai_compat" | "anthropic_compat") {
            return None;
        }
        Some(Self {
            providers: vec![provider.clone()],
            transport: self.transport.clone(),
            timeout: self.timeout,
            resolver: self.resolver.clone(),
            references: vec![self.references.get(index).cloned().flatten()],
            activity: self.activity.clone(),
        })
    }

    pub fn with_activity(mut self, activity: Option<ModelActivityObserver>) -> Self {
        self.activity = activity;
        self
    }

    pub fn select_embedding_adapter(self: &Arc<Self>, index: usize) -> Option<EmbeddingAdapter> {
        (self.providers.get(index)?.protocol == "openai_embeddings").then(|| EmbeddingAdapter {
            model: self.clone(),
            index,
        })
    }

    pub fn with_timeout(mut self, timeout: Duration) -> io::Result<Self> {
        if timeout.is_zero() || timeout > Duration::from_secs(120) {
            return Err(io::Error::other(
                "Model timeout must be positive and at most 120 seconds",
            ));
        }
        self.timeout = timeout;
        Ok(self)
    }
    fn call(
        &self,
        provider: &Provider,
        binding: Option<&CredentialBinding>,
        request: &ModelRequest,
        control: &Control<'_>,
    ) -> io::Result<AssistantMessage> {
        let route = model_route(provider, binding);
        let mut body = if matches!(provider.protocol.as_str(),"system_one"|"clef"|"openai_decisions") {
            if !request.tools.is_empty()||request.messages.len()!=1||request.messages[0]["role"]!="user" {return Err(io::Error::other("Typed backend accepts only fixed questions"));}
            let typed:kageha_contracts::decisions::Request=serde_json::from_str(request.messages[0]["content"].as_str().ok_or_else(||io::Error::other("Missing typed payload"))?).map_err(io::Error::other)?;
            let selector=if provider.protocol=="clef"{provider.model.rsplit('/').next().unwrap_or(&provider.model)}else{&provider.model};
            if provider.protocol=="openai_decisions" { decisions::openai_payload(&typed,selector)? } else { typed.payload(selector)? }
        } else if provider.protocol == "anthropic_compat" {
            protocol::anthropic_request(&provider.model, &request.messages, &request.tools)?
        } else {
            openai_request(&provider.model, &route, request)?
        };
        if provider.protocol == "openai_compat" && self.activity.is_some() {
            body["stream"] = json!(true);
        }
        let value = self.request_json(provider, binding, body, control)?;
        if matches!(provider.protocol.as_str(), "system_one" | "clef" | "openai_decisions") {
            let result = if provider.protocol == "openai_decisions" { decisions::openai_result(&value)? } else { value };
            return Ok(AssistantMessage { continuation: None, content: json!(result.to_string()), tool_calls: vec![] });
        }
        let legacy = if provider.protocol == "anthropic_compat" {
            protocol::anthropic_response(&value)?
        } else {
            openai_response(&value, &route)?
        };
        AssistantMessage::from_legacy(&legacy)
    }

    fn request_json(
        &self,
        provider: &Provider,
        binding: Option<&CredentialBinding>,
        body: Value,
        control: &Control<'_>,
    ) -> io::Result<Value> {
        let bytes = serde_json::to_vec(&body)?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err(io::Error::other("Model request exceeds limit"));
        }
        let deadline = control
            .deadline
            .unwrap_or(Instant::now() + self.timeout)
            .min(Instant::now() + self.timeout);
        self.transport.runtime.block_on(async {
            let operation=async {
                let lease=if let(Some(resolver),Some(binding))=(&self.resolver,binding){Some(resolver.resolve(&binding.access,&binding.reference,control).await.map_err(|_|io::Error::new(io::ErrorKind::PermissionDenied,"Model credential resolution denied or unavailable"))?)}else{None};
                let key=if let Some(lease)=&lease {std::str::from_utf8(lease.secret.expose()).map_err(|_|io::Error::other("Model credential is not UTF-8"))?}else{provider.key.as_str()};
                if binding.is_some_and(|b|!b.allow_empty)&&key.is_empty(){return Err(io::Error::new(io::ErrorKind::PermissionDenied,"Explicit model credential is empty"));}
                if key.chars().any(char::is_control)||!key.is_empty()&&!provider.url.starts_with("https://")&&!reqwest::Url::parse(&provider.url).is_ok_and(|url|literal_loopback(&url)){return Err(io::Error::other("Model credentials require TLS or literal loopback and valid headers"));}
                let mut builder=self.transport.client.post(&provider.url).header("Content-Type","application/json").body(bytes);
                if provider.protocol=="anthropic_compat" {
                    builder=builder.header("anthropic-version","2023-06-01");
                    if !key.is_empty(){builder=builder.header("x-api-key",key);}
                } else if !key.is_empty(){builder=builder.bearer_auth(key);}
                if let(Some(resolver),Some(binding),Some(lease))=(&self.resolver,binding,&lease){resolver.validate_lease(&binding.access,lease,control).await.map_err(|_|io::Error::new(io::ErrorKind::PermissionDenied,"Model credential changed before dispatch"))?;}
                control.check()?;
                // Error text never includes endpoint URLs, credentials or remote response bodies.
                let mut response=builder.send().await.map_err(|_|io::Error::other("Model transport failed"))?;
                if !response.status().is_success(){
                    let status=response.status().as_u16();
                    if status==429 || (500..600).contains(&status) {
                        // An unsupported date-form Retry-After must not cause an
                        // early retry. Leave it to the caller rather than guess.
                        let after=response.headers().get("retry-after").map(|h|h.to_str().ok().and_then(|h|h.parse::<u64>().ok()).unwrap_or(61)).unwrap_or(10);
                        return Err(io::Error::other(TemporaryModelFailure{status,after:Duration::from_secs(after.max(1))}));
                    }
                    return Err(io::Error::other(format!("Model returned HTTP {status}")));
                }
                if response.content_length().is_some_and(|n|n>8*1024*1024){return Err(io::Error::other("Model response exceeds limit"));}
                let streamed = response.headers().get(reqwest::header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(|value| value.split(';').next() == Some("text/event-stream"));
                static STREAM_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                let mut stream = streamed.then(|| chat_stream::ChatStream::new(self.activity.as_ref(),
                    format!("{}-{}", std::process::id(), STREAM_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed))));
                let mut bytes=Vec::new();
                let mut received = 0;
                while let Some(chunk)=response.chunk().await.map_err(|_|io::Error::other("Model response interrupted"))? {
                    received += chunk.len();
                    if received>8*1024*1024{return Err(io::Error::other("Model response exceeds limit"));}
                    if let Some(stream) = &mut stream {
                        stream.push(&chunk)?;
                        if stream.ended { break; }
                    }
                    else { bytes.extend_from_slice(&chunk); }
                }
                if let Some(stream) = stream { return stream.finish(); }
                serde_json::from_slice(&bytes).map_err(|_| io::Error::other("Invalid model response JSON"))
            };
            tokio::select! {
                result=operation => result,
                _=async {loop {control.check()?;tokio::time::sleep(Duration::from_millis(10)).await; } #[allow(unreachable_code)] Ok::<(),io::Error>(())} => {control.check()?;Err(io::Error::new(io::ErrorKind::Interrupted,"Model request cancelled"))},
                _=tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => Err(io::Error::new(io::ErrorKind::TimedOut,"Model deadline exceeded")),
            }
        })
    }
}
impl ModelPort for HttpModel {
    fn complete(
        &self,
        request: ModelRequest,
        control: &Control<'_>,
    ) -> io::Result<AssistantMessage> {
        control.check()?;
        if tokio::runtime::Handle::try_current().is_ok() {
            return Err(io::Error::other(
                "Invoke model ports on blocking execution threads",
            ));
        }
        // The timeout is a budget for the complete model response, including
        // provider backoff and fallback, rather than a new budget per attempt.
        let deadline = Instant::now() + self.timeout;
        let control = &Control {
            cancelled: control.cancelled,
            deadline: Some(control.deadline.map_or(deadline, |caller| caller.min(deadline))),
        };
        let mut last = io::Error::other("No eligible model adapter");
        for (index, provider) in self.providers.iter().enumerate() {
            if provider.protocol == "openai_embeddings" { continue; }
            for attempt in 0..=4 {
                control.check()?;
                if let Some(observe) = &self.activity {
                    observe(ModelActivity::Requesting {
                        model: provider.model.chars().take(100).collect(),
                        fallback: index > 0,
                    });
                }
                match self.call(
                    provider,
                    self.references.get(index).and_then(Option::as_ref),
                    &request,
                    control,
                ) {
                    Ok(reply) => {
                        control.check()?;
                        return Ok(reply);
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::Interrupted | io::ErrorKind::TimedOut
                        ) =>
                    {
                        return Err(error)
                    }
                    Err(error) => {
                        if attempt < 4 {
                            if let Some(delay) = retry_delay(&error).filter(|delay| {
                                control.deadline.is_some_and(|end| *delay < end.saturating_duration_since(Instant::now()))
                            }) {
                                if let Some(observe) = &self.activity {
                                    observe(ModelActivity::Backoff { seconds: delay.as_secs() });
                                }
                                wait_for_model(delay, control)?;
                                continue;
                            }
                        }
                        last = error;
                        break;
                    }
                }
            }
        }
        Err(last)
    }
}
fn model_route(provider: &Provider, binding: Option<&CredentialBinding>) -> String {
    use sha2::{Digest, Sha256};
    let identity = json!([provider.protocol, provider.url, provider.model,
        binding.map(|binding| &binding.reference)]);
    let mut hash = Sha256::new();
    hash.update(identity.to_string());
    if binding.is_none() {
        hash.update([0]);
        hash.update(provider.key.as_bytes());
    }
    format!("{:x}", hash.finalize())
}

fn openai_request(model: &str, route: &str, request: &ModelRequest) -> io::Result<Value> {
    let mut messages = request.messages.clone();
    for message in &mut messages {
        let fields = message
            .as_object_mut()
            .ok_or_else(|| io::Error::other("Invalid conversation message"))?;
        fields.remove("is_error");
        let continuation = fields.remove("continuation")
            .and_then(|state| serde_json::from_value::<kageha_contracts::execution::ModelContinuation>(state).ok())
            .filter(|state| state.route == route);
        if let Some(calls) = fields.get("tool_calls").and_then(Value::as_array) {
            if calls.is_empty() {
                fields.remove("tool_calls");
                continue;
            }
            let calls: Vec<_> = calls.iter().map(|call| {
                let mut wire = json!({"id":call["id"],"type":"function","function":{
                    "name":call["name"],"arguments":call["arguments"].to_string()
                }});
                if let Some(signature) = continuation.as_ref()
                    .and_then(|state| state.tool_signatures.get(call["id"].as_str()?))
                {
                    wire["extra_content"] = json!({"google":{"thought_signature":signature}});
                }
                wire
            }).collect();
            fields.insert("tool_calls".into(), json!(calls));
        }
    }
    let mut body=json!({"model":model,"messages":messages});
    if !request.tools.is_empty(){body["tools"]=json!(request.tools);}
    Ok(body)
}
fn openai_response(response: &Value, route: &str) -> io::Result<Value> {
    let choice = &response["choices"][0];
    if choice["finish_reason"] == "length" {
        return Err(io::Error::other("Model output limit reached before the response completed"));
    }
    let message = &choice["message"];
    if message["role"] != "assistant" {
        return Err(io::Error::other(
            "Provider did not return an assistant message",
        ));
    }
    let mut calls = Vec::new();
    let mut signatures = std::collections::BTreeMap::new();
    if let Some(raw) = message.get("tool_calls").filter(|v| !v.is_null()) {
        let raw = raw
            .as_array()
            .ok_or_else(|| io::Error::other("Invalid model tool calls"))?;
        for call in raw {
            let arguments: Value = serde_json::from_str(
                call["function"]["arguments"]
                    .as_str()
                    .ok_or_else(|| io::Error::other("Invalid tool arguments"))?,
            )
            .map_err(|_| io::Error::other("Invalid tool argument JSON"))?;
            if let (Some(id), Some(signature)) = (
                call["id"].as_str(),
                call["extra_content"]["google"]["thought_signature"].as_str(),
            ) {
                signatures.insert(id.to_owned(), signature.to_owned());
            }
            calls.push(
                json!({"id":call["id"],"name":call["function"]["name"],"arguments":arguments}),
            );
        }
    }
    let content = match message["content"].as_str() {
        Some(text) if text.contains("<|channel|>") || text.contains("<|start|>") || text.contains("<|constrain|>") => {
            let final_text = text.rsplit_once("<|channel|>final<|message|>")
                .map(|(_, text)| text.split("<|").next().unwrap_or_default().trim())
                .filter(|text| !text.is_empty());
            if final_text.is_none() && calls.is_empty() {
                return Err(io::Error::other("Provider returned internal model text without a final answer"));
            }
            json!(final_text)
        }
        _ => message["content"].clone(),
    };
    let mut message = json!({"role":"assistant","content":content,"tool_calls":calls});
    if !signatures.is_empty() {
        message["continuation"] = json!({"route":route,"tool_signatures":signatures});
    }
    Ok(json!({"message":message}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn provider_control_tokens_do_not_become_the_final_answer() {
        let response = |content| json!({"choices":[{"message":{"role":"assistant","content":content}}]});
        assert!(openai_response(&response("Checking.<|start|>commentary to=functions.ask_human<|constrain|>{}"), "").is_err());
        let reply = openai_response(&response("<|channel|>analysis<|message|>Private<|channel|>final<|message|>Verified."), "").unwrap();
        assert_eq!(reply["message"]["content"], "Verified.");
    }

    #[test]
    fn truncated_completion_is_not_published_as_success() {
        let response = json!({"choices":[{"finish_reason":"length","message":{"role":"assistant","content":"Partial answer"}}]});
        assert!(openai_response(&response, "").unwrap_err().to_string().contains("output limit"));
    }

    #[test]
    fn temporary_provider_wait_is_bounded_and_cancellable() {
        assert_eq!(
            retry_delay(&io::Error::other(TemporaryModelFailure {
                status: 429,
                after: Duration::from_secs(5)
            })),
            Some(Duration::from_secs(5))
        );
        assert!(retry_delay(&io::Error::other(TemporaryModelFailure {
            status: 429,
            after: Duration::from_secs(61)
        }))
        .is_none());
        assert!(retry_delay(&io::Error::other("HTTP 401")).is_none());
        let cancelled = std::sync::atomic::AtomicBool::new(true);
        let error = wait_for_model(
            Duration::from_secs(60),
            &Control {
                cancelled: &cancelled,
                deadline: None,
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    }
    #[test]
    fn endpoint_and_credentials_are_validated_before_execution() {
        let mut config = Provider {
            url: "http://127.0.0.1:1/v1".into(),
            key: String::new(),
            model: "fixture".into(),
            protocol: "openai_compat".into(),
        };
        assert!(config.validate().is_ok());
        config.key = "secret".into();
        assert!(config.validate().is_ok());
        for endpoint in ["http://example.com/v1","http://127.0.0.1.example.com/v1","http://192.168.1.10/v1","http://localhost/v1"] {
            config.url=endpoint.into();assert!(config.validate().is_err());
        }
        config.key.clear();
        config.url = "http://example.com/v1".into();
        assert!(config.validate().is_ok());
        for endpoint in [
            "https://user:secret@example.com",
            "file:///tmp/fixture",
            "https://example.com/#fragment",
        ] {
            config.url = endpoint.into();
            assert!(config.validate().is_err());
        }
    }
    #[test]
    fn provider_formats_are_normalized_without_changing_call_order() {
        let request = ModelRequest {
            role: Default::default(),
            messages: vec![
                json!({"role":"assistant","content":null,"tool_calls":[{"id":"a","name":"echo","arguments":{"n":1}}]}),
                json!({"role":"tool","tool_call_id":"a","content":"result","is_error":false}),
            ],
            tools: vec![],
        };
        let wire = openai_request("model", "", &request).unwrap();
        assert!(wire.get("tools").is_none(),"Empty optional tools must be omitted for compatible gateways");
        assert_eq!(
            wire["messages"][0]["tool_calls"][0]["function"]["arguments"],
            "{\"n\":1}"
        );
        assert!(wire["messages"][1].get("is_error").is_none());
        let reply=openai_response(&json!({"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":"a","function":{"name":"echo","arguments":"{\"n\":1}"}}]}}]}), "").unwrap();
        assert_eq!(
            AssistantMessage::from_legacy(&reply).unwrap().tool_calls[0].id,
            "a"
        );
    }
    #[test]
    fn invalid_tool_arguments_never_escape_the_model_adapter() {
        let reply=openai_response(&json!({"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":"a","function":{"name":"echo","arguments":"[1]"}}]}}]}), "").unwrap();
        assert!(AssistantMessage::from_legacy(&reply).is_err());
        assert!(openai_response(
            &json!({"choices":[{"message":{"role":"assistant","tool_calls":"invalid"}}]}), ""
        )
        .is_err());
    }

    #[test]
    fn tool_continuation_round_trips_only_to_its_issuing_model_route() {
        let route = "a".repeat(64);
        let response = json!({"choices":[{"message":{"role":"assistant","tool_calls":[
            {"id":"first","function":{"name":"echo","arguments":"{}"},
             "extra_content":{"google":{"thought_signature":"opaque"},"unexpected":"discard"}},
            {"id":"second","function":{"name":"echo","arguments":"{}"}}
        ]}}]});
        let parsed = openai_response(&response, &route).unwrap();
        let assistant = AssistantMessage::from_legacy(&parsed).unwrap();
        assert_eq!(assistant.tool_calls[0].arguments, json!({}));
        assert!(!serde_json::to_string(&assistant.tool_calls).unwrap().contains("opaque"));
        let request = ModelRequest {
            role: Default::default(),
            messages: vec![assistant.legacy()["message"].clone(),
                json!({"role":"tool","tool_call_id":"first","content":"first result"}),
                json!({"role":"tool","tool_call_id":"second","content":"second result"})],
            tools: vec![],
        };
        let wire = openai_request("model", &route, &request).unwrap();
        assert_eq!(wire["messages"][0]["tool_calls"][0]["extra_content"],
            json!({"google":{"thought_signature":"opaque"}}));
        assert!(wire["messages"][0]["tool_calls"][1].get("extra_content").is_none());
        assert!(wire["messages"][0].get("continuation").is_none());
        let other = openai_request("different-model", &"b".repeat(64), &request).unwrap();
        assert!(!other.to_string().contains("opaque"));
    }
}

pub mod embedding;
pub use embedding::EmbeddingAdapter;
