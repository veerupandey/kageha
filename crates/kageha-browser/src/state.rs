use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Marker {
    protocol_version: u64,
    instance_id: String,
    application_id: String,
}

pub struct State {
    pub root: PathBuf,
    pub socket: PathBuf,
    pub instance_id: String,
    pub application_id: String,
    pub token: String,
    pub epoch: String,
    _lock: File,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

pub fn uid() -> u32 {
    // No arguments or memory ownership cross this system call.
    unsafe { libc::geteuid() }
}

fn private_metadata(path: &Path, directory: bool) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.uid() != uid()
        || meta.mode() & 0o077 != 0
        || (if directory {
            !meta.is_dir()
        } else {
            !meta.is_file()
        })
    {
        return Err(invalid(
            "state paths must be private, owned, and not symlinks",
        ));
    }
    Ok(())
}

fn create_file(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    file.write_all(data)?;
    file.sync_all()
}

fn read_private(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    private_metadata(path, false)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(invalid("state file too large"));
    }
    Ok(bytes)
}

pub fn managed_profile(root: &Path, initial: &str) -> io::Result<String> {
    match read_private(&root.join("selected-profile"), 16) {
        Ok(profile) => match profile.as_slice() {
            b"private" | b"chrome" | b"comet" => Ok(String::from_utf8(profile).unwrap()),
            _ => Err(invalid("invalid selected browser profile")),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if initial != "private" {
                save_managed_profile(root, initial)?;
            }
            Ok(initial.into())
        }
        Err(error) => Err(error),
    }
}

pub fn save_managed_profile(root: &Path, profile: &str) -> io::Result<()> {
    replace_private(root, "selected-profile", profile.as_bytes())
}

#[derive(Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct PersonalTabs {
    pub port: u16,
    pub debugger_path: String,
    pub selected: String,
    pub targets: Vec<String>,
}

fn tabs_file(profile: &str) -> io::Result<&'static str> {
    match profile {
        "chrome" => Ok("chrome-tabs.json"),
        "comet" => Ok("comet-tabs.json"),
        _ => Err(invalid("unsupported personal browser profile")),
    }
}

