//! Per-process filesystem/network confinement. Never silently falls back to host access.
use serde::{Deserialize, Serialize};
use std::{
    io,
    path::{Path, PathBuf},
    process::Command,
};
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProcessSandbox {
    pub read_roots: Vec<PathBuf>,
    pub write_roots: Vec<PathBuf>,
    pub network: bool,
    /// Subtrees of the read roots that the process can neither read nor write. They are
    /// applied after the read roots and before the write roots, so a write root inside a
    /// hidden subtree stays usable (for example a delegated worker's own directory inside
    /// its parent's read-only workspace, whose other workers stay hidden).
    pub hidden_roots: Vec<PathBuf>,
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
fn roots(values: &[PathBuf]) -> io::Result<Vec<PathBuf>> {
    if values.len() > 32 {
        return Err(invalid("Sandbox root limit exceeded"));
    }
    values
        .iter()
        .map(|p| {
            let p = p.canonicalize()?;
            if p == Path::new("/")
                || p.to_str()
                    .is_none_or(|s| s.len() > 4096 || s.chars().any(char::is_control))
            {
                return Err(invalid("Invalid sandbox root"));
            }
            Ok(p)
        })
        .collect()
}
fn executable(program: &str) -> io::Result<PathBuf> {
    let p = Path::new(program);
    if p.is_absolute() {
        return p.canonicalize();
    }
    if p.components().count() != 1 {
        return Err(invalid("Use an absolute sandbox executable path"));
    }
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|dir| dir.join(p))
        .find(|p| p.is_file())
        .ok_or_else(|| invalid("Sandbox executable not found"))?
        .canonicalize()
}
pub fn available() -> bool {
    #[cfg(target_os = "macos")]
    {
        Path::new("/usr/bin/sandbox-exec").is_file()
    }
    #[cfg(target_os = "linux")]
    {
        executable("bwrap").is_ok()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        false
    }
}
/// The fixed part of the macOS Seatbelt profile: what the shell, ordinary command-line
/// tools and the system frameworks they load need, independent of the roots.
///
/// It is written out rather than imported from Apple's `bsd.sb`/`system.sb`. Those files
/// are private interface that Apple can change without notice (widening or narrowing what
/// every sandboxed process may do) and they allow all XPC-service lookups. Their lists
/// were the starting point, together with the Codex CLI Seatbelt base policy (itself
/// derived from Chromium's `common.sb`), trimmed to what the shell tests need:
///
/// - Device files: `/dev/fd/*` (process substitution; xnu applies the normal descriptor
///   checks), `/dev/stdin`, `/dev/stdout` and `/dev/stderr` (links there), `/dev/null`,
///   `/dev/zero`, `/dev/tty`, `/dev/dtracehelper` (opened by dyld), read-only
///   `/dev/random` and `/dev/urandom`.
/// - `file-read-metadata` everywhere, as in `bsd.sb`: path resolution, `stat`, `ls -ld ..`
///   and `pwd` work on the ancestors of the roots. [`seatbelt_policy`] also lets the
///   ancestor directories of the caller's roots be listed (literal paths, so `ls ..` works
///   but nothing below them opens). Other listings and file contents outside the read
///   roots stay denied.
/// - Mach services, by name or `global-name-prefix`, that libSystem, CoreFoundation and
///   ordinary command-line tools use: `coreservices.*`/`CoreServices.*` and `lsd.*`
///   (LaunchServices type and bundle lookups), `cfprefsd.*` (preferences), `trustd*` and
///   `SecurityServer` (certificate trust, keychain-free Security calls), `logd*`,
///   `diagnosticd` and `system.*` (logging, notify, user and group lookups), `analyticsd*`,
///   `distributed_notifications*`, `FontObjectsServer` and `fonts*` (CoreText), plus
///   `bsd.dirhelper` and `appleneuralengine` for Vision/CoreML. No blanket
///   `(allow mach-lookup)`: Apple Events and DiskArbitration (mount and eject) stay
///   denied, and `coreservices.launchservicesd`, `lsd.open` and `lsd.modifydb` are denied
///   explicitly, because they launch or register applications outside the sandbox.
/// - Preference reads for the global domain and `com.apple.*` (framework settings), not
///   third-party application preferences, which may hold credentials. Writes go through
///   `cfprefsd`, which enforces `user-preference-write`, and are denied.
/// - IOKit user clients for IOSurface, the GPU and the Neural Engine (Vision, CoreML,
///   Metal), and the power-management root domain that libraries query at start-up.
/// - POSIX semaphores and shared memory (Python multiprocessing, OpenMP), except Apple's
///   `apple.*` objects, of which only the notification center and preferences are readable.
/// - `process-info` and signals within the sandbox, so `ps`, `kill %1` and `timeout` work
///   on the session's own processes.
/// - The one write outside the roots: `/usr/bin/diff` copies a non-regular input (such as
///   `<(cmd)`) to a temporary file at the fixed path `/tmp/diff.XXXXXXXX`, ignoring
///   TMPDIR. Only regular files matching exactly that name can be created, read, written
///   and removed there; nothing else under `/tmp`.
///
/// `file-issue-extension`, which frameworks use to hand a cache file to a system service
/// (Vision gives the Neural Engine compiler its model cache this way), is allowed only for
/// the write roots ([`seatbelt_policy`]).
#[cfg(target_os = "macos")]
const SEATBELT_BASE: &str = r#"(version 1)
(deny default)
(allow process-fork)
(allow process-exec)
(allow process-info* (target same-sandbox))
(allow signal (target same-sandbox))
(allow sysctl-read)
(allow file-read-metadata)
(allow file-read* (literal "/"))
(allow file-read-data file-write-data file-ioctl (subpath "/dev/fd"))
(allow file-read* file-write-data file-ioctl
  (literal "/dev/null")
  (literal "/dev/zero")
  (literal "/dev/tty")
  (literal "/dev/stdin")
  (literal "/dev/stdout")
  (literal "/dev/stderr")
  (literal "/dev/dtracehelper"))
