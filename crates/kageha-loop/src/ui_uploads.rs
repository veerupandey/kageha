//! Chat-owned upload storage using the existing multipart upload contract.
use crate::{browser_client, server, webui::Database};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use tiny_http::{Header, Method, Request, Response};

pub(crate) const MAX_BYTES: usize = 16 * 1024 * 1024;

pub(crate) fn initialize(db: &rusqlite::Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS ui_uploads(chat_id TEXT NOT NULL,path TEXT NOT NULL,
        filename TEXT NOT NULL,mime TEXT NOT NULL,sha256 TEXT NOT NULL,size INTEGER NOT NULL,
        PRIMARY KEY(chat_id,path));
        CREATE TABLE IF NOT EXISTS ui_turn_uploads(chat_id TEXT NOT NULL,request_id TEXT NOT NULL,
            paths TEXT NOT NULL,PRIMARY KEY(chat_id,request_id));
        CREATE TABLE IF NOT EXISTS ui_reused_files(
            chat_id TEXT NOT NULL, sha256 TEXT NOT NULL, path TEXT NOT NULL,
            source_chat TEXT NOT NULL, source_path TEXT NOT NULL,
            PRIMARY KEY(chat_id,sha256));",
    )
}

pub(crate) fn root(db: &Database, chat: &str) -> io::Result<PathBuf> {
    let path = root_path(db, chat)?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&path)?;
    browser_client::private(&path)?;
    path.canonicalize()
}

/// The chat's private root, as [`root`] returns it, without creating it.
pub(crate) fn root_path(db: &Database, chat: &str) -> io::Result<PathBuf> {
    if !server::identifier(chat) {
        return Err(io::Error::other("invalid session"));
    }
    let connection = db
        .lock()
        .map_err(|_| io::Error::other("database unavailable"))?;
    let exists: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM ui_sessions WHERE id=?)",
            [chat],
            |row| row.get(0),
        )
        .map_err(io::Error::other)?;
    if !exists {
        return Err(io::Error::other("unknown session"));
    }
    let state = Path::new(
        connection
            .path()
            .ok_or_else(|| io::Error::other("persistent database required"))?,
    )
    .parent()
    .unwrap()
    .join("sessions");
    Ok(state.join(chat))
}

fn value(header: &str, name: &str) -> Option<String> {
    header
        .split(';')
        .skip(1)
        .filter_map(|part| part.trim().split_once('='))
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.trim().trim_matches('"').to_owned())
}

fn find(bytes: &[u8], pattern: &[u8]) -> Option<usize> {
    bytes
        .windows(pattern.len())
        .position(|window| window == pattern)
}

pub(crate) fn mime(name: &str) -> &'static str {
    match name
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "txt" | "md" | "csv" => "text/plain",
        "json" => "application/json",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        _ => "application/octet-stream",
    }
}

pub(crate) fn pinned(db: &Database, chat: &str, path: &str) -> io::Result<(PathBuf, Value)> {
    let owned = root(db, chat)?;
    let record = db
        .lock()
        .map_err(|_| io::Error::other("database unavailable"))?
        .query_row(
            "SELECT sha256,filename,mime FROM ui_uploads WHERE chat_id=? AND path=?",
            params![chat, path],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .map_err(io::Error::other)?;
    let Some(record) = record else {
        return crate::ui_legacy::pinned_file(db, chat, path);
    };
    if let Some(relative) = path.strip_prefix(WORKSPACE_PREFIX) {
        // A delegated worker's workspace is not under its chat root (`ui_workspace`).
        let root = crate::ui_workspace::existing_root(db, chat)?
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "upload file is unavailable"))?;
        return pinned_workspace(&root, relative, path, record);
    }
    let source = owned.join(path);
    let info = std::fs::symlink_metadata(&source)?;
    if !info.is_file() || info.file_type().is_symlink() || info.len() > MAX_BYTES as u64 {
        return Err(io::Error::other("upload file is unavailable"));
    }
    let canonical = source.canonicalize()?;
    if !canonical.starts_with(&owned) {
        return Err(io::Error::other("upload is outside this chat"));
    }
    let snapshot = kageha_workspace::file_snapshot(
        &canonical,
        Some(&record.0),
        &kageha_contracts::execution::Control {
            cancelled: &std::sync::atomic::AtomicBool::new(false),
            deadline: None,
        },
    )
    .map_err(|_| io::Error::other("upload bytes changed"))?;
    let bytes = snapshot.bytes;
    let hash = snapshot.sha256;
    Ok((
        canonical,
        json!({"path":path,"sha256":hash,"filename":record.1,"content_type":record.2,"bytes":bytes.len()}),
    ))
}

