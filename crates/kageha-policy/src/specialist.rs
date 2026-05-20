//! Portable, restrictive specialist policy. Independent of hosting and storage.
use serde_json::Value;
use std::io;
fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "Invalid specialist tool policy",
    )
}
pub fn validate(policy: &Value) -> io::Result<()> {
    if policy.is_null() {
        return Ok(());
    }
    let map = policy.as_object().ok_or_else(invalid)?;
    for key in ["allow", "deny"] {
        if let Some(value) = map.get(key).filter(|v| !v.is_null()) {
            let rows = value
                .as_array()
                .filter(|r| r.len() <= 512)
                .ok_or_else(invalid)?;
            for row in rows {
                let s = row.as_str().ok_or_else(invalid)?;
                if s.is_empty() || s.len() > 256 || s.chars().any(char::is_control) {
                    return Err(invalid());
                }
            }
        }
    }
    if let Some(value) = map.get("risk_class_overrides").filter(|v| !v.is_null()) {
        let rows = value
            .as_object()
            .filter(|r| r.len() <= 512)
            .ok_or_else(invalid)?;
        for (name, risk) in rows {
            let risk = risk.as_str().ok_or_else(invalid)?;
            if name.is_empty()
                || name.len() > 256
                || name.chars().any(char::is_control)
                || risk.trim().is_empty()
                || risk.len() > 64
                || risk.chars().any(char::is_control)
            {
                return Err(invalid());
            }
        }
    }
    Ok(())
}
fn rank(risk: &str) -> usize {
    match risk.trim().to_ascii_lowercase().as_str() {
        "safe" => 0,
        "low" => 1,
        "medium" => 2,
        "high" => 3,
        "critical" => 4,
        _ => 5,
    }
}
/// Unknown/categorical risk values retain maximum severity, as in the reference.
pub fn raise<'a>(base: &'a str, requested: &'a str) -> &'a str {
    if !requested.trim().is_empty() && rank(requested) > rank(base) {
        requested
    } else {
        base
    }
}
#[derive(Debug, PartialEq, Eq)]
pub struct Decision {
    pub allowed: bool,
    pub review: bool,
}
/// Each allowlist is an independent constraint, never a union that widens authority.
pub fn evaluate(policies: &[Value], tool: &str) -> io::Result<Decision> {
    let mut allowed = true;
    let mut risk = "safe";
    for policy in policies {
        validate(policy)?;
        if policy["deny"]
            .as_array()
            .is_some_and(|v| v.iter().any(|p| glob(p.as_str().unwrap(), tool)))
        {
            allowed = false;
        }
        if policy["allow"]
            .as_array()
            .is_some_and(|v| !v.is_empty() && !v.iter().any(|p| glob(p.as_str().unwrap(), tool)))
        {
            allowed = false;
        }
        if let Some(value) = policy["risk_class_overrides"][tool].as_str() {
            risk = raise(risk, value);
        }
    }
    Ok(Decision {
        allowed,
        review: rank(risk) > 0,
    })
}
/// Capture restrictions into the child's durable run configuration, including ancestors.
pub fn inherit(child: &mut Value, parent: &Value) -> io::Result<()> {
    let mut policies = policies(parent)?;
    // An imported definition cannot override host-owned ancestry.
    child
        .as_object_mut()
        .ok_or_else(invalid)?
        .remove("_inherited_tool_policies");
    if policies.len() > 3 {
        return Err(invalid());
    }
    policies.retain(|p| !p.is_null());
    child["_inherited_tool_policies"] = Value::Array(policies);
    Ok(())
}
pub fn policies(persona: &Value) -> io::Result<Vec<Value>> {
    let mut result = match persona.get("_inherited_tool_policies") {
        None => vec![],
        Some(v) => v
            .as_array()
            .filter(|v| v.len() <= 3)
            .cloned()
            .ok_or_else(invalid)?,
    };
    result.push(persona["tool_policy"].clone());
    for p in &result {
        validate(p)?;
    }
    Ok(result)
}
fn glob(pattern: &str, value: &str) -> bool {
    let (p, v) = (pattern.as_bytes(), value.as_bytes());
    let (mut i, mut j, mut star, mut resume) = (0, 0, None, 0);
    while j < v.len() {
        if i < p.len() && (p[i] == b'?' || p[i] == v[j]) {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == b'*' {
            star = Some(i);
            i += 1;
            resume = j;
        } else if let Some(mark) = star {
            resume += 1;
            j = resume;
            i = mark + 1;
        } else {
            return false;
        }
    }
    while i < p.len() && p[i] == b'*' {
        i += 1;
    }
    i == p.len()
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn raises_never_lower_and_unknown_is_conservative() {
        assert_eq!(raise("high", "low"), "high");
        assert_eq!(raise("browser", "critical"), "browser");
        assert_eq!(raise("safe", "unknown"), "unknown");
        assert_eq!(raise("safe", " HIGH "), " HIGH ");
    }
    #[test]
    fn inherited_denials_allowlists_and_risk_survive_serialization() {
        let parent = json!({"tool_policy":{"allow":["read_*","agent_spawn"],"deny":["read_secret"],"risk_class_overrides":{"read_file":"high"}}});
        let mut child =
            json!({"tool_policy":{"allow":["*"],"risk_class_overrides":{"read_file":"safe"}}});
        inherit(&mut child, &parent).unwrap();
        let recovered: Value = serde_json::from_str(&child.to_string()).unwrap();
        let p = policies(&recovered).unwrap();
        assert_eq!(
            evaluate(&p, "read_file").unwrap(),
            Decision {
                allowed: true,
                review: true
            }
        );
        for name in ["read_secret", "bash"] {
            assert!(!evaluate(&p, name).unwrap().allowed);
        }
        let mut grandchild = json!({});
        inherit(&mut grandchild, &recovered).unwrap();
        assert!(
            evaluate(&policies(&grandchild).unwrap(), "read_file")
                .unwrap()
                .review
        );
    }
    #[test]
    fn invalid_policy_fails_closed() {
        for p in [
            json!({"allow":[4]}),
            json!({"risk_class_overrides":{"read_file":true}}),
            json!({"risk_class_overrides":{"read_file":""}}),
        ] {
            assert!(validate(&p).is_err());
        }
        assert!(
            evaluate(
                &[json!({"risk_class_overrides":{"read_file":"vendor-risk"}})],
                "read_file"
            )
            .unwrap()
            .review
        );
        assert!(
            !evaluate(&[json!({"deny":["*"]})], "read_file")
                .unwrap()
                .allowed
        );
    }
}
