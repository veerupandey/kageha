//! Component-owned approval journal and blocking approval adapter. No application tables.
use fs2::FileExt;
use kageha_contracts::{execution::Control, policy::*, ErrorCode, Result, ServiceError};
use rusqlite::{params, Connection, OptionalExtension};
use std::{io, path::Path, sync::Mutex, time::Duration};
pub struct Journal {
    db: Mutex<Connection>,
    pub requirements: crate::requirements::Requirements,
    _lock: std::fs::File,
}
fn error(e: impl std::fmt::Display) -> ServiceError {
    ServiceError::new(ErrorCode::Unavailable, format!("Policy journal: {e}"))
}
fn conflict(s: &str) -> ServiceError {
    ServiceError::new(ErrorCode::Conflict, s)
}
fn encode<T: serde::Serialize>(v: &T) -> Result<String> {
    serde_json::to_string(v).map_err(error)
}
fn row(db: &Connection, id: &str) -> Result<Option<ApprovalRecord>> {
    db.query_row("SELECT body FROM approvals WHERE id=?", [id], |r| {
        r.get::<_, String>(0)
    })
    .optional()
    .map_err(error)?
    .map(|v| serde_json::from_str(&v).map_err(error))
    .transpose()
}
fn save(db: &Connection, record: &ApprovalRecord) -> Result<()> {
    let state = encode(&record.state)?;
    db.execute(
        "UPDATE approvals SET state=?,body=? WHERE id=?",
        params![state.trim_matches('"'), encode(record)?, record.id],
    )
    .map_err(error)?;
    db.execute(
        "INSERT INTO policy_events(session,body) VALUES (?,?)",
        params![record.request.authority.session, encode(record)?],
    )
    .map_err(error)?;
    Ok(())
}
impl Journal {
    pub fn open(path: &Path) -> Result<Self> {
        let rules = std::env::var_os("KAGEHA_POLICY_REQUIREMENTS")
            .map(|p| crate::requirements::Requirements::load(Path::new(&p)))
            .transpose()
            .map_err(error)?
            .unwrap_or_default();
        Self::open_with_requirements(path, rules)
    }
    pub fn open_with_requirements(
        path: &Path,
        requirements: crate::requirements::Requirements,
    ) -> Result<Self> {
        requirements.validate().map_err(error)?;
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)
                .map_err(error)?;
        }
        for p in [path.to_owned(), path.with_extension("lock")] {
            if std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(conflict("Policy journal paths cannot be symlinks"));
            }
        }
        let mut options = std::fs::OpenOptions::new();
        options.create(true).read(true).write(true).mode(0o600);
        let lock = options.open(path.with_extension("lock")).map_err(error)?;
        lock.try_lock_exclusive()
            .map_err(|_| conflict("Policy journal already has a writer"))?;
        drop(options.open(path).map_err(error)?);
        let db = Connection::open(path).map_err(error)?;
        db.busy_timeout(Duration::from_secs(5)).map_err(error)?;
        let version: u32 = db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(error)?;
        if version > 3 {
            return Err(conflict(
                "Policy journal schema is newer than this component",
            ));
        }
        {
            let mut q=db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'").map_err(error)?;
            let names = q
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(error)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(error)?;
            if names.iter().any(|n| {
                !matches!(
                    n.as_str(),
                    "approvals"
                        | "policy_events"
                        | "policy_settings"
                        | "policy_imports"
                        | "policy_tombstones"
                        | "policy_profiles"
                        | "policy_delegations"
                        | "policy_overrides"
                )
            }) {
                return Err(conflict("Expected a dedicated policy journal"));
            }
        }
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;
          CREATE TABLE IF NOT EXISTS approvals(sequence INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE,session TEXT,turn TEXT,state TEXT,body TEXT);
          CREATE INDEX IF NOT EXISTS policy_pending ON approvals(session,turn,state,sequence);
          CREATE TABLE IF NOT EXISTS policy_events(sequence INTEGER PRIMARY KEY AUTOINCREMENT,session TEXT,body TEXT);
          CREATE TABLE IF NOT EXISTS policy_settings(id INTEGER PRIMARY KEY CHECK(id=1),mode TEXT NOT NULL);
          INSERT OR IGNORE INTO policy_settings VALUES (1,'ask');
          CREATE TABLE IF NOT EXISTS policy_imports(source TEXT PRIMARY KEY,cursor INTEGER NOT NULL);
          CREATE TABLE IF NOT EXISTS policy_tombstones(session TEXT PRIMARY KEY);
          CREATE TABLE IF NOT EXISTS policy_profiles(session TEXT PRIMARY KEY,profile TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS policy_delegations(child TEXT PRIMARY KEY,parent TEXT NOT NULL);
          CREATE TABLE IF NOT EXISTS policy_overrides(approval TEXT PRIMARY KEY,session TEXT NOT NULL,consumed INTEGER NOT NULL DEFAULT 0);
          CREATE INDEX IF NOT EXISTS policy_event_approval ON policy_events(json_extract(body,'$.approval_id'),sequence);
          CREATE INDEX IF NOT EXISTS policy_delegate_owner ON policy_delegations(parent,child);
          PRAGMA user_version=3;").map_err(error)?;
        let journal = Self {
            db: Mutex::new(db),
            requirements,
            _lock: lock,
        };
        journal.recover()?;
        Ok(journal)
    }
    pub fn recover(&self) -> Result<()> {
        let mut db = self.db.lock().map_err(error)?;
        let tx = db.transaction().map_err(error)?;
        let mut q = tx
            .prepare("SELECT id FROM approvals WHERE state IN ('pending','approved')")
            .map_err(error)?;
        let ids = q
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(error)?;
        drop(q);
        for id in ids {
            let mut record = row(&tx, &id)?.unwrap();
            record.state = ApprovalState::Interrupted;
            record.revision += 1;
            save(&tx, &record)?;
        }
        tx.execute(
            "UPDATE policy_settings SET mode='ask' WHERE mode='session'",
            [],
        )
        .map_err(error)?;
        tx.commit().map_err(error)
    }
    pub fn create(
        &self,
        id: &str,
        request: &ApprovalRequest,
        automatic: bool,
    ) -> Result<ApprovalRecord> {
        self.check_request(request).map_err(error)?;
        crate::DefaultPolicy
            .evaluate(request, PermissionMode::Ask)
            .map_err(|e| ServiceError::new(ErrorCode::Invalid, e.to_string()))?;
        if automatic && self.requires_manual(&request.action) {
            return Err(conflict("Managed policy requires manual approval"));
        }
        if automatic
            && !matches!(
                request.class,
                ActionClass::ReadOnly | ActionClass::RoutineFill
            )
        {
            return Err(ServiceError::new(
                ErrorCode::Unauthorized,
                "This operation requires an explicit decision",
            ));
        }
        if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
            return Err(ServiceError::new(ErrorCode::Invalid, "Invalid approval id"));
        }
        let mut db = self.db.lock().map_err(error)?;
        let tx = db.transaction().map_err(error)?;
        let erased: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM policy_tombstones WHERE session=?)",
                [&request.authority.session],
                |r| r.get(0),
            )
            .map_err(error)?;
        if erased {
            return Err(conflict("Approval session was erased"));
        }
        if let Some(record) = row(&tx, id)? {
            if record.request != *request || record.automatic != automatic {
                return Err(conflict("Approval id reused for another request"));
            }
            return Ok(record);
        }
        let record = ApprovalRecord {
            id: id.into(),
            request: request.clone(),
            state: if automatic {
                ApprovalState::Approved
            } else {
                ApprovalState::Pending
            },
            revision: 1,
            decision: automatic.then_some(true),
            scope: GrantScope::Once,
            automatic,
        };
        tx.execute(
            "INSERT INTO approvals(id,session,turn,state,body) VALUES (?,?,?,?,?)",
            params![
                id,
                request.authority.session,
                request.authority.turn,
                if automatic { "approved" } else { "pending" },
                encode(&record)?
            ],
        )
        .map_err(error)?;
        tx.execute(
            "INSERT INTO policy_events(session,body) VALUES (?,?)",
            params![request.authority.session, encode(&record)?],
        )
        .map_err(error)?;
        tx.commit().map_err(error)?;
        Ok(record)
    }
    pub fn get(&self, id: &str) -> Result<Option<ApprovalRecord>> {
        row(&*self.db.lock().map_err(error)?, id)
    }
    pub fn list(
        &self,
        session: Option<&str>,
        turn: Option<&str>,
        pending_only: bool,
        after: u64,
        limit: usize,
    ) -> Result<Vec<(u64, ApprovalRecord)>> {
        if limit == 0 || limit > 256 || after > i64::MAX as u64 {
            return Err(ServiceError::new(
                ErrorCode::Invalid,
                "Invalid approval page",
            ));
        }
        let db = self.db.lock().map_err(error)?;
        let mut q=db.prepare("SELECT sequence,body FROM approvals WHERE sequence>? AND (? IS NULL OR session=?) AND (? IS NULL OR turn=?) AND (?=0 OR state='pending') ORDER BY sequence LIMIT ?").map_err(error)?;
        let rows = q
            .query_map(
                params![after, session, session, turn, turn, pending_only, limit],
                |r| Ok((r.get::<_, u64>(0)?, r.get::<_, String>(1)?)),
            )
            .map_err(error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(error)?;
        rows.into_iter()
            .map(|(n, s)| Ok((n, serde_json::from_str(&s).map_err(error)?)))
            .collect()
    }
    pub fn decide(&self, id: &str, decision: &ApprovalDecision) -> Result<ApprovalRecord> {
        let mut db = self.db.lock().map_err(error)?;
        let tx = db.transaction().map_err(error)?;
        let mut record = row(&tx, id)?
            .ok_or_else(|| ServiceError::new(ErrorCode::Invalid, "Approval not found"))?;
        record
            .request
            .authority
            .verify_current(&decision.authority)
            .map_err(|e| ServiceError::new(ErrorCode::StaleGeneration, e.to_string()))?;
        let scoped: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM policy_profiles WHERE session=?)",
                [&record.request.authority.session],
                |r| r.get(0),
            )
            .map_err(error)?;
        let scope = if scoped
            || self.requirements.restricted()
            || Self::owner_db(&tx, &record.request.authority.session)?
                != record.request.authority.session
        {
            GrantScope::Once
        } else {
            crate::resolved_scope(record.request.class, decision.scope, decision.approved)
        };
        if record.state != ApprovalState::Pending {
            if record.decision == Some(decision.approved)
                && record.scope == scope
                && matches!(
                    record.state,
                    ApprovalState::Approved | ApprovalState::Denied | ApprovalState::Consumed
                )
            {
                return Ok(record);
            }
            return Err(conflict("Approval is no longer pending"));
        }
        if record.revision != decision.expected_revision {
            return Err(conflict("Approval revision changed"));
        }
        record.state = if decision.approved {
            ApprovalState::Approved
        } else {
            ApprovalState::Denied
        };
        record.decision = Some(decision.approved);
        record.scope = scope;
        record.revision += 1;
        save(&tx, &record)?;
        if decision.approved && scope != GrantScope::Once {
            tx.execute(
                "UPDATE policy_settings SET mode=? WHERE id=1",
                [if scope == GrantScope::Full {
                    "full"
                } else {
                    "session"
                }],
            )
            .map_err(error)?;
        }
        tx.commit().map_err(error)?;
        Ok(record)
    }
    pub fn consume(&self, id: &str, authority: &Authority) -> Result<bool> {
        let mut db = self.db.lock().map_err(error)?;
        let tx = db.transaction().map_err(error)?;
        let mut record = row(&tx, id)?
            .ok_or_else(|| ServiceError::new(ErrorCode::Invalid, "Approval not found"))?;
        record
            .request
            .authority
            .verify_current(authority)
            .map_err(|e| ServiceError::new(ErrorCode::StaleGeneration, e.to_string()))?;
        if record.state != ApprovalState::Approved {
            return Ok(false);
        }
        record.state = ApprovalState::Consumed;
        record.revision += 1;
        save(&tx, &record)?;
        tx.commit().map_err(error)?;
        Ok(true)
    }
    pub fn cancel(&self, id: &str) -> Result<()> {
        let mut db = self.db.lock().map_err(error)?;
        let tx = db.transaction().map_err(error)?;
        if let Some(mut record) = row(&tx, id)? {
            if matches!(
                record.state,
                ApprovalState::Pending | ApprovalState::Approved
            ) {
                record.state = ApprovalState::Cancelled;
                record.revision += 1;
                save(&tx, &record)?;
            }
        }
        tx.commit().map_err(error)
    }
    pub fn cancel_turn(&self, session: &str, turn: &str) -> Result<()> {
        let mut db = self.db.lock().map_err(error)?;
        let tx = db.transaction().map_err(error)?;
        let mut q=tx.prepare("SELECT id FROM approvals WHERE session=? AND turn=? AND state IN ('pending','approved')").map_err(error)?;
        let ids = q
            .query_map(params![session, turn], |r| r.get::<_, String>(0))
            .map_err(error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(error)?;
        drop(q);
        for id in ids {
            let mut record = row(&tx, &id)?.unwrap();
            record.state = ApprovalState::Cancelled;
            record.revision += 1;
            save(&tx, &record)?;
        }
        tx.commit().map_err(error)
    }
    pub fn mode(&self) -> Result<PermissionMode> {
        let db = self.db.lock().map_err(error)?;
        let text: String = db
            .query_row("SELECT mode FROM policy_settings WHERE id=1", [], |r| {
                r.get(0)
            })
            .map_err(error)?;
        match text.as_str() {
            "ask" => Ok(PermissionMode::Ask),
            "session" => Ok(PermissionMode::Routine),
            "full" => Ok(PermissionMode::Full),
            _ => Err(conflict("Unknown stored permission mode")),
        }
    }
    pub fn set_mode(&self, mode: PermissionMode) -> Result<()> {
        if self.requirements.restricted() && mode != PermissionMode::Ask {
            return Err(ServiceError::new(
                ErrorCode::Unauthorized,
                "Managed requirements disable legacy broad grants",
            ));
        }
        self.db
            .lock()
            .map_err(error)?
            .execute(
                "UPDATE policy_settings SET mode=? WHERE id=1",
                [match mode {
                    PermissionMode::Ask => "ask",
                    PermissionMode::Routine => "session",
                    PermissionMode::Full => "full",
                }],
            )
            .map_err(error)?;
        Ok(())
    }

    /// None preserves the retained permission contract until the operator selects a profile.
    pub fn last_review(&self, id: &str) -> Result<Option<ReviewDecision>> {
        let db = self.db.lock().map_err(error)?;
        db.query_row("SELECT json_extract(body,'$.review') FROM policy_events WHERE json_extract(body,'$.approval_id')=? AND json_extract(body,'$.kind')='approval_review' ORDER BY sequence DESC LIMIT 1",[id],|r|r.get::<_,String>(0)).optional().map_err(error)?.map(|body|serde_json::from_str(&body).map_err(error)).transpose()
    }
    pub fn check_request(&self, request: &ApprovalRequest) -> io::Result<()> {
        self.check_action(&request.action)
    }
    pub fn check_action(&self, action: &str) -> io::Result<()> {
        if self.requirements.denied_actions.iter().any(|a| a == action) {
            return Err(stop(
                "Managed policy denies this action; do not circumvent it",
            ));
        }
        Ok(())
    }
    pub fn requires_manual(&self, action: &str) -> bool {
        self.requirements.manual_actions.iter().any(|a| a == action)
    }
    pub fn owner(&self, session: &str) -> Result<String> {
        let db = self.db.lock().map_err(error)?;
        Self::owner_db(&db, session)
    }
    fn owner_db(db: &Connection, session: &str) -> Result<String> {
        let mut current = session.to_owned();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..32 {
            if !seen.insert(current.clone()) {
                return Err(conflict("Permission delegation cycle"));
            }
            let parent: Option<String> = db
                .query_row(
                    "SELECT parent FROM policy_delegations WHERE child=?",
                    [&current],
                    |r| r.get(0),
                )
                .optional()
                .map_err(error)?;
            match parent {
                Some(p) => current = p,
                None => return Ok(current),
            }
        }
        Err(conflict("Permission delegation depth exceeded"))
    }
    pub fn delegate(&self, parent: &str, child: &str) -> Result<()> {
        if parent == child
            || [parent, child]
                .iter()
                .any(|s| s.is_empty() || s.len() > 4096 || s.chars().any(char::is_control))
        {
            return Err(conflict("Invalid delegated scope"));
        }
        let mut db = self.db.lock().map_err(error)?;
        let tx = db.transaction().map_err(error)?;
        if tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM policy_tombstones WHERE session IN (?,?))",
                params![parent, child],
                |r| r.get::<_, bool>(0),
            )
            .map_err(error)?
        {
            return Err(conflict("Session was erased"));
        }
        let owner = Self::owner_db(&tx, parent)?;
        if owner == child {
            return Err(conflict("Permission delegation cycle"));
        }
        let existing: Option<String> = tx
            .query_row(
                "SELECT parent FROM policy_delegations WHERE child=?",
                [child],
                |r| r.get(0),
            )
            .optional()
            .map_err(error)?;
        if existing.as_ref().is_some_and(|p| p != &owner) {
            return Err(conflict("Delegated scope already belongs to another chat"));
        }
        tx.execute(
            "INSERT OR IGNORE INTO policy_delegations VALUES (?,?)",
            params![child, owner],
        )
        .map_err(error)?;
        tx.commit().map_err(error)
    }
    pub fn denial_limit(&self, session: &str, turn: &str) -> Result<bool> {
        self.denial_limit_db(&*self.db.lock().map_err(error)?, session, turn)
    }
    fn denial_limit_db(&self, db: &Connection, session: &str, turn: &str) -> Result<bool> {
        let mut q=db.prepare("SELECT json_extract(e.body,'$.review.verdict') FROM policy_events e JOIN approvals a ON a.id=json_extract(e.body,'$.approval_id') WHERE a.session=? AND a.turn=? AND json_extract(e.body,'$.kind')='approval_review' ORDER BY e.sequence DESC LIMIT 50").map_err(error)?;
        let verdicts = q
            .query_map(params![session, turn], |r| r.get::<_, String>(0))
            .map_err(error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(error)?;
        Ok(verdicts.iter().take_while(|v| v.as_str() == "deny").count()
            >= self.requirements.consecutive_denials
            || verdicts.iter().filter(|v| v.as_str() == "deny").count()
                >= self.requirements.rolling_denials)
    }
    pub fn denials(&self, session: &str) -> Result<Vec<ApprovalRecord>> {
        let owner = self.owner(session)?;
        let db = self.db.lock().map_err(error)?;
        let mut q=db.prepare("SELECT a.body FROM approvals a WHERE (a.session=? OR a.session IN (SELECT child FROM policy_delegations WHERE parent=?1)) AND a.state='denied' AND EXISTS(SELECT 1 FROM policy_events e WHERE json_extract(e.body,'$.approval_id')=a.id AND json_extract(e.body,'$.review.verdict')='deny') ORDER BY a.sequence DESC LIMIT 10").map_err(error)?;
        let rows = q
            .query_map([owner], |r| r.get::<_, String>(0))
            .map_err(error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(error)?;
        rows.iter()
            .map(|s| serde_json::from_str(s).map_err(error))
            .collect()
    }
    /// User-only exact retry marker. It is not approval to dispatch input.
    pub fn request_override(&self, id: &str, authority: &Authority, revision: u64) -> Result<()> {
        if !self.requirements.allow_review_override {
            return Err(ServiceError::new(
                ErrorCode::Unauthorized,
                "Managed policy disables review overrides",
            ));
        }
        let mut db = self.db.lock().map_err(error)?;
        let tx = db.transaction().map_err(error)?;
        let record = row(&tx, id)?.ok_or_else(|| conflict("Unknown denied action"))?;
        record
            .request
            .authority
            .verify_current(authority)
            .map_err(|e| ServiceError::new(ErrorCode::StaleGeneration, e.to_string()))?;
        self.check_request(&record.request).map_err(error)?;
        if record.state != ApprovalState::Denied || record.revision != revision {
            return Err(conflict("Denied action changed"));
        }
        let review:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM policy_events WHERE json_extract(body,'$.approval_id')=? AND json_extract(body,'$.review.verdict')='deny')",[id],|r|r.get(0)).map_err(error)?;
        if !review {
            return Err(conflict(
                "Only automatic review denials can request a retry",
            ));
        }
        tx.execute(
            "INSERT INTO policy_overrides(approval,session) VALUES (?,?)",
            params![id, authority.session],
        )
        .map_err(|_| conflict("This action already has a one-use retry marker"))?;
        tx.execute(
            "INSERT INTO policy_events(session,body) VALUES (?,?)",
            params![
                authority.session,
                serde_json::json!({"kind":"user_review_override","approval_id":id}).to_string()
            ],
        )
        .map_err(error)?;
        tx.commit().map_err(error)
    }
    fn take_override(&self, request: &ApprovalRequest) -> io::Result<bool> {
        let mut db = self
            .db
            .lock()
            .map_err(|_| io::Error::other("Policy unavailable"))?;
        let tx = db.transaction().map_err(io::Error::other)?;
        let mut q=tx.prepare("SELECT o.approval,a.body FROM policy_overrides o JOIN approvals a ON a.id=o.approval WHERE o.session=? AND o.consumed=0 ORDER BY a.sequence DESC").map_err(io::Error::other)?;
        let rows = q
            .query_map([&request.authority.session], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(io::Error::other)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(io::Error::other)?;
        drop(q);
        for (id, body) in rows {
            let old: ApprovalRecord = serde_json::from_str(&body)?;
            let a = &old.request.authority;
            let b = &request.authority;
            if old.request.action == request.action
                && old.request.class == request.class
                && a.session == b.session
                && a.binding == b.binding
                && a.generation == b.generation
                && a.resource == b.resource
                && a.input_digest == b.input_digest
            {
                tx.execute(
                    "UPDATE policy_overrides SET consumed=1 WHERE approval=?",
                    [id],
                )
                .map_err(io::Error::other)?;
                tx.commit().map_err(io::Error::other)?;
                return Ok(true);
            }
        }
        tx.commit().map_err(io::Error::other)?;
        Ok(false)
    }
    fn profile_db(&self, db: &Connection, session: &str) -> Result<Option<PermissionProfile>> {
        let owner = Self::owner_db(db, session)?;
        if db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM policy_tombstones WHERE session IN (?,?))",
                params![session, owner],
                |r| r.get::<_, bool>(0),
            )
            .map_err(error)?
        {
            return Err(conflict("Session was erased"));
        }
        let stored = db
            .query_row(
                "SELECT profile FROM policy_profiles WHERE session=?",
                [owner],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(error)?
            .map(|s| serde_json::from_str::<PermissionProfile>(&s).map_err(error))
            .transpose()?;
        if let Some(p) = self.requirements.forced_profile {
            return Ok(Some(p));
        }
        Ok(match stored {
            Some(p) if self.requirements.allowed_profiles.contains(&p) => Some(p),
            Some(_) => Some(PermissionProfile::Ask),
            None if self.requirements.restricted() => Some(PermissionProfile::Ask),
            None => None,
        })
    }
    pub fn profile(&self, session: &str) -> Result<Option<PermissionProfile>> {
        self.profile_db(&*self.db.lock().map_err(error)?, session)
    }
    pub fn pending_family(&self, session: &str) -> Result<Vec<ApprovalRecord>> {
        let owner = self.owner(session)?;
        let db = self.db.lock().map_err(error)?;
        let mut q=db.prepare("SELECT body FROM approvals WHERE (session=? OR session IN (SELECT child FROM policy_delegations WHERE parent=?1)) AND state='pending' ORDER BY sequence LIMIT 256").map_err(error)?;
        let bodies = q
            .query_map([owner], |r| r.get::<_, String>(0))
            .map_err(error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(error)?;
        bodies
            .iter()
            .map(|body| serde_json::from_str(body).map_err(error))
            .collect()
    }
    pub fn set_profile(&self, session: &str, profile: PermissionProfile) -> Result<()> {
        if !self.requirements.allowed_profiles.contains(&profile)
            || self
                .requirements
                .forced_profile
                .is_some_and(|p| p != profile)
        {
            return Err(ServiceError::new(
                ErrorCode::Unauthorized,
                "Permission profile is disabled by managed policy",
            ));
        }
        if self.owner(session)? != session {
            return Err(conflict(
                "Change a delegated worker profile through its owning chat",
            ));
        }
        if session.is_empty() || session.len() > 4096 || session.chars().any(char::is_control) {
            return Err(ServiceError::new(
                ErrorCode::Invalid,
                "A valid chat scope is required",
            ));
        }
        let mut db = self.db.lock().map_err(error)?;
        let tx = db.transaction().map_err(error)?;
        if tx
            .query_row(
                "SELECT 1 FROM policy_tombstones WHERE session=?",
                [session],
                |_| Ok(()),
            )
            .optional()
            .map_err(error)?
            .is_some()
        {
            return Err(conflict("Session was erased"));
        }
        // Revocation wins over waiting decisions, including reviews still in flight.
        let mut q = tx
            .prepare("SELECT id FROM approvals WHERE (session=? OR session IN (SELECT child FROM policy_delegations WHERE parent=?1)) AND state IN ('pending','approved')")
            .map_err(error)?;
        let ids = q
            .query_map([session], |r| r.get::<_, String>(0))
            .map_err(error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(error)?;
        drop(q);
        for id in ids {
            let mut record = row(&tx, &id)?.unwrap();
            record.state = ApprovalState::Cancelled;
            record.revision += 1;
            save(&tx, &record)?;
        }
        tx.execute("DELETE FROM policy_overrides WHERE session=? OR session IN (SELECT child FROM policy_delegations WHERE parent=?1)",[session]).map_err(error)?;
        tx.execute("INSERT INTO policy_profiles VALUES (?,?) ON CONFLICT(session) DO UPDATE SET profile=excluded.profile", params![session,encode(&profile)?]).map_err(error)?;
        tx.execute(
            "INSERT INTO policy_events(session,body) VALUES (?,?)",
            params![
                session,
                serde_json::json!({"kind":"permission_profile","profile":profile}).to_string()
            ],
        )
        .map_err(error)?;
        tx.commit().map_err(error)
    }
    /// Creates a durable request before review. Every allow is consumed only once.
    /// An unavailable/invalid reviewer leaves a human decision pending; never allows.
    pub fn prepare_profile(
        &self,
        request: &ApprovalRequest,
        reviewer: Option<&dyn ReviewPort>,
        control: &Control<'_>,
    ) -> io::Result<ApprovalRecord> {
        control.check()?;
        self.check_request(request)?;
        let explicit_retry = self.take_override(request)?;
        if !explicit_retry
            && self
                .denial_limit(&request.authority.session, &request.authority.turn)
                .map_err(io::Error::other)?
        {
            return Err(stop("Automatic review denial limit reached. This turn has stopped; ask the user before continuing."));
        }
        let profile = self
            .profile(&request.authority.session)
            .map_err(io::Error::other)?
            .unwrap_or(PermissionProfile::Ask);
        let record = self
            .create(&uuid::Uuid::new_v4().to_string(), request, false)
            .map_err(io::Error::other)?;
        let review = if matches!(request.class, ActionClass::Plan | ActionClass::Restricted)
            || self.requires_manual(&request.action)
            || profile == PermissionProfile::Ask
        {
            None
        } else if profile == PermissionProfile::Full {
            Some(ReviewDecision {
                input_digest: request.authority.input_digest.clone(),
                verdict: ReviewVerdict::Allow,
                rationale: "Operator selected Full access for this chat".into(),
            })
        } else {
            let result = reviewer
                .ok_or_else(|| io::Error::other("Automatic reviewer unavailable"))
                .and_then(|r| {
                    if explicit_retry {
                        r.review_retry(request, control)
                    } else {
                        r.review(request, control)
                    }
                });
            control.check().inspect_err(|_| {
                let _ = self.cancel(&record.id);
            })?;
            match result {
                Ok(r)
                    if r.input_digest == request.authority.input_digest
                        && !r.rationale.trim().is_empty()
                        && r.rationale.len() <= 2048
                        && !r.rationale.chars().any(char::is_control) =>
                {
                    Some(r)
                }
                _ => Some(ReviewDecision {
                    input_digest: request.authority.input_digest.clone(),
                    verdict: ReviewVerdict::Ask,
                    rationale: "Automatic review unavailable or invalid; manual approval required"
                        .into(),
                }),
            }
        };
        if let Some(review) = review {
            control.check().inspect_err(|_| {
                let _ = self.cancel(&record.id);
            })?;
            // Decision + audit are fenced by the pending revision, which profile changes revoke.
            let mut db = self
                .db
                .lock()
                .map_err(|_| io::Error::other("Policy journal unavailable"))?;
            let tx = db.transaction().map_err(io::Error::other)?;
            let mut current = row(&tx, &record.id)
                .map_err(io::Error::other)?
                .ok_or_else(|| io::Error::other("Approval missing"))?;
            if current.state != ApprovalState::Pending
                || self
                    .profile_db(&tx, &request.authority.session)
                    .map_err(io::Error::other)?
                    != Some(profile)
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Permission profile changed during review",
                ));
            }
            if !explicit_retry
                && self
                    .denial_limit_db(&tx, &request.authority.session, &request.authority.turn)
                    .map_err(io::Error::other)?
            {
                current.state = ApprovalState::Cancelled;
                current.revision += 1;
                save(&tx, &current).map_err(io::Error::other)?;
                tx.commit().map_err(io::Error::other)?;
                return Err(stop(
                    "Automatic review denial limit reached during review; input was not dispatched",
                ));
            }
            if review.verdict != ReviewVerdict::Ask {
                current.state = if review.verdict == ReviewVerdict::Allow {
                    ApprovalState::Approved
                } else {
                    ApprovalState::Denied
                };
                current.decision = Some(review.verdict == ReviewVerdict::Allow);
                current.revision += 1;
                save(&tx, &current).map_err(io::Error::other)?;
            }
            tx.execute("INSERT INTO policy_events(session,body) VALUES (?,?)",params![request.authority.session,serde_json::json!({"kind":"approval_review","approval_id":record.id,"profile":profile,"review":review}).to_string()]).map_err(io::Error::other)?;
            tx.commit().map_err(io::Error::other)?;
        }
        control.check()?;
        let outcome = self
            .get(&record.id)
            .map_err(io::Error::other)?
            .ok_or_else(|| io::Error::other("Approval missing"))?;
        if outcome.state == ApprovalState::Denied
            && self
                .denial_limit(&request.authority.session, &request.authority.turn)
                .map_err(io::Error::other)?
        {
            return Err(stop("Automatic review denial limit reached; this turn has stopped. Choose a materially safer alternative or ask the user."));
        }
        Ok(outcome)
    }
    pub fn import_cursor(&self, source: &str) -> Result<Option<u64>> {
        self.db
            .lock()
            .map_err(error)?
            .query_row(
                "SELECT cursor FROM policy_imports WHERE source=?",
                [source],
                |r| r.get(0),
            )
            .optional()
            .map_err(error)
    }
    pub fn import_batch(
        &self,
        source: &str,
        mode: PermissionMode,
        records: &[(u64, ApprovalRecord)],
    ) -> Result<()> {
        if records.iter().any(|(n, _)| *n == 0 || *n > i64::MAX as u64)
            || records.windows(2).any(|pair| pair[0].0 >= pair[1].0)
        {
            return Err(ServiceError::new(
                ErrorCode::Invalid,
                "Import records must have ordered source cursors",
            ));
        }
        let mut db = self.db.lock().map_err(error)?;
        let tx = db.transaction().map_err(error)?;
        let cursor: Option<u64> = tx
            .query_row(
                "SELECT cursor FROM policy_imports WHERE source=?",
                [source],
                |r| r.get(0),
            )
            .optional()
            .map_err(error)?;
        if cursor.is_none() {
            tx.execute(
                "UPDATE policy_settings SET mode=? WHERE id=1",
                [if mode == PermissionMode::Full {
                    "full"
                } else {
                    "ask"
                }],
            )
            .map_err(error)?;
        }
        let mut next = cursor.unwrap_or(0);
        for (sequence, original) in records {
            if *sequence <= next {
                continue;
            }
            let mut record = original.clone();
            record.request.authority.validate().map_err(error)?;
            if record.state == ApprovalState::Pending {
                record.state = ApprovalState::Interrupted;
                record.decision = None;
            } else if record.state == ApprovalState::Approved {
                record.state = ApprovalState::Consumed;
                record.decision = Some(true);
            }
            let erased: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM policy_tombstones WHERE session=?)",
                    [&record.request.authority.session],
                    |r| r.get(0),
                )
                .map_err(error)?;
            let existing = row(&tx, &record.id)?;
            if existing
                .as_ref()
                .is_some_and(|prior| prior.request != record.request)
            {
                return Err(conflict(
                    "Imported approval conflicts with the policy journal",
                ));
            }
            if !erased && existing.is_none() {
                tx.execute(
                    "INSERT INTO approvals(id,session,turn,state,body) VALUES (?,?,?,?,?)",
                    params![
                        record.id,
                        record.request.authority.session,
                        record.request.authority.turn,
                        encode(&record.state)?.trim_matches('"'),
                        encode(&record)?
                    ],
                )
                .map_err(error)?;
                tx.execute(
                    "INSERT INTO policy_events(session,body) VALUES (?,?)",
                    params![record.request.authority.session, encode(&record)?],
                )
                .map_err(error)?;
            }
            next = *sequence;
        }
        tx.execute("INSERT INTO policy_imports VALUES (?,?) ON CONFLICT(source) DO UPDATE SET cursor=excluded.cursor",params![source,next]).map_err(error)?;
        tx.commit().map_err(error)
    }
    pub fn erase_session(&self, session: &str) -> Result<()> {
        let children = {
            let db = self.db.lock().map_err(error)?;
            let mut q = db
                .prepare("SELECT child FROM policy_delegations WHERE parent=?")
                .map_err(error)?;
            let rows = q
                .query_map([session], |r| r.get::<_, String>(0))
                .map_err(error)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(error)?;
            rows
        };
        for child in children {
            self.erase_session(&child)?;
        }
        let mut db = self.db.lock().map_err(error)?;
        let tx = db.transaction().map_err(error)?;
        tx.execute(
            "INSERT OR IGNORE INTO policy_tombstones VALUES (?)",
            [session],
        )
        .map_err(error)?;
        tx.execute("DELETE FROM approvals WHERE session=?", [session])
            .map_err(error)?;
        tx.execute("DELETE FROM policy_overrides WHERE session=?", [session])
            .map_err(error)?;
        tx.execute(
            "DELETE FROM policy_delegations WHERE child=? OR parent=?1",
            [session],
        )
        .map_err(error)?;
        tx.execute("DELETE FROM policy_profiles WHERE session=?", [session])
            .map_err(error)?;
        tx.execute("DELETE FROM policy_events WHERE session=?", [session])
            .map_err(error)?;
        tx.commit().map_err(error)
    }
}
impl Journal {
    pub fn wait(&self, id: &str, authority: &Authority, control: &Control<'_>) -> io::Result<bool> {
        loop {
            if let Err(e) = control.check() {
                self.cancel(id).map_err(io::Error::other)?;
                return Err(e);
            }
            let current = self
                .get(id)
                .map_err(io::Error::other)?
                .ok_or_else(|| io::Error::other("Approval erased"))?;
            match current.state {
                ApprovalState::Pending => std::thread::sleep(Duration::from_millis(25)),
                ApprovalState::Approved => {
                    control.check()?;
                    let allowed = self.consume(id, authority).map_err(io::Error::other)?;
                    control.check()?;
                    return Ok(allowed);
                }
                _ => return Ok(false),
            }
        }
    }
}
impl ApprovalPort for Journal {
    fn authorize_once(&self, request: &ApprovalRequest, control: &Control<'_>) -> io::Result<bool> {
        control.check()?;
        let record = self
            .create(&uuid::Uuid::new_v4().to_string(), request, false)
            .map_err(io::Error::other)?;
        self.wait(&record.id, &request.authority, control)
    }
}
