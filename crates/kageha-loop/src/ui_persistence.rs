//! Pinned request-wide limits. Reservations are committed before provider dispatch.
use crate::{ui_store, webui::Database};
use kageha_contracts::execution::{ModelUsage, PersistencePolicy};
use kageha_contracts::request_budget::{BudgetExceeded, BudgetHandle, BudgetPort};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Policy {
    pub enabled: bool,
    pub max_duration_seconds: u64,
    pub max_model_calls: u32,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            enabled: true,
            max_duration_seconds: PersistencePolicy::DEFAULT_DURATION_SECONDS,
            max_model_calls: PersistencePolicy::DEFAULT_MODEL_CALLS,
        }
    }
}
impl Policy {
    pub(crate) fn parse(value: &Value) -> io::Result<Self> {
        let mut value = value.clone();
        if let Some(object) = value.as_object_mut() {
            object.remove("revision");
        }
        let policy: Self = serde_json::from_value(value).map_err(io::Error::other)?;
        if !(1..=PersistencePolicy::MAX_DURATION_SECONDS).contains(&policy.max_duration_seconds)
            || !(1..=PersistencePolicy::MAX_MODEL_CALLS).contains(&policy.max_model_calls)
        {
            return Err(io::Error::other(
                "Persistence must allow 1–604800 seconds and 1–100000 actual model calls",
            ));
        }
        Ok(policy)
    }
}
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
pub(crate) fn initialize(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS ui_request_budgets(
        id TEXT PRIMARY KEY, started_at_ms INTEGER NOT NULL, deadline_ms INTEGER NOT NULL,
        max_model_calls INTEGER NOT NULL, used_model_calls INTEGER NOT NULL DEFAULT 0,
        max_duration_ms INTEGER NOT NULL DEFAULT 0,active_ms INTEGER NOT NULL DEFAULT 0,
        cached_tokens INTEGER NOT NULL DEFAULT 0,last_call_cached_tokens INTEGER,
        reported_input_tokens INTEGER NOT NULL DEFAULT 0,reported_output_tokens INTEGER NOT NULL DEFAULT 0,
        input_usage_calls INTEGER NOT NULL DEFAULT 0,output_usage_calls INTEGER NOT NULL DEFAULT 0,
        last_call_input_tokens INTEGER,last_call_output_tokens INTEGER);
        CREATE TABLE IF NOT EXISTS ui_request_recovery(budget TEXT NOT NULL,agent TEXT NOT NULL,state TEXT NOT NULL,PRIMARY KEY(budget,agent));
        CREATE TABLE IF NOT EXISTS ui_request_activity(budget TEXT NOT NULL,run TEXT NOT NULL,lease TEXT NOT NULL,process TEXT NOT NULL,paused INTEGER NOT NULL,heartbeat_ms INTEGER NOT NULL,PRIMARY KEY(budget,run,lease));")?;
    let columns: Vec<String> = db
        .prepare("PRAGMA table_info(ui_request_budgets)")?
        .query_map([], |r| r.get(1))?
        .collect::<Result<_, _>>()?;
    if !columns.iter().any(|name| name == "max_duration_ms") {
        db.execute_batch("ALTER TABLE ui_request_budgets ADD COLUMN max_duration_ms INTEGER NOT NULL DEFAULT 0;
            ALTER TABLE ui_request_budgets ADD COLUMN active_ms INTEGER NOT NULL DEFAULT 0;
            UPDATE ui_request_budgets SET max_duration_ms=MAX(0,deadline_ms-started_at_ms),active_ms=MIN(MAX(0,deadline_ms-started_at_ms),MAX(0,CAST(strftime('%s','now') AS INTEGER)*1000-started_at_ms));")?;
    }
    if !columns.iter().any(|name| name == "cached_tokens") {
        db.execute_batch("ALTER TABLE ui_request_budgets ADD COLUMN cached_tokens INTEGER NOT NULL DEFAULT 0;
            ALTER TABLE ui_request_budgets ADD COLUMN last_call_cached_tokens INTEGER;")?;
    }
    // Upgrade columns independently so an interrupted migration can be resumed.
    for (name, definition) in [
        ("reported_input_tokens", "INTEGER NOT NULL DEFAULT 0"),
        ("reported_output_tokens", "INTEGER NOT NULL DEFAULT 0"),
        ("input_usage_calls", "INTEGER NOT NULL DEFAULT 0"),
        ("output_usage_calls", "INTEGER NOT NULL DEFAULT 0"),
        ("last_call_input_tokens", "INTEGER"),
        ("last_call_output_tokens", "INTEGER"),
    ] {
        if !columns.iter().any(|column| column == name) {
            db.execute_batch(&format!("ALTER TABLE ui_request_budgets ADD COLUMN {name} {definition}"))?;
        }
    }
    Ok(())
}
pub(crate) fn settings(db: &Connection) -> io::Result<Policy> {
    ui_store::get(db, "setting", "persistence")
        .map_err(io::Error::other)?
        .map(|value| Policy::parse(&value))
        .unwrap_or_else(|| Ok(Policy::default()))
}
pub(crate) fn configuration_for_turn(db: &Database, turn: &str) -> io::Result<Option<Value>> {
    let db = db
        .lock()
        .map_err(|_| io::Error::other("Pinned request configuration unavailable"))?;
    let text: Option<String> = db
        .query_row(
            "SELECT spec FROM ui_runtime_runs WHERE json_extract(spec,'$.turn')=?",
            [turn],
            |r| r.get(0),
        )
        .optional()
        .map_err(io::Error::other)?;
    text.map(|text| {
        serde_json::from_str::<Value>(&text)
            .map(|run| run["input"]["configuration"].clone())
            .map_err(io::Error::other)
    })
    .transpose()
}
/// Only called during a new request's admission transaction, never on recovery.
pub(crate) fn admit(db: &Connection, id: &str, configuration: &mut Value) -> io::Result<()> {
    let policy = settings(db)?;
    configuration["persistence"] = json!(policy);
    if !policy.enabled {
        return Ok(());
    }
    let now = now_ms();
    db.execute("INSERT INTO ui_request_budgets(id,started_at_ms,deadline_ms,max_model_calls,max_duration_ms) VALUES (?,?,?,?,?)",
        params![id,now,now+policy.max_duration_seconds as i64*1000,policy.max_model_calls,policy.max_duration_seconds as i64*1000]).map_err(io::Error::other)?;
    configuration["request_budget_id"] = json!(id);
    Ok(())
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
            "UPDATE ui_request_budgets SET active_ms=? WHERE id=?",
            params![state.accrued, self.id],
        )
        .map_err(io::Error::other)?;
        db.execute(
            "UPDATE ui_request_activity SET heartbeat_ms=? WHERE budget=? AND process=?",
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
            db.execute("INSERT INTO ui_request_activity VALUES (?,?,?,?,?,?) ON CONFLICT(budget,run,lease) DO UPDATE SET paused=excluded.paused,heartbeat_ms=excluded.heartbeat_ms",params![self.id,key.0,key.1,process_id(),*paused as i64,now_ms()]).map_err(io::Error::other)?;
        } else {
            db.execute(
                "DELETE FROM ui_request_activity WHERE budget=? AND run=? AND lease=?",
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
                "SELECT active_ms,max_duration_ms FROM ui_request_budgets WHERE id=?",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(io::Error::other)?;
        // Persisted accrual includes all work through the last heartbeat. No process downtime is charged.
        connection
            .execute("DELETE FROM ui_request_activity WHERE budget=?", [id])
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
impl BudgetPort for DurableBudget {
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
        self.reserve(0)
    }
    fn before_optional_invocation(&self) -> io::Result<()> {
        self.reserve(2)
    }
    /// Records only provider-reported usage. Counts of reported calls distinguish
    /// missing usage from zero usage; failed calls and older rows may lack reports.
    fn record_usage(&self, usage: &ModelUsage) -> io::Result<()> {
        let cached = usage.cached_tokens.map(|tokens| tokens.min(i64::MAX as u64) as i64);
        let input = usage.input_tokens.map(|tokens| tokens.min(i64::MAX as u64) as i64);
        let output = usage.output_tokens.map(|tokens| tokens.min(i64::MAX as u64) as i64);
        self.clock
            .db
            .lock()
            .map_err(|_| io::Error::other("Request budget unavailable"))?
            .execute(
                "UPDATE ui_request_budgets SET
                    last_call_cached_tokens=?1,cached_tokens=CASE WHEN cached_tokens>9223372036854775807-COALESCE(?1,0) THEN 9223372036854775807 ELSE cached_tokens+COALESCE(?1,0) END,
                    last_call_input_tokens=?2,last_call_output_tokens=?3,
                    reported_input_tokens=CASE WHEN reported_input_tokens>9223372036854775807-COALESCE(?2,0) THEN 9223372036854775807 ELSE reported_input_tokens+COALESCE(?2,0) END,
                    reported_output_tokens=CASE WHEN reported_output_tokens>9223372036854775807-COALESCE(?3,0) THEN 9223372036854775807 ELSE reported_output_tokens+COALESCE(?3,0) END,
                    input_usage_calls=input_usage_calls+CASE WHEN ?2 IS NULL THEN 0 ELSE 1 END,
                    output_usage_calls=output_usage_calls+CASE WHEN ?3 IS NULL THEN 0 ELSE 1 END WHERE id=?4",
                params![cached, input, output, self.clock.id],
            )
            .map_err(io::Error::other)?;
        self.publish_aggregate();
        Ok(())
    }
}
impl DurableBudget {
    fn reserve(&self, leave: u32) -> io::Result<()> {
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
        let updated=db.execute("UPDATE ui_request_budgets SET used_model_calls=used_model_calls+1 WHERE id=?1 AND used_model_calls+?2<max_model_calls AND active_ms<max_duration_ms",params![self.clock.id, leave]).map_err(io::Error::other)?;
        if updated == 1 {
            drop(db);
            self.publish_aggregate();
            return Ok(());
        };
        Err(io::Error::other(BudgetExceeded::new(
            "Request reached its aggregate model-call or active execution limit",
        )))
    }

    /// Publish aggregate usage to the originating chat, including calls made by workers.
    fn publish_aggregate(&self) {
        let target=self.clock.db.lock().ok().and_then(|db|db.query_row(
            "SELECT json_extract(spec,'$.session'),json_extract(spec,'$.turn') FROM ui_runtime_runs WHERE id=?",
            [&self.clock.id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))
        ).optional().ok().flatten());
        if let Some((chat, turn)) = target {
            let _ = publish(
                &self.clock.db,
                &json!({"request_budget_id":self.clock.id}),
                &chat,
                &turn,
            );
        }
    }
}
#[cfg(test)]
pub(crate) fn handle(db: &Database, configuration: &Value) -> io::Result<Option<BudgetHandle>> {
    let Some(id) = configuration["request_budget_id"].as_str() else {
        return Ok(None);
    };
    Ok(Some(Arc::new(DurableBudget {
        clock: clock(db, id)?,
        lease: None,
    })))
}
pub(crate) struct ActivityLease {
    clock: Arc<ActiveClock>,
    key: (String, String),
}
impl Drop for ActivityLease {
    fn drop(&mut self) {
        if self.clock.change(&self.key, 2).is_err() {
            self.clock.expiry.store(0, Ordering::Release);
        }
    }
}
pub(crate) fn begin(
    db: &Database,
    configuration: &Value,
    run: &str,
    lease: &str,
) -> io::Result<(Option<BudgetHandle>, Option<ActivityLease>)> {
    let Some(id) = configuration["request_budget_id"].as_str() else {
        return Ok((None, None));
    };
    let clock = clock(db, id)?;
    let key = (run.to_owned(), lease.to_owned());
    clock.change(&key, 0)?;
    Ok((
        Some(Arc::new(DurableBudget {
            clock: clock.clone(),
            lease: Some(key.clone()),
        })),
        Some(ActivityLease { clock, key }),
    ))
}
pub(crate) fn snapshot(db: &Connection, id: &str) -> io::Result<Value> {
    let row = db.query_row("SELECT started_at_ms,max_duration_ms,active_ms,max_model_calls,used_model_calls,cached_tokens,last_call_cached_tokens,reported_input_tokens,reported_output_tokens,input_usage_calls,output_usage_calls,last_call_input_tokens,last_call_output_tokens FROM ui_request_budgets WHERE id=?",[id],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?,r.get::<_,u32>(3)?,r.get::<_,u32>(4)?,r.get::<_,i64>(5)?,r.get::<_,Option<i64>>(6)?,r.get::<_,i64>(7)?,r.get::<_,i64>(8)?,r.get::<_,u32>(9)?,r.get::<_,u32>(10)?,r.get::<_,Option<i64>>(11)?,r.get::<_,Option<i64>>(12)?))).optional().map_err(io::Error::other)?;
    let (started, max_ms, active, max, used, cached, last_cached, input, output, input_calls, output_calls, last_input, last_output) =
        row.ok_or_else(|| io::Error::other("Pinned request budget is missing"))?;
    Ok(
        json!({"id":id,"started_at_ms":started,"max_duration_ms":max_ms,"active_duration_ms":active,"max_model_calls":max,"used_model_calls":used,"remaining_model_calls":max.saturating_sub(used),"remaining_duration_ms":max_ms.saturating_sub(active).max(0),"cached_tokens":cached,"last_call_cached_tokens":last_cached,
            "reported_input_tokens":input,"reported_output_tokens":output,"input_usage_calls":input_calls,"output_usage_calls":output_calls,"last_call_input_tokens":last_input,"last_call_output_tokens":last_output}),
    )
}
#[cfg(test)]
pub(crate) fn available(db: &Connection, configuration: &Value) -> io::Result<bool> {
    let Some(id) = configuration["request_budget_id"].as_str() else {
        return Ok(false);
    };
    let state = snapshot(db, id)?;
    Ok(state["remaining_model_calls"].as_u64().unwrap_or(0) > 0
        && state["remaining_duration_ms"].as_u64().unwrap_or(0) > 0)
}

