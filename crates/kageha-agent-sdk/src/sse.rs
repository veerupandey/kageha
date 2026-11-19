use crate::{Error, Result, RunEvent, LIMIT};
use std::{collections::VecDeque, time::Duration};
#[derive(Default)]
struct Parser {
    line: Vec<u8>,
    data: Vec<String>,
    kind: String,
    id: Option<String>,
    size: usize,
    cr: bool,
}
impl Parser {
    fn feed(&mut self, bytes: &[u8]) -> Result<Vec<RunEvent>> {
        let mut events = Vec::new();
        for &b in bytes {
            if self.cr {
                self.cr = false;
                if b == b'\n' {
                    continue;
                }
            }
            self.size += 1;
            if self.size > LIMIT {
                return Err(Error::protocol("Runtime event exceeds size limit"));
            }
            if b == b'\n' || b == b'\r' {
                self.cr = b == b'\r';
                let line = String::from_utf8(std::mem::take(&mut self.line))
                    .map_err(|_| Error::protocol("Invalid UTF-8 event"))?;
                if line.is_empty() {
                    if !self.data.is_empty() {
                        if self.kind != "run" {
                            return Err(Error::protocol(
                                "Runtime event stream reported an error or unknown event",
                            ));
                        }
                        let event: RunEvent = serde_json::from_str(&self.data.join("\n"))
                            .map_err(|_| Error::protocol("Invalid runtime event JSON"))?;
                        if self.id.as_ref().and_then(|v| v.parse::<u64>().ok())
                            != Some(event.sequence)
                        {
                            return Err(Error::protocol("Event ID does not match sequence"));
                        }
                        events.push(event);
                    }
                    self.data.clear();
                    self.kind.clear();
                    self.id = None;
                    self.size = 0;
                } else if !line.starts_with(':') {
                    let (field, value) = line.split_once(':').unwrap_or((&line, ""));
                    let value = value.strip_prefix(' ').unwrap_or(value);
                    match field {
                        "data" => self.data.push(value.into()),
                        "event" => self.kind = value.into(),
                        "id" => self.id = Some(value.into()),
                        _ => {}
                    }
                }
            } else {
                self.line.push(b);
            }
        }
        Ok(events)
    }
    fn finish(&self) -> Result<()> {
        if !self.line.is_empty() || !self.data.is_empty() {
            return Err(Error::protocol(
                "Truncated runtime event; resume from the last delivered cursor",
            ));
        }
        Ok(())
    }
}
/// A resumable, bounded lifecycle stream. Drop to close; resume explicitly with last_sequence().
pub struct Events {
    response: Option<reqwest::Response>,
    parser: Parser,
    queue: VecDeque<RunEvent>,
    run: String,
    last: u64,
    timeout: Duration,
}
impl Events {
    pub(crate) fn new(
        response: reqwest::Response,
        run: String,
        last: u64,
        timeout: Duration,
    ) -> Self {
        Self {
            response: Some(response),
            parser: Parser::default(),
            queue: VecDeque::new(),
            run,
            last,
            timeout,
        }
    }
    pub fn last_sequence(&self) -> u64 {
        self.last
    }
    pub fn close(&mut self) {
        self.response = None;
        self.queue.clear();
    }
    pub async fn next(&mut self) -> Result<Option<RunEvent>> {
        let result = self.read().await;
        if result.is_err() {
            self.close();
        }
        result
    }
    async fn read(&mut self) -> Result<Option<RunEvent>> {
        loop {
            if let Some(event) = self.queue.pop_front() {
                if event.sequence <= self.last
                    || event.sequence > i64::MAX as u64
                    || event.record.spec.id != self.run
                {
                    return Err(Error::protocol("Out-of-order event or wrong run identity"));
                }
                self.last = event.sequence;
                if event.record.state.terminal() {
                    self.close();
                }
                return Ok(Some(event));
            }
            let Some(response) = self.response.as_mut() else {
                return Ok(None);
            };
            let bytes = tokio::time::timeout(self.timeout, response.chunk())
                .await
                .map_err(|_| Error::protocol("Runtime event stream idle timeout"))?
                .map_err(|_| Error::transport())?;
            match bytes {
                Some(bytes) => {
                    if bytes.len() > LIMIT {
                        return Err(Error::protocol("Runtime event chunk exceeds size limit"));
                    }
                    self.queue.extend(self.parser.feed(&bytes)?);
                }
                None => {
                    self.response = None;
                    self.parser.finish()?;
                    return Ok(None);
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn event() -> serde_json::Value {
        serde_json::json!({"sequence":4,"record":{"spec":{"id":"run","session":"s","turn":"t","request":"r","agent":"main","objective":"héllo","depth":0,"parent":null,"parent_run":null,"input":{}},"state":"running","revision":1,"lease":null,"completion":null,"cancel_requested":false}})
    }
    #[test]
    fn split_unicode_crlf_comments_and_multiline() {
        let data = serde_json::to_string_pretty(&event())
            .unwrap()
            .lines()
            .map(|v| format!("data: {v}\r\n"))
            .collect::<String>();
        let bytes = format!(": heartbeat\r\n\r\nid: 4\r\nevent: run\r\n{data}\r\n");
        let mut parser = Parser::default();
        let mut events = Vec::new();
        for byte in bytes.as_bytes() {
            events.extend(parser.feed(&[*byte]).unwrap());
        }
        parser.finish().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].sequence, 4);
    }
    #[test]
    fn invalid_or_truncated_frames_are_not_delivered() {
        let mut parser = Parser::default();
        parser.feed(b"event: run\ndata: {\n").unwrap();
        assert!(parser.finish().is_err());
        assert!(Parser::default()
            .feed(b"event: error\ndata: unavailable\n\n")
            .is_err());
        let bad = format!("event: run\nid: 5\ndata: {}\n\n", event());
        assert!(Parser::default().feed(bad.as_bytes()).is_err());
    }
}
