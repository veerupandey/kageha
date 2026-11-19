//! Read-only compatibility projection. The agents library never reads app tables.
use kageha_agents::repository::{Bundle, Entry, Kind, Revision};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use std::io;
fn err(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}
pub(crate) fn export(db: &Connection) -> io::Result<Bundle> {
    let tx = db.unchecked_transaction().map_err(err)?;
    let keys: Vec<(String, String)> = {
        let mut q=tx.prepare("SELECT kind,id FROM ui_documents WHERE kind IN ('agent','team') UNION SELECT kind,id FROM ui_document_history WHERE kind IN ('agent','team') ORDER BY kind,id").map_err(err)?;
        let v = q
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(err)?
            .collect::<Result<_, _>>()
            .map_err(err)?;
        v
    };
    let mut entries = vec![];
    for (kind, id) in keys {
        let mut revisions: Vec<Revision> = {
            let mut q=tx.prepare("SELECT h.revision,h.body,h.updated,(SELECT source_revision FROM ui_agent_source_revisions s WHERE h.kind='agent' AND s.id=h.id AND s.native_revision=h.revision) FROM ui_document_history h WHERE h.kind=? AND h.id=? ORDER BY h.revision").map_err(err)?;
            let rows = q
                .query_map(params![kind, id], |r| {
                    Ok((
                        r.get::<_, u64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, f64>(2)?,
                        r.get::<_, Option<String>>(3)?,
                    ))
                })
                .map_err(err)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(err)?;
            rows.into_iter()
                .map(|(number, body, timestamp, source_revision)| {
                    Ok(Revision {
                        number,
                        value: Some(serde_json::from_str(&body)?),
                        timestamp: Some(timestamp),
                        source_revision,
                    })
                })
                .collect::<io::Result<_>>()?
        };
        let current: Option<(u64, String, f64)> = tx
            .query_row(
                "SELECT revision,body,updated FROM ui_documents WHERE kind=? AND id=?",
                params![kind, id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(err)?;
        match current {
            Some((number, body, timestamp)) => {
                let value: Value = serde_json::from_str(&body)?;
                if revisions.is_empty() {
                    revisions.push(Revision {
                        number,
                        value: Some(value),
                        timestamp: Some(timestamp),
                        source_revision: None,
                    });
                } else if revisions
                    .last()
                    .is_none_or(|r| r.number != number || r.value.as_ref() != Some(&value))
                {
                    return Err(err(
                        "Current profile and history disagree; reconcile source before export",
                    ));
                }
            }
            None => {
                let next = revisions
                    .last()
                    .ok_or_else(|| err("Deleted profile lacks history"))?
                    .number
                    .checked_add(1)
                    .ok_or_else(|| err("Revision exhausted"))?;
                revisions.push(Revision {
                    number: next,
                    value: None,
                    timestamp: None,
                    source_revision: None,
                });
            }
        }
        entries.push(Entry {
            kind: if kind == "agent" {
                Kind::Agent
            } else {
                Kind::Team
            },
            id,
            revisions,
        });
    }
    let source = format!(
        "webui:{}",
        kageha_agents::hash(&serde_json::json!(db.path().unwrap_or("memory")))
    );
    let b = Bundle {
        schema_version: 1,
        source,
        entries,
    };
    b.validate()?;
    tx.commit().map_err(err)?;
    Ok(b)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn export_preserves_history_aliases_teams_and_deletions_without_changing_source() {
        let db = Connection::open_in_memory().unwrap();
        crate::ui_store::initialize(&db).unwrap();
        crate::ui_agents::initialize(&db).unwrap();
        for name in ["First", "Second"] {
            crate::ui_store::put(
                &db,
                "agent",
                "research",
                serde_json::json!({"id":"research","name":name}),
            )
            .unwrap();
        }
        crate::ui_store::put(
            &db,
            "team",
            "reviewers",
            serde_json::json!({"id":"reviewers","name":"Reviewers","agents":["research"]}),
        )
        .unwrap();
        db.execute(
            "INSERT INTO ui_agent_source_revisions VALUES ('research','source-v1',1)",
            [],
        )
        .unwrap();
        db.execute(
            "DELETE FROM ui_documents WHERE kind='agent' AND id='research'",
            [],
        )
        .unwrap();
        let before = db.total_changes();
        let b = export(&db).unwrap();
        assert_eq!(before, db.total_changes());
        let e = b.entries.iter().find(|e| e.id == "research").unwrap();
        assert_eq!(e.revisions.len(), 3);
        assert_eq!(e.revisions[0].source_revision.as_deref(), Some("source-v1"));
        assert_eq!(e.revisions[1].value.as_ref().unwrap()["name"], "Second");
        assert!(e.head().value.is_none());
        assert!(b.entries.iter().any(|e| e.kind == Kind::Team));
    }
}
