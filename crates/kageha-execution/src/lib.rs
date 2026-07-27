//! Reusable SSH execution, scoped file transfer and durable outcomes. No runtime/WebUI/database imports from a host.
pub mod http;
pub mod journal;
pub mod hooks;
pub mod remote;
use kageha_contracts::{execution::*, policy::ActionClass};
use kageha_workspace::{ReadWorkspace, Snapshot};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{io, path::Path, sync::Arc};
pub fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(crate) fn digest(value: &Value) -> String {
    hash(value.to_string().as_bytes())
}
pub trait UploadSource: Send + Sync {
    fn snapshot(
        &self,
        context: &ToolContext,
        path: &str,
        control: &Control<'_>,
    ) -> io::Result<Snapshot>;
}
impl UploadSource for ReadWorkspace {
    fn snapshot(&self, _: &ToolContext, path: &str, control: &Control<'_>) -> io::Result<Snapshot> {
        ReadWorkspace::snapshot(self, path, kageha_workspace::DOCUMENT_LIMIT, control)
    }
}
impl UploadSource for () {
    fn snapshot(&self, _: &ToolContext, _: &str, _: &Control<'_>) -> io::Result<Snapshot> {
        Err(io::Error::other("Upload source is unavailable"))
    }
}
pub struct PreparedUpload {
    path: String,
    bytes: Vec<u8>,
    sha256: String,
}
impl PreparedUpload {
    pub fn new(path: String, snapshot: Snapshot) -> Self {
        Self {
            path,
            bytes: snapshot.bytes,
            sha256: snapshot.sha256,
        }
    }
}
impl UploadSource for PreparedUpload {
    fn snapshot(&self, _: &ToolContext, path: &str, control: &Control<'_>) -> io::Result<Snapshot> {
        control.check()?;
        if path != self.path || hash(&self.bytes) != self.sha256 {
            return Err(io::Error::other("Prepared upload identity changed"));
        }
        Ok(Snapshot {
            bytes: self.bytes.clone(),
            sha256: self.sha256.clone(),
        })
    }
}
pub fn specs() -> Vec<Value> {
    vec![
        json!({"type":"function","function":{"name":"remote_exec","description":"Execute an approved command on the configured SSH destination. Unknown outcomes must not be replayed.","parameters":{"type":"object","properties":{"command":{"type":"string"},"timeout_s":{"type":"integer","minimum":1,"maximum":120}},"required":["command"],"additionalProperties":false}}}),
        json!({"type":"function","function":{"name":"remote_upload","description":"Upload exact authorized bytes to a relative path on the configured SSH destination, with SHA-256 verification.","parameters":{"type":"object","properties":{"path":{"type":"string"},"destination":{"type":"string"}},"required":["path","destination"],"additionalProperties":false}}}),
        json!({"type":"function","function":{"name":"read_file","description":"Read a UTF-8 file inside the configured remote workspace (64 KiB maximum).","parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}}}),
        json!({"type":"function","function":{"name":"remote_receipts","description":"Inspect this session's SSH execution receipts. Inspection sends no remote input and does not resolve uncertain outcomes.","parameters":{"type":"object","properties":{},"additionalProperties":false}}}),
    ]
}
pub struct Tools {
    remote: remote::Remote,
    source: Arc<dyn UploadSource>,
    journal: Arc<journal::Journal>,
}
impl Tools {
    pub fn new(
        config: remote::Config,
        source: Arc<dyn UploadSource>,
        journal: Arc<journal::Journal>,
    ) -> io::Result<Self> {
        Ok(Self {
            remote: remote::Remote::new(config)?,
            source,
            journal,
        })
    }
    pub fn workspace(
        config: remote::Config,
        root: &Path,
        journal: Arc<journal::Journal>,
    ) -> io::Result<Self> {
        Self::new(config, Arc::new(ReadWorkspace::new(root)?), journal)
    }
    fn validate(&self, context: &ToolContext, call: &ToolCall) -> io::Result<()> {
        call.validate()?;
        if [&context.session, &context.turn]
            .iter()
            .any(|s| s.is_empty() || s.len() > 256 || s.chars().any(char::is_control))
        {
            return Err(io::Error::other("Invalid execution context"));
        }
        if call.name != "remote_receipts" {
            self.journal.ensure_session(&context.session)?;
        }
        let allowed: &[&str] = match call.name.as_str() {
            "remote_exec" => &["command", "timeout_s"],
            "remote_upload" => &["path", "destination"],
            "read_file" => &["path"],
            "remote_receipts" => &[],
            _ => return Err(io::Error::other("Unknown execution tool")),
        };
        if call
            .arguments
            .as_object()
            .unwrap()
            .keys()
            .any(|key| !allowed.contains(&key.as_str()))
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Tool arguments cannot change the execution destination or authority",
            ));
        }
        match call.name.as_str() {
            "remote_exec" => {
                let script = call.arguments["command"]
                    .as_str()
                    .filter(|s| !s.is_empty() && s.len() <= 32768 && !s.contains('\0'))
                    .ok_or_else(|| io::Error::other("A bounded command is required"))?;
                let _ = script;
                if call
                    .arguments
                    .get("timeout_s")
                    .is_some_and(|v| v.as_u64().is_none_or(|n| !(1..=120).contains(&n)))
                {
                    return Err(io::Error::other("Remote timeout must be 1 to 120 seconds"));
                }
            }
            "remote_upload" => {
                self.path(call)?;
                remote::relative(
                    call.arguments["destination"]
                        .as_str()
                        .ok_or_else(|| io::Error::other("Destination required"))?,
                )?;
                if call.arguments["destination"].as_str().unwrap().len() > 1024 {
                    return Err(io::Error::other(
                        "Remote upload destination exceeds 1024 bytes",
                    ));
                }
            }
            "read_file" => {
                remote::relative(self.path(call)?)?;
            }
            _ => {}
        }
        Ok(())
    }
    fn path<'a>(&self, call: &'a ToolCall) -> io::Result<&'a str> {
        call.arguments["path"]
            .as_str()
            .ok_or_else(|| io::Error::other("Path required"))
    }
    fn authority(
        &self,
        context: &ToolContext,
        call: &ToolCall,
        snapshot: Option<&Snapshot>,
    ) -> io::Result<ToolBinding> {
        self.validate(context, call)?;
        let mut fingerprint = json!({"configuration":self.remote.fingerprint(),"tool":call.name,"arguments":call.arguments});
        if let Some(snapshot) = snapshot {
            fingerprint["upload_sha256"] = json!(snapshot.sha256);
            fingerprint["upload_bytes"] = json!(snapshot.bytes.len());
        }
        let (class, detail) = match call.name.as_str() {
            "remote_exec" => (
                ActionClass::Mutation,
                format!(
                    "Run on {}: {}",
                    self.remote.config.host,
                    call.arguments["command"].as_str().unwrap()
                ),
            ),
            "remote_upload" => (
                ActionClass::Upload,
                format!(
                    "Upload {} ({} bytes, SHA-256 {}) to {}:{}",
                    self.path(call)?,
                    snapshot.map(|s| s.bytes.len()).unwrap_or(0),
                    snapshot.map(|s| s.sha256.as_str()).unwrap_or(""),
                    self.remote.config.host,
                    call.arguments["destination"].as_str().unwrap()
                ),
            ),
            _ => (
                ActionClass::ReadOnly,
                format!("{} on {}", call.name, self.remote.config.host),
            ),
        };
        Ok(ToolBinding {
            binding: format!("execution.ssh.{}", self.remote.config.id),
            generation: 1,
            resource: self.remote.config.target(),
            class,
            detail,
            fingerprint: digest(&fingerprint),
        })
    }
}
impl ToolPort for Tools {
    fn discover(&self) -> io::Result<Vec<Value>> {
        Ok(specs())
    }
    fn execute(&self, _: &ToolCall, _: &Control<'_>) -> io::Result<ToolReply> {
        Err(io::Error::other(
            "Execution tools require an explicit session and turn",
        ))
    }
    fn dispatch_access_in(&self, _: &ToolContext, call: &ToolCall) -> io::Result<DispatchAccess> {
        if call.name == "remote_receipts" {
            Ok(DispatchAccess::Resource(String::new()))
        } else {
            Ok(DispatchAccess::Resource(format!(
                "execution:{}",
                self.remote.config.target()
            )))
        }
    }
    fn binding(&self, context: &ToolContext, call: &ToolCall) -> io::Result<ToolBinding> {
        self.validate(context, call)?;
        let snapshot = if call.name == "remote_upload" {
            Some(self.source.snapshot(
                context,
                self.path(call)?,
                &Control {
                    cancelled: &std::sync::atomic::AtomicBool::new(false),
                    deadline: None,
                },
            )?)
        } else {
            None
        };
        self.authority(context, call, snapshot.as_ref())
    }
    fn execute_in(
        &self,
        context: &ToolContext,
        call: &ToolCall,
        control: &Control<'_>,
    ) -> io::Result<ToolReply> {
        control.check()?;
        let binding = self.binding(context, call)?;
        self.execute_authorized_in(context, call, &binding, control)
    }
    fn execute_authorized_in(
        &self,
        context: &ToolContext,
        call: &ToolCall,
        binding: &ToolBinding,
        control: &Control<'_>,
    ) -> io::Result<ToolReply> {
        control.check()?;
        self.validate(context, call)?;
        let snapshot = if call.name == "remote_upload" {
            Some(self.source.snapshot(context, self.path(call)?, control)?)
        } else {
            None
        };
        if self.authority(context, call, snapshot.as_ref())? != *binding {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Execution target or upload bytes changed after approval",
            ));
        }
        if call.name == "remote_receipts" {
            return Ok(ToolReply::success(serde_json::to_string(
                &self.journal.receipts(Some(&context.session))?,
            )?));
        }
        if call.name == "read_file" {
            return self
                .remote
                .read(self.path(call)?, control)
                .map(ToolReply::success);
        }
        let digest =
            digest(&json!({"binding":binding,"name":call.name,"arguments":call.arguments}));
        self.journal.run(
            context,
            &call.id,
            &self.remote.config.target(),
            &digest,
            control,
            || {
                let output = if call.name == "remote_exec" {
                    self.remote.execute(
                        call.arguments["command"].as_str().unwrap(),
                        vec![],
                        control,
                        call.arguments["timeout_s"].as_u64().unwrap_or(30),
                    )?
                } else {
                    self.remote.upload(
                        call.arguments["destination"].as_str().unwrap(),
                        snapshot.unwrap().bytes,
                        control,
                    )?
                };
                Ok(ToolReply {
                    outcome: None,
                    is_error: output["ok"] != true,
                    content: output.to_string(),
                })
            },
        )
    }
}
