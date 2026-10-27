//! Minimal framework-owned configuration for provider-confidence routing.
use super::*;
pub const DEFAULT_CONFIDENCE_CUTOFF: f64 = 0.90;
pub const DEFAULT_BATCH_CONFIDENCE_CUTOFF: f64 = 0.99;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionConfig {
    pub decision_model: String,
    #[serde(default)]
    pub confidence_cutoff: Option<f64>,
    #[serde(default)]
    pub batch_confidence_cutoff: Option<f64>,
}
impl DecisionConfig {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            decision_model: model.into(),
            confidence_cutoff: None,
            batch_confidence_cutoff: None,
        }
    }
    /// Apply a host-owned context override without replacing unrelated budgets/review policy.
    pub fn apply_to(&self, base: &Policy) -> io::Result<Policy> {
        if self.decision_model.trim().is_empty() {
            return Err(io::Error::other("Decision model is required"));
        }
        let mut policy = base.clone();
        let cutoff = self
            .confidence_cutoff
            .or_else(|| {
                base.overrides
                    .get("browser_action")
                    .and_then(|p| p.confidence_cutoff)
            })
            .or(base.confidence_cutoff)
            .unwrap_or(DEFAULT_CONFIDENCE_CUTOFF);
        if let Some(value) = self.batch_confidence_cutoff {
            policy.batch_confidence_cutoff = value;
        }
        let action = policy.overrides.entry("browser_action".into()).or_default();
        action.model = Some(self.decision_model.clone());
        action.confidence_cutoff = Some(cutoff);
        action.mode = Some(Mode::Active);
        policy.validate()?;
        Ok(policy)
    }
    pub fn policy(&self) -> io::Result<Policy> {
        if self.decision_model.trim().is_empty() {
            return Err(io::Error::other("Decision model is required"));
        }
        let mut policy = Policy {
            model: self.decision_model.clone(),
            confidence_cutoff: Some(self.confidence_cutoff.unwrap_or(DEFAULT_CONFIDENCE_CUTOFF)),
            batch_confidence_cutoff: self
                .batch_confidence_cutoff
                .unwrap_or(DEFAULT_BATCH_CONFIDENCE_CUTOFF),
            ..Policy::default()
        };
        policy.overrides.insert(
            "browser_action".into(),
            Override {
                mode: Some(Mode::Active),
                ..Override::default()
            },
        );
        policy.validate()?;
        Ok(policy)
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DecisionOverrides {
    pub decision_model: Option<String>,
    pub confidence_cutoff: Option<f64>,
}
impl Session {
    pub fn action_settings(&self) -> Effective {
        self.policy.effective(Purpose::Action)
    }
    pub fn action_confidence_cutoff(&self) -> Option<f64> {
        self.policy
            .overrides
            .get("browser_action")
            .and_then(|p| p.confidence_cutoff)
            .or(self.policy.confidence_cutoff)
    }
    pub fn from_decision_config(
        scope: Scope,
        config: &DecisionConfig,
        backends: typed::Backends,
    ) -> io::Result<Self> {
        Ok(Self::new(scope, config.policy()?, Models::new())?.with_decision_backends(backends))
    }
}