/// Chat-root relative prefix of Task_Workspace artifacts (see `ui_workspace`).
const WORKSPACE_PREFIX: &str = "workspace/";

/// Verifies a registered Task_Workspace artifact by streaming its bytes through SHA-256.
/// Workspace artifacts have no size limit (Requirement 3.6), so unlike uploads the file is
/// never buffered and the 16 MiB upload cap does not apply. `root` is the chat's workspace
/// root and `relative` the artifact `path` without its `workspace/` prefix.
fn pinned_workspace(
    root: &Path,
    relative: &str,
    path: &str,
    (sha256, filename, mime): (String, String, String),
) -> io::Result<(PathBuf, Value)> {
    use std::os::unix::fs::MetadataExt;
    let root = root.canonicalize()?;
    let source = root.join(relative);
    let info = std::fs::symlink_metadata(&source)?;
    if !info.is_file() || info.file_type().is_symlink() {
        return Err(io::Error::other("upload file is unavailable"));
    }
    let canonical = source.canonicalize()?;
    if !canonical.starts_with(&root) {
        return Err(io::Error::other("upload is outside this chat"));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&canonical)?;
    let before = file.metadata()?;
    if !before.is_file() {
        return Err(io::Error::other("upload file is unavailable"));
    }
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    let mut size = 0u64;
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        size += n as u64;
    }
    let after = file.metadata()?;
    let identity = |m: &std::fs::Metadata| (m.dev(), m.ino(), m.len(), m.mtime(), m.mtime_nsec());
    let hash = format!("{:x}", hasher.finalize());
    if identity(&before) != identity(&after) || size != after.len() || hash != sha256 {
        return Err(io::Error::other("upload bytes changed"));
    }
    Ok((
        canonical,
        json!({"path":path,"sha256":hash,"filename":filename,"content_type":mime,"bytes":size}),
    ))
}

pub(crate) fn read_upload(request: &mut Request) -> io::Result<(String, Vec<u8>)> {
    let content_type = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Content-Type"))
        .map(|h| h.value.as_str())
        .unwrap_or("")
        .to_owned();
    let boundary = value(&content_type, "boundary")
        .filter(|b| {
            !b.is_empty() && b.len() <= 70 && b.bytes().all(|byte| byte >= 33 && byte < 127)
        })
        .ok_or_else(|| io::Error::other("expected multipart/form-data with a valid boundary"))?;
    if !content_type
        .to_lowercase()
        .starts_with("multipart/form-data;")
    {
        return Err(io::Error::other("expected multipart/form-data"));
    }
    let mut body = Vec::new();
    request
        .as_reader()
        .take((MAX_BYTES + 16385) as u64)
        .read_to_end(&mut body)?;
    if body.len() > MAX_BYTES + 16384 {
        return Err(io::Error::new(
            io::ErrorKind::FileTooLarge,
            "Upload exceeds 16 MiB",
        ));
    }
    let (name, file_bytes) = multipart_file(&body, &boundary)?;
    let name = safe_filename(&name);
    let bytes = file_bytes.as_slice();
    if bytes.is_empty() || bytes.len() > MAX_BYTES {
        return Err(io::Error::other("Upload must contain 1 byte to 16 MiB"));
    }
    Ok((name, file_bytes))
}

pub(crate) fn safe_filename(name: &str) -> String {
    let name = name
        .replace('\\', "/")
        .rsplit('/')
        .next()
        .unwrap_or("upload.bin")
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .take(100)
        .collect::<String>();
    if name.trim_matches('.').is_empty() {
        "upload.bin".into()
    } else {
        name
    }
}

pub(crate) fn upload(request: &mut Request, db: &Database, chat: &str) -> io::Result<Value> {
    let (name, file_bytes) = read_upload(request)?;
    save(db, chat, &name, &file_bytes, None)
}

