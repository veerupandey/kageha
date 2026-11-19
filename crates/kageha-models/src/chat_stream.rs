use crate::{failure, ModelActivity, ModelActivityObserver};
use kageha_contracts::execution::{ModelFailure, ModelFailureKind};
use serde_json::{json, Value};
use std::{collections::BTreeMap, io};

pub(crate) fn reasoning_text(value: &Value) -> Option<&str> {
    ["reasoning_content", "reasoning"]
        .iter()
        .find_map(|field| value[*field].as_str().filter(|text| !text.is_empty()))
}

pub(crate) struct ChatStream<'a> {
    observer: Option<&'a ModelActivityObserver>,
    id: String,
    pending: Vec<u8>,
    text: String,
    shown: usize,
    reasoning: String,
    reasoning_offset: usize,
    offset: usize,
    calls: BTreeMap<usize, Value>,
    finish: Option<String>,
    anthropic_blocks: Option<BTreeMap<usize, Value>>,
    anthropic_input: BTreeMap<usize, String>,
    suppress: bool,
    /// Provider `usage` object, merged across frames; attached to the final response.
    usage: serde_json::Map<String, Value>,
    pub(crate) ended: bool,
}

impl<'a> ChatStream<'a> {
    pub(crate) fn new(observer: Option<&'a ModelActivityObserver>, id: String) -> Self {
        Self {
            observer,
            id,
            pending: Vec::new(),
            text: String::new(),
            shown: 0,
            reasoning: String::new(),
            reasoning_offset: 0,
            offset: 0,
            calls: BTreeMap::new(),
            finish: None,
            anthropic_blocks: None,
            anthropic_input: BTreeMap::new(),
            suppress: false,
            usage: serde_json::Map::new(),
            ended: false,
        }
    }

    /// Later frames override earlier fields (Anthropic `message_delta` carries
    /// the final counts after `message_start`).
    fn merge_usage(&mut self, usage: &Value) {
        if let Some(fields) = usage.as_object() {
            for (key, value) in fields.iter().filter(|(_, value)| !value.is_null()) {
                self.usage.insert(key.clone(), value.clone());
            }
        }
    }

    pub(crate) fn anthropic(mut self) -> Self {
        self.anthropic_blocks = Some(BTreeMap::new());
        self
    }

