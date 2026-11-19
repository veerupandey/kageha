//! Framework-neutral MCP stdio transport. Browser policy and receipts stay in Service.
use crate::{protocol, Service};
use data_encoding::BASE64;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{io, sync::Arc};
use tokio::{
    io::{AsyncWriteExt, BufReader},
    sync::{Mutex, Semaphore},
    task::JoinSet,
};

const VERSION: &str = "2024-11-05";
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Call {
    request_id: String,
    service_epoch: String,
    method: String,
    params: Value,
    authority: Option<protocol::Authority>,
    timeout_ms: Option<u64>,
}
fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
fn result(id: Value, value: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":value})
}
fn request(
    service: &Service,
    workspace: &str,
    client: &str,
    method: &str,
    params: Value,
) -> protocol::Request {
    protocol::Request {
        protocol_version: protocol::version(),
        request_id: uuid::Uuid::new_v4().to_string(),
        instance_id: service.state.instance_id.clone(),
        application_id: service.state.application_id.clone(),
        workspace_id: workspace.to_owned(),
        client_id: client.to_owned(),
        state_root: service.state.root.to_string_lossy().into_owned(),
        service_epoch: Some(service.state.epoch.clone()),
        deadline_ms: protocol::now_ms() + protocol::limit("deadline_ms") as u64,
        expected_binding: None,
        authority: None,
        method: method.to_owned(),
        params,
    }
}
async fn info(service: &Service, workspace: &str, client: &str) -> Value {
    let mut req = request(
        service,
        workspace,
        client,
        "handshake",
        json!({"token":service.state.token}),
    );
    req.service_epoch = None;
    let (reply, _) = service.handle(&req, &mut None).await;
    if reply.status != "ok" {
        return serde_json::to_value(reply).unwrap();
    }
    let methods = reply.result["methods"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| !matches!(m.as_str(), Some("handshake" | "shutdown")))
        .cloned()
        .collect::<Vec<_>>();
    let schema: Value = serde_json::from_str(protocol::SCHEMA).unwrap();
    let schemas = schema["$defs"].as_object().unwrap().iter()
        .filter(|(name, _)| matches!(name.as_str(), "RunScriptParams" | "BatchObservedParams" | "BatchAction" | "BrowserDialog" | "FillObservedParams" | "PressObservedParams" | "NavigateTabParams" | "ClaimControlParams"))
        .map(|(name, value)| (name.clone(), value.clone())).collect::<serde_json::Map<_, _>>();
    json!({"protocol_version":protocol::version(),"service_epoch":service.state.epoch,
        "workspace_id":workspace,"client_id":client,"methods":methods,"limits":reply.result["limits"],
        "action_backend":reply.result.get("action_backend"),
        "parameter_schemas":schemas,
        "startup":{"method":"launch_browser","params":{},"authority":null},
        "method_parameter_examples":{
            "browser_selection":{},
            "browser_adapters":{},
            "launch_browser":{},
            "switch_profile":{"profile":"comet","binding_id":"CURRENT browser_selection.binding_id","consent":true,"endpoint":"http://127.0.0.1:9222"},
            "list_user_tabs":{},
            "claim_user_tab":{"binding_id":"CURRENT binding_id","browser_id":"CURRENT browser_id","target_id":"EXACT listed tab id","url":"EXACT listed URL","title":"EXACT listed title"},
            "open_tab":{"browser_id":"CURRENT browser_id","url":"https://example.com/"},
            "close_tab":{"binding_id":"CURRENT binding_id","browser_id":"CURRENT browser_id","target_id":"OWNED tab id"},
            "select_tab":{"binding_id":"CURRENT binding_id","browser_id":"CURRENT browser_id","target_id":"OWNED tab id"},
            "list_tabs":{"browser_id":"CURRENT browser_id"},
            "observe_tab":{"browser_id":"CURRENT browser_id","target_id":"OWNED tab id","compact":false},
            "claim_control":{"ttl_ms":30000},
            "release_control":{},
            "panel_action":{"selection":{"binding_id":"CURRENT binding_id","browser_id":"CURRENT browser_id","target_id":"OWNED tab id"},"viewport":{"width":1280,"height":960},"input":{"action":"focus"}},
            "query_tab":{"selection":{"binding_id":"CURRENT binding_id","browser_id":"CURRENT browser_id","target_id":"OWNED tab id"},"selector":"body"}
        },
        "profile_guidance":"launch_browser is ONLY for creating a new private browser. To attach an existing Chrome or Comet, read browser_selection for binding_id then switch_profile using the exact parameter names shown, with the operator-authorized endpoint and consent. Do not substitute a private browser when a personal profile was requested. browser_adapters.hosted_profiles lists optional remote adapters, NOT supported local Chrome/Comet profiles. Refresh browser_selection after profile/tab changes. For panel_action copy the ACTUAL viewport returned by browser_selection, never the example dimensions. Parameter examples are API shapes, not a task sequence.",
        "authority_methods":["close_tab","click_observed","navigate_tab","fill_observed","press_observed","select_observed","check_observed","batch_observed","reconcile_click","reconcile_input","reconcile_upload","run_script","panel_action","release_control"],
        "usage":"Authority is ONLY for the listed authority_methods. Omit authority or pass null for launch_browser, open_tab, switch_profile, browser_selection and observations. A grant on another method is rejected before execution. launch_browser with empty params creates the private browser. Then open_tab takes browser_id,url; observe_tab takes browser_id,target_id,compact:false. Typed input takes the fresh observation_id and observed target ref; click adds expected_selector and exact expected_text. press_observed takes key and NO target. Use schemas for other input/script parameters. A browser_execution:false legacy health field does not mean the standalone action backend is unavailable; use action_backend and advertised methods. Inspect native errors and current state, never blindly replay potentially dispatched input.",
        "guidance":"Keep request_id stable for the same attempted action. Never blindly repeat uncertain input; inspect action_receipt and current state. Pass the current service_epoch and control authority for mutations."})
}
fn tools(info: &Value) -> Value {
    json!({"tools":[
        {"name":"browser_info","description":"Read browser runtime identity, current service epoch, available methods and limits. No browser input is sent.",
         "inputSchema":{"type":"object","properties":{},"additionalProperties":false}},
        {"name":"browser_call","description":"Call the standalone Rust browser runtime. First use browser_info for the epoch and methods. Claim control before input; pass the returned authority. Observe the exact tab, then use typed observed actions. Preserve request_id across an ambiguous attempt and inspect its receipt instead of replaying input. Native status, execution_phase and receipt states are authoritative; dispatch alone does not prove task completion.",
         "inputSchema":{"type":"object","additionalProperties":false,
            "required":["request_id","service_epoch","method","params"],
            "properties":{
                "request_id":{"type":"string","minLength":1,"maxLength":protocol::limit("id_bytes")},
                "service_epoch":{"type":"string","minLength":1},
                "method":{"type":"string","enum":info["methods"]},
                "params":{"type":"object","description":"Parameters of the selected native browser method; see the published protocol schema."},
                "authority":{"type":["object","null"],"description":"Use only for browser_info.authority_methods. Omit or null for launch_browser/open_tab/switch_profile/observations and other methods. A grant on unsupported methods is rejected.","additionalProperties":false,"required":["generation","grant_id"],"properties":{"generation":{"type":"integer","minimum":0},"grant_id":{"type":"string"}}},
                "timeout_ms":{"type":"integer","minimum":1,"maximum":protocol::limit("deadline_ms")}
            }}}
    ]})
}
fn tool_result(reply: protocol::Reply) -> Value {
    let mut content = vec![json!({"type":"text","text":serde_json::to_string(&reply).unwrap()})];
    if !reply.binary.is_empty() {
        let mime = reply
            .result
            .get("content_type")
            .or_else(|| reply.result.get("mime_type"))
            .and_then(Value::as_str);
        let mime = mime.or_else(|| {
            if reply.binary.starts_with(b"\x89PNG\r\n\x1a\n") {
                Some("image/png")
            } else if reply.binary.starts_with(&[0xff, 0xd8, 0xff]) {
                Some("image/jpeg")
            } else {
                None
            }
        });
        if let Some(mime @ ("image/jpeg" | "image/png")) = mime {
            content
                .push(json!({"type":"image","mimeType":mime,"data":BASE64.encode(&reply.binary)}));
        }
    }
    json!({"content":content,"isError":reply.status != "ok" || matches!(reply.execution_phase.as_str(), "uncertain" | "partial")})
}
async fn dispatch(
    service: &Service,
    workspace: &str,
    client: &str,
    catalog: &Value,
    message: &Value,
) -> Value {
    let id = message["id"].clone();
    let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
    match message["method"].as_str().unwrap() {
        "ping" => result(id, json!({})),
        "tools/list" if params.as_object().is_some_and(|p| p.is_empty()) => {
            result(id, tools(catalog))
        }
        "tools/list" => error(id, -32602, "tools/list takes no parameters"),
        "tools/call" => {
            let Some(p) = params.as_object() else {
                return error(id, -32602, "Expected tool call parameters");
            };
            if p.keys()
                .any(|k| !matches!(k.as_str(), "name" | "arguments" | "_meta"))
            {
                return error(id, -32602, "Unknown tool call field");
            }
            let args = p.get("arguments").cloned().unwrap_or_else(|| json!({}));
            match p.get("name").and_then(Value::as_str) {
                Some("browser_info") if args.as_object().is_some_and(|p| p.is_empty()) => result(
                    id,
                    json!({"content":[{"type":"text","text":serde_json::to_string(catalog).unwrap()}],"isError":false}),
                ),
                Some("browser_info") => error(id, -32602, "browser_info takes no arguments"),
                Some("browser_call") => {
                    let Ok(call) = serde_json::from_value::<Call>(args) else {
                        return error(id, -32602, "Invalid browser_call arguments");
                    };
                    let timeout = call
                        .timeout_ms
                        .unwrap_or(protocol::limit("deadline_ms") as u64);
                    if timeout == 0
                        || timeout > protocol::limit("deadline_ms") as u64
                        || !catalog["methods"]
                            .as_array()
                            .is_some_and(|methods| methods.contains(&json!(call.method)))
                    {
                        return error(id, -32602, "Unsupported method or timeout");
                    }
                    let mut req = request(service, workspace, client, &call.method, call.params);
                    req.request_id = call.request_id;
                    req.service_epoch = Some(call.service_epoch);
                    req.authority = call.authority;
                    req.deadline_ms = protocol::now_ms() + timeout;
                    let (reply, _) = service
                        .handle(&req, &mut Some((client.to_owned(), workspace.to_owned())))
                        .await;
                    result(id, tool_result(reply))
                }
                _ => error(id, -32602, "Unknown browser tool"),
            }
        }
        _ => error(id, -32601, "Method not found"),
    }
}
async fn write(output: &Mutex<tokio::io::Stdout>, value: Value) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(&value)?;
    bytes.push(b'\n');
    let mut output = output.lock().await;
    output.write_all(&bytes).await?;
    output.flush().await
}
/// One trusted MCP client, pinned to a workspace. No credentials are accepted over stdin.
/// EOF drains already accepted actions; it never aborts uncertain browser input.
pub async fn serve(service: Arc<Service>, workspace: String, client: String) -> io::Result<()> {
    let probe = request(&service, &workspace, &client, "health", json!({}));
    probe.validate().map_err(io::Error::other)?;
    let catalog = info(&service, &workspace, &client).await;
    if catalog.get("methods").is_none() {
        return Err(io::Error::other("MCP browser bootstrap failed"));
    }
    let mut input = BufReader::new(tokio::io::stdin());
    let output = Arc::new(Mutex::new(tokio::io::stdout()));
    let permits = Arc::new(Semaphore::new(protocol::limit("connections")));
    let mut tasks = JoinSet::new();
    let mut initialized = false;
    let mut ready = false;
    let reading = async { loop {
        while let Some(done) = tasks.try_join_next() { done.map_err(io::Error::other)??; }
        let message = match crate::read_message(&mut input).await {
            Ok(Some(bytes)) => bytes,
            Ok(None) => break,
            Err(e) => { write(&output,error(Value::Null,-32700,"Invalid or oversized MCP message")).await?; return Err(e); }
        };
        let message: Value = match serde_json::from_slice(&message) {
            Ok(value) => value,
            Err(_) => { write(&output,error(Value::Null,-32700,"Parse error")).await?; continue; }
        };
        let valid = message.as_object().is_some_and(|o|o.keys().all(|k|matches!(k.as_str(),"jsonrpc"|"id"|"method"|"params")))
            && message["jsonrpc"] == "2.0" && message["method"].is_string()
            && message.get("params").is_none_or(Value::is_object)
            && message.get("id").is_none_or(|id|id.is_string()||id.is_i64()||id.is_u64());
        if !valid { write(&output,error(Value::Null,-32600,"Invalid Request")).await?; continue; }
        let method = message["method"].as_str().unwrap();
        let Some(id) = message.get("id").cloned() else {
            if initialized && method == "notifications/initialized" { ready = true; }
            continue;
        };
        if method == "initialize" {
            if initialized { write(&output,error(id,-32600,"Already initialized")).await?; continue; }
            if !message["params"]["protocolVersion"].is_string() || !message["params"]["capabilities"].is_object() || !message["params"]["clientInfo"].is_object() {
                write(&output,error(id,-32602,"Invalid initialization parameters")).await?; continue;
            }
            initialized = true;
            write(&output,result(id,json!({"protocolVersion":VERSION,"capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"kageha-browser","version":env!("CARGO_PKG_VERSION")},"instructions":"Standalone Rust browser control. Read browser_info, then call native methods through browser_call. Workspace and client identity are fixed by the server launcher."}))).await?;
            continue;
        }
        if !ready { write(&output,error(id,-32002,"Initialize and send notifications/initialized first")).await?; continue; }
        let Ok(permit) = permits.clone().try_acquire_owned() else { write(&output,error(id,-32000,"MCP request capacity exceeded")).await?; continue; };
        let (service,workspace,client,catalog,output) = (service.clone(),workspace.clone(),client.clone(),catalog.clone(),output.clone());
        tasks.spawn(async move {
            let _permit = permit;
            let reply = dispatch(&service,&workspace,&client,&catalog,&message).await;
            write(&output,reply).await
        });
    }
    Ok::<(), io::Error>(()) }.await;
    let mut task_error = None;
    while let Some(done) = tasks.join_next().await {
        if let Err(error) = done.map_err(io::Error::other).and_then(|r| r) {
            task_error = Some(error);
        }
    }
    reading?;
    if let Some(error) = task_error {
        return Err(error);
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn service() -> (tempfile::TempDir, Service) {
        let root = tempfile::tempdir().unwrap();
        let state = crate::state::State::open(
            &root.path().join("state"),
            "mcp-test".into(),
            "browser-test".into(),
        )
        .unwrap();
        let (shutdown, _) = tokio::sync::watch::channel(false);
        (
            root,
            Service {
                state,
                seen: std::sync::Mutex::new(Default::default()),
                active_reads: std::sync::Mutex::new(Default::default()),
                shutdown,
                browsers: None,
                canary_workspace: None,
                decisions: None,
            },
        )
    }
    #[tokio::test]
    async fn discovery_describes_bootstrap_and_authority_without_client_wrappers() {
        let (_root, service) = service();
        let catalog = info(&service, "workspace", "client").await;
        assert_eq!(catalog["startup"]["method"], "launch_browser");
        assert!(catalog["startup"]["authority"].is_null());
        let controlled = catalog["authority_methods"].as_array().unwrap();
        assert!(controlled.contains(&json!("fill_observed")));
        assert!(!controlled.contains(&json!("launch_browser")));
        assert!(!controlled.contains(&json!("open_tab")));
        assert!(catalog["parameter_schemas"]["FillObservedParams"].is_object());
        assert!(catalog["parameter_schemas"]["PressObservedParams"].is_object());
        assert_eq!(catalog["method_parameter_examples"]["list_user_tabs"], json!({}));
        assert_eq!(catalog["method_parameter_examples"]["switch_profile"]["consent"], true);
        assert_eq!(catalog["method_parameter_examples"]["switch_profile"]["endpoint"], "http://127.0.0.1:9222");
    }
    #[tokio::test]
    async fn native_epoch_errors_remain_tool_errors_and_unknown_fields_never_dispatch() {
        let (_root, service) = service();
        let catalog = info(&service, "workspace", "client").await;
        assert!(!catalog["methods"]
            .as_array()
            .unwrap()
            .contains(&json!("shutdown")));
        let mut call = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"browser_call","arguments":{"request_id":"health","service_epoch":"old","method":"health","params":{}}}});
        let reply = dispatch(&service, "workspace", "client", &catalog, &call).await;
        assert_eq!(reply["result"]["isError"], true);
        let native: Value =
            serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(native["error"]["code"], "stale_service_epoch");
        call["params"]["arguments"]["service_epoch"] = json!(service.state.epoch);
        call["params"]["arguments"]["workspace_id"] = json!("other");
        assert_eq!(
            dispatch(&service, "workspace", "client", &catalog, &call).await["error"]["code"],
            -32602
        );
        call["params"]["arguments"]
            .as_object_mut()
            .unwrap()
            .remove("workspace_id");
        assert_eq!(
            dispatch(&service, "workspace", "client", &catalog, &call).await["result"]["isError"],
            false
        );
    }
    #[test]
    fn image_results_keep_native_metadata_and_encode_image_content() {
        let mut reply = protocol::Reply::new(None, "i", "a", "e");
        reply.binary = vec![0xff, 0xd8, 0xff];
        reply.result = json!({"content_type":"image/jpeg"});
        let value = tool_result(reply);
        assert_eq!(value["content"][1]["mimeType"], "image/jpeg");
        assert_eq!(value["content"][1]["data"], "/9j/");
    }
}
