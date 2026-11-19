//! Evidence-backed completion for ordinary tasks, independent of browser workflows.
use crate::{ui_store, ui_tools::Tools, webui::Database};
use kageha_contracts::execution::{CompletionCheck, CompletionStatus, Control};
use kageha_workflow::completion::{Blocker, BlockerCategory, Contract, Evidence, Origin, OutcomeKind, RequiredOutcome, OUTCOME_UNSATISFIABLE};
use rusqlite::{params, OptionalExtension};
use kageha_workflow::verification::Verification;
use serde_json::{json, Value};
use std::{collections::{BTreeMap, BTreeSet}, io};
const KIND: &str = "task-completion";
#[cfg(test)]
mod lean_checks;

pub(crate) fn scope(db:&Database, turn:&str)->io::Result<String> {
    // Task identity survives a new user request without changing its invocation
    // budget. Only the host records links after an explicit continuation choice.
    let linked={
        let connection=db.lock().map_err(|_|io::Error::other("Task scope unavailable"))?;
        match ui_store::get(&connection,"task-scope",turn) {
            Ok(value)=>value,
            Err(rusqlite::Error::SqliteFailure(_,Some(message))) if message.starts_with("no such table")=>None,
            Err(error)=>return Err(io::Error::other(error)),
        }
    };
    if let Some(root)=linked.as_ref().and_then(|value|value["root"].as_str()) {return Ok(root.into())}
    Ok(crate::ui_persistence::configuration_for_turn(db,turn)?
        .and_then(|config|config["request_budget_id"].as_str().map(str::to_owned))
        .unwrap_or_else(||turn.to_owned()))
}

/// Resolve the immediately preceding user request in this chat. The model chooses
/// continuation explicitly; arbitrary user text is never heuristically classified.
fn continuation_scope(db:&Database,chat:&str,turn:&str)->io::Result<String> {
    let source={
        let connection=db.lock().map_err(|_|io::Error::other("Task history unavailable"))?;
        connection.query_row("SELECT prior.request_id FROM native_chat_turns prior WHERE prior.chat_id=?1 AND prior.sequence < (SELECT current.sequence FROM native_chat_turns current JOIN ui_runtime_runs run ON json_extract(run.spec,'$.request')=current.request_id AND json_extract(run.spec,'$.session')=current.chat_id WHERE current.chat_id=?1 AND json_extract(run.spec,'$.turn')=?2 LIMIT 1) ORDER BY prior.sequence DESC LIMIT 1",
            params![chat,turn],|row|row.get::<_,String>(0)).optional().map_err(io::Error::other)?
    }.ok_or_else(||io::Error::other("There is no previous task in this chat to continue."))?;
    scope(db,&crate::webui::turn_id(chat,&source))
}
fn key(chat: &str, turn: &str) -> String { format!("{chat}/{turn}") }

pub(crate) fn initialize(db: &rusqlite::Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS ui_rejected_answers(chat_id TEXT NOT NULL,turn_id TEXT NOT NULL,
        n INTEGER NOT NULL,content TEXT NOT NULL,reason TEXT NOT NULL,draft_path TEXT,PRIMARY KEY(chat_id,turn_id,n));")
}

/// Rejected candidates longer than this are also saved as a workspace draft (Requirement 8.3).
const DRAFT_BYTES: usize = 4096;

/// Keeps a candidate answer the Completion_Gate rejected (Requirement 8.3). The row is keyed
/// by the request `root`, so `n` counts the request's rejections across resumed turns. A
/// candidate over [`DRAFT_BYTES`] is also written to `drafts/answer-<n>.md`, registered as an
/// artifact of `turn`, and its path is appended to the instruction. Failures are logged and
/// never change the gate decision.
pub(crate) fn keep_rejected(db: &Database, chat: &str, turn: &str, root: &str, candidate: &str, check: &mut CompletionCheck) {
    if check.status == CompletionStatus::Ready || candidate.trim().is_empty() { return; }
    let reason=if check.instruction.trim().is_empty() {
        serde_json::to_value(check.status).ok().and_then(|value|value.as_str().map(str::to_owned)).unwrap_or_default()
    } else { check.instruction.clone() };
    let n:i64={
        let Ok(connection)=db.lock() else { eprintln!("Rejected answer not kept: database unavailable"); return };
        let inserted=connection.query_row(
            "INSERT INTO ui_rejected_answers(chat_id,turn_id,n,content,reason)
             SELECT ?1,?2,COALESCE(MAX(n),0)+1,?3,?4 FROM ui_rejected_answers WHERE chat_id=?1 AND turn_id=?2 RETURNING n",
            params![chat,root,candidate,reason],|row|row.get(0));
        match inserted { Ok(n)=>n, Err(error)=>{ eprintln!("Rejected answer not kept: {error}"); return } }
    };
    if candidate.len() <= DRAFT_BYTES { return; }
    match save_draft(db,chat,turn,root,n,candidate) {
        Ok(path)=>check.instruction.push_str(&format!(" Your rejected answer is saved at {path} (artifact_path {path}); you can edit it or attach it as artifact evidence.")),
        Err(error)=>eprintln!("Rejected answer draft not saved: {error}"),
    }
}

/// Writes the draft to the first free `drafts/answer-<m>.md` with `m >= n`, so an earlier
/// draft (from another request, or edited by the agent) is never overwritten.
fn save_draft(db: &Database, chat: &str, turn: &str, root: &str, n: i64, candidate: &str) -> io::Result<String> {
    let workspace=crate::ui_workspace::open_for_chat(db,chat)?;
    // A worktree worker's root is a Git checkout: the Host adds no files of its own there.
    // The rejected answer stays in `ui_rejected_answers` and in the context.
    if !workspace.state_inside() { return Err(io::Error::other("drafts are not written into a Git worktree")); }
    // Repeated identical candidates should receive the same feedback and
    // cannot create an endless sequence of identical files.
    let previous: Option<String> = db.lock().map_err(|_| io::Error::other("Database unavailable"))?
        .query_row("SELECT draft_path FROM ui_rejected_answers WHERE chat_id=? AND turn_id=? AND n=? AND content=?",
            params![chat, root, n - 1, candidate], |row| row.get::<_, Option<String>>(0))
        .optional().map_err(io::Error::other)?.flatten();
    if let Some(path) = previous {
        if let Ok((_, info)) = crate::ui_uploads::pinned(db, chat, &path) {
            use sha2::{Digest, Sha256};
            if info["sha256"] == format!("{:x}", Sha256::digest(candidate.as_bytes())) {
                db.lock().map_err(|_| io::Error::other("Database unavailable"))?
                    .execute("UPDATE ui_rejected_answers SET draft_path=? WHERE chat_id=? AND turn_id=? AND n=?",
                        params![path, chat, root, n]).map_err(io::Error::other)?;
                return Ok(path);
            }
        }
    }
    let relative=(n..n.saturating_add(10_000)).map(|m|format!("drafts/answer-{m}.md"))
        .find(|relative|workspace.resolve(relative).is_ok_and(|path|!path.exists()))
        .ok_or_else(||io::Error::other("no free draft name"))?;
    let change=workspace.write(&relative,candidate.as_bytes())?;
    let path=crate::ui_workspace::register_changes(db,chat,turn,&[change])?.pop()
        .ok_or_else(||io::Error::other("draft was not registered"))?;
    db.lock().map_err(|_|io::Error::other("database unavailable"))?
        .execute("UPDATE ui_rejected_answers SET draft_path=? WHERE chat_id=? AND turn_id=? AND n=?",params![path,chat,root,n])
        .map_err(io::Error::other)?;
    Ok(path)
}
pub(crate) fn load(db: &rusqlite::Connection, chat: &str, turn: &str) -> io::Result<Contract> {
    ui_store::get(db,KIND,&key(chat,turn)).map_err(io::Error::other)?
        .map_or_else(|| Ok(Contract::default()), |value| serde_json::from_value(value).map_err(io::Error::other))
}
fn save(db: &rusqlite::Connection, chat: &str, turn: &str, contract: &Contract) -> io::Result<Value> {
    ui_store::put(db,KIND,&key(chat,turn),serde_json::to_value(contract)?).map_err(io::Error::other)
}

/// Outcome kinds the given tool names can prove, from the Capability_Registry.
/// An answer is always provable; tools outside the registry prove nothing.
pub(crate) fn provable_kinds<'a>(names: impl IntoIterator<Item = &'a str>) -> BTreeSet<OutcomeKind> {
    let mut kinds=BTreeSet::from([OutcomeKind::Answer]);
    for name in names { kinds.extend(crate::ui_tools::capability(name).proves.iter().copied()); }
    kinds
}

/// A short lowercase slug such as `interview-prep`.
fn task_type(args: &Value) -> io::Result<Option<String>> {
    let Some(value)=args.get("task_type").filter(|value|!value.is_null()) else { return Ok(None) };
    let slug=value.as_str().map(|text|text.trim().to_ascii_lowercase()).unwrap_or_default();
    let valid=(1..=64).contains(&slug.len())
        && slug.bytes().all(|byte|byte.is_ascii_lowercase()||byte.is_ascii_digit()||byte==b'-'||byte==b'_')
        && slug.as_bytes()[0].is_ascii_alphanumeric();
    if !valid { return Err(io::Error::other("task_type must be a short slug of up to 64 lowercase letters, digits, - or _")); }
    Ok(Some(slug))
}

/// Records outcomes against the turn's provable kinds. `origin` is taken from the model
/// and defaults to `user`, the stricter choice: the Host cannot tell which outcomes came
/// from the user's words. The first recorded `task_type` of a request is kept.
pub(crate) fn record(db: &Database, chat: &str, turn: &str, args: &Value, provable: &BTreeSet<OutcomeKind>) -> io::Result<Value> {
    let outcomes: Vec<RequiredOutcome> = serde_json::from_value(args["outcomes"].clone())
        .map_err(|error| io::Error::other(format!("Invalid required outcome: {error}. Provide id, description, kind, verify, optional origin and structured proof for action/validation outcomes.")))?;
    if outcomes.is_empty() || outcomes.len()>64 { return Err(io::Error::other("Record one to 64 required outcomes")); }
    if outcomes.iter().any(|outcome|outcome.verify.len()>4000 || outcome.verify.contains('\0')) {
        return Err(io::Error::other("verify must be at most 4000 bytes"));
    }
    let task_type=task_type(args)?;
    let prior_scope=match args.get("continue_previous") {
        None|Some(Value::Bool(false))=>None,
        Some(Value::Bool(true))=>Some(continuation_scope(db,chat,turn)?),
        _=>return Err(io::Error::other("continue_previous must be a boolean")),
    };
    let current_scope=scope(db,turn)?;
    let root=prior_scope.as_deref().unwrap_or(&current_scope).to_owned();
    let mut connection=db.lock().map_err(|_|io::Error::other("Completion state unavailable"))?;
    let tx=connection.transaction().map_err(io::Error::other)?;
    if prior_scope.is_some() && root!=current_scope && !load(&tx,chat,&current_scope)?.outcomes.is_empty() {
        return Err(io::Error::other("This request already has a task contract. Continue a previous task before recording a new contract."));
    }
    let mut contract=load(&tx,chat,&root)?;
    if prior_scope.is_some() && contract.outcomes.is_empty() {
        return Err(io::Error::other("The preceding request has no recorded task to continue. Record the current request normally."));
    }
    let result=contract.record(outcomes,provable)?;
    // A missing capability is host-observed, so do not ask the model to fabricate
    // a failed attempt merely to report it. Keep the requirement and record a
    // blocker. An explicit marker distinguishes them from prior blockers,
    // including legacy blockers without references.
    contract.blockers.retain(|blocker| !blocker.missing_capability
        || contract.outcomes.iter().find(|outcome| outcome.id == blocker.outcome_id)
            .is_none_or(|outcome| !provable.contains(&outcome.kind)));
    let missing: Vec<_> = contract.outcomes.iter()
        .filter(|outcome| !provable.contains(&outcome.kind)
            && !contract.blockers.iter().any(|blocker| blocker.outcome_id == outcome.id))
        .map(|outcome| (outcome.id.clone(), outcome.kind)).collect();
    for (outcome_id, kind) in missing {
        contract.block(Blocker { outcome_id, category: BlockerCategory::MissingAccess,
            reason: format!("A required capability is unavailable: {}. Enable an appropriate tool if available, then retry.", kind.missing_capability()),
            references: vec![], at: now(), missing_capability: true })?;
    }
    if prior_scope.is_some() {
        ui_store::put(&tx,"task-scope",turn,json!({"chat":chat,"root":root})).map_err(io::Error::other)?;
    }
    if contract.task_type.is_none() { contract.task_type=task_type; }
    let saved=save(&tx,chat,&root,&contract)?;
    let mut journaled=Vec::new();
    for rejected in &result.rejected {
        let event=json!({"kind":"outcome_rejected","payload":{
            "outcome_id":rejected.id,"outcome_kind":rejected.kind,"code":rejected.code,
            "missing_capability":rejected.missing_capability,"alternatives":rejected.alternatives}});
        journaled.extend(crate::ui_learning::entry(db,"event",&event));
        crate::webui::insert_event(&tx,chat,turn,"event",event).map_err(io::Error::other)?;
    }
    tx.commit().map_err(io::Error::other)?;
    drop(connection);
    for pending in journaled { crate::ui_learning::offer(db,turn,pending); }
    if !result.rejected.is_empty() { crate::ui_session_events::notify(); }
    let mut instruction=String::from("Preserve every required outcome. Use record_outcome_evidence to attach actual artifact paths, original verified receipts, or successful validation results before concluding.");
    if !result.rejected.is_empty() {
        instruction.push_str(" Outcomes listed as rejected are preserved and blocked because no available tool can prove them. Continue other work; the host will list the blockers and their reasons in the final answer. Summarize verified results and essential information without repeating blocker reasons. Amend an outcome only if its declaration was genuinely incorrect.");
    }
    Ok(json!({"accepted":result.accepted,"rejected":result.rejected,"required_outcomes":saved["outcomes"],"task_type":saved["task_type"],"task_scope":root,"instruction":instruction}))
}

/// The Host-built result object of a tool call. Hooks wrap a tool's own result as the
/// `tool_result` string, which is unwrapped once. Proof checks read only the top-level
/// fields of this object, so JSON that a command prints into `stdout` or a job log
/// cannot impersonate an exit code or a receipt.
fn host_result(content: &str) -> Option<Value> {
    let value=serde_json::from_str::<Value>(content).ok()?;
    Some(value["tool_result"].as_str().and_then(|inner|serde_json::from_str::<Value>(inner).ok()).unwrap_or(value))
}

// Full adapter results are immutable proof, independent of clipped transcript cards.
pub(crate) fn remember_tool(db:&Database,chat:&str,turn:&str,call:&Value,content:&str)->io::Result<()> {
    let name=call["name"].as_str().unwrap_or("");
    if !matches!(name,"remote_exec"|"remote_verify_file"|"remote_job"|"shell"|"shell_job"|"browser_validate"|"computer_execute"|"computer_receipt"|"browser_reconcile"|"browser_receipts") && !name.starts_with("browser_") {return Ok(());}
    let Some(mut value)=host_result(content) else{return Ok(())};
    if name=="browser_receipts" {
        if let Some(receipts)=value["receipts"].as_array() {
            for receipt in receipts {
                if let Some(id)=receipt["request_id"].as_str() {
                    let mut item_call=call.clone();item_call["id"]=json!(format!("{}:{id}",call["id"].as_str().unwrap_or("list")));
                    remember_tool(db,chat,turn,&item_call,&json!({"receipt":receipt}).to_string())?;
                }
            }
        }
        if let Some(confirmations)=value["confirmations"].as_array() {
            for confirmation in confirmations {
                let mut item_call=call.clone();item_call["id"]=json!(format!("{}:{}",call["id"].as_str().unwrap_or("list"),confirmation["receipt"]["request_id"].as_str().unwrap_or("proof")));
                remember_tool(db,chat,turn,&item_call,&confirmation.to_string())?;
            }
        }
    }

    let credential_commit = name == "browser_credential" && credential_commit_matches(&value, &call["arguments"], chat);
    if !verified_action(name,&value) && !credential_commit && !verified_validation(name,&value)
        && !(name=="remote_verify_file" && value["ok"]==true && value["path"]==call["arguments"]["path"])
        && !(name=="browser_validate" && value["validation"].is_object())
        && !(matches!(name,"browser_click"|"browser_receipts"|"browser_reconcile") && value["receipt"]["request_id"].is_string()) {return Ok(());}
    // The exit status is the proof. Command output (up to 32 KB per stream) is not
    // needed to re-check it and stays in the transcript and spill files.
    if matches!(name,"shell"|"shell_job"|"remote_exec") {
        if let Some(fields)=value.as_object_mut() { for field in ["stdout","stderr","output"] { fields.remove(field); } }
    }
    // This metadata is set by the host and cannot be supplied by command stdout.
    value["_host_arguments"] = call["arguments"].clone();
    let connection=db.lock().map_err(|_|io::Error::other("Evidence storage unavailable"))?;
    connection.execute("INSERT OR IGNORE INTO ui_tool_evidence(chat_id,turn_id,call_id,name,receipt_id,body) VALUES (?,?,?,?,?,?)",
        params![chat,turn,call["id"].as_str().unwrap_or(""),name,value["receipt"]["request_id"].as_str(),value.to_string()]).map_err(io::Error::other)?;
    Ok(())
}

pub(crate) fn matches_expected(actual: &Value, expected: &Value) -> bool {
    if actual == expected {
        return true;
    }
    match (actual, expected) {
        (Value::String(a), Value::String(e)) => {
            a.trim() == e.trim() || a.trim().eq_ignore_ascii_case(e.trim())
        }
        (Value::Bool(a), Value::String(s)) => {
            if *a { s.eq_ignore_ascii_case("true") } else { s.eq_ignore_ascii_case("false") }
        }
        (Value::String(s), Value::Bool(b)) => {
            if *b { s.eq_ignore_ascii_case("true") } else { s.eq_ignore_ascii_case("false") }
        }
        (Value::Number(n), Value::String(s)) => n.to_string() == *s,
        (Value::String(s), Value::Number(n)) => *s == n.to_string(),
        _ => false,
    }
}

pub(crate) fn field_assertion(result:&Value,assertion:&Value)->Value {
    let items=result["matches"]["items"].as_array();
    let item=items.and_then(|items|(items.len()==1).then(||&items[0]));
    let actual=item.and_then(|item|match assertion["property"].as_str() {
        Some("validity.valid")=>item.get("validity").and_then(|v|v.get("valid")),
        Some("selected_text"|"selected_label")=>item.get(assertion["property"].as_str().unwrap()).or_else(||item.get("text")),
        Some("value"|"checked"|"text")=>{
            if assertion["property"].as_str()==Some("text") && item["tag"]=="select" && item.get("selected_text").is_some() {
                item.get("selected_text")
            } else {
                item.get(assertion["property"].as_str().unwrap())
            }
        },
        _=>None,
    });
    let passed=item.is_some_and(|item|item["visible"]==true)
        && result["matches"]["next_offset"].is_null()
        && item.is_some_and(|item|match assertion["property"].as_str() {Some("value")=>item["value_truncated"]!=true,Some("text")=>item["text_truncated"]!=true,_=>true})
        && actual.is_some_and(|actual|!actual.is_null() && matches_expected(actual, &assertion["expected"]));
    json!({"selector":assertion["selector"],"property":assertion["property"],"expected":assertion["expected"],"passed":passed})
}

