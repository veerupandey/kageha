//! Read-only Task_Workspace routes for the Activity panel (design section 16, task 27.3,
//! Requirements 3.8 and 18.5). The token check is `server::handle`'s, as for every route.
//!
//! - `GET /api/sessions/<id>/workspace/tree`: the files the workspace manifest records,
//!   `{path, size, sha256, artifact_path}`. Host state (`.kageha/`) is excluded, and so
//!   are delegated workers' nested workspaces, whose files are the workers' own (their
//!   manifest is separate). The route only reads the manifest: scanning the disk here
//!   would record changes the turn has not registered as artifacts yet.
//! - `GET /api/sessions/<id>/workspace/file?path=<rel>[&download=1]`: one workspace file,
//!   streamed without a size cap. The path is confined with [`TaskWorkspace::resolve`],
//!   and `.kageha/` and nested worker workspaces are refused.
//! - `GET /api/sessions/<id>/workspace/plan`: `todo.md` ([`ui_workspace::read_plan`])
//!   with its SHA-256, so the panel can tell when the plan changed.
//!
//! None of the routes create a workspace for a chat that never used one.
use crate::{server, ui_uploads, ui_workspace, webui::Database};
use kageha_workspace::task::{is_nested_root, TaskWorkspace, WorkspaceError, AGENTS_DIR, STATE_DIR, WORKSPACE_DIR};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};
use tiny_http::{Header, Method, Request, Response};

/// Bound on the listing (entries, not file sizes), so a workspace holding a dependency
/// tree can't stall the panel.
const TREE_MAX_FILES: usize = 5000;

/// The operations this module serves under `/api/sessions/<id>/workspace/`.
pub(crate) fn handles(operation: &str) -> bool {
    matches!(operation, "tree" | "file" | "plan")
}

pub(crate) fn handle(request: Request, db: &Database, chat: &str, operation: &str) -> io::Result<()> {
    if request.method() != &Method::Get {
        server::response(request, 405, json!({"error":"GET required"}));
        return Ok(());
    }
    let result = match operation {
        "tree" => tree(db, chat),
        "plan" => plan(db, chat),
        "file" => return file(request, db, chat),
        _ => Err((404, "route_not_found".into())),
    };
    match result {
        Ok(value) => server::response(request, 200, value),
        Err((status, error)) => server::response(request, status, json!({"error":error})),
    }
    Ok(())
}

type Reply<T> = Result<T, (u16, String)>;

/// Maps a lookup failure to a status: unknown chats and missing files are 404, paths
/// outside the workspace or reserved for the Host are 403, invalid paths 400.
fn failure(error: io::Error) -> (u16, String) {
    let message = error.to_string();
    let status = match error.kind() {
        io::ErrorKind::NotFound => 404,
        io::ErrorKind::PermissionDenied => 403,
        io::ErrorKind::InvalidInput => 400,
        _ if message == "unknown session" || message == "invalid session" => 404,
        _ => 500,
    };
    (status, message)
}

/// The chat's workspace, or `None` when it has none yet (nothing is created).
fn existing(db: &Database, chat: &str) -> Reply<Option<TaskWorkspace>> {
    if ui_workspace::existing_root(db, chat).map_err(failure)?.is_none() {
        return Ok(None);
    }
    ui_workspace::open_for_chat(db, chat).map(Some).map_err(failure)
}

fn tree(db: &Database, chat: &str) -> Reply<Value> {
    let Some(workspace) = existing(db, chat)? else {
        return Ok(json!({"root":WORKSPACE_DIR,"exists":false,"files":[],"truncated":false}));
    };
    let entries = workspace.manifest_entries().map_err(|error| failure(error.into()))?;
    let mut files = Vec::new();
    let mut truncated = false;
    for entry in entries {
        // `artifact_path` is `None` for `.kageha/` and non-canonical paths.
        let Some(artifact) = ui_workspace::artifact_path(&entry.path) else { continue };
        if in_nested_worker(&workspace, Path::new(&entry.path))
            || kageha_workspace::task::is_dependency_path(&entry.path)
        {
            continue;
        }
        if files.len() >= TREE_MAX_FILES {
            truncated = true;
            break;
        }
        files.push(json!({"path":entry.path,"size":entry.size,"sha256":entry.sha256,"artifact_path":artifact}));
    }
    Ok(json!({"root":WORKSPACE_DIR,"exists":true,"files":files,"truncated":truncated}))
}

fn plan(db: &Database, chat: &str) -> Reply<Value> {
    use kageha_contracts::execution::PLAN_PATH;
    let content = ui_workspace::read_plan(db, chat).map_err(failure)?;
    let sha256 = content.as_ref().map(|text| format!("{:x}", Sha256::digest(text.as_bytes())));
    Ok(json!({"path":PLAN_PATH,"exists":content.is_some(),"content":content.unwrap_or_default(),"sha256":sha256}))
}

