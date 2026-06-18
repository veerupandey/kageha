//! Independent workspace read capabilities and bounded document/process execution.
//! Hosts supply roots, worker executables, ownership and authority; this library has no WebUI/database dependency.
pub mod documents;
pub mod process;
pub mod projects;
pub mod worktrees;
pub mod hooks;
pub mod brain;
pub mod worker;
pub mod handoff;
pub mod task;
pub mod shell;
pub mod api_cli;
use kageha_contracts::{execution::*, policy::ActionClass};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    ffi::CString,
    fs::{File, OpenOptions},
    io::{self, Read},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt},
        },
    },
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
pub const TEXT_LIMIT: usize = 65536;
pub const DOCUMENT_LIMIT: usize = 16 * 1024 * 1024;
pub const EXTRACT_LIMIT: usize = 1024 * 1024;
/// A directory capability pins the actual root, including when paths are renamed.
pub struct ReadWorkspace {
    root: PathBuf,
    directory: File,
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn denied(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
impl ReadWorkspace {
    pub fn new(root: &Path) -> io::Result<Self> {
        let root = root.canonicalize()?;
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&root)?;
        Ok(Self { root, directory })
    }
    fn open(&self, path: &str) -> io::Result<File> {
        self.open_confined(path, false)
    }
    pub fn open_visible_file(&self, path: &str) -> io::Result<File> {
        self.open_confined(path, true)
    }
    fn open_confined(&self, path: &str, visible_only: bool) -> io::Result<File> {
        if path.is_empty() || path.len() > 4096 || path.chars().any(char::is_control) {
            return Err(denied("Invalid workspace path"));
        }
        // Resolve legitimate internal symlinks, then walk from the pinned directory descriptor without following any more links.
        let resolved = self.root.join(path).canonicalize()?;
        let relative = resolved
            .strip_prefix(&self.root)
            .map_err(|_| denied("Workspace path escapes root"))?;
        let mut parent = self.directory.try_clone()?;
        let parts = relative.components().collect::<Vec<_>>();
        if parts.is_empty() {
            return Err(denied("Expected workspace file"));
        }
        for (index, part) in parts.iter().enumerate() {
            let std::path::Component::Normal(name) = part else {
                return Err(denied("Invalid workspace path"));
            };
            if visible_only && name.as_bytes().starts_with(b".") {
                return Err(denied("Expected a visible workspace file"));
            }
            let name =
                CString::new(name.as_bytes()).map_err(|_| denied("Invalid workspace path"))?;
            let flags = libc::O_RDONLY
                | libc::O_NOFOLLOW
                | libc::O_CLOEXEC
                | libc::O_NONBLOCK
                | if index + 1 == parts.len() {
                    0
                } else {
                    libc::O_DIRECTORY
                };
            let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            parent = unsafe { File::from_raw_fd(fd) };
        }
        if !parent.metadata()?.is_file() {
            return Err(denied("Expected regular workspace file"));
        }
        Ok(parent)
    }
    pub fn snapshot(
        &self,
        path: &str,
        limit: usize,
        control: &Control<'_>,
    ) -> io::Result<Snapshot> {
        control.check()?;
        snapshot(self.open(path)?, limit, control)
    }
    pub fn read_text(&self, path: &str, control: &Control<'_>) -> io::Result<String> {
        String::from_utf8(self.snapshot(path, TEXT_LIMIT, control)?.bytes)
            .map_err(|_| io::Error::other("Expected a UTF-8 workspace file"))
    }
    /// A UTF-8 window of at most `len` bytes starting near byte `offset`. Files of any size
    /// can be read this way, a window at a time. The window is trimmed to character
    /// boundaries: `start` skips continuation bytes at `offset` and `end` drops a character
    /// split by the window, so `end` is the offset to read next.
    pub fn read_text_range(
        &self,
        path: &str,
        offset: u64,
        len: usize,
        control: &Control<'_>,
    ) -> io::Result<TextRange> {
        use std::io::{Seek, SeekFrom};
        control.check()?;
        let mut file = self.open(path)?;
        let total = file.metadata()?.len();
        if offset > total {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("offset {offset} is past the end of the file ({total} bytes)"),
            ));
        }
        file.seek(SeekFrom::Start(offset))?;
        let wanted = (total - offset).min(len as u64) as usize;
        let mut bytes = vec![0; wanted];
        let mut filled = 0;
        while filled < wanted {
            control.check()?;
            match file.read(&mut bytes[filled..])? {
                0 => break,
                n => filled += n,
            }
        }
        bytes.truncate(filled);
        let skipped = bytes
            .iter()
            .take(3)
            .take_while(|byte| (**byte & 0b1100_0000) == 0b1000_0000)
            .count();
        let window = &bytes[skipped..];
        let text = match std::str::from_utf8(window) {
            Ok(text) => text,
            // A character split by the end of the window is read with the next window.
            Err(error) if error.error_len().is_none() && offset + (filled as u64) < total => {
                std::str::from_utf8(&window[..error.valid_up_to()]).unwrap_or_default()
            }
            Err(_) => return Err(io::Error::other("Expected a UTF-8 workspace file")),
        };
        let start = offset + skipped as u64;
        Ok(TextRange {
            text: text.to_owned(),
            start,
            end: start + text.len() as u64,
            total,
        })
    }
    fn arguments<'a>(&self, call: &'a ToolCall) -> io::Result<&'a str> {
        call.validate()?;
        if call.name != "read_file" || call.arguments.as_object().is_none_or(|a| a.len() != 1) {
            return Err(denied("Expected read_file with only a workspace path"));
        }
        call.arguments["path"]
            .as_str()
            .ok_or_else(|| denied("Supply a workspace path"))
    }
}
pub struct Snapshot {
    pub bytes: Vec<u8>,
    pub sha256: String,
}
/// One window of a text file; see [`ReadWorkspace::read_text_range`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextRange {
    pub text: String,
    /// Byte offset of `text` in the file.
    pub start: u64,
    /// Byte offset just past `text`; read from here for the next window.
    pub end: u64,
    /// File size in bytes.
    pub total: u64,
}
fn snapshot(mut file: File, limit: usize, control: &Control<'_>) -> io::Result<Snapshot> {
    if limit > DOCUMENT_LIMIT {
        return Err(io::Error::other("Workspace read limit exceeds 16 MiB"));
    }
    control.check()?;
    let before = file.metadata()?;
    if !before.is_file() || before.len() > limit as u64 {
        return Err(io::Error::other(
            "Expected a regular file within the configured read limit",
        ));
    }
    let mut bytes = Vec::new();
    let mut buffer = [0; 16384];
    loop {
        control.check()?;
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        if bytes.len() + n > limit {
            return Err(io::Error::other("File exceeds configured read limit"));
        }
        bytes.extend_from_slice(&buffer[..n]);
    }
    let after = file.metadata()?;
    if (
        before.dev(),
        before.ino(),
        before.len(),
        before.mtime(),
        before.mtime_nsec(),
    ) != (
        after.dev(),
        after.ino(),
        after.len(),
        after.mtime(),
        after.mtime_nsec(),
    ) {
        return Err(io::Error::other("Workspace file changed during read"));
    }
    control.check()?;
    Ok(Snapshot {
        sha256: hash(&bytes),
        bytes,
    })
}
/// An already host-authorized exact file. Authorization/ownership is deliberately not inferred here.
pub fn file_snapshot(
    path: &Path,
    expected_sha256: Option<&str>,
    control: &Control<'_>,
) -> io::Result<Snapshot> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    let result = snapshot(file, DOCUMENT_LIMIT, control)?;
    if expected_sha256.is_some_and(|expected| expected != result.sha256) {
        return Err(denied("Authorized file bytes changed"));
    }
    Ok(result)
}
impl ToolPort for ReadWorkspace {
    fn dispatch_access(&self, _: &ToolCall) -> io::Result<DispatchAccess> {
        Ok(DispatchAccess::Resource(String::new()))
    }
    fn binding(&self, _: &ToolContext, call: &ToolCall) -> io::Result<ToolBinding> {
        let path = self.arguments(call)?;
        let file = self.open(path)?;
        let root = self.directory.metadata()?;
        let info = file.metadata()?;
        Ok(ToolBinding{binding:"tools.workspace".into(),generation:1,resource:path.into(),class:ActionClass::ReadOnly,detail:format!("Read workspace file {path}"),fingerprint:hash(json!({"root":self.root,"root_device":root.dev(),"root_inode":root.ino(),"file_device":info.dev(),"file_inode":info.ino()}).to_string().as_bytes())})
    }
    fn discover(&self) -> io::Result<Vec<serde_json::Value>> {
        Ok(vec![
            json!({"type":"function","function":{"name":"read_file","description":"Read a UTF-8 file inside the configured workspace","parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}}}),
        ])
    }
    fn execute(&self, call: &ToolCall, control: &Control<'_>) -> io::Result<ToolReply> {
        control.check()?;
        Ok(ToolReply::success(
            self.read_text(self.arguments(call)?, control)?,
        ))
    }
    fn execute_authorized_in(
        &self,
        context: &ToolContext,
        call: &ToolCall,
        authority: &ToolBinding,
        control: &Control<'_>,
    ) -> io::Result<ToolReply> {
        if self.binding(context, call)? != *authority {
            return Err(denied("Workspace identity changed after approval"));
        }
        let path = self.arguments(call)?;
        let file = self.open(path)?;
        let root = self.directory.metadata()?;
        let info = file.metadata()?;
        let fingerprint=hash(json!({"root":self.root,"root_device":root.dev(),"root_inode":root.ino(),"file_device":info.dev(),"file_inode":info.ino()}).to_string().as_bytes());
        if fingerprint != authority.fingerprint {
            return Err(denied("Workspace file identity changed before read"));
        }
        let bytes = snapshot(file, TEXT_LIMIT, control)?.bytes;
        Ok(ToolReply::success(String::from_utf8(bytes).map_err(
            |_| io::Error::other("Expected UTF-8 workspace text"),
        )?))
    }
}
/// An operator-selected worker executable, never model-supplied code or a path lookup fallback.
pub struct DocumentWorker {
    program: PathBuf,
}
impl DocumentWorker {
    pub fn new(program: &Path) -> io::Result<Self> {
        let program = program.canonicalize()?;
        if !program.is_file() {
            return Err(io::Error::other("Document worker unavailable"));
        }
        if kageha_sandbox::required()? && !kageha_sandbox::available() {
            return Err(denied("Required document worker sandbox is unavailable"));
        }
        Ok(Self { program })
    }
    pub fn read(
        &self,
        path: &Path,
        expected_sha256: Option<&str>,
        control: &Control<'_>,
    ) -> io::Result<String> {
        let snapshot = file_snapshot(path, expected_sha256, control)?;
        self.extract(
            snapshot.bytes,
            path.extension().and_then(|s| s.to_str()).unwrap_or("txt"),
            control,
        )
    }
    pub fn extract(
        &self,
        bytes: Vec<u8>,
        suffix: &str,
        control: &Control<'_>,
    ) -> io::Result<String> {
        if bytes.len() > DOCUMENT_LIMIT {
            return Err(io::Error::other("Document exceeds 16 MiB"));
        }
        let normalized = suffix.to_ascii_lowercase();
        let suffix = match normalized.as_str() {
            "pdf" | "docx" | "pptx" | "xlsx" | "xlsm" | "html" | "htm" | "xml" => {
                normalized.as_str()
            }
            _ => "txt",
        };
        let mut command = if kageha_sandbox::required()? {
            kageha_sandbox::ProcessSandbox::default().command(
                self.program
                    .to_str()
                    .ok_or_else(|| denied("Invalid worker executable path"))?,
                &[],
            )?
        } else {
            std::process::Command::new(&self.program)
        };
        command.env_clear();
        command.arg("extract-document-stdin").arg(suffix);
        let deadline = Instant::now() + Duration::from_secs(20);
        let bounded = Control {
            cancelled: control.cancelled,
            deadline: Some(control.deadline.unwrap_or(deadline).min(deadline)),
        };
        let result = process::run(&mut command, bytes, &bounded, EXTRACT_LIMIT)?;
        if !result.success {
            return Err(io::Error::other("Native document extraction failed; encrypted or image-only files may need a password or OCR"));
        }
        String::from_utf8(result.bytes).map_err(io::Error::other)
    }
}

/// Optional read_document tool. The host chooses both root and native worker.
pub struct DocumentTools {
    workspace: ReadWorkspace,
    worker: DocumentWorker,
}
impl DocumentTools {
    pub fn new(root: &Path, worker: &Path) -> io::Result<Self> {
        Ok(Self {
            workspace: ReadWorkspace::new(root)?,
            worker: DocumentWorker::new(worker)?,
        })
    }
    fn normalized(&self, call: &ToolCall) -> io::Result<ToolCall> {
        if call.name != "read_document" {
            return Err(denied("Unknown document tool"));
        }
        let normalized = ToolCall {
            name: "read_file".into(),
            ..call.clone()
        };
        self.workspace.arguments(&normalized)?;
        Ok(normalized)
    }
    fn fingerprint(&self, file: &File) -> io::Result<String> {
        let root = self.workspace.directory.metadata()?;
        let file = file.metadata()?;
        Ok(hash(json!({"root_device":root.dev(),"root_inode":root.ino(),"file_device":file.dev(),"file_inode":file.ino(),"worker":self.worker.program}).to_string().as_bytes()))
    }
}
impl ToolPort for DocumentTools {
    fn discover(&self) -> io::Result<Vec<serde_json::Value>> {
        Ok(vec![
            json!({"type":"function","function":{"name":"read_document","description":"Extract text from a PDF, Office or text document inside the configured workspace using an isolated native worker.","parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}}}),
        ])
    }
    fn binding(&self, _: &ToolContext, call: &ToolCall) -> io::Result<ToolBinding> {
        let normalized = self.normalized(call)?;
        let path = self.workspace.arguments(&normalized)?;
        let file = self.workspace.open(path)?;
        Ok(ToolBinding {
            binding: "tools.workspace.documents".into(),
            generation: 1,
            resource: path.into(),
            class: ActionClass::ReadOnly,
            detail: format!("Read workspace document {path}"),
            fingerprint: self.fingerprint(&file)?,
        })
    }
    fn dispatch_access(&self, _: &ToolCall) -> io::Result<DispatchAccess> {
        Ok(DispatchAccess::Resource(String::new()))
    }
    fn execute(&self, call: &ToolCall, control: &Control<'_>) -> io::Result<ToolReply> {
        let context = ToolContext {
            session: "embedded".into(),
            turn: "read".into(),
        };
        let authority = self.binding(&context, call)?;
        self.execute_authorized_in(&context, call, &authority, control)
    }
    fn execute_authorized_in(
        &self,
        _: &ToolContext,
        call: &ToolCall,
        authority: &ToolBinding,
        control: &Control<'_>,
    ) -> io::Result<ToolReply> {
        control.check()?;
        let normalized = self.normalized(call)?;
        let path = self.workspace.arguments(&normalized)?;
        let file = self.workspace.open(path)?;
        let current = ToolBinding {
            binding: "tools.workspace.documents".into(),
            generation: 1,
            resource: path.into(),
            class: ActionClass::ReadOnly,
            detail: format!("Read workspace document {path}"),
            fingerprint: self.fingerprint(&file)?,
        };
        if current != *authority {
            return Err(denied("Document authority changed before read"));
        }
        let bytes = snapshot(file, DOCUMENT_LIMIT, control)?.bytes;
        let suffix = Path::new(path)
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("txt");
        Ok(ToolReply::success(
            self.worker.extract(bytes, suffix, control)?,
        ))
    }
}
/// Apply limits only inside a dedicated worker process, never inside an embedding host.
pub fn worker_limits() {
    unsafe {
        let address = libc::rlimit {
            rlim_cur: 1024 * 1024 * 1024,
            rlim_max: 1024 * 1024 * 1024,
        };
        let _ = libc::setrlimit(libc::RLIMIT_AS, &address);
        let cpu = libc::rlimit {
            rlim_cur: 20,
            rlim_max: 21,
        };
        let _ = libc::setrlimit(libc::RLIMIT_CPU, &cpu);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn text_ranges_cover_a_large_file_on_character_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let text = format!("{}{}", "a".repeat(70_000), "🌲é".repeat(30_000));
        std::fs::write(dir.path().join("big.txt"), &text).unwrap();
        let workspace = ReadWorkspace::new(dir.path()).unwrap();
        let cancelled = AtomicBool::new(false);
        let control = Control { cancelled: &cancelled, deadline: None };
        // Beyond the whole-file read limit, but readable a window at a time.
        assert!(workspace.read_text("big.txt", &control).is_err());
        let mut joined = String::new();
        let mut offset = 0;
        loop {
            let range = workspace.read_text_range("big.txt", offset, 4097, &control).unwrap();
            assert_eq!((range.start, range.total), (offset, text.len() as u64));
            assert!(range.text.len() <= 4097);
            joined.push_str(&range.text);
            if range.end == range.total {
                break;
            }
            assert!(range.end > offset);
            offset = range.end;
        }
        assert_eq!(joined, text);
        // An offset inside a character starts at the next one.
        let inside = 70_001;
        let range = workspace.read_text_range("big.txt", inside, 64, &control).unwrap();
        assert_eq!(range.start, 70_004);
        assert!(range.text.starts_with('é'));
        assert!(workspace
            .read_text_range("big.txt", text.len() as u64 + 1, 64, &control)
            .is_err());
        assert!(workspace.read_text_range("../big.txt", 0, 64, &control).is_err());
    }
}
