//! Hosting-independent session and worker lifecycle. Storage and execution are ports.
pub(crate) mod worker_slots;
pub use worker_slots::while_waiting;

use kageha_contracts::{ErrorCode, Result, ServiceError};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Condvar, Mutex,
    },
    time::{Duration, Instant},
};
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Staged,
    Queued,
    Running,
    Cancelling,
    Cancelled,
    Succeeded,
    Failed,
    Interrupted,
}
impl RunState {
    pub fn terminal(self) -> bool {
        matches!(
            self,
            Self::Cancelled | Self::Succeeded | Self::Failed | Self::Interrupted
        )
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunSpec {
    pub id: String,
    pub session: String,
    pub turn: String,
    pub request: String,
    pub parent: Option<String>,
    pub parent_run: Option<String>,
    pub agent: String,
    pub objective: String,
    pub depth: u32,
    pub input: Value,
}
impl RunSpec {
    pub fn validate(&self) -> Result<()> {
        if [
            &self.id,
            &self.session,
            &self.turn,
            &self.request,
            &self.agent,
        ]
        .into_iter()
        .any(|v| v.is_empty() || v.len() > 128 || v.chars().any(char::is_control))
            || self.objective.trim().is_empty()
            || self.objective.len() > 65536
            || self.depth > 3
            || serde_json::to_vec(&self.input)
                .map_err(|_| ServiceError::new(ErrorCode::Invalid, "Invalid runtime input"))?
                .len()
                > 32 * 1024 * 1024
        {
            return Err(ServiceError::new(
                ErrorCode::Invalid,
                "Invalid runtime run specification",
            ));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Completion {
    pub state: RunState,
    pub result: Value,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunRecord {
    pub spec: RunSpec,
    pub state: RunState,
    pub revision: u64,
    pub lease: Option<String>,
    pub completion: Option<Completion>,
    pub cancel_requested: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Steering {
    pub id: String,
    pub body: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Effect {
    pub scope: String,
    pub id: String,
    pub digest: String,
    pub receipt: Option<Value>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunEvent {
    pub sequence: u64,
    pub record: RunRecord,
}
pub trait RunStore: Send + Sync {
    fn request_budget_begin(&self,run:&RunRecord)->Result<Option<kageha_contracts::request_budget::BudgetHandle>> {
        if run.spec.input["persistence"]["enabled"]==true {
            return Err(ServiceError::new(ErrorCode::Unsupported,"This run store cannot persist request budgets"));
        }
        Ok(None)
    }
    fn recovery_checkpoint(&self,_scope:&str)->Result<Value>{Ok(Value::Null)}
    fn request_budget_snapshot(&self,_root:&str)->Result<Value>{Ok(Value::Null)}
    fn save_recovery_checkpoint(&self,_scope:&str,state:&Value)->Result<()> {
        if state.is_null(){Ok(())}else{Err(ServiceError::new(ErrorCode::Unsupported,"This run store cannot persist recovery checkpoints"))}
    }
    fn events(&self, run: &str, after: u64, limit: usize) -> Result<Vec<RunEvent>>;
    fn stage(&self, spec: &RunSpec, limit: usize) -> Result<RunRecord>;
    fn import_run(&self, record: &RunRecord) -> Result<()>;
    fn activate(&self, id: &str) -> Result<RunRecord>;
    fn get(&self, id: &str) -> Result<Option<RunRecord>>;
    fn find_turn(&self, turn: &str) -> Result<Option<RunRecord>>;
    fn list(&self) -> Result<Vec<RunRecord>>;
    fn active(&self) -> Result<Vec<RunRecord>>;
    fn claim(&self, depth: u32) -> Result<Option<RunRecord>>;
    fn finish(&self, id: &str, lease: &str, completion: Completion) -> Result<RunRecord>;
    fn recover(&self, id: &str, completion: Completion) -> Result<RunRecord>;
    /// Returns a `running` run whose execution ended with the previous process to
    /// `queued`, without a lease, so a worker runs it again. Stores that cannot
    /// requeue report `Unsupported` and the run is recovered as interrupted.
    fn requeue(&self, _id: &str) -> Result<RunRecord> {
        Err(ServiceError::new(ErrorCode::Unsupported, "This run store cannot requeue runs"))
    }
    fn late_receipts(&self, id: &str) -> Result<Vec<Value>>;
    fn cancel(&self, id: &str) -> Result<RunRecord>;
    fn steer(&self, id: &str, message: &Steering) -> Result<()>;
    fn steering(&self, id: &str) -> Result<Vec<Steering>>;
    fn steering_history(&self, id: &str) -> Result<Vec<Steering>>;
    fn erase_session(&self, session: &str, scopes: &[String]) -> Result<()>;
    fn publications(&self) -> Result<Vec<RunRecord>>;
    fn published(&self, id: &str, revision: u64) -> Result<()>;
    fn effect_get(&self, scope: &str, id: &str) -> Result<Option<Effect>>;
    fn effect_begin(&self, scope: &str, id: &str, digest: &str) -> Result<Option<Value>>;
    fn effect_finish(&self, scope: &str, id: &str, digest: &str, result: Value) -> Result<()>;
    fn effect_import(&self, effect: &Effect) -> Result<()>;
}
/// What [`Supervisor::recover`] does with a run that was `running` when the previous
/// process stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Restart {
    /// Inspect committed evidence; without it the run is interrupted.
    Inspect,
    /// Run it again; the executor continues from its own checkpoint.
    Resume,
    /// Leave it `running` without a worker until [`Supervisor::resume`] is called.
    Hold,
}
pub trait RunExecutor: Send + Sync {
    fn execute(&self, run: &RunRecord, cancel: Arc<AtomicBool>) -> Result<Completion>;
    /// Startup decision for a run that was `running` when the previous process stopped.
    /// Only called by [`Supervisor::recover`], before any worker starts.
    fn restart(&self, _run: &RunRecord) -> Result<Restart> {
        Ok(Restart::Inspect)
    }
    /// Read committed evidence only. Never dispatch application inputs during inspection.
    fn inspect(&self, run: &RunRecord) -> Result<Option<Completion>>;
    /// Idempotent compatibility projection; it must never execute the run.
    fn publish(&self, run: &RunRecord) -> Result<()>;
}
#[derive(Clone, Copy)]
pub struct Limits {
    pub per_depth: usize,
    pub queue_per_depth: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            per_depth: 4,
            queue_per_depth: 32,
        }
    }
}
pub struct Supervisor {
    pub store: Arc<dyn RunStore>,
    executor: Arc<dyn RunExecutor>,
    limits: Limits,
    flags: Mutex<HashMap<String, Arc<AtomicBool>>>,
    wake: (Mutex<u64>, Condvar),
    stopping: AtomicBool,
    started: AtomicBool,
    alive: AtomicUsize,
    publication: Mutex<()>,
    slots: worker_slots::WorkerSlots,
    threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
}
fn fault() -> ServiceError {
    ServiceError::new(
        ErrorCode::Unavailable,
        "Runtime supervisor state unavailable",
    )
}
impl Supervisor {
    pub fn new(
        store: Arc<dyn RunStore>,
        executor: Arc<dyn RunExecutor>,
        limits: Limits,
    ) -> Result<Arc<Self>> {
        if limits.per_depth == 0
            || limits.per_depth > 8
            || limits.queue_per_depth < limits.per_depth
            || limits.queue_per_depth > 128
        {
            return Err(ServiceError::new(
                ErrorCode::Invalid,
                "Invalid runtime queue limits",
            ));
        }
        Ok(Arc::new(Self {
            store,
            executor,
            limits,
            flags: Mutex::new(HashMap::new()),
            wake: (Mutex::new(0), Condvar::new()),
            stopping: AtomicBool::new(false),
            started: AtomicBool::new(false),
            alive: AtomicUsize::new(0),
            publication: Mutex::new(()),
            slots: worker_slots::WorkerSlots::default(),
            threads: Mutex::new(Vec::new()),
        }))
    }
    pub fn stage(&self, spec: &RunSpec) -> Result<RunRecord> {
        spec.validate()?;
        if self.stopping.load(Ordering::SeqCst) {
            return Err(ServiceError::new(
                ErrorCode::Unavailable,
                "Runtime is draining",
            ));
        }
        self.store.stage(spec, self.limits.queue_per_depth)
    }
    pub fn activate(&self, id: &str) -> Result<RunRecord> {
        let run = self.store.activate(id)?;
        self.notify();
        self.flush()?;
        Ok(run)
    }
    pub fn flush(&self) -> Result<()> {
        let _publication = self.publication.lock().map_err(|_| fault())?;
        for record in self.store.publications()? {
            self.executor.publish(&record)?;
            self.store.published(&record.spec.id, record.revision)?;
        }
        Ok(())
    }
    pub fn recover(&self) -> Result<()> {
        if self.started.load(Ordering::SeqCst) {
            return Err(ServiceError::new(
                ErrorCode::Conflict,
                "Recovery must precede worker admission",
            ));
        }
        for record in self.store.active()? {
            if record.state == RunState::Running && !record.cancel_requested {
                match self.executor.restart(&record)? {
                    Restart::Hold => continue,
                    Restart::Resume => match self.store.requeue(&record.spec.id) {
                        Ok(_) => continue,
                        Err(error) if error.code == ErrorCode::Unsupported => {}
                        Err(error) => return Err(error),
                    },
                    Restart::Inspect => {}
                }
            }
            if matches!(record.state, RunState::Running | RunState::Cancelling) {
                let completion=self.executor.inspect(&record)?.unwrap_or(Completion{state:if record.cancel_requested{RunState::Cancelled}else{RunState::Interrupted},result:serde_json::json!({"interrupted":true,"message":"Execution was interrupted after admission. Inspect its receipts before retrying."})});
                self.store.recover(&record.spec.id, completion)?;
            }
        }
        self.flush()
    }
    /// Runs a run that [`Restart::Hold`] left `running` without a worker.
    pub fn resume(&self, id: &str) -> Result<RunRecord> {
        if self.flags.lock().map_err(|_| fault())?.contains_key(id) {
            return Err(ServiceError::new(ErrorCode::Conflict, "Run is already executing"));
        }
        let record = self.store.requeue(id)?;
        self.notify();
        self.flush()?;
        Ok(record)
    }
    /// Ends a run that [`Restart::Hold`] left `running` without a worker.
    pub fn settle_held(&self, id: &str, completion: Completion) -> Result<RunRecord> {
        if self.flags.lock().map_err(|_| fault())?.contains_key(id) {
            return Err(ServiceError::new(ErrorCode::Conflict, "Run is already executing"));
        }
        let record = self.store.recover(id, completion)?;
        self.notify();
        self.flush()?;
        Ok(record)
    }
    pub fn start(self: &Arc<Self>) -> Result<()> {
        if self.stopping.load(Ordering::SeqCst) {
            return Err(ServiceError::new(
                ErrorCode::Unavailable,
                "Runtime is draining; workers cannot restart",
            ));
        }
        if self.started.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.slots.set_base_threads(self.limits.per_depth);
        for depth in 0..=3 {
            for _ in 0..self.limits.per_depth {
                self.spawn_worker(depth).map_err(|_| {
                    self.stopping.store(true, Ordering::SeqCst);
                    self.notify();
                    fault()
                })?;
            }
        }
        Ok(())
    }
    fn spawn_worker(self: &Arc<Self>, depth: u32) -> std::io::Result<()> {
        let mut threads = self.threads.lock().map_err(|_| std::io::Error::other("Runtime threads unavailable"))?;
        let supervisor = self.clone();
        self.alive.fetch_add(1, Ordering::SeqCst);
        let worker = std::thread::Builder::new()
            .name(format!("kageha-runtime-{depth}"))
            .spawn(move || {
                supervisor.worker(depth);
                supervisor.alive.fetch_sub(1, Ordering::SeqCst);
            })
            .inspect_err(|_| {
                self.alive.fetch_sub(1, Ordering::SeqCst);
            })?;
        threads.push(worker);
        Ok(())
    }
    fn notify(&self) {
        if let Ok(mut version) = self.wake.0.lock() {
            *version = version.wrapping_add(1);
            self.wake.1.notify_all();
        }
    }
    fn worker(self: &Arc<Self>, depth: u32) {
        while !self.stopping.load(Ordering::SeqCst) {
            let observed = self.wake.0.lock().map(|v| *v).unwrap_or(0);
            let Some(slot) = worker_slots::Slot::acquire(self, depth) else {
                self.wait_for_work(observed);
                continue;
            };
            let claimed = self.store.claim(depth);
            if let Ok(Some(run)) = claimed {
                let flag = Arc::new(AtomicBool::new(run.cancel_requested));
                if let Ok(mut flags) = self.flags.lock() {
                    flags.insert(run.spec.id.clone(), flag.clone());
                }
                if self
                    .store
                    .get(&run.spec.id)
                    .ok()
                    .flatten()
                    .is_some_and(|r| r.cancel_requested)
                {
                    flag.store(true, Ordering::SeqCst);
                }
                let _ = self.flush();
                let result = if flag.load(Ordering::SeqCst) {
                    Ok(Completion {
                        state: RunState::Cancelled,
                        result: Value::Null,
                    })
                } else {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        worker_slots::with_slot(Some(slot.clone()), || self.executor.execute(&run, flag.clone()))
                    }))
                    .unwrap_or_else(|_| {
                        Err(ServiceError::new(
                            ErrorCode::Unavailable,
                            "Run executor panicked",
                        ))
                    })
                };
                let completion = result.unwrap_or_else(|e| Completion {
                    state: RunState::Failed,
                    result: serde_json::json!({"message":e.message}),
                });
                // Keep the result in this worker until the journal commits it. A
                // transient write failure must not discard evidence or reexecute.
                let mut retry = Duration::from_millis(10);
                loop {
                    match self.store.finish(
                        &run.spec.id,
                        run.lease.as_deref().unwrap_or(""),
                        completion.clone(),
                    ) {
                        Ok(record) => {
                            if matches!(
                                record.state,
                                RunState::Failed | RunState::Interrupted | RunState::Cancelled
                            ) {
                                loop {
                                    match self.cancel_descendants(&run.spec.id) {
                                        Ok(()) => break,
                                        Err(error) if error.code == ErrorCode::Unavailable => {
                                            std::thread::sleep(retry);
                                            retry = (retry * 2).min(Duration::from_millis(250));
                                        }
                                        Err(error) => {
                                            // A broken ancestry/store contract won't heal by retrying.
                                            // Fence new admission and surface a redacted control-plane fault.
                                            self.stopping.store(true, Ordering::SeqCst);
                                            self.notify();
                                            eprintln!("Runtime descendant cancellation stopped admission: {:?}", error.code);
                                            break;
                                        }
                                    }
                                }
                            }
                            break;
                        }
                        Err(error) if error.code == ErrorCode::Unavailable => {
                            // Retain the completion during graceful drain too. If
                            // storage stays unavailable, join reports a deadline.
                            std::thread::sleep(retry);
                            retry = (retry * 2).min(Duration::from_millis(250));
                        }
                        Err(_) => break, // obsolete lease or erased run: never overwrite it
                    }
                }
                if let Ok(mut flags) = self.flags.lock() {
                    flags.remove(&run.spec.id);
                }
                let _ = self.flush();
                drop(slot);
                self.notify();
                continue;
            }
            drop(slot);
            let _ = self.flush();
            self.wait_for_work(observed);
        }
    }
    fn wait_for_work(&self, observed: u64) {
        if let Ok(version) = self.wake.0.lock() {
            let _ = self.wake.1.wait_timeout_while(version, Duration::from_secs(1), |version| {
                *version == observed && !self.stopping.load(Ordering::SeqCst)
            });
        }
    }
    pub fn cancel(&self, id: &str) -> Result<RunRecord> {
        let initial = self.store.cancel(id)?;
        if initial.state.terminal() && !initial.cancel_requested {
            return Ok(initial);
        }
        if let Some(flag) = self.flags.lock().map_err(|_| fault())?.get(id) {
            flag.store(true, Ordering::SeqCst);
        }
        let descendants = self.cancel_descendants(id);
        self.notify();
        self.flush()?;
        descendants?;
        Ok(initial)
    }
    fn cancel_descendants(&self, parent: &str) -> Result<()> {
        let mut failure = None;
        for child in self.store.active()? {
            // Traverse durable edges, including completed intermediate agents.
            let mut ancestor = child.spec.parent_run.clone();
            let mut visited = std::collections::HashSet::new();
            while let Some(id) = ancestor {
                if id == parent {
                    if let Err(error) = self.store.cancel(&child.spec.id) {
                        failure.get_or_insert(error);
                    }
                    if let Some(flag) = self.flags.lock().map_err(|_| fault())?.get(&child.spec.id)
                    {
                        flag.store(true, Ordering::SeqCst);
                    }
                    break;
                }
                if !visited.insert(id.clone()) {
                    return Err(ServiceError::new(
                        ErrorCode::Conflict,
                        "Run ancestry contains a cycle",
                    ));
                }
                ancestor = self.store.get(&id)?.and_then(|r| r.spec.parent_run);
            }
        }
        failure.map_or(Ok(()), Err)
    }
    /// Wait for any direct child of the parent session, without holding a store lock while asleep.
    /// Read authoritative records, not eventually consistent UI projections.
    pub fn wait_children(
        &self,
        parent: &str,
        ids: &[String],
        control: &kageha_contracts::execution::Control<'_>,
        timeout: Duration,
    ) -> Result<Vec<RunRecord>> {
        self.wait_children_until(parent, ids, control, timeout, false)
    }

    /// Wait for every child to become terminal. On timeout, return the children
    /// already terminal so callers can continue waiting only for the remainder.
    pub fn wait_all_children(
        &self,
        parent: &str,
        ids: &[String],
        control: &kageha_contracts::execution::Control<'_>,
        timeout: Duration,
    ) -> Result<Vec<RunRecord>> {
        self.wait_children_until(parent, ids, control, timeout, true)
    }

    fn wait_children_until(
        &self,
        parent: &str,
        ids: &[String],
        control: &kageha_contracts::execution::Control<'_>,
        timeout: Duration,
        all: bool,
    ) -> Result<Vec<RunRecord>> {
        if ids.is_empty()
            || ids.len() > 32
            || timeout > Duration::from_secs(30)
            || ids.iter().collect::<std::collections::HashSet<_>>().len() != ids.len()
        {
            return Err(ServiceError::new(
                ErrorCode::Invalid,
                "Invalid child wait request",
            ));
        }
        let until = Instant::now() + timeout;
        loop {
            control.check().map_err(|e| {
                ServiceError::new(
                    if e.kind() == std::io::ErrorKind::Interrupted {
                        ErrorCode::Cancelled
                    } else {
                        ErrorCode::Deadline
                    },
                    e.to_string(),
                )
            })?;
            let observed = *self.wake.0.lock().map_err(|_| fault())?;
            let mut terminal = Vec::new();
            for id in ids {
                let run = self
                    .store
                    .get(id)?
                    .filter(|r| r.spec.parent.as_deref() == Some(parent))
                    .ok_or_else(|| {
                        ServiceError::new(
                            ErrorCode::Invalid,
                            "Worker is not a direct child of this session",
                        )
                    })?;
                if run.state.terminal() {
                    terminal.push(run);
                }
            }
            if (all && terminal.len() == ids.len()) || (!all && !terminal.is_empty()) || Instant::now() >= until {
                return Ok(terminal);
            }
            let remaining = until.saturating_duration_since(Instant::now());
            // Atomic cancellation has no notifier; bound cancellation latency to 20ms.
            let duration = remaining.min(Duration::from_millis(20)).min(
                control
                    .deadline
                    .map(|d| d.saturating_duration_since(Instant::now()))
                    .unwrap_or(remaining),
            );
            let version = self.wake.0.lock().map_err(|_| fault())?;
            let _ = self
                .wake
                .1
                .wait_timeout_while(version, duration, |v| *v == observed)
                .map_err(|_| fault())?;
        }
    }
    pub fn shutdown(&self) -> Result<()> {
        self.stopping.store(true, Ordering::SeqCst);
        let mut failure = None;
        for record in self.store.active()? {
            if !record.state.terminal() {
                if let Err(error) = self.cancel(&record.spec.id) {
                    failure.get_or_insert(error);
                }
            }
        }
        // Signal every in-process executor even if a journal/projection is unavailable.
        for flag in self.flags.lock().map_err(|_| fault())?.values() {
            flag.store(true, Ordering::SeqCst);
        }
        self.notify();
        failure.map_or(Ok(()), Err)
    }
    pub fn join(&self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        while self.alive.load(Ordering::SeqCst) > 0 {
            if Instant::now() >= deadline {
                return Err(ServiceError::new(
                    ErrorCode::Deadline,
                    "Runtime still has cooperative executors draining",
                ));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        for thread in self.threads.lock().map_err(|_| fault())?.drain(..) {
            let _ = thread.join();
        }
        Ok(())
    }
}
