//! Portable revision history and cold import. Stores own their data; imports
//! never overwrite existing identities or resurrect deletions on repeat import.
use crate::{hash, invalid, validate, Definition, Port};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    sync::Mutex,
};
#[cfg(feature = "sqlite")]
pub mod sqlite;
const MAX_BYTES: usize = 64 * 1024 * 1024;
fn label(s: &str) -> io::Result<()> {
    if s.is_empty() || s.len() > 512 || s.chars().any(char::is_control) {
        return Err(invalid("Invalid repository/source scope"));
    }
    Ok(())
}
fn conflict(s: &str) -> io::Error {
    io::Error::new(io::ErrorKind::AlreadyExists, s)
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Agent,
    Team,
}
impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Team => "team",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Revision {
    pub number: u64,
    /// None is a durable deletion, not an absent import record.
    pub value: Option<Value>,
    pub timestamp: Option<f64>,
    pub source_revision: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub kind: Kind,
    pub id: String,
    pub revisions: Vec<Revision>,
}
impl Entry {
    pub fn head(&self) -> &Revision {
        self.revisions.last().expect("validated nonempty history")
    }
    pub fn validate(&self) -> io::Result<()> {
        let id_valid = match self.kind {
            Kind::Agent => kageha_contracts::agents::valid_id(&self.id),
            Kind::Team => {
                !self.id.is_empty()
                    && self.id.len() <= 64
                    && self
                        .id
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
            }
        };
        if !id_valid || self.revisions.is_empty() || self.revisions.len() > 10000 {
            return Err(invalid("Invalid profile history identity/size"));
        }
        let mut n = 0;
        let mut aliases = BTreeSet::new();
        for r in &self.revisions {
            if r.number <= n
                || r.number > i64::MAX as u64
                || r.timestamp.is_some_and(|t| !t.is_finite() || t < 0.0)
            {
                return Err(invalid("Invalid profile revision order/timestamp"));
            }
            n = r.number;
            if let Some(alias) = &r.source_revision {
                label(alias)?;
                if !aliases.insert(alias) {
                    return Err(invalid("Duplicate source revision"));
                }
            }
            if let Some(v) = &r.value {
                validate(v, self.kind == Kind::Team)?;
                if v["id"] != self.id
                    || v.get("revision")
                        .is_some_and(|x| x.as_u64() != Some(r.number))
                {
                    return Err(invalid("Profile revision identity differs from document"));
                }
            }
        }
        if serde_json::to_vec(self)?.len() > MAX_BYTES {
            return Err(invalid("Profile history exceeds size limit"));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bundle {
    pub schema_version: u32,
    pub source: String,
    pub entries: Vec<Entry>,
}
impl Bundle {
    pub fn validate(&self) -> io::Result<()> {
        label(&self.source)?;
        if self.schema_version != 1
            || self.entries.len() > 4096
            || serde_json::to_vec(self)?.len() > MAX_BYTES
        {
            return Err(invalid("Unsupported or oversized profile bundle"));
        }
        let mut ids = BTreeSet::new();
        for e in &self.entries {
            e.validate()?;
            if !ids.insert((e.kind, &e.id)) {
                return Err(invalid("Duplicate profile identity"));
            }
        }
        Ok(())
    }
    pub fn digest(&self) -> io::Result<String> {
        self.validate()?;
        Ok(hash(&serde_json::to_value(self)?))
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImportStatus {
    pub source: String,
    pub digest: String,
    pub next: usize,
    pub total: usize,
}
impl ImportStatus {
    pub fn complete(&self) -> bool {
        self.next == self.total
    }
}
/// An expected head is mandatory, including 0 for creation. Deletion retains
/// history. Export/import includes both agents and teams, without execution grants.
pub trait Repository: Port {
    fn scope(&self) -> &str;
    fn entries(&self) -> io::Result<Vec<Entry>>;
    fn entry(&self, kind: Kind, id: &str) -> io::Result<Option<Entry>>;
    fn write(
        &self,
        kind: Kind,
        id: &str,
        expected: u64,
        value: Option<Value>,
        timestamp: f64,
    ) -> io::Result<Entry>;
    /// Commit at most limit records, each with its import checkpoint atomically.
    fn import(&self, bundle: &Bundle, limit: usize) -> io::Result<ImportStatus>;
    fn import_status(&self, _source: &str) -> io::Result<Option<ImportStatus>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Import checkpoint inspection is unavailable",
        ))
    }
    fn export(&self) -> io::Result<Bundle> {
        let b = Bundle {
            schema_version: 1,
            source: format!("repository:{}", hash(&json!(self.scope()))),
            entries: self.entries()?,
        };
        b.validate()?;
        Ok(b)
    }
    fn restore(
        &self,
        kind: Kind,
        id: &str,
        expected: u64,
        revision: u64,
        timestamp: f64,
    ) -> io::Result<Entry> {
        let e = self
            .entry(kind, id)?
            .ok_or_else(|| invalid("Profile not found"))?;
        let v = e
            .revisions
            .iter()
            .find(|r| r.number == revision)
            .and_then(|r| r.value.clone())
            .ok_or_else(|| invalid("Restorable revision not found"))?;
        self.write(kind, id, expected, Some(v), timestamp)
    }
}
fn changed(
    prior: Option<Entry>,
    kind: Kind,
    id: &str,
    expected: u64,
    mut value: Option<Value>,
    timestamp: f64,
) -> io::Result<Entry> {
    let mut e = prior.unwrap_or(Entry {
        kind,
        id: id.into(),
        revisions: vec![],
    });
    if e.kind != kind || e.id != id || e.revisions.last().map_or(0, |r| r.number) != expected {
        return Err(conflict("Profile revision conflict"));
    }
    let next = expected
        .checked_add(1)
        .filter(|x| *x <= i64::MAX as u64)
        .ok_or_else(|| invalid("Profile revision exhausted"))?;
    if let Some(v) = &mut value {
        validate(v, kind == Kind::Team)?;
        v["revision"] = json!(next);
    }
    e.revisions.push(Revision {
        number: next,
        value,
        timestamp: Some(timestamp),
        source_revision: None,
    });
    e.validate()?;
    Ok(e)
}
fn status(bundle: &Bundle, prior: Option<ImportStatus>) -> io::Result<ImportStatus> {
    let digest = bundle.digest()?;
    let s = prior.unwrap_or(ImportStatus {
        source: bundle.source.clone(),
        digest: digest.clone(),
        next: 0,
        total: bundle.entries.len(),
    });
    if s.source != bundle.source
        || s.digest != digest
        || s.total != bundle.entries.len()
        || s.next > s.total
    {
        return Err(conflict(
            "Import source changed; use a new destination or reconcile explicitly",
        ));
    }
    Ok(s)
}
fn definitions(entries: Vec<Entry>) -> io::Result<Vec<Definition>> {
    entries
        .into_iter()
        .filter(|e| e.kind == Kind::Agent)
        .filter_map(|e| e.head().value.clone())
        .map(Definition::parse)
        .collect()
}
#[derive(Default)]
struct State {
    entries: BTreeMap<(Kind, String), Entry>,
    imports: BTreeMap<String, ImportStatus>,
}
pub struct MemoryRepository {
    scope: String,
    state: Mutex<State>,
}
impl MemoryRepository {
    pub fn new(scope: &str) -> io::Result<Self> {
        label(scope)?;
        Ok(Self {
            scope: scope.into(),
            state: Mutex::default(),
        })
    }
}
impl Repository for MemoryRepository {
    fn import_status(&self, source: &str) -> io::Result<Option<ImportStatus>> {
        label(source)?;
        Ok(self
            .state
            .lock()
            .map_err(|_| io::Error::other("Profile repository lock poisoned"))?
            .imports
            .get(source)
            .cloned())
    }
    fn scope(&self) -> &str {
        &self.scope
    }
    fn entries(&self) -> io::Result<Vec<Entry>> {
        Ok(self
            .state
            .lock()
            .map_err(|_| io::Error::other("Profile repository lock poisoned"))?
            .entries
            .values()
            .cloned()
            .collect())
    }
    fn entry(&self, k: Kind, id: &str) -> io::Result<Option<Entry>> {
        Ok(self
            .state
            .lock()
            .map_err(|_| io::Error::other("Profile repository lock poisoned"))?
            .entries
            .get(&(k, id.into()))
            .cloned())
    }
    fn write(
        &self,
        k: Kind,
        id: &str,
        expected: u64,
        value: Option<Value>,
        timestamp: f64,
    ) -> io::Result<Entry> {
        let mut s = self
            .state
            .lock()
            .map_err(|_| io::Error::other("Profile repository lock poisoned"))?;
        let key = (k, id.into());
        let e = changed(
            s.entries.get(&key).cloned(),
            k,
            id,
            expected,
            value,
            timestamp,
        )?;
        s.entries.insert(key, e.clone());
        Ok(e)
    }
    fn import(&self, b: &Bundle, limit: usize) -> io::Result<ImportStatus> {
        if limit == 0 || limit > 4096 {
            return Err(invalid("Invalid import batch size"));
        }
        let mut s = self
            .state
            .lock()
            .map_err(|_| io::Error::other("Profile repository lock poisoned"))?;
        let mut p = status(b, s.imports.get(&b.source).cloned())?;
        s.imports.insert(b.source.clone(), p.clone());
        let stop = (p.next + limit).min(p.total);
        while p.next < stop {
            let e = &b.entries[p.next];
            let key = (e.kind, e.id.clone());
            if s.entries.contains_key(&key) {
                return Err(conflict(
                    "Import cannot overwrite or resurrect an existing profile",
                ));
            }
            s.entries.insert(key, e.clone());
            p.next += 1;
            s.imports.insert(b.source.clone(), p.clone());
        }
        s.imports.insert(b.source.clone(), p.clone());
        Ok(p)
    }
}
impl Port for MemoryRepository {
    fn get(&self, id: &str) -> io::Result<Option<Definition>> {
        self.entry(Kind::Agent, id)?
            .and_then(|e| e.head().value.clone())
            .map(Definition::parse)
            .transpose()
    }
    fn list(&self) -> io::Result<Vec<Definition>> {
        definitions(self.entries()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn value(id: &str, name: &str) -> Value {
        json!({"id":id,"name":name,"system_prompt":"Inspect carefully","custom":{"retained":true}})
    }
    fn conformance(r: &dyn Repository) {
        let first = r
            .write(
                Kind::Agent,
                "reader",
                0,
                Some(value("reader", "Original")),
                1.0,
            )
            .unwrap();
        assert_eq!(first.head().number, 1);
        assert!(r
            .write(
                Kind::Agent,
                "reader",
                0,
                Some(value("reader", "Overwrite")),
                2.0
            )
            .is_err());
        assert!(r
            .write(Kind::Agent, "reader", 1, Some(json!("invalid")), 2.0)
            .is_err());
        r.write(
            Kind::Agent,
            "reader",
            1,
            Some(value("reader", "Revised")),
            2.0,
        )
        .unwrap();
        r.write(
            Kind::Team,
            "team",
            0,
            Some(json!({"id":"team","name":"Reviewers","agents":["reader"]})),
            2.0,
        )
        .unwrap();
        r.write(Kind::Agent, "reader", 2, None, 3.0).unwrap();
        assert!(r.get("reader").unwrap().is_none());
        assert!(r.list().unwrap().is_empty());
        let e = r.restore(Kind::Agent, "reader", 3, 1, 4.0).unwrap();
        assert_eq!(e.head().number, 4);
        assert_eq!(e.head().value.as_ref().unwrap()["name"], "Original");
        assert_eq!(
            r.get("reader").unwrap().unwrap().body()["custom"]["retained"],
            true
        );
        assert!(r.restore(Kind::Agent, "reader", 3, 1, 4.0).is_err());
        let b = r.export().unwrap();
        b.validate().unwrap();
        assert_eq!(b.entries.len(), 2);
        let other = MemoryRepository::new("other").unwrap();
        let p = other.import(&b, 1).unwrap();
        assert_eq!(p.next, 1);
        assert!(!p.complete());
        let p = other.import(&b, 1).unwrap();
        assert!(p.complete());
        assert_eq!(other.entries().unwrap(), r.entries().unwrap());
        other.write(Kind::Agent, "reader", 4, None, 5.0).unwrap();
        assert!(other.import(&b, 1).unwrap().complete());
        assert!(other.get("reader").unwrap().is_none());
        let mut changed = b.clone();
        changed.entries[0].revisions[0].timestamp = Some(9.0);
        assert!(other.import(&changed, 1).is_err());
        let mut forged = b.clone();
        forged.entries[0].id = "wrong".into();
        assert!(other.import(&forged, 1).is_err());
        let mut duplicate = b.clone();
        duplicate.entries.push(b.entries[0].clone());
        assert!(other.import(&duplicate, 1).is_err());
    }
    #[test]
    fn memory_repository_revision_and_import_conformance() {
        conformance(&MemoryRepository::new("local").unwrap());
    }
    #[cfg(feature = "sqlite")]
    #[test]
    fn sqlite_repository_revision_and_import_conformance() {
        let dir = tempfile::tempdir().unwrap();
        conformance(
            &sqlite::SqliteRepository::open(&dir.path().join("profiles.sqlite"), "local").unwrap(),
        );
    }
    #[cfg(feature = "sqlite")]
    #[test]
    fn checkpoint_restart_and_repeated_import_preserve_tombstones_and_revisions() {
        let src = MemoryRepository::new("source").unwrap();
        src.write(Kind::Agent, "a", 0, Some(value("a", "A")), 1.0)
            .unwrap();
        src.write(Kind::Agent, "b", 0, Some(value("b", "B")), 1.0)
            .unwrap();
        src.write(Kind::Agent, "b", 1, None, 2.0).unwrap();
        let mut b = src.export().unwrap();
        b.entries[0].revisions[0].source_revision = Some("original-rev".into());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.sqlite");
        {
            let r = sqlite::SqliteRepository::open(&path, "target").unwrap();
            assert_eq!(r.import(&b, 1).unwrap().next, 1);
            assert!(sqlite::SqliteRepository::open(&path, "target").is_err());
        }
        {
            let r = sqlite::SqliteRepository::open(&path, "target").unwrap();
            assert!(r.import(&b, 1).unwrap().complete());
            assert!(r.get("b").unwrap().is_none());
            assert_eq!(
                r.entry(Kind::Agent, "a").unwrap().unwrap().revisions[0]
                    .source_revision
                    .as_deref(),
                Some("original-rev")
            );
            r.write(Kind::Agent, "a", 1, None, 3.0).unwrap();
        }
        {
            let r = sqlite::SqliteRepository::open(&path, "target").unwrap();
            assert!(r.import(&b, 1).unwrap().complete());
            assert!(r.get("a").unwrap().is_none());
            let mut changed = b.clone();
            changed.entries[0].revisions[0].timestamp = Some(4.0);
            assert!(r.import(&changed, 1).is_err());
        }
        assert!(sqlite::SqliteRepository::open(&path, "another").is_err());
    }
    #[cfg(feature = "sqlite")]
    #[test]
    fn concurrent_cas_has_one_winner_and_import_cannot_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let r = std::sync::Arc::new(
            sqlite::SqliteRepository::open(&dir.path().join("profiles.sqlite"), "local").unwrap(),
        );
        let b = std::sync::Arc::new(std::sync::Barrier::new(3));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let r = r.clone();
                let b = b.clone();
                std::thread::spawn(move || {
                    b.wait();
                    r.write(Kind::Agent, "a", 0, Some(value("a", "A")), 1.0)
                        .is_ok()
                })
            })
            .collect();
        b.wait();
        let winners = handles
            .into_iter()
            .map(|h| usize::from(h.join().unwrap()))
            .sum::<usize>();
        assert_eq!(winners, 1);
        let bundle = r.export().unwrap();
        assert!(r.import(&bundle, 1).is_err());
        assert_eq!(
            r.entry(Kind::Agent, "a").unwrap().unwrap().revisions.len(),
            1
        );
    }
    #[cfg(feature = "sqlite")]
    #[test]
    fn dedicated_private_scope_and_schema_are_enforced() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.sqlite");
        let r = sqlite::SqliteRepository::open(&path, "local").unwrap();
        drop(r);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(sqlite::SqliteRepository::open(&path, "local").is_err());
        let link = dir.path().join("link");
        symlink(&path, &link).unwrap();
        assert!(sqlite::SqliteRepository::open(&link, "local").is_err());
        let db = dir.path().join("app.sqlite");
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute("CREATE TABLE ui_sessions(id TEXT)", []).unwrap();
        drop(c);
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(sqlite::SqliteRepository::open(&db, "local").is_err());
    }
}
