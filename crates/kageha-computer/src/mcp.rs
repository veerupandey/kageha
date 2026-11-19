//! Minimal MCP stdio transport over the same serialized computer authority.
//!
//! Observation screenshots keep the canonical reply and append validated native
//! image content. Stdio is an owned, trusted process boundary, not multi-tenant
//! authentication; native capture paths are resolved by the controller.
use crate::{control::Controller, DesktopBackend};
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};

const MAX_FRAME: usize = 8 * 1024 * 1024;
const VERSION: &str = "2024-11-05";
const OPERATIONS: &[&str] = &[
    "info",
    "permissions",
    "list_apps",
    "list_windows",
    "observe",
    "inspect",
    "activity",
    "preview",
    "acquire",
    "renew",
    "release",
    "execute",
    "receipt",
];

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
}
fn only_fields(value: &Value, fields: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|object| object.keys().all(|key| fields.contains(&key.as_str())))
}
fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
fn result(id: Value, value: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":value})
}
fn tool_result(reply: Value) -> Value {
    json!({"content":[{"type":"text","text":reply.to_string()}],"isError":reply["status"] != "ok"})
}
fn catalog() -> Value {
    json!({"tools":[
        {"name":"computer_info","description":"Discover protocol, pinned service epoch and native computer capabilities. Observation screenshots include validated native JPEG/PNG image content.","inputSchema":{"type":"object","properties":{},"additionalProperties":false}},
        {"name":"computer_call","description":"Call native computer protocol 2. Acquire, observe the exact window, then execute with lease and observation. Never replay uncertain input; use receipt lookup. Scope is fixed by the host process.","inputSchema":{
            "type":"object","properties":{
                "request_id":{"type":"string","minLength":1,"maxLength":128,"pattern":"^[A-Za-z0-9._:-]+$"},
                "service_epoch":{"type":"string","minLength":1,"maxLength":128},
                "operation":{"type":"string","enum":OPERATIONS},
                "args":{"type":"object"}
            },"required":["request_id","service_epoch","operation","args"],"additionalProperties":false
        }}
    ]})
}

/// Serve newline-delimited MCP JSON-RPC. One launch has one fixed trusted scope.
/// Input and output frames are bounded. Calls run once, serially; closing stdin
/// cannot cause an already entered OS operation to be replayed or rolled back.
pub fn serve<B: DesktopBackend, R: BufRead, W: Write>(
    controller: &mut Controller<B>,
    workspace: &str,
    client: &str,
    mut reader: R,
    mut writer: W,
) -> io::Result<()> {
    if !identifier(workspace) || !identifier(client) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid MCP scope",
        ));
    }
    let epoch = controller.epoch().to_owned();
    let mut initialized = false;
    let mut ready = false;
    loop {
        // read_line/read_until without a cap can allocate arbitrarily before a
        // newline arrives. Inspect bounded buffer slices before copying them.
        let mut frame = Vec::new();
        loop {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                if frame.is_empty() {
                    return Ok(());
                }
                break;
            }
            let count = available
                .iter()
                .position(|byte| *byte == b'\n')
                .map(|index| index + 1)
                .unwrap_or(available.len());
            if frame.len().saturating_add(count) > MAX_FRAME {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "MCP frame exceeds 8 MiB",
                ));
            }
            let newline = available[count - 1] == b'\n';
            frame.extend_from_slice(&available[..count]);
            reader.consume(count);
            if newline {
                break;
            }
        }
        let response = match serde_json::from_slice::<Value>(&frame) {
            Err(_) => Some(rpc_error(Value::Null, -32700, "invalid JSON")),
            Ok(request) => handle(
                controller,
                workspace,
                client,
                &epoch,
                &mut initialized,
                &mut ready,
                request,
            ),
        };
        if let Some(response) = response {
            let mut bytes = serde_json::to_vec(&response).map_err(io::Error::other)?;
            if bytes.len() + 1 > MAX_FRAME {
                // The action may have run. Never report it as not-dispatched or
                // repeat it to construct a smaller reply.
                bytes = rpc_error(response["id"].clone(), -32000,
                    "Response exceeds 8 MiB. Execution may have occurred; inspect the original request receipt before further input.")
                    .to_string().into_bytes();
            }
            bytes.push(b'\n');
            writer.write_all(&bytes)?;
            writer.flush()?;
        }
    }
}

