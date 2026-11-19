//! WebUI-owned projection adapter for the independent portable history importer.
use fs2::FileExt;
use kageha_migration::{HistorySink, Importer, Report, Session};
use rusqlite::{params, Connection, OptionalExtension};
use std::{
    fs::{self, OpenOptions},
    io,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
};
fn error(message: &str) -> io::Error {
    io::Error::other(message)
}
fn sql(e: rusqlite::Error) -> io::Error {
    io::Error::other(e)
}
struct Projection {
    db: Connection,
    source: PathBuf,
}
impl Projection {
    fn verify_rows(db: &Connection, s: &Session) -> io::Result<()> {
        let exists: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM ui_sessions WHERE id=?)",
                [&s.id],
                |r| r.get(0),
            )
            .map_err(sql)?;
        if !exists {
            return Err(error("Imported chat was removed; refusing to resurrect it"));
        }
        let mut q = db
            .prepare("SELECT record FROM ui_legacy_messages WHERE chat_id=? ORDER BY ordinal")
            .map_err(sql)?;
        let rows = q
            .query_map([&s.id], |r| r.get::<_, String>(0))
            .map_err(sql)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql)?;
        let rows = rows
            .iter()
            .map(|v| serde_json::from_str::<serde_json::Value>(v).map_err(io::Error::other))
            .collect::<io::Result<Vec<_>>>()?;
        if rows != s.messages {
            return Err(error("Historical message projection changed or conflicts"));
        }
        let count: usize = db
            .query_row(
                "SELECT COUNT(*) FROM ui_legacy_files WHERE chat_id=?",
                [&s.id],
                |r| r.get(0),
            )
            .map_err(sql)?;
        if count != s.attachments.len() {
            return Err(error("Historical attachment ownership conflicts"));
        }
        for a in &s.attachments {
            let row: Option<(String, String)> = db
                .query_row(
                    "SELECT source,sha256 FROM ui_legacy_files WHERE chat_id=? AND path=?",
                    params![s.id, a.relative],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(sql)?;
            if row != Some((a.owned_path.to_string_lossy().into(), a.sha256.clone())) {
                return Err(error("Historical attachment projection changed"));
            }
        }
        Ok(())
    }
}
impl HistorySink for Projection {
    fn apply(&mut self, s: &Session) -> io::Result<()> {
        if !crate::server::identifier(&s.id) || s.format_version != 1 {
            return Err(error("Unsupported portable history identity"));
        }
        let tx = self.db.transaction().map_err(sql)?;
        let receipt: Option<(String, String)> = tx
            .query_row(
                "SELECT source,digest FROM ui_portable_history_v1 WHERE chat_id=?",
                [&s.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(sql)?;
        if let Some((source, digest)) = receipt {
            if source != s.source_id || digest != s.source_digest {
                return Err(error("Chat is already owned by another migration source"));
            }
            Self::verify_rows(&tx, s)?;
            return Ok(());
        }
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM ui_sessions WHERE id=?)",
                [&s.id],
                |r| r.get(0),
            )
            .map_err(sql)?;
        if exists {
            // A previous legacy import can be adopted only if its messages and attachment
            // hashes match exactly. Existing titles/flags and new Rust turns stay untouched.
            let rows = tx
                .prepare("SELECT record FROM ui_legacy_messages WHERE chat_id=? ORDER BY ordinal")
                .map_err(sql)?
                .query_map([&s.id], |r| r.get::<_, String>(0))
                .map_err(sql)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sql)?;
            let rows = rows
                .iter()
                .map(|v| serde_json::from_str::<serde_json::Value>(v).map_err(io::Error::other))
                .collect::<io::Result<Vec<_>>>()?;
            let known: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM ui_legacy_imports WHERE chat_id=?)",
                    [&s.id],
                    |r| r.get(0),
                )
                .map_err(sql)?;
            if !known || rows != s.messages {
                return Err(error("Existing chat is not a matching legacy import"));
            }
            let count: usize = tx
                .query_row(
                    "SELECT COUNT(*) FROM ui_legacy_files WHERE chat_id=?",
                    [&s.id],
                    |r| r.get(0),
                )
                .map_err(sql)?;
            if count != s.attachments.len() {
                return Err(error("Existing legacy attachment set conflicts"));
            }
            for a in &s.attachments {
                let hash: Option<String> = tx
                    .query_row(
                        "SELECT sha256 FROM ui_legacy_files WHERE chat_id=? AND path=?",
                        params![s.id, a.relative],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(sql)?;
                if hash.as_deref() != Some(a.sha256.as_str()) {
                    return Err(error("Existing legacy attachment bytes conflict"));
                }
            }
        }
        let title = s.metadata["title"]
            .as_str()
            .or_else(|| s.metadata["name"].as_str());
        let now = crate::ui_store::now();
        let created = s.metadata["created_at"]
            .as_f64()
            .filter(|v| v.is_finite() && *v >= 0.0)
            .unwrap_or(now);
        let updated = s.metadata["updated_at"]
            .as_f64()
            .filter(|v| v.is_finite() && *v >= 0.0)
            .unwrap_or(created);
        tx.execute(
            "INSERT OR IGNORE INTO ui_sessions(id,title,created_at,updated_at) VALUES (?,?,?,?)",
            params![s.id, title, created, updated],
        )
        .map_err(sql)?;
        tx.execute(
            "INSERT OR IGNORE INTO ui_session_flags(chat_id,pinned,archived) VALUES (?,?,?)",
            params![
                s.id,
                s.metadata["pinned"] == true,
                s.metadata["archived"] == true
            ],
        )
        .map_err(sql)?;
        for (ordinal, row) in s.messages.iter().enumerate() {
            tx.execute(
                "INSERT OR IGNORE INTO ui_legacy_messages(chat_id,ordinal,record) VALUES (?,?,?)",
                params![s.id, ordinal, row.to_string()],
            )
            .map_err(sql)?;
        }
        for a in &s.attachments {
            tx.execute("INSERT INTO ui_legacy_files(chat_id,path,source,sha256) VALUES (?,?,?,?) ON CONFLICT(chat_id,path) DO UPDATE SET source=excluded.source,sha256=excluded.sha256",params![s.id,a.relative,a.owned_path.to_string_lossy(),a.sha256]).map_err(sql)?;
        }
        let history = self.source.join(&s.id).join("chat.jsonl");
        tx.execute(
            "INSERT OR IGNORE INTO ui_legacy_imports(source,chat_id,records) VALUES (?,?,?)",
            params![history.to_string_lossy(), s.id, s.messages.len()],
        )
        .map_err(sql)?;
        tx.execute(
            "INSERT INTO ui_portable_history_v1(chat_id,source,digest) VALUES (?,?,?)",
            params![s.id, s.source_id, s.source_digest],
        )
        .map_err(sql)?;
        Self::verify_rows(&tx, s)?;
        tx.commit().map_err(sql)?;
        Ok(())
    }
    fn verify(&mut self, s: &Session) -> io::Result<()> {
        let receipt: Option<(String, String)> = self
            .db
            .query_row(
                "SELECT source,digest FROM ui_portable_history_v1 WHERE chat_id=?",
                [&s.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(sql)?;
        if receipt != Some((s.source_id.clone(), s.source_digest.clone())) {
            return Err(error("Historical projection receipt is missing or changed"));
        }
        Self::verify_rows(&self.db, s)
    }
}
/// Explicit offline migration only: never starts providers, credential helpers or
/// active runs. The same writer lock as the WebUI host prevents concurrent writers.
pub(crate) struct Destination {
    pub db: Connection,
    pub source: PathBuf,
    pub state: PathBuf,
    pub _lock: std::fs::File,
}
pub(crate) fn destination(source: &Path, state: &Path) -> io::Result<Destination> {
    if fs::symlink_metadata(source)?.file_type().is_symlink() {
        return Err(error("Source root must not be a symlink"));
    }
    let source = source.canonicalize()?;
    let absolute = if state.is_absolute() {
        state.to_owned()
    } else {
        std::env::current_dir()?.join(state)
    };
    if absolute
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return Err(error("State path must not contain parent traversal"));
    }
    let mut ancestor = absolute.clone();
    let mut suffix = Vec::new();
    while !ancestor.exists() {
        suffix.push(
            ancestor
                .file_name()
                .ok_or_else(|| error("Invalid state path"))?
                .to_owned(),
        );
        ancestor.pop();
    }
    let mut resolved = ancestor.canonicalize()?;
    for part in suffix.iter().rev() {
        resolved.push(part);
    }
    if resolved.starts_with(&source) || source.starts_with(&resolved) {
        return Err(error("Source and Rust state directories must not overlap"));
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(state)?;
    let meta = fs::symlink_metadata(state)?;
    if !meta.is_dir() || meta.file_type().is_symlink() || meta.permissions().mode() & 0o077 != 0 {
        return Err(error("Rust state must be a private real directory"));
    }
    let state = state.canonicalize()?;
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(state.join("native-server.lock"))?;
    lock.try_lock_exclusive()?;
    let path = state.join("native-chats.sqlite");
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)?;
    let db = Connection::open(path).map_err(sql)?;
    db.execute_batch("PRAGMA journal_mode=WAL;PRAGMA synchronous=FULL;
 CREATE TABLE IF NOT EXISTS ui_sessions(id TEXT PRIMARY KEY,title TEXT,created_at REAL NOT NULL,updated_at REAL NOT NULL);
 CREATE TABLE IF NOT EXISTS ui_session_flags(chat_id TEXT PRIMARY KEY,pinned INTEGER NOT NULL DEFAULT 0,archived INTEGER NOT NULL DEFAULT 0);
 CREATE TABLE IF NOT EXISTS ui_portable_history_v1(chat_id TEXT PRIMARY KEY,source TEXT NOT NULL,digest TEXT NOT NULL);").map_err(sql)?;
    crate::ui_legacy::initialize(&db).map_err(sql)?;
    Ok(Destination {
        db,
        source,
        state,
        _lock: lock,
    })
}
pub fn run(source: &Path, state: &Path) -> io::Result<Report> {
    let Destination {
        db,
        source,
        state,
        _lock,
    } = destination(source, state)?;
    let mut projection = Projection {
        db,
        source: source.clone(),
    };
    Importer::open(&source, &state.join("history-import-v1"))?.run(&mut projection)
}
