use crate::browser::Browsers;
use crate::cdp::Result;
use crate::protocol::Request;
use crate::script_artifact::{self, Content};
use rquickjs::{Context, Function, Runtime};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{self, BufRead, Read, Write};
use std::process::Stdio;
use std::sync::{atomic::AtomicBool, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::watch;

const MAX_CODE: usize = 24 * 1024;
const MAX_CALLS: usize = 32;
const MAX_RESULT: usize = 8 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Params {
    version: u8,
    code: String,
    browser_id: String,
    binding_id: String,
    target_id: Option<String>,
    upload: Option<crate::upload::Approval>,
}

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Open {
        url: String,
    },
    Navigate {
        url: String,
    },
    Select {
        tab_handle: String,
    },
    AttachTab {
        tab_handle: String,
        url: String,
        title: String,
    },
    PreviewReady,
    Tabs,
    Snapshot,
    Credential {
        account: String,
        mode: Option<String>,
        credential: Option<String>,
    },
    FillCredential {
        target: String,
        credential: String,
    },
    Batch {
        actions: Vec<Value>,
    },
    Artifact {
        name: String,
        content: Content,
    },
    Download {
        target: String,
        name: String,
        expected_url: Option<String>,
    },
    Upload {
        target: String,
        paths: Vec<String>,
    },
    Wait {
        ms: u64,
    },
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Message {
    Call { call: Operation },
    Complete { result: Value },
    Failed { code: String },
}

struct Active {
    client: String,
    id: String,
    stop: watch::Sender<bool>,
}

#[derive(Default)]
pub struct Registry(Mutex<HashMap<String, Active>>);

struct Registration<'a> {
    registry: &'a Registry,
    workspace: &'a str,
}

impl Drop for Registration<'_> {
    fn drop(&mut self) {
        self.registry.0.lock().unwrap().remove(self.workspace);
    }
}

impl Registry {
    fn start<'a>(
        &'a self,
        request: &'a Request,
    ) -> Result<(Registration<'a>, watch::Receiver<bool>)> {
        let mut active = self.0.lock().unwrap();
        if active.contains_key(&request.workspace_id) {
            return Err("script_already_running");
        }
        let (stop, receiver) = watch::channel(false);
        active.insert(
            request.workspace_id.clone(),
            Active {
                client: request.client_id.clone(),
                id: request.request_id.clone(),
                stop,
            },
        );
        Ok((
            Registration {
                registry: self,
                workspace: &request.workspace_id,
            },
            receiver,
        ))
    }

    pub fn cancel(&self, request: &Request, id: &str) -> bool {
        self.0
            .lock()
            .unwrap()
            .get(&request.workspace_id)
            .is_some_and(|active| {
                active.client == request.client_id
                    && active.id == id
                    && active.stop.send(true).is_ok()
            })
    }
}

