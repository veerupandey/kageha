//! Submission-scoped receipt discovery and independent site confirmation provenance.
use crate::{ui_store, ui_task_completion, webui::Database};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use std::io;

fn failure(code: &str) -> io::Error {
    io::Error::other(json!({"error_code":code,"retryable":false,"summary":"Recover the original submission and fresh confirmation for the same application. Do not submit again."}).to_string())
}
pub(crate) fn record(db: &Database, chat: &str, turn: &str, request: &str) -> io::Result<Value> {
    let root = ui_task_completion::scope(db, turn)?;
    let c = db
        .lock()
        .map_err(|_| failure("evidence_storage_unavailable"))?;
    let v = ui_store::get(&c, "application-submission", &format!("{chat}/{request}"))
        .map_err(io::Error::other)?
        .ok_or_else(|| failure("submission_not_found"))?;
    if v["chat"] != chat || v["scope"] != root {
        return Err(failure("wrong_request_scope"));
    }
    Ok(v)
}
pub(crate) fn requests(
    db: &Database,
    chat: &str,
    turn: &str,
    outcome: &str,
) -> io::Result<Vec<String>> {
    let root = ui_task_completion::scope(db, turn)?;
    let c = db
        .lock()
        .map_err(|_| failure("evidence_storage_unavailable"))?;
    let mut q=c.prepare("SELECT json_extract(body,'$.request_id') FROM ui_documents WHERE kind='application-submission' AND json_extract(body,'$.chat')=? AND json_extract(body,'$.scope')=? AND json_extract(body,'$.outcome_id')=? ORDER BY updated DESC LIMIT 51").map_err(io::Error::other)?;
    let rows = q
        .query_map(params![chat, root, outcome], |r| r.get(0))
        .map_err(io::Error::other)?;
    let rows = rows
        .collect::<rusqlite::Result<Vec<String>>>()
        .map_err(io::Error::other)?;
    if rows.len() > 50 {
        return Err(failure("receipt_history_requires_request_id"));
    }
    Ok(rows)
}
fn body(raw: &str) -> Option<Value> {
    let v: Value = serde_json::from_str(raw.strip_prefix("Tool failed: ").unwrap_or(raw)).ok()?;
    v["tool_result"]
        .as_str()
        .and_then(|s| serde_json::from_str(s).ok())
        .or(Some(v))
}
/// Backfill only from a saved browser-owned observation of the original document.
pub(crate) fn legacy_source(db: &Database, chat: &str, record: &Value) -> io::Result<Value> {
    let target = &record["proof"]["target_id"];
    let document = &record["proof"]["document_id"];
    let c = db
        .lock()
        .map_err(|_| failure("evidence_storage_unavailable"))?;
    let anchor:Option<i64>=c.query_row("SELECT sequence FROM ui_events WHERE chat_id=? AND turn_id=? AND event='event' AND json_extract(payload,'$.kind')='outcome_snapshot' AND instr(payload,?)>0 ORDER BY sequence LIMIT 1",params![chat,record["turn"].as_str(),format!("application-{}",record["request_id"].as_str().unwrap_or(""))],|r|r.get(0)).optional().map_err(io::Error::other)?;
    let anchor = anchor.ok_or_else(|| failure("confirmation_source_missing"))?;
    let mut q=c.prepare("SELECT payload FROM ui_events WHERE chat_id=? AND turn_id=? AND sequence<? AND event='tool_card' AND json_extract(payload,'$.name') IN ('browser_snapshot','browser_inspect') AND json_extract(payload,'$.status')='done' ORDER BY sequence DESC LIMIT 2000").map_err(io::Error::other)?;
    let rows = q
        .query_map(params![chat, record["turn"].as_str(), anchor], |r| {
            r.get::<_, String>(0)
        })
        .map_err(io::Error::other)?;
    for raw in rows {
        let card: Value = serde_json::from_str(&raw.map_err(io::Error::other)?)?;
        let Some(v) = card["result_preview"].as_str().and_then(body) else {
            continue;
        };
        let observation = if v["observation"].is_object() {
            &v["observation"]
        } else {
            &v
        };
        if observation["target_id"] == *target
            && observation["document_id"] == *document
            && observation["url"]
                .as_str()
                .is_some_and(|u| linkedin_job(u).is_some())
        {
            let title = observation["title"]
                .as_str()
                .or_else(|| {
                    observation["ax"]
                        .as_array()
                        .and_then(|nodes| {
                            nodes.iter().find(|n| {
                                n["role"] == "RootWebArea"
                                    && n["frame_id"] == observation["main_frame_id"]
                            })
                        })
                        .and_then(|n| n["name"].as_str())
                })
                .unwrap_or("");
            let parts: Vec<_> = title.split('|').map(str::trim).collect();
            let employer = if parts.len() >= 3
                && parts
                    .last()
                    .is_some_and(|p| p.eq_ignore_ascii_case("LinkedIn"))
            {
                parts[parts.len() - 2]
            } else {
                continue;
            };
            if employer.is_empty() {
                continue;
            }
            return Ok(
                json!({"target_id":target,"document_id":document,"url":observation["url"],"employer":employer,"profile":observation["profile"],"call_id":card["id"]}),
            );
        }
    }
    Err(failure("confirmation_source_missing"))
}
pub(crate) fn linkedin_job(raw: &str) -> Option<String> {
    let u = url::Url::parse(raw).ok()?;
    if u.scheme() != "https" || !matches!(u.host_str()?, "linkedin.com" | "www.linkedin.com") {
        return None;
    }
    let p: Vec<_> = u.path().trim_matches('/').split('/').collect();
    (p.len() == 3
        && p[..2] == ["jobs", "view"]
        && !p[2].is_empty()
        && p[2].bytes().all(|b| b.is_ascii_digit()))
    .then(|| p[2].into())
}
pub(crate) fn application_job(raw: &str) -> Option<String> {
    if let Some(id) = linkedin_job(raw) { return Some(id); }
    let u = url::Url::parse(raw).ok()?;
    if u.scheme() != "https" || !u.username().is_empty() || u.password().is_some() || !u.host_str()?.ends_with(".myworkdayjobs.com") { return None; }
    let parts: Vec<_> = u.path().split('/').collect();
    let job = parts.iter().position(|p| *p == "job")?;
    let id = parts.get(job + 2)?.rsplit('_').next()?;
    id.strip_prefix('R').filter(|s|!s.is_empty() && s.split('-').all(|p|!p.is_empty()&&p.bytes().all(|b|b.is_ascii_digit()))).map(|_|id.into())
}
fn workday_completion(source: &Value, current: &Value) -> bool {
    use sha2::Digest;
    let (Some(before),Some(after))=(source["url"].as_str(),current["url"].as_str()) else {return false};
    let (Ok(mut a),Ok(b))=(url::Url::parse(before),url::Url::parse(after)) else {return false};
    let Some(id)=application_job(before) else {return false};
    if a.scheme()!="https" || !a.host_str().is_some_and(|h|h.ends_with(".myworkdayjobs.com"))
        || a.origin()!=b.origin() || !b.username().is_empty() || b.password().is_some() {return false;}
    let Some(prefix)=b.path().strip_suffix("/jobTasks/completed/application") else {return false};
    let Some((old_prefix,_))=a.path().split_once("/job/") else {return false};
    let tenant=|prefix:&str| {
        let parts:Vec<_>=prefix.trim_matches('/').split('/').collect();
        match parts.as_slice() {
            [tenant] if !tenant.is_empty()=>Some((*tenant).to_owned()),
            [locale,tenant] if locale.len()==5 && locale.as_bytes()[2]==b'-'
                && locale.bytes().enumerate().all(|(i,b)|i==2||b.is_ascii_alphabetic()) && !tenant.is_empty()=>Some((*tenant).to_owned()),
            _=>None,
        }
    };
    if tenant(old_prefix).is_none() || tenant(old_prefix)!=tenant(prefix) {return false;}
    a.set_fragment(None);
    source["job_id"]==id || source["job_id"]==format!("url-sha256:{:x}",sha2::Sha256::digest(a.as_str().as_bytes()))
}
/// Only host-persisted, linked browser verification can satisfy an action outcome.
pub(crate) fn valid(
    db: &Database,
    chat: &str,
    turn: &str,
    outcome: &str,
    value: &Value,
) -> io::Result<bool> {
    let r = &value["receipt"];
    let confirmation = &r["confirmation"];
    let original = &value["original_receipt"];
    if r["state"] != "verified"
        || r["action"] != "verify"
        || value["verification"] != "independent_site_confirmation"
        || value["input_sent"] != false
    {
        return Ok(false);
    }
    let Some(request) = confirmation["original_request_id"].as_str() else {
        return Ok(false);
    };
    let reservation = match record(db, chat, turn, request) {
        Ok(v) => v,
        Err(_) => return Ok(false),
    };
    let root = ui_task_completion::scope(db, turn)?;
    let contract = {
        let c = db.lock().map_err(|_| failure("evidence_storage_failed"))?;
        ui_task_completion::load(&c, chat, &root)?
    };
    let Some(required) = contract
        .outcomes
        .iter()
        .find(|o| o.id == outcome && o.kind == kageha_workflow::completion::OutcomeKind::Action)
    else {
        return Ok(false);
    };
    let description = reservation["outcome_description"].as_str().or_else(|| {
        contract
            .history
            .iter()
            .find(|a| {
                a.id == outcome
                    && serde_json::to_value(&a.after.proof).ok().as_ref()
                        == Some(&reservation["proof"])
            })
            .map(|a| a.after.description.as_str())
    });
    if description.is_some_and(|expected| expected != required.description) {
        return Ok(false);
    }
    if reservation["outcome_id"] != outcome
        || original["request_id"] != request
        || original["workspace_id"] != chat
        || original["action"] != "click"
        || original["resolution"]["request_id"] != r["request_id"]
        || r["workspace_id"] != chat
        || original["target_id"] != reservation["proof"]["target_id"]
        || original["document_id"] != reservation["proof"]["document_id"]
        || original["expected"]["selector"] != reservation["proof"]["selector"]
        || original["expected"]["text"] != reservation["proof"]["text"]
    {
        return Ok(false);
    }
    if reservation["proof"]["type"] == "browser_submission"
        && (original["expected"].is_object()
            || reservation["proof"]["source_url"] != original["submission_context"]["url"]) {
        return Ok(false);
    }
    let source = &original["submission_context"];
    let current = &confirmation["context"];
    for key in ["job_id", "employer", "profile", "session_digest"] {
        if source[key].as_str().is_none_or(str::is_empty) || source[key] != current[key] {
            return Ok(false);
        }
    }
    let account_match = source["account_digest"]
        .as_str()
        .is_some_and(|s| !s.is_empty())
        && source["account_digest"] == current["account_digest"];
    let original_document = source["target_id"] == current["target_id"]
        && source["document_id"] == current["document_id"]
        && source["account_digest"] == current["account_digest"];
    if !account_match && !original_document {
        return Ok(false);
    }
    let completion_bridge=confirmation["kind"]=="workday_completion" && original_document && workday_completion(source,current);
    if !completion_bridge && (application_job(source["url"].as_str().unwrap_or("")).as_deref() != source["job_id"].as_str()
        || application_job(current["url"].as_str().unwrap_or("")).as_deref()
            != source["job_id"].as_str())
    {
        return Ok(false);
    }
    if let Some(expected) = reservation.get("source_context").filter(|v| v.is_object()) {
        for key in [
            "job_id",
            "employer",
            "profile",
            "session_digest",
            "account_digest",
        ] {
            if expected[key] != source[key] {
                return Ok(false);
            }
        }
    } else {
        let saved = match legacy_source(db, chat, &reservation) {
            Ok(v) => v,
            Err(_) => return Ok(false),
        };
        if original["confirmation_source"] != saved || saved["url"] != source["url"] {
            return Ok(false);
        }
    }
    let kind = confirmation["kind"].as_str().unwrap_or("");
    let text = confirmation["text"].as_str().unwrap_or("");
    Ok(match kind {
        "workday_completion" => {
            let text=text.to_lowercase();
            completion_bridge && text.len()<=512 && text.starts_with("application submitted")
                && (text.contains("thank you for your application") || text.contains("you have successfully applied"))
        },
        "workday_confirmation" => {
            let text = text.to_lowercase();
            ["your application has been submitted", "your application was submitted", "your application has been successfully submitted", "your application was successfully submitted", "you have successfully applied", "application submitted successfully"]
                .iter().any(|prefix| text.strip_prefix(prefix).is_some_and(|rest| rest.is_empty() || rest.starts_with(['.', '!', ' ', '\n'])))
        },
        "application_status" => text == "Application submitted",
        "sent_modal" => text.eq_ignore_ascii_case(&format!(
            "Your application was sent to {}!",
            current["employer"].as_str().unwrap_or("")
        )),
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn completion_bridge_requires_exact_tenant_and_captured_job_identity() {
        let url="https://employer.wd1.myworkdayjobs.com/en-US/Careers/job/Remote/Lead_R12345-1/apply";
        let mut source=json!({"url":url,"job_id":"R12345-1"});
        let current=json!({"url":"https://employer.wd1.myworkdayjobs.com/en-US/Careers/jobTasks/completed/application"});
        assert_eq!(application_job(url).as_deref(),Some("R12345-1"));
        assert!(workday_completion(&source,&current));
        source["job_id"]=json!(format!("url-sha256:{:x}",<sha2::Sha256 as sha2::Digest>::digest(url.as_bytes())));
        assert!(workday_completion(&source,&current));
        for bad in ["https://employer.wd1.myworkdayjobs.com/en-US/Other/jobTasks/completed/application",
            "https://another.wd1.myworkdayjobs.com/en-US/Careers/jobTasks/completed/application",
            "https://employer.wd1.myworkdayjobs.com.evil.test/en-US/Careers/jobTasks/completed/application",
            "https://employer.wd1.myworkdayjobs.com/en-US/Careers/jobTasks/completed/application/extra"] {
            assert!(!workday_completion(&source,&json!({"url":bad})),"{bad}");
        }
        source["job_id"]=json!("R12345");assert!(!workday_completion(&source,&current));
        source["job_id"]=json!("url-sha256:unrelated");assert!(!workday_completion(&source,&current));
    }
}
/// Action evidence errors carry an exact recovery path, not a guessing exercise.
pub(crate) fn recovery_hint(
    db: &Database,
    chat: &str,
    turn: &str,
    outcome: &str,
) -> io::Result<Value> {
    let ids = requests(db, chat, turn, outcome)?;
    let supported = ids.first().and_then(|id|record(db,chat,turn,id).ok()).is_some_and(|reservation| {
        if reservation["source_context"].is_object() {
            application_job(reservation["source_context"]["url"].as_str().unwrap_or("")).is_some()
        } else { legacy_source(db,chat,&reservation).is_ok() }
    });
    Ok(
        json!({"original_request_ids":ids,"outcome_id":outcome,"pending_list_is_history":false,
  "next_call":if supported {json!({"name":"browser_receipts","arguments":{"outcome_id":outcome}})} else {Value::Null},
  "receipt_lookup":{"name":"browser_receipts","arguments":{"outcome_id":outcome}},
  "terminal":!supported && !ids.is_empty(),
  "confirmation_supported":supported,
  "confirmation_call":if supported {json!({"name":"browser_reconcile","arguments":{"request_id":ids.first(),"outcome_id":outcome,"mode":"confirmation"}})} else {Value::Null},
  "instruction":if supported {"Read and reconcile the original request once. Never resubmit."} else {"Native confirmation is unavailable for this source. Preserve the observed site result and original receipt, continue independent work, and finish with the formal evidence limitation stated once. Do not retry evidence attachment, confirmation or submission. Do not invent a failed-attempt reference to report a blocker."}}),
    )
}
pub(crate) fn publish(db: &Database, chat: &str, turn: &str, value: &Value) -> io::Result<()> {
    let Some(request) = value["receipt"]["confirmation"]["original_request_id"].as_str() else {
        return Ok(());
    };
    let reservation = record(db, chat, turn, request)?;
    let id = reservation["outcome_id"]
        .as_str()
        .ok_or_else(|| failure("submission_not_found"))?;
    if !valid(db, chat, turn, id, value)? {
        return Err(failure("confirmation_receipt_invalid"));
    }
    let mut c = db
        .lock()
        .map_err(|_| failure("evidence_storage_unavailable"))?;
    let tx = c.transaction().map_err(io::Error::other)?;
    let key = format!("{chat}/{request}");
    let mut latest = ui_store::get(&tx, "application-submission", &key)
        .map_err(io::Error::other)?
        .ok_or_else(|| failure("submission_not_found"))?;
    if latest["verification_reference"] == value["receipt"]["request_id"] {
        return Ok(());
    }
    latest["verified"] = json!(true);
    latest["verification_reference"] = value["receipt"]["request_id"].clone();
    latest["verification_kind"] = json!("independent_site_confirmation");
    ui_store::put(&tx, "application-submission", &key, latest).map_err(io::Error::other)?;
    crate::webui::insert_event(&tx,chat,reservation["turn"].as_str().unwrap_or(turn),"event",json!({"kind":"outcome_snapshot","payload":{"merge":true,"outcomes":[{"id":format!("application-{request}"),"label":"Confirmed by site","status":"pass","evidence":[{"id":value["receipt"]["request_id"],"label":"Independent site confirmation","reference":format!("receipt:{}",value["receipt"]["request_id"].as_str().unwrap_or("")),"detail":value["receipt"]["confirmation"]["text"],"timestamp":value["receipt"]["confirmation"]["observed_ms"].as_f64().unwrap_or(0.0)/1000.0}]}]}})).map_err(io::Error::other)?;
    tx.commit().map_err(io::Error::other)?;
    crate::ui_session_events::notify();
    Ok(())
}

/// Stable proof revision: no observation IDs, tool-call IDs, amendment history or logs.
pub(crate) fn recovery_context(
    tools: &crate::ui_tools::Tools<'_>,
    call: &kageha_contracts::execution::ToolCall,
) -> io::Result<Option<kageha_contracts::execution::RecoveryContext>> {
    if !matches!(
        call.name.as_str(),
        "browser_reconcile" | "record_outcome_evidence"
    ) || !crate::browser_client::configured()
    {
        return Ok(None);
    }
    let id = if call.name == "browser_reconcile" {
        call.arguments["request_id"].as_str().map(str::to_owned)
    } else {
        match call.arguments["outcome_id"].as_str() {
            Some(outcome) => requests(tools.db, tools.chat, tools.turn, outcome)?
                .into_iter()
                .next(),
            None => None,
        }
    };
    let Some(id) = id else { return Ok(None) };
    let reservation = match record(tools.db, tools.chat, tools.turn, &id) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let mut client = crate::browser_client::Client::connect(tools.chat, "agent")?;
    let original = client.call("action_receipt", json!({"request_id":id}), None)?;
    let selection = client.selection()?;
    let target = original["receipt"]["target_id"].as_str().unwrap_or("");
    let job = original["receipt"]["submission_context"]["job_id"].as_str();
    let tab = selection["tabs"].as_array().and_then(|tabs| {
        tabs.iter().find(|t| t["target_id"] == target).or_else(|| {
            tabs.iter().find(|t| {
                job.is_some() && linkedin_job(t["url"].as_str().unwrap_or("")).as_deref() == job
            })
        })
    });
    let fresh=tab.and_then(|tab|client.call("submission_context",json!({"browser_id":selection["browser_id"],"binding_id":selection["binding_id"],"target_id":tab["target_id"],"request_id":id}),None).ok()).unwrap_or(Value::Null);
    let revision = crate::ui_tool_outcomes::submission_revision(&original, &fresh);
    Ok(Some(kageha_contracts::execution::RecoveryContext {
        resource_key: format!(
            "submission:{}:{id}",
            reservation["scope"].as_str().unwrap_or("")
        ),
        element_revision: revision,
        journal_key:kageha_policy::input_digest(&json!(client.root.to_string_lossy())),
        journal_revision:selection["input_status"]["revision"].as_str().unwrap_or("legacy").into(),
        journal_unavailable:selection["input_status"]["ready"]==false,
        uses_journal:call.name=="browser_reconcile",
        evidence_scope: true,
        ..Default::default()
    }))
}
pub(crate) fn objective_revision(tools: &crate::ui_tools::Tools<'_>) -> io::Result<Option<String>> {
    let root = ui_task_completion::scope(tools.db, tools.turn)?;
    let contract = {
        let c = tools
            .db
            .lock()
            .map_err(|_| failure("evidence_storage_failed"))?;
        ui_task_completion::load(&c, tools.chat, &root)?
    };
    let mut verified = std::collections::BTreeSet::new();
    for evidence in &contract.evidence {
        let Some(outcome) = contract
            .outcomes
            .iter()
            .find(|o| o.id == evidence.outcome_id)
        else {
            continue;
        };
        if ui_task_completion::outcome_proof(
            tools.db,
            tools.chat,
            &evidence.turn,
            outcome,
            &evidence.reference,
        )? && crate::ui_workspace::evidence_current(
            tools.db,
            tools.chat,
            &evidence.turn,
            &outcome.id,
            &evidence.reference,
        )? {
            verified.insert(format!("{}:{}", outcome.id, evidence.reference));
        }
    }
    Ok(Some(kageha_policy::input_digest(&json!(verified))))
}

/// Repair old proof-search amendments only after native confirmation exists. The
/// business description and user origin must still match the original reservation.
pub(crate) fn restore_original_requirement(
    db: &Database,
    chat: &str,
    turn: &str,
    reservation: &Value,
) -> io::Result<()> {
    let root = ui_task_completion::scope(db, turn)?;
    let id = reservation["outcome_id"]
        .as_str()
        .ok_or_else(|| failure("submission_not_found"))?;
    let mut c = db
        .lock()
        .map_err(|_| failure("evidence_storage_unavailable"))?;
    let tx = c.transaction().map_err(io::Error::other)?;
    let mut contract = ui_task_completion::load(&tx, chat, &root)?;
    let current = contract
        .outcomes
        .iter()
        .find(|o| o.id == id)
        .cloned()
        .ok_or_else(|| failure("confirmation_outcome_mismatch"))?;
    let canonical = contract
        .history
        .iter()
        .find(|a| {
            a.id == id
                && a.after.kind == kageha_workflow::completion::OutcomeKind::Action
                && serde_json::to_value(&a.after.proof).ok().as_ref() == Some(&reservation["proof"])
        })
        .map(|a| a.after.clone())
        .or_else(|| {
            (current.kind == kageha_workflow::completion::OutcomeKind::Action
                && serde_json::to_value(&current.proof).ok().as_ref()
                    == Some(&reservation["proof"]))
            .then(|| current.clone())
        })
        .ok_or_else(|| failure("confirmation_source_missing"))?;
    if current.description != canonical.description || current.origin != canonical.origin {
        return Err(failure("confirmation_requirement_changed"));
    }
    if current == canonical {
        return Ok(());
    }
    let reason="Restore the original external action requirement after unsuccessful proof-search amendments; separate native site confirmation establishes the result.";
    let amendment = contract.amend(id, canonical, reason)?;
    // This restores the exact pre-dispatch obligation and cannot weaken that obligation.
    if let Some(last) = contract.history.last_mut() {
        last.weakened = false;
    }
    ui_store::put(
        &tx,
        "task-completion",
        &format!("{chat}/{root}"),
        serde_json::to_value(&contract)?,
    )
    .map_err(io::Error::other)?;
    crate::webui::insert_event(&tx,chat,turn,"event",json!({"kind":"outcome_amended","payload":{"outcome_id":id,"before":amendment.before,"after":amendment.after,"reason":reason,"weakened":false,"recovery_source":"original_submission_reservation"}})).map_err(io::Error::other)?;
    tx.commit().map_err(io::Error::other)?;
    Ok(())
}

/// Uses only browser-owned reads and the confirmation metadata operation. No input lease is taken.
pub(crate) fn recover(
    db: &Database,
    chat: &str,
    turn: &str,
    id: &str,
    outcome: &str,
) -> io::Result<Value> {
    let reservation = record(db, chat, turn, id)?;
    if reservation["outcome_id"] != outcome {
        return Err(failure("confirmation_outcome_mismatch"));
    }
    let mut client = crate::browser_client::Client::connect(chat, "agent")?;
    let original = client.call("action_receipt", json!({"request_id":id}), None)?;
    if let Some(proof) = original["receipt"]["resolution"]["request_id"].as_str() {
        let proof = client.call("action_receipt", json!({"request_id":proof}), None)?;
        restore_original_requirement(db, chat, turn, &reservation)?;
        return Ok(
            json!({"receipt":proof["receipt"],"original_receipt":original["receipt"],"verification":"independent_site_confirmation","input_sent":false}),
        );
    }
    let selection = client.selection()?;
    let source = if original["receipt"]["submission_context"].is_object() {
        None
    } else {
        Some(legacy_source(db, chat, &reservation)?)
    };
    let tabs = selection["tabs"]
        .as_array()
        .ok_or_else(|| failure("confirmation_tab_unavailable"))?;
    let original_target = original["receipt"]["target_id"].as_str().unwrap_or("");
    let job = original["receipt"]["submission_context"]["job_id"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| {
            source
                .as_ref()
                .and_then(|v| linkedin_job(v["url"].as_str().unwrap_or("")))
        });
    let tab = tabs
        .iter()
        .find(|t| t["target_id"] == original_target)
        .or_else(|| {
            tabs.iter()
                .find(|t| job.is_some() && linkedin_job(t["url"].as_str().unwrap_or("")) == job)
        })
        .ok_or_else(|| failure("confirmation_tab_unavailable"))?;
    let value=client.call("confirm_submission",json!({"browser_id":selection["browser_id"],"binding_id":selection["binding_id"],"target_id":tab["target_id"],"request_id":id,"legacy_source":source}),None)?;
    restore_original_requirement(db, chat, turn, &reservation)?;
    Ok(value)
}

/// Authenticated operator recovery never resumes a model run or submits an application.
pub(crate) fn operator_recover(db: &Database, chat: &str, id: &str) -> io::Result<Value> {
    if id.is_empty() || id.len() > 128 {
        return Err(failure("invalid_request"));
    }
    let reservation = {
        let c = db.lock().map_err(|_| failure("evidence_storage_failed"))?;
        ui_store::get(&c, "application-submission", &format!("{chat}/{id}"))
            .map_err(io::Error::other)?
            .ok_or_else(|| failure("submission_not_found"))?
    };
    if reservation["chat"] != chat {
        return Err(failure("submission_not_found"));
    }
    let turn = reservation["turn"]
        .as_str()
        .ok_or_else(|| failure("submission_not_found"))?;
    let outcome = reservation["outcome_id"]
        .as_str()
        .ok_or_else(|| failure("submission_not_found"))?;
    let mut value = recover(db, chat, turn, id, outcome)?;
    let call = json!({"id":format!("operator-confirmation:{}",value["receipt"]["request_id"].as_str().unwrap_or("")),"name":"browser_reconcile","arguments":{"request_id":id,"outcome_id":outcome,"mode":"confirmation"}});
    ui_task_completion::remember_tool(db, chat, turn, &call, &value.to_string())?;
    let attached = ui_task_completion::attach_submission(db, chat, turn, &value.to_string())?
        .ok_or_else(|| failure("evidence_attachment_failed"))?;
    if attached["error"].is_object() {
        return Err(io::Error::other(attached.to_string()));
    }
    value["submission_evidence"] = attached;
    {
        let c = db.lock().map_err(|_| failure("evidence_storage_failed"))?;
        crate::webui::insert_event(&c,chat,turn,"tool_card",json!({"id":call["id"],"name":"browser_reconcile","status":"done","result_preview":value.to_string(),"recovery_source":"authenticated_operator"})).map_err(io::Error::other)?;
    }
    crate::ui_session_events::notify();
    Ok(value)
}
