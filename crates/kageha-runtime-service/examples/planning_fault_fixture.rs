//! Fault-injection host for the planning acceptance test, not a production launcher.
//! It uses an ordinary RunStore adapter to stop immediately before publication.
use kageha_contracts::{ErrorCode, Result, ServiceError};
use kageha_runtime::lifecycle::*;
use kageha_runtime_service::{Components, ExecutionHost, ReadWorkspace};
use kageha_runtime_store::SqliteRunStore;
use serde_json::Value;
use std::{path::PathBuf, sync::Arc, time::Duration};
struct Paused {
    inner: Arc<SqliteRunStore>,
    state: PathBuf,
}
impl RunStore for Paused {
    fn events(&self, run: &str, after: u64, limit: usize) -> Result<Vec<RunEvent>> {
        self.inner.events(run, after, limit)
    }
    fn stage(&self, spec: &RunSpec, limit: usize) -> Result<RunRecord> {
        self.inner.stage(spec, limit)
    }
    fn import_run(&self, record: &RunRecord) -> Result<()> {
        self.inner.import_run(record)
    }
    fn activate(&self, id: &str) -> Result<RunRecord> {
        self.inner.activate(id)
    }
    fn get(&self, id: &str) -> Result<Option<RunRecord>> {
        self.inner.get(id)
    }
    fn find_turn(&self, turn: &str) -> Result<Option<RunRecord>> {
        self.inner.find_turn(turn)
    }
    fn list(&self) -> Result<Vec<RunRecord>> {
        self.inner.list()
    }
    fn active(&self) -> Result<Vec<RunRecord>> {
        self.inner.active()
    }
    fn claim(&self, depth: u32) -> Result<Option<RunRecord>> {
        self.inner.claim(depth)
    }
    fn finish(&self, id: &str, lease: &str, completion: Completion) -> Result<RunRecord> {
        self.inner.finish(id, lease, completion)
    }
    fn recover(&self, id: &str, completion: Completion) -> Result<RunRecord> {
        self.inner.recover(id, completion)
    }
    fn late_receipts(&self, id: &str) -> Result<Vec<Value>> {
        self.inner.late_receipts(id)
    }
    fn cancel(&self, id: &str) -> Result<RunRecord> {
        self.inner.cancel(id)
    }
    fn steer(&self, id: &str, message: &Steering) -> Result<()> {
        self.inner.steer(id, message)
    }
    fn steering(&self, id: &str) -> Result<Vec<Steering>> {
        self.inner.steering(id)
    }
    fn steering_history(&self, id: &str) -> Result<Vec<Steering>> {
        self.inner.steering_history(id)
    }
    fn erase_session(&self, session: &str, scopes: &[String]) -> Result<()> {
        self.inner.erase_session(session, scopes)
    }
    fn publications(&self) -> Result<Vec<RunRecord>> {
        self.inner.publications()
    }
    fn published(&self, id: &str, revision: u64) -> Result<()> {
        self.inner.published(id, revision)
    }
    fn effect_get(&self, scope: &str, id: &str) -> Result<Option<Effect>> {
        self.inner.effect_get(scope, id)
    }
    fn effect_begin(&self, scope: &str, id: &str, digest: &str) -> Result<Option<Value>> {
        self.inner.effect_begin(scope, id, digest)
    }
    fn effect_import(&self, effect: &Effect) -> Result<()> {
        self.inner.effect_import(effect)
    }

    fn effect_finish(&self, scope: &str, id: &str, digest: &str, result: Value) -> Result<()> {
        if scope == "turn-crash" && id == "tool:commit-before-receipt" {
            std::fs::write(
                self.state.join("plan-committed-before-receipt"),
                b"committed",
            )
            .map_err(|e| ServiceError::new(ErrorCode::Unavailable, e.to_string()))?;
            for _ in 0..3000 {
                if self.state.join("release-publication").exists() {
                    return self.inner.effect_finish(scope, id, digest, result);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            return Err(ServiceError::new(
                ErrorCode::Uncertain,
                "Fixture publication remains paused",
            ));
        }
        self.inner.effect_finish(scope, id, digest, result)
    }
}
#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().collect::<Vec<_>>();
    let root = PathBuf::from(&args[1]);
    let source = root.join("workspace");
    let state = root.join("state");
    let store = Arc::new(Paused {
        inner: Arc::new(SqliteRunStore::open(&state.join("runtime.sqlite"))?),
        state: state.clone(),
    });
    let model = Arc::new(kageha_models::HttpModel::new(vec![
        kageha_models::Provider {
            url: args[2].clone(),
            key: String::new(),
            model: "fixture".into(),
            protocol: "openai_compat".into(),
        },
    ])?);
    let journal = Arc::new(kageha_policy::journal::Journal::open(
        &state.join("policy.sqlite"),
    )?);
    let workers = kageha_runtime_service::workers::Composition::new(
        kageha_runtime_service::workers::Config {
            source: source.clone(),
            journal: state.join("workspace/worktrees.sqlite"),
            git: "/usr/bin/git".into(),
            document_worker: None,
            local_files: true,
            specialists: vec![kageha_runtime_service::workers::Specialist {
                id: "restricted".into(),
                instructions: "Read only".into(),
                allowed_tools: Some(vec!["read_file".into()]),
            }],
        },
    )?;
    let hooks = Arc::new(kageha_runtime_service::hooks::Lifecycle::new(
        &source,
        vec![],
        Arc::new(kageha_execution::journal::Journal::open(
            &state.join("hooks/execution.sqlite"),
        )?),
        true,
    )?);
    let planning = Arc::new(kageha_runtime_service::planning::Coordinator::new(
        Arc::new(kageha_workflow::planning::sqlite::SqliteStore::open(
            &state.join("planning/plans.sqlite"),
        )?),
    ));
    let host = ExecutionHost::with_components(
        store,
        model,
        Arc::new(ReadWorkspace::new(&source)?),
        Limits::default(),
        Arc::new(kageha_policy::DefaultPolicy),
        journal,
        Components {
            workers: Some(workers),
            hooks: Some(hooks),
            planning: Some(planning),
        },
    )?;
    let token = std::fs::read_to_string(root.join("token"))?;
    let router = kageha_runtime_service::http::router(host.clone(), token)?;
    host.supervisor.start()?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    eprintln!("runtime listening on {}", listener.local_addr()?);
    axum::serve(listener, router).await?;
    Ok(())
}
