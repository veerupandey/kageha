//! Reusable client-side model/tool adapters for the standalone runtime bridge.
mod inline_images;
use inline_images::InlineImages;
use crate::bridge::{encode, Command};
use kageha_contracts::execution::*;
use kageha_runtime::{lifecycle::RunSpec, turn::Start};
use serde_json::{json, Value};
use std::{
    io::{self, Read},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
#[derive(Clone)]
pub struct Client {
    base: String,
    token: Arc<String>,
    http: reqwest::blocking::Client,
}
/// Older bridge journals stored only the fixed deadline message.
pub fn deadline_failure(result: &Value) -> bool {
    result["deadline_exceeded"] == true
        || matches!(result["error_code"].as_str(), Some("deadline" | "deadline_exceeded"))
        || result["failure_code"] == "deadline_exceeded"
        || result["message"] == "Execution deadline exceeded"
}

impl Client {
    pub fn new(base: &str, token: String) -> io::Result<Self> {
        let url = reqwest::Url::parse(base)
            .map_err(|_| io::Error::other("Invalid runtime service URL"))?;
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !matches!(url.scheme(), "http" | "https")
            || url.scheme() == "http"
                && !matches!(
                    url.host_str(),
                    Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
                )
            || token.len() < 32
            || token.chars().any(char::is_whitespace)
        {
            return Err(io::Error::other(
                "Runtime service requires scoped credentials and HTTPS outside loopback",
            ));
        }
        let mut builder = reqwest::blocking::Client::builder()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5))
            .connect_timeout(Duration::from_secs(2));
        if let Some(path) =
            std::env::var_os("KAGEHA_CA_BUNDLE").or_else(|| std::env::var_os("SSL_CERT_FILE"))
        {
            let pem = std::fs::read(path)?;
            if pem.len() > 1024 * 1024 {
                return Err(io::Error::other("CA bundle exceeds limit"));
            }
            for cert in reqwest::Certificate::from_pem_bundle(&pem).map_err(io::Error::other)? {
                builder = builder.add_root_certificate(cert);
            }
        }
        Ok(Self {
            base: base.trim_end_matches('/').into(),
            token: Arc::new(token),
            http: builder.build().map_err(io::Error::other)?,
        })
    }
    pub fn from_env() -> io::Result<Option<Self>> {
        let Some(url) = std::env::var_os("KAGEHA_RUNTIME_SERVICE_URL") else {
            return Ok(None);
        };
        let path = std::env::var_os("KAGEHA_RUNTIME_SERVICE_TOKEN_FILE")
            .ok_or_else(|| io::Error::other("Runtime service token file missing"))?;
        let meta = std::fs::symlink_metadata(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if meta.permissions().mode() & 0o077 != 0 {
                return Err(io::Error::other("Runtime service token must be private"));
            }
        }
        if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 8192 {
            return Err(io::Error::other("Invalid runtime service token file"));
        }
        Self::new(
            &url.to_string_lossy(),
            std::fs::read_to_string(path)?.trim().into(),
        )
        .map(Some)
    }
    fn owner(&self, id: &str) -> String {
        use sha2::{Digest, Sha256};
        format!(
            "{:x}",
            Sha256::digest(format!("{}:{id}", self.token).as_bytes())
        )
    }
    fn request(&self, path: &str, id: Option<&str>, body: Option<&Value>) -> io::Result<Value> {
        let mut req = if body.is_some() {
            self.http.post(format!("{}{path}", self.base))
        } else {
            self.http.get(format!("{}{path}", self.base))
        }
        .bearer_auth(self.token.as_str());
        if let Some(id) = id {
            req = req.header("x-kageha-port-owner", self.owner(id));
        }
        if let Some(body) = body {
            req = req.json(body);
        }
        let response = req.send().map_err(|_| {
            io::Error::new(io::ErrorKind::ConnectionAborted, "Standalone runtime transport unavailable; no local fallback was used")
        })?;
        if !response.status().is_success() {
            return Err(io::Error::other(format!(
                "Standalone runtime returned {}",
                response.status()
            )));
        }
        let mut bytes = Vec::new();
        response.take(8 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err(io::Error::other("Runtime response exceeds limit"));
        }
        serde_json::from_slice(&bytes).map_err(io::Error::other)
    }
    pub fn info(&self) -> io::Result<Value> {
        self.request("/v1/info", None, None)
    }
    pub fn status(&self, id: &str) -> io::Result<Value> {
        self.request(&format!("/v1/runs/{id}"), Some(id), None)
    }
    pub fn cancel(&self, id: &str) -> io::Result<Value> {
        self.request(&format!("/v1/runs/{id}/cancel"), Some(id), Some(&json!({})))
    }
    pub fn execute(
        &self,
        mut spec: RunSpec,
        mut start: Start,
        model: &(dyn ModelPort + Sync),
        tools: &(dyn ToolPort + Sync),
        control: &Control<'_>,
        mut observe: impl FnMut(&Value) -> io::Result<()>,
    ) -> io::Result<String> {
        // Image bytes originate on this host. Keep stable references on the wire
        // and restore the original messages at the model and checkpoint boundaries.
        let mut images = InlineImages::default();
        images.pack(&mut start.messages)?;
        let images = Mutex::new(images);
        spec.id = spec.turn.clone();
        let id = spec.id.clone();
        spec.input = json!({"owner":self.owner(&id),"start":start,"recovery_protocol":1});
        let initial = self.request("/v1/runs", Some(&id), Some(&serde_json::to_value(&spec)?))?;
        let active = AtomicUsize::new(0);
        let fatal = Mutex::new(None::<String>);
        let local_cancel = std::sync::atomic::AtomicBool::new(false);
        struct CancelOnDrop<'a>(&'a std::sync::atomic::AtomicBool);
        impl Drop for CancelOnDrop<'_> {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let callback_control = Control {
            cancelled: &local_cancel,
            deadline: control.deadline,
        };
        std::thread::scope(|scope| {
            let _cancel_on_return = CancelOnDrop(&local_cancel);
            let mut current = initial;
            loop {
                if let Err(error) = control.check() {
                    let _ = self.cancel(&id);
                    return Err(error);
                }
                if let Some(message) = fatal
                    .lock()
                    .map_err(|_| io::Error::other("Port response state unavailable"))?
                    .take()
                {
                    let _ = self.cancel(&id);
                    return Err(uncertain_stop(message));
                }
                match current["state"].as_str() {
                    Some("succeeded") => {
                        return current["completion"]["result"]["message"]
                            .as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| io::Error::other("Runtime completion missing"))
                    }
                    Some("failed" | "interrupted" | "cancelled") => {
                        let message = current["completion"]["result"]["message"]
                            .as_str()
                            .unwrap_or("Standalone turn stopped");
                        return Err(if current["state"] == "cancelled" {
                            io::Error::new(io::ErrorKind::Interrupted, message)
                        } else if current["completion"]["result"]["uncertain"] == true {
                            uncertain_stop(message)
                        } else if deadline_failure(&current["completion"]["result"]) {
                            io::Error::new(io::ErrorKind::TimedOut, message)
                        } else if current["completion"]["result"]["failure_code"] == "authorization_required" {
                            io::Error::new(io::ErrorKind::PermissionDenied, message)
                        } else if current["completion"]["result"]["failure_code"] == "runtime_disconnected" {
                            io::Error::new(io::ErrorKind::ConnectionAborted, message)
                        } else {
                            io::Error::other(message.to_owned())
                        });
                    }
                    _ => {}
                }
                let packet = match self.request(&format!("/v1/runs/{id}/commands"), Some(&id), None)
                {
                    Ok(packet) => packet,
                    Err(error) => {
                        let _ = self.cancel(&id);
                        return Err(error);
                    }
                };
                if let Some(run) = packet.get("run") {
                    current = run.clone();
                }
                let commands: Vec<Command> = serde_json::from_value(packet["commands"].clone())?;
                let empty = commands.is_empty();
                for mut command in commands {
                    // The daemon lists unclaimed commands and journals each claim.
                    // Bound concurrent callbacks independently of the lifetime of the turn.
                    if command.kind != "observe" && active.load(Ordering::Acquire) >= 4 {
                        let _ = self.cancel(&id);
                        return Err(io::Error::other("Runtime port concurrency exceeded"));
                    }
                    control.check()?;
                    let prefix = format!("/v1/runs/{id}/commands/{}", command.id);
                    let claim =
                        self.request(&format!("{prefix}/claim"), Some(&id), Some(&json!({})))?;
                    if claim["claimed"] != true {
                        return Err(uncertain_stop(
                            "Port command was already claimed; input was not repeated",
                        ));
                    }
                    if command.kind == "observe" {
                        let reply = encode((|| {
                            let messages = match command.payload["kind"].as_str() {
                                Some("model") => command.payload["messages"].as_array_mut(),
                                Some("tool_results") => command.payload.get_mut("after").and_then(Value::as_array_mut),
                                _ => None,
                            };
                            if let Some(messages) = messages {
                                images.lock().map_err(|_| io::Error::other("Image store unavailable"))?.restore(messages)?;
                            }
                            observe(&command.payload).map(|_| Value::Null)
                        })());
                        self.request(&format!("{prefix}/reply"), Some(&id), Some(&reply))?;
                        continue;
                    }
                    let client = self.clone();
                    let id = id.clone();
                    let active = &active;
                    let images = &images;
                    let fatal = &fatal;
                    let control = &callback_control;
                    let budget = kageha_contracts::request_budget::current();
                    active.fetch_add(1, Ordering::AcqRel);
                    std::thread::Builder::new()
                        .name("kageha-client-port".into())
                        .spawn_scoped(scope, move || {
                            let reply = encode(
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                                    || kageha_contracts::request_budget::inherited(budget, || -> io::Result<Value> {
                                        control.check()?;
                                        match command.kind.as_str() {
                                            "discover" => serde_json::to_value(tools.discover()?)
                                                .map_err(io::Error::other),
                                            "access" => {
                                                serde_json::to_value(tools.dispatch_access(
                                                    &serde_json::from_value(command.payload)?,
                                                )?)
                                                .map_err(io::Error::other)
                                            }
                                            "checkpoint" => {
                                                serde_json::to_value(tools.checkpoint()?)
                                                    .map_err(io::Error::other)
                                            }
                                            "recovery_revision" => serde_json::to_value(tools.recovery_revision(&serde_json::from_value(command.payload)?)?).map_err(io::Error::other),
                                            "persistence_policy" => serde_json::to_value(tools.persistence_policy()?).map_err(io::Error::other),
                                            "recovery_checkpoint" => tools.recovery_checkpoint(),
                                            "save_recovery" => tools.save_recovery(&command.payload).map(|_|Value::Null),
                                            "on_compaction" => tools.on_compaction(&serde_json::from_value(command.payload)?).map(|_|Value::Null),
                                            "plan_snapshot" => serde_json::to_value(tools.plan_snapshot()?).map_err(io::Error::other),
                                            "take_attachments" => {
                                                let mut messages = tools.take_attachments()?;
                                                images.lock().map_err(|_| io::Error::other("Image store unavailable"))?.pack(&mut messages)?;
                                                Ok(Value::Array(messages))
                                            }
                                            "context_window" => Ok(json!(model.context_window())),
                                            "completion_check" => serde_json::to_value(tools.completion_check(command.payload["candidate"].as_str().unwrap_or(""),control)?).map_err(io::Error::other),
                                            "model" => {
                                                let mut request: ModelRequest = serde_json::from_value(command.payload)?;
                                                images.lock().map_err(|_| io::Error::other("Image store unavailable"))?.restore(&mut request.messages)?;
                                                serde_json::to_value(model.complete(request, control)?).map_err(io::Error::other)
                                            }
                                            "tool" => serde_json::to_value(tools.execute(
                                                &serde_json::from_value(command.payload)?,
                                                control,
                                            )?)
                                            .map_err(io::Error::other),
                                            _ => Err(io::Error::other(
                                                "Unknown runtime port request",
                                            )),
                                        }
                                    }),
                                ))
                                .unwrap_or_else(|_| {
                                    Err(uncertain_stop(
                                        "Application adapter panicked; do not repeat its input",
                                    ))
                                }),
                            );
                            active.fetch_sub(1, Ordering::AcqRel);
                            let mut error = None;
                            // Retry publication only, never the adapter call itself.
                            for _ in 0..3 {
                                match client.request(
                                    &format!("{prefix}/reply"),
                                    Some(&id),
                                    Some(&reply),
                                ) {
                                    Ok(_) => {
                                        error = None;
                                        break;
                                    }
                                    Err(e) => {
                                        error = Some(e.to_string());
                                        std::thread::sleep(Duration::from_millis(20));
                                    }
                                }
                            }
                            if let Some(error) = error {
                                if let Ok(mut fatal) = fatal.lock() {
                                    *fatal = Some(error);
                                }
                            }
                        })
                        .map_err(io::Error::other)?;
                }
                if empty {
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
        })
    }
}

#[cfg(test)]
mod reliability_tests {
    use super::*;
    #[test]
    fn recognizes_legacy_and_structured_deadlines() {
        assert!(deadline_failure(&serde_json::json!({"message":"Execution deadline exceeded"})));
        assert!(deadline_failure(&serde_json::json!({"deadline_exceeded":true})));
        assert!(!deadline_failure(&serde_json::json!({"message":"unrelated deadline text"})));
    }
}
