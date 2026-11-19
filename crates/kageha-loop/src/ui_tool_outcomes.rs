//! Trusted adapter metadata for recovery. Model arguments never choose retryability.
use crate::ui_tools::Tools;
use kageha_contracts::execution::{RecoveryAction, ToolCall, ToolOutcome, ToolOutcomeKind};
use serde_json::{json, Value};
use std::io;

pub(crate) fn revision(tools:&Tools<'_>,call:&ToolCall)->io::Result<Option<String>> {
    if call.name=="integration_read" && call.arguments["integration"]=="gmail" {
        // The same revision keys the read cache, for the tool and the local connector API.
        let agent=tools.persona["id"].as_str().unwrap_or("main");
        return crate::ui_read_cache::connector_revision(tools.db,agent,"gmail").map(Some);
    }
    if matches!(call.name.as_str(),"report_blocker"|"record_outcome_evidence") {
        let root=crate::ui_task_completion::scope(tools.db,tools.turn)?;
        let attempts=crate::ui_task_completion::failed_attempt_candidates(tools.db,tools.chat,tools.turn,&root)?;
        let contract={let db=tools.db.lock().map_err(|_|io::Error::other("Completion state unavailable"))?;
            crate::ui_task_completion::load(&db,tools.chat,&root)?};
        let id=call.arguments["outcome_id"].as_str().unwrap_or("");
        return Ok(Some(outcome_recovery_revision(&contract,id,&attempts)));
    }
    if call.name.starts_with("browser_") && !matches!(call.name.as_str(),"browser_receipts"|"browser_reconcile")
        && crate::browser_client::configured() {
        let mut client=crate::browser_client::Client::connect(tools.chat,"agent")?;
        let selection=client.selection()?;
        return Ok(Some(kageha_policy::input_digest(&json!([selection["browser_id"],selection["target_id"],selection["document_id"]]))));
    }
    Ok(None)
}
fn outcome_recovery_revision(contract:&kageha_workflow::completion::Contract,id:&str,attempts:&[Value])->String {
    // Amendment logs, unrelated outcomes and rejected references are diagnostics,
    // not changes to the requirement or the evidence available for this branch.
    kageha_policy::input_digest(&json!([
        contract.outcomes.iter().find(|o|o.id==id),
        contract.evidence.iter().filter(|e|e.outcome_id==id).collect::<Vec<_>>(),
        contract.blockers.iter().filter(|b|b.outcome_id==id).collect::<Vec<_>>(),
        attempts,
    ]))
}

