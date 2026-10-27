use serde_json::{json, Value};
use std::io;
fn blocks(content: &Value) -> io::Result<Value> {
    if let Some(text) = content.as_str() {
        return Ok(json!([{"type":"text","text":text}]));
    }
    let mut result = Vec::new();
    for block in content.as_array().into_iter().flatten() {
        match block["type"].as_str() {
            Some("text") => result.push(json!({"type":"text","text":block["text"]})),
            Some("image_url") => {
                let value = block["image_url"]["url"].as_str().unwrap_or("");
                let (mime, data) = value
                    .strip_prefix("data:")
                    .and_then(|v| v.split_once(";base64,"))
                    .ok_or_else(|| {
                        io::Error::other("Anthropic images require a pinned base64 image")
                    })?;
                result.push(json!({"type":"image","source":{"type":"base64","media_type":mime,"data":data}}));
            }
            _ => return Err(io::Error::other("Unsupported provider content block")),
        }
    }
    Ok(json!(result))
}
pub fn anthropic_request(model: &str, messages: &[Value], tools: &[Value]) -> io::Result<Value> {
    let mut system = Vec::new();
    let mut converted: Vec<Value> = Vec::new();
    for message in messages {
        match message["role"].as_str() {
            Some("system") => {
                system.push(message["content"].as_str().unwrap_or("").to_owned());
            }
            Some("user" | "assistant") => {
                let mut content = blocks(&message["content"])?.as_array().unwrap().clone();
                for call in message["tool_calls"].as_array().into_iter().flatten() {
                    content.push(json!({"type":"tool_use","id":call["id"],"name":call["name"],"input":call["arguments"]}));
                }
                if content.is_empty() {
                    continue;
                }
                converted.push(json!({"role":message["role"],"content":content}));
            }
            Some("tool") => {
                let block = json!({"type":"tool_result","tool_use_id":message["tool_call_id"],"content":message["content"].as_str().unwrap_or(""),"is_error":message["is_error"].as_bool().unwrap_or(false)});
                if let Some(last) = converted.last_mut().filter(|v| v["role"] == "user") {
                    last["content"].as_array_mut().unwrap().push(block);
                } else {
                    converted.push(json!({"role":"user","content":[block]}));
                }
            }
            _ => return Err(io::Error::other("Invalid conversation role")),
        }
    }
    let specs:Vec<_>=tools.iter().map(|t|json!({"name":t["function"]["name"],"description":t["function"]["description"],"input_schema":t["function"]["parameters"]})).collect();
    Ok(
        json!({"model":model,"max_tokens":8192,"system":system.join("\n\n"),"messages":converted,"tools":specs}),
    )
}
pub fn anthropic_response(response: &Value) -> io::Result<Value> {
    if response["stop_reason"] == "max_tokens" {
        return Err(io::Error::other("Model output limit reached before the response completed"));
    }
    if response["role"] != "assistant" {
        return Err(io::Error::other(
            "Provider did not return an assistant response",
        ));
    }
    let mut text = Vec::new();
    let mut calls = Vec::new();
    for block in response["content"]
        .as_array()
        .ok_or_else(|| io::Error::other("Assistant content missing"))?
    {
        match block["type"].as_str() {
            Some("text") => text.push(block["text"].as_str().unwrap_or("")),
            Some("tool_use") => {
                if !block["input"].is_object()
                    || block["id"].as_str().is_none()
                    || block["name"].as_str().is_none()
                {
                    return Err(io::Error::other("Invalid tool call"));
                }
                calls.push(
                    json!({"id":block["id"],"name":block["name"],"arguments":block["input"]}),
                );
            }
            _ => {}
        }
    }
    let content = text.join("\n");
    if content.len() > 262144 || calls.len() > 16 {
        return Err(io::Error::other("Provider response exceeds turn limits"));
    }
    Ok(json!({"message":{"role":"assistant","content":content,"tool_calls":calls}}))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tool_results_keep_ids_and_order() {
        let body=anthropic_request("fixture",&[json!({"role":"system","content":"rules"}),json!({"role":"user","content":"work"}),json!({"role":"assistant","content":null,"tool_calls":[{"id":"a","name":"read","arguments":{"path":"notes"}},{"id":"b","name":"read","arguments":{}}]}),json!({"role":"tool","tool_call_id":"a","content":"first"}),json!({"role":"tool","tool_call_id":"b","content":"second"})],&[]).unwrap();
        assert_eq!(body["system"], "rules");
        assert_eq!(body["messages"][2]["content"][0]["tool_use_id"], "a");
        assert_eq!(body["messages"][2]["content"][1]["tool_use_id"], "b");
    }
}
