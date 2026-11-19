//! Explicit opt-in native smoke. Only the owned AppKit fixture is modified.
use kageha_computer::client::ComputerClient;
use serde_json::{json, Value};
use std::{
    fs,
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::Duration,
};

struct Cleanup {
    directory: PathBuf,
    fixture: Option<Child>,
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Some(child) = self.fixture.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.directory);
    }
}
fn required<'a>(value: &'a Value, key: &str) -> Result<&'a str, Box<dyn std::error::Error>> {
    value[key]
        .as_str()
        .ok_or_else(|| format!("missing {key}: {value}").into())
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::DirBuilderExt;
    let binary = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("supply the absolute protocol-2 helper path")?;
    let directory = std::env::temp_dir().join(format!(
        "kageha-control-smoke-{}",
        kageha_computer::instance_nonce()
    ));
    fs::DirBuilder::new().mode(0o700).create(&directory)?;
    let mut cleanup = Cleanup {
        directory: directory.clone(),
        fixture: None,
    };
    let mut client = ComputerClient::launch_with_input(
        &binary,
        &directory.join("actions.sqlite"),
        "native-fixture",
        "rust-smoke",
    )?;
    let permissions = client.call("permissions", json!({}), "permissions")?;
    if permissions["result"]["accessibility"] != true {
        eprintln!(
            "{}",
            json!({"status":"blocked","reason":"Accessibility is not granted to this helper; no prompt or desktop input was attempted","permissions":permissions})
        );
        client.close();
        drop(cleanup);
        std::process::exit(3);
    }
    let fixture = directory.join("NativeControlFixture");
    let compiled = Command::new("xcrun")
        .arg("swiftc")
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/Fixture.swift"))
        .arg("-o")
        .arg(&fixture)
        .status()?;
    if !compiled.success() {
        return Err("fixture compilation failed".into());
    }
    let mut child = Command::new(fixture).stdout(Stdio::piped()).spawn()?;
    let pid = child.id();
    let output = child.stdout.take().ok_or("fixture stdout missing")?;
    cleanup.fixture = Some(child);
    let mut ready = String::new();
    BufReader::new(output).read_line(&mut ready)?;
    if ready.trim().parse::<u32>()? != pid {
        return Err("fixture identity mismatch".into());
    }
    let mut window = None;
    for index in 0..20 {
        let windows = client.call(
            "list_windows",
            json!({"pid":pid}),
            &format!("windows-{index}"),
        )?;
        window = windows["result"]["windows"]
            .as_array()
            .and_then(|items| {
                items
                    .iter()
                    .find(|item| item["is_on_screen"] == true && item["pid"] == pid)
            })
            .map(|item| item["window_id"].clone());
        if window.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let window = window.ok_or("owned fixture window was not published")?;
    let lease = client.call("acquire", json!({"ttl_ms":60000}), "acquire")?;
    let lease = required(&lease, "lease_id")?.to_owned();
    let observe_args =
        json!({"lease_id":lease,"pid":pid,"window_id":window,"include_screenshot":false});
    // Window publication precedes AX readiness during AppKit's opening animation.
    // Re-observe only; never retry an input to work around fixture startup.
    let mut observed = Value::Null;
    for index in 0..20 {
        observed = client.call(
            "observe",
            observe_args.clone(),
            &format!("observe-ready-{index}"),
        )?;
        if observed["result"]["elements"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["role"] == "AXTextField"))
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let token = observed["result"]["elements"]
        .as_array()
        .and_then(|items| items.iter().find(|item| item["role"] == "AXTextField"))
        .and_then(|item| item["element_token"].as_str())
        .ok_or("owned fixture text field unavailable")?;
    let args = json!({"lease_id":lease,"observation_id":required(&observed,"observation_id")?,"action":{"method":"set_value","args":{"element_token":token,"value":"Rust standalone fixture"}}});
    let changed = client.call("execute", args.clone(), "fixture-write")?;
    if changed["receipt"]["state"] != "verified" {
        return Err(
            format!("native outcome was not verified; no retry attempted: {changed}").into(),
        );
    }
    let recovered = client.call("execute", args, "fixture-write")?;
    if recovered != changed {
        return Err("duplicate request did not return original receipt".into());
    }
    let mut observed = Value::Null;
    for index in 0..20 {
        observed = client.call(
            "observe",
            observe_args.clone(),
            &format!("observe-readback-{index}"),
        )?;
        if observed["result"]["elements"]
            .as_array()
            .is_some_and(|items| {
                items.iter().any(|item| {
                    item["role"] == "AXTextField" && item["value"] == "Rust standalone fixture"
                })
            })
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if !observed["result"]["elements"]
        .as_array()
        .is_some_and(|items| {
            items.iter().any(|item| {
                item["role"] == "AXTextField" && item["value"] == "Rust standalone fixture"
            })
        })
    {
        return Err(format!(
            "fresh native AX readback did not match; input was not replayed: {observed}"
        )
        .into());
    }
    let element = observed["result"]["elements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["role"] == "AXTextField" && item["value"] == "Rust standalone fixture")
        .unwrap();
    let selection = client.call("execute", json!({"lease_id":lease,"observation_id":required(&observed,"observation_id")?,"action":{"method":"select_text","args":{"element_token":element["element_token"],"mode":"text","text":"standalone"}}}), "fixture-select-text")?;
    if selection["receipt"]["state"] != "verified" {
        return Err(format!("native selection not verified; no retry: {selection}").into());
    }
    client.call("release", json!({"lease_id":lease}), "release")?;
    println!(
        "{}",
        json!({"status":"ok","scope":"owned disposable native fixture only","receipt":changed["receipt"],"duplicate_receipt_recovered":true,"fresh_ax_value_verified":true,"native_text_selection_verified":true,"screenshots":false})
    );
    client.close();
    Ok(())
}
