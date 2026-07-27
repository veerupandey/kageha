//! Model chains by role (design section 10). An agent's `model_preferences` and a
//! chat's role override name a model, or a list of models tried in order, for each
//! of `executor`, `planner` and `reviewer`. A single string is a chain of one.
use kageha_contracts::execution::Role;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io;

/// A configured model id, or a model name the host resolves to one.
pub type ModelRef = String;

/// Longest chain accepted for one role.
pub const MAX_CHAIN: usize = 8;

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn model_ref(value: &Value) -> Option<&str> {
    value
        .as_str()
        .filter(|s| s.len() <= 256 && !s.chars().any(char::is_control))
}

/// Parses one role value: missing, `null`, `""` or `[]` is no chain; a string is a
/// chain of one; a list holds 1 to [`MAX_CHAIN`] non-empty names.
pub fn chain(value: &Value) -> io::Result<Vec<ModelRef>> {
    match value {
        Value::Null => Ok(Vec::new()),
        Value::String(_) => {
            let name = model_ref(value).ok_or_else(|| invalid("Invalid model name".into()))?;
            Ok(if name.trim().is_empty() { Vec::new() } else { vec![name.into()] })
        }
        Value::Array(items) => {
            if items.len() > MAX_CHAIN {
                return Err(invalid(format!("A model chain holds at most {MAX_CHAIN} models")));
            }
            items
                .iter()
                .map(|item| {
                    model_ref(item)
                        .filter(|name| !name.trim().is_empty())
                        .map(str::to_owned)
                        .ok_or_else(|| invalid("A model chain must list model names".into()))
                })
                .collect()
        }
        _ => Err(invalid("A model role must be a model name or a list of names".into())),
    }
}

/// Checks the role keys of an agent's `model_preferences` object. Other keys
/// (`fast_worker`, `embedding`, host metadata) are left to their hosts.
pub fn validate(preferences: &Value) -> io::Result<()> {
    for role in Role::ALL {
        chain(&preferences[role.as_str()])
            .map_err(|e| invalid(format!("model_preferences.{}: {e}", role.as_str())))?;
    }
    Ok(())
}

/// The model chain for each role of one turn. An empty `executor` chain means the
/// host's default model. An empty `planner` or `reviewer` chain uses the executor's.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleChains {
    #[serde(default)]
    pub executor: Vec<ModelRef>,
    #[serde(default)]
    pub planner: Vec<ModelRef>,
    #[serde(default)]
    pub reviewer: Vec<ModelRef>,
}

impl RoleChains {
    /// For each role, the chat's chain when it names one, else the agent's
    /// `model_preferences` chain, else none (host default or executor chain).
    pub fn resolve(chat: &Value, agent: &Value) -> io::Result<Self> {
        let pick = |role: Role| -> io::Result<Vec<ModelRef>> {
            let key = role.as_str();
            let chat = chain(&chat[key]).map_err(|e| invalid(format!("model_roles.{key}: {e}")))?;
            if !chat.is_empty() {
                return Ok(chat);
            }
            chain(&agent[key]).map_err(|e| invalid(format!("model_preferences.{key}: {e}")))
        };
        Ok(Self {
            executor: pick(Role::Executor)?,
            planner: pick(Role::Planner)?,
            reviewer: pick(Role::Reviewer)?,
        })
    }

    /// The chain set for `role` itself, without the executor fallback.
    pub fn configured(&self, role: Role) -> &[ModelRef] {
        match role {
            Role::Executor => &self.executor,
            Role::Planner => &self.planner,
            Role::Reviewer => &self.reviewer,
        }
    }

    /// The chain a request with `role` uses.
    pub fn chain(&self, role: Role) -> &[ModelRef] {
        match self.configured(role) {
            [] => &self.executor,
            chain => chain,
        }
    }

    /// True when `role` uses other models than the executor. Checks that need a
    /// second opinion (the blocker check, the reviewer) are skipped otherwise.
    pub fn distinct(&self, role: Role) -> bool {
        self.chain(role) != self.executor.as_slice()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn strings_and_lists_parse_as_chains() {
        assert_eq!(chain(&json!("a")).unwrap(), ["a"]);
        assert_eq!(chain(&json!(["a", "b"])).unwrap(), ["a", "b"]);
        for none in [json!(null), json!(""), json!("  "), json!([])] {
            assert!(chain(&none).unwrap().is_empty(), "{none}");
        }
        for bad in [json!(1), json!({}), json!([""]), json!([1]), json!(["a\n"]), json!(vec!["m"; 9])] {
            assert!(chain(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn chat_overrides_agent_per_role_and_missing_roles_use_the_executor() {
        let agent = json!({"executor":["cheap","backup"],"planner":"strong","fast_worker":"fast"});
        let chains = RoleChains::resolve(&json!({"planner":""}), &agent).unwrap();
        assert_eq!(chains.chain(Role::Executor), ["cheap", "backup"]);
        assert_eq!(chains.chain(Role::Planner), ["strong"]);
        assert_eq!(chains.chain(Role::Reviewer), ["cheap", "backup"]);
        assert!(chains.distinct(Role::Planner) && !chains.distinct(Role::Reviewer));
        assert!(chains.configured(Role::Reviewer).is_empty());

        let chat = json!({"executor":"picked","reviewer":["judge","judge-2"]});
        let chains = RoleChains::resolve(&chat, &agent).unwrap();
        assert_eq!(chains.chain(Role::Executor), ["picked"]);
        assert_eq!(chains.chain(Role::Planner), ["strong"]);
        assert_eq!(chains.chain(Role::Reviewer), ["judge", "judge-2"]);

        // Nothing configured: the host default model serves every role.
        let none = RoleChains::resolve(&json!(null), &json!({})).unwrap();
        assert_eq!(none, RoleChains::default());
        assert!(Role::ALL.iter().all(|role| none.chain(*role).is_empty() && !none.distinct(*role)));

        // A planner equal to the executor is not distinct.
        let same = RoleChains::resolve(&json!({}), &json!({"executor":"m","planner":["m"]})).unwrap();
        assert!(!same.distinct(Role::Planner));
        assert!(RoleChains::resolve(&json!({"planner":3}), &agent).is_err());
    }
}
