use crate::action_journal::{Action, Receipt, ReceiptState};
use crate::actions::{resolve_target, valid_script_locator, Page};
use crate::application_journal::ActionJournal;
use crate::cdp::{field, Cdp, Result};
use crate::protocol::{now_ms, Request};
use crate::script_artifact::{open_owned_directory, open_root, run_directory, Workspace};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::ffi::{CStr, CString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use tempfile::TempDir;
use tokio::sync::watch;

const MAX_FILES: usize = 8;
const MAX_BYTES: u64 = 16 * 1024 * 1024;
const STAGE_PREFIX: &str = "browser-upload-";
const STAGE_SUFFIX_LENGTH: usize = 6;

/// Approved names and destination survive input replacement and daemon restarts.
/// They prove the requested attachment, not a server-side application submission.
#[derive(Clone, Deserialize, PartialEq, Serialize)]
pub struct Evidence {
    origin: String,
    names: Vec<String>,
}

fn valid_names(names: &[String]) -> bool {
    (1..=MAX_FILES).contains(&names.len())
        && names.iter().all(|name| {
            !name.is_empty()
                && name.len() <= 255
                && !name.contains(['/', '\\'])
                && !name.chars().any(char::is_control)
        })
}

async fn visible_attachments(cdp: &mut Cdp, page: &Page, names: &[String]) -> Result<bool> {
    // Match complete visible filename text, or input elements that hold the file.
    let expression = format!(
        r#"((names) => names.every(name => {{
        const fileInputs = Array.from(document.querySelectorAll('input[type="file"]'));
        if (fileInputs.some(inp => Array.from(inp.files || []).some(f => f.name === name))) return true;
        return Array.from(document.querySelectorAll('body *')).some(el => {{
            if (['SCRIPT','STYLE','INPUT','TEXTAREA'].includes(el.tagName)) return false;
            if (!el.checkVisibility({{opacityProperty:true,visibilityProperty:true}})) return false;
            const text = (el.innerText || '').trim();
            if (text === name) return true;
            const lines = text.split(/[\r\n]+/);
            if (lines.some(l => l.trim() === name)) return true;
            const escaped = name.replace(/[.*+?^${{}}()|[\]\\]/g, '\\$&');
            const regex = new RegExp('(?:^|\\s|[\\(\\[«"\'›—])' + escaped + '(?:$|\\s|[\\)\\]»"\'‹—✕×])');
            return regex.test(text);
        }});
    }}))({})"#,
        json!(names)
    );
    let reply = cdp
        .call(
            Some(&page.session),
            "Runtime.evaluate",
            json!({
                "expression":expression,"contextId":page.context,"returnByValue":true
            }),
        )
        .await?;
    Ok(reply["result"]["value"] == true)
}