fn verified_action(name: &str, value: &Value) -> bool {
    // Only adapter-owned receipt fields count. JSON printed by a command or
    // supplied by an arbitrary connector cannot impersonate an action receipt.
    matches!(name,"browser_click"|"browser_fill"|"browser_check"|"browser_select"|"browser_press"|"browser_upload"|"browser_fill_credential"|"browser_receipts"|"browser_reconcile"|"computer_execute"|"computer_receipt")
        && (value["receipt"]["action"]!="verify" || value["verification"]=="independent_site_confirmation")
        && value["receipt"]["state"] == "verified"
        && value["receipt"]["request_id"].as_str().is_some_and(|id| !id.is_empty())
}

/// This host-owned result is emitted only after the browser vault has durably
/// committed and checked the candidate. It proves that operation at that time,
/// not a later vault state or the website's reset.
fn credential_commit_matches(value: &Value, args: &Value, chat: &str) -> bool {
    let receipt = &value["result"]["commit_receipt"];
    value["status"] == "completed" && value["result"]["stored"] == true
        && value["result"]["operation"] == "commit_rotation" && args["operation"] == "commit_rotation"
        && receipt["state"] == "verified" && receipt["operation"] == "commit_rotation"
        && receipt["workspace_id"] == chat && receipt["origin"] == value["result"]["origin"]
        && receipt["account"] == args["account"] && receipt["candidate"] == args["credential"]
        && receipt["active"] == value["result"]["credential"]
        && ["origin","account","candidate","active"].iter().all(|key| receipt[*key].as_str().is_some_and(|s| !s.is_empty()))
}
/// Validation proof in a Host-built tool result (see [`host_result`]).
///
/// - `remote_exec`: `ok` and exit code 0.
/// - `shell`: a finished foreground command with integer exit code 0 (Requirement 2.8).
///   A background start (`{job_id,state,log_path}`) carries a `job_id` and no exit code,
///   and a timed-out command is a failed call, so neither counts.
/// - `shell_job`: a job in state `exited` with exit code 0 proves the command it ran, so a
///   long build or test suite run in the background is validation once it ends. A job
///   `stop`ped by the agent is `killed` and never counts. [`proof`] also requires the job
///   to have been started in the same request as the evidence, so polling a job of an
///   earlier request cannot satisfy this one.
fn verified_validation(name: &str, value: &Value) -> bool {
    (name == "remote_exec" && value["ok"] == true && value["exit_code"] == 0)
        || (name == "shell" && value["exit_code"] == 0 && value.get("job_id").is_none())
        || (matches!(name,"shell_job"|"remote_job") && value["state"] == "exited" && value["exit_code"] == 0
            && value["job_id"].as_str().is_some_and(|id|!id.is_empty()))
        || (name == "browser_validate" && value["validation"]["passed"] == true
            && value["validation"]["checks"].as_array().is_some_and(|checks|!checks.is_empty() && checks.iter().all(|check|check["passed"]==true)))
}

pub(crate) fn proof(db: &Database, chat: &str, turn: &str, kind: &OutcomeKind, reference: &str) -> io::Result<bool> {
    if reference.is_empty() || reference.len()>1024 { return Ok(false); }
    if *kind == OutcomeKind::Artifact {
        if let Some(id) = reference.strip_prefix("handoff:") {
            return crate::ui_handoff::evidence(db, chat, turn, id);
        }
        let exists = db.lock().map_err(|_|io::Error::other("Artifact evidence unavailable"))?
            .query_row("SELECT EXISTS(SELECT 1 FROM ui_turn_artifacts WHERE chat_id=? AND turn_id=? AND path=?)",params![chat,turn,reference],|row|row.get::<_,bool>(0)).map_err(io::Error::other)?;
        return Ok(exists && crate::ui_uploads::pinned(db,chat,reference).is_ok());
    }
    let Some((name,value)) = tool_evidence(db,chat,turn,reference)? else {return Ok(false)};
    Ok(match kind {
        OutcomeKind::Action => verified_action(&name,&value) || (name=="browser_credential" && credential_commit_matches(&value,&value["_host_arguments"],chat)),
        OutcomeKind::Validation => verified_validation(&name,&value)
            && (name != "shell_job" || job_in_request(db,chat,turn,value["job_id"].as_str().unwrap_or(""))?)
            && (name != "remote_job" || value["started_turn"].as_str().is_some_and(|source| {
                scope(db,source).ok().zip(scope(db,turn).ok()).is_some_and(|(source, current)| source == current)
            })),
        OutcomeKind::Artifact | OutcomeKind::Answer => false,
    })
}

