use crate::cdp::Result;
use crate::protocol::{now_ms, Authority, Request};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use rusqlite::{params, OptionalExtension};
use crate::journal_store::Store;
use std::fs::{File, OpenOptions};

use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use uuid::Uuid;

const DEFAULT_TTL_MS: u64 = 30_000;
const MAX_TTL_MS: u64 = 60_000;
pub const CONTROL_METHODS: &[&str] = &["claim_control", "release_control"];

#[derive(Deserialize, Serialize)]
pub(crate) struct Grant {
    generation: u64,
    grant_id: String,
    client_id: String,
    service_epoch: String,
    expires_ms: u64,
}

impl Grant {
    fn active(&self, request: &Request, now: u64) -> bool {
        Some(self.service_epoch.as_str()) == request.service_epoch.as_deref()
            && self.expires_ms > now
    }

    fn matches(&self, request: &Request, authority: &Authority, now: u64) -> bool {
        self.active(request, now)
            && self.client_id == request.client_id
            && self.generation == authority.generation
            && self.grant_id == authority.grant_id
    }

    fn control_state(&self) -> Value {
        json!({"authority": {"generation": self.generation, "grant_id": self.grant_id},
               "expires_ms": self.expires_ms})
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaimControl {
    #[serde(default = "default_ttl_ms")]
    ttl_ms: u64,
}

fn default_ttl_ms() -> u64 {
    DEFAULT_TTL_MS
}

#[derive(Clone, Deserialize, PartialEq, Serialize)]
pub struct Expected {
    pub selector: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Clone, Copy, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    #[default]
    Click,
    Fill,
    Press,
    Select,
    Check,
    Verify,
    Batch,
    Download,
    Upload,
}

#[derive(Clone, Deserialize, PartialEq, Serialize)]
pub struct Receipt {
    pub request_id: String,
    pub workspace_id: String,
    pub client_id: String,
    pub service_epoch: String,
    pub browser_id: String,
    pub target_id: String,
    pub document_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_document_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result_document_id: Option<String>,
    #[serde(default)]
    pub action: Action,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<Expected>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch: Option<BatchStep>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<Vec<Action>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_evidence: Option<crate::input_evidence::Evidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upload_evidence: Option<crate::upload::Evidence>,
    #[serde(default,skip_serializing_if="Option::is_none")]
    pub submission_context:Option<crate::confirmation::Context>,
    #[serde(default,skip_serializing_if="Option::is_none")]
    pub confirmation:Option<crate::confirmation::Confirmation>,
    #[serde(default,skip_serializing_if="Option::is_none")]
    pub resolution:Option<crate::confirmation::Resolution>,
    #[serde(default,skip_serializing_if="Option::is_none")]
    pub confirmation_source:Option<Value>,
    pub state: ReceiptState,
}

#[derive(Clone, Deserialize, PartialEq, Serialize)]
pub struct BatchStep {
    pub request_id: String,
    pub index: usize,
}

#[derive(Clone, Copy, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptState {
    Planned,
    Dispatching,
    Returned,
    NotDispatched,
    Verified,
    Reconciled,
}

pub struct Journal {
    _lock: Option<File>,
    store: Store,
}

pub(crate) fn private_open(path: &Path, create: bool) -> std::io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(create)
        .create(create)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.uid() != crate::state::uid() || metadata.mode() & 0o077 != 0
    {
        return Err(std::io::ErrorKind::PermissionDenied.into());
    }
    Ok(file)
}

