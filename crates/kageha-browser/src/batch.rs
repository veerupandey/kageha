use crate::action_journal::{Action, BatchStep, Expected, Receipt, ReceiptState};
use crate::actions::{self, valid_script_locator, Click, Input, Page, Selection};
use crate::application_journal::{ActionBackend, ActionJournal};
use crate::cdp::{Cdp, Result};
use crate::keyboard::Key;
use crate::protocol::{now_ms, Request};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

pub const MAX_STEPS: usize = 8;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Batch {
    pub browser_id: String,
    pub target_id: String,
    pub observation_id: String,
    actions: Vec<Step>,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Step {
    FillCredential {
        target: String,
        credential: String,
    },
    Fill {
        target: String,
        frame_id: Option<String>,
        text: String,
        #[serde(default, rename = "verify_value")]
        _verify_value: bool,
    },
    Press {
        key: String,
        frame_id: Option<String>,
    },
    Select {
        target: String,
        frame_id: Option<String>,
        option: Selection,
    },
    Check {
        target: String,
        frame_id: Option<String>,
        checked: bool,
    },
    Click {
        target: String,
        #[serde(default)]
        expect_selector: String,
        expect_url: Option<String>,
        timeout_ms: Option<u64>,
        frame_id: Option<String>,
        dialog: Option<crate::dialog::Dialog>,
    },
    Verify {
        target: String,
        frame_id: Option<String>,
        #[serde(default)]
        text: String,
    },
}

impl Step {
    fn action(&self) -> Action {
        match self {
            Self::Fill { .. } | Self::FillCredential { .. } => Action::Fill,
            Self::Press { .. } => Action::Press,
            Self::Select { .. } => Action::Select,
            Self::Check { .. } => Action::Check,
            Self::Click { .. } => Action::Click,
            Self::Verify { .. } => Action::Verify,
        }
    }

    fn valid(&self) -> bool {
        match self {
            Self::FillCredential { target, credential } => {
                valid_script_locator(target)
                    && credential.len() == 69
                    && credential.starts_with("cred_")
            }
            Self::Fill { target, text, .. } => {
                valid_script_locator(target) && text.len() <= 16384 && !text.contains('\0')
            }
            Self::Press { key, .. } => key.len() <= 64 && Key::parse(key).is_ok(),
            Self::Select { target, option, .. } => valid_script_locator(target) && option.valid(),
            Self::Check { target, .. } => valid_script_locator(target),
            Self::Click {
                target,
                expect_selector,
                expect_url,
                timeout_ms,
                dialog,
                ..
            } => {
                valid_script_locator(target)
                    && expect_selector.len() <= 512
                    && dialog.as_ref().is_none_or(crate::dialog::Dialog::valid)
                    && expect_url
                        .as_ref()
                        .is_none_or(|url| !url.is_empty() && url.len() <= 2048)
                    && timeout_ms.is_none_or(|ms| {
                        (1..=crate::protocol::limit("deadline_ms") as u64).contains(&ms)
                    })
            }
            Self::Verify { target, text, .. } => {
                !target.trim().is_empty() && target.len() <= 512 && text.len() <= 512
            }
        }
    }
}

fn step_id(batch_id: &str, index: usize) -> String {
    format!("{batch_id}:step:{index}")
}

async fn optional_receipt(
    journal: &ActionJournal<'_>,
    workspace: &str,
    id: &str,
) -> Result<Option<Receipt>> {
    match journal.receipt(workspace, id).await {
        Ok(receipt) => Ok(Some(receipt)),
        Err("action_receipt_missing") => Ok(None),
        Err(error) => Err(error),
    }
}

pub async fn status(
    root: &Path,
    backend: ActionBackend,
    request: &Request,
    id: &str,
) -> Result<Value> {
    let journal = ActionJournal::open(root, backend, request, false).await?;
    let manifest = journal.receipt(&request.workspace_id, id).await?;
    report(&journal, &manifest).await
}

async fn report(journal: &ActionJournal<'_>, manifest: &Receipt) -> Result<Value> {
    let plan = manifest
        .plan
        .as_ref()
        .filter(|plan| {
            manifest.action == Action::Batch
                && manifest.state == ReceiptState::Planned
                && !plan.is_empty()
                && plan.len() <= MAX_STEPS
        })
        .ok_or("batch_not_found")?;
    let mut steps = Vec::new();
    let mut document_id = manifest.document_id.clone();
    for (index, action) in plan.iter().enumerate() {
        let id = step_id(&manifest.request_id, index);
        let receipt = optional_receipt(journal, &manifest.workspace_id, &id).await?;
        let (state, id) =
            if let Some(receipt) = receipt {
                if receipt.batch.as_ref().is_none_or(|link| {
                    link.request_id != manifest.request_id || link.index != index
                }) || receipt.action != *action
                    || receipt.browser_id != manifest.browser_id
                    || receipt.target_id != manifest.target_id
                    || receipt.document_id != document_id
                {
                    return Err("batch_receipt_mismatch");
                }
                let state = match receipt.state {
                    ReceiptState::Verified => "verified",
                    ReceiptState::Reconciled => "reconciled",
                    ReceiptState::NotDispatched => "not_dispatched",
                    ReceiptState::Returned | ReceiptState::Dispatching => "uncertain",
                    ReceiptState::Planned => return Err("batch_receipt_mismatch"),
                };
                if receipt.state == ReceiptState::Verified {
                    if let Some(next) = receipt.result_document_id {
                        document_id = next;
                    }
                }
                (state, Some(id))
            } else {
                ("not_attempted", None)
            };
        steps.push(
            json!({"index": index, "action": action, "state": state, "receipt_request_id": id}),
        );
    }
    let completed = steps.iter().all(|step| step["state"] == "verified");
    Ok(
        json!({"status": if completed { "completed" } else { "stopped" },
              "batch_id": manifest.request_id, "target_id": manifest.target_id, "steps": steps}),
    )
}

pub async fn run(
    cdp: &mut Cdp,
    root: &Path,
    backend: ActionBackend,
    request: &Request,
    batch: Batch,
    mut snapshot: Value,
    dispatched: &AtomicBool,
) -> Result<Value> {
    actions::recover_pending(
        cdp,
        root,
        backend,
        request,
        &batch.browser_id,
        &batch.target_id,
        &snapshot,
    )
    .await?;
    if batch.actions.is_empty()
        || batch.actions.len() > MAX_STEPS
        || !batch.actions.iter().all(Step::valid)
    {
        return Err("invalid_batch_actions");
    }
    let mut journal = ActionJournal::open(root, backend, request, true).await?;
    journal.authorize(request).await?;
    let page = Page::from_observation(cdp, &batch.target_id, &snapshot).await?;
    let manifest = Receipt {
        request_id: request.request_id.clone(),
        workspace_id: request.workspace_id.clone(),
        client_id: request.client_id.clone(),
        service_epoch: request.service_epoch.clone().ok_or("stale_service_epoch")?,
        browser_id: batch.browser_id,
        target_id: batch.target_id,
        document_id: page.document.clone(),
        frame_id: None,
        frame_document_id: None,
        result_document_id: None,
        action: Action::Batch,
        expected: None,
        input_evidence: None,
        upload_evidence: None,
        batch: None,
        plan: Some(batch.actions.iter().map(Step::action).collect()),
        submission_context: None, confirmation: None, resolution: None, confirmation_source: None,
        state: ReceiptState::Planned,
    };
    cdp.detach(&page.session).await?;
    for index in 0..batch.actions.len() {
        if optional_receipt(
            &journal,
            &manifest.workspace_id,
            &step_id(&manifest.request_id, index),
        )
        .await?
        .is_some()
        {
            return Err("action_already_recorded");
        }
    }
    if request.deadline_ms <= now_ms() {
        return Err("deadline_exceeded");
    }
    journal.begin(manifest.clone()).await?;
    // After the manifest is durable, any lost batch response requires status
    // recovery even when the current step has not dispatched.
    dispatched.store(true, Ordering::Relaxed);
    let mut failure = None;
    let mut document_id = manifest.document_id.clone();
    let mut last_step_ms: u64 = 0;
    for (index, step) in batch.actions.into_iter().enumerate() {
        // Leave time for input readback, receipt commit and the structured reply.
        // Never begin a new step just because the RPC still has a few milliseconds.
        let reserve = 2000_u64.max(last_step_ms.saturating_mul(2).saturating_add(500));
        if request.deadline_ms.saturating_sub(now_ms()) < reserve {
            failure = Some((index, "batch_deadline_budget_exhausted"));
            break;
        }
        let started = now_ms();
        let step_dispatched = AtomicBool::new(false);
        let action = step.action();
        let link = BatchStep {
            request_id: manifest.request_id.clone(),
            index,
        };
        let mut step_receipt = Receipt {
            request_id: step_id(&manifest.request_id, index),
            action,
            plan: None,
            submission_context: None, confirmation: None, resolution: None, confirmation_source: None,
            state: ReceiptState::NotDispatched,
            batch: Some(link.clone()),
            document_id: document_id.clone(),
            ..manifest.clone()
        };
        let mut child = request.clone();
        child.request_id = step_receipt.request_id.clone();
        let result = async {
            if index > 0 {
                let targets = cdp.call(None, "Target.getTargets", json!({})).await?;
                snapshot = crate::observe::capture(
                    cdp,
                    &manifest.browser_id,
                    &manifest.target_id,
                    targets["targetInfos"]
                        .as_array()
                        .ok_or("cdp_invalid_reply")?,
                )
                .await?;
            }
            let frame = snapshot["main_frame_id"]
                .as_str()
                .ok_or("cdp_invalid_reply")?;
            if snapshot["frames"][frame] != document_id {
                return Err("document_changed");
            }
            match step {
                Step::FillCredential { target, credential } => {
                    let origin = crate::credentials::origin(
                        snapshot["url"].as_str().ok_or("cdp_invalid_reply")?,
                    )?;
                    let vault = crate::credentials::Vault::configured()?;
                    let secret = vault.resolve(&request.workspace_id, &origin, &credential)?;
                    drop(vault);
                    child.method = "fill_observed".into();
                    actions::credential_input(
                        cdp,
                        &mut journal,
                        &child,
                        manifest.browser_id.clone(),
                        manifest.target_id.clone(),
                        target,
                        origin,
                        secret,
                        &snapshot,
                        &step_dispatched,
                        link,
                    )
                    .await?;
                }
                step @ (Step::Fill { .. }
                | Step::Press { .. }
                | Step::Select { .. }
                | Step::Check { .. }) => {
                    let mut input = Input {
                        browser_id: manifest.browser_id.clone(),
                        target_id: manifest.target_id.clone(),
                        observation_id: String::new(),
                        target: None,
                        text: None,
                        key: None,
                        frame_id: None,
                        option: None,
                        checked: None,
                        otp_origin: None,
                    };
                    child.method = match step {
                        Step::Fill {
                            target,
                            text,
                            frame_id,
                            ..
                        } => {
                            input.target = Some(target);
                            input.text = Some(text);
                            input.frame_id = frame_id;
                            "fill_observed"
                        }
                        Step::Press { key, frame_id } => {
                            input.key = Some(key);
                            input.frame_id = frame_id;
                            "press_observed"
                        }
                        Step::Select {
                            target,
                            option,
                            frame_id,
                        } => {
                            input.target = Some(target);
                            input.option = Some(option);
                            input.frame_id = frame_id;
                            "select_observed"
                        }
                        Step::Check {
                            target,
                            checked,
                            frame_id,
                        } => {
                            input.target = Some(target);
                            input.checked = Some(checked);
                            input.frame_id = frame_id;
                            "check_observed"
                        }
                        _ => unreachable!(),
                    }
                    .into();
                    actions::batch_input(
                        cdp,
                        &mut journal,
                        &child,
                        input,
                        &snapshot,
                        &step_dispatched,
                        link,
                    )
                    .await?;
                }
                Step::Click {
                    target,
                    expect_selector,
                    expect_url,
                    timeout_ms,
                    frame_id,
                    dialog,
                } => {
                    let result = actions::click_step(
                        cdp,
                        &mut journal,
                        &child,
                        Click {
                            await_confirmation: false,
                            submission_context: None,
                            browser_id: manifest.browser_id.clone(),
                            target_id: manifest.target_id.clone(),
                            observation_id: String::new(),
                            target: Some(target),
                            backend_node_id: None,
                            expected_selector: expect_selector,
                            expected_text: String::new(),
                            expected_url: expect_url,
                            timeout_ms,
                            button: None,
                            click_count: None,
                            gesture: None,
                            frame_id,
                            dialog,
                        },
                        &snapshot,
                        &step_dispatched,
                        Some(link),
                        || {},
                    )
                    .await?;
                    if let Some(next) = result["receipt"]["result_document_id"].as_str() {
                        document_id = next.to_owned();
                    }
                }
                Step::Verify {
                    target,
                    text,
                    frame_id,
                } => {
                    journal.authorize(request).await?;
                    let page =
                        Page::from_scope(cdp, &manifest.target_id, &snapshot, frame_id.as_deref())
                            .await?;
                    let matched = page
                        .expected(
                            cdp,
                            &Expected {
                                selector: target,
                                text,
                                url: None,
                            },
                        )
                        .await?;
                    page.check_document(cdp).await?;
                    page.detach(cdp).await?;
                    if !matched {
                        return Err("postcondition_unobserved");
                    }
                    step_receipt.state = ReceiptState::Verified;
                    journal.begin(step_receipt.clone()).await?;
                }
            }
            Ok(())
        }
        .await;
        last_step_ms = now_ms().saturating_sub(started);
        if let Err(error) = result {
            if !step_dispatched.load(Ordering::Relaxed)
                && optional_receipt(&journal, &manifest.workspace_id, &step_receipt.request_id)
                    .await?
                    .is_none()
            {
                step_receipt.state = ReceiptState::NotDispatched;
                journal.begin(step_receipt).await?;
            }
            failure = Some((index, error));
            break;
        }
    }
    let mut result = report(&journal, &manifest).await?;
    if let Some((index, error)) = failure.filter(|_| result["status"] == "stopped") {
        result["failed_index"] = json!(index);
        result["error_code"] = json!(error);
    }
    Ok(result)
}
