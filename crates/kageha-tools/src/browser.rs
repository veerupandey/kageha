//! Host-neutral adapter over the existing browser local protocol. No UI or chat DB.
use kageha_browser_client::{target, Client};
use kageha_contracts::{execution::*, policy::ActionClass};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{io, path::PathBuf};
#[derive(Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub state_dir: PathBuf,
}
pub struct BrowserTools {
    config: Config,
}
impl BrowserTools {
    pub fn new(config: Config) -> io::Result<Self> {
        if !config.state_dir.is_absolute() {
            return Err(io::Error::other("Browser state_dir must be absolute"));
        }
        kageha_browser_client::private(&config.state_dir)?;
        Ok(Self {
            config: Config {
                state_dir: config.state_dir.canonicalize()?,
            },
        })
    }
    fn client(&self, context: &ToolContext) -> io::Result<Client> {
        if context.session.is_empty() || context.turn.is_empty() {
            return Err(io::Error::other("Browser scope is required"));
        }
        // Independent runtimes cannot accidentally share Kageha's chat workspace.
        let workspace = format!("runtime-{:x}", Sha256::digest(context.session.as_bytes()));
        Client::connect_to(&workspace, "agent", self.config.state_dir.clone(), false)
    }
    fn validate(call: &ToolCall) -> io::Result<()> {
        call.validate()?;
        let spec = specs()
            .into_iter()
            .find(|s| s["function"]["name"] == call.name)
            .ok_or_else(|| io::Error::other("Browser tool is not exposed"))?;
        let schema = &spec["function"]["parameters"];
        for (key, value) in call.arguments.as_object().unwrap() {
            let rule = &schema["properties"][key];
            if rule.is_null() {
                return Err(io::Error::other("Unknown browser argument"));
            }
            let valid = match rule["type"].as_str() {
                Some("string") => value
                    .as_str()
                    .is_some_and(|s| (key == "text" || !s.is_empty()) && s.len() <= 65536),
                Some("boolean") => value.is_boolean(),
                _ if call.name == "browser_select" && key == "option" => {
                    selection_value(value).is_ok()
                }
                _ => {
                    value.as_str().is_some_and(|s| s.len() <= 65536)
                        || value.as_array().is_some_and(|a| {
                            a.len() <= 1000
                                && a.iter()
                                    .all(|v| v.as_str().is_some_and(|s| s.len() <= 65536))
                        })
                }
            };
            if !valid {
                return Err(io::Error::other("Invalid browser argument type"));
            }
        }
        for key in schema["required"].as_array().unwrap() {
            if call.arguments.get(key.as_str().unwrap()).is_none() {
                return Err(io::Error::other("Required browser argument missing"));
            }
        }
        if call.name == "browser_click" {
            validate_click_expectation(&call.arguments)?;
        }
        if let Some(url) = call.arguments["url"].as_str() {
            if !(url.starts_with("https://") || url.starts_with("http://"))
                || url.chars().any(char::is_control)
            {
                return Err(io::Error::other("Browser URL must use HTTP(S)"));
            }
        }
        Ok(())
    }
    fn selected_binding(
        &self,
        call: &ToolCall,
        client: &mut Client,
        selection: &Value,
    ) -> ToolBinding {
        let selected = selection["tabs"].as_array().and_then(|tabs| {
            tabs.iter()
                .find(|t| t["target_id"] == selection["target_id"])
        });
        let resource = json!({"epoch":client.epoch,"selection":target(selection),"url":selected.map(|t|t["url"].clone()).unwrap_or(Value::Null)});
        let digest = format!(
            "{:x}",
            Sha256::digest(
                json!([
                    resource,
                    client.workspace_id(),
                    client.root,
                    call.name,
                    call.arguments
                ])
                .to_string()
                .as_bytes()
            )
        );
        let generation = u64::from_str_radix(
            // Approval APIs are also consumed by JavaScript; keep this marker exact.
            // The complete epoch remains in the resource and configuration digest.
            &format!("{:x}", Sha256::digest(client.epoch.as_bytes()))[..12],
            16,
        )
        .unwrap()
        .max(1);
        let class = if matches!(
            call.name.as_str(),
            "browser_snapshot" | "browser_receipts" | "browser_receipt"
        ) || call.name == "browser_tabs" && call.arguments["tab_handle"].is_null()
        {
            ActionClass::ReadOnly
        } else {
            ActionClass::Mutation
        };
        ToolBinding {
            binding: "browser.workspace".into(),
            generation,
            resource: resource.to_string(),
            class,
            detail: format!("{} on the exact selected browser tab", call.name),
            fingerprint: digest,
        }
    }
    fn run(
        &self,
        context: &ToolContext,
        call: &ToolCall,
        expected: &ToolBinding,
        control: &Control<'_>,
    ) -> io::Result<ToolReply> {
        control.check()?;
        Self::validate(call)?;
        let mut client = self.client(context)?;
        let selection = client.call_controlled(
            "browser_selection",
            json!({}),
            None,
            &kageha_browser_client::unique_id(),
            control,
        )?;
        if self.selected_binding(call, &mut client, &selection) != *expected {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Browser authority changed; obtain fresh approval",
            ));
        }
        let attempt = format!(
            "tool-{:x}",
            Sha256::digest(
                json!([context.session, context.turn, call.id, expected])
                    .to_string()
                    .as_bytes()
            )
        );
        let args = &call.arguments;
        let mut params = target(&selection);
        params.as_object_mut().unwrap().remove("binding_id");
        let (method, authority) = match call.name.as_str() {
            "browser_tabs" if args["tab_handle"].is_null() => {
                return Ok(ToolReply::success(selection.to_string()))
            }
            "browser_snapshot" => {
                params["compact"] = json!(false);
                ("observe_tab", None)
            }
            "browser_receipts" => ("pending_actions", None),
            "browser_receipt" => {
                params = json!({"request_id":args["request_id"]});
                ("action_receipt", None)
            }
            "browser_tabs" => {
                if !selection["tabs"]
                    .as_array()
                    .is_some_and(|tabs| tabs.iter().any(|t| t["target_id"] == args["tab_handle"]))
                {
                    return Err(io::Error::other("Select a managed tab from browser_tabs"));
                }
                params = target(&selection);
                params["target_id"] = args["tab_handle"].clone();
                ("select_tab", None)
            }
            "browser_open" => {
                if selection["browser_id"].as_str().unwrap_or("").is_empty() {
                    if selection["profile"] != "private" {
                        return Err(io::Error::other(
                            "The selected signed-in profile is disconnected. Ask the user to reconnect it in the browser panel; do not substitute a private profile.",
                        ));
                    }
                    client.call_controlled(
                        "switch_profile",
                        json!({"profile":"private","binding_id":selection["binding_id"]}),
                        None,
                        &format!("{attempt}-connect"),
                        control,
                    )?;
                    let selected = client.selection()?;
                    params["browser_id"] = selected["browser_id"].clone();
                }
                params.as_object_mut().unwrap().remove("target_id");
                params["url"] = args["url"].clone();
                ("open_tab", None)
            }
            _ => {
                params["observation_id"] = args["observation_id"].clone();
                params["target"] = args["target"].clone();
                let grant = client.call_controlled(
                    "claim_control",
                    json!({"ttl_ms":10000}),
                    None,
                    &format!("{attempt}-claim"),
                    control,
                )?;
                let (method, native) = observed_action(
                    &call.name,
                    args,
                    &selection,
                    args["observation_id"].as_str().unwrap_or(""),
                    args["target"].as_str().unwrap_or(""),
                    args["frame_id"].as_str(),
                )?;
                params = native;
                (method, Some(grant["authority"].clone()))
            }
        };
        let result = client.call_controlled(method, params, authority.as_ref(), &attempt, control).and_then(|value|{
            if method=="open_tab" {
                let selection=client.selection()?;
                client.call_controlled("select_tab",json!({"binding_id":selection["binding_id"],"browser_id":value["browser_id"],"target_id":value["target_id"]}),None,&format!("{attempt}-select"),control)?;
            }
            Ok(value)
        });
        if let Some(authority) = authority {
            let _ = client.call("release_control", json!({}), Some(&authority));
        }
        match result {
            Ok(value) => Ok(ToolReply::success(
                json!({"request_id":attempt,"result":value}).to_string(),
            )),
            Err(e) if expected.class != ActionClass::ReadOnly => {
                // A confirmed rejection did not dispatch input; a lost transport may have.
                let rejection = serde_json::from_str::<Value>(&e.to_string()).ok();
                if rejection
                    .as_ref()
                    .is_some_and(|v| v["execution_phase"] == "not_dispatched")
                {
                    Ok(ToolReply {
                        outcome: None,
                        is_error: true,
                        content: json!({"request_id":attempt,"error":rejection}).to_string(),
                    })
                } else {
                    Err(uncertain_stop(format!("Browser attempt {attempt} may have dispatched; inspect browser_receipt before continuing")))
                }
            }
            Err(e) => {
                let rejection = serde_json::from_str::<Value>(&e.to_string()).ok();
                if rejection
                    .as_ref()
                    .is_some_and(|v| v["execution_phase"] == "not_dispatched")
                {
                    Ok(ToolReply {
                        outcome: None,
                        is_error: true,
                        content: json!({"request_id":attempt,"error":rejection}).to_string(),
                    })
                } else {
                    Err(e)
                }
            }
        }
    }
}
impl ToolPort for BrowserTools {
    fn discover(&self) -> io::Result<Vec<Value>> {
        Ok(specs())
    }
    fn binding(&self, c: &ToolContext, call: &ToolCall) -> io::Result<ToolBinding> {
        Self::validate(call)?;
        let mut client = self.client(c)?;
        let selection = client.selection()?;
        Ok(self.selected_binding(call, &mut client, &selection))
    }
    fn execute(&self, _: &ToolCall, _: &Control<'_>) -> io::Result<ToolReply> {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Scoped browser authorization is required",
        ))
    }
    fn execute_in(
        &self,
        c: &ToolContext,
        call: &ToolCall,
        control: &Control<'_>,
    ) -> io::Result<ToolReply> {
        let binding = self.binding(c, call)?;
        if binding.class != ActionClass::ReadOnly {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Browser mutations require exact authorization",
            ));
        }
        self.run(c, call, &binding, control)
    }
    fn execute_authorized_in(
        &self,
        c: &ToolContext,
        call: &ToolCall,
        b: &ToolBinding,
        control: &Control<'_>,
    ) -> io::Result<ToolReply> {
        self.run(c, call, b, control)
    }
}
pub fn specs() -> Vec<Value> {
    let s = || json!({"type":"string"});
    let mut result = vec![];
    for (name, description, properties, required) in [
        (
            "browser_open",
            "Open HTTP(S) in a managed tab of the operator-selected profile. When an existing session matters, inspect browser_tabs and reuse a relevant managed tab. Verify the site session; a profile name does not prove sign-in. Respect the operator-selected profile. If a needed personal profile is unavailable, request a browser panel handoff; never attach it without consent or silently substitute private browsing for a requested personal profile.",
            json!({"url":s()}),
            vec!["url"],
        ),
        (
            "browser_tabs",
            "List managed tabs and the selected profile when an existing session matters. A profile name does not prove the site is signed in; verify the site session. tab_handle selects a managed tab without navigation. Ask the operator to select or connect the needed signed-in profile in the browser panel if unavailable.",
            json!({"tab_handle":s()}),
            vec![],
        ),
        (
            "browser_snapshot",
            "Observe the selected tab with full controls. Never reuse stale refs.",
            json!({}),
            vec![],
        ),
        (
            "browser_receipts",
            "Inspect pending input; never repeat uncertain actions.",
            json!({}),
            vec![],
        ),
        (
            "browser_receipt",
            "Inspect the exact prior attempt without replaying it.",
            json!({"request_id":s()}),
            vec!["request_id"],
        ),
    ] {
        result.push(json!({"type":"function","function":{"name":name,"description":description,"parameters":{"type":"object","properties":properties,"required":required,"additionalProperties":false}}}));
    }
    for (name, key, rule) in [
        ("browser_fill", "text", s()),
        ("browser_press", "key", s()),
        ("browser_check", "checked", json!({"type":"boolean"})),
        (
            "browser_select",
            "option",
            json!({"oneOf":[s(),{"type":"integer","minimum":0,"maximum":9007199254740991u64},{"type":"array","items":{"oneOf":[s(),{"type":"integer","minimum":0,"maximum":9007199254740991u64}]}}]}),
        ),
        ("browser_click", "expected_selector", s()),
    ] {
        let mut props = json!({"target":s(),"observation_id":s(),"frame_id":s()});
        props[key] = rule;
        let mut required = vec!["target", "observation_id"];
        if name == "browser_press" {
            // Native keyboard input acts on the focused control in the observation.
            // Do not accept a target argument that cannot be enforced on the wire.
            props.as_object_mut().unwrap().remove("target");
            required = vec!["observation_id"];
        }
        if name == "browser_click" {
            props["expected_text"] = s();
            props["expected_url"] = s();
        } else {
            required.push(key);
        }
        result.push(json!({"type":"function","function":{"name":name,"description":"Act on a fresh observed control with exact approval. A receipt is not task completion.","parameters":{"type":"object","properties":props,"required":required,"additionalProperties":false}}}));
    }
    result
}

