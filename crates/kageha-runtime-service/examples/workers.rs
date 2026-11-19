//! Run specialists as a Rust library, without HTTP, WebUI, Node or Python.
use kageha_contracts::{execution::*, policy::PermissionProfile};
use kageha_runtime::lifecycle::*;
use kageha_runtime_service::{
    workers::{Composition, Config, Specialist},
    ExecutionHost, ReadWorkspace,
};
use serde_json::{json, Value};
use std::{io, path::Path, sync::Arc, time::Duration};
struct Model;
impl ModelPort for Model {
    fn complete(&self, request: ModelRequest, _: &Control<'_>) -> io::Result<AssistantMessage> {
        let intent = request
            .messages
            .iter()
            .find(|v| v["role"] == "user")
            .and_then(|v| v["content"].as_str())
            .unwrap_or("");
        let results = request
            .messages
            .iter()
            .filter(|v| v["role"] == "tool")
            .collect::<Vec<_>>();
        let (name, args) = if intent == "delegate" {
            if results.len() >= 2 {
                return Ok(AssistantMessage {
                    continuation: None,
                    content: json!("Independent specialist finished"),
                    tool_calls: vec![],
                });
            }
            if let Some(result) = results.first() {
                let child: Value = serde_json::from_str(result["content"].as_str().unwrap())?;
                (
                    "agent_wait_any",
                    json!({"job_ids":[child["spec"]["id"]],"timeout_ms":10000}),
                )
            } else {
                (
                    "agent_spawn",
                    json!({"agent_id":"worker","objective":"read proof","isolation":"worktree","keep_worktree":false}),
                )
            }
        } else {
            if let Some(result) = results.last() {
                return Ok(AssistantMessage {
                    continuation: None,
                    content: result["content"].clone(),
                    tool_calls: vec![],
                });
            }
            ("read_file", json!({"path":"proof.txt"}))
        };
        Ok(AssistantMessage {
            continuation: None,
            content: Value::Null,
            tool_calls: vec![ToolCall {
                id: format!("call-{}", results.len()),
                name: name.into(),
                arguments: args,
            }],
        })
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let source = root.path().join("source");
    std::fs::create_dir(&source)?;
    std::fs::write(source.join("proof.txt"), "Independent proof")?;
    for args in [
        vec!["init", "-b", "main"],
        vec!["add", "proof.txt"],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            "fixture",
        ],
    ] {
        if !std::process::Command::new("/usr/bin/git")
            .current_dir(&source)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(args)
            .output()?
            .status
            .success()
        {
            return Err("Disposable Git setup failed".into());
        }
    }
    let store = Arc::new(kageha_runtime_store::SqliteRunStore::open(
        &root.path().join("runtime.sqlite"),
    )?);
    let journal = Arc::new(kageha_policy::journal::Journal::open(
        &root.path().join("policy.sqlite"),
    )?);
    journal.set_profile("parent", PermissionProfile::Full)?;
    let workers = Composition::new(Config {
        source: source.clone(),
        journal: root.path().join("workspace/worktrees.sqlite"),
        git: Path::new("/usr/bin/git").into(),
        document_worker: None,
        local_files: true,
        specialists: vec![Specialist {
            id: "worker".into(),
            instructions: "Read the committed proof".into(),
            allowed_tools: Some(vec!["read_file".into()]),
        }],
    })?;
    let host = ExecutionHost::with_workers(
        store.clone(),
        Arc::new(Model),
        Arc::new(ReadWorkspace::new(&source)?),
        Limits::default(),
        Arc::new(kageha_policy::DefaultPolicy),
        journal,
        workers,
    )?;
    host.submit(&RunSpec {
        id: "parent".into(),
        session: "parent".into(),
        turn: "parent-turn".into(),
        request: "start".into(),
        parent: None,
        parent_run: None,
        agent: "main".into(),
        objective: "delegate".into(),
        depth: 0,
        input: json!({"messages":[{"role":"user","content":"delegate"}],"max_model_calls":5}),
    })?;
    host.supervisor.start()?;
    let until = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let run = store.get("parent")?.unwrap();
        if run.state.terminal() {
            assert_eq!(run.state, RunState::Succeeded);
            break;
        }
        if std::time::Instant::now() >= until {
            return Err("Embedding timed out".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    host.supervisor.shutdown()?;
    host.supervisor.join(Duration::from_secs(5))?;
    println!("Independent Rust specialist embedding passed");
    Ok(())
}