pub fn control(root: &Path, request: &Request) -> Result<Value> {
    let journal = Journal::lock(root)?;
    let now = now_ms();
    if request.deadline_ms <= now {return Err("deadline_exceeded");}
    let prior=journal.grant(&request.workspace_id)?;
    if request.method == "release_control" {
        if request.params != json!({}) {return Err("invalid_params");}
        let authority=request.authority.as_ref().ok_or("action_authority_required")?;
        let mut grant=prior.filter(|grant|grant.matches(request,authority,now)).ok_or("action_authority_changed")?;
        grant.expires_ms=0;journal.save_grant(&request.workspace_id,&grant)?;
        return Ok(json!({"released":true}));
    }
    let p:ClaimControl=serde_json::from_value(request.params.clone()).map_err(|_|"invalid_params")?;
    if !(1..=MAX_TTL_MS).contains(&p.ttl_ms) {return Err("invalid_params");}
    if let Some(mut grant)=prior.as_ref().filter(|g|g.active(request,now)).map(|g|Grant{generation:g.generation,grant_id:g.grant_id.clone(),client_id:g.client_id.clone(),service_epoch:g.service_epoch.clone(),expires_ms:g.expires_ms}) {
        if grant.client_id!=request.client_id {return Err("action_control_busy");}
        grant.expires_ms=now+p.ttl_ms;let result=grant.control_state();journal.save_grant(&request.workspace_id,&grant)?;return Ok(result);
    }
    let grant=Grant{generation:prior.map_or(0,|g|g.generation).checked_add(1).ok_or("action_authority_changed")?,
        grant_id:Uuid::new_v4().simple().to_string(),client_id:request.client_id.clone(),
        service_epoch:request.service_epoch.clone().ok_or("stale_service_epoch")?,expires_ms:now+p.ttl_ms};
    let result=grant.control_state();journal.save_grant(&request.workspace_id,&grant)?;Ok(result)
}

impl Journal {
    pub fn lock(root: &Path) -> Result<Self> {
        let lock = private_open(&root.join("action-control.lock"), true)
            .map_err(|_| crate::journal_store::failure(root,"journal_storage_unavailable"))?;
        FileExt::try_lock_exclusive(&lock).map_err(|_| "action_control_busy")?;
        let store=Store::open(root)?;
        Ok(Self {_lock:Some(lock),store})
    }
    pub fn read(root:&Path)->Result<Self> {
        if !root.join("action-journal.sqlite").exists() {return Self::lock(root);}
        Ok(Self{_lock:None,store:Store::read(root)?})
    }
    fn grant(&self,chat:&str)->Result<Option<Grant>> {
        let value:Option<String>=self.store.connection().query_row("SELECT payload FROM grants WHERE workspace_id=?",[chat],|r|r.get(0)).optional().map_err(|e|self.store.error(e))?;
        value.map(|v|serde_json::from_str(&v).map_err(|_|"journal_corrupt")).transpose()
    }
    fn save_grant(&self,chat:&str,grant:&Grant)->Result<()> {
        let connection=self.store.connection();
        let old:Option<String>=connection.query_row("SELECT payload FROM grants WHERE workspace_id=?",[chat],|r|r.get(0)).optional().map_err(|e|self.store.error(e))?;
        let mut payload:Value=old.map(|v|serde_json::from_str(&v).map_err(|_|"journal_corrupt")).transpose()?.unwrap_or_else(||json!({}));
        for (key,value) in serde_json::to_value(grant).map_err(|_|"journal_storage_unavailable")?.as_object().ok_or("journal_corrupt")? {payload[key]=value.clone();}
        connection.execute("INSERT INTO grants VALUES(?,?) ON CONFLICT(workspace_id) DO UPDATE SET payload=excluded.payload",params![chat,payload.to_string()]).map_err(|e|self.store.error(e))?;Ok(())
    }
    pub fn authorize(&self,request:&Request)->Result<()> {
        let supplied=request.authority.as_ref().ok_or("action_authority_required")?;
        let grant=self.grant(&request.workspace_id)?.ok_or("action_authority_changed")?;
        if !grant.matches(request,supplied,now_ms()) {return Err("action_authority_changed");}Ok(())
    }
    fn payload(&self,workspace:&str,id:&str)->Result<Option<String>> {
        self.store.connection().query_row("SELECT payload FROM receipts WHERE workspace_id=? AND request_id=?",params![workspace,id],|r|r.get(0)).optional().map_err(|e|self.store.error(e))
    }
    pub fn receipt(&self,workspace:&str,id:&str)->Result<Receipt> {
        serde_json::from_str(&self.payload(workspace,id)?.ok_or("action_receipt_missing")?).map_err(|_|"journal_corrupt")
    }

