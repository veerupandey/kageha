use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Deserialize)]
pub struct DispatchRequest {
    version: u32,
    max_parallel: usize,
    calls: Vec<Call>,
}

#[derive(Deserialize)]
struct Call {
    exclusive: bool,
    resource: String,
}

#[derive(Serialize)]
pub struct DispatchPlan {
    version: u32,
    waves: Vec<Vec<usize>>,
}

/// Preserve input order across barriers and conflicting calls. Calls in one
/// wave may run together. An exclusive call occupies a wave by itself.
pub fn plan_dispatch(request: DispatchRequest) -> Result<DispatchPlan, &'static str> {
    if request.version != 1 || request.max_parallel == 0 {
        return Err("unsupported dispatch version or concurrency limit");
    }
    let mut waves = Vec::new();
    let mut wave = Vec::new();
    let mut resources = HashSet::new();
    for (index, call) in request.calls.iter().enumerate() {
        if !wave.is_empty()
            && (call.exclusive
                || wave.len() == request.max_parallel
                || resources.contains(call.resource.as_str()))
        {
            waves.push(std::mem::take(&mut wave));
            resources.clear();
        }
        if call.exclusive {
            waves.push(vec![index]);
        } else {
            wave.push(index);
            if !call.resource.is_empty() {
                resources.insert(call.resource.as_str());
            }
        }
    }
    if !wave.is_empty() {
        waves.push(wave);
    }
    Ok(DispatchPlan { version: 1, waves })
}
