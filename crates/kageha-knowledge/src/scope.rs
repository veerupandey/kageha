//! Scope resolution is independent of WebUI payloads and database types.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, io};
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub id: String,
    pub generation: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub workspace: String,
    pub agent: String,
    pub shared: Vec<Binding>,
}
pub use kageha_contracts::knowledge::{Config,ids,valid_id};
impl Scope {
    pub fn validate(&self) -> io::Result<()> {
        if self.workspace.is_empty()
            || self.workspace.len() > 4096
            || self.agent.is_empty()
            || self.agent.len() > 128
            || self.workspace.chars().any(char::is_control)
            || self.agent.chars().any(char::is_control)
            || self.shared.len() > 64
        {
            return Err(io::Error::other("Invalid knowledge scope"));
        }
        let mut seen = BTreeSet::new();
        for b in &self.shared {
            if !valid_id(&b.id) || b.generation == 0 || !seen.insert(&b.id) {
                return Err(io::Error::other("Invalid knowledge binding"));
            }
        }
        Ok(())
    }
    pub fn resolve(
        workspace: &str,
        agent: &str,
        config: &Config,
        selected: &[String],
        mut lookup: impl FnMut(&str) -> io::Result<Binding>,
    ) -> io::Result<Self> {
        let mut shared = Vec::new();
        if config.inherit {
            let requested: BTreeSet<_> = selected.iter().chain(&config.extras).collect();
            if requested.len() > 64 {
                return Err(io::Error::other("Too many attached knowledge bases"));
            }
            for id in requested {
                if !valid_id(id) {
                    return Err(io::Error::other("Invalid shared knowledge base"));
                }
                let binding = lookup(id)?;
                if binding.id != *id {
                    return Err(io::Error::other(
                        "Knowledge adapter returned another binding",
                    ));
                }
                shared.push(binding);
            }
        }
        let result = Self {
            workspace: workspace.into(),
            agent: agent.into(),
            shared,
        };
        result.validate()?;
        Ok(result)
    }
    /// Worker definitions cannot widen a delegating run's immutable shared scope.
    pub fn delegate(&self, agent: &str, config: &Config) -> Self {
        Self {
            workspace: self.workspace.clone(),
            agent: agent.into(),
            shared: if config.inherit {
                self.shared.clone()
            } else {
                vec![]
            },
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn inheritance_deduplicates_and_private_mode_never_looks_up_shared_ids() {
        let cfg = Config::parse(&json!({"extra_kb_ids":["notes","extra"]})).unwrap();
        let parent = Scope::resolve("workspace-one", "parent", &cfg, &["notes".into()], |id| {
            Ok(Binding {
                id: id.into(),
                generation: 7,
            })
        })
        .unwrap();
        assert_eq!(parent.shared.len(), 2);
        let child_cfg = Config::parse(&json!({"extra_kb_ids":["unattached"]})).unwrap();
        let child = parent.delegate("child", &child_cfg);
        assert_eq!(child.shared, parent.shared);
        let private =
            Config::parse(&json!({"inherit_workspace":false,"extra_kb_ids":["notes"]})).unwrap();
        assert!(Scope::resolve(
            "workspace-one",
            "private",
            &private,
            &["extra".into()],
            |_| panic!("private mode cannot resolve shared KBs")
        )
        .unwrap()
        .shared
        .is_empty());
        assert!(child.delegate("nested", &private).shared.is_empty());
        let recovered: Scope =
            serde_json::from_str(&serde_json::to_string(&child).unwrap()).unwrap();
        assert_eq!(recovered, child);
    }
    #[test]
    fn malformed_settings_and_private_ids_fail_closed() {
        for v in [
            json!({"inherit_workspace":"false"}),
            json!({"extra_kb_ids":["agent-other"]}),
            json!({"extra_kb_ids":"notes"}),
        ] {
            assert!(Config::parse(&v).is_err());
        }
        assert!(ids(&json!(["../notes"])).is_err());
        assert!(ids(&json!(["notes", "notes"])).unwrap().len() == 1);
    }
}