/// Reject incompatible click expectations before requesting input approval.
pub fn validate_click_expectation(args: &Value) -> io::Result<()> {
    let selector = args["expected_selector"].as_str().unwrap_or_default();
    let text = args["expected_text"].as_str().unwrap_or_default();
    if !text.is_empty() && selector.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected_text requires expected_selector. Supply both, use expected_url alone for navigation, or omit expectations. No click was sent.",
        ));
    }
    if selector.len() > 512 || text.len() > 512 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Click expectation selector and text must be at most 512 bytes each. No click was sent.",
        ));
    }
    Ok(())
}

/// Shared native argument projection. Hosts retain ownership, approval, observed
/// identity refresh and focused-control checks; they do not invent another wire mapping.
// HTML option values are strings. Models sometimes emit an integer for a
// numeric value despite the schema; convert it without parsing strings (which
// would incorrectly strip leading zeroes). The browser still checks that the
// resulting value uniquely exists in the observed select before dispatch.
fn selection_value(value: &Value) -> io::Result<Value> {
    fn one(value: &Value) -> io::Result<Value> {
        match value {
            Value::String(text) if text.len() <= 512 && !text.contains('\0') => Ok(value.clone()),
            Value::Number(number) if number.as_u64().is_some_and(|n| n <= 9_007_199_254_740_991) => Ok(json!(number.to_string())),
            _ => Err(io::Error::other("Select option must be a string or integer")),
        }
    }
    match value {
        Value::Array(values) if values.len() <= 64 => {
            let values = values.iter().map(one).collect::<io::Result<Vec<_>>>()?;
            let unique = values.iter().filter_map(Value::as_str).collect::<std::collections::BTreeSet<_>>();
            if unique.len() != values.len() { return Err(io::Error::other("Select option values must be unique")); }
            Ok(Value::Array(values))
        }
        Value::Array(_) => Err(io::Error::other("Too many select option values")),
        _ => one(value),
    }
}

