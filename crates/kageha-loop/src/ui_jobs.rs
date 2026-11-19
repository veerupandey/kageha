//! Durable specialist jobs. Native workers retain their own chat and cancellation scope.
use crate::{
    server, ui_agents,
    ui_store::{self as store, db_error, ApiResult},
    webui::{self, Controls, Database},
};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use std::path::Path;
use tiny_http::{Method, Request};

/// Optional presentation title; the objective remains the authoritative task.
fn worker_title(options: &Value) -> ApiResult<Option<String>> {
    let Some(value) = options.get("title").filter(|v| !v.is_null()) else { return Ok(None) };
    let title = value.as_str().ok_or((400, "Worker title must be text".into()))?.trim();
    if title.is_empty() || title.chars().count() > 80 || title.chars().any(char::is_control) {
        return Err((400, "Worker title must be 1–80 characters on one line".into()));
    }
    Ok(Some(title.to_owned()))
}

/// Review-only patches must stay in an isolated, retained local checkout.
fn review_patch_options(options: &mut Value, inherited: bool) -> ApiResult<bool> {
    let requested = match options.get("review_patch") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => return Err((400, "review_patch must be a boolean".into())),
    };
    let review = inherited || requested;
    if review {
        if options.get("isolation").is_some_and(|value| !value.is_null() && value != "worktree") {
            return Err((400, "A review-only patch requires a local worktree".into()));
        }
        if options.get("keep_worktree").is_some_and(|value| !value.is_null() && value != true) {
            return Err((400, "A review-only patch requires a retained worktree".into()));
        }
        options["isolation"] = json!("worktree");
        options["keep_worktree"] = json!(true);
        options["review_patch"] = json!(true);
    }
    Ok(review)
}

#[cfg(test)]
mod title_tests {
    use super::*;

    #[test]
    fn review_patch_is_retained_isolated_and_inherited() {
        let mut options=json!({"review_patch":true});
        assert!(review_patch_options(&mut options,false).unwrap());
        assert_eq!(options["isolation"], "worktree");
        assert_eq!(options["keep_worktree"], true);
        let mut child=json!({"review_patch":false});
        assert!(review_patch_options(&mut child,true).unwrap());
        assert_eq!(child["review_patch"], true);
        for mut options in [json!({"review_patch":true,"isolation":"shared"}),
            json!({"review_patch":true,"keep_worktree":false}),json!({"review_patch":"yes"})] {
            assert!(review_patch_options(&mut options,false).is_err());
        }
    }

    #[test]
    fn titles_are_optional_bounded_single_line_text() {
        assert_eq!(worker_title(&json!({})).unwrap(), None);
        assert_eq!(worker_title(&json!({"title":" Review tests "})).unwrap().as_deref(), Some("Review tests"));
        for invalid in [json!(""), json!("two\nlines"), json!("x".repeat(81)), json!(12)] {
            assert!(worker_title(&json!({"title":invalid})).is_err());
        }
    }
}

