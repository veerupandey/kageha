//! Terminal and file panel for the same workspace used by a Docker-bound turn.
use std::{io, path::Path, sync::atomic::AtomicBool};

use serde_json::{Value, json};
use tiny_http::{Header, Method, Request, Response};

use crate::{
    docker_desktop, ui_store,
    webui::{self, Database},
};

const TERMINAL: &str = "machine-terminal";
const MAX_ENTRIES: usize = 200;

fn entries(root: &Path, parent: &Path, depth: u8, result: &mut Vec<Value>) -> io::Result<()> {
    if depth == 4 {
        return Ok(());
    }
    let mut children = std::fs::read_dir(root.join(parent))?
        .take(MAX_ENTRIES)
        .collect::<Result<Vec<_>, _>>()?;
    children.sort_unstable_by_key(|entry| entry.file_name());
    for entry in children {
        if result.len() == MAX_ENTRIES {
            break;
        }
        let Some(name) = entry
            .file_name()
            .into_string()
            .ok()
            .filter(|s| !s.starts_with('.') && !s.contains(char::is_control))
        else {
            continue;
        };
        let kind = entry.file_type()?;
        if !kind.is_dir() && !kind.is_file() {
            continue;
        }
        let path = parent.join(&name);
        result.push(json!({"path":path,"name":name,"dir":kind.is_dir()}));
        if kind.is_dir() {
            entries(root, &path, depth + 1, result)?;
        }
    }
    Ok(())
}

pub(crate) fn watch(db: &Database, chat: &str) -> io::Result<Value> {
    let workspace = crate::ui_workspace::open_for_chat(db, chat)?;
    let mut files = Vec::new();
    entries(workspace.root(), Path::new(""), 0, &mut files)?;
    let connection = db
        .lock()
        .map_err(|_| io::Error::other("Database unavailable"))?;
    let commands = ui_store::get(&connection, TERMINAL, chat)
        .map_err(io::Error::other)?
        .map(|document| document["commands"].clone())
        .unwrap_or(json!([]));
    Ok(json!({"commands":commands,"workTree":files}))
}

fn terminal(db: &Database, chat: &str, machine: &str, command: &str) -> io::Result<Value> {
    let id = format!("terminal-{}", uuid::Uuid::new_v4().simple());
    // Keep this request bounded. The shell backend gives longer commands a job ID.
    let result = crate::ui_shell::docker_tool(
        db,
        chat,
        &id,
        machine,
        &json!({"id":id,"name":"shell","arguments":{"command":command,"timeout_s":8}}),
        &AtomicBool::new(false),
    );
    let row = match result {
        Ok(value) => {
            let data: Value = serde_json::from_str(&value)?;
            json!({"command":command,"exitCode":data["exit_code"],"stdout":data["stdout"],"stderr":data["stderr"]})
        }
        Err(error) if error.kind() == io::ErrorKind::TimedOut => {
            let data: Value = serde_json::from_str(&error.to_string())?;
            json!({"command":command,"stderr":data["summary"],"jobId":data["job_id"]})
        }
        Err(error) => return Err(error),
    };
    let connection = db
        .lock()
        .map_err(|_| io::Error::other("Database unavailable"))?;
    let mut history = ui_store::get(&connection, TERMINAL, chat)
        .map_err(io::Error::other)?
        .and_then(|value| value["commands"].as_array().cloned())
        .unwrap_or_default();
    history.push(row.clone());
    history.drain(..history.len().saturating_sub(30));
    ui_store::put(&connection, TERMINAL, chat, json!({"commands":history}))
        .map_err(io::Error::other)?;
    Ok(row)
}

pub(crate) fn handle(
    mut request: Request,
    db: &Database,
    chat: &str,
    operation: &str,
    tail: &[&str],
) -> io::Result<()> {
    if operation == "lease" && tail.is_empty() && request.method() == &Method::Post {
        return docker_desktop::handle(request, db, chat, "lease");
    }
    let _guard = docker_desktop::lock(chat);
    let binding = {
        let connection = db
            .lock()
            .map_err(|_| io::Error::other("Database unavailable"))?;
        docker_desktop::binding(&connection, chat).map_err(io::Error::other)?
    };
    let Some(machine) = binding else {
        crate::server::response(
            request,
            409,
            json!({"error":"Select a Docker workstation first"}),
        );
        return Ok(());
    };
    if operation == "file" && request.method() == &Method::Get {
        let path = tail
            .iter()
            .map(|part| {
                webui::form_decode(part)
                    .filter(|part| !part.contains('/') && !part.starts_with('.'))
            })
            .collect::<Option<Vec<_>>>()
            .map(|parts| parts.join("/"));
        let workspace = crate::ui_workspace::open_for_chat(db, chat)?;
        let directory = kageha_workspace::ReadWorkspace::new(workspace.root())?;
        let file = path.and_then(|relative| directory.open_visible_file(&relative).ok());
        if let Some(file) = file {
            let response = Response::from_file(file)
                .with_header(
                    Header::from_bytes("Content-Type", "application/octet-stream").unwrap(),
                )
                .with_header(Header::from_bytes("Content-Disposition", "attachment").unwrap())
                .with_header(Header::from_bytes("X-Content-Type-Options", "nosniff").unwrap());
            let _ = request.respond(response);
        } else {
            crate::server::response(
                request,
                404,
                json!({"error":"Workspace file is unavailable"}),
            );
        }
        return Ok(());
    }
    if operation == "exec" && tail.is_empty() && request.method() == &Method::Post {
        let args = webui::body(&mut request)?;
        let result = docker_desktop::require_running(&machine)
            .and_then(|_| {
                args["command"].as_str().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "command is required")
                })
            })
            .and_then(|command| terminal(db, chat, &machine, command));
        match result {
            Ok(value) => crate::server::response(request, 200, value),
            Err(error) => crate::server::response(request, 409, json!({"error":error.to_string()})),
        }
        return Ok(());
    }
    crate::server::response(request, 404, json!({"error":"Unknown machine operation"}));
    Ok(())
}
