//! Checkout lifecycle shared by embedded and service hosts; no runtime/database types.
use super::Options;
use crate::worktrees::{Cleanup, Git, Operation, Worktrees};
use kageha_contracts::execution::{uncertain_stop, Control};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, OnceLock, Weak},
    time::Duration,
};
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub mode: String,
    pub source_root: PathBuf,
    pub root: Option<PathBuf>,
    pub options: Options,
    pub operation: Option<Operation>,
    pub cleanup: Option<Cleanup>,
    pub retained: bool,
    pub ready_for_input: bool,
    pub reason: Option<String>,
}
#[derive(Clone)]
pub struct Manager {
    source: PathBuf,
    journal: PathBuf,
    git: PathBuf,
}
fn lane(root: &Path) -> io::Result<Arc<Mutex<()>>> {
    static LANES: OnceLock<Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>> = OnceLock::new();
    let mut lanes = LANES
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| io::Error::other("Workspace queue unavailable"))?;
    lanes.retain(|_, v| v.strong_count() > 0);
    if let Some(l) = lanes.get(root).and_then(Weak::upgrade) {
        return Ok(l);
    }
    if lanes.len() >= 128 {
        return Err(io::Error::other("Workspace queue capacity reached"));
    }
    let l = Arc::new(Mutex::new(()));
    lanes.insert(root.into(), Arc::downgrade(&l));
    Ok(l)
}
fn enter<'a>(lane: &'a Mutex<()>, control: &Control<'_>) -> io::Result<MutexGuard<'a, ()>> {
    loop {
        control.check()?;
        match lane.try_lock() {
            Ok(g) => return Ok(g),
            Err(std::sync::TryLockError::WouldBlock) => {
                std::thread::sleep(Duration::from_millis(5))
            }
            Err(_) => return Err(io::Error::other("Workspace queue poisoned")),
        }
    }
}
impl Manager {
    pub fn new(source: &Path, journal: &Path, git: &Path) -> io::Result<Self> {
        if !git.is_absolute() {
            return Err(io::Error::other("Absolute Git executable required"));
        }
        let source = source.canonicalize()?;
        if !source.is_dir() {
            return Err(io::Error::other("Workspace directory required"));
        }
        Ok(Self {
            source,
            journal: journal.into(),
            git: git.into(),
        })
    }
    pub fn source(&self) -> &Path {
        &self.source
    }
    fn git(&self) -> io::Result<Git> {
        Git::new(&self.git, &self.source)
    }
    fn store(&self) -> io::Result<Worktrees> {
        Worktrees::open(&self.journal)
    }
    fn id(&self, key: &str) -> io::Result<String> {
        if key.is_empty() || key.len() > 96 || key.chars().any(char::is_control) {
            return Err(io::Error::other("Invalid worker workspace identity"));
        }
        Ok(format!("worker-{key}"))
    }
    pub fn validate_source(
        source: &Path,
        program: &Path,
        options: &Options,
        control: &Control<'_>,
    ) -> io::Result<()> {
        let canonical = Options::parse(&serde_json::to_value(options)?)?;
        if canonical != *options {
            return Err(io::Error::other("Noncanonical worker workspace policy"));
        }
        if !program.is_absolute() {
            return Err(io::Error::other("Absolute Git executable required"));
        }
        if options.isolated() {
            let git = Git::new(program, source)?;
            if !git.is_repository(control)? {
                return Err(io::Error::other(
                    "Worktree isolation requires a Git project; no shared-workspace fallback",
                ));
            }
            git.require_project_root(control)?;
        }
        Ok(())
    }
    pub fn validate(&self, options: &Options, control: &Control<'_>) -> io::Result<()> {
        Self::validate_source(&self.source, &self.git, options, control)
    }
    /// Observation only, shared by hosts after process loss. Never repeats Git mutations.
    pub fn reconcile(
        &self,
        key: &str,
        options: &Options,
        control: &Control<'_>,
    ) -> io::Result<Snapshot> {
        if !options.isolated() {
            return self.snapshot(key, options);
        }
        let id = self.id(key)?;
        let mut store = self.store()?;
        let git = self.git()?;
        if store.status(&id)?.is_some() {
            store.reconcile(&git, &id, control)?;
        }
        if store.cleanup_status(&format!("cleanup-{id}"))?.is_some() {
            store.reconcile_cleanup(&git, &format!("cleanup-{id}"), control)?;
        }
        self.snapshot(key, options)
    }
    pub fn prepare(
        &self,
        key: &str,
        options: &Options,
        control: &Control<'_>,
    ) -> io::Result<Snapshot> {
        self.validate(options, control)?;
        if !options.isolated() {
            return self.snapshot(key, options);
        }
        let id = self.id(key)?;
        let lane = lane(&self.source)?;
        let _guard = enter(&lane, control)?;
        let mut store = self.store()?;
        let op = store.create(&self.git()?, &id, "specialist", &options.base_ref, control)?;
        if op.state != "completed" {
            return Err(uncertain_stop("Worker checkout creation is unverified. Inspect its receipt before execution; no shared fallback was used."));
        }
        let snapshot = self.snapshot(key, options)?;
        if !snapshot.ready_for_input {
            return Err(io::Error::other(
                "Worker checkout is missing, changed or settled; it cannot be silently recreated",
            ));
        }
        Ok(snapshot)
    }
    pub fn snapshot(&self, key: &str, options: &Options) -> io::Result<Snapshot> {
        if !options.isolated() {
            return Ok(Snapshot {
                mode: "shared".into(),
                source_root: self.source.clone(),
                root: Some(self.source.clone()),
                options: options.clone(),
                operation: None,
                cleanup: None,
                retained: true,
                ready_for_input: true,
                reason: None,
            });
        }
        let id = self.id(key)?;
        let store = self.store()?;
        let op = store.status(&id)?;
        if op.as_ref().is_some_and(|o| o.handle.root != self.source) {
            return Err(io::Error::other(
                "Worker receipt belongs to another workspace",
            ));
        }
        let cleanup = store.cleanup_status(&format!("cleanup-{id}"))?;
        let ready = op.as_ref().is_some_and(|o| {
            o.state == "completed"
                && o.handle.path.canonicalize().ok().as_ref() == Some(&o.handle.path)
        }) && cleanup.is_none();
        Ok(Snapshot {
            mode: "worktree".into(),
            source_root: self.source.clone(),
            root: op.as_ref().map(|o| o.handle.path.clone()),
            options: options.clone(),
            operation: op,
            retained: cleanup.as_ref().is_none_or(|c| c.state != "completed"),
            cleanup,
            ready_for_input: ready,
            reason: None,
        })
    }
    pub fn execution_root(&self, key: &str, options: &Options) -> io::Result<PathBuf> {
        let s = self.snapshot(key, options)?;
        if !s.ready_for_input {
            return Err(io::Error::other(
                "Isolated checkout is unavailable; no workspace fallback",
            ));
        }
        s.root
            .ok_or_else(|| io::Error::other("Workspace root unavailable"))
    }
    pub fn settle(
        &self,
        key: &str,
        options: &Options,
        successful: bool,
        terminal: bool,
        control: &Control<'_>,
    ) -> io::Result<Snapshot> {
        let mut s = self.snapshot(key, options)?;
        if !options.isolated() || !terminal {
            return Ok(s);
        }
        if options.keep_worktree
            || !successful
            || s.operation.as_ref().is_none_or(|o| o.state != "completed")
        {
            s.reason = Some(
                if options.keep_worktree {
                    "keep_worktree"
                } else {
                    "stopped_or_unverified_run"
                }
                .into(),
            );
            return Ok(s);
        }
        let lane = lane(&self.source)?;
        let _guard = enter(&lane, control)?;
        let id = self.id(key)?;
        match self
            .store()?
            .cleanup(&self.git()?, &format!("cleanup-{id}"), &id, control)
        {
            Ok(_) => s = self.snapshot(key, options)?,
            Err(e) => s.reason = Some(e.to_string()),
        }
        Ok(s)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    #[test]
    fn queue_cancellation_keeps_other_repositories_available() {
        let one = tempfile::tempdir().unwrap();
        let two = tempfile::tempdir().unwrap();
        let l = lane(one.path()).unwrap();
        let g = l.lock().unwrap();
        let cancelled = AtomicBool::new(true);
        assert_eq!(
            enter(
                &l,
                &Control {
                    cancelled: &cancelled,
                    deadline: None
                }
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::Interrupted
        );
        let active = AtomicBool::new(false);
        let c = Control {
            cancelled: &active,
            deadline: None,
        };
        let other = lane(two.path()).unwrap();
        let _guard = enter(&other, &c).unwrap();
        drop(g);
        assert!(enter(&l, &c).is_ok());
    }
}