fn save(
    db: &Database,
    chat: &str,
    name: &str,
    bytes: &[u8],
    origin: Option<(&str, &str, bool)>,
) -> io::Result<Value> {
    if bytes.is_empty() || bytes.len() > MAX_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::FileTooLarge,
            "Attachment must contain 1 byte to 16 MiB",
        ));
    }
    let hash = format!("{:x}", Sha256::digest(bytes));
    let owned = root(db, chat)?;
    let inputs = owned.join("inputs");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&inputs)
        .or_else(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                Ok(())
            } else {
                Err(error)
            }
        })?;
    browser_client::private(&inputs)?;
    let mut connection = db
        .lock()
        .map_err(|_| io::Error::other("database unavailable"))?;
    let tx = connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(io::Error::other)?;
    if let Some((source_chat, source_path, agent)) = origin {
        if !visible(&tx, chat)? || !visible(&tx, source_chat)? {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "Source chat or destination chat is unavailable",
            ));
        }
        if agent && !agent_source(&tx, source_chat, &crate::ui_projects::projection(&tx, chat)?["project_id"])? {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "This file is outside the current chat's past-file search scope"));
        }
        let existing = tx
            .query_row(
                "SELECT u.path,u.filename,u.size FROM ui_reused_files r JOIN ui_uploads u
             ON u.chat_id=r.chat_id AND u.path=r.path WHERE r.chat_id=? AND r.sha256=?",
                params![chat, hash],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, usize>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(io::Error::other)?;
        if let Some((path, filename, size)) = existing {
            return Ok(upload_result(chat, &path, &filename, size));
        }
        let still_indexed: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM ui_uploads WHERE chat_id=?1 AND path=?2 AND sha256=?3
                 UNION ALL SELECT 1 FROM ui_legacy_files WHERE chat_id=?1 AND path=?2 AND sha256=?3)",
            params![source_chat, source_path, hash], |row| row.get(0),
        ).map_err(io::Error::other)?;
        if !still_indexed {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "Source attachment is unavailable",
            ));
        }
    }
    let relative = format!("inputs/{}-{}", browser_client::unique_id(), name);
    let destination = owned.join(&relative);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&destination)?;
    let saved = (|| -> io::Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        tx.execute(
            "INSERT INTO ui_uploads(chat_id,path,filename,mime,sha256,size) VALUES (?,?,?,?,?,?)",
            params![chat, relative, name, mime(name), hash, bytes.len()],
        )
        .map_err(io::Error::other)?;
        if let Some((source_chat, source_path, _)) = origin {
            tx.execute(
                "INSERT INTO ui_reused_files VALUES (?,?,?,?,?)",
                params![chat, hash, relative, source_chat, source_path],
            )
            .map_err(io::Error::other)?;
        }
        tx.commit().map_err(io::Error::other)
    })();
    if let Err(error) = saved {
        let _ = std::fs::remove_file(&destination);
        return Err(error);
    }
    Ok(upload_result(chat, &relative, name, bytes.len()))
}

fn upload_result(chat: &str, relative: &str, name: &str, size: usize) -> Value {
    let kind = if mime(&name).starts_with("image/") {
        "image"
    } else if mime(&name).starts_with("video/") {
        "video"
    } else if mime(&name).starts_with("audio/") {
        "audio"
    } else {
        "file"
    };
    json!({"session_id":chat,"path":relative,"filename":name,
            "bytes":size,"content_type":mime(name),"kind":kind})
}

pub(crate) fn read_failure(error: io::Error) -> io::Error {
    let (code, summary) = match error.to_string().as_str() {
        "File is not owned by this chat" => (
            "file_attachment_required",
            "Attach the file to this chat before reading it.",
        ),
        "expected a file inside the workspace" | "upload is outside this chat" => (
            "file_outside_workspace",
            "Select a file inside the current workspace or attach it to this chat.",
        ),
        "Legacy attachment changed after import" | "upload bytes changed" => (
            "file_changed",
            "The file differs from its saved copy. Attach the current version.",
        ),
        _ if error.kind() == io::ErrorKind::NotFound => (
            "file_missing",
            "The file is missing. Select an available file or attach it again.",
        ),
        _ => return error,
    };
    io::Error::new(
        error.kind(),
        json!({"error_code":code,"summary":summary,"retryable":false}).to_string(),
    )
}