#[cfg(test)]
mod recovery_revision_tests {
    use super::*;
    #[test]
    fn terminal_submission_evidence_is_blocked_without_advertising_a_retry() {
        let fixture=crate::audited_session_tests::Fixture::new();let tools=fixture.tools(json!({"id":"main"}));
        let call=ToolCall{id:"attachment".into(),name:"record_outcome_evidence".into(),arguments:json!({"outcome_id":"submit","reference":"original"})};
        let result=Err(io::Error::other(json!({"error_code":"invalid_outcome_evidence","recovery":{"terminal":true,"original_request_ids":["original"]}}).to_string()));
        let outcome=classify(&tools,&call,&result).unwrap();
        assert_eq!(outcome.code,"confirmation_unavailable");assert_eq!(outcome.kind,ToolOutcomeKind::Blocked);
        assert_eq!(outcome.recovery,Some(RecoveryAction::ChangeStrategy));
    }
    #[test]
    fn unrelated_contract_churn_does_not_refresh_an_evidence_retry_budget() {
        use kageha_workflow::completion::{Contract,RequiredOutcome,OutcomeKind,Origin,Amendment,Evidence};
        let target=RequiredOutcome{id:"submit".into(),description:"Submit exact application".into(),kind:OutcomeKind::Action,verify:String::new(),origin:Origin::User,proof:None};
        let mut c=Contract::default();c.outcomes.push(target.clone());
        let revision=outcome_recovery_revision(&c,"submit",&[]);
        let mut other=target.clone();other.id="other".into();c.outcomes.push(other.clone());
        c.history.push(Amendment{id:"other".into(),before:other.clone(),after:other,reason:"Unrelated description".into(),weakened:false,at:1.0});
        c.evidence.push(Evidence{outcome_id:"other".into(),reference:"other-proof".into(),turn:"turn".into()});
        assert_eq!(revision,outcome_recovery_revision(&c,"submit",&[]));
        assert_ne!(revision,outcome_recovery_revision(&c,"submit",&[json!({"reference":"actual-new-failure"})]));
        c.outcomes[0].description="A genuinely changed obligation".into();
        assert_ne!(revision,outcome_recovery_revision(&c,"submit",&[]));
    }
}
fn known_read(name:&str)->bool {
    matches!(name,"read_file"|"list_workspace_files"|"web_search"|"news_search"|"web_fetch"|"knowledge_search"|"knowledge_list"|"integration_read"|"browser_snapshot"|"browser_inspect"|"browser_validate"|"browser_tabs"|"browser_receipts"|"computer_observe"|"computer_receipt"|"mcp_receipts"|"remote_receipts")
}
fn file_tool(name:&str)->bool { matches!(name,"read_file"|"write_file"|"edit_file") }
fn shell_tool(name:&str)->bool { matches!(name,"shell"|"shell_job") }
pub(crate) fn classify(tools:&Tools<'_>,call:&ToolCall,result:&io::Result<String>)->Option<ToolOutcome> {
    let body=match result {Ok(content)=>content.clone(),Err(error)=>error.to_string()};
    let value=serde_json::from_str::<Value>(&body).unwrap_or(Value::Null);
    let value=value["tool_result"].as_str().and_then(|inner|serde_json::from_str::<Value>(inner).ok()).unwrap_or(value);
    let connector=&value["connector_outcome"];
    let resource=if connector["connector"].is_string(){format!("connector:{}",connector["connector"].as_str().unwrap())}
        else { ["url","path","integration","target_id","observation_id","request_id"].into_iter()
            .find_map(|key|call.arguments[key].as_str().map(|value|format!("{key}:{value}"))).unwrap_or_else(||call.name.clone()) };
    let revision=revision(tools,call).ok().flatten().unwrap_or_else(||
        json!([connector["revision"],connector["agents"],value["document_id"],value["revision"]]).to_string());
    let (kind,code,recovery)=if matches!(connector["state"].as_str(),Some("disconnected"|"needs_verification"|"agent_not_assigned")) {
        (ToolOutcomeKind::Blocked,connector["state"].as_str().unwrap().to_owned(),Some(RecoveryAction::RequestInput))
    } else if result.is_ok() && call.name == "browser_validate" && value["validation"]["passed"] == false {
        // Transport success is not validation success. Repeated failed checks
        // must exhaust instead of clearing the failure ledger on every call.
        (ToolOutcomeKind::Recoverable,"validation_failed".into(),Some(RecoveryAction::ChangeStrategy))
    } else if result.is_ok() && matches!(call.name.as_str(), "shell" | "remote_exec" | "shell_job")
        && value["exit_code"].as_i64().is_some_and(|code|code != 0) {
        (ToolOutcomeKind::Recoverable,"command_failed".into(),Some(RecoveryAction::ChangeStrategy))
    } else if result.is_ok() {
        let evidence=value["receipt"]["request_id"].as_str().filter(|_|value["receipt"]["state"]=="verified"
            && matches!(call.name.as_str(),"browser_click"|"browser_fill"|"browser_check"|"browser_select"|"browser_press"|"browser_upload"|"browser_fill_credential"|"browser_receipts"|"browser_reconcile"|"computer_execute"|"computer_receipt"))
            .map(|id|vec![format!("receipt:{id}")]).unwrap_or_default();
        let revision=if matches!(call.name.as_str(),"browser_snapshot"|"browser_inspect") {
            kageha_runtime::recovery::semantic_revision(&value)
        } else {revision};
        return Some(ToolOutcome{kind:ToolOutcomeKind::Success,code:"ok".into(),resource,revision,recovery:None,evidence});
    } else {
        let code=value["error_code"].as_str().or_else(||value["error"]["code"].as_str()).or_else(||value["detail"]["code"].as_str())?;
        if matches!(code,"action_receipt_pending"|"action_outcome_uncertain"|"close_outcome_unknown"|"script_upload_unconfirmed"|"form_refresh_required")
            && call.name.starts_with("browser_") {
            // The adapter fences further input. Only observation/reconciliation
            // can resolve this; it is never an automatically retryable action.
            (ToolOutcomeKind::Blocked,code.into(),Some(RecoveryAction::ChangeStrategy))
        } else if call.name.starts_with("browser_") && matches!(code,"action_journal_capacity"|"journal_storage_full"|"journal_storage_unavailable"|"journal_corrupt"|"journal_migration_failed"|"element_not_editable"|"element_not_actionable"|"input_readonly"|"input_disabled"|"unsupported_input_type"|"numeric_input_invalid"|"frame_scope"|"action_control_busy") {
            (ToolOutcomeKind::Blocked,code.into(),Some(RecoveryAction::ChangeStrategy))
        } else if matches!(code,"capability_not_found"|"capability_discovery_exhausted"|"tool_unavailable_now") {
            (ToolOutcomeKind::Blocked,code.into(),Some(RecoveryAction::ChangeStrategy))
        } else if matches!(code,"disk_low"|"host_file_permission_required") {
            // The user can free disk space or grant the host OS permission.
            (ToolOutcomeKind::Blocked,code.into(),Some(RecoveryAction::RequestInput))
        } else if shell_tool(&call.name) && code=="command_timeout" {
            // The command keeps running as a job; follow it with shell_job.
            (ToolOutcomeKind::Recoverable,code.into(),Some(RecoveryAction::ChangeStrategy))
        } else if shell_tool(&call.name) && code=="policy_denied" {
            // The agent can now call the approval-bearing tool. A pending user
            // prerequisite here would stay attached to the original shell arguments
            // even after an approved import or escalated command succeeds.
            (ToolOutcomeKind::Blocked,code.into(),Some(RecoveryAction::ChangeStrategy))
        } else if shell_tool(&call.name) && matches!(code,"approval_denied"|"sandbox_unavailable") {
            // `approval_denied`: the user declined the escalated command. It is never
            // retried; use another method or a blocker report that references the approval.
            (ToolOutcomeKind::Blocked,code.into(),Some(RecoveryAction::ChangeStrategy))
        } else if call.name == "ask_human" && code == "invalid_arguments" {
            (ToolOutcomeKind::Recoverable,code.into(),Some(RecoveryAction::ChangeStrategy))
        } else if shell_tool(&call.name) && matches!(code,"invalid_arguments"|"job_not_found"|"shell_error") {
            (ToolOutcomeKind::Recoverable,code.into(),Some(RecoveryAction::ChangeStrategy))
        } else if file_tool(&call.name) && matches!(code,"path_outside_workspace"|"path_reserved") {
            (ToolOutcomeKind::Blocked,code.into(),Some(RecoveryAction::ChangeStrategy))
        } else if file_tool(&call.name) && matches!(code,"invalid_arguments"|"invalid_path"|"invalid_edit"|"edit_not_found"|"edit_ambiguous"|"file_not_found"|"not_a_file"|"not_utf8") {
            (ToolOutcomeKind::Recoverable,code.into(),Some(RecoveryAction::ChangeStrategy))
        } else if code=="browser_profile_disconnected" {
            // Only the user can reconnect a signed-in profile; opening it again cannot help.
            (ToolOutcomeKind::Blocked,code.into(),Some(RecoveryAction::RequestInput))
        } else if code=="browser_not_connected" {
            (ToolOutcomeKind::Recoverable,code.into(),Some(RecoveryAction::ChangeStrategy))
        } else if code=="browser_user_control" {
            (ToolOutcomeKind::Blocked,code.into(),Some(RecoveryAction::RequestInput))
        } else if code=="invalid_outcome_evidence" && value["recovery"]["terminal"]==true
            && value["recovery"]["original_request_ids"].as_array().is_some_and(|ids|!ids.is_empty()) {
            (ToolOutcomeKind::Blocked,"confirmation_unavailable".into(),Some(RecoveryAction::ChangeStrategy))
        } else if matches!(code,"invalid_outcome_evidence"|"invalid_blocker"|"submission_proof_locked"|"postcondition_unobserved"|"confirmation_source_missing"|"confirmation_identity_missing"|"confirmation_provider_unsupported"|"confirmation_tab_unavailable"|"submission_identity_changed"|"confirmation_receipt_invalid"|"confirmation_document_changed") {
            (ToolOutcomeKind::Recoverable,code.into(),Some(RecoveryAction::ChangeStrategy))
        } else if code=="research_article_unverified" {
            // The page was read but is not the expected article. Reading it again with the
            // same expected_title cannot change that; the next step is another source or a
            // read without expected_title.
            (ToolOutcomeKind::Recoverable,code.into(),Some(RecoveryAction::ChangeStrategy))
        } else if code=="required_outcomes_missing" {
            (ToolOutcomeKind::Recoverable,code.into(),Some(RecoveryAction::ChangeStrategy))
        } else if matches!(code,"research_recovery_exhausted"|"research_source_unavailable"|"authorization_required"|"agent_not_assigned"|"needs_verification") {
            (ToolOutcomeKind::Blocked,code.into(),Some(RecoveryAction::ChangeStrategy))
        } else if known_read(&call.name) && matches!(code,"observation_too_large"|"research_response_too_large"|"research_read_failed") {
            (ToolOutcomeKind::Recoverable,code.into(),Some(RecoveryAction::ChangeStrategy))
        } else if call.name.starts_with("browser_")
            && matches!(code,"stale_observation"|"stale_document"|"stale_binding"|"document_changed"|"browser_binding_changed"|"observation_changed"|"observed_control_ambiguous"|"observed_password_required"|"element_not_observed_in_main_frame") {
            // The action did not run. The next step is a fresh observation, not a replay.
            (ToolOutcomeKind::Recoverable,code.into(),Some(RecoveryAction::RefreshObservation))
        } else if known_read(&call.name) && value["retryable"]==true && matches!(code,"network_error"|"transport_error"|"temporarily_unavailable") {
            (ToolOutcomeKind::Recoverable,code.into(),Some(RecoveryAction::RetryRead))
        } else if matches!(code,"tool_failed"|"transient_upstream") {
            // `tool_failed`: a plain-text failure wrapped by ui_tool_errors::normalize.
            // `transient_upstream`: the Host already retried the read 3 times, so this is
            // ChangeStrategy, never RetryRead; the runtime must not replay it again.
            (ToolOutcomeKind::Recoverable,code.into(),Some(RecoveryAction::ChangeStrategy))
        } else { return None; }
    };
    Some(ToolOutcome{kind,code,resource,revision,recovery,evidence:vec![]})
}

/// Only changes to actual receipt or observed confirmation state permit another attempt.
pub(crate) fn submission_revision(original:&Value,fresh:&Value)->String {
 kageha_policy::input_digest(&json!([original["receipt"]["state"],original["receipt"]["resolution"],fresh["context"]["job_id"],fresh["context"]["employer"],fresh["context"]["account_digest"],fresh["context"]["profile"],fresh["context"]["session_digest"],fresh["confirmation"]]))
}