pub(crate) async fn reconcile(
    cdp: &mut Cdp,
    journal: &mut ActionJournal<'_>,
    request: &Request,
    params: &crate::actions::Reconcile,
    snapshot: &Value,
    receipt: &Receipt,
) -> Result<Value> {
    if receipt.action != Action::Upload
        || receipt.client_id != request.client_id
        || receipt.target_id != params.target_id
        || (receipt.browser_id != params.browser_id
            && receipt.service_epoch == request.service_epoch.as_deref().unwrap_or(""))
    {
        return Err("receipt_target_changed");
    }
    // Only a returned DOM.setFileInputFiles is acknowledged. An uncertain
    // dispatch needs stronger evidence than a filename and stays blocked.
    if receipt.state != ReceiptState::Returned {
        return Err("upload_dispatch_not_acknowledged");
    }
    let page = Page::from_observation(cdp, &params.target_id, snapshot).await?;
    if page.document != receipt.document_id {
        return Err("receipt_document_changed");
    }
    let legacy = receipt.upload_evidence.is_none();
    let names = if let Some(evidence) = &receipt.upload_evidence {
        if page.current_frame(cdp).await?["securityOrigin"] != evidence.origin {
            return Err("script_upload_destination_mismatch");
        }
        if params
            .attachment_names
            .as_ref()
            .is_some_and(|names| names != &evidence.names)
        {
            return Err("upload_evidence_mismatch");
        }
        &evidence.names
    } else {
        // Older receipts omitted the approved names. A trusted controller must
        // explicitly nominate its known attachment. Retire only the hold;
        // never upgrade legacy metadata to a verified upload or byte proof.
        params
            .attachment_names
            .as_ref()
            .ok_or("upload_evidence_missing")?
    };
    if !valid_names(names) {
        return Err("invalid_attachment_names");
    }
    if !visible_attachments(cdp, &page, names).await? {
        return Err("upload_attachment_unobserved");
    }
    page.check_document(cdp).await?;
    journal.authorize(request).await?;
    let result = journal
        .finish(
            &request.workspace_id,
            &params.request_id,
            if legacy {
                ReceiptState::Reconciled
            } else {
                ReceiptState::Verified
            },
        )
        .await?;
    Ok(json!({"receipt":result,"input_sent":false,"files":names,
        "verification":"visible_attachment_name","legacy_metadata":legacy,
        "server_bytes_verified":false,"application_submitted":false}))
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Approval {
    origin: String,
    files: Vec<ApprovedFile>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ApprovedFile {
    path: String,
    sha256: String,
}

impl Approval {
    pub(crate) fn validate(&self) -> Result<()> {
        let uri = self
            .origin
            .parse::<tokio_tungstenite::tungstenite::http::Uri>()
            .map_err(|_| "invalid_script_upload_approval")?;
        if self.origin.len() > 300
            || !matches!(uri.scheme_str(), Some("http" | "https"))
            || !uri.authority().is_some_and(|authority| {
                !authority.as_str().contains('@')
                    && self
                        .origin
                        .split_once("://")
                        .is_some_and(|(_, host)| host == authority.as_str())
            })
            || !(1..=MAX_FILES).contains(&self.files.len())
        {
            return Err("invalid_script_upload_approval");
        }
        for file in &self.files {
            path_parts(&file.path)?;
            if file.sha256.len() != 64
                || !file
                    .sha256
                    .bytes()
                    .all(|c| c.is_ascii_digit() || matches!(c, b'a'..=b'f'))
            {
                return Err("invalid_script_upload_approval");
            }
        }
        Ok(())
    }

    async fn check_destination(&self, page: &Page, cdp: &mut Cdp) -> Result<()> {
        let frame = page.current_frame(cdp).await?;
        if frame["url"] == "about:blank" || frame["securityOrigin"] != self.origin {
            return Err("script_upload_destination_mismatch");
        }
        Ok(())
    }
}

pub fn clean_stages(root: &Path) -> io::Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir()
            && entry.file_name().to_str().is_some_and(|name| {
                name.strip_prefix(STAGE_PREFIX).is_some_and(|suffix| {
                    suffix.len() == STAGE_SUFFIX_LENGTH
                        && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
                })
            })
        {
            fs::remove_dir_all(entry.path())?;
        }
    }
    Ok(())
}

struct StagedFile {
    path: PathBuf,
    name: String,
    size: u64,
}

struct Staged {
    _directory: TempDir,
    files: Vec<StagedFile>,
    size: u64,
}

fn path_parts(path: &str) -> Result<Vec<&str>> {
    let parts: Vec<_> = path.split('/').collect();
    if path.len() > 512
        || !matches!(
            parts.as_slice(),
            ["inputs" | "artifacts" | "outputs", _, ..]
        )
        || parts.iter().any(|part| {
            part.is_empty()
                || part.starts_with('.')
                || part.contains('\\')
                || part.chars().any(char::is_control)
                || matches!(*part, "_conversation" | "_memory")
        })
    {
        return Err("invalid_script_upload_path");
    }
    Ok(parts)
}