pub(crate) fn erase_chat(db:&Connection, chat:&str)->rusqlite::Result<()> {
    let roots={
        let mut query=db.prepare("SELECT DISTINCT json_extract(spec,'$.input.configuration.request_budget_id') FROM ui_runtime_runs WHERE json_extract(spec,'$.session')=? AND json_extract(spec,'$.parent_run') IS NULL AND json_extract(spec,'$.input.configuration.request_budget_id')=json_extract(spec,'$.turn')")?;
        let rows=query.query_map([chat],|row|row.get::<_,String>(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for root in roots {
        for table in ["ui_request_activity","ui_request_recovery"] {
            db.execute(&format!("DELETE FROM {table} WHERE budget=?"),[&root])?;
        }
        db.execute("DELETE FROM ui_request_budgets WHERE id=?",[root])?;
    }
    for table in ["ui_documents","ui_document_history"] {
        db.execute(&format!("DELETE FROM {table} WHERE kind='task-completion' AND substr(id,1,length(?1)+1)=?1||'/'"),[chat])?;
    }
    Ok(())
}
pub(crate) fn publish(
    db: &Database,
    configuration: &Value,
    chat: &str,
    turn: &str,
) -> io::Result<()> {
    let Some(id) = configuration["request_budget_id"].as_str() else {
        return Ok(());
    };
    let state = {
        let db = db
            .lock()
            .map_err(|_| io::Error::other("Request budget unavailable"))?;
        snapshot(&db, id)?
    };
    crate::webui::emit(
        db,
        chat,
        turn,
        "event",
        json!({"kind":"request_budget","payload":state}),
    )?;
    let db = db
        .lock()
        .map_err(|_| io::Error::other("Request budget unavailable"))?;
    db.execute("UPDATE ui_runtime_results SET result=json_set(result,'$.request_budget',json(?)) WHERE turn=?",params![state.to_string(),turn]).map_err(io::Error::other)?;
    Ok(())
}
pub(crate) fn policy(
    db: &Connection,
    configuration: &Value,
) -> io::Result<Option<kageha_contracts::execution::PersistencePolicy>> {
    if configuration["persistence"].is_null() {
        return Ok(None);
    };
    let policy = Policy::parse(&configuration["persistence"])?;
    if !policy.enabled {
        return Ok(None);
    };
    let id = configuration["request_budget_id"]
        .as_str()
        .ok_or_else(|| io::Error::other("Pinned request budget is missing"))?;
    let state = snapshot(db, id)?;
    let remaining = state["remaining_duration_ms"].as_u64().unwrap_or(0);
    let calls: u32 = serde_json::from_value(state["remaining_model_calls"].clone())?;
    if remaining < 1000 || calls == 0 {
        return Err(io::Error::other(BudgetExceeded::new(
            "Request persistence limit reached",
        )));
    };
    Ok(Some(kageha_contracts::execution::PersistencePolicy {
        enabled: true,
        max_model_calls: policy.max_model_calls.min(calls),
        max_duration_seconds: policy.max_duration_seconds.min(remaining / 1000),
    }))
}
pub(crate) fn recovery(db: &Connection, configuration: &Value, agent: &str) -> io::Result<Value> {
    let Some(id) = configuration["request_budget_id"].as_str() else {
        return Ok(Value::Null);
    };
    let text: Option<String> = db
        .query_row(
            "SELECT state FROM ui_request_recovery WHERE budget=? AND agent=?",
            params![id, agent],
            |r| r.get(0),
        )
        .optional()
        .map_err(io::Error::other)?;
    text.map(|text| serde_json::from_str(&text).map_err(io::Error::other))
        .unwrap_or(Ok(Value::Null))
}
pub(crate) fn save_recovery(
    db: &Connection,
    configuration: &Value,
    agent: &str,
    state: &Value,
) -> io::Result<()> {
    let Some(id) = configuration["request_budget_id"].as_str() else {
        return Ok(());
    };
    db.execute("INSERT INTO ui_request_recovery VALUES (?,?,?) ON CONFLICT(budget,agent) DO UPDATE SET state=excluded.state",params![id,agent,state.to_string()]).map_err(io::Error::other)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn active_fixture() -> (Database, Value) {
        let connection = Connection::open_in_memory().unwrap();
        ui_store::initialize(&connection).unwrap();
        initialize(&connection).unwrap();
        let mut config = json!({});
        admit(&connection, "root", &mut config).unwrap();
        (
            crate::agent_host::DatabaseState::open(connection).unwrap(),
            config,
        )
    }
    fn advance(clock: &ActiveClock, milliseconds: u64) {
        let mut state = clock.state.lock().unwrap();
        state.last = Instant::now() - Duration::from_millis(milliseconds);
        clock.settle(&mut state).unwrap();
    }
    #[test]
    fn active_clock_excludes_waiting_and_counts_parallel_workers_once() {
        let (db, config) = active_fixture();
        let (main, main_lease) = begin(&db, &config, "main", "lease-main").unwrap();
        let (worker, worker_lease) = begin(&db, &config, "worker", "lease-worker").unwrap();
        let clock = clock(&db, "root").unwrap();
        advance(&clock, 100);
        let accrued = snapshot(&db.lock().unwrap(), "root").unwrap()["active_duration_ms"]
            .as_i64()
            .unwrap();
        assert!(
            (100..150).contains(&accrued),
            "parallel workers must not multiply active time: {accrued}"
        );
        main.as_ref().unwrap().suspend().unwrap();
        worker.as_ref().unwrap().suspend().unwrap();
        let paused = snapshot(&db.lock().unwrap(), "root").unwrap()["active_duration_ms"]
            .as_i64()
            .unwrap();
        advance(&clock, 10_000);
        assert_eq!(
            snapshot(&db.lock().unwrap(), "root").unwrap()["active_duration_ms"],
            paused
        );
        main.as_ref().unwrap().check().unwrap();
        worker.as_ref().unwrap().resume().unwrap();
        advance(&clock, 100);
        assert!(
            snapshot(&db.lock().unwrap(), "root").unwrap()["active_duration_ms"]
                .as_i64()
                .unwrap()
                >= paused + 100
        );
        drop(main_lease);
        drop(worker_lease);
        let leases: i64 = db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM ui_request_activity", [], |r| r.get(0))
            .unwrap();
        assert_eq!(leases, 0);
    }
    #[test]
    fn cancellation_and_panics_release_active_leases() {
        let (db, config) = active_fixture();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let (_budget, _lease) = begin(&db, &config, "cancelled", "lease").unwrap();
            panic!("simulated executor cancellation");
        }));
        assert!(result.is_err());
        let count: i64 = db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM ui_request_activity", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
        let state = snapshot(&db.lock().unwrap(), "root").unwrap();
        let remaining = state["remaining_duration_ms"].clone();
        let clock = clock(&db, "root").unwrap();
        advance(&clock, 10_000);
        assert_eq!(
            snapshot(&db.lock().unwrap(), "root").unwrap()["remaining_duration_ms"],
            remaining
        );
    }
    #[test]
    fn reopening_uses_last_accrual_and_reconciles_stale_activity_without_downtime() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("active.sqlite");
        let config = {
            let connection = Connection::open(&path).unwrap();
            ui_store::initialize(&connection).unwrap();
            initialize(&connection).unwrap();
            let mut config = json!({});
            admit(&connection, "root", &mut config).unwrap();
            connection
                .execute(
                    "UPDATE ui_request_budgets SET active_ms=1500,used_model_calls=4",
                    [],
                )
                .unwrap();
            connection.execute("INSERT INTO ui_request_activity VALUES ('root','old-run','old-lease','old-process',0,1)",[]).unwrap();
            config
        };
        let db = crate::agent_host::DatabaseState::open(Connection::open(path).unwrap()).unwrap();
        let (budget, lease) = begin(&db, &config, "resumed", "new-lease").unwrap();
        let before = snapshot(&db.lock().unwrap(), "root").unwrap();
        assert_eq!(before["used_model_calls"], 4);
        assert!((1500..1600).contains(&before["active_duration_ms"].as_i64().unwrap()));
        let stale: i64 = db
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM ui_request_activity WHERE process='old-process'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stale, 0);
        budget.unwrap().before_model_invocation().unwrap();
        drop(lease);
        assert_eq!(
            snapshot(&db.lock().unwrap(), "root").unwrap()["used_model_calls"],
            5
        );
    }
    #[test]
    fn shared_budget_reservations_survive_handle_recreation_and_never_reset() {
        let connection = Connection::open_in_memory().unwrap();
        ui_store::initialize(&connection).unwrap();
        initialize(&connection).unwrap();
        ui_store::put(
            &connection,
            "setting",
            "persistence",
            json!({"enabled":true,"max_duration_seconds":7200,"max_model_calls":2}),
        )
        .unwrap();
        let mut config = json!({});
        admit(&connection, "request", &mut config).unwrap();
        let db = crate::agent_host::DatabaseState::open(connection).unwrap();
        handle(&db, &config)
            .unwrap()
            .unwrap()
            .before_model_invocation()
            .unwrap();
        let worker = handle(&db, &config).unwrap().unwrap();
        {
            let connection = db.lock().unwrap();
            worker.check().unwrap();
            assert_eq!(
                snapshot(&connection, "request").unwrap()["used_model_calls"],
                1
            );
        }
        worker.before_model_invocation().unwrap();
        assert!(worker.before_model_invocation().is_err());
        assert_eq!(
            snapshot(&db.lock().unwrap(), "request").unwrap()["used_model_calls"],
            2
        );
    }
    #[test]
    fn expired_and_missing_budgets_fail_closed_and_legacy_has_no_budget() {
        let connection = Connection::open_in_memory().unwrap();
        ui_store::initialize(&connection).unwrap();
        initialize(&connection).unwrap();
        let mut config = json!({});
        admit(&connection, "request", &mut config).unwrap();
        connection
            .execute(
                "UPDATE ui_request_budgets SET active_ms=max_duration_ms",
                [],
            )
            .unwrap();
        let db = crate::agent_host::DatabaseState::open(connection).unwrap();
        assert!(handle(&db, &config)
            .unwrap()
            .unwrap()
            .before_model_invocation()
            .is_err());
        assert!(handle(&db, &json!({"request_budget_id":"missing"})).is_err());
        assert!(handle(&db, &json!({})).unwrap().is_none());
    }
    #[test]
    fn reopening_database_preserves_aggregate_and_recovery_checkpoint() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("requests.sqlite");
        let config = {
            let connection = Connection::open(&path).unwrap();
            ui_store::initialize(&connection).unwrap();
            initialize(&connection).unwrap();
            let mut config = json!({});
            admit(&connection, "root", &mut config).unwrap();
            let db = crate::agent_host::DatabaseState::open(connection).unwrap();
            handle(&db, &config)
                .unwrap()
                .unwrap()
                .before_model_invocation()
                .unwrap();
            save_recovery(
                &db.lock().unwrap(),
                &config,
                "session",
                &json!({"segments":3}),
            )
            .unwrap();
            config
        };
        let reopened = Connection::open(path).unwrap();
        initialize(&reopened).unwrap();
        assert_eq!(snapshot(&reopened, "root").unwrap()["used_model_calls"], 1);
        assert_eq!(policy(&reopened, &config).unwrap().unwrap().max_model_calls, 4999);
        assert_eq!(
            recovery(&reopened, &config, "session").unwrap()["segments"],
            3
        );
        assert!(recovery(&reopened, &config, "worker-session")
            .unwrap()
            .is_null());
    }

    #[test]
    fn optional_invocation_leaves_capacity_for_executor_and_synthesis() {
        let (db, config) = active_fixture();
        let budget = handle(&db, &config).unwrap().unwrap();
        db.lock().unwrap().execute("UPDATE ui_request_budgets SET max_model_calls=3 WHERE id='root'", []).unwrap();
        budget.before_optional_invocation().unwrap();
        assert!(budget.before_optional_invocation().is_err());
        budget.before_model_invocation().unwrap();
        budget.before_model_invocation().unwrap();
        assert!(budget.before_model_invocation().is_err());
    }

    #[test]
    fn cached_tokens_are_recorded_per_call_and_cumulatively() {
        let (db, config) = active_fixture();
        let budget = handle(&db, &config).unwrap().unwrap();
        let fresh = snapshot(&db.lock().unwrap(), "root").unwrap();
        assert_eq!(fresh["cached_tokens"], 0);
        assert!(fresh["last_call_cached_tokens"].is_null());
        let reported = |cached| ModelUsage { input_tokens: Some(10), output_tokens: Some(1), cached_tokens: cached };
        budget.record_usage(&reported(Some(1024))).unwrap();
        budget.record_usage(&reported(Some(512))).unwrap();
        let state = snapshot(&db.lock().unwrap(), "root").unwrap();
        assert_eq!(state["last_call_cached_tokens"], 512);
        assert_eq!(state["cached_tokens"], 1536);
        assert_eq!(state["reported_input_tokens"], 20);
        assert_eq!(state["reported_output_tokens"], 2);
        assert_eq!(state["input_usage_calls"], 2);
        assert_eq!(state["output_usage_calls"], 2);
        assert_eq!(state["last_call_input_tokens"], 10);
        assert_eq!(state["last_call_output_tokens"], 1);
        budget.record_usage(&reported(None)).unwrap();
        let state = snapshot(&db.lock().unwrap(), "root").unwrap();
        assert!(state["last_call_cached_tokens"].is_null());
        assert_eq!(state["cached_tokens"], 1536);
        budget.record_usage(&ModelUsage::default()).unwrap();
        let state = snapshot(&db.lock().unwrap(), "root").unwrap();
        assert!(state["last_call_input_tokens"].is_null());
        assert!(state["last_call_output_tokens"].is_null());
        assert_eq!(state["reported_input_tokens"], 30);
        assert_eq!(state["reported_output_tokens"], 3);
        assert_eq!(state["input_usage_calls"], 3);
        assert_eq!(state["output_usage_calls"], 3);
        budget.record_usage(&ModelUsage { input_tokens: Some(0), output_tokens: None, cached_tokens: None }).unwrap();
        let state = snapshot(&db.lock().unwrap(), "root").unwrap();
        assert_eq!(state["input_usage_calls"], 4);
        assert_eq!(state["output_usage_calls"], 3);
        assert_eq!(state["last_call_input_tokens"], 0);
        assert!(state["last_call_output_tokens"].is_null());
    }

    #[test]
    fn reported_usage_totals_saturate_without_sqlite_integer_overflow() {
        let (db, config) = active_fixture();
        let budget = handle(&db, &config).unwrap().unwrap();
        let usage = ModelUsage { input_tokens: Some(u64::MAX), output_tokens: Some(u64::MAX), cached_tokens: Some(u64::MAX) };
        budget.record_usage(&usage).unwrap();
        budget.record_usage(&usage).unwrap();
        let state = snapshot(&db.lock().unwrap(), "root").unwrap();
        assert_eq!(state["reported_input_tokens"], i64::MAX);
        assert_eq!(state["reported_output_tokens"], i64::MAX);
        assert_eq!(state["cached_tokens"], i64::MAX);
    }
    #[test]
    fn legacy_budget_table_gains_cache_columns() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE ui_request_budgets(id TEXT PRIMARY KEY, started_at_ms INTEGER NOT NULL, deadline_ms INTEGER NOT NULL,
            max_model_calls INTEGER NOT NULL, used_model_calls INTEGER NOT NULL DEFAULT 0,
            max_duration_ms INTEGER NOT NULL DEFAULT 0,active_ms INTEGER NOT NULL DEFAULT 0);
            INSERT INTO ui_request_budgets(id,started_at_ms,deadline_ms,max_model_calls) VALUES ('old',0,1000,5);").unwrap();
        initialize(&connection).unwrap();
        let state = snapshot(&connection, "old").unwrap();
        assert_eq!(state["cached_tokens"], 0);
        assert!(state["last_call_cached_tokens"].is_null());
        assert_eq!(state["reported_input_tokens"], 0);
        assert_eq!(state["reported_output_tokens"], 0);
        assert_eq!(state["input_usage_calls"], 0);
        assert_eq!(state["output_usage_calls"], 0);
        assert!(state["last_call_input_tokens"].is_null());
        assert!(state["last_call_output_tokens"].is_null());
        initialize(&connection).unwrap();
    }

    #[test]
    fn policy_changes_only_apply_to_new_requests() {
        let connection = Connection::open_in_memory().unwrap();
        ui_store::initialize(&connection).unwrap();
        initialize(&connection).unwrap();
        let mut first = json!({});
        admit(&connection, "first", &mut first).unwrap();
        ui_store::put(
            &connection,
            "setting",
            "persistence",
            json!({
                "enabled":false,"max_duration_seconds":60,"max_model_calls":4
            }),
        )
        .unwrap();
        let mut second = json!({});
        admit(&connection, "second", &mut second).unwrap();
        assert_eq!(first["persistence"]["max_model_calls"], 5000);
        assert_eq!(first["persistence"]["max_duration_seconds"], 86400);
        assert_eq!(first["request_budget_id"], "first");
        assert_eq!(second["persistence"]["enabled"], false);
        assert!(second["request_budget_id"].is_null());
        assert!(available(&connection, &first).unwrap());
        assert!(!available(&connection, &second).unwrap());
    }

    #[test]
    fn settings_accept_up_to_100000_calls_and_7_days() {
        let parse = |calls: u64, seconds: u64| {
            Policy::parse(&json!({"enabled":true,"max_model_calls":calls,"max_duration_seconds":seconds}))
        };
        assert!(parse(100_000, 604_800).is_ok());
        assert!(parse(1, 1).is_ok());
        assert!(parse(100_001, 86_400).is_err());
        assert!(parse(5_000, 604_801).is_err());
        assert!(parse(0, 86_400).is_err());
    }
}
