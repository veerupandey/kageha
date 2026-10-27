use crate::{ModelActivity, ModelActivityObserver};
use serde_json::{json, Value};
use std::{collections::BTreeMap, io};

pub(crate) struct ChatStream<'a> {
    observer: Option<&'a ModelActivityObserver>,
    id: String,
    pending: Vec<u8>,
    text: String,
    shown: usize,
    offset: usize,
    calls: BTreeMap<usize, Value>,
    finish: Option<String>,
    suppress: bool,
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
            offset: 0,
            calls: BTreeMap::new(),
            finish: None,
            suppress: false,
            ended: false,
        }
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
                .map_err(|_| io::Error::other("Invalid model stream event"))?;
            if frame.get("error").is_some() {
                return Err(io::Error::other("Model stream returned an error"));
            }
            let choice = &frame["choices"][0];
            let delta = &choice["delta"];
            if let Some(text) = delta["content"].as_str() {
                self.text.push_str(text);
                self.publish();
            }
            if let Some(calls) = delta["tool_calls"].as_array() {
                for call in calls {
                    let index = call["index"]
                        .as_u64()
                        .filter(|index| *index < 16)
                        .ok_or_else(|| io::Error::other("Invalid streamed tool index"))?
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

    pub(crate) fn finish(mut self) -> io::Result<Value> {
        if !self.pending.is_empty() {
            self.push(b"\n")?;
        }
        let finish = self
            .finish
            .take()
            .ok_or_else(|| io::Error::other("Model stream ended before completion"))?;
        let calls = std::mem::take(&mut self.calls)
            .into_values()
            .collect::<Vec<_>>();
        Ok(json!({"choices":[{"finish_reason":finish,"message":{
            "role":"assistant","content":self.text,"tool_calls":calls
        }}]}))
    }
}

impl Drop for ChatStream<'_> {
    fn drop(&mut self) {
        self.discard();
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
    fn rejects_truncated_stream_and_discards_partial_text() {
        let mut stream = ChatStream::new(None, "answer".into());
        stream
            .push(b"data: {\"choices\":[{\"delta\":{\"content\":\"Unfinished\"}}]}\n\n")
            .unwrap();
        assert!(stream.finish().is_err());
    }
}
