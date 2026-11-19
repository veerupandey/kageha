//! Explicit, durable session close; archive and deletion remain independent.
use crate::{ui_store,webui::{self,Controls,Database}};
use serde_json::{json,Value};
use kageha_runtime::lifecycle::RunStore;
use std::{collections::HashSet,io,path::Path,sync::{Arc,Mutex,OnceLock},sync::atomic::{AtomicBool,Ordering}};

static RUNNING:OnceLock<Mutex<HashSet<String>>>=OnceLock::new();
fn running()->&'static Mutex<HashSet<String>> {RUNNING.get_or_init(Default::default)}
fn error(s:&str)->(u16,String){(409,s.into())}
fn lock_error()->(u16,String){(500,"Session lifecycle unavailable".into())}

/// Public projection excludes captured hook bytes and workspace internals.
pub(crate) fn status(conn:&rusqlite::Connection,chat:&str)->rusqlite::Result<Value> {
    let Some(row)=ui_store::get(conn,"session-lifecycle",chat)? else {return Ok(json!({"status":"open"}));};
    let mut state=row["status"].as_str().unwrap_or("open");
    if state=="closing" && !running().lock().map(|v|v.contains(row["turn_id"].as_str().unwrap_or(""))).unwrap_or(false) {state="interrupted";}
    Ok(json!({"status":state,"turn_id":row["turn_id"],"error":row["error"],"archive":row["archive"]==true}))
}