    pub fn check_new(&self, candidate: &Receipt) -> Result<()> {
        if self.payload(&candidate.workspace_id, &candidate.request_id)?.is_some() {
            return Err("action_already_recorded");
        }
        self.check_navigation(
            &candidate.workspace_id,
            &candidate.browser_id,
            &candidate.target_id,
            Some(&candidate.service_epoch),
        )?;
        Ok(())
    }

    pub fn check_navigation(
        &self,
        workspace: &str,
        browser: &str,
        target: &str,
        epoch: Option<&str>,
    ) -> Result<()> {
        if !self.pending(workspace,browser,target,epoch)?.is_empty() {return Err("action_receipt_pending");}
        Ok(())
    }
    pub fn pending(&self,workspace:&str,browser:&str,target:&str,epoch:Option<&str>)->Result<Vec<Receipt>> {
        let connection=self.store.connection();
        let mut statement=connection.prepare("SELECT r.payload FROM receipts r WHERE r.workspace_id=? AND r.target_id=? AND r.state IN ('dispatching','returned') AND (r.browser_id=? OR r.service_epoch!=?) AND NOT EXISTS(SELECT 1 FROM receipts p WHERE p.workspace_id=r.workspace_id AND p.request_id=json_extract(r.payload,'$.resolution.request_id') AND p.state='verified' AND json_extract(p.payload,'$.action')='verify' AND json_extract(p.payload,'$.confirmation.original_request_id')=r.request_id) ORDER BY r.request_id").map_err(|e|self.store.error(e))?;
        let rows=statement.query_map(params![workspace,target,browser,epoch.unwrap_or("")],|r|r.get::<_,String>(0)).map_err(|e|self.store.error(e))?;
        rows.map(|row|serde_json::from_str(&row.map_err(|e|self.store.error(e))?).map_err(|_|"journal_corrupt")).collect()
    }
    pub fn begin(&mut self,receipt:Receipt)->Result<()> {
        self.check_new(&receipt)?;
        let state=serde_json::to_value(receipt.state).map_err(|_|"journal_storage_unavailable")?;
        self.store.connection().execute("INSERT INTO receipts VALUES(?,?,?,?,?,?,?)",params![receipt.workspace_id,receipt.request_id,receipt.browser_id,receipt.target_id,receipt.service_epoch,state.as_str(),serde_json::to_string(&receipt).map_err(|_|"journal_storage_unavailable")?]).map_err(|e|self.store.error(e))?;Ok(())
    }