fn tool_evidence(db:&Database,chat:&str,turn:&str,reference:&str)->io::Result<Option<(String,Value)>> {
    let (reference,namespace)=if let Some(id)=reference.strip_prefix("receipt:") {(id,"receipt")}
        else if let Some(id)=reference.strip_prefix("tool:") {(id,"tool")}
        else {(reference.strip_prefix("browser_fill:").unwrap_or(reference),"auto")};
    let mut raw:Option<(String,String)>={
        let db=db.lock().map_err(|_|io::Error::other("Tool evidence unavailable"))?;
        if reference == "latest" || reference == "browser_validate" {
            db.query_row("SELECT name,body FROM ui_tool_evidence WHERE chat_id=?1 AND turn_id=?2 AND (?3='latest' OR name=?3) ORDER BY rowid DESC LIMIT 1",
                params![chat,turn,reference],|row|Ok((row.get(0)?,row.get(1)?))).optional().map_err(io::Error::other)?
        } else {
            // Explicit namespaces never fall through to a different identity. Bare
            // IDs prefer their exact tool call, then an original adapter receipt.
            db.query_row("SELECT name,body FROM ui_tool_evidence WHERE chat_id=?1 AND turn_id=?2 AND ((?4!='receipt' AND call_id=?3) OR (?4!='tool' AND receipt_id=?3)) ORDER BY CASE WHEN call_id=?3 AND ?4!='receipt' THEN 0 ELSE 1 END,rowid DESC LIMIT 1",
                params![chat,turn,reference,namespace],|row|Ok((row.get(0)?,row.get(1)?))).optional().map_err(io::Error::other)?
        }
    };
    if raw.is_none() && namespace=="receipt" {
        let root=scope(db,turn)?;
        let candidates:Vec<(String,String,String)>={let c=db.lock().map_err(|_|io::Error::other("Tool evidence unavailable"))?;
            let mut q=c.prepare("SELECT turn_id,name,body FROM ui_tool_evidence WHERE chat_id=? AND receipt_id=? ORDER BY rowid DESC LIMIT 100").map_err(io::Error::other)?;
            let rows=q.query_map(params![chat,reference],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(io::Error::other)?;rows.collect::<rusqlite::Result<_>>().map_err(io::Error::other)?};
        for (source,name,value) in candidates {if scope(db,&source)?==root {raw=Some((name,value));break;}}
    }
    let (name,value)=match raw {
        Some((name,body))=>(name,serde_json::from_str::<Value>(&body)?),
        None=>{
            // Cards keep the full result, or the spill excerpt for results over the card
            // bound; the excerpt keeps the top-level `exit_code` (`ui_tool_spill::KEPT_FIELDS`).
            let body: Option<String> = db.lock().map_err(|_|io::Error::other("Tool evidence unavailable"))?
                .query_row("SELECT payload FROM ui_events WHERE chat_id=? AND turn_id=? AND event='tool_card' AND ((?!='receipt' AND json_extract(payload,'$.id')=?) OR (?!='tool' AND CASE WHEN json_valid(json_extract(payload,'$.result_preview')) THEN json_extract(json_extract(payload,'$.result_preview'),'$.receipt.request_id')=? ELSE 0 END)) AND json_extract(payload,'$.status')='done' ORDER BY CASE WHEN json_extract(payload,'$.id')=? AND ?!='receipt' THEN 0 ELSE 1 END,sequence DESC LIMIT 1",params![chat,turn,namespace,reference,namespace,reference,reference,namespace],|row|row.get(0)).optional().map_err(io::Error::other)?;
            let Some(card) = body.and_then(|body|serde_json::from_str::<Value>(&body).ok()) else { return Ok(None); };
            let Some(value) = card["result_preview"].as_str().and_then(host_result) else { return Ok(None); };
            (card["name"].as_str().unwrap_or("").to_owned(),value)
        }
    };
    Ok(Some((name,value)))
}

/// A provenance-valid receipt is necessary but does not establish an arbitrary
/// requirement. Match its declared target, postcondition or every field assertion.
pub(crate) fn outcome_proof(db:&Database,chat:&str,turn:&str,outcome:&RequiredOutcome,reference:&str)->io::Result<bool> {
    if let Some(Verification::RemoteFile { host_id, path, sha256 }) = &outcome.proof {
        return Ok(outcome.kind == OutcomeKind::Artifact && reference.starts_with("tool:")
            && tool_evidence(db, chat, turn, reference)?.is_some_and(|(name, value)| {
                name == "remote_verify_file" && value["ok"] == true && value["host_id"] == *host_id
                    && value["path"] == *path && value["sha256"] == *sha256 && value["_host_arguments"]["path"] == *path
            }));
    }
    if !proof(db,chat,turn,&outcome.kind,reference)? {return Ok(false)}
    if !matches!(outcome.kind,OutcomeKind::Action|OutcomeKind::Validation) {return Ok(true)}
    if let Some((_,value))=tool_evidence(db,chat,turn,reference)? {
        if value["verification"]=="independent_site_confirmation" {return crate::ui_submission_confirmation::valid(db,chat,turn,&outcome.id,&value);}
    }
    let Some(requirement)=outcome.proof.as_ref() else {return Ok(false)};
    let Some((name,value))=tool_evidence(db,chat,turn,reference)? else {return Ok(false)};
    Ok(match requirement {
        Verification::BrowserSubmission { .. } | Verification::RemoteFile { .. } => false,
        Verification::BrowserPostcondition{target_id,document_id,selector,text} => {
            let receipt=&value["receipt"];
            name.starts_with("browser_") && receipt["action"]=="click" && receipt["target_id"]==*target_id && receipt["document_id"]==*document_id
                && receipt["expected"]["selector"]==*selector && receipt["expected"]["text"]==*text
        }
        Verification::BrowserFields{target_id,document_id,assertions} => name=="browser_validate"
            && value["page"]["target_id"]==*target_id && value["page"]["document_id"]==*document_id
            && assertions.iter().all(|assertion|value["validation"]["checks"].as_array().is_some_and(|checks|
                checks.iter().any(|check|check["passed"]==true && check["selector"]==assertion.selector
                    && check["property"]==assertion.property && matches_expected(&check["expected"], &assertion.expected)))),
        Verification::Command{command} => (matches!(name.as_str(),"shell"|"remote_exec")
            && value["_host_arguments"]["command"]==*command)
            || (matches!(name.as_str(),"shell_job"|"remote_job") && value["command"]==*command),
        Verification::ComputerAction{arguments} => name=="computer_execute" && value["_host_arguments"]==*arguments,
        Verification::CredentialRotation{origin,account,candidate} => name=="browser_credential"
            && credential_commit_matches(&value,&value["_host_arguments"],chat)
            && value["result"]["commit_receipt"]["origin"]==*origin
            && value["result"]["commit_receipt"]["account"]==*account
            && value["result"]["commit_receipt"]["candidate"]==*candidate,
    })
}

/// Whether shell job `job_id` was started (by `shell` with `background`, or by a foreground
/// timeout) in a turn of the same request as `turn`. The start is read from the
/// `shell_command` journal entry that `ui_shell` writes with the starting call's id. Without
/// that entry the job does not count.
fn job_in_request(db: &Database, chat: &str, turn: &str, job_id: &str) -> io::Result<bool> {
    if job_id.is_empty() { return Ok(false); }
    let turns: Vec<String> = {
        let connection=db.lock().map_err(|_|io::Error::other("Tool evidence unavailable"))?;
        let mut query=connection.prepare("SELECT DISTINCT turn_id FROM ui_events WHERE chat_id=? AND event='event' AND json_extract(payload,'$.kind')='shell_command' AND json_extract(payload,'$.payload.job_id')=? AND json_extract(payload,'$.payload.call_id') IS NOT NULL").map_err(io::Error::other)?;
        let rows=query.query_map(params![chat,job_id],|row|row.get(0)).map_err(io::Error::other)?;
        rows.collect::<rusqlite::Result<_>>().map_err(io::Error::other)?
    };
    let root=scope(db,turn)?;
    for started in turns { if scope(db,&started)? == root { return Ok(true); } }
    Ok(false)
}

#[derive(Debug)]
pub(crate) enum EvidenceFailure {
    MissingProof, EvidenceNotFound, WrongRequestScope, UnverifiedReceipt,
    IncompatibleEvidenceKind, ProofMismatch(Vec<&'static str>),
}
impl EvidenceFailure {
    fn value(&self, id: &str) -> Value {
        let (reason, next, arguments) = match self {
            Self::MissingProof => ("missing_proof", "amend_required_outcome", vec!["id", "proof", "reason"]),
            Self::EvidenceNotFound => ("evidence_not_found", "browser_receipts", vec!["request_id"]),
            Self::WrongRequestScope => ("wrong_request_scope", "record_outcome_evidence", vec!["outcome_id", "reference", "evidence_turn"]),
            Self::UnverifiedReceipt => ("unverified_receipt", "browser_reconcile", vec!["request_id"]),
            Self::IncompatibleEvidenceKind => ("incompatible_evidence_kind", "record_outcome_evidence", vec!["outcome_id", "reference"]),
            Self::ProofMismatch(_) => ("proof_mismatch", "record_outcome_evidence", vec!["outcome_id", "reference"]),
        };
        json!({"error_code":"invalid_outcome_evidence", "reason_code":reason,"retryable":false,
            "outcome_id":id,"next_tool":next,"required_arguments":arguments,
            "mismatched_fields":match self {Self::ProofMismatch(fields)=>fields.clone(),_=>vec![]},
            "summary":"Recover using the original receipt and matching declared proof. Do not repeat submission. If proof cannot be established, report_blocker."})
    }
    fn error(&self,id:&str)->io::Error {io::Error::other(self.value(id).to_string())}
}
fn matching_submission_proof(expected:&Verification, actual:&Verification)->Result<(),EvidenceFailure> {
    if expected==actual {return Ok(())}
    let mut fields=vec![];
    match (expected,actual) {
        (Verification::BrowserPostcondition{target_id:a,document_id:b,selector:c,text:d},
         Verification::BrowserPostcondition{target_id:w,document_id:x,selector:y,text:z}) => {
            for (name,diff) in [("target_id",a!=w),("document_id",b!=x),("selector",c!=y),("text",d!=z)] {if diff {fields.push(name)}}
        },
        _=>return Err(EvidenceFailure::IncompatibleEvidenceKind),
    }
    Err(EvidenceFailure::ProofMismatch(fields))
}
fn assess_evidence(db:&Database,chat:&str,turn:&str,outcome:&RequiredOutcome,reference:&str)->io::Result<Result<(),EvidenceFailure>> {
    if matches!(outcome.kind,OutcomeKind::Action|OutcomeKind::Validation) && outcome.proof.is_none() {return Ok(Err(EvidenceFailure::MissingProof))}
    if outcome.kind==OutcomeKind::Artifact {
        if matches!(outcome.proof, Some(Verification::RemoteFile { .. })) {
            return Ok(if outcome_proof(db,chat,turn,outcome,reference)? {Ok(())} else {Err(EvidenceFailure::ProofMismatch(vec!["host_id","path","sha256"]))});
        }
        return Ok(if proof(db,chat,turn,&outcome.kind,reference)? {Ok(())} else {Err(EvidenceFailure::EvidenceNotFound)});
    }
    let Some((name,value))=tool_evidence(db,chat,turn,reference)? else {return Ok(Err(EvidenceFailure::EvidenceNotFound))};
    if value["verification"]=="independent_site_confirmation" {
        return Ok(if crate::ui_submission_confirmation::valid(db,chat,turn,&outcome.id,&value)? {Ok(())}else{Err(EvidenceFailure::ProofMismatch(vec!["submission_identity"]))});
    }
    if let Some(expected @ Verification::BrowserPostcondition{..})=&outcome.proof {
        if !name.starts_with("browser_") || value["receipt"]["action"]!="click" {return Ok(Err(EvidenceFailure::IncompatibleEvidenceKind))}
        if !verified_action(&name,&value) {return Ok(Err(EvidenceFailure::UnverifiedReceipt))}
        let r=&value["receipt"];
        let actual=Verification::BrowserPostcondition{target_id:r["target_id"].as_str().unwrap_or("").into(),document_id:r["document_id"].as_str().unwrap_or("").into(),selector:r["expected"]["selector"].as_str().unwrap_or("").into(),text:r["expected"]["text"].as_str().unwrap_or("").into()};
        return Ok(matching_submission_proof(expected,&actual));
    }
    if !proof(db,chat,turn,&outcome.kind,reference)? {return Ok(Err(EvidenceFailure::IncompatibleEvidenceKind))}
    if outcome_proof(db,chat,turn,outcome,reference)? {return Ok(Ok(()))}
    let mut fields=Vec::new();
    match &outcome.proof {
        Some(Verification::BrowserFields{target_id,document_id,assertions}) => {
            if value["page"]["target_id"]!=*target_id {fields.push("target_id")}
            if value["page"]["document_id"]!=*document_id {fields.push("document_id")}
            if !assertions.iter().all(|assertion|value["validation"]["checks"].as_array().is_some_and(|checks| checks.iter().any(|check|
                check["passed"]==true && check["selector"]==assertion.selector && check["property"]==assertion.property
                    && matches_expected(&check["expected"],&assertion.expected)))) {fields.push("assertions")}
        },
        Some(Verification::Command{..})=>fields.push("command"),
        Some(Verification::ComputerAction{..})=>fields.push("arguments"),
        _=>fields.push("proof"),
    }
    Ok(Err(EvidenceFailure::ProofMismatch(fields)))
}

/// Reserve an explicitly linked submission before any browser input. The reservation
/// remains unresolved on transport failure, fencing duplicate external effects.
pub(crate) fn prepare_submission(db:&Database,chat:&str,turn:&str,args:&Value,selection:&Value,request:&str)->io::Result<()> {
    let id=args["outcome_id"].as_str().filter(|s|!s.is_empty()).ok_or_else(||io::Error::other(json!({"error_code":"invalid_outcome_evidence","reason_code":"missing_outcome_link","execution_phase":"not_dispatched","retryable":false,"next_tool":"browser_click","required_arguments":["outcome_id"],"summary":"Link the recorded action outcome before submission."}).to_string()))?;
    let root=scope(db,turn)?;
    let proof=if args["confirmation_mode"]=="observe_after_submit" {
        Verification::BrowserSubmission{target_id:selection["target_id"].as_str().unwrap_or("").into(),document_id:selection["document_id"].as_str().unwrap_or("").into(),source_url:selection["_submission_context"]["url"].as_str().unwrap_or("").into()}
    } else {
        Verification::BrowserPostcondition{target_id:selection["target_id"].as_str().unwrap_or("").into(),document_id:selection["document_id"].as_str().unwrap_or("").into(),selector:args["expected_selector"].as_str().unwrap_or("").into(),text:args["expected_text"].as_str().unwrap_or("").into()}
    };
    if !proof.valid() {return Err(EvidenceFailure::ProofMismatch(vec!["proof"]).error(id))}
    let mut connection=db.lock().map_err(|_|io::Error::other("Completion state unavailable"))?;
    let tx=connection.transaction().map_err(io::Error::other)?;
    let mut contract=load(&tx,chat,&root)?;
    let before=contract.outcomes.iter().find(|o|o.id==id).cloned().ok_or_else(||EvidenceFailure::EvidenceNotFound.error(id))?;
    if before.kind!=OutcomeKind::Action {return Err(EvidenceFailure::IncompatibleEvidenceKind.error(id))}
    for record in ui_store::list(&tx,"application-submission").map_err(io::Error::other)? {
        let next = &selection["_submission_context"];
        let old = &record["source_context"];
        if args["confirmation_mode"] == "observe_after_submit" && record["chat"] == chat
            && next["job_id"].is_string() && old["job_id"] == next["job_id"]
            && old["employer"] == next["employer"] && old["profile"] == next["profile"]
            && record["not_dispatched"] != true {
            return Err(io::Error::other(json!({"error_code":"submission_already_attempted","execution_phase":"not_dispatched","retryable":false,"request_id":record["request_id"],"summary":"This application already has an original attempt. Inspect that receipt; do not submit in another turn or tab."}).to_string()));
        }
        if record["chat"]==chat && record["scope"]==root && record["outcome_id"]==id && record["not_dispatched"]!=true {
            return Err(io::Error::other(json!({"error_code":"invalid_outcome_evidence","reason_code":"submission_already_attempted","retryable":false,"execution_phase":"not_dispatched","outcome_id":id,"request_id":record["request_id"],"next_tool":"browser_receipts","required_arguments":["request_id"],"summary":"Recover the original submission receipt; do not submit again."}).to_string()));
        }
    }
    if let Some(existing)=&before.proof {matching_submission_proof(existing,&proof).map_err(|e| {
            let mut value=e.value(id);value["next_tool"]=json!("amend_required_outcome");value["required_arguments"]=json!(["id","proof","reason"]);value["execution_phase"]=json!("not_dispatched");io::Error::other(value.to_string())
        })?;} else {
        let mut next=before.clone();next.proof=Some(proof.clone());
        let reason="Bind proof from the explicitly linked submission before dispatch";
        let amendment=contract.amend(id,next,reason)?;
        save(&tx,chat,&root,&contract)?;
        crate::webui::insert_event(&tx,chat,turn,"event",json!({"kind":"outcome_amended","payload":{"outcome_id":id,"before":amendment.before,"after":amendment.after,"reason":reason,"weakened":amendment.weakened}})).map_err(io::Error::other)?;
    }
    ui_store::put(&tx,"application-submission",&format!("{chat}/{request}"),json!({"chat":chat,"scope":root,"turn":turn,"outcome_id":id,"request_id":request,"proof":proof,"confirmation":args["expected_text"],"source_context":selection["_submission_context"],"outcome_description":before.description})).map_err(io::Error::other)?;
    tx.commit().map_err(io::Error::other)?;
    Ok(())
}

pub(crate) fn submission_dispatch_failed(db:&Database,chat:&str,request:&str,error:&io::Error)->io::Result<()> {
    let Ok(value)=serde_json::from_str::<Value>(&error.to_string()) else {return Ok(())};
    if value["execution_phase"]!="not_dispatched" || value["input_dispatched"]==true {return Ok(())}
    submission_not_dispatched(db,chat,request)
}

/// The caller knows no page input was sent (for example, recording the initial
/// pending state failed), so this reservation may be released for a fresh attempt.
pub(crate) fn submission_not_dispatched(db:&Database,chat:&str,request:&str)->io::Result<()> {
    release_submission(db,chat,request).map(|_|())
}

fn release_submission(db:&Database,chat:&str,request:&str)->io::Result<bool> {
    let mut c=db.lock().map_err(|_|io::Error::other("Completion state unavailable"))?;
    let tx=c.transaction().map_err(io::Error::other)?;
    let key=format!("{chat}/{request}");
    if let Some(mut record)=ui_store::get(&tx,"application-submission",&key).map_err(io::Error::other)? {
        // Never let a stale or contradictory non-dispatch report undo a verified
        // submission, nor emit duplicate events on repeated receipt reads.
        if record["verified"]==true {return Ok(false)}
        if record["not_dispatched"]==true {return Ok(true)}
        let turn=record["turn"].as_str().filter(|turn|!turn.is_empty())
            .ok_or_else(||io::Error::other("Submission has no original turn"))?.to_owned();
        record["not_dispatched"]=json!(true);
        ui_store::put(&tx,"application-submission",&key,record).map_err(io::Error::other)?;
        crate::webui::insert_event(&tx,chat,&turn,"event",json!({"kind":"outcome_snapshot","payload":{
            "merge":true,"outcomes":[{"id":format!("application-{request}"),"label":"Submission not sent",
                "status":"fail","evidence":[]}]}})).map_err(io::Error::other)?;
        tx.commit().map_err(io::Error::other)?;
        crate::ui_session_events::notify();
        return Ok(true)
    }
    Ok(false)
}

/// Whether a result's original receipt belongs to an explicitly recorded submission.
/// This lets the dispatcher preserve a successful page action when evidence persistence
/// itself fails, without changing the behavior of unrelated browser actions.
pub(crate) fn is_submission_receipt(db:&Database,chat:&str,content:&str)->io::Result<bool> {
    let Some(value)=host_result(content) else {return Ok(false)};
    let Some(request)=value["receipt"]["request_id"].as_str() else {return Ok(false)};
    let c=db.lock().map_err(|_|io::Error::other("Completion state unavailable"))?;
    Ok(ui_store::get(&c,"application-submission",&format!("{chat}/{request}")).map_err(io::Error::other)?.is_some())
}

/// Called only after the original host result has been saved as evidence.
pub(crate) fn attach_submission(db:&Database,chat:&str,turn:&str,content:&str)->io::Result<Option<Value>> {
    let Some(value)=host_result(content) else {return Ok(None)};
    if let Some(confirmations)=value["confirmations"].as_array() {
        let mut results=Vec::new();
        for confirmation in confirmations {if let Some(result)=attach_submission(db,chat,turn,&confirmation.to_string())?{results.push(result);}}
        return Ok((!results.is_empty()).then(||json!({"confirmations":results})));
    }

    let Some(request)=value["receipt"]["confirmation"]["original_request_id"].as_str().or_else(||value["receipt"]["request_id"].as_str()) else {return Ok(None)};
    let record={let c=db.lock().map_err(|_|io::Error::other("Completion state unavailable"))?;ui_store::get(&c,"application-submission",&format!("{chat}/{request}")).map_err(io::Error::other)?};
    let Some(record)=record else {return Ok(None)};
    let Some(id)=record["outcome_id"].as_str() else {return Ok(None)};
    let root=scope(db,turn)?;
    if record["scope"]!=root {return Ok(Some(json!({"error":EvidenceFailure::WrongRequestScope.value(id)})))}
    if value["receipt"]["state"]=="not_dispatched" {
        // Only a persisted browser-owned receipt for this exact original action
        // can release the fence. An arbitrary tool's output or a receipt for a
        // different target/document/postcondition cannot authorize a retry.
        let reference=format!("receipt:{request}");
        let stored=tool_evidence(db,chat,turn,&reference)?;
        let valid=stored.is_some_and(|(name,evidence)| {
            matches!(name.as_str(),"browser_click"|"browser_receipts"|"browser_reconcile")
                && evidence["receipt"]==value["receipt"]
                && evidence["receipt"]["action"]=="click"
                && evidence["receipt"]["request_id"]==request
                && evidence["receipt"]["target_id"]==record["proof"]["target_id"]
                && evidence["receipt"]["document_id"]==record["proof"]["document_id"]
                && evidence["receipt"]["expected"]["selector"]==record["proof"]["selector"]
                && evidence["receipt"]["expected"]["text"]==record["proof"]["text"]
                && !record["proof"].is_null()
        });
        if !valid {return Ok(Some(json!({"error":{"error_code":"invalid_outcome_evidence","reason_code":"proof_mismatch","outcome_id":id,"summary":"The saved non-dispatch receipt does not match the original submission. Do not retry."}})))}
        if !release_submission(db,chat,request)? {return Ok(Some(json!({"error":{"error_code":"invalid_outcome_evidence","reason_code":"contradictory_receipt","outcome_id":id,"summary":"The submission was already verified; do not retry."}})))}
        return Ok(Some(json!({"outcome_id":id,"request_id":request,"not_dispatched":true,"summary":"The original submission was not sent; a new attempt may be made."})))
    }
    if value["receipt"]["state"]!="verified" {return Ok(None)}
    let contract={let c=db.lock().map_err(|_|io::Error::other("Completion state unavailable"))?;load(&c,chat,&root)?};
    let reference=format!("receipt:{}",value["receipt"]["request_id"].as_str().unwrap_or(request));
    if let Some(outcome)=contract.outcomes.iter().find(|outcome|outcome.id==id) {
        for existing in contract.evidence.iter().filter(|item|item.outcome_id==id && item.reference==reference) {
            if outcome_proof(db,chat,&existing.turn,outcome,&reference)? {
                return Ok(Some(json!({"outcome_id":id,"verified_reference":reference,"already_attached":true})));
            }
        }
    }
    let result=attach(db,chat,turn,&json!({"outcome_id":id,"reference":reference}));
    if result.is_ok() && value["verification"]=="independent_site_confirmation" {crate::ui_submission_confirmation::publish(db,chat,turn,&value)?;}
    let status=match result {Ok(v)=>v,Err(e)=>json!({"error":serde_json::from_str::<Value>(&e.to_string()).unwrap_or(json!({"summary":e.to_string()}))})};
    {let c=db.lock().map_err(|_|io::Error::other("Completion state unavailable"))?;crate::webui::insert_event(&c,chat,turn,"event",json!({"kind":"submission_evidence","payload":{"outcome_id":id,"request_id":request,"result":status}})).map_err(io::Error::other)?;}
    Ok(Some(status))
}

pub(crate) fn attach(db: &Database, chat: &str, turn: &str, args: &Value) -> io::Result<Value> {
    let id=args["outcome_id"].as_str().ok_or_else(||io::Error::other("outcome_id is required"))?;
    let reference=args["reference"].as_str().ok_or_else(||io::Error::other("A recorded evidence reference is required"))?;
    let root=scope(db,turn)?;
    let contract={let connection=db.lock().map_err(|_|io::Error::other("Completion state unavailable"))?;load(&connection,chat,&root)?};
    let outcome=contract.outcomes.iter().find(|outcome|outcome.id==id)
        .ok_or_else(||io::Error::other("Record this required outcome first"))?;
    let evidence_turn=args["evidence_turn"].as_str().unwrap_or(turn);
    if scope(db,evidence_turn)? != root {return Err(EvidenceFailure::WrongRequestScope.error(id))}
    assess_evidence(db,chat,evidence_turn,outcome,reference)?.map_err(|failure|{
        let mut value=failure.value(id);
        if let Ok(hint)=crate::ui_submission_confirmation::recovery_hint(db,chat,turn,id) {
            if hint["original_request_ids"].as_array().is_some_and(|ids|!ids.is_empty()) {
                // A submission-specific route supersedes generic attachment
                // repair. Never advertise a reattach or blocker call when no
                // compatible confirmation or failed-attempt reference exists.
                value["next_tool"]=hint["next_call"]["name"].clone();
                value["required_arguments"]=if hint["terminal"]==true {json!([])}else{json!(["outcome_id"])};
                value["summary"]=hint["instruction"].clone();
                value["recovery"]=hint;
            }
        }
        io::Error::other(value.to_string())
    })?;
    if outcome.kind == OutcomeKind::Artifact { crate::ui_workspace::pin_evidence(db,chat,evidence_turn,id,reference)?; }
    let validated_outcome=outcome.clone();
    let evidence=Evidence{outcome_id:id.into(),reference:reference.into(),turn:evidence_turn.into()};
    let mut connection=db.lock().map_err(|_|io::Error::other("Completion state unavailable"))?;
    let tx=connection.transaction().map_err(io::Error::other)?;
    // Merge into the latest state: parallel evidence attachment cannot erase a
    // newly added requirement or another confirmed result.
    let mut current=load(&tx,chat,&root)?;
    if current.outcomes.iter().find(|outcome|outcome.id==id)!=Some(&validated_outcome) {
        return Err(io::Error::other("The required outcome changed while its proof was checked. Read the current requirement and attach matching proof again."));
    }
    current.attach(evidence)?;
    save(&tx,chat,&root,&current)?;
    tx.commit().map_err(io::Error::other)?;
    Ok(json!({"outcome_id":id,"verified_reference":reference}))
}

/// Outcome ids with current Host-verified proof, exactly as the Completion_Gate counts them.
fn verified_outcomes(db: &Database, chat: &str, contract: &Contract) -> io::Result<BTreeMap<String,Vec<String>>> {
    let mut verified=BTreeMap::<String,Vec<String>>::new();
    for evidence in &contract.evidence {
        let Some(outcome)=contract.outcomes.iter().find(|outcome|outcome.id==evidence.outcome_id) else {continue};
        if outcome_proof(db,chat,&evidence.turn,outcome,&evidence.reference)?
            && crate::ui_workspace::evidence_current(db,chat,&evidence.turn,&outcome.id,&evidence.reference)? {
            verified.entry(outcome.id.clone()).or_default().push(evidence.reference.clone());
        }
    }
    Ok(verified)
}
fn has_pending_submission(records:&[Value], verified:&BTreeMap<String,Vec<String>>)->bool {
    records.iter().any(|record| {
        record["not_dispatched"] != true && record["verified"] != true
            && !record["outcome_id"].as_str().zip(record["request_id"].as_str()).is_some_and(|(id, request)| {
                verified.get(id).is_some_and(|refs| refs.contains(&format!("receipt:{request}")))
            })
    })
}
fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0,|elapsed|elapsed.as_secs_f64())
}
fn optional_text(args: &Value, key: &str) -> io::Result<Option<String>> {
    match &args[key] {
        Value::Null => Ok(None),
        Value::String(text) => Ok(Some(text.clone())),
        _ => Err(io::Error::other(format!("{key} must be a string"))),
    }
}

/// Changes the kind, description or `verify` text of a recorded outcome. Unset fields keep
/// their stored value. A changed kind must be provable by this turn's tools.
pub(crate) fn amend(db: &Database, chat: &str, turn: &str, args: &Value, provable: &BTreeSet<OutcomeKind>) -> io::Result<Value> {
    let id=args["id"].as_str().filter(|id|!id.is_empty()).ok_or_else(||io::Error::other("id of a recorded required outcome is required"))?;
    let reason=optional_text(args,"reason")?.unwrap_or_default();
    let kind=match &args["kind"] {
        Value::Null => None,
        value => Some(serde_json::from_value::<OutcomeKind>(value.clone())
            .map_err(|_|io::Error::other("kind must be answer, artifact, action or validation"))?),
    };
    let (description,verify)=(optional_text(args,"description")?,optional_text(args,"verify")?);
    let root=scope(db,turn)?;
    let mut connection=db.lock().map_err(|_|io::Error::other("Completion state unavailable"))?;
    let tx=connection.transaction().map_err(io::Error::other)?;
    let mut contract=load(&tx,chat,&root)?;
    let before=contract.outcomes.iter().find(|outcome|outcome.id==id).cloned()
        .ok_or_else(||io::Error::other("Record this required outcome first; an amendment changes an existing outcome"))?;
    let next=RequiredOutcome{id:id.into(),kind:kind.unwrap_or(before.kind),
        description:description.unwrap_or_else(||before.description.clone()),
        verify:verify.unwrap_or_else(||before.verify.clone()),origin:before.origin,
        proof:match args.get("proof") { Some(value)=>serde_json::from_value(value.clone()).map_err(io::Error::other)?, None if kind.is_some_and(|kind|kind!=before.kind)=>None, None=>before.proof.clone() }};
    if (next.kind!=before.kind || next.proof!=before.proof) && ui_store::list(&tx,"application-submission").map_err(io::Error::other)?.iter().any(|r|r["chat"]==chat && r["scope"]==root && r["outcome_id"]==id && r["not_dispatched"]!=true) {
        return Err(io::Error::other(json!({"error_code":"submission_proof_locked","outcome_id":id,"retryable":false,"next_tool":"browser_reconcile","summary":"The original submission proof is preserved after dispatch. Use independent confirmation recovery; changing targets, text or outcome kind cannot prove that submission."}).to_string()));
    }
    if next.kind != before.kind && !provable.contains(&next.kind) {
        let alternatives:Vec<String>=provable.iter().map(|kind|kind.as_str().to_owned()).chain(["report_blocker".to_owned()]).collect();
        return Err(io::Error::other(json!({"error_code":OUTCOME_UNSATISFIABLE,"retryable":false,"outcome_id":id,
            "kind":next.kind,"missing_capability":next.kind.missing_capability(),"alternatives":alternatives,
            "summary":format!("The outcome was not amended: no available tool can prove a {} outcome; that needs {}. Amend it to one of the alternative kinds, or call report_blocker if it cannot be completed.",next.kind.as_str(),next.kind.missing_capability())}).to_string()));
    }
    let amendment=contract.amend(id,next,&reason)?;
    save(&tx,chat,&root,&contract)?;
    let event=json!({"kind":"outcome_amended","payload":{
        "outcome_id":id,"before":amendment.before,"after":amendment.after,"reason":amendment.reason,"weakened":amendment.weakened}});
    let journaled=crate::ui_learning::entry(db,"event",&event);
    crate::webui::insert_event(&tx,chat,turn,"event",event).map_err(io::Error::other)?;
    tx.commit().map_err(io::Error::other)?;
    drop(connection);
    if let Some(pending)=journaled { crate::ui_learning::offer(db,turn,pending); }
    crate::ui_session_events::notify();
    let mut instruction=String::from("The amended outcome replaces the previous version for this request. Attach proof that matches its current kind with record_outcome_evidence.");
    if amendment.weakened && amendment.before.origin == Origin::User {
        instruction.push_str(" This weakens a requirement from the user's words; the final answer will list it with your reason under Changed requirements.");
    }
    Ok(json!({"outcome_id":id,"before":amendment.before,"after":amendment.after,"weakened":amendment.weakened,"instruction":instruction}))
}

