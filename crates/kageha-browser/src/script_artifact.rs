//! A script supplies file content and a single name. Only the daemon chooses
//! the directory, and all publication is relative to opened directory fds.
use crate::cdp::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::ffi::{CStr, CString};
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub const MAX_ARTIFACTS: usize = 8;
pub const MAX_TEXT_BYTES: usize = 256 * 1024;
pub const MAX_DOWNLOAD_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_WORKER_MESSAGE: usize = 6 * MAX_TEXT_BYTES + 4096;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    path: PathBuf,
    device: u64,
    inode: u64,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Content {
    Text {
        text: String,
    },
    Json {
        data: Value,
    },
    Screenshot,
    #[serde(skip)]
    Download,
}

#[derive(Serialize)]
pub struct Artifact {
    path: String,
    mime_type: &'static str,
    size: usize,
}

pub struct Destination {
    directory: File,
    name: CString,
    path: String,
}

impl Destination {
    pub fn publish(self, mime_type: &'static str, bytes: &[u8]) -> Result<Artifact> {
        publish(&self.directory, &self.name, bytes)?;
        Ok(Artifact {
            path: self.path,
            mime_type,
            size: bytes.len(),
        })
    }
}

pub fn run_directory(request_id: &str) -> String {
    data_encoding::BASE64URL_NOPAD.encode(request_id.as_bytes())
}

pub fn validate_name(name: &str, content: &Content) -> Result<&'static str> {
    if name.is_empty()
        || name.len() > 96
        || !name.as_bytes()[0].is_ascii_alphanumeric()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        || name.contains("..")
    {
        return Err("invalid_script_artifact_name");
    }
    let extension = name.rsplit_once('.').map(|(_, extension)| extension);
    match (content, extension) {
        (Content::Text { .. }, Some("txt")) => Ok("text/plain"),
        (Content::Text { .. }, Some("md")) => Ok("text/markdown"),
        (Content::Text { .. }, Some("csv")) => Ok("text/csv"),
        (Content::Json { .. }, Some("json")) => Ok("application/json"),
        (Content::Screenshot, Some("jpg" | "jpeg")) => Ok("image/jpeg"),
        (Content::Download, Some("jpg" | "jpeg")) => Ok("image/jpeg"),
        (Content::Download, Some("png")) => Ok("image/png"),
        (Content::Download, Some("pdf")) => Ok("application/pdf"),
        (Content::Download, Some("zip")) => Ok("application/zip"),
        (Content::Download, Some("csv")) => Ok("text/csv"),
        (Content::Download, Some("txt")) => Ok("text/plain"),
        (Content::Download, Some("json")) => Ok("application/json"),
        (Content::Download, Some("bin")) => Ok("application/octet-stream"),
        _ => Err("unsupported_script_artifact_type"),
    }
}

fn open_directory(parent: &File, name: &CStr) -> io::Result<File> {
    let result = unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) };
    if result < 0 && io::Error::last_os_error().kind() != io::ErrorKind::AlreadyExists {
        return Err(io::Error::last_os_error());
    }
    open_owned_directory(parent, name)
}

pub(crate) fn open_owned_directory(parent: &File, name: &CStr) -> io::Result<File> {
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let file = unsafe { File::from_raw_fd(fd) };
    let metadata = file.metadata()?;
    if metadata.uid() != crate::state::uid() || metadata.mode() & 0o022 != 0 {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(file)
}

struct Staged<'a> {
    directory: &'a File,
    name: CString,
}

impl Drop for Staged<'_> {
    fn drop(&mut self) {
        unsafe { libc::unlinkat(self.directory.as_raw_fd(), self.name.as_ptr(), 0) };
    }
}

fn publish(directory: &File, name: &CStr, bytes: &[u8]) -> Result<()> {
    let stage = CString::new(format!(".{}.part", Uuid::new_v4().simple())).unwrap();
    let dir = directory.as_raw_fd();
    let fd = unsafe {
        libc::openat(
            dir,
            stage.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err("script_artifact_write_failed");
    }
    let staged = Staged {
        directory,
        name: stage,
    };
    let mut file = unsafe { File::from_raw_fd(fd) };
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| "script_artifact_write_failed")?;
    // linkat publishes a complete file and fails if the final name already
    // exists, including an existing symbolic link or directory.
    if unsafe { libc::linkat(dir, staged.name.as_ptr(), dir, name.as_ptr(), 0) } < 0 {
        return Err(
            if io::Error::last_os_error().kind() == io::ErrorKind::AlreadyExists {
                "script_artifact_exists"
            } else {
                "script_artifact_write_failed"
            },
        );
    }
    Ok(())
}

pub fn write(
    state: &Path,
    application: Option<&Workspace>,
    workspace_id: &str,
    request_id: &str,
    name: &str,
    mime_type: &'static str,
    bytes: &[u8],
) -> Result<Artifact> {
    prepare(state, application, workspace_id, request_id, name)?.publish(mime_type, bytes)
}

pub(crate) fn open_root(state: &Path, application: Option<&Workspace>) -> Result<File> {
    let root = application.map_or(state, |workspace| &workspace.path);
    if !root.is_absolute() {
        return Err("script_artifact_workspace_unavailable");
    }
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(root)
        .map_err(|_| "script_artifact_workspace_unavailable")?;
    let metadata = directory
        .metadata()
        .map_err(|_| "script_artifact_workspace_unavailable")?;
    if metadata.uid() != crate::state::uid()
        || metadata.mode() & 0o022 != 0
        || application.is_some_and(|workspace| {
            metadata.dev() != workspace.device || metadata.ino() != workspace.inode
        })
    {
        return Err("script_artifact_workspace_unavailable");
    }
    Ok(directory)
}

pub fn prepare(
    state: &Path,
    application: Option<&Workspace>,
    workspace_id: &str,
    request_id: &str,
    name: &str,
) -> Result<Destination> {
    let mut directory = open_root(state, application)?;
    let owner = if application.is_some() {
        "browser-script".into()
    } else {
        run_directory(workspace_id)
    };
    let run = run_directory(request_id);
    let components = ["artifacts", owner.as_str(), run.as_str()];
    for part in components {
        let component = CString::new(part).map_err(|_| "script_artifact_workspace_unavailable")?;
        directory = open_directory(&directory, &component)
            .map_err(|_| "script_artifact_workspace_unavailable")?;
    }
    let file = CString::new(name).map_err(|_| "invalid_script_artifact_name")?;
    let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            file.as_ptr(),
            metadata.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } == 0
    {
        return Err("script_artifact_exists");
    }
    if io::Error::last_os_error().kind() != io::ErrorKind::NotFound {
        return Err("script_artifact_workspace_unavailable");
    }
    Ok(Destination {
        directory,
        name: file,
        path: format!("{}/{name}", components.join("/")),
    })
}
