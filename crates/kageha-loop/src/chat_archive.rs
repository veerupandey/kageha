//! Import a private, read-only export at host cutover without executing the old host.
use crate::{browser_client, server};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
#[cfg(test)] use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs, io,
    path::{Component, Path, PathBuf},
};
fn invalid(message: impl std::fmt::Display) -> io::Error {
    io::Error::other(message.to_string())
}
fn json_file(path: &Path) -> io::Result<(Value, String)> {
    browser_client::private(path)?;
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.len() > 64 * 1024 * 1024 {
        return Err(invalid("Invalid chat archive file"));
    }
    let bytes = fs::read(path)?;
    let hash = format!("{:x}", Sha256::digest(&bytes));
    Ok((
        serde_json::from_slice(&bytes).map_err(|_| invalid("Invalid chat archive JSON"))?,
        hash,
    ))
}
fn session_root() -> io::Result<PathBuf> {
    std::env::var_os("KAGEHA_LEGACY_SESSIONS_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("KAGEHA_HOME").map(|p| PathBuf::from(p).join("sessions")))
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".kageha/sessions")))
        .ok_or_else(|| invalid("Historical chat root is not configured"))?
        .canonicalize()
}
pub(crate) fn import(db: &mut Connection, root: &Path) -> io::Result<usize> {
    import_from(db, root, &session_root()?)
}
fn import_from(db: &mut Connection, root: &Path, sessions: &Path) -> io::Result<usize> {
    browser_client::private(root)?;
    let (manifest, _) = json_file(&root.join("manifest.json"))?;
    let ids = manifest["sessions"]
        .as_array()
        .filter(|v| v.len() <= 10000)
        .ok_or_else(|| invalid("Invalid chat archive manifest"))?;
    if manifest["version"] != 1 {
        return Err(invalid("Unsupported chat archive version"));
    }
    db.execute_batch("CREATE TABLE IF NOT EXISTS ui_chat_archive_imports(chat_id TEXT PRIMARY KEY,digest TEXT NOT NULL)").map_err(invalid)?;
    let mut seen = HashSet::new();
    let mut imported = 0;
    for value in ids {
        let id = value
            .as_str()
            .filter(|v| server::identifier(v) && seen.insert(v.to_string()))
            .ok_or_else(|| invalid("Invalid or duplicate archive chat ID"))?;
        let (archive, digest) = json_file(&root.join(format!("{id}.json")))?;
        let digest=format!("v2:{digest}");
        if archive["version"] != 1 || archive["session_id"] != id {
            return Err(invalid("Chat archive identity mismatch"));
        }
        let prior: Option<String> = db
            .query_row(
                "SELECT digest FROM ui_chat_archive_imports WHERE chat_id=?",
                [id],
                |r| r.get(0),
            )
            .optional()
            .map_err(invalid)?;
        if prior.as_deref() == Some(&digest) {
            continue;
        }
        let messages = archive["messages"]
            .as_array()
            .filter(|m| m.len() <= 500000)
            .ok_or_else(|| invalid("Invalid archive history"))?;
        if messages.iter().any(|m| {
            !matches!(m["role"].as_str(), Some("user" | "assistant")) || !m["text"].is_string()
        }) {
            return Err(invalid("Invalid archive message"));
        }
        let directory=sessions.join(id);
        if fs::symlink_metadata(&directory)?.file_type().is_symlink(){return Err(invalid("Unsafe historical chat directory"));}
        let expected = directory.canonicalize()?;
        if !expected.starts_with(sessions.canonicalize()?) {return Err(invalid("Historical chat escaped its root"));}
        if Path::new(archive["source_root"].as_str().unwrap_or("")).canonicalize()? != expected {
            return Err(invalid("Archive file ownership mismatch"));
        }
        let mut files = Vec::new();
        for file in archive["files"]
            .as_array()
            .filter(|v| v.len() <= 100000)
            .ok_or_else(|| invalid("Invalid archive files"))?
        {
            let relative = file["path"]
                .as_str()
                .ok_or_else(|| invalid("Missing archive file path"))?;
            let path = Path::new(relative);
            if !path.components().all(|c| matches!(c, Component::Normal(_)))
                || !matches!(path.components().next(),Some(Component::Normal(name)) if name=="artifacts"||name=="inputs")
            {
                return Err(invalid("Unsafe archive file path"));
            }
            let source = expected.join(path);
            let meta = fs::symlink_metadata(&source)?;
            if !meta.is_file()
                || meta.file_type().is_symlink()
                || !source.canonicalize()?.starts_with(&expected)
            {
                return Err(invalid("Unsafe archive file source"));
            }
            let hash = file["sha256"]
                .as_str()
                .filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
                .ok_or_else(|| invalid("Invalid archive file digest"))?;
            if meta.len() != file["bytes"].as_u64().unwrap_or(u64::MAX)
                || meta.len() > 16 * 1024 * 1024
            {
                return Err(invalid("Archive file changed during cutover"));
            }
            files.push((
                relative.to_string(),
                source.to_string_lossy().into_owned(),
                hash.to_string(),
            ));
        }
        let meta = &archive["metadata"];
        let created = meta["created_at"].as_f64().unwrap_or(0.0);
        let updated = meta["updated_at"].as_f64().unwrap_or(created);
        let title = meta["title"]
            .as_str()
            .filter(|v| !v.is_empty())
            .or_else(|| meta["objective"].as_str())
            .map(str::to_owned);
        let tx = db.transaction().map_err(invalid)?;
        tx.execute("INSERT INTO ui_sessions VALUES (?,?,?,?) ON CONFLICT(id) DO UPDATE SET title=excluded.title,created_at=excluded.created_at,updated_at=CASE WHEN EXISTS(SELECT 1 FROM native_chat_turns WHERE chat_id=excluded.id) THEN MAX(ui_sessions.updated_at,excluded.updated_at) ELSE excluded.updated_at END",params![id,title,created,updated]).map_err(invalid)?;
        tx.execute("INSERT INTO ui_session_flags VALUES (?,?,?) ON CONFLICT(chat_id) DO UPDATE SET pinned=excluded.pinned,archived=excluded.archived",params![id,meta["pinned"]==true,meta["archived"]==true]).map_err(invalid)?;
        // Only replace the historical mirror. Native turns and their journals remain untouched.
        tx.execute("DELETE FROM ui_legacy_messages WHERE chat_id=?", [id])
            .map_err(invalid)?;
        for (ordinal, message) in messages.iter().enumerate() {
            tx.execute(
                "INSERT INTO ui_legacy_messages VALUES (?,?,?)",
                params![id, ordinal as u64, message.to_string()],
            )
            .map_err(invalid)?;
        }
        for (path, source, hash) in files {
            tx.execute("INSERT INTO ui_legacy_files VALUES (?,?,?,?) ON CONFLICT(chat_id,path) DO UPDATE SET source=excluded.source,sha256=excluded.sha256",params![id,path,source,hash]).map_err(invalid)?;
        }
        tx.execute("INSERT INTO ui_chat_archive_imports VALUES (?,?) ON CONFLICT(chat_id) DO UPDATE SET digest=excluded.digest",params![id,digest]).map_err(invalid)?;
        tx.commit().map_err(invalid)?;
        imported += 1;
    }
    Ok(imported)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn imports_empty_and_full_chats_idempotently_without_replacing_native_turns() {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let sessions = root.path().join("sessions");
        fs::create_dir_all(sessions.join("empty")).unwrap();
        fs::create_dir_all(sessions.join("full/artifacts")).unwrap();
        fs::write(sessions.join("full/artifacts/a.pdf"), b"fixture").unwrap();
        let write = |name: &str, value: Value| {
            let p = root.path().join(name);
            fs::write(&p, value.to_string()).unwrap();
            fs::set_permissions(p, fs::Permissions::from_mode(0o600)).unwrap();
        };
        write(
            "manifest.json",
            json!({"version":1,"sessions":["empty","full"]}),
        );
        for id in ["empty", "full"] {
            write(
                &format!("{id}.json"),
                json!({"version":1,"session_id":id,"source_root":sessions.join(id),"metadata":{"title":id,"created_at":1,"updated_at":2},"messages":if id=="empty"{vec![]}else{vec![json!({"role":"user","text":"past message"})]},"files":if id=="empty"{vec![]}else{vec![json!({"path":"artifacts/a.pdf","sha256":format!("{:x}",Sha256::digest(b"fixture")),"bytes":7})]}}),
            );
        }
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE ui_sessions(id TEXT PRIMARY KEY,title TEXT,created_at REAL,updated_at REAL); CREATE TABLE ui_session_flags(chat_id TEXT PRIMARY KEY,pinned INTEGER,archived INTEGER); CREATE TABLE native_chat_turns(chat_id TEXT,answer TEXT); INSERT INTO native_chat_turns VALUES ('full','new native work');").unwrap();
        crate::ui_legacy::initialize(&db).unwrap();
        assert_eq!(import_from(&mut db, root.path(), &sessions).unwrap(), 2);
        assert_eq!(import_from(&mut db, root.path(), &sessions).unwrap(), 0);
        assert_eq!(
            db.query_row("SELECT count(*) FROM ui_sessions", [], |r| r
                .get::<_, u64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            db.query_row("SELECT answer FROM native_chat_turns", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "new native work"
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM ui_legacy_messages", [], |r| r
                .get::<_, u64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM ui_legacy_files", [], |r| r
                .get::<_, u64>(0))
                .unwrap(),
            1
        );
        write("full.json", json!({"version":1,"session_id":"wrong"}));
        assert!(import_from(&mut db, root.path(), &sessions).is_err());
    }
}
