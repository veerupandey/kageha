//! Per-chat Task_Workspace: the directory where the agent reads, writes and runs code.
//!
//! Layout under the chat's private root:
//!
//! ```text
//! workspace/          Task_Workspace root
//!   todo.md           the agent's plan
//!   notes/  drafts/  outputs/
//!   inputs/           the user's attachments, placed by the Host (see below)
//!   .kageha/          Host-owned state (manifest, spilled output, job logs)
//! ```
//!
//! **Inputs.** [`TaskWorkspace::place_input`] copies a user upload, which the Host keeps
//! outside the workspace, to `inputs/<name>` so every tool, including the sandboxed shell,
//! can read it (Requirement 20.1). The manifest's `inputs` table records it, and
//! [`TaskWorkspace::scan_changes`] does not report an input while its bytes are the
//! upload's, so it is never an agent artifact. Once the agent changes or replaces an input
//! (shell, `write_file` or `edit_file`), it is released: the file is an ordinary agent
//! file from then on, reported and registered like any other, and the Host doesn't place
//! that upload again. An input the agent deletes is placed again next time. Inputs are
//! copies, not hard links: a hard link shares the upload's inode, so an in-place shell
//! write in the workspace would change the Host's copy outside the shell's write roots.
//! On APFS and other copy-on-write file systems the copy is cheap.
//!
//! Every model-supplied path goes through [`TaskWorkspace::resolve`], which rejects paths
//! that leave the root through `..`, absolute paths or symbolic links. Writes are atomic
//! (temp file then rename) and are performed relative to directory descriptors opened with
//! `O_NOFOLLOW`, so a symlink swapped in after resolution cannot redirect them. There are no
//! file or workspace size limits; callers check [`TaskWorkspace::free_disk_bytes`] against
//! [`DISK_LOW_BYTES`] instead.
use rusqlite::{params, Connection, ErrorCode, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    ffi::{CStr, CString, OsStr, OsString},
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        },
    },
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// Name of the Task_Workspace directory inside the chat's private root.
pub const WORKSPACE_DIR: &str = "workspace";
/// Host-owned state directory inside the workspace. File tools may read it but not write it.
pub const STATE_DIR: &str = ".kageha";
/// Below this much free space the Host pauses writes and asks the user (Requirement 3.7).
pub const DISK_LOW_BYTES: u64 = 5 * 1024 * 1024 * 1024;
/// Local connector API socket, relative to the workspace root (design section 3).
pub const API_SOCKET: &str = ".kageha/api.sock";
/// Per-turn connector API capability token, relative to the workspace root. Written with
/// mode 0600 by [`TaskWorkspace::write_host_file`] while a turn is active and removed with
/// [`TaskWorkspace::remove_host_file`] when it ends. The token is a file, not an environment
/// variable, because the shell environment drops every `*_TOKEN` name.
pub const API_TOKEN: &str = ".kageha/api-token";
/// The chat's saved shell state (working directory and non-secret exported variables),
/// relative to the workspace root. Written with mode 0600 after each foreground command and
/// read when a new shell session starts (`shell::state`).
pub const SHELL_ENV: &str = ".kageha/shell-env";
/// Directory of the user's attachments inside the workspace ([`TaskWorkspace::place_input`]).
pub const INPUTS_DIR: &str = "inputs";
const MANIFEST: &str = "manifest.sqlite";
const TODO: &str = "todo.md";
const TEMP_PREFIX: &str = ".kageha-tmp-";
const LAYOUT_DIRS: [&str; 6] = [
    "notes",
    "drafts",
    "outputs",
    STATE_DIR,
    ".kageha/tool-output",
    ".kageha/jobs",
];
/// Path-syntax sanity bound (PATH_MAX), not a file size limit.
const MAX_PATH_BYTES: usize = 4096;
const MAX_SYMLINK_HOPS: usize = 40;

/// A file created or changed in the workspace. `path` is relative to the root, `/`-separated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub sha256: String,
    pub size: u64,
}

/// A user upload to place in the workspace's `inputs/` ([`TaskWorkspace::place_input`]).
#[derive(Debug, Clone, Copy)]
pub struct InputSource<'a> {
    /// Stable identity of the upload, such as its chat-relative path `inputs/<id>-<name>`.
    pub key: &'a str,
    /// Preferred file name in `inputs/`: the upload's original name.
    pub name: &'a str,
    /// File name used when `name` is taken, such as the stored name `<id>-<name>`.
    pub fallback: &'a str,
    /// The upload's bytes on disk, outside the workspace. Opened without following a
    /// final symlink.
    pub file: &'a Path,
    /// Expected SHA-256 of the bytes (lowercase hex); a mismatch places nothing.
    pub sha256: &'a str,
}

/// An upload the workspace holds in `inputs/` with the upload's bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacedInput {
    /// The [`InputSource::key`] it was placed from.
    pub key: String,
    /// Workspace-relative path, `inputs/<name>`.
    pub path: String,
    pub sha256: String,
}

/// One exact replacement. `old` must occur exactly once in the current text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextEdit {
    pub old: String,
    pub new: String,
}

#[derive(Debug)]
pub enum WorkspaceError {
    /// The path resolves outside the workspace (via `..`, an absolute path or a symlink).
    PathOutsideWorkspace {
        path: String,
    },
    InvalidPath {
        path: String,
        reason: &'static str,
    },
    /// The path is inside the Host-owned `.kageha/` directory.
    ReservedPath {
        path: String,
    },
    NotFound {
        path: String,
    },
    NotAFile {
        path: String,
    },
    NotUtf8 {
        path: String,
    },
    /// Edit `index` did not match the file text.
    EditNotFound {
        path: String,
        index: usize,
    },
    /// Edit `index` matched more than once.
    EditAmbiguous {
        path: String,
        index: usize,
        matches: usize,
    },
    InvalidEdit {
        index: usize,
        reason: &'static str,
    },
    /// A layout entry exists but is not a real directory (for example a symlink).
    InvalidLayout {
        path: PathBuf,
    },
    Io(io::Error),
    Manifest(rusqlite::Error),
}

impl WorkspaceError {
    /// Stable error code for the tool error envelope.
    pub fn code(&self) -> &'static str {
        match self {
            Self::PathOutsideWorkspace { .. } => "path_outside_workspace",
            Self::InvalidPath { .. } => "invalid_path",
            Self::ReservedPath { .. } => "path_reserved",
            Self::NotFound { .. } => "file_not_found",
            Self::NotAFile { .. } => "not_a_file",
            Self::NotUtf8 { .. } => "not_utf8",
            Self::EditNotFound { .. } => "edit_not_found",
            Self::EditAmbiguous { .. } => "edit_ambiguous",
            Self::InvalidEdit { .. } => "invalid_edit",
            Self::InvalidLayout { .. } => "workspace_layout_invalid",
            Self::Io(_) => "io_error",
            Self::Manifest(_) => "manifest_error",
        }
    }
    /// Whether retrying the same call unchanged can succeed.
    pub fn retryable(&self) -> bool {
        match self {
            Self::Io(e) => matches!(
                e.kind(),
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ),
            Self::Manifest(rusqlite::Error::SqliteFailure(e, _)) => {
                matches!(e.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
            }
            _ => false,
        }
    }
}

impl fmt::Display for WorkspaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let code = self.code();
        match self {
            Self::PathOutsideWorkspace { path } => {
                write!(f, "{code}: `{path}` resolves outside the task workspace")
            }
            Self::InvalidPath { path, reason } => write!(f, "{code}: `{path}`: {reason}"),
            Self::ReservedPath { path } => {
                write!(f, "{code}: `{path}` is inside the host-owned {STATE_DIR}/ directory")
            }
            Self::NotFound { path } => write!(f, "{code}: `{path}` does not exist"),
            Self::NotAFile { path } => write!(f, "{code}: `{path}` is not a regular file"),
            Self::NotUtf8 { path } => write!(f, "{code}: `{path}` is not UTF-8 text"),
            Self::EditNotFound { path, index } => {
                write!(f, "{code}: edit {index}: `old` text not found in `{path}`")
            }
            Self::EditAmbiguous { path, index, matches } => write!(
                f,
                "{code}: edit {index}: `old` text matches {matches} times in `{path}`; include more context"
            ),
            Self::InvalidEdit { index, reason } => write!(f, "{code}: edit {index}: {reason}"),
            Self::InvalidLayout { path } => {
                write!(f, "{code}: {} is not a real directory", path.display())
            }
            Self::Io(e) => write!(f, "{code}: {e}"),
            Self::Manifest(e) => write!(f, "{code}: {e}"),
        }
    }
}

impl std::error::Error for WorkspaceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Manifest(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for WorkspaceError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<rusqlite::Error> for WorkspaceError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Manifest(e)
    }
}

impl From<WorkspaceError> for io::Error {
    fn from(e: WorkspaceError) -> Self {
        use io::ErrorKind::*;
        let kind = match &e {
            WorkspaceError::Io(inner) => return io::Error::new(inner.kind(), e.to_string()),
            WorkspaceError::PathOutsideWorkspace { .. } | WorkspaceError::ReservedPath { .. } => {
                PermissionDenied
            }
            WorkspaceError::NotFound { .. } => NotFound,
            WorkspaceError::NotUtf8 { .. } => InvalidData,
            WorkspaceError::Manifest(_) | WorkspaceError::InvalidLayout { .. } => Other,
            _ => InvalidInput,
        };
        io::Error::new(kind, e.to_string())
    }
}

