//! Runtime-owned journal. Never opens the harness or WebUI database.
use fs2::FileExt;
use kageha_contracts::{ErrorCode, Result, ServiceError};
use kageha_runtime::lifecycle::*;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use std::{path::Path, sync::Mutex};
mod request_budget;
pub struct SqliteRunStore {
    db: std::sync::Arc<Mutex<Connection>>,
    _lock: std::fs::File,
    incarnation: String,
}
fn error(e: impl std::fmt::Display) -> ServiceError {
    ServiceError::new(ErrorCode::Unavailable, format!("Runtime journal: {e}"))
}
fn conflict(s: &str) -> ServiceError {
    ServiceError::new(ErrorCode::Conflict, s)
}
fn encode(record: &RunRecord) -> Result<String> {
    serde_json::to_string(record).map_err(error)
}
impl SqliteRunStore {
    pub fn open(path: &Path) -> Result<Self> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)
                .map_err(error)?;
        }
        for file in [path.to_owned(), path.with_extension("lock")] {
            if std::fs::symlink_metadata(file).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(conflict("Runtime journal paths cannot be symlinks"));
            }
        }
        let mut options = std::fs::OpenOptions::new();
        options.create(true).write(true).read(true).mode(0o600);
        let lock = options.open(path.with_extension("lock")).map_err(error)?;
        lock.try_lock_exclusive()
            .map_err(|_| conflict("Runtime journal already has a writer"))?;
        drop(options.open(path).map_err(error)?);
        let db = Connection::open(path).map_err(error)?;
        let version: u32 = db
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(error)?;
        if version > 1 {
            return Err(conflict(
                "Runtime journal schema is newer than this runtime",
            ));
        }
        db.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(error)?;

        {
            let mut query=db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'").map_err(error)?;
            let tables = query
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(error)?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(error)?;
            if tables.iter().any(|t| {
                !matches!(
                    t.as_str(),
                    "runs"
                        | "steering"
                        | "effects"
                        | "runtime_events"
                        | "runtime_meta"
                        | "run_tombstones"
                        | "scope_tombstones"
                        | "runtime_late_receipts"
                        | "runtime_request_budgets"
                        | "runtime_request_activity"
                        | "runtime_request_recovery"
                )
            }) {
                return Err(conflict(
                    "Expected a dedicated runtime journal, not an application database",
                ));
            }
        }
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS runs(sequence INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE,session TEXT,depth INTEGER,state TEXT,body TEXT,published INTEGER NOT NULL DEFAULT 0,revision INTEGER NOT NULL DEFAULT 1); CREATE TABLE IF NOT EXISTS steering(run TEXT,id TEXT,body TEXT,delivered INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(run,id)); CREATE TABLE IF NOT EXISTS effects(scope TEXT,id TEXT,digest TEXT,state TEXT,result TEXT,PRIMARY KEY(scope,id)); CREATE TABLE IF NOT EXISTS run_tombstones(id TEXT PRIMARY KEY,session TEXT); CREATE TABLE IF NOT EXISTS scope_tombstones(scope TEXT PRIMARY KEY); CREATE TABLE IF NOT EXISTS runtime_late_receipts(sequence INTEGER PRIMARY KEY AUTOINCREMENT,run TEXT,lease TEXT,completion TEXT); CREATE TABLE IF NOT EXISTS runtime_events(sequence INTEGER PRIMARY KEY AUTOINCREMENT,run TEXT,body TEXT);").map_err(error)?;

        let has_revision: bool = db
            .prepare("PRAGMA table_info(runs)")
            .map_err(error)?
            .query_map([], |r| r.get::<_, String>(1))
            .map_err(error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(error)?
            .iter()
            .any(|v| v == "revision");
        if !has_revision {
            db.execute_batch("BEGIN IMMEDIATE; ALTER TABLE runs ADD COLUMN revision INTEGER NOT NULL DEFAULT 1; UPDATE runs SET revision=json_extract(body,'$.revision'); COMMIT;").map_err(error)?;
        }
        db.execute_batch("CREATE INDEX IF NOT EXISTS runs_admission ON runs(depth,state,sequence); CREATE INDEX IF NOT EXISTS runs_session_state ON runs(session,state); CREATE INDEX IF NOT EXISTS runs_turn ON runs(json_extract(body,'$.spec.turn')); CREATE UNIQUE INDEX IF NOT EXISTS runs_unique_turn ON runs(json_extract(body,'$.spec.turn')); CREATE INDEX IF NOT EXISTS runs_publication ON runs(sequence) WHERE published<revision;").map_err(error)?;
        db.pragma_update(None, "user_version", 1).map_err(error)?;
        request_budget::initialize(&db).map_err(error)?;
        Ok(Self {
            db: std::sync::Arc::new(Mutex::new(db)),
            _lock: lock,
            incarnation: uuid::Uuid::new_v4().to_string(),
        })
    }
    fn row(db: &Connection, id: &str) -> Result<Option<RunRecord>> {
        let body: Option<String> = db
            .query_row("SELECT body FROM runs WHERE id=?", [id], |r| r.get(0))
            .optional()
            .map_err(error)?;
        body.map(|s| serde_json::from_str(&s).map_err(error))
            .transpose()
    }
    fn write(db: &Connection, record: &RunRecord) -> Result<()> {
        let body = encode(record)?;
        let state = serde_json::to_string(&record.state).map_err(error)?;
        db.execute(
            "UPDATE runs SET state=?,body=?,revision=? WHERE id=?",
            params![
                state.trim_matches('"'),
                body,
                record.revision,
                record.spec.id
            ],
        )
        .map_err(error)?;
        db.execute(
            "INSERT INTO runtime_events(run,body) VALUES (?,?)",
            params![record.spec.id, body],
        )
        .map_err(error)?;
        Ok(())
    }
    fn change(
        &self,
        id: &str,
        update: impl FnOnce(&mut RunRecord) -> Result<()>,
    ) -> Result<RunRecord> {
        let mut db = self.db.lock().map_err(error)?;
        let tx = db.transaction().map_err(error)?;
        let mut record = Self::row(&tx, id)?.ok_or_else(|| conflict("Unknown runtime run"))?;
        let before = encode(&record)?;
        update(&mut record)?;
        if encode(&record)? == before {
            return Ok(record);
        }
        record.revision += 1;
        Self::write(&tx, &record)?;
        tx.commit().map_err(error)?;
        Ok(record)
    }
}
impl RunStore for SqliteRunStore {
    fn request_budget_begin(&self,run:&RunRecord)->Result<Option<kageha_contracts::request_budget::BudgetHandle>> {
        request_budget::begin(self.db.clone(),run).map_err(error)
    }
    fn recovery_checkpoint(&self,scope:&str)->Result<Value>{
        let db=self.db.lock().map_err(error)?;
        let body:Option<String>=db.query_row("SELECT state FROM runtime_request_recovery WHERE scope=?",[scope],|r|r.get(0)).optional().map_err(error)?;
        body.map(|body|serde_json::from_str(&body).map_err(error)).unwrap_or(Ok(Value::Null))
    }
    fn request_budget_snapshot(&self,root:&str)->Result<Value>{
        let db=self.db.lock().map_err(error)?;request_budget::snapshot(&db,root).map_err(error)
    }
    fn save_recovery_checkpoint(&self,scope:&str,state:&Value)->Result<()> {
        let db=self.db.lock().map_err(error)?;
        let deleted:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM scope_tombstones WHERE scope=?)",[scope],|r|r.get(0)).map_err(error)?;
        if deleted{return Err(conflict("Recovery scope was erased"))};
        db.execute("INSERT INTO runtime_request_recovery VALUES (?,?) ON CONFLICT(scope) DO UPDATE SET state=excluded.state",params![scope,state.to_string()]).map_err(error)?;
        Ok(())
    }
    fn events(&self, run: &str, after: u64, limit: usize) -> Result<Vec<RunEvent>> {
        if limit == 0 || limit > 256 || after > i64::MAX as u64 {
            return Err(conflict("Invalid event cursor or page limit"));
        }
        let db = self.db.lock().map_err(error)?;
        let mut q=db.prepare("SELECT sequence,body FROM runtime_events WHERE run=? AND sequence>? ORDER BY sequence LIMIT ?").map_err(error)?;
        let rows = q
            .query_map(params![run, after, limit], |r| {
                Ok((r.get::<_, u64>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(error)?;
        rows.into_iter()
            .map(|(sequence, body)| {
                Ok(RunEvent {
                    sequence,
                    record: serde_json::from_str(&body).map_err(error)?,
                })
            })
            .collect()
    }
    fn stage(&self, spec: &RunSpec, limit: usize) -> Result<RunRecord> {
        spec.validate()?;
        let mut db = self.db.lock().map_err(error)?;
        let tx = db.transaction().map_err(error)?;
        let deleted: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM run_tombstones WHERE id=?)",
                [&spec.id],
                |r| r.get(0),
            )
            .map_err(error)?;
        if deleted {
            return Err(conflict("Run belongs to a deleted session"));
        }
        if let Some(record) = Self::row(&tx, &spec.id)? {
            if record.spec != *spec {
                return Err(conflict("Runtime request ID reused for different input"));
            }
            return Ok(record);
        }
        let collision:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE json_extract(body,'$.spec.turn')=?) OR EXISTS(SELECT 1 FROM scope_tombstones WHERE scope=?)",params![spec.turn,spec.turn],|r|r.get(0)).map_err(error)?;
        if collision {
            return Err(conflict(
                "Turn identity is already bound to another run or was erased",
            ));
        }
        if let Some(parent) = &spec.parent_run {
            let parent = Self::row(&tx, parent)?.ok_or_else(|| conflict("Parent run not found"))?;
            if parent.cancel_requested
                || parent.state.terminal()
                || spec.session == parent.spec.session
                || spec.depth != parent.spec.depth + 1
                || spec.parent.as_deref() != Some(&parent.spec.session)
            {
                return Err(conflict("Parent run is stopped or its scope changed"));
            }
        }
        let active:u64=tx.query_row("SELECT count(*) FROM runs WHERE depth=? AND state IN ('staged','queued','running','cancelling')",[spec.depth],|r|r.get(0)).map_err(error)?;
        if active >= limit as u64 {
            return Err(conflict("Runtime admission queue is full"));
        }
        let record = RunRecord {
            spec: spec.clone(),
            state: RunState::Staged,
            revision: 1,
            lease: None,
            completion: None,
            cancel_requested: false,
        };
        tx.execute(
            "INSERT INTO runs(id,session,depth,state,body) VALUES (?,?,?,'staged',?)",
            params![spec.id, spec.session, spec.depth, encode(&record)?],
        )
        .map_err(error)?;
        tx.commit().map_err(error)?;
        Ok(record)
    }
    fn import_run(&self, record: &RunRecord) -> Result<()> {
        record.spec.validate()?;
        if record.revision == 0
            || record.lease.is_some()
            || record.state.terminal() && record.completion.is_none()
        {
            return Err(conflict("Imported run has incomplete lifecycle evidence"));
        }
        if !record.state.terminal() && record.state != RunState::Staged {
            return Err(conflict("Imported runs must be staged or terminal"));
        }
        let mut db = self.db.lock().map_err(error)?;
        let tx = db.transaction().map_err(error)?;
        let deleted: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM run_tombstones WHERE id=?)",
                [&record.spec.id],
                |r| r.get(0),
            )
            .map_err(error)?;
        if deleted {
            return Ok(());
        }
        if let Some(prior) = Self::row(&tx, &record.spec.id)? {
            if prior.spec != record.spec {
                return Err(conflict("Imported run conflicts with journal"));
            }
            return Ok(());
        }
        let state = serde_json::to_string(&record.state).map_err(error)?;
        tx.execute(
            "INSERT INTO runs(id,session,depth,state,body,revision) VALUES (?,?,?,?,?,?)",
            params![
                record.spec.id,
                record.spec.session,
                record.spec.depth,
                state.trim_matches('"'),
                encode(record)?,
                record.revision
            ],
        )
        .map_err(error)?;
        tx.commit().map_err(error)?;
        Ok(())
    }
    fn activate(&self, id: &str) -> Result<RunRecord> {
        self.change(id, |r| {
            if r.state == RunState::Staged {
                r.state = RunState::Queued;
            }
            Ok(())
        })
    }
    fn get(&self, id: &str) -> Result<Option<RunRecord>> {
        Self::row(&*self.db.lock().map_err(error)?, id)
    }
    fn find_turn(&self, turn: &str) -> Result<Option<RunRecord>> {
        let db = self.db.lock().map_err(error)?;
        let body:Option<String>=db.query_row("SELECT body FROM runs WHERE json_extract(body,'$.spec.turn')=? ORDER BY sequence DESC LIMIT 1",[turn],|r|r.get(0)).optional().map_err(error)?;
        body.map(|s| serde_json::from_str(&s).map_err(error))
            .transpose()
    }
    fn list(&self) -> Result<Vec<RunRecord>> {
        let db = self.db.lock().map_err(error)?;
        let mut q = db
            .prepare("SELECT body FROM runs ORDER BY sequence")
            .map_err(error)?;
        let rows = q
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(error)?;
        rows.into_iter()
            .map(|s| serde_json::from_str(&s).map_err(error))
            .collect()
    }
    fn active(&self) -> Result<Vec<RunRecord>> {
        let db = self.db.lock().map_err(error)?;
        let mut q=db.prepare("SELECT body FROM runs WHERE state IN ('staged','queued','running','cancelling') ORDER BY sequence").map_err(error)?;
        let rows = q
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(error)?;
        rows.into_iter()
            .map(|s| serde_json::from_str(&s).map_err(error))
            .collect()
    }
    fn claim(&self, depth: u32) -> Result<Option<RunRecord>> {
        'admission: loop {
            let mut db = self.db.lock().map_err(error)?;
            let tx = db.transaction().map_err(error)?;
            let id:Option<String>=tx.query_row("SELECT candidate.id FROM runs candidate WHERE depth=? AND state='queued' AND NOT EXISTS(SELECT 1 FROM runs active WHERE active.session=candidate.session AND active.state IN ('running','cancelling')) ORDER BY sequence LIMIT 1",[depth],|r|r.get(0)).optional().map_err(error)?;
            let Some(id) = id else { return Ok(None) };
            let mut record = Self::row(&tx, &id)?.unwrap();

            let mut parent = record.spec.parent_run.clone();
            let mut visited = std::collections::HashSet::new();
            while let Some(id) = parent {
                let ancestor = if visited.insert(id.clone()) {
                    Self::row(&tx, &id)?
                } else {
                    None
                };
                if ancestor.as_ref().is_none_or(|r| {
                    r.cancel_requested
                        || matches!(
                            r.state,
                            RunState::Failed | RunState::Interrupted | RunState::Cancelled
                        )
                }) {
                    record.state = RunState::Cancelled;
                    record.cancel_requested = true;
                    record.completion = Some(Completion {
                        state: RunState::Cancelled,
                        result: serde_json::json!({"not_dispatched":true,"reason":"Run ancestor is missing, stopped or cyclic"}),
                    });
                    record.revision += 1;
                    Self::write(&tx, &record)?;
                    tx.commit().map_err(error)?;
                    continue 'admission;
                }
                parent = ancestor.unwrap().spec.parent_run;
            }
            record.state = RunState::Running;
            record.lease = Some(format!("{}:{}", self.incarnation, uuid::Uuid::new_v4()));
            record.revision += 1;
            Self::write(&tx, &record)?;
            tx.commit().map_err(error)?;
            return Ok(Some(record));
        }
    }
    fn finish(&self, id: &str, lease: &str, mut completion: Completion) -> Result<RunRecord> {
        let receipt = completion.clone();
        let result = self.change(id, |r| {
            if !lease.starts_with(&format!("{}:", self.incarnation))
                || r.lease.as_deref() != Some(lease)
                || !matches!(r.state, RunState::Running | RunState::Cancelling)
            {
                return Err(ServiceError::new(
                    ErrorCode::StaleGeneration,
                    "Run receipt belongs to an obsolete execution",
                ));
            }
            if !completion.state.terminal() {
                return Err(conflict("Run completion must be terminal"));
            }
            if r.cancel_requested {
                completion.state = RunState::Cancelled;
            }
            r.state = completion.state;
            r.completion = Some(completion);
            r.lease = None;
            Ok(())
        });
        if result
            .as_ref()
            .is_err_and(|e| e.code == ErrorCode::StaleGeneration)
        {
            let db = self.db.lock().map_err(error)?;
            let deleted: bool = db
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM run_tombstones WHERE id=?)",
                    [id],
                    |r| r.get(0),
                )
                .map_err(error)?;
            if !deleted {
                db.execute(
                    "INSERT INTO runtime_late_receipts(run,lease,completion) VALUES (?,?,?)",
                    params![id, lease, serde_json::to_string(&receipt).map_err(error)?],
                )
                .map_err(error)?;
            }
        }
        result
    }
    fn late_receipts(&self, id: &str) -> Result<Vec<Value>> {
        let db = self.db.lock().map_err(error)?;
        let mut q = db
            .prepare(
                "SELECT lease,completion FROM runtime_late_receipts WHERE run=? ORDER BY sequence",
            )
            .map_err(error)?;
        let rows = q
            .query_map([id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(error)?;
        rows.into_iter().map(|(lease,body)|Ok(serde_json::json!({"lease":lease,"completion":serde_json::from_str::<Value>(&body).map_err(error)?}))).collect()
    }
    fn recover(&self, id: &str, mut completion: Completion) -> Result<RunRecord> {
        self.change(id, |r| {
            if !matches!(
                r.state,
                RunState::Running | RunState::Cancelling | RunState::Staged
            ) {
                return Err(conflict("Only incomplete runs can be recovered"));
            }
            if !completion.state.terminal() {
                return Err(conflict("Recovery requires terminal evidence"));
            }
            if r.cancel_requested {
                completion.state = RunState::Cancelled;
            }
            r.state = completion.state;
            r.completion = Some(completion);
            r.lease = None;
            Ok(())
        })
    }
    fn requeue(&self, id: &str) -> Result<RunRecord> {
        self.change(id, |r| {
            if r.state != RunState::Running || r.cancel_requested {
                return Err(conflict("Only a running run without a stop request can be requeued"));
            }
            r.state = RunState::Queued;
            r.lease = None;
            Ok(())
        })
    }
    fn cancel(&self, id: &str) -> Result<RunRecord> {
        self.change(id, |r| {
            if !r.state.terminal() {
                r.cancel_requested = true;
                if matches!(r.state, RunState::Staged | RunState::Queued) {
                    r.state = RunState::Cancelled;
                    r.completion = Some(Completion {
                        state: RunState::Cancelled,
                        result: Value::Null,
                    });
                } else {
                    r.state = RunState::Cancelling;
                }
            }
            Ok(())
        })
    }
    fn steer(&self, id: &str, message: &Steering) -> Result<()> {
        if message.id.is_empty()
            || message.id.len() > 128
            || message.body.trim().is_empty()
            || message.body.len() > 16384
        {
            return Err(conflict("Invalid steering message"));
        }
        let mut db = self.db.lock().map_err(error)?;
        let tx = db.transaction().map_err(error)?;
        let run = Self::row(&tx, id)?.ok_or_else(|| conflict("Unknown run"))?;
        if run.state.terminal() || run.cancel_requested {
            return Err(conflict("Run no longer accepts steering"));
        }
        let prior: Option<String> = tx
            .query_row(
                "SELECT body FROM steering WHERE run=? AND id=?",
                params![id, message.id],
                |r| r.get(0),
            )
            .optional()
            .map_err(error)?;
        if prior.is_some_and(|v| v != message.body) {
            return Err(conflict("Steering message ID reused"));
        }
        tx.execute(
            "INSERT OR IGNORE INTO steering(run,id,body) VALUES (?,?,?)",
            params![id, message.id, message.body],
        )
        .map_err(error)?;
        tx.commit().map_err(error)?;
        Ok(())
    }
    fn steering(&self, id: &str) -> Result<Vec<Steering>> {
        let mut db = self.db.lock().map_err(error)?;
        let tx = db.transaction().map_err(error)?;
        let mut q = tx
            .prepare("SELECT id,body FROM steering WHERE run=? AND delivered=0 ORDER BY rowid")
            .map_err(error)?;
        let rows = q
            .query_map([id], |r| {
                Ok(Steering {
                    id: r.get(0)?,
                    body: r.get(1)?,
                })
            })
            .map_err(error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(error)?;
        drop(q);
        if rows.is_empty() {
            return Ok(rows);
        }
        tx.execute(
            "UPDATE steering SET delivered=1 WHERE run=? AND delivered=0",
            [id],
        )
        .map_err(error)?;
        tx.commit().map_err(error)?;
        Ok(rows)
    }

    fn steering_history(&self, id: &str) -> Result<Vec<Steering>> {
        let db = self.db.lock().map_err(error)?;
        let mut q = db
            .prepare("SELECT id,body FROM steering WHERE run=? ORDER BY rowid")
            .map_err(error)?;
        let rows = q
            .query_map([id], |r| {
                Ok(Steering {
                    id: r.get(0)?,
                    body: r.get(1)?,
                })
            })
            .map_err(error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(error)?;
        Ok(rows)
    }
    fn erase_session(&self, session: &str, scopes: &[String]) -> Result<()> {
        let mut db = self.db.lock().map_err(error)?;
        let tx = db.transaction().map_err(error)?;
        let mut q = tx
            .prepare("SELECT body FROM runs WHERE session=?")
            .map_err(error)?;
        let rows = q
            .query_map([session], |r| r.get::<_, String>(0))
            .map_err(error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(error)?;
        drop(q);
        let records = rows
            .into_iter()
            .map(|s| serde_json::from_str::<RunRecord>(&s).map_err(error))
            .collect::<Result<Vec<_>>>()?;
        if records.iter().any(|r| !r.state.terminal()) {
            return Err(conflict(
                "Stop active execution before deleting its session",
            ));
        }
        let descendants_active: bool = tx.query_row(
            "WITH RECURSIVE descendants(id) AS (SELECT id FROM runs WHERE session=? UNION SELECT r.id FROM runs r JOIN descendants d ON json_extract(r.body,'$.spec.parent_run')=d.id) SELECT EXISTS(SELECT 1 FROM runs WHERE id IN (SELECT id FROM descendants) AND state IN ('staged','queued','running','cancelling'))",
            [session], |r| r.get(0)).map_err(error)?;
        if descendants_active {
            return Err(conflict(
                "Stop active descendants before deleting their ancestor session",
            ));
        }
        let mut scopes = scopes.to_vec();
        for record in records {
            if let Some(root)=record.spec.input["request_budget_id"].as_str(){
                let recovery=serde_json::json!([root,record.spec.session]).to_string();
                tx.execute("INSERT OR IGNORE INTO scope_tombstones VALUES (?)",[&recovery]).map_err(error)?;
                tx.execute("DELETE FROM runtime_request_recovery WHERE scope=?",[&recovery]).map_err(error)?;
            }
            tx.execute("DELETE FROM runtime_request_activity WHERE run=?",[&record.spec.id]).map_err(error)?;
            tx.execute(
                "INSERT OR IGNORE INTO run_tombstones VALUES (?,?)",
                params![record.spec.id, session],
            )
            .map_err(error)?;
            tx.execute("DELETE FROM steering WHERE run=?", [&record.spec.id])
                .map_err(error)?;
            tx.execute(
                "DELETE FROM runtime_late_receipts WHERE run=?",
                [&record.spec.id],
            )
            .map_err(error)?;
            tx.execute("DELETE FROM runtime_events WHERE run=?", [&record.spec.id])
                .map_err(error)?;
            scopes.push(record.spec.turn);
        }
        for scope in scopes {
            tx.execute(
                "INSERT OR IGNORE INTO scope_tombstones VALUES (?)",
                [&scope],
            )
            .map_err(error)?;
            tx.execute("DELETE FROM effects WHERE scope=?", [&scope])
                .map_err(error)?;
            tx.execute("DELETE FROM runtime_request_recovery WHERE scope=?",[&scope]).map_err(error)?;
        }
        tx.execute("DELETE FROM runtime_request_activity WHERE budget IN (SELECT id FROM runtime_request_budgets WHERE owner_session=?)",[session]).map_err(error)?;
        tx.execute("DELETE FROM runtime_request_budgets WHERE owner_session=?",[session]).map_err(error)?;
        tx.execute("DELETE FROM runtime_request_recovery WHERE json_valid(scope) AND json_extract(scope,'$[1]')=?",[session]).map_err(error)?;
        tx.execute("DELETE FROM runs WHERE session=?", [session])
            .map_err(error)?;
        tx.commit().map_err(error)?;
        Ok(())
    }
    fn publications(&self) -> Result<Vec<RunRecord>> {
        let db = self.db.lock().map_err(error)?;
        let mut q = db
            .prepare("SELECT body FROM runs WHERE published<revision ORDER BY sequence LIMIT 256")
            .map_err(error)?;
        let rows = q
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(error)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(error)?;
        rows.into_iter()
            .map(|s| serde_json::from_str(&s).map_err(error))
            .collect()
    }
    fn published(&self, id: &str, revision: u64) -> Result<()> {
        self.db
            .lock()
            .map_err(error)?
            .execute(
                "UPDATE runs SET published=max(published,?) WHERE id=?",
                params![revision, id],
            )
            .map_err(error)?;
        Ok(())
    }
    fn effect_get(&self, scope: &str, id: &str) -> Result<Option<Effect>> {
        let db = self.db.lock().map_err(error)?;
        let row: Option<(String, Option<String>)> = db
            .query_row(
                "SELECT digest,result FROM effects WHERE scope=? AND id=?",
                params![scope, id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(error)?;
        row.map(|(digest, result)| {
            Ok(Effect {
                scope: scope.into(),
                id: id.into(),
                digest,
                receipt: result
                    .map(|s| serde_json::from_str(&s).map_err(error))
                    .transpose()?,
            })
        })
        .transpose()
    }
    fn effect_begin(&self, scope: &str, id: &str, digest: &str) -> Result<Option<Value>> {
        let mut db = self.db.lock().map_err(error)?;
        let tx = db.transaction().map_err(error)?;
        let deleted: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM scope_tombstones WHERE scope=?)",
                [scope],
                |r| r.get(0),
            )
            .map_err(error)?;
        if deleted {
            return Err(conflict("Effect belongs to a deleted session"));
        }
        let prior: Option<(String, String, Option<String>)> = tx
            .query_row(
                "SELECT digest,state,result FROM effects WHERE scope=? AND id=?",
                params![scope, id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(error)?;
        if let Some((hash, state, result)) = prior {
            if hash != digest {
                return Err(conflict(
                    "Effect ID reused for different input or authority",
                ));
            }
            if state == "completed" {
                return result
                    .map(|s| serde_json::from_str(&s).map_err(error))
                    .transpose();
            }
            return Err(ServiceError::new(
                ErrorCode::Uncertain,
                "Earlier dispatch outcome is uncertain; inspect its receipt before retrying",
            ));
        }
        tx.execute(
            "INSERT INTO effects VALUES (?,?,?,'dispatched',NULL)",
            params![scope, id, digest],
        )
        .map_err(error)?;
        tx.commit().map_err(error)?;
        Ok(None)
    }
    fn effect_finish(&self, scope: &str, id: &str, digest: &str, result: Value) -> Result<()> {
        let rows=self.db.lock().map_err(error)?.execute("UPDATE effects SET state='completed',result=? WHERE scope=? AND id=? AND digest=? AND state='dispatched'",params![result.to_string(),scope,id,digest]).map_err(error)?;
        if rows != 1 {
            return Err(conflict("Effect receipt no longer matches its dispatch"));
        }
        Ok(())
    }
    fn effect_import(&self, effect: &Effect) -> Result<()> {
        let mut db = self.db.lock().map_err(error)?;
        let tx = db.transaction().map_err(error)?;
        let deleted: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM scope_tombstones WHERE scope=?)",
                [&effect.scope],
                |r| r.get(0),
            )
            .map_err(error)?;
        if deleted {
            return Ok(());
        }
        let prior: Option<(String, Option<String>)> = tx
            .query_row(
                "SELECT digest,result FROM effects WHERE scope=? AND id=?",
                params![effect.scope, effect.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(error)?;
        if let Some((digest, result)) = prior {
            if digest != effect.digest {
                return Err(conflict("Imported effect conflicts with runtime journal"));
            }
            let incoming = effect.receipt.as_ref().map(Value::to_string);
            if result.is_some() && incoming.is_some() && result != incoming {
                return Err(conflict(
                    "Imported effect has a conflicting confirmed receipt",
                ));
            }
            if result.is_none() && incoming.is_some() {
                tx.execute(
                    "UPDATE effects SET state='completed',result=? WHERE scope=? AND id=?",
                    params![incoming, effect.scope, effect.id],
                )
                .map_err(error)?;
            }
        } else {
            tx.execute(
                "INSERT INTO effects VALUES (?,?,?,?,?)",
                params![
                    effect.scope,
                    effect.id,
                    effect.digest,
                    if effect.receipt.is_some() {
                        "completed"
                    } else {
                        "dispatched"
                    },
                    effect.receipt.as_ref().map(Value::to_string)
                ],
            )
            .map_err(error)?;
        }
        tx.commit().map_err(error)?;
        Ok(())
    }
}
