//! Run-scoped workspace capability port. Adapters own their state and tool clients.
use kageha_contracts::execution::{Control, ToolPort};
use kageha_workspace::worker::{Manager, Options, Snapshot};
use std::{io, path::PathBuf, sync::Arc};
pub trait WorkspacePort: Send + Sync {
    fn binding(&self) -> io::Result<String>;
    fn validate(&self, options: &Options, control: &Control<'_>) -> io::Result<()>;
    fn prepare(&self, key: &str, options: &Options, control: &Control<'_>) -> io::Result<Snapshot>;
    fn snapshot(&self, key: &str, options: &Options) -> io::Result<Snapshot>;
    fn tools(
        &self,
        key: &str,
        options: &Options,
    ) -> io::Result<Option<Arc<dyn ToolPort + Send + Sync>>>;
    fn settle(
        &self,
        key: &str,
        options: &Options,
        successful: bool,
        terminal: bool,
        control: &Control<'_>,
    ) -> io::Result<Snapshot>;
    fn reconcile(
        &self,
        key: &str,
        options: &Options,
        control: &Control<'_>,
    ) -> io::Result<Snapshot>;
}
pub struct LocalWorkspace {
    manager: Manager,
    document_worker: Option<PathBuf>,
    local_files: bool,
}
impl LocalWorkspace {
    pub fn new(manager: Manager, document_worker: Option<PathBuf>, local_files: bool) -> Self {
        Self {
            manager,
            document_worker,
            local_files,
        }
    }
}
impl WorkspacePort for LocalWorkspace {
    fn binding(&self) -> io::Result<String> {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(self.manager.source())?;
        Ok(super::hash(&serde_json::json!([
            self.manager.source(),
            meta.dev(),
            meta.ino(),
            self.local_files,
            self.document_worker
        ])))
    }
    fn validate(&self, options: &Options, control: &Control<'_>) -> io::Result<()> {
        if options.isolated() && !self.local_files {
            return Err(io::Error::other(
                "Local Git isolation is unavailable for the configured remote workspace",
            ));
        }
        self.manager.validate(options, control)
    }
    fn prepare(&self, key: &str, options: &Options, control: &Control<'_>) -> io::Result<Snapshot> {
        self.manager.prepare(key, options, control)
    }
    fn snapshot(&self, key: &str, options: &Options) -> io::Result<Snapshot> {
        self.manager.snapshot(key, options)
    }
    fn tools(
        &self,
        key: &str,
        options: &Options,
    ) -> io::Result<Option<Arc<dyn ToolPort + Send + Sync>>> {
        if !options.isolated() {
            return Ok(None);
        }
        let root = self.manager.execution_root(key, options)?;
        let mut ports: Vec<Arc<dyn ToolPort + Send + Sync>> =
            vec![Arc::new(kageha_workspace::ReadWorkspace::new(&root)?)];
        if let Some(worker) = &self.document_worker {
            ports.push(Arc::new(kageha_workspace::DocumentTools::new(
                &root, worker,
            )?));
        }
        Ok(Some(Arc::new(kageha_tools::Composite::new(ports)?)))
    }
    fn settle(
        &self,
        key: &str,
        options: &Options,
        successful: bool,
        terminal: bool,
        control: &Control<'_>,
    ) -> io::Result<Snapshot> {
        self.manager
            .settle(key, options, successful, terminal, control)
    }
    fn reconcile(
        &self,
        key: &str,
        options: &Options,
        control: &Control<'_>,
    ) -> io::Result<Snapshot> {
        self.manager.reconcile(key, options, control)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{workers::Composition, ExecutionHost};
    use kageha_contracts::{execution::*, policy::PermissionProfile};
    use kageha_runtime::lifecycle::*;
    use serde_json::{json, Value};
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct ValueTool(&'static str, Arc<AtomicUsize>);
    impl ToolPort for ValueTool {
        fn discover(&self) -> io::Result<Vec<Value>> {
            Ok(vec![
                json!({"type":"function","function":{"name":"custom","parameters":{"type":"object"}}}),
            ])
        }
        fn execute(&self, _: &ToolCall, _: &Control<'_>) -> io::Result<ToolReply> {
            self.1.fetch_add(1, Ordering::SeqCst);
            Ok(ToolReply::success(self.0.into()))
        }
    }
    struct Virtual {
        label: &'static str,
        calls: Arc<AtomicUsize>,
        switch: Option<Arc<std::sync::atomic::AtomicBool>>,
    }
    impl Virtual {
        fn snapshot_value(&self, options: &Options) -> Snapshot {
            Snapshot {
                mode: "shared".into(),
                source_root: "/virtual-source".into(),
                root: None,
                options: options.clone(),
                operation: None,
                cleanup: None,
                retained: true,
                ready_for_input: true,
                reason: None,
            }
        }
    }
    impl WorkspacePort for Virtual {
        fn binding(&self) -> io::Result<String> {
            Ok(
                if self
                    .switch
                    .as_ref()
                    .is_some_and(|s| s.load(Ordering::SeqCst))
                {
                    format!("{}:changed", self.label)
                } else {
                    self.label.into()
                },
            )
        }
        fn validate(&self, _: &Options, c: &Control<'_>) -> io::Result<()> {
            c.check()
        }
        fn prepare(&self, _: &str, o: &Options, c: &Control<'_>) -> io::Result<Snapshot> {
            c.check()?;
            Ok(self.snapshot_value(o))
        }
        fn snapshot(&self, _: &str, o: &Options) -> io::Result<Snapshot> {
            Ok(self.snapshot_value(o))
        }
        fn tools(
            &self,
            _: &str,
            _: &Options,
        ) -> io::Result<Option<Arc<dyn ToolPort + Send + Sync>>> {
            Ok(Some(Arc::new(ValueTool(self.label, self.calls.clone()))))
        }
        fn settle(
            &self,
            _: &str,
            o: &Options,
            _: bool,
            _: bool,
            c: &Control<'_>,
        ) -> io::Result<Snapshot> {
            c.check()?;
            Ok(self.snapshot_value(o))
        }
        fn reconcile(&self, _: &str, o: &Options, c: &Control<'_>) -> io::Result<Snapshot> {
            c.check()?;
            Ok(self.snapshot_value(o))
        }
    }
    struct Model;
    impl ModelPort for Model {
        fn complete(&self, r: ModelRequest, _: &Control<'_>) -> io::Result<AssistantMessage> {
            Ok(if r.messages.last().is_some_and(|m| m["role"] == "tool") {
                AssistantMessage {
                    continuation: None,
                    content: r.messages.last().unwrap()["content"].clone(),
                    tool_calls: vec![],
                }
            } else {
                AssistantMessage {
                    continuation: None,
                    content: Value::Null,
                    tool_calls: vec![ToolCall {
                        id: "custom-call".into(),
                        name: "custom".into(),
                        arguments: json!({}),
                    }],
                }
            })
        }
    }
    #[test]
    fn alternate_workspace_ports_work_without_a_filesystem_or_consumer_changes() {
        for label in ["workspace-one", "workspace-two"] {
            let root = tempfile::tempdir().unwrap();
            let store = Arc::new(
                kageha_runtime_store::SqliteRunStore::open(&root.path().join("runtime.sqlite"))
                    .unwrap(),
            );
            let journal = Arc::new(
                kageha_policy::journal::Journal::open(&root.path().join("policy.sqlite")).unwrap(),
            );
            journal
                .set_profile("parent", PermissionProfile::Full)
                .unwrap();
            let calls = Arc::new(AtomicUsize::new(0));
            let base_calls = Arc::new(AtomicUsize::new(0));
            let workers = Composition::with_workspace(
                Arc::new(Virtual {
                    label,
                    calls: calls.clone(),
                    switch: None,
                }),
                vec![],
            )
            .unwrap();
            let host = ExecutionHost::with_workers(
                store.clone(),
                Arc::new(Model),
                Arc::new(ValueTool("base-must-not-run", base_calls.clone())),
                Limits::default(),
                Arc::new(kageha_policy::DefaultPolicy),
                journal,
                workers,
            )
            .unwrap();
            host.submit(&RunSpec {
                id: "run".into(),
                session: "parent".into(),
                turn: "turn".into(),
                request: "request".into(),
                parent: None,
                parent_run: None,
                agent: "main".into(),
                objective: "read virtual value".into(),
                depth: 0,
                input: json!({"messages":[{"role":"user","content":"read"}],"max_model_calls":3}),
            })
            .unwrap();
            host.supervisor.start().unwrap();
            let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                let run = store.get("run").unwrap().unwrap();
                if run.state.terminal() {
                    assert_eq!(run.state, RunState::Succeeded);
                    assert_eq!(run.completion.unwrap().result["message"], label);
                    break;
                }
                assert!(std::time::Instant::now() < until);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            host.supervisor.shutdown().unwrap();
            host.supervisor
                .join(std::time::Duration::from_secs(2))
                .unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert_eq!(base_calls.load(Ordering::SeqCst), 0);
        }
    }
    #[test]
    fn changing_workspace_during_authorization_dispatches_no_tool() {
        use kageha_contracts::policy::*;
        struct Switch(Arc<std::sync::atomic::AtomicBool>);
        impl PolicyPort for Switch {
            fn evaluate(
                &self,
                _: &ApprovalRequest,
                _: PermissionMode,
            ) -> io::Result<Authorization> {
                self.0.store(true, Ordering::SeqCst);
                Ok(Authorization::Allow)
            }
        }
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(
            kageha_runtime_store::SqliteRunStore::open(&root.path().join("runtime.sqlite"))
                .unwrap(),
        );
        let journal = Arc::new(
            kageha_policy::journal::Journal::open(&root.path().join("policy.sqlite")).unwrap(),
        );
        let changed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let calls = Arc::new(AtomicUsize::new(0));
        let workers = Composition::with_workspace(
            Arc::new(Virtual {
                label: "before",
                calls: calls.clone(),
                switch: Some(changed.clone()),
            }),
            vec![],
        )
        .unwrap();
        let host = ExecutionHost::with_workers(
            store.clone(),
            Arc::new(Model),
            Arc::new(ValueTool("base", calls.clone())),
            Limits::default(),
            Arc::new(Switch(changed)),
            journal,
            workers,
        )
        .unwrap();
        host.submit(&RunSpec {
            id: "run".into(),
            session: "parent".into(),
            turn: "turn".into(),
            request: "request".into(),
            parent: None,
            parent_run: None,
            agent: "main".into(),
            objective: "test authority fence".into(),
            depth: 0,
            input: json!({"messages":[{"role":"user","content":"read"}],"max_model_calls":3}),
        })
        .unwrap();
        host.supervisor.start().unwrap();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let r = store.get("run").unwrap().unwrap();
            if r.state.terminal() {
                assert_eq!(r.state, RunState::Failed);
                assert!(r.completion.unwrap().result["message"]
                    .as_str()
                    .unwrap()
                    .contains("binding generation changed"));
                break;
            }
            assert!(std::time::Instant::now() < until);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        host.supervisor.shutdown().unwrap();
        host.supervisor
            .join(std::time::Duration::from_secs(2))
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(store
            .effect_get("turn", "tool:custom-call")
            .unwrap()
            .is_none());
    }
    #[test]
    fn accepting_isolation_without_providing_it_never_reaches_tools() {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(
            kageha_runtime_store::SqliteRunStore::open(&root.path().join("runtime.sqlite"))
                .unwrap(),
        );
        let journal = Arc::new(
            kageha_policy::journal::Journal::open(&root.path().join("policy.sqlite")).unwrap(),
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let workers = Composition::with_workspace(
            Arc::new(Virtual {
                label: "broken-adapter",
                calls: calls.clone(),
                switch: None,
            }),
            vec![],
        )
        .unwrap();
        let host = ExecutionHost::with_workers(
            store.clone(),
            Arc::new(Model),
            Arc::new(ValueTool("source-must-not-run", calls.clone())),
            Limits::default(),
            Arc::new(kageha_policy::DefaultPolicy),
            journal,
            workers,
        )
        .unwrap();
        host.submit(&RunSpec{id:"run".into(),session:"parent".into(),turn:"turn".into(),request:"request".into(),parent:None,parent_run:None,agent:"main".into(),objective:"request isolation".into(),depth:0,input:json!({"messages":[{"role":"user","content":"read"}],"max_model_calls":3,"worker_workspace":{"isolation":"worktree"}})}).unwrap();
        host.supervisor.start().unwrap();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let r = store.get("run").unwrap().unwrap();
            if r.state.terminal() {
                assert_eq!(r.state, RunState::Failed);
                assert!(r.completion.unwrap().result["message"]
                    .as_str()
                    .unwrap()
                    .contains("requested workspace isolation"));
                break;
            }
            assert!(std::time::Instant::now() < until);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        host.supervisor.shutdown().unwrap();
        host.supervisor
            .join(std::time::Duration::from_secs(2))
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
