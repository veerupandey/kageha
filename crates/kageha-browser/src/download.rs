//! A CDP download is accepted only after a trusted click on an observed link,
//! a matching main-frame URL event, and terminal progress for that GUID.
use crate::action_journal::{Action, Receipt, ReceiptState};
use crate::actions::{observed_identity, resolve_target, Page};
use crate::application_journal::ActionJournal;
use crate::cdp::{field, Cdp, Result};
use crate::protocol::{now_ms, Request};
use crate::script_artifact::MAX_DOWNLOAD_BYTES;
use serde_json::{json, Value};
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Weak,
};
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::watch;
use tokio::time::Instant;

pub struct Download {
    stage: TempDir,
    browser_path: PathBuf,
    transfer: Option<crate::workstation_transfer::Transfer>,
    deadline: Instant,
    guid: Option<String>,
    armed: bool,
    view: Arc<()>,
}

pub struct Trigger {
    frame: String,
    url: String,
    input_reply: u64,
}

impl Download {
    pub fn new(root: &Path, request: &Request, transfer: Option<&crate::workstation_transfer::Transfer>) -> Result<Self> {
        let remaining = request
            .deadline_ms
            .saturating_sub(now_ms() + 400)
            .min(15_000);
        if remaining == 0 {
            return Err("script_download_timed_out");
        }
        let stage = tempfile::Builder::new()
            .prefix("browser-download-")
            .tempdir_in(transfer.map(|v| v.transfer.as_path()).unwrap_or(root))
            .map_err(|_| "script_download_storage_unavailable")?;
        let browser_path = if let Some(transfer) = transfer {
            transfer.guest(stage.path())?
        } else { stage.path().to_owned() };
        Ok(Self {
            stage,
            browser_path,
            transfer: transfer.cloned(),
            deadline: Instant::now() + Duration::from_millis(remaining),
            guid: None,
            armed: false,
            view: Arc::new(()),
        })
    }

    pub fn view(&self) -> Weak<()> {
        Arc::downgrade(&self.view)
    }

    pub async fn start(
        &mut self,
        cdp: &mut Cdp,
        journal: &mut ActionJournal<'_>,
        request: &Request,
        browser: &str,
        target: &str,
        locator: &str,
        expected_url: Option<&str>,
        snapshot: &Value,
        dispatched: &AtomicBool,
        stop: &watch::Receiver<bool>,
    ) -> Result<Trigger> {
        tokio::time::timeout_at(self.deadline, async {
            journal.authorize(request).await?;
            let page = Page::from_observation(cdp, target, snapshot).await?;
            let node = resolve_target(cdp, &page, snapshot, locator).await?;
            let identity = observed_identity(snapshot, &page.frame, node)?;
            let object = page.object(cdp, node).await?;
            let initial_link = match link_url(cdp, &page, &object).await {
                Ok(url) => Some(url),
                Err("script_download_requires_link") if expected_url.is_some() => None,
                Err(error) => return Err(error),
            };
            let url = expected_url.or(initial_link.as_deref()).ok_or("script_download_requires_link")?.to_owned();
            if !url.starts_with("http://") && !url.starts_with("https://") {
                return Err("unsupported_page_url");
            }
            cdp.check_url(&url)?;
            page.scroll_to(cdp, &object, false).await?;
            let point = page.point(cdp, &object, false).await?;
            let receipt = Receipt {
                input_evidence: None,
            upload_evidence: None,
                request_id: request.request_id.clone(), workspace_id: request.workspace_id.clone(),
                client_id: request.client_id.clone(), service_epoch: request.service_epoch.clone().ok_or("stale_service_epoch")?,
                browser_id: browser.into(), target_id: target.into(), document_id: page.document.clone(), frame_id: None, frame_document_id: None, result_document_id: None,
                action: Action::Download, expected: None, batch: None, plan: None, submission_context: None, confirmation: None, resolution: None, confirmation_source: None, state: ReceiptState::Dispatching,
            };
            journal.check_new(&receipt).await?;
            self.armed = true;
            cdp.watch_downloads();
            cdp.call(None, "Browser.setDownloadBehavior", json!({
                "behavior": "allowAndName", "downloadPath": self.browser_path, "eventsEnabled": true,
            })).await?;
            journal.authorize(request).await?;
            if *stop.borrow() {
                return Err("script_cancelled");
            }
            journal.begin(receipt).await?;
            let checked = async {
                page.check_document(cdp).await?;
                page.check_label(cdp, node, identity).await?;
                if resolve_target(cdp, &page, snapshot, locator).await? != node
                    || page.point(cdp, &object, false).await? != point
                {
                    return Err("locator_changed");
                }
                match (initial_link.as_deref(), link_url(cdp, &page, &object).await) {
                    (Some(initial), Ok(current)) if initial == current => (),
                    (None, Err("script_download_requires_link")) => (),
                    _ => return Err("locator_changed"),
                }
                journal.authorize(request).await?;
                if *stop.borrow() {
                    return Err("script_cancelled");
                }
                Ok(())
            }.await;
            if let Err(error) = checked {
                journal.finish(&request.workspace_id, &request.request_id, ReceiptState::NotDispatched).await?;
                return Err(error);
            }
            dispatched.store(true, Ordering::Relaxed);
            cdp.watch_downloads();
            let mouse = |kind| json!({"type": kind, "x": point.0, "y": point.1, "button": "left", "clickCount": 1});
            cdp.call(Some(&page.session), "Input.dispatchMouseEvent", mouse("mousePressed")).await?;
            page.check_document(cdp).await?;
            // Chromium may defer mouseReleased's reply until navigation has
            // settled. Keep listening for the download without holding the tab registry.
            let input_reply = cdp.send_call(Some(&page.session), "Input.dispatchMouseEvent", mouse("mouseReleased")).await?;
            Ok(Trigger { frame: page.frame, url, input_reply })
        }).await.unwrap_or(Err("script_download_timed_out"))
    }