fn visible(db: &rusqlite::Connection, chat: &str) -> io::Result<bool> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM ui_sessions WHERE id=?1
         AND NOT EXISTS(SELECT 1 FROM ui_internal_sessions WHERE id=?1))",
        [chat],
        |row| row.get(0),
    )
    .map_err(io::Error::other)
}

fn agent_source(db: &rusqlite::Connection, source: &str, project: &Value) -> io::Result<bool> {
    let persona = crate::ui_store::get(db, "session-persona", source).map_err(io::Error::other)?;
    Ok(persona.as_ref().and_then(|p| p["id"].as_str()).unwrap_or("main") == "main"
        && crate::ui_projects::projection(db, source)?["project_id"] == *project)
}

pub(crate) fn past_files(db: &Database, chat: &str, query: &str, agent: bool) -> io::Result<Value> {
    let mut result = search_files(db, chat, query, agent)?;
    if agent {
        for file in result["files"].as_array_mut().unwrap() {
            let sources = file["sources"].as_array().unwrap();
            *file = json!({"filename":file["filename"],"version":sources[0]["sha256"],
                "latest_chat":sources[0]["title"],"updated_at":sources[0]["updated_at"]});
        }
        result["next_step"] = if result["files"].as_array().unwrap().is_empty() {
            json!("Try another filename or ask the user to attach the file.")
        } else {
            json!("To bring a requested file into this chat, call attach_past_file with its filename and version. If different versions match and the choice is unclear, ask which one. If attachment is unavailable in this turn, ask the user to choose From past chats. Earlier chat directories are private.")
        };
    }
    Ok(result)
}

fn search_files(db: &Database, chat: &str, query: &str, agent: bool) -> io::Result<Value> {
    let query = query.trim().to_lowercase();
    if query.len() > 256 || (agent && query.is_empty()) {
        return Err(io::Error::other("Provide a filename or part of a filename"));
    }
    let db = db
        .lock()
        .map_err(|_| io::Error::other("database unavailable"))?;
    if !visible(&db, chat)? {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "Chat is unavailable",
        ));
    }
    let project = crate::ui_projects::projection(&db, chat)?["project_id"].clone();
    let mut stmt = db
        .prepare(
            "WITH files AS (
             SELECT chat_id,path,filename,sha256 FROM ui_uploads
                WHERE path LIKE 'inputs/%' OR path LIKE 'artifacts/%'
             UNION ALL SELECT chat_id,path,NULL,sha256 FROM ui_legacy_files
                WHERE path LIKE 'inputs/%' OR path LIKE 'artifacts/%'
         )
         SELECT chat_id,path,filename,sha256,title,updated_at
         FROM files JOIN ui_sessions ON id=chat_id
         WHERE chat_id<>?1 AND instr(lower(coalesce(filename,path)),?2)>0
             AND NOT EXISTS(SELECT 1 FROM ui_internal_sessions WHERE id=chat_id)
         ORDER BY updated_at DESC, chat_id, path LIMIT 501",
        )
        .map_err(io::Error::other)?;
    let rows = stmt
        .query_map(params![chat, query], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, f64>(5)?,
            ))
        })
        .map_err(io::Error::other)?;
    let mut groups = Vec::<Value>::new();
    let mut index = std::collections::HashMap::new();
    let mut truncated = false;
    let mut eligible = std::collections::HashMap::new();
    for (offset, row) in rows.enumerate() {
        if offset == 500 {
            truncated = true;
            break;
        }
        let (id, path, filename, hash, title, date) = row.map_err(io::Error::other)?;
        if agent {
            let allowed = match eligible.entry(id.clone()) {
                std::collections::hash_map::Entry::Occupied(value) => *value.get(),
                std::collections::hash_map::Entry::Vacant(entry) => *entry.insert(agent_source(&db, &id, &project)?),
            };
            if !allowed { continue; }
        }
        let filename = filename
            .as_deref()
            .unwrap_or_else(|| path.rsplit('/').next().unwrap_or(&path));
        let source = json!({"chat_id":id,"title":title,"updated_at":date,"path":path,"filename":filename,"sha256":hash});
        if let Some(&position) = index.get(&hash) {
            let group: &mut Value = &mut groups[position];
            group["sources"].as_array_mut().unwrap().push(source);
        } else if groups.len() < 30 {
            index.insert(hash, groups.len());
            groups.push(json!({"filename":filename,"sources":[source]}));
        } else {
            truncated = true;
        }
    }
    Ok(json!({"files":groups,"truncated":truncated}))
}