const INVALID_BLOCKER: &str = "invalid_blocker";
/// Host task-state tools. Their failures are not attempts at the work, so a rejected
/// `report_blocker` or evidence call cannot serve as a blocker reference.
const TASK_STATE_TOOLS: &[&str] = &["record_required_outcomes","amend_required_outcome","record_outcome_evidence","report_blocker","task_plan_update","task_plan_revise"];
fn invalid_blocker(outcome_id: &str, missing: &[String]) -> io::Error {
    io::Error::other(json!({"error_code":INVALID_BLOCKER,"retryable":false,"outcome_id":outcome_id,"missing":missing,
        "summary":format!("The blocker was not accepted. Missing: {}.",missing.join("; "))}).to_string())
}

/// Whether `reference` names a failed attempt in the request `root`: a failed tool card,
/// a denied approval, or an `ask_human` that was skipped or ended without an answer.
fn unsuccessful_card(card:&Value)->bool {
    let name=card["name"].as_str().unwrap_or("");
    if TASK_STATE_TOOLS.contains(&name) {return false}
    if matches!(card["status"].as_str(),Some("failed"|"blocked"|"unconfirmed")) {return true}
    if card["status"]!="done" {return false}
    let value=card["result_preview"].as_str().and_then(host_result).unwrap_or(Value::Null);
    (name=="ask_human" && value["status"]=="skipped")
        || (name=="browser_validate" && value["validation"]["passed"]==false)
        || (matches!(name,"shell"|"shell_job"|"remote_exec") && value["exit_code"].as_i64().is_some_and(|code|code!=0))
}

/// Exact references offered for repair, scoped to this chat's logical request.
/// Only host-owned card identities are returned, never adapter-supplied aliases.
pub(crate) fn failed_attempt_candidates(db:&Database,chat:&str,_turn:&str,root:&str)->io::Result<Vec<Value>> {
    let rows:Vec<(String,String)>={
        let connection=db.lock().map_err(|_|io::Error::other("Attempt history unavailable"))?;
        let mut query=connection.prepare("SELECT turn_id,payload FROM ui_events WHERE chat_id=? AND event='tool_card' ORDER BY sequence DESC LIMIT 512").map_err(io::Error::other)?;
        let rows=query.query_map([chat],|row|Ok((row.get(0)?,row.get(1)?))).map_err(io::Error::other)?;
        rows.collect::<rusqlite::Result<_>>().map_err(io::Error::other)?
    };
    let mut seen=BTreeSet::new();
    let mut attempts=Vec::new();
    for (turn,raw) in rows {
        let card:Value=serde_json::from_str(&raw).unwrap_or(Value::Null);
        let Some(id)=card["id"].as_str() else {continue};
        if !seen.insert(id.to_owned()) || !unsuccessful_card(&card) || scope(db,&turn)?!=root {continue}
        attempts.push(json!({"tool_call_id":id,"tool":card["name"],"turn_id":turn}));
        if attempts.len()==12 {break}
    }
    Ok(attempts)
}

fn failed_attempt(db: &Database, chat: &str, turn: &str, root: &str, reference: &str) -> io::Result<bool> {
    if reference.is_empty() || reference.len()>1024 { return Ok(false); }
    let (card,approval,question)={
        let connection=db.lock().map_err(|_|io::Error::other("Attempt history unavailable"))?;
        // The latest card for a call id carries its final status (running → done/failed).
        let card:Option<(String,String)>=connection.query_row(
            "SELECT turn_id,payload FROM ui_events WHERE chat_id=? AND event='tool_card' AND json_extract(payload,'$.id')=? ORDER BY sequence DESC LIMIT 1",
            params![chat,reference],|row|Ok((row.get(0)?,row.get(1)?))).optional().map_err(io::Error::other)?;
        let approval=crate::ui_approvals::owned(&connection)?.get(reference).map_err(io::Error::other)?
            .filter(|record|record.state==kageha_contracts::policy::ApprovalState::Denied && record.request.authority.session==chat)
            .map(|record|record.request.authority.turn);
        let question:Option<(String,Option<String>)>=connection.query_row(
            "SELECT turn,response FROM ui_questions WHERE chat=? AND id=?",params![chat,reference],
            |row|Ok((row.get(0)?,row.get(1)?))).optional().map_err(io::Error::other)?;
        (card,approval,question)
    };
    if let Some((card_turn,payload))=card {
        let card:Value=serde_json::from_str(&payload).unwrap_or(Value::Null);
        let name=card["name"].as_str().unwrap_or("");
        if unsuccessful_card(&card) && !TASK_STATE_TOOLS.contains(&name) && scope(db,&card_turn)?==root { return Ok(true); }
    }
    if let Some(approval_turn)=approval {
        if scope(db,&approval_turn)?==root { return Ok(true); }
    }
    if let Some((question_turn,response))=question {
        let response=response.and_then(|text|serde_json::from_str::<Value>(&text).ok());
        // No response after its turn ended means the question timed out or was abandoned.
        let unanswered=match response { Some(value)=>value["status"]=="skipped", None=>question_turn!=turn };
        if unanswered && scope(db,&question_turn)?==root { return Ok(true); }
    }
    Ok(false)
}

/// The host checks the cited failure against this request's journal.
fn report_blocker(db: &Database, chat: &str, turn: &str, args: &Value) -> io::Result<Value> {
    let outcome_id=args["outcome_id"].as_str().unwrap_or("").trim();
    let category=serde_json::from_value::<BlockerCategory>(args["category"].clone()).ok();
    let reason=args["reason"].as_str().map(str::trim).unwrap_or("");
    let references:Option<Vec<String>>=args["references"].as_array()
        .and_then(|items|items.iter().map(|item|item.as_str().map(str::trim).filter(|text|!text.is_empty()).map(str::to_owned)).collect());
    let mut missing=Vec::<String>::new();
    if outcome_id.is_empty() { missing.push("outcome_id of a recorded required outcome".into()); }
    if category.is_none() { missing.push("category, one of missing_access, missing_input, external_failure or policy_denied".into()); }
    if reason.is_empty() || reason.len()>4000 { missing.push("a reason of up to 4000 bytes saying what is missing and what the user can do".into()); }
    match &references {
        Some(items) if !items.is_empty() && items.len()<=64 => {}
        _ => missing.push("references: one to 64 non-empty ids of failed attempts from this request (failed tool-call ids, denied approval ids, or skipped ask_human tool-call ids)".into()),
    }
    if !missing.is_empty() { return Err(invalid_blocker(outcome_id,&missing)); }
    let (category,references)=(category.unwrap(),references.unwrap());
    let root=scope(db,turn)?;
    let contract={let connection=db.lock().map_err(|_|io::Error::other("Completion state unavailable"))?;load(&connection,chat,&root)?};
    if !contract.outcomes.iter().any(|outcome|outcome.id==outcome_id) {
        let ids: Vec<_> = contract.outcomes.iter().map(|outcome| outcome.id.as_str()).collect();
        return Err(invalid_blocker(outcome_id,&[format!("a recorded required outcome with id {outcome_id}; record it first if it is a requested deliverable. Current IDs: {ids:?}. An unavailable research method can be explained directly in the answer.")]));
    };
    if verified_outcomes(db,chat,&contract)?.get(outcome_id).is_some_and(|refs|!refs.is_empty()) {
        missing.push(format!("an unverified outcome: {outcome_id} already has verified proof and cannot be blocked"));
    }
    let mut resolved=Vec::new();
    for reference in &references {
        if failed_attempt(db,chat,turn,&root,reference)? { resolved.push(reference.clone()); }
    }
    if resolved.is_empty() {
        missing.push(format!("a reference to a failed attempt in this request: none of [{}] is a failed tool result, a denied approval, or an ask_human that was skipped or timed out", references.join(", ")));
    }
    if !missing.is_empty() {
        let attempts=failed_attempt_candidates(db,chat,turn,&root)?;
        let mut value:Value=serde_json::from_str(&invalid_blocker(outcome_id,&missing).to_string())?;
        value["failed_attempts"]=json!(attempts);
        value["repair"]=json!("Use an exact tool_call_id from failed_attempts when it supports this blocker. Do not invent or reformat IDs. A browser RPC request_id is not a failed tool-call ID. If no relevant attempt exists, perform one relevant check; do not loop on rejected reports.");
        return Err(io::Error::other(value.to_string()));
    }
    let blocker=Blocker{outcome_id:outcome_id.into(),category,reason:reason.into(),references:resolved,at:now(),missing_capability:false};
    let mut connection=db.lock().map_err(|_|io::Error::other("Completion state unavailable"))?;
    let tx=connection.transaction().map_err(io::Error::other)?;
    // Block on the latest state so a parallel record or attach is not lost.
    let mut current=load(&tx,chat,&root)?;
    current.block(blocker.clone())?;
    save(&tx,chat,&root,&current)?;
    let event=json!({"kind":"outcome_blocked","payload":{
        "outcome_id":blocker.outcome_id,"category":blocker.category,"reason":blocker.reason,"references":blocker.references}});
    let journaled=crate::ui_learning::entry(db,"event",&event);
    crate::webui::insert_event(&tx,chat,turn,"event",event).map_err(io::Error::other)?;
    tx.commit().map_err(io::Error::other)?;
    drop(connection);
    if let Some(pending)=journaled { crate::ui_learning::offer(db,turn,pending); }
    crate::ui_session_events::notify();
    Ok(json!({"outcome_id":blocker.outcome_id,"blocked":true,"category":blocker.category,"references":blocker.references,
        "instruction":"The host will list unresolved blockers and their reasons in the final answer. Finish other unmet outcomes, then summarize verified results and any essential information not already in the blocker list; do not repeat blocker reasons."}))
}

