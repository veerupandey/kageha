//! Durable accounting for direct standalone runtime requests.
use kageha_contracts::request_budget::{BudgetExceeded, BudgetHandle, BudgetPort};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io,
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc, Mutex, OnceLock, Weak,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
type Database = Arc<Mutex<Connection>>;
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
pub(crate) fn initialize(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS runtime_request_budgets(id TEXT PRIMARY KEY,owner_session TEXT NOT NULL,started_at_ms INTEGER NOT NULL,max_duration_ms INTEGER NOT NULL,active_ms INTEGER NOT NULL DEFAULT 0,max_model_calls INTEGER NOT NULL,used_model_calls INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE IF NOT EXISTS runtime_request_activity(budget TEXT NOT NULL,run TEXT NOT NULL,lease TEXT NOT NULL,process TEXT NOT NULL,paused INTEGER NOT NULL,heartbeat_ms INTEGER NOT NULL,PRIMARY KEY(budget,run,lease));
        CREATE TABLE IF NOT EXISTS runtime_request_recovery(scope TEXT PRIMARY KEY,state TEXT NOT NULL);")
}
static PROCESS: OnceLock<String> = OnceLock::new();
static CLOCKS: OnceLock<Mutex<HashMap<(usize, String), Weak<ActiveClock>>>> = OnceLock::new();
fn process_id() -> &'static str {
    PROCESS.get_or_init(|| uuid::Uuid::new_v4().to_string())
}
struct ClockState {
    active: HashMap<(String, String), usize>,
    last: Instant,
    accrued: i64,
    max_ms: i64,
}
struct ActiveClock {
    db: Database,
    id: String,
    state: Mutex<ClockState>,
    expiry: AtomicI64,
}
impl ActiveClock {
    fn settle(&self, state: &mut ClockState) -> io::Result<()> {
        let now = Instant::now();
        if state.active.values().any(|paused| *paused == 0) {
            state.accrued = state
                .accrued
                .saturating_add(
                    now.duration_since(state.last)
                        .as_millis()
                        .min(i64::MAX as u128) as i64,
                )
                .min(state.max_ms);
        }
        state.last = now;
        let db = self
            .db
            .lock()
            .map_err(|_| io::Error::other("Active budget unavailable"))?;
        db.execute(
            "UPDATE runtime_request_budgets SET active_ms=? WHERE id=?",
            params![state.accrued, self.id],
        )
        .map_err(io::Error::other)?;
        db.execute(
            "UPDATE runtime_request_activity SET heartbeat_ms=? WHERE budget=? AND process=?",
            params![now_ms(), self.id, process_id()],
        )
        .map_err(io::Error::other)?;
        self.expiry.store(
            if state.accrued >= state.max_ms {
                0
            } else if state.active.values().any(|paused| *paused == 0) {
                now_ms().saturating_add(state.max_ms.saturating_sub(state.accrued))
            } else {
                i64::MAX
            },
            Ordering::Release,
        );
        Ok(())
    }
    fn change(&self, key: &(String, String), operation: i8) -> io::Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("Active budget unavailable"))?;
        self.settle(&mut state)?;
        match operation {
            0 => {
                state.active.insert(key.clone(), 0);
            }
            1 => {
                if let Some(paused) = state.active.get_mut(key) {
                    *paused += 1;
                }
            }
            -1 => {
                if let Some(paused) = state.active.get_mut(key) {
                    *paused = paused.saturating_sub(1);
                }
            }
            _ => {
                state.active.remove(key);
            }
        }
        let db = self
            .db
            .lock()
            .map_err(|_| io::Error::other("Active budget unavailable"))?;
        if let Some(paused) = state.active.get(key) {
            db.execute("INSERT INTO runtime_request_activity VALUES (?,?,?,?,?,?) ON CONFLICT(budget,run,lease) DO UPDATE SET paused=excluded.paused,heartbeat_ms=excluded.heartbeat_ms",params![self.id,key.0,key.1,process_id(),*paused as i64,now_ms()]).map_err(io::Error::other)?;
        } else {
            db.execute(
                "DELETE FROM runtime_request_activity WHERE budget=? AND run=? AND lease=?",
                params![self.id, key.0, key.1],
            )
            .map_err(io::Error::other)?;
        }
        drop(db);
        self.expiry.store(
            if state.accrued >= state.max_ms {
                0
            } else if state.active.values().any(|paused| *paused == 0) {
                now_ms().saturating_add(state.max_ms - state.accrued)
            } else {
                i64::MAX
            },
            Ordering::Release,
        );
        Ok(())
    }
}
fn clock(db: &Database, id: &str) -> io::Result<Arc<ActiveClock>> {
    let mut clocks = CLOCKS
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| io::Error::other("Active clock registry unavailable"))?;
    let key = (Arc::as_ptr(db) as usize, id.to_owned());
    if let Some(clock) = clocks.get(&key).and_then(Weak::upgrade) {
        return Ok(clock);
    };
    let (accrued, max_ms) = {
        let connection = db
            .lock()
            .map_err(|_| io::Error::other("Active budget unavailable"))?;
        let row: Option<(i64, i64)> = connection
            .query_row(
                "SELECT active_ms,max_duration_ms FROM runtime_request_budgets WHERE id=?",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(io::Error::other)?;
        // Persisted accrual includes all work through the last heartbeat. No process downtime is charged.
        connection
            .execute("DELETE FROM runtime_request_activity WHERE budget=?", [id])
            .map_err(io::Error::other)?;
        row.ok_or_else(|| io::Error::other("Pinned request budget is missing"))?
    };
    let clock = Arc::new(ActiveClock {
        db: db.clone(),
        id: id.into(),
        state: Mutex::new(ClockState {
            active: HashMap::new(),
            last: Instant::now(),
            accrued,
            max_ms,
        }),
        expiry: AtomicI64::new(if accrued >= max_ms { 0 } else { i64::MAX }),
    });
    clocks.insert(key, Arc::downgrade(&clock));
    let weak = Arc::downgrade(&clock);
    std::thread::Builder::new()
        .name("request-active-clock".into())
        .spawn(move || loop {
            std::thread::sleep(Duration::from_millis(250));
            let Some(clock) = weak.upgrade() else { break };
            if let Ok(mut state) = clock.state.lock() {
                if clock.settle(&mut state).is_err() {
                    clock.expiry.store(0, Ordering::Release);
                }
            };
        })
        .map_err(io::Error::other)?;
    Ok(clock)
}
struct DurableBudget {
    clock: Arc<ActiveClock>,
    lease: Option<(String, String)>,
}
impl Drop for DurableBudget {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}
impl BudgetPort for DurableBudget {
    fn finish(&self) -> io::Result<()> {
        if let Some(key) = &self.lease {
            self.clock.change(key, 2)?;
        }
        Ok(())
    }