fn open_source(parent: &File, name: &CStr) -> io::Result<File> {
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

impl Staged {
    fn copy(
        root: &Path,
        transfer: Option<&crate::workstation_transfer::Transfer>,
        application: Option<&Workspace>,
        workspace: &str,
        approval: &Approval,
        limit: u64,
        deadline: std::time::Instant,
    ) -> Result<Self> {
        let workspace_root = open_root(root, application)?;
        let owner = run_directory(workspace);
        let stage = tempfile::Builder::new()
            .prefix(STAGE_PREFIX)
            .rand_bytes(STAGE_SUFFIX_LENGTH)
            .tempdir_in(transfer.map(|v| v.transfer.as_path()).unwrap_or(root))
            .map_err(|_| "script_upload_storage_unavailable")?;
        let mut staged = Self {
            _directory: stage,
            files: Vec::new(),
            size: 0,
        };
        for (index, approved) in approval.files.iter().enumerate() {
            let mut parts = path_parts(&approved.path)?;
            if application.is_none() {
                if parts[0] == "artifacts" {
                    if parts.get(1) != Some(&owner.as_str()) || parts.len() < 3 {
                        return Err("invalid_script_upload_path");
                    }
                } else {
                    parts.insert(1, &owner);
                }
            }
            let (name, directories) = parts.split_last().ok_or("invalid_script_upload_path")?;
            let mut directory = workspace_root
                .try_clone()
                .map_err(|_| "script_upload_file_unavailable")?;
            for part in directories {
                let component = CString::new(*part).map_err(|_| "invalid_script_upload_path")?;
                directory = open_owned_directory(&directory, &component)
                    .map_err(|_| "script_upload_file_unavailable")?;
            }
            let name_c = CString::new(*name).map_err(|_| "invalid_script_upload_path")?;
            let source =
                open_source(&directory, &name_c).map_err(|_| "script_upload_file_unavailable")?;
            let before = source
                .metadata()
                .map_err(|_| "script_upload_file_unavailable")?;
            if !before.is_file()
                || before.uid() != crate::state::uid()
                || before.nlink() != 1
                || before.mode() & 0o022 != 0
            {
                return Err("script_upload_file_unavailable");
            }
            let remaining = limit.min(MAX_BYTES).saturating_sub(staged.size);
            if before.len() > remaining {
                return Err("script_upload_too_large");
            }
            let destination = staged._directory.path().join(index.to_string());
            fs::create_dir(&destination).map_err(|_| "script_upload_storage_unavailable")?;
            let destination = destination.join(name);
            let mut output = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&destination)
                .map_err(|_| "script_upload_storage_unavailable")?;
            let mut reader = source.take(remaining + 1);
            let size =
                io::copy(&mut reader, &mut output).map_err(|_| "script_upload_copy_failed")?;
            if size > remaining {
                return Err("script_upload_too_large");
            }
            let after = reader
                .get_ref()
                .metadata()
                .map_err(|_| "script_upload_file_unavailable")?;
            if size != before.len()
                || size != after.len()
                || (
                    before.mtime(),
                    before.mtime_nsec(),
                    before.ctime(),
                    before.ctime_nsec(),
                ) != (
                    after.mtime(),
                    after.mtime_nsec(),
                    after.ctime(),
                    after.ctime_nsec(),
                )
            {
                return Err("script_upload_file_changed");
            }
            output.rewind().map_err(|_| "script_upload_copy_failed")?;
            let mut digest = Sha256::new();
            let mut buffer = [0; 8192];
            loop {
                let count = output
                    .read(&mut buffer)
                    .map_err(|_| "script_upload_copy_failed")?;
                if count == 0 {
                    break;
                }
                digest.update(&buffer[..count]);
            }
            if data_encoding::HEXLOWER.encode(&digest.finalize()) != approved.sha256 {
                return Err("script_upload_file_changed");
            }
            staged.size += size;
            staged.files.push(StagedFile {
                path: if let Some(transfer) = transfer {
                    transfer.stage(&destination, deadline)?;
                    transfer.guest(&destination)?
                } else { destination },
                name: (*name).into(),
                size,
            });
        }
        Ok(staged)
    }
}

