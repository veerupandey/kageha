//! Portable conversation snapshots for the thread menu.
use crate::{
    server,
    ui_store::{self, db_error, ApiResult},
    webui::{self, Database},
};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
};
use tiny_http::{Header, Method, Request, Response};
use zip::{write::SimpleFileOptions, ZipArchive, ZipWriter};

const MAX_ARCHIVE_BYTES: u64 = 500 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024 * 1024;
const MAX_FILES: usize = 10_000;
const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct Manifest {
    version: u32,
    title: Option<String>,
    messages: Vec<Message>,
    files: Vec<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Role {
    User,
    Assistant,
}

#[derive(Serialize, Deserialize)]
struct Message {
    role: Role,
    text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    speaker: Option<Speaker>,
    attachments: Vec<usize>,
}

#[derive(Serialize, Deserialize)]
struct Speaker {
    id: Option<String>,
    name: Option<String>,
    avatar: Option<String>,
}

fn too_large() -> (u16, String) {
    (
        413,
        "Session archive exceeds the transfer size limit".into(),
    )
}

fn invalid_archive() -> (u16, String) {
    (400, "Invalid Kageha session archive".into())
}

// Use the existing attachment ownership and hash checks. Recheck the actual
// bytes we copy because the file could change after it was verified.
fn attachment(db: &Database, chat: &str, path: &str) -> Option<(String, Vec<u8>)> {
    let (source, metadata) = crate::ui_uploads::pinned(db, chat, path).ok()?;
    let limit = crate::ui_uploads::MAX_BYTES as u64;
    let expected_size = metadata["bytes"].as_u64().filter(|bytes| *bytes <= limit)?;
    let mut bytes = Vec::new();
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(source)
        .ok()?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 != expected_size
        || format!("{:x}", Sha256::digest(&bytes)) != metadata["sha256"].as_str()?
    {
        return None;
    }
    let name = crate::ui_uploads::safe_filename(metadata["filename"].as_str()?);
    Some((name, bytes))
}

fn archive(db: &Database, chat: &str, session: &Value) -> ApiResult<(File, usize)> {
    let mut zip = ZipWriter::new(tempfile::tempfile().map_err(db_error)?);
    let mut manifest = Manifest {
        version: VERSION,
        title: session["title"].as_str().map(str::to_owned),
        messages: Vec::new(),
        files: Vec::new(),
    };
    let mut files = HashMap::new();
    let mut unavailable = 0;
    let mut size = 0;
    let mut text_size = 0;
    for row in session["messages"].as_array().into_iter().flatten() {
        let role = match row["role"].as_str() {
            Some("user") => Role::User,
            Some("assistant") => Role::Assistant,
            _ => continue,
        };
        let Some(text) = row["text"].as_str() else {
            continue;
        };
        if row["steering"] == "pending" {
            continue;
        }
        text_size += text.len() as u64;
        if text_size > MAX_MANIFEST_BYTES {
            return Err(too_large());
        }
        let mut attachments = Vec::new();
        for path in row["attachments"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            let index = if let Some(index) = files.get(path) {
                *index
            } else {
                let index = if let Some((name, bytes)) = attachment(db, chat, path) {
                    size += bytes.len() as u64;
                    if size > MAX_ARCHIVE_BYTES || manifest.files.len() == MAX_FILES {
                        return Err(too_large());
                    }
                    let index = manifest.files.len();
                    zip.start_file(format!("files/{index}"), SimpleFileOptions::default())
                        .map_err(db_error)?;
                    zip.write_all(&bytes).map_err(db_error)?;
                    manifest.files.push(name);
                    Some(index)
                } else {
                    unavailable += 1;
                    None
                };
                files.insert(path, index);
                index
            };
            if let Some(index) = index {
                attachments.push(index);
            }
        }
        manifest.messages.push(Message {
            role,
            text: text.to_owned(),
            speaker: serde_json::from_value(row["speaker"].clone()).ok(),
            attachments,
        });
    }
    let manifest = serde_json::to_vec(&manifest).map_err(db_error)?;
    if manifest.len() as u64 > MAX_MANIFEST_BYTES
        || size + manifest.len() as u64 > MAX_ARCHIVE_BYTES
    {
        return Err(too_large());
    }
    zip.start_file("session.json", SimpleFileOptions::default())
        .map_err(db_error)?;
    zip.write_all(&manifest).map_err(db_error)?;
    let mut file = zip.finish().map_err(db_error)?;
    if file.metadata().map_err(db_error)?.len() > MAX_ARCHIVE_BYTES {
        return Err(too_large());
    }
    file.rewind().map_err(db_error)?;
    Ok((file, unavailable))
}

fn files_root(db: &Connection) -> io::Result<PathBuf> {
    db.path()
        .and_then(|path| Path::new(path).parent())
        .map(|state| state.join("portable-session-files"))
        .ok_or_else(|| io::Error::other("Session transfers require a state directory"))
}

pub(crate) fn erase_files(db: &Connection, chat: &str) {
    if let Ok(root) = files_root(db) {
        let _ = fs::remove_dir_all(root.join(chat));
    }
}

fn read_package(db: &Database, file: File, fork: Option<&Value>) -> ApiResult<String> {
    let mut zip = ZipArchive::new(file).map_err(|_| invalid_archive())?;
    if zip.len() > MAX_FILES + 1 {
        return Err(too_large());
    }
    let mut metadata = Vec::new();
    zip.by_name("session.json")
        .map_err(|_| invalid_archive())?
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut metadata)
        .map_err(|_| invalid_archive())?;
    if metadata.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(too_large());
    }
    let manifest: Manifest = serde_json::from_slice(&metadata).map_err(|_| invalid_archive())?;
    if manifest.version != VERSION {
        return Err((400, "Unsupported session archive version".into()));
    }
    if zip.len() != manifest.files.len() + 1
        || manifest
            .files
            .iter()
            .any(|name| name != &crate::ui_uploads::safe_filename(name))
        || manifest
            .messages
            .iter()
            .flat_map(|m| &m.attachments)
            .any(|index| *index >= manifest.files.len())
    {
        return Err(invalid_archive());
    }

    let root = files_root(&*db.lock().map_err(db_error)?).map_err(db_error)?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&root)
        .map_err(db_error)?;
    crate::browser_client::private(&root).map_err(db_error)?;
    let staged = tempfile::Builder::new()
        .prefix("chat-")
        .rand_bytes(24)
        .tempdir_in(root)
        .map_err(db_error)?;
    let id = staged
        .path()
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(invalid_archive)?
        .to_owned();
    let mut total = metadata.len() as u64;
    let mut attachments = Vec::with_capacity(manifest.files.len());
    for (index, name) in manifest.files.iter().enumerate() {
        let entry = zip
            .by_name(&format!("files/{index}"))
            .map_err(|_| invalid_archive())?;
        let size = entry.size();
        let limit = (MAX_ARCHIVE_BYTES - total).min(crate::ui_uploads::MAX_BYTES as u64);
        if size > limit {
            return Err(too_large());
        }
        let mut bytes = Vec::new();
        entry
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| invalid_archive())?;
        if bytes.len() as u64 > limit {
            return Err(too_large());
        }
        if bytes.len() as u64 != size {
            return Err(invalid_archive());
        }
        total += size;
        let relative = format!("transcript-files/{index}/{name}");
        // The ZIP entry is never used as a filesystem path.
        let owned = staged.path().join(index.to_string());
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&owned)
            .map_err(db_error)?;
        let owned = owned.join(name);
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&owned)
            .map_err(db_error)?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(db_error)?;
        attachments.push((relative, owned, format!("{:x}", Sha256::digest(&bytes))));
    }

    let title = manifest
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty());
    let title = if fork.is_some() {
        Some(format!(
            "{} (fork)",
            title
                .unwrap_or("Thread")
                .chars()
                .take(193)
                .collect::<String>()
        ))
    } else {
        title.map(|title| title.chars().take(200).collect())
    };
    let persona = if let Some(persona) = fork.and_then(|source| source["persona_id"].as_str()) {
        if crate::ui_agents::agent(db, persona).is_ok() {
            persona
        } else {
            "main"
        }
    } else {
        "main"
    };
    let mut conn = db.lock().map_err(db_error)?;
    let tx = conn.transaction().map_err(db_error)?;
    let now = ui_store::now();
    tx.execute(
        "INSERT INTO ui_sessions(id,title,created_at,updated_at) VALUES (?,?,?,?)",
        params![id, title, now, now],
    )
    .map_err(db_error)?;
    ui_store::put(&tx, "session-persona", &id, json!({"id":persona})).map_err(db_error)?;
    for (ordinal, message) in manifest.messages.iter().enumerate() {
        let row = json!({
            "role": message.role, "text": message.text, "speaker": message.speaker,
            "attachments": message.attachments.iter().map(|index| &attachments[*index].0).collect::<Vec<_>>(),
        });
        tx.execute(
            "INSERT INTO ui_legacy_messages(chat_id,ordinal,record) VALUES (?,?,?)",
            params![id, ordinal, row.to_string()],
        )
        .map_err(db_error)?;
    }
    for (relative, source, hash) in &attachments {
        tx.execute(
            "INSERT INTO ui_legacy_files(chat_id,path,source,sha256) VALUES (?,?,?,?)",
            params![id, relative, source.to_string_lossy(), hash],
        )
        .map_err(db_error)?;
    }
    let project = fork.and_then(|source| source["project_id"].as_str());
    if let Some(project) = project {
        crate::ui_projects::assign(&tx, &id, project).map_err(db_error)?;
    }
    if let Err(error) = tx.commit() {
        if project.is_some() {
            let _ = crate::ui_projects::erase(&conn, &id);
        }
        return Err(db_error(error));
    }
    if !attachments.is_empty() {
        let _ = staged.keep();
    }
    Ok(id)
}