type Result<T> = std::result::Result<T, WorkspaceError>;

/// A resolved write/edit target, relative to the root and free of symlinks.
struct Target {
    rel: String,
    /// Parent directory, relative to the root, or for a Host file ([`host_target`]) relative
    /// to the state directory.
    parent: PathBuf,
    name: OsString,
    /// A Host file under the state directory.
    host: bool,
}

pub struct TaskWorkspace {
    /// Canonical root.
    root: PathBuf,
    /// `(dev, ino)` of the root when opened; each operation checks it is unchanged.
    identity: (u64, u64),
    /// Canonical Host state directory: `<root>/.kageha`, or, for a workspace opened with
    /// [`TaskWorkspace::open_with_state`], a private directory outside the root.
    state: PathBuf,
    /// `(dev, ino)` of the state directory when opened.
    state_identity: (u64, u64),
}

/// Directory of delegated workers' nested workspaces inside a parent workspace:
/// `agents/<job_id>/` is a worker's Task_Workspace root with its own layout and manifest.
pub const AGENTS_DIR: &str = "agents";

impl TaskWorkspace {
    /// Opens (creating if needed) `<chat_root>/workspace` and its layout.
    pub fn open(chat_root: &Path) -> Result<Self> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(chat_root)?;
        let path = chat_root.canonicalize()?.join(WORKSPACE_DIR);
        ensure_dir(&path)?;
        Self::create_layout(path)
    }

    /// Opens (creating if needed) the nested workspace `<parent>/agents/<name>/` of a
    /// delegated worker, with its own layout and manifest. `name` must be one plain path
    /// segment. `agents/` and the worker root must be real directories (not symlinks), so
    /// the nested root is always inside the parent's canonical root. The parent's
    /// [`TaskWorkspace::scan_changes`] skips nested workspaces and its file writes can't
    /// enter them, so each file belongs to exactly one workspace.
    pub fn open_nested(parent: &TaskWorkspace, name: &str) -> Result<Self> {
        let invalid = |reason| WorkspaceError::InvalidPath { path: name.into(), reason };
        if name.is_empty() || name.len() > 255 || name == "." || name == ".." || name.starts_with(TEMP_PREFIX)
            || name.contains('/') || name.chars().any(char::is_control)
        {
            return Err(invalid("expected a plain directory name"));
        }
        if !parent.state_inside() {
            return Err(invalid("a workspace with external state cannot hold nested workspaces"));
        }
        parent.root_dir()?;
        let agents = parent.root.join(AGENTS_DIR);
        ensure_dir(&agents)?;
        let path = agents.join(name);
        ensure_dir(&path)?;
        if path.canonicalize()? != path {
            return Err(WorkspaceError::InvalidLayout { path });
        }
        Self::create_layout(path)
    }

    /// Opens a workspace whose root is an existing directory the Host does not own (a
    /// delegated worker's Git worktree) and whose Host state lives in `state`, outside the
    /// root. Nothing is created in the root: no layout directories, no `todo.md` and no
    /// `.kageha/`, so the checkout gets no untracked files. `state` is created with mode
    /// 0700. A new manifest records the files already in the root, so only files created
    /// or changed afterwards are reported as changes. Scans skip a top-level `.git`.
    pub fn open_with_state(root: &Path, state: &Path) -> Result<Self> {
        let root = root.canonicalize()?;
        if !fs::symlink_metadata(&root)?.is_dir() {
            return Err(WorkspaceError::InvalidLayout { path: root });
        }
        fs::DirBuilder::new().recursive(true).mode(0o700).create(state)?;
        let state = state.canonicalize()?;
        if state.starts_with(&root) || root.starts_with(&state) {
            return Err(WorkspaceError::InvalidLayout { path: state });
        }
        let meta = fs::symlink_metadata(&root)?;
        let state_meta = fs::symlink_metadata(&state)?;
        if !state_meta.is_dir() {
            return Err(WorkspaceError::InvalidLayout { path: state });
        }
        let workspace = Self {
            root,
            identity: (meta.dev(), meta.ino()),
            state_identity: (state_meta.dev(), state_meta.ino()),
            state,
        };
        for dir in ["tool-output", "jobs"] {
            ensure_dir(&workspace.state.join(dir))?;
        }
        let baseline = !workspace.state.join(MANIFEST).exists();
        workspace.manifest()?;
        if baseline {
            workspace.scan_changes()?;
        }
        Ok(workspace)
    }

    /// Opens an existing workspace without creating anything: `root` and `state` must be
    /// real directories, and `state` must be `<root>/.kageha` or lie outside `root`. Used by
    /// the shell, which only spills output and saves its state there.
    pub fn attach(root: &Path, state: &Path) -> Result<Self> {
        let root = root.canonicalize()?;
        let state = state.canonicalize()?;
        let inside = state == root.join(STATE_DIR);
        if !inside && (state.starts_with(&root) || root.starts_with(&state)) {
            return Err(WorkspaceError::InvalidLayout { path: state });
        }
        let meta = fs::symlink_metadata(&root)?;
        let state_meta = fs::symlink_metadata(&state)?;
        if !meta.is_dir() || !state_meta.is_dir() {
            return Err(WorkspaceError::InvalidLayout { path: state });
        }
        Ok(Self {
            root,
            identity: (meta.dev(), meta.ino()),
            state,
            state_identity: (state_meta.dev(), state_meta.ino()),
        })
    }

    /// The layout of a Host-owned root (`open` and `open_nested`): `.kageha/` inside it.
    fn create_layout(path: PathBuf) -> Result<Self> {
        let meta = fs::symlink_metadata(&path)?;
        let state = path.join(STATE_DIR);
        ensure_dir(&state)?;
        let state_meta = fs::symlink_metadata(&state)?;
        let workspace = Self {
            root: path,
            identity: (meta.dev(), meta.ino()),
            state,
            state_identity: (state_meta.dev(), state_meta.ino()),
        };
        for dir in LAYOUT_DIRS {
            ensure_dir(&workspace.root.join(dir))?;
        }
        let db = workspace.manifest()?;
        let todo = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(workspace.root.join(TODO));
        match todo {
            // Record the empty plan so the first scan does not report it as a change.
            Ok(file) => upsert(
                &db,
                &FileChange {
                    path: TODO.into(),
                    sha256: hex(&Sha256::digest(b"")),
                    size: 0,
                },
                mtime_ns(&file.metadata()?),
            )?,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
        Ok(workspace)
    }

    /// Canonical workspace root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Canonical Host state directory, which holds what `.kageha/` paths name.
    pub fn state_dir(&self) -> &Path {
        &self.state
    }

    /// Whether the state directory is `<root>/.kageha` (every workspace except one opened
    /// with [`TaskWorkspace::open_with_state`]).
    pub fn state_inside(&self) -> bool {
        self.state == self.root.join(STATE_DIR)
    }

    /// The absolute path of a Host state path such as [`API_SOCKET`] or
    /// `.kageha/tool-output/<file>`: `rel` must start with `.kageha/` and be a plain
    /// `/`-separated path. `None` otherwise.
    pub fn host_path(&self, rel: &str) -> Option<PathBuf> {
        let rest = rel.strip_prefix(STATE_DIR)?.strip_prefix('/')?;
        let plain = rest
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != ".." && !part.chars().any(char::is_control));
        plain.then(|| self.state.join(rest))
    }

    /// Whether `rel` (relative to the root, `/`-separated) is inside a nested worker
    /// workspace `agents/<name>/` (see [`TaskWorkspace::open_nested`]).
    fn in_nested(&self, rel: &Path) -> bool {
        if !self.state_inside() {
            return false;
        }
        let mut parts = rel.components();
        match (parts.next(), parts.next()) {
            (Some(Component::Normal(agents)), Some(Component::Normal(name))) if agents == AGENTS_DIR => {
                is_nested_root(&self.root.join(AGENTS_DIR).join(name))
            }
            _ => false,
        }
    }

    /// Resolves a model-supplied relative path. Rejects absolute paths, `..` segments that
    /// leave the root, and symlinks whose target leaves the root. Symlinks that stay inside
    /// are followed, so the returned path contains no symlinks in its existing components.
    /// Absolute symlink targets are accepted only when they start with the canonical root.
    pub fn resolve(&self, rel: &str) -> Result<PathBuf> {
        let invalid = |reason| WorkspaceError::InvalidPath {
            path: rel.into(),
            reason,
        };
        let outside = || WorkspaceError::PathOutsideWorkspace { path: rel.into() };
        if rel.is_empty() {
            return Err(invalid("path is empty"));
        }
        if rel.len() > MAX_PATH_BYTES {
            return Err(invalid("path is too long"));
        }
        if rel.chars().any(char::is_control) {
            return Err(invalid("path contains control characters"));
        }
        let rel_path = Path::new(rel);
        if rel_path.has_root() {
            return Err(outside());
        }
        let mut pending: VecDeque<OsString> = parts(rel_path).ok_or_else(outside)?;
        let mut current = self.root.clone();
        let mut hops = 0;
        while let Some(part) = pending.pop_front() {
            if part == ".." {
                if current == self.root {
                    return Err(outside());
                }
                current.pop();
                continue;
            }
            let candidate = current.join(&part);
            match fs::symlink_metadata(&candidate) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    hops += 1;
                    if hops > MAX_SYMLINK_HOPS {
                        return Err(invalid("too many levels of symbolic links"));
                    }
                    let target = fs::read_link(&candidate)?;
                    let target = if target.has_root() {
                        current = self.root.clone();
                        target
                            .strip_prefix(&self.root)
                            .map_err(|_| outside())?
                            .to_path_buf()
                    } else {
                        target
                    };
                    let mut expanded = parts(&target).ok_or_else(outside)?;
                    expanded.extend(pending);
                    pending = expanded;
                }
                Ok(_) => current = candidate,
                // Missing components cannot be symlinks; keep resolving lexically.
                Err(e)
                    if e.kind() == io::ErrorKind::NotFound
                        || e.raw_os_error() == Some(libc::ENOTDIR) =>
                {
                    current = candidate
                }
                Err(e) => return Err(e.into()),
            }
        }
        Ok(current)
    }

    /// Atomically creates or replaces a file, creating parent directories as needed.
    pub fn write(&self, rel: &str, bytes: &[u8]) -> Result<FileChange> {
        let target = self.target(rel)?;
        self.commit(&target, bytes)
    }

    /// Applies exact single-match replacements in order, then writes the result atomically.
    /// If any edit fails, the file is left unchanged.
    pub fn edit(&self, rel: &str, edits: &[TextEdit]) -> Result<FileChange> {
        if edits.is_empty() {
            return Err(WorkspaceError::InvalidEdit {
                index: 0,
                reason: "no edits supplied",
            });
        }
        let target = self.target(rel)?;
        let missing = |e: io::Error| {
            if e.kind() == io::ErrorKind::NotFound {
                WorkspaceError::NotFound { path: rel.into() }
            } else {
                nofollow_error(e, rel)
            }
        };
        let parent = self.open_parent(false, &target.parent, false).map_err(missing)?;
        let mut file = openat(
            &parent,
            &cstring(&target.name)?,
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            0,
        )
        .map_err(missing)?;
        if !file.metadata()?.is_file() {
            return Err(WorkspaceError::NotAFile { path: rel.into() });
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let mut text =
            String::from_utf8(bytes).map_err(|_| WorkspaceError::NotUtf8 { path: rel.into() })?;
        for (index, edit) in edits.iter().enumerate() {
            text = apply_edit(&text, edit, index, &target.rel)?;
        }
        self.commit(&target, text.as_bytes())
    }

    /// Diffs the manifest against disk and returns created or changed regular files outside
    /// `.kageha/`, sorted by path. Deleted files are dropped from the manifest. Symlinks and
    /// files with non-UTF-8 names are skipped. So are nested worker workspaces
    /// (`agents/<name>/` holding a `.kageha/` directory): their files belong to the worker.
    /// With external state, the root's own `.kageha/` is ordinary content and a top-level
    /// `.git` is skipped instead.
    pub fn scan_changes(&self) -> Result<Vec<FileChange>> {
        self.root_dir()?;
        let mut db = self.manifest()?;
        let known: HashMap<String, (i64, i64, String)> = {
            let mut statement = db.prepare("SELECT path, size, mtime_ns, sha256 FROM files")?;
            let rows = statement.query_map([], |row| {
                Ok((row.get(0)?, (row.get(1)?, row.get(2)?, row.get(3)?)))
            })?;
            rows.collect::<std::result::Result<_, _>>()?
        };
        let inputs = held_inputs(&db)?;
        let mut files = Vec::new();
        walk(&self.root, "", &mut files, self.state_inside())?;
        let tx = db.transaction()?;
        let mut seen = HashSet::new();
        let mut changes = Vec::new();
        for (rel, meta) in files {
            let size = meta.len();
            let mtime = mtime_ns(&meta);
            seen.insert(rel.clone());
            let previous = known.get(&rel);
            if previous.is_some_and(|(s, m, _)| *s == size as i64 && *m == mtime) {
                continue;
            }
            let Some(sha256) = hash_file(&self.root.join(&rel))? else {
                seen.remove(&rel);
                continue;
            };
            let change = FileChange {
                path: rel,
                sha256,
                size,
            };
            upsert(&tx, &change, mtime)?;
            // An input with the upload's bytes is the user's file, not a change. Changed,
            // it is released and becomes an ordinary agent file.
            match inputs.get(&change.path) {
                Some(sha) if *sha == change.sha256 => continue,
                Some(_) => release_input(&tx, &change.path)?,
                None => {}
            }
            if previous.is_none_or(|(_, _, sha)| *sha != change.sha256) {
                changes.push(change);
            }
        }
        for path in known.keys().filter(|path| !seen.contains(*path)) {
            tx.execute("DELETE FROM files WHERE path = ?1", params![path])?;
        }
        tx.commit()?;
        changes.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(changes)
    }

    /// The files the manifest currently records, sorted by path. Read-only: unlike
    /// [`Self::scan_changes`], it does not look at disk, so pending changes stay reportable.
    pub fn manifest_entries(&self) -> Result<Vec<FileChange>> {
        self.root_dir()?;
        let db = self.manifest()?;
        let mut statement = db.prepare("SELECT path, sha256, size FROM files ORDER BY path")?;
        let rows = statement.query_map([], |row| {
            Ok(FileChange {
                path: row.get(0)?,
                sha256: row.get(1)?,
                size: row.get::<_, i64>(2)?.max(0) as u64,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Places a user upload at `inputs/<name>`, or `inputs/<fallback>` when `<name>` is
    /// taken, with the upload's bytes (see the module docs). Idempotent per
    /// [`InputSource::key`]: an input already in place is left alone, one the agent deleted
    /// is placed again at the same path, and one the agent changed is released and never
    /// placed again. `None` when nothing holds the upload's bytes: the input was released,
    /// both names are taken, or the workspace keeps its state outside the root (a Git
    /// worktree gets no Host files). The copy is written to a temp file and linked into
    /// place without replacing anything, from directories opened with `O_NOFOLLOW`. Its
    /// SHA-256 must equal [`InputSource::sha256`].
    pub fn place_input(&self, source: &InputSource<'_>) -> Result<Option<PlacedInput>> {
        if !self.state_inside() {
            return Ok(None);
        }
        for name in [source.name, source.fallback] {
            if !plain_name(name) {
                return Err(WorkspaceError::InvalidPath { path: name.into(), reason: "expected a plain file name" });
            }
        }
        let db = self.manifest()?;
        let row: Option<(String, String, bool)> = db
            .query_row("SELECT path, sha256, released FROM inputs WHERE key = ?1", [source.key], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .optional()?;
        let placed = |path: String| PlacedInput { key: source.key.into(), path, sha256: source.sha256.into() };
        let dir = self
            .open_parent(false, Path::new(INPUTS_DIR), true)
            .map_err(|e| nofollow_error(e, INPUTS_DIR))?;
        let path = match row {
            Some((_, _, true)) => return Ok(None),
            Some((path, sha256, false)) => {
                let name = cstring(OsStr::new(&path[INPUTS_DIR.len() + 1..]))?;
                match existing_mode(&dir, &name, &path) {
                    Ok(None) if !exists_at(&dir, &name)? => path,
                    Ok(Some(_)) if sha256 == source.sha256 => {
                        // In place unless the agent changed it since the last scan.
                        let held = hash_file(&self.root.join(&path))?;
                        if held.as_deref() == Some(source.sha256) {
                            return Ok(Some(placed(path)));
                        }
                        release_input(&db, &path)?;
                        return Ok(None);
                    }
                    // A symlink, a directory, or an upload whose recorded bytes differ.
                    Ok(_) | Err(WorkspaceError::NotAFile { .. }) => {
                        release_input(&db, &path)?;
                        return Ok(None);
                    }
                    Err(e) => return Err(e),
                }
            }
            None => {
                let mut free = None;
                for name in [source.name, source.fallback] {
                    let path = format!("{INPUTS_DIR}/{name}");
                    let claimed: bool =
                        db.query_row("SELECT EXISTS(SELECT 1 FROM inputs WHERE path = ?1)", [&path], |row| row.get(0))?;
                    if !claimed && !exists_at(&dir, &cstring(OsStr::new(name))?)? {
                        free = Some(path);
                        break;
                    }
                }
                let Some(path) = free else { return Ok(None) };
                db.execute("INSERT INTO inputs(key, path, sha256) VALUES (?1, ?2, ?3)", params![source.key, path, source.sha256])?;
                path
            }
        };
        // The row exists before the file appears, so a concurrent scan never reports it.
        let name = cstring(OsStr::new(&path[INPUTS_DIR.len() + 1..]))?;
        let meta = match self.link_copy(&dir, &name, source) {
            Ok(meta) => meta,
            Err(e) => {
                // Taken meanwhile, or unreadable: this upload holds no path.
                db.execute("DELETE FROM inputs WHERE key = ?1", [source.key])?;
                return match e {
                    WorkspaceError::Io(ref io) if io.kind() == io::ErrorKind::AlreadyExists => Ok(None),
                    e => Err(e),
                };
            }
        };
        let change = FileChange { path: path.clone(), sha256: source.sha256.into(), size: meta.len() };
        upsert(&db, &change, mtime_ns(&meta))?;
        Ok(Some(placed(path)))
    }

    /// Copies `source` to a temp file in `dir`, checks its SHA-256 and links it as `name`
    /// without replacing an existing entry.
    fn link_copy(&self, dir: &File, name: &CStr, source: &InputSource<'_>) -> Result<fs::Metadata> {
        let mut from = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(source.file)?;
        if !from.metadata()?.is_file() {
            return Err(WorkspaceError::NotAFile { path: source.key.into() });
        }
        let (temp, mut file) = create_temp(dir)?;
        let copied = (|| -> Result<fs::Metadata> {
            let mut hasher = Sha256::new();
            let mut buffer = vec![0; 64 * 1024];
            loop {
                let n = from.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                hasher.update(&buffer[..n]);
                file.write_all(&buffer[..n])?;
            }
            if hex(&hasher.finalize()) != source.sha256 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "upload bytes changed").into());
            }
            file.sync_all()?;
            self.verify_parent(false, dir, Path::new(INPUTS_DIR))?;
            if unsafe { libc::linkat(dir.as_raw_fd(), temp.as_ptr(), dir.as_raw_fd(), name.as_ptr(), 0) } != 0 {
                return Err(io::Error::last_os_error().into());
            }
            Ok(file.metadata()?)
        })();
        unsafe { libc::unlinkat(dir.as_raw_fd(), temp.as_ptr(), 0) };
        let _ = dir.sync_all();
        copied
    }

    /// The uploads the workspace holds in `inputs/` and has not released, sorted by path.
    /// Read-only: it does not look at disk.
    pub fn inputs(&self) -> Result<Vec<PlacedInput>> {
        let db = self.manifest()?;
        let mut statement = db.prepare("SELECT key, path, sha256 FROM inputs WHERE released = 0 ORDER BY path")?;
        let rows = statement.query_map([], |row| {
            Ok(PlacedInput { key: row.get(0)?, path: row.get(1)?, sha256: row.get(2)? })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// Bytes available to unprivileged writers on the workspace's file system.
    pub fn free_disk_bytes(&self) -> Result<u64> {
        let path = CString::new(self.root.as_os_str().as_bytes())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(path.as_ptr(), &mut stat) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        // Field widths differ between macOS and Linux.
        #[allow(clippy::unnecessary_cast)]
        let free = (stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64);
        Ok(free)
    }

    fn target(&self, rel: &str) -> Result<Target> {
        let resolved = self.resolve(rel)?;
        let relative = resolved
            .strip_prefix(&self.root)
            .map_err(|_| WorkspaceError::PathOutsideWorkspace { path: rel.into() })?;
        let Some(name) = relative.file_name() else {
            return Err(WorkspaceError::InvalidPath {
                path: rel.into(),
                reason: "expected a file path inside the workspace",
            });
        };
        if self.state_inside()
            && relative.components().next() == Some(Component::Normal(OsStr::new(STATE_DIR)))
        {
            return Err(WorkspaceError::ReservedPath { path: rel.into() });
        }
        // A nested worker workspace belongs to the worker, not to this workspace.
        if self.in_nested(relative) {
            return Err(WorkspaceError::PathOutsideWorkspace { path: rel.into() });
        }
        let text = relative.to_str().ok_or(WorkspaceError::InvalidPath {
            path: rel.into(),
            reason: "path is not UTF-8",
        })?;
        Ok(Target {
            rel: text.into(),
            parent: relative.parent().unwrap_or(Path::new("")).to_path_buf(),
            name: name.to_os_string(),
            host: false,
        })
    }

    /// Atomically writes a Host-owned file under `.kageha/tool-output/` or `.kageha/jobs/`
    /// (spilled tool output, job logs), or the connector API token [`API_TOKEN`] or the
    /// saved shell state [`SHELL_ENV`], which are always written with mode 0600. File tools
    /// cannot write there; this is the Host's
    /// path. `rel` must be a plain `/`-separated path with no `.`, `..` or empty segments.
    /// Every directory is opened with `O_NOFOLLOW` from the pinned root, so a symlink in the
    /// path is an error rather than a redirect. The file is not recorded in the manifest,
    /// so it is never reported as a workspace change or artifact.
    pub fn write_host_file(&self, rel: &str, bytes: &[u8]) -> Result<FileChange> {
        let target = host_target(rel)?;
        let mode = (rel == API_TOKEN || rel == SHELL_ENV).then_some(0o600);
        self.replace_with_mode(&target, bytes, mode)?;
        Ok(FileChange {
            path: target.rel,
            sha256: hex(&Sha256::digest(bytes)),
            size: bytes.len() as u64,
        })
    }

    /// Reads a Host-owned file that [`TaskWorkspace::write_host_file`] accepts, walking
    /// from the pinned root with `O_NOFOLLOW`. `None` when it does not exist. A file larger
    /// than `limit` bytes is an error.
    pub fn read_host_file(&self, rel: &str, limit: usize) -> Result<Option<Vec<u8>>> {
        let target = host_target(rel)?;
        let parent = match self.open_parent(true, &target.parent, false) {
            Ok(parent) => parent,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(nofollow_error(e, &target.rel)),
        };
        let file = match openat(
            &parent,
            &cstring(&target.name)?,
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            0,
        ) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(nofollow_error(e, &target.rel)),
        };
        if !file.metadata()?.is_file() {
            return Err(WorkspaceError::NotAFile { path: target.rel });
        }
        let mut bytes = Vec::new();
        file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} exceeds {limit} bytes", target.rel),
            )
            .into());
        }
        Ok(Some(bytes))
    }

    /// Removes a Host-owned file that [`TaskWorkspace::write_host_file`] accepts. The
    /// parent is opened with `O_NOFOLLOW` from the pinned root, so a symlinked directory is
    /// an error rather than a redirect. Returns `false` when the file did not exist.
    pub fn remove_host_file(&self, rel: &str) -> Result<bool> {
        let target = host_target(rel)?;
        let parent = match self.open_parent(true, &target.parent, false) {
            Ok(parent) => parent,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(nofollow_error(e, &target.rel)),
        };
        let name = cstring(&target.name)?;
        if existing_mode(&parent, &name, &target.rel)?.is_none() {
            return Ok(false);
        }
        if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            let e = io::Error::last_os_error();
            return if e.kind() == io::ErrorKind::NotFound { Ok(false) } else { Err(e.into()) };
        }
        Ok(true)
    }

    fn commit(&self, target: &Target, bytes: &[u8]) -> Result<FileChange> {
        let meta = self.replace(target, bytes)?;
        let change = FileChange {
            path: target.rel.clone(),
            sha256: hex(&Sha256::digest(bytes)),
            size: bytes.len() as u64,
        };
        let db = self.manifest()?;
        upsert(&db, &change, mtime_ns(&meta))?;
        // `write_file`/`edit_file` with other bytes than the upload's releases an input.
        db.execute(
            "UPDATE inputs SET released = 1 WHERE path = ?1 AND sha256 <> ?2",
            params![change.path, change.sha256],
        )?;
        Ok(change)
    }

    /// Temp file then rename inside the parent held by descriptor.
    fn replace(&self, target: &Target, bytes: &[u8]) -> Result<fs::Metadata> {
        self.replace_with_mode(target, bytes, None)
    }

    /// Like [`Self::replace`]; `mode` overrides the existing file's mode when set.
    fn replace_with_mode(&self, target: &Target, bytes: &[u8], mode: Option<u32>) -> Result<fs::Metadata> {
        let parent = self
            .open_parent(target.host, &target.parent, true)
            .map_err(|e| nofollow_error(e, &target.rel))?;
        let name = cstring(&target.name)?;
        let existing = existing_mode(&parent, &name, &target.rel)?;
        let mode = mode.or(existing);
        let (temp, mut file) = create_temp(&parent)?;
        let written = (|| -> Result<fs::Metadata> {
            // Mode first, so private bytes are never readable through a wider mode.
            if let Some(mode) = mode {
                file.set_permissions(fs::Permissions::from_mode(mode))?;
            }
            file.write_all(bytes)?;
            file.sync_all()?;
            let meta = file.metadata()?;
            // Re-check after opening: the directory we hold must still be the one at the
            // resolved path inside the root, or a rename moved it elsewhere.
            self.verify_parent(target.host, &parent, &target.parent)?;
            if unsafe {
                libc::renameat(
                    parent.as_raw_fd(),
                    temp.as_ptr(),
                    parent.as_raw_fd(),
                    name.as_ptr(),
                )
            } != 0
            {
                return Err(io::Error::last_os_error().into());
            }
            Ok(meta)
        })();
        let meta = match written {
            Ok(meta) => meta,
            Err(e) => {
                unsafe { libc::unlinkat(parent.as_raw_fd(), temp.as_ptr(), 0) };
                return Err(e);
            }
        };
        let _ = parent.sync_all();
        Ok(meta)
    }

    fn root_dir(&self) -> Result<File> {
        let dir = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&self.root)?;
        let meta = dir.metadata()?;
        if (meta.dev(), meta.ino()) != self.identity {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "task workspace root was replaced",
            )
            .into());
        }
        Ok(dir)
    }

    /// The pinned state directory, opened without following a symlink.
    fn state_dir_file(&self) -> Result<File> {
        let dir = if self.state_inside() {
            openat(
                &self.root_dir()?,
                &cstring(OsStr::new(STATE_DIR))?,
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0,
            )
            .map_err(|e| nofollow_error(e, STATE_DIR))?
        } else {
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&self.state)?
        };
        let meta = dir.metadata()?;
        if (meta.dev(), meta.ino()) != self.state_identity {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "task workspace state directory was replaced",
            )
            .into());
        }
        Ok(dir)
    }

    /// Walks from the pinned root descriptor, or with `host` the pinned state directory,
    /// without following any symlink.
    fn open_parent(&self, host: bool, relative: &Path, create: bool) -> io::Result<File> {
        let mut dir = if host { self.state_dir_file() } else { self.root_dir() }.map_err(io::Error::from)?;
        for part in relative.components() {
            let Component::Normal(name) = part else {
                return Err(io::Error::from(io::ErrorKind::InvalidInput));
            };
            let name = cstring(name)?;
            if create && unsafe { libc::mkdirat(dir.as_raw_fd(), name.as_ptr(), 0o755) } != 0 {
                let e = io::Error::last_os_error();
                if e.kind() != io::ErrorKind::AlreadyExists {
                    return Err(e);
                }
            }
            dir = openat(
                &dir,
                &name,
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0,
            )?;
        }
        Ok(dir)
    }

    fn verify_parent(&self, host: bool, dir: &File, relative: &Path) -> Result<()> {
        let held = dir.metadata()?;
        let base = if host { &self.state } else { &self.root };
        let current = fs::metadata(base.join(relative))?;
        self.root_dir()?;
        if host {
            self.state_dir_file()?;
        }
        if (held.dev(), held.ino()) != (current.dev(), current.ino()) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "workspace directory moved during write",
            )
            .into());
        }
        Ok(())
    }

    fn manifest(&self) -> Result<Connection> {
        let path = self.state.join(MANIFEST);
        for check in [self.state.clone(), path.clone()] {
            match fs::symlink_metadata(&check) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(WorkspaceError::InvalidLayout { path: check })
                }
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound && check == path => {}
                Err(e) => return Err(e.into()),
            }
        }
        let db = Connection::open(&path)?;
        db.busy_timeout(Duration::from_secs(5))?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS files(
                path TEXT PRIMARY KEY,
                size INTEGER NOT NULL,
                mtime_ns INTEGER NOT NULL,
                sha256 TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS inputs(
                key TEXT PRIMARY KEY,
                path TEXT NOT NULL UNIQUE,
                sha256 TEXT NOT NULL,
                released INTEGER NOT NULL DEFAULT 0
            );",
        )?;
        Ok(db)
    }
}

