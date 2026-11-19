//! Progress_Fingerprint inputs of a WebUI turn (Requirement 7.1).
//!
//! The runtime fingerprints this state after each tool batch and each Completion_Gate
//! decision. Everything here is read-only: outcome statuses, verified evidence and
//! blockers come from the request-scoped outcome contract, file hashes from the
//! Task_Workspace manifest, and capability names from the turn's tool catalog.
use crate::{ui_task_completion, ui_tools::Tools, ui_workspace, webui::Database};
#[cfg(test)]
use crate::ui_uploads;
use kageha_contracts::execution::ProgressInputs;
#[cfg(test)]
use kageha_workspace::task::WORKSPACE_DIR;
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
};

/// Progress inputs for `tools`' turn, offered tools included.
pub(crate) fn for_turn(tools: &Tools<'_>) -> io::Result<ProgressInputs> {
    let specs = crate::native_chat::ToolHost::specs(tools);
    inputs(
        tools.db,
        tools.chat,
        tools.turn,
        specs.iter().filter_map(|spec| spec["function"]["name"].as_str()),
    )
}

/// Each outcome is `verified` when current evidence proves it, else `blocked` when it has
/// an accepted blocker, else `unmet`. Verification wins over a blocker, as in the gate.
pub(crate) fn inputs<'a>(
    db: &Database,
    chat: &str,
    turn: &str,
    capabilities: impl IntoIterator<Item = &'a str>,
) -> io::Result<ProgressInputs> {
    let root = ui_task_completion::scope(db, turn)?;
    let contract = {
        let connection = db
            .lock()
            .map_err(|_| io::Error::other("Completion state unavailable"))?;
        ui_task_completion::load(&connection, chat, &root)?
    };
    let mut progress = ProgressInputs::default();
    let mut verified = BTreeSet::new();
    for evidence in &contract.evidence {
        let Some(outcome) = contract.outcomes.iter().find(|outcome| outcome.id == evidence.outcome_id) else {
            continue;
        };
        if ui_task_completion::outcome_proof(db, chat, &evidence.turn, outcome, &evidence.reference)?
            && ui_workspace::evidence_current(db, chat, &evidence.turn, &outcome.id, &evidence.reference)?
        {
            verified.insert(outcome.id.as_str());
            progress
                .verified_evidence
                .insert(format!("{}:{}", outcome.id, evidence.reference));
        }
    }
    // One blocker per outcome is kept, so outcome and category identify it.
    progress.blockers = contract
        .blockers
        .iter()
        .map(|blocker| format!("{}:{}", blocker.outcome_id, blocker.category.as_str()))
        .collect();
    for outcome in &contract.outcomes {
        let status = if verified.contains(outcome.id.as_str()) {
            "verified"
        } else if contract.blockers.iter().any(|blocker| blocker.outcome_id == outcome.id) {
            "blocked"
        } else {
            "unmet"
        };
        progress.outcome_statuses.insert(outcome.id.clone(), status.into());
    }
    progress.workspace = workspace_hashes(db, chat)?;
    progress.capabilities = capabilities.into_iter().map(str::to_owned).collect();
    Ok(progress)
}

/// Path -> sha256 of the files the Task_Workspace manifest records. A chat that never
/// used its workspace has none, and reading does not create one.
fn workspace_hashes(db: &Database, chat: &str) -> io::Result<BTreeMap<String, String>> {
    if ui_workspace::existing_root(db, chat)?.is_none() {
        return Ok(BTreeMap::new());
    }
    Ok(ui_workspace::open_for_chat(db, chat)?
        .manifest_entries()?
        .into_iter()
        .map(|entry| (entry.path, entry.sha256))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kageha_workflow::completion::{Blocker, BlockerCategory};
    use serde_json::json;

    fn database(directory: &std::path::Path) -> Database {
        let connection = rusqlite::Connection::open(directory.join("native.sqlite")).unwrap();
        crate::ui_store::initialize(&connection).unwrap();
        ui_uploads::initialize(&connection).unwrap();
        crate::native_media::initialize(&connection).unwrap();
        ui_workspace::initialize(&connection).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE ui_sessions(id TEXT PRIMARY KEY);INSERT INTO ui_sessions VALUES ('chat');
                 CREATE TABLE ui_runtime_runs(id TEXT PRIMARY KEY,spec TEXT);",
            )
            .unwrap();
        crate::agent_host::DatabaseState::open(connection).unwrap()
    }

    #[test]
    fn statuses_evidence_blockers_files_and_capabilities_come_from_host_state() {
        let directory = tempfile::tempdir().unwrap();
        let db = database(directory.path());
        // No contract and no workspace yet: only capabilities, and nothing is created.
        let empty = inputs(&db, "chat", "turn", ["write_file", "read_file"]).unwrap();
        assert_eq!(
            empty,
            ProgressInputs {
                capabilities: BTreeSet::from(["read_file".into(), "write_file".into()]),
                ..Default::default()
            }
        );
        assert!(!ui_uploads::root(&db, "chat").unwrap().join(WORKSPACE_DIR).exists());

        let provable = ui_task_completion::provable_kinds(["write_file", "browser_click"]);
        ui_task_completion::record(&db, "chat", "turn", &json!({"outcomes":[
            {"id":"brief","description":"Write the brief","kind":"artifact"},
            {"id":"submit","description":"Submit the form","kind":"action"}]}), &provable).unwrap();
        let workspace = ui_workspace::open_for_chat(&db, "chat").unwrap();
        let change = workspace.write("outputs/brief.md", b"draft").unwrap();
        ui_workspace::register_changes(&db, "chat", "turn", std::slice::from_ref(&change)).unwrap();
        let pending = inputs(&db, "chat", "turn", ["write_file"]).unwrap();
        assert_eq!(pending.outcome_statuses["brief"], "unmet");
        assert_eq!(pending.workspace["outputs/brief.md"], change.sha256);
        assert!(pending.workspace.contains_key("todo.md"));

        ui_task_completion::attach(&db, "chat", "turn",
            &json!({"outcome_id":"brief","reference":"workspace/outputs/brief.md"})).unwrap();
        {
            let connection = db.lock().unwrap();
            let mut contract = ui_task_completion::load(&connection, "chat", "turn").unwrap();
            contract.block(Blocker { outcome_id: "submit".into(), category: BlockerCategory::MissingAccess,
                reason: "No login".into(), references: vec![], at: 1.0, missing_capability: false }).unwrap();
            crate::ui_store::put(&connection, "task-completion", "chat/turn",
                serde_json::to_value(&contract).unwrap()).unwrap();
        }
        let done = inputs(&db, "chat", "turn", ["write_file"]).unwrap();
        assert_eq!(
            done.outcome_statuses,
            BTreeMap::from([("brief".into(), "verified".into()), ("submit".into(), "blocked".into())])
        );
        assert_eq!(done.verified_evidence, BTreeSet::from(["brief:workspace/outputs/brief.md".into()]));
        assert_eq!(done.blockers, BTreeSet::from(["submit:missing_access".into()]));

        // New bytes invalidate the evidence and change the recorded hash.
        let rewritten = workspace.write("outputs/brief.md", b"changed").unwrap();
        let changed = inputs(&db, "chat", "turn", ["write_file"]).unwrap();
        assert_eq!(changed.outcome_statuses["brief"], "unmet");
        assert!(changed.verified_evidence.is_empty());
        assert_eq!(changed.workspace["outputs/brief.md"], rewritten.sha256);
    }
}