pub(crate) fn initialize(db: &rusqlite::Connection) -> rusqlite::Result<()> {
    crate::ui_job_controls::initialize(db)?;
    db.execute_batch("CREATE TABLE IF NOT EXISTS ui_jobs(id TEXT PRIMARY KEY,parent_chat TEXT NOT NULL,child_chat TEXT NOT NULL,agent_id TEXT NOT NULL,objective TEXT NOT NULL,state TEXT NOT NULL,revision INTEGER NOT NULL,created REAL NOT NULL,result TEXT,request TEXT NOT NULL,depth INTEGER NOT NULL);
 CREATE TABLE IF NOT EXISTS ui_job_messages(job_id TEXT NOT NULL,message_id TEXT NOT NULL,body TEXT NOT NULL,created REAL NOT NULL,delivered INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(job_id,message_id));")
}
pub(crate) fn spawn_configured(
    db:&Database,_controls:&Controls,workspace:&Path,parent:&str,agent_id:&str,
    objective:&str,request_id:&str,depth:u32,parent_turn:Option<&str>,options:&Value,
)->ApiResult<Value>{
    let mut options=options.clone();
    let _target_guard = crate::docker_desktop::lock(parent);
    if crate::docker_desktop::binding(&*db.lock().map_err(db_error)?, parent).map_err(db_error)?.is_some() {
        return Err((409,"Specialist dispatch is unavailable on the Docker workstation".into()));
    }
    let title=worker_title(&options)?;
    if let Some(root)=options.get("project_root").filter(|v|!v.is_null()){
        let root=root.as_str().ok_or((400,"Worker project_root must be text".into()))?;
        if Path::new(root).canonicalize().ok()!=workspace.canonicalize().ok(){return Err((403,"Worker project_root is outside this workspace".into()));}
    }
    if depth > 3 {
        return Err((409, "Specialist nesting is limited to three levels".into()));
    }
    if objective.trim().is_empty() || objective.len() > 65536 {
        return Err((400, "A bounded objective is required".into()));
    }
    let runtime = crate::runtime_host::get(db)?;
    let parent_run = parent_turn
        .map(|turn| {
            runtime
                .supervisor
                .store
                .find_turn(turn)
                .map_err(|e| (409, e.message))
                .and_then(|r| {
                    r.filter(|r| r.spec.session == parent)
                        .map(|r| r.spec.id)
                        .ok_or((409, "Parent runtime scope not found".into()))
                })
        })
        .transpose()?;
    let mut persona = ui_agents::agent(db, agent_id)?;
    if options.get("team_persona").is_some_and(|pinned| pinned != &persona) {
        return Err((409, "Team member profile changed since the team was admitted".into()));
    }
    let mut inherited_knowledge=None;
    let mut inherited_project=None;
    let mut inherited_hooks=None;
    let mut inherited_persistence=None;
    let mut inherited_review_patch=false;
    // The parent's Run_Journal request, so the worker's entries join it even when request
    // persistence is off and there is no budget id to inherit.
    let parent_request=parent_turn.map(|turn|crate::ui_learning::request_key(db,turn));
    if let Some(turn)=parent_turn {
        let run=runtime.supervisor.store.find_turn(turn).map_err(|e|(409,e.message))?
            .filter(|r|r.spec.session==parent).ok_or((409,"Parent policy scope not found".into()))?;
        inherited_review_patch = run.spec.input["configuration"]["review_patch"] == true;
        let parent_workspace=kageha_workspace::worker::Options::parse(&run.spec.input["configuration"]["worker_workspace"]).map_err(db_error)?;
        if parent_workspace.isolated(){
            if options.get("isolation").is_none(){options["isolation"]=json!("worktree");}
            else if !kageha_workspace::worker::Options::parse(&options).map_err(|e|(400,e.to_string()))?.isolated(){return Err((409,"An isolated specialist cannot delegate into the shared source workspace".into()));}
        }
        inherited_hooks=Some(run.spec.input["configuration"]["project_hooks"].clone());
        inherited_persistence=Some((run.spec.input["configuration"]["persistence"].clone(),run.spec.input["configuration"]["request_budget_id"].clone()));
        inherited_project=Some(run.spec.input["configuration"]["project_instructions"].as_str().unwrap_or("").to_owned());
        let parent_scope=crate::ui_knowledge::pinned(workspace,&run.spec.input["configuration"]["persona"],&run.spec.input["configuration"]["knowledge_scope"]).map_err(db_error)?;
        inherited_knowledge=Some(parent_scope.delegate(agent_id,&kageha_knowledge::scope::Config::parse(&persona["knowledge"]).map_err(db_error)?));
        kageha_policy::specialist::inherit(&mut persona,&run.spec.input["configuration"]["persona"])
            .map_err(|e|(409,e.to_string()))?;
        // A deterministic team fan-out gives each member the same user request.
        // Restrict these workers at the policy boundary so parallel or sequential
        // members cannot repeat an external mutation. The coordinator remains
        // responsible for any authorized writes after reviewing the reports.
        if options["team_read_only"] == true {
            let inherited = persona["_inherited_tool_policies"].as_array_mut()
                .ok_or((409, "Could not apply team worker policy".into()))?;
            inherited.push(json!({"allow":["read_file","view_image","list_workspace_files",
                "web_search","news_search","web_fetch","knowledge_search","knowledge_list",
                "search_past_chats","find_past_files","skill_read"]}));
        }
    }
    let review_patch = review_patch_options(&mut options, inherited_review_patch)?;
    let id = webui::turn_id(parent, &format!("worker:{request_id}"));
    let child = format!("job-{}", &id[..24]);
    let mut connection = db.lock().map_err(db_error)?;
    if let Some(prior) = job(&connection, &id).map_err(db_error)? {
        if prior["objective"] != objective || prior["spec"]["persona_id"] != agent_id {
            return Err((
                409,
                "Worker request id was reused for a different objective".into(),
            ));
        }
        let old: String=connection.query_row("SELECT spec FROM ui_run_specs WHERE chat_id=? AND request_id='job'",[&child],|r|r.get(0)).map_err(db_error)?;
        let old:Value=serde_json::from_str(&old).map_err(db_error)?;
        if old["worker_title"].as_str()!=title.as_deref(){return Err((409,"Worker request ID was reused with a different title".into()));}
        if (old["review_patch"] == true) != review_patch {return Err((409,"Worker request ID was reused with a different review requirement".into()));}
        let old_options=kageha_workspace::worker::Options::parse(&old["worker_workspace"]).map_err(db_error)?;
        let requested=kageha_workspace::worker::Options::parse(&options).map_err(|e|(400,e.to_string()))?;
        if old_options!=requested{return Err((409,"Worker request ID was reused with different workspace isolation".into()));}
        let prepared: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM ui_runtime_runs WHERE id=?)",
                [&id],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        drop(connection);
        if prepared {
            {
                let c = db.lock().map_err(db_error)?;
                crate::ui_approvals::owned(&c)
                    .map_err(db_error)?
                    .delegate(parent, &child)
                    .map_err(db_error)?;
            }
            runtime.submit(&id)?;
            runtime.start()?;
        }
        return get(db, &id);
    }
    let knowledge_scope=match inherited_knowledge {
        Some(v)=>v,None=>crate::ui_knowledge::resolve_connection(&connection,workspace,&persona,&Value::Null).map_err(db_error)?,
    };
    let parent_exists: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM ui_sessions WHERE id=?)",
            [parent],
            |r| r.get(0),
        )
        .map_err(db_error)?;
    if !parent_exists {
        return Err((404, "Parent chat not found".into()));
    }
    let remote = store::get(&connection, "remote-session", parent)
        .map_err(db_error)?
        .filter(|v| v["active"] == true)
        .and_then(|v| v["host_id"].as_str().map(str::to_owned))
        .and_then(|id| {
            store::get(&connection, "config:remote:hosts", &id)
                .ok()
                .flatten()
        })
        .unwrap_or(Value::Null);
    let worker_workspace=crate::ui_job_worktrees::validate(workspace,&options,!remote.is_null())?;
    let browser_decisions = store::get(&connection, "setting", "decisions")
        .map_err(db_error)?
        .unwrap_or(json!({"mode":"off"}));
    let tx = connection.transaction().map_err(db_error)?;
    tx.execute(
        "INSERT INTO ui_sessions(id,title,created_at,updated_at) VALUES (?,?,?,?)",
        params![
            child,
            format!(
                "{} · {objective}",
                persona["name"].as_str().unwrap_or(agent_id)
            )
            .chars()
            .take(200)
            .collect::<String>(),
            store::now(),
            store::now()
        ],
    )
    .map_err(db_error)?;
    tx.execute(
        "INSERT OR IGNORE INTO ui_internal_sessions VALUES (?)",
        [&child],
    )
    .map_err(db_error)?;
    tx.execute("INSERT INTO native_chat_turns(chat_id,request_id,message,status) VALUES (?,'job',?,'running')",params![child,objective]).map_err(db_error)?;
    tx.execute(
        "INSERT INTO ui_turn_uploads VALUES (?,'job','[]')",
        [&child],
    )
    .map_err(db_error)?;
    let project_instructions=match inherited_project{Some(v)=>v,None=>crate::ui_projects::instructions(workspace,"").map_err(db_error)?};
    let project_hooks=match inherited_hooks{Some(v)=>v,None=>crate::ui_hooks::capture_connection(&tx,workspace).map_err(db_error)?};
    let mut configuration = json!({"worker_workspace":worker_workspace,"project_hooks":project_hooks,"project_instructions":project_instructions,"agent_id":agent_id,"persona":persona,"knowledge_scope":knowledge_scope,"model":kageha_agents::model_roles::chain(&persona["model_preferences"]["executor"]).unwrap_or_default().first(),"agent_mode":"normal","depth":depth,"parent_session":parent,"delegation_source":if parent_turn.is_none(){"operator"}else{"agent"},"remote":remote,"browser_decisions":browser_decisions});
    if review_patch {configuration["review_patch"]=json!(true);}
    if let Some(title)=&title {configuration["worker_title"]=json!(title);}
    if let Some(request)=&parent_request {configuration["parent_request_id"]=json!(request);}
    if let Some((policy,budget))=inherited_persistence {
        configuration["persistence"]=policy;
        configuration["request_budget_id"]=budget;
    } else if parent_turn.is_none() {
        crate::ui_persistence::admit(&tx,&id,&mut configuration).map_err(db_error)?;
    }
    tx.execute(
        "INSERT INTO ui_run_specs(chat_id,request_id,spec) VALUES (?,'job',?)",
        params![child, configuration.to_string()],
    )
    .map_err(db_error)?;
    tx.execute(
        "INSERT INTO ui_jobs VALUES (?,?,?,?,?,'queued',1,?,NULL,?,?)",
        params![
            id,
            parent,
            child,
            agent_id,
            objective,
            store::now(),
            request_id,
            depth
        ],
    )
    .map_err(db_error)?;
    let run = crate::runtime_host::spec(
        workspace,
        &child,
        "job",
        &id,
        agent_id,
        objective,
        depth,
        Some(parent),
        parent_run,
        vec![json!({"role":"user","content":objective})],
        configuration,
        128,
    );
    crate::runtime_host::prepare(&tx, &run).map_err(db_error)?;
    tx.commit().map_err(db_error)?;
    drop(connection);
    {
        let c = db.lock().map_err(db_error)?;
        crate::ui_approvals::owned(&c)
            .map_err(db_error)?
            .delegate(parent, &child)
            .map_err(db_error)?;
    }
    runtime.submit(&id)?;
    runtime.start()?;
    get(db, &id)
}