pub(crate) fn copy(request: Request, db: &Database, chat: &str, operation: &str) -> io::Result<()> {
    let fork = operation == "fork";
    let method = if fork { Method::Post } else { Method::Get };
    let result = (|| -> ApiResult<_> {
        if request.method() != &method {
            return Err((405, format!("Use {method} to {operation} a session")));
        }
        let session = webui::session_for_transfer(&*db.lock().map_err(db_error)?, chat)?;
        let (archive, unavailable) = archive(db, chat, &session)?;
        let response = if fork {
            let id = read_package(db, archive, Some(&session))?;
            Response::from_data(
                json!({"session_id":id,"unavailable_attachments":unavailable})
                    .to_string()
                    .into_bytes(),
            )
            .with_header(Header::from_bytes("Content-Type", "application/json").unwrap())
            .boxed()
        } else {
            Response::from_file(archive)
                .with_header(Header::from_bytes("Content-Type", "application/zip").unwrap())
                .with_header(
                    Header::from_bytes(
                        "Content-Disposition",
                        format!("attachment; filename=\"kageha-session-{chat}.zip\""),
                    )
                    .unwrap(),
                )
                .with_header(
                    Header::from_bytes("X-Kageha-Unavailable-Attachments", unavailable.to_string())
                        .unwrap(),
                )
                .boxed()
        };
        Ok(response.with_header(Header::from_bytes("Cache-Control", "no-store").unwrap()))
    })();
    match result {
        Ok(response) => request.respond(response),
        Err((status, error)) => {
            server::response(request, status, json!({"error":error}));
            Ok(())
        }
    }
}

pub(crate) fn import(mut request: Request, db: &Database) -> io::Result<()> {
    let result = (|| -> ApiResult<_> {
        if request.method() != &Method::Post {
            return Err((405, "Use POST to import a session".into()));
        }
        if request
            .body_length()
            .is_some_and(|len| len as u64 > MAX_ARCHIVE_BYTES)
        {
            return Err(too_large());
        }
        let mut file = tempfile::tempfile().map_err(db_error)?;
        let bytes = io::copy(
            &mut request.as_reader().take(MAX_ARCHIVE_BYTES + 1),
            &mut file,
        )
        .map_err(|_| invalid_archive())?;
        if bytes > MAX_ARCHIVE_BYTES {
            return Err(too_large());
        }
        file.rewind().map_err(db_error)?;
        read_package(db, file, None)
    })();
    match result {
        Ok(id) => server::response(request, 200, json!({"session_id":id})),
        Err((status, error)) => server::response(request, status, json!({"error":error})),
    }
    Ok(())
}
