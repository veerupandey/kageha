//! Real macOS runtime benchmark. This measures deterministic control, not a VLM.
use kageha_computer::client::ComputerClient;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::unix::{fs::DirBuilderExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
const CASES: &[&str] = &[
    "unicode_roundtrip",
    "same_label_window_isolation",
    "checkbox",
    "radio_selection",
    "disabled_refusal",
    "secure_refusal",
    "stale_snapshot",
    "consumed_snapshot",
    "duplicate_click",
    "reparented_control_refusal",
    "wrong_window_key_refusal",
    "released_lease_refusal",
    "restart_receipt_recovery",
    "lost_reply_recovery",
    "unicode_selection_replace",
    "ambiguous_selection_refusal",
    "persisted_save_workflow",
    "save_validation_failure",
    "delayed_action_acknowledgment",
    "toggle_roundtrip",
    "reacquire_requires_observation",
    "foreign_window_token_refusal",
    "changed_request_id_refusal",
    "old_token_after_restart_refusal",
    "disabled_button_refusal",
    "reparented_button_refusal",
    "focused_key_activation",
    "jpeg_preview",
    "png_preview",
    "moved_window_pixel_refusal",
];
fn requires_capture(name: &str) -> bool {
    ["jpeg_preview", "png_preview", "moved_window_pixel_refusal"].contains(&name)
}
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn ensure(ok: bool, message: &str) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(message.into())
    }
}
fn read_json(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn fingerprint(path: &Path) -> Result<String> {
    let mut digest = Sha256::new();
    let mut file = fs::File::open(path)?;
    let mut bytes = [0u8; 16384];
    loop {
        let n = file.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        digest.update(&bytes[..n]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
fn wait_oracle(path: &Path, test: impl Fn(&Value) -> bool) -> Result<Value> {
    let end = Instant::now() + Duration::from_secs(2);
    let mut last = Value::Null;
    while Instant::now() < end {
        if let Ok(value) = read_json(path) {
            last = value;
            if test(&last) {
                return Ok(last);
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err(format!("independent fixture oracle did not reach expected state: {last}").into())
}
struct Fixture {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    oracle: PathBuf,
}
impl Fixture {
    fn start(executable: &Path, root: &Path, seed: u64) -> Result<Self> {
        let oracle = root.join("oracle.json");
        let mut child = Command::new(executable)
            .arg(&oracle)
            .arg(seed.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        let f = Self {
            child,
            input,
            output,
            oracle,
        };
        wait_oracle(&f.oracle, |v| v["ready"] == true)?;
        Ok(f)
    }
    fn command(&mut self, name: &str) -> Result<()> {
        writeln!(self.input, "{name}")?;
        self.input.flush()?;
        let mut line = String::new();
        self.output.read_line(&mut line)?;
        ensure(line.trim() == name, "fixture setup acknowledgment mismatch")
    }
    fn state(&self) -> Result<Value> {
        read_json(&self.oracle)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
struct Run {
    binary: PathBuf,
    journal: PathBuf,
    client: Option<ComputerClient>,
    lease: String,
    sequence: u64,
    latencies: Vec<f64>,
    calls: u64,
    reply: Value,
    trace: Vec<Value>,
}
impl Run {
    fn new(binary: &Path, root: &Path) -> Result<Self> {
        let journal = root.join("actions.sqlite");
        let client = ComputerClient::launch_with_input(binary, &journal, "benchmark", "runner")?;
        Ok(Self {
            binary: binary.into(),
            journal,
            client: Some(client),
            lease: String::new(),
            sequence: 0,
            latencies: Vec::new(),
            calls: 0,
            reply: Value::Null,
            trace: Vec::new(),
        })
    }
    fn call(&mut self, op: &str, args: Value, id: Option<&str>) -> Result<Value> {
        self.sequence += 1;
        self.calls += 1;
        let key = id
            .map(str::to_owned)
            .unwrap_or_else(|| format!("bench-{}", self.sequence));
        let action_method = args["action"]["method"].clone();
        let at = Instant::now();
        let result = self
            .client
            .as_mut()
            .ok_or("client closed")?
            .call(op, args, &key);
        let elapsed = at.elapsed().as_secs_f64() * 1000.0;
        self.latencies.push(elapsed);
        self.trace.push(match &result { Ok(value) => json!({"operation":op,"action_method":action_method,"request_id":key,"ms":elapsed,"status":value["status"],"receipt":value["receipt"],"error":value["error"],"execution_phase":value["execution_phase"]}), Err(error) => json!({"operation":op,"request_id":key,"ms":elapsed,"status":"transport_error","error":error.to_string()}) });
        let value = result?;
        self.reply = value.clone();
        Ok(value)
    }
    fn acquire(&mut self) -> Result<()> {
        let v = self.call("acquire", json!({"ttl_ms":60000}), None)?;
        self.lease = v["lease_id"]
            .as_str()
            .ok_or_else(|| format!("lease failed: {v}"))?
            .into();
        Ok(())
    }
    fn reset_lease(&mut self) -> Result<()> {
        if !self.lease.is_empty() {
            let _ = self.call("release", json!({"lease_id":self.lease}), None);
        }
        self.acquire()
    }
    fn observe(&mut self, identity: &Value, secondary: bool) -> Result<Value> {
        let window = &identity[if secondary {
            "secondary_window"
        } else {
            "primary_window"
        }];
        let end = Instant::now() + Duration::from_secs(2);
        loop {
            let v=self.call("observe",json!({"lease_id":self.lease,"pid":identity["pid"],"window_id":window,"include_screenshot":false}),None)?;
            if v["result"]["elements"]
                .as_array()
                .is_some_and(|a| a.iter().any(|e| e["label"] == "Primary field"))
            {
                return Ok(v);
            }
            if Instant::now() > end {
                return Err(format!("AX readiness failed: {v}").into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    fn token(observation: &Value, label: &str) -> Result<Value> {
        let rows = observation["result"]["elements"]
            .as_array()
            .ok_or("missing observed elements")?;
        let matches = rows
            .iter()
            .filter(|e| e["label"] == label)
            .collect::<Vec<_>>();
        ensure(
            matches.len() == 1,
            &format!(
                "expected one observed control labeled {label}, got {}",
                matches.len()
            ),
        )?;
        Ok(matches[0]["element_token"].clone())
    }
    fn action_args(&self, obs: &Value, method: &str, args: Value) -> Value {
        json!({"lease_id":self.lease,"observation_id":obs["observation_id"],"action":{"method":method,"args":args}})
    }
    fn act(&mut self, obs: &Value, method: &str, args: Value, id: Option<&str>) -> Result<Value> {
        self.call("execute", self.action_args(obs, method, args), id)
    }
    fn reconnect(&mut self) -> Result<()> {
        if let Some(mut client) = self.client.take() {
            client.close()
        }
        self.client = Some(ComputerClient::launch_with_input(
            &self.binary,
            &self.journal,
            "benchmark",
            "runner",
        )?);
        self.lease.clear();
        self.acquire()
    }
}
fn refused(reply: &Value) -> Result<()> {
    ensure(
        reply["status"] == "error" || reply["status"] == "unknown",
        &format!("unsafe input was acknowledged: {reply}"),
    )?;
    ensure(
        reply["execution_phase"] == "not_dispatched",
        "known preflight refusal was reported as an unknown outcome",
    )
}
fn scenario(name: &str, run: &mut Run, fixture: &mut Fixture, seed: u64) -> Result<Value> {
    fixture.command("reset")?;
    run.reset_lease()?;
    let identity = fixture.state()?;
    let obs = run.observe(&identity, false)?;
    match name {
        "unicode_selection_replace" | "ambiguous_selection_refusal" => {
            let original = if name == "unicode_selection_replace" {
                format!("pre 👩🏽‍💻 needle tail {seed}")
            } else {
                "needle needle".into()
            };
            run.act(
                &obs,
                "set_value",
                json!({"element_token":Run::token(&obs,"Primary field")?,"value":original}),
                None,
            )?;
            wait_oracle(&fixture.oracle, |value| value["primary"] == original)?;
            if run.client.as_ref().unwrap().info()["actions"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item == "focus_element"))
            {
                let window = run.observe(&identity, false)?;
                let window_focus = run.act(&window, "focus_window", json!({}), None)?;
                ensure(
                    window_focus["receipt"]["state"] == "verified",
                    &format!("window focus was not verified: {window_focus}"),
                )?;
                let focused = run.observe(&identity, false)?;
                let element_focus = run.act(
                    &focused,
                    "focus_element",
                    json!({"element_token":Run::token(&focused,"Primary field")?}),
                    None,
                )?;
                ensure(
                    element_focus["receipt"]["state"] == "verified",
                    &format!("element focus was not verified: {element_focus}"),
                )?;
                wait_oracle(&fixture.oracle, |value| value["primary_focused"] == true)?;
            }
            let selected = run.observe(&identity, false)?;
            let reply = run.act(&selected,"select_text",json!({"element_token":Run::token(&selected,"Primary field")?,"mode":"text","text":"needle"}),None)?;
            if name == "ambiguous_selection_refusal" {
                ensure(
                    reply["status"] != "ok",
                    "ambiguous text selection was acknowledged",
                )?;
                ensure(
                    fixture.state()?["primary"] == original,
                    "ambiguous selection changed text",
                )?;
                return fixture.state();
            }
            ensure(
                reply["receipt"]["state"] == "verified",
                &format!("selection was not verified: {reply}"),
            )?;
            let inserted = run.observe(&identity, false)?;
            run.act(
                &inserted,
                "type_text",
                json!({"element_token":Run::token(&inserted,"Primary field")?,"text":"REPLACED"}),
                None,
            )?;
            wait_oracle(&fixture.oracle, |value| {
                value["primary_visible"] == original.replace("needle", "REPLACED")
            })?;
            let commit = run.observe(&identity, false)?;
            run.act(&commit, "press_key", json!({"key":"tab"}), None)?;
            wait_oracle(&fixture.oracle, |value| {
                value["primary"] == original.replace("needle", "REPLACED")
            })
        }
        "delayed_action_acknowledgment" => {
            let args = run.action_args(
                &obs,
                "click",
                json!({"element_token":Run::token(&obs,"Delayed action")?}),
            );
            let id = format!("delayed-{seed}");
            let first = run.call("execute", args.clone(), Some(&id))?;
            wait_oracle(&fixture.oracle, |value| value["delayed_clicks"] == 1)?;
            ensure(
                first["receipt"]["state"] == "dispatched",
                &format!("delayed action was not acknowledged: {first}"),
            )?;
            let duplicate = run.call("execute", args, Some(&id))?;
            ensure(
                duplicate == first,
                "delayed action receipt was not recovered",
            )?;
            std::thread::sleep(Duration::from_millis(300));
            ensure(
                fixture.state()?["delayed_clicks"] == 1,
                "delayed action was dispatched twice",
            )?;
            fixture.state()
        }
        "persisted_save_workflow" | "save_validation_failure" => {
            let saved = fixture.oracle.parent().unwrap().join("saved.json");
            let title = format!("doc-{seed}-東京");
            if name == "persisted_save_workflow" {
                run.act(
                    &obs,
                    "set_value",
                    json!({"element_token":Run::token(&obs,"Primary field")?,"value":title}),
                    None,
                )?;
                wait_oracle(&fixture.oracle, |value| value["primary"] == title)?;
                for label in ["Enable setting", "Option B"] {
                    let fresh = run.observe(&identity, false)?;
                    run.act(
                        &fresh,
                        "click",
                        json!({"element_token":Run::token(&fresh,label)?}),
                        None,
                    )?;
                }
                wait_oracle(&fixture.oracle, |value| {
                    value["checkbox"] == 1 && value["radio_b"] == 1
                })?;
            }
            let fresh = run.observe(&identity, false)?;
            let args = run.action_args(
                &fresh,
                "click",
                json!({"element_token":Run::token(&fresh,"Save document")?}),
            );
            let id = format!("save-{seed}-{name}");
            let first = run.call("execute", args.clone(), Some(&id))?;
            if name == "save_validation_failure" {
                std::thread::sleep(Duration::from_millis(100));
                ensure(
                    !saved.exists() && fixture.state()?["saves"] == 0,
                    "invalid form wrote a document",
                )?;
                ensure(
                    first["receipt"]["state"] == "dispatched",
                    &format!("invalid-save click was not acknowledged as dispatched: {first}"),
                )?;
                return fixture.state();
            }
            wait_oracle(&fixture.oracle, |value| value["saves"] == 1)?;
            ensure(
                read_json(&saved)? == json!({"title":title,"enabled":true,"option":"B"}),
                "saved file does not match the form",
            )?;
            let duplicate = run.call("execute", args, Some(&id))?;
            ensure(duplicate == first, "save receipt was not recovered")?;
            std::thread::sleep(Duration::from_millis(100));
            ensure(
                fixture.state()?["saves"] == 1,
                "duplicate request saved twice",
            )?;
            fixture.state()
        }
        "toggle_roundtrip" => {
            run.act(
                &obs,
                "click",
                json!({"element_token":Run::token(&obs,"Enable setting")?}),
                None,
            )?;
            wait_oracle(&fixture.oracle, |value| value["checkbox"] == 1)?;
            let fresh = run.observe(&identity, false)?;
            run.act(
                &fresh,
                "click",
                json!({"element_token":Run::token(&fresh,"Enable setting")?}),
                None,
            )?;
            wait_oracle(&fixture.oracle, |value| value["checkbox"] == 0)
        }
        "reacquire_requires_observation" => {
            run.reset_lease()?;
            let reply = run.act(
                &obs,
                "click",
                json!({"element_token":Run::token(&obs,"Increment once")?}),
                None,
            )?;
            refused(&reply)?;
            ensure(
                fixture.state()?["primary_clicks"] == 0,
                "old observation survived reacquisition",
            )?;
            fixture.state()
        }
        "foreign_window_token_refusal" => {
            let other = run.observe(&identity, true)?;
            let reply = run.act(
                &other,
                "click",
                json!({"element_token":Run::token(&obs,"Increment once")?}),
                None,
            )?;
            refused(&reply)?;
            let state = fixture.state()?;
            ensure(
                state["primary_clicks"] == 0 && state["secondary_clicks"] == 0,
                "foreign window token received input",
            )?;
            Ok(state)
        }
        "changed_request_id_refusal" => {
            let id = format!("conflict-{seed}");
            run.act(
                &obs,
                "click",
                json!({"element_token":Run::token(&obs,"Increment once")?}),
                Some(&id),
            )?;
            wait_oracle(&fixture.oracle, |value| value["primary_clicks"] == 1)?;
            let fresh = run.observe(&identity, true)?;
            let reply = run.act(
                &fresh,
                "click",
                json!({"element_token":Run::token(&fresh,"Increment once")?}),
                Some(&id),
            )?;
            refused(&reply)?;
            ensure(
                reply["error"]["code"] == "request_id_conflict",
                "conflicting request identity was not rejected",
            )?;
            let state = fixture.state()?;
            ensure(
                state["primary_clicks"] == 1 && state["secondary_clicks"] == 0,
                "request id was retargeted",
            )?;
            Ok(state)
        }
        "old_token_after_restart_refusal" => {
            let old = Run::token(&obs, "Increment once")?;
            run.reconnect()?;
            let fresh = run.observe(&identity, false)?;
            let reply = run.act(&fresh, "click", json!({"element_token":old}), None)?;
            refused(&reply)?;
            ensure(
                fixture.state()?["primary_clicks"] == 0,
                "old native token survived restart",
            )?;
            fixture.state()
        }
        "disabled_button_refusal" => {
            let reply = run.act(
                &obs,
                "click",
                json!({"element_token":Run::token(&obs,"Disabled button")?}),
                None,
            )?;
            refused(&reply)?;
            ensure(
                fixture.state()?["disabled_clicks"] == 0,
                "disabled button callback fired",
            )?;
            fixture.state()
        }
        "reparented_button_refusal" => {
            let token = Run::token(&obs, "Increment once")?;
            fixture.command("reparent_button")?;
            let reply = run.act(&obs, "click", json!({"element_token":token}), None)?;
            refused(&reply)?;
            let state = fixture.state()?;
            ensure(
                state["button_owner"] == state["secondary_window"],
                "button reparent fault was not injected",
            )?;
            ensure(
                state["primary_clicks"] == 0 && state["secondary_clicks"] == 0,
                "moved button received input",
            )?;
            Ok(state)
        }
        "focused_key_activation" => {
            run.act(&obs, "focus_window", json!({}), None)?;
            let fresh = run.observe(&identity, false)?;
            run.act(&fresh, "press_key", json!({"key":"space"}), None)?;
            wait_oracle(&fixture.oracle, |value| {
                value["primary_clicks"] == 1 && value["secondary_clicks"] == 0
            })
        }
        "jpeg_preview" | "png_preview" | "moved_window_pixel_refusal" => {
            let format = if name == "png_preview" { "png" } else { "jpeg" };
            let capture=run.call("observe",json!({"lease_id":run.lease,"pid":identity["pid"],"window_id":identity["primary_window"],"include_screenshot":true,"image_format":format,"max_image_dimension":512}),None)?;
            ensure(
                capture["status"] == "ok",
                &format!("capture failed: {capture}"),
            )?;
            let path = PathBuf::from(
                capture["result"]["screenshot_file_path"]
                    .as_str()
                    .ok_or("capture path missing")?,
            );
            let captures = run
                .journal
                .parent()
                .unwrap()
                .join("actions.sqlite.captures")
                .canonicalize()?;
            ensure(
                path.canonicalize()?.starts_with(&captures),
                "capture escaped private benchmark state",
            )?;
            let bytes = fs::read(&path)?;
            ensure(
                if format == "png" {
                    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
                } else {
                    bytes.starts_with(b"\xff\xd8\xff")
                },
                "image codec signature mismatch",
            )?;
            let output = Command::new(
                fixture
                    .oracle
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join("ImageOracle"),
            )
            .arg(&path)
            .output()?;
            ensure(output.status.success(), "independent image decoder failed")?;
            let decoded: Value = serde_json::from_slice(&output.stdout)?;
            ensure(
                decoded["decoded"] == true && decoded["contrast"].as_i64().unwrap_or(0) > 32,
                "capture was empty or uniform",
            )?;
            ensure(
                decoded["width"].as_f64() == capture["result"]["screenshot_width"].as_f64()
                    && decoded["height"].as_f64()
                        == capture["result"]["screenshot_height"].as_f64(),
                "encoded dimensions differ from metadata",
            )?;
            if name == "moved_window_pixel_refusal" {
                fixture.command("move")?;
                let reply=run.act(&capture,"click",json!({"x":decoded["width"].as_f64().unwrap()/2.0,"y":decoded["height"].as_f64().unwrap()/2.0}),None)?;
                refused(&reply)?;
            }
            let mut state = fixture.state()?;
            state["decoded_image"] = decoded;
            Ok(state)
        }
        "unicode_roundtrip" | "same_label_window_isolation" => {
            let secondary = name == "same_label_window_isolation";
            let obs = if secondary {
                run.observe(&identity, true)?
            } else {
                obs
            };
            let token = Run::token(&obs, "Primary field")?;
            let value = format!("seed {seed} 東京 λ 👩🏽‍💻 e\u{301}");
            run.act(
                &obs,
                "set_value",
                json!({"element_token":token,"value":value}),
                None,
            )?;
            let oracle = wait_oracle(&fixture.oracle, |v| {
                v[if secondary { "secondary" } else { "primary" }] == value
            })?;
            ensure(
                oracle[if secondary { "primary" } else { "secondary" }]
                    == if secondary {
                        "initial-primary"
                    } else {
                        "initial-secondary"
                    },
                "wrong window changed",
            )?;
            Ok(oracle)
        }
        "checkbox" | "radio_selection" => {
            let label = if name == "checkbox" {
                "Enable setting"
            } else {
                "Option B"
            };
            run.act(
                &obs,
                "click",
                json!({"element_token":Run::token(&obs,label)?}),
                None,
            )?;
            wait_oracle(&fixture.oracle, |v| {
                if name == "checkbox" {
                    v["checkbox"] == 1
                } else {
                    v["radio_b"] == 1 && v["radio_a"] == 0
                }
            })
        }
        "disabled_refusal" | "secure_refusal" => {
            let label = if name == "disabled_refusal" {
                "Disabled field"
            } else {
                "[secure control]"
            };
            let reply = run.act(
                &obs,
                "set_value",
                json!({"element_token":Run::token(&obs,label)?,"value":"must-not-write"}),
                None,
            )?;
            std::thread::sleep(Duration::from_millis(100));
            let state = fixture.state()?;
            ensure(
                state[if name == "disabled_refusal" {
                    "disabled"
                } else {
                    "secure"
                }] == if name == "disabled_refusal" {
                    "unchanged-disabled"
                } else {
                    ""
                },
                "forbidden field was modified",
            )?;
            refused(&reply)?;
            Ok(state)
        }
        "stale_snapshot" => {
            let _fresh = run.observe(&identity, false)?;
            let reply = run.act(
                &obs,
                "click",
                json!({"element_token":Run::token(&obs,"Increment once")?}),
                None,
            )?;
            refused(&reply)?;
            ensure(
                reply["execution_phase"] == "not_dispatched",
                "stale input did not report pre-dispatch rejection",
            )?;
            let state = fixture.state()?;
            ensure(
                state["primary_clicks"] == 0,
                "stale snapshot dispatched input",
            )?;
            Ok(state)
        }
        "consumed_snapshot" | "duplicate_click" => {
            let args = run.action_args(
                &obs,
                "click",
                json!({"element_token":Run::token(&obs,"Increment once")?}),
            );
            let id = format!("once-{seed}-{name}");
            let first = run.call("execute", args.clone(), Some(&id))?;
            wait_oracle(&fixture.oracle, |v| v["primary_clicks"] == 1)?;
            let second = run.call(
                "execute",
                args,
                if name == "duplicate_click" {
                    Some(&id)
                } else {
                    None
                },
            )?;
            if name == "duplicate_click" {
                ensure(first == second, "duplicate did not recover exact receipt")?
            } else {
                refused(&second)?
            }
            std::thread::sleep(Duration::from_millis(100));
            let state = fixture.state()?;
            ensure(
                state["primary_clicks"] == 1 && state["secondary_clicks"] == 0,
                "input was duplicated or misdirected",
            )?;
            Ok(state)
        }
        "reparented_control_refusal" => {
            let token = Run::token(&obs, "Primary field")?;
            fixture.command("reparent")?;
            let reply = run.act(
                &obs,
                "set_value",
                json!({"element_token":token,"value":"cross-window-breach"}),
                None,
            )?;
            std::thread::sleep(Duration::from_millis(100));
            let state = fixture.state()?;
            ensure(
                state["primary_owner"] == state["secondary_window"],
                "reparent fault was not injected",
            )?;
            ensure(
                state["primary"] == "initial-primary",
                "observed control moved to another window but still received input",
            )?;
            refused(&reply)?;
            Ok(state)
        }
        "wrong_window_key_refusal" => {
            fixture.command("secondary")?;
            let reply = run.act(&obs, "press_key", json!({"key":"space"}), None)?;
            refused(&reply)?;
            let state = fixture.state()?;
            ensure(
                state["primary_clicks"] == 0 && state["secondary_clicks"] == 0,
                "wrong-window key activated a control",
            )?;
            Ok(state)
        }
        "released_lease_refusal" => {
            let args = run.action_args(
                &obs,
                "click",
                json!({"element_token":Run::token(&obs,"Increment once")?}),
            );
            run.call("release", json!({"lease_id":run.lease}), None)?;
            let reply = run.call("execute", args, None)?;
            refused(&reply)?;
            let state = fixture.state()?;
            ensure(state["primary_clicks"] == 0, "revoked lease dispatched")?;
            Ok(state)
        }
        "restart_receipt_recovery" => {
            let id = format!("restart-{seed}");
            run.act(
                &obs,
                "click",
                json!({"element_token":Run::token(&obs,"Increment once")?}),
                Some(&id),
            )?;
            wait_oracle(&fixture.oracle, |v| v["primary_clicks"] == 1)?;
            run.reconnect()?;
            let receipt = run.call("receipt", json!({"request_id":id}), None)?;
            ensure(
                receipt["receipt"]["request_id"] == id,
                "receipt missing after restart",
            )?;
            let state = fixture.state()?;
            ensure(state["primary_clicks"] == 1, "restart repeated an action")?;
            Ok(state)
        }
        "lost_reply_recovery" => lost_reply(run, fixture, &identity, seed),
        _ => Err("unknown benchmark case".into()),
    }
}
// Fault injection bypasses SDK receive only. Every action still passes through
// the actual standalone authority/native adapter; no OS action is mocked.
fn lost_reply(run: &mut Run, fixture: &Fixture, identity: &Value, seed: u64) -> Result<Value> {
    if let Some(mut client) = run.client.take() {
        client.close()
    }
    let mut child = Command::new(&run.binary)
        .args(["control", "--journal"])
        .arg(&run.journal)
        .arg("--allow-input")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    struct Kill(Child);
    impl Drop for Kill {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut owned = Kill(child);
    let mut n = 0u64;
    let (epoch, lease, obs) = {
        let mut exchange = |operation: &str, args: Value, epoch: &Value| -> Result<Value> {
            n += 1;
            writeln!(
                input,
                "{}",
                json!({"id":n,"method":"control","args":{"protocol":2,"workspace_id":"benchmark","client_id":"runner","request_id":format!("lost-setup-{n}"),"service_epoch":epoch,"operation":operation,"args":args}})
            )?;
            input.flush()?;
            let mut line = String::new();
            output.read_line(&mut line)?;
            ensure(line.len() <= 8 * 1024 * 1024, "oversize fault reply")?;
            let v: Value = serde_json::from_str(&line)?;
            Ok(v["result"].clone())
        };
        let info = exchange("info", json!({}), &Value::Null)?;
        let epoch = info["service_epoch"].clone();
        let lease = exchange("acquire", json!({"ttl_ms":60000}), &epoch)?;
        let obs = exchange(
            "observe",
            json!({"lease_id":lease["lease_id"],"pid":identity["pid"],"window_id":identity["primary_window"],"include_screenshot":false}),
            &epoch,
        )?;
        (epoch, lease, obs)
    };
    let id = format!("lost-native-{seed}");
    n += 1;
    writeln!(
        input,
        "{}",
        json!({"id":n,"method":"control","args":{"protocol":2,"workspace_id":"benchmark","client_id":"runner","request_id":id,"service_epoch":epoch,"operation":"execute","args":{"lease_id":lease["lease_id"],"observation_id":obs["observation_id"],"action":{"method":"click","args":{"element_token":Run::token(&obs,"Increment once")?}}}}})
    )?;
    input.flush()?;
    wait_oracle(&fixture.oracle, |v| v["primary_clicks"] == 1)?;
    // Deliberately never consume the input reply. No repeated execute request.
    let _ = owned.0.kill();
    let _ = owned.0.wait();
    drop(owned);
    drop(input);
    drop(output);
    run.client = Some(ComputerClient::launch_with_input(
        &run.binary,
        &run.journal,
        "benchmark",
        "runner",
    )?);
    run.lease.clear();
    run.acquire()?;
    let receipt = run.call("receipt", json!({"request_id":id}), None)?;
    ensure(
        receipt["receipt"]["request_id"] == id,
        "lost response had no recoverable receipt",
    )?;
    ensure(
        ["unknown", "dispatched", "verified"]
            .contains(&receipt["receipt"]["state"].as_str().unwrap_or("")),
        "invalid recovery receipt",
    )?;
    let state = fixture.state()?;
    ensure(
        state["primary_clicks"] == 1,
        "lost response recovery duplicated input",
    )?;
    Ok(state)
}
fn worker(binary: &Path, fixture_binary: &Path, root: &Path, seed: u64) -> Result<()> {
    let mut fixture = Fixture::start(fixture_binary, root, seed)?;
    let mut run = Run::new(binary, root)?;
    let permissions = run.call("permissions", json!({}), None)?;
    fs::write(
        root.join("permissions.json"),
        serde_json::to_vec_pretty(&permissions)?,
    )?;
    let mut results = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(root.join("results.jsonl"))?;
    for name in CASES {
        let at = Instant::now();
        let start = run.latencies.len();
        let calls = run.calls;
        run.reply = Value::Null;
        let blocked = if permissions["result"]["accessibility"] != true {
            Some("Accessibility unavailable; no native input attempted")
        } else if requires_capture(name) && permissions["result"]["screen_recording"] != true {
            Some("Screen Recording unavailable; no capture or pixel input attempted")
        } else {
            None
        };
        let result = if let Some(reason) = blocked {
            Err(reason.into())
        } else {
            scenario(name, &mut run, &mut fixture, seed)
        };
        let oracle = fixture.state().unwrap_or(Value::Null);
        let row = match result {
            Ok(state) => json!({"case":name,"seed":seed,"status":"passed","oracle":state}),
            Err(error) => {
                json!({"case":name,"seed":seed,"status":if blocked.is_some(){"blocked"}else{"failed"},"error":error.to_string(),"oracle":oracle})
            }
        };
        let mut row = row;
        row["task_ms"] = json!(at.elapsed().as_secs_f64() * 1000.0);
        row["tool_calls"] = json!(run.calls - calls);
        row["call_ms"] = json!(&run.latencies[start..]);
        row["last_reply"] = run.reply.clone();
        row["trace"] = json!(&run.trace[start..]);
        row["false_confirmation"] =
            json!(row["status"] == "failed" && run.reply["receipt"]["state"] == "verified");
        row["false_unknown"] = json!(row["error"]
            .as_str()
            .is_some_and(|message| message.contains("known preflight refusal")));
        writeln!(results, "{row}")?;
        results.flush()?;
        results.sync_all()?;
    }
    if let Some(mut client) = run.client.take() {
        client.close()
    }
    Ok(())
}
fn percentile(samples: &[f64], p: f64) -> Option<f64> {
    if samples.is_empty() {
        return None;
    }
    let mut values = samples.to_vec();
    values.sort_by(f64::total_cmp);
    Some(
        values[((values.len() as f64 * p).ceil() as usize)
            .saturating_sub(1)
            .min(values.len() - 1)],
    )
}
fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.first().map(String::as_str) == Some("--worker") {
        return worker(
            Path::new(&args[1]),
            Path::new(&args[2]),
            Path::new(&args[3]),
            args[4].parse()?,
        );
    }
    ensure(
        args.len() >= 2,
        "usage: runtime_bench ABSOLUTE_HELPER NEW_REPORT_DIRECTORY [SEED,SEED,...]",
    )?;
    let binary = PathBuf::from(&args[0]).canonicalize()?;
    let root = PathBuf::from(&args[1]);
    ensure(
        !root.exists(),
        "report directory must be new; never overwrite a baseline",
    )?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&root)?;
    let root = root.canonicalize()?;
    let seeds = args
        .get(2)
        .map(String::as_str)
        .unwrap_or("101,307,509,701,907")
        .split(',')
        .map(str::parse::<u64>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    ensure(!seeds.is_empty() && seeds.len() <= 100, "use 1..100 seeds")?;
    ensure(
        seeds.iter().collect::<std::collections::HashSet<_>>().len() == seeds.len(),
        "seeds must be unique",
    )?;
    let fixture_source =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bench/BenchmarkFixture.swift");
    // A linker-only ad-hoc signature is not a reliable AppKit AX identity on
    // current macOS. Give our owned fixture a real app bundle and sign it.
    // Never re-sign an existing user application or weaken exact-window checks.
    let fixture_app = root.join("BenchmarkFixture.app");
    let contents = fixture_app.join("Contents");
    fs::create_dir_all(contents.join("MacOS"))?;
    fs::write(contents.join("Info.plist"), format!(
        "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict><key>CFBundleIdentifier</key><string>test.kageha.native.{}</string><key>CFBundleName</key><string>Kageha Benchmark</string><key>CFBundleExecutable</key><string>BenchmarkFixture</string><key>CFBundlePackageType</key><string>APPL</string></dict></plist>",
        kageha_computer::instance_nonce()))?;
    let fixture_binary = contents.join("MacOS/BenchmarkFixture");
    let status = Command::new("xcrun")
        .args(["swiftc", "-O"])
        .arg(&fixture_source)
        .arg("-o")
        .arg(&fixture_binary)
        .status()?;
    ensure(status.success(), "fixture compilation failed")?;
    ensure(
        Command::new("/usr/bin/codesign")
            .args(["--force", "--sign", "-"])
            .arg(&fixture_app)
            .status()?
            .success(),
        "fixture signing failed",
    )?;
    ensure(
        Command::new("/usr/bin/codesign")
            .args(["--verify", "--strict"])
            .arg(&fixture_app)
            .status()?
            .success(),
        "fixture signature verification failed",
    )?;
    let image_oracle = Command::new("xcrun")
        .args(["swiftc", "-O"])
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bench/ImageOracle.swift"))
        .arg("-o")
        .arg(root.join("ImageOracle"))
        .status()?;
    ensure(image_oracle.success(), "image evaluator compilation failed")?;
    let helper_sha256_start = fingerprint(&binary)?;
    let mut rows = Vec::new();
    let mut worker_failures = Vec::new();
    for seed in &seeds {
        let trial = root.join(format!("seed-{seed}"));
        fs::DirBuilder::new().mode(0o700).create(&trial)?;
        let log = fs::File::create(trial.join("worker.log"))?;
        let mut child = Command::new(std::env::current_exe()?)
            .arg("--worker")
            .arg(&binary)
            .arg(&fixture_binary)
            .arg(&trial)
            .arg(seed.to_string())
            .process_group(0)
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            .spawn()?;
        let end = Instant::now() + Duration::from_secs(60);
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break Some(status);
            }
            if Instant::now() > end {
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                let _ = child.wait();
                break None;
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        if !status.is_some_and(|s| s.success()) {
            worker_failures.push(json!({"seed":seed,"error":if status.is_none(){"worker_timeout"}else{"worker_failed"}}));
        }
        // Normal workers close their owned fixture/helper. Only kill a live owned
        // process group in the timeout branch, before reaping its leader.
        let text = fs::read_to_string(trial.join("results.jsonl")).unwrap_or_default();
        let mut completed = std::collections::HashSet::new();
        for line in text.lines() {
            if let Ok(value) = serde_json::from_str::<Value>(line) {
                if let Some(name) = value["case"].as_str() {
                    completed.insert(name.to_owned());
                }
                rows.push(value)
            }
        }
        for name in CASES {
            if !completed.contains(*name) {
                rows.push(json!({"case":name,"seed":seed,"status":"blocked","error":"worker did not complete this case"}));
            }
        }
    }
    let passed = rows.iter().filter(|v| v["status"] == "passed").count();
    let failed = rows.iter().filter(|v| v["status"] == "failed").count();
    let blocked = rows.len() - passed - failed;
    let false_confirmations = rows
        .iter()
        .filter(|v| v["false_confirmation"] == true)
        .count();
    let false_unknowns = rows.iter().filter(|v| v["false_unknown"] == true).count();
    let latency = rows
        .iter()
        .flat_map(|v| {
            v["call_ms"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_f64)
        })
        .collect::<Vec<_>>();
    let mut per_case = serde_json::Map::new();
    for name in CASES {
        let subset = rows
            .iter()
            .filter(|v| v["case"] == *name)
            .collect::<Vec<_>>();
        per_case.insert((*name).into(),json!({"passed":subset.iter().filter(|v|v["status"]=="passed").count(),"failed":subset.iter().filter(|v|v["status"]=="failed").count(),"blocked":subset.iter().filter(|v|v["status"]=="blocked").count()}));
    }
    let mut operation_samples: std::collections::BTreeMap<String, Vec<f64>> =
        std::collections::BTreeMap::new();
    for row in &rows {
        for trace in row["trace"].as_array().into_iter().flatten() {
            if let Some(ms) = trace["ms"].as_f64() {
                let operation = trace["operation"].as_str().unwrap_or("unknown");
                let key = if operation == "execute" {
                    format!(
                        "execute:{}",
                        trace["action_method"].as_str().unwrap_or("unknown")
                    )
                } else {
                    operation.into()
                };
                operation_samples.entry(key).or_default().push(ms);
            }
        }
    }
    let operation_latency=operation_samples.iter().map(|(name,values)|(name.clone(),json!({"p50":percentile(values,0.5),"p95":percentile(values,0.95),"samples":values.len()}))).collect::<serde_json::Map<_,_>>();
    let permission_records=seeds.iter().map(|seed|json!({"seed":seed,"result":read_json(&root.join(format!("seed-{seed}/permissions.json"))).unwrap_or(Value::Null)})).collect::<Vec<_>>();
    let helper_sha256_end = fingerprint(&binary)?;
    let artifact_changed = helper_sha256_start != helper_sha256_end;
    let worker_failed = !worker_failures.is_empty();
    let report = json!({"benchmark":"kageha-native-runtime-v3","scope":"scripted runtime reliability; not an agent-model or public benchmark score","timestamp_unix_ms":SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),"seeds":seeds,"permissions_by_seed":permission_records,"operation_latency_ms":operation_latency,"image_evaluator_sha256":fingerprint(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bench/ImageOracle.swift"))?,"helper_sha256":helper_sha256_end,"helper_sha256_start":helper_sha256_start,"artifact_changed_during_run":artifact_changed,"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"macos_version":Command::new("/usr/bin/sw_vers").arg("-productVersion").output().ok().filter(|v|v.status.success()).map(|v|String::from_utf8_lossy(&v.stdout).trim().to_owned()),"runner_sha256":fingerprint(&std::env::current_exe()?)?,"fixture_sha256":fingerprint(&fixture_source)?,"passed":passed,"failed":failed,"blocked":blocked,"total":rows.len(),"success_rate_all_attempts":passed as f64/rows.len() as f64,"false_confirmations":false_confirmations,"false_unknowns":false_unknowns,"call_latency_ms":{"p50":percentile(&latency,0.5),"p95":percentile(&latency,0.95),"samples":latency.len()},"worker_failures":worker_failures,"per_case":per_case,"results":rows});
    fs::write(
        root.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!(
        "{}",
        json!({"report":root.join("report.json"),"passed":passed,"failed":failed,"blocked":blocked,"false_confirmations":false_confirmations,"false_unknowns":false_unknowns,"latency":report["call_latency_ms"]})
    );
    if failed > 0 || blocked > 0 || false_confirmations > 0 || artifact_changed || worker_failed {
        std::process::exit(1)
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn latency_percentiles_use_nearest_rank_and_do_not_invent_empty_samples() {
        assert_eq!(percentile(&[], 0.95), None);
        assert_eq!(percentile(&[4.0, 1.0, 3.0, 2.0], 0.5), Some(2.0));
        assert_eq!(percentile(&[4.0, 1.0, 3.0, 2.0], 0.95), Some(4.0));
    }
    #[test]
    fn safe_refusal_gate_rejects_unknown_or_success_claims() {
        assert!(refused(&json!({"status":"error","execution_phase":"not_dispatched"})).is_ok());
        assert!(
            refused(&json!({"status":"unknown","execution_phase":"dispatch_started"})).is_err()
        );
        assert!(refused(&json!({"status":"ok","execution_phase":"not_dispatched"})).is_err());
    }
}
