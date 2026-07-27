//! Owned skill versions, resource snapshots and lifecycle. Never shares app tables.
use super::*;
use rusqlite::{params, Connection, OptionalExtension};
use std::{
    fs::{self, OpenOptions},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
};
fn sql(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    pub skill: Skill,
    pub revision: u64,
    pub pinned: bool,
    pub archived: bool,
    pub deleted: bool,
    pub source: String,
    pub package_sha256: String,
    pub required_runtimes: BTreeSet<String>,
    /// The skill's recent uses mostly failed ([`Registry::degrade`]). A degraded skill
    /// is not suggested to the model but stays listed for the user. Installing a new
    /// package clears it, unless that package degraded before.
    #[serde(default)]
    pub degraded: bool,
    /// Packages (`package_sha256`) whose uses degraded the skill. They are never
    /// restored as the previous passing revision.
    #[serde(default)]
    pub degraded_packages: BTreeSet<String>,
    /// The revision whose package a degradation restored, if any.
    #[serde(default)]
    pub restored_from: Option<u64>,
    /// The first revision that holds the current package: the install, or the
    /// degradation that restored it. Lifecycle changes (pin, archive, ...) keep it.
    /// Uses recorded at this revision or later are the uses of the current package.
    /// 0 for records written before this field existed.
    #[serde(default)]
    pub content_revision: u64,
}

