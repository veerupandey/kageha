//! Operator requirements are loaded once, outside application settings and model inputs.
use kageha_contracts::policy::*;
use serde::{Deserialize, Serialize};
use std::{io, path::Path};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Requirements {
    pub require_process_sandbox: bool,
    pub allowed_profiles: Vec<PermissionProfile>,
    pub forced_profile: Option<PermissionProfile>,
    pub denied_actions: Vec<String>,
    pub manual_actions: Vec<String>,
    pub allow_review_override: bool,
    pub consecutive_denials: usize,
    pub rolling_denials: usize,
}
impl Default for Requirements {
    fn default() -> Self {
        Self {
            require_process_sandbox: false,
            allowed_profiles: vec![
                PermissionProfile::Ask,
                PermissionProfile::AutoReview,
                PermissionProfile::Full,
            ],
            forced_profile: None,
            denied_actions: vec![],
            manual_actions: vec![],
            allow_review_override: true,
            consecutive_denials: 3,
            rolling_denials: 10,
        }
    }
}
impl Requirements {
    pub fn load(path: &Path) -> io::Result<Self> {
        use std::io::Read;
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take(65537)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 65536 {
            return Err(io::Error::other("Requirements exceed limit"));
        }
        let rules: Self = serde_json::from_slice(&bytes)
            .map_err(|_| io::Error::other("Invalid policy requirements"))?;
        rules.validate()?;
        Ok(rules)
    }
    pub fn validate(&self) -> io::Result<()> {
        if self
            .allowed_profiles
            .iter()
            .enumerate()
            .any(|(i, p)| self.allowed_profiles[..i].contains(p))
            || self.allowed_profiles.is_empty()
            || self.allowed_profiles.len() > 3
            || !self
                .allowed_profiles
                .contains(&self.forced_profile.unwrap_or(PermissionProfile::Ask))
            || self.consecutive_denials == 0
            || self.consecutive_denials > 3
            || self.rolling_denials == 0
            || self.rolling_denials > 10
            || self.denied_actions.len() > 256
            || self.manual_actions.len() > 256
            || self
                .denied_actions
                .iter()
                .chain(&self.manual_actions)
                .any(|s| s.is_empty() || s.len() > 256 || s.chars().any(char::is_control))
        {
            return Err(io::Error::other("Invalid managed policy requirements"));
        }
        Ok(())
    }
    pub fn restricted(&self) -> bool {
        self.forced_profile.is_some()
            || self.allowed_profiles.len() != 3
            || !self.denied_actions.is_empty()
            || !self.manual_actions.is_empty()
    }
}