pub async fn run(browsers: &Browsers, request: &Request, dispatched: &AtomicBool) -> Result<Value> {
    let mut p: Params =
        serde_json::from_value(request.params.clone()).map_err(|_| "invalid_params")?;
    if !matches!(p.version, 1..=3) {
        return Err("unsupported_script_version");
    }
    if p.code.trim().is_empty() || p.code.len() > MAX_CODE || request.request_id.len() > 96 {
        return Err("invalid_script");
    }
    if let Some(approval) = &p.upload {
        if p.version != 3 {
            return Err("invalid_params");
        }
        approval.validate()?;
    }
    let (_registration, mut stop) = browsers.scripts.start(request)?;
    browsers
        .script_start(
            request,
            &p.browser_id,
            &p.binding_id,
            p.target_id.as_deref(),
        )
        .await?;
    let mut child = Command::new(std::env::current_exe().map_err(|_| "script_worker_unavailable")?)
        .arg("--script-worker")
        .env_clear()
        .current_dir("/")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| "script_worker_unavailable")?;
    let mut input = child.stdin.take().ok_or("script_worker_unavailable")?;
    let mut output = BufReader::new(child.stdout.take().ok_or("script_worker_unavailable")?);
    let mut init = serde_json::to_vec(&json!({
        "code": p.code, "version": p.version,
        "ms": request.deadline_ms.saturating_sub(crate::protocol::now_ms() + 100),
    }))
    .map_err(|_| "invalid_script")?;
    init.push(b'\n');
    input
        .write_all(&init)
        .await
        .map_err(|_| "script_worker_unavailable")?;
    // Leave time to return a structured report before the RPC deadline.
    // Mutating browser calls keep their own dispatch/receipt deadline.
    let deadline = tokio::time::sleep(Duration::from_millis(
        request
            .deadline_ms
            .saturating_sub(crate::protocol::now_ms() + 50),
    ));
    tokio::pin!(deadline);

    let mut selected = p.target_id.unwrap_or_default();
    let mut steps = 0;
    let mut opened = Vec::new();
    let mut batches = Vec::new();
    let mut upload_receipt_ids = Vec::new();
    let mut artifacts = Vec::new();
    let mut result = Value::Null;
    let mut failed_batch = Value::Null;
    let (status, error) = loop {
        let read = tokio::select! {
            biased;
            _ = stop.changed() => break ("cancelled", Some("script_cancelled")),
            _ = &mut deadline => break ("stopped", Some("script_deadline_exceeded")),
            message = crate::read_message_limit(&mut output, script_artifact::MAX_WORKER_MESSAGE) => message,
        };
        let Ok(Some(line)) = read else {
            break ("stopped", Some("script_worker_disconnected"));
        };
        let Ok(message) = serde_json::from_slice::<Message>(&line) else {
            break ("stopped", Some("script_worker_invalid_reply"));
        };
        match message {
            Message::Complete { result: value } => {
                if serde_json::to_vec(&value).map_or(true, |data| data.len() > MAX_RESULT) {
                    break ("stopped", Some("script_result_too_large"));
                }
                result = value;
                break ("completed", None);
            }
            Message::Failed { code } => {
                break (
                    "stopped",
                    Some(match code.as_str() {
                        "script_sandbox_unavailable" => "script_sandbox_unavailable",
                        "script_result_too_large" => "script_result_too_large",
                        "script_syntax_error" => "script_syntax_error",
                        "script_deadline_exceeded" => "script_deadline_exceeded",
                        _ => "script_execution_failed",
                    }),
                )
            }
            Message::Call { call } => {
                if steps >= MAX_CALLS {
                    break ("stopped", Some("script_call_limit"));
                }
                let is_open = matches!(call, Operation::Open { .. });
                let is_batch = matches!(
                    call,
                    Operation::Batch { .. } | Operation::FillCredential { .. }
                );
                let step = if let Operation::PreviewReady = call {
                    browsers
                        .script_preview_ready(request, &p.browser_id, &selected)
                        .await
                } else if let Operation::Wait { ms } = call {
                    if ms > 5_000 {
                        break ("stopped", Some("invalid_script_wait"));
                    }
                    tokio::select! {
                        biased;
                        _ = stop.changed() => break ("cancelled", Some("script_cancelled")),
                        _ = &mut deadline => break ("stopped", Some("script_deadline_exceeded")),
                        _ = tokio::time::sleep(Duration::from_millis(ms)) => Ok(Value::Null),
                    }
                } else if let Operation::Artifact { name, content } = call {
                    if p.version < 2 || artifacts.len() >= script_artifact::MAX_ARTIFACTS {
                        break ("stopped", Some("script_artifact_limit"));
                    }
                    match browsers
                        .script_artifact(
                            &request,
                            &p.browser_id,
                            &selected,
                            &name,
                            content,
                            dispatched,
                        )
                        .await
                    {
                        Ok(artifact) => {
                            let value = json!(artifact);
                            artifacts.push(artifact);
                            Ok(value)
                        }
                        Err(error) => Err(error),
                    }
                } else if let Operation::Download {
                    target,
                    name,
                    expected_url,
                } = call
                {
                    if p.version < 2 || artifacts.len() >= script_artifact::MAX_ARTIFACTS {
                        break ("stopped", Some("script_artifact_limit"));
                    }
                    let mut action = request.clone();
                    action.request_id = format!("{}:download:{steps}", request.request_id);
                    match browsers
                        .script_download(
                            &action,
                            &request.request_id,
                            &p.browser_id,
                            &selected,
                            &target,
                            &name,
                            expected_url.as_deref(),
                            dispatched,
                            &mut stop,
                        )
                        .await
                    {
                        Ok(artifact) => {
                            let value = json!(artifact);
                            artifacts.push(artifact);
                            Ok(value)
                        }
                        Err(error) => Err(error),
                    }
                } else if let Operation::Upload { target, paths } = call {
                    let Some(approval) = p.upload.take() else {
                        break ("stopped", Some("script_upload_approval_required"));
                    };
                    let mut action = request.clone();
                    action.request_id = format!("{}:upload:{steps}", request.request_id);
                    // Preserve the actual action identity even if the script stops
                    // before it can return the upload result to its worker.
                    upload_receipt_ids.push(action.request_id.clone());
                    browsers
                        .script_upload(
                            &action,
                            &p.browser_id,
                            &selected,
                            &target,
                            paths,
                            approval,
                            dispatched,
                            &stop,
                        )
                        .await
                } else {
                    let mut action = request.clone();
                    if is_batch {
                        action.request_id = format!("{}:batch:{steps}", request.request_id);
                        batches.push(action.request_id.clone());
                    }
                    browsers
                        .script_step(&action, &p.browser_id, &mut selected, call, dispatched)
                        .await
                };
                let value = match step {
                    Ok(value) => value,
                    Err("script_cancelled") => break ("cancelled", Some("script_cancelled")),
                    Err(error) => break ("stopped", Some(error)),
                };
                steps += 1;
                if is_open {
                    opened.push(selected.clone());
                }
                if is_batch && value["status"] != "completed" {
                    failed_batch = value;
                    break ("stopped", Some("script_batch_stopped"));
                }
                let mut response =
                    serde_json::to_vec(&value).map_err(|_| "script_worker_invalid_reply")?;
                response.push(b'\n');
                if response.len() > crate::protocol::limit("message_bytes")
                    || input.write_all(&response).await.is_err()
                {
                    break ("stopped", Some("script_worker_disconnected"));
                }
            }
        }
    };
    // The child may be waiting for an operation reply. Always terminate and
    // reap it, including for partial scripts and cancellation.
    let _ = child.kill().await;
    let mut report = json!({
        "status": status, "error_code": error, "completed_calls": steps, "result": result,
        "input_dispatched": dispatched.load(std::sync::atomic::Ordering::Relaxed),
        "tab_handle": if selected.is_empty() { None } else { Some(selected) },
        "opened_tabs": opened, "batch_ids": batches, "failed_batch": failed_batch,
        "upload_receipt_ids": upload_receipt_ids,

    });
    if p.version >= 2 {
        report["artifacts"] = json!(artifacts);
    }
    Ok(report)
}