    /// One transaction publishes independent proof and resolves the original hold.
    /// Original state and expected condition remain unchanged.
    pub fn confirm(&mut self,request:&Request,original_id:&str,context:crate::confirmation::Context,kind:String,text:String,legacy:Option<&Value>)->Result<Receipt> {
        let original=self.receipt(&request.workspace_id,original_id)?;
        if let Some(resolution)=&original.resolution {
            let proof=self.receipt(&request.workspace_id,&resolution.request_id)?;
            if proof.state==ReceiptState::Verified && proof.action==Action::Verify && proof.confirmation.as_ref().is_some_and(|c|c.original_request_id==original_id) {return Ok(proof);}
            return Err("confirmation_receipt_invalid");
        }
        if original.action!=Action::Click || !matches!(original.state,ReceiptState::Dispatching|ReceiptState::Returned) {return Err("action_receipt_not_pending");}
        if let Some(source)=&original.submission_context {
            if !crate::confirmation::same_identity(source,&context) {return Err("submission_identity_changed");}
        } else {
            let source=legacy.ok_or("confirmation_source_missing")?;
            if original.target_id!=context.target_id || original.document_id!=context.document_id
                || source["target_id"]!=original.target_id || source["document_id"]!=original.document_id
                || !crate::confirmation::same_job_url(source["url"].as_str().unwrap_or(""),&context.url) || source["employer"]!=context.employer || source["profile"].as_str().is_some_and(|profile|profile!=context.profile) || source["call_id"].as_str().is_none_or(str::is_empty) {return Err("confirmation_source_missing");}
        }
        if !matches!(kind.as_str(),"sent_modal"|"application_status"|"workday_confirmation"|"workday_completion") || text.is_empty() || text.len()>512
            || kind=="workday_confirmation" && !crate::confirmation::workday_success_text(&text) {return Err("postcondition_unobserved");}
        if kind=="workday_completion" {
            let source=original.submission_context.as_ref().ok_or("confirmation_source_missing")?;
            let lower=text.to_ascii_lowercase();
            if source.target_id!=context.target_id || source.document_id!=context.document_id
                || !crate::confirmation::source_job_matches(source,true)
                || !crate::confirmation::workday_completion_url(&source.url,&context.url,true)
                || !lower.starts_with("application submitted")
                || !(lower.contains("thank you for your application") || lower.contains("you have successfully applied")) {return Err("submission_identity_changed");}
        }
        if self.payload(&request.workspace_id,&request.request_id)?.is_some(){return Err("action_already_recorded");}
        let proof:Receipt=serde_json::from_value(json!({"request_id":request.request_id,"workspace_id":request.workspace_id,
            "client_id":request.client_id,"service_epoch":request.service_epoch,"browser_id":request.params["browser_id"],
            "target_id":context.target_id,"document_id":context.document_id,"action":"verify","state":"verified",
            "confirmation":{"original_request_id":original_id,"context":context,"kind":kind,"text":text,"observed_ms":now_ms()}})).map_err(|_|"confirmation_receipt_invalid")?;
        let mut payload:Value=serde_json::from_str(&self.payload(&request.workspace_id,original_id)?.ok_or("action_receipt_missing")?).map_err(|_|"journal_corrupt")?;
        payload["resolution"]=json!({"request_id":proof.request_id});
        if original.submission_context.is_none() {let mut anchored=context.clone();anchored.url=legacy.and_then(|v|v["url"].as_str()).ok_or("confirmation_source_missing")?.into();payload["submission_context"]=serde_json::to_value(anchored).map_err(|_|"confirmation_receipt_invalid")?;payload["confirmation_source"]=legacy.cloned().unwrap_or(Value::Null);}
        let mut connection=self.store.connection();let tx=connection.transaction().map_err(|e|self.store.error(e))?;
        tx.execute("INSERT INTO receipts VALUES(?,?,?,?,?,?,?)",params![proof.workspace_id,proof.request_id,proof.browser_id,proof.target_id,proof.service_epoch,"verified",serde_json::to_string(&proof).map_err(|_|"confirmation_receipt_invalid")?]).map_err(|e|self.store.error(e))?;
        tx.execute("UPDATE receipts SET payload=? WHERE workspace_id=? AND request_id=?",params![payload.to_string(),request.workspace_id,original_id]).map_err(|e|self.store.error(e))?;
        tx.commit().map_err(|e|self.store.error(e))?;Ok(proof)
    }

