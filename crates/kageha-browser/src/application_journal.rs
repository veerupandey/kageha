use crate::action_journal::{private_open, Journal, Receipt, ReceiptState};
use crate::cdp::Result;
use crate::protocol::Request;
use fs2::FileExt;
use serde_json::{json, Value};
use std::fs::File;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

#[derive(Clone, Copy, PartialEq)]
pub enum ActionBackend {
    Disabled,
    Fixture,
    Application,
    Standalone,
}

impl ActionBackend {
    pub fn name(self) -> Option<&'static str> {
        match self {
            Self::Disabled => None,
            Self::Fixture => Some("fixture"),
            Self::Application => Some("application"),
            Self::Standalone => Some("standalone"),
        }
    }
}

pub struct ApplicationJournal<'a> {
    path: PathBuf,
    request: &'a Request,
    _fence: Option<File>,
}

impl ApplicationJournal<'_> {
    async fn call(&self, operation: &str, data: Value) -> Result<Value> {
        let metadata =
            std::fs::symlink_metadata(&self.path).map_err(|_| "application_journal_unavailable")?;
        if !metadata.file_type().is_socket()
            || metadata.uid() != crate::state::uid()
            || metadata.mode() & 0o077 != 0
        {
            return Err("application_journal_unavailable");
        }
        let stream = UnixStream::connect(&self.path)
            .await
            .map_err(|_| "application_journal_unavailable")?;
        if stream
            .peer_cred()
            .map_err(|_| "application_journal_unavailable")?
            .uid()
            != crate::state::uid()
        {
            return Err("application_journal_unavailable");
        }
        let (read, mut write) = stream.into_split();
        let message = json!({
            "workspace_id": self.request.workspace_id, "client_id": self.request.client_id,
            "service_epoch": self.request.service_epoch, "authority": self.request.authority,
            "operation": operation, "method": self.request.method,
            "request_id": self.request.request_id, "data": data,
        });
        let mut bytes =
            serde_json::to_vec(&message).map_err(|_| "application_journal_unavailable")?;
        bytes.push(b'\n');
        write
            .write_all(&bytes)
            .await
            .map_err(|_| "application_journal_unavailable")?;
        let response = crate::read_message(&mut BufReader::new(read))
            .await
            .map_err(|_| "application_journal_unavailable")?
            .ok_or("application_journal_unavailable")?;
        let mut response: Value =
            serde_json::from_slice(&response).map_err(|_| "application_journal_unavailable")?;
        if let Some(error) = response.get("error") {
            return Err(match error.as_str() {
                Some("action_authority_changed") => "action_authority_changed",
                Some("application_authority_mismatch") => "application_authority_mismatch",
                Some("application_attempt_unavailable") => "application_attempt_unavailable",
                Some("application_receipt_redacted") => "application_receipt_redacted",
                Some("action_already_recorded") => "action_already_recorded",
                Some("action_receipt_pending") => "action_receipt_pending",
                Some("action_receipt_missing") => "action_receipt_missing",
                Some("action_receipt_changed") => "action_receipt_changed",
                Some("action_receipt_not_pending") => "action_receipt_not_pending",
                Some("action_receipt_not_reconcilable") => "action_receipt_not_reconcilable",
                Some("script_upload_approval_required") => "script_upload_approval_required",
                Some("browser_dialog_approval_required") => "browser_dialog_approval_required",
                Some("script_artifact_workspace_unavailable") => {
                    "script_artifact_workspace_unavailable"
                }
                _ => "application_journal_unavailable",
            });
        }
        response
            .as_object_mut()
            .and_then(|object| object.remove("result"))
            .filter(Value::is_object)
            .ok_or("application_journal_unavailable")
    }
}

pub enum ActionJournal<'a> {
    Local(Journal),
    Application(ApplicationJournal<'a>),
}