/// Validates a Host-owned file path: a plain path under `.kageha/tool-output/` or
/// `.kageha/jobs/`, or exactly [`API_TOKEN`] or [`SHELL_ENV`].
fn host_target(rel: &str) -> Result<Target> {
    let invalid = |reason| WorkspaceError::InvalidPath {
        path: rel.into(),
        reason,
    };
    if rel.len() > MAX_PATH_BYTES {
        return Err(invalid("path is too long"));
    }
    let segments: Vec<&str> = rel.split('/').collect();
    if segments.iter().any(|part| {
        part.is_empty() || *part == "." || *part == ".." || part.chars().any(char::is_control)
    }) {
        return Err(invalid("expected a plain relative path"));
    }
    let host_owned = rel == API_TOKEN
        || rel == SHELL_ENV
        || segments.len() >= 3
            && segments[0] == STATE_DIR
            && matches!(segments[1], "tool-output" | "jobs");
    if !host_owned {
        return Err(invalid(
            "host files must be under .kageha/tool-output/ or .kageha/jobs/, or be .kageha/api-token or .kageha/shell-env",
        ));
    }
    let name = segments[segments.len() - 1];
    if name.starts_with(TEMP_PREFIX) {
        return Err(invalid("file name uses the reserved temp prefix"));
    }
    Ok(Target {
        rel: rel.into(),
        // Relative to the state directory: `.kageha/` itself is the state directory.
        parent: segments[1..segments.len() - 1].iter().collect(),
        name: name.into(),
        host: true,
    })
}

