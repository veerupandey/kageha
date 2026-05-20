//! Portable knowledge-selection configuration; no storage or host dependencies.
use serde_json::Value;
use std::{collections::BTreeSet, io};
#[derive(Clone, Debug)]
pub struct Config {
    pub inherit: bool,
    pub extras: Vec<String>,
}
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 96
        && id.as_bytes()[0].is_ascii_alphanumeric()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && !id.starts_with("agent-")
        && !id.starts_with("agent_")
        && !matches!(id, "agent_memory" | "memory_v2_index")
        && !id.starts_with("memory_")
}
impl Config {
    pub fn parse(value: &Value) -> io::Result<Self> {
        if value.is_null() {
            return Ok(Self {
                inherit: true,
                extras: vec![],
            });
        }
        if !value.is_object() {
            return Err(io::Error::other(
                "Knowledge configuration must be an object",
            ));
        }
        let inherit = match value.get("inherit_workspace") {
            None => true,
            Some(v) => v
                .as_bool()
                .ok_or_else(|| io::Error::other("inherit_workspace must be boolean"))?,
        };
        let extras = ids(value.get("extra_kb_ids").unwrap_or(&Value::Null))?;
        Ok(Self { inherit, extras })
    }
}
pub fn ids(value: &Value) -> io::Result<Vec<String>> {
    if value.is_null() {
        return Ok(vec![]);
    }
    let rows = value
        .as_array()
        .filter(|v| v.len() <= 64)
        .ok_or_else(|| io::Error::other("Select at most 64 workspace knowledge bases"))?;
    let mut result = BTreeSet::new();
    for row in rows {
        let id = row
            .as_str()
            .filter(|s| valid_id(s))
            .ok_or_else(|| io::Error::other("Invalid or private knowledge base selection"))?;
        result.insert(id.to_owned());
    }
    Ok(result.into_iter().collect())
}