fn read_line(reader: &mut impl BufRead) -> io::Result<String> {
    let mut line = Vec::new();
    (&mut *reader)
        .take((crate::protocol::limit("message_bytes") + 1) as u64)
        .read_until(b'\n', &mut line)?;
    if line.len() > crate::protocol::limit("message_bytes") || !line.ends_with(b"\n") {
        return Err(io::ErrorKind::InvalidData.into());
    }
    String::from_utf8(line).map_err(|_| io::ErrorKind::InvalidData.into())
}

fn emit(value: &Value) -> io::Result<()> {
    let mut out = io::stdout().lock();
    serde_json::to_writer(&mut out, value)?;
    out.write_all(b"\n")?;
    out.flush()
}

pub fn worker() -> i32 {
    match evaluate() {
        Ok(result) => {
            if emit(&json!({"kind": "complete", "result": result})).is_ok() {
                0
            } else {
                1
            }
        }
        Err(code) => {
            let _ = emit(&json!({"kind": "failed", "code": code}));
            1
        }
    }
}

fn evaluate() -> Result<Value> {
    if !crate::script_sandbox::restrict() {
        return Err("script_sandbox_unavailable");
    }
    let init: Value =
        serde_json::from_str(&read_line(&mut io::stdin().lock()).map_err(|_| "invalid_script")?)
            .map_err(|_| "invalid_script")?;
    let code = init["code"]
        .as_str()
        .filter(|code| code.len() <= MAX_CODE)
        .ok_or("invalid_script")?;
    let version = init["version"].as_u64().unwrap_or(0);
    let deadline =
        Instant::now() + Duration::from_millis(init["ms"].as_u64().unwrap_or(0).min(30_000));
    let runtime = Runtime::new().map_err(|_| "script_execution_failed")?;
    runtime.set_memory_limit(16 * 1024 * 1024);
    runtime.set_max_stack_size(256 * 1024);
    runtime.set_interrupt_handler(Some(Box::new(move || Instant::now() >= deadline)));
    let context = Context::full(&runtime).map_err(|_| "script_execution_failed")?;
    context.with(|ctx| {
        let host = Function::new(ctx.clone(), |request: String| -> rquickjs::Result<String> {
            let error = || {
                rquickjs::Error::new_from_js_message(
                    "browser",
                    "operation",
                    "invalid browser operation",
                )
            };
            if request.len() > script_artifact::MAX_WORKER_MESSAGE - 256 {
                return Err(error());
            }
            let call: Value = serde_json::from_str(&request).map_err(|_| error())?;
            emit(&json!({"kind":"call","call":call})).map_err(|_| error())?;
            read_line(&mut io::stdin().lock()).map_err(|_| error())
        })
        .map_err(|_| "script_execution_failed")?;
        let make_api: Function = ctx
            .eval(
                r#"((host, version) => {
            const {parse, stringify} = JSON;
            const call = (operation, args = {}) => parse(host(stringify({...args, operation})));
            return Object.freeze({
                open: url => call("open", {url}),
                navigate: url => call("navigate", {url}),
                select: tab_handle => call("select", {tab_handle}),
                tabs: () => call("tabs"),
                snapshot: () => call("snapshot"),
                batch: actions => call("batch", {actions}),
                wait: ms => call("wait", {ms}),
                ...(version === 3 ? {
                    upload: (target, paths) => call("upload", {target, paths}),
                    attachTab: (tab_handle, url, title) => call("attach_tab", {tab_handle, url, title}),
                    previewReady: () => call("preview_ready"),
                    credential: (account, mode, credential) => call("credential", {account, mode, credential}),
                    fillCredential: (target, credential) => call("fill_credential", {target, credential}),
                } : {}),
                ...(version >= 2 ? {artifacts: Object.freeze({
                    writeText: (name, text) => call("artifact", {name, content: {kind: "text", text}}),
                    writeJson: (name, data) => call("artifact", {name, content: {kind: "json", data}}),
                    screenshot: name => call("artifact", {name, content: {kind: "screenshot"}}),
                    download: (target, name, expectedUrl) => call("download", {target, name, expected_url: expectedUrl}),
                })} : {}),
            });
        })"#,
            )
            .map_err(|_| "script_execution_failed")?;
        let api: rquickjs::Object = make_api
            .call((host, version))
            .map_err(|_| "script_execution_failed")?;
        // Compile before invoking so syntax errors have no browser effects.
        let script: Function = ctx
            .eval(format!("(function(browser) {{ 'use strict';\n{code}\n}})"))
            .map_err(|_| "script_syntax_error")?;
        let result: rquickjs::Value = script.call((api,)).map_err(|_| {
            if Instant::now() >= deadline {
                "script_deadline_exceeded"
            } else {
                "script_execution_failed"
            }
        })?;
        if result.is_promise() {
            return Err("script_execution_failed");
        }
        let json = ctx
            .json_stringify(result)
            .map_err(|_| "script_execution_failed")?;
        let value = json
            .map(|text| text.to_string())
            .transpose()
            .map_err(|_| "script_execution_failed")?;
        match value {
            Some(value) if value.len() <= MAX_RESULT => {
                serde_json::from_str(&value).map_err(|_| "script_execution_failed")
            }
            Some(_) => Err("script_result_too_large"),
            None => Ok(Value::Null),
        }
    })
}