pub fn load_personal_tabs(root: &Path, profile: &str) -> io::Result<Option<PersonalTabs>> {
    match read_private(&root.join(tabs_file(profile)?), 8192) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub fn save_personal_tabs(root: &Path, profile: &str, tabs: &PersonalTabs) -> io::Result<()> {
    replace_private(root, tabs_file(profile)?, &serde_json::to_vec(tabs)?)
}

/// Profile directories older than this are eligible for removal when unused.
const STALE_PROFILE_AGE: std::time::Duration = std::time::Duration::from_secs(3600);

/// `chromium-` plus the six alphanumerics `tempfile::Builder` appends.
fn is_profile_name(name: &str) -> bool {
    name.strip_prefix("chromium-")
        .is_some_and(|suffix| suffix.len() == 6 && suffix.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// Disposable Chromium profiles left by launches that were killed before their
/// watcher could delete them. `processes` is the running command lines. Only
/// real directories of the exact name shape that no process mentions and that
/// are not recent are removed. A live profile's directory time can be old, so
/// being named by a process always wins over age.
pub fn sweep_stale_profiles(root: &Path, processes: &str, now: std::time::SystemTime) -> usize {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str().filter(|name| is_profile_name(name)) else {
            continue;
        };
        let path = entry.path();
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        let old = metadata
            .modified()
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= STALE_PROFILE_AGE);
        if metadata.is_dir()
            && !metadata.file_type().is_symlink()
            && old
            && !processes.contains(name)
            && std::fs::remove_dir_all(&path).is_ok()
        {
            removed += 1;
        }
    }
    removed
}

/// Command lines of every running process. Empty when `ps` fails, which makes
/// callers skip the sweep, never run it blind.
pub fn running_commands() -> Option<String> {
    let output = std::process::Command::new("/bin/ps").args(["-axo", "command="]).output().ok()?;
    (output.status.success() && !output.stdout.is_empty())
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

pub fn forget_personal_tabs(root: &Path, profile: &str) -> io::Result<()> {
    match fs::remove_file(root.join(tabs_file(profile)?)) {
        Ok(()) => File::open(root)?.sync_all(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn replace_private(root: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(root)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(root.join(name))?;
    File::open(root)?.sync_all()
}

impl State {
    pub fn claim_action_backend(&self, backend: &str) -> io::Result<()> {
        let path = self.root.join("action-backend");
        match read_private(&path, 32) {
            Ok(current) if current == backend.as_bytes()
                || matches!(backend, "standalone" | "fixture") && current == format!("{backend}-sqlite-v1").as_bytes() => Ok(()),
            Ok(_) => Err(invalid("state directory belongs to another action journal")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if backend == "application"
                    && fs::symlink_metadata(self.root.join("action-receipts.json")).is_ok()
                {
                    return Err(invalid(
                        "application mode requires a separate state directory",
                    ));
                }
                create_file(&path, backend.as_bytes())?;
                File::open(&self.root)?.sync_all()
            }
            Err(error) => Err(error),
        }
    }

    pub fn open(root: &Path, instance_id: String, application_id: String) -> io::Result<Self> {
        if !root.is_absolute() || root.parent().is_none() {
            return Err(invalid(
                "an explicit absolute development state directory is required",
            ));
        }
        for id in [&instance_id, &application_id] {
            if id.is_empty()
                || id.len() > crate::protocol::limit("id_bytes")
                || id.chars().any(char::is_control)
            {
                return Err(invalid("invalid instance or application identity"));
            }
        }
        match DirBuilder::new().mode(0o700).create(root) {
            Ok(()) => (),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(e),
        }
        private_metadata(root, true)?;
        let root = root.canonicalize()?;
        let marker_path = root.join("instance.json");
        let marker = Marker {
            protocol_version: crate::protocol::version(),
            instance_id,
            application_id,
        };
        if !marker_path.exists() {
            if fs::read_dir(&root)?.next().is_some() {
                return Err(invalid(
                    "refusing a nonempty state directory without an instance marker",
                ));
            }
            create_file(&marker_path, &serde_json::to_vec(&marker)?)?;
        }
        let recorded: Marker = serde_json::from_slice(&read_private(&marker_path, 4096)?)?;
        if recorded != marker {
            return Err(invalid(
                "state directory belongs to another instance or application",
            ));
        }

        let lock_path = root.join("daemon.lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&lock_path)?;
        private_metadata(&lock_path, false)?;
        lock.try_lock_exclusive()
            .map_err(|_| invalid("this instance is already running"))?;
        crate::upload::clean_stages(&root)?;

        let token_path = root.join("auth.token");
        if !token_path.exists() {
            create_file(&token_path, Uuid::new_v4().simple().to_string().as_bytes())?;
        }
        let token = String::from_utf8(read_private(&token_path, 128)?)
            .map_err(|_| invalid("invalid authentication token"))?;
        if token.len() != 32 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(invalid("invalid authentication token"));
        }
        let socket = root.join("control.sock");
        let socket = if socket.as_os_str().len() > 100 {
            use sha2::{Digest, Sha256};
            let digest = data_encoding::HEXLOWER.encode(&Sha256::digest(root.as_os_str().as_encoded_bytes()));
            let runtime = Path::new("/tmp").join(format!("kageha-browser-{}-{}", uid(), &digest[..24]));
            match DirBuilder::new().mode(0o700).create(&runtime) {
                Ok(()) => (),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
                Err(error) => return Err(error),
            }
            private_metadata(&runtime, true)?;
            runtime.join("control.sock")
        } else { socket };
        match fs::symlink_metadata(&socket) {
            Ok(meta) if meta.file_type().is_socket() && meta.uid() == uid() => {
                fs::remove_file(&socket)?
            }
            Ok(_) => return Err(invalid("refusing to replace a foreign socket path")),
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => return Err(e),
        }
        Ok(Self {
            root,
            socket,
            instance_id: marker.instance_id,
            application_id: marker.application_id,
            token,
            epoch: Uuid::new_v4().simple().to_string(),
            _lock: lock,
        })
    }

    pub fn publish(&self) -> io::Result<()> {
        fs::set_permissions(&self.socket, fs::Permissions::from_mode(0o600))?;
        let path = self.root.join("daemon.json");
        // This file is descriptive only. The locked marker, peer UID, token,
        // and per-connection handshake establish the actual identity.
        if path.exists() {
            private_metadata(&path, false)?;
            fs::remove_file(&path)?;
        }
        create_file(
            &path,
            &serde_json::to_vec(&serde_json::json!({
                "protocol_version": crate::protocol::version(), "instance_id": self.instance_id,
                "application_id": self.application_id, "state_root": self.root,
                "service_epoch": self.epoch, "pid": std::process::id(), "browser_execution": false,
                "socket": self.socket,
            }))?,
        )
    }
}

impl Drop for State {
    fn drop(&mut self) {
        // Keep the lock until both instance-owned paths have been cleaned up.
        let _ = fs::remove_file(&self.socket);
        if let Some(runtime) = self.socket.parent().filter(|runtime| *runtime != self.root) {
            let _ = fs::remove_dir(runtime);
        }
        let descriptor = self.root.join("daemon.json");
        if let Ok(bytes) = read_private(&descriptor, 4096) {
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                if value["service_epoch"] == self.epoch {
                    let _ = fs::remove_file(descriptor);
                }
            }
        }
    }
}

#[cfg(test)]
mod sweep_tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn aged(path: &Path, age: Duration) {
        let file = std::fs::File::open(path).unwrap();
        file.set_modified(SystemTime::now() - age).unwrap();
    }

    #[test]
    fn removes_only_old_unused_profile_directories_of_the_exact_shape() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let day = Duration::from_secs(86400);
        for name in ["chromium-OLD123", "chromium-USED12", "chromium-FRESH1", "chromium-toolong1", "chromium-sh", "other-dir"] {
            std::fs::create_dir(root.join(name)).unwrap();
            std::fs::write(root.join(name).join("marker"), "x").unwrap();
        }
        for name in ["chromium-OLD123", "chromium-USED12", "chromium-toolong1", "chromium-sh", "other-dir"] {
            aged(&root.join(name), day);
        }
        // A symlink to a directory outside the root must never be followed or removed.
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("keep"), "x").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("chromium-LINK12")).unwrap();
        // A regular file with a profile-shaped name is not a profile.
        std::fs::write(root.join("chromium-FILE12"), "x").unwrap();
        // The path has spaces: naming the profile anywhere in the command line must be enough.
        let processes = "Google Chrome --headless --user-data-dir=/My State/browser/chromium-USED12 about:blank\n/bin/sh -c other";
        assert_eq!(sweep_stale_profiles(root, processes, SystemTime::now()), 1);
        assert!(!root.join("chromium-OLD123").exists());
        for kept in ["chromium-USED12", "chromium-FRESH1", "chromium-toolong1", "chromium-sh", "other-dir", "chromium-FILE12"] {
            assert!(root.join(kept).exists(), "{kept} must stay");
        }
        assert!(outside.path().join("keep").exists(), "a symlink target must not be removed");
    }

    #[test]
    fn the_process_list_is_available_on_this_host() {
        assert!(running_commands().is_some_and(|ps| ps.contains("kageha") || ps.lines().count() > 1));
    }
}