pub(crate) fn attach_past_file(db: &Database, chat: &str, filename: &str, version: &str) -> io::Result<Value> {
    if version.len() != 64 || !version.bytes().all(|v| v.is_ascii_hexdigit()) {
        return Err(io::Error::other("Use the exact version returned by find_past_files"));
    }
    let results = search_files(db, chat, filename, true)?;
    let group = results["files"].as_array().unwrap().iter().find(|file|
        file["filename"] == filename && file["sources"][0]["sha256"] == version)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "This version is unavailable in the current project. Search again or use From past chats."))?;
    for source in group["sources"].as_array().unwrap() {
        match reuse(db, chat, source, true) {
            Ok(mut attachment) => {
                let path = attachment["path"].as_str().unwrap();
                pinned(db, chat, path).map_err(read_failure)?;
                if let Some(placed) = crate::ui_workspace::place_inputs(db, chat)?
                    .into_iter().find(|input| input.key == path && input.sha256 == version) {
                    attachment["workspace_path"] = json!(placed.path);
                }
                return Ok(attachment);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(io::ErrorKind::NotFound, "The indexed copies are missing. Ask the user to attach the file."))
}

fn reuse(db: &Database, chat: &str, payload: &Value, agent: bool) -> io::Result<Value> {
    let source = payload["chat_id"]
        .as_str()
        .filter(|id| *id != chat && server::identifier(id))
        .ok_or_else(|| io::Error::other("Select a source chat"))?;
    let path = payload["path"]
        .as_str()
        .filter(|path| path.starts_with("inputs/") || path.starts_with("artifacts/"))
        .ok_or_else(|| io::Error::other("Select a source attachment"))?;
    {
        let connection = db
            .lock()
            .map_err(|_| io::Error::other("database unavailable"))?;
        if !visible(&connection, chat)? || !visible(&connection, source)? {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "Source chat or destination chat is unavailable",
            ));
        }
    }
    let (file, metadata) = pinned(db, source, path).map_err(read_failure)?;
    if payload["sha256"].as_str() != metadata["sha256"].as_str() {
        return Err(io::Error::other(
            "The source version changed. Search again and select the current file.",
        ));
    }
    let snapshot = kageha_workspace::file_snapshot(
        &file,
        metadata["sha256"].as_str(),
        &kageha_contracts::execution::Control {
            cancelled: &std::sync::atomic::AtomicBool::new(false),
            deadline: None,
        },
    )?;
    let name = safe_filename(metadata["filename"].as_str().unwrap_or("attachment"));
    save(db, chat, &name, &snapshot.bytes, Some((source, path, agent)))
}

