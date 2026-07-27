//! Independent scoped MCP client, tool adapter and receipt journal.
mod config;
mod journal;
mod transport;
use config::hash;
pub use config::{specs, tool_id, Config};
use kageha_contracts::{execution::*, policy::ActionClass};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io,
    path::Path,
    sync::{Arc, Mutex, MutexGuard, TryLockError},
    time::{Duration, Instant},
};
struct Client {
    connection: Mutex<Option<transport::Connection>>,
    retired: std::sync::atomic::AtomicBool,
    config: Config,
}
pub struct Pool {
    journal: journal::Journal,
    clients: Mutex<HashMap<String, (String, Arc<Client>)>>,
}
fn lock<'a, T>(value: &'a Mutex<T>, control: &Control<'_>) -> io::Result<MutexGuard<'a, T>> {
    loop {
        control.check()?;
        match value.try_lock() {
            Ok(v) => return Ok(v),
            Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(5)),
            Err(_) => return Err(io::Error::other("MCP connection state unavailable")),
        }
    }
}
fn checked_context(context: &ToolContext) -> io::Result<()> {
    if [&context.session, &context.turn]
        .iter()
        .any(|s| s.is_empty() || s.len() > 256 || s.chars().any(char::is_control))
    {
        return Err(io::Error::other("Invalid MCP execution context"));
    }
    Ok(())
}
impl Pool {
    pub fn open(path: &Path) -> io::Result<Self> {
        Ok(Self {
            journal: journal::Journal::open(path)?,
            clients: Mutex::new(HashMap::new()),
        })
    }
    fn client(&self, config: &Config, session: &str) -> io::Result<Arc<Client>> {
        let key = hash(&json!({"configuration":config.fingerprint(),"session":session}));
        let mut clients = self
            .clients
            .lock()
            .map_err(|_| io::Error::other("MCP connection pool unavailable"))?;
        if let Some((_, client)) = clients.get(&key) {
            return Ok(client.clone());
        }
        if clients.len() >= 128 {
            return Err(io::Error::other(
                "MCP session capacity reached; release an idle tool session",
            ));
        }
        let client = Arc::new(Client {
            connection: Mutex::new(None),
            retired: std::sync::atomic::AtomicBool::new(false),
            config: config.clone(),
        });
        clients.insert(key, (session.to_owned(), client.clone()));
        Ok(client)
    }
    pub fn exchange(
        &self,
        config: &Config,
        context: &ToolContext,
        method: &str,
        params: Value,
        call_id: Option<&str>,
        control: &Control<'_>,
    ) -> io::Result<Value> {
        config.validate()?;
        checked_context(context)?;
        if !matches!(method, "tools/list" | "tools/call") {
            return Err(io::Error::other("Unsupported MCP operation"));
        }
        let deadline = control
            .deadline
            .unwrap_or(Instant::now() + Duration::from_secs(12))
            .min(Instant::now() + Duration::from_secs(12));
        let control = Control {
            cancelled: control.cancelled,
            deadline: Some(deadline),
        };
        control.check()?;
        let digest =
            hash(&json!({"configuration":config.fingerprint(),"method":method,"params":params}));
        if method == "tools/call" {
            let id = call_id
                .filter(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
                .ok_or_else(|| io::Error::other("A stable MCP call ID is required"))?;
            if let Some(result) = self.journal.preflight(context, id, &config.id, &digest)? {
                return Ok(result);
            }
        }
        let client = self.client(config, &context.session)?;
        let mut connection = lock(&client.connection, &control)?;
        if client.retired.load(std::sync::atomic::Ordering::Acquire) {
            return Err(io::Error::other("MCP session was released before dispatch"));
        }
        if connection.is_none() {
            *connection = Some(transport::Connection::connect(config, &control)?);
        }
        control.check()?;
        if method == "tools/call" {
            if let Some(result) =
                self.journal
                    .begin(context, call_id.unwrap(), &config.id, &digest)?
            {
                return Ok(result);
            }
        }
        let response = connection
            .as_mut()
            .unwrap()
            .call(config, method, params, &control);
        match response {
            Ok(result) => {
                if method == "tools/list" {
                    let tools = result["tools"]
                        .as_array()
                        .ok_or_else(|| io::Error::other("MCP tool list missing"))?;
                    specs(&config.id, tools)?;
                } else {
                    self.journal
                        .finish(context, call_id.unwrap(), &digest, &result)?;
                }
                Ok(result)
            }
            Err(error) => {
                *connection = None;
                if method == "tools/call" {
                    if control.check().is_err() {
                        return Err(error);
                    }
                    Err(journal::uncertain())
                } else {
                    Err(error)
                }
            }
        }
    }
    /// Fetch private mailbox data without writing the response to MCP receipts.
    /// This is limited to the operator's explicitly configured, read-only mailbox
    /// adapter; the caller must enforce its own agent, policy and destination grants.
    pub fn verification_mail_read(
        &self, config: &Config, context: &ToolContext, arguments: Value, control: &Control<'_>,
    ) -> io::Result<Value> {
        config.validate()?;
        checked_context(context)?;
        let tool = config.verification_mail_tool.as_deref()
            .filter(|tool| config.read_only_tools.iter().any(|name| name == tool))
            .ok_or_else(|| io::Error::new(io::ErrorKind::PermissionDenied, "Protected mailbox adapter is not configured"))?;
        if !arguments.is_object() { return Err(io::Error::other("Invalid protected mailbox arguments")); }
        let deadline = control.deadline.unwrap_or(Instant::now() + Duration::from_secs(12))
            .min(Instant::now() + Duration::from_secs(12));
        let control = Control { cancelled: control.cancelled, deadline: Some(deadline) };
        control.check()?;
        let client = self.client(config, &context.session)?;
        let mut connection = lock(&client.connection, &control)?;
        if client.retired.load(std::sync::atomic::Ordering::Acquire) {
            return Err(io::Error::other("MCP session was released before dispatch"));
        }
        if connection.is_none() { *connection = Some(transport::Connection::connect(config, &control)?); }
        let result = connection.as_mut().unwrap().call(config, "tools/call", json!({"name":tool,"arguments":arguments}), &control);
        match result {
            Ok(value) if value["isError"] != true => Ok(value),
            Ok(_) => Err(io::Error::other("Protected mailbox adapter reported failure")),
            Err(_) => { *connection = None; Err(io::Error::other("Protected mailbox adapter unavailable")) }
        }
    }
    pub fn receipts(&self, session: &str) -> io::Result<Vec<Value>> {
        self.journal.receipts(session)
    }
    pub fn release(&self, session: &str) -> io::Result<()> {
        let mut clients = self
            .clients
            .lock()
            .map_err(|_| io::Error::other("MCP connection pool unavailable"))?;
        let keys = clients
            .iter()
            .filter(|(_, (scope, _))| scope == session)
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        let selected = keys
            .iter()
            .map(|key| clients[key].1.clone())
            .collect::<Vec<_>>();
        let mut guards = Vec::new();
        for client in &selected {
            guards.push(
                client
                    .connection
                    .try_lock()
                    .map_err(|_| io::Error::other("MCP session still has an active call"))?,
            );
        }
        for client in &selected {
            client
                .retired
                .store(true, std::sync::atomic::Ordering::Release);
        }
        for key in keys {
            clients.remove(&key);
        }
        drop(clients);
        for (client, state) in selected.iter().zip(guards.iter_mut()) {
            if let Some(connection) = state.as_mut() {
                connection.close(&client.config);
            }
            **state = None;
        }
        Ok(())
    }
    pub fn erase(&self, session: &str) -> io::Result<()> {
        self.release(session)?;
        self.journal.erase(session)
    }
}
pub struct McpTools {
    pool: Arc<Pool>,
    servers: Vec<(Config, Vec<Value>)>,
}
pub fn receipts_spec() -> Value {
    json!({"type":"function","function":{"name":"mcp_receipts","description":"Inspect this session's MCP outcomes without sending input. An uncertain remote effect cannot be repeated automatically.","parameters":{"type":"object","properties":{},"additionalProperties":false}}})
}
impl McpTools {
    pub fn connect(
        pool: Arc<Pool>,
        servers: Vec<Config>,
        control: &Control<'_>,
    ) -> io::Result<Self> {
        if servers.is_empty() || servers.len() > 16 {
            return Err(io::Error::other("Configure one to sixteen MCP servers"));
        }
        let mut names = std::collections::HashSet::new();
        let mut catalog = Vec::new();
        for config in servers {
            config.validate()?;
            if !names.insert(config.id.clone()) {
                return Err(io::Error::other("MCP server ID is configured twice"));
            }
            let context = ToolContext {
                session: format!("catalog:{}", config.id),
                turn: "discovery".into(),
            };
            let reply = pool.exchange(&config, &context, "tools/list", json!({}), None, control)?;
            let tools = reply["tools"]
                .as_array()
                .cloned()
                .ok_or_else(|| io::Error::other("MCP tool list missing"))?;
            specs(&config.id, &tools)?;
            if config
                .read_only_tools
                .iter()
                .any(|name| !tools.iter().any(|t| t["name"] == *name))
            {
                return Err(io::Error::other(
                    "Read-only MCP configuration names an unavailable tool",
                ));
            }
            catalog.push((config, tools));
        }
        Ok(Self {
            pool,
            servers: catalog,
        })
    }
    fn resolve(&self, name: &str) -> io::Result<(&Config, String)> {
        for (config, tools) in &self.servers {
            for tool in tools {
                let original = tool["name"].as_str().unwrap();
                if tool_id(&config.id, original) == name {
                    return Ok((config, original.to_owned()));
                }
            }
        }
        Err(io::Error::other(
            "MCP tool is not in the configured catalog",
        ))
    }
}
impl ToolPort for McpTools {
    fn dispatch_access_in(
        &self,
        context: &ToolContext,
        call: &ToolCall,
    ) -> io::Result<DispatchAccess> {
        checked_context(context)?;
        if call.name == "mcp_receipts" {
            return Ok(DispatchAccess::Resource(String::new()));
        }
        let (config, _) = self.resolve(&call.name)?;
        Ok(DispatchAccess::Resource(format!(
            "mcp:{}:{}",
            context.session, config.id
        )))
    }

