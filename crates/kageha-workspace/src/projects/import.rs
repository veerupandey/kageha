//! Portable legacy catalog import. Original snapshots remain immutable after edits.
use super::*;
use serde_json::Value;
use std::collections::BTreeMap;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Association {
    pub project: Option<String>,
    pub tags: BTreeMap<u64, Tag>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Archive {
    pub projects: Vec<Project>,
    pub sessions: BTreeMap<String, Association>,
    pub original: Value,
}
impl Archive {
    pub fn from_legacy(catalog: Value, sessions: BTreeMap<String, Value>) -> io::Result<Self> {
        let rows = catalog
            .as_object()
            .filter(|v| v.len() <= 4096)
            .ok_or_else(|| err("Invalid or oversized project catalog"))?;
        let mut projects = Vec::new();
        for (id, raw) in rows {
            if !valid(id) || !raw.is_object() || raw.get("id").is_some_and(|v| v != id) {
                return Err(err("Invalid historical project identity"));
            }
            let name = raw["name"].as_str().unwrap_or(id);
            bounded(name, 256)?;
            if name.trim().is_empty() {
                return Err(err("Project name is empty"));
            }
            let description = raw["description"].as_str().unwrap_or("");
            bounded(description, 16384)?;
            let color = raw["color"].as_str().unwrap_or("blue");
            bounded(color, 64)?;
            let mut times = Vec::new();
            for key in ["created_at", "updated_at"] {
                let time = match raw.get(key) {
                    None | Some(Value::Null) => 0.0,
                    Some(v) => v
                        .as_f64()
                        .filter(|n| n.is_finite() && *n >= 0.0)
                        .ok_or_else(|| err("Invalid historical project timestamp"))?,
                };
                times.push(time);
            }
            let tags = match raw.get("tags") {
                None | Some(Value::Null) => vec![],
                Some(v) => v
                    .as_array()
                    .filter(|v| v.len() <= 256)
                    .ok_or_else(|| err("Invalid project tags"))?
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .filter(|v| valid(v))
                            .map(str::to_owned)
                            .ok_or_else(|| err("Invalid project tag"))
                    })
                    .collect::<io::Result<Vec<_>>>()?,
            };
            projects.push(Project {
                id: id.clone(),
                name: name.into(),
                description: description.into(),
                color: color.into(),
                created_at: times[0],
                updated_at: times[1],
                tags,
                revision: 1,
                session_count: 0,
                tagged_message_count: 0,
            });
        }
        if sessions.len() > 10000 {
            return Err(err("Too many historical project chats"));
        }
        let mut associations = BTreeMap::new();
        let mut total_tags = 0usize;
        for (chat, meta) in &sessions {
            if !valid(chat) || !meta.is_object() {
                return Err(err("Invalid historical chat metadata"));
            }
            let reference = meta
                .get("project_id")
                .filter(|v| !v.is_null())
                .or_else(|| meta.get("project").and_then(|v| v.get("id")));
            let project = match reference {
                None | Some(Value::Null) => None,
                Some(v) => Some(
                    v.as_str()
                        .filter(|id| projects.iter().any(|p| p.id == *id))
                        .ok_or_else(|| err("Chat references a missing project"))?
                        .to_owned(),
                ),
            };
            if let (Some(id), Some(embedded)) =
                (&project, meta.get("project").filter(|v| !v.is_null()))
            {
                if embedded["id"] != *id {
                    return Err(err("Chat project identities conflict"));
                }
            }
            let mut tags = BTreeMap::new();
            if let Some(raw) = meta.get("tagged_turns").filter(|v| !v.is_null()) {
                for (index, tag) in raw
                    .as_object()
                    .filter(|v| v.len() <= 10000)
                    .ok_or_else(|| err("Invalid historical tagged turns"))?
                {
                    let index = index
                        .parse::<u64>()
                        .ok()
                        .filter(|n| *n <= i64::MAX as u64)
                        .ok_or_else(|| err("Invalid historical turn index"))?;
                    let id = tag["project_id"]
                        .as_str()
                        .and_then(|id| projects.iter().find(|p| p.id == id))
                        .ok_or_else(|| err("Turn references a missing project"))?;
                    if tag.get("session_id").is_some_and(|v| v != chat)
                        || tag
                            .get("turn_index")
                            .is_some_and(|v| v.as_u64() != Some(index))
                    {
                        return Err(err("Tagged turn identity mismatch"));
                    }
                    let label = tag["tag"].as_str().unwrap_or("");
                    bounded(label, 1024)?;
                    if tags.contains_key(&index) {
                        return Err(err("Duplicate historical turn index"));
                    }
                    let project_name = tag["project_name"].as_str().unwrap_or(&id.name);
                    bounded(project_name, 256)?;
                    tags.insert(
                        index,
                        Tag {
                            session_id: chat.clone(),
                            turn_index: index,
                            project_id: id.id.clone(),
                            project_name: project_name.into(),
                            tag: label.into(),
                        },
                    );
                    total_tags += 1;
                    if total_tags > 100000 {
                        return Err(err("Historical project tags exceed limits"));
                    }
                }
            }
            if project.is_some() || !tags.is_empty() {
                associations.insert(chat.clone(), Association { project, tags });
            }
        }
        let original = serde_json::json!({"catalog":catalog,"sessions":sessions});
        if serde_json::to_vec(&original)?.len() > 64 * 1024 * 1024 {
            return Err(err("Historical project archive exceeds limits"));
        }
        Ok(Self {
            projects,
            sessions: associations,
            original,
        })
    }
    fn validated(&self) -> io::Result<()> {
        let expected = Self::from_legacy(
            self.original["catalog"].clone(),
            serde_json::from_value(self.original["sessions"].clone())?,
        )?;
        if serde_json::to_value(self)? != serde_json::to_value(expected)? {
            return Err(err("Project import projection changed"));
        }
        Ok(())
    }
}
impl Projects {
    pub fn has_import(&self, owner: &str, source: &str) -> io::Result<bool> {
        self.db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM project_imports WHERE owner=? AND source=?)",
                params![owner, source],
                |r| r.get(0),
            )
            .map_err(err)
    }
    pub fn import_archive(
        &mut self,
        owner: &str,
        source: &str,
        digest: &str,
        archive: &Archive,
    ) -> io::Result<bool> {
        if !valid(owner)
            || !valid(source)
            || digest.len() != 64
            || !digest.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(err("Invalid project import identity"));
        }
        archive.validated()?;
        let snapshot = serde_json::to_string(archive)?;
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(err)?;
        let old: Option<(String, String)> = tx
            .query_row(
                "SELECT digest,snapshot FROM project_imports WHERE owner=? AND source=?",
                params![owner, source],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(err)?;
        if let Some((old, body)) = old {
            if old != digest || body != snapshot {
                return Err(err("Historical project import changed"));
            }
            verify(&tx, owner, source, digest, archive)?;
            tx.commit().map_err(err)?;
            return Ok(false);
        }
        for project in &archive.projects {
            let exists: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM projects WHERE owner=? AND id=?)",
                    params![owner, project.id],
                    |r| r.get(0),
                )
                .map_err(err)?;
            if exists {
                return Err(err(
                    "Historical projects cannot overwrite native project identities",
                ));
            }
            let body = serde_json::to_string(project)?;
            tx.execute(
                "INSERT INTO projects VALUES (?,?,?)",
                params![owner, project.id, body],
            )
            .map_err(err)?;
            tx.execute(
                "INSERT INTO project_history VALUES (?,?,1,?)",
                params![owner, project.id, body],
            )
            .map_err(err)?;
        }
        for (chat, association) in &archive.sessions {
            let used:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM session_projects WHERE owner=? AND session=?) OR EXISTS(SELECT 1 FROM project_tags WHERE owner=? AND session=?)",params![owner,chat,owner,chat],|r|r.get(0)).map_err(err)?;
            if used {
                return Err(err(
                    "Historical associations cannot overwrite native chat organization",
                ));
            }
            if let Some(project) = &association.project {
                tx.execute(
                    "INSERT INTO session_projects VALUES (?,?,?)",
                    params![owner, chat, project],
                )
                .map_err(err)?;
            }
            for (index, tag) in &association.tags {
                tx.execute(
                    "INSERT INTO project_tags VALUES (?,?,?,?,?)",
                    params![
                        owner,
                        chat,
                        index,
                        tag.project_id,
                        serde_json::to_string(tag)?
                    ],
                )
                .map_err(err)?;
            }
        }
        tx.execute(
            "INSERT INTO project_imports VALUES (?,?,?,?)",
            params![owner, source, digest, snapshot],
        )
        .map_err(err)?;
        verify(&tx, owner, source, digest, archive)?;
        tx.commit().map_err(err)?;
        Ok(true)
    }
    pub fn verify_archive(
        &self,
        owner: &str,
        source: &str,
        digest: &str,
        archive: &Archive,
    ) -> io::Result<()> {
        archive.validated()?;
        let tx = self.db.unchecked_transaction().map_err(err)?;
        verify(&tx, owner, source, digest, archive)?;
        tx.commit().map_err(err)
    }
}
fn verify(
    db: &Connection,
    owner: &str,
    source: &str,
    digest: &str,
    archive: &Archive,
) -> io::Result<()> {
    let (saved, body): (String, String) = db
        .query_row(
            "SELECT digest,snapshot FROM project_imports WHERE owner=? AND source=?",
            params![owner, source],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(err)?;
    if saved != digest || body != serde_json::to_string(archive)? {
        return Err(err("Project import receipt changed"));
    }
    for project in &archive.projects {
        let body: String = db
            .query_row(
                "SELECT body FROM project_history WHERE owner=? AND id=? AND revision=1",
                params![owner, project.id],
                |r| r.get(0),
            )
            .map_err(err)?;
        if body != serde_json::to_string(project)? {
            return Err(err("Imported project history changed"));
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn archive() -> Archive {
        Archive::from_legacy(json!({"kept-id":{"id":"kept-id","name":"Original Name","created_at":12,"updated_at":34,"tags":["kept"],"extra":"retained"}}),BTreeMap::from([("chat".into(),json!({"project_id":"kept-id","tagged_turns":{"0":{"project_id":"kept-id","tag":"saved"}}}))])).unwrap()
    }
    #[test]
    fn atomic_import_receipts_preserve_history_after_native_edits_and_deletions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("projects/store.sqlite");
        let mut store = Projects::open(&path).unwrap();
        let archive = archive();
        let hash = "a".repeat(64);
        assert!(store
            .import_archive("owner", "source", &hash, &archive)
            .unwrap());
        assert!(!store
            .import_archive("owner", "source", &hash, &archive)
            .unwrap());
        assert_eq!(
            store.assigned("owner", "chat").unwrap().unwrap().created_at,
            12.0
        );
        store.assign("owner", "chat", "Native", 56.0).unwrap();
        store.tag("owner", "chat", 0, "", "", 56.0).unwrap();
        drop(store);
        let mut store = Projects::open(&path).unwrap();
        assert!(!store
            .import_archive("owner", "source", &hash, &archive)
            .unwrap());
        assert_eq!(
            store.assigned("owner", "chat").unwrap().unwrap().name,
            "Native"
        );
        assert!(store.tags("owner", "chat").unwrap().is_empty());
        store.erase_session("owner", "chat").unwrap();
        assert!(!store
            .import_archive("owner", "source", &hash, &archive)
            .unwrap());
        assert!(store.assigned("owner", "chat").unwrap().is_none());
        store
            .db
            .execute(
                "UPDATE project_history SET body='{}' WHERE id='kept-id'",
                [],
            )
            .unwrap();
        assert!(store
            .verify_archive("owner", "source", &hash, &archive)
            .is_err());
    }
    #[test]
    fn inline_project_references_survive_and_duplicate_turn_aliases_fail() {
        let raw = serde_json::json!({"kept-id":{"name":"Original"}});
        let mut meta = serde_json::json!({"project":{"id":"kept-id"}});
        let archive =
            Archive::from_legacy(raw.clone(), BTreeMap::from([("chat".into(), meta.clone())]))
                .unwrap();
        assert_eq!(archive.sessions["chat"].project.as_deref(), Some("kept-id"));
        meta["tagged_turns"] =
            serde_json::json!({"0":{"project_id":"kept-id"},"00":{"project_id":"kept-id"}});
        assert!(Archive::from_legacy(raw, BTreeMap::from([("chat".into(), meta)])).is_err());
    }
    #[test]
    fn conflicting_native_identities_and_tampered_projections_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Projects::open(&dir.path().join("projects/store.sqlite")).unwrap();
        store
            .save("owner", "Kept Id", "Native", "blue", 0.0, None)
            .unwrap();
        let archive = archive();
        assert!(store
            .import_archive("owner", "source", &"a".repeat(64), &archive)
            .is_err());
        assert!(store.assigned("owner", "chat").unwrap().is_none());
        let mut invalid = archive;
        invalid.projects[0].name = "Changed projection".into();
        assert!(store
            .import_archive("other", "source", &"a".repeat(64), &invalid)
            .is_err());
        assert!(store.list("other", "").unwrap().is_empty());
    }
}