    pub fn finish(
        &mut self,
        workspace: &str,
        id: &str,
        state: ReceiptState,
        result_document_id: Option<&str>,
    ) -> Result<Receipt> {
        let mut receipt=self.receipt(workspace,id)?;
        if state == ReceiptState::Reconciled
            && !(matches!(
                receipt.action,
                Action::Fill | Action::Select | Action::Check
            ) || receipt.action == Action::Upload && receipt.state == ReceiptState::Returned)
        {
            return Err("input_not_convergent");
        }
        if !matches!(
            receipt.state,
            ReceiptState::Dispatching | ReceiptState::Returned
        ) && (receipt.state != state
            || receipt.result_document_id.as_deref() != result_document_id)
        {
            return Err("action_receipt_not_pending");
        }
        receipt.state = state;
        receipt.result_document_id = result_document_id.map(str::to_owned);
        let mut payload:Value=serde_json::from_str(&self.payload(workspace,id)?.ok_or("action_receipt_missing")?).map_err(|_|"journal_corrupt")?;
        payload["state"]=json!(state);
        if let Some(document)=result_document_id {payload["result_document_id"]=json!(document);} else {payload.as_object_mut().ok_or("journal_corrupt")?.remove("result_document_id");}
        self.store.connection().execute("UPDATE receipts SET state=?,payload=? WHERE workspace_id=? AND request_id=?",params![payload["state"].as_str(),payload.to_string(),workspace,id]).map_err(|e|self.store.error(e))?;
        Ok(receipt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt(action: Action) -> Receipt {
        serde_json::from_value(
            json!({"request_id":"input", "workspace_id":"chat", "client_id":"agent",
            "service_epoch":"old", "browser_id":"comet", "target_id":"tab", "document_id":"doc",
            "action":action,"state":"dispatching"}),
        )
        .unwrap()
    }

    fn confirmation_request()->Request {
        serde_json::from_value(json!({"protocol_version":1,"request_id":"proof","instance_id":"test","application_id":"test","workspace_id":"chat","client_id":"agent","state_root":"/tmp/test","service_epoch":"new","deadline_ms":now_ms()+5000,"expected_binding":null,"authority":null,"method":"confirm_submission","params":{"browser_id":"comet"}})).unwrap()
    }
    #[test]
    fn workday_completion_cannot_cross_document_tenant_or_session() {
        let root=tempfile::tempdir().unwrap();let mut journal=Journal::lock(root.path()).unwrap();
        let mut source=confirmation_context();source.url="https://employer.wd1.myworkdayjobs.com/en-US/Careers/job/Remote/Lead_R12345-1/apply".into();source.job_id="R12345-1".into();
        let mut original=receipt(Action::Click);original.state=ReceiptState::Returned;original.submission_context=Some(source.clone());journal.begin(original).unwrap();
        let mut current=source.clone();current.url="https://employer.wd1.myworkdayjobs.com/en-US/Careers/jobTasks/completed/application".into();
        for key in ["target_id","document_id","profile","session_digest","account_digest","job_id","employer"] {
            let mut value=serde_json::to_value(&current).unwrap();value[key]=json!("different");
            assert!(journal.confirm(&confirmation_request(),"input",serde_json::from_value(value).unwrap(),"workday_completion".into(),"Application Submitted\nThank you for your application.".into(),None).is_err(),"{key}");
        }
        let mut other=current.clone();other.url=other.url.replace("Careers","OtherTenant");
        assert!(journal.confirm(&confirmation_request(),"input",other,"workday_completion".into(),"Application Submitted\nThank you for your application.".into(),None).is_err());
        assert!(journal.confirm(&confirmation_request(),"input",current.clone(),"workday_completion".into(),"Application Submitted\nReview your application.".into(),None).is_err());
        let proof=journal.confirm(&confirmation_request(),"input",current,"workday_completion".into(),"Application Submitted\nThank you for your application.".into(),None).unwrap();
        assert_eq!(proof.confirmation.unwrap().context.job_id,source.job_id);
        assert!(journal.receipt("chat","input").unwrap().state==ReceiptState::Returned);
    }
    fn confirmation_context()->crate::confirmation::Context {
        serde_json::from_value(json!({"url":"https://www.linkedin.com/jobs/view/1/","job_id":"1","employer":"Iris","account_digest":"person","profile":"comet","session_digest":"session","target_id":"tab","document_id":"doc"})).unwrap()
    }
    #[test]
    fn independent_confirmation_preserves_original_and_is_atomic_durable_and_idempotent() {
        let root=tempfile::tempdir().unwrap();let mut journal=Journal::lock(root.path()).unwrap();
        let mut original=receipt(Action::Click);original.state=ReceiptState::Returned;original.submission_context=Some(confirmation_context());original.expected=Some(Expected{selector:"#wrong".into(),text:"Application sent".into(),url:None});journal.begin(original.clone()).unwrap();
        let mut other=confirmation_context();other.target_id="other-tab".into();other.document_id="other-doc".into();
        let proof=journal.confirm(&confirmation_request(),"input",other.clone(),"application_status".into(),"Application submitted".into(),None).unwrap();
        let saved=journal.receipt("chat","input").unwrap();assert!(saved.state==original.state&&saved.expected==original.expected);assert_eq!(saved.resolution.unwrap().request_id,proof.request_id);assert!(journal.pending("chat","comet","tab",Some("new")).unwrap().is_empty());drop(journal);
        let mut reopened=Journal::lock(root.path()).unwrap();let again=reopened.confirm(&confirmation_request(),"input",other,"application_status".into(),"Application submitted".into(),None).unwrap();assert_eq!(again.request_id,proof.request_id);
    }
    #[test]
    fn legacy_confirmation_accepts_tracking_url_changes_but_preserves_anchored_source() {
        let root=tempfile::tempdir().unwrap();let mut journal=Journal::lock(root.path()).unwrap();let mut original=receipt(Action::Click);original.state=ReceiptState::Returned;journal.begin(original).unwrap();
        let source=json!({"target_id":"tab","document_id":"doc","url":"https://www.linkedin.com/jobs/view/1/?trk=old","employer":"Iris","profile":"comet","call_id":"saved-observation"});
        journal.confirm(&confirmation_request(),"input",confirmation_context(),"application_status".into(),"Application submitted".into(),Some(&source)).unwrap();
        let original=journal.receipt("chat","input").unwrap();assert_eq!(original.submission_context.unwrap().url,source["url"].as_str().unwrap());assert!(original.state==ReceiptState::Returned);
    }
    #[test]
    fn confirmation_rejects_identity_changes_and_rolls_back_if_resolution_write_fails() {
        let root=tempfile::tempdir().unwrap();let mut journal=Journal::lock(root.path()).unwrap();let mut original=receipt(Action::Click);original.state=ReceiptState::Returned;original.submission_context=Some(confirmation_context());journal.begin(original).unwrap();
        for key in ["job_id","employer","account_digest","profile","session_digest"] {
            let mut value=serde_json::to_value(confirmation_context()).unwrap();value[key]=json!("other");let changed=serde_json::from_value(value).unwrap();
            assert_eq!(journal.confirm(&confirmation_request(),"input",changed,"application_status".into(),"Application submitted".into(),None).err(),Some("submission_identity_changed"));
        }
        journal.store.connection().execute_batch("CREATE TRIGGER fail_confirmation BEFORE UPDATE ON receipts BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
        assert!(journal.confirm(&confirmation_request(),"input",confirmation_context(),"application_status".into(),"Application submitted".into(),None).is_err());
        assert!(journal.receipt("chat","proof").is_err());assert!(journal.receipt("chat","input").unwrap().resolution.is_none());assert_eq!(journal.pending("chat","comet","tab",Some("new")).unwrap().len(),1);
    }

    #[test]
    fn pending_is_scoped_to_original_tab_across_restarts() {
        let root = tempfile::tempdir().unwrap();
        let mut journal = Journal::lock(root.path()).unwrap();
        journal.begin(receipt(Action::Fill)).unwrap();
        assert_eq!(
            journal.check_navigation("chat", "new-comet", "tab", Some("new")),
            Err("action_receipt_pending")
        );
        assert!(journal
            .check_navigation("chat", "chrome", "other-tab", Some("new"))
            .is_ok());
        assert!(journal
            .check_navigation("other-chat", "comet", "tab", Some("old"))
            .is_ok());
        assert_eq!(
            journal
                .pending("chat", "new-comet", "tab", Some("new")).unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn only_convergent_inputs_can_be_retired_without_success() {
        for action in [
            Action::Click,
            Action::Press,
            Action::Upload,
            Action::Fill,
            Action::Select,
            Action::Check,
        ] {
            let root = tempfile::tempdir().unwrap();
            let mut journal = Journal::lock(root.path()).unwrap();
            journal.begin(receipt(action)).unwrap();
            let result = journal.finish("chat", "input", ReceiptState::Reconciled, Some("doc"));
            if matches!(action, Action::Fill | Action::Select | Action::Check) {
                assert!(result.is_ok());
                assert!(journal
                    .check_navigation("chat", "comet", "tab", Some("old"))
                    .is_ok());
                assert!(journal
                    .finish("chat", "input", ReceiptState::Verified, Some("doc"))
                    .is_err());
            } else {
                assert_eq!(result.err(), Some("input_not_convergent"));
                assert_eq!(
                    journal.check_navigation("chat", "comet", "tab", Some("old")),
                    Err("action_receipt_pending")
                );
            }
        }
    }

    #[test]
    fn acknowledged_upload_can_retire_only_its_hold() {
        let root = tempfile::tempdir().unwrap();
        let mut journal = Journal::lock(root.path()).unwrap();
        let mut upload = receipt(Action::Upload);
        upload.state = ReceiptState::Returned;
        journal.begin(upload).unwrap();
        assert!(journal
            .finish("chat", "input", ReceiptState::Reconciled, None)
            .is_ok());
        assert!(journal
            .finish("chat", "input", ReceiptState::Verified, None)
            .is_err());
    }

    #[test]
    fn renewal_extends_expiry_without_changing_generation_or_owner() {
        let root = tempfile::tempdir().unwrap();
        let request = crate::protocol::Request {
            protocol_version: 1,
            request_id: "claim".into(),
            instance_id: "test".into(),
            application_id: "test".into(),
            workspace_id: "chat".into(),
            client_id: "user".into(),
            state_root: root.path().to_string_lossy().into(),
            service_epoch: Some("epoch".into()),
            deadline_ms: now_ms() + 5000,
            expected_binding: None,
            authority: None,
            method: "claim_control".into(),
            params: json!({"ttl_ms":1000}),
        };
        let first = control(root.path(), &request).unwrap();
        let mut renewal = request.clone();
        renewal.params = json!({"ttl_ms":3000});
        let renewed = control(root.path(), &renewal).unwrap();
        assert_eq!(first["authority"], renewed["authority"]);
        assert!(renewed["expires_ms"].as_u64().unwrap() > first["expires_ms"].as_u64().unwrap());
        renewal.client_id = "other-client".into();
        assert_eq!(
            control(root.path(), &renewal).unwrap_err(),
            "action_control_busy"
        );
    }
}

#[cfg(test)]
mod durable_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    fn receipt(id:usize,state:ReceiptState)->Receipt {
        serde_json::from_value(json!({"request_id":format!("r-{id}"),"workspace_id":format!("chat-{}",id%3),"client_id":"agent","service_epoch":"old","browser_id":"browser","target_id":format!("tab-{id}"),"document_id":"doc","state":state})).unwrap()
    }
    fn write_private(path:&Path,bytes:&[u8]) {std::fs::write(path,bytes).unwrap();std::fs::set_permissions(path,std::fs::Permissions::from_mode(0o600)).unwrap();}
    #[test]
    fn imports_all_legacy_history_once_and_keeps_unknown_evidence() {
        let root=tempfile::tempdir().unwrap();
        let mut values:Vec<Value>=(0..1024).map(|i|json!(receipt(i,if i==0{ReceiptState::Returned}else{ReceiptState::Verified}))).collect();
        values[0]["future_evidence"]=json!({"keep":"exactly"});
        let bytes=serde_json::to_vec(&values).unwrap();write_private(&root.path().join("action-receipts.json"),&bytes);
        write_private(&root.path().join("action-backend"),b"standalone");
        {
            let mut j=Journal::lock(root.path()).unwrap();
            assert_eq!(j.pending("chat-0","new","tab-0",Some("new")).unwrap().len(),1);
            j.finish("chat-0","r-0",ReceiptState::Verified,None).unwrap();
            let payload:Value=serde_json::from_str(&j.payload("chat-0","r-0").unwrap().unwrap()).unwrap();
            assert_eq!(payload["future_evidence"],values[0]["future_evidence"]);
        }
        assert_eq!(std::fs::read(root.path().join("action-receipts.json")).unwrap(),bytes);
        assert_eq!(std::fs::read_to_string(root.path().join("action-backend")).unwrap(),"standalone-sqlite-v1");
        let mut j=Journal::lock(root.path()).unwrap();
        assert_eq!(j.store.connection().query_row("SELECT count(*) FROM receipts",[],|r|r.get::<_,i64>(0)).unwrap(),1024);
        j.begin(receipt(1024,ReceiptState::Dispatching)).unwrap();
        assert!(j.begin(receipt(1024,ReceiptState::Dispatching)).is_err());
    }
    #[test]
    fn ten_thousand_receipts_survive_restart_with_pending_fences() {
        let root=tempfile::tempdir().unwrap();
        {
            let mut j=Journal::lock(root.path()).unwrap();
            for i in 0..10001 {j.begin(receipt(i,if i==10000{ReceiptState::Returned}else{ReceiptState::Verified})).unwrap();}
        }
        let j=Journal::read(root.path()).unwrap();
        for i in [0,1023,1024,9999,10000] {assert!(j.receipt(&format!("chat-{}",i%3),&format!("r-{i}")).unwrap()==receipt(i,if i==10000{ReceiptState::Returned}else{ReceiptState::Verified}));}
        assert_eq!(j.check_navigation("chat-1","new-browser","tab-10000",Some("new")),Err("action_receipt_pending"));
        assert!(j.check_navigation("chat-2","new-browser","tab-10000",Some("new")).is_ok());
    }
    #[test]
    fn failed_import_rolls_back_and_does_not_publish_storage_fence() {
        let root=tempfile::tempdir().unwrap();
        write_private(&root.path().join("action-backend"),b"standalone");
        let payload=serde_json::to_vec(&vec![receipt(0,ReceiptState::Verified);2]).unwrap();
        write_private(&root.path().join("action-receipts.json"),&payload);
        assert!(Journal::lock(root.path()).is_err());
        assert_eq!(std::fs::read(root.path().join("action-receipts.json")).unwrap(),payload);
        assert_eq!(std::fs::read(root.path().join("action-backend")).unwrap(),b"standalone");
        write_private(&root.path().join("action-receipts.json"),&serde_json::to_vec(&vec![receipt(0,ReceiptState::Verified)]).unwrap());
        let j=Journal::lock(root.path()).unwrap();assert!(j.receipt("chat-0","r-0").is_ok());
    }
    #[test]
    fn disk_full_preserves_history_and_read_access() {
        let root=tempfile::tempdir().unwrap();let mut j=Journal::lock(root.path()).unwrap();
        j.begin(receipt(0,ReceiptState::Verified)).unwrap();
        let pages:i64=j.store.connection().query_row("PRAGMA page_count",[],|r|r.get(0)).unwrap();
        j.store.connection().execute_batch(&format!("PRAGMA max_page_count={pages}")).unwrap();
        let mut huge=receipt(1,ReceiptState::Dispatching);huge.document_id="x".repeat(128*1024);
        assert_eq!(j.begin(huge),Err("journal_storage_full"));
        let read=Journal::read(root.path()).unwrap();assert!(read.receipt("chat-0","r-0").is_ok());
        assert_eq!(crate::journal_store::status(root.path())["ready"],false);
    }
    #[test]
    fn readonly_storage_blocks_writes_but_keeps_receipt_lookup() {
        use std::os::unix::fs::PermissionsExt;
        let root=tempfile::tempdir().unwrap();let mut journal=Journal::lock(root.path()).unwrap();
        journal.begin(receipt(0,ReceiptState::Verified)).unwrap();drop(journal);
        let path=root.path().join("action-journal.sqlite");std::fs::set_permissions(&path,std::fs::Permissions::from_mode(0o400)).unwrap();
        assert!(Journal::lock(root.path()).is_err());assert!(Journal::read(root.path()).unwrap().receipt("chat-0","r-0").is_ok());
        assert_eq!(crate::journal_store::status(root.path())["ready"],false);
        for suffix in ["","-wal","-shm"] {let file=root.path().join(format!("action-journal.sqlite{suffix}"));if file.exists(){std::fs::set_permissions(&file,std::fs::Permissions::from_mode(0o600)).unwrap();}}assert!(Journal::lock(root.path()).is_ok());assert_eq!(crate::journal_store::status(root.path())["ready"],true);
    }
    #[test]
    fn more_than_a_thousand_grant_histories_keep_monotonic_generations() {
        let root=tempfile::tempdir().unwrap();let j=Journal::lock(root.path()).unwrap();
        for i in 0..1100 {j.save_grant(&format!("c-{i}"),&Grant{generation:1,grant_id:"id".into(),client_id:"agent".into(),service_epoch:"old".into(),expires_ms:0}).unwrap();}
        assert_eq!(j.grant("c-0").unwrap().unwrap().generation,1);
        assert_eq!(j.store.connection().query_row("SELECT count(*) FROM grants",[],|r|r.get::<_,i64>(0)).unwrap(),1100);
    }
}
