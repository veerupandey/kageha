//! Shell service process: hosts one sandboxed shell session per chat over a local Unix
//! socket (mode 0600) using JSON lines (`open_session`, `run`, `run_once`, `cancel`, the
//! job operations, `session_dir`, `ping` and `close`; see `ShellService`). It runs apart
//! from the main server so shells survive a server restart. The Supervisor
//! (`kageha-loop::supervisor`) starts it when `KAGEHA_SHELL_SERVICE=process`.
//!
//! Usage: `kageha-shell-service --socket PATH [--toolchains PATH]`
use kageha_workspace::shell::{
    default_toolchain_cache, ensure_private_dir, host_environment, ShellService,
};
use std::{
    os::unix::{
        fs::{FileTypeExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    sync::Arc,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut socket = None;
    let mut toolchains = None;
    let mut container = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => socket = args.next().map(PathBuf::from),
            "--toolchains" => toolchains = args.next().map(PathBuf::from),
            "--container" => container = true,
            _ => return Err(usage()),
        }
    }
    let socket = socket.ok_or_else(usage)?;
    let toolchains = match toolchains {
        Some(path) => path,
        None => default_toolchain_cache(&host_environment())?,
    };
    if container && !std::path::Path::new("/.dockerenv").is_file() {
        return Err("Container shell service requires Docker".into());
    }
    if !container && !kageha_sandbox::available() {
        return Err("Required OS process sandbox unavailable; refusing to start".into());
    }
    let toolchains = ensure_private_dir(&toolchains)?;
    let listener = bind(&socket)?;
    let service = Arc::new(if container {
        ShellService::in_container(toolchains)
    } else {
        ShellService::new(toolchains)
    });
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(error) => {
                eprintln!("kageha-shell-service: accept failed: {error}");
                continue;
            }
        };
        let service = service.clone();
        std::thread::spawn(move || {
            if let Err(error) = service.serve_connection(stream) {
                eprintln!("kageha-shell-service: connection ended: {error}");
            }
        });
    }
    Ok(())
}

fn usage() -> Box<dyn std::error::Error> {
    "Usage: kageha-shell-service --socket PATH [--toolchains PATH] [--container]".into()
}

/// Binds with umask 0177 so the socket is never reachable by other users, replacing only a
/// stale socket file (one nobody is listening on).
fn bind(path: &PathBuf) -> Result<UnixListener, Box<dyn std::error::Error>> {
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if !meta.file_type().is_socket() {
            return Err("Socket path exists and is not a socket".into());
        }
        if UnixStream::connect(path).is_ok() {
            return Err("Another shell service is listening on this socket".into());
        }
        std::fs::remove_file(path)?;
    }
    let previous = unsafe { libc::umask(0o177) };
    let listener = UnixListener::bind(path);
    unsafe {
        libc::umask(previous);
    }
    let listener = listener?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}
