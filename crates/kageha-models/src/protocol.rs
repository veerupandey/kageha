use serde_json::{json, Value};
use kageha_contracts::execution::{ModelFailure, ModelFailureKind};
use crate::failure;
use std::io;
// Bound the actual wire encoding. Keep tool receipts and descriptive text even
// when many earlier inline frames can no longer fit in the same provider call.
pub(crate) fn request_bytes(mut body: Value, limit: usize) -> io::Result<Vec<u8>> {
    let bytes = serde_json::to_vec(&body)?;
    if bytes.len() <= limit {
        return Ok(bytes);
    }
    let mut size = bytes.len();
    drop(bytes);
    let omitted = json!({"type":"text","text":"[Image omitted from this model call to fit the request. Inspect its source separately if needed.]"});
    let marker_size = serde_json::to_vec(&omitted)?.len();
    for message in body.get_mut("messages").and_then(Value::as_array_mut).into_iter().flatten() {
        for part in message.get_mut("content").and_then(Value::as_array_mut).into_iter().flatten() {
            let inline = match part["type"].as_str() {
                Some("image_url") => part["image_url"]["url"].as_str().is_some_and(|s| s.starts_with("data:image/")),
                Some("image") => part["source"]["type"] == "base64",
                _ => false,
            };
            if inline {
                let part_size = serde_json::to_vec(&part)?.len();
                if part_size > marker_size {
                    *part = omitted.clone();
                    size -= part_size - marker_size;
                    if size <= limit {
                        return Ok(serde_json::to_vec(&body)?);
                    }
                }
            }
        }
    }
    Err(failure(ModelFailureKind::ContextLimit, "Model request exceeds limit"))
}

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
        let mut error = ModelFailure::new(ModelFailureKind::OutputLimit,"Model output limit reached before the response completed");
        error.partial_tool_calls = response["content"].as_array().is_some_and(|blocks| blocks.iter().any(|block| block["type"] == "tool_use"));
        return Err(io::Error::other(error));
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
        .ok_or_else(|| failure(ModelFailureKind::InvalidResponse,"Assistant content missing"))?
    {
        match block["type"].as_str() {
            Some("text") => text.push(block["text"].as_str().unwrap_or("")),
            Some("tool_use") => {
                // Input that is not an object stays in the call; the runtime
                // answers it with `invalid_tool_call` (Requirement 10.4).
                if block["id"].as_str().is_none() || block["name"].as_str().is_none() {
                    return Err(failure(ModelFailureKind::InvalidResponse,"Invalid tool call"));
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
        return Err(failure(ModelFailureKind::InvalidResponse,"Provider response exceeds turn limits"));
    }
    Ok(json!({"message":{"role":"assistant","content":content,"tool_calls":calls}}))
}
/// Reads provider-reported token usage from an OpenAI-format
/// (`prompt_tokens`, `prompt_tokens_details.cached_tokens`) or Anthropic-format
/// (`input_tokens`, `cache_read_input_tokens`) `usage` object.
pub fn model_usage(response: &Value) -> Option<kageha_contracts::execution::ModelUsage> {
    let usage = response.get("usage").filter(|usage| usage.is_object())?;
    let first = |fields: &[&str]| fields.iter().find_map(|field| usage[*field].as_u64());
    Some(kageha_contracts::execution::ModelUsage {
        input_tokens: first(&["prompt_tokens", "input_tokens"]),
        output_tokens: first(&["completion_tokens", "output_tokens"]),
        cached_tokens: usage["prompt_tokens_details"]["cached_tokens"]
            .as_u64()
            .or_else(|| usage["cache_read_input_tokens"].as_u64()),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use kageha_contracts::execution::ModelUsage;
    #[test]
    fn older_anthropic_frames_yield_space_without_losing_receipts_or_newer_frames() {
        let older = json!({"type":"image","source":{"type":"base64","data":"A".repeat(800)}});
        let recent = json!({"type":"image","source":{"type":"base64","data":"B".repeat(800)}});
        let receipt = json!({"type":"tool_result","tool_use_id":"extract","content":"Read frames."});
        let body = json!({"messages":[{"role":"user","content":[older,receipt,recent]}]});
        let output = request_bytes(body, 1300).unwrap();
        assert!(output.len() <= 1300);
        let result:Value = serde_json::from_slice(&output).unwrap();
        let parts = &result["messages"][0]["content"];
        assert_eq!(parts[0]["type"],"text");
        assert_eq!(parts[1],receipt);
        assert_eq!(parts[2],recent);
        assert!(request_bytes(json!({"messages":[{"role":"user","content":"x".repeat(2000)}]}),1300).is_err());
    }

    #[test]
    fn reads_cached_tokens_in_openai_and_anthropic_formats() {
        let openai = json!({"usage":{"prompt_tokens":1200,"completion_tokens":40,
            "prompt_tokens_details":{"cached_tokens":1024}}});
        assert_eq!(model_usage(&openai), Some(ModelUsage {
            input_tokens: Some(1200), output_tokens: Some(40), cached_tokens: Some(1024) }));
        let anthropic = json!({"usage":{"input_tokens":30,"output_tokens":12,
            "cache_creation_input_tokens":0,"cache_read_input_tokens":2048}});
        assert_eq!(model_usage(&anthropic), Some(ModelUsage {
            input_tokens: Some(30), output_tokens: Some(12), cached_tokens: Some(2048) }));
        let unreported = json!({"usage":{"prompt_tokens":5,"completion_tokens":1}});
        assert_eq!(model_usage(&unreported).unwrap().cached_tokens, None);
        assert_eq!(model_usage(&json!({"usage":null})), None);
        assert_eq!(model_usage(&json!({})), None);
    }
    #[test]
    fn tool_results_keep_ids_and_order() {
        let body=anthropic_request("fixture",&[json!({"role":"system","content":"rules"}),json!({"role":"user","content":"work"}),json!({"role":"assistant","content":null,"tool_calls":[{"id":"a","name":"read","arguments":{"path":"notes"}},{"id":"b","name":"read","arguments":{}}]}),json!({"role":"tool","tool_call_id":"a","content":"first"}),json!({"role":"tool","tool_call_id":"b","content":"second"})],&[]).unwrap();
        assert_eq!(body["system"], "rules");
        assert_eq!(body["messages"][2]["content"][0]["tool_use_id"], "a");
        assert_eq!(body["messages"][2]["content"][1]["tool_use_id"], "b");
    }
}
