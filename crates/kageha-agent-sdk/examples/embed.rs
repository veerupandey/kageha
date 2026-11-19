//! In-process durable agent. No HTTP server, WebUI, Python or API key required.
//! Set KAGEHA_EXAMPLE_ENDPOINT / KAGEHA_EXAMPLE_MODEL / KAGEHA_EXAMPLE_KEY for a real provider.
use kageha_contracts::execution::*;
use kageha_runtime::lifecycle::{Limits, RunSpec, RunState, RunStore};
use kageha_runtime_service::ExecutionHost;
use kageha_runtime_store::SqliteRunStore;
use serde_json::{json, Value};
use std::{
    io,
    sync::Arc,
    time::{Duration, Instant},
};
struct DemonstrationModel;
impl ModelPort for DemonstrationModel {
    fn complete(
        &self,
        request: ModelRequest,
        control: &Control<'_>,
    ) -> io::Result<AssistantMessage> {
        control.check()?;
        if let Some(message) = request.messages.last().filter(|v| v["role"] == "tool") {
            Ok(AssistantMessage {
                continuation: None,
                content: message["content"].clone(),
                tool_calls: vec![],
            })
        } else {
            Ok(AssistantMessage {
                continuation: None,
                content: Value::Null,
                tool_calls: vec![ToolCall {
                    id: "read-proof".into(),
                    name: "read_file".into(),
                    arguments: json!({"path":"notes.txt"}),
                }],
            })
        }
    }
}
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let workspace = directory.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    std::fs::write(
        workspace.join("notes.txt"),
        "Embedded Rust agent read and verified this file.",
    )?;
    let store = Arc::new(SqliteRunStore::open(
        &directory.path().join("runtime.sqlite"),
    )?);
    let model: Arc<dyn ModelPort + Send + Sync> =
        if let Ok(url) = std::env::var("KAGEHA_EXAMPLE_ENDPOINT") {
            Arc::new(kageha_models::HttpModel::new(vec![
                kageha_models::Provider {
                    url,
                    key: std::env::var("KAGEHA_EXAMPLE_KEY").unwrap_or_default(),
                    model: std::env::var("KAGEHA_EXAMPLE_MODEL")?,
                    protocol: "openai_compat".into(),
                },
            ])?)
        } else {
            Arc::new(DemonstrationModel)
        };
    let tools = Arc::new(kageha_workspace::ReadWorkspace::new(&workspace)?);
    let host = ExecutionHost::new(store.clone(), model, tools, Limits::default())?;
    host.supervisor.start()?;
    let outcome = (|| -> std::result::Result<(), Box<dyn std::error::Error>> {
        host.submit(&RunSpec{id:"example-run".into(),session:"example-chat".into(),turn:"example-turn".into(),request:"example-request".into(),agent:"main".into(),objective:"Read notes.txt and report its contents".into(),depth:0,parent:None,parent_run:None,input:json!({"messages":[{"role":"user","content":"Read notes.txt and report its contents"}],"max_model_calls":8,"timeout_ms":30000})})?;
        let deadline = Instant::now() + Duration::from_secs(35);
        let mut cursor = 0;
        loop {
            for event in store.events("example-run", cursor, 128)? {
                cursor = event.sequence;
                println!("{}", serde_json::to_string(&event)?);
            }
            let record = store.get("example-run")?.ok_or("Run disappeared")?;
            if record.state.terminal() {
                if record.state != RunState::Succeeded {
                    return Err("Embedded run failed".into());
                }
                break;
            }
            if Instant::now() >= deadline {
                return Err("Embedded example deadline exceeded".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    })();
    host.supervisor.shutdown()?;
    host.supervisor.join(Duration::from_secs(5))?;
    outcome
}