#[derive(Default)]
pub struct Uploads(Vec<(String, Staged)>);

impl Uploads {
    pub fn retain(&mut self, mut keep: impl FnMut(&str) -> bool) {
        self.0.retain(|(target, _)| keep(target));
    }

    pub async fn attach(
        &mut self,
        cdp: &mut Cdp,
        journal: &mut ActionJournal<'_>,
        request: &Request,
        root: &Path,
        transfer: Option<&crate::workstation_transfer::Transfer>,
        browser: &str,
        target: &str,
        locator: &str,
        paths: Vec<String>,
        approval: Approval,
        snapshot: &Value,
        dispatched: &AtomicBool,
        stop: &watch::Receiver<bool>,
    ) -> Result<Value> {
        if !valid_script_locator(locator) {
            return Err("invalid_locator");
        }
        let count = paths.len();
        if !(1..=MAX_FILES).contains(&count) {
            return Err("script_upload_file_count");
        }
        if !paths
            .iter()
            .map(String::as_str)
            .eq(approval.files.iter().map(|file| file.path.as_str()))
        {
            return Err("script_upload_files_mismatch");
        }
        if self.0.len() >= 16 {
            return Err("script_upload_storage_full");
        }
        let remaining = (64 * 1024 * 1024_u64)
            .saturating_sub(self.0.iter().map(|(_, staged)| staged.size).sum());
        let page = Page::from_observation(cdp, target, snapshot).await?;
        approval.check_destination(&page, cdp).await?;
        let node = resolve_target(cdp, &page, snapshot, locator).await?;
        let object = page.object(cdp, node).await?;
        let eligibility_args = json!([{"value": count}]);
        if !page
            .test_object(cdp, &object, ELIGIBLE, eligibility_args.clone())
            .await?
        {
            return Err("script_upload_requires_file_input");
        }
        let receipt = Receipt {
            input_evidence: None,
            upload_evidence: Some(Evidence {
                origin: approval.origin.clone(),
                names: approval
                    .files
                    .iter()
                    .map(|file| file.path.rsplit('/').next().unwrap_or("").to_owned())
                    .collect(),
            }),
            request_id: request.request_id.clone(),
            workspace_id: request.workspace_id.clone(),
            client_id: request.client_id.clone(),
            service_epoch: request.service_epoch.clone().ok_or("stale_service_epoch")?,
            browser_id: browser.into(),
            target_id: target.into(),
            document_id: page.document.clone(),
            frame_id: None,
            frame_document_id: None,
            result_document_id: None,
            action: Action::Upload,
            expected: None,
            batch: None,
            plan: None,
            submission_context: None, confirmation: None, resolution: None, confirmation_source: None,
            state: ReceiptState::Dispatching,
        };
        journal.check_new(&receipt).await?;
        let application = journal.script_artifact_workspace().await?;
        journal.authorize_upload(&approval).await?;
        let root = root.to_owned();
        let transfer = transfer.cloned();
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(request.deadline_ms.saturating_sub(now_ms()).min(30000));
        let workspace = request.workspace_id.clone();
        let (approval, staged) = tokio::task::spawn_blocking(move || {
            let staged = Staged::copy(
                &root,
                transfer.as_ref(),
                application.as_ref(),
                &workspace,
                &approval,
                remaining,
                deadline,
            )?;
            Ok::<_, &'static str>((approval, staged))
        })
        .await
        .map_err(|_| "script_upload_copy_failed")??;
        journal.authorize(request).await?;
        check_live(request, stop)?;
        journal.begin(receipt).await?;
        let ready = async {
            page.check_document(cdp).await?;
            if resolve_target(cdp, &page, snapshot, locator).await? != node
                || !page
                    .test_object(cdp, &object, ELIGIBLE, eligibility_args)
                    .await?
            {
                return Err("locator_changed");
            }
            journal.authorize(request).await?;
            approval.check_destination(&page, cdp).await?;
            check_live(request, stop)
        }
        .await;
        if let Err(error) = ready {
            journal
                .finish(
                    &request.workspace_id,
                    &request.request_id,
                    ReceiptState::NotDispatched,
                )
                .await?;
            return Err(error);
        }

        let files: Vec<_> = staged.files.iter().map(|file| json!(file.path)).collect();
        // Retain the copies before sending CDP. Chromium reads them lazily, including
        // after script exit and when the command's outcome could not be confirmed.
        self.0.push((target.into(), staged));
        dispatched.store(true, Ordering::Relaxed);
        let staged = &self.0.last().ok_or("script_upload_unconfirmed")?.1;
        let confirmed = async {
            cdp.call(
                Some(&page.session),
                "DOM.setFileInputFiles",
                json!({"objectId": object, "files": files}),
            )
            .await?;
            // Chromium dispatches input and change itself, including on widgets that clear FileList.
            journal
                .finish(
                    &request.workspace_id,
                    &request.request_id,
                    ReceiptState::Returned,
                )
                .await?;
            let files = read_files(cdp, &page, &object, &staged.files).await?;
            page.check_document(cdp).await?;
            journal
                .finish(
                    &request.workspace_id,
                    &request.request_id,
                    ReceiptState::Verified,
                )
                .await?;
            Ok::<_, &'static str>(json!({"files": files}))
        }
        .await;
        confirmed.map_err(|_| "script_upload_unconfirmed")
    }
}

