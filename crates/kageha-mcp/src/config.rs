use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io};
#[derive(Clone, Deserialize)]
pub struct Config {
    #[serde(skip)]
    pub(crate) mutual_tls_identity: Option<zeroize::Zeroizing<String>>,
    #[serde(default)]
    pub sandbox: Option<kageha_sandbox::ProcessSandbox>,
    pub id: String,
    #[serde(default = "default_transport")]
    pub transport: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub inherit_env: bool,
    #[serde(default)]
    pub read_only_tools: Vec<String>,
    /// Operator-selected mailbox adapter implementing the protected verification contract.
    #[serde(default)]
    pub verification_mail_tool: Option<String>,
    #[serde(default = "generation")]
    pub generation: u64,
}
fn default_transport() -> String {
    "stdio".into()
}
fn generation() -> u64 {
    1
}
pub(crate) fn hash(value: &Value) -> String {
    format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
}
pub fn tool_id(server: &str, name: &str) -> String {
    format!(
        "mcp_{}",
        &format!("{:x}", Sha256::digest(format!("{server}:{name}")))[..32]
    )
}
impl Config {
    /// Installs an in-memory client identity. The key is never deserialized from
    /// persisted configuration and is cleared when the last configuration drops.
    pub fn with_mutual_tls_identity(mut self, pem: &str) -> io::Result<Self> {
        let url=reqwest::Url::parse(&self.url).map_err(|_|io::Error::other("Invalid mutual TLS endpoint"))?;
        if !matches!(self.transport.as_str(),"http"|"streamable_http") || url.scheme()!="https" || pem.is_empty() || pem.len()>1024*1024 {
            return Err(io::Error::other("Invalid mutual TLS identity or endpoint"));
        }
        reqwest::Identity::from_pem(pem.as_bytes()).map_err(|_|io::Error::other("Invalid mutual TLS identity"))?;
        self.mutual_tls_identity=Some(zeroize::Zeroizing::new(pem.to_owned()));
        Ok(self)
    }
    pub fn validate(&self) -> io::Result<()> {
        if self.transport != "stdio" && self.sandbox.is_some() {
            return Err(io::Error::other(
                "Process sandbox applies only to stdio adapters",
            ));
        }
        if self.transport == "stdio" && kageha_sandbox::required()? && self.sandbox.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Managed policy requires an MCP process sandbox",
            ));
        }
        if self.sandbox.is_some() && self.inherit_env {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Sandboxed processes require an explicit environment",
            ));
        }
        if self.id.is_empty()
            || self.id.len() > 64
            || !self
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
            || self.generation == 0
        {
            return Err(io::Error::other(
                "Invalid MCP server identity or generation",
            ));
        }
        if self.read_only_tools.len() > 128
            || self
                .read_only_tools
                .iter()
                .any(|s| s.is_empty() || s.len() > 256 || s.chars().any(char::is_control))
        {
            return Err(io::Error::other("Invalid read-only tool configuration"));
        }
        if self.verification_mail_tool.as_ref().is_some_and(|tool|
            !self.read_only_tools.iter().any(|name| name == tool)) {
            return Err(io::Error::other("Verification mailbox tool must be explicitly marked read-only"));
        }
        if self.args.len() > 64
            || self.args.iter().any(|a| a.len() > 8192 || a.contains('\0'))
            || self.env.len() > 128
            || self.headers.len() > 64
        {
            return Err(io::Error::other("MCP configuration exceeds limits"));
        }
        if self.env.iter().any(|(k, v)| {
            k.is_empty()
                || k.contains(['=', '\0'])
                || v.contains('\0')
                || k.len() > 256
                || v.len() > 65536
        }) {
            return Err(io::Error::other("Invalid MCP process environment"));
        }
        match self.transport.as_str() {
            "stdio" => {
                if self.command.is_empty()
                    || self.command.len() > 4096
                    || self.command.contains('\0')
                {
                    return Err(io::Error::other("Invalid MCP command"));
                }
            }
            "http" | "streamable_http" => {
                let url = reqwest::Url::parse(&self.url)
                    .map_err(|_| io::Error::other("Invalid MCP endpoint"))?;
                if self.url.len() > 2048
                    || self.url.chars().any(char::is_control)
                    || !matches!(url.scheme(), "http" | "https")
                    || !url.username().is_empty()
                    || url.password().is_some()
                    || url.fragment().is_some()
                {
                    return Err(io::Error::other("Invalid MCP endpoint"));
                }
                if !self.headers.is_empty() && url.scheme() != "https" {
                    return Err(io::Error::other("MCP custom headers require HTTPS"));
                }
                for (key, value) in &self.headers {
                    if reqwest::header::HeaderName::from_bytes(key.as_bytes()).is_err()
                        || reqwest::header::HeaderValue::from_str(value).is_err()
                        || matches!(
                            key.to_ascii_lowercase().as_str(),
                            "host"
                                | "content-length"
                                | "mcp-session-id"
                                | "mcp-protocol-version"
                                | "accept"
                                | "content-type"
                        )
                    {
                        return Err(io::Error::other("Invalid or reserved MCP header"));
                    }
                }
            }
            _ => return Err(io::Error::other("Unsupported MCP transport")),
        }
        Ok(())
    }
    pub(crate) fn fingerprint(&self) -> String {
        hash(
            &json!({"id":self.id,"transport":self.transport,"command":self.command,"args":self.args,"url":self.url,"env":self.env,"headers":self.headers,"sandbox":self.sandbox,"inherit_env":self.inherit_env,"read_only_tools":self.read_only_tools,"verification_mail_tool":self.verification_mail_tool,"generation":self.generation,
                "mutual_tls_identity_sha256":self.mutual_tls_identity.as_ref().map(|pem|format!("{:x}",Sha256::digest(pem.as_bytes())))}),
        )
    }
    pub fn from_legacy(value: &Value) -> io::Result<Self> {
        let mut copy = value.clone();
        copy["id"] = value["name"]
            .as_str()
            .or_else(|| value["id"].as_str())
            .map(|s| json!(s))
            .unwrap_or(json!("legacy"));
        copy["inherit_env"] = json!(copy["sandbox"].is_null());
        copy["generation"] = json!(value["revision"].as_u64().unwrap_or(1));
        for field in ["env", "headers"] {
            if copy[field].is_null() {
                copy[field] = json!({});
            }
        }
        let result: Self = serde_json::from_value(copy)
            .map_err(|_| io::Error::other("Invalid MCP configuration"))?;
        result.validate()?;
        Ok(result)
    }
}
pub fn specs(server: &str, tools: &[Value]) -> io::Result<Vec<Value>> {
    if tools.len() > 128 {
        return Err(io::Error::other("MCP catalog exceeds limit"));
    }
    let mut names = std::collections::HashSet::new();
    let mut result = Vec::new();
    for tool in tools {
        let name = tool["name"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
            .ok_or_else(|| io::Error::other("Invalid MCP tool name"))?;
        if !names.insert(name) || !tool["inputSchema"].is_object() {
            return Err(io::Error::other(
                "Duplicate MCP tool or invalid input schema",
            ));
        }
        result.push(json!({"type":"function","function":{"name":tool_id(server,name),"description":tool["description"].as_str().unwrap_or("MCP tool"),"parameters":tool["inputSchema"]}}));
    }
    Ok(result)
}
