//! Validate dependencies for a proposed task plan.
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

#[derive(Deserialize)]
pub struct Step {
    id: String,
    depends_on: Vec<String>,
}

pub fn validate(steps: &[Step]) -> Result<(), String> {
    let graph: HashMap<_, _> = steps
        .iter()
        .map(|step| (step.id.as_str(), &step.depends_on))
        .collect();
    if graph.len() != steps.len() {
        return Err("step IDs must be unique; do not repurpose an existing ID".into());
    }
    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    for step in steps {
        let mut stack = vec![(step.id.as_str(), false)];
        while let Some((id, leaving)) = stack.pop() {
            if leaving {
                visiting.remove(id);
                visited.insert(id);
            } else if !visited.contains(id) {
                if !visiting.insert(id) {
                    return Err("step dependencies contain a cycle".into());
                }
                let dependencies = graph
                    .get(id)
                    .ok_or_else(|| format!("unknown dependency: {id}"))?;
                stack.push((id, true));
                stack.extend(dependencies.iter().rev().map(|dep| (dep.as_str(), false)));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn accepts_forward_references_and_rejects_invalid_graphs() {
        for (data, expected) in [
            (
                json!([{"id":"a","depends_on":["b"]},{"id":"b","depends_on":[]}]),
                None,
            ),
            (
                json!([{"id":"a","depends_on":["missing"]}]),
                Some("unknown dependency: missing"),
            ),
            (
                json!([{"id":"a","depends_on":["b"]},{"id":"b","depends_on":["a"]}]),
                Some("step dependencies contain a cycle"),
            ),
            (
                json!([{"id":"a","depends_on":[]},{"id":"a","depends_on":[]}]),
                Some("step IDs must be unique; do not repurpose an existing ID"),
            ),
        ] {
            let steps: Vec<Step> = serde_json::from_value(data).unwrap();
            assert_eq!(validate(&steps).err().as_deref(), expected);
        }
    }
}