pub(crate) fn handle(
    mut request: Request,
    db: &Database,
    chat: &str,
    operation: &str,
) -> io::Result<()> {
    if operation == "files/from-past-chats" {
        let result = match *request.method() {
            Method::Get => crate::webui::form_decode(&crate::webui::query(request.url(), "q"))
                .ok_or_else(|| io::Error::other("Invalid filename query"))
                .and_then(|query| past_files(db, chat, &query, false)),
            Method::Post => crate::webui::body(&mut request)
                .and_then(|payload| reuse(db, chat, &payload, false))
                .inspect(|_| crate::ui_workspace::place_new_inputs(db, chat)),
            _ => {
                server::response(
                    request,
                    405,
                    json!({"error":"Use GET to search or POST to attach"}),
                );
                return Ok(());
            }
        };
        match result {
            Ok(value) => server::response(request, 200, value),
            Err(error) => {
                let status = match error.kind() {
                    io::ErrorKind::NotFound => 404,
                    io::ErrorKind::FileTooLarge => 413,
                    _ => 400,
                };
                server::response(request, status, json!({"error":error.to_string()}));
            }
        }
        return Ok(());
    }
    if operation == "upload" && request.method() == &Method::Post {
        match upload(&mut request, db, chat) {
            Ok(value) => {
                // A running turn can read it in `workspace/inputs/` (Requirement 20.1).
                crate::ui_workspace::place_new_inputs(db, chat);
                server::response(request, 200, value)
            }
            Err(error) => server::response(
                request,
                if error.kind() == io::ErrorKind::FileTooLarge {
                    413
                } else {
                    400
                },
                json!({"error":error.to_string()}),
            ),
        }
        return Ok(());
    }
    if let Some(path) = operation.strip_prefix("files/") {
        if request.method() != &Method::Get {
            return Err(io::Error::other("file reads require GET"));
        }
        let (path, metadata) = match pinned(db, chat, path) {
            Ok(file) => file,
            Err(_) => {
                server::response(request, 404, json!({"error":"Upload is unavailable"}));
                return Ok(());
            }
        };
        let response = Response::from_file(std::fs::File::open(path)?)
            .with_header(
                Header::from_bytes("Content-Type", metadata["content_type"].as_str().unwrap())
                    .unwrap(),
            )
            .with_header(Header::from_bytes("X-Content-Type-Options", "nosniff").unwrap());
        let _ = request.respond(response);
        return Ok(());
    }
    Err(io::Error::other("unsupported upload operation"))
}