/// Whether `relative` (to the root) is inside a delegated worker's nested workspace.
fn in_nested_worker(workspace: &TaskWorkspace, relative: &Path) -> bool {
    let mut parts = relative.components();
    match (parts.next(), parts.next()) {
        (Some(Component::Normal(agents)), Some(Component::Normal(name))) if agents == AGENTS_DIR => {
            workspace.state_inside() && is_nested_root(&workspace.root().join(AGENTS_DIR).join(name))
        }
        _ => false,
    }
}

/// Resolves a user-supplied workspace path for reading. The path is confined to the root
/// (no `..` escape, absolute path or outside symlink), and the Host's `.kageha/` and the
/// nested workspaces of delegated workers are refused.
fn resolve(workspace: &TaskWorkspace, rel: &str) -> Result<PathBuf, WorkspaceError> {
    let reserved = || WorkspaceError::ReservedPath { path: rel.into() };
    let resolved = workspace.resolve(rel)?;
    let relative = resolved
        .strip_prefix(workspace.root())
        .map_err(|_| WorkspaceError::PathOutsideWorkspace { path: rel.into() })?;
    if relative.components().next() == Some(Component::Normal(STATE_DIR.as_ref())) {
        return Err(reserved());
    }
    // A workspace with external state keeps it outside the root; refuse it all the same.
    if resolved.starts_with(workspace.state_dir()) || in_nested_worker(workspace, relative) {
        return Err(reserved());
    }
    Ok(resolved)
}

/// `Content-Disposition` with an ASCII fallback name and the RFC 5987 UTF-8 name.
fn disposition(download: bool, name: &str) -> String {
    let fallback: String = name
        .chars()
        .map(|c| if (c.is_ascii_graphic() && c != '"' && c != '\\') || c == ' ' { c } else { '_' })
        .collect();
    let encoded: String = name
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'_' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect();
    let kind = if download { "attachment" } else { "inline" };
    format!("{kind}; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}

fn file(request: Request, db: &Database, chat: &str) -> io::Result<()> {
    let url = request.url().to_owned();
    let refuse = |request: Request, status: u16, error: &str| {
        server::response(request, status, json!({"error":error}));
        Ok(())
    };
    let Some(rel) = crate::webui::form_decode(&crate::webui::query(&url, "path")) else {
        return refuse(request, 400, "Invalid path");
    };
    // The artifact form `workspace/<path>` names the same file.
    let rel = crate::ui_tools::workspace_relative(&rel).to_owned();
    if rel.is_empty() {
        return refuse(request, 400, "path required");
    }
    let download = matches!(crate::webui::query(&url, "download").as_str(), "1" | "true");
    let workspace = match existing(db, chat) {
        Ok(Some(workspace)) => workspace,
        Ok(None) => return refuse(request, 404, "File not found"),
        Err((status, error)) => return refuse(request, status, &error),
    };
    let path = match resolve(&workspace, &rel) {
        Ok(path) => path,
        Err(error) => {
            let status = match &error {
                WorkspaceError::PathOutsideWorkspace { .. } | WorkspaceError::ReservedPath { .. } => 403,
                WorkspaceError::InvalidPath { .. } => 400,
                _ => 404,
            };
            return refuse(request, status, error.code());
        }
    };
    let opened = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(&path);
    let file = match opened.and_then(|file| file.metadata().map(|meta| (file, meta))) {
        Ok((file, meta)) if meta.is_file() => (file, meta.len()),
        _ => return refuse(request, 404, "File not found"),
    };
    let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    // `mime` names no active types (HTML, SVG, XML), so an opened file can't run script
    // in the app's origin; `nosniff` keeps the browser from guessing one.
    let mime = match ui_uploads::mime(&name) {
        "text/plain" => "text/plain; charset=utf-8",
        mime => mime,
    };
    let header = |name: &str, value: &str| Header::from_bytes(name, value).expect("valid header");
    // Streamed from the open file with its length; no size cap (Requirement 3.6).
    let response = Response::new(
        200.into(),
        vec![
            header("Content-Type", mime),
            header("Content-Disposition", &disposition(download, &name)),
            header("X-Content-Type-Options", "nosniff"),
            header("Cache-Control", "no-store"),
        ],
        file.0,
        usize::try_from(file.1).ok(),
        None,
    );
    let _ = request.respond(response);
    Ok(())
}

#[cfg(test)]
#[path = "ui_workspace_api_tests.rs"]
mod tests;
