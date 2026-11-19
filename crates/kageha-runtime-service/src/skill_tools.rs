//! Read the exact active skill package admitted for this run.
use kageha_contracts::{execution::*, policy::ActionClass};
use kageha_skills::registry::Registry;
use serde_json::{json, Value};
use std::{collections::BTreeMap, io, path::PathBuf};

pub(crate) const READ: &str = "read_skill_resource";

pub(crate) struct Tools {
    registry: PathBuf,
    pins: BTreeMap<String, (u64, String)>,
}

impl Tools {
    pub(crate) fn new(registry: PathBuf, pin: &Value) -> io::Result<Option<Self>> {
        let Some(values) = pin.as_array() else {
            return Ok(None);
        };
        let rows: Vec<(String, u64, String)> = serde_json::from_value(json!(values))?;
        let pins: BTreeMap<_, _> = rows
            .into_iter()
            .map(|(name, revision, digest)| (name, (revision, digest)))
            .collect();
        Ok((!pins.is_empty()).then_some(Self { registry, pins }))
    }

    fn target<'a>(&self, call: &'a ToolCall) -> io::Result<(&'a str, &'a str, u64, &str)> {
        let invalid = || {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Expected a pinned skill and a relative resource path",
            )
        };
        let args = call
            .arguments
            .as_object()
            .filter(|args| args.len() == 2)
            .ok_or_else(invalid)?;
        let name = args
            .get("skill")
            .and_then(Value::as_str)
            .ok_or_else(invalid)?;
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .filter(|path| {
                path.len() <= 512
                    && !path.contains('\\')
                    && !path.chars().any(char::is_control)
                    && path.split('/').all(|part| !matches!(part, "" | "." | ".."))
            })
            .ok_or_else(invalid)?;
        let (revision, digest) = self.pins.get(name).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Skill is outside the run's pinned scope",
            )
        })?;
        Ok((name, path, *revision, digest))
    }
}

impl ToolPort for Tools {
    fn discover(&self) -> io::Result<Vec<Value>> {
        Ok(vec![json!({"type":"function","function":{
            "name":READ,"description":"Read a UTF-8 resource from an admitted skill package.",
            "parameters":{"type":"object","properties":{
                "skill":{"type":"string","enum":self.pins.keys().collect::<Vec<_>>()},
                "path":{"type":"string","description":"Path relative to the skill package"}
            },"required":["skill","path"],"additionalProperties":false}
        }})])
    }
    fn binding(&self, _: &ToolContext, call: &ToolCall) -> io::Result<ToolBinding> {
        let (name, path, revision, digest) = self.target(call)?;
        Ok(ToolBinding {
            binding: "runtime.skills".into(),
            generation: revision,
            resource: format!("skills/{name}/{path}"),
            class: ActionClass::ReadOnly,
            detail: "Read a pinned skill resource".into(),
            fingerprint: digest.into(),
        })
    }
    fn dispatch_access_in(&self, _: &ToolContext, _: &ToolCall) -> io::Result<DispatchAccess> {
        Ok(DispatchAccess::Resource(String::new()))
    }
    fn execute(&self, call: &ToolCall, control: &Control<'_>) -> io::Result<ToolReply> {
        control.check()?;
        let (name, path, revision, digest) = self.target(call)?;
        crate::activation::require_file(&self.registry)?;
        let bytes =
            Registry::open(&self.registry)?.pinned_resource(name, path, revision, digest)?;
        control.check()?;
        if bytes.len() > 65536 {
            return Err(io::Error::other(
                "Skill resource exceeds the text output limit",
            ));
        }
        let text =
            String::from_utf8(bytes).map_err(|_| io::Error::other("Skill resource is binary"))?;
        Ok(ToolReply::success(text))
    }
}
