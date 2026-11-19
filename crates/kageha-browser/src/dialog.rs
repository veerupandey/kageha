//! Handle one declared native JavaScript dialog during a receipted click.
use crate::cdp::{Cdp, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::time::Instant;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Dialog {
    pub url: String,
    kind: String,
    message: String,
    accept: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_text: Option<String>,
}

impl Dialog {
    pub fn valid(&self) -> bool {
        matches!(self.kind.as_str(), "alert" | "confirm" | "prompt")
            && !self.url.is_empty()
            && self.url.len() <= 2048
            && self.message.len() <= 512
            && self
                .prompt_text
                .as_ref()
                .is_none_or(|text| self.kind == "prompt" && self.accept && text.len() <= 2048)
    }

    pub async fn input(
        &self,
        cdp: &mut Cdp,
        session: &str,
        frame: &str,
        deadline: Instant,
        input: Value,
        handled: &mut bool,
        required: bool,
    ) -> Result<()> {
        tokio::time::timeout_at(deadline, async {
            let input = cdp
                .send_call(Some(session), "Input.dispatchMouseEvent", input)
                .await?;
            let mut input_returned = false;
            let mut handling = None;
            loop {
                let event = cdp.dialog_message().await?;
                if event["method"] == "Page.javascriptDialogOpening" {
                    let params = &event["params"];
                    if *handled
                        || handling.is_some()
                        || event["sessionId"] != session
                        || params["type"] != self.kind
                        || params["message"] != self.message
                        || params["url"] != self.url
                        || params["frameId"]
                            .as_str()
                            .is_some_and(|actual| actual != frame)
                    {
                        return Err("unexpected_browser_dialog");
                    }
                    let mut command = json!({"accept": self.accept});
                    if let Some(text) = &self.prompt_text {
                        command["promptText"] = json!(text);
                    }
                    handling = Some(
                        cdp.send_call(Some(session), "Page.handleJavaScriptDialog", command)
                            .await?,
                    );
                } else if let Some(id) = event["id"].as_u64() {
                    if id == input || Some(id) == handling {
                        if event.get("error").is_some() {
                            return Err("browser_dialog_action_failed");
                        }
                        if id == input {
                            input_returned = true;
                        } else {
                            handling = None;
                            *handled = true;
                        }
                    }
                }
                if input_returned && handling.is_none() && (!required || *handled) {
                    return Ok(());
                }
            }
        })
        .await
        .map_err(|_| "browser_dialog_unobserved")?
    }
}
