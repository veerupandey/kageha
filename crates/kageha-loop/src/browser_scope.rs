//! Caller-pinned browser task limits. These are not editable by model tools.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io;
#[derive(Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Scope {
    read_only_tabs: Vec<String>,
    blocked_control_names: Vec<String>,
}
impl Scope {
    pub(crate) fn parse(value: &Value) -> io::Result<Self> {
        let scope: Self = if value.is_null() {
            Self::default()
        } else {
            serde_json::from_value(value.clone())
                .map_err(|_| io::Error::other("Invalid pinned browser scope"))?
        };
        for (values, max) in [
            (&scope.read_only_tabs, 128),
            (&scope.blocked_control_names, 180),
        ] {
            if values.len() > 32
                || values
                    .iter()
                    .any(|v| v.is_empty() || v.len() > max || v.chars().any(char::is_control))
            {
                return Err(io::Error::other("Invalid pinned browser scope"));
            }
        }
        Ok(scope)
    }
    pub(crate) fn enforce(
        &self,
        tool: &str,
        target_id: &str,
        control_name: Option<&str>,
    ) -> io::Result<()> {
        let input = matches!(
            tool,
            "browser_click"
                | "browser_fill"
                | "browser_press"
                | "browser_reload"
                | "browser_navigate"
                | "browser_close"
                | "browser_select"
                | "browser_check"
                | "browser_upload"
                | "browser_fill_credential"
                | "browser_fill_email_code"
        );
        if input && self.read_only_tabs.iter().any(|id| id == target_id) {
            return Err(io::Error::other("browser_task_scope_read_only: preserve this tab; open a separate managed tab for recovery"));
        }
        if matches!(tool, "browser_click" | "browser_press")
            && !self.blocked_control_names.is_empty()
            && control_name.is_none()
        {
            return Err(io::Error::other("browser_task_control_identity_required: use the exact observed control reference so the caller's excluded controls can be checked"));
        }
        if matches!(tool, "browser_click" | "browser_press")
            && control_name.is_some_and(|name| {
                self.blocked_control_names
                    .iter()
                    .any(|blocked| blocked.eq_ignore_ascii_case(name.trim()))
            })
        {
            return Err(io::Error::other(
                "browser_task_control_blocked: this control is excluded by the caller's task scope",
            ));
        }
        Ok(())
    }
}
pub(crate) fn validate(value: &Value) -> io::Result<Value> {
    serde_json::to_value(Scope::parse(value)?).map_err(io::Error::other)
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn read_only_draft_and_final_submit_are_fenced_before_input() {
        let scope=Scope::parse(&json!({"read_only_tabs":["original"],"blocked_control_names":["Submit","Submit Application","Withdraw"]})).unwrap();
        assert!(scope
            .enforce("browser_click", "original", Some("Candidate Home"))
            .is_err());
        for tool in ["browser_navigate", "browser_close"] {
            assert!(scope.enforce(tool, "original", None).is_err());
            assert!(scope.enforce(tool, "recovery", None).is_ok());
        }
        assert!(scope
            .enforce("browser_fill", "original", Some("First Name"))
            .is_err());
        assert!(scope.enforce("browser_fill_email_code", "original", Some("Verification Code")).is_err());
        assert!(scope.enforce("browser_fill_email_code", "recovery", Some("Verification Code")).is_ok());
        assert!(scope.enforce("browser_snapshot", "original", None).is_ok());
        assert!(scope.enforce("browser_open", "original", None).is_ok());
        assert!(scope.enforce("browser_tabs", "original", None).is_ok());
        assert!(scope
            .enforce("browser_click", "recovery", Some("Submit"))
            .is_err());
        assert!(scope
            .enforce("browser_click", "recovery", Some("Save and Continue"))
            .is_ok());
        assert!(scope
            .enforce("browser_click", "recovery", Some("Candidate Home"))
            .is_ok());
    }
    #[test]
    fn unnamed_targets_cannot_bypass_excluded_controls() {
        let scope = Scope::parse(&json!({"blocked_control_names":["Submit"]})).unwrap();
        assert!(scope.enforce("browser_click", "draft", None).is_err());
        assert!(scope.enforce("browser_press", "draft", None).is_err());
        assert!(scope
            .enforce("browser_click", "draft", Some("Save and Continue"))
            .is_ok());
        assert!(Scope::default()
            .enforce("browser_click", "draft", None)
            .is_ok());
    }
    #[test]
    fn malformed_scopes_fail_closed() {
        for value in [
            json!({"read_only_tabs":"original"}),
            json!({"ignored":true}),
            json!({"read_only_tabs":[""]}),
        ] {
            assert!(Scope::parse(&value).is_err());
        }
    }
}
