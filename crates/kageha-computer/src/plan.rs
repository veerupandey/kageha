//! Convert public tool steps into a bounded native plan before approval/dispatch.
use serde_json::{json, Map, Value};

fn text<'a>(step: &'a Value, name: &str) -> Result<&'a str, String> {
    step.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("batch {name} must be text"))
}
fn number(step: &Value, name: &str, default: Option<f64>) -> Result<f64, String> {
    let value = match step.get(name) {
        Some(v) => v.as_f64(),
        None => default,
    };
    value
        .filter(|n| n.is_finite())
        .ok_or_else(|| format!("batch {name} must be finite"))
}
pub fn prepare(input: &Value) -> Result<Value, String> {
    let steps = input
        .get("steps")
        .and_then(Value::as_array)
        .ok_or("batch steps required")?;
    if steps.is_empty() || steps.len() > 24 {
        return Err("batch needs 1..24 steps".into());
    }
    let pid = input
        .get("pid")
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
        .ok_or("positive pid required")?;
    let wid = input
        .get("window_id")
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
        .ok_or("positive window id required")?;
    let refs = input
        .get("refs")
        .and_then(Value::as_object)
        .ok_or("observed refs required")?;
    let labels = input.get("labels").and_then(Value::as_object);
    let mut plan = Vec::new();
    for step in steps {
        let object = step.as_object().ok_or("batch step must be an object")?;
        let action = text(step, "action")?;
        let fields: &[&str] = match action {
            "click" => &["action", "ref", "x", "y", "button", "count"],
            "set_value" => &["action", "ref", "value"],
            "type" => &["action", "ref", "text"],
            "key" => &["action", "ref", "key"],
            "scroll" => &["action", "direction", "amount"],
            "drag" => &["action", "from_x", "from_y", "to_x", "to_y"],
            _ => return Err("unsupported batch action".into()),
        };
        if object.keys().any(|k| !fields.contains(&k.as_str())) {
            return Err(format!("unsupported fields for batch {action}"));
        }
        let mut args: Map<String, Value> =
            serde_json::from_value(json!({"pid":pid,"window_id":wid})).unwrap();
        let reference = step
            .get("ref")
            .map(|v| v.as_str().ok_or("ref must be text"))
            .transpose()?
            .unwrap_or("");
        let label = if !reference.is_empty() {
            let index = reference
                .strip_prefix('e')
                .and_then(|s| s.parse::<u64>().ok())
                .ok_or("invalid batch ref")?;
            if refs.get(reference).and_then(Value::as_u64) != Some(index) {
                return Err("batch ref is absent from the current observation".into());
            }
            args.insert("element_index".into(), json!(index));
            labels
                .and_then(|m| m.get(reference))
                .and_then(Value::as_str)
                .unwrap_or("")
        } else {
            ""
        };
        let method = match action {
            "click" => {
                if !reference.is_empty() && (object.contains_key("x") || object.contains_key("y")) {
                    return Err("click needs a ref or coordinates, not both".into());
                }
                if reference.is_empty() {
                    for name in ["x", "y"] {
                        let n = number(step, name, None)?;
                        if n < 0.0 {
                            return Err("window coordinates must be nonnegative".into());
                        }
                        args.insert(name.into(), json!(n));
                    }
                }
                let button = step
                    .get("button")
                    .map(|_| text(step, "button"))
                    .transpose()?
                    .unwrap_or("left");
                let count = number(step, "count", Some(1.0))?;
                if !["left", "right"].contains(&button) || ![1.0, 2.0, 3.0].contains(&count) {
                    return Err("invalid batch click button/count".into());
                }
                if !reference.is_empty() && count != 1.0 {
                    return Err("multiple clicks require coordinates".into());
                }
                args.insert("button".into(), json!(button));
                args.insert("count".into(), json!(count as u64));
                "click"
            }
            "set_value" => {
                if reference.is_empty() {
                    return Err("set_value requires an observed ref".into());
                }
                args.insert("value".into(), json!(text(step, "value")?));
                "set_value"
            }
            "type" => {
                args.insert("text".into(), json!(text(step, "text")?));
                "type_text"
            }
            "key" => {
                let k = text(step, "key")?.trim().to_lowercase();
                if k.is_empty() {
                    return Err("key required".into());
                }
                args.insert(
                    "key".into(),
                    json!(if k == "enter" { "return" } else { &k }),
                );
                "press_key"
            }
            "scroll" => {
                let d = step
                    .get("direction")
                    .map(|_| text(step, "direction"))
                    .transpose()?
                    .unwrap_or("down");
                let n = number(step, "amount", Some(3.0))?;
                if !["up", "down", "left", "right"].contains(&d)
                    || !(1.0..=100.0).contains(&n)
                    || n.fract() != 0.0
                {
                    return Err("invalid batch scroll direction/amount".into());
                }
                args.insert("direction".into(), json!(d));
                args.insert("amount".into(), json!(n as u64));
                "scroll"
            }
            "drag" => {
                for name in ["from_x", "from_y", "to_x", "to_y"] {
                    let n = number(step, name, None)?;
                    if n < 0.0 {
                        return Err("coordinates must be nonnegative".into());
                    }
                    args.insert(name.into(), json!(n));
                }
                "drag"
            }
            _ => unreachable!(),
        };
        let mut detail = format!("{action} {reference} {label}");
        if action == "key" {
            detail.push_str(&format!(" key {}", args["key"].as_str().unwrap()));
        }
        plan.push(json!({"action":action,"method":method,"args":args,"detail":detail}));
    }
    Ok(json!({"plan":plan}))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn input(steps: Value) -> Value {
        json!({"pid":7,"window_id":9,"refs":{"e2":2},"labels":{"e2":"Send"},"steps":steps})
    }
    #[test]
    fn maps_observed_refs_and_approval_labels() {
        let p = prepare(&input(json!([{"action":"click","ref":"e2"}]))).unwrap();
        assert_eq!(p["plan"][0]["args"]["element_index"], 2);
        assert!(p["plan"][0]["detail"].as_str().unwrap().contains("Send"));
    }
    #[test]
    fn rejects_unobserved_ref() {
        assert!(prepare(&input(json!([{"action":"click","ref":"e9"}]))).is_err());
    }
    #[test]
    fn preflights_later_bad_field() {
        assert!(prepare(&input(
            json!([{"action":"type","text":"hello"},{"action":"set_value","value":"missing ref"}])
        ))
        .is_err());
    }
    #[test]
    fn normalizes_enter() {
        assert_eq!(
            prepare(&input(json!([{"action":"key","key":"Enter"}]))).unwrap()["plan"][0]["args"]
                ["key"],
            "return"
        );
    }
    #[test]
    fn rejects_legacy_pacing() {
        assert!(prepare(&input(json!([{"action":"type","text":"x","delay_ms":20}]))).is_err());
    }
    #[test]
    fn rejects_mixed_click_addressing() {
        assert!(prepare(&input(json!([{"action":"click","ref":"e2","x":1,"y":2}]))).is_err());
    }
    #[test]
    fn direct_values_no_key_synthesis() {
        assert_eq!(
            prepare(&input(
                json!([{"action":"set_value","ref":"e2","value":"test"}])
            ))
            .unwrap()["plan"][0]["method"],
            "set_value"
        );
    }
}