impl Tools<'_> {
    pub(crate) fn task_report_blocker(&self, args: &Value) -> io::Result<Value> {
        Control { cancelled: self.cancel, deadline: None }.check()?;
        report_blocker(self.db, self.chat, self.turn, args)
    }
    /// Outcome kinds this turn's tool list can prove.
    pub(crate) fn provable_outcomes(&self)->BTreeSet<OutcomeKind> {
        let specs=crate::native_chat::ToolHost::specs(self);
        provable_kinds(specs.iter().filter_map(|spec|spec["function"]["name"].as_str()))
    }
    pub(crate) fn task_completion_check(&self,candidate:&str,control:&Control<'_>) -> io::Result<CompletionCheck> {
        control.check()?;
        let root=scope(self.db,self.turn)?;
        let contract={let connection=self.db.lock().map_err(|_|io::Error::other("Completion state unavailable"))?;load(&connection,self.chat,&root)?};
        let verified=verified_outcomes(self.db,self.chat,&contract)?;
        // Submission records are keyed by chat/request. Use the primary-key range
        // so an ordinary answer never loads another chat's submission history.
        let unresolved = {
            let db = self.db.lock().map_err(|_| io::Error::other("Submission evidence unavailable"))?;
            let mut query = db.prepare(
                "SELECT body FROM ui_documents WHERE kind='application-submission'
                 AND id >= ?1 AND id < ?2 AND COALESCE(json_extract(body,'$.verified'),0)=0
                 AND COALESCE(json_extract(body,'$.not_dispatched'),0)=0"
            ).map_err(io::Error::other)?;
            let records = query.query_map(params![format!("{}/", self.chat), format!("{}0", self.chat)],
                |row| row.get::<_, String>(0)).map_err(io::Error::other)?;
            records.collect::<rusqlite::Result<Vec<_>>>().map_err(io::Error::other)?
        };
        let mut pending_submission = false;
        for raw in unresolved {
            let record: Value = serde_json::from_str(&raw)?;
            if let Some(turn) = record["turn"].as_str() {
                if scope(self.db, turn)? == root && has_pending_submission(std::slice::from_ref(&record), &verified) {
                    pending_submission = true;
                    break;
                }
            }
        }
        let mut check=kageha_workflow::completion::evaluate(&contract,&verified,candidate,
            pending_submission.then_some("An attempted submission has no verified original confirmation receipt. The task remains incomplete. Inspect its original receipt and current page; do not submit it again."));
        check.uncertain_effect = pending_submission;
        if check.status == CompletionStatus::Ready {
            if let Some(plan)=crate::ui_planning::approved_context(self.db,self.chat).map_err(|(_,error)|io::Error::other(error))?
                .filter(|plan|plan["turn_id"].as_str().is_some_and(|turn|scope(self.db,turn).is_ok_and(|value|value==root))) {
                let board=crate::ui_planning::board(&plan);
                if board["complete"] != true {
                    check.status=CompletionStatus::Continue;
                    check.instruction="The approved plan still has unfinished required outcomes. Continue the task and update the plan with confirmed evidence; do not treat an intermediate result as task completion.".into();
                }
            }
        }
        keep_rejected(self.db,self.chat,self.turn,&root,candidate,&mut check);
        // The decision's `completion_check` event is recorded from the runtime's frame
        // (`webui::record_gate_decision`), which adds the Progress_Fingerprint and the
        // stop reason (Requirement 18.1), so each decision has one event.
        Ok(check)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deferred_submission_requires_independent_proof_and_cannot_be_replayed() {
        let db=fixture();
        let context=json!({"url":"https://web.wd1.myworkdayjobs.com/en-US/Careers/job/Remote/AI_R15157/apply",
            "job_id":"R15157","employer":"workday:web.wd1.myworkdayjobs.com/en-US/Careers",
            "profile":"comet","session_digest":"session","account_digest":"","target_id":"tab","document_id":"doc"});
        record(&db,"chat","turn",&json!({"outcomes":[{"id":"submit","description":"Submit Workday application","kind":"action"}]}),&provable_kinds(["browser_click"])).unwrap();
        let args=json!({"outcome_id":"submit","confirmation_mode":"observe_after_submit"});
        let selection=json!({"target_id":"tab","document_id":"doc","_submission_context":context});
        prepare_submission(&db,"chat","turn",&args,&selection,"original").unwrap();
        assert!(prepare_submission(&db,"chat","turn",&args,&selection,"repeat").is_err());
        record(&db,"chat","other",&json!({"outcomes":[{"id":"other-submit","description":"Same job in a different tab","kind":"action"}]}),&provable_kinds(["browser_click"])).unwrap();
        let mut another=selection.clone();another["target_id"]=json!("other-tab");another["document_id"]=json!("other-doc");
        assert!(prepare_submission(&db,"chat","other",&json!({"outcome_id":"other-submit","confirmation_mode":"observe_after_submit"}),&another,"repeat-in-new-turn").is_err());
        let delivery=json!({"receipt":{"request_id":"original","workspace_id":"chat","action":"click","state":"verified","target_id":"tab","document_id":"doc"},"input_sent":true,"verification":"input_delivery"});
        remember_tool(&db,"chat","turn",&json!({"id":"delivery","name":"browser_click"}),&delivery.to_string()).unwrap();
        assert!(attach(&db,"chat","turn",&json!({"outcome_id":"submit","reference":"delivery"})).is_err());
        let proof=json!({"verification":"independent_site_confirmation","input_sent":false,
          "receipt":{"request_id":"proof","workspace_id":"chat","state":"verified","action":"verify","confirmation":{"original_request_id":"original","context":context,"kind":"workday_confirmation","text":"Your application has been successfully submitted.","observed_ms":1}},
          "original_receipt":{"request_id":"original","workspace_id":"chat","action":"click","state":"returned","target_id":"tab","document_id":"doc","submission_context":context,"resolution":{"request_id":"proof"}}});
        assert!(crate::ui_submission_confirmation::valid(&db,"chat","turn","submit",&proof).unwrap());
        let mut wrong=proof.clone();wrong["receipt"]["confirmation"]["context"]["document_id"]=json!("different-doc");
        assert!(!crate::ui_submission_confirmation::valid(&db,"chat","turn","submit",&wrong).unwrap());
        wrong=proof.clone();wrong["receipt"]["confirmation"]["context"]["url"]=json!("https://evil.test/job/Remote/AI_R15157/apply");
        assert!(!crate::ui_submission_confirmation::valid(&db,"chat","turn","submit",&wrong).unwrap());
        let mut proof=proof;
        proof["receipt"]["confirmation"]["kind"]=json!("workday_completion");
        proof["receipt"]["confirmation"]["context"]["url"]=json!("https://web.wd1.myworkdayjobs.com/en-US/Careers/jobTasks/completed/application");
        proof["receipt"]["confirmation"]["text"]=json!("Application Submitted\nThank you for your application.");
        assert!(crate::ui_submission_confirmation::valid(&db,"chat","turn","submit",&proof).unwrap());
        for (key,value) in [("document_id","different-doc"),("target_id","different-tab"),("session_digest","different-session"),("url","https://web.wd1.myworkdayjobs.com/en-US/OtherTenant/jobTasks/completed/application")] {
            let mut wrong=proof.clone();wrong["receipt"]["confirmation"]["context"][key]=json!(value);
            assert!(!crate::ui_submission_confirmation::valid(&db,"chat","turn","submit",&wrong).unwrap(),"{key}");
        }
        remember_tool(&db,"chat","turn",&json!({"id":"confirm","name":"browser_reconcile"}),&proof.to_string()).unwrap();
        assert!(attach_submission(&db,"chat","turn",&proof.to_string()).unwrap().is_some());
        assert!(verified_outcomes(&db,"chat",&contract(&db)).unwrap().contains_key("submit"));
    }
    fn independent_confirmation_fixture(db:&Database)->Value {
        let context=json!({"url":"https://www.linkedin.com/jobs/view/4489586201/","job_id":"4489586201","employer":"Iris Software Inc.","profile":"comet","session_digest":"session","account_digest":"person","target_id":"tab","document_id":"doc"});
        record(db,"chat","turn",&json!({"outcomes":[{"id":"submit","description":"Submit Iris application","kind":"action"}]}),&provable_kinds(["browser_click"])).unwrap();
        prepare_submission(db,"chat","turn",&json!({"outcome_id":"submit","expected_selector":"#old-confirmation","expected_text":"Application sent"}),&json!({"target_id":"tab","document_id":"doc","_submission_context":context}),"original").unwrap();
        json!({"verification":"independent_site_confirmation","input_sent":false,
          "receipt":{"request_id":"proof","workspace_id":"chat","state":"verified","action":"verify","target_id":"tab","document_id":"doc","confirmation":{"original_request_id":"original","context":context,"kind":"application_status","text":"Application submitted","observed_ms":1}},
          "original_receipt":{"request_id":"original","workspace_id":"chat","action":"click","state":"returned","target_id":"tab","document_id":"doc","expected":{"selector":"#old-confirmation","text":"Application sent"},"submission_context":context,"resolution":{"request_id":"proof"}}})
    }
    #[test]
    fn pending_receipt_lists_are_indexed_without_claiming_action_success() {
        let db=fixture();let value=independent_confirmation_fixture(&db);let mut original=value["original_receipt"].clone();original.as_object_mut().unwrap().remove("resolution");
        remember_tool(&db,"chat","turn",&json!({"id":"list","name":"browser_receipts"}),&json!({"receipts":[original]}).to_string()).unwrap();
        assert!(tool_evidence(&db,"chat","turn","receipt:original").unwrap().is_some());
        let error=error(attach(&db,"chat","turn",&json!({"outcome_id":"submit","reference":"receipt:original"})));
        assert_eq!(error["reason_code"],"unverified_receipt");assert_eq!(error["recovery"]["original_request_ids"],json!(["original"]));
        assert_eq!(error["recovery"]["next_call"]["arguments"]["outcome_id"],"submit");
        assert_eq!(error["next_tool"],"browser_receipts");
        assert_eq!(error["required_arguments"],json!(["outcome_id"]));
        assert_eq!(error["summary"],error["recovery"]["instruction"]);
        assert_eq!(error["recovery"]["terminal"],false);
    }
    #[test]
    fn unsupported_submission_evidence_has_one_terminal_instruction_without_retry_or_blocker_dead_ends() {
        let db=fixture();let value=independent_confirmation_fixture(&db);
        {let c=db.lock().unwrap();let mut reservation=ui_store::get(&c,"application-submission","chat/original").unwrap().unwrap();
            reservation["source_context"]["url"]=json!("https://unsupported.example/application");
            ui_store::put(&c,"application-submission","chat/original",reservation).unwrap();}
        remember_tool(&db,"chat","turn",&json!({"id":"original","name":"browser_receipts"}),&json!({"receipt":value["original_receipt"]}).to_string()).unwrap();
        let error=error(attach(&db,"chat","turn",&json!({"outcome_id":"submit","reference":"original"})));
        assert_eq!(error["error_code"],"invalid_outcome_evidence");
        assert_eq!(error["recovery"]["confirmation_supported"],false);
        assert_eq!(error["recovery"]["terminal"],true);
        assert!(error["next_tool"].is_null());assert_eq!(error["required_arguments"],json!([]));
        assert!(error["recovery"]["next_call"].is_null());assert!(error["recovery"]["confirmation_call"].is_null());
        assert_eq!(error["summary"],error["recovery"]["instruction"]);
        assert!(error["summary"].as_str().unwrap().contains("Do not retry evidence attachment"));
        let current=contract(&db);assert!(current.evidence.is_empty());assert!(current.blockers.is_empty());
        assert_eq!(current.outcomes[0].kind,OutcomeKind::Action);
    }
    #[test]
    fn ordinary_evidence_errors_do_not_invent_an_unsupported_application() {
        let db=fixture();record(&db,"chat","turn",&json!({"outcomes":[{"id":"summary","description":"Summarize the findings","kind":"answer"}]}),&provable_kinds(["web_search"])).unwrap();
        let error=error(attach(&db,"chat","turn",&json!({"outcome_id":"summary","reference":"missing-reference"})));
        assert_eq!(error["reason_code"],"evidence_not_found");assert!(error.get("recovery").is_none());
    }
    #[test]
    fn independent_confirmation_attaches_across_continued_turns_and_updates_ui_once() {
        let db=fixture();let value=independent_confirmation_fixture(&db);
        remember_tool(&db,"chat","earlier",&json!({"id":"confirm","name":"browser_reconcile"}),&value.to_string()).unwrap();
        let attached=attach_submission(&db,"chat","turn",&value.to_string()).unwrap().unwrap();assert_eq!(attached["verified_reference"],"receipt:proof");
        assert!(verified_outcomes(&db,"chat",&contract(&db)).unwrap().contains_key("submit"));
        let reservation=crate::ui_submission_confirmation::record(&db,"chat","turn","original").unwrap();assert_eq!(reservation["verified"],true);
        let again=attach_submission(&db,"chat","turn",&value.to_string()).unwrap().unwrap();assert_eq!(again["already_attached"],true);
        assert_eq!(events(&db,"outcome_snapshot").iter().filter(|v|v["payload"]["outcomes"][0]["label"]=="Confirmed by site").count(),1);
        assert!(attach(&db,"chat","other",&json!({"outcome_id":"submit","reference":"receipt:proof"})).is_err());
    }
    #[test]
    fn independent_confirmation_rejects_wrong_identity_and_weak_evidence() {
        let db=fixture();let value=independent_confirmation_fixture(&db);
        assert!(crate::ui_submission_confirmation::valid(&db,"chat","turn","submit",&value).unwrap());
        for key in ["job_id","employer","profile","session_digest","account_digest"] {
            let mut wrong=value.clone();wrong["receipt"]["confirmation"]["context"][key]=json!("other");assert!(!crate::ui_submission_confirmation::valid(&db,"chat","turn","submit",&wrong).unwrap(),"{key}");
        }
        let mut weak=value.clone();weak["receipt"]["confirmation"]["text"]=json!("View resume");assert!(!crate::ui_submission_confirmation::valid(&db,"chat","turn","submit",&weak).unwrap());
        let mut wrong=value.clone();wrong["original_receipt"]["resolution"]["request_id"]=json!("unrelated");assert!(!crate::ui_submission_confirmation::valid(&db,"chat","turn","submit",&wrong).unwrap());
        assert!(!crate::ui_submission_confirmation::valid(&db,"chat","other","submit",&value).unwrap());
    }
    #[test]
    fn legacy_confirmation_uses_pre_submission_identity_despite_later_spa_navigation() {
        let db=fixture();let mut value=independent_confirmation_fixture(&db);
        {let c=db.lock().unwrap();let mut reservation=ui_store::get(&c,"application-submission","chat/original").unwrap().unwrap();reservation.as_object_mut().unwrap().remove("source_context");ui_store::put(&c,"application-submission","chat/original",reservation).unwrap();}
        let observation=|job:&str,employer:&str|json!({"target_id":"tab","document_id":"doc","main_frame_id":"frame","profile":"comet","url":format!("https://www.linkedin.com/jobs/view/{job}/?trk=before"),"ax":[{"role":"RootWebArea","frame_id":"frame","name":format!("AI Architect | {employer} | LinkedIn")}]});
        card(&db,"turn",json!({"id":"source-before","name":"browser_snapshot","status":"done","result_preview":observation("4489586201","Iris Software Inc.").to_string()}));
        {let c=db.lock().unwrap();crate::webui::insert_event(&c,"chat","turn","event",json!({"kind":"outcome_snapshot","payload":{"outcomes":[{"id":"application-original"}]}})).unwrap();}
        card(&db,"turn",json!({"id":"source-after","name":"browser_snapshot","status":"done","result_preview":observation("2","Wrong Company").to_string()}));
        let reservation=crate::ui_submission_confirmation::record(&db,"chat","turn","original").unwrap();let source=crate::ui_submission_confirmation::legacy_source(&db,"chat",&reservation).unwrap();assert_eq!(source["call_id"],"source-before");
        value["original_receipt"]["confirmation_source"]=source.clone();value["original_receipt"]["submission_context"]["url"]=source["url"].clone();
        assert!(crate::ui_submission_confirmation::valid(&db,"chat","turn","submit",&value).unwrap());
        value["original_receipt"]["confirmation_source"]["call_id"]=json!("source-after");assert!(!crate::ui_submission_confirmation::valid(&db,"chat","turn","submit",&value).unwrap());
    }

    #[test]
    fn old_resume_link_amendments_are_restored_to_the_original_action_requirement() {
        let db=fixture();independent_confirmation_fixture(&db);
        {let c=db.lock().unwrap();let mut stored=load(&c,"chat","root").unwrap();let mut weak=stored.outcomes[0].clone();weak.kind=OutcomeKind::Validation;weak.proof=Some(Verification::BrowserFields{target_id:"other".into(),document_id:"other-doc".into(),assertions:vec![kageha_workflow::verification::FieldCheck{selector:"a.resume".into(),property:"text".into(),expected:json!("View resume")}]});stored.amend("submit",weak,"Old proof-search workaround").unwrap();save(&c,"chat","root",&stored).unwrap();}
        let reservation=crate::ui_submission_confirmation::record(&db,"chat","turn","original").unwrap();
        crate::ui_submission_confirmation::restore_original_requirement(&db,"chat","turn",&reservation).unwrap();
        let restored=contract(&db);assert_eq!(restored.outcomes[0].kind,OutcomeKind::Action);assert_eq!(serde_json::to_value(&restored.outcomes[0].proof).unwrap(),reservation["proof"]);assert!(!restored.history.last().unwrap().weakened);
    }
    #[test]
    fn attempted_submission_proof_cannot_be_replaced_by_a_resume_link() {
        let db=fixture();independent_confirmation_fixture(&db);
        let rejected=error(amend(&db,"chat","turn",&json!({"id":"submit","proof":{"type":"browser_postcondition","target_id":"other","document_id":"new","selector":"a.resume","text":"View resume"},"reason":"Use the resume link"}),&provable_kinds(["browser_click"])));
        assert_eq!(rejected["error_code"],"submission_proof_locked");
    }

    #[test]
    fn credential_rotation_proof_requires_the_scoped_commit_result() {
        let db=fixture();
        let args=json!({"operation":"commit_rotation","account":"user@example.test","credential":"cred_candidate"});
        let call=json!({"id":"commit-call","name":"browser_credential","arguments":args});
        let value=json!({"status":"completed","result":{"stored":true,"operation":"commit_rotation","origin":"https://example.test","credential":"cred_active",
            "commit_receipt":{"state":"verified","operation":"commit_rotation","workspace_id":"chat","origin":"https://example.test","account":"user@example.test","candidate":"cred_candidate","active":"cred_active"}}});
        remember_tool(&db,"chat","turn",&call,&value.to_string()).unwrap();
        let outcome:RequiredOutcome=serde_json::from_value(json!({"id":"rotation","description":"Retain the reset password","kind":"action",
            "proof":{"type":"credential_rotation","origin":"https://example.test","account":"user@example.test","candidate":"cred_candidate"}})).unwrap();
        assert!(outcome_proof(&db,"chat","turn",&outcome,"tool:commit-call").unwrap());
        record(&db,"chat","turn",&json!({"outcomes":[outcome]}),&provable_kinds(["browser_credential"])).unwrap();
        attach(&db,"chat","turn",&json!({"outcome_id":"rotation","reference":"tool:commit-call"})).unwrap();
        assert!(!outcome_proof(&db,"other-chat","turn",&outcome,"tool:commit-call").unwrap());
        let mut wrong=outcome.clone();
        wrong.proof=Some(Verification::CredentialRotation{origin:"https://example.test".into(),account:"user@example.test".into(),candidate:"cred_other".into()});
        assert!(!outcome_proof(&db,"chat","turn",&wrong,"tool:commit-call").unwrap());
        for operation in ["get","stage_rotation"] {
            let mut other=call.clone();other["id"]=json!(operation);other["arguments"]["operation"]=json!(operation);
            remember_tool(&db,"chat","turn",&other,&value.to_string()).unwrap();
            assert!(!proof(&db,"chat","turn",&OutcomeKind::Action,&format!("tool:{operation}")).unwrap());
        }
        let mut failed=value.clone();failed["result"]["commit_receipt"]["state"]=json!("failed");
        let mut other=call.clone();other["id"]=json!("failed");
        remember_tool(&db,"chat","turn",&other,&failed.to_string()).unwrap();
        assert!(!proof(&db,"chat","turn",&OutcomeKind::Action,"tool:failed").unwrap());
    }
    #[test]
    fn an_explicit_follow_up_keeps_the_canonical_task_and_original_receipt() {
        let db=fixture();
        let source=crate::webui::turn_id("chat","previous-request");
        {
            let c=db.lock().unwrap();
            c.execute_batch("INSERT INTO native_chat_turns(sequence,chat_id,request_id) VALUES(1,'chat','previous-request'),(2,'chat','current-request');").unwrap();
            c.execute("INSERT INTO ui_runtime_runs(id,spec,state) VALUES (?,? ,'finished')",params![source,json!({"turn":source,"session":"chat","request":"previous-request","input":{"configuration":{"request_budget_id":"root"}}}).to_string()]).unwrap();
            c.execute("UPDATE ui_runtime_runs SET spec=? WHERE id='turn'",[json!({"turn":"turn","session":"chat","request":"current-request","input":{"configuration":{"request_budget_id":"new-budget"}}}).to_string()]).unwrap();
        }
        let requirement=json!({"id":"submit","description":"Submit application","kind":"action","proof":{
            "type":"browser_postcondition","target_id":"tab","document_id":"doc","selector":"#success","text":"Received"}});
        let provable=provable_kinds(["browser_click"]);
        record(&db,"chat",&source,&json!({"outcomes":[requirement]}),&provable).unwrap();
        remember_tool(&db,"chat",&source,&json!({"id":"original","name":"browser_click"}),&json!({"receipt":{
            "state":"verified","request_id":"original-receipt","target_id":"tab","document_id":"doc","action":"click","expected":{"selector":"#success","text":"Received"}}}).to_string()).unwrap();
        attach(&db,"chat",&source,&json!({"outcome_id":"submit","reference":"receipt:original-receipt"})).unwrap();
        let result=record(&db,"chat","turn",&json!({"continue_previous":true,"outcomes":[{"id":"summary","description":"Report the remaining work","kind":"answer"}]}),&provable).unwrap();
        assert_eq!(result["task_scope"],"root");
        assert_eq!(scope(&db,"turn").unwrap(),scope(&db,&source).unwrap());
        let contract=load(&db.lock().unwrap(),"chat","root").unwrap();
        assert!(verified_outcomes(&db,"chat",&contract).unwrap().contains_key("submit"));
        assert_eq!(contract.outcomes.len(),2);
        assert_eq!(crate::ui_persistence::configuration_for_turn(&db,"turn").unwrap().unwrap()["request_budget_id"],"new-budget");
        assert!(continuation_scope(&db,"other-chat","turn").is_err());
        assert_eq!(scope(&db,"other").unwrap(),"other-root");
    }
    #[test]
    fn continued_task_clears_only_capability_blockers_when_tools_change() {
        let db=fixture();
        let source=crate::webui::turn_id("chat","previous-request");
        {
            let c=db.lock().unwrap();
            c.execute_batch("INSERT INTO native_chat_turns(sequence,chat_id,request_id) VALUES(1,'chat','previous-request'),(2,'chat','current-request');").unwrap();
            c.execute("INSERT INTO ui_runtime_runs(id,spec,state) VALUES (?,? ,'finished')",params![source,json!({"turn":source,"session":"chat","request":"previous-request","input":{"configuration":{"request_budget_id":"root"}}}).to_string()]).unwrap();
            c.execute("UPDATE ui_runtime_runs SET spec=? WHERE id='turn'",[json!({"turn":"turn","session":"chat","request":"current-request","input":{"configuration":{"request_budget_id":"new-budget"}}}).to_string()]).unwrap();
        }
        record(&db,"chat",&source,&json!({"outcomes":[{"id":"submit","description":"Submit application","kind":"action"}]}),&provable_kinds(["web_search"])).unwrap();
        assert!(load(&db.lock().unwrap(),"chat","root").unwrap().blockers[0].missing_capability);
        record(&db,"chat","turn",&json!({"continue_previous":true,"outcomes":[{"id":"summary","description":"Report result","kind":"answer"}]}),&provable_kinds(["browser_click"])).unwrap();
        let task=load(&db.lock().unwrap(),"chat","root").unwrap();
        assert!(task.blockers.is_empty());
        assert!(task.outcomes.iter().any(|outcome|outcome.id=="submit" && outcome.kind==OutcomeKind::Action));
        assert_eq!(kageha_workflow::completion::evaluate(&task,&BTreeMap::new(),"Report",None).status,CompletionStatus::Continue);
    }
    #[test]
    fn submission_proof_requires_the_declared_target_and_affirmative_postcondition() {
        let db=fixture();
        let outcome:RequiredOutcome=serde_json::from_value(json!({"id":"submit","description":"Submit the application","kind":"action",
            "proof":{"type":"browser_postcondition","target_id":"original-tab","document_id":"document","selector":"#success","text":"Application received"}})).unwrap();
        for (id,action,target,text) in [("fill","fill","original-tab","Application received"),
            ("wrong-tab","click","other-tab","Application received"),("wrong-text","click","original-tab","Form filled"),
            ("submit","click","original-tab","Application received")] {
            remember_tool(&db,"chat","turn",&json!({"id":id,"name":"browser_click","arguments":{}}),&json!({"receipt":{
                "request_id":format!("receipt-{id}"),"state":"verified","action":action,"target_id":target,"document_id":"document",
                "expected":{"selector":"#success","text":text}}}).to_string()).unwrap();
            assert_eq!(outcome_proof(&db,"chat","turn",&outcome,id).unwrap(),id=="submit","{id}");
        }
        let mut wrong_document=outcome.clone();
        if let Some(Verification::BrowserPostcondition{document_id,..})=&mut wrong_document.proof {*document_id="other-document".into();}
        assert!(!outcome_proof(&db,"chat","turn",&wrong_document,"submit").unwrap());
        let mut legacy=outcome.clone();legacy.proof=None;
        assert!(!outcome_proof(&db,"chat","turn",&legacy,"submit").unwrap());
        assert!(!outcome_proof(&db,"chat","other",&outcome,"submit").unwrap());
    }
    #[test]
    fn a_partial_validation_and_an_unrelated_successful_command_do_not_verify_the_requirement() {
        let db=fixture();
        let outcome:RequiredOutcome=serde_json::from_value(json!({"id":"fields","description":"Verify both required fields","kind":"validation",
            "proof":{"type":"browser_fields","target_id":"tab","document_id":"document","assertions":[
                {"selector":"#email","property":"value","expected":"a@example.test"},
                {"selector":"#country","property":"value","expected":"Canada"}]}})).unwrap();
        let email=json!({"selector":"#email","property":"value","expected":"a@example.test","passed":true});
        let country=json!({"selector":"#country","property":"value","expected":"Canada","passed":true});
        for (id,target,checks) in [("partial","tab",vec![email.clone()]),("wrong-tab","other",vec![email.clone(),country.clone()]),
            ("full","tab",vec![email,country])] {
            remember_tool(&db,"chat","turn",&json!({"id":id,"name":"browser_validate"}),&json!({"page":{"target_id":target,"document_id":"document"},"validation":{"passed":true,"checks":checks}}).to_string()).unwrap();
            assert_eq!(outcome_proof(&db,"chat","turn",&outcome,id).unwrap(),id=="full");
        }
        let command:RequiredOutcome=serde_json::from_value(json!({"id":"tests","description":"Run the suite","kind":"validation","proof":{"type":"command","command":"cargo test"}})).unwrap();
        for (id,command_text) in [("echo","echo all tests passed"),("suite","cargo test")] {
            remember_tool(&db,"chat","turn",&json!({"id":id,"name":"shell","arguments":{"command":command_text}}),&shell_result(json!(0),"passed")).unwrap();
            assert_eq!(outcome_proof(&db,"chat","turn",&command,id).unwrap(),id=="suite");
        }
    }
    #[test]
    fn blocker_repairs_offer_only_real_failed_calls_from_the_same_request() {
        let db=fixture();
        record(&db,"chat","turn",&json!({"outcomes":[{"id":"submit","description":"Submit","kind":"action"}]}),&provable_kinds(["browser_click"])).unwrap();
        for (turn,id,name,status) in [("turn","actual-failure","web_fetch","failed"),
            ("other","wrong-request","web_fetch","failed"),("turn","bookkeeping","report_blocker","failed"),
            ("turn","success","web_fetch","done")] {
            card(&db,turn,json!({"id":id,"name":name,"status":status}));
        }
        let rejected=error(report_blocker(&db,"chat","turn",&json!({"outcome_id":"submit","category":"missing_access","reason":"Sign-in required","references":["invented"]})));
        assert_eq!(rejected["failed_attempts"],json!([{"tool_call_id":"actual-failure","tool":"web_fetch","turn_id":"turn"}]));
    }
    #[test]
    fn receipt_and_tool_namespaces_cannot_resolve_to_each_other() {
        let db=fixture();
        remember_tool(&db,"chat","turn",&json!({"id":"collision","name":"shell"}),&shell_result(json!(0),"")).unwrap();
        remember_tool(&db,"chat","turn",&json!({"id":"other","name":"browser_click"}),&json!({"receipt":{"state":"verified","request_id":"collision"}}).to_string()).unwrap();
        assert!(!proof(&db,"chat","turn",&OutcomeKind::Action,"tool:collision").unwrap());
        assert!(proof(&db,"chat","turn",&OutcomeKind::Action,"receipt:collision").unwrap());
        assert!(!proof(&db,"chat","turn",&OutcomeKind::Action,"collision").unwrap());
    }
    #[test]
    fn browser_assertions_require_unique_visible_exact_matches() {
        let assertion=json!({"selector":"#email","property":"value","expected":"test@example.com"});
        let item=json!({"visible":true,"value":"test@example.com"});
        let result=json!({"matches":{"items":[item.clone()],"next_offset":null}});
        assert_eq!(field_assertion(&result,&assertion)["passed"],true);
        assert_eq!(field_assertion(&json!({"matches":{"items":[item.clone(),item.clone()]}}),&assertion)["passed"],false);
        assert_eq!(field_assertion(&json!({"matches":{"items":[{"visible":false,"value":"test@example.com"}]}}),&assertion)["passed"],false);
        assert_eq!(field_assertion(&result,&json!({"property":"value","expected":"wrong"}))["passed"],false);
        // String vs Boolean coercion for validity.valid and checked
        let valid_item = json!({"visible": true, "validity": {"valid": true}});
        let valid_result = json!({"matches": {"items": [valid_item], "next_offset": null}});
        assert_eq!(field_assertion(&valid_result, &json!({"selector": "#email", "property": "validity.valid", "expected": "true"}))["passed"], true);
        assert_eq!(field_assertion(&valid_result, &json!({"selector": "#email", "property": "validity.valid", "expected": true}))["passed"], true);
        assert_eq!(field_assertion(&valid_result, &json!({"selector": "#email", "property": "validity.valid", "expected": "false"}))["passed"], false);
        assert_eq!(field_assertion(&valid_result, &json!({"selector": "#email", "property": "validity.valid", "expected": false}))["passed"], false);
        let checked_item = json!({"visible": true, "checked": true});
        let checked_result = json!({"matches": {"items": [checked_item], "next_offset": null}});
        assert_eq!(field_assertion(&checked_result, &json!({"selector": "#agree", "property": "checked", "expected": "true"}))["passed"], true);
        assert_eq!(field_assertion(&checked_result, &json!({"selector": "#agree", "property": "checked", "expected": true}))["passed"], true);
        assert!(!verified_validation("browser_inspect",&json!({"validation":{"passed":true,"checks":[{"passed":true}]}})));
        assert!(!verified_validation("browser_validate",&json!({"validation":{"passed":true,"checks":[]}})));
    }
    #[test]
    fn durable_receipts_accept_original_ids_and_survive_clipped_cards() {
        let connection=rusqlite::Connection::open_in_memory().unwrap();
        ui_store::initialize(&connection).unwrap();
        connection.execute_batch("CREATE TABLE ui_events(sequence INTEGER,chat_id TEXT,turn_id TEXT,event TEXT,payload TEXT);").unwrap();
        let db=crate::agent_host::DatabaseState::open(connection).unwrap();
        let receipt=json!({"receipt":{"state":"verified","request_id":"receipt-original"},"large": "x".repeat(20000)});
        remember_tool(&db,"chat","turn",&json!({"id":"call-original","name":"browser_fill"}),&receipt.to_string()).unwrap();
        assert!(proof(&db,"chat","turn",&OutcomeKind::Action,"receipt-original").unwrap());
        assert!(proof(&db,"chat","turn",&OutcomeKind::Action,"call-original").unwrap());
        assert!(!proof(&db,"chat","other",&OutcomeKind::Action,"receipt-original").unwrap());
        assert!(!proof(&db,"chat","turn",&OutcomeKind::Validation,"call-original").unwrap());
        remember_tool(&db,"chat","turn",&json!({"id":"validate","name":"browser_validate"}),&json!({"validation":{"passed":true,"checks":[{"passed":true}]}}).to_string()).unwrap();
        assert!(proof(&db,"chat","turn",&OutcomeKind::Validation,"validate").unwrap());
        // A later result with the same call ID cannot replace original proof.
        remember_tool(&db,"chat","turn",&json!({"id":"call-original","name":"browser_fill"}),&json!({"receipt":{"state":"verified","request_id":"replacement"}}).to_string()).unwrap();
        assert!(!proof(&db,"chat","turn",&OutcomeKind::Action,"replacement").unwrap());
    }
    #[test]
    fn resumed_contracts_reuse_original_proof_and_artifact_changes_invalidate_it() {
        use sha2::{Digest,Sha256};
        let directory=tempfile::tempdir().unwrap();
        let connection=rusqlite::Connection::open(directory.path().join("native.sqlite")).unwrap();
        ui_store::initialize(&connection).unwrap();
        crate::ui_uploads::initialize(&connection).unwrap();
        connection.execute_batch("CREATE TABLE ui_runtime_runs(id TEXT PRIMARY KEY,spec TEXT);CREATE TABLE ui_sessions(id TEXT PRIMARY KEY);INSERT INTO ui_sessions VALUES ('chat');CREATE TABLE ui_turn_artifacts(chat_id TEXT,turn_id TEXT,path TEXT);").unwrap();
        for (turn,root) in [("original","root"),("resumed","root"),("other","different-root")] {
            connection.execute("INSERT INTO ui_runtime_runs VALUES (?,?)",params![turn,json!({"turn":turn,"input":{"configuration":{"request_budget_id":root}}}).to_string()]).unwrap();
        }
        let db=crate::agent_host::DatabaseState::open(connection).unwrap();
        let artifact_root=crate::ui_uploads::root(&db,"chat").unwrap();
        std::fs::write(artifact_root.join("answer.txt"),b"verified deliverable").unwrap();
        {
            let connection=db.lock().unwrap();
            connection.execute("INSERT INTO ui_uploads VALUES (?,?,?,?,?,?)",params!["chat","answer.txt","answer.txt","text/plain",format!("{:x}",Sha256::digest(b"verified deliverable")),20]).unwrap();
            connection.execute("INSERT INTO ui_turn_artifacts VALUES ('chat','original','answer.txt')",[]).unwrap();
        }
        record(&db,"chat","original",&json!({"outcomes":[{"id":"deliver","description":"Create the requested document","kind":"artifact"}]}),&provable_kinds(["generate_image"])).unwrap();
        attach(&db,"chat","resumed",&json!({"outcome_id":"deliver","reference":"answer.txt","evidence_turn":"original"})).unwrap();
        assert_eq!(load(&db.lock().unwrap(),"chat","root").unwrap().evidence[0].turn,"original");
        assert!(proof(&db,"chat","original",&OutcomeKind::Artifact,"answer.txt").unwrap());
        assert!(attach(&db,"chat","resumed",&json!({"outcome_id":"deliver","reference":"answer.txt","evidence_turn":"other"})).is_err());
        std::fs::write(artifact_root.join("answer.txt"),b"changed deliverable").unwrap();
        assert!(!proof(&db,"chat","original",&OutcomeKind::Artifact,"answer.txt").unwrap());
    }
    #[test]
    fn provable_kinds_follow_the_turn_tool_list() {
        assert_eq!(provable_kinds(["read_file","web_search"]),BTreeSet::from([OutcomeKind::Answer]));
        assert_eq!(provable_kinds(["text_to_speech","browser_click","remote_exec"]),
            BTreeSet::from([OutcomeKind::Answer,OutcomeKind::Artifact,OutcomeKind::Action,OutcomeKind::Validation]));
        assert_eq!(provable_kinds(["browser_snapshot","browser_validate"]),BTreeSet::from([OutcomeKind::Answer,OutcomeKind::Validation]));
        assert_eq!(provable_kinds(["write_file"]),BTreeSet::from([OutcomeKind::Answer,OutcomeKind::Artifact]));
        assert_eq!(provable_kinds(["shell"]),BTreeSet::from([OutcomeKind::Answer,OutcomeKind::Artifact,OutcomeKind::Validation]));
        // MCP and other unknown tools prove nothing; an answer is always provable.
        assert_eq!(provable_kinds(["github__create_issue"]),BTreeSet::from([OutcomeKind::Answer]));
        assert_eq!(provable_kinds(std::iter::empty::<&str>()),BTreeSet::from([OutcomeKind::Answer]));
    }
    #[test]
    fn remote_artifact_requires_the_exact_host_path_and_digest() {
        let fixture = crate::audited_session_tests::Fixture::for_chat("chat");
        let db = &fixture.db;
        let turn = &fixture.turn;
        let digest = "a".repeat(64);
        let value = json!({"ok":true,"host_id":"ssh-one","path":"outputs/report.json","sha256":digest});
        remember_tool(db,"chat",turn,&json!({"id":"verify","name":"remote_verify_file","arguments":{"path":"outputs/report.json"}}),&value.to_string()).unwrap();
        remember_tool(db,"chat",turn,&json!({"id":"exec","name":"remote_exec","arguments":{"command":"true"}}),&json!({"ok":true,"exit_code":0,"stdout":value.to_string()}).to_string()).unwrap();
        let mut outcome: RequiredOutcome = serde_json::from_value(json!({"id":"report","kind":"artifact","description":"Leave report on SSH", "proof":{
            "type":"remote_file","host_id":"ssh-one","path":"outputs/report.json","sha256":digest}})).unwrap();
        assert!(outcome_proof(db,"chat",turn,&outcome,"tool:verify").unwrap());
        assert!(!outcome_proof(db,"chat",turn,&outcome,"tool:exec").unwrap());
        assert!(!outcome_proof(db,"different-chat",turn,&outcome,"tool:verify").unwrap());
        if let Some(Verification::RemoteFile{host_id,..})=&mut outcome.proof { *host_id = "ssh-two".into(); }
        assert!(!outcome_proof(db,"chat",turn,&outcome,"tool:verify").unwrap());
    }
    #[test]
    fn provable_kinds_follow_the_registry_and_match_accepted_proof() {
        let receipt=json!({"receipt":{"state":"verified","request_id":"r"}});
        for spec in crate::ui_tools::CAPABILITIES {
            let mut expected=BTreeSet::from([OutcomeKind::Answer]);
            expected.extend(spec.proves.iter().copied());
            assert_eq!(provable_kinds([spec.name]),expected,"{}",spec.name);
            // A tool that proves an action must produce an accepted receipt. Some
            // accepted receipts are intermediate steps and prove no final outcome.
            // Credential rotation uses a scoped commit receipt and host arguments,
            // not the generic action receipt. The dedicated test above checks both
            // successful commits and rejected mismatches.
            if spec.proves.contains(&OutcomeKind::Action) && spec.name != "browser_credential" {
                assert!(verified_action(spec.name,&receipt),"{}",spec.name);
            }
        }
        assert!(!verified_action("browser_credential", &receipt));
        assert!(verified_validation("remote_exec",&json!({"ok":true,"exit_code":0})));
        assert!(verified_validation("browser_validate",&json!({"validation":{"passed":true,"checks":[{"passed":true}]}})));
    }
    #[test]
    fn record_stores_verify_origin_and_task_type() {
        let connection=rusqlite::Connection::open_in_memory().unwrap();
        ui_store::initialize(&connection).unwrap();
        connection.execute_batch("CREATE TABLE ui_runtime_runs(id TEXT PRIMARY KEY,spec TEXT);").unwrap();
        let db=crate::agent_host::DatabaseState::open(connection).unwrap();
        let provable=provable_kinds(["write_file"]);
        let args=json!({"task_type":"interview-prep","outcomes":[
            {"id":"plan","description":"Study plan","kind":"artifact","verify":"outputs/plan.md exists"},
            {"id":"summary","description":"Short summary","kind":"answer","verify":"Final reply","origin":"agent"}]});
        let value=record(&db,"chat","turn",&args,&provable).unwrap();
        assert_eq!(value["accepted"],json!(["plan","summary"])); assert_eq!(value["task_type"],"interview-prep");
        let contract=load(&db.lock().unwrap(),"chat","turn").unwrap();
        let plan=contract.outcomes.iter().find(|o|o.id=="plan").unwrap();
        assert_eq!((plan.verify.as_str(),plan.origin),("outputs/plan.md exists",kageha_workflow::completion::Origin::User));
        assert_eq!(contract.outcomes.iter().find(|o|o.id=="summary").unwrap().origin,kageha_workflow::completion::Origin::Agent);
        // The first task_type of a request is kept.
        record(&db,"chat","turn",&json!({"task_type":"other","outcomes":[{"id":"more","description":"More","kind":"answer"}]}),&provable).unwrap();
        assert_eq!(load(&db.lock().unwrap(),"chat","turn").unwrap().task_type.as_deref(),Some("interview-prep"));
        for bad in [json!("Has Spaces"),json!(""),json!(7),json!("-lead"),json!("x".repeat(65))] {
            assert!(record(&db,"chat","fresh",&json!({"task_type":bad,"outcomes":[{"id":"a","description":"A","kind":"answer"}]}),&provable).is_err());
        }
        assert!(record(&db,"chat","fresh",&json!({"outcomes":[{"id":"a","description":"A","kind":"answer","origin":"system"}]}),&provable).is_err());
        assert!(record(&db,"chat","fresh",&json!({"outcomes":[{"id":"a","description":"A","kind":"answer","verify":"v".repeat(4001)}]}),&provable).is_err());
        assert!(load(&db.lock().unwrap(),"chat","fresh").unwrap().outcomes.is_empty());
    }
    /// A full WebUI schema with three turns: `turn` and `earlier` share the request
    /// `root`; `other` belongs to a different request.
    fn fixture() -> Database {
        let connection=rusqlite::Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE native_chat_turns(sequence INTEGER PRIMARY KEY,chat_id TEXT,request_id TEXT,message TEXT,answer TEXT,status TEXT);").unwrap();
        crate::webui::initialize(&connection).unwrap();
        for (turn,root) in [("turn","root"),("earlier","root"),("other","other-root")] {
            connection.execute("INSERT INTO ui_runtime_runs(id,spec,state) VALUES (?,?,'finished')",params![turn,json!({"turn":turn,"input":{"configuration":{"request_budget_id":root}}}).to_string()]).unwrap();
        }
        crate::agent_host::DatabaseState::open(connection).unwrap()
    }
    fn events(db:&Database,kind:&str)->Vec<Value> {
        let connection=db.lock().unwrap();
        let mut query=connection.prepare("SELECT payload FROM ui_events WHERE event='event' AND json_extract(payload,'$.kind')=? ORDER BY sequence").unwrap();
        query.query_map([kind],|row|row.get::<_,String>(0)).unwrap().map(|text|serde_json::from_str(&text.unwrap()).unwrap()).collect()
    }
    fn card(db:&Database,turn:&str,card:Value) { crate::webui::insert_event(&db.lock().unwrap(),"chat",turn,"tool_card",card).unwrap(); }
    fn error(result:io::Result<Value>)->Value { serde_json::from_str(&result.unwrap_err().to_string()).unwrap() }
    fn contract(db:&Database)->Contract { load(&db.lock().unwrap(),"chat","root").unwrap() }
    #[test]
    fn each_dispatched_submission_must_be_resolved_individually() {
        let verified=BTreeMap::from([("first".to_owned(),vec!["receipt:first".to_owned()])]);
        let first=json!({"outcome_id":"first","verified":true});
        let undispatched=json!({"outcome_id":"second","not_dispatched":true});
        assert!(!has_pending_submission(&[first.clone(),undispatched],&verified));
        assert!(has_pending_submission(&[first,json!({"outcome_id":"second","verified":false})],&verified));
        assert!(has_pending_submission(&[json!({"turn":"legacy","verified":false})],&verified));
        assert!(has_pending_submission(&[json!({"outcome_id":"first","request_id":"other","verified":false})],&verified));
        assert!(!has_pending_submission(&[json!({"outcome_id":"first","request_id":"first","verified":false})],&verified));
    }
    #[test]
    fn submission_reservation_binds_proof_and_fences_duplicates() {
        let db=fixture();
        record(&db,"chat","turn",&json!({"outcomes":[{"id":"submit","description":"Submit application","kind":"action"}]}),&provable_kinds(["browser_click"])).unwrap();
        let args=json!({"outcome_id":"submit","expected_selector":"#success","expected_text":"Received"});
        let selection=json!({"target_id":"tab","document_id":"doc"});
        assert!(prepare_submission(&db,"chat","turn",&json!({}),&selection,"missing").is_err());
        prepare_submission(&db,"chat","turn",&args,&selection,"request").unwrap();
        assert!(contract(&db).outcomes[0].proof.is_some());
        assert_eq!(events(&db,"outcome_amended").len(),1);
        let duplicate=prepare_submission(&db,"chat","earlier",&args,&selection,"duplicate").unwrap_err();
        assert_eq!(serde_json::from_str::<Value>(&duplicate.to_string()).unwrap()["reason_code"],"submission_already_attempted");
        let content=json!({"receipt":{"request_id":"request","state":"verified","action":"click","target_id":"tab","document_id":"doc","result_document_id":"after-navigation","expected":{"selector":"#success","text":"Received"}}}).to_string();
        remember_tool(&db,"chat","earlier",&json!({"id":"recovery","name":"browser_reconcile"}),&content).unwrap();
        assert!(attach_submission(&db,"chat","earlier",&content).unwrap().unwrap()["error"].is_null());
        attach_submission(&db,"chat","earlier",&content).unwrap();
        remember_tool(&db,"chat","turn",&json!({"id":"again","name":"browser_receipts"}),&content).unwrap();
        assert_eq!(attach_submission(&db,"chat","turn",&content).unwrap().unwrap()["already_attached"],true);
        assert_eq!(contract(&db).evidence.len(),1);
        assert!(attach_submission(&db,"chat","other",&content).unwrap().unwrap()["error"].is_object());
    }
    #[test]
    fn evidence_diagnostics_and_changed_requirements_preserve_submission() {
        let db=fixture();
        let provable=provable_kinds(["browser_click"]);
        record(&db,"chat","turn",&json!({"outcomes":[{"id":"submit","description":"Submit application","kind":"action"}]}),&provable).unwrap();
        assert_eq!(error(attach(&db,"chat","turn",&json!({"outcome_id":"submit","reference":"fake"})))["reason_code"],"missing_proof");
        let args=json!({"outcome_id":"submit","expected_selector":"#success","expected_text":"Received"});
        prepare_submission(&db,"chat","turn",&args,&json!({"target_id":"tab","document_id":"doc"}),"request").unwrap();
        assert_eq!(error(attach(&db,"chat","turn",&json!({"outcome_id":"submit","reference":"fake"})))["reason_code"],"evidence_not_found");
        assert_eq!(error(attach(&db,"chat","turn",&json!({"outcome_id":"submit","reference":"fake","evidence_turn":"other"})))["reason_code"],"wrong_request_scope");
        let pending=json!({"receipt":{"request_id":"pending","action":"click","state":"pending"}}).to_string();
        remember_tool(&db,"chat","turn",&json!({"id":"pending","name":"browser_click"}),&pending).unwrap();
        assert_eq!(error(attach(&db,"chat","turn",&json!({"outcome_id":"submit","reference":"tool:pending"})))["reason_code"],"unverified_receipt");
        for (action,target,expected) in [("fill","tab","incompatible_evidence_kind"),("upload","tab","incompatible_evidence_kind"),("click","wrong","proof_mismatch")] {
            let content=json!({"receipt":{"request_id":"request","state":"verified","action":action,"target_id":target,"document_id":"doc","expected":{"selector":"#success","text":"Received"}}}).to_string();
            remember_tool(&db,"chat","turn",&json!({"id":action,"name":"browser_click"}),&content).unwrap();
            assert_eq!(error(attach(&db,"chat","turn",&json!({"outcome_id":"submit","reference":format!("tool:{action}")})))["reason_code"],expected);
        }
        assert_eq!(error(amend(&db,"chat","turn",&json!({"id":"submit","proof":{"type":"browser_postcondition","target_id":"tab","document_id":"doc","selector":"#success","text":"Different"},"reason":"Updated confirmation"}),&provable))["error_code"],"submission_proof_locked");
        let content=json!({"receipt":{"request_id":"request","state":"verified","action":"click","target_id":"tab","document_id":"doc","expected":{"selector":"#success","text":"Received"}}}).to_string();
        remember_tool(&db,"chat","turn",&json!({"id":"original","name":"browser_click"}),&content).unwrap();
        assert_eq!(attach_submission(&db,"chat","turn",&content).unwrap().unwrap()["verified_reference"],"receipt:request");
        assert_eq!(contract(&db).evidence.len(),1);
    }
    #[test]
    fn only_authoritative_non_dispatch_releases_submission() {
        let db=fixture();
        record(&db,"chat","turn",&json!({"outcomes":[{"id":"submit","description":"Submit","kind":"action"}]}),&provable_kinds(["browser_click"])).unwrap();
        let args=json!({"outcome_id":"submit","expected_selector":"h1","expected_text":"Received"});
        let selection=json!({"target_id":"tab","document_id":"doc"});
        prepare_submission(&db,"chat","turn",&args,&selection,"first").unwrap();
        submission_dispatch_failed(&db,"chat","first",&io::Error::other("transport lost")).unwrap();
        assert!(prepare_submission(&db,"chat","turn",&args,&selection,"second").is_err());
        submission_dispatch_failed(&db,"chat","first",&io::Error::other(json!({"execution_phase":"not_dispatched","input_dispatched":false}).to_string())).unwrap();
        let snapshots=events(&db,"outcome_snapshot");
        assert_eq!(snapshots.len(),1);
        assert_eq!(snapshots[0]["payload"]["outcomes"][0],json!({"id":"application-first","label":"Submission not sent","status":"fail","evidence":[]}));
        submission_not_dispatched(&db,"chat","first").unwrap();
        assert_eq!(events(&db,"outcome_snapshot").len(),1);
        prepare_submission(&db,"chat","turn",&args,&selection,"second").unwrap();
    }
    #[test]
    fn recovered_non_dispatch_receipt_releases_only_its_matching_submission() {
        let db=fixture();
        record(&db,"chat","turn",&json!({"outcomes":[{"id":"submit","description":"Submit","kind":"action"}]}),&provable_kinds(["browser_click"])).unwrap();
        let args=json!({"outcome_id":"submit","expected_selector":"h1","expected_text":"Received"});
        let selection=json!({"target_id":"tab","document_id":"doc"});
        prepare_submission(&db,"chat","turn",&args,&selection,"first").unwrap();
        let receipt=json!({"receipt":{"request_id":"first","state":"not_dispatched","action":"click","target_id":"tab","document_id":"doc","expected":{"selector":"h1","text":"Received"}}}).to_string();
        // An unrecorded or unrelated result must never release a real attempt.
        assert!(attach_submission(&db,"chat","earlier",&receipt).unwrap().unwrap()["error"].is_object());
        remember_tool(&db,"chat","earlier",&json!({"id":"unrelated","name":"shell"}),&receipt).unwrap();
        assert!(attach_submission(&db,"chat","earlier",&receipt).unwrap().unwrap()["error"].is_object());
        let mut wrong:Value=serde_json::from_str(&receipt).unwrap();
        wrong["receipt"]["target_id"]=json!("other");
        remember_tool(&db,"chat","earlier",&json!({"id":"wrong","name":"browser_receipts"}),&wrong.to_string()).unwrap();
        assert!(attach_submission(&db,"chat","earlier",&wrong.to_string()).unwrap().unwrap()["error"].is_object());
        assert!(prepare_submission(&db,"chat","turn",&args,&selection,"second").is_err());
        remember_tool(&db,"chat","earlier",&json!({"id":"right","name":"browser_receipts"}),&receipt).unwrap();
        assert!(attach_submission(&db,"chat","other",&receipt).unwrap().unwrap()["error"].is_object());
        assert_eq!(attach_submission(&db,"chat","earlier",&receipt).unwrap().unwrap()["not_dispatched"],true);
        assert_eq!(events(&db,"outcome_snapshot").len(),1);
        let event_turn:String=db.lock().unwrap().query_row("SELECT turn_id FROM ui_events WHERE event='event' AND json_extract(payload,'$.kind')='outcome_snapshot'",[],|row|row.get(0)).unwrap();
        assert_eq!(event_turn,"turn", "recovery updates the original submission turn");
        assert_eq!(attach_submission(&db,"chat","earlier",&receipt).unwrap().unwrap()["not_dispatched"],true);
        assert_eq!(events(&db,"outcome_snapshot").len(),1);
        prepare_submission(&db,"chat","turn",&args,&selection,"second").unwrap();
        let record=ui_store::get(&db.lock().unwrap(),"application-submission","chat/first").unwrap().unwrap();
        assert_eq!(record["not_dispatched"],true);
    }
    #[test]
    fn verified_submission_cannot_be_released_by_a_conflicting_receipt() {
        let db=fixture();
        record(&db,"chat","turn",&json!({"outcomes":[{"id":"submit","description":"Submit","kind":"action"}]}),&provable_kinds(["browser_click"])).unwrap();
        let args=json!({"outcome_id":"submit","expected_selector":"h1","expected_text":"Received"});
        let selection=json!({"target_id":"tab","document_id":"doc"});
        prepare_submission(&db,"chat","turn",&args,&selection,"first").unwrap();
        {
            let c=db.lock().unwrap();
            let mut record=ui_store::get(&c,"application-submission","chat/first").unwrap().unwrap();
            record["verified"]=json!(true);
            ui_store::put(&c,"application-submission","chat/first",record).unwrap();
        }
        let receipt=json!({"receipt":{"request_id":"first","state":"not_dispatched","action":"click","target_id":"tab","document_id":"doc","expected":{"selector":"h1","text":"Received"}}}).to_string();
        remember_tool(&db,"chat","turn",&json!({"id":"read","name":"browser_receipts"}),&receipt).unwrap();
        assert_eq!(attach_submission(&db,"chat","turn",&receipt).unwrap().unwrap()["error"]["reason_code"],"contradictory_receipt");
        submission_not_dispatched(&db,"chat","first").unwrap();
        assert!(events(&db,"outcome_snapshot").is_empty());
        assert!(prepare_submission(&db,"chat","turn",&args,&selection,"second").is_err());
    }
    #[test]
    fn preflight_conflict_does_not_reserve_or_overwrite_and_legacy_is_ignored() {
        let db=fixture();
        record(&db,"chat","turn",&json!({"outcomes":[{"id":"submit","description":"Submit application","kind":"action","proof":{"type":"browser_postcondition","target_id":"tab","document_id":"doc","selector":"#success","text":"Received"}}]}),&provable_kinds(["browser_click"])).unwrap();
        let err=prepare_submission(&db,"chat","turn",&json!({"outcome_id":"submit","expected_selector":"#success","expected_text":"Other"}),&json!({"target_id":"tab","document_id":"doc"}),"request").unwrap_err();
        assert_eq!(serde_json::from_str::<Value>(&err.to_string()).unwrap()["mismatched_fields"],json!(["text"]));
        assert!(ui_store::get(&db.lock().unwrap(),"application-submission","chat/request").unwrap().is_none());
        ui_store::put(&db.lock().unwrap(),"application-submission","chat/legacy",json!({"turn":"turn","confirmation":"Received"})).unwrap();
        assert!(attach_submission(&db,"chat","turn",&json!({"receipt":{"request_id":"legacy","state":"verified"}}).to_string()).unwrap().is_none());
    }
    #[test]
    fn unprovable_outcomes_are_reported_to_the_model_without_failing_the_call() {
        let db=fixture();
        let args=json!({"outcomes":[{"id":"reply","description":"Answer the question","kind":"answer"},{"id":"file","description":"Save the report","kind":"artifact"}]});
        let value=record(&db,"chat","turn",&args,&provable_kinds(["web_search"])).unwrap();
        let rejected=events(&db,"outcome_rejected");
        assert_eq!(rejected.len(),1);
        assert_eq!((&rejected[0]["payload"]["outcome_id"],&rejected[0]["payload"]["outcome_kind"],&rejected[0]["payload"]["code"]),
            (&json!("file"),&json!("artifact"),&json!("outcome_unsatisfiable")));
        assert_eq!(value["accepted"],json!(["reply"]));
        assert_eq!(value["rejected"][0]["id"],"file");
        assert_eq!(value["rejected"][0]["code"],"outcome_unsatisfiable");
        assert_eq!(value["rejected"][0]["alternatives"],json!(["answer"]));
        assert_eq!(value["required_outcomes"].as_array().unwrap().len(),2);
        let stored=contract(&db);
        assert_eq!(stored.blockers.len(),1);
        assert_eq!(stored.blockers[0].outcome_id,"file");
        assert!(stored.blockers[0].reason.contains("required capability is unavailable"));
        assert_eq!(kageha_workflow::completion::evaluate(&stored,&BTreeMap::new(),"Answer",None).status,CompletionStatus::Partial);
        // Once a file tool becomes available the host-created blocker is cleared.
        record(&db,"chat","turn",&args,&provable_kinds(["write_file"])).unwrap();
        assert!(contract(&db).blockers.is_empty());
        assert_eq!(kageha_workflow::completion::evaluate(&contract(&db),&BTreeMap::new(),"Answer",None).status,CompletionStatus::Continue);
        let only_rejected=json!({"outcomes":[{"id":"submit","description":"Submit the form","kind":"action"}]});
        let value=record(&db,"chat","turn",&only_rejected,&provable_kinds(["web_search"])).unwrap();
        assert_eq!(value["accepted"],json!([])); assert_eq!(value["rejected"][0]["kind"],"action");
        assert_eq!(contract(&db).outcomes.len(),3);
        assert_eq!(events(&db,"outcome_rejected").len(),2);
        // Fully accepted records emit no rejection event.
        record(&db,"chat","turn",&json!({"outcomes":[{"id":"more","description":"More","kind":"answer"}]}),&provable_kinds(["web_search"])).unwrap();
        assert_eq!(events(&db,"outcome_rejected").len(),2);
    }
    #[test]
    fn intermediate_browser_inputs_do_not_count_as_action_proof_capability() {
        let db=fixture();
        let args=json!({"outcomes":[{"id":"submit","description":"Submit the application","kind":"action"}]});
        let intermediate=provable_kinds(["browser_fill","browser_fill_credential","browser_press","browser_select","browser_check","browser_upload"]);
        let value=record(&db,"chat","turn",&args,&intermediate).unwrap();
        assert_eq!(value["rejected"][0]["id"],"submit");
        assert_eq!(contract(&db).outcomes[0].kind,OutcomeKind::Action);
        assert!(contract(&db).blockers[0].missing_capability);
        for name in ["browser_click","computer_execute"] {
            assert!(provable_kinds([name]).contains(&OutcomeKind::Action));
        }
        record(&db,"chat","turn",&args,&provable_kinds(["browser_click"])).unwrap();
        assert!(contract(&db).blockers.is_empty());
        assert_eq!(kageha_workflow::completion::evaluate(&contract(&db),&BTreeMap::new(),"",None).status,CompletionStatus::Continue);
    }
    #[test]
    fn enabling_capability_keeps_legacy_and_reported_blockers() {
        let db=fixture();
        let args=json!({"outcomes":[{"id":"file","description":"Save the report","kind":"artifact"}]});
        record(&db,"chat","turn",&args,&provable_kinds(["write_file"])).unwrap();
        {
            let mut c=db.lock().unwrap();
            let mut task=load(&c,"chat","root").unwrap();
            task.block(Blocker {outcome_id:"file".into(),category:BlockerCategory::MissingInput,
                reason:"Need the document contents".into(),references:vec![],at:now(),missing_capability:false}).unwrap();
            save(&mut c,"chat","root",&task).unwrap();
        }
        record(&db,"chat","turn",&args,&provable_kinds(["write_file"])).unwrap();
        assert_eq!(contract(&db).blockers[0].reason,"Need the document contents");
        assert!(!contract(&db).blockers[0].missing_capability);
    }
    #[test]
    fn amend_records_the_change_flags_weakening_and_rejects_unprovable_kinds() {
        let db=fixture();
        let provable=provable_kinds(["write_file"]);
        record(&db,"chat","turn",&json!({"outcomes":[{"id":"report","description":"Write the report","kind":"artifact","verify":"outputs/report.md exists and covers every section"}]}),&provable).unwrap();
        let value=amend(&db,"chat","turn",&json!({"id":"report","kind":"answer","reason":"The user only wants a chat summary"}),&provable).unwrap();
        assert_eq!((value["weakened"].clone(),value["after"]["kind"].clone(),value["after"]["origin"].clone()),(json!(true),json!("answer"),json!("user")));
        assert_eq!(value["after"]["verify"],"outputs/report.md exists and covers every section");
        assert!(value["instruction"].as_str().unwrap().contains("Changed requirements"));
        let amended=events(&db,"outcome_amended");
        assert_eq!(amended.len(),1);
        let payload=&amended[0]["payload"];
        assert_eq!((&payload["outcome_id"],&payload["before"]["kind"],&payload["after"]["kind"],&payload["weakened"]),
            (&json!("report"),&json!("artifact"),&json!("answer"),&json!(true)));
        assert_eq!(payload["reason"],"The user only wants a chat summary");
        // Rewording without a weaker kind or shorter verify text is not weakening.
        let value=amend(&db,"chat","turn",&json!({"id":"report","description":"Summarize the report in chat","reason":"Clarified wording"}),&provable).unwrap();
        assert_eq!((value["weakened"].clone(),value["after"]["kind"].clone()),(json!(false),json!("answer")));
        assert_eq!(contract(&db).history.len(),2);
        // A kind no available tool can prove is rejected and nothing changes.
        let rejected=error(amend(&db,"chat","turn",&json!({"id":"report","kind":"action","reason":"Submit it instead"}),&provable));
        assert_eq!((&rejected["error_code"],&rejected["kind"]),(&json!("outcome_unsatisfiable"),&json!("action")));
        assert_eq!(rejected["alternatives"],json!(["answer","artifact","report_blocker"]));
        assert!(amend(&db,"chat","turn",&json!({"id":"report","description":"No reason"}),&provable).is_err());
        assert!(amend(&db,"chat","turn",&json!({"id":"missing","description":"New","reason":"r"}),&provable).is_err());
        assert!(amend(&db,"chat","turn",&json!({"id":"report","kind":"essay","reason":"r"}),&provable).is_err());
        let stored=contract(&db);
        assert_eq!((stored.history.len(),stored.outcomes[0].kind),(2,OutcomeKind::Answer));
        assert_eq!(events(&db,"outcome_amended").len(),2);
    }
    #[test]
    fn report_blocker_needs_an_unverified_outcome_and_a_failed_attempt_from_this_request() {
        let db=fixture();
        record(&db,"chat","turn",&json!({"outcomes":[
            {"id":"submit","description":"Submit the form","kind":"action"},
            {"id":"fill","description":"Complete the action","kind":"action","proof":{"type":"browser_postcondition","target_id":"tab","document_id":"document","selector":"#success","text":"Done"}}]}),&provable_kinds(["browser_click"])).unwrap();
        let args=|references:Value|json!({"outcome_id":"submit","category":"missing_access","reason":"The portal needs a login the user has not given","references":references});
        let rejected=error(report_blocker(&db,"chat","turn",&args(json!([]))));
        assert_eq!(rejected["error_code"],"invalid_blocker");
        assert!(rejected["summary"].as_str().unwrap().contains("references"),"{rejected}");
        let rejected=error(report_blocker(&db,"chat","turn",&json!({"outcome_id":"submit","category":"bored","reason":" ","references":["x"]})));
        let summary=rejected["summary"].as_str().unwrap();
        assert!(summary.contains("category") && summary.contains("reason"),"{summary}");
        // A successful card, a failure in another request, a failed task-state call and an
        // unknown id are all unrelated.
        card(&db,"turn",json!({"id":"call-ok","name":"web_fetch","status":"done"}));
        card(&db,"other",json!({"id":"call-other","name":"browser_click","status":"failed"}));
        card(&db,"turn",json!({"id":"call-self","name":"report_blocker","status":"failed"}));
        let rejected=error(report_blocker(&db,"chat","turn",&args(json!(["call-ok","call-other","call-self","nothing"]))));
        assert!(rejected["summary"].as_str().unwrap().contains("none of [call-ok, call-other, call-self, nothing]"),"{rejected}");
        assert!(contract(&db).blockers.is_empty() && events(&db,"outcome_blocked").is_empty());
        // A failed card from an earlier turn of the same request is accepted; only resolved references are kept.
        card(&db,"earlier",json!({"id":"call-fail","name":"browser_click","status":"running"}));
        card(&db,"earlier",json!({"id":"call-fail","name":"browser_click","status":"failed","result_preview":"Login required"}));
        let value=report_blocker(&db,"chat","turn",&args(json!(["nothing","call-fail"]))).unwrap();
        assert_eq!((&value["blocked"],&value["references"]),(&json!(true),&json!(["call-fail"])));
        let stored=contract(&db);
        assert_eq!(stored.blockers.len(),1);
        assert_eq!((stored.blockers[0].category,stored.blockers[0].references.clone()),(BlockerCategory::MissingAccess,vec!["call-fail".to_owned()]));
        let blocked=events(&db,"outcome_blocked");
        assert_eq!(blocked.len(),1);
        assert_eq!((&blocked[0]["payload"]["outcome_id"],&blocked[0]["payload"]["category"]),(&json!("submit"),&json!("missing_access")));
        // An outcome with verified proof cannot be blocked.
        remember_tool(&db,"chat","turn",&json!({"id":"click-1","name":"browser_click"}),&json!({"receipt":{"state":"verified","request_id":"receipt-1","action":"click","target_id":"tab","document_id":"document","expected":{"selector":"#success","text":"Done"}}}).to_string()).unwrap();
        attach(&db,"chat","turn",&json!({"outcome_id":"fill","reference":"click-1"})).unwrap();
        let rejected=error(report_blocker(&db,"chat","turn",&json!({"outcome_id":"fill","category":"missing_access","reason":"No login","references":["call-fail"]})));
        assert!(rejected["summary"].as_str().unwrap().contains("already has verified proof"),"{rejected}");
        let rejected=error(report_blocker(&db,"chat","turn",&json!({"outcome_id":"unknown","category":"missing_access","reason":"No login","references":["call-fail"]})));
        assert!(rejected["summary"].as_str().unwrap().contains("record it first"),"{rejected}");
        assert_eq!(contract(&db).blockers.len(),1);
    }
    #[test]
    fn skipped_or_timed_out_questions_and_denied_approvals_are_failed_attempts() {
        use kageha_contracts::policy::{ActionClass,ApprovalDecision,ApprovalRequest,Authority,GrantScope};
        let db=fixture();
        card(&db,"turn",json!({"id":"ask-skipped","name":"ask_human","status":"done","result_preview":json!({"status":"skipped","answer":null}).to_string()}));
        card(&db,"turn",json!({"id":"ask-answered","name":"ask_human","status":"done","result_preview":json!({"status":"answered","answer":"Canada"}).to_string()}));
        {
            let connection=db.lock().unwrap();
            for (id,turn,response) in [("question-timed-out","earlier",None),("question-waiting","turn",None),
                ("question-skipped","turn",Some(json!({"status":"skipped","answer":null}))),("question-answered","earlier",Some(json!({"status":"answered","answer":"yes"}))),
                ("question-elsewhere","other",Some(json!({"status":"skipped","answer":null})))] {
                connection.execute("INSERT INTO ui_questions VALUES (?,'chat',?,'Which region?','[]',?)",params![id,turn,response.map(|value|value.to_string())]).unwrap();
            }
            let journal=crate::ui_approvals::owned(&connection).unwrap();
            for (id,session,approved) in [("approval-denied","chat",false),("approval-approved","chat",true),("approval-elsewhere","elsewhere",false)] {
                let request=ApprovalRequest{authority:Authority{session:session.into(),turn:"earlier".into(),binding:"workspace".into(),generation:1,
                    resource:"https://example.test".into(),input_digest:kageha_policy::input_digest(&json!({"id":id}))},
                    action:"browser_click".into(),class:ActionClass::Mutation,detail:"Submit the form".into()};
                let record=journal.create(id,&request,false).unwrap();
                journal.decide(id,&ApprovalDecision{expected_revision:record.revision,authority:request.authority.clone(),approved,scope:GrantScope::Once}).unwrap();
            }
        }
        let resolves=|reference:&str|failed_attempt(&db,"chat","turn","root",reference).unwrap();
        for reference in ["ask-skipped","question-timed-out","question-skipped","approval-denied"] { assert!(resolves(reference),"{reference}"); }
        for reference in ["ask-answered","question-waiting","question-answered","question-elsewhere","approval-approved","approval-elsewhere",""] { assert!(!resolves(reference),"{reference}"); }
    }
    #[test]
    fn external_failure_accepts_a_scoped_failed_attempt_immediately() {
        let db=fixture();
        record(&db,"chat","turn",&json!({"outcomes":[{"id":"fetch","description":"Download the dataset","kind":"answer"}]}),&provable_kinds(std::iter::empty::<&str>())).unwrap();
        card(&db,"turn",json!({"id":"call-503","name":"web_fetch","status":"failed"}));
        let args=json!({"outcome_id":"fetch","category":"external_failure","reason":"The dataset host returns 503","references":["call-503"]});
        report_blocker(&db,"chat","turn",&args).unwrap();
        let stored=contract(&db);
        assert_eq!((stored.blockers.len(),stored.blockers[0].category),(1,BlockerCategory::ExternalFailure));
        assert_eq!(events(&db,"outcome_blocked").len(),1);
        report_blocker(&db,"chat","turn",&json!({"outcome_id":"fetch","category":"missing_input","reason":"Which dataset version?","references":["call-503"]})).unwrap();
        assert_eq!(contract(&db).blockers[0].category,BlockerCategory::MissingInput);
    }
    fn workspace_fixture(directory:&std::path::Path)->Database {
        let connection=rusqlite::Connection::open(directory.join("native.sqlite")).unwrap();
        crate::ui_uploads::initialize(&connection).unwrap();
        crate::native_media::initialize(&connection).unwrap();
        crate::ui_workspace::initialize(&connection).unwrap();
        initialize(&connection).unwrap();
        connection.execute_batch("CREATE TABLE ui_sessions(id TEXT PRIMARY KEY);INSERT INTO ui_sessions VALUES ('chat');").unwrap();
        crate::agent_host::DatabaseState::open(connection).unwrap()
    }
    fn rejected_rows(db:&Database)->Vec<(i64,String,String,Option<String>)> {
        let connection=db.lock().unwrap();
        let mut query=connection.prepare("SELECT n,content,reason,draft_path FROM ui_rejected_answers WHERE chat_id='chat' AND turn_id='root' ORDER BY n").unwrap();
        query.query_map([],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).unwrap().map(Result::unwrap).collect()
    }
    fn decision(status:CompletionStatus,instruction:&str)->CompletionCheck {
        CompletionCheck{status,instruction:instruction.into(),..CompletionCheck::default()}
    }
    #[test]
    fn rejected_answers_are_kept_and_long_ones_become_registered_drafts() {
        let directory=tempfile::tempdir().unwrap();
        let db=workspace_fixture(directory.path());
        // Ready decisions and empty candidates keep nothing.
        let mut ready=decision(CompletionStatus::Ready,"");
        keep_rejected(&db,"chat","turn","root","The final answer",&mut ready);
        let mut empty=decision(CompletionStatus::Continue,"Attach evidence");
        keep_rejected(&db,"chat","turn","root","  \n",&mut empty);
        assert!(rejected_rows(&db).is_empty());
        assert_eq!(empty.instruction,"Attach evidence");
        // A short rejected answer gets a row but no draft.
        let mut short=decision(CompletionStatus::Continue,"Attach evidence for report");
        keep_rejected(&db,"chat","turn","root","Short answer",&mut short);
        assert_eq!(rejected_rows(&db),vec![(1,"Short answer".into(),"Attach evidence for report".into(),None)]);
        assert_eq!(short.instruction,"Attach evidence for report");
        // A second rejection of the same request over 4 KB is n=2 and saved as a registered draft.
        let long="# Report\n".repeat(500);
        assert!(long.len()>DRAFT_BYTES);
        let mut rejected=decision(CompletionStatus::Continue,"Attach evidence for report");
        keep_rejected(&db,"chat","turn","root",&long,&mut rejected);
        let rows=rejected_rows(&db);
        assert_eq!((rows[1].0,rows[1].1.as_str(),rows[1].3.as_deref()),(2,long.as_str(),Some("workspace/drafts/answer-2.md")));
        assert!(rejected.instruction.contains("saved at workspace/drafts/answer-2.md (artifact_path workspace/drafts/answer-2.md)"),"{}",rejected.instruction);
        assert!(proof(&db,"chat","turn",&OutcomeKind::Artifact,"workspace/drafts/answer-2.md").unwrap());
        let workspace=crate::ui_workspace::open_for_chat(&db,"chat").unwrap();
        assert_eq!(std::fs::read_to_string(workspace.root().join("drafts/answer-2.md")).unwrap(),long);
    }
    #[test]
    fn a_long_first_rejection_is_answer_1_and_existing_drafts_are_not_overwritten() {
        let directory=tempfile::tempdir().unwrap();
        let db=workspace_fixture(directory.path());
        let long="x".repeat(DRAFT_BYTES+1);
        let mut check=decision(CompletionStatus::Blocked,"");
        keep_rejected(&db,"chat","turn","root",&long,&mut check);
        let rows=rejected_rows(&db);
        assert_eq!((rows[0].0,rows[0].2.as_str(),rows[0].3.as_deref()),(1,"blocked",Some("workspace/drafts/answer-1.md")));
        assert!(check.instruction.contains("workspace/drafts/answer-1.md"));
        assert!(proof(&db,"chat","turn",&OutcomeKind::Artifact,"workspace/drafts/answer-1.md").unwrap());
        // Another request in the same chat starts at n=1 but keeps the first draft intact.
        let other="y".repeat(DRAFT_BYTES+1);
        let mut check=decision(CompletionStatus::Continue,"Still unmet");
        keep_rejected(&db,"chat","next","other-root",&other,&mut check);
        let path:(i64,String)=db.lock().unwrap().query_row("SELECT n,draft_path FROM ui_rejected_answers WHERE turn_id='other-root'",[],|row|Ok((row.get(0)?,row.get(1)?))).unwrap();
        assert_eq!(path,(1,"workspace/drafts/answer-2.md".into()));
        let workspace=crate::ui_workspace::open_for_chat(&db,"chat").unwrap();
        assert_eq!(std::fs::read_to_string(workspace.root().join("drafts/answer-1.md")).unwrap(),long);
        assert_eq!(std::fs::read_to_string(workspace.root().join("drafts/answer-2.md")).unwrap(),other);
    }
    #[test]
    fn a_failed_draft_write_still_keeps_the_row_and_the_decision() {
        let directory=tempfile::tempdir().unwrap();
        let db=workspace_fixture(directory.path());
        // A file where the drafts directory should be makes the draft write fail.
        let workspace=crate::ui_workspace::open_for_chat(&db,"chat").unwrap();
        std::fs::remove_dir_all(workspace.root().join("drafts")).unwrap();
        std::fs::write(workspace.root().join("drafts"),b"not a directory").unwrap();
        let mut check=decision(CompletionStatus::Continue,"Attach evidence");
        keep_rejected(&db,"chat","turn","root",&"z".repeat(DRAFT_BYTES+1),&mut check);
        assert_eq!((check.status,check.instruction.as_str()),(CompletionStatus::Continue,"Attach evidence"));
        let rows=rejected_rows(&db);
        assert_eq!((rows.len(),rows[0].3.clone()),(1,None));
        // Without the table the gate decision is still returned unchanged.
        let bare=fixture();
        bare.lock().unwrap().execute_batch("DROP TABLE ui_rejected_answers").unwrap();
        let mut check=decision(CompletionStatus::Continue,"Attach evidence");
        keep_rejected(&bare,"chat","turn","root","answer",&mut check);
        assert_eq!(check.instruction,"Attach evidence");
    }
    #[test]
    fn adapter_receipts_are_required_and_command_stdout_cannot_impersonate_them() {
        let receipt=json!({"receipt":{"state":"verified","request_id":"original"}});
        assert!(verified_action("browser_reconcile",&receipt));
        assert!(!verified_action("remote_exec",&receipt));
        assert!(!verified_action("integration_read",&receipt));
        assert!(!verified_action("browser_reconcile",&json!({"receipt":{"state":"returned","request_id":"original"}})));
        assert!(!verified_action("browser_click",&json!({"receipt":{"state":"verified"}})));
        assert!(!verified_validation("read_file",&json!({"ok":true,"exit_code":0})));
        assert!(verified_validation("remote_exec",&json!({"ok":true,"exit_code":0})));
        assert!(!verified_validation("remote_exec",&json!({"ok":true,"exit_code":1})));
    }
    fn shell_result(exit_code:Value,stdout:&str)->String {
        json!({"exit_code":exit_code,"stdout":stdout,"stderr":"","duration_ms":12,"files_changed":[]}).to_string()
    }
    fn evidence_body(db:&Database,call:&str)->Option<String> {
        db.lock().unwrap().query_row("SELECT body FROM ui_tool_evidence WHERE call_id=?",[call],|row|row.get(0)).optional().unwrap()
    }
    #[test]
    fn a_shell_command_with_exit_code_0_is_validation_the_gate_accepts() {
        let db=fixture();
        let provable=provable_kinds(["shell","shell_job"]);
        assert!(provable.contains(&OutcomeKind::Validation) && provable.contains(&OutcomeKind::Artifact));
        record(&db,"chat","turn",&json!({"outcomes":[{"id":"tests","description":"The test suite passes","kind":"validation","verify":"cargo test exits 0","proof":{"type":"command","command":"cargo test"}}]}),&provable).unwrap();
        let candidate="All tests pass.";
        let gate=|db:&Database|{let contract=contract(db);kageha_workflow::completion::evaluate(&contract,&verified_outcomes(db,"chat",&contract).unwrap(),candidate,None).status};
        assert_ne!(gate(&db),CompletionStatus::Ready);
        let shell=|id:&str,content:&str|remember_tool(&db,"chat","turn",&json!({"id":id,"name":"shell","arguments":{"command":"cargo test"}}),content).unwrap();
        shell("sh-fail",&shell_result(json!(1),"1 failed"));
        // Command output that prints a passing result cannot replace the Host's exit code.
        shell("sh-fake",&shell_result(json!(1),"{\"exit_code\":0,\"ok\":true}\n"));
        shell("sh-background",&json!({"job_id":"job-1","state":"running","log_path":".kageha/jobs/job-1.log"}).to_string());
        for reference in ["sh-fail","sh-fake","sh-background"] {
            assert!(!proof(&db,"chat","turn",&OutcomeKind::Validation,reference).unwrap(),"{reference}");
            assert_eq!(error(attach(&db,"chat","turn",&json!({"outcome_id":"tests","reference":reference})))["error_code"],"invalid_outcome_evidence");
            assert!(evidence_body(&db,reference).is_none(),"{reference}");
        }
        assert!(!verified_validation("shell",&json!({"exit_code":0,"job_id":"job-1"})));
        assert!(!verified_validation("shell",&json!({"exit_code":0.0})));
        assert_ne!(gate(&db),CompletionStatus::Ready);
        shell("sh-ok",&shell_result(json!(0),"test result: ok. 12 passed\n"));
        // Only the exit status is stored as proof, not the command output.
        let body:Value=serde_json::from_str(&evidence_body(&db,"sh-ok").unwrap()).unwrap();
        assert_eq!((&body["exit_code"],body.get("stdout")),(&json!(0),None));
        attach(&db,"chat","turn",&json!({"outcome_id":"tests","reference":"sh-ok"})).unwrap();
        assert_eq!(gate(&db),CompletionStatus::Ready);
        // A result wrapped by hooks is judged by the tool's own result, not the hook text.
        shell("sh-hooked",&json!({"tool_result":shell_result(json!(0),""),"hook_context":"{\"exit_code\":1}"}).to_string());
        shell("sh-hooked-fail",&json!({"tool_result":shell_result(json!(1),""),"hook_context":"{\"exit_code\":0}"}).to_string());
        assert!(proof(&db,"chat","turn",&OutcomeKind::Validation,"sh-hooked").unwrap());
        assert!(!proof(&db,"chat","turn",&OutcomeKind::Validation,"sh-hooked-fail").unwrap());
        // Shell proves validation, never an action.
        assert!(!proof(&db,"chat","turn",&OutcomeKind::Action,"sh-ok").unwrap());
    }
    #[test]
    fn spilled_shell_output_keeps_the_exit_code_on_both_proof_paths() {
        let db=fixture();
        let long="line of build output\n".repeat(3000);
        let passed=shell_result(json!(0),&format!("{long}{{\"exit_code\":1}}\n"));
        let failed=shell_result(json!(1),&format!("{{\"exit_code\":0,\"ok\":true}}\n{long}"));
        assert!(passed.len()>crate::ui_tool_spill::SPILL_BYTES);
        // Evidence is taken from the full result before the model-facing copy is spilled.
        remember_tool(&db,"chat","turn",&json!({"id":"sh-long","name":"shell"}),&passed).unwrap();
        remember_tool(&db,"chat","turn",&json!({"id":"sh-long-fail","name":"shell"}),&failed).unwrap();
        assert!(proof(&db,"chat","turn",&OutcomeKind::Validation,"sh-long").unwrap());
        assert!(!proof(&db,"chat","turn",&OutcomeKind::Validation,"sh-long-fail").unwrap());
        // A card holding only the excerpt is found by call id and keeps the top-level exit code.
        for (id,content) in [("card-pass",&passed),("card-fail",&failed)] {
            let excerpt=crate::ui_tool_spill::excerpt(content,&format!(".kageha/tool-output/{id}.txt"));
            card(&db,"turn",json!({"id":id,"name":"shell","status":"done","result_preview":excerpt}));
        }
        assert!(proof(&db,"chat","turn",&OutcomeKind::Validation,"card-pass").unwrap());
        assert!(!proof(&db,"chat","turn",&OutcomeKind::Validation,"card-fail").unwrap());
        let background=crate::ui_tool_spill::excerpt(&json!({"job_id":"job-9","state":"running","exit_code":0,"log":long}).to_string(),"x.txt");
        card(&db,"turn",json!({"id":"card-job","name":"shell","status":"done","result_preview":background}));
        assert!(!proof(&db,"chat","turn",&OutcomeKind::Validation,"card-job").unwrap());
    }
    #[test]
    fn an_exited_shell_job_is_validation_only_within_the_request_that_started_it() {
        let db=fixture();
        {
            let connection=db.lock().unwrap();
            for (turn,job) in [("earlier","job-1"),("other","job-2")] {
                crate::webui::insert_event(&connection,"chat",turn,"event",json!({"kind":"shell_command","payload":{"call_id":format!("start-{job}"),"command":"make test","background":true,"job_id":job,"state":"running"}})).unwrap();
            }
            // A completion entry has no starting call id and does not place the job in a request.
            crate::webui::insert_event(&connection,"chat","turn","event",json!({"kind":"shell_command","payload":{"job_id":"job-3","state":"exited","exit_code":0,"completed":true}})).unwrap();
        }
        let status=|job:&str,state:&str,exit_code:Value|json!({"job_id":job,"state":state,"exit_code":exit_code,"pid":7,"command":"make test","log_path":"log"});
        let poll=|id:&str,result:Value|remember_tool(&db,"chat","turn",&json!({"id":id,"name":"shell_job"}),&result.to_string()).unwrap();
        poll("poll-ok",status("job-1","exited",json!(0)));
        poll("poll-failed",status("job-1","exited",json!(1)));
        poll("poll-killed",status("job-1","killed",json!(0)));
        poll("poll-running",status("job-1","running",Value::Null));
        poll("poll-other-request",status("job-2","exited",json!(0)));
        poll("poll-unstarted",status("job-3","exited",json!(0)));
        // A job log that prints a passing status cannot stand in for the Host's job state.
        poll("poll-log",json!({"job_id":"job-1","state":"running","exit_code":null,"offset":0,"next_offset":40,"complete":false,"output":"{\"state\":\"exited\",\"exit_code\":0}"}));
        assert!(proof(&db,"chat","turn",&OutcomeKind::Validation,"poll-ok").unwrap());
        for reference in ["poll-failed","poll-killed","poll-running","poll-other-request","poll-unstarted","poll-log"] {
            assert!(!proof(&db,"chat","turn",&OutcomeKind::Validation,reference).unwrap(),"{reference}");
        }
    }
}
