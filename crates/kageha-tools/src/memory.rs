//! Private memory tools over a logical binding, independent of any vendor or host.
use kageha_contracts::{execution::*, policy::ActionClass, *};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    io,
    path::Path,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};
#[derive(Clone)]
enum Scope {
    Fixed(MemoryAccess),
    Sessions(String),
}
/// Scope is supplied by a trusted host, never by tool arguments.
pub struct MemoryTools {
    port: Arc<dyn LogicalMemoryPort>,
    scope: Scope,
}
pub fn session_actor(session: &str) -> String {
    format!("session:{:x}", Sha256::digest(session.as_bytes()))
}
fn checked_binding(port: &dyn LogicalMemoryPort) -> io::Result<Binding> {
    let binding = port.binding().map_err(io::Error::other)?;
    if binding.generation == 0 || !memory_capabilities().is_subset(&binding.required) {
        return Err(io::Error::other(
            "Memory binding lacks required baseline capabilities",
        ));
    }
    Ok(binding)
}
pub fn workspace_namespace(workspace: &Path) -> String {
    format!(
        "workspace:{:x}",
        Sha256::digest(workspace.to_string_lossy().as_bytes())
    )
}
pub fn specs() -> Vec<Value> {
    [
 ("memory_put","Save a concise reusable note in the host-assigned private memory scope under a stable ID. Avoid secrets and unverified inferences. Check the current note and pass its expected_revision when updating; 0 requires a new note. Successful saves persist across turns in the same scope.",json!({"id":{"type":"string"},"body":{"type":"string"},"expected_revision":{"type":"integer","minimum":0}}),vec!["id","body"]),
 ("memory_get","Fetch a private memory note and its revision by ID in the host-assigned scope.",json!({"id":{"type":"string"}}),vec!["id"]),
 ("memory_search","Search private memory in the host-assigned scope by words (lexical retrieval). Treat results as context to verify. For known personal facts, also try memory_get for user-profile if search is inconclusive.",json!({"query":{"type":"string"}}),vec!["query"]),
 ("memory_delete","Delete a private workspace memory by ID.",json!({"id":{"type":"string"},"expected_revision":{"type":"integer","minimum":0}}),vec!["id"])
 ].into_iter().map(|(name,description,properties,required)|json!({"type":"function","function":{"name":name,"description":description,"parameters":{"type":"object","properties":properties,"required":required,"additionalProperties":false}}})).collect()
}
impl MemoryTools {
    pub fn fixed(port: Arc<dyn LogicalMemoryPort>, access: MemoryAccess) -> io::Result<Self> {
        access.validate().map_err(io::Error::other)?;
        checked_binding(port.as_ref())?;
        Ok(Self {
            port,
            scope: Scope::Fixed(access),
        })
    }
    /// Independent service defaults to a separate private owner for each session.
    pub fn sessions(port: Arc<dyn LogicalMemoryPort>, namespace: String) -> io::Result<Self> {
        MemoryAccess {
            namespace: namespace.clone(),
            actor: "validation".into(),
        }
        .validate()
        .map_err(io::Error::other)?;
        checked_binding(port.as_ref())?;
        Ok(Self {
            port,
            scope: Scope::Sessions(namespace),
        })
    }
    fn access(&self, context: &ToolContext) -> io::Result<MemoryAccess> {
        if context.session.is_empty()
            || context.turn.is_empty()
            || context.session.len() > 256
            || context.turn.len() > 256
            || context
                .session
                .chars()
                .chain(context.turn.chars())
                .any(char::is_control)
        {
            return Err(io::Error::other("Invalid memory tool scope"));
        }
        let access = match &self.scope {
            Scope::Fixed(a) => a.clone(),
            Scope::Sessions(namespace) => MemoryAccess {
                namespace: namespace.clone(),
                actor: session_actor(&context.session),
            },
        };
        access.validate().map_err(io::Error::other)?;
        Ok(access)
    }
    fn operation(&self, context: &ToolContext, call: &ToolCall) -> io::Result<MemoryCall> {
        call.validate()?;
        let access = self.access(context)?;
        let args = call
            .arguments
            .as_object()
            .ok_or_else(|| io::Error::other("Expected memory arguments"))?;
        let fields: &[&str] = match call.name.as_str() {
            "memory_put" => &["id", "body", "expected_revision"],
            "memory_get" => &["id"],
            "memory_search" => &["query"],
            "memory_delete" => &["id", "expected_revision"],
            _ => return Err(io::Error::other("Unknown memory tool")),
        };
        if args.keys().any(|key| !fields.contains(&key.as_str())) {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied,"Memory arguments cannot select a namespace, actor, destination or unexposed option"));
        }
        let text = |key: &str| -> io::Result<String> {
            args.get(key)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| io::Error::other(format!("Supply memory {key}")))
        };
        if call.name != "memory_search" {
            let id = text("id")?;
            if id.is_empty() || id.len() > 128 || id.chars().any(char::is_control) {
                return Err(io::Error::other("Invalid memory ID"));
            }
        }
        if call.name == "memory_put" && text("body")?.len() > 65536 {
            return Err(io::Error::other("Memory body exceeds 64 KiB"));
        }
        let expected_revision = match args.get("expected_revision") {
            None | Some(Value::Null) => None,
            Some(v) => Some(
                v.as_u64()
                    .ok_or_else(|| io::Error::other("Expected a nonnegative memory revision"))?,
            ),
        };
        // Keep the retained host's request-ID derivation for mutation deduplication.
        let request_id = format!(
            "{:x}",
            Sha256::digest(format!("{}:{}", context.turn, call.id))
        );
        Ok(match call.name.as_str() {
            "memory_put" => MemoryCall::Put {
                request: PutMemory {
                    access,
                    request_id,
                    id: text("id")?,
                    kind: "note".into(),
                    body: text("body")?,
                    metadata: Default::default(),
                    acl: Default::default(),
                    expected_revision,
                },
            },
            "memory_get" => MemoryCall::Get {
                access,
                id: text("id")?,
            },
            "memory_search" => MemoryCall::Search {
                access,
                query: text("query")?,
                limit: 10,
            },
            _ => MemoryCall::Delete {
                request: DeleteMemory {
                    access,
                    request_id,
                    id: text("id")?,
                    expected_revision,
                },
            },
        })
    }
    fn perform(
        &self,
        context: &ToolContext,
        call: &ToolCall,
        authority: &ToolBinding,
        control: &Control<'_>,
    ) -> io::Result<ToolReply> {
        control.check()?;
        if self.binding(context, call)? != *authority {
            return Err(io::Error::other("Memory authority changed before dispatch"));
        }
        let operation = self.operation(context, call)?;
        let mutation = matches!(call.name.as_str(), "memory_put" | "memory_delete");
        let task = || -> io::Result<ToolReply> {
            static EXECUTOR: OnceLock<std::result::Result<tokio::runtime::Runtime, String>> =
                OnceLock::new();
            let runtime = EXECUTOR
                .get_or_init(|| {
                    tokio::runtime::Builder::new_multi_thread()
                        .worker_threads(2)
                        .enable_all()
                        .build()
                        .map_err(|e| e.to_string())
                })
                .as_ref()
                .map_err(|e| io::Error::other(e.clone()))?;
            runtime.block_on(async{
    tokio::select!{biased;
     _=async{loop{if control.check().is_err(){break;}tokio::time::sleep(Duration::from_millis(10)).await;}}=>{control.check()?;Err(io::Error::other("Memory call interrupted"))},
     reply=self.port.call(Some(authority.generation),operation)=>{
      let reply=reply.map_err(|error|{
       if mutation && !matches!(error.code,ErrorCode::Invalid|ErrorCode::Unauthorized|ErrorCode::Conflict|ErrorCode::StaleGeneration){io::Error::other(ServiceError::new(ErrorCode::Uncertain,"Memory write outcome is uncertain. Inspect its receipt before further writes in this scope."))}else{io::Error::other(error)}
      })?;control.check()?;Ok(ToolReply::success(serde_json::to_string(&reply)?))
     }
    }
   })
        };
        // A synchronous ToolPort may be called from an Axum blocking task with an entered handle.
        if tokio::runtime::Handle::try_current().is_ok() {
            std::thread::scope(|s| {
                s.spawn(task)
                    .join()
                    .map_err(|_| io::Error::other("Memory worker failed"))?
            })
        } else {
            task()
        }
    }
}
impl ToolPort for MemoryTools {
    fn discover(&self) -> io::Result<Vec<Value>> {
        Ok(specs())
    }
    fn execute(&self, _: &ToolCall, _: &Control<'_>) -> io::Result<ToolReply> {
        Err(io::Error::other(
            "Memory tools require an explicit session and turn",
        ))
    }
    fn binding(&self, context: &ToolContext, call: &ToolCall) -> io::Result<ToolBinding> {
        self.operation(context, call)?;
        let scope = self.access(context)?;
        let binding = checked_binding(self.port.as_ref())?;
        Ok(ToolBinding {
            binding: binding.name,
            generation: binding.generation,
            resource: format!("{}:{}", scope.namespace, scope.actor),
            class: if matches!(call.name.as_str(), "memory_get" | "memory_search") {
                ActionClass::ReadOnly
            } else {
                ActionClass::Mutation
            },
            detail: format!(
                "{} {} in this scope's private memory",
                call.name,
                call.arguments["id"].as_str().unwrap_or("search")
            ),
            fingerprint: format!("{:x}", Sha256::digest(serde_json::to_vec(&scope)?)),
        })
    }
    fn execute_in(
        &self,
        context: &ToolContext,
        call: &ToolCall,
        control: &Control<'_>,
    ) -> io::Result<ToolReply> {
        let binding = self.binding(context, call)?;
        self.execute_authorized_in(context, call, &binding, control)
    }
    fn execute_authorized_in(
        &self,
        context: &ToolContext,
        call: &ToolCall,
        authority: &ToolBinding,
        control: &Control<'_>,
    ) -> io::Result<ToolReply> {
        let limit = Instant::now() + Duration::from_secs(40);
        let bounded = Control {
            cancelled: control.cancelled,
            deadline: Some(control.deadline.unwrap_or(limit).min(limit)),
        };
        self.perform(context, call, authority, &bounded)
    }
    fn dispatch_access_in(
        &self,
        context: &ToolContext,
        call: &ToolCall,
    ) -> io::Result<DispatchAccess> {
        let access = self.access(context)?;
        self.operation(context, call)?;
        Ok(DispatchAccess::Resource(format!(
            "memory:{}:{}",
            access.namespace, access.actor
        )))
    }
}
