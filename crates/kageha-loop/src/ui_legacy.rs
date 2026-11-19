//! Read-only, one-time import of existing Python chat.jsonl histories.
use crate::{server, ui_store};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::{
    io::{self, BufRead, Read},
    path::PathBuf,
};
pub(crate) fn initialize(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS ui_legacy_messages(chat_id TEXT NOT NULL,ordinal INTEGER NOT NULL,record TEXT NOT NULL,PRIMARY KEY(chat_id,ordinal));
 CREATE TABLE IF NOT EXISTS ui_legacy_imports(source TEXT PRIMARY KEY,chat_id TEXT NOT NULL,records INTEGER NOT NULL);
 CREATE TABLE IF NOT EXISTS ui_legacy_files(chat_id TEXT NOT NULL,path TEXT NOT NULL,source TEXT NOT NULL,sha256 TEXT NOT NULL,PRIMARY KEY(chat_id,path));")
}
pub(crate) fn import(db: &mut Connection) -> io::Result<()> {
    let root = std::env::var_os("KAGEHA_LEGACY_SESSIONS_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("KAGEHA_HOME").map(|p| PathBuf::from(p).join("sessions")))
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".kageha/sessions")));
    let Some(root) = root.filter(|p| p.is_dir()) else {
        return Ok(());
    };
    let portable: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='ui_portable_history_v1')",
        [], |r| r.get(0),
    ).map_err(io::Error::other)?;
    for entry in std::fs::read_dir(root)?.take(10000) {
        let entry = entry?;
        let id = entry.file_name().to_string_lossy().into_owned();
        if !server::identifier(&id) || !entry.file_type()?.is_dir() {
            continue;
        }
        // A portable receipt also fences deleted chats. Never use a path alias
        // or a changed source root to bypass ownership or resurrect its content.
        if portable
            && db
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM ui_portable_history_v1 WHERE chat_id=?)",
                    [&id],
                    |r| r.get::<_, bool>(0),
                )
                .map_err(io::Error::other)?
        {
            continue;
        }
        let history = entry.path().join("chat.jsonl");
        let metadata = match std::fs::symlink_metadata(&history) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() > 16 * 1024 * 1024
        {
            continue;
        }
        let source = history.to_string_lossy().into_owned();
        let imported: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM ui_legacy_imports WHERE source=?)",
                [&source],
                |r| r.get(0),
            )
            .map_err(io::Error::other)?;
        if imported {
            continue;
        }
        let mut records = Vec::new();
        let file = std::fs::File::open(&history)?;
        // Read a bounded snapshot and never rewrite the legacy history or its locks.
        for line in io::BufReader::new(file.take(metadata.len()))
            .lines()
            .take(20000)
        {
            let line = line?;
            if line.len() > 262144 {
                continue;
            }
            if let Ok(mut row) = serde_json::from_str::<Value>(&line) {
                if !matches!(row["role"].as_str(), Some("user" | "assistant")) {
                    continue;
                }
                if row["text"].is_null() {
                    row["text"] = row["content"].clone()
                }
                if row["text"].is_string() {
                    records.push(row)
                }
            }
        }
        let mut meta = json!({});
        for name in ["meta.json", "metadata.json", "session.json"] {
            let path = entry.path().join(name);
            if std::fs::symlink_metadata(&path)
                .is_ok_and(|m| m.is_file() && !m.file_type().is_symlink() && m.len() < 262144)
            {
                if let Ok(bytes) = std::fs::read(path) {
                    if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
                        meta = v;
                        break;
                    }
                }
            }
        }
        records.retain(|row| {
            let deleted = meta["deleted_history_turn_ids"]
                .as_array()
                .is_some_and(|ids| ids.contains(&row["turn_id"]));
            !deleted
                || (row["role"] == "user"
                    && meta["retained_history_user_turn_ids"]
                        .as_array()
                        .is_some_and(|ids| ids.contains(&row["turn_id"])))
        });
        let title = meta["title"]
            .as_str()
            .or_else(|| meta["name"].as_str())
            .map(str::to_owned);
        let tx = db.transaction().map_err(io::Error::other)?;
        tx.execute(
            "INSERT OR IGNORE INTO ui_sessions(id,title,created_at,updated_at) VALUES (?,?,?,?)",
            params![id, title, ui_store::now(), ui_store::now()],
        )
        .map_err(io::Error::other)?;
        tx.execute(
            "INSERT OR IGNORE INTO ui_session_flags VALUES (?,?,?)",
            params![id, meta["pinned"] == true, meta["archived"] == true],
        )
        .map_err(io::Error::other)?;
        for record in &records {
            for relative in record["attachments"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .take(64)
            {
                if relative.len() > 512
                    || !std::path::Path::new(relative)
                        .components()
                        .all(|p| matches!(p, std::path::Component::Normal(_)))
                {
                    continue;
                }
                let file = entry.path().join(relative);
                let valid = std::fs::symlink_metadata(&file).is_ok_and(|m| {
                    m.is_file() && !m.file_type().is_symlink() && m.len() <= 16 * 1024 * 1024
                });
                if !valid {
                    continue;
                }
                let canonical = file.canonicalize()?;
                if !canonical.starts_with(entry.path().canonicalize()?) {
                    continue;
                }
                let bytes = std::fs::read(&canonical)?;
                use sha2::{Digest, Sha256};
                let hash = format!("{:x}", Sha256::digest(&bytes));
                tx.execute(
                    "INSERT OR IGNORE INTO ui_legacy_files VALUES (?,?,?,?)",
                    params![id, relative, canonical.to_string_lossy(), hash],
                )
                .map_err(io::Error::other)?;
            }
        }
        for (ordinal, record) in records.iter().enumerate() {
            tx.execute(
                "INSERT OR IGNORE INTO ui_legacy_messages VALUES (?,?,?)",
                params![id, ordinal, record.to_string()],
            )
            .map_err(io::Error::other)?;
        }
        tx.execute(
            "INSERT INTO ui_legacy_imports VALUES (?,?,?)",
            params![source, id, records.len()],
        )
        .map_err(io::Error::other)?;
        tx.commit().map_err(io::Error::other)?;
    }
    Ok(())
}
pub(crate) fn records(db: &Connection, chat: &str) -> rusqlite::Result<Vec<Value>> {
    let mut q =
        db.prepare("SELECT record FROM ui_legacy_messages WHERE chat_id=? ORDER BY ordinal")?;
    let rows = q
        .query_map([chat], |r| r.get::<_, String>(0))?
        .map(|r| r.map(|text| serde_json::from_str(&text).unwrap_or(json!({}))))
        .collect();
    rows
}