/// The revision a degradation of `current` restores: the most recent earlier revision
/// in `versions` (any order) that holds a different package, is not deleted, and whose
/// package is not in `bad`. Every stored package passed its test when it was installed
/// (learned skills are installed only after `test.sh` passes), so this is the previous
/// passing revision. `None` when there is none.
pub fn previous_passing<'a>(
    versions: &'a [Record],
    current: &Record,
    bad: &BTreeSet<String>,
) -> Option<&'a Record> {
    let before = if current.content_revision == 0 {
        current.revision
    } else {
        current.content_revision
    };
    versions
        .iter()
        .filter(|version| {
            version.revision < before
                && !version.deleted
                && version.package_sha256 != current.package_sha256
                && !bad.contains(&version.package_sha256)
        })
        .max_by_key(|version| version.revision)
}
pub struct Registry {
    db: Connection,
}
impl Registry {
    pub fn open(path: &Path) -> io::Result<Self> {
        let parent = path
            .parent()
            .ok_or_else(|| error("Skill registry requires a directory"))?;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
        let meta = fs::symlink_metadata(parent)?;
        if !meta.is_dir() || meta.file_type().is_symlink() || meta.permissions().mode() & 0o077 != 0
        {
            return Err(error("Skill registry directory must be private"));
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        if !file.metadata()?.is_file() {
            return Err(error("Skill registry must be a regular file"));
        }
        let db = Connection::open(path).map_err(sql)?;
        db.busy_timeout(std::time::Duration::from_secs(2))
            .map_err(sql)?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS skill_schema(version INTEGER NOT NULL);")
            .map_err(sql)?;
        let version: Option<u64> = db
            .query_row("SELECT version FROM skill_schema LIMIT 1", [], |r| r.get(0))
            .optional()
            .map_err(sql)?;
        if version.is_some_and(|v| v != 1) {
            return Err(error("Unsupported skill registry version"));
        }
        db.execute_batch("PRAGMA journal_mode=WAL;PRAGMA synchronous=FULL;BEGIN IMMEDIATE;
        INSERT INTO skill_schema SELECT 1 WHERE NOT EXISTS(SELECT 1 FROM skill_schema);
        CREATE TABLE IF NOT EXISTS skills(name TEXT PRIMARY KEY,revision INTEGER NOT NULL,body TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS skill_versions(name TEXT NOT NULL,revision INTEGER NOT NULL,body TEXT NOT NULL,PRIMARY KEY(name,revision));
        CREATE TABLE IF NOT EXISTS skill_resources(name TEXT NOT NULL,revision INTEGER NOT NULL,path TEXT NOT NULL,hash TEXT NOT NULL,body BLOB NOT NULL,PRIMARY KEY(name,revision,path));
        CREATE TABLE IF NOT EXISTS skill_imports(source TEXT NOT NULL,name TEXT NOT NULL,digest TEXT NOT NULL,PRIMARY KEY(source,name));
        CREATE TABLE IF NOT EXISTS skill_import_snapshots(source TEXT NOT NULL,name TEXT NOT NULL,body TEXT NOT NULL,PRIMARY KEY(source,name));COMMIT;").map_err(sql)?;
        Ok(Self { db })
    }
    pub fn get(&self, name: &str) -> io::Result<Option<Record>> {
        self.db
            .query_row("SELECT body FROM skills WHERE name=?", [name], |r| {
                r.get::<_, String>(0)
            })
            .optional()
            .map_err(sql)?
            .map(|v| serde_json::from_str(&v).map_err(sql))
            .transpose()
    }
    pub fn list(&self) -> io::Result<Vec<Record>> {
        let mut q = self
            .db
            .prepare("SELECT body FROM skills ORDER BY name LIMIT 1025")
            .map_err(sql)?;
        let records = q
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(sql)?
            .map(|v| serde_json::from_str(&v.map_err(sql)?).map_err(sql))
            .collect::<io::Result<Vec<Record>>>()?;
        if records.len() > 1024 {
            return Err(error("Skill registry exceeds catalog limits"));
        }
        Ok(records.into_iter().filter(|r| !r.deleted).collect())
    }
    pub fn install(
        &mut self,
        package: &Package,
        source: &str,
        expected: Option<u64>,
    ) -> io::Result<Record> {
        self.write(package, source, expected, None)
    }
    fn write(
        &mut self,
        package: &Package,
        source: &str,
        expected: Option<u64>,
        legacy: Option<(&str, &str, bool, bool)>,
    ) -> io::Result<Record> {
        package.validate()?;
        if source.len() > 4096 || source.chars().any(char::is_control) {
            return Err(error("Invalid skill source reference"));
        }
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(sql)?;
        if let Some((origin, digest, _, _)) = legacy {
            let prior: Option<String> = tx
                .query_row(
                    "SELECT digest FROM skill_imports WHERE source=? AND name=?",
                    params![origin, package.skill.name],
                    |r| r.get(0),
                )
                .optional()
                .map_err(sql)?;
            if let Some(prior) = prior {
                if prior != digest {
                    return Err(error("Legacy skill changed after adoption"));
                }
                let row: String = tx
                    .query_row(
                        "SELECT body FROM skills WHERE name=?",
                        [&package.skill.name],
                        |r| r.get(0),
                    )
                    .map_err(sql)?;
                let record = serde_json::from_str(&row)?;
                tx.commit().map_err(sql)?;
                return Ok(record);
            }
        }
        let old: Option<String> = tx
            .query_row(
                "SELECT body FROM skills WHERE name=?",
                [&package.skill.name],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql)?;
        let old = old
            .map(|r| serde_json::from_str::<Record>(&r))
            .transpose()?;
        if legacy.is_some() && old.is_some() {
            return Err(error(
                "An existing native skill cannot be replaced by legacy adoption",
            ));
        }
        if expected.is_some_and(|v| v != old.as_ref().map(|r| r.revision).unwrap_or(0)) {
            return Err(error("Skill revision changed"));
        }
        if let Some(record) = old
            .as_ref()
            .filter(|r| !r.deleted && r.package_sha256 == package.sha256)
        {
            let record = record.clone();
            tx.commit().map_err(sql)?;
            return Ok(record);
        }
        let revision = old.as_ref().map(|r| r.revision + 1).unwrap_or(1);
        let record = Record {
            skill: package.skill.clone(),
            revision,
            pinned: legacy
                .map(|v| v.2)
                .unwrap_or_else(|| old.as_ref().is_some_and(|r| r.pinned)),
            archived: legacy
                .map(|v| v.3)
                .unwrap_or_else(|| old.as_ref().is_some_and(|r| r.archived)),
            deleted: false,
            source: source.into(),
            package_sha256: package.sha256.clone(),
            required_runtimes: package.required_runtimes(),
            degraded: old
                .as_ref()
                .is_some_and(|r| r.degraded_packages.contains(&package.sha256)),
            degraded_packages: old
                .as_ref()
                .map(|r| r.degraded_packages.clone())
                .unwrap_or_default(),
            restored_from: None,
            content_revision: revision,
        };
        let body = serde_json::to_string(&record)?;
        tx.execute("INSERT INTO skills VALUES (?,?,?) ON CONFLICT(name) DO UPDATE SET revision=excluded.revision,body=excluded.body",params![record.skill.name,revision,body]).map_err(sql)?;
        tx.execute(
            "INSERT INTO skill_versions VALUES (?,?,?)",
            params![record.skill.name, revision, body],
        )
        .map_err(sql)?;
        for (path, bytes) in &package.files {
            tx.execute(
                "INSERT INTO skill_resources VALUES (?,?,?,?,?)",
                params![record.skill.name, revision, path, hash(bytes), bytes],
            )
            .map_err(sql)?;
        }
        if let Some((origin, digest, _, _)) = legacy {
            tx.execute(
                "INSERT INTO skill_imports VALUES (?,?,?)",
                params![origin, record.skill.name, digest],
            )
            .map_err(sql)?;
        }
        tx.commit().map_err(sql)?;
        Ok(record)
    }
    pub fn adopt(
        &mut self,
        source: &str,
        digest: &str,
        package: &Package,
        pinned: bool,
        archived: bool,
    ) -> io::Result<Record> {
        self.write(
            package,
            source,
            None,
            Some((source, digest, pinned, archived)),
        )
    }
    pub fn adopt_with_snapshot(
        &mut self,
        source: &str,
        digest: &str,
        package: &Package,
        pinned: bool,
        archived: bool,
        snapshot: &Value,
    ) -> io::Result<Record> {
        let bytes = serde_json::to_string(snapshot)?;
        if bytes.len() > 1024 * 1024 {
            return Err(error("Legacy skill snapshot exceeds limits"));
        }
        // Snapshot persistence is immutable. The import digest above is committed
        // with its current projection; a lost snapshot acknowledgment is resumable.
        let record = self.adopt(source, digest, package, pinned, archived)?;
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(sql)?;
        let prior: Option<String> = tx
            .query_row(
                "SELECT body FROM skill_import_snapshots WHERE source=? AND name=?",
                params![source, package.skill.name],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql)?;
        if prior.as_ref().is_some_and(|p| p != &bytes) {
            return Err(error("Legacy skill snapshot changed"));
        }
        tx.execute(
            "INSERT OR IGNORE INTO skill_import_snapshots VALUES (?,?,?)",
            params![source, package.skill.name, bytes],
        )
        .map_err(sql)?;
        tx.commit().map_err(sql)?;
        Ok(record)
    }
    pub fn change(
        &mut self,
        name: &str,
        action: &str,
        expected: Option<u64>,
    ) -> io::Result<Record> {
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(sql)?;
        let row: String = tx
            .query_row("SELECT body FROM skills WHERE name=?", [name], |r| r.get(0))
            .optional()
            .map_err(sql)?
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Skill is missing"))?;
        let mut record: Record = serde_json::from_str(&row)?;
        if expected.is_some_and(|v| v != record.revision) {
            return Err(error("Skill revision changed"));
        }
        if record.deleted && action != "restore" {
            return Err(error("Skill was deleted"));
        }
        let prior = record.revision;
        match action {
            "pin" => record.pinned = true,
            "unpin" => record.pinned = false,
            "archive" => record.archived = true,
            "restore" => {
                record.archived = false;
                record.deleted = false;
            }
            "delete" => {
                record.deleted = true;
                record.pinned = false;
            }
            _ => return Err(error("Unsupported skill lifecycle operation")),
        }
        record.revision += 1;
        let body = serde_json::to_string(&record)?;
        tx.execute(
            "UPDATE skills SET revision=?,body=? WHERE name=? AND revision=?",
            params![record.revision, body, name, prior],
        )
        .map_err(sql)?;
        tx.execute(
            "INSERT INTO skill_versions VALUES (?,?,?)",
            params![name, record.revision, body],
        )
        .map_err(sql)?;
        tx.execute("INSERT INTO skill_resources SELECT name,?,path,hash,body FROM skill_resources WHERE name=? AND revision=?",params![record.revision,name,prior]).map_err(sql)?;
        tx.commit().map_err(sql)?;
        Ok(record)
    }
    /// Marks `name` degraded because its recent uses mostly failed (Requirement 16.5),
    /// and makes the previous passing revision ([`previous_passing`]) active again.
    ///
    /// A new revision is written with the restored revision's package (instructions,
    /// resources, source and runtimes), `degraded` set, `restored_from` naming the
    /// restored revision, and the current package added to `degraded_packages`. Without
    /// an earlier passing revision the current package stays active, marked degraded.
    /// Pin and archive state are kept. A skill that is already degraded is returned
    /// unchanged; a deleted one is an error.
    pub fn degrade(&mut self, name: &str, expected: Option<u64>) -> io::Result<Record> {
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(sql)?;
        let row: String = tx
            .query_row("SELECT body FROM skills WHERE name=?", [name], |r| r.get(0))
            .optional()
            .map_err(sql)?
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Skill is missing"))?;
        let current: Record = serde_json::from_str(&row)?;
        if expected.is_some_and(|v| v != current.revision) {
            return Err(error("Skill revision changed"));
        }
        if current.deleted {
            return Err(error("Skill was deleted"));
        }
        if current.degraded {
            tx.commit().map_err(sql)?;
            return Ok(current);
        }
        let versions = {
            let mut query = tx
                .prepare("SELECT body FROM skill_versions WHERE name=? ORDER BY revision LIMIT 1024")
                .map_err(sql)?;
            let rows = query
                .query_map([name], |r| r.get::<_, String>(0))
                .map_err(sql)?;
            rows.map(|body| serde_json::from_str::<Record>(&body.map_err(sql)?).map_err(sql))
                .collect::<io::Result<Vec<_>>>()?
        };
        let mut bad = current.degraded_packages.clone();
        bad.insert(current.package_sha256.clone());
        let revision = current.revision + 1;
        let (mut record, resources_from) = match previous_passing(&versions, &current, &bad) {
            Some(target) => (
                Record {
                    skill: target.skill.clone(),
                    source: target.source.clone(),
                    package_sha256: target.package_sha256.clone(),
                    required_runtimes: target.required_runtimes.clone(),
                    restored_from: Some(target.revision),
                    content_revision: revision,
                    ..current.clone()
                },
                target.revision,
            ),
            None => (current.clone(), current.revision),
        };
        record.revision = revision;
        record.degraded = true;
        record.degraded_packages = bad;
        let body = serde_json::to_string(&record)?;
        tx.execute(
            "UPDATE skills SET revision=?,body=? WHERE name=? AND revision=?",
            params![revision, body, name, current.revision],
        )
        .map_err(sql)?;
        tx.execute(
            "INSERT INTO skill_versions VALUES (?,?,?)",
            params![name, revision, body],
        )
        .map_err(sql)?;
        tx.execute("INSERT INTO skill_resources SELECT name,?,path,hash,body FROM skill_resources WHERE name=? AND revision=?",params![revision,name,resources_from]).map_err(sql)?;
        tx.commit().map_err(sql)?;
        Ok(record)
    }
    pub fn edit(&mut self, name: &str, text: &str, expected: Option<u64>) -> io::Result<Record> {
        let tx = self.db.unchecked_transaction().map_err(sql)?;
        let record = self
            .get(name)?
            .filter(|r| !r.deleted)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Skill is missing"))?;
        if expected.is_some_and(|v| v != record.revision) {
            return Err(error("Skill revision changed"));
        }
        let mut q=self.db.prepare("SELECT path,body,hash FROM skill_resources WHERE name=? AND revision=? ORDER BY path LIMIT 257").map_err(sql)?;
        let rows = q
            .query_map(params![name, record.revision], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Vec<u8>>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(sql)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql)?;
        let mut files = BTreeMap::new();
        for (path, bytes, digest) in rows {
            if hash(&bytes) != digest {
                return Err(error("Skill resource changed"));
            }
            files.insert(path, bytes);
        }
        files.insert("SKILL.md".into(), text.as_bytes().to_vec());
        let package = Package::from_files(name, files)?;
        if package.skill.name != name {
            return Err(error("Editing cannot rename a skill"));
        }
        drop(q);
        tx.commit().map_err(sql)?;
        self.install(&package, &record.source, Some(record.revision))
    }
    pub fn resource(&self, name: &str, path: &str) -> io::Result<Vec<u8>> {
        self.read_resource(name, path, None)
    }
    /// Every stored revision of `name`, oldest first: installs and lifecycle changes.
    /// Earlier revisions are never removed. At most 1024.
    pub fn versions(&self, name: &str) -> io::Result<Vec<Record>> {
        let mut query = self
            .db
            .prepare("SELECT body FROM skill_versions WHERE name=? ORDER BY revision LIMIT 1024")
            .map_err(sql)?;
        let rows = query
            .query_map([name], |r| r.get::<_, String>(0))
            .map_err(sql)?;
        rows.map(|body| serde_json::from_str(&body.map_err(sql)?).map_err(sql))
            .collect()
    }
    /// Paths of the active revision's files, `SKILL.md` included, sorted. At most 256.
    pub fn resource_paths(&self, name: &str) -> io::Result<Vec<String>> {
        let record = self
            .get(name)?
            .filter(|r| !r.deleted && !r.archived)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Skill is not active"))?;
        let mut query = self
            .db
            .prepare("SELECT path FROM skill_resources WHERE name=? AND revision=? ORDER BY path LIMIT 256")
            .map_err(sql)?;
        let paths = query
            .query_map(params![name, record.revision], |r| r.get::<_, String>(0))
            .map_err(sql)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql)?;
        Ok(paths)
    }

    pub fn pinned_resource(
        &self,
        name: &str,
        path: &str,
        revision: u64,
        package_sha256: &str,
    ) -> io::Result<Vec<u8>> {
        self.read_resource(name, path, Some((revision, package_sha256)))
    }

    fn read_resource(
        &self,
        name: &str,
        path: &str,
        pin: Option<(u64, &str)>,
    ) -> io::Result<Vec<u8>> {
        let tx = self.db.unchecked_transaction().map_err(sql)?;
        let record = self
            .get(name)?
            .filter(|r| !r.deleted && !r.archived)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Skill is not active"))?;
        if pin.is_some_and(|(revision, digest)| {
            record.revision != revision
                || record.package_sha256 != digest
                || !record.required_runtimes.is_empty()
        }) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Pinned skill changed or requires unavailable runtimes",
            ));
        }
        let row: Option<(String, Vec<u8>)> = tx
            .query_row(
                "SELECT hash,body FROM skill_resources WHERE name=? AND revision=? AND path=?",
                params![name, record.revision, path],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(sql)?;
        let Some((digest, body)) = row else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "Skill resource is missing",
            ));
        };
        if body.len() > 2 * 1024 * 1024 || hash(&body) != digest {
            return Err(error("Skill resource changed"));
        }
        tx.commit().map_err(sql)?;
        Ok(body)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn revisions_resource_copy_and_tombstones_survive_restart() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("skills/registry.sqlite");
        let mut registry = Registry::open(&path).unwrap();
        let package = Package::plain("reader", "Read references carefully").unwrap();
        let saved = registry.install(&package, "operator", Some(0)).unwrap();
        assert_eq!(saved.revision, 1);
        assert_eq!(
            registry
                .install(&package, "operator", None)
                .unwrap()
                .revision,
            1
        );
        registry.change("reader", "pin", Some(1)).unwrap();
        assert!(registry.get("reader").unwrap().unwrap().pinned);
        assert_eq!(
            registry.resource("reader", "SKILL.md").unwrap(),
            b"Read references carefully"
        );
        let changed = Package::plain("reader", "Read references twice").unwrap();
        assert_eq!(registry.install(&changed, "learned", None).unwrap().revision, 3);
        let versions: Vec<_> = registry
            .versions("reader")
            .unwrap()
            .into_iter()
            .map(|r| (r.revision, r.source, r.skill.body))
            .collect();
        assert_eq!(
            versions,
            [
                (1, "operator".into(), "Read references carefully".into()),
                (2, "operator".into(), "Read references carefully".into()),
                (3, "learned".into(), "Read references twice".into()),
            ]
        );
        registry.change("reader", "archive", None).unwrap();
        assert!(registry.resource("reader", "SKILL.md").is_err());
        registry.change("reader", "restore", None).unwrap();
        registry.change("reader", "delete", None).unwrap();
        drop(registry);
        let mut registry = Registry::open(&path).unwrap();
        assert!(registry.list().unwrap().is_empty());
        registry.change("reader", "restore", None).unwrap();
        assert_eq!(
            registry.resource("reader", "SKILL.md").unwrap(),
            b"Read references twice"
        );
    }
    #[test]
    fn edits_preserve_resources_and_validate_revision_and_integrity() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("skills/registry.sqlite");
        let mut registry = Registry::open(&path).unwrap();
        let text="---\nname: reader\nmetadata:\n  required_resources: [references/data.json]\n---\nOriginal";
        let package = Package::from_files(
            "reader",
            BTreeMap::from([
                ("SKILL.md".into(), text.as_bytes().to_vec()),
                ("references/data.json".into(), b"{}".to_vec()),
            ]),
        )
        .unwrap();
        registry.install(&package, "fixture", Some(0)).unwrap();
        registry
            .edit("reader", &text.replace("Original", "Edited"), Some(1))
            .unwrap();
        assert_eq!(
            registry.resource("reader", "references/data.json").unwrap(),
            b"{}"
        );
        assert!(registry.edit("reader", text, Some(1)).is_err());
        assert!(registry
            .edit(
                "reader",
                &text.replace("name: reader", "name: renamed"),
                None
            )
            .is_err());
        registry
            .db
            .execute(
                "UPDATE skill_resources SET body=X'626164' WHERE path='references/data.json'",
                [],
            )
            .unwrap();
        assert!(registry.resource("reader", "references/data.json").is_err());
        assert!(registry.edit("reader", text, None).is_err());
    }
    #[test]
    fn degrading_restores_the_previous_passing_revision_and_keeps_history() {
        let root = tempfile::tempdir().unwrap();
        let mut registry = Registry::open(&root.path().join("skills/registry.sqlite")).unwrap();
        // Only one revision: it stays active, marked degraded.
        let first = Package::plain("digest", "First instructions").unwrap();
        registry.install(&first, "learned", None).unwrap();
        let alone = registry.degrade("digest", Some(1)).unwrap();
        assert!(alone.degraded && alone.restored_from.is_none());
        assert_eq!((alone.revision, alone.skill.body.as_str()), (2, "First instructions"));
        // Degrading again changes nothing.
        assert_eq!(registry.degrade("digest", None).unwrap().revision, 2);

        let mut registry = {
            let root = root.path().join("second");
            std::fs::create_dir(&root).unwrap();
            Registry::open(&root.join("skills/registry.sqlite")).unwrap()
        };
        let one = Package::plain("digest", "Version one").unwrap();
        let two = Package::plain("digest", "Version two").unwrap();
        registry.install(&one, "learned", None).unwrap();
        registry.change("digest", "pin", None).unwrap();
        let installed = registry.install(&two, "learned", None).unwrap();
        assert_eq!((installed.revision, installed.content_revision), (3, 3));
        assert!(installed.pinned && !installed.degraded);
        let degraded = registry.degrade("digest", Some(3)).unwrap();
        assert_eq!(degraded.revision, 4);
        assert!(degraded.degraded && degraded.pinned);
        // Revision 2 (the pin) is the latest revision holding version one.
        assert_eq!(degraded.restored_from, Some(2));
        assert_eq!(degraded.content_revision, 4);
        assert_eq!(degraded.package_sha256, one.sha256);
        assert!(degraded.degraded_packages.contains(&two.sha256));
        assert_eq!(registry.instructions("digest").unwrap(), "Version one");
        assert_eq!(registry.resource("digest", "SKILL.md").unwrap(), b"Version one");
        // Earlier revisions are kept.
        assert_eq!(registry.versions("digest").unwrap().len(), 4);
        // A package that degraded before is degraded again when installed again.
        assert!(registry.install(&two, "learned", None).unwrap().degraded);
        // A new package clears the flag and keeps the known-bad packages.
        let three = Package::plain("digest", "Version three").unwrap();
        let fresh = registry.install(&three, "learned", None).unwrap();
        assert!(!fresh.degraded && fresh.degraded_packages.contains(&two.sha256));
        assert_eq!(fresh.content_revision, fresh.revision);
        // Degrading it skips the bad package (revision 5) and restores version one.
        let again = registry.degrade("digest", None).unwrap();
        assert_eq!(again.restored_from, Some(4));
        assert_eq!(again.package_sha256, one.sha256);
        assert!(again.degraded_packages.contains(&three.sha256));
        registry.change("digest", "delete", None).unwrap();
        assert!(registry.degrade("digest", None).is_err());
    }
    #[test]
    fn legacy_adoption_never_reverts_new_edits_or_resurrects_deletions() {
        let root = tempfile::tempdir().unwrap();
        let mut registry = Registry::open(&root.path().join("skills/registry.sqlite")).unwrap();
        let old = Package::plain("reader", "Original instructions").unwrap();
        registry
            .adopt("old-db", "digest-one", &old, true, false)
            .unwrap();
        let new = Package::plain("reader", "New instructions").unwrap();
        registry.install(&new, "operator", None).unwrap();
        registry
            .adopt("old-db", "digest-one", &old, true, false)
            .unwrap();
        assert_eq!(
            registry.get("reader").unwrap().unwrap().skill.body,
            "New instructions"
        );
        registry.change("reader", "delete", None).unwrap();
        registry
            .adopt("old-db", "digest-one", &old, true, false)
            .unwrap();
        assert!(registry.list().unwrap().is_empty());
        assert!(registry
            .adopt("old-db", "different", &old, true, false)
            .is_err());
    }
}

impl Registry {
    /// Listing/editing archived data does not grant tool resource access.
    pub fn instructions(&self, name: &str) -> io::Result<String> {
        let tx = self.db.unchecked_transaction().map_err(sql)?;
        let record = self
            .get(name)?
            .filter(|r| !r.deleted)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Skill is missing"))?;
        let row:Option<(String,Vec<u8>)>=tx.query_row("SELECT hash,body FROM skill_resources WHERE name=? AND revision=? AND path='SKILL.md'",params![name,record.revision],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(sql)?;
        let Some((digest, bytes)) = row else {
            return Err(error("Skill instructions are missing"));
        };
        if bytes.len() > 65536 || hash(&bytes) != digest {
            return Err(error("Skill instructions changed"));
        }
        tx.commit().map_err(sql)?;
        String::from_utf8(bytes).map_err(sql)
    }
}