fn multipart_file(body: &[u8], boundary: &str) -> io::Result<(String, Vec<u8>)> {
    let opening = format!("--{boundary}\r\n");
    let delimiter = format!("\r\n--{boundary}");
    if !body.starts_with(opening.as_bytes()) {
        return Err(io::Error::other("Invalid multipart opening"));
    }
    let mut at = opening.len();
    let mut file = None;
    for _ in 0..16 {
        let end = find(&body[at..], b"\r\n\r\n")
            .filter(|n| *n < 8192)
            .ok_or_else(|| io::Error::other("Invalid multipart headers"))?
            + at;
        let headers = std::str::from_utf8(&body[at..end]).map_err(io::Error::other)?;
        let disposition = headers
            .lines()
            .find(|h| h.to_ascii_lowercase().starts_with("content-disposition:"))
            .ok_or_else(|| io::Error::other("Multipart disposition missing"))?;
        let start = end + 4;
        let stop = find(&body[start..], delimiter.as_bytes())
            .ok_or_else(|| io::Error::other("Incomplete multipart file"))?
            + start;
        if let Some(name) = value(disposition, "filename") {
            if file.is_some() {
                return Err(io::Error::other("Upload one file per request"));
            }
            file = Some((name, body[start..stop].to_vec()));
        } else if stop - start > 4096 {
            return Err(io::Error::other("Multipart field exceeds limit"));
        }
        at = stop + delimiter.len();
        if body[at..].starts_with(b"--") {
            let tail = &body[at + 2..];
            if !tail.is_empty() && tail != b"\r\n" {
                return Err(io::Error::other("Invalid multipart trailer"));
            }
            return file.ok_or_else(|| io::Error::other("Multipart file missing"));
        }
        if !body[at..].starts_with(b"\r\n") {
            return Err(io::Error::other("Invalid multipart boundary"));
        }
        at += 2;
    }
    Err(io::Error::other("Too many multipart fields"))
}
#[cfg(test)]
mod multipart_tests {
    use super::*;
    #[test]
    fn past_file_reuse_preserves_scope_versions_and_content() {
        let directory = tempfile::tempdir().unwrap();
        let db = crate::agent_host::DatabaseState::open(
            rusqlite::Connection::open(directory.path().join("chats.sqlite")).unwrap(),
        )
        .unwrap();
        {
            let connection = db.lock().unwrap();
            initialize(&connection).unwrap();
            crate::ui_legacy::initialize(&connection).unwrap();
            connection
                .execute_batch(
                    "CREATE TABLE ui_sessions(id TEXT PRIMARY KEY,title TEXT,created_at REAL,updated_at REAL);
                 INSERT INTO ui_sessions(id,title,created_at,updated_at) VALUES
                 ('source','Source',1,2),('other','Other',1,3),('target','Target',1,4),
                 ('hidden','Hidden',1,5);
                 INSERT INTO ui_internal_sessions(id) VALUES ('hidden');",
                )
                .unwrap();
        }
        let original = save(&db, "source", "resume.txt", b"first", None).unwrap();
        let duplicate = save(&db, "other", "resume.txt", b"first", None).unwrap();
        let changed = save(&db, "other", "resume.txt", b"second", None).unwrap();
        let hidden = save(&db, "hidden", "resume.txt", b"hidden", None).unwrap();
        let found = past_files(&db, "target", "resume", false).unwrap();
        assert_eq!(found["files"].as_array().unwrap().len(), 2);
        assert!(found["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|file| file["sources"].as_array().unwrap().len() == 2));
        let matches = past_files(&db, "target", "resume", true).unwrap();
        let versions = matches["files"].as_array().unwrap();
        assert_eq!(versions.len(), 2);
        assert!(versions.iter().all(|v| v["sources"].is_null() && v["version"].as_str().unwrap().len() == 64));
        let version = format!("{:x}", Sha256::digest(b"first"));
        let matching = versions.iter().find(|v| v["version"] == version).unwrap();
        // The most recent source disappeared after indexing. Reuse the
        // identical checked copy, without substituting the other version.
        let (missing, _) = pinned(&db, "other", duplicate["path"].as_str().unwrap()).unwrap();
        std::fs::remove_file(missing).unwrap();
        let attached_by_agent = attach_past_file(&db, "target", matching["filename"].as_str().unwrap(), &version).unwrap();
        assert_eq!(attach_past_file(&db, "target", "resume.txt", &version).unwrap(), attached_by_agent);
        assert!(attach_past_file(&db, "target", "resume.txt", &format!("{:x}", Sha256::digest(b"hidden"))).is_err());
        assert!(attach_past_file(&db, "target", "resume.txt", "../source").is_err());
        let path = original["path"].as_str().unwrap();
        assert!(pinned(&db, "target", path).is_err());
        assert!(reuse(
            &db,
            "target",
            &json!({"chat_id":"hidden","path":hidden["path"]}), false
        )
        .is_err());
        let selection = json!({"chat_id":"source","path":path,"sha256":format!("{:x}", Sha256::digest(b"first"))});
        let attached = reuse(&db, "target", &selection, false).unwrap();
        assert_eq!(reuse(&db, "target", &selection, false).unwrap(), attached);
        let (copy, _) = pinned(&db, "target", attached["path"].as_str().unwrap()).unwrap();
        let (source, _) = pinned(&db, "source", path).unwrap();
        assert_ne!(copy, source);
        assert_eq!(std::fs::read(&source).unwrap(), b"first");
        assert_eq!(std::fs::read(&copy).unwrap(), b"first");
        let (modified, _) = pinned(&db, "other", changed["path"].as_str().unwrap()).unwrap();
        let selection = json!({"chat_id":"other","path":changed["path"],"sha256":format!("{:x}", Sha256::digest(b"second"))});
        std::fs::write(&modified, b"edited").unwrap();
        assert!(reuse(&db, "target", &selection, false).is_err());
        std::fs::remove_file(modified).unwrap();
        assert!(reuse(&db, "target", &selection, false).is_err());
        {
            let connection = db.lock().unwrap();
            crate::ui_store::put(&connection, "session-persona", "other", json!({"id":"specialist"})).unwrap();
            crate::ui_projects::assign(&connection, "source", "Other project").unwrap();
        }
        assert!(past_files(&db, "target", "resume", true).unwrap()["files"].as_array().unwrap().is_empty());
        assert!(attach_past_file(&db, "target", "resume.txt", &version).is_err());

    }

    #[test]
    fn fields_do_not_enter_the_file() {
        let body=b"--b\r\nContent-Disposition: form-data; name=\"language\"\r\n\r\nen\r\n--b\r\nContent-Disposition: form-data; name=\"audio\"; filename=\"clip.wav\"\r\n\r\nWAV\r\n--b--\r\n";
        assert_eq!(
            multipart_file(body, "b").unwrap(),
            ("clip.wav".into(), b"WAV".to_vec())
        );
        assert!(multipart_file(&body[..body.len() - 5], "b").is_err());
    }
}
