//! Standalone lifecycle composition. Hook configuration is captured by the host,
//! never supplied by a model. The same execution library serves the WebUI.
use crate::{digest, BoundTools};
use kageha_contracts::execution::*;
use kageha_runtime::lifecycle::{RunRecord, RunStore};
use kageha_workspace::hooks::{Executor, Hook, Outcome, Set};
use serde_json::{json, Value};
use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

pub struct Lifecycle {
    paths: Vec<PathBuf>,
    source: PathBuf,
    pub journal: Arc<kageha_execution::journal::Journal>,
    local_commands: bool,
}
pub(crate) struct RunHooks<'a> {
    owner: &'a Lifecycle,
    set: Set,
    root: PathBuf,
    root_binding: String,
}
impl Lifecycle {
    /// Explicit global files are followed by the workspace's .kageha/hooks.json.
    /// Environment variables are not consulted by library consumers.
    pub fn new(
        source: &Path,
        files: Vec<PathBuf>,
        journal: Arc<kageha_execution::journal::Journal>,
        local_commands: bool,
    ) -> io::Result<Self> {
        if files.len() > 32 || files.iter().any(|p| !p.is_absolute()) {
            return Err(io::Error::other("Configure at most 32 absolute hook paths"));
        }
        let source = source.canonicalize()?;
        let mut paths = files;
        paths.push(source.join(".kageha/hooks.json"));
        let mut seen = std::collections::HashSet::new();
        paths.retain(|p| seen.insert(p.clone()));
        Ok(Self {
            paths,
            source,
            journal,
            local_commands,
        })
    }
    pub(crate) fn prepare<'a>(
        &'a self,
        run: &RunRecord,
        store: &dyn RunStore,
        root: Option<&str>,
        control: &Control<'_>,
    ) -> io::Result<RunHooks<'a>> {
        let current = Set::load(&self.paths, control)?;
        if !self.local_commands && current.hooks.iter().any(|h| !h.command.is_empty()) {
            return Err(io::Error::other(
                "Local hook commands are unavailable for remote execution",
            ));
        }
        let mut pinned = None;
        if let Some(prior) = run.spec.input["resume_from"].as_str() {
            let previous = store
                .get(prior)
                .map_err(io::Error::other)?
                .ok_or_else(|| io::Error::other("Hook continuation source is missing"))?;
            pinned = store
                .effect_get(&previous.spec.turn, "runtime:hooks")
                .map_err(io::Error::other)?;
            if pinned.as_ref().is_some_and(|e| e.receipt.is_none())
                || (pinned.is_none() && !current.hooks.is_empty())
            {
                return Err(io::Error::other(
                    "Continuation has no confirmed hook configuration",
                ));
            }
        }
        let captured = serde_json::to_value(&current)?;
        if let Some(old) = pinned {
            if old.receipt.as_ref() != Some(&captured) {
                return Err(kageha_contracts::policy::stop(
                    "Hooks changed since the continuation checkpoint; start a new assignment",
                ));
            }
        }
        let key = digest(&captured);
        if let Some(old) = store
            .effect_get(&run.spec.turn, "runtime:hooks")
            .map_err(io::Error::other)?
        {
            if old.digest != key || old.receipt.as_ref() != Some(&captured) {
                return Err(kageha_contracts::policy::stop(
                    "Captured hook configuration changed",
                ));
            }
        } else {
            store
                .effect_begin(&run.spec.turn, "runtime:hooks", &key)
                .map_err(io::Error::other)?;
            store
                .effect_finish(&run.spec.turn, "runtime:hooks", &key, captured)
                .map_err(io::Error::other)?;
        }
        let root = root
            .map(PathBuf::from)
            .unwrap_or_else(|| self.source.clone())
            .canonicalize()?;
        let root_binding = root_identity(&root)?;
        Ok(RunHooks {
            owner: self,
            set: current,
            root,
            root_binding,
        })
    }
}
fn root_identity(root: &Path) -> io::Result<String> {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(root)?;
    if !m.is_dir() {
        return Err(io::Error::other("Hook execution root is unavailable"));
    }
    Ok(digest(&json!([root, m.dev(), m.ino()])))
}
impl RunHooks<'_> {
    pub(crate) fn run(
        &self,
        event: &str,
        call: Option<&ToolCall>,
        reply: Option<&ToolReply>,
        tools: &BoundTools<'_>,
        control: &Control<'_>,
    ) -> io::Result<Outcome> {
        let mut payload = json!({"event":event,"tool_name":call.map(|c|c.name.as_str()).unwrap_or(""),"tool_call_id":call.map(|c|c.id.as_str()).unwrap_or(event),"arguments":call.map(|c|&c.arguments),"project_root":self.root,"session_id":tools.run.spec.session,"turn_id":tools.run.spec.turn});
        if let Some(reply) = reply {
            payload["result_preview"] = json!(reply.content.chars().take(4000).collect::<String>());
        }
        let out = self.set.run(
            event,
            call.map(|c| c.name.as_str()).unwrap_or(""),
            &payload,
            &Host { hooks: self, tools },
            control,
        )?;
        if !out.allowed {
            return Err(kageha_contracts::policy::stop(if out.message.is_empty() {
                format!("The configured {event} hook blocked this action")
            } else {
                out.message
            }));
        }
        Ok(out)
    }
}
struct Host<'a, 'b> {
    hooks: &'a RunHooks<'a>,
    tools: &'a BoundTools<'b>,
}
impl Executor for Host<'_, '_> {
    fn execute(&self, hook: &Hook, payload: &Value, control: &Control<'_>) -> io::Result<Outcome> {
        let port = HookPort {
            hooks: self.hooks,
            hook,
            payload,
        };
        // Private synthetic port is never advertised to the model. It uses the
        // identical policy/review/receipt path, with recursion disabled.
        let tools = BoundTools {
            base: &port,
            hooks: None,
            completed_calls: Default::default(),
            completed_mutations: Default::default(),
            journal: self.tools.journal,
            model: self.tools.model,
            store: self.tools.store,
            run: self.tools.run,
            policy: self.tools.policy,
            approvals: self.tools.approvals,
        };
        let call = ToolCall {
            id: format!(
                "hook:{}",
                digest(&json!([
                    payload["tool_call_id"],
                    hook.fingerprint(),
                    hook.event
                ]))
            ),
            name: "project_hook".into(),
            arguments: kageha_execution::hooks::context(hook, payload, &self.hooks.root)?,
        };
        let result = tools.execute(&call, control).map_err(|e| {
            if is_uncertain(&e)
                || kageha_contracts::policy::is_stop(&e)
                || matches!(
                    e.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::TimedOut
                )
            {
                e
            } else {
                kageha_contracts::policy::stop(format!("Configured hook stopped this action: {e}"))
            }
        })?;
        if result.is_error {
            return Err(kageha_contracts::policy::stop(
                "Configured hook could not complete; inspect the original receipt",
            ));
        }
        serde_json::from_str(&result.content).map_err(io::Error::other)
    }
}
struct HookPort<'a> {
    hooks: &'a RunHooks<'a>,
    hook: &'a Hook,
    payload: &'a Value,
}
impl ToolPort for HookPort<'_> {
    fn discover(&self) -> io::Result<Vec<Value>> {
        Ok(vec![
            json!({"type":"function","function":{"name":"project_hook"}}),
        ])
    }
    fn binding(&self, _: &ToolContext, _: &ToolCall) -> io::Result<ToolBinding> {
        kageha_execution::hooks::verify(
            self.hook,
            &Control {
                cancelled: &std::sync::atomic::AtomicBool::new(false),
                deadline: None,
            },
        )?;
        if root_identity(&self.hooks.root)? != self.hooks.root_binding {
            return Err(kageha_contracts::policy::stop(
                "Hook execution root changed",
            ));
        }
        Ok(ToolBinding {
            binding: "hooks.workspace".into(),
            generation: 1,
            resource: digest(&json!([self.hooks.root_binding, self.hook.resource()])),
            class: kageha_contracts::policy::ActionClass::Mutation,
            detail: format!(
                "Run {} hook from {}. Exact execution context: {}",
                self.hook.event,
                self.hook.config_path.display(),
                kageha_execution::hooks::context(self.hook, self.payload, &self.hooks.root)?
            ),
            fingerprint: digest(&json!([self.hook, self.hooks.root_binding])),
        })
    }
    fn execute(&self, _: &ToolCall, _: &Control<'_>) -> io::Result<ToolReply> {
        Err(io::Error::other("Hook requires a host execution scope"))
    }
    fn execute_in(
        &self,
        context: &ToolContext,
        _: &ToolCall,
        control: &Control<'_>,
    ) -> io::Result<ToolReply> {
        let out = kageha_execution::hooks::execute(
            &self.hooks.owner.journal,
            context,
            self.hook,
            self.payload,
            &self.hooks.root,
            control,
            |_| Ok(true),
        )?;
        Ok(ToolReply::success(serde_json::to_string(&out)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ExecutionHost;
    use kageha_contracts::policy::*;
    use kageha_runtime::lifecycle::*;
    use kageha_runtime_store::SqliteRunStore;
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::{Duration, Instant},
    };
    struct Model;
    impl ModelPort for Model {
        fn complete(&self, r: ModelRequest, _: &Control<'_>) -> io::Result<AssistantMessage> {
            if r.messages.last().unwrap()["role"] == "tool" {
                assert!(r.messages.last().unwrap()["content"]
                    .as_str()
                    .unwrap()
                    .contains("beforeShell"));
                Ok(AssistantMessage {
                    continuation: None,
                    content: json!("completed"),
                    tool_calls: vec![],
                })
            } else {
                Ok(AssistantMessage {
                    continuation: None,
                    content: Value::Null,
                    tool_calls: vec![ToolCall {
                        id: "shell".into(),
                        name: "bash".into(),
                        arguments: json!({}),
                    }],
                })
            }
        }
    }
    struct Tools(Arc<AtomicUsize>);
    impl ToolPort for Tools {
        fn discover(&self) -> io::Result<Vec<Value>> {
            Ok(vec![json!({"function":{"name":"bash"}})])
        }
        fn execute(&self, _: &ToolCall, _: &Control<'_>) -> io::Result<ToolReply> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(ToolReply::success("fixture tool result".into()))
        }
    }
    struct Allow;
    impl PolicyPort for Allow {
        fn evaluate(&self, _: &ApprovalRequest, _: PermissionMode) -> io::Result<Authorization> {
            Ok(Authorization::Allow)
        }
    }
    #[test]
    fn legacy_continuation_cannot_silently_enable_new_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteRunStore::open(&dir.path().join("runtime.sqlite")).unwrap();
        let receipts = Arc::new(
            kageha_execution::journal::Journal::open(&dir.path().join("hooks/execution.sqlite"))
                .unwrap(),
        );
        let lifecycle = Lifecycle::new(dir.path(), vec![], receipts, true).unwrap();
        let old = RunSpec {
            id: "old".into(),
            session: "s".into(),
            turn: "old-turn".into(),
            request: "old-request".into(),
            parent: None,
            parent_run: None,
            agent: "main".into(),
            objective: "fixture".into(),
            depth: 0,
            input: json!({"messages":[{"role":"user","content":"fixture"}]}),
        };
        store.stage(&old, 4).unwrap();
        let mut next = old.clone();
        next.id = "next".into();
        next.turn = "next-turn".into();
        next.request = "next-request".into();
        next.input["resume_from"] = json!("old");
        let run = store.stage(&next, 4).unwrap();
        let flag = std::sync::atomic::AtomicBool::new(false);
        let control = Control {
            cancelled: &flag,
            deadline: None,
        };
        assert!(lifecycle.prepare(&run, &store, None, &control).is_ok());
        std::fs::create_dir(dir.path().join(".kageha")).unwrap();
        std::fs::write(
            dir.path().join(".kageha/hooks.json"),
            json!({"hooks":[{"event":"preToolUse","command":"printf added"}]}).to_string(),
        )
        .unwrap();
        assert!(lifecycle.prepare(&run, &store, None, &control).is_err());
    }
    #[test]
    fn embedding_runs_before_shell_through_the_same_policy_and_receipt_ports() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".kageha")).unwrap();
        let hooks = json!({"hooks":[{"event":"beforeShell","command":"printf '{\"extra_context\":\"beforeShell\"}'"}]});
        std::fs::write(dir.path().join(".kageha/hooks.json"), hooks.to_string()).unwrap();
        let store = Arc::new(SqliteRunStore::open(&dir.path().join("runtime.sqlite")).unwrap());
        let policy = Arc::new(
            kageha_policy::journal::Journal::open(&dir.path().join("policy.sqlite")).unwrap(),
        );
        let receipts = Arc::new(
            kageha_execution::journal::Journal::open(&dir.path().join("hooks/execution.sqlite"))
                .unwrap(),
        );
        let lifecycle =
            Arc::new(Lifecycle::new(dir.path(), vec![], receipts.clone(), true).unwrap());
        let calls = Arc::new(AtomicUsize::new(0));
        let host = ExecutionHost::with_lifecycle(
            store.clone(),
            Arc::new(Model),
            Arc::new(Tools(calls.clone())),
            Limits::default(),
            Arc::new(Allow),
            policy,
            None,
            lifecycle,
        )
        .unwrap();
        let spec = RunSpec {
            id: "run".into(),
            session: "session".into(),
            turn: "turn".into(),
            request: "request".into(),
            parent: None,
            parent_run: None,
            agent: "main".into(),
            objective: "fixture".into(),
            depth: 0,
            input: json!({"messages":[{"role":"user","content":"fixture"}]}),
        };
        host.submit(&spec).unwrap();
        host.supervisor.start().unwrap();
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            let r = store.get("run").unwrap().unwrap();
            if r.state.terminal() {
                assert_eq!(r.state, RunState::Succeeded, "{:?}", r.completion);
                break;
            }
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(receipts.receipts(Some("session")).unwrap().len(), 1);
        host.supervisor.shutdown().unwrap();
        host.supervisor.join(Duration::from_secs(5)).unwrap();
    }
}