pub fn observed_action(
    name: &str,
    args: &Value,
    selection: &Value,
    observation: &str,
    target_ref: &str,
    frame: Option<&str>,
) -> io::Result<(&'static str, Value)> {
    let mut params = json!({"browser_id":selection["browser_id"],"target_id":selection["target_id"],"observation_id":observation,"target":target_ref});
    if let Some(frame) = frame {
        params["frame_id"] = json!(frame);
    }
    let method = match name {
        "browser_click" => {
            for key in ["expected_selector", "expected_text", "expected_url"] {
                if !args[key].is_null() {
                    params[key] = args[key].clone();
                }
            }
            "click_observed"
        }
        "browser_fill" => {
            params["text"] = args["text"].clone();
            "fill_observed"
        }
        "browser_press" => {
            params.as_object_mut().unwrap().remove("target");
            params["key"] = args["key"].clone();
            "press_observed"
        }
        "browser_reload" | "browser_navigate" => {
            // navigate_tab reloads when given the tab's own committed URL. The
            // daemon rejects target and frame fields for navigation.
            let url = args["url"].as_str().filter(|url| !url.is_empty())
                .ok_or_else(|| io::Error::other("Reload requires the current tab URL"))?;
            let params = params.as_object_mut().unwrap();
            params.remove("target");
            params.remove("frame_id");
            params.insert("url".into(), json!(url));
            "navigate_tab"
        }
        "browser_select" => {
            params["option"] = selection_value(&args["option"])?;
            "select_observed"
        }
        "browser_check" => {
            params["checked"] = args["checked"].clone();
            "check_observed"
        }
        _ => return Err(io::Error::other("Unsupported observed browser action")),
    };
    Ok((method, params))
}

