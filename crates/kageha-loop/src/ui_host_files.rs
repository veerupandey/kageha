//! Import the exact host files approved for a local chat into its task workspace.
use crate::{ui_approvals, ui_workspace, webui::Database};
use kageha_workspace::task::{InputSource, TaskWorkspace};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    ffi::CString,
    fs::File,
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    },
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn check_cancel(cancel: &AtomicBool) -> io::Result<()> {
    if cancel.load(Ordering::Acquire) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Host file import cancelled",
        ));
    }
    Ok(())
}

// Walk the approved canonical path with directory descriptors. A symlink introduced
// while the approval is on screen cannot redirect the broker into another subtree.
fn open_regular(path: &Path) -> io::Result<File> {
    let mut file = File::open("/")?;
    let mut parts = path.components().skip(1).peekable();
    while let Some(Component::Normal(name)) = parts.next() {
        let name = CString::new(name.as_bytes())?;
        let mut flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
        if parts.peek().is_some() {
            flags |= libc::O_DIRECTORY;
        }
        let fd = unsafe { libc::openat(file.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        file = unsafe { File::from_raw_fd(fd) };
    }
    if !file.metadata()?.is_file() {
        return Err(invalid("Select regular files, not directories or devices"));
    }
    Ok(file)
}

pub(crate) fn import(
    db: &Database,
    chat: &str,
    turn: &str,
    args: &Value,
    cancel: &AtomicBool,
) -> io::Result<String> {
    let reason = args["reason"]
        .as_str()
        .map(str::trim)
        .filter(|r| !r.is_empty() && r.len() <= 1024)
        .ok_or_else(|| invalid("Describe why this task needs the files"))?;
    let requested = args["paths"]
        .as_array()
        .filter(|v| (1..=16).contains(&v.len()))
        .ok_or_else(|| invalid("Specify 1 to 16 host file paths"))?;
    let state = crate::ui_uploads::root_path(db, chat)?
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| invalid("Chat state unavailable"))?
        .canonicalize()?;
    let mut paths = Vec::new();
    for item in requested {
        let text = item
            .as_str()
            .filter(|s| s.len() <= 4096 && !s.chars().any(char::is_control))
            .ok_or_else(|| invalid("Invalid host file path"))?;
        let path = match text.strip_prefix("~/") {
            Some(relative) => PathBuf::from(
                std::env::var_os("HOME").ok_or_else(|| invalid("Host home unavailable"))?,
            )
            .join(relative),
            None => PathBuf::from(text),
        };
        if !path.is_absolute() {
            return Err(invalid("Specify absolute host paths or ~/ paths"));
        }
        let path = path.canonicalize().map_err(|e| {
            let error = source_error(&path, e);
            if error.kind() == io::ErrorKind::PermissionDenied {
                crate::ui_tools::tool_error(
                    error.kind(),
                    "host_file_permission_required",
                    false,
                    &error.to_string(),
                )
            } else {
                error
            }
        })?;
        if path.starts_with(&state)
            || path.starts_with("/dev")
            || path.starts_with("/proc")
            || path.starts_with("/sys")
        {
            return Err(invalid(
                "Select a host input file outside runtime state and devices",
            ));
        }
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    let detail = format!(
        "Read these files into this chat's workspace for {reason}\n{}",
        paths
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );
    let resource = format!(
        "host-input:{:x}",
        Sha256::digest(serde_json::to_vec(&paths)?)
    );
    if !ui_approvals::require(
        db,
        chat,
        turn,
        "host_import_files",
        &detail,
        &json!({"paths":paths,"reason":reason,"resource":resource}),
        cancel,
    )? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Host file access declined",
        ));
    }
    let workspace = ui_workspace::open_for_chat(db, chat)?;
    let mut files = Vec::new();
    let mut failed = Vec::new();
    let mut buffer = vec![0; 128 * 1024];
    for path in &paths {
        check_cancel(cancel)?;
        crate::ui_tools::ensure_disk_space(
            db,
            chat,
            turn,
            "inputs",
            crate::ui_shell::free_bytes(&workspace)?,
        )?;
        match import_one(&workspace, path, &mut buffer, cancel) {
            Ok(file) => files.push(file),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => return Err(error),
            Err(error) => failed.push(json!({"source":path,"error":error.to_string(),"os_denied":error.kind()==io::ErrorKind::PermissionDenied})),
        }
    }
    let mut result = json!({"complete":failed.is_empty(),"files":files,"failed":failed});
    if files.is_empty() {
        let denial = failed.iter().find(|failure| failure["os_denied"] == true);
        result["error_code"] = json!(if denial.is_some() {
            "host_file_permission_required"
        } else {
            "host_input_unavailable"
        });
        result["summary"] = denial.unwrap_or(&failed[0])["error"].clone();
        result["retryable"] = json!(false);
        return Err(io::Error::other(result.to_string()));
    }
    Ok(result.to_string())
}

fn import_one(
    workspace: &TaskWorkspace,
    path: &Path,
    buffer: &mut [u8],
    cancel: &AtomicBool,
) -> io::Result<Value> {
    let source = open_regular(path).map_err(|e| source_error(path, e))?;
    let size = source.metadata()?.len();
    let mut source = source.take(size);
    let mut temp = tempfile::NamedTempFile::new()?;
    let mut hash = Sha256::new();
    loop {
        check_cancel(cancel)?;
        let len = source.read(buffer).map_err(|e| source_error(path, e))?;
        if len == 0 {
            break;
        }
        temp.write_all(&buffer[..len])?;
        hash.update(&buffer[..len]);
    }
    if source.limit() != 0 || source.into_inner().metadata()?.len() != size {
        return Err(io::Error::other("Source file changed during import"));
    }
    check_cancel(cancel)?;
    let digest = format!("{:x}", hash.finalize());
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| invalid("Invalid file name"))?;
    let key = format!("host/{digest}/{name}");
    let fallback = format!("{}-{name}", &digest[..12]);
    let input = InputSource {
        key: &key,
        name,
        fallback: &fallback,
        file: temp.path(),
        sha256: &digest,
    };
    let placed = workspace
        .place_input(&input)
        .map_err(io::Error::other)?
        .ok_or_else(|| {
            io::Error::other("Input name unavailable; existing workspace files were preserved")
        })?;
    Ok(json!({"source":path,"workspace_path":placed.path,"sha256":digest}))
}

