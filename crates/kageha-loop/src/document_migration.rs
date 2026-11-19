//! Host-owned document projection. Importing does not start agents or reuse approvals.
use kageha_migration::{
    documents::{Document, DocumentSink, Domain},
    Importer, Report,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{io, path::Path};
fn sql(e: rusqlite::Error) -> io::Error {
    io::Error::other(e)
}
fn error(message: &str) -> io::Error {
    io::Error::other(message)
}
struct Projection {
    db: Connection,
    profiles_owned: bool,
}
fn definition(value: &Value) -> Value {
    let mut v = value.clone();
    v["schema_version"] = json!(1);
    v["builtin"] = json!(false);
    for (field, default) in [
        ("pinned_skills", json!([])),
        ("tool_policy", json!({"allow":[],"deny":[]})),
        ("model_preferences", json!({})),
        (
            "knowledge",
            json!({"inherit_workspace":true,"extra_kb_ids":[]}),
        ),
    ] {
        if v[field].is_null() {
            v[field] = default;
        }
    }
    v
}
fn revision_time(id: &str, value: &Value) -> f64 {
    let named = id
        .find('Z')
        .and_then(|index| {
            chrono::NaiveDateTime::parse_from_str(&id[..=index], "%Y%m%dT%H%M%S%.fZ").ok()
        })
        .map(|v| v.and_utc().timestamp_millis() as f64 / 1000.0);
    named
        .or_else(|| value["updated_at"].as_f64())
        .or_else(|| {
            value["updated_at"]
                .as_str()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|v| v.timestamp_millis() as f64 / 1000.0)
        })
        .unwrap_or(0.0)
}
fn plan(d: &Document) -> io::Result<Value> {
    let raw = d
        .value
        .as_ref()
        .ok_or_else(|| error("Missing task state"))?;
    let steps=raw["stages"].as_array().unwrap().iter().map(|s|json!({"id":s["id"],"title":s["description"],"status":match s["status"].as_str(){Some("done")=>"completed",Some("pending")=>"pending",_=>"blocked"},"depends_on":s.get("depends_on").cloned().unwrap_or(json!([])),"outcome_ids":s.get("outcome_ids").cloned().unwrap_or(json!([])),"acceptance_criteria":s.get("expected_evidence").cloned().unwrap_or(json!("")),"evidence":[],"historical_status":s["status"],"historical_evidence":s.get("evidence_refs").cloned().unwrap_or(json!([]))})).collect::<Vec<_>>();
    let outcomes=raw["goals"].as_array().into_iter().flatten().map(|g|json!({"id":g["id"],"text":g["description"],"required":g.get("required").cloned().unwrap_or(json!(true)),"done":false,"historical_passes":g["passes"],"evidence":"","status":"requires_review"})).collect::<Vec<_>>();
    let turn = raw["turn_id"]
        .as_str()
        .filter(|s| crate::server::identifier(s))
        .map(str::to_owned)
        .unwrap_or_else(|| format!("imported-{}", &d.source_digest[..32]));
    Ok(
        json!({"objective":raw["objective"],"steps":steps,"outcomes":outcomes,"constraints":raw.get("constraints").cloned().unwrap_or(json!([])),"historical_budget":raw["budget"],"historical_answers":raw.get("answered_questions").cloned().unwrap_or(json!({})),"revision":raw["revision"].as_i64().filter(|v|*v>=0).unwrap_or(0),"turn_id":turn,"approved":false,"migration":{"interrupted":true,"source_digest":d.source_digest,"source_turn_id":raw["turn_id"],"current_stage_id":raw["current_stage_id"],"historical_evidence_is_fresh":false}}),
    )
}
impl DocumentSink for Projection {
    fn apply(&mut self, d: &Document) -> io::Result<()> {
        let kind = d.domain.name();
        let tx = self.db.transaction().map_err(sql)?;
        let prior: Option<(String, String)> = tx
            .query_row(
                "SELECT source,digest FROM ui_portable_documents_v1 WHERE domain=? AND id=?",
                params![kind, d.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(sql)?;
        if let Some((source, digest)) = prior {
            if source != d.source_id || digest != d.source_digest {
                return Err(error("Document belongs to another source"));
            }
            return Ok(());
        }
        if matches!(d.domain,Domain::Agents) && self.profiles_owned {return Err(error("Agent storage has moved to the owned repository; import a portable profile bundle instead of changing the frozen legacy source"));}
        match d.domain {
            Domain::Agents => {
                if d.id == "main" {
                    let mut builtin = crate::ui_store::get(&tx, "agent", "main")
                        .map_err(sql)?
                        .ok_or_else(|| error("Default agent is missing"))?;
                    if builtin["knowledge"] != json!({"inherit_workspace":true,"extra_kb_ids":[]})
                        && builtin["knowledge"] != d.value.clone().unwrap_or(Value::Null)
                    {
                        return Err(error("Existing default knowledge cannot be overwritten"));
                    }
                    builtin["knowledge"] = d
                        .value
                        .clone()
                        .ok_or_else(|| error("Default knowledge is missing"))?;
                    crate::ui_store::put(&tx, "agent", "main", builtin).map_err(sql)?;
                } else {
                    let used:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM ui_document_history WHERE kind='agent' AND id=?) OR EXISTS(SELECT 1 FROM ui_documents WHERE kind='agent' AND id=?)",params![d.id,d.id],|r|r.get(0)).map_err(sql)?;
                    if used {
                        return Err(error(
                            "Existing agent identity cannot be overwritten or resurrected",
                        ));
                    }
                    for (index, revision) in d.revisions.iter().enumerate() {
                        let mut value = definition(&revision.value);
                        value["revision"] = json!(index + 1);
                        tx.execute(
                            "INSERT INTO ui_document_history VALUES ('agent',?,?,?,?)",
                            params![
                                d.id,
                                index + 1,
                                value.to_string(),
                                revision_time(&revision.id, &revision.value)
                            ],
                        )
                        .map_err(sql)?;
                        tx.execute(
                            "INSERT INTO ui_agent_source_revisions VALUES (?,?,?)",
                            params![d.id, revision.id, index + 1],
                        )
                        .map_err(sql)?;
                    }
                    if let Some(value) = &d.value {
                        crate::ui_store::put(&tx, "agent", &d.id, definition(value))
                            .map_err(sql)?;
                    }
                }
            }
            Domain::Knowledge => crate::knowledge_migration::apply(&tx,d)?,
            Domain::WorkspaceKnowledge => return Err(error("Use workspace-scoped knowledge migration")),
            Domain::Projects => return Err(error("Use owned project migration")),
            Domain::TaskPlans => {
                let chat: bool = tx
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM ui_sessions WHERE id=?)",
                        [&d.id],
                        |r| r.get(0),
                    )
                    .map_err(sql)?;
                if !chat {
                    return Err(error("Import chat history before its task plan"));
                }
                let exists: bool = tx
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM ui_plans WHERE chat_id=?)",
                        [&d.id],
                        |r| r.get(0),
                    )
                    .map_err(sql)?;
                if exists {
                    return Err(error("Existing plan cannot be overwritten"));
                }
                let value = plan(d)?;
                tx.execute(
                    "INSERT INTO ui_plans VALUES (?,?,?,?,0)",
                    params![
                        d.id,
                        value["turn_id"].as_str(),
                        value.to_string(),
                        value["revision"].as_i64()
                    ],
                )
                .map_err(sql)?;
            }
        }
        tx.execute(
            "INSERT INTO ui_portable_documents_v1 VALUES (?,?,?, ?,?)",
            params![
                kind,
                d.id,
                d.source_id,
                d.source_digest,
                serde_json::to_string(d)?
            ],
        )
        .map_err(sql)?;
        tx.commit().map_err(sql)?;
        Ok(())
    }
    fn verify(&mut self, d: &Document) -> io::Result<()> {
        let row:Option<(String,String,String)>=self.db.query_row("SELECT source,digest,snapshot FROM ui_portable_documents_v1 WHERE domain=? AND id=?",params![d.domain.name(),d.id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(sql)?;
        let Some((source, digest, snapshot)) = row else {
            return Err(error("Imported document receipt is missing"));
        };
        if source != d.source_id
            || digest != d.source_digest
            || serde_json::from_str::<Value>(&snapshot)? != serde_json::to_value(d)?
        {
            return Err(error("Imported document receipt was modified"));
        }
        if matches!(d.domain, Domain::Agents) && d.id != "main" {
            for (index, revision) in d.revisions.iter().enumerate() {
                let native:Option<i64>=self.db.query_row("SELECT native_revision FROM ui_agent_source_revisions WHERE id=? AND source_revision=?",params![d.id,revision.id],|r|r.get(0)).optional().map_err(sql)?;
                if native != Some(index as i64 + 1) {
                    return Err(error("Imported revision identity changed"));
                }
                verify_agent_revision(&self.db, &d.id, index as i64 + 1, &revision.value)?;
            }
            if let Some(value) = &d.value {
                verify_agent_revision(&self.db, &d.id, d.revisions.len() as i64 + 1, value)?;
            }
        }
        // Definitions and plans may be edited/deleted after migration; the immutable
        // source snapshot/receipt survives, and repeat import must not undo those edits.
        Ok(())
    }
}
fn verify_agent_revision(
    db: &Connection,
    id: &str,
    revision: i64,
    value: &Value,
) -> io::Result<()> {
    let body: Option<String> = db
        .query_row(
            "SELECT body FROM ui_document_history WHERE kind='agent' AND id=? AND revision=?",
            params![id, revision],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql)?;
    let mut expected = definition(value);
    expected["revision"] = json!(revision);
    if body
        .map(|v| serde_json::from_str::<Value>(&v))
        .transpose()?
        .as_ref()
        != Some(&expected)
    {
        return Err(error("Imported agent revision was changed or removed"));
    }
    Ok(())
}
pub fn run(source: &Path, state: &Path, domain: Domain) -> io::Result<Report> {
    let crate::history_migration::Destination {
        db,
        source,
        state,
        _lock,
    } = crate::history_migration::destination(source, state)?;
    crate::ui_store::initialize(&db).map_err(sql)?;
    crate::ui_agents::initialize(&db).map_err(sql)?;
    crate::ui_planning::initialize(&db).map_err(sql)?;
    db.execute_batch("CREATE TABLE IF NOT EXISTS ui_portable_documents_v1(domain TEXT NOT NULL,id TEXT NOT NULL,source TEXT NOT NULL,digest TEXT NOT NULL,snapshot TEXT NOT NULL,PRIMARY KEY(domain,id));").map_err(sql)?;
    let mut projection = Projection { db, profiles_owned:state.join("components/agents/profiles.sqlite").exists() };
    Importer::open(
        &source,
        &state.join(format!("document-import-{}-v1", domain.name())),
    )?
    .run_documents(domain, &mut projection)
}

/// Explicit offline import into the independently owned workspace knowledge store.
pub fn run_workspace_knowledge(source:&Path,state:&Path,workspace:&Path)->io::Result<Report>{
    crate::knowledge_migration::run_workspace(source,state,workspace)
}
