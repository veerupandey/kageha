//! Bounded, owner-authorized worker handoffs. Hosts bind proposals to an owner,
//! store them privately and journal dispatch before calling `apply`.
//!
//! Every path is relative to a pinned root. Symlinks and host-internal paths are
//! rejected. Conflicts are checked before writing and again during installation.
//! Ordinary failures trigger a best-effort rollback; a process crash or concurrent
//! external writer can leave an uncertain outcome, which the host must reconcile
//! rather than replay automatically.
use crate::process;
use kageha_contracts::execution::Control;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, ffi::{CString, OsStr}, fs::{self, File, OpenOptions}, io::{self, Read, Write}, os::{fd::{AsRawFd, FromRawFd}, unix::{ffi::OsStrExt, fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt}}}, path::{Component, Path, PathBuf}, process::Command, sync::{atomic::{AtomicU64, Ordering}, Mutex}, time::{Duration, Instant}};

const MAX_FILES: usize = 128;
const MAX_BYTES: usize = 4 * 1024 * 1024;
static APPLY_LOCK: Mutex<()> = Mutex::new(());
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
fn fail(s: &str) -> io::Error { io::Error::other(s) }
fn hash(bytes: &[u8]) -> String { format!("{:x}", Sha256::digest(bytes)) }

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileMapping { pub source: String, pub destination: String }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileVersion { pub sha256: String, pub bytes: Vec<u8>, pub executable: bool, pub mode: u32, pub uid: u32, pub gid: u32, pub provenance: Option<Vec<u8>> }
impl FileVersion {
    fn new(bytes: Vec<u8>, mode: u32, uid: u32, gid: u32, provenance: Option<Vec<u8>>) -> Self { Self { sha256: hash(&bytes), bytes, executable: mode & 0o111 != 0, mode, uid, gid, provenance } }
    fn valid(&self) -> bool { self.bytes.len() <= MAX_BYTES && self.sha256 == hash(&self.bytes) && self.mode <= 0o777 && self.executable == (self.mode & 0o111 != 0) && self.provenance.as_ref().is_none_or(|v|v.len()<=1024) }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Change { pub source: String, pub path: String, pub source_snapshot: Option<FileVersion>, pub before: Option<FileVersion>, pub after: Option<FileVersion> }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Root { pub path: PathBuf, pub device: u64, pub inode: u64 }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Proposal { pub version: u32, pub source: Root, pub target: Root, pub base: Option<String>, pub changes: Vec<Change>, pub conflicts: Vec<String> }
impl Proposal {
    /// Public preview: bounded text changes and hashes, without absolute host paths.
    pub fn summary(&self) -> serde_json::Value {
        const PER_FILE_TEXT: usize = 8 * 1024;
        const TOTAL_TEXT: usize = 64 * 1024;
        fn text(value: &Option<FileVersion>, remaining: &mut usize) -> (Option<String>, bool, bool) {
            let Some(value) = value else { return (None, false, false) };
            let Ok(text) = std::str::from_utf8(&value.bytes) else { return (None, false, true) };
            if text.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')) { return (None, false, true); }
            let mut end = text.len().min(PER_FILE_TEXT).min(*remaining);
            while !text.is_char_boundary(end) { end -= 1; }
            *remaining -= end;
            (Some(text[..end].to_owned()), end < text.len(), false)
        }
        let mut remaining = TOTAL_TEXT;
        let files: Vec<_> = self.changes.iter().map(|change| {
            let kind = match (&change.before, &change.after) {
                (None, Some(_)) => "added", (Some(_), None) => "deleted",
                (Some(before), Some(after)) if before == after => "unchanged", _ => "modified",
            };
            let (before, before_truncated, before_binary) = text(&change.before, &mut remaining);
            let (after, after_truncated, after_binary) = text(&change.after, &mut remaining);
            serde_json::json!({"path":change.path,"change":kind,
                "before_sha256":change.before.as_ref().map(|v| &v.sha256),
                "after_sha256":change.after.as_ref().map(|v| &v.sha256),
                "before_mode":change.before.as_ref().map(|v| v.mode),"after_mode":change.after.as_ref().map(|v| v.mode),
                "before_text":before,"after_text":after,
                "truncated":before_truncated || after_truncated,
                "binary":before_binary || after_binary})
        }).collect();
        serde_json::json!({"files":files,"conflicts":self.conflicts})
    }
    pub fn has_conflicts(&self) -> bool { !self.conflicts.is_empty() }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApplyReceipt { pub paths: Vec<String> }
#[derive(Clone, Debug, Serialize, Deserialize)]
struct MadeDir { path: String, device: u64, inode: u64 }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Identity { device: u64, inode: u64 }

fn checked_path(path: &str) -> io::Result<Vec<&OsStr>> {
    if path.is_empty() || path.len() > 4096 || path.chars().any(char::is_control) { return Err(fail("Invalid handoff path")); }
    let mut parts = Vec::new();
    for part in Path::new(path).components() {
        let Component::Normal(part) = part else { return Err(fail("Handoff paths must be relative and contain no traversal")); };
        if matches!(part.to_str(), Some(".git" | ".kageha" | "agents")) { return Err(fail("Handoff path names reserved state")); }
        parts.push(part);
    }
    if parts.is_empty() { return Err(fail("Invalid handoff path")); }
    Ok(parts)
}
fn user_path(path: &str) -> io::Result<()> {
    if checked_path(path)?.iter().any(|part| part.as_bytes().starts_with(b".kageha-handoff-")) {
        return Err(fail("Handoff path names reserved staging state"));
    }
    Ok(())
}
fn cstr(value: &OsStr) -> io::Result<CString> { CString::new(value.as_bytes()).map_err(|_| fail("Invalid handoff path")) }
fn root(path: &Path) -> io::Result<(Root, File)> {
    let path = path.canonicalize()?;
    let file = OpenOptions::new().read(true).custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC).open(&path)?;
    let metadata = file.metadata()?;
    Ok((Root { path, device: metadata.dev(), inode: metadata.ino() }, file))
}
fn open_root(expected: &Root) -> io::Result<File> {
    let (actual, file) = root(&expected.path)?;
    if &actual != expected { return Err(fail("Handoff root changed since preview")); }
    Ok(file)
}
fn parent(root: &File, path: &str, create: bool, made: &mut Vec<MadeDir>) -> io::Result<(File, CString)> {
    let parts = checked_path(path)?;
    let mut dir = root.try_clone()?;
    let mut prefix = PathBuf::new();
    for part in &parts[..parts.len()-1] {
        prefix.push(part);
        let name = cstr(part)?;
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        let mut fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags) };
        let mut created=false;
        if fd < 0 && create && io::Error::last_os_error().kind() == io::ErrorKind::NotFound {
            let rc = unsafe { libc::mkdirat(dir.as_raw_fd(), name.as_ptr(), 0o755) };
            if rc == 0 { created=true; }
            else if io::Error::last_os_error().kind() != io::ErrorKind::AlreadyExists { return Err(io::Error::last_os_error()); }
            fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags) };
        }
        if fd < 0 { return Err(io::Error::last_os_error()); }
        dir = unsafe { File::from_raw_fd(fd) };
        if created {let meta=dir.metadata()?;made.push(MadeDir{path:prefix.to_string_lossy().into_owned(),device:meta.dev(),inode:meta.ino()});}
    }
    Ok((dir, cstr(parts[parts.len()-1])?))
}
fn metadata_safe(file: &File, reject_hardlinks: bool) -> io::Result<Option<Vec<u8>>> {
    let meta=file.metadata()?;
    if meta.uid()!=unsafe{libc::geteuid()} || meta.mode() & 0o7000 != 0 || (reject_hardlinks && meta.nlink()!=1) {
        return Err(fail("Handoff refuses files with different ownership, special mode bits, or hard links"));
    }
    #[cfg(target_os="macos")]
    let count=unsafe{libc::flistxattr(file.as_raw_fd(),std::ptr::null_mut(),0,0)};
    #[cfg(target_os="linux")]
    let count=unsafe{libc::flistxattr(file.as_raw_fd(),std::ptr::null_mut(),0)};
    #[cfg(not(any(target_os="macos",target_os="linux")))]
    let count: isize=-1;
    if count<0 {let error=io::Error::last_os_error();if error.raw_os_error()!=Some(libc::EOPNOTSUPP){return Err(error);}}
    let mut provenance=None;
    if count>0 {
        if count>65536{return Err(fail("Handoff metadata exceeds limit"));}
        let mut names=vec![0u8;count as usize];
        #[cfg(target_os="macos")]
        let read=unsafe{libc::flistxattr(file.as_raw_fd(),names.as_mut_ptr().cast(),names.len(),0)};
        #[cfg(target_os="linux")]
        let read=unsafe{libc::flistxattr(file.as_raw_fd(),names.as_mut_ptr().cast(),names.len())};
        #[cfg(not(any(target_os="macos",target_os="linux")))]
        let read:isize=-1;
        if read!=count{return Err(fail("Handoff metadata changed while reading"));}
        for name in names.split(|v|*v==0).filter(|v|!v.is_empty()) {
            #[cfg(target_os="macos")]
            if name==b"com.apple.provenance" {
                let n=CString::new(name).map_err(|_|fail("Invalid metadata"))?;
                let size=unsafe{libc::fgetxattr(file.as_raw_fd(),n.as_ptr(),std::ptr::null_mut(),0,0,0)};
                if !(0..=1024).contains(&size){return Err(fail("Invalid provenance metadata"));}
                let mut value=vec![0u8;size as usize];
                let got=unsafe{libc::fgetxattr(file.as_raw_fd(),n.as_ptr(),value.as_mut_ptr().cast(),value.len(),0,0)};
                if got!=size{return Err(fail("Provenance changed while reading"));}
                provenance=Some(value);continue;
            }
            return Err(fail("Handoff refuses unsupported extended attributes and ACLs"));
        }
    }
    #[cfg(target_os="macos")]
    {
        extern "C" { fn acl_get_fd_np(fd: libc::c_int, acl_type: libc::c_int) -> *mut libc::c_void; fn acl_free(acl: *mut libc::c_void) -> libc::c_int; }
        let acl=unsafe{acl_get_fd_np(file.as_raw_fd(),0x100)};
        if !acl.is_null(){unsafe{acl_free(acl);};return Err(fail("Handoff refuses files with ACLs"));}
        let error=io::Error::last_os_error();
        if !matches!(error.raw_os_error(),Some(libc::ENOENT)|Some(libc::EOPNOTSUPP)){return Err(error);}
        let mut stat=unsafe{std::mem::zeroed::<libc::stat>()};
        if unsafe{libc::fstat(file.as_raw_fd(),&mut stat)}!=0{return Err(io::Error::last_os_error());}
        if stat.st_flags!=0{return Err(fail("Handoff refuses files with filesystem flags"));}
    }
    Ok(provenance)
}
fn read_at_impl(root: &File, path: &str, reject_hardlinks: bool) -> io::Result<Option<FileVersion>> {
    let (dir, name) = match parent(root, path, false, &mut vec![]) {
        Ok(v) => v, Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None), Err(e) => return Err(e)
    };
    let fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC) };
    if fd < 0 { let e = io::Error::last_os_error(); return if e.kind() == io::ErrorKind::NotFound { Ok(None) } else { Err(e) }; }
    let mut file = unsafe { File::from_raw_fd(fd) };
    let before = file.metadata()?;
    if !before.is_file() || before.len() > MAX_BYTES as u64 { return Err(fail("Handoff requires bounded regular files")); }
    let provenance=metadata_safe(&file,reject_hardlinks)?;
    let mut bytes = Vec::new(); Read::by_ref(&mut file).take((MAX_BYTES + 1) as u64).read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    if bytes.len() > MAX_BYTES || (before.dev(), before.ino(), before.len(), before.mtime(), before.mtime_nsec()) != (after.dev(), after.ino(), after.len(), after.mtime(), after.mtime_nsec()) { return Err(fail("Handoff file changed while reading")); }
    if metadata_safe(&file,reject_hardlinks)?!=provenance || before.uid()!=after.uid() || before.gid()!=after.gid() || before.permissions().mode()!=after.permissions().mode() {return Err(fail("Handoff metadata changed while reading"));}
    Ok(Some(FileVersion::new(bytes, before.permissions().mode() & 0o777,before.uid(),before.gid(),provenance)))
}
fn read_at(root: &File, path: &str) -> io::Result<Option<FileVersion>> {read_at_impl(root,path,false)}
fn read_user_at(root: &File, path: &str) -> io::Result<Option<FileVersion>> {read_at_impl(root,path,true)}
fn bounded(changes: &[Change]) -> io::Result<()> {
    if changes.is_empty() || changes.len() > MAX_FILES { return Err(fail("Handoff must contain 1 to 128 files")); }
    let mut total = 0usize; let mut paths = BTreeSet::new();
    for change in changes {
        user_path(&change.source)?; user_path(&change.path)?;
        if !paths.insert(&change.path) { return Err(fail("Duplicate handoff destination")); }
        for v in [&change.source_snapshot, &change.before, &change.after].into_iter().flatten() {
            if !v.valid() { return Err(fail("Invalid handoff content digest")); }
            total = total.checked_add(v.bytes.len()).ok_or_else(|| fail("Handoff exceeds size limit"))?;
        }
    }
    if total > MAX_BYTES { return Err(fail("Handoff exceeds 4 MiB content limit")); }
    Ok(())
}