const ELIGIBLE: &str = r#"function(count) {
    return this instanceof HTMLInputElement && this.getRootNode() === document
        && this.type === "file" && !this.webkitdirectory && (this.multiple || count === 1)
        && !this.closest(":disabled,[aria-disabled=true],[inert]");
}"#;

fn check_live(request: &Request, stop: &watch::Receiver<bool>) -> Result<()> {
    if *stop.borrow() {
        return Err("script_cancelled");
    }
    if request.deadline_ms <= now_ms() {
        return Err("deadline_exceeded");
    }
    Ok(())
}

async fn read_files(
    cdp: &mut Cdp,
    page: &Page,
    object: &str,
    expected: &[StagedFile],
) -> Result<Vec<Value>> {
    const GROUP: &str = "kageha-upload-readback";
    let verified = async {
        // Capture a single FileList before checking its individual File objects.
        let snapshot = cdp.call(Some(&page.session), "Runtime.callFunctionOn", json!({
            "objectId": object, "objectGroup": GROUP,
            "functionDeclaration": "function(count) { return this.isConnected && this.files?.length === count ? Array.from(this.files) : null; }",
            "arguments": [{"value": expected.len()}],
        })).await?;
        let snapshot = field(&snapshot["result"], "objectId")?;
        let properties = cdp.call(Some(&page.session), "Runtime.getProperties", json!({
            "objectId": snapshot, "ownProperties": true,
        })).await?;
        let properties = properties["result"].as_array().ok_or("cdp_invalid_reply")?;
        if properties.len() != expected.len() + 1
            || !properties.iter().any(|property| property["name"] == "length" && property["value"]["value"] == expected.len())
        {
            return Err("script_upload_unconfirmed");
        }
        for (index, file) in expected.iter().enumerate() {
            let index = index.to_string();
            let property = properties.iter().find(|property| property["name"] == index)
                .ok_or("cdp_invalid_reply")?;
            let id = field(&property["value"], "objectId")?;
            let info = cdp.call(Some(&page.session), "DOM.getFileInfo", json!({"objectId": id})).await?;
            if info["path"].as_str() != file.path.to_str() {
                return Err("script_upload_unconfirmed");
            }
        }
        Ok(expected.iter().map(|file| json!({"name": file.name, "size": file.size})).collect())
    }.await;
    let _ = cdp
        .call(
            Some(&page.session),
            "Runtime.releaseObjectGroup",
            json!({"objectGroup": GROUP}),
        )
        .await;
    verified
}
