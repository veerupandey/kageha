//! Application composition and compatibility projections for the independent runtime.
use crate::{
    ui_store::{db_error, ApiResult},
    webui::{self, Controls, Database},
};
use kageha_contracts::{ErrorCode, ServiceError};
use kageha_runtime::lifecycle::*;
use kageha_runtime_store::SqliteRunStore;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{atomic::AtomicBool, Arc, Mutex, OnceLock},
};
pub(crate) struct Host {
    pub supervisor: Arc<Supervisor>,
    pub controls: Controls,
    db: Database,
}
struct Executor {
    store: Arc<dyn RunStore>,
    db: Database,
    controls: Controls,
}
static STORES: OnceLock<Mutex<HashMap<PathBuf, (Arc<SqliteRunStore>, Database)>>> = OnceLock::new();
static HOSTS: OnceLock<Mutex<HashMap<PathBuf, Arc<Host>>>> = OnceLock::new();
fn runtime_error(e: impl std::fmt::Display) -> ServiceError {
    ServiceError::new(ErrorCode::Unavailable, e.to_string())
}
fn api(e: ServiceError) -> (u16, String) {
    (
        if matches!(e.code, ErrorCode::Invalid) {
            400
        } else if matches!(e.code, ErrorCode::Conflict | ErrorCode::StaleGeneration) {
            409
        } else {
            500
        },
        e.message,
    )
}
pub(crate) fn initialize(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS ui_runtime_runs(id TEXT PRIMARY KEY,spec TEXT NOT NULL,state TEXT NOT NULL,projected_revision INTEGER NOT NULL DEFAULT 0); CREATE TABLE IF NOT EXISTS ui_runtime_results(turn TEXT PRIMARY KEY,status TEXT NOT NULL,result TEXT NOT NULL); CREATE TABLE IF NOT EXISTS ui_runtime_steering(run TEXT,id TEXT,turn TEXT,body TEXT,PRIMARY KEY(run,id));")
}
pub(crate) fn spec(
    workspace: &Path,
    session: &str,
    request: &str,
    id: &str,
    agent: &str,
    objective: &str,
    depth: u32,
    parent: Option<&str>,
    parent_run: Option<String>,
    messages: Vec<Value>,
    configuration: Value,
    max_model_calls: u32,
) -> RunSpec {
    RunSpec {
        id: id.into(),
        session: session.into(),
        turn: webui::turn_id(session, request),
        request: request.into(),
        parent: parent.map(str::to_owned),
        parent_run,
        agent: agent.into(),
        objective: objective.into(),
        depth,
        input: json!({"workspace":workspace,"messages":messages,"configuration":configuration,"max_model_calls":max_model_calls}),
    }
}
pub(crate) fn prepare(db: &Connection, spec: &RunSpec) -> rusqlite::Result<()> {
    let body = serde_json::to_string(spec)
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    db.execute(
        "INSERT INTO ui_runtime_runs VALUES (?,?,'prepared',0)",
        params![spec.id, body],
    )?;
    Ok(())
}
fn path(db: &Database) -> ApiResult<PathBuf> {
    let connection = db.lock().map_err(db_error)?;
    Ok(connection
        .path()
        .map(|p| {
            Path::new(p)
                .parent()
                .unwrap_or(Path::new("."))
                .join("components/runtime.sqlite")
        })
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!(
                "kageha-runtime-{}-{:p}/journal.sqlite",
                std::process::id(),
                Arc::as_ptr(db)
            ))
        }))
}
pub(crate) fn store(db: &Database) -> ApiResult<Arc<SqliteRunStore>> {
    let path = path(db)?;
    let mut stores = STORES
        .get_or_init(Default::default)
        .lock()
        .map_err(db_error)?;
    if let Some((store, _)) = stores.get(&path) {
        return Ok(store.clone());
    }
    let store = Arc::new(SqliteRunStore::open(&path).map_err(api)?);
    let legacy = {
        let db = db.lock().map_err(db_error)?;
        let exists: bool = db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='ui_effects')",
                [],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        if exists {
            let mut q = db
                .prepare("SELECT turn_id,call_id,digest,state,result FROM ui_effects")
                .map_err(db_error)?;
            let rows = q
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, Option<String>>(4)?,
                    ))
                })
                .map_err(db_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(db_error)?;
            rows
        } else {
            Vec::new()
        }
    };
    for (scope, id, digest, state, receipt) in legacy {
        store
            .effect_import(&Effect {
                scope,
                id,
                digest,
                receipt: if state == "completed" {
                    Some(
                        receipt
                            .and_then(|s| serde_json::from_str(&s).ok())
                            .ok_or((500, "Legacy effect receipt is invalid".into()))?,
                    )
                } else {
                    None
                },
            })
            .map_err(api)?;
    }
    stores.insert(path, (store.clone(), db.clone()));
    Ok(store)
}
pub(crate) fn get(db: &Database) -> ApiResult<Arc<Host>> {
    let path = path(db)?;
    let mut hosts = HOSTS
        .get_or_init(Default::default)
        .lock()
        .map_err(db_error)?;
    if let Some(host) = hosts.get(&path) {
        return Ok(host.clone());
    }
    let store = store(db)?;
    let controls: Controls = Arc::new(Mutex::new(HashMap::new()));
    let executor = Arc::new(Executor {
        store: store.clone(),
        db: db.clone(),
        controls: controls.clone(),
    });
    let supervisor = Supervisor::new(store, executor, Limits::default()).map_err(api)?;
    let host = Arc::new(Host {
        supervisor,
        controls,
        db: db.clone(),
    });
    host.import_legacy()?;
    host.reconcile()?;
    host.supervisor.recover().map_err(api)?;
    let goal_host = Arc::downgrade(&host);
    std::thread::Builder::new().name("goal-dispatch".into()).spawn(move || {
        while let Some(host) = goal_host.upgrade() {
            let _ = host.dispatch_continuations();
            drop(host);
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }).map_err(db_error)?;
    hosts.insert(path, host.clone());
    Ok(host)
}
impl Host {
    pub fn start(&self) -> ApiResult<()> {
        self.supervisor.start().map_err(api)
    }
    pub fn submit(&self, id: &str) -> ApiResult<RunRecord> {
        let (spec, state, revision) = {
            let db = self.db.lock().map_err(db_error)?;
            db.query_row(
                "SELECT spec,state,projected_revision FROM ui_runtime_runs WHERE id=?",
                [id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, u64>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(db_error)?
            .ok_or((404, "Runtime preparation missing".into()))?
        };
        let spec: RunSpec = serde_json::from_str(&spec).map_err(db_error)?;
        if state == "rejected" || state == "cancelled" {
            if let Some(record) = self.supervisor.store.get(id).map_err(api)? {
                if !record.state.terminal() {
                    return self.supervisor.cancel(id).map_err(api);
                }
                return Ok(record);
            }
            let record = RunRecord {
                spec,
                state: if state == "cancelled" {
                    RunState::Cancelled
                } else {
                    RunState::Failed
                },
                revision: revision + 1,
                lease: None,
                completion: Some(Completion {
                    state: if state == "cancelled" {
                        RunState::Cancelled
                    } else {
                        RunState::Failed
                    },
                    result: json!({"message":"Runtime admission was stopped","not_dispatched":true}),
                }),
                cancel_requested: state == "cancelled",
            };
            self.supervisor.store.import_run(&record).map_err(api)?;
            self.supervisor.flush().map_err(api)?;
            return Ok(record);
        }
        let previous = self.supervisor.store.get(id).map_err(api)?;
        if previous.is_none() && state == "submitted" {
            let mut record = RunRecord {
                spec,
                state: RunState::Staged,
                revision: revision + 1,
                lease: None,
                completion: None,
                cancel_requested: false,
            };
            let executor = Executor {
                store: self.supervisor.store.clone(),
                db: self.db.clone(),
                controls: self.controls.clone(),
            };
            let completion=executor.inspect(&record).map_err(api)?.unwrap_or(Completion{state:RunState::Interrupted,result:json!({"interrupted":true,"message":"Runtime journal is missing after admission. Inspect receipts before retrying."})});
            record.state = completion.state;
            record.completion = Some(completion);
            self.supervisor.store.import_run(&record).map_err(api)?;
            self.supervisor.flush().map_err(api)?;
            return Ok(record);
        }
        let record = match self.supervisor.stage(&spec) {
            Ok(record) => record,
            Err(e) => {
                if matches!(e.code, ErrorCode::Invalid | ErrorCode::Conflict) {
                    let record = RunRecord {
                        spec: spec.clone(),
                        state: RunState::Failed,
                        revision: revision + 1,
                        lease: None,
                        completion: Some(Completion {
                            state: RunState::Failed,
                            result: json!({"message":e.message,"not_dispatched":true}),
                        }),
                        cancel_requested: false,
                    };
                    self.supervisor.store.import_run(&record).map_err(api)?;
                    self.db.lock().map_err(db_error)?.execute("UPDATE ui_runtime_runs SET state='rejected' WHERE id=? AND state='prepared'",[id]).map_err(db_error)?;
                    self.supervisor.flush().map_err(api)?;
                }
                return Err(api(e));
            }
        };
        if record.state.terminal() {
            self.supervisor.flush().map_err(api)?;
            return Ok(record);
        }
        let cancelled = {
            let db = self.db.lock().map_err(db_error)?;
            let state: String = db
                .query_row("SELECT state FROM ui_runtime_runs WHERE id=?", [id], |r| {
                    r.get(0)
                })
                .map_err(db_error)?;
            if matches!(state.as_str(), "cancelled" | "rejected") {
                true
            } else {
                db.execute(
                    "UPDATE ui_runtime_runs SET state='submitted' WHERE id=?",
                    [id],
                )
                .map_err(db_error)?;
                false
            }
        };
        if cancelled {
            return self.supervisor.cancel(id).map_err(api);
        }
        self.supervisor.activate(id).map_err(api)
    }
    fn dispatch_continuations(&self) -> ApiResult<()> {
        let ids = {
            let db = self.db.lock().map_err(db_error)?;
            let mut query = db.prepare(
                "SELECT r.id FROM ui_runtime_runs r LEFT JOIN ui_goals g ON g.turn=r.id
                 WHERE r.state='prepared' AND
                 ((g.state='active' AND json_extract(r.spec,'$.input.configuration.goal_continuation')=1)
                    OR json_extract(r.spec,'$.input.configuration.run_change_continuation')=1)
                 ORDER BY r.rowid LIMIT 16",
            ).map_err(db_error)?;
            let rows = query.query_map([], |row| row.get::<_, String>(0)).map_err(db_error)?
                .collect::<Result<Vec<_>, _>>().map_err(db_error)?;
            rows
        };
        for id in ids {
            self.submit(&id)?;
        }
        Ok(())
    }

    fn reconcile(&self) -> ApiResult<()> {
        let ids = {
            let db = self.db.lock().map_err(db_error)?;
            let mut q = db
                .prepare("SELECT id FROM ui_runtime_runs ORDER BY rowid")
                .map_err(db_error)?;
            let rows = q
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(db_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(db_error)?;
            rows
        };
        for id in ids {
            match self.submit(&id) {
                Ok(_) => {}
                Err((409, _)) => {}
                Err(e) => {
                    eprintln!("runtime_host: reconcile skipping run {id}: {e:?}");
                }
            }
        }
        Ok(())
    }
    fn import_legacy(&self) -> ApiResult<()> {
        let rows = {
            let db = self.db.lock().map_err(db_error)?;
            let mut q=db.prepare("SELECT id,parent_chat,child_chat,agent_id,objective,state,revision,result,depth FROM ui_jobs WHERE id NOT IN (SELECT id FROM ui_runtime_runs)").map_err(db_error)?;
            let rows = q
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, u64>(6)?,
                        r.get::<_, Option<String>>(7)?,
                        r.get::<_, u32>(8)?,
                    ))
                })
                .map_err(db_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(db_error)?;
            rows
        };
        for (id, parent, child, agent, objective, state, revision, result, depth) in rows {
            if self.supervisor.store.get(&id).map_err(api)?.is_some() {
                continue;
            }
            let spec = spec(
                Path::new("."),
                &child,
                "job",
                &id,
                &agent,
                &objective,
                depth,
                Some(&parent),
                None,
                vec![json!({"role":"user","content":objective})],
                json!({"legacy":true}),
                128,
            );
            let terminal = match state.as_str() {
                "succeeded" => RunState::Succeeded,
                "failed" => RunState::Failed,
                "cancelled" => RunState::Cancelled,
                _ => RunState::Interrupted,
            };
            let mut record = RunRecord {
                spec,
                state: RunState::Staged,
                revision: revision + 1,
                lease: None,
                completion: None,
                cancel_requested: false,
            };
            let executor = Executor {
                store: self.supervisor.store.clone(),
                db: self.db.clone(),
                controls: self.controls.clone(),
            };
            let completion=executor.inspect(&record).map_err(api)?.unwrap_or(Completion{state:terminal,result:result.and_then(|s|serde_json::from_str(&s).ok()).unwrap_or(json!({"message":"Legacy worker interrupted; inspect receipts before retrying.","interrupted":terminal==RunState::Interrupted}))});
            record.state = completion.state;
            record.completion = Some(completion);
            self.supervisor.store.import_run(&record).map_err(api)?;
        }
        Ok(())
    }
    pub fn cancel_turn(&self, session: &str, turn: &str) -> ApiResult<bool> {
        if let Some(record) = self.supervisor.store.find_turn(turn).map_err(api)? {
            if record.spec.session != session || record.state.terminal() {
                return Ok(false);
            }
            self.supervisor.cancel(&record.spec.id).map_err(api)?;
            // A turn held for credential unlock has no worker to observe the stop.
            crate::ui_turn_resume::cancel_held(&self.db, &self.supervisor, turn)?;
            return Ok(true);
        }
        let id: Option<String> = {
            let db = self.db.lock().map_err(db_error)?;
            db.query_row("SELECT id FROM ui_runtime_runs WHERE json_extract(spec,'$.session')=? AND json_extract(spec,'$.turn')=?",params![session,turn],|r|r.get(0)).optional().map_err(db_error)?
        };
        if let Some(id) = id {
            self.db
                .lock()
                .map_err(db_error)?
                .execute(
                    "UPDATE ui_runtime_runs SET state='cancelled' WHERE id=? AND state='prepared'",
                    [&id],
                )
                .map_err(db_error)?;
            self.submit(&id)?;
            return Ok(true);
        }
        Ok(false)
    }
}
fn remote_completion(remote: &Value, run: &RunRecord) -> Option<Completion> {
    let (state, native, code) = match remote["state"].as_str()? {
        "succeeded" => (RunState::Succeeded, "completed", None),
        "failed" => (RunState::Failed, "failed", Some("execution_failed")),
        "cancelled" => (RunState::Cancelled, "cancelled", Some("cancelled")),
        "interrupted" => (RunState::Interrupted, "interrupted", Some("runtime_disconnected")),
        _ => return None,
    };
    let mut result = remote["completion"]["result"].clone();
    if !result.is_object() { return None; }
    let code = if result["uncertain"] == true {
        Some("tool_outcome_unconfirmed")
    } else if kageha_runtime_service::bridge_client::deadline_failure(&result) {
        Some("deadline_exceeded")
    } else {
        result["failure_code"].as_str().or(code)
    };
    let code = code.map(str::to_owned);
    let configuration = &run.spec.input["configuration"];
    result["failure_code"] = json!(code);
    result["native_status"] = json!(native);
    result["speaker"] = json!({"id":configuration["persona"]["id"],"name":configuration["persona"]["name"],"avatar":configuration["persona"]["avatar"]});
    result["job_reply_id"] = json!(format!("job:{}:result", run.spec.id));
    result["chat_reply"] = json!({"attachments":[]});
    result["recovered_from"] = json!("standalone_runtime");
    Some(Completion { state, result })
}

impl RunExecutor for Executor {
    fn restart(&self, run: &RunRecord) -> kageha_contracts::Result<Restart> {
        // A failed decision must not stop the server; the run is recovered as before.
        Ok(crate::ui_turn_resume::restart(&self.db, run).unwrap_or_else(|error| {
            eprintln!("Turn {} could not be prepared to continue after restart: {error}", run.spec.turn);
            Restart::Inspect
        }))
    }
    fn execute(
        &self,
        run: &RunRecord,
        cancel: Arc<AtomicBool>,
    ) -> kageha_contracts::Result<Completion> {
        let configuration = run
            .spec
            .input
            .get("configuration")
            .ok_or_else(|| runtime_error("Pinned run configuration missing"))?;
        {
            let db = self.db.lock().map_err(runtime_error)?;
            let saved: Option<String> = db
                .query_row(
                    "SELECT spec FROM ui_run_specs WHERE chat_id=? AND request_id=?",
                    params![run.spec.session, run.spec.request],
                    |r| r.get(0),
                )
                .optional()
                .map_err(runtime_error)?;
            let saved: Value = saved
                .and_then(|s| serde_json::from_str(&s).ok())
                .ok_or_else(|| runtime_error("Pinned execution configuration unavailable"))?;
            if &saved != configuration {
                return Err(runtime_error(
                    "Pinned execution configuration changed before dispatch",
                ));
            }
        }
        self.controls
            .lock()
            .map_err(runtime_error)?
            .insert(run.spec.turn.clone(), cancel.clone());
        let workspace = PathBuf::from(
            run.spec.input["workspace"]
                .as_str()
                .ok_or_else(|| runtime_error("Pinned workspace missing"))?,
        );
        crate::ui_job_worktrees::prepare(&self.db,run,&cancel).map_err(runtime_error)?;
        let (budget,activity)=crate::ui_persistence::begin(&self.db,configuration,&run.spec.id,run.lease.as_deref().unwrap_or("local")).map_err(runtime_error)?;
        kageha_contracts::request_budget::inherited(budget, || webui::execute(
            self.db.clone(),
            self.controls.clone(),
            workspace,
            run.spec.session.clone(),
            run.spec.request.clone(),
            run.spec.input["messages"]
                .as_array()
                .cloned()
                .unwrap_or_default(),
            cancel.clone(),
            run.spec.input["max_model_calls"].as_u64().unwrap_or(128) as u32,
        ));
        drop(activity);
        crate::ui_persistence::publish(&self.db,configuration,&run.spec.session,&run.spec.turn).map_err(runtime_error)?;
        crate::native_channels::deliver_run(&self.db, &run.spec, &cancel)
            .map_err(|(_, e)| runtime_error(e))?;
        self.inspect(run)?
            .ok_or_else(|| runtime_error("Run finished without a committed result"))
    }

    fn inspect(&self, run: &RunRecord) -> kageha_contracts::Result<Option<Completion>> {
        let db = self.db.lock().map_err(runtime_error)?;
        let receipt: Option<(String, String)> = db
            .query_row(
                "SELECT status,result FROM ui_runtime_results WHERE turn=?",
                [&run.spec.turn],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(runtime_error)?;
        if receipt.is_none() {
            if let Ok(Some(client)) = kageha_runtime_service::bridge_client::Client::from_env() {
                if let Ok(remote) = client.status(&run.spec.turn) {
                    if let Some(completion) = remote_completion(&remote, run) {
                        return Ok(Some(completion));
                    }
                    if matches!(
                        remote["state"].as_str(),
                        Some("running" | "queued" | "staged" | "cancelling")
                    ) {
                        let _ = client.cancel(&run.spec.turn);
                    }
                }
            }
        }
        let (status, mut result) = if let Some((status, result)) = receipt {
            (
                status,
                serde_json::from_str::<Value>(&result).map_err(runtime_error)?,
            )
        } else {
            let legacy: Option<(String, Option<String>)> = db
                .query_row(
                    "SELECT status,answer FROM native_chat_turns WHERE chat_id=? AND request_id=?",
                    params![run.spec.session, run.spec.request],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(runtime_error)?;
            let Some((status, answer)) = legacy else {
                return Ok(None);
            };
            let configuration: Option<String> = db
                .query_row(
                    "SELECT spec FROM ui_run_specs WHERE chat_id=? AND request_id=?",
                    params![run.spec.session, run.spec.request],
                    |r| r.get(0),
                )
                .optional()
                .map_err(runtime_error)?;
            let configuration: Value = configuration
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or(Value::Null);
            (
                status,
                json!({"message":answer.unwrap_or_default(),"speaker":{"id":configuration["persona"]["id"],"name":configuration["persona"]["name"],"avatar":configuration["persona"]["avatar"]},"artifacts":[],"turn_artifacts":[]}),
            )
        };
        let mut state = match status.as_str() {
            "completed" => RunState::Succeeded,
            "failed" | "unverified" => RunState::Failed,
            "cancelled" => RunState::Cancelled,
            _ => return Ok(None),
        };
        if state == RunState::Succeeded && !run.spec.input["delivery"].is_null() {
            let effect = self.store.effect_get(&run.spec.turn, "channel-reply")?;
            let delivery = match effect {
                Some(e) if e.receipt.is_some() => "confirmed",
                Some(_) => "uncertain",
                None => "not_dispatched",
            };
            result["delivery_status"] = json!(delivery);
            if delivery != "confirmed" {
                state = RunState::Interrupted;
                result["interrupted"] = json!(true);
            }
        }
        result["job_reply_id"] = json!(format!("job:{}:result", run.spec.id));
        result["chat_reply"] = json!({"attachments":[]});
        result["native_status"] = json!(status);
        Ok(Some(Completion { state, result }))
    }
    fn publish(&self, run: &RunRecord) -> kageha_contracts::Result<()> {
        let workspace_result=crate::ui_job_worktrees::settle(&self.db,run).unwrap_or_else(|e|Some(json!({"mode":"worktree","retained":true,"error":e.to_string()})));
        let steering = self.store.steering_history(&run.spec.id)?;
        let mut db = self.db.lock().map_err(runtime_error)?;
        let tx = db.transaction().map_err(runtime_error)?;
        for message in steering {
            tx.execute(
                "INSERT OR IGNORE INTO ui_runtime_steering VALUES (?,?,?,?)",
                params![run.spec.id, message.id, run.spec.turn, message.body],
            )
            .map_err(runtime_error)?;
        }
        let previous: Option<u64> = tx
            .query_row(
                "SELECT projected_revision FROM ui_runtime_runs WHERE id=?",
                [&run.spec.id],
                |r| r.get(0),
            )
            .optional()
            .map_err(runtime_error)?;
        if previous.is_some_and(|r| r >= run.revision) {
            return Ok(());
        }
        let state = match run.state {
            RunState::Staged | RunState::Queued => "queued",
            RunState::Running => "running",
            RunState::Cancelling => "cancelling",
            RunState::Interrupted => "interrupted",
            RunState::Succeeded => "succeeded",
            RunState::Cancelled => "cancelled",
            _ => "failed",
        };
        let result = run.completion.as_ref().map(|c| {
            let mut result = c.result.clone();
            if result.is_null() {
                result = json!({});
            }
            if let Some(workspace)=&workspace_result {result["workspace"]=workspace.clone();}
            if run.state == RunState::Interrupted {
                result["interrupted"] = json!(true);
            }
            result
        });
        let job_id = run.spec.input["configuration"]["job_id"]
            .as_str()
            .unwrap_or(&run.spec.id);
        let current: Option<String> = tx
            .query_row(
                "SELECT run_id FROM ui_job_current WHERE job_id=?",
                [job_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(runtime_error)?;
        if current.as_deref().unwrap_or(job_id) == run.spec.id {
            tx.execute("UPDATE ui_jobs SET state=?,revision=revision+1,result=COALESCE(?,result) WHERE id=?",params![state,result.as_ref().map(Value::to_string),job_id]).map_err(runtime_error)?;
            if run.state == RunState::Running {
                tx.execute("INSERT INTO ui_job_times(job_id,started,ended) VALUES (?,?,NULL) ON CONFLICT(job_id) DO UPDATE SET started=COALESCE(started,excluded.started),ended=NULL",params![job_id,crate::ui_store::now()]).map_err(runtime_error)?;
            }
            if run.state.terminal() {
                tx.execute("INSERT INTO ui_job_times(job_id,started,ended) VALUES (?,?,?) ON CONFLICT(job_id) DO UPDATE SET ended=COALESCE(ended,excluded.ended)",params![job_id,crate::ui_store::now(),crate::ui_store::now()]).map_err(runtime_error)?;
            }
            crate::ui_session_events::publish_worker(&tx, job_id, &run.spec.turn)
                .map_err(runtime_error)?;
        }

        let exists_session: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM ui_sessions WHERE id=?)",
                [&run.spec.session],
                |r| r.get(0),
            )
            .map_err(runtime_error)?;
        if run.state.terminal() && exists_session {
            let detail = result.clone().unwrap_or(json!({}));
            let native = match run.state {
                RunState::Succeeded => "completed",
                RunState::Cancelled => "cancelled",
                RunState::Interrupted => "interrupted",
                _ if detail["native_status"] == "unverified" => "unverified",
                _ => "failed",
            };
            tx.execute(
                "UPDATE native_chat_turns SET status=?,answer=COALESCE(answer,?) WHERE chat_id=? AND request_id=?",
                params![native,detail["message"].as_str().unwrap_or(""),run.spec.session,run.spec.request],
            )
            .map_err(runtime_error)?;
            crate::ui_approvals::cancel_turn(&tx, &run.spec.session, &run.spec.turn)
                .map_err(runtime_error)?;
            let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM ui_events WHERE chat_id=? AND turn_id=? AND event='done')",params![run.spec.session,run.spec.turn],|r|r.get(0)).map_err(runtime_error)?;
            if !exists {
                let message = detail["message"].as_str().unwrap_or("");
                let artifacts =
                    crate::native_media::turn_artifacts(&tx, &run.spec.session, &run.spec.turn)
                        .map_err(runtime_error)?;
                let failure_code = detail["failure_code"].as_str().or(match run.state {
                    RunState::Interrupted => Some("runtime_disconnected"),
                    RunState::Cancelled => Some("cancelled"),
                    _ => None,
                });
                webui::insert_event(&tx,&run.spec.session,&run.spec.turn,"done",json!({"run_id":run.spec.session,"status":if native=="unverified"{"unverified"}else if run.state==RunState::Succeeded{"success"}else if run.state==RunState::Cancelled{"cancelled"}else{"error"},"message":message,"speaker":detail["speaker"],"failure_cause":if run.state==RunState::Succeeded{""}else{message},"failure_code":failure_code,"failure_id":detail["failure_id"],"failure_detail":detail["failure_detail"],"artifacts":artifacts,"turn_artifacts":artifacts,"sources":[]})).map_err(runtime_error)?;
            }
        }
        if run.state.terminal() && exists_session
            && !crate::ui_run_changes::settle(&tx, run).map_err(runtime_error)? {
            crate::ui_goals::settle(&tx, run).map_err(runtime_error)?;
        }
        tx.execute(
            "UPDATE ui_runtime_runs SET projected_revision=? WHERE id=?",
            params![run.revision, run.spec.id],
        )
        .map_err(runtime_error)?;
        tx.commit().map_err(runtime_error)?;
        crate::ui_session_events::notify();
        Ok(())
    }
}

/// Consume only the current turn's durable steering at a model checkpoint.
pub(crate) fn steering(db: &Database, session: &str, turn: &str) -> ApiResult<Vec<String>> {
    let store = store(db)?;
    let Some(run) = store.find_turn(turn).map_err(api)? else {
        return Ok(Vec::new());
    };
    if run.spec.session != session {
        return Err((409, "Steering session mismatch".into()));
    }
    let messages = store.steering(&run.spec.id).map_err(api)?;
    for message in &messages {
        webui::emit(db, session, turn, "event", json!({"kind":"user_inject","payload":{"source_kind":"user_inject","message":message.body,"message_id":message.id}})).map_err(db_error)?;
    }
    Ok(messages.into_iter().map(|m| m.body).collect())
}

pub(crate) fn steering_projection(db: &Connection, turn: &str) -> rusqlite::Result<Vec<String>> {
    let mut q = db.prepare("SELECT body FROM ui_runtime_steering WHERE turn=? ORDER BY rowid")?;
    let rows = q.query_map([turn], |r| r.get(0))?.collect();
    rows
}

#[cfg(test)]
mod reliability_tests {
    use super::*;
    #[test]
    fn terminal_remote_failures_keep_their_cause() {
        let run: RunRecord = serde_json::from_value(json!({
            "spec":{"id":"run","session":"chat","turn":"turn","request":"request","parent":null,"parent_run":null,"agent":"main","objective":"test","depth":0,"input":{}},
            "state":"running","revision":1,"completion":null,"cancel_requested":false
        })).unwrap();
        for (state, result, expected) in [
            ("failed", json!({"message":"Execution deadline exceeded"}), "deadline_exceeded"),
            ("failed", json!({"message":"Unknown outcome","uncertain":true}), "tool_outcome_unconfirmed"),
            ("cancelled", json!({"message":"Execution cancelled"}), "cancelled"),
            ("failed", json!({"message":"Access revoked","failure_code":"authorization_required"}), "authorization_required"),
        ] {
            let completion = remote_completion(&json!({"state":state,"completion":{"result":result}}), &run).unwrap();
            assert_eq!(completion.result["failure_code"], expected);
            assert!(completion.state.terminal());
        }
        assert!(remote_completion(&json!({"state":"running"}), &run).is_none());
    }
}