fn ensure_dir(path: &Path) -> Result<()> {
    match fs::DirBuilder::new().mode(0o755).create(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    if !fs::symlink_metadata(path)?.is_dir() {
        return Err(WorkspaceError::InvalidLayout { path: path.into() });
    }
    Ok(())
}

/// Path components as a queue; `None` if the path contains a root or prefix.
fn parts(path: &Path) -> Option<VecDeque<OsString>> {
    let mut out = VecDeque::new();
    for component in path.components() {
        match component {
            Component::Normal(name) => out.push_back(name.to_os_string()),
            Component::ParentDir => out.push_back("..".into()),
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(out)
}

fn apply_edit(text: &str, edit: &TextEdit, index: usize, path: &str) -> Result<String> {
    if edit.old.is_empty() {
        return Err(WorkspaceError::InvalidEdit {
            index,
            reason: "`old` must not be empty",
        });
    }
    let Some(first) = text.find(&edit.old) else {
        return Err(WorkspaceError::EditNotFound {
            path: path.into(),
            index,
        });
    };
    // Count overlapping matches so `aa` in `aaa` is ambiguous.
    let mut matches = 1;
    let mut from = first;
    while let Some(step) = text[from..].chars().next().map(char::len_utf8) {
        from += step;
        match text[from..].find(&edit.old) {
            Some(offset) => {
                matches += 1;
                from += offset;
            }
            None => break,
        }
    }
    if matches > 1 {
        return Err(WorkspaceError::EditAmbiguous {
            path: path.into(),
            index,
            matches,
        });
    }
    let mut out = String::with_capacity(text.len() - edit.old.len() + edit.new.len());
    out.push_str(&text[..first]);
    out.push_str(&edit.new);
    out.push_str(&text[first + edit.old.len()..]);
    Ok(out)
}

/// Mode of an existing regular file at `name`, `None` if absent or a symlink (replaced).
// `mode_t` is u16 on macOS and u32 on Linux.
#[allow(clippy::unnecessary_cast)]
fn existing_mode(dir: &File, name: &CStr, rel: &str) -> Result<Option<u32>> {
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            name.as_ptr(),
            &mut stat,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        let e = io::Error::last_os_error();
        return if e.kind() == io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(e.into())
        };
    }
    let mode = stat.st_mode as u32;
    match mode & libc::S_IFMT as u32 {
        m if m == libc::S_IFREG as u32 => Ok(Some(mode & 0o7777)),
        m if m == libc::S_IFLNK as u32 => Ok(None),
        _ => Err(WorkspaceError::NotAFile { path: rel.into() }),
    }
}

fn create_temp(dir: &File) -> io::Result<(CString, File)> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    loop {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let name = CString::new(format!("{TEMP_PREFIX}{}-{nanos}-{n}", std::process::id()))?;
        match openat(
            dir,
            &name,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o666,
        ) {
            Ok(file) => return Ok((name, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
}

fn openat(dir: &File, name: &CStr, flags: libc::c_int, mode: libc::c_uint) -> io::Result<File> {
    let fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags, mode) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn cstring(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

/// A symlink met while walking with `O_NOFOLLOW` means the path changed after resolution.
fn nofollow_error(e: io::Error, rel: &str) -> WorkspaceError {
    if e.raw_os_error() == Some(libc::ELOOP) {
        WorkspaceError::PathOutsideWorkspace { path: rel.into() }
    } else {
        e.into()
    }
}

/// Whether `state` is a Task_Workspace state directory: it holds a manifest the Host made
/// when it opened the workspace.
pub fn has_manifest(state: &Path) -> bool {
    fs::symlink_metadata(state).is_ok_and(|meta| meta.is_dir())
        && fs::symlink_metadata(state.join(MANIFEST)).is_ok_and(|meta| meta.is_file())
}

/// Whether `dir` is a nested worker workspace root: a real directory holding a real
/// `.kageha/` directory.
pub fn is_nested_root(dir: &Path) -> bool {
    fs::symlink_metadata(dir).is_ok_and(|meta| meta.is_dir())
        && fs::symlink_metadata(dir.join(STATE_DIR)).is_ok_and(|meta| meta.is_dir())
}

/// Directory names holding installed dependencies or tool caches. Files under them are
/// not changes, artifacts or listed workspace files (Requirement 20.8).
pub const DEPENDENCY_DIRS: &[&str] = &[
    "venv", ".venv", "node_modules", "__pycache__", "site-packages", "pylibs", ".cache",
    ".git", ".mypy_cache", ".pytest_cache",
];

/// Whether one path segment names a dependency or cache directory: one of
/// [`DEPENDENCY_DIRS`], or a name ending in `.dist-info` or `.egg-info`.
pub fn is_dependency_dir(name: &str) -> bool {
    DEPENDENCY_DIRS.contains(&name) || name.ends_with(".dist-info") || name.ends_with(".egg-info")
}

/// Whether a `/`-separated workspace-relative path lies under a dependency or cache
/// directory ([`is_dependency_dir`] for any directory segment).
pub fn is_dependency_path(rel: &str) -> bool {
    let mut segments: Vec<&str> = rel.split('/').collect();
    segments.pop();
    segments.into_iter().any(is_dependency_dir)
}

/// Collects regular files under `dir`, skipping temp files, symlinks and dependency or
/// cache directories ([`is_dependency_dir`]). With
/// `host_layout` (state inside the root) it skips the top-level `.kageha/` and nested worker
/// workspaces `agents/<name>/`; otherwise a top-level `.git`.
fn walk(dir: &Path, prefix: &str, out: &mut Vec<(String, fs::Metadata)>, host_layout: bool) -> io::Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if prefix.is_empty() => return Err(e),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
            ) =>
        {
            return Ok(())
        }
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let skipped = if host_layout {
            (prefix.is_empty() && name == STATE_DIR)
                || (prefix == AGENTS_DIR && is_nested_root(&entry.path()))
        } else {
            prefix.is_empty() && name == ".git"
        };
        if skipped || name.starts_with(TEMP_PREFIX) {
            continue;
        }
        let rel = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        // DirEntry::metadata does not follow symlinks.
        let meta = match entry.metadata() {
            Ok(meta) => meta,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        if meta.is_dir() {
            if is_dependency_dir(rel.rsplit('/').next().unwrap_or(&rel)) {
                continue;
            }
            walk(&entry.path(), &rel, out, host_layout)?;
        } else if meta.is_file() {
            out.push((rel, meta));
        }
    }
    Ok(())
}

/// Streams a file's SHA-256. `None` if it vanished or stopped being a regular file.
fn hash_file(path: &Path) -> io::Result<Option<String>> {
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound || e.raw_os_error() == Some(libc::ELOOP) => {
            return Ok(None)
        }
        Err(e) => return Err(e),
    };
    if !file.metadata()?.is_file() {
        return Ok(None);
    }
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(Some(hex(&hasher.finalize())))
}

fn upsert(db: &Connection, change: &FileChange, mtime_ns: i64) -> rusqlite::Result<()> {
    db.execute(
        "INSERT INTO files(path, size, mtime_ns, sha256) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(path) DO UPDATE SET size = excluded.size, mtime_ns = excluded.mtime_ns,
         sha256 = excluded.sha256",
        params![change.path, change.size as i64, mtime_ns, change.sha256],
    )?;
    Ok(())
}

/// Path -> upload SHA-256 of the inputs the workspace has not released.
fn held_inputs(db: &Connection) -> rusqlite::Result<HashMap<String, String>> {
    let mut statement = db.prepare("SELECT path, sha256 FROM inputs WHERE released = 0")?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect()
}

/// Marks the input at `path` as the agent's file (see the module docs).
fn release_input(db: &Connection, path: &str) -> rusqlite::Result<()> {
    db.execute("UPDATE inputs SET released = 1 WHERE path = ?1", [path])?;
    Ok(())
}

/// One plain path segment that a write may create: not `.`, `..`, empty, a temp name, or
/// with a `/` or control character.
fn plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name != "."
        && name != ".."
        && !name.starts_with(TEMP_PREFIX)
        && !name.contains('/')
        && !name.chars().any(char::is_control)
}

/// Whether `dir` has any entry called `name`, without following a symlink.
fn exists_at(dir: &File, name: &CStr) -> io::Result<bool> {
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatat(dir.as_raw_fd(), name.as_ptr(), &mut stat, libc::AT_SYMLINK_NOFOLLOW) } == 0 {
        return Ok(true);
    }
    let e = io::Error::last_os_error();
    if e.kind() == io::ErrorKind::NotFound {
        Ok(false)
    } else {
        Err(e)
    }
}