    fn push_anthropic(&mut self, frame: &Value) -> io::Result<()> {
        let kind = frame["type"].as_str().unwrap_or_default();
        match kind {
            "message_start" => self.merge_usage(&frame["message"]["usage"]),
            "message_delta" => {
                self.finish = frame["delta"]["stop_reason"].as_str().map(str::to_owned);
                self.merge_usage(&frame["usage"]);
            }
            "message_stop" => self.ended = true,
            "content_block_start" | "content_block_delta" => {
                let index = frame["index"]
                    .as_u64()
                    .filter(|i| *i < 256)
                    .ok_or_else(|| failure(ModelFailureKind::InvalidResponse,"Invalid Messages stream block index"))?
                    as usize;
                let blocks = self.anthropic_blocks.as_mut().unwrap();
                if kind == "content_block_start" {
                    let block = &frame["content_block"];
                    // Keep only the fields consumed by the ordinary response adapter.
                    match block["type"].as_str() {
                        Some("text" | "tool_use") => {
                            blocks.insert(index, block.clone());
                            if block["type"] == "tool_use" {
                                self.discard();
                                self.suppress = true;
                            } else if let Some(text) = block["text"].as_str() {
                                self.text.push_str(text);
                                self.publish();
                            }
                        }
                        Some("thinking") => {
                            if let Some(text) = block["thinking"].as_str() {
                                self.publish_reasoning(text);
                            }
                        }
                        _ => {}
                    }
                } else {
                    let delta = &frame["delta"];
                    match delta["type"].as_str() {
                        Some("thinking_delta") => {
                            if let Some(text) = delta["thinking"].as_str() {
                                self.publish_reasoning(text);
                            }
                        }
                        Some("text_delta") => {
                            if let (Some(block), Some(text)) =
                                (blocks.get_mut(&index), delta["text"].as_str())
                            {
                                let mut content =
                                    block["text"].as_str().unwrap_or_default().to_owned();
                                content.push_str(text);
                                block["text"] = json!(content);
                                self.text.push_str(text);
                                self.publish();
                            }
                        }
                        Some("input_json_delta") => {
                            if let Some(fragment) = delta["partial_json"].as_str() {
                                self.anthropic_input
                                    .entry(index)
                                    .or_default()
                                    .push_str(fragment);
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub(crate) fn push(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.pending.extend_from_slice(bytes);
        let end = self.pending.iter().rposition(|&byte| byte == b'\n');
        let Some(end) = end else { return Ok(()) };
        let lines: Vec<_> = self.pending.drain(..=end).collect();
        for line in lines.split(|&byte| byte == b'\n') {
            let Some(data) = line.strip_prefix(b"data:") else {
                continue;
            };
            let data = data.trim_ascii();
            if data == b"[DONE]" {
                self.ended = true;
                break;
            }
            let frame: Value = serde_json::from_slice(data)
                .map_err(|_| failure(ModelFailureKind::InvalidResponse,"Invalid model stream event"))?;
            if frame.get("error").is_some() {
                return Err(crate::provider_failure(400, data, None));
            }
            if self.anthropic_blocks.is_some() {
                self.push_anthropic(&frame)?;
                if self.ended {
                    break;
                }
                continue;
            }
            // With `stream_options.include_usage`, usage arrives in a final chunk with no choices.
            self.merge_usage(&frame["usage"]);
            let choice = &frame["choices"][0];
            let delta = &choice["delta"];
            if let Some(text) = reasoning_text(delta) {
                self.publish_reasoning(text);
            }
            if let Some(text) = delta["content"].as_str() {
                self.text.push_str(text);
                self.publish();
            }
            if let Some(calls) = delta["tool_calls"].as_array() {
                for call in calls {
                    let index = call["index"]
                        .as_u64()
                        .filter(|index| *index < 16)
                        .ok_or_else(|| failure(ModelFailureKind::InvalidResponse,"Invalid streamed tool index"))?
                        as usize;
                    let entry = self
                        .calls
                        .entry(index)
                        .or_insert_with(|| json!({"id":"","function":{"name":"","arguments":""}}));
                    if let Some(id) = call["id"].as_str() {
                        entry["id"] = json!(id);
                    }
                    for key in ["name", "arguments"] {
                        if let Some(fragment) = call["function"][key].as_str() {
                            let mut value = entry["function"][key]
                                .as_str()
                                .unwrap_or_default()
                                .to_owned();
                            value.push_str(fragment);
                            entry["function"][key] = json!(value);
                        }
                    }
                    if let Some(extra) = call.get("extra_content") {
                        entry["extra_content"] = extra.clone();
                    }
                }
            }
            if let Some(reason) = choice["finish_reason"].as_str() {
                self.finish = Some(reason.to_owned());
            }
        }
        Ok(())
    }

    pub(crate) fn publish_reasoning(&mut self, text: &str) {
        let Some(observer) = self.observer else {
            return;
        };
        let remaining = (64 * 1024usize).saturating_sub(self.reasoning.len());
        let mut end = remaining.min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if end == 0 {
            return;
        }
        let chunk = &text[..end];
        observer(ModelActivity::ReasoningDelta {
            id: format!("{}-reasoning", self.id),
            offset: self.reasoning_offset,
            text: chunk.to_owned(),
        });
        self.reasoning.push_str(chunk);
        self.reasoning_offset += chunk.encode_utf16().count();
    }

    fn publish(&mut self) {
        // Hold a trailing '<' so a control marker split across chunks stays private.
        if self.text[self.shown..].contains("<|") {
            self.discard();
            self.suppress = true;
        }
        if self.suppress {
            return;
        }
        let end = self.text.trim_end_matches('<').len();
        if end <= self.shown {
            return;
        }
        let chunk = &self.text[self.shown..end];
        if let Some(observer) = self.observer {
            observer(ModelActivity::Text {
                id: self.id.clone(),
                offset: self.offset,
                text: chunk.to_owned(),
            });
        }
        self.shown = end;
        self.offset += chunk.encode_utf16().count();
    }

    fn discard(&mut self) {
        if self.shown > 0 {
            if let Some(observer) = self.observer {
                observer(ModelActivity::DiscardText {
                    id: self.id.clone(),
                });
            }
            self.shown = 0;
        }
    }

    pub(crate) fn complete_reasoning(&mut self) {
        if let Some(observer) = self.observer.filter(|_| !self.reasoning.is_empty()) {
            observer(ModelActivity::CompleteReasoning {
                id: format!("{}-reasoning", self.id),
                text: std::mem::take(&mut self.reasoning),
            });
        }
    }

    pub(crate) fn has_partial_tool_calls(&self) -> bool {
        !self.calls.is_empty() || !self.anthropic_input.is_empty()
            || self.anthropic_blocks.as_ref().is_some_and(|blocks|
                blocks.values().any(|block| block["type"] == "tool_use"))
    }

    fn interrupted(&self, message: &str) -> io::Error {
        let mut error = ModelFailure::new(ModelFailureKind::InterruptedStream, message);
        error.partial_tool_calls = self.has_partial_tool_calls();
        io::Error::other(error)
    }

    pub(crate) fn finish(mut self) -> io::Result<Value> {
        if !self.pending.is_empty() {
            self.push(b"\n")?;
        }
        let finish = self
            .finish
            .take()
            .ok_or_else(|| self.interrupted("Model stream ended before completion"))?;
        if self.anthropic_blocks.is_some() && !self.ended {
            return Err(self.interrupted("Messages stream ended before message_stop"));
        }
        let calls = std::mem::take(&mut self.calls)
            .into_values()
            .collect::<Vec<_>>();
        if let Some(mut blocks) = self.anthropic_blocks.take() {
            for (index, input) in &self.anthropic_input {
                let block = blocks
                    .get_mut(index)
                    .filter(|block| block["type"] == "tool_use")
                    .ok_or_else(|| failure(ModelFailureKind::InvalidResponse,"Messages stream tool block missing"))?;
                // Invalid JSON is kept as its raw text for the runtime's
                // `invalid_tool_call` reply (Requirement 10.4).
                block["input"] = serde_json::from_str(input).unwrap_or_else(|_| json!(input));
            }
            let mut response = json!({"role":"assistant","stop_reason":finish,
                "content":blocks.into_values().collect::<Vec<_>>()});
            crate::protocol::anthropic_response(&response)?;
            self.attach_usage(&mut response);
            self.complete_reasoning();
            return Ok(response);
        }
        self.complete_reasoning();
        let mut response = json!({"choices":[{"finish_reason":finish,"message":{
            "role":"assistant","content":self.text,"tool_calls":calls
        }}]});
        self.attach_usage(&mut response);
        Ok(response)
    }

    fn attach_usage(&mut self, response: &mut Value) {
        if !self.usage.is_empty() {
            response["usage"] = Value::Object(std::mem::take(&mut self.usage));
        }
    }
}

impl Drop for ChatStream<'_> {
    fn drop(&mut self) {
        self.discard();
        if let Some(observer) = self.observer.filter(|_| !self.reasoning.is_empty()) {
            observer(ModelActivity::DiscardReasoning {
                id: format!("{}-reasoning", self.id),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn streams_text_and_assembles_split_tool_arguments_privately() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let output = events.clone();
        let observer: ModelActivityObserver =
            Arc::new(move |activity| output.lock().unwrap().push(activity));
        let mut stream = ChatStream::new(Some(&observer), "answer".into());
        let first = b"data: {\"choices\":[{\"delta\":{\"content\":\"Hi \",\"tool_calls\":[{\"index\":0,\"id\":\"call-1\",\"function\":{\"name\":\"search\",\"arguments\":\"{\"}}]}}]}\r\n\r\n";
        stream.push(&first[..12]).unwrap();
        assert!(events.lock().unwrap().is_empty());
        stream.push(&first[12..]).unwrap();
        assert!(
            matches!(events.lock().unwrap().as_slice(), [ModelActivity::Text { text, offset: 0, .. }] if text == "Hi ")
        );
        stream.push(b"data: {\"choices\":[{\"delta\":{\"content\":\"there\",\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n").unwrap();
        let reply = stream.finish().unwrap();
        assert_eq!(
            reply["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
            "{}"
        );
        assert!(matches!(
            events.lock().unwrap().as_slice(),
            [
                ModelActivity::Text { .. },
                ModelActivity::Text { offset: 3, .. },
                ModelActivity::DiscardText { .. }
            ]
        ));
    }

    #[test]
    fn streams_reasoning_separately_from_the_answer() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let output = events.clone();
        let observer: ModelActivityObserver =
            Arc::new(move |activity| output.lock().unwrap().push(activity));
        let mut stream = ChatStream::new(Some(&observer), "answer".into());
        stream.push("data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"🧩\",\"reasoning\":\"🧩\"}}]}\n".as_bytes()).unwrap();
        assert!(matches!(events.lock().unwrap().as_slice(),
            [ModelActivity::ReasoningDelta { id, offset: 0, text }] if id == "answer-reasoning" && text == "🧩"));
        stream.push(b"data: {\"choices\":[{\"delta\":{\"reasoning\":\" Check\",\"content\":\"Answer\"},\"finish_reason\":\"stop\"}]}\n").unwrap();
        let reply = stream.finish().unwrap();
        assert_eq!(reply["choices"][0]["message"]["content"], "Answer");
        assert!(matches!(events.lock().unwrap().as_slice(),
            [ModelActivity::ReasoningDelta { .. }, ModelActivity::ReasoningDelta { offset: 2, .. },
             ModelActivity::Text { .. }, ModelActivity::CompleteReasoning { text, .. },
             ModelActivity::DiscardText { .. }] if text == "🧩 Check"));
    }

    #[test]
    fn messages_text_is_visible_before_message_stop() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let output = events.clone();
        let observer: ModelActivityObserver =
            Arc::new(move |activity| output.lock().unwrap().push(activity));
        let mut stream = ChatStream::new(Some(&observer), "ack".into()).anthropic();
        stream.push(br#"data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Checking"}}
"#).unwrap();
        assert!(matches!(events.lock().unwrap().as_slice(),
            [ModelActivity::Text { offset: 0, text, .. }] if text == "Checking"));
    }

    #[test]
    fn streams_messages_thinking_and_preserves_tool_input() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let output = events.clone();
        let observer: ModelActivityObserver =
            Arc::new(move |activity| output.lock().unwrap().push(activity));
        let mut stream = ChatStream::new(Some(&observer), "messages".into()).anthropic();
        stream.push(br#"data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}
data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Checking."}}
"#).unwrap();
        assert!(matches!(events.lock().unwrap().as_slice(),
            [ModelActivity::ReasoningDelta { offset: 0, text, .. }] if text == "Checking."));
        stream.push(br#"data: {"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"call","name":"lookup","input":{}}}
data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"a\":"}}
data: {"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"1}"}}
data: {"type":"message_delta","delta":{"stop_reason":"tool_use"}}
data: {"type":"message_stop"}
"#).unwrap();
        let reply = crate::protocol::anthropic_response(&stream.finish().unwrap()).unwrap();
        assert_eq!(
            reply["message"]["tool_calls"][0]["arguments"],
            json!({"a":1})
        );
        assert_eq!(reply["message"]["content"], "");
    }

    #[test]
    fn openai_stream_keeps_usage_from_the_final_chunk() {
        let mut stream = ChatStream::new(None, "usage".into());
        stream.push(br#"data: {"choices":[{"delta":{"content":"Done"},"finish_reason":"stop"}],"usage":null}
data: {"choices":[],"usage":{"prompt_tokens":900,"completion_tokens":3,"prompt_tokens_details":{"cached_tokens":768}}}
data: [DONE]
"#).unwrap();
        let reply = stream.finish().unwrap();
        assert_eq!(reply["choices"][0]["message"]["content"], "Done");
        let usage = crate::protocol::model_usage(&reply).unwrap();
        assert_eq!(usage.cached_tokens, Some(768));
        assert_eq!(usage.input_tokens, Some(900));
    }

    #[test]
    fn anthropic_stream_merges_usage_from_message_start_and_delta() {
        let mut stream = ChatStream::new(None, "usage".into()).anthropic();
        stream.push(br#"data: {"type":"message_start","message":{"role":"assistant","usage":{"input_tokens":20,"cache_read_input_tokens":4096,"output_tokens":1}}}
data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}
data: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":7}}
data: {"type":"message_stop"}
"#).unwrap();
        let reply = stream.finish().unwrap();
        let usage = crate::protocol::model_usage(&reply).unwrap();
        assert_eq!(usage.cached_tokens, Some(4096));
        assert_eq!(usage.input_tokens, Some(20));
        assert_eq!(usage.output_tokens, Some(7));
        assert_eq!(
            crate::protocol::anthropic_response(&reply).unwrap()["message"]["content"],
            "Hi"
        );
    }

    #[test]
    fn interrupted_stream_reports_pending_tool_arguments_without_exposing_them() {
        let mut stream=ChatStream::new(None,"partial".into());
        stream.push(br#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call","function":{"name":"write","arguments":"{"}}]}}]}
"#).unwrap();
        let error=stream.finish().unwrap_err();
        let typed=kageha_contracts::execution::model_failure(&error).unwrap();
        assert_eq!(typed.kind,ModelFailureKind::InterruptedStream);
        assert!(typed.partial_tool_calls);
        assert!(!error.to_string().contains("arguments"));
    }

    #[test]
    fn rejects_truncated_stream_and_discards_partial_text() {
        let mut stream = ChatStream::new(None, "answer".into());
        stream
            .push(b"data: {\"choices\":[{\"delta\":{\"content\":\"Unfinished\"}}]}\n\n")
            .unwrap();
        let error=stream.finish().unwrap_err();
        let typed=kageha_contracts::execution::model_failure(&error).unwrap();
        assert_eq!(typed.kind,ModelFailureKind::InterruptedStream);
        assert!(!typed.partial_tool_calls);
    }
}
