//! Installs the `kageha` connector API shim on the shell `PATH` (design section 2 and 3,
//! Requirement 1.4).
//!
//! The shim is [`crate::api_cli`] compiled by `build.rs` into a standalone, std-only
//! executable whose bytes are embedded here ([`SHIM_BINARY`]). Embedding means the Host
//! needs no separately shipped file and no interpreter: whichever binary links this
//! library can install the shim.
//!
//! [`install`] writes it to `<Toolchain_Cache>/bin/kageha`, the first `PATH` entry of every
//! shell session ([`super::shell_environment`]). It runs on every [`super::ShellPaths::prepare`]
//! and is idempotent: an installed file that is a regular executable file with the
//! embedded bytes' SHA-256 is left alone. Anything else (missing, changed by a command,
//! not executable, a symlink, an older shim) is replaced atomically: the bytes go to a
//! fresh temp file in the same directory, are synced, made `0755` and renamed over the
//! old path, and the result is hashed again before it is reported as installed.
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        OnceLock,
    },
    time::{SystemTime, UNIX_EPOCH},
};

/// Command name on the shell `PATH`.
pub const SHIM_NAME: &str = "kageha";
/// Directory inside the Toolchain_Cache that holds the shim; first on the shell `PATH`.
pub const BIN_DIR: &str = "bin";
/// The compiled shim. Empty when built with `KAGEHA_SKIP_API_SHIM=1`.
pub static SHIM_BINARY: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/kageha-api"));

/// Hex SHA-256 of [`SHIM_BINARY`].
pub fn shim_sha256() -> &'static str {
    static HASH: OnceLock<String> = OnceLock::new();
    HASH.get_or_init(|| hex(&Sha256::digest(SHIM_BINARY)))
}

/// `<toolchain_cache>/bin/kageha`.
pub fn shim_path(toolchain_cache: &Path) -> PathBuf {
    toolchain_cache.join(BIN_DIR).join(SHIM_NAME)
}

/// Makes sure `<toolchain_cache>/bin/kageha` is the embedded shim, executable. Returns its
/// path. Fails with `Unsupported` when this build has no shim.
pub fn install(toolchain_cache: &Path) -> io::Result<PathBuf> {
    if SHIM_BINARY.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this build has no kageha CLI shim (built with KAGEHA_SKIP_API_SHIM)",
        ));
    }
    let bin = super::ensure_private_dir(&toolchain_cache.join(BIN_DIR))?;
    let target = bin.join(SHIM_NAME);
    if is_installed(&target)? {
        return Ok(target);
    }
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let temp = bin.join(format!(
        ".{SHIM_NAME}-install-{}-{nanos}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let written = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temp)?;
        file.write_all(SHIM_BINARY)?;
        file.set_permissions(fs::Permissions::from_mode(0o755))?;
        file.sync_all()?;
        fs::rename(&temp, &target)
    })();
    if let Err(error) = written {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    if !is_installed(&target)? {
        return Err(io::Error::other("the installed kageha CLI shim does not match the embedded hash"));
    }
    Ok(target)
}

