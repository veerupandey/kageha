//! Project catalog and chat/turn bindings. Hosts validate chat ownership before binding.
pub mod import;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::Path,
};
fn err(message: impl std::fmt::Display) -> io::Error {
    io::Error::other(message.to_string())
}
fn valid(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}
fn bounded(value: &str, limit: usize) -> io::Result<()> {
    if value.len() > limit || value.contains('\0') {
        Err(err("Project value exceeds limits"))
    } else {
        Ok(())
    }
}
fn slug(value: &str) -> String {
    let mut out = String::new();
    for c in value.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-');
    if out.is_empty() {
        "project".into()
    } else {
        out.into()
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub description: String,
    pub color: String,
    pub created_at: f64,
    pub updated_at: f64,
    pub tags: Vec<String>,
    pub revision: u64,
    pub session_count: u64,
    pub tagged_message_count: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tag {
    pub session_id: String,
    pub turn_index: u64,
    pub project_id: String,
    pub project_name: String,
    pub tag: String,
}
pub struct Projects {
    db: Connection,
}
impl Projects {
    pub fn open(path: &Path) -> io::Result<Self> {
        let parent = path
            .parent()
            .ok_or_else(|| err("Project store requires a directory"))?;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
        let metadata = fs::symlink_metadata(parent)?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(err("Project store directory must be private"));
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        if !file.metadata()?.is_file() {
            return Err(err("Project store must be a regular file"));
        }
        let db = Connection::open(path).map_err(err)?;
        db.busy_timeout(std::time::Duration::from_secs(2))
            .map_err(err)?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS project_schema(version INTEGER NOT NULL);")
            .map_err(err)?;
        let version: Option<u64> = db
            .query_row("SELECT version FROM project_schema LIMIT 1", [], |r| {
                r.get(0)
            })
            .optional()
            .map_err(err)?;
        if version.is_some_and(|v| v != 1) {
            return Err(err("Unsupported project store version"));
        }
        db.execute_batch("PRAGMA journal_mode=WAL;PRAGMA synchronous=FULL;BEGIN IMMEDIATE;
            INSERT INTO project_schema SELECT 1 WHERE NOT EXISTS(SELECT 1 FROM project_schema);
            CREATE TABLE IF NOT EXISTS project_imports(owner TEXT NOT NULL,source TEXT NOT NULL,digest TEXT NOT NULL,snapshot TEXT NOT NULL,PRIMARY KEY(owner,source));
            CREATE TABLE IF NOT EXISTS projects(owner TEXT NOT NULL,id TEXT NOT NULL,body TEXT NOT NULL,PRIMARY KEY(owner,id));
            CREATE TABLE IF NOT EXISTS project_history(owner TEXT NOT NULL,id TEXT NOT NULL,revision INTEGER NOT NULL,body TEXT NOT NULL,PRIMARY KEY(owner,id,revision));
            CREATE TABLE IF NOT EXISTS session_projects(owner TEXT NOT NULL,session TEXT NOT NULL,project TEXT NOT NULL,PRIMARY KEY(owner,session));
            CREATE TABLE IF NOT EXISTS project_tags(owner TEXT NOT NULL,session TEXT NOT NULL,turn_index INTEGER NOT NULL,project TEXT NOT NULL,body TEXT NOT NULL,PRIMARY KEY(owner,session,turn_index));COMMIT;").map_err(err)?;
        Ok(Self { db })
    }
    pub fn list(&self, owner: &str, query: &str) -> io::Result<Vec<Project>> {
        if !valid(owner) {
            return Err(err("Project owner is required"));
        }
        bounded(query, 1024)?;
        let tx = self.db.unchecked_transaction().map_err(err)?;
        let mut q = self
            .db
            .prepare("SELECT body FROM projects WHERE owner=? ORDER BY id LIMIT 4097")
            .map_err(err)?;
        let rows = q
            .query_map([owner], |r| r.get::<_, String>(0))
            .map_err(err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(err)?;
        if rows.len() > 4096 {
            return Err(err("Project catalog exceeds limits"));
        }
        let query = query.to_lowercase();
        let mut result = Vec::new();
        for row in rows {
            let mut project: Project = serde_json::from_str(&row)?;
            if !query.is_empty()
                && ![&project.id, &project.name, &project.description]
                    .iter()
                    .any(|s| s.to_lowercase().contains(&query))
            {
                continue;
            }
            project.session_count = self
                .db
                .query_row(
                    "SELECT count(*) FROM session_projects WHERE owner=? AND project=?",
                    params![owner, project.id],
                    |r| r.get(0),
                )
                .map_err(err)?;
            project.tagged_message_count = self
                .db
                .query_row(
                    "SELECT count(*) FROM project_tags WHERE owner=? AND project=?",
                    params![owner, project.id],
                    |r| r.get(0),
                )
                .map_err(err)?;
            result.push(project);
        }
        result.sort_by(|a, b| {
            b.updated_at
                .total_cmp(&a.updated_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        drop(q);
        tx.commit().map_err(err)?;
        Ok(result)
    }
    pub fn get(&self, owner: &str, id: &str) -> io::Result<Option<Project>> {
        if !valid(owner) || !valid(id) {
            return Err(err("Invalid project identity"));
        }
        self.db
            .query_row(
                "SELECT body FROM projects WHERE owner=? AND id=?",
                params![owner, id],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(err)?
            .map(|s| serde_json::from_str(&s).map_err(err))
            .transpose()
    }
    pub fn save(
        &mut self,
        owner: &str,
        name: &str,
        description: &str,
        color: &str,
        now: f64,
        expected: Option<u64>,
    ) -> io::Result<Project> {
        if !valid(owner) || name.trim().is_empty() || !now.is_finite() || now < 0.0 {
            return Err(err("Invalid project identity or timestamp"));
        }
        bounded(name, 256)?;
        bounded(description, 16384)?;
        bounded(color, 64)?;
        let id = slug(name);
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(err)?;
        let old: Option<String> = tx
            .query_row(
                "SELECT body FROM projects WHERE owner=? AND id=?",
                params![owner, id],
                |r| r.get(0),
            )
            .optional()
            .map_err(err)?;
        let old = old
            .map(|s| serde_json::from_str::<Project>(&s))
            .transpose()?;
        if expected.is_some_and(|n| n != old.as_ref().map(|p| p.revision).unwrap_or(0)) {
            return Err(err("Project revision changed"));
        }
        let project = Project {
            id: id.clone(),
            name: name.trim().into(),
            description: if description.is_empty() {
                old.as_ref()
                    .map(|p| p.description.clone())
                    .unwrap_or_default()
            } else {
                description.into()
            },
            color: if color.is_empty() {
                old.as_ref()
                    .map(|p| p.color.clone())
                    .unwrap_or("blue".into())
            } else {
                color.into()
            },
            created_at: old.as_ref().map(|p| p.created_at).unwrap_or(now),
            updated_at: now,
            tags: old.as_ref().map(|p| p.tags.clone()).unwrap_or(vec![id]),
            revision: old.as_ref().map(|p| p.revision + 1).unwrap_or(1),
            session_count: 0,
            tagged_message_count: 0,
        };
        let body = serde_json::to_string(&project)?;
        tx.execute("INSERT INTO projects VALUES (?,?,?) ON CONFLICT(owner,id) DO UPDATE SET body=excluded.body",params![owner,project.id,body]).map_err(err)?;
        tx.execute(
            "INSERT INTO project_history VALUES (?,?,?,?)",
            params![owner, project.id, project.revision, body],
        )
        .map_err(err)?;
        tx.commit().map_err(err)?;
        Ok(project)
    }
    pub fn resolve(&mut self, owner: &str, name: &str, now: f64) -> io::Result<Option<Project>> {
        bounded(name, 256)?;
        if name.trim().is_empty() {
            return Ok(None);
        }
        if let Some(p) = self.get(owner, name)? {
            return Ok(Some(p));
        }
        let normalized = slug(name).replace('-', "");
        let rows = self.list(owner, "")?;
        if let Some(p) = rows.iter().find(|p| {
            p.name.eq_ignore_ascii_case(name)
                || p.id.replace('-', "") == normalized
                || slug(&p.name).replace('-', "") == normalized
        }) {
            return Ok(Some(p.clone()));
        }
        // Retain prefix/substring lookup, but reject ambiguous bindings rather than guessing.
        let candidates = rows
            .iter()
            .filter(|p| {
                p.id.replace('-', "").contains(&normalized)
                    || slug(&p.name).replace('-', "").contains(&normalized)
            })
            .collect::<Vec<_>>();
        match candidates.as_slice() {
            [p] => Ok(Some((*p).clone())),
            [] => self.save(owner, name, "", "blue", now, None).map(Some),
            _ => Err(err("Project name is ambiguous; use its ID")),
        }
    }
    pub fn assign(
        &mut self,
        owner: &str,
        session: &str,
        name: &str,
        now: f64,
    ) -> io::Result<Option<Project>> {
        if !valid(session) {
            return Err(err("Invalid chat identity"));
        }
        let project = self.resolve(owner, name, now)?;
        match &project {
            Some(p) => {
                self.db.execute("INSERT INTO session_projects VALUES (?,?,?) ON CONFLICT(owner,session) DO UPDATE SET project=excluded.project",params![owner,session,p.id]).map_err(err)?;
            }
            None => {
                self.db
                    .execute(
                        "DELETE FROM session_projects WHERE owner=? AND session=?",
                        params![owner, session],
                    )
                    .map_err(err)?;
            }
        }
        Ok(project)
    }
    pub fn assigned(&self, owner: &str, session: &str) -> io::Result<Option<Project>> {
        let id: Option<String> = self
            .db
            .query_row(
                "SELECT project FROM session_projects WHERE owner=? AND session=?",
                params![owner, session],
                |r| r.get(0),
            )
            .optional()
            .map_err(err)?;
        id.map(|id| {
            self.get(owner, &id)?
                .ok_or_else(|| err("Project binding is missing"))
        })
        .transpose()
    }
    pub fn tag(
        &mut self,
        owner: &str,
        session: &str,
        index: u64,
        name: &str,
        label: &str,
        now: f64,
    ) -> io::Result<Option<Tag>> {
        if !valid(session) || index > i64::MAX as u64 {
            return Err(err("Invalid tagged turn identity"));
        }
        bounded(label, 1024)?;
        let Some(project) = self.resolve(owner, name, now)? else {
            self.db
                .execute(
                    "DELETE FROM project_tags WHERE owner=? AND session=? AND turn_index=?",
                    params![owner, session, index],
                )
                .map_err(err)?;
            return Ok(None);
        };
        let tag = Tag {
            session_id: session.into(),
            turn_index: index,
            project_id: project.id,
            project_name: project.name,
            tag: label.trim().into(),
        };
        self.db.execute("INSERT INTO project_tags VALUES (?,?,?,?,?) ON CONFLICT(owner,session,turn_index) DO UPDATE SET project=excluded.project,body=excluded.body",params![owner,session,index,tag.project_id,serde_json::to_string(&tag)?]).map_err(err)?;
        Ok(Some(tag))
    }
    /// The host calls this while deleting a chat; retrying is idempotent.
    pub fn erase_session(&mut self, owner: &str, session: &str) -> io::Result<()> {
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(err)?;
        tx.execute(
            "DELETE FROM session_projects WHERE owner=? AND session=?",
            params![owner, session],
        )
        .map_err(err)?;
        tx.execute(
            "DELETE FROM project_tags WHERE owner=? AND session=?",
            params![owner, session],
        )
        .map_err(err)?;
        tx.commit().map_err(err)
    }
    pub fn tags(
        &self,
        owner: &str,
        session: &str,
    ) -> io::Result<std::collections::BTreeMap<u64, Tag>> {
        let mut q=self.db.prepare("SELECT turn_index,body FROM project_tags WHERE owner=? AND session=? ORDER BY turn_index LIMIT 10001").map_err(err)?;
        let rows = q
            .query_map(params![owner, session], |r| {
                Ok((r.get::<_, u64>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(err)?;
        if rows.len() > 10000 {
            return Err(err("Tagged turn catalog exceeds limits"));
        }
        rows.into_iter()
            .map(|(id, body)| Ok((id, serde_json::from_str(&body)?)))
            .collect()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_bindings_and_tags_survive_restart_without_cross_owner_access() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("projects/store.sqlite");
        let mut s = Projects::open(&path).unwrap();
        let p = s
            .save("one", "Bare & Fair", "Catalog", "emerald", 1.0, Some(0))
            .unwrap();
        assert_eq!(p.id, "bare-fair");
        assert!(s
            .save("one", "Bare & Fair", "bad", "", 2.0, Some(0))
            .is_err());
        s.assign("one", "chat", "bare", 2.0).unwrap();
        s.tag("one", "chat", 0, "bare-fair", "evidence", 2.0)
            .unwrap();
        assert!(s.assigned("two", "chat").unwrap().is_none());
        assert!(s.tags("two", "chat").unwrap().is_empty());
        drop(s);
        let mut s = Projects::open(&path).unwrap();
        assert_eq!(s.assigned("one", "chat").unwrap().unwrap().id, "bare-fair");
        let rows = s.list("one", "catalog").unwrap();
        assert_eq!(rows[0].session_count, 1);
        assert_eq!(rows[0].tagged_message_count, 1);
        s.assign("one", "chat", "", 3.0).unwrap();
        s.tag("one", "chat", 0, "", "", 3.0).unwrap();
        assert!(s.assigned("one", "chat").unwrap().is_none());
        assert!(s.tags("one", "chat").unwrap().is_empty());
    }
}