/// Snapshot explicitly named worker files. Existing unequal destination files
/// are conflicts: a worker artifact cannot overwrite a parent's work.
pub fn preview_files(source_root: &Path, target_root: &Path, mappings: &[FileMapping]) -> io::Result<Proposal> {
    if mappings.is_empty() || mappings.len() > MAX_FILES { return Err(fail("Invalid handoff file count")); }
    let (source, src) = root(source_root)?; let (target, dst) = root(target_root)?;
    if source.path == target.path { return Err(fail("Handoff roots must differ")); }
    let mut changes = Vec::new(); let mut conflicts = Vec::new();
    for m in mappings {
        let after = read_user_at(&src, &m.source)?.ok_or_else(|| fail("Worker artifact is missing"))?;
        let before = read_user_at(&dst, &m.destination)?;
        if before.as_ref().is_some_and(|v| v != &after) { conflicts.push(m.destination.clone()); }
        changes.push(Change { source: m.source.clone(), path: m.destination.clone(), source_snapshot: Some(after.clone()), before, after: Some(after) });
    }
    bounded(&changes)?;
    Ok(Proposal { version: 1, source, target, base: None, changes, conflicts })
}

fn git(at: &Path, args: &[&str], control: &Control<'_>) -> io::Result<process::Output> {
    let mut cmd = Command::new(std::env::var_os("KAGEHA_GIT_BIN").unwrap_or_else(|| "/usr/bin/git".into()));
    cmd.current_dir(at).env_clear().env("PATH", "/usr/bin:/bin").env("GIT_CONFIG_NOSYSTEM", "1").env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_TERMINAL_PROMPT", "0").env("GIT_NO_LAZY_FETCH", "1").env("GIT_LITERAL_PATHSPECS", "1")
        .args(["--no-pager", "-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false", "-c", "submodule.recurse=false"]).args(args);
    let until = Instant::now() + Duration::from_secs(30);
    let control = Control { cancelled: control.cancelled, deadline: Some(control.deadline.unwrap_or(until).min(until)) };
    process::run(&mut cmd, vec![], &control, MAX_BYTES)
}
fn git_ok(at: &Path, args: &[&str], control: &Control<'_>) -> io::Result<Vec<u8>> {
    let output = git(at,args,control)?; if !output.success { return Err(fail("Git handoff inspection failed")); } Ok(output.bytes)
}
fn git_text(at: &Path, args: &[&str], control: &Control<'_>) -> io::Result<String> {
    String::from_utf8(git_ok(at,args,control)?).map(|s| s.trim_end_matches('\n').to_owned()).map_err(|_| fail("Invalid Git output"))
}
fn base_valid(base: &str) -> bool { matches!(base.len(), 40 | 64) && base.bytes().all(|b| b.is_ascii_hexdigit()) }
fn common(at: &Path, control: &Control<'_>) -> io::Result<PathBuf> {
    let value = git_text(at,&["rev-parse","--git-common-dir"],control)?;
    let value = Path::new(&value); (if value.is_absolute() { value.to_path_buf() } else { at.join(value) }).canonicalize()
}
fn root_git(at: &Path, control: &Control<'_>) -> io::Result<()> {
    let top = git_text(at,&["rev-parse","--show-toplevel"],control)?;
    if Path::new(&top).canonicalize()? != at { return Err(fail("Git handoff requires a project root")); } Ok(())
}
fn base_file(at: &Path, base: &str, path: &str, control: &Control<'_>) -> io::Result<Option<FileVersion>> {
    let output = git_ok(at,&["ls-tree","-z",base,"--",path],control)?;
    if output.is_empty() { return Ok(None); }
    let entry = std::str::from_utf8(&output).map_err(|_| fail("Invalid Git tree entry"))?;
    let (description, actual) = entry.strip_suffix('\0').and_then(|e| e.split_once('\t')).ok_or_else(|| fail("Invalid Git tree entry"))?;
    if actual != path { return Err(fail("Unexpected Git tree entry")); }
    let mode = description.split_whitespace().next().unwrap_or("");
    if mode != "100644" && mode != "100755" { return Err(fail("Git handoff does not support symlinks or submodules")); }
    let spec = format!("{base}:{path}");
    Ok(Some(FileVersion::new(git_ok(at,&["show",&spec],control)?,if mode == "100755" {0o755} else {0o644},unsafe{libc::geteuid()},unsafe{libc::getegid()},None)))
}
/// Preview tracked and untracked changes in a worker's worktree against its
/// pinned committed base. The target must be another checkout of that same
/// repository at the same base; touched dirty or staged files are conflicts.
pub fn preview_git(source_root: &Path, target_root: &Path, base: &str, control: &Control<'_>) -> io::Result<Proposal> {
    if !base_valid(base) { return Err(fail("Git handoff needs a pinned commit ID")); }
    let (source, src) = root(source_root)?; let (target, dst) = root(target_root)?;
    if source.path == target.path { return Err(fail("Handoff roots must differ")); }
    root_git(&source.path, control)?; root_git(&target.path, control)?;
    if common(&source.path,control)? != common(&target.path,control)? { return Err(fail("Worktree and target do not share a repository")); }
    if git_text(&source.path,&["rev-parse","HEAD"],control)? != base || git_text(&target.path,&["rev-parse","HEAD"],control)? != base { return Err(fail("Git handoff base changed")); }
    let tracked = git_ok(&source.path,&["diff","--no-renames","--name-only","-z",base,"--"],control)?;
    let untracked = git_ok(&source.path,&["ls-files","--others","--exclude-standard","-z"],control)?;
    let mut paths = BTreeSet::new();
    for entry in tracked.split(|b| *b == 0).chain(untracked.split(|b| *b == 0)).filter(|s| !s.is_empty()) {
        let path = std::str::from_utf8(entry).map_err(|_| fail("Git handoff requires UTF-8 paths"))?;
        user_path(path)?; paths.insert(path.to_owned());
        if paths.len() > MAX_FILES { return Err(fail("Too many handoff files")); }
    }
    let mut changes = Vec::new(); let mut conflicts = Vec::new();
    for path in paths {
        let base_file = base_file(&source.path,base,&path,control)?;
        let source_snapshot = read_user_at(&src,&path)?;
        let same_git = |a: &Option<FileVersion>,b: &Option<FileVersion>| match (a,b) {
            (None,None)=>true,(Some(a),Some(b))=>a.sha256==b.sha256 && a.executable==b.executable,_=>false,
        };
        if same_git(&base_file,&source_snapshot) { continue; }
        let destination = read_user_at(&dst,&path)?;
        let status = git_ok(&target.path,&["status","--porcelain=v1","-z","--untracked-files=all","--",&path],control)?;
        if !same_git(&destination,&base_file) || !status.is_empty() { conflicts.push(path.clone()); }
        let mut after=source_snapshot.clone();
        if let (Some(before),Some(after))=(&destination,&mut after) {
            // Keep the target's ownership and non-executable permission bits;
            // Git only records executable mode, not private Unix permissions.
            after.uid=before.uid;after.gid=before.gid;after.provenance=before.provenance.clone();
            after.mode=(before.mode & !0o111) | (after.mode & 0o111);
        }
        changes.push(Change { source: path.clone(), path, source_snapshot, before:destination, after });
    }
    bounded(&changes)?;
    Ok(Proposal { version: 1, source, target, base: Some(base.to_owned()), changes, conflicts })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Transaction { version: u32, state: String, proposal: Proposal, temp_paths: Vec<String>, #[serde(default)] planned_dirs: Vec<String>, #[serde(default)] made_dirs: Vec<MadeDir>, states: Vec<String>, stage_ids: Vec<Option<Identity>>, original_ids: Vec<Option<Identity>> }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Recovery { pub state: String, pub paths: Vec<String> }

fn journal_write(dir: &Path, tx: &Transaction) -> io::Result<()> {
    let bytes=serde_json::to_vec(tx)?;
    // A proposal holds at most 4 MiB of data, serialized as JSON byte arrays.
    if bytes.len()>32*1024*1024 { return Err(fail("Handoff journal exceeds limit")); }
    let path=dir.join("transaction.new");
    // The private scratch file may survive a crash before its atomic rename.
    match fs::remove_file(&path) { Ok(()) => {}, Err(error) if error.kind()==io::ErrorKind::NotFound => {}, Err(error) => return Err(error) }
    let mut file=OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(&path)?;
    if let Err(error)=(|| {file.write_all(&bytes)?; file.sync_all()})() {let _=fs::remove_file(&path);return Err(error);}
    fs::rename(&path,dir.join("transaction.json"))?;
    File::open(dir)?.sync_all()
}
fn journal_read(dir: &Path) -> io::Result<Transaction> {
    let meta=fs::symlink_metadata(dir)?;
    if !meta.is_dir() || meta.permissions().mode() & 0o077 != 0 { return Err(fail("Handoff journal directory is not private")); }
    let mut file=OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(dir.join("transaction.json"))?;
    let mut bytes=Vec::new(); Read::by_ref(&mut file).take(32*1024*1024+1).read_to_end(&mut bytes)?;
    if bytes.len()>32*1024*1024 {return Err(fail("Handoff journal exceeds limit"));}
    let tx:Transaction=serde_json::from_slice(&bytes)?;
    let count=tx.proposal.changes.len();
    if tx.version!=1 || tx.temp_paths.len()!=count || tx.states.len()!=count || tx.stage_ids.len()!=count || tx.original_ids.len()!=count || tx.states.iter().any(|s|!matches!(s.as_str(),"pending"|"staged"|"dispatching"|"installed")) {return Err(fail("Invalid handoff journal"));}
    bounded(&tx.proposal.changes)?;
    for (change,temp) in tx.proposal.changes.iter().zip(&tx.temp_paths) {
        checked_path(temp)?;
        if Path::new(temp).parent()!=Path::new(&change.path).parent()
            || !Path::new(temp).file_name().is_some_and(|v|v.as_bytes().starts_with(b".kageha-handoff-"))
        {return Err(fail("Invalid handoff staging path"));}
    }
    let mut dirs=BTreeSet::new();
    for path in &tx.planned_dirs {user_path(path)?;if !dirs.insert(path){return Err(fail("Duplicate handoff planned directory"));}}
    let mut made=BTreeSet::new();
    for dir in &tx.made_dirs {user_path(&dir.path)?;if !dirs.contains(&dir.path)||!made.insert(&dir.path){return Err(fail("Invalid handoff created directory"));}}
    Ok(tx)
}
fn staged_path(path: &str, index: usize, id: u64) -> String {
    let name=format!(".kageha-handoff-{}-{id}-{index}",std::process::id());
    match path.rsplit_once('/') {Some((parent,_))=>format!("{parent}/{name}"),None=>name}
}
fn stage_file(root: &File, path: &str, data: &FileVersion, made: &mut Vec<MadeDir>) -> io::Result<()> {
    let (dir,name)=parent(root,path,true,made)?;
    let fd=unsafe{libc::openat(dir.as_raw_fd(),name.as_ptr(),libc::O_WRONLY|libc::O_CREAT|libc::O_EXCL|libc::O_NOFOLLOW|libc::O_CLOEXEC,0o600)};
    if fd<0 {return Err(io::Error::last_os_error());}
    let mut file=unsafe{File::from_raw_fd(fd)};
    let result=(|| {
        metadata_safe(&file,true)?;
        if file.metadata()?.gid()!=data.gid && unsafe{libc::fchown(file.as_raw_fd(),libc::uid_t::MAX,data.gid)}!=0{return Err(io::Error::last_os_error());}
        file.write_all(&data.bytes)?;file.set_permissions(fs::Permissions::from_mode(data.mode))?;
        let actual=file.metadata()?;
        if actual.uid()!=data.uid || actual.gid()!=data.gid || actual.permissions().mode() & 0o777 != data.mode || metadata_safe(&file,true)?!=data.provenance {return Err(fail("Cannot preserve handoff file metadata"));}
        file.sync_all()?;dir.sync_all()
    })();
    if let Err(error)=result {unsafe{libc::unlinkat(dir.as_raw_fd(),name.as_ptr(),0);};return Err(error);}
    Ok(())
}
fn unlink_file(root:&File,path:&str)->io::Result<()> {
    let (dir,name)=parent(root,path,false,&mut vec![])?;
    if unsafe{libc::unlinkat(dir.as_raw_fd(),name.as_ptr(),0)}!=0{return Err(io::Error::last_os_error());}dir.sync_all()
}
fn exchange(root:&File,a:&str,b:&str)->io::Result<()> {
    let (da,na)=parent(root,a,false,&mut vec![])?;let (db,nb)=parent(root,b,false,&mut vec![])?;
    #[cfg(target_os="macos")]
    let rc=unsafe{libc::renameatx_np(da.as_raw_fd(),na.as_ptr(),db.as_raw_fd(),nb.as_ptr(),libc::RENAME_SWAP)};
    #[cfg(target_os="linux")]
    let rc=unsafe{libc::renameat2(da.as_raw_fd(),na.as_ptr(),db.as_raw_fd(),nb.as_ptr(),libc::RENAME_EXCHANGE)};
    #[cfg(not(any(target_os="macos",target_os="linux")))]
    let rc=-1;
    if rc!=0{return Err(io::Error::last_os_error());}da.sync_all()?;db.sync_all()
}
fn move_exclusive(root:&File,a:&str,b:&str)->io::Result<()> {
    let (da,na)=parent(root,a,false,&mut vec![])?;let (db,nb)=parent(root,b,false,&mut vec![])?;
    #[cfg(target_os="macos")]
    let rc=unsafe{libc::renameatx_np(da.as_raw_fd(),na.as_ptr(),db.as_raw_fd(),nb.as_ptr(),libc::RENAME_EXCL)};
    #[cfg(target_os="linux")]
    let rc=unsafe{libc::renameat2(da.as_raw_fd(),na.as_ptr(),db.as_raw_fd(),nb.as_ptr(),libc::RENAME_NOREPLACE)};
    #[cfg(not(any(target_os="macos",target_os="linux")))]
    let rc=-1;
    if rc!=0{return Err(io::Error::last_os_error());}da.sync_all()?;db.sync_all()
}
fn exists_at(root:&File,path:&str)->io::Result<bool> {
    let (dir,name)=match parent(root,path,false,&mut vec![]){Ok(v)=>v,Err(e) if e.kind()==io::ErrorKind::NotFound=>return Ok(false),Err(e)=>return Err(e)};
    let mut st=unsafe{std::mem::zeroed::<libc::stat>()};
    if unsafe{libc::fstatat(dir.as_raw_fd(),name.as_ptr(),&mut st,libc::AT_SYMLINK_NOFOLLOW)}==0{return Ok(true);}
    let e=io::Error::last_os_error();if e.kind()==io::ErrorKind::NotFound{Ok(false)}else{Err(e)}
}
fn identity_at(root:&File,path:&str)->io::Result<Option<Identity>> {
    let (dir,name)=match parent(root,path,false,&mut vec![]){Ok(v)=>v,Err(e) if e.kind()==io::ErrorKind::NotFound=>return Ok(None),Err(e)=>return Err(e)};
    let mut stat=unsafe{std::mem::zeroed::<libc::stat>()};
    if unsafe{libc::fstatat(dir.as_raw_fd(),name.as_ptr(),&mut stat,libc::AT_SYMLINK_NOFOLLOW)}==0 {
        return Ok(Some(Identity{device:stat.st_dev as u64,inode:stat.st_ino as u64}));
    }
    let e=io::Error::last_os_error();if e.kind()==io::ErrorKind::NotFound{Ok(None)}else{Err(e)}
}
fn missing_parent_dirs(root:&File,changes:&[Change])->io::Result<Vec<String>> {
    let mut missing=BTreeSet::new();
    for change in changes {
        let parts=checked_path(&change.path)?;let mut prefix=PathBuf::new();
        for part in &parts[..parts.len()-1] {
            prefix.push(part);let path=prefix.to_str().ok_or_else(||fail("Invalid handoff directory"))?;
            let probe=format!("{path}/.handoff-probe");
            match parent(root,&probe,false,&mut vec![]) {
                Ok(_)=>{},Err(e) if e.kind()==io::ErrorKind::NotFound=>{missing.insert(path.to_owned());},Err(e)=>return Err(e)
            }
        }
    }
    Ok(missing.into_iter().collect())
}
/// Undo an uncommitted transaction. Unknown state is retained and reported as
/// uncertain; never delete or replace an inode the transaction cannot identify.
fn rollback(tx:&Transaction,root:&File)->io::Result<()> {
    for index in (0..tx.proposal.changes.len()).rev() {
        let change=&tx.proposal.changes[index];let temp=&tx.temp_paths[index];
        if change.before==change.after {continue;}
        let state=tx.states[index].as_str();
        let stage=identity_at(root,temp)?;let target=identity_at(root,&change.path)?;
        let stage_id=tx.stage_ids[index].as_ref();let original=tx.original_ids[index].as_ref();
        if state=="pending" {
            if stage.is_some(){return Err(fail("Handoff recovery uncertain: unrecorded staging file"));}
            continue;
        }
        if state=="staged" {
            if change.after.is_some() {
                // A prior recovery may already have removed our staged file.
                if stage.is_none(){continue;}
                if stage.as_ref()!=stage_id || read_at(root,temp)?!=change.after{return Err(fail("Handoff recovery uncertain: staged file changed"));}
                unlink_file(root,temp)?;
            } else if stage.is_some(){return Err(fail("Handoff recovery uncertain: unexpected deletion backup"));}
            continue;
        }
        match (&change.before,&change.after) {
            (None,Some(after))=>{
                if stage.is_none() && target.is_none(){continue;}
                if stage.is_none() && target.as_ref()==stage_id {
                    if read_at(root,&change.path)?.as_ref()!=Some(after){return Err(fail("Handoff recovery uncertain: added file changed"));}
                    // Rename without replacement first; only delete the inode we staged.
                    move_exclusive(root,&change.path,temp)?;
                    if identity_at(root,temp)?.as_ref()!=stage_id || read_at(root,temp)?.as_ref()!=Some(after) {
                        let _=move_exclusive(root,temp,&change.path);
                        return Err(fail("Handoff recovery uncertain: added file raced"));
                    }
                    unlink_file(root,temp)?;
                } else if stage.as_ref()==stage_id {
                    if read_at(root,temp)?.as_ref()!=Some(after){return Err(fail("Handoff recovery uncertain: staging changed"));}
                    unlink_file(root,temp)?;
                } else {return Err(fail("Handoff recovery uncertain: added file identity changed"));}
            }
            (Some(before),Some(after))=>{
                if stage.is_none() && target.as_ref()==original {
                    if read_at(root,&change.path)?.as_ref()!=Some(before) {return Err(fail("Handoff recovery uncertain: restored original changed"));}
                    continue;
                }
                if target.as_ref()==stage_id && stage.is_some() {
                    // Exchange retains the exact displaced inode, including a
                    // concurrent edit; swap back, and only discard our own stage.
                    exchange(root,&change.path,temp)?;
                    if identity_at(root,temp)?.as_ref()!=stage_id || read_at(root,temp)?.as_ref()!=Some(after){return Err(fail("Handoff recovery uncertain: destination raced"));}
                    unlink_file(root,temp)?;
                    if identity_at(root,&change.path)?.as_ref()!=original || read_at(root,&change.path)?.as_ref()!=Some(before) {return Err(fail("Handoff recovery uncertain: restored original changed"));}
                } else if stage.as_ref()==stage_id && target.as_ref()==original {
                    if read_at(root,temp)?.as_ref()!=Some(after){return Err(fail("Handoff recovery uncertain: staging changed"));}
                    unlink_file(root,temp)?;
                } else {return Err(fail("Handoff recovery uncertain: modified file identity changed"));}
            }
            (Some(before),None)=>{
                if target.is_none() && stage.as_ref()==original {move_exclusive(root,temp,&change.path)?;if read_at(root,&change.path)?.as_ref()!=Some(before){return Err(fail("Handoff recovery uncertain: restored original changed"));}}
                else if target.as_ref()==original && stage.is_none() {if read_at(root,&change.path)?.as_ref()!=Some(before){return Err(fail("Handoff recovery uncertain: restored original changed"));}}
                else{return Err(fail("Handoff recovery uncertain: deletion identity changed"));}
            }
            (None,None)=>{}
        }
    }
    Ok(())
}
fn cleanup_committed(tx:&Transaction,root:&File,journal_dir:&Path)->io::Result<()> {
    let journal=OpenOptions::new().read(true).custom_flags(libc::O_DIRECTORY|libc::O_NOFOLLOW|libc::O_CLOEXEC).open(journal_dir)?;
    for(index,(change,temp)) in tx.proposal.changes.iter().zip(&tx.temp_paths).enumerate() {
        if !exists_at(root,temp)? {continue;}
        let expected=if change.before.is_none(){&change.after}else{&change.before};
        let unchanged=&read_at(root,temp)?==expected;
        let (from,name)=parent(root,temp,false,&mut vec![])?;
        let backup=CString::new(format!("backup-{index}")).unwrap();
        #[cfg(target_os="macos")]
        let rc=unsafe{libc::renameatx_np(from.as_raw_fd(),name.as_ptr(),journal.as_raw_fd(),backup.as_ptr(),libc::RENAME_EXCL)};
        #[cfg(target_os="linux")]
        let rc=unsafe{libc::renameat2(from.as_raw_fd(),name.as_ptr(),journal.as_raw_fd(),backup.as_ptr(),libc::RENAME_NOREPLACE)};
        #[cfg(not(any(target_os="macos",target_os="linux")))]
        let rc=-1;
        if rc!=0 { return Err(io::Error::last_os_error()); }
        from.sync_all()?;journal.sync_all()?;
        if !unchanged{return Err(fail("Handoff committed while an original changed; retained backup requires inspection"));}
    }
    Ok(())
}
fn recover_locked(dir:&Path,mut tx:Transaction)->io::Result<Recovery> {
    let root=open_root(&tx.proposal.target)?;
    match tx.state.as_str() {
        "committed"=>cleanup_committed(&tx,&root,dir)?,
        "rolled_back"=>{},
        "active"=>{
            rollback(&tx,&root)?;
            for made in tx.made_dirs.iter().rev() {
                user_path(&made.path)?;
                if let Some(actual)=identity_at(&root,&made.path)? {
                    if actual.device!=made.device || actual.inode!=made.inode {return Err(fail("Handoff recovery uncertain: created directory changed"));}
                    let (parent,name)=parent(&root,&made.path,false,&mut vec![])?;
                    if unsafe{libc::unlinkat(parent.as_raw_fd(),name.as_ptr(),libc::AT_REMOVEDIR)}!=0{return Err(fail("Handoff recovery uncertain: created directory is not empty"));}
                }
            }
            for path in &tx.planned_dirs {
                if !tx.made_dirs.iter().any(|made| &made.path==path) && identity_at(&root,path)?.is_some() {
                    return Err(fail("Handoff recovery uncertain: directory creation was not recorded"));
                }
            }
            tx.state="rolled_back".into();journal_write(dir,&tx)?;
        },
        _=>return Err(fail("Unknown handoff journal state"))
    }
    Ok(Recovery{state:tx.state,paths:tx.proposal.changes.iter().map(|v|v.path.clone()).collect()})
}
/// Recover an interrupted handoff using its private durable journal. A clean
/// rollback never replays the operation. Unknown externally changed state is
/// retained and returned as an error requiring inspection.
pub fn recover(journal_dir:&Path)->io::Result<Recovery> {
    let _guard=APPLY_LOCK.lock().map_err(|_|fail("Handoff lock unavailable"))?;
    recover_locked(journal_dir,journal_read(journal_dir)?)
}
/// Recheck a committed handoff's current files without mutating the target.
pub fn verify_applied(proposal:&Proposal)->io::Result<bool> {
    if proposal.version!=1 || !proposal.conflicts.is_empty(){return Err(fail("Invalid handoff proposal"));}
    bounded(&proposal.changes)?;
    let root=open_root(&proposal.target)?;
    for change in &proposal.changes {if read_at(&root,&change.path)?!=change.after{return Ok(false);}}
    Ok(true)
}
/// Apply a conflict-free preview under a private, unique durable journal.
/// Reusing the same committed journal is idempotent. On failure, reverse all
/// completed changes; uncertain concurrent modifications are retained for review.
pub fn apply_with_journal(proposal:&Proposal,journal_dir:&Path)->io::Result<ApplyReceipt> {
    let _guard=APPLY_LOCK.lock().map_err(|_|fail("Handoff lock unavailable"))?;
    if journal_dir.exists(){
        let existing=journal_read(journal_dir)?;
        if existing.proposal!=*proposal{return Err(fail("Handoff journal identity changed"));}
        let recovery=recover_locked(journal_dir,existing)?;
        return if recovery.state=="committed"{Ok(ApplyReceipt{paths:recovery.paths})}else{Err(fail("Interrupted handoff rolled back; create a fresh preview"))};
    }
    if proposal.version!=1 || !proposal.conflicts.is_empty(){return Err(fail("Handoff has unresolved conflicts"));}
    bounded(&proposal.changes)?;
    let source=open_root(&proposal.source)?;let target=open_root(&proposal.target)?;
    if let Some(base)=&proposal.base {
        if !base_valid(base){return Err(fail("Invalid handoff base"));}
        let cancelled=std::sync::atomic::AtomicBool::new(false);let control=Control{cancelled:&cancelled,deadline:None};
        root_git(&proposal.source.path,&control)?;root_git(&proposal.target.path,&control)?;
        if common(&proposal.source.path,&control)?!=common(&proposal.target.path,&control)? || git_text(&proposal.source.path,&["rev-parse","HEAD"],&control)?!=*base || git_text(&proposal.target.path,&["rev-parse","HEAD"],&control)?!=*base{return Err(fail("Git handoff binding changed"));}
    }
    for change in &proposal.changes {
        if read_user_at(&source,&change.source)?!=change.source_snapshot || read_user_at(&target,&change.path)?!=change.before{return Err(fail("Handoff source or destination changed since preview"));}
        if proposal.base.is_some(){
            let cancelled=std::sync::atomic::AtomicBool::new(false);let control=Control{cancelled:&cancelled,deadline:None};
            if !git_ok(&proposal.target.path,&["status","--porcelain=v1","-z","--untracked-files=all","--",&change.path],&control)?.is_empty(){return Err(fail("Git handoff target is dirty"));}
        } else if change.before.is_some()&&change.before!=change.after{return Err(fail("Worker artifact would overwrite a parent file"));}
    }
    let parent_dir=journal_dir.parent().ok_or_else(||fail("Invalid handoff journal path"))?;
    fs::create_dir_all(parent_dir)?;
    // Retaining original inodes in the private journal requires the same volume.
    // Refuse cross-volume handoffs before touching the destination.
    if fs::metadata(parent_dir)?.dev()!=proposal.target.device {return Err(fail("Handoff journal and destination must be on the same filesystem"));}
    fs::DirBuilder::new().mode(0o700).create(journal_dir)?;
    let id=NEXT_TEMP.fetch_add(1,Ordering::Relaxed);
    let planned_dirs=missing_parent_dirs(&target,&proposal.changes)?;
    let original_ids=proposal.changes.iter().map(|c|identity_at(&target,&c.path)).collect::<io::Result<Vec<_>>>()?;
    let mut tx=Transaction{version:1,state:"active".into(),proposal:proposal.clone(),temp_paths:proposal.changes.iter().enumerate().map(|(i,c)|staged_path(&c.path,i,id)).collect(),planned_dirs,made_dirs:vec![],states:vec!["pending".into();proposal.changes.len()],stage_ids:vec![None;proposal.changes.len()],original_ids};
    journal_write(journal_dir,&tx)?;
    let operation=(||->io::Result<()> {
        // Stage all new bytes before changing destination files.
        for index in 0..proposal.changes.len() {
            let change=&proposal.changes[index];
            if change.before!=change.after {
                if let Some(after)=&change.after {
                    stage_file(&target,&tx.temp_paths[index],after,&mut tx.made_dirs)?;
                    tx.stage_ids[index]=identity_at(&target,&tx.temp_paths[index])?;
                }
                tx.states[index]="staged".into();journal_write(journal_dir,&tx)?;
            }
        }
        for index in 0..proposal.changes.len() {
            let change=&proposal.changes[index];
            if change.before==change.after{continue;}
            if read_user_at(&target,&change.path)?!=change.before || identity_at(&target,&change.path)?!=tx.original_ids[index]{return Err(fail("Handoff destination changed during application"));}
            tx.states[index]="dispatching".into();journal_write(journal_dir,&tx)?;
            let temp=&tx.temp_paths[index];
            match (&change.before,&change.after){
                (None,Some(_))=>move_exclusive(&target,temp,&change.path)?,
                (Some(before),Some(_))=>{exchange(&target,&change.path,temp)?;if read_at(&target,temp)?.as_ref()!=Some(before)||identity_at(&target,temp)?!=tx.original_ids[index]{return Err(fail("Handoff destination raced during exchange"));}},
                (Some(before),None)=>{move_exclusive(&target,&change.path,temp)?;if read_at(&target,temp)?.as_ref()!=Some(before)||identity_at(&target,temp)?!=tx.original_ids[index]{return Err(fail("Handoff destination raced during deletion"));}},
                _=>{}
            }
            tx.states[index]="installed".into();journal_write(journal_dir,&tx)?;
        }
        tx.state="committed".into();journal_write(journal_dir,&tx)?;
        cleanup_committed(&tx,&target,journal_dir)?;Ok(())
    })();
    if let Err(error)=operation {
        // A committed journal is a success receipt even if backup cleanup failed;
        // retain the backup and report uncertainty rather than rolling it back.
        if tx.state=="committed" {return Err(fail("Handoff committed but cleanup is uncertain; inspect the journal"));}
        return match recover_locked(journal_dir,tx){Ok(_)=>Err(error),Err(_)=>Err(fail("Handoff failed and recovery is uncertain; inspect retained backups"))};
    }
    Ok(ApplyReceipt{paths:proposal.changes.iter().map(|v|v.path.clone()).collect()})
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::AtomicBool;
    use tempfile::TempDir;

    fn control() -> Control<'static> { static CANCEL: AtomicBool = AtomicBool::new(false); Control { cancelled: &CANCEL, deadline: None } }
    fn write(root: &Path, path: &str, data: &[u8]) { let path=root.join(path); fs::create_dir_all(path.parent().unwrap()).unwrap(); fs::write(path,data).unwrap(); }
    fn git_run(root: &Path, args: &[&str]) -> String {
        let output=Command::new("/usr/bin/git").current_dir(root).args(args).env("GIT_CONFIG_NOSYSTEM","1").env("GIT_CONFIG_GLOBAL","/dev/null").output().unwrap();
        assert!(output.status.success(),"git {args:?}: {}",String::from_utf8_lossy(&output.stderr));
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }
    fn setup_git() -> (TempDir, PathBuf, PathBuf, String) {
        let temp=TempDir::new().unwrap(); let project=temp.path().join("project"); fs::create_dir(&project).unwrap();
        git_run(&project,&["init","-q"]); git_run(&project,&["config","user.email","test@example.invalid"]); git_run(&project,&["config","user.name","Test"]);
        write(&project,"src/core.py",b"old\n");write(&project,"src/remove.py",b"remove\n");write(&project,"unrelated.txt",b"clean\n");
        git_run(&project,&["add","."]);git_run(&project,&["commit","-qm","baseline"]);let base=git_run(&project,&["rev-parse","HEAD"]);
        let worker=temp.path().join("worker");git_run(&project,&["worktree","add","-q","--detach",worker.to_str().unwrap(),&base]);
        (temp,project,worker,base)
    }
    fn tx_for(proposal: &Proposal, root: &File, id: u64) -> Transaction {
        let count=proposal.changes.len();
        Transaction{version:1,state:"active".into(),proposal:proposal.clone(),
            temp_paths:proposal.changes.iter().enumerate().map(|(i,c)|staged_path(&c.path,i,id)).collect(),
            planned_dirs:missing_parent_dirs(root,&proposal.changes).unwrap(),made_dirs:vec![],
            states:vec!["pending".into();count],stage_ids:vec![None;count],
            original_ids:proposal.changes.iter().map(|c|identity_at(root,&c.path).unwrap()).collect()}
    }
    fn stage_all(tx:&mut Transaction,root:&File) {
        for i in 0..tx.proposal.changes.len() {
            if let Some(after)=&tx.proposal.changes[i].after {stage_file(root,&tx.temp_paths[i],after,&mut tx.made_dirs).unwrap();tx.stage_ids[i]=identity_at(root,&tx.temp_paths[i]).unwrap();}
            tx.states[i]="staged".into();
        }
    }
    #[test]
    fn file_handoffs_preserve_worker_and_parent_and_reject_stale_data() {
        let t=TempDir::new().unwrap();let src=t.path().join("src");let dst=t.path().join("dst");fs::create_dir(&src).unwrap();fs::create_dir(&dst).unwrap();
        write(&src,"outputs/code.py",b"print(1)\n");write(&src,"outputs/test.py",b"test\n");
        let maps=vec![FileMapping{source:"outputs/code.py".into(),destination:"outputs/code.py".into()},FileMapping{source:"outputs/test.py".into(),destination:"outputs/test.py".into()}];
        let proposal=preview_files(&src,&dst,&maps).unwrap();assert!(!proposal.has_conflicts());
        let journal=t.path().join("journals/file");
        assert_eq!(proposal.summary()["files"][0]["change"],"added");
        assert!(!proposal.summary().to_string().contains(src.to_str().unwrap()));
        write(&src,"outputs/test.py",b"changed\n"); assert!(apply_with_journal(&proposal,&journal).is_err());assert!(!dst.join("outputs/code.py").exists());
        write(&src,"outputs/test.py",b"test\n");let proposal=preview_files(&src,&dst,&maps).unwrap();
        write(&dst,"outputs/test.py",b"parent work\n");assert!(apply_with_journal(&proposal,&journal).is_err());assert!(!dst.join("outputs/code.py").exists());
        fs::remove_file(dst.join("outputs/test.py")).unwrap();apply_with_journal(&proposal,&journal).unwrap();
        assert_eq!(fs::read(dst.join("outputs/code.py")).unwrap(),b"print(1)\n");assert_eq!(fs::read(src.join("outputs/code.py")).unwrap(),b"print(1)\n");
        assert!(apply_with_journal(&proposal,&journal).is_ok(),"committed replay returns the original receipt");
        write(&dst,"outputs/test.py",b"parent work\n");let conflict=preview_files(&src,&dst,&maps).unwrap();assert!(conflict.has_conflicts());assert!(apply_with_journal(&conflict,&t.path().join("journals/conflict")).is_err());assert_eq!(fs::read(dst.join("outputs/test.py")).unwrap(),b"parent work\n");
    }
    #[test]
    fn paths_reject_traversal_reserved_state_and_symlinks() {
        let t=TempDir::new().unwrap();let src=t.path().join("src");let dst=t.path().join("dst");fs::create_dir(&src).unwrap();fs::create_dir(&dst).unwrap();write(&src,"valid",b"ok");
        for path in ["../escape", "/tmp/escape", ".git/config", ".kageha/state", "agents/other/file"] {
            assert!(preview_files(&src,&dst,&[FileMapping{source:"valid".into(),destination:path.into()}]).is_err(),"{path}");
        }
        symlink(t.path(),dst.join("link")).unwrap();
        assert!(preview_files(&src,&dst,&[FileMapping{source:"valid".into(),destination:"link/escape".into()}]).is_err());
        symlink(t.path().join("outside"),src.join("symlink")).unwrap();
        assert!(preview_files(&src,&dst,&[FileMapping{source:"symlink".into(),destination:"copy".into()}]).is_err());
    }
    #[test]
    fn git_handoff_previews_and_applies_without_touching_unrelated_dirty_work() {
        let (t,project,worker,base)=setup_git();
        write(&project,"unrelated.txt",b"user changes\n");write(&worker,"src/core.py",b"new\n");fs::remove_file(worker.join("src/remove.py")).unwrap();write(&worker,"tests/test_core.py",b"tests\n");
        let proposal=preview_git(&worker,&project,&base,&control()).unwrap();assert!(!proposal.has_conflicts(),"{:?}",proposal.conflicts);assert_eq!(proposal.changes.len(),3);
        let journal=t.path().join("journals/git");apply_with_journal(&proposal,&journal).unwrap();assert_eq!(fs::read(project.join("src/core.py")).unwrap(),b"new\n");assert!(!project.join("src/remove.py").exists());assert_eq!(fs::read(project.join("tests/test_core.py")).unwrap(),b"tests\n");assert_eq!(fs::read(project.join("unrelated.txt")).unwrap(),b"user changes\n");
        assert_eq!(git_run(&project,&["diff","--cached","--name-only"]),"");assert!(apply_with_journal(&proposal,&journal).is_ok());
    }
    #[test]
    fn git_handoff_rejects_dirty_targets_and_worker_changes_without_partial_apply() {
        let (t,project,worker,base)=setup_git();write(&worker,"src/core.py",b"worker\n");write(&worker,"tests/test_core.py",b"test\n");write(&project,"src/core.py",b"user\n");
        let conflict=preview_git(&worker,&project,&base,&control()).unwrap();assert_eq!(conflict.conflicts,["src/core.py"]);assert!(apply_with_journal(&conflict,&t.path().join("journals/conflict")).is_err());assert!(!project.join("tests/test_core.py").exists());
        write(&project,"src/core.py",b"old\n");let proposal=preview_git(&worker,&project,&base,&control()).unwrap();write(&worker,"src/core.py",b"changed again\n");assert!(apply_with_journal(&proposal,&t.path().join("journals/stale")).is_err());assert!(!project.join("tests/test_core.py").exists());
    }
    #[test]
    fn git_handoff_rejects_untracked_collision_staged_changes_and_symlinks() {
        let (_t,project,worker,base)=setup_git();write(&worker,"tests/test_core.py",b"worker\n");write(&project,"tests/test_core.py",b"user\n");let p=preview_git(&worker,&project,&base,&control()).unwrap();assert!(p.has_conflicts());
        fs::remove_file(project.join("tests/test_core.py")).unwrap();write(&worker,"src/core.py",b"worker\n");write(&project,"src/core.py",b"staged\n");git_run(&project,&["add","src/core.py"]);write(&project,"src/core.py",b"old\n");let p=preview_git(&worker,&project,&base,&control()).unwrap();assert!(p.conflicts.contains(&"src/core.py".to_owned()));
        symlink("/tmp",worker.join("outside")).unwrap();assert!(preview_git(&worker,&project,&base,&control()).is_err());
    }
    #[test]
    fn interrupted_multi_file_exchange_rolls_back_and_does_not_replay() {
        let (t,project,worker,base)=setup_git();
        write(&worker,"src/core.py",b"new\n");write(&worker,"tests/test_core.py",b"tests\n");
        let proposal=preview_git(&worker,&project,&base,&control()).unwrap();
        let root=open_root(&proposal.target).unwrap();
        let journal=t.path().join("journal");fs::DirBuilder::new().mode(0o700).create(&journal).unwrap();
        let mut tx=tx_for(&proposal,&root,123);
        journal_write(&journal,&tx).unwrap();stage_all(&mut tx,&root);tx.states[0]="dispatching".into();journal_write(&journal,&tx).unwrap();
        let first=&tx.proposal.changes[0]; assert_eq!(first.path,"src/core.py");
        exchange(&root,&first.path,&tx.temp_paths[0]).unwrap();
        assert_eq!(fs::read(project.join("src/core.py")).unwrap(),b"new\n");
        // A crash during journal replacement leaves scratch behind.
        write(&journal,"transaction.new",b"interrupted journal update");
        let recovered=recover(&journal).unwrap();assert_eq!(recovered.state,"rolled_back");
        assert_eq!(fs::read(project.join("src/core.py")).unwrap(),b"old\n");
        assert!(!project.join("tests/test_core.py").exists());
        assert!(apply_with_journal(&proposal,&journal).is_err());
    }
    #[test]
    fn committed_journal_recovers_receipt_and_retains_original_inode() {
        let (t,project,worker,base)=setup_git();write(&worker,"src/core.py",b"new\n");
        let proposal=preview_git(&worker,&project,&base,&control()).unwrap();
        let journal=t.path().join("journals/change");let result=apply_with_journal(&proposal,&journal).unwrap();
        assert_eq!(result.paths,["src/core.py"]);assert_eq!(fs::read(journal.join("backup-0")).unwrap(),b"old\n");
        assert_eq!(recover(&journal).unwrap().state,"committed");
        assert_eq!(apply_with_journal(&proposal,&journal).unwrap(),result);
        assert_eq!(fs::read(project.join("src/core.py")).unwrap(),b"new\n");
    }
    #[test]
    fn recovery_handles_before_staging_and_after_commit_marker() {
        let (t,project,worker,base)=setup_git();write(&worker,"src/core.py",b"new\n");
        let proposal=preview_git(&worker,&project,&base,&control()).unwrap();
        let root=open_root(&proposal.target).unwrap();
        let before=t.path().join("before");fs::DirBuilder::new().mode(0o700).create(&before).unwrap();
        let mut tx=tx_for(&proposal,&root,444);
        journal_write(&before,&tx).unwrap();assert_eq!(recover(&before).unwrap().state,"rolled_back");
        assert_eq!(fs::read(project.join("src/core.py")).unwrap(),b"old\n");
        let after=t.path().join("after");fs::DirBuilder::new().mode(0o700).create(&after).unwrap();
        tx.temp_paths=vec![staged_path("src/core.py",0,445)];journal_write(&after,&tx).unwrap();
        stage_all(&mut tx,&root);tx.states[0]="dispatching".into();journal_write(&after,&tx).unwrap();
        exchange(&root,"src/core.py",&tx.temp_paths[0]).unwrap();
        tx.states[0]="installed".into();tx.state="committed".into();journal_write(&after,&tx).unwrap();
        assert_eq!(recover(&after).unwrap().state,"committed");
        assert_eq!(fs::read(project.join("src/core.py")).unwrap(),b"new\n");
        assert_eq!(fs::read(after.join("backup-0")).unwrap(),b"old\n");
    }
    #[test]
    fn recovery_rejects_forged_paths_and_symlinked_journals() {
        let (t,project,worker,base)=setup_git();write(&worker,"src/core.py",b"new\n");
        let proposal=preview_git(&worker,&project,&base,&control()).unwrap();
        let dir=t.path().join("journal");fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        let root=open_root(&proposal.target).unwrap();let mut tx=tx_for(&proposal,&root,555);tx.temp_paths=vec!["../../outside".into()];
        journal_write(&dir,&tx).unwrap();assert!(recover(&dir).is_err());
        fs::remove_file(dir.join("transaction.json")).unwrap();
        symlink(project.join("src/core.py"),dir.join("transaction.json")).unwrap();assert!(recover(&dir).is_err());
        assert_eq!(fs::read(project.join("src/core.py")).unwrap(),b"old\n");
    }
    #[test]
    fn unexpected_external_change_is_retained_and_not_overwritten() {
        let (t,project,worker,base)=setup_git();write(&worker,"src/core.py",b"new\n");
        let proposal=preview_git(&worker,&project,&base,&control()).unwrap();let root=open_root(&proposal.target).unwrap();
        let journal=t.path().join("journal");fs::DirBuilder::new().mode(0o700).create(&journal).unwrap();
        let mut tx=tx_for(&proposal,&root,333);journal_write(&journal,&tx).unwrap();
        stage_all(&mut tx,&root);tx.states[0]="dispatching".into();journal_write(&journal,&tx).unwrap();
        // External edits arrive between preflight and the exchange. The exchange
        // retains that exact file and recovery restores it without deleting it.
        write(&project,"src/core.py",b"external user edit\n");exchange(&root,"src/core.py",&tx.temp_paths[0]).unwrap();
        assert!(recover(&journal).is_err());assert_eq!(fs::read(project.join("src/core.py")).unwrap(),b"external user edit\n");
        assert!(recover(&journal).is_err(),"changed original remains uncertain on retry");
    }
    #[test]
    fn file_permissions_are_preserved_and_hardlinks_fail_closed() {
        let t=TempDir::new().unwrap();let src=t.path().join("src");let dst=t.path().join("dst");fs::create_dir(&src).unwrap();fs::create_dir(&dst).unwrap();
        write(&src,"private.py",b"private\n");fs::set_permissions(src.join("private.py"),fs::Permissions::from_mode(0o600)).unwrap();
        let mapping=[FileMapping{source:"private.py".into(),destination:"private.py".into()}];
        let proposal=preview_files(&src,&dst,&mapping).unwrap();apply_with_journal(&proposal,&t.path().join("journal")).unwrap();
        let result=fs::metadata(dst.join("private.py")).unwrap();assert_eq!(result.permissions().mode() & 0o777,0o600);assert_eq!(result.nlink(),1);
        fs::hard_link(src.join("private.py"),src.join("alias")).unwrap();assert!(preview_files(&src,&dst,&mapping).is_err());
    }
    #[test]
    fn git_private_mode_is_not_widened_and_extended_attributes_are_rejected() {
        let (t,project,worker,base)=setup_git();write(&worker,"src/core.py",b"worker\n");
        fs::set_permissions(project.join("src/core.py"),fs::Permissions::from_mode(0o600)).unwrap();
        let proposal=preview_git(&worker,&project,&base,&control()).unwrap();assert!(!proposal.has_conflicts());
        apply_with_journal(&proposal,&t.path().join("journals/private")).unwrap();
        assert_eq!(fs::metadata(project.join("src/core.py")).unwrap().permissions().mode() & 0o777,0o600);
        #[cfg(target_os="macos")]
        {
            let (_t,project,worker,base)=setup_git();write(&worker,"src/core.py",b"worker\n");
            let set=Command::new("/usr/bin/xattr").args(["-w","com.example.handoff-test","metadata",project.join("src/core.py").to_str().unwrap()]).status().unwrap();
            assert!(set.success());assert!(preview_git(&worker,&project,&base,&control()).is_err());
        }
    }
    #[test]
    fn created_directories_are_removed_only_when_their_identity_is_verified() {
        let t=TempDir::new().unwrap();let src=t.path().join("src");let dst=t.path().join("dst");fs::create_dir(&src).unwrap();fs::create_dir(&dst).unwrap();write(&src,"code.py",b"code");
        let proposal=preview_files(&src,&dst,&[FileMapping{source:"code.py".into(),destination:"new/deep/code.py".into()}]).unwrap();let root=open_root(&proposal.target).unwrap();
        let journal=t.path().join("journal");fs::DirBuilder::new().mode(0o700).create(&journal).unwrap();let mut tx=tx_for(&proposal,&root,777);journal_write(&journal,&tx).unwrap();
        stage_all(&mut tx,&root);journal_write(&journal,&tx).unwrap();assert_eq!(recover(&journal).unwrap().state,"rolled_back");assert!(!dst.join("new").exists());
        // Crash after a mkdir but before recording its inode: preserve the
        // ambiguous directory and surface uncertainty instead of removing it.
        let journal2=t.path().join("journal2");fs::DirBuilder::new().mode(0o700).create(&journal2).unwrap();let tx2=tx_for(&proposal,&root,778);journal_write(&journal2,&tx2).unwrap();fs::create_dir(dst.join("new")).unwrap();
        assert!(recover(&journal2).is_err());assert!(dst.join("new").is_dir());
    }
    #[test]
    fn recovery_can_resume_after_a_partial_rollback() {
        let (t,project,worker,base)=setup_git();write(&worker,"src/core.py",b"new\n");write(&worker,"src/new.py",b"new file\n");
        let proposal=preview_git(&worker,&project,&base,&control()).unwrap();let root=open_root(&proposal.target).unwrap();
        let journal=t.path().join("journal");fs::DirBuilder::new().mode(0o700).create(&journal).unwrap();let mut tx=tx_for(&proposal,&root,888);stage_all(&mut tx,&root);
        for i in 0..tx.proposal.changes.len(){tx.states[i]="dispatching".into();}
        journal_write(&journal,&tx).unwrap();
        for (i,c) in tx.proposal.changes.iter().enumerate(){if c.before.is_none(){move_exclusive(&root,&tx.temp_paths[i],&c.path).unwrap();}else{exchange(&root,&c.path,&tx.temp_paths[i]).unwrap();}}
        // First rollback one addition and crash before recording rollback status.
        let index=tx.proposal.changes.iter().position(|c|c.before.is_none()).unwrap();
        move_exclusive(&root,&tx.proposal.changes[index].path,&tx.temp_paths[index]).unwrap();unlink_file(&root,&tx.temp_paths[index]).unwrap();
        assert_eq!(recover(&journal).unwrap().state,"rolled_back");
        assert_eq!(fs::read(project.join("src/core.py")).unwrap(),b"old\n");assert!(!project.join("src/new.py").exists());
        assert_eq!(recover(&journal).unwrap().state,"rolled_back");
    }
}