fn source_error(path: &Path, error: io::Error) -> io::Error {
    if error.kind() == io::ErrorKind::PermissionDenied {
        io::Error::new(error.kind(), format!("The host OS denied access to {}. Ask the user to grant the Kageha host access to this location in OS privacy settings or select the file for attachment, then retry. Do not use Terminal or Finder to route around the denial.",path.display()))
    } else {
        io::Error::new(error.kind(), format!("{}: {error}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audited_session_tests::{main_persona, Fixture, CHAT};
    use crate::native_chat::ToolHost;
    use kageha_contracts::policy::{ApprovalDecision, GrantScope, PermissionProfile};

    #[test]
    fn full_imports_binary_files_and_reuses_same_input() {
        let f = Fixture::new();
        let outside = tempfile::tempdir().unwrap();
        let path = outside.path().join("clip.mov");
        let bytes = [0_u8, 255, 5, 0];
        std::fs::write(&path, bytes).unwrap();
        let host = f.tools(main_persona(Value::Null));
        let request = json!({"id":"read-clip","name":"host_import_files","arguments":{"paths":[path],"reason":"Edit the supplied footage"}});
        assert!(host
            .specs()
            .iter()
            .any(|s| s["function"]["name"] == "host_import_files"));
        for _ in 0..2 {
            let value: Value = serde_json::from_str(&host.invoke(&request).unwrap()).unwrap();
            assert_eq!(value["files"][0]["workspace_path"], "inputs/clip.mov");
            assert_eq!(
                std::fs::read(
                    ui_workspace::open_for_chat(&f.db, CHAT)
                        .unwrap()
                        .root()
                        .join("inputs/clip.mov")
                )
                .unwrap(),
                bytes
            );
        }
    }

    #[test]
    fn shell_processes_imported_files_and_reports_partial_imports() {
        let f = Fixture::for_chat("host-file-flow");
        let outside = tempfile::tempdir().unwrap();
        let clip = outside.path().join("footage.bin");
        std::fs::write(&clip, [0xFE; 200_000]).unwrap();
        let host = f.tools(main_persona(Value::Null));
        let request = json!({"id":"open-inputs","name":"host_import_files","arguments":{
            "paths":[clip, outside.path()],"reason":"Count the bytes in this input"}});
        let imported: Value = serde_json::from_str(&host.invoke(&request).unwrap()).unwrap();
        assert_eq!(imported["complete"], false);
        assert_eq!(imported["failed"].as_array().unwrap().len(), 1);
        assert_eq!(imported["files"][0]["workspace_path"], "inputs/footage.bin");
        if crate::ui_shell::available() {
            let read = json!({"id":"process-input","name":"shell","arguments":{"command":"wc -c < inputs/footage.bin"}});
            let result: Value = serde_json::from_str(&host.invoke(&read).unwrap()).unwrap();
            assert_eq!(result["exit_code"], 0);
            assert_eq!(result["stdout"].as_str().unwrap().trim(), "200000");
        }
    }

    #[test]
    fn ask_displays_the_exact_source_before_copy_and_honors_denial() {
        let f = Fixture::new();
        let outside = tempfile::tempdir().unwrap();
        let path = outside.path().join("clip.mov");
        std::fs::write(&path, "clip").unwrap();
        let input = ui_workspace::open_for_chat(&f.db, CHAT)
            .unwrap()
            .root()
            .join("inputs/clip.mov");
        let journal = ui_approvals::owned(&f.db.lock().unwrap()).unwrap();
        journal.set_profile(CHAT, PermissionProfile::Ask).unwrap();
        let args = json!({"paths":[path],"reason":"Prepare requested clip"});
        for approved in [false, true] {
            std::thread::scope(|threads| {
                let call =
                    threads.spawn(|| import(&f.db, CHAT, &f.turn, &args, &AtomicBool::new(false)));
                let mut pending = vec![];
                let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
                while pending.is_empty() && std::time::Instant::now() < end {
                    pending = journal.pending_family(CHAT).unwrap();
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                let record = pending.pop().expect("host access prompt");
                assert!(record
                    .request
                    .detail
                    .contains(path.canonicalize().unwrap().to_str().unwrap()));
                assert!(!input.exists());
                journal
                    .decide(
                        &record.id,
                        &ApprovalDecision {
                            expected_revision: record.revision,
                            authority: record.request.authority,
                            approved,
                            scope: GrantScope::Once,
                        },
                    )
                    .unwrap();
                assert_eq!(call.join().unwrap().is_ok(), approved);
                assert_eq!(input.exists(), approved);
            });
        }
    }

    #[test]
    fn component_swap_cannot_redirect_an_approved_path() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("before")).unwrap();
        std::fs::write(root.join("before/clip"), "original").unwrap();
        let approved = root.join("before/clip").canonicalize().unwrap();
        std::fs::rename(root.join("before"), root.join("after")).unwrap();
        std::os::unix::fs::symlink(root.join("after"), root.join("before")).unwrap();
        assert!(open_regular(&approved).is_err());
    }
}