    fn discover(&self) -> io::Result<Vec<Value>> {
        let mut all = vec![receipts_spec()];
        for (config, tools) in &self.servers {
            all.extend(specs(&config.id, tools)?);
        }
        Ok(all)
    }
    fn execute(&self, _: &ToolCall, _: &Control<'_>) -> io::Result<ToolReply> {
        Err(io::Error::other(
            "MCP tools require an explicit execution context",
        ))
    }
    fn binding(&self, context: &ToolContext, call: &ToolCall) -> io::Result<ToolBinding> {
        checked_context(context)?;
        if call.name == "mcp_receipts" {
            return Ok(ToolBinding {
                binding: "tools.mcp.receipts".into(),
                generation: 1,
                resource: context.session.clone(),
                class: ActionClass::ReadOnly,
                detail: "Inspect MCP receipts for this session".into(),
                fingerprint: String::new(),
            });
        }
        let (config, name) = self.resolve(&call.name)?;
        Ok(ToolBinding {
            binding: format!("mcp.{}", config.id),
            generation: config.generation,
            resource: name.clone(),
            class: if config.read_only_tools.contains(&name) {
                ActionClass::ReadOnly
            } else {
                ActionClass::Mutation
            },
            detail: format!("Use MCP tool {} on server {}", name, config.id),
            fingerprint: config.fingerprint(),
        })
    }
    fn execute_in(
        &self,
        context: &ToolContext,
        call: &ToolCall,
        control: &Control<'_>,
    ) -> io::Result<ToolReply> {
        control.check()?;
        call.validate()?;
        if call.name == "mcp_receipts" {
            if !call.arguments.as_object().unwrap().is_empty() {
                return Err(io::Error::other(
                    "MCP receipt inspection has no caller-supplied scope",
                ));
            }
            return Ok(ToolReply::success(serde_json::to_string(
                &self.pool.receipts(&context.session)?,
            )?));
        }
        let (config, name) = self.resolve(&call.name)?;
        let response = self.pool.exchange(
            config,
            context,
            "tools/call",
            json!({"name":name,"arguments":call.arguments}),
            Some(&call.id),
            control,
        )?;
        Ok(ToolReply {
            outcome: None,
            is_error: response["isError"] == true,
            content: response.to_string(),
        })
    }
    fn release_context(&self, session: &str) -> io::Result<()> {
        self.pool.release(session)
    }
    fn erase_context(&self, session: &str) -> io::Result<()> {
        self.pool.erase(session)
    }
}
