//! Standalone Git worktree operations with durable dispatch/outcome receipts.
//! Hosts supply a trusted workspace and Git executable; there is no WebUI dependency.
mod cleanup;
use crate::process;
pub use cleanup::Cleanup;
use kageha_contracts::execution::Control;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};
fn err(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}
fn hash(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Handle {
    pub root: PathBuf,
    pub path: PathBuf,
    pub branch: String,
    pub head: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Operation {
    pub id: String,
    pub state: String,
    pub handle: Handle,
    pub error_code: Option<String>,
    pub label: String,
    pub base: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct Request {
    root: PathBuf,
    label: String,
    base: String,
}
pub struct Git {
    program: PathBuf,
    root: PathBuf,
}
impl Git {
    pub fn new(program: &Path, root: &Path) -> io::Result<Self> {
        let program = program.canonicalize()?;
        let meta = fs::metadata(&program)?;
        if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 {
            return Err(err("Git executable is unavailable"));
        }
        let root = root.canonicalize()?;
        if !root.is_dir() {
            return Err(err("Git workspace must be a directory"));
        }
        Ok(Self { program, root })
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    fn run(
        &self,
        at: &Path,
        args: &[String],
        control: &Control<'_>,
    ) -> io::Result<process::Output> {
        let deadline = Instant::now() + Duration::from_secs(30);
        let control = Control {
            cancelled: control.cancelled,
            deadline: Some(control.deadline.unwrap_or(deadline).min(deadline)),
        };
        let mut command = Command::new(&self.program);
        command
            .current_dir(at)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_NO_LAZY_FETCH", "1");
        command
            .args([
                "--no-pager",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "submodule.recurse=false",
            ])
            .args(args);
        process::run(&mut command, vec![], &control, 4 * 1024 * 1024)
    }
    fn text(&self, args: &[&str], control: &Control<'_>) -> io::Result<String> {
        let out = self.run(
            &self.root,
            &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            control,
        )?;
        if !out.success {
            return Err(err("Git inspection failed"));
        }
        String::from_utf8(out.bytes).map_err(err)
    }
    pub fn is_repository(&self, control: &Control<'_>) -> io::Result<bool> {
        let out = self.run(
            &self.root,
            &["rev-parse".into(), "--is-inside-work-tree".into()],
            control,
        )?;
        Ok(out.success && out.bytes == b"true\n")
    }
    /// A checkout of a repository subtree would expose files outside its source
    /// capability. Hosts must explicitly bind the repository's actual top level.
    pub fn require_project_root(&self, control: &Control<'_>) -> io::Result<()> {
        let top = self.text(&["rev-parse", "--show-toplevel"], control)?;
        let top = top.strip_suffix('\n').unwrap_or(&top);
        if Path::new(top).canonicalize()? != self.root {
            return Err(err("Worktree isolation requires the configured workspace to be the Git project root; subdirectory scope cannot be expanded"));
        }
        Ok(())
    }

    pub fn list(&self, control: &Control<'_>) -> io::Result<Vec<Handle>> {
        if !self.is_repository(control)? {
            return Ok(vec![]);
        }
        let raw = self.text(&["worktree", "list", "--porcelain", "-z"], control)?;
        let mut rows = Vec::new();
        let mut path = None;
        let mut branch = String::new();
        let mut head = String::new();
        for item in raw.split('\0') {
            if item.is_empty() {
                if let Some(path) = path.take() {
                    rows.push(Handle {
                        root: self.root.clone(),
                        path,
                        branch: std::mem::take(&mut branch),
                        head: std::mem::take(&mut head),
                    });
                }
                continue;
            }
            if let Some(value) = item.strip_prefix("worktree ") {
                path = Some(PathBuf::from(value));
            } else if let Some(value) = item.strip_prefix("branch ") {
                branch = value.trim_start_matches("refs/heads/").into();
            } else if let Some(value) = item.strip_prefix("HEAD ") {
                head = value.into();
            }
        }
        if let Some(path) = path {
            rows.push(Handle {
                root: self.root.clone(),
                path,
                branch,
                head,
            });
        }
        if rows.len() > 4096 {
            return Err(err("Git worktree list exceeds limits"));
        }
        Ok(rows)
    }
    fn commit(&self, base: &str, control: &Control<'_>) -> io::Result<String> {
        if base.is_empty()
            || base.len() > 256
            || base.starts_with('-')
            || base.chars().any(char::is_control)
        {
            return Err(err("Invalid worktree base reference"));
        }
        let out = self.text(
            &[
                "rev-parse",
                "--verify",
                "--end-of-options",
                &format!("{base}^{{commit}}"),
            ],
            control,
        )?;
        let sha = out.trim();
        if !matches!(sha.len(), 40 | 64) || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(err("Git did not resolve an immutable commit"));
        }
        Ok(sha.into())
    }
    fn filters_disabled(&self, control: &Control<'_>) -> io::Result<Vec<String>> {
        let out = self.run(
            &self.root,
            &[
                "config".into(),
                "--null".into(),
                "--name-only".into(),
                "--get-regexp".into(),
                "^filter\\.".into(),
            ],
            control,
        )?;
        if !out.success && out.exit_code != Some(1) {
            return Err(err("Cannot inspect Git checkout filters"));
        }
        let mut names = std::collections::BTreeSet::new();
        for key in String::from_utf8(out.bytes)
            .map_err(err)?
            .split('\0')
            .filter(|s| !s.is_empty())
        {
            let Some(name) = key
                .strip_prefix("filter.")
                .and_then(|v| v.rsplit_once('.'))
                .map(|v| v.0)
            else {
                return Err(err("Invalid Git filter configuration"));
            };
            if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
                return Err(err("Invalid Git filter name"));
            }
            names.insert(name.to_owned());
            if names.len() > 128 {
                return Err(err("Too many Git checkout filters"));
            }
        }
        Ok(names
            .into_iter()
            .flat_map(|name| {
                ["clean", "smudge", "process", "required"]
                    .into_iter()
                    .flat_map(move |field| {
                        [
                            "-c".into(),
                            format!(
                                "filter.{name}.{field}={}",
                                if field == "required" { "false" } else { "" }
                            ),
                        ]
                    })
            })
            .collect())
    }
    fn create(&self, handle: &Handle, control: &Control<'_>) -> io::Result<bool> {
        self.worktree_directory()?;
        if fs::symlink_metadata(&handle.path).is_ok() {
            return Err(err("Worktree destination already exists; files preserved"));
        }
        let mut args = self.filters_disabled(control)?;
        args.extend([
            "worktree".into(),
            "add".into(),
            "-b".into(),
            handle.branch.clone(),
            handle
                .path
                .to_str()
                .ok_or_else(|| err("Worktree path must be UTF-8"))?
                .into(),
            handle.head.clone(),
        ]);
        Ok(self.run(&self.root, &args, control)?.success)
    }
    fn worktree_directory(&self) -> io::Result<PathBuf> {
        let mut path = self.root.clone();
        for part in [".kageha", "worktrees"] {
            path.push(part);
            match fs::symlink_metadata(&path) {
                Ok(meta) => {
                    if !meta.is_dir() || meta.file_type().is_symlink() {
                        return Err(err("Worktree directories must not be symlinks or files"));
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    fs::DirBuilder::new().mode(0o700).create(&path)?;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(path)
    }
    fn verified(&self, handle: &Handle, control: &Control<'_>) -> io::Result<bool> {
        if !self
            .list(control)?
            .iter()
            .any(|h| h.path == handle.path && h.branch == handle.branch && h.head == handle.head)
        {
            return Ok(false);
        }
        let meta = match fs::symlink_metadata(&handle.path) {
            Ok(m) => m,
            Err(_) => return Ok(false),
        };
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Ok(false);
        }
        let mut args = self.filters_disabled(control)?;
        args.extend([
            "status".into(),
            "--porcelain=v1".into(),
            "-z".into(),
            "--untracked-files=all".into(),
        ]);
        let out = self.run(&handle.path, &args, control)?;
        Ok(out.success && out.bytes.is_empty())
    }
}
pub struct Worktrees {
    db: Connection,
}
impl Worktrees {
    pub fn open(path: &Path) -> io::Result<Self> {
        let parent = path
            .parent()
            .ok_or_else(|| err("Worktree journal requires a directory"))?;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
        let meta = fs::symlink_metadata(parent)?;
        if !meta.is_dir() || meta.file_type().is_symlink() || meta.permissions().mode() & 0o077 != 0
        {
            return Err(err("Worktree journal directory must be private"));
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        if !file.metadata()?.is_file() {
            return Err(err("Worktree journal must be regular"));
        }
        let db = Connection::open(path).map_err(err)?;
        db.busy_timeout(Duration::from_secs(2)).map_err(err)?;
        db.execute_batch("PRAGMA journal_mode=WAL;PRAGMA synchronous=FULL;CREATE TABLE IF NOT EXISTS worktree_schema(version INTEGER NOT NULL);INSERT INTO worktree_schema SELECT 1 WHERE NOT EXISTS(SELECT 1 FROM worktree_schema);").map_err(err)?;
        let version: u64 = db
            .query_row("SELECT version FROM worktree_schema", [], |r| r.get(0))
            .map_err(err)?;
        if version != 1 {
            return Err(err("Unsupported worktree journal version"));
        }
        db.execute_batch("CREATE TABLE IF NOT EXISTS worktree_operations(id TEXT PRIMARY KEY,request TEXT NOT NULL,handle TEXT NOT NULL,state TEXT NOT NULL,error TEXT);").map_err(err)?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS worktree_cleanup(id TEXT PRIMARY KEY,creation_id TEXT NOT NULL,handle TEXT NOT NULL,state TEXT NOT NULL);").map_err(err)?;
        Ok(Self { db })
    }
    pub fn status(&self, id: &str) -> io::Result<Option<Operation>> {
        self.db
            .query_row(
                "SELECT state,handle,error,request FROM worktree_operations WHERE id=?",
                [id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(err)?
            .map(|(state, handle, error_code, request)| {
                let request: Request = serde_json::from_str(&request)?;
                Ok(Operation {
                    id: id.into(),
                    state,
                    handle: serde_json::from_str(&handle)?,
                    error_code,
                    label: request.label,
                    base: request.base,
                })
            })
            .transpose()
    }
    pub fn pending(&self, root: &Path) -> io::Result<Vec<Operation>> {
        let mut q=self.db.prepare("SELECT id FROM worktree_operations WHERE state!='completed' AND json_extract(handle,'$.root')=? ORDER BY CASE WHEN state='prepared' THEN 1 ELSE 0 END,rowid LIMIT 257").map_err(err)?;
        let ids = q
            .query_map([root.to_string_lossy()], |r| r.get::<_, String>(0))
            .map_err(err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(err)?;
        if ids.len() > 256 {
            return Err(err("Too many pending worktree operations"));
        }
        ids.into_iter()
            .map(|id| {
                self.status(&id)?
                    .ok_or_else(|| err("Worktree receipt missing"))
            })
            .collect()
    }
    pub fn create(
        &mut self,
        git: &Git,
        id: &str,
        label: &str,
        base: &str,
        control: &Control<'_>,
    ) -> io::Result<Operation> {
        if id.is_empty() || id.len() > 128 || id.chars().any(char::is_control) || label.len() > 256
        {
            return Err(err("Invalid worktree request identity"));
        }
        control.check()?;
        git.require_project_root(control)?;
        let request = Request {
            root: git.root.clone(),
            label: label.into(),
            base: base.into(),
        };
        let request = serde_json::to_string(&request)?;
        let existing: Option<String> = self
            .db
            .query_row(
                "SELECT request FROM worktree_operations WHERE id=?",
                [id],
                |r| r.get(0),
            )
            .optional()
            .map_err(err)?;
        if let Some(existing) = existing {
            if existing != request {
                return Err(err("Worktree request ID was reused with different input"));
            }
            let operation = self.status(id)?.unwrap();
            if operation.state != "prepared" {
                return Ok(operation);
            }
        } else {
            let head = git.commit(base, control)?;
            git.worktree_directory()?;
            let slug = label
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                        c
                    } else {
                        '-'
                    }
                })
                .take(40)
                .collect::<String>();
            let slug = slug.trim_matches('-');
            let slug = if slug.is_empty() { "agent" } else { slug };
            let token = hash(format!("{}\0{id}", git.root.display()).as_bytes());
            let suffix = format!("{slug}-{}", &token[..24]);
            let handle = Handle {
                root: git.root.clone(),
                path: git.root.join(".kageha/worktrees").join(&suffix),
                branch: format!("kageha/{suffix}"),
                head,
            };
            self.db
                .execute(
                    "INSERT OR IGNORE INTO worktree_operations VALUES (?,?,?,'prepared',NULL)",
                    params![id, request, serde_json::to_string(&handle)?],
                )
                .map_err(err)?;
            let saved: String = self
                .db
                .query_row(
                    "SELECT request FROM worktree_operations WHERE id=?",
                    [id],
                    |r| r.get(0),
                )
                .map_err(err)?;
            if saved != request {
                return Err(err("Concurrent worktree request conflict"));
            }
        }
        let operation = self.status(id)?.unwrap();
        control.check()?;
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(err)?;
        let other:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM worktree_operations WHERE id!=?1 AND state IN ('dispatched','uncertain') AND json_extract(handle,'$.root')=?2) OR EXISTS(SELECT 1 FROM worktree_cleanup WHERE state!='completed' AND json_extract(handle,'$.root')=?2)",params![id,git.root.to_string_lossy()],|r|r.get(0)).map_err(err)?;
        if other {
            return Err(err(
                "A worktree mutation has an unresolved outcome in this repository",
            ));
        }
        let changed = tx
            .execute(
                "UPDATE worktree_operations SET state='dispatched' WHERE id=? AND state='prepared'",
                [id],
            )
            .map_err(err)?;
        tx.commit().map_err(err)?;
        if changed != 1 {
            return self
                .status(id)?
                .ok_or_else(|| err("Worktree receipt disappeared"));
        }
        let result = git.create(&operation.handle, control);
        let verified = result.as_ref().is_ok_and(|v| *v)
            && git.verified(&operation.handle, control).unwrap_or(false);
        let state = if verified { "completed" } else { "uncertain" };
        let error = if verified {
            None
        } else {
            Some("worktree_outcome_unverified")
        };
        self.db
            .execute(
                "UPDATE worktree_operations SET state=?,error=? WHERE id=? AND state='dispatched'",
                params![state, error, id],
            )
            .map_err(err)?;
        self.status(id)?
            .ok_or_else(|| err("Worktree receipt disappeared"))
    }
    /// Observation only. Never repeats an interrupted worktree mutation.
    pub fn reconcile(
        &mut self,
        git: &Git,
        id: &str,
        control: &Control<'_>,
    ) -> io::Result<Operation> {
        let operation = self
            .status(id)?
            .ok_or_else(|| err("Unknown worktree operation"))?;
        if operation.handle.root != git.root {
            return Err(err("Worktree receipt belongs to a different repository"));
        }
        if matches!(operation.state.as_str(), "dispatched" | "uncertain")
            && git.verified(&operation.handle, control)?
        {
            self.db.execute("UPDATE worktree_operations SET state='completed',error=NULL WHERE id=? AND state IN ('dispatched','uncertain')",[id]).map_err(err)?;
        }
        self.status(id)?
            .ok_or_else(|| err("Worktree receipt disappeared"))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    pub(super) fn setup() -> (tempfile::TempDir, Git, Worktrees) {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        fs::create_dir(&root).unwrap();
        let git = Path::new("/usr/bin/git");
        for args in [
            vec!["init", "-b", "main"],
            vec!["add", "evidence.txt"],
            vec![
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                "fixture",
            ],
        ] {
            if args[0] == "add" {
                fs::write(root.join("evidence.txt"), "Retained fixture contents").unwrap();
            }
            let out = Command::new(git)
                .current_dir(&root)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .args(&args)
                .output()
                .unwrap();
            assert!(out.status.success(), "fixture Git setup failed");
        }
        let journal = Worktrees::open(&temp.path().join("journal/worktrees.sqlite")).unwrap();
        let git = Git::new(git, &root).unwrap();
        (temp, git, journal)
    }
    pub(super) fn control(cancelled: &AtomicBool) -> Control<'_> {
        Control {
            cancelled,
            deadline: Some(Instant::now() + Duration::from_secs(10)),
        }
    }
    #[test]
    fn worktree_creation_is_idempotent_and_recovery_observes_without_replay() {
        let (temp, git, mut journal) = setup();
        let flag = AtomicBool::new(false);
        let c = control(&flag);
        let op = journal
            .create(&git, "create-one", "agent", "HEAD", &c)
            .unwrap();
        assert_eq!(op.state, "completed");
        assert_eq!(
            fs::read_to_string(op.handle.path.join("evidence.txt")).unwrap(),
            "Retained fixture contents"
        );
        assert_eq!(git.list(&c).unwrap().len(), 2);
        assert_eq!(
            journal
                .create(&git, "create-one", "agent", "HEAD", &c)
                .unwrap()
                .handle,
            op.handle
        );
        assert_eq!(git.list(&c).unwrap().len(), 2);
        assert!(journal
            .create(&git, "create-one", "changed", "HEAD", &c)
            .is_err());
        journal
            .db
            .execute(
                "UPDATE worktree_operations SET state='dispatched' WHERE id='create-one'",
                [],
            )
            .unwrap();
        drop(journal);
        let mut journal = Worktrees::open(&temp.path().join("journal/worktrees.sqlite")).unwrap();
        assert_eq!(
            journal
                .create(&git, "create-one", "agent", "HEAD", &c)
                .unwrap()
                .state,
            "dispatched"
        );
        assert_eq!(
            journal.reconcile(&git, "create-one", &c).unwrap().state,
            "completed"
        );
        assert_eq!(git.list(&c).unwrap().len(), 2);
    }
    #[test]
    fn unknown_dispatch_blocks_only_the_same_repository_and_never_replays() {
        let (_temp, git, mut journal) = setup();
        let flag = AtomicBool::new(false);
        let c = control(&flag);
        let op = journal
            .create(&git, "create-one", "agent", "HEAD", &c)
            .unwrap();
        fs::write(
            op.handle.path.join("evidence.txt"),
            "Human changed the file",
        )
        .unwrap();
        journal
            .db
            .execute("UPDATE worktree_operations SET state='uncertain'", [])
            .unwrap();
        assert_eq!(
            journal.reconcile(&git, "create-one", &c).unwrap().state,
            "uncertain"
        );
        assert!(journal
            .create(&git, "create-two", "other", "HEAD", &c)
            .is_err());
        assert_eq!(git.list(&c).unwrap().len(), 2);
        assert_eq!(
            fs::read_to_string(op.handle.path.join("evidence.txt")).unwrap(),
            "Human changed the file"
        );
        let (_other, other_git, _) = setup();
        assert_eq!(
            journal
                .create(&other_git, "other-repo", "agent", "HEAD", &c)
                .unwrap()
                .state,
            "completed"
        );
        assert!(journal.reconcile(&other_git, "create-one", &c).is_err());
    }
    #[test]
    fn cancelled_requests_bad_refs_and_symlink_destinations_dispatch_no_git_input() {
        let (temp, git, mut journal) = setup();
        let flag = AtomicBool::new(true);
        assert!(journal
            .create(&git, "cancelled", "agent", "HEAD", &control(&flag))
            .is_err());
        assert!(journal.status("cancelled").unwrap().is_none());
        flag.store(false, std::sync::atomic::Ordering::Relaxed);
        assert!(journal
            .create(&git, "bad-ref", "agent", "--help", &control(&flag))
            .is_err());
        assert!(journal.status("bad-ref").unwrap().is_none());
        fs::create_dir(git.root.join(".kageha")).unwrap();
        std::os::unix::fs::symlink(temp.path(), git.root.join(".kageha/worktrees")).unwrap();
        assert!(journal
            .create(&git, "escape", "agent", "HEAD", &control(&flag))
            .is_err());
        assert_eq!(git.list(&control(&flag)).unwrap().len(), 1);
    }
    #[test]
    fn configured_checkout_commands_and_hooks_are_not_executed() {
        let (temp, git, mut journal) = setup();
        let marker = temp.path().join("must-not-run");
        let command = format!("touch {}", marker.display());
        for key in [
            "filter.fixture.smudge",
            "filter.fixture.clean",
            "filter.fixture.process",
        ] {
            assert!(Command::new(&git.program)
                .current_dir(&git.root)
                .args(["config", key, &command])
                .status()
                .unwrap()
                .success());
        }
        fs::write(
            git.root.join(".gitattributes"),
            "evidence.txt filter=fixture\n",
        )
        .unwrap();
        // Commit with the test filter disabled; the configured filter remains hostile.
        for args in [
            vec!["add", ".gitattributes"],
            vec![
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                "attributes",
            ],
        ] {
            let mut cmd = Command::new(&git.program);
            cmd.current_dir(&git.root)
                .args([
                    "-c",
                    "filter.fixture.smudge=",
                    "-c",
                    "filter.fixture.clean=",
                    "-c",
                    "filter.fixture.process=",
                    "-c",
                    "filter.fixture.required=false",
                ])
                .args(args);
            assert!(cmd.status().unwrap().success());
        }
        fs::write(
            git.root.join(".git/hooks/post-checkout"),
            format!("#!/bin/sh\n{command}\n"),
        )
        .unwrap();
        fs::set_permissions(
            git.root.join(".git/hooks/post-checkout"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let flag = AtomicBool::new(false);
        let op = journal
            .create(&git, "filtered", "agent", "HEAD", &control(&flag))
            .unwrap();
        assert_eq!(op.state, "completed");
        assert!(!marker.exists());
    }
    #[test]
    fn subtree_workspaces_cannot_gain_access_to_parent_repository_files() {
        let (_temp, git, mut journal) = setup();
        let flag = AtomicBool::new(false);
        let c = control(&flag);
        let nested = git.root.join("narrow-scope");
        fs::create_dir(&nested).unwrap();
        let scoped = Git::new(Path::new("/usr/bin/git"), &nested).unwrap();
        assert!(scoped.is_repository(&c).unwrap());
        assert!(scoped.require_project_root(&c).is_err());
        assert!(journal
            .create(&scoped, "narrow", "worker", "HEAD", &c)
            .is_err());
        assert!(journal.status("narrow").unwrap().is_none());
        assert_eq!(git.list(&c).unwrap().len(), 1);
    }
}
