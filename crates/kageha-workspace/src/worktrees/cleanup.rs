//! Managed cleanup only removes an unchanged checkout originally created by this journal.
use super::*;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Cleanup {
    pub id: String,
    pub creation_id: String,
    pub state: String,
    pub handle: Handle,
}
impl Git {
    fn branch_head(&self, branch: &str, control: &Control<'_>) -> io::Result<Option<String>> {
        let out = self.run(
            &self.root,
            &[
                "show-ref".into(),
                "--verify".into(),
                "--hash".into(),
                format!("refs/heads/{branch}"),
            ],
            control,
        )?;
        if out.success {
            return Ok(Some(
                String::from_utf8(out.bytes).map_err(err)?.trim().into(),
            ));
        }
        // show-ref --verify uses 128 for a missing ref too; distinguish by a
        // successful complete ref listing instead of treating all errors as absent.
        let refs = self.text(
            &["for-each-ref", "--format=%(refname)", "refs/heads/"],
            control,
        )?;
        if refs.lines().any(|r| r == format!("refs/heads/{branch}")) {
            return Err(err("Cannot read managed branch"));
        }
        Ok(None)
    }
    fn cleanup_ready(&self, handle: &Handle, control: &Control<'_>) -> io::Result<()> {
        self.require_project_root(control)?;
        self.worktree_directory()?;
        if handle.path.parent() != Some(self.root.join(".kageha/worktrees").as_path())
            || !handle.branch.starts_with("kageha/")
        {
            return Err(err("Cleanup requires an owned worktree"));
        }
        if !self.verified(handle, control)?
            || self.branch_head(&handle.branch, control)?.as_deref() != Some(handle.head.as_str())
        {
            return Err(err(
                "Worktree changed or unavailable; checkout and branch retained",
            ));
        }
        let mut args = self.filters_disabled(control)?;
        args.extend([
            "status".into(),
            "--porcelain=v1".into(),
            "-z".into(),
            "--untracked-files=all".into(),
            "--ignored=matching".into(),
        ]);
        let out = self.run(&handle.path, &args, control)?;
        if !out.success || !out.bytes.is_empty() {
            return Err(err(
                "Worktree contains tracked, untracked or ignored changes; retained",
            ));
        }
        Ok(())
    }
    fn checkout_absent(&self, handle: &Handle, control: &Control<'_>) -> io::Result<bool> {
        let absent = match fs::symlink_metadata(&handle.path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => true,
            Err(e) => return Err(e),
            Ok(_) => false,
        };
        Ok(absent && !self.list(control)?.iter().any(|h| h.path == handle.path))
    }
}
impl Worktrees {
    pub fn creation_id(&self, handle: &Handle) -> io::Result<Option<String>> {
        self.db.query_row("SELECT id FROM worktree_operations WHERE state='completed' AND json_extract(handle,'$.root')=? AND json_extract(handle,'$.path')=? AND json_extract(handle,'$.branch')=? LIMIT 1",params![handle.root.to_string_lossy(),handle.path.to_string_lossy(),handle.branch],|r|r.get(0)).optional().map_err(err)
    }
    pub fn cleanup_status(&self, id: &str) -> io::Result<Option<Cleanup>> {
        self.db
            .query_row(
                "SELECT creation_id,state,handle FROM worktree_cleanup WHERE id=?",
                [id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(err)?
            .map(|(creation_id, state, handle)| {
                Ok(Cleanup {
                    id: id.into(),
                    creation_id,
                    state,
                    handle: serde_json::from_str(&handle)?,
                })
            })
            .transpose()
    }
    pub fn cleanup_pending(&self, root: &Path) -> io::Result<Vec<Cleanup>> {
        let mut q=self.db.prepare("SELECT id FROM worktree_cleanup WHERE state!='completed' AND json_extract(handle,'$.root')=? ORDER BY rowid LIMIT 257").map_err(err)?;
        let ids = q
            .query_map([root.to_string_lossy()], |r| r.get::<_, String>(0))
            .map_err(err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(err)?;
        if ids.len() > 256 {
            return Err(err("Too many pending cleanup operations"));
        }
        ids.into_iter()
            .map(|id| {
                self.cleanup_status(&id)?
                    .ok_or_else(|| err("Cleanup receipt missing"))
            })
            .collect()
    }
    /// Exact request identity; unknown dispatched steps are never repeated.
    pub fn cleanup(
        &mut self,
        git: &Git,
        id: &str,
        creation: &str,
        control: &Control<'_>,
    ) -> io::Result<Cleanup> {
        if id.is_empty() || id.len() > 128 || id.chars().any(char::is_control) {
            return Err(err("Invalid cleanup identity"));
        }
        control.check()?;
        if let Some(op) = self.cleanup_status(id)? {
            if op.creation_id != creation || op.handle.root != git.root {
                return Err(err("Cleanup request identity conflict"));
            }
            if op.state != "prepared" && op.state != "worktree_removed" {
                return Ok(op);
            }
        } else {
            let created = self
                .status(creation)?
                .filter(|o| o.state == "completed" && o.handle.root == git.root)
                .ok_or_else(|| {
                    err("Cleanup requires a completed creation receipt in this repository")
                })?;
            git.cleanup_ready(&created.handle, control)?;
            let tx = self
                .db
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(err)?;
            let conflict:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM worktree_cleanup WHERE id!=?1 AND state!='completed' AND json_extract(handle,'$.root')=?2)",params![id,git.root.to_string_lossy()],|r|r.get(0)).map_err(err)?;
            if conflict {
                return Err(err("Another cleanup needs review in this repository"));
            }
            tx.execute(
                "INSERT OR IGNORE INTO worktree_cleanup VALUES (?,?,?,'prepared')",
                params![id, creation, serde_json::to_string(&created.handle)?],
            )
            .map_err(err)?;
            tx.commit().map_err(err)?;
        }
        let op = self
            .cleanup_status(id)?
            .ok_or_else(|| err("Cleanup receipt missing"))?;
        if op.creation_id != creation || op.handle.root != git.root {
            return Err(err("Concurrent cleanup identity conflict"));
        }
        if op.state == "prepared" {
            git.cleanup_ready(&op.handle, control)?;
            self.cleanup_admit(git, id, "prepared", "removing", control)?;
            let mut args = git.filters_disabled(control)?;
            args.extend([
                "worktree".into(),
                "remove".into(),
                "--".into(),
                op.handle
                    .path
                    .to_str()
                    .ok_or_else(|| err("Worktree path must be UTF-8"))?
                    .into(),
            ]);
            // Even a timeout leaves the durable removing intent. No retry here.
            let _ = git.run(&git.root, &args, control);
            if !git.checkout_absent(&op.handle, control).unwrap_or(false) {
                return self
                    .cleanup_status(id)?
                    .ok_or_else(|| err("Cleanup receipt missing"));
            }
            self.db.execute("UPDATE worktree_cleanup SET state='worktree_removed' WHERE id=? AND state='removing'",[id]).map_err(err)?;
        }
        let op = self.cleanup_status(id)?.unwrap();
        if op.state == "worktree_removed" {
            if !git.checkout_absent(&op.handle, control)?
                || git
                    .list(control)?
                    .iter()
                    .any(|h| h.branch == op.handle.branch)
            {
                return Err(err("Checkout or branch became active; retained"));
            }
            if git.branch_head(&op.handle.branch, control)?.as_deref()
                != Some(op.handle.head.as_str())
            {
                return Err(err(
                    "Managed branch changed or disappeared; inspect cleanup receipt",
                ));
            }
            self.cleanup_admit(git, id, "worktree_removed", "deleting_branch", control)?;
            // Compare-and-delete protects commits created since the last check.
            let _ = git.run(
                &git.root,
                &[
                    "update-ref".into(),
                    "-d".into(),
                    format!("refs/heads/{}", op.handle.branch),
                    op.handle.head.clone(),
                ],
                control,
            );
            if git.branch_head(&op.handle.branch, control).ok() == Some(None) {
                self.db.execute("UPDATE worktree_cleanup SET state='completed' WHERE id=? AND state='deleting_branch'",[id]).map_err(err)?;
            }
        }
        self.cleanup_status(id)?
            .ok_or_else(|| err("Cleanup receipt missing"))
    }
    fn cleanup_admit(
        &mut self,
        git: &Git,
        id: &str,
        from: &str,
        to: &str,
        control: &Control<'_>,
    ) -> io::Result<()> {
        control.check()?;
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(err)?;
        let busy:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM worktree_operations WHERE state IN ('dispatched','uncertain') AND json_extract(handle,'$.root')=?1) OR EXISTS(SELECT 1 FROM worktree_cleanup WHERE id!=?2 AND state!='completed' AND json_extract(handle,'$.root')=?1)",params![git.root.to_string_lossy(),id],|r|r.get(0)).map_err(err)?;
        if busy {
            return Err(err("Repository has another unresolved worktree operation"));
        }
        if tx
            .execute(
                "UPDATE worktree_cleanup SET state=? WHERE id=? AND state=?",
                params![to, id, from],
            )
            .map_err(err)?
            != 1
        {
            return Err(err("Cleanup state changed; inspect receipt"));
        }
        tx.commit().map_err(err)
    }
    /// Observation only. A verified removed checkout may then resume branch cleanup
    /// through cleanup(), but this method never dispatches either mutation.
    pub fn reconcile_cleanup(
        &mut self,
        git: &Git,
        id: &str,
        control: &Control<'_>,
    ) -> io::Result<Cleanup> {
        let op = self
            .cleanup_status(id)?
            .filter(|o| o.handle.root == git.root)
            .ok_or_else(|| err("Cleanup receipt not found in this repository"))?;
        let next = match op.state.as_str() {
            "removing" if git.checkout_absent(&op.handle, control)? => Some("worktree_removed"),
            "deleting_branch" | "worktree_removed"
                if git.checkout_absent(&op.handle, control)?
                    && git.branch_head(&op.handle.branch, control)?.is_none() =>
            {
                Some("completed")
            }
            _ => None,
        };
        if let Some(next) = next {
            self.db
                .execute(
                    "UPDATE worktree_cleanup SET state=? WHERE id=? AND state=?",
                    params![next, id, op.state],
                )
                .map_err(err)?;
        }
        self.cleanup_status(id)?
            .ok_or_else(|| err("Cleanup receipt missing"))
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{control, setup};
    use super::*;
    use std::sync::atomic::AtomicBool;
    #[test]
    fn cleanup_is_owned_idempotent_and_preserves_every_kind_of_work() {
        for change in ["none", "tracked", "untracked", "ignored", "committed"] {
            let (_temp, git, mut journal) = setup();
            let flag = AtomicBool::new(false);
            let c = control(&flag);
            // Establish ignored patterns in the immutable base before creation.
            fs::write(git.root.join(".git/info/exclude"), "ignored.log\n").unwrap();
            let created = journal
                .create(&git, "create", "worker", "HEAD", &c)
                .unwrap();
            let file = created.handle.path.join(match change {
                "ignored" => "ignored.log",
                "untracked" => "new.txt",
                _ => "evidence.txt",
            });
            if change != "none" {
                fs::write(&file, "retained user work").unwrap();
            }
            if change == "committed" {
                assert!(
                    git.run(
                        &created.handle.path,
                        &["add".into(), "evidence.txt".into()],
                        &c
                    )
                    .unwrap()
                    .success
                );
                assert!(
                    git.run(
                        &created.handle.path,
                        &[
                            "-c".into(),
                            "user.name=Fixture".into(),
                            "-c".into(),
                            "user.email=test@example.invalid".into(),
                            "-c".into(),
                            "commit.gpgsign=false".into(),
                            "commit".into(),
                            "-m".into(),
                            "worker".into()
                        ],
                        &c
                    )
                    .unwrap()
                    .success
                );
            }
            if change == "none" {
                assert_eq!(
                    journal.cleanup(&git, "remove", "create", &c).unwrap().state,
                    "completed"
                );
                assert!(!created.handle.path.exists());
                assert!(git
                    .branch_head(&created.handle.branch, &c)
                    .unwrap()
                    .is_none());
                assert_eq!(
                    journal.cleanup(&git, "remove", "create", &c).unwrap().state,
                    "completed"
                );
                assert!(journal.cleanup(&git, "remove", "other", &c).is_err());
            } else {
                assert!(
                    journal.cleanup(&git, "remove", "create", &c).is_err(),
                    "{change}"
                );
                assert_eq!(fs::read_to_string(file).unwrap(), "retained user work");
                assert!(git
                    .branch_head(&created.handle.branch, &c)
                    .unwrap()
                    .is_some());
            }
            assert!(journal.cleanup(&git, "unowned", "unknown", &c).is_err());
        }
    }
    #[test]
    fn cleanup_recovery_never_replays_and_branch_compare_delete_preserves_new_commits() {
        let (temp, git, mut journal) = setup();
        let flag = AtomicBool::new(false);
        let c = control(&flag);
        let created = journal
            .create(&git, "create", "worker", "HEAD", &c)
            .unwrap();
        journal
            .db
            .execute(
                "INSERT INTO worktree_cleanup VALUES ('remove','create',?,'removing')",
                [serde_json::to_string(&created.handle).unwrap()],
            )
            .unwrap();
        assert_eq!(
            journal.cleanup(&git, "remove", "create", &c).unwrap().state,
            "removing"
        );
        assert!(created.handle.path.exists());
        assert!(journal.create(&git, "next", "worker", "HEAD", &c).is_err());
        assert!(
            git.run(
                &git.root,
                &[
                    "worktree".into(),
                    "remove".into(),
                    created.handle.path.to_str().unwrap().into()
                ],
                &c
            )
            .unwrap()
            .success
        );
        drop(journal);
        let mut journal = Worktrees::open(&temp.path().join("journal/worktrees.sqlite")).unwrap();
        assert_eq!(
            journal.reconcile_cleanup(&git, "remove", &c).unwrap().state,
            "worktree_removed"
        );
        assert!(
            git.branch_head(&created.handle.branch, &c)
                .unwrap()
                .is_some(),
            "observation must not delete the branch"
        );
        assert_eq!(
            journal.cleanup(&git, "remove", "create", &c).unwrap().state,
            "completed"
        );
        journal
            .db
            .execute(
                "UPDATE worktree_cleanup SET state='deleting_branch' WHERE id='remove'",
                [],
            )
            .unwrap();
        assert_eq!(
            journal.reconcile_cleanup(&git, "remove", &c).unwrap().state,
            "completed"
        );
        let created = journal
            .create(&git, "second", "worker", "HEAD", &c)
            .unwrap();
        let out = git
            .run(
                &git.root,
                &[
                    "-c".into(),
                    "user.name=Fixture".into(),
                    "-c".into(),
                    "user.email=test@example.invalid".into(),
                    "commit-tree".into(),
                    "HEAD^{tree}".into(),
                    "-p".into(),
                    "HEAD".into(),
                    "-m".into(),
                    "preserved new commit".into(),
                ],
                &c,
            )
            .unwrap();
        assert!(out.success);
        let new_head = String::from_utf8(out.bytes).unwrap().trim().to_owned();
        assert!(
            git.run(
                &git.root,
                &[
                    "update-ref".into(),
                    format!("refs/heads/{}", created.handle.branch),
                    new_head.clone()
                ],
                &c
            )
            .unwrap()
            .success
        );
        assert!(journal
            .cleanup(&git, "remove-second", "second", &c)
            .is_err());
        assert_eq!(
            git.branch_head(&created.handle.branch, &c).unwrap(),
            Some(new_head)
        );
        assert!(created.handle.path.exists());
    }
    #[test]
    fn cancellation_foreign_roots_and_symlink_replacement_cannot_remove_files() {
        use std::os::unix::fs::symlink;
        let (temp, git, mut journal) = setup();
        let flag = AtomicBool::new(false);
        let c = control(&flag);
        let op = journal
            .create(&git, "created", "worker", "HEAD", &c)
            .unwrap();
        let (_other, other_git, _) = setup();
        assert!(journal
            .cleanup(&other_git, "foreign", "created", &c)
            .is_err());
        let cancelled = AtomicBool::new(true);
        assert!(journal
            .cleanup(&git, "cancelled", "created", &control(&cancelled))
            .is_err());
        assert!(journal.cleanup_status("cancelled").unwrap().is_none());
        let saved = temp.path().join("saved-checkout");
        fs::rename(&op.handle.path, &saved).unwrap();
        symlink(&saved, &op.handle.path).unwrap();
        assert!(journal.cleanup(&git, "symlink", "created", &c).is_err());
        assert!(saved.join("evidence.txt").exists());
        assert!(fs::symlink_metadata(&op.handle.path)
            .unwrap()
            .file_type()
            .is_symlink());
        fs::remove_file(&op.handle.path).unwrap();
        fs::rename(saved, &op.handle.path).unwrap();
        journal
            .db
            .execute(
                "INSERT INTO worktree_cleanup VALUES ('held','created',?,'removing')",
                [serde_json::to_string(&op.handle).unwrap()],
            )
            .unwrap();
        let outside = journal
            .create(&other_git, "other", "worker", "HEAD", &c)
            .unwrap();
        assert_eq!(outside.state, "completed");
        assert!(journal
            .cleanup(&git, "fresh-removal", "created", &c)
            .is_err());
        assert!(op.handle.path.exists());
    }
}