#[cfg(test)]
mod reload_tests {
    use super::*;

    #[test]
    fn reload_navigates_the_tab_to_its_own_url_without_control_fields() {
        let selection = json!({"browser_id":"b1","target_id":"t1"});
        let (method, params) = observed_action(
            "browser_reload",
            &json!({"url":"http://127.0.0.1:1/form"}),
            &selection,
            "obs1",
            "e3",
            Some("frame1"),
        )
        .unwrap();
        assert_eq!(method, "navigate_tab");
        // The daemon's NavigateTabParams denies unknown fields.
        assert_eq!(
            params,
            json!({"browser_id":"b1","target_id":"t1","observation_id":"obs1","url":"http://127.0.0.1:1/form"})
        );
    }

    #[test]
    fn reload_without_a_url_is_rejected() {
        let selection = json!({"browser_id":"b1","target_id":"t1"});
        assert!(observed_action("browser_reload", &json!({}), &selection, "obs1", "", None).is_err());
    }
}

#[cfg(test)]
mod navigation_tests {
    use super::*;
    #[test]
    fn navigation_uses_the_requested_destination_and_no_control_fields() {
        let (method, params) = observed_action("browser_navigate", &json!({"url":"https://example.test/search?q=next"}),
            &json!({"browser_id":"b","target_id":"t"}), "fresh", "", None).unwrap();
        assert_eq!(method,"navigate_tab");
        assert_eq!(params,json!({"browser_id":"b","target_id":"t","observation_id":"fresh","url":"https://example.test/search?q=next"}));
    }
}