pub(crate) fn job(db: &rusqlite::Connection, id: &str) -> rusqlite::Result<Option<Value>> {
    db.query_row("SELECT id,parent_chat,child_chat,agent_id,objective,state,revision,created,result,depth FROM ui_jobs WHERE id=?",[id],|r|{
        let id:String=r.get(0)?; let parent:String=r.get(1)?; let child:String=r.get(2)?; let agent:String=r.get(3)?;
        let objective:String=r.get(4)?; let state:String=r.get(5)?; let created:f64=r.get(7)?;
        let answer:Option<String>=r.get(8)?; let result=answer.and_then(|s|serde_json::from_str::<Value>(&s).ok()).unwrap_or(json!({}));
        let (run_id,_,worker_turn)=crate::ui_job_controls::attempt(db,&id,&child)?;
        let steps:i64=db.query_row("SELECT count(DISTINCT json_extract(payload,'$.id')) FROM ui_events WHERE chat_id=? AND turn_id=? AND event='tool_card'",params![child,worker_turn],|r|r.get(0))?;
        let last_tool:Option<String>=db.query_row("SELECT payload FROM ui_events WHERE chat_id=? AND turn_id=? AND event='tool_card' ORDER BY sequence DESC LIMIT 1",params![child,worker_turn],|r|r.get(0)).optional()?;
        let last_tool=last_tool.and_then(|s|serde_json::from_str::<Value>(&s).ok()).unwrap_or(Value::Null);
        let active=matches!(state.as_str(),"running"|"queued"|"cancelling");
        let current_tool=if active&&matches!(last_tool["status"].as_str(),Some("running"|"pending")){json!({"id":last_tool["id"],"name":last_tool["name"],"label":if last_tool["name"].as_str().unwrap_or("").starts_with("mcp_"){ "Integration tool".to_owned() }else{last_tool["name"].as_str().unwrap_or("Tool").replace('_'," ")},"status":last_tool["status"],"started_at":last_tool["started_at"]})}else{Value::Null};
        let pending=crate::ui_session_events::worker_pending(db,&child)?.map(|value|json!({"question":value["detail"],"approval_id":value["approval_id"]}));
        let parent_job:Option<String>=db.query_row("SELECT id FROM ui_jobs WHERE child_chat=? LIMIT 1",[&parent],|r|r.get(0)).optional()?;
        let mut root=parent.clone(); for _ in 0..4 {let next:Option<String>=db.query_row("SELECT parent_chat FROM ui_jobs WHERE child_chat=? LIMIT 1",[&root],|r|r.get(0)).optional()?;if let Some(next)=next{if next==root{break}root=next}else{break}}
        let times:Option<(Option<f64>,Option<f64>)>=db.query_row("SELECT started,ended FROM ui_job_times WHERE job_id=?",[&id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let started=times.and_then(|t|t.0).unwrap_or(created); let ended=times.and_then(|t|t.1);
        let mut resume=crate::ui_job_controls::resume_state(db,&worker_turn,&state)?;
        if result["uncertain"]==true {resume["allowed"]=json!(false);resume["reason"]=json!("An action has an unconfirmed outcome. Inspect its original receipt before continuing.");}
        let worker_workspace=crate::ui_job_worktrees::describe(db,&run_id)?;
        let title:Option<String>=db.query_row("SELECT json_extract(spec,'$.worker_title') FROM ui_run_specs WHERE chat_id=? AND request_id='job'",[&child],|r|r.get(0)).optional()?.flatten();
        let review_patch:bool=db.query_row("SELECT COALESCE(json_extract(spec,'$.review_patch'),0) FROM ui_run_specs WHERE chat_id=? AND request_id='job'",[&child],|r|r.get(0)).optional()?.unwrap_or(false);
        let label=title.clone().unwrap_or_else(||objective.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(80).collect());
        // Only paths registered to this worker's turns are discoverable. The actual
        // handoff revalidates each pinned file and its current bytes before use.
        let have_artifacts:bool=db.query_row("SELECT count(*)=2 FROM sqlite_master WHERE type='table' AND name IN ('ui_turn_artifacts','ui_uploads')",[],|r|r.get(0))?;
        let artifacts:Vec<String>=if have_artifacts {
            let mut q=db.prepare("SELECT DISTINCT a.path FROM ui_turn_artifacts a JOIN ui_uploads u ON u.chat_id=a.chat_id AND u.path=a.path WHERE a.chat_id=? ORDER BY a.path LIMIT 128")?;
            let paths=q.query_map([&child],|r|r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            paths
        } else {Vec::new()};
        if resume["allowed"]==true && worker_workspace["mode"]=="worktree" && worker_workspace["ready_for_input"]!=true {resume["allowed"]=json!(false);resume["reason"]=json!("The isolated checkout is missing, unverified or has entered cleanup. It cannot be silently recreated.");}
        Ok(json!({"id":id,"label":label,"title":title,"kind":"agent","parent_session":parent,"parent_job_id":parent_job,"root_session_id":root,"depth":r.get::<_,u32>(9)?,"session_id":child,"objective":objective,
            "workspace":worker_workspace,"run_id":run_id,"state":if pending.is_some(){"waiting_input"}else{&state},"revision":r.get::<_,i64>(6)?,"created_at":created,"started_at":started,"ended_at":ended,
            "can_cancel":matches!(state.as_str(),"queued"|"running"|"cancelling"),"can_resume":resume["allowed"],"resume_blocker":resume["reason"],"resume":resume,
            "controls":{"inspect":true,"steer":active&&state!="cancelling","cancel":active,"approve":pending.is_some(),"resume":resume["allowed"]},
            "attachable":true,"artifacts":artifacts,"handoff":crate::ui_handoff::latest(db,&id)?,"result":result,"error":{"approval_id":pending.as_ref().map(|v|v["approval_id"].clone()),"code":if state=="interrupted"||result["interrupted"]==true{"worker_interrupted"}else{""}},
            "question":pending,"verification":{"status":"not_verified"},"spec":{"persona_id":agent,"depth":r.get::<_,u32>(9)?,"isolation":worker_workspace["options"]["isolation"],"keep_worktree":worker_workspace["options"]["keep_worktree"],"review_patch":review_patch},
            "progress":{"label":state,"steps":steps,"updated_at":store::now(),"turn_id":worker_turn,"current_tool":current_tool,"last_tool":last_tool,"elapsed_s":(ended.unwrap_or_else(store::now)-started).max(0.0)}}))
    }).optional()
}
pub(crate) fn get(db: &Database, id: &str) -> ApiResult<Value> {
    crate::runtime_host::get(db)?
        .supervisor
        .flush()
        .map_err(|e| (500, e.message))?;
    job(&*db.lock().map_err(db_error)?, id)
        .map_err(db_error)?
        .ok_or((404, "Worker not found".into()))
}
pub(crate) fn cancel(db: &Database, _controls: &Controls, id: &str) -> ApiResult<Value> {
    let row = get(db, id)?;
    cancel_run(db, id, row["run_id"].as_str().unwrap_or(id))
}
fn cancel_run(db:&Database,id:&str,run_id:&str)->ApiResult<Value>{
 let runtime=crate::runtime_host::get(db)?;
 let scope:Option<(String,String)>=db.lock().map_err(db_error)?.query_row("SELECT json_extract(spec,'$.session'),json_extract(spec,'$.turn') FROM ui_runtime_runs WHERE id=?",[run_id],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(db_error)?;
 let (session,turn)=scope.ok_or((404,"Worker run not found".into()))?;
 let cancelled=runtime.cancel_turn(&session,&turn)?;
 Ok(json!({"ok":true,"job_id":id,"cancelled":cancelled}))
}
pub(crate) fn handle(
    mut request: Request,
    db: &Database,
    controls: &Controls,
    workspace: &Path,
) -> std::io::Result<()> {
    crate::runtime_host::get(db)
        .and_then(|host| host.supervisor.flush().map_err(|e| (500, e.message)))
        .map_err(|(_, e)| std::io::Error::other(e))?;
    let url = request.url().to_owned();
    let path = url.split('?').next().unwrap_or("");
    let parts: Vec<_> = path.split('/').collect();
    let method = request.method().clone();
    let payload = if method == Method::Post {
        webui::body(&mut request).map_err(|_| (400, "Invalid JSON".into()))
    } else {
        Ok(json!({}))
    };
    let result=payload.and_then(|payload|->ApiResult<Value>{
  if parts.len()==3&&method==Method::Post {return spawn_configured(db,controls,workspace,payload["session_id"].as_str().unwrap_or(""),payload["agent_id"].as_str().or_else(||payload["persona_id"].as_str()).unwrap_or("main"),payload["objective"].as_str().or_else(||payload["message"].as_str()).unwrap_or(""),payload["client_request_id"].as_str().unwrap_or(&crate::browser_client::unique_id()),1,None,&payload)}
  if parts.len()==3&&method==Method::Get {
   let parent=webui::form_decode(&webui::query(&url,"session_id")).unwrap_or_default();let connection=db.lock().map_err(db_error)?;
   if !parent.is_empty(){return Ok(json!({"jobs":crate::ui_session_events::workers(&connection,&parent).map_err(db_error)?}))}
   let mut q=connection.prepare("SELECT id FROM ui_jobs ORDER BY created DESC LIMIT 1000").map_err(db_error)?;
   let ids=q.query_map([],|r|r.get::<_,String>(0)).map_err(db_error)?.collect::<Result<Vec<_>,_>>().map_err(db_error)?;
   let rows=ids.iter().map(|id|job(&connection,id)).collect::<Result<Vec<_>,_>>().map_err(db_error)?.into_iter().flatten().collect::<Vec<_>>();return Ok(json!({"jobs":rows}));
  }
  let id=parts.get(3).copied().unwrap_or("");
  if id=="events"{return Ok(json!({"events":[],"cursor":0}))}

  let row=get(db,id)?;
  if parts.len()>4&&method!=Method::Post{return Err((405,"Agent controls require POST".into()))}
  if parts.get(4)!=Some(&"resume"){crate::ui_job_controls::expected(&payload,&row)?;}
  if parts.get(4)==Some(&"cancel"){return cancel_run(db,id,row["run_id"].as_str().unwrap_or(id))}
  if parts.get(4)==Some(&"resume"){return crate::ui_job_controls::resume(db,controls,workspace,id,&payload)}
  if parts.get(4)==Some(&"resolve") {
   if payload["retry"]==true {return crate::ui_job_controls::resume(db,controls,workspace,id,&payload)}
   cancel_run(db,id,row["run_id"].as_str().unwrap_or(id))?;return Ok(json!({"ok":true,"resolved":true}))
  }
  if parts.get(4)==Some(&"attach"){return Ok(json!({"job":row,"session_id":row["session_id"],"thread_id":format!("web-{}",row["session_id"].as_str().unwrap()),"turn_id":row["progress"]["turn_id"]}))}
  if parts.get(4)==Some(&"approve") {
   if payload["approval_id"].as_str().is_none_or(str::is_empty)||payload["approval_id"]!=row["question"]["approval_id"]{return Err((409,"Pending approval changed. Refresh and decide the exact request.".into()))}
   let approved=payload.clone();
   return crate::ui_approvals::decide(db,&approved);
  }
  if parts.get(4)==Some(&"message") {
   if !matches!(row["state"].as_str(),Some("running"|"queued"|"waiting_input")){return Err((409,"Worker is no longer running".into()))}
   let body=store::text(&payload,"message",16384)?;let idempotency=payload["message_id"].as_str().filter(|s|server::identifier(s)).map(str::to_owned).unwrap_or_else(crate::browser_client::unique_id);
   crate::runtime_host::get(db)?.supervisor.store.steer(row["run_id"].as_str().unwrap_or(id),&kageha_runtime::lifecycle::Steering{id:idempotency.clone(),body:body.into()}).map_err(|e|(409,e.message))?;
   return Ok(json!({"ok":true,"message_id":idempotency,"queued":true}));
  }
  Ok(json!({"job":row}))
 });
    match result {
        Ok(value) => server::response(request, 200, value),
        Err((code, error)) => server::response(request, code, json!({"error":error})),
    }
    Ok(())
}

pub(crate) fn cancel_children(db: &Database, _controls: &Controls, parent: &str) {
    if let Ok(host) = crate::runtime_host::get(db) {
        if let Ok(runs) = host.supervisor.store.active() {
            for run in runs {
                if run.spec.session == parent && !run.state.terminal() {
                    let _ = host.supervisor.cancel(&run.spec.id);
                }
            }
        }
    }
}