/// Whether `path` is a regular, executable file whose SHA-256 is [`shim_sha256`]. The file is
/// opened with `O_NOFOLLOW`, so a symlink is never trusted.
pub fn is_installed(path: &Path) -> io::Result<bool> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if !meta.is_file() || meta.len() != SHIM_BINARY.len() as u64 || meta.permissions().mode() & 0o755 != 0o755 {
        return Ok(false);
    }
    let mut file = match fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(path) {
        Ok(file) => file,
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => return Ok(false),
        Err(error) => return Err(error),
    };
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()) == shim_sha256())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn install_is_idempotent_and_repairs_tampering() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("toolchains");
        let path = install(&cache).unwrap();
        assert_eq!(path, shim_path(&cache.canonicalize().unwrap()));
        let meta = fs::metadata(&path).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o755);
        assert_eq!(hex(&Sha256::digest(fs::read(&path).unwrap())), shim_sha256());
        // A second install leaves the same file in place.
        install(&cache).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().ino(), meta.ino());
        // A changed file, a non-executable copy and a symlink are all replaced.
        fs::write(&path, b"#!/bin/sh\necho hijacked\n").unwrap();
        assert!(!is_installed(&path).unwrap());
        install(&cache).unwrap();
        assert!(is_installed(&path).unwrap());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        install(&cache).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o755);
        let elsewhere = dir.path().join("real");
        fs::copy(&path, &elsewhere).unwrap();
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &path).unwrap();
        assert!(!is_installed(&path).unwrap());
        install(&cache).unwrap();
        assert!(fs::symlink_metadata(&path).unwrap().is_file());
        // No temp files are left behind.
        let names: Vec<_> = fs::read_dir(path.parent().unwrap()).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(names, vec![std::ffi::OsString::from(SHIM_NAME)]);
    }

    /// The installed shim against a minimal peer speaking the connector API protocol:
    /// requests carry the current token, replies are unwrapped, and exit codes follow
    /// `ok`. The real server is exercised from `kageha-loop`.
    #[test]
    fn installed_shim_speaks_the_protocol_and_reads_the_token_each_time() {
        use std::{io::BufRead, os::unix::net::UnixListener, process::Command};
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("w");
        fs::create_dir_all(workspace.join(".kageha")).unwrap();
        let workspace = workspace.canonicalize().unwrap();
        let listener = UnixListener::bind(workspace.join(".kageha/api.sock")).unwrap();
        let server = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..4 {
                let (stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                io::BufReader::new(&stream).read_line(&mut line).unwrap();
                let request: serde_json::Value = serde_json::from_str(&line).unwrap();
                let reply = match request["operation"].as_str().unwrap() {
                    "status" => serde_json::json!({"ok":true,"result":{"status":"disconnected","integration":"gmail"}}),
                    "search" => serde_json::json!({"ok":true,"result":{"messages":[{"id":"m1"}]}}),
                    _ => serde_json::json!({"ok":false,"error":{"code":"read_failed","message":"no such message"}}),
                };
                (&stream).write_all(format!("{reply}\n").as_bytes()).unwrap();
                seen.push(request);
            }
            seen
        });
        let shim = install(&dir.path().join("toolchains")).unwrap();
        let run = |args: &[&str], env: (&str, &Path)| {
            Command::new(&shim).args(args).env_clear().env(env.0, env.1).current_dir("/").output().unwrap()
        };
        fs::write(workspace.join(".kageha/api-token"), "first-token\n").unwrap();
        let status = run(&["gmail", "status"], ("KAGEHA_WORKSPACE", workspace.as_path()));
        assert_eq!(status.status.code(), Some(0));
        assert_eq!(String::from_utf8_lossy(&status.stdout), "{\n  \"integration\": \"gmail\",\n  \"status\": \"disconnected\"\n}\n");
        assert!(String::from_utf8_lossy(&status.stderr).contains("gmail is not connected (status: disconnected)"));
        // The token is read again on each call; the socket-path fallback works too.
        fs::write(workspace.join(".kageha/api-token"), "second-token").unwrap();
        let socket = workspace.join(".kageha/api.sock");
        let search = run(&["gmail", "search", "is:unread", "--json", "--refresh"], ("KAGEHA_API_SOCKET", socket.as_path()));
        assert_eq!(search.status.code(), Some(0), "{search:?}");
        assert_eq!(String::from_utf8_lossy(&search.stdout), "{\"messages\":[{\"id\":\"m1\"}]}\n");
        let message = run(&["gmail", "message", "m9"], ("KAGEHA_WORKSPACE", workspace.as_path()));
        assert_eq!(message.status.code(), Some(1));
        assert!(message.stdout.is_empty());
        assert_eq!(String::from_utf8_lossy(&message.stderr), "kageha: read_failed: no such message\n");
        let thread = run(&["gmail", "thread", "t1"], ("KAGEHA_WORKSPACE", workspace.as_path()));
        assert_eq!(thread.status.code(), Some(1));
        let seen = server.join().unwrap();
        assert_eq!(seen[0], serde_json::json!({"token":"first-token","integration":"gmail","operation":"status"}));
        assert_eq!(seen[1], serde_json::json!({"token":"second-token","integration":"gmail","operation":"search","query":"is:unread","refresh":true}));
        assert_eq!(seen[2]["id"], "m9");
        for output in [&status, &search, &message, &thread] {
            let all = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
            assert!(!all.contains("-token"), "the token is never printed: {all}");
        }
    }

    #[test]
    fn sandboxed_shell_has_kageha_on_path() {
        use crate::shell::{ShellPaths, ShellSession};
        use std::time::Duration;
        if !kageha_sandbox::available() {
            eprintln!("skipping: OS process sandbox unavailable");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let paths = ShellPaths::prepare(&workspace, &dir.path().join("toolchains")).unwrap();
        let mut shell = ShellSession::open(paths.clone()).unwrap();
        let t = Duration::from_secs(20);
        let which = shell.run("command -v kageha", "c", t).unwrap();
        assert_eq!(which.stdout.trim_end(), shim_path(&paths.toolchain_cache).to_str().unwrap());
        let help = shell.run("kageha --help", "c", t).unwrap();
        assert_eq!(help.exit_code, 0, "{help:?}");
        assert!(help.stdout.contains("kageha [--json] [--refresh] gmail search <query>"));
        // No turn is active, so there is no socket.
        let missing = shell.run("kageha gmail status", "c", t).unwrap();
        assert_eq!(missing.exit_code, 3, "{missing:?}");
        assert!(missing.stdout.is_empty());
        assert!(missing.stderr.starts_with(&format!(
            "kageha: api_unavailable: The local connector API is not running at {}/.kageha/api.sock (no socket there).",
            paths.workspace.display()
        )), "{}", missing.stderr);
        let usage = shell.run("kageha gmail send m1", "c", t).unwrap();
        assert_eq!(usage.exit_code, 2);
        assert!(usage.stderr.contains("read-only"));
    }
}