    fn check(&self) -> io::Result<()> {
        if now_ms() >= self.clock.expiry.load(Ordering::Acquire) {
            return Err(io::Error::other(BudgetExceeded::new(
                "Request reached its active execution time limit",
            )));
        };
        Ok(())
    }
    fn suspend(&self) -> io::Result<()> {
        if let Some(key) = &self.lease {
            self.clock.change(key, 1)?;
        }
        Ok(())
    }
    fn resume(&self) -> io::Result<()> {
        if let Some(key) = &self.lease {
            self.clock.change(key, -1)?;
        }
        Ok(())
    }
    fn before_model_invocation(&self) -> io::Result<()> {
        self.check()?;
        {
            let mut state = self
                .clock
                .state
                .lock()
                .map_err(|_| io::Error::other("Active budget unavailable"))?;
            self.clock.settle(&mut state)?;
        }
        let db = self
            .clock
            .db
            .lock()
            .map_err(|_| io::Error::other("Request budget unavailable"))?;
        if let Some((run, lease)) = &self.lease {
            let current:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE id=? AND state='running' AND json_extract(body,'$.lease')=? AND json_extract(body,'$.cancel_requested')=0)",params![run,lease],|r|r.get(0)).map_err(io::Error::other)?;
            if !current {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "Run budget ownership ended before model dispatch",
                ));
            };
        }
        let updated=db.execute("UPDATE runtime_request_budgets SET used_model_calls=used_model_calls+1 WHERE id=? AND used_model_calls<max_model_calls AND active_ms<max_duration_ms",[&self.clock.id]).map_err(io::Error::other)?;
        if updated == 1 {
            return Ok(());
        };
        Err(io::Error::other(BudgetExceeded::new(
            "Request reached its aggregate model-call or active execution limit",
        )))
    }
}
pub(crate) fn begin(
    db: Database,
    run: &kageha_runtime::lifecycle::RunRecord,
) -> io::Result<Option<BudgetHandle>> {
    if run.spec.input["persistence"].is_null() {
        return Ok(None);
    };
    let policy: kageha_contracts::execution::PersistencePolicy =
        serde_json::from_value(run.spec.input["persistence"].clone()).map_err(io::Error::other)?;
    policy.validate()?;
    if !policy.enabled {
        return Ok(None);
    };
    let id = run.spec.input["request_budget_id"]
        .as_str()
        .ok_or_else(|| io::Error::other("Pinned request budget root is missing"))?;
    {
        let connection = db
            .lock()
            .map_err(|_| io::Error::other("Runtime request budget unavailable"))?;
        let root: Option<String> = connection
            .query_row("SELECT body FROM runs WHERE id=?", [id], |r| r.get(0))
            .optional()
            .map_err(io::Error::other)?;
        let root: kageha_runtime::lifecycle::RunRecord = serde_json::from_str(
            &root.ok_or_else(|| io::Error::other("Pinned budget root run is missing"))?,
        )
        .map_err(io::Error::other)?;
        if root.spec.input["persistence"] != run.spec.input["persistence"] {
            return Err(io::Error::other(
                "Request budget policy differs from its root",
            ));
        };
        let owner: Option<String> = connection
            .query_row("SELECT body FROM runs WHERE id=?", [&run.spec.id], |r| {
                r.get(0)
            })
            .optional()
            .map_err(io::Error::other)?;
        let owner: kageha_runtime::lifecycle::RunRecord = serde_json::from_str(
            &owner.ok_or_else(|| io::Error::other("Budget run ownership is missing"))?,
        )
        .map_err(io::Error::other)?;
        if owner.lease != run.lease || owner.state != kageha_runtime::lifecycle::RunState::Running {
            return Err(io::Error::other("Budget run lease is no longer current"));
        };
        connection.execute("INSERT OR IGNORE INTO runtime_request_budgets(id,owner_session,started_at_ms,max_duration_ms,max_model_calls) VALUES (?,?,?,?,?)",params![id,root.spec.session,now_ms(),policy.max_duration_seconds as i64*1000,policy.max_model_calls]).map_err(io::Error::other)?;
    }
    let clock = clock(&db, id)?;
    let key = (
        run.spec.id.clone(),
        run.lease
            .clone()
            .ok_or_else(|| io::Error::other("Budget run has no owned lease"))?,
    );
    clock.change(&key, 0)?;
    Ok(Some(Arc::new(DurableBudget {
        clock,
        lease: Some(key),
    })))
}
pub(crate) fn snapshot(db: &Connection, id: &str) -> io::Result<Value> {
    let row:Option<(i64,i64,i64,u32,u32)>=db.query_row("SELECT started_at_ms,max_duration_ms,active_ms,max_model_calls,used_model_calls FROM runtime_request_budgets WHERE id=?",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional().map_err(io::Error::other)?;
    let (started, max_ms, active, max, used) =
        row.ok_or_else(|| io::Error::other("Pinned request budget is missing"))?;
    Ok(
        json!({"id":id,"started_at_ms":started,"max_duration_ms":max_ms,"active_duration_ms":active,"max_model_calls":max,"used_model_calls":used,"remaining_model_calls":max.saturating_sub(used),"remaining_duration_ms":max_ms.saturating_sub(active).max(0)}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SqliteRunStore;
    use kageha_runtime::lifecycle::*;
    fn spec(id: &str, depth: u32) -> RunSpec {
        RunSpec {
            id: id.into(),
            session: format!("session-{id}"),
            turn: format!("turn-{id}"),
            request: id.into(),
            parent: None,
            parent_run: None,
            agent: "test".into(),
            objective: "work".into(),
            depth,
            input: json!({"persistence":{"enabled":true,"max_model_calls":3,"max_duration_seconds":7200},"request_budget_id":"root"}),
        }
    }
    fn claim(store: &SqliteRunStore, spec: &RunSpec) -> RunRecord {
        store.stage(spec, 16).unwrap();
        store.activate(&spec.id).unwrap();
        store.claim(spec.depth).unwrap().unwrap()
    }
    fn advance(clock: &ActiveClock, ms: u64) {
        let mut state = clock.state.lock().unwrap();
        state.last = Instant::now() - Duration::from_millis(ms);
        clock.settle(&mut state).unwrap();
    }
    #[test]
    fn direct_workers_share_calls_and_union_active_time_and_waits_pause() {
        let directory = tempfile::tempdir().unwrap();
        let store = SqliteRunStore::open(&directory.path().join("runtime.sqlite")).unwrap();
        let root = claim(&store, &spec("root", 0));
        let mut child = spec("worker", 1);
        child.parent = Some(root.spec.session.clone());
        child.parent_run = Some("root".into());
        let worker = claim(&store, &child);
        let main = store.request_budget_begin(&root).unwrap().unwrap();
        let worker_budget = store.request_budget_begin(&worker).unwrap().unwrap();
        let clock = clock(&store.db, "root").unwrap();
        advance(&clock, 100);
        assert!((100..150).contains(
            &store.request_budget_snapshot("root").unwrap()["active_duration_ms"]
                .as_i64()
                .unwrap()
        ));
        main.suspend().unwrap();
        worker_budget.suspend().unwrap();
        let paused = store.request_budget_snapshot("root").unwrap()["active_duration_ms"].clone();
        advance(&clock, 20_000);
        assert_eq!(
            store.request_budget_snapshot("root").unwrap()["active_duration_ms"],
            paused
        );
        main.resume().unwrap();
        main.before_model_invocation().unwrap();
        worker_budget.resume().unwrap();
        worker_budget.before_model_invocation().unwrap();
        main.before_model_invocation().unwrap();
        assert!(worker_budget.before_model_invocation().is_err());
        assert_eq!(
            store.request_budget_snapshot("root").unwrap()["used_model_calls"],
            3
        );
        main.finish().unwrap();
        worker_budget.finish().unwrap();
        let count: i64 = store
            .db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM runtime_request_activity", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
    }
    #[test]
    fn reopen_preserves_accrued_usage_and_recovery_and_rejects_stale_lease() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("runtime.sqlite");
        let scope = json!(["root", "session-root"]).to_string();
        {
            let store = SqliteRunStore::open(&path).unwrap();
            let run = claim(&store, &spec("root", 0));
            let budget = store.request_budget_begin(&run).unwrap().unwrap();
            budget.before_model_invocation().unwrap();
            advance(&clock(&store.db, "root").unwrap(), 1200);
            budget.finish().unwrap();
            store
                .save_recovery_checkpoint(&scope, &json!({"continuations":2}))
                .unwrap();
            store
                .finish(
                    "root",
                    run.lease.as_deref().unwrap(),
                    Completion {
                        state: RunState::Succeeded,
                        result: Value::Null,
                    },
                )
                .unwrap();
            assert!(store.request_budget_begin(&run).is_err());
        }
        let store = SqliteRunStore::open(&path).unwrap();
        assert_eq!(
            store.request_budget_snapshot("root").unwrap()["used_model_calls"],
            1
        );
        assert!((1200..1400).contains(
            &store.request_budget_snapshot("root").unwrap()["active_duration_ms"]
                .as_i64()
                .unwrap()
        ));
        assert_eq!(
            store.recovery_checkpoint(&scope).unwrap()["continuations"],
            2
        );
        store.erase_session("session-root", &[]).unwrap();
        assert!(store.request_budget_snapshot("root").is_err());
        assert!(store.recovery_checkpoint(&scope).unwrap().is_null());
        assert!(store
            .save_recovery_checkpoint(&scope, &json!({"revive":true}))
            .is_err());
    }
    #[test]
    fn legacy_inflight_run_remains_unbudgeted() {
        let directory = tempfile::tempdir().unwrap();
        let store = SqliteRunStore::open(&directory.path().join("runtime.sqlite")).unwrap();
        let mut legacy = spec("legacy", 0);
        legacy.input = json!({});
        let run = claim(&store, &legacy);
        assert!(store.request_budget_begin(&run).unwrap().is_none());
    }
}
