//! Component-owned import journal and portable history snapshots.
//! Hosts own projection transactions; this crate never opens their databases.
pub mod documents;
mod knowledge;
mod projects;
use fs2::FileExt;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        },
    },
    path::{Component, Path, PathBuf},
};

const FILE_LIMIT: usize = 16 * 1024 * 1024;
const SESSION_LIMIT: usize = 64 * 1024 * 1024;
fn error(message: &str) -> io::Error {
    io::Error::other(message)
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn sql(e: rusqlite::Error) -> io::Error {
    io::Error::other(e)
}
fn identifier(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
}
fn relative(path: &Path) -> io::Result<()> {
    if path.as_os_str().is_empty() || !path.components().all(|c| matches!(c, Component::Normal(_)))
    {
        return Err(error("Invalid relative import path"));
    }
    Ok(())
}
fn private_dir(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir() || meta.file_type().is_symlink() || meta.permissions().mode() & 0o077 != 0 {
        return Err(error(
            "Migration directories must be private real directories",
        ));
    }
    Ok(())
}
/// Read from a pinned source directory, never following a path-component symlink.
fn read_at(root: &File, path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    relative(path)?;
    let mut directory = root.try_clone()?;
    let components = path.components().collect::<Vec<_>>();
    for (index, component) in components.iter().enumerate() {
        let name = std::ffi::CString::new(component.as_os_str().as_bytes())
            .map_err(|_| error("Invalid import path"))?;
        let last = index + 1 == components.len();
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY
                    | libc::O_NOFOLLOW
                    | libc::O_CLOEXEC
                    | libc::O_NONBLOCK
                    | if last { 0 } else { libc::O_DIRECTORY },
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut file = unsafe { File::from_raw_fd(fd) };
        if !last {
            directory = file;
            continue;
        }
        let before = file.metadata()?;
        if !before.is_file() || before.len() > limit as u64 {
            return Err(error("Import file is not regular or exceeds its limit"));
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)?;
        let after = file.metadata()?;
        if bytes.len() > limit
            || before.len() != bytes.len() as u64
            || before.len() != after.len()
            || before.mtime() != after.mtime()
            || before.mtime_nsec() != after.mtime_nsec()
            || before.ctime() != after.ctime()
            || before.ctime_nsec() != after.ctime_nsec()
        {
            return Err(error("Import source changed during read"));
        }
        return Ok(bytes);
    }
    Err(error("Expected an import file"))
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Attachment {
    pub relative: String,
    pub sha256: String,
    pub owned_path: PathBuf,
    pub bytes: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub format_version: u32,
    pub id: String,
    pub source_id: String,
    pub source_digest: String,
    pub metadata: Value,
    pub messages: Vec<Value>,
    pub attachments: Vec<Attachment>,
    pub history_snapshot: PathBuf,
    pub source_files: BTreeMap<String, String>,
}
/// apply must commit its own durable digest receipt atomically with projection.
/// Repeating the same source/digest is idempotent; conflicting ownership is denied.
pub trait HistorySink {
    fn apply(&mut self, session: &Session) -> io::Result<()>;
    fn verify(&mut self, session: &Session) -> io::Result<()>;
}
#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub imported: usize,
    pub already_present: usize,
    pub messages: usize,
    pub attachments: usize,
}
pub struct Importer {
    source: PathBuf,
    directory: File,
    destination: PathBuf,
    journal: Connection,
    _lock: File,
}
impl Importer {
    fn project_record(
        &self,
        source: &str,
        id: &str,
        fingerprint: &str,
        snapshot: String,
        mut project: impl FnMut(bool) -> io::Result<()>,
    ) -> io::Result<bool> {
        let previous: Option<(String, String)> = self
            .journal
            .query_row(
                "SELECT digest,state FROM history_import_v1 WHERE source=? AND chat=?",
                params![source, id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(sql)?;
        if previous.as_ref().is_some_and(|(old, _)| old != fingerprint) {
            return Err(error(
                "Historical source changed since import; refusing to overwrite",
            ));
        }
        let applied = previous
            .as_ref()
            .is_some_and(|(_, state)| state == "applied");
        if !applied {
            self.journal
                .execute(
                    "INSERT OR IGNORE INTO history_import_v1 VALUES (?,?,?,'prepared',?)",
                    params![source, id, fingerprint, snapshot],
                )
                .map_err(sql)?;
        }
        project(applied)?;
        if !applied {
            self.journal.execute("UPDATE history_import_v1 SET state='applied' WHERE source=? AND chat=? AND digest=?",params![source,id,fingerprint]).map_err(sql)?;
        }
        Ok(!applied)
    }
    /// The destination parent must already exist. Source and destination cannot overlap.
    pub fn open(source: &Path, destination: &Path) -> io::Result<Self> {
        if fs::symlink_metadata(source)?.file_type().is_symlink() {
            return Err(error("Source root must not be a symlink"));
        }
        let source = source.canonicalize()?;
        let destination = destination
            .parent()
            .ok_or_else(|| error("Missing destination parent"))?
            .canonicalize()?
            .join(
                destination
                    .file_name()
                    .ok_or_else(|| error("Missing destination name"))?,
            );
        if destination.starts_with(&source) || source.starts_with(&destination) {
            return Err(error("Import source and destination must not overlap"));
        }
        private_dir(&destination)?;
        let destination = destination.canonicalize()?;
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&source)?;
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(destination.join("migration.lock"))?;
        lock.try_lock_exclusive()?;
        let path = destination.join("migration.sqlite");
        OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)?;
        let journal = Connection::open(path).map_err(sql)?;
        journal.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS migration_versions(component TEXT PRIMARY KEY,version INTEGER NOT NULL);").map_err(sql)?;
        let version: Option<u32> = journal
            .query_row(
                "SELECT version FROM migration_versions WHERE component='history-import'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql)?;
        if version.is_some_and(|version| version != 1) {
            return Err(error("Unsupported history import journal version"));
        }
        journal
            .execute(
                "INSERT OR IGNORE INTO migration_versions VALUES ('history-import',1)",
                [],
            )
            .map_err(sql)?;
        journal.execute_batch("CREATE TABLE IF NOT EXISTS history_import_v1(source TEXT NOT NULL,chat TEXT NOT NULL,digest TEXT NOT NULL,state TEXT NOT NULL CHECK(state IN ('prepared','applied')),snapshot TEXT NOT NULL,PRIMARY KEY(source,chat));").map_err(sql)?;
        Ok(Self {
            source,
            directory,
            destination,
            journal,
            _lock: lock,
        })
    }
    fn blob(&self, name: &str, bytes: &[u8]) -> io::Result<PathBuf> {
        let folder = self.destination.join("blobs").join(digest(bytes));
        private_dir(&self.destination.join("blobs"))?;
        private_dir(&folder)?;
        let path = folder.join(name);
        if path.exists() {
            let dir = File::open(&folder)?;
            if read_at(&dir, Path::new(name), FILE_LIMIT)? != bytes {
                return Err(error("Owned migration blob was modified"));
            }
            return Ok(path);
        }
        let mut temp = tempfile::NamedTempFile::new_in(&folder)?;
        temp.write_all(bytes)?;
        temp.as_file().sync_all()?;
        temp.persist_noclobber(&path)
            .map_err(|_| error("Cannot persist migration blob"))?;
        File::open(&folder)?.sync_all()?;
        Ok(path)
    }
    fn snapshot(&self, id: &str) -> io::Result<Session> {
        let history = read_at(
            &self.directory,
            &Path::new(id).join("chat.jsonl"),
            FILE_LIMIT,
        )?;
        let text = std::str::from_utf8(&history).map_err(|_| error("History is not UTF-8"))?;
        let mut source_files = BTreeMap::new();
        source_files.insert("chat.jsonl".to_owned(), digest(&history));
        let mut metadata = json!({});
        let mut metadata_selected = false;
        for name in ["meta.json", "metadata.json", "session.json"] {
            match read_at(&self.directory, &Path::new(id).join(name), 262144) {
                Ok(bytes) => {
                    source_files.insert(name.to_owned(), digest(&bytes));
                    let value: Value = serde_json::from_slice(&bytes)
                        .map_err(|_| error("Invalid session metadata JSON"))?;
                    if !value.is_object() {
                        return Err(error("Session metadata must be an object"));
                    }
                    if !metadata_selected {
                        metadata = value;
                        metadata_selected = true;
                    }
                    self.blob(name, &bytes)?;
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        let mut messages = Vec::new();
        let mut paths = BTreeSet::new();
        for (line_index, line) in text.lines().enumerate() {
            if line_index >= 20000 || line.len() > 262144 {
                return Err(error("History exceeds record limits"));
            }
            if line.trim().is_empty() {
                continue;
            }
            let mut row: Value =
                serde_json::from_str(line).map_err(|_| error("Invalid history record JSON"))?;
            if !row.is_object() {
                return Err(error("History records must be objects"));
            }
            // All raw records remain in the private history snapshot. Project only
            // retained conversation roles and honor deletion/retained-user tombstones.
            if !matches!(row["role"].as_str(), Some("user" | "assistant")) {
                continue;
            }
            if row["text"].is_null() {
                row["text"] = row["content"].clone();
            }
            if !row["text"].is_string() {
                return Err(error(
                    "Unsupported conversation content; import requires review",
                ));
            }
            let deleted = metadata["deleted_history_turn_ids"]
                .as_array()
                .is_some_and(|ids| ids.contains(&row["turn_id"]));
            if deleted
                && !(row["role"] == "user"
                    && metadata["retained_history_user_turn_ids"]
                        .as_array()
                        .is_some_and(|ids| ids.contains(&row["turn_id"])))
            {
                continue;
            }
            if let Some(attachments) = row["attachments"].as_array() {
                if attachments.len() > 64 {
                    return Err(error("Too many message attachments"));
                }
                for value in attachments {
                    let path = value.as_str().ok_or_else(|| {
                        error("Unsupported attachment record; import requires review")
                    })?;
                    if path.len() > 512 {
                        return Err(error("Attachment path exceeds limit"));
                    }
                    relative(Path::new(path))?;
                    paths.insert(path.to_owned());
                }
            }
            messages.push(row);
        }
        let mut attachments = Vec::new();
        let mut total = history.len();
        for path in paths {
            let bytes = read_at(&self.directory, &Path::new(id).join(&path), FILE_LIMIT)?;
            total += bytes.len();
            if total > SESSION_LIMIT {
                return Err(error("Session exceeds import byte limit"));
            }
            let sha256 = digest(&bytes);
            source_files.insert(path.clone(), sha256.clone());
            let name = Path::new(&path)
                .file_name()
                .unwrap()
                .to_str()
                .ok_or_else(|| error("Attachment filename is not UTF-8"))?
                .to_owned();
            attachments.push(Attachment {
                relative: path,
                sha256,
                owned_path: self.blob(&name, &bytes)?,
                bytes: bytes.len(),
            });
        }
        let history_snapshot = self.blob("chat.jsonl", &history)?;
        let source_id = digest(self.source.as_os_str().as_bytes());
        let source_digest = digest(&serde_json::to_vec(&source_files)?);
        Ok(Session {
            format_version: 1,
            id: id.into(),
            source_id,
            source_digest,
            metadata,
            messages,
            attachments,
            history_snapshot,
            source_files,
        })
    }
    pub fn run(&mut self, sink: &mut dyn HistorySink) -> io::Result<Report> {
        let mut ids = BTreeSet::new();
        for entry in fs::read_dir(&self.source)? {
            let entry = entry?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| error("Source ID is not UTF-8"))?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            if !entry.path().join("chat.jsonl").exists() {
                continue;
            }
            if !identifier(&name) {
                return Err(error("Invalid session ID"));
            }
            ids.insert(name);
            if ids.len() > 10000 {
                return Err(error("Too many sessions"));
            }
        }
        let source_id = digest(self.source.as_os_str().as_bytes());
        let old = self
            .journal
            .prepare("SELECT chat FROM history_import_v1 WHERE source=?")
            .map_err(sql)?
            .query_map([&source_id], |r| r.get::<_, String>(0))
            .map_err(sql)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql)?;
        if old.iter().any(|id| !ids.contains(id)) {
            return Err(error("Previously imported source session is missing"));
        }
        let mut report = Report::default();
        for id in ids {
            let session = self.snapshot(&id)?;
            let imported = self.project_record(
                &source_id,
                &id,
                &session.source_digest,
                serde_json::to_string(&session)?,
                |applied| {
                    if applied {
                        return sink.verify(&session);
                    }
                    if self.snapshot(&id)?.source_digest != session.source_digest {
                        return Err(error("Source changed before projection"));
                    }
                    sink.apply(&session)?;
                    sink.verify(&session)
                },
            )?;
            if imported {
                report.imported += 1;
            } else {
                report.already_present += 1;
            }
            report.messages += session.messages.len();
            report.attachments += session.attachments.len();
        }
        Ok(report)
    }
}