impl<'a> ActionJournal<'a> {
    pub fn pending(&self, request: &Request, browser: &str, target: &str) -> Result<Vec<Receipt>> {
        match self {
            Self::Local(journal) => journal.pending(
                &request.workspace_id,
                browser,
                target,
                request.service_epoch.as_deref(),
            ),
            Self::Application(_) => Err("application_input_recovery_unavailable"),
        }
    }
    pub async fn open(
        root: &Path,
        backend: ActionBackend,
        request: &'a Request,
        fenced: bool,
    ) -> Result<Self> {
        match backend {
            ActionBackend::Fixture | ActionBackend::Standalone => {
                Ok(Self::Local(if fenced {Journal::lock(root)?} else {Journal::read(root)?}))
            }
            ActionBackend::Application => {
                let mut journal = ApplicationJournal {
                    path: root.join("application-journal.sock"),
                    request,
                    _fence: None,
                };
                if fenced {
                    let authorized = journal.call("authorize", json!({})).await?;
                    let path = authorized["fence_path"]
                        .as_str()
                        .ok_or("application_journal_unavailable")?;
                    let file = private_open(Path::new(path), false)
                        .map_err(|_| "action_authority_changed")?;
                    FileExt::try_lock_exclusive(&file).map_err(|_| "action_control_busy")?;
                    journal._fence = Some(file);
                }
                Ok(Self::Application(journal))
            }
            ActionBackend::Disabled => Err("fixture_actions_unavailable"),
        }
    }

    pub async fn authorize(&self, request: &Request) -> Result<()> {
        match self {
            Self::Local(journal) => journal.authorize(request),
            Self::Application(journal) => journal.call("authorize", json!({})).await.map(|_| ()),
        }
    }

    pub async fn authorize_upload(&self, approval: &crate::upload::Approval) -> Result<()> {
        match self {
            Self::Local(_) => Ok(()),
            Self::Application(journal) => journal
                .call("authorize_upload", json!(approval))
                .await
                .map(|_| ()),
        }
    }

    pub async fn authorize_dialog(&self, dialog: &crate::dialog::Dialog) -> Result<()> {
        match self {
            Self::Local(_) => Ok(()),
            Self::Application(journal) => journal
                .call("authorize_dialog", json!(dialog))
                .await
                .map(|_| ()),
        }
    }

    pub async fn script_artifact_workspace(
        &self,
    ) -> Result<Option<crate::script_artifact::Workspace>> {
        match self {
            Self::Local(_) => Ok(None),
            Self::Application(journal) => {
                let result = journal.call("artifact_workspace", json!({})).await?;
                serde_json::from_value(result)
                    .map(Some)
                    .map_err(|_| "script_artifact_workspace_unavailable")
            }
        }
    }

    pub async fn check_navigation(
        &self,
        request: &Request,
        browser: &str,
        target: &str,
    ) -> Result<()> {
        match self {
            Self::Local(journal) => journal.check_navigation(
                &request.workspace_id,
                browser,
                target,
                request.service_epoch.as_deref(),
            ),
            Self::Application(journal) => journal
                .call(
                    "check_navigation",
                    json!({"browser_id": browser, "target_id": target}),
                )
                .await
                .map(|_| ()),
        }
    }

    pub async fn check_new(&self, receipt: &Receipt) -> Result<()> {
        match self {
            Self::Local(journal) => journal.check_new(receipt),
            Self::Application(journal) => journal.call("check", json!(receipt)).await.map(|_| ()),
        }
    }

    pub async fn begin(&mut self, receipt: Receipt) -> Result<()> {
        match self {
            Self::Local(journal) => journal.begin(receipt),
            Self::Application(journal) => {
                let acknowledged = decode_receipt(journal.call("begin", json!(receipt)).await?)?;
                if acknowledged == receipt {
                    Ok(())
                } else {
                    Err("application_journal_unavailable")
                }
            }
        }
    }

    pub async fn receipt(&self, workspace: &str, id: &str) -> Result<Receipt> {
        match self {
            Self::Local(journal) => journal.receipt(workspace, id),
            Self::Application(journal) => {
                decode_receipt(journal.call("read", json!({"request_id": id})).await?)
            }
        }
    }

    pub async fn finish(
        &mut self,
        workspace: &str,
        id: &str,
        state: ReceiptState,
    ) -> Result<Receipt> {
        self.finish_with_document(workspace, id, state, None).await
    }

    pub async fn finish_with_document(
        &mut self,
        workspace: &str,
        id: &str,
        state: ReceiptState,
        result_document_id: Option<&str>,
    ) -> Result<Receipt> {
        match self {
            Self::Local(journal) => journal.finish(workspace, id, state, result_document_id),
            Self::Application(journal) => decode_receipt(
                journal
                    .call("finish", json!({"request_id": id, "state": state, "result_document_id": result_document_id}))
                    .await?,
            ),
        }
    }
}

fn decode_receipt(mut result: Value) -> Result<Receipt> {
    serde_json::from_value(result["receipt"].take()).map_err(|_| "application_journal_unavailable")
}