(allow file-read*
  (literal "/dev/random")
  (literal "/dev/urandom")
  (literal "/dev/autofs_nowait")
  (literal "/private/etc/passwd")
  (literal "/private/etc/group")
  (literal "/private/etc/protocols")
  (literal "/private/etc/services")
  (literal "/private/etc/apache2/mime.types")
  (subpath "/Library/Preferences/Logging"))
(allow mach-lookup
  (global-name-prefix "com.apple.analyticsd")
  (global-name "com.apple.appleneuralengine")
  (global-name "com.apple.bsd.dirhelper")
  (global-name-prefix "com.apple.cfprefsd.")
  (global-name-prefix "com.apple.coreservices.")
  (global-name-prefix "com.apple.CoreServices.")
  (global-name "com.apple.diagnosticd")
  (global-name-prefix "com.apple.distributed_notifications")
  (global-name "com.apple.FontObjectsServer")
  (global-name-prefix "com.apple.fonts")
  (global-name-prefix "com.apple.logd")
  (global-name-prefix "com.apple.lsd.")
  (global-name "com.apple.SecurityServer")
  (global-name-prefix "com.apple.system.")
  (global-name-prefix "com.apple.trustd")
  (local-name "com.apple.cfprefsd.agent"))
; Headless Chromium connects renderer children to the browser process by PID.
(allow mach-register mach-lookup
  (global-name-regex #"^org[.]chromium[.]Chromium[.]MachPortRendezvousServer[.][0-9]+$"))
(deny mach-lookup
  (global-name "com.apple.coreservices.launchservicesd")
  (global-name "com.apple.lsd.open")
  (global-name "com.apple.lsd.modifydb"))
(allow user-preference-read
  (preference-domain "kCFPreferencesAnyApplication")
  (preference-domain-prefix "com.apple."))
(allow iokit-open
  (iokit-registry-entry-class "RootDomainUserClient")
  (iokit-user-client-class
    "AGXDeviceUserClient"
    "AGXSharedUserClient"
    "H11ANEInDirectPathClient"
    "H11ANEInUserClient"
    "IOAccelerationUserClient"
    "IOSurfaceRootUserClient"
    "IOSurfaceSendRight"
    "RootDomainUserClient"))
(allow file-read-data file-write-create file-write-data file-write-unlink
  (require-all
    (regex #"^/private/tmp/diff\.[A-Za-z0-9][A-Za-z0-9][A-Za-z0-9][A-Za-z0-9][A-Za-z0-9][A-Za-z0-9][A-Za-z0-9][A-Za-z0-9]$")
    (vnode-type REGULAR-FILE)))
(allow ipc-posix-sem)
(allow ipc-posix-shm*)
(deny ipc-posix-shm* (ipc-posix-name-prefix "apple."))
(allow ipc-posix-shm-read*
  (ipc-posix-name "apple.shm.notification_center")
  (ipc-posix-name-prefix "apple.cfprefs."))
"#;

/// The complete macOS profile for canonical `reads`, `hidden` and `writes`. The last
/// matching rule wins: hidden subtrees after the read roots, write roots after both, so a
/// write root inside a hidden subtree stays usable.
#[cfg(target_os = "macos")]
fn seatbelt_policy(
    reads: &[PathBuf],
    ancestors: &[PathBuf],
    hidden: &[PathBuf],
    writes: &[PathBuf],
    network: bool,
) -> String {
    fn quoted(path: &Path) -> String {
        format!(
            "\"{}\"",
            path.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"")
        )
    }
    let mut policy = String::from(SEATBELT_BASE);
    // The ancestors of the caller's roots, as literal directories: their entries can be
    // listed (`ls ..` from the working directory), nothing below them is opened.
    for p in ancestors {
        policy += &format!("(allow file-read-data (literal {}))\n", quoted(p));
    }
    for p in reads {
        policy += &format!("(allow file-read* (subpath {}))\n", quoted(p));
    }
    for p in hidden {
        policy += &format!(
            "(deny file-read* file-write* file-issue-extension (subpath {}))\n",
            quoted(p)
        );
    }
    for p in writes {
        policy += &format!(
            "(allow file-read* file-write* file-issue-extension (subpath {}))\n",
            quoted(p)
        );
    }
    if network {
        policy += "(allow network*)\n";
    }
    policy
}

impl ProcessSandbox {
    pub fn command(&self, program: &str, args: &[String]) -> io::Result<Command> {
        let program = executable(program)?;
        let mut reads = roots(&self.read_roots)?;
        let writes = roots(&self.write_roots)?;
        let hidden = roots(&self.hidden_roots)?;
        #[cfg(target_os = "macos")]
        let user_roots: Vec<PathBuf> = reads.iter().chain(&writes).cloned().collect();
        // Executables and their system runtime dependencies are readable, not writable.
        reads.push(program.clone());
        for root in [
            "/usr/bin",
            "/usr/lib",
            "/usr/share",
            "/bin",
            "/sbin",
            "/lib",
            "/lib64",
            "/System",
            "/Library/Apple",
            "/opt/homebrew/Cellar",
            "/opt/homebrew/opt",
        ] {
            if Path::new(root).exists() {
                reads.push(PathBuf::from(root));
            }
        }
        // Public runtime configuration, never private-key directories or user dotfiles.
        for file in [
            "/etc/ssl/openssl.cnf",
            "/etc/ssl/cert.pem",
            "/etc/ssl/certs",
        ] {
            if let Ok(path) = Path::new(file).canonicalize() {
                reads.push(path);
            }
        }
        if self.network {
            for file in [
                "/etc/resolv.conf",
                "/etc/hosts",
                "/etc/nsswitch.conf",
                "/etc/gai.conf",
            ] {
                if let Ok(path) = Path::new(file).canonicalize() {
                    reads.push(path);
                }
            }
        }
        #[cfg(target_os = "macos")]
        {
            if !available() {
                return Err(invalid("Required macOS process sandbox unavailable"));
            }
            let mut ancestors: Vec<PathBuf> = user_roots
                .iter()
                .flat_map(|p| p.ancestors().skip(1).map(Path::to_path_buf))
                .collect();
            ancestors.sort();
            ancestors.dedup();
            let policy = seatbelt_policy(&reads, &ancestors, &hidden, &writes, self.network);
            let mut command = Command::new("/usr/bin/sandbox-exec");
            command.args(["-p", &policy]).arg(program).args(args);
            Ok(command)
        }
        #[cfg(target_os = "linux")]
        {
            // `--dev /dev` provides /dev/null, /dev/zero, /dev/random, /dev/urandom,
            // /dev/tty and the /dev/fd, /dev/stdin, /dev/stdout and /dev/stderr links into
            // the `--proc` mount, so process substitution works. Bash writes here-documents
            // under TMPDIR (a write root for shells) or the private /tmp tmpfs. The parents of
            // every bound root exist as empty directories, so they can be stat'ed.
            let mut command = Command::new(executable("bwrap")?);
            command.args([
                "--unshare-all",
                "--die-with-parent",
                "--new-session",
                "--proc",
                "/proc",
                "--dev",
                "/dev",
                "--tmpfs",
                "/tmp",
            ]);
            if self.network {
                command.arg("--share-net");
            }
            for p in &reads {
                command.arg("--ro-bind").arg(p).arg(p);
            }
            // Mounted in order: an empty tmpfs over each hidden subtree, then the write
            // binds, which may sit inside one.
            for p in &hidden {
                command.arg("--tmpfs").arg(p);
            }
            for p in &writes {
                command.arg("--bind").arg(p).arg(p);
            }
            command.arg("--").arg(program).args(args);
            Ok(command)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = (reads, writes, hidden, args);
            Err(invalid("Required process sandbox unsupported on this OS"))
        }
    }
}
/// Shared immutable operator requirements also apply to process discovery/startup.
pub fn required() -> io::Result<bool> {
    static REQUIREMENT: std::sync::OnceLock<Result<bool, String>> = std::sync::OnceLock::new();
    REQUIREMENT
        .get_or_init(|| load_requirement().map_err(|e| e.to_string()))
        .as_ref()
        .copied()
        .map_err(|e| invalid(e))
}
fn load_requirement() -> io::Result<bool> {
    let Some(path) = std::env::var_os("KAGEHA_POLICY_REQUIREMENTS") else {
        return Ok(false);
    };
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(65537)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 65536 {
        return Err(invalid("Requirements exceed limit"));
    }
    let config: serde_json::Value = serde_json::from_slice(&bytes)?;
    match config.get("require_process_sandbox") {
        None => Ok(false),
        Some(serde_json::Value::Bool(v)) => Ok(*v),
        _ => Err(invalid("Invalid process sandbox requirement")),
    }
}