pub(crate) fn close(db:&Database,controls:&Controls,workspace:&Path,chat:&str)->crate::ui_store::ApiResult<Value> {
    close_with_intent(db,controls,workspace,chat,false)
}
pub(crate) fn archive(db:&Database,controls:&Controls,workspace:&Path,chat:&str)->crate::ui_store::ApiResult<Value> {
    close_with_intent(db,controls,workspace,chat,true)
}
fn close_with_intent(db:&Database,controls:&Controls,workspace:&Path,chat:&str,archive:bool)->crate::ui_store::ApiResult<Value> {
    let runtime=crate::runtime_host::store(db)?;
    let mut conn=db.lock().map_err(|_|lock_error())?;
    let tx=conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(ui_store::db_error)?;
    let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM ui_sessions WHERE id=?)",[chat],|r|r.get(0)).map_err(ui_store::db_error)?;
    if !exists{return Err((404,"Session not found".into()));}
    let remote=ui_store::get(&tx,"remote-session",chat).map_err(ui_store::db_error)?.is_some_and(|v|v["active"]==true);
    let worker:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM ui_jobs WHERE child_chat=?)",[chat],|r|r.get(0)).map_err(ui_store::db_error)?;
    let current=ui_store::get(&tx,"session-lifecycle",chat).map_err(ui_store::db_error)?;
    if current.as_ref().is_some_and(|v|v["status"]=="closed") {
        if archive {
            tx.execute("INSERT INTO ui_session_flags(chat_id,pinned,archived) VALUES (?,0,1) ON CONFLICT(chat_id) DO UPDATE SET archived=1",[chat]).map_err(ui_store::db_error)?;
            tx.commit().map_err(ui_store::db_error)?;
        }
        return Ok(json!({"status":"closed","turn_id":current.unwrap()["turn_id"]}));
    }
    if current.as_ref().is_some_and(|r|matches!(r["status"].as_str(),Some("closing"|"failed")) && (r["archive"]==true)!=archive) {return Err(error("Finish or reopen the existing session close first"));}
    if let Some(row)=current.as_ref().filter(|r|r["status"]=="closing") {
        if running().lock().map_err(|_|lock_error())?.contains(row["turn_id"].as_str().unwrap_or("")) {return Ok(json!({"status":"closing","turn_id":row["turn_id"],"archive":row["archive"]==true}));}
    }
    let busy:bool=tx.query_row(&format!("SELECT EXISTS(SELECT 1 FROM native_chat_turns WHERE chat_id=? AND status IN {})",crate::ui_turn_resume::LIVE_STATUSES),[chat],|r|r.get(0)).map_err(ui_store::db_error)?;
    let children:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM ui_jobs WHERE parent_chat=? AND state IN ('queued','running'))",[chat],|r|r.get(0)).map_err(ui_store::db_error)?;
    let pending:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM ui_pending_admissions WHERE chat_id=?)",[chat],|r|r.get(0)).map_err(ui_store::db_error)?;
    let active=runtime.active().map_err(|e|error(&e.message))?.iter().any(|run|run.spec.session==chat);
    if busy || children || pending || active {return Err(error("Stop active turns and workers before closing the session"));}
    let row=if let Some(mut row)=current.filter(|r|matches!(r["status"].as_str(),Some("closing"|"failed"))) {row["status"]=json!("closing");row["error"]=Value::Null;row}
    else {
        let request=format!("session-close-{}",crate::browser_client::unique_id());
        let turn=webui::turn_id(chat,&request);
        let hooks=crate::ui_hooks::capture_connection(&tx,workspace).map_err(|e|error(&e.to_string()))?;
        json!({"status":"closing","turn_id":turn,"hooks":hooks,"workspace":workspace,"archive":archive,"error":null})
    };
    let snapshot_hooks: kageha_workspace::hooks::Set=serde_json::from_value(row["hooks"].clone()).map_err(|_|error("Invalid close hook snapshot"))?;
    if (remote || worker) && snapshot_hooks.hooks.iter().any(|hook|hook.event=="sessionEnd") {
        return Err(error("SessionEnd hooks require the original remote or delegated workspace; close there"));
    }
    let turn=row["turn_id"].as_str().ok_or_else(||error("Invalid close state"))?.to_owned();
    let snapshot=row["hooks"].clone();
    let root=row["workspace"].as_str().ok_or_else(||error("Invalid close workspace"))?.to_owned();
    let archive=row["archive"]==true;
    ui_store::put(&tx,"session-lifecycle",chat,row).map_err(ui_store::db_error)?;
    let flag=Arc::new(AtomicBool::new(false));
    {
        let mut active=running().lock().map_err(|_|lock_error())?;
        let mut entries=controls.lock().map_err(|_|lock_error())?;
        active.insert(turn.clone());entries.insert(turn.clone(),flag.clone());
    }
    if let Err(e)=tx.commit() {running().lock().ok().map(|mut v|v.remove(&turn));controls.lock().ok().map(|mut v|v.remove(&turn));return Err(ui_store::db_error(e));}
    drop(conn);
    let cleanup_controls=controls.clone();
    let (db,controls,chat,worker_turn)=(db.clone(),controls.clone(),chat.to_owned(),turn.clone());
    let spawn=std::thread::Builder::new().name("session-close".into()).spawn(move || {
        let result=(||->io::Result<()> {
            let hooks:kageha_workspace::hooks::Set=serde_json::from_value(snapshot).map_err(io::Error::other)?;
            let outcome=crate::ui_hooks::run(&db,&chat,&worker_turn,Path::new(&root),&hooks,"sessionEnd",
                &json!({"id":"lifecycle-sessionEnd","name":"session","arguments":{},"reason":"other","end_trigger":if archive {"archive"}else{"user_close"}}),&flag)?;
            if outcome.warning.is_some() {return Err(io::Error::other("SessionEnd hook did not complete successfully"));}
            if flag.load(Ordering::Acquire) {return Err(io::Error::new(io::ErrorKind::Interrupted,"Session close cancelled"));}
            Ok(())
        })();
        let _=webui::emit(&db,&chat,&worker_turn,"event",json!({"kind":"session_close_finished"}));
        if let Ok(conn)=db.lock() {
            if let Ok(tx)=conn.unchecked_transaction() {
                if let Ok(Some(mut state))=ui_store::get(&tx,"session-lifecycle",&chat) {
                    if state["status"]=="closing" && state["turn_id"]==worker_turn {
                        let success=result.is_ok();
                        state["status"]=json!(if success {"closed"}else{"failed"});
                        state["error"]=result.err().map(|e|e.to_string().chars().take(500).collect::<String>()).map(Value::String).unwrap_or(Value::Null);
                        if (!success || !archive || tx.execute("INSERT INTO ui_session_flags(chat_id,pinned,archived) VALUES (?,0,1) ON CONFLICT(chat_id) DO UPDATE SET archived=1",[&chat]).is_ok()) && ui_store::put(&tx,"session-lifecycle",&chat,state).is_ok() {let _=tx.commit();}
                    }
                }
            }
        }
        if let Ok(mut items)=running().lock(){items.remove(&worker_turn);}
        if let Ok(mut items)=controls.lock(){items.remove(&worker_turn);}
    });
    if spawn.is_err(){running().lock().ok().map(|mut v|v.remove(&turn));cleanup_controls.lock().ok().map(|mut v|v.remove(&turn));return Err(error("Could not start session close"));}
    Ok(json!({"status":"closing","turn_id":turn,"archive":archive}))
}
pub(crate) fn reopen(db:&Database,chat:&str)->crate::ui_store::ApiResult<Value> {
    let mut conn=db.lock().map_err(|_|lock_error())?;
    let tx=conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(ui_store::db_error)?;
    let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM ui_sessions WHERE id=?)",[chat],|r|r.get(0)).map_err(ui_store::db_error)?;
    if !exists{return Err((404,"Session not found".into()));}
    let state=status(&tx,chat).map_err(ui_store::db_error)?;
    reopen_in(&tx, chat, &state)?;
    tx.commit().map_err(ui_store::db_error)?;
    Ok(json!({"status":"open"}))
}

pub(crate) fn reopen_in(tx: &rusqlite::Transaction<'_>, chat: &str, state: &Value) -> crate::ui_store::ApiResult<()> {
    if state["status"] == "closing" {
        return Err(error("Session close is still running"));
    }
    ui_store::put(tx, "session-lifecycle", chat, json!({"status":"open","last_close_turn":state["turn_id"]}))
        .map_err(ui_store::db_error)?;
    Ok(())
}
