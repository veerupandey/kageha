//! Assemble indexed tool call fragments from normalized provider deltas.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

#[derive(Deserialize)]
pub struct AssembleRequest {
    version: u32,
    fragments: Vec<Fragment>,
}

#[derive(Deserialize)]
struct Fragment {
    index: i64,
    id: Option<String>,
    name: Option<String>,
    arguments: String,
    meta: Map<String, Value>,
}

#[derive(Default, Serialize)]
struct ToolSlot {
    id: String,
    name: String,
    arguments: String,
    meta: Map<String, Value>,
}

#[derive(Serialize)]
pub struct AssembleReply {
    version: u32,
    slots: Vec<ToolSlot>,
}

pub fn assemble_tools(request: AssembleRequest) -> Result<AssembleReply, &'static str> {
    if request.version != 1 {
        return Err("unsupported model stream version");
    }
    let mut slots = BTreeMap::<i64, ToolSlot>::new();
    for fragment in request.fragments {
        let slot = slots.entry(fragment.index).or_default();
        if let Some(id) = fragment.id.filter(|id| !id.is_empty()) {
            slot.id = id;
        }
        if let Some(name) = fragment.name.filter(|name| !name.is_empty()) {
            slot.name = name;
        }
        slot.arguments.push_str(&fragment.arguments);
        slot.meta.extend(fragment.meta);
    }
    Ok(AssembleReply {
        version: 1,
        slots: slots.into_values().collect(),
    })
}