fn handle<B: DesktopBackend>(
    controller: &mut Controller<B>,
    workspace: &str,
    client: &str,
    epoch: &str,
    initialized: &mut bool,
    ready: &mut bool,
    request: Value,
) -> Option<Value> {
    let id = request.get("id").cloned();
    let valid_id = id
        .as_ref()
        .is_none_or(|id| id.is_string() || id.is_i64() || id.is_u64());
    if !only_fields(&request, &["jsonrpc", "id", "method", "params"])
        || request["jsonrpc"] != "2.0"
        || !request["method"].is_string()
        || !valid_id
    {
        return Some(rpc_error(Value::Null, -32600, "invalid JSON-RPC request"));
    }
    let method = request["method"].as_str().unwrap();
    if id.is_none() {
        if method == "notifications/initialized" && *initialized {
            *ready = true;
        }
        return None;
    }
    let id = id.unwrap();
    if method == "initialize" {
        if *initialized {
            return Some(rpc_error(id, -32600, "already initialized"));
        }
        if !request["params"].is_object()
            || !request["params"]["protocolVersion"].is_string()
            || !request["params"]["capabilities"].is_object()
            || !request["params"]["clientInfo"]["name"].is_string()
            || !request["params"]["clientInfo"]["version"].is_string()
        {
            return Some(rpc_error(id, -32602, "invalid initialize parameters"));
        }
        *initialized = true;
        return Some(result(
            id,
            json!({"protocolVersion":VERSION,"capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"kageha-computer","version":env!("CARGO_PKG_VERSION")},"instructions":"Fixed host scope; no automatic input replay. Screenshot paths are validated by the controller before emitting image content."}),
        ));
    }
    if !*ready {
        return Some(rpc_error(
            id,
            -32002,
            "initialize and send notifications/initialized first",
        ));
    }
    match method {
        "ping" => Some(result(id, json!({}))),
        "tools/list" => {
            if request
                .get("params")
                .is_some_and(|params| !only_fields(params, &[]))
            {
                return Some(rpc_error(id, -32602, "invalid tools/list parameters"));
            }
            Some(result(id, catalog()))
        }
        "tools/call" => {
            let params = &request["params"];
            if !only_fields(params, &["name", "arguments", "_meta"]) {
                return Some(rpc_error(id, -32602, "invalid tool call"));
            }
            let arguments = &params["arguments"];
            let (request_id, call_epoch, operation, args) = match params["name"].as_str() {
                Some("computer_info")
                    if params.get("arguments").is_none() || only_fields(arguments, &[]) =>
                {
                    ("mcp-info", epoch, "info", json!({}))
                }
                Some("computer_call")
                    if only_fields(
                        arguments,
                        &["request_id", "service_epoch", "operation", "args"],
                    ) =>
                {
                    let request_id = arguments["request_id"]
                        .as_str()
                        .filter(|value| identifier(value));
                    let call_epoch = arguments["service_epoch"]
                        .as_str()
                        .filter(|value| identifier(value));
                    let operation = arguments["operation"]
                        .as_str()
                        .filter(|value| OPERATIONS.contains(value));
                    match (
                        request_id,
                        call_epoch,
                        operation,
                        arguments["args"].is_object(),
                    ) {
                        (Some(request_id), Some(call_epoch), Some(operation), true) => {
                            (request_id, call_epoch, operation, arguments["args"].clone())
                        }
                        _ => return Some(rpc_error(id, -32602, "invalid computer_call arguments")),
                    }
                }
                _ => return Some(rpc_error(id, -32602, "unknown tool or invalid arguments")),
            };
            let reply = controller.handle(json!({"protocol":2,"workspace_id":workspace,"client_id":client,
                "request_id":request_id,"service_epoch":call_epoch,"operation":operation,"args":args}));
            let image = if operation == "observe" || operation == "inspect" {
                controller.observation_image(&reply)
            } else {
                Ok(None)
            };
            let mut value = tool_result(reply);
            match image {
                Ok(Some((mime_type, data))) => value["content"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"type":"image","mimeType":mime_type,"data":data})),
                Ok(None) => {}
                Err(message) => {
                    value["isError"] = json!(true);
                    value["content"].as_array_mut().unwrap().push(
                        json!({"type":"text","text":format!("Screenshot unavailable: {message}")}),
                    );
                }
            }
            Some(result(id, value))
        }
        _ => Some(rpc_error(id, -32601, "method not found")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, io::Cursor, path::PathBuf};
    struct Fake;
    impl DesktopBackend for Fake {
        fn call(&mut self, method: &str, _args: Value) -> Result<Value, String> {
            assert_eq!(method, "check_permissions");
            Ok(json!({"accessibility":false,"screen_recording":false}))
        }
    }
    struct TestDirectory(PathBuf);
    impl std::ops::Deref for TestDirectory {
        type Target = std::path::Path;
        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }
    impl AsRef<std::path::Path> for TestDirectory {
        fn as_ref(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn controller() -> (Controller<Fake>, TestDirectory) {
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!(
            "computer-mcp-{}-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        (
            Controller::open(Fake, &path.join("journal.sqlite")).unwrap(),
            TestDirectory(path),
        )
    }
    fn run<B: DesktopBackend>(controller: &mut Controller<B>, requests: Vec<Value>) -> Vec<Value> {
        let input = requests
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let mut output = Vec::new();
        serve(
            controller,
            "workspace",
            "agent",
            Cursor::new(input),
            &mut output,
        )
        .unwrap();
        String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    fn initialize() -> Value {
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2026-01-01","capabilities":{},"clientInfo":{"name":"test","version":"1"}}})
    }
    fn ready() -> Value {
        json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    }
    #[test]
    fn lifecycle_scope_and_native_errors() {
        let (mut controller, path) = controller();
        let replies = run(
            &mut controller,
            vec![
                json!({"jsonrpc":"2.0","id":0,"method":"tools/list"}),
                initialize(),
                ready(),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
                json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"computer_info","arguments":{}}}),
                json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"computer_call","arguments":{"request_id":"permissions","service_epoch":"old","operation":"permissions","args":{}}}}),
                json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"computer_call","arguments":{"workspace_id":"spoof","request_id":"x","service_epoch":"old","operation":"info","args":{}}}}),
                json!({"jsonrpc":"2.0","id":6,"method":"host_takeover"}),
                json!({"jsonrpc":"2.0","id":true,"method":"tools/list"}),
            ],
        );
        assert_eq!(replies[0]["error"]["code"], -32002);
        assert_eq!(replies[1]["result"]["protocolVersion"], VERSION);
        assert_eq!(replies[2]["result"]["tools"].as_array().unwrap().len(), 2);
        let info: Value =
            serde_json::from_str(replies[3]["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        assert_eq!(info["service_epoch"], controller.epoch());
        assert_eq!(info["automatic_input_retry"], false);
        assert_eq!(replies[4]["result"]["isError"], true);
        let stale: Value =
            serde_json::from_str(replies[4]["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        assert_eq!(stale["error"]["code"], "service_epoch_stale");
        assert_eq!(stale["execution_phase"], "not_dispatched");
        assert_eq!(replies[5]["error"]["code"], -32602);
        assert_eq!(replies[6]["error"]["code"], -32601);
        assert_eq!(replies[7]["error"]["code"], -32600);
        drop(controller);
        fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn fixed_scope_real_query_and_unknown_receipt_preservation() {
        let (mut controller, path) = controller();
        let epoch = controller.epoch().to_owned();
        let replies = run(
            &mut controller,
            vec![
                initialize(),
                ready(),
                json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"computer_call","arguments":{"request_id":"query","service_epoch":epoch,"operation":"permissions","args":{}}}}),
            ],
        );
        assert_eq!(replies[1]["result"]["isError"], false);
        let unknown = json!({"status":"unknown","execution_phase":"dispatch_started","receipt":{"state":"unknown","request_id":"original"},"retryable":false});
        let wrapped = tool_result(unknown.clone());
        assert_eq!(wrapped["isError"], true);
        assert_eq!(
            serde_json::from_str::<Value>(wrapped["content"][0]["text"].as_str().unwrap()).unwrap(),
            unknown
        );
        drop(controller);
        fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn bounded_frames_and_invalid_scope() {
        let (mut controller, path) = controller();
        let mut output = Vec::new();
        let error = serve(
            &mut controller,
            "w",
            "c",
            Cursor::new(vec![b'x'; MAX_FRAME + 1]),
            &mut output,
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(output.is_empty());
        assert_eq!(
            serve(
                &mut controller,
                "bad scope",
                "c",
                Cursor::new(Vec::<u8>::new()),
                Vec::<u8>::new()
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::InvalidInput
        );
        drop(controller);
        fs::remove_dir_all(path).unwrap();
    }
    struct ImageBackend {
        misbind: bool,
        missing: bool,
    }
    impl DesktopBackend for ImageBackend {
        fn call(&mut self, method: &str, args: Value) -> Result<Value, String> {
            use base64::Engine;
            use std::os::unix::fs::OpenOptionsExt;
            assert_eq!(method, "get_window_state");
            let destination = args["screenshot_out_file"].as_str().unwrap();
            // Actual tiny image fixtures, rather than reading any user's pixels.
            let data = if args["image_format"] == "png" {
                "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII="
            } else {
                "/9j/4AAQSkZJRgABAQEASABIAAD/2wBDAP//////////////////////////////////////////////////////////////////////////////////////2wBDAf//////////////////////////////////////////////////////////////////////////////////////wAARCAABAAEDASIAAhEBAxEB/8QAFQABAQAAAAAAAAAAAAAAAAAAAAX/xAAUEAEAAAAAAAAAAAAAAAAAAAAA/8QAFQEBAQAAAAAAAAAAAAAAAAAAAgP/xAAUEQEAAAAAAAAAAAAAAAAAAAAA/9oADAMBAAIRAxEAPwCdABmX/9k="
            };
            if !self.missing {
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(destination)
                    .map_err(|error| error.to_string())?;
                file.write_all(
                    &base64::engine::general_purpose::STANDARD
                        .decode(data)
                        .unwrap(),
                )
                .map_err(|error| error.to_string())?;
            }
            Ok(
                json!({"ok":true,"pid":args["pid"],"window_id":args["window_id"],"elements":[],
                "screenshot_file_path":if self.misbind {"/untrusted/path.png"}else{destination}}),
            )
        }
    }
    #[test]
    fn observation_images_keep_native_reply_and_reject_path_misbinding() {
        use base64::Engine;
        for (format, misbind, missing) in [
            ("png", false, false),
            ("jpeg", false, false),
            ("png", true, false),
            ("png", false, true),
        ] {
            let (temporary, path) = controller();
            drop(temporary);
            let mut controller = Controller::open(
                ImageBackend { misbind, missing },
                &path.join("journal.sqlite"),
            )
            .unwrap();
            let epoch = controller.epoch().to_owned();
            let lease=controller.handle(json!({"protocol":2,"workspace_id":"workspace","client_id":"agent","request_id":"acquire","service_epoch":epoch,"operation":"acquire","args":{"ttl_ms":60000}}));
            assert_eq!(lease["status"], "ok");
            let replies = run(
                &mut controller,
                vec![
                    initialize(),
                    ready(),
                    json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"computer_call","arguments":{"request_id":"observe","service_epoch":epoch,"operation":"observe","args":{"lease_id":lease["lease_id"],"pid":1,"window_id":2,"include_screenshot":true,"image_format":format}}}}),
                ],
            );
            let output = &replies[1]["result"];
            assert_eq!(output["content"][0]["type"], "text");
            let canonical: Value =
                serde_json::from_str(output["content"][0]["text"].as_str().unwrap()).unwrap();
            if misbind {
                assert_eq!(canonical["error"]["code"], "capture_binding_failed");
                assert_eq!(output["content"].as_array().unwrap().len(), 1);
                assert_eq!(output["isError"], true);
            } else if missing {
                assert_eq!(canonical["status"], "ok");
                assert_eq!(output["isError"], true);
                assert_eq!(output["content"][1]["type"], "text");
                assert!(output["content"][1]["text"]
                    .as_str()
                    .unwrap()
                    .starts_with("Screenshot unavailable:"));
            } else {
                assert_eq!(canonical["status"], "ok");
                assert_eq!(canonical["result"]["pid"], 1);
                assert_eq!(output["isError"], false);
                assert_eq!(output["content"][1]["type"], "image");
                assert_eq!(output["content"][1]["mimeType"], format!("image/{format}"));
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(output["content"][1]["data"].as_str().unwrap())
                    .unwrap();
                assert!(if format == "png" {
                    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
                } else {
                    bytes.starts_with(b"\xff\xd8\xff")
                });
            }
            drop(controller);
            fs::remove_dir_all(path).unwrap();
        }
    }
}