pub(crate) fn pinned_file(
    db: &crate::webui::Database,
    chat: &str,
    path: &str,
) -> io::Result<(PathBuf, Value)> {
    use rusqlite::OptionalExtension;
    let record: Option<(String, String)> = db
        .lock()
        .map_err(|_| io::Error::other("database unavailable"))?
        .query_row(
            "SELECT source,sha256 FROM ui_legacy_files WHERE chat_id=? AND path=?",
            params![chat, path],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(io::Error::other)?;
    let (source, hash) =
        record.ok_or_else(|| io::Error::other("File is not owned by this chat"))?;
    let source = PathBuf::from(source);
    let metadata = std::fs::symlink_metadata(&source)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 16 * 1024 * 1024
    {
        return Err(io::Error::other("Legacy attachment is unavailable"));
    }
    let snapshot = kageha_workspace::file_snapshot(
        &source,
        Some(&hash),
        &kageha_contracts::execution::Control {
            cancelled: &std::sync::atomic::AtomicBool::new(false),
            deadline: None,
        },
    )
    .map_err(|_| io::Error::other("Legacy attachment changed after import"))?;
    let bytes = snapshot.bytes;
    let name = source.file_name().unwrap().to_string_lossy();
    Ok((
        source.clone(),
        json!({"path":path,"filename":name,"sha256":hash,"bytes":bytes.len(),"content_type":crate::ui_uploads::mime(&name)}),
    ))
}
