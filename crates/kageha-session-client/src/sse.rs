use serde_json::Value;
use std::{
    collections::VecDeque,
    io::{self, Read},
};
#[derive(Debug)]
pub struct Packet {
    pub event: String,
    pub data: Value,
}
#[derive(Default)]
struct Parser {
    line: Vec<u8>,
    data: Vec<String>,
    event: String,
    size: usize,
    cr: bool,
}
impl Parser {
    fn feed(&mut self, bytes: &[u8]) -> io::Result<Vec<Packet>> {
        let mut packets = Vec::new();
        for &byte in bytes {
            if self.cr {
                self.cr = false;
                if byte == b'\n' {
                    continue;
                }
            }
            self.size += 1;
            if self.size > 8 * 1024 * 1024 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Session event exceeds 8 MiB",
                ));
            }
            if byte == b'\n' || byte == b'\r' {
                self.cr = byte == b'\r';
                let line = String::from_utf8(std::mem::take(&mut self.line)).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "Invalid event UTF-8")
                })?;
                if line.is_empty() {
                    if !self.data.is_empty() {
                        packets.push(Packet {
                            event: std::mem::take(&mut self.event),
                            data: serde_json::from_str(&self.data.join("\n")).map_err(|_| {
                                io::Error::new(io::ErrorKind::InvalidData, "Invalid event JSON")
                            })?,
                        });
                    }
                    self.data.clear();
                    self.event.clear();
                    self.size = 0;
                } else if !line.starts_with(':') {
                    let (field, value) = line.split_once(':').unwrap_or((&line, ""));
                    let value = value.strip_prefix(' ').unwrap_or(value);
                    match field {
                        "event" => self.event = value.into(),
                        "data" => self.data.push(value.into()),
                        _ => {}
                    }
                }
            } else {
                self.line.push(byte);
            }
        }
        Ok(packets)
    }
}
pub struct Feed {
    response: reqwest::blocking::Response,
    parser: Parser,
    queue: VecDeque<Packet>,
    session: String,
    last: u64,
}
impl Feed {
    pub(crate) fn new(response: reqwest::blocking::Response, session: String, last: u64) -> Self {
        Self {
            response,
            parser: Parser::default(),
            queue: Default::default(),
            session,
            last,
        }
    }
    pub fn last_sequence(&self) -> u64 {
        self.last
    }
    pub fn next_packet(&mut self) -> io::Result<Packet> {
        loop {
            if let Some(mut packet) = self.queue.pop_front() {
                if packet.event == "snapshot" {
                    if packet.data["session_id"] != self.session {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "Wrong session snapshot",
                        ));
                    }
                    return Ok(packet);
                }
                if packet.event != "update" {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Unknown session event",
                    ));
                }
                let frames = packet.data["events"].as_array_mut().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "Missing session events")
                })?;
                let mut fresh = Vec::new();
                for frame in frames.drain(..) {
                    let sequence = frame["sequence"]
                        .as_u64()
                        .filter(|n| *n <= i64::MAX as u64)
                        .ok_or_else(|| {
                            io::Error::new(io::ErrorKind::InvalidData, "Invalid event cursor")
                        })?;
                    if sequence > self.last {
                        self.last = sequence;
                        fresh.push(frame)
                    }
                }
                *frames = fresh;
                if !frames.is_empty() {
                    return Ok(packet);
                }
                continue;
            }
            let mut buffer = [0u8; 8192];
            let count = self.response.read(&mut buffer)?;
            if count == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Session feed ended; reconnect from the delivered cursor",
                ));
            }
            self.queue.extend(self.parser.feed(&buffer[..count])?);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn split_unicode_crlf_comments_and_multiline() {
        let bytes=": heartbeat\r\nevent: update\r\ndata: {\"events\":\r\ndata: [{\"text\":\"東京\",\"sequence\":7}]}\r\n\r\n";
        let mut p = Parser::default();
        let mut out = Vec::new();
        for b in bytes.as_bytes() {
            out.extend(p.feed(&[*b]).unwrap())
        }
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].data["events"][0]["text"], "東京");
    }
    #[test]
    fn malformed_and_oversized_events_fail() {
        assert!(Parser::default().feed(b"data: nope\n\n").is_err());
        assert!(Parser::default()
            .feed(&vec![b'x'; 8 * 1024 * 1024 + 1])
            .is_err());
    }
}
