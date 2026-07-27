//! Portable specialist configuration. No application database, runtime, UI,
//! browser or provider client is imported here. Settings are data, not grants.
use kageha_contracts::agents::valid_id;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io};
fn invalid(s: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, s)
}
pub fn hash(v: &Value) -> String {
    format!("{:x}", Sha256::digest(v.to_string().as_bytes()))
}
fn text<'a>(v: &'a Value, key: &str, max: usize) -> io::Result<&'a str> {
    v[key]
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.len() <= max && !s.contains('\0'))
        .ok_or_else(|| invalid(&format!("Invalid {key}")))
}
/// Validate retained agent/team documents without erasing unknown metadata.
/// Runtime-owned ancestry cannot be supplied by a settings editor.
pub fn validate(value: &Value, team: bool) -> io::Result<()> {
    if !value.is_object() || value.to_string().len() > 256 * 1024 {
        return Err(invalid("Agent definition exceeds limits"));
    }
    let id = text(value, "id", 80)?;
    let team_id = !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'));
    if if team { !team_id } else { !valid_id(id) } {
        return Err(invalid("Invalid agent/team id"));
    }
    text(value, "name", 200)?;
    if value.get("system_prompt").is_some_and(|v| {
        !v.is_null()
            && v.as_str()
                .is_none_or(|s| s.len() > 65536 || s.contains('\0'))
    }) {
        return Err(invalid("Instructions must be text within 64 KiB"));
    }
    if team {
        let members = value["agents"]
            .as_array()
            .filter(|a| !a.is_empty() && a.len() <= 8)
            .ok_or_else(|| invalid("A team needs one to eight agent ids"))?;
        if members.iter().any(|id| !id.as_str().is_some_and(valid_id)) {
            return Err(invalid("Invalid team member identity"));
        }
    }
    if let Some(skills) = value.get("pinned_skills") {
        let skills = skills
            .as_array()
            .filter(|v| v.len() <= 256)
            .ok_or_else(|| invalid("pinned_skills must be a bounded array"))?;
        if skills.iter().any(|v| {
            !v.as_str().is_some_and(|s| {
                !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control)
            })
        }) {
            return Err(invalid("Pinned skills must contain names"));
        }
    }
    for field in ["tool_policy", "model_preferences", "knowledge"] {
        if value
            .get(field)
            .is_some_and(|v| !v.is_object() && !v.is_null())
        {
            return Err(invalid(&format!("{field} must be an object")));
        }
    }
    model_roles::validate(&value["model_preferences"])?;
    if value
        .get("skills_restricted_to_pinned")
        .is_some_and(|v| !v.is_boolean())
    {
        return Err(invalid("skills_restricted_to_pinned must be boolean"));
    }
    kageha_policy::specialist::validate(&value["tool_policy"])?;
    if value.get("_inherited_tool_policies").is_some() {
        return Err(invalid("Policy ancestry is runtime-owned"));
    }
    kageha_contracts::knowledge::Config::parse(&value["knowledge"])?;
    Ok(())
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(try_from = "Value", into = "Value")]
pub struct Definition {
    body: Value,
}
impl TryFrom<Value> for Definition {
    type Error = io::Error;
    fn try_from(value: Value) -> io::Result<Self> {
        Self::parse(value)
    }
}
impl From<Definition> for Value {
    fn from(value: Definition) -> Value {
        value.body
    }
}
impl Definition {
    pub fn parse(body: Value) -> io::Result<Self> {
        validate(&body, false)?;
        Ok(Self { body })
    }
    pub fn id(&self) -> &str {
        self.body["id"].as_str().unwrap()
    }
    pub fn body(&self) -> &Value {
        &self.body
    }
    pub fn fingerprint(&self) -> String {
        hash(&self.body)
    }
    pub fn instructions(&self) -> &str {
        self.body["system_prompt"].as_str().unwrap_or("")
    }
    pub fn policy(&self) -> &Value {
        &self.body["tool_policy"]
    }
    pub fn effective(&self) -> Value {
        json!({"system_prompt":self.body["system_prompt"],"pinned_skills":self.body.get("pinned_skills").cloned().unwrap_or(json!([])),"skills_restricted_to_pinned":self.body["skills_restricted_to_pinned"]==true,"tool_policy":self.body.get("tool_policy").cloned().unwrap_or(json!({"allow":[],"deny":[]})),"model_preferences":self.body.get("model_preferences").cloned().unwrap_or(json!({})),"memory_scope":format!("persona:{}",self.id()),"knowledge":self.body["knowledge"]})
    }
    /// Persisted metadata and activation support are separate. Hosts advertise
    /// which executable settings they can actually apply, and reject the rest.
    pub fn unsupported(&self, supported: &Support) -> Vec<String> {
        let mut result = Vec::new();
        if !supported.skills
            && (self.body["pinned_skills"]
                .as_array()
                .is_some_and(|v| !v.is_empty())
                || self.body["skills_restricted_to_pinned"] == true)
        {
            result.push("skills".into());
        }
        if !supported.models
            && self.body["model_preferences"]
                .as_object()
                .is_some_and(|v| !v.is_empty())
        {
            result.push("model_preferences".into());
        }
        if !supported.knowledge {
            let c = kageha_contracts::knowledge::Config::parse(&self.body["knowledge"])
                .expect("validated knowledge configuration");
            if !c.inherit || !c.extras.is_empty() {
                result.push("knowledge".into());
            }
        }
        result
    }
    pub fn require(&self, supported: &Support) -> io::Result<()> {
        let missing = self.unsupported(supported);
        if !missing.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "This host cannot activate these specialist settings: {}",
                    missing.join(", ")
                ),
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub struct Support {
    pub skills: bool,
    pub models: bool,
    pub knowledge: bool,
}
/// Implementations may be local configuration or a managed service. The caller
/// must pin fingerprints and fence changes; this port does not grant authority.
pub trait Port: Send + Sync {
    fn get(&self, id: &str) -> io::Result<Option<Definition>>;
    fn list(&self) -> io::Result<Vec<Definition>>;
}
pub struct Catalog {
    definitions: BTreeMap<String, Definition>,
}
impl Catalog {
    pub fn new(definitions: Vec<Definition>) -> io::Result<Self> {
        if definitions.len() > 128 {
            return Err(invalid("Too many specialist definitions"));
        }
        let mut result = BTreeMap::new();
        for definition in definitions {
            if result
                .insert(definition.id().to_owned(), definition)
                .is_some()
            {
                return Err(invalid("Duplicate specialist identity"));
            }
        }
        Ok(Self {
            definitions: result,
        })
    }
}
impl Port for Catalog {
    fn get(&self, id: &str) -> io::Result<Option<Definition>> {
        Ok(self.definitions.get(id).cloned())
    }
    fn list(&self) -> io::Result<Vec<Definition>> {
        Ok(self.definitions.values().cloned().collect())
    }
}
/// Defensive boundary around adapters: a profile lookup cannot return another
/// identity, and a catalog cannot hide duplicates or unbounded data.
pub fn get(port: &dyn Port, id: &str) -> io::Result<Option<Definition>> {
    let value = port.get(id)?;
    if value.as_ref().is_some_and(|d| d.id() != id) {
        return Err(invalid("Profile adapter returned another identity"));
    }
    Ok(value)
}
pub fn list(port: &dyn Port) -> io::Result<Vec<Definition>> {
    let values = port.list()?;
    Catalog::new(values.clone())?;
    for value in &values {
        if get(port, value.id())?.as_ref().map(Definition::fingerprint) != Some(value.fingerprint())
        {
            return Err(invalid(
                "Profile adapter changed during catalog observation",
            ));
        }
    }
    Ok(values)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profiles_preserve_metadata_and_expose_activation_limits() {
        let body = json!({"id":"research.ai","name":"Research","system_prompt":"Retain constraints","tool_policy":{"allow":["read_*"],"deny":["read_secret"]},"knowledge":{"inherit_workspace":false,"extra_kb_ids":[]},"pinned_skills":["review"],"model_preferences":{"default":"model-ref"},"metadata":{"original_id":"old"}});
        let d = Definition::parse(body.clone()).unwrap();
        assert_eq!(d.body(), &body);
        assert_eq!(d.effective()["memory_scope"], "persona:research.ai");
        assert_eq!(d.unsupported(&Support::default()).len(), 3);
        assert!(d.require(&Support::default()).is_err());
        assert!(d
            .require(&Support {
                skills: true,
                models: true,
                knowledge: true
            })
            .is_ok());
        let round: Definition = serde_json::from_value(serde_json::to_value(&d).unwrap()).unwrap();
        assert_eq!(round.fingerprint(), d.fingerprint());
    }
    #[test]
    fn malformed_settings_and_forged_ancestry_are_rejected() {
        for patch in [
            json!({"system_prompt":12}),
            json!({"pinned_skills":[{}]}),
            json!({"tool_policy":{"deny":"all"}}),
            json!({"knowledge":{"extra_kb_ids":["agent-private"]}}),
            json!({"_inherited_tool_policies":[]}),
            json!({"model_preferences":{"planner":7}}),
            json!({"model_preferences":{"reviewer":["ok",""]}}),
        ] {
            let mut body = json!({"id":"a","name":"A"});
            for (k, v) in patch.as_object().unwrap() {
                body[k] = v.clone();
            }
            assert!(Definition::parse(body).is_err());
        }
        assert!(validate(&json!({"id":"team","name":"T","agents":["a.b"]}), true).is_ok());
        let chains = json!({"id":"a","name":"A","model_preferences":{
            "executor":["cheap","backup"],"planner":"strong","reviewer":["judge"],"fast_worker":"fast"}});
        assert!(Definition::parse(chains).is_ok());
    }
    struct Wrong;
    impl Port for Wrong {
        fn get(&self, _: &str) -> io::Result<Option<Definition>> {
            Ok(Some(
                Definition::parse(json!({"id":"other","name":"Other"})).unwrap(),
            ))
        }
        fn list(&self) -> io::Result<Vec<Definition>> {
            Ok(vec![])
        }
    }
    #[test]
    fn adapter_identity_and_duplicate_catalogs_fail_closed() {
        assert!(get(&Wrong, "caller").is_err());
        let d = Definition::parse(json!({"id":"a","name":"A"})).unwrap();
        assert!(Catalog::new(vec![d.clone(), d]).is_err());
    }
}

pub mod model_roles;
pub mod repository;