    pub async fn finish(
        &mut self,
        cdp: &mut Cdp,
        journal: &mut ActionJournal<'_>,
        request: &Request,
        trigger: &Trigger,
        stop: &mut watch::Receiver<bool>,
    ) -> Result<Vec<u8>> {
        loop {
            let event = tokio::select! {
                biased;
                _ = stop.changed() => return Err("script_cancelled"),
                event = tokio::time::timeout_at(self.deadline, cdp.next_download_message(trigger.input_reply)) => {
                    event.unwrap_or(Err("script_download_timed_out"))?
                }
            };
            if event["id"].as_u64() == Some(trigger.input_reply) {
                if self.guid.is_none() {
                    if event.get("error").is_some() {
                        return Err("script_download_input_unconfirmed");
                    }
                    journal
                        .finish(
                            &request.workspace_id,
                            &request.request_id,
                            ReceiptState::Returned,
                        )
                        .await?;
                }
                continue;
            }
            let params = &event["params"];
            if event["method"] == "Browser.downloadWillBegin" {
                if params["frameId"] != trigger.frame || params["url"] != trigger.url {
                    continue;
                }
                let guid = field(params, "guid")?;
                if guid.len() != 36 || uuid::Uuid::parse_str(guid).is_err() {
                    return Err("invalid_download_event");
                }
                if self.guid.as_deref().is_some_and(|prior| prior != guid) {
                    return Err("script_download_ambiguous");
                }
                if self.guid.is_none() {
                    self.guid = Some(guid.into());
                    journal
                        .finish(
                            &request.workspace_id,
                            &request.request_id,
                            ReceiptState::Verified,
                        )
                        .await?;
                }
                continue;
            }
            if self
                .guid
                .as_deref()
                .is_none_or(|guid| params["guid"] != guid)
            {
                continue;
            }
            if ["receivedBytes", "totalBytes"].iter().any(|key| {
                params[key]
                    .as_f64()
                    .is_some_and(|size| size > MAX_DOWNLOAD_BYTES as f64)
            }) {
                return Err("script_download_too_large");
            }
            match params["state"].as_str() {
                Some("completed") => return self.read_completed().await,
                Some("canceled") => return Err("script_download_interrupted"),
                _ => {}
            }
        }
    }

    async fn read_completed(&self) -> Result<Vec<u8>> {
        let guid = self.guid.as_deref().ok_or("invalid_download_event")?;
        if let Some(transfer) = self.transfer.clone().filter(|v| v.remote.is_some()) {
            let path = self.stage.path().join(guid);
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            return tokio::task::spawn_blocking(move || transfer.retrieve(&path, remaining, MAX_DOWNLOAD_BYTES))
                .await.map_err(|_| "workstation_download_failed")?;
        }
        // Only use the GUID under our own staging directory. CDP's optional
        // filePath and the site's suggested filename are never filesystem inputs.
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(self.stage.path().join(guid))
            .map_err(|_| "script_download_file_unavailable")?;
        let metadata = file
            .metadata()
            .map_err(|_| "script_download_file_unavailable")?;
        if !metadata.is_file() || metadata.uid() != crate::state::uid() {
            return Err("script_download_file_unavailable");
        }
        if metadata.len() > MAX_DOWNLOAD_BYTES as u64 {
            return Err("script_download_too_large");
        }
        let mut bytes = Vec::new();
        file.take((MAX_DOWNLOAD_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| "script_download_file_unavailable")?;
        if bytes.len() > MAX_DOWNLOAD_BYTES {
            return Err("script_download_too_large");
        }
        if bytes.len() as u64 != metadata.len() {
            return Err("script_download_file_unavailable");
        }
        Ok(bytes)
    }

    pub async fn cleanup(&self, cdp: &mut Cdp, interrupted: bool) {
        if !self.armed {
            return;
        }
        let _ = tokio::time::timeout(Duration::from_millis(180), async {
            if let Some(guid) = self.guid.as_ref().filter(|_| interrupted) {
                let _ = cdp
                    .call(None, "Browser.cancelDownload", json!({"guid": guid}))
                    .await;
            }
            let _ = cdp
                .call(
                    None,
                    "Browser.setDownloadBehavior",
                    json!({"behavior": "deny", "eventsEnabled": false}),
                )
                .await;
        })
        .await;
    }
}

async fn link_url(cdp: &mut Cdp, page: &Page, object: &str) -> Result<String> {
    let result = cdp.call(Some(&page.session), "Runtime.callFunctionOn", json!({
        "objectId": object, "returnByValue": true,
        "functionDeclaration": "function(){return this instanceof HTMLAnchorElement && (!this.target || this.target === '_self') ? this.href.split('#')[0] : null;}",
    })).await?;
    result["result"]["value"]
        .as_str()
        .filter(|url| url.starts_with("http://") || url.starts_with("https://"))
        .map(str::to_owned)
        .ok_or("script_download_requires_link")
}