fn mtime_ns(meta: &fs::Metadata) -> i64 {
    meta.mtime()
        .saturating_mul(1_000_000_000)
        .saturating_add(meta.mtime_nsec())
}

fn hex(digest: &[u8]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn workspace() -> (tempfile::TempDir, TaskWorkspace) {
        let dir = tempfile::tempdir().unwrap();
        let workspace = TaskWorkspace::open(&dir.path().join("chat")).unwrap();
        (dir, workspace)
    }

    fn sha(bytes: &[u8]) -> String {
        hex(&Sha256::digest(bytes))
    }

    fn edit(old: &str, new: &str) -> TextEdit {
        TextEdit {
            old: old.into(),
            new: new.into(),
        }
    }

    #[test]
    fn open_creates_layout_and_is_idempotent() {
        let (dir, workspace) = workspace();
        let root = workspace.root();
        assert_eq!(
            root,
            dir.path().canonicalize().unwrap().join("chat/workspace")
        );
        assert!(root.join("todo.md").is_file());
        for sub in [
            "notes",
            "drafts",
            "outputs",
            ".kageha",
            ".kageha/tool-output",
            ".kageha/jobs",
        ] {
            assert!(root.join(sub).is_dir(), "{sub}");
        }
        assert!(root.join(".kageha/manifest.sqlite").is_file());
        // The freshly created todo.md is in the manifest, so it is not reported as a change.
        assert!(workspace.scan_changes().unwrap().is_empty());

        fs::write(root.join("todo.md"), "- step\n").unwrap();
        let reopened = TaskWorkspace::open(&dir.path().join("chat")).unwrap();
        assert_eq!(
            fs::read_to_string(reopened.root().join("todo.md")).unwrap(),
            "- step\n"
        );
    }

    #[test]
    fn open_rejects_symlinked_state_dir() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::create_dir_all(dir.path().join("chat/workspace")).unwrap();
        symlink(&outside, dir.path().join("chat/workspace/.kageha")).unwrap();
        let err = TaskWorkspace::open(&dir.path().join("chat")).err().unwrap();
        assert_eq!(err.code(), "workspace_layout_invalid");
    }

    #[test]
    fn resolve_confines_paths() {
        let (dir, workspace) = workspace();
        let root = workspace.root().to_path_buf();
        let outside = dir.path().canonicalize().unwrap().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret"), "x").unwrap();
        symlink(&outside, root.join("out-dir")).unwrap();
        symlink(outside.join("secret"), root.join("out-file")).unwrap();
        symlink("../../outside", root.join("out-relative")).unwrap();
        symlink("notes", root.join("in-dir")).unwrap();
        symlink(root.join("outputs"), root.join("in-absolute")).unwrap();

        for bad in [
            "..",
            "../x",
            "notes/../../x",
            "/etc/passwd",
            "out-dir",
            "out-dir/new.txt",
            "out-file",
            "out-relative/secret",
            "in-dir/../../x",
        ] {
            let err = workspace.resolve(bad).unwrap_err();
            assert_eq!(err.code(), "path_outside_workspace", "{bad}");
        }
        assert_eq!(
            workspace.resolve("notes/../outputs/a.md").unwrap(),
            root.join("outputs/a.md")
        );
        assert_eq!(
            workspace.resolve("./new/deep/file.txt").unwrap(),
            root.join("new/deep/file.txt")
        );
        assert_eq!(
            workspace.resolve("in-dir/p.md").unwrap(),
            root.join("notes/p.md")
        );
        assert_eq!(
            workspace.resolve("in-absolute/r.md").unwrap(),
            root.join("outputs/r.md")
        );
        assert_eq!(
            workspace.resolve("missing/../todo.md").unwrap(),
            root.join("todo.md")
        );
        for invalid in ["", "a\0b", "a\nb"] {
            assert_eq!(
                workspace.resolve(invalid).unwrap_err().code(),
                "invalid_path"
            );
        }
    }

    #[test]
    fn resolve_rejects_symlink_loops() {
        let (_dir, workspace) = workspace();
        symlink("loop-b", workspace.root().join("loop-a")).unwrap();
        symlink("loop-a", workspace.root().join("loop-b")).unwrap();
        assert_eq!(
            workspace.resolve("loop-a").unwrap_err().code(),
            "invalid_path"
        );
    }

    #[test]
    fn write_is_atomic_and_confined() {
        let (dir, workspace) = workspace();
        let root = workspace.root().to_path_buf();
        let change = workspace
            .write("outputs/report/final.md", b"hello")
            .unwrap();
        assert_eq!(
            change,
            FileChange {
                path: "outputs/report/final.md".into(),
                sha256: sha(b"hello"),
                size: 5
            }
        );
        assert_eq!(
            fs::read(root.join("outputs/report/final.md")).unwrap(),
            b"hello"
        );

        fs::set_permissions(
            root.join("outputs/report/final.md"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        workspace
            .write("outputs/report/final.md", b"again")
            .unwrap();
        let meta = fs::metadata(root.join("outputs/report/final.md")).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert_eq!(
            fs::read(root.join("outputs/report/final.md")).unwrap(),
            b"again"
        );

        // Internal symlinks resolve to the real file and report its real path.
        symlink("outputs/report/final.md", root.join("alias.md")).unwrap();
        assert_eq!(
            workspace.write("alias.md", b"via alias").unwrap().path,
            "outputs/report/final.md"
        );
        assert!(fs::symlink_metadata(root.join("alias.md"))
            .unwrap()
            .file_type()
            .is_symlink());

        let outside = dir.path().join("victim.txt");
        fs::write(&outside, "original").unwrap();
        symlink(&outside, root.join("escape.txt")).unwrap();
        assert_eq!(
            workspace.write("escape.txt", b"pwned").unwrap_err().code(),
            "path_outside_workspace"
        );
        assert_eq!(
            workspace
                .write("../victim.txt", b"pwned")
                .unwrap_err()
                .code(),
            "path_outside_workspace"
        );
        assert_eq!(fs::read_to_string(&outside).unwrap(), "original");

        assert_eq!(
            workspace
                .write(".kageha/manifest.sqlite", b"x")
                .unwrap_err()
                .code(),
            "path_reserved"
        );
        assert_eq!(
            workspace.write(".", b"x").unwrap_err().code(),
            "invalid_path"
        );
        assert_eq!(
            workspace.write("notes", b"x").unwrap_err().code(),
            "not_a_file"
        );

        // No temp files are left behind.
        let leftovers: Vec<_> = fs::read_dir(root.join("outputs/report"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(TEMP_PREFIX))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn write_has_no_size_limit() {
        let (_dir, workspace) = workspace();
        let bytes = vec![b'k'; 20 * 1024 * 1024];
        let change = workspace.write("outputs/big.bin", &bytes).unwrap();
        assert_eq!(change.size, bytes.len() as u64);
        assert_eq!(change.sha256, sha(&bytes));
    }

    #[test]
    fn edit_requires_exactly_one_match() {
        let (_dir, workspace) = workspace();
        workspace
            .write("notes/a.md", "alpha beta gamma beta\n".as_bytes())
            .unwrap();

        let change = workspace
            .edit(
                "notes/a.md",
                &[edit("alpha", "ALPHA"), edit("gamma beta", "GAMMA")],
            )
            .unwrap();
        let text = fs::read_to_string(workspace.root().join("notes/a.md")).unwrap();
        assert_eq!(text, "ALPHA beta GAMMA\n");
        assert_eq!(change.sha256, sha(text.as_bytes()));

        let err = workspace
            .edit("notes/a.md", &[edit("ALPHA", "x"), edit("missing", "y")])
            .unwrap_err();
        assert!(
            matches!(err, WorkspaceError::EditNotFound { index: 1, .. }),
            "{err}"
        );
        // A failed batch changes nothing.
        assert_eq!(
            fs::read_to_string(workspace.root().join("notes/a.md")).unwrap(),
            text
        );

        workspace.write("notes/b.md", b"aaa").unwrap();
        let err = workspace
            .edit("notes/b.md", &[edit("aa", "b")])
            .unwrap_err();
        assert!(
            matches!(err, WorkspaceError::EditAmbiguous { matches: 2, .. }),
            "{err}"
        );
        assert_eq!(
            workspace
                .edit("notes/b.md", &[edit("", "b")])
                .unwrap_err()
                .code(),
            "invalid_edit"
        );
        assert_eq!(
            workspace.edit("notes/b.md", &[]).unwrap_err().code(),
            "invalid_edit"
        );
        assert_eq!(
            workspace
                .edit("notes/none.md", &[edit("a", "b")])
                .unwrap_err()
                .code(),
            "file_not_found"
        );

        workspace.write("notes/bin", &[0xff, 0xfe]).unwrap();
        assert_eq!(
            workspace
                .edit("notes/bin", &[edit("a", "b")])
                .unwrap_err()
                .code(),
            "not_utf8"
        );
    }

    #[test]
    fn scan_skips_dependency_and_cache_directories() {
        let (_dir, workspace) = workspace();
        let root = workspace.root().to_path_buf();
        for rel in [
            "venv/lib/libx.dylib", ".venv/bin/python", "node_modules/a/index.js",
            "pylibs/numpy/core.so", "src/__pycache__/m.pyc", "lib/site-packages/p.py",
            "pkg-1.0.dist-info/METADATA", "x.egg-info/PKG-INFO", ".cache/pip/x", "sub/.git/HEAD",
            "outputs/report.md", "src/venv.py",
        ] {
            fs::create_dir_all(root.join(rel).parent().unwrap()).unwrap();
            fs::write(root.join(rel), rel).unwrap();
        }
        let paths: Vec<_> = workspace.scan_changes().unwrap().into_iter().map(|c| c.path).collect();
        assert_eq!(paths, ["outputs/report.md", "src/venv.py"]);
        assert!(is_dependency_path("a/node_modules/b.js") && !is_dependency_path("node_modules"));
    }

    proptest::proptest! {
        /// **Validates: Requirements 20.8**
        #[test]
        fn dependency_segments_anywhere_are_skipped(
            head in proptest::collection::vec("[a-z]{1,6}", 0..3),
            dep in proptest::sample::select(DEPENDENCY_DIRS.to_vec()),
            tail in proptest::collection::vec("[a-z]{1,6}", 0..3),
            file in "[a-z]{1,6}\\.txt",
        ) {
            let mut parts: Vec<String> = head.clone();
            parts.push(dep.to_string());
            parts.extend(tail.clone());
            parts.push(file.clone());
            proptest::prop_assert!(is_dependency_path(&parts.join("/")));
            let mut plain = head;
            plain.extend(tail);
            plain.push(file);
            proptest::prop_assert!(!is_dependency_path(&plain.join("/")));
        }
    }

    #[test]
    fn scan_reports_created_and_changed_files_only() {
        let (_dir, workspace) = workspace();
        let root = workspace.root().to_path_buf();
        workspace.write("outputs/a.txt", b"one").unwrap();
        assert!(
            workspace.scan_changes().unwrap().is_empty(),
            "write updates the manifest"
        );

        fs::create_dir_all(root.join("build/out")).unwrap();
        fs::write(root.join("build/out/b.txt"), "two").unwrap();
        fs::write(root.join("outputs/a.txt"), "uno").unwrap();
        fs::write(root.join(".kageha/tool-output/call.txt"), "ignored").unwrap();
        symlink("outputs/a.txt", root.join("link.txt")).unwrap();
        let changes = workspace.scan_changes().unwrap();
        assert_eq!(
            changes,
            vec![
                FileChange {
                    path: "build/out/b.txt".into(),
                    sha256: sha(b"two"),
                    size: 3
                },
                FileChange {
                    path: "outputs/a.txt".into(),
                    sha256: sha(b"uno"),
                    size: 3
                },
            ]
        );
        assert!(workspace.scan_changes().unwrap().is_empty());

        // Rewriting identical bytes changes mtime but not content: no change reported.
        std::thread::sleep(Duration::from_millis(20));
        fs::write(root.join("build/out/b.txt"), "two").unwrap();
        assert!(workspace.scan_changes().unwrap().is_empty());

        // A deleted then recreated file is reported as created again.
        fs::remove_file(root.join("build/out/b.txt")).unwrap();
        assert!(workspace.scan_changes().unwrap().is_empty());
        fs::write(root.join("build/out/b.txt"), "two").unwrap();
        assert_eq!(workspace.scan_changes().unwrap().len(), 1);
    }

    #[test]
    fn manifest_entries_list_recorded_files_without_scanning() {
        let (_dir, workspace) = workspace();
        let todo = FileChange {
            path: "todo.md".into(),
            sha256: sha(b""),
            size: 0,
        };
        assert_eq!(workspace.manifest_entries().unwrap(), [todo.clone()]);
        let written = workspace.write("outputs/a.txt", b"one").unwrap();
        assert_eq!(workspace.manifest_entries().unwrap(), [written, todo]);
        // A file changed outside the tools is not in the manifest until a scan, and the
        // read leaves the change for the scan to report.
        fs::write(workspace.root().join("notes/n.txt"), "x").unwrap();
        assert_eq!(workspace.manifest_entries().unwrap().len(), 2);
        assert_eq!(workspace.scan_changes().unwrap().len(), 1);
        assert_eq!(workspace.manifest_entries().unwrap().len(), 3);
    }

    #[test]
    fn host_files_are_confined_to_spill_and_job_dirs_and_not_in_the_manifest() {
        let (dir, workspace) = workspace();
        let root = workspace.root().to_path_buf();
        let bytes = "🌲".repeat(10_000);
        let change = workspace
            .write_host_file(".kageha/tool-output/call_1.txt", bytes.as_bytes())
            .unwrap();
        assert_eq!(change.size, bytes.len() as u64);
        assert_eq!(change.sha256, sha(bytes.as_bytes()));
        assert_eq!(
            fs::read(root.join(".kageha/tool-output/call_1.txt")).unwrap(),
            bytes.as_bytes()
        );
        // Replacing is atomic and nested job directories are created.
        workspace
            .write_host_file(".kageha/tool-output/call_1.txt", b"second")
            .unwrap();
        assert_eq!(
            fs::read(root.join(".kageha/tool-output/call_1.txt")).unwrap(),
            b"second"
        );
        workspace
            .write_host_file(".kageha/jobs/j1/stdout.log", b"log")
            .unwrap();
        assert!(root.join(".kageha/jobs/j1/stdout.log").is_file());
        // Host files are not workspace changes or manifest entries.
        assert!(workspace.scan_changes().unwrap().is_empty());
        assert_eq!(workspace.manifest_entries().unwrap().len(), 1);

        for bad in [
            ".kageha/manifest.sqlite",
            ".kageha/tool-output",
            ".kageha/other/x.txt",
            "outputs/x.txt",
            ".kageha/tool-output/../manifest.sqlite",
            ".kageha/tool-output/./x.txt",
            ".kageha//tool-output/x.txt",
            "/.kageha/tool-output/x.txt",
            ".kageha/tool-output/.kageha-tmp-x",
        ] {
            let err = workspace.write_host_file(bad, b"x").unwrap_err();
            assert_eq!(err.code(), "invalid_path", "{bad}");
        }
        // A symlinked directory in the path is refused, not followed.
        let outside = dir.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, root.join(".kageha/jobs/linked")).unwrap();
        assert!(workspace
            .write_host_file(".kageha/jobs/linked/x.txt", b"x")
            .is_err());
        assert!(!outside.join("x.txt").exists());
        // File tools still cannot write host-owned paths.
        assert_eq!(
            workspace
                .write(".kageha/tool-output/y.txt", b"x")
                .unwrap_err()
                .code(),
            "path_reserved"
        );
    }
    #[test]
    fn api_token_is_a_private_host_file_that_can_be_removed() {
        let (dir, workspace) = workspace();
        let root = workspace.root().to_path_buf();
        workspace.write_host_file(API_TOKEN, b"secret-1").unwrap();
        let path = root.join(API_TOKEN);
        assert_eq!(fs::read(&path).unwrap(), b"secret-1");
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        // Rotation keeps the mode even if something widened it.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        workspace.write_host_file(API_TOKEN, b"secret-2").unwrap();
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(workspace.scan_changes().unwrap().is_empty());
        assert!(workspace.remove_host_file(API_TOKEN).unwrap());
        assert!(!path.exists());
        assert!(!workspace.remove_host_file(API_TOKEN).unwrap());
        // Only the exact token path is accepted next to the state files.
        for bad in [".kageha/api-token/x", ".kageha/api.sock", ".kageha/manifest.sqlite", "todo.md"] {
            assert_eq!(workspace.write_host_file(bad, b"x").unwrap_err().code(), "invalid_path", "{bad}");
            assert_eq!(workspace.remove_host_file(bad).unwrap_err().code(), "invalid_path", "{bad}");
        }
        // A symlinked state directory is refused, not followed.
        let outside = dir.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("api-token"), b"keep").unwrap();
        fs::rename(root.join(STATE_DIR), dir.path().join("moved")).unwrap();
        symlink(&outside, root.join(STATE_DIR)).unwrap();
        assert!(workspace.remove_host_file(API_TOKEN).is_err());
        assert!(workspace.write_host_file(API_TOKEN, b"x").is_err());
        assert_eq!(fs::read(outside.join("api-token")).unwrap(), b"keep");
    }
    #[test]
    fn free_disk_bytes_reports_space() {
        let (_dir, workspace) = workspace();
        assert!(workspace.free_disk_bytes().unwrap() > 0);
    }

    #[test]
    fn errors_convert_to_io_with_code() {
        let err: io::Error = WorkspaceError::PathOutsideWorkspace {
            path: "../x".into(),
        }
        .into();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(err.to_string().starts_with("path_outside_workspace:"));
        assert!(!WorkspaceError::NotFound { path: "x".into() }.retryable());
    }

    #[test]
    fn nested_worker_workspaces_have_their_own_layout_and_parent_scans_skip_them() {
        let (_dir, parent) = workspace();
        parent.write("outputs/brief.md", b"brief").unwrap();
        let worker = TaskWorkspace::open_nested(&parent, "job1").unwrap();
        assert_eq!(worker.root(), parent.root().join("agents/job1"));
        assert!(worker.state_inside());
        for sub in ["todo.md", "outputs", ".kageha/tool-output", ".kageha/manifest.sqlite"] {
            assert!(worker.root().join(sub).exists(), "{sub}");
        }
        // The worker's files are its own changes, never the parent's.
        let change = worker.write("outputs/report.md", b"report").unwrap();
        assert_eq!(change.path, "outputs/report.md");
        fs::write(worker.root().join("notes/shell.txt"), "from the shell").unwrap();
        assert_eq!(worker.scan_changes().unwrap().iter().map(|c| c.path.as_str()).collect::<Vec<_>>(), ["notes/shell.txt"]);
        let parent_changes = parent.scan_changes().unwrap();
        assert!(parent_changes.iter().all(|c| !c.path.starts_with("agents/")), "{parent_changes:?}");
        // A plain `agents/` file of the parent is still the parent's.
        fs::write(parent.root().join("agents/readme.md"), "mine").unwrap();
        assert_eq!(parent.scan_changes().unwrap().iter().map(|c| c.path.as_str()).collect::<Vec<_>>(), ["agents/readme.md"]);
        // The parent can read the worker's files but not write into its workspace.
        assert!(parent.resolve("agents/job1/outputs/report.md").unwrap().is_file());
        for path in ["agents/job1/outputs/x.md", "agents/job1/.kageha/api-token"] {
            assert_eq!(parent.write(path, b"x").unwrap_err().code(), "path_outside_workspace", "{path}");
        }
        // Reopening is idempotent; bad names and symlinked directories are refused.
        assert_eq!(TaskWorkspace::open_nested(&parent, "job1").unwrap().root(), worker.root());
        for name in ["", ".", "..", "a/b", ".kageha-tmp-x"] {
            assert!(TaskWorkspace::open_nested(&parent, name).is_err(), "{name:?}");
        }
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), parent.root().join("agents/linked")).unwrap();
        assert!(TaskWorkspace::open_nested(&parent, "linked").is_err());
        assert!(fs::read_dir(outside.path()).unwrap().next().is_none());
    }
    #[test]
    fn a_workspace_with_external_state_adds_nothing_to_its_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("checkout");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join(".kageha/rules")).unwrap();
        fs::write(root.join("src/lib.rs"), "fn main() {}").unwrap();
        fs::write(root.join(".git"), "gitdir: elsewhere").unwrap();
        fs::write(root.join(".kageha/rules/style.md"), "tracked rule").unwrap();
        let state = dir.path().join("private/state");
        let workspace = TaskWorkspace::open_with_state(&root, &state).unwrap();
        assert!(!workspace.state_inside());
        assert_eq!(workspace.state_dir(), state.canonicalize().unwrap());
        let mut names: Vec<_> = fs::read_dir(&root).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        names.sort();
        assert_eq!(names, [".git", ".kageha", "src"]);
        assert_eq!(fs::read_dir(root.join(".kageha")).unwrap().count(), 1);
        assert!(state.join("manifest.sqlite").is_file() && state.join("tool-output").is_dir() && state.join("jobs").is_dir());
        assert_eq!(fs::metadata(&state).unwrap().permissions().mode() & 0o777, 0o700);
        // Files already in the checkout are the baseline, not changes; `.git` is skipped.
        assert!(workspace.scan_changes().unwrap().is_empty());
        assert!(workspace.manifest_entries().unwrap().iter().all(|c| c.path != ".git"));
        workspace.write("src/new.rs", b"// new").unwrap();
        fs::write(root.join("src/lib.rs"), "fn main() { run() }").unwrap();
        // The checkout's own `.kageha/` is ordinary content.
        workspace.write(".kageha/rules/new.md", b"rule").unwrap();
        let changed: Vec<_> = workspace.scan_changes().unwrap().into_iter().map(|c| c.path).collect();
        assert_eq!(changed, ["src/lib.rs"]);
        // Host files go to the state directory.
        workspace.write_host_file(".kageha/tool-output/call.txt", b"spilled").unwrap();
        workspace.write_host_file(API_TOKEN, b"token").unwrap();
        assert_eq!(fs::read(state.join("tool-output/call.txt")).unwrap(), b"spilled");
        assert_eq!(workspace.read_host_file(API_TOKEN, 64).unwrap().unwrap(), b"token");
        assert_eq!(workspace.host_path(API_SOCKET).unwrap(), workspace.state_dir().join("api.sock"));
        assert!(workspace.host_path("outputs/x").is_none() && workspace.host_path(".kageha/../x").is_none());
        assert!(!root.join(".kageha/tool-output").exists() && !root.join(".kageha/api-token").exists());
        assert!(workspace.remove_host_file(API_TOKEN).unwrap());
        // Reopening keeps the manifest; attach opens without creating anything.
        let reopened = TaskWorkspace::open_with_state(&root, &state).unwrap();
        assert!(reopened.scan_changes().unwrap().is_empty());
        let attached = TaskWorkspace::attach(&root, &state).unwrap();
        assert_eq!((attached.root(), attached.state_dir()), (workspace.root(), workspace.state_dir()));
        // State inside the root, or a root inside the state, is refused.
        assert!(TaskWorkspace::open_with_state(&root, &root.join("state")).is_err());
        fs::create_dir(state.join("inner")).unwrap();
        assert!(TaskWorkspace::open_with_state(&state.join("inner"), &state).is_err());
    }
}
