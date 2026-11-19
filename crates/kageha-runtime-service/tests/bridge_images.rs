//! Exercise both directions of the real bounded HTTP bridge with an image-heavy checkpoint.
use kageha_runtime::{execution::*, lifecycle::RunSpec, turn::Start};
use kageha_runtime_service::bridge_client::Client;
use serde_json::{json, Value};
use std::{
    fs,
    io::{self, BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};

struct Daemon(Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn image_batch(letter: char) -> Vec<Value> {
    let url = format!(
        "data:image/png;base64,{}",
        letter.to_string().repeat(512 * 1024)
    );
    (0..18)
        .map(|index| {
            json!({"role":"user","content":[
                {"type":"text","text":format!("frame {index}")},
                {"type":"image_url","image_url":{"url":url,"detail":"high"}}
            ]})
        })
        .collect()
}

fn images(messages: &[Value]) -> Vec<&Value> {
    messages
        .iter()
        .filter(|message| message["content"].is_array())
        .collect()
}

// Compare without dumping megabytes of input on failure.
fn check_images(messages: &[Value], expected: &[Value]) {
    let actual = images(messages);
    assert_eq!(actual.len(), expected.len());
    assert!(actual
        .into_iter()
        .zip(expected)
        .all(|(left, right)| left == right));
}

struct Frames {
    initial: Vec<Value>,
    updated: Vec<Value>,
    pending: Mutex<Option<Vec<Value>>>,
    steps: AtomicUsize,
    reads: AtomicUsize,
}

impl ModelPort for Frames {
    fn complete(&self, request: ModelRequest, _: &Control<'_>) -> io::Result<AssistantMessage> {
        let step = self.steps.fetch_add(1, Ordering::SeqCst);
        check_images(
            &request.messages,
            if step == 0 {
                &self.initial
            } else {
                &self.updated
            },
        );
        let call = (step < 2).then(|| ToolCall {
            id: format!("inspection-{step}"),
            name: "inspect".into(),
            arguments: json!({"step":step}),
        });
        Ok(AssistantMessage {
            content: if call.is_some() {
                Value::Null
            } else {
                json!("Frames checked")
            },
            tool_calls: call.into_iter().collect(),
            continuation: None,
        })
    }
}
impl ToolPort for Frames {
    fn discover(&self) -> io::Result<Vec<Value>> {
        Ok(vec![
            json!({"function":{"name":"inspect","parameters":{"type":"object"}}}),
        ])
    }
    fn execute(&self, call: &ToolCall, _: &Control<'_>) -> io::Result<ToolReply> {
        let step = self.reads.fetch_add(1, Ordering::SeqCst);
        assert_eq!(call.arguments["step"], step);
        Ok(ToolReply::success(format!("Checked frame set {step}")))
    }
    fn take_attachments(&self) -> io::Result<Vec<Value>> {
        Ok(self.pending.lock().unwrap().take().unwrap_or_default())
    }
    fn persistence_policy(&self) -> io::Result<Option<PersistencePolicy>> {
        Ok(Some(PersistencePolicy {
            enabled: true,
            max_model_calls: 3,
            max_duration_seconds: 120,
        }))
    }
}

#[test]
fn resumed_images_and_fresh_frames_cross_the_bridge_and_finish_at_the_call_ceiling() {
    let dir = tempfile::tempdir().unwrap();
    let token_file = dir.path().join("token");
    let token = "private-runtime-image-test-token-value";
    fs::write(&token_file, token).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&token_file, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let mut daemon = Daemon(
        Command::new(env!("CARGO_BIN_EXE_kageha-runtime-service"))
            .arg("bridge")
            .arg("--state")
            .arg(dir.path().join("state"))
            .arg("--token-file")
            .arg(token_file)
            .args(["--bind", "127.0.0.1:0"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut line = String::new();
    BufReader::new(daemon.0.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let addr = line
        .trim()
        .strip_prefix("runtime bridge listening on ")
        .unwrap();
    let client = Client::new(&format!("http://{addr}"), token.into()).unwrap();

    let initial = image_batch('A');
    let added = image_batch('B');
    for batch in [&initial, &added] {
        assert!(serde_json::to_vec(batch).unwrap().len() > 8 * 1024 * 1024);
    }
    let frames = Frames {
        updated: [initial.clone(), added.clone()].concat(),
        initial: initial.clone(),
        pending: Mutex::new(Some(added.clone())),
        steps: AtomicUsize::new(0),
        reads: AtomicUsize::new(0),
    };
    let spec = RunSpec {
        id: "images".into(),
        turn: "images".into(),
        session: "test".into(),
        request: "request".into(),
        agent: "agent".into(),
        objective: "Inspect frames".into(),
        parent: None,
        parent_run: None,
        depth: 0,
        input: Value::Null,
    };
    let start = Start {
        version: 1,
        max_model_calls: 2,
        messages: initial,
    };
    let cancel = AtomicBool::new(false);
    let mut checkpoints = 0;
    let mut attachment_batches = 0;
    let mut extensions = 0;
    let result = client
        .execute(
            spec,
            start,
            &frames,
            &frames,
            &Control {
                cancelled: &cancel,
                deadline: Some(Instant::now() + Duration::from_secs(120)),
            },
            |frame| {
                match frame["kind"].as_str() {
                    Some("model") => {
                        check_images(
                            frame["messages"].as_array().unwrap(),
                            if checkpoints == 0 {
                                &frames.initial
                            } else {
                                &frames.updated
                            },
                        );
                        checkpoints += 1;
                    }
                    Some("tool_results") if frame["after"].is_array() => {
                        check_images(frame["after"].as_array().unwrap(), &added);
                        attachment_batches += 1;
                    }
                    Some("continuation") => {
                        assert_eq!(frame["previous_model_calls"], 2);
                        assert_eq!(frame["working_model_calls"], 3);
                        extensions += 1;
                    }
                    _ => {}
                }
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(result, "Frames checked");
    assert_eq!((checkpoints, attachment_batches, extensions), (3, 1, 1));
    assert_eq!(frames.reads.load(Ordering::SeqCst), 2);
}
