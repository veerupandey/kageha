use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

fn nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

pub const SCHEMA: &str = include_str!("../protocol/schema.json");
static CONTRACT: LazyLock<Value> = LazyLock::new(|| serde_json::from_str(SCHEMA).unwrap());

pub fn version() -> u64 {
    CONTRACT["x-protocol-version"].as_u64().unwrap()
}

pub fn limit(name: &str) -> usize {
    CONTRACT["x-limits"][name].as_u64().unwrap() as usize
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Authority {
    pub generation: u64,
    pub grant_id: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub protocol_version: u64,
    pub request_id: String,
    pub instance_id: String,
    pub application_id: String,
    pub workspace_id: String,
    pub client_id: String,
    pub state_root: String,
    #[serde(deserialize_with = "nullable")]
    pub service_epoch: Option<String>,
    pub deadline_ms: u64,
    #[serde(deserialize_with = "nullable")]
    pub expected_binding: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub authority: Option<Authority>,
    pub method: String,
    pub params: Value,
}

impl Request {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.protocol_version != version() {
            return Err("protocol_version_mismatch");
        }
        for id in [
            &self.request_id,
            &self.instance_id,
            &self.application_id,
            &self.workspace_id,
            &self.client_id,
        ] {
            if id.is_empty() || id.len() > limit("id_bytes") || id.chars().any(char::is_control) {
                return Err("invalid_request");
            }
        }
        if self.method.is_empty() || self.method.len() > 64 || !self.params.is_object() {
            return Err("invalid_request");
        }
        let now = now_ms();
        if self.deadline_ms <= now {
            return Err("deadline_exceeded");
        }
        let deadline_limit = if self.method == "credential_vault_authorize"
            && self.workspace_id == "settings" && self.client_id == "native-user-settings" {
            300_000
        } else {
            limit("deadline_ms") as u64
        };
        if self.deadline_ms > now.saturating_add(deadline_limit) {
            return Err("invalid_deadline");
        }
        // Control operations validate grants under the dispatch lock.
        if (self.authority.is_some()
            && !crate::actions::METHODS.contains(&self.method.as_str())
            && self.method != "panel_action"
            && self.method != "run_script"
            && self.method != "release_control")
            || self.expected_binding.is_some()
        {
            return Err("browser_execution_unavailable");
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reply {
    pub protocol_version: u64,
    pub request_id: String,
    pub instance_id: String,
    pub application_id: String,
    pub workspace_id: String,
    pub client_id: String,
    pub service_epoch: String,
    pub status: String,
    pub execution_phase: String,
    pub result: Value,
    #[serde(skip)]
    pub binary: Vec<u8>,
    #[serde(deserialize_with = "nullable")]
    pub error: Option<Value>,
}

impl Reply {
    pub fn new(request: Option<&Request>, instance: &str, application: &str, epoch: &str) -> Self {
        Self {
            protocol_version: version(),
            request_id: request.map(|r| r.request_id.clone()).unwrap_or_default(),
            workspace_id: request.map(|r| r.workspace_id.clone()).unwrap_or_default(),
            client_id: request.map(|r| r.client_id.clone()).unwrap_or_default(),
            instance_id: instance.to_owned(),
            application_id: application.to_owned(),
            service_epoch: epoch.to_owned(),
            status: "ok".into(),
            execution_phase: "not_dispatched".into(),
            result: json!({}),
            binary: Vec::new(),
            error: None,
        }
    }

    pub fn reject(mut self, code: &str) -> Self {
        self.status = "error".into();
        self.error =
            Some(json!({"code": code, "message": describe(code), "retryable": false}));
        self
    }
}

/// The readable message for an error code, with a recovery hint where the
/// caller can act on one. The code stays the stable identifier.
pub fn describe(code: &str) -> String {
    let plain = code.replace('_', " ");
    match hint(code) {
        Some(hint) => format!("{plain}. {hint}"),
        None => plain,
    }
}

/// What a caller should do next. Hints name no host tool, because MCP and HTTP
/// clients use this runtime directly. They never suggest repeating input.
pub fn hint(code: &str) -> Option<&'static str> {
    Some(match code {
        "tab_capacity_exceeded" => "The managed tab limit is 32. List managed tabs, select an existing tab, navigate a reusable tab, or explicitly close a completed tab. Do not repeat an open until capacity changes.",
        "tab_close_unconfirmed" => "Closure was not confirmed. List managed tabs to check the exact target before doing anything else. Do not repeat the close automatically.",
        "cdp_disconnected" => "The debugging connection was lost; this does not prove the tab closed or crashed. Read browser selection and observe the original managed tab. Do not open a replacement or repeat uncertain input.",
        "target_not_owned" => "The target is not managed by this chat or no longer exists. List this chat's managed tabs and use an exact returned target ID.",
        "cdp_unavailable" | "browser_not_launched" | "browser_exited" => "Read the current browser selection and connection state. Reconnect the requested profile if needed; do not substitute another profile or open duplicate tabs to recover an existing session.",
        "browser_binding_changed" => "The browser, tab or document changed. Read current selection before continuing and obtain fresh approval for a changed target. Do not replay uncertain input.",
        "observation_stale" | "observation_expired" => "Take a fresh observation of the original managed tab before further input; old references cannot authorize an action.",
        "invalid_key" => "Send one key or a chord joined by +. Keys: Enter, Tab, Escape, Space, Backspace, Delete, Home, End, PageUp, PageDown, ArrowUp, ArrowDown, ArrowLeft, ArrowRight, or a single letter, digit or punctuation character. Modifiers: Alt, Control, Meta, Shift, each at most once and case-sensitive, for example Control+A. Function keys such as F5 and aliases such as cmd or ctrl are not supported, and key presses do not trigger browser shortcuts such as reload.",
        "invalid_locator" => "The target is empty, longer than 512 characters, or neither an observed ref such as e12 nor a CSS selector that parses. Use a ref or selector from the latest observation or inspection.",
        "invalid_params" => "A parameter is missing, unknown, empty or the wrong type. Check the method schema and correct the arguments; the same arguments will fail again.",
        "select_option_unavailable" => "No enabled option matches the requested value or visible label, or the control is not a native select. Read the select's options and pass one exactly. For a custom dropdown, click it open and click the option.",
        "element_not_actionable" => "The element exists but cannot receive input now: it may be disabled, hidden, covered by an overlay, or have no clickable position. Observe again, dismiss any overlay or scroll, and use a fresh ref.",
        "confirmation_source_missing" => "The original submission has no matching saved pre-submission identity in this document. Keep the original receipt; do not submit again. Use independent work or report the missing source evidence.",
        "confirmation_document_changed" => "The original Workday submission document was replaced. This completion URL cannot identify the submitted job independently. Preserve the site result and original receipt, report the formal proof limitation once, and stop this confirmation route. Do not retry confirmation or submit again.",
        "confirmation_identity_missing" | "submission_identity_changed" => "The job, employer, account, profile or source document does not match the original submission. Read the exact original application; never substitute a resume link or resubmit.",
        "postcondition_unobserved" => "The recorded or supported affirmative site confirmation is absent. A pending receipt is preserved. Do not repeat submission; a changed reference or diagnostic report is not new proof.",
        "confirmation_disabled" | "confirmation_provider_unsupported" => "Independent confirmation is unavailable for this provider or conservative runtime. Keep the original receipt and report the unresolved evidence.",
        "numeric_input_invalid" => "Use a finite HTML number, such as 3.00 or 1e2, or an empty string to clear the field. This action was not dispatched. Do not round or clamp a requested value.",
        "input_readonly" => "The actual input is read-only. Continue independent work or wait for a real field-state change; refreshing its reference does not make it editable.",
        "input_disabled" => "The actual input is disabled or inert. Resolve the prerequisite before filling it.",
        "unsupported_input_type" => "This field needs a supported action for its native input type. Repeating a text fill cannot resolve this.",
        "journal_storage_full" | "journal_storage_unavailable" | "journal_corrupt" | "journal_migration_failed" => "Action history storage is unavailable. Recorded input is paused globally; page reads and receipt lookup remain available. Do not retry with another input method or tab. Wait for healthy input_status before continuing.",
        "element_not_editable" => "The element is not an editable text field. Observe again and fill the real input, which may differ from its visible label.",
        "element_identity_changed" => "The element behind that ref no longer has the role or name that was observed. Observe again and use the new ref.",
        "element_not_observed_in_main_frame" => "That element was not in the observed page content. It may be in a frame or a closed shadow root. Observe again, and pass the frame_id of the frame that contains the control.",
        "frame_scope_unavailable" => "The requested frame is gone or not reachable. Observe again and use a current frame_id.",
        "observation_too_large" => "The result exceeded the size limit. Narrow it with a specific selector and a small limit instead of reading the whole page.",
        "input_postcondition_changed" => "The page changed between observing the control and sending input. Observe again before trying again.",
        "document_changed" => "The page navigated or reloaded after the last observation. Observe again before acting.",
        "action_control_busy" => "Another action or a person has control of this browser. Wait briefly and try again; if it persists, look for a pending action or a user takeover.",
        "action_receipt_pending" => "An earlier input on this tab has no confirmed outcome. Do not repeat it. Look up its receipt and reconcile it before sending new input.",
        "action_outcome_uncertain" => "The input may have been delivered but could not be verified. Do not repeat it. Observe the page and reconcile the receipt to learn the real state.",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_requests_round_trip_without_losing_fields() {
        let fixtures: Value =
            serde_json::from_str(include_str!("../protocol/fixtures.json")).unwrap();
        for original in fixtures["Request"].as_array().unwrap() {
            let request: Request = serde_json::from_value(original.clone()).unwrap();
            assert_eq!(serde_json::to_value(request).unwrap(), *original);
        }
        for original in fixtures["Reply"].as_array().unwrap() {
            let reply: Reply = serde_json::from_value(original.clone()).unwrap();
            assert_eq!(serde_json::to_value(reply).unwrap(), *original);
        }
    }

    #[test]
    fn validation_distinguishes_expired_and_excessive_deadlines() {
        let fixtures: Value =
            serde_json::from_str(include_str!("../protocol/fixtures.json")).unwrap();
        let mut request: Request = serde_json::from_value(fixtures["Request"][0].clone()).unwrap();
        request.deadline_ms = now_ms() - 1;
        assert_eq!(request.validate(), Err("deadline_exceeded"));
        request.deadline_ms = now_ms() + limit("deadline_ms") as u64 + 1000;
        assert_eq!(request.validate(), Err("invalid_deadline"));
        request.deadline_ms = now_ms() + 1000;
        assert_eq!(request.validate(), Ok(()));
        request.authority = Some(Authority {
            generation: 1,
            grant_id: "untrusted".into(),
        });
        assert_eq!(request.validate(), Err("browser_execution_unavailable"));
    }

    #[test]
    fn only_the_user_keychain_action_gets_a_longer_deadline() {
        let fixtures: Value = serde_json::from_str(include_str!("../protocol/fixtures.json")).unwrap();
        let mut request: Request = serde_json::from_value(fixtures["Request"][0].clone()).unwrap();
        request.deadline_ms = now_ms() + 120_000;
        request.method = "credential_vault_authorize".into();
        request.workspace_id = "settings".into();
        request.client_id = "native-agent-settings".into();
        assert_eq!(request.validate(), Err("invalid_deadline"));
        request.client_id = "native-user-settings".into();
        assert_eq!(request.validate(), Ok(()));
        request.deadline_ms = now_ms() + 301_000;
        assert_eq!(request.validate(), Err("invalid_deadline"));
        request.deadline_ms = now_ms() + 120_000;
        request.method = "browser_snapshot".into();
        assert_eq!(request.validate(), Err("invalid_deadline"));
    }

    #[test]
    fn hints_are_bounded_and_never_suggest_replaying_input() {
        let codes = [
            "invalid_key", "invalid_locator", "invalid_params", "select_option_unavailable",
            "element_not_actionable", "element_not_editable", "element_identity_changed",
            "element_not_observed_in_main_frame", "frame_scope_unavailable", "observation_too_large",
            "input_postcondition_changed", "document_changed", "action_control_busy",
            "action_receipt_pending", "action_outcome_uncertain",
            "tab_capacity_exceeded", "tab_close_unconfirmed", "cdp_disconnected", "target_not_owned",
            "cdp_unavailable", "browser_binding_changed", "observation_stale", "observation_expired",
        ];
        for code in codes {
            let message = describe(code);
            assert!(message.starts_with(&code.replace('_', " ")), "{code}");
            assert!(message.len() <= 1024, "{code}: {}", message.len());
            assert!(!message.to_lowercase().contains("retry the same"), "{code}");
        }
        for code in ["action_receipt_pending", "action_outcome_uncertain"] {
            assert!(describe(code).contains("Do not repeat"), "{code}");
        }
        // Codes without a hint keep their plain message.
        assert_eq!(describe("unsupported_method"), "unsupported method");
    }

    #[test]
    fn the_key_hint_names_only_keys_the_parser_accepts() {
        let hint = hint("invalid_key").unwrap();
        for key in ["Enter", "Tab", "Escape", "Space", "Backspace", "Delete", "Home", "End", "PageUp",
            "PageDown", "ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "Control+A", "Shift+Tab"] {
            assert!(crate::keyboard::Key::parse(key).is_ok(), "{key}");
        }
        for modifier in ["Alt", "Control", "Meta", "Shift"] {
            assert!(hint.contains(modifier), "{modifier}");
            assert!(crate::keyboard::Key::parse(&format!("{modifier}+a")).is_ok(), "{modifier}");
        }
        // The cases the hint says are unsupported really are rejected.
        for key in ["F5", "cmd+r", "ctrl+a", "control+a", "Return"] {
            assert!(crate::keyboard::Key::parse(key).is_err(), "{key}");
        }
    }
}
