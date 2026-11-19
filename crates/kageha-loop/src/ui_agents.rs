//! Specialist definitions, revisions and teams using the existing WebUI API.
use crate::{
    server,
    ui_store::{self as store, db_error, text, ApiResult},
    webui::{self, Database},
};
use rusqlite::params;
use serde_json::{json, Value};
use tiny_http::{Method, Request};
pub(crate) fn initialize(db: &rusqlite::Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS ui_agent_source_revisions(id TEXT NOT NULL,source_revision TEXT NOT NULL,native_revision INTEGER NOT NULL,PRIMARY KEY(id,source_revision),UNIQUE(id,native_revision));")?;
    if store::get(db, "agent", "main")?.is_none() {
        store::put(
            db,
            "agent",
            "main",
            json!({"schema_version":1,"id":"main","name":"Kageha","builtin":true,"system_prompt":"","pinned_skills":[],"tool_policy":{"allow":[],"deny":[]},"knowledge":{"inherit_workspace":true,"extra_kb_ids":[]}}),
        )?;
    }
    Ok(())
}
pub(crate) fn agent(db: &Database, id: &str) -> ApiResult<Value> {
    db.profiles
        .get(id)
        .map_err(profile_error)?
        .map(|d| effective_persona(d.body().clone()))
        .ok_or((404, "Agent not found".into()))
}
fn effective_persona(mut value: Value) -> Value {
    if value["id"] == "main" {
        if !value["knowledge"].is_object() { value["knowledge"] = json!({}); }
        value["knowledge"]["inherit_workspace"] = json!(true);
        value["knowledge"]["extra_kb_ids"] = json!([]);
        value["skills_restricted_to_pinned"] = json!(false);
        value["tool_policy"] = json!({"allow":[],"deny":[]});
    }
    value
}
pub(crate) fn handles(path: &str) -> bool {
    path == "/api/agents"
        || path.starts_with("/api/agents/")
        || path == "/api/teams"
        || path.starts_with("/api/teams/")
}
fn validate(value: &Value, team: bool) -> ApiResult<()> {
    kageha_agents::validate(value, team).map_err(|e| (400, e.to_string()))
}
fn operation(db: &Database, path: &str, method: &Method, payload: Value) -> ApiResult<Value> {
    use kageha_agents::repository::Kind;
    let parts: Vec<_> = path.split('/').collect();
    let team = parts[2] == "teams";
    let kind = if team { Kind::Team } else { Kind::Agent };
    // Admission and team membership changes use the existing application lock;
    // profile transactions and history are owned by the component repository.
    let connection = db.lock().map_err(db_error)?;
    if parts.len() == 3 && method == &Method::Get {
        let values = db
            .profiles
            .entries()
            .map_err(profile_error)?
            .into_iter()
            .filter(|e| e.kind == kind)
            .filter_map(|e| e.head().value.clone())
            .map(|value| if team { value } else { effective_persona(value) })
            .collect::<Vec<_>>();
        return Ok(json!({if team{"teams"}else{"agents"}:values}));
    }
    if path == "/api/agents/_portable/export" && method == &Method::Get {
        return Ok(json!({"bundle":db.profiles.export().map_err(profile_error)?,"repository":{"component":"agents","scope":db.profiles.scope(),"schema_version":1}}));
    }
    if !team && parts.get(3) == Some(&"draft") {
        drop(connection);
        let provider = crate::ui_settings::configured(db, "", "default")?;
        let reply=crate::native_chat::complete_with(&provider,vec![json!({"role":"system","content":"Draft specialist instructions for the user to review. Return only JSON with description and system_prompt strings. Preserve the user's constraints. Do not execute tools."}),json!({"role":"user","content":payload.to_string()})],&std::sync::atomic::AtomicBool::new(false),vec![]).map_err(|_|(502,"Agent draft generation failed".into()))?;
        let draft: Value = serde_json::from_str(reply["message"]["content"].as_str().unwrap_or(""))
            .map_err(|_| (502, "Draft provider returned invalid JSON".into()))?;
        text(&draft, "system_prompt", 65536)?;
        return Ok(json!({"draft":draft}));
    }
    let id = if parts.len() == 3 {
        text(&payload, "id", 80)?.to_owned()
    } else {
        parts[3].to_owned()
    };
    if !(if team {
        server::identifier(&id)
    } else {
        kageha_contracts::agents::valid_id(&id)
    }) {
        return Err((400, "Invalid agent/team id".into()));
    }
    let entry = db.profiles.entry(kind, &id).map_err(profile_error)?;
    let head = entry.as_ref().map_or(0, |e| e.head().number);
    let previous = entry.as_ref().and_then(|e| e.head().value.clone());
    if parts.get(4) == Some(&"history") && method == &Method::Get {
        let history=entry.as_ref().map(|e|e.revisions.iter().rev().filter(|r|r.value.is_some()).map(|r|json!({"id":r.source_revision.clone().unwrap_or_else(||r.number.to_string()),"timestamp":r.timestamp})).collect::<Vec<_>>()).unwrap_or_default();
        return Ok(json!({"history":history}));
    }
    if parts.get(4) == Some(&"knowledge") {
        if previous.is_none() {
            return Err((404, "Agent not found".into()));
        }
        if parts.get(5) == Some(&"search") {
            if method != &Method::Post {
                return Err((405, "POST required".into()));
            }
            drop(connection);
            return search_knowledge(
                db,
                &id,
                &payload,
                &std::sync::atomic::AtomicBool::new(false),
            )
            .map_err(|e| (400, e.to_string()));
        }
        let files = store::list(&connection, &format!("knowledge:{id}")).map_err(db_error)?;
        return Ok(
            json!({"agent_id":id,"sources":files.iter().map(|v|v["filename"].clone()).collect::<Vec<_>>(),"files":files,"engine":"native-text","retrieval":kageha_knowledge::capabilities(),"knowledge":previous.as_ref().map(|v|v["knowledge"].clone())}),
        );
    }
    if parts.len() == 4 && method == &Method::Get {
        let persona = previous.ok_or((404, "Agent/team not found".into()))?;
        let persona = if team { persona } else { effective_persona(persona) };
        return Ok(if team {
            json!({"team":persona})
        } else {
            json!({"agent_id":id,"persona":persona,"effective":kageha_agents::Definition::parse(persona.clone()).map_err(profile_error)?.effective()})
        });
    }
    if id == "main"
        && !(method == &Method::Put
            && payload
                .as_object()
                .is_some_and(|v| v.len() == 1 && v.contains_key("knowledge")))
    {
        return Err((400, "The built-in default agent is protected".into()));
    }
    if let Some(expected) = payload.get("expected_revision") {
        if expected.as_u64() != Some(head) {
            return Err((409, "Profile revision conflict".into()));
        }
    }
    if method == &Method::Delete {
        if previous.is_none() {
            return Err((404, "Agent/team not found".into()));
        }
        db.profiles
            .write(kind, &id, head, None, store::now())
            .map_err(profile_error)?;
        return Ok(json!({"ok":true,"deleted":true,"id":id}));
    }
    let mut next = if parts.get(4) == Some(&"rollback") {
        if method != &Method::Post {
            return Err((405, "POST required".into()));
        }
        let version = payload["snapshot_id"]
            .as_str()
            .or_else(|| payload["version_id"].as_str())
            .or_else(|| payload["history_id"].as_str())
            .unwrap_or("");
        let number = version.parse::<u64>().ok();
        entry
            .as_ref()
            .and_then(|e| {
                e.revisions.iter().find(|r| {
                    if let Some(n) = number {
                        r.number == n
                    } else {
                        r.source_revision.as_deref() == Some(version)
                    }
                })
            })
            .and_then(|r| r.value.clone())
            .ok_or((404, "Revision not found".into()))?
    } else {
        if method != &Method::Post && method != &Method::Put {
            return Err((405, "Unsupported agent operation".into()));
        }
        if parts.len() == 3 && previous.is_some() {
            return Err((409, "Agent/team already exists".into()));
        }
        if parts.len() > 3 && previous.is_none() {
            return Err((404, "Agent/team not found".into()));
        }
        let mut v = previous
            .unwrap_or(json!({"schema_version":1,"builtin":false,"created_at":store::now()}));
        for (key, value) in payload
            .as_object()
            .ok_or((400, "Expected an object".into()))?
        {
            if !["builtin", "created_at", "revision", "expected_revision"].contains(&key.as_str()) {
                v[key] = value.clone();
            }
        }
        v
    };
    next["id"] = json!(id);
    next["updated_at"] = json!(store::now());
    next["builtin"] = json!(id == "main");
    validate(&next, team)?;
    if team {
        for member in next["agents"].as_array().unwrap() {
            if db
                .profiles
                .get(member.as_str().unwrap())
                .map_err(profile_error)?
                .is_none()
            {
                return Err((400, "Unknown team member".into()));
            }
        }
    }
    let saved = db
        .profiles
        .write(kind, &id, head, Some(next), store::now())
        .map_err(profile_error)?;
    Ok(json!({if team{"team"}else{"agent"}:saved.head().value}))
}
fn profile_error(e: std::io::Error) -> (u16, String) {
    (
        match e.kind() {
            std::io::ErrorKind::AlreadyExists => 409,
            std::io::ErrorKind::InvalidInput => 400,
            _ => 500,
        },
        e.to_string(),
    )
}
pub(crate) fn handle(mut request: Request, db: &Database) -> std::io::Result<()> {
    let path = request.url().split('?').next().unwrap_or("").to_owned();
    let method = request.method().clone();
    if path.ends_with("/knowledge/upload") && method == Method::Post {
        let id = path.split('/').nth(3).unwrap_or("");
        let result = (|| -> ApiResult<Value> {
            agent(db, id)?;
            let kb = knowledge_chat(id);
            {
                let connection = db.lock().map_err(db_error)?;
                connection.execute("INSERT OR IGNORE INTO ui_sessions(id,title,created_at,updated_at) VALUES (?,'Agent knowledge',?,?)",params![kb,store::now(),store::now()]).map_err(db_error)?;
                connection
                    .execute(
                        "INSERT OR IGNORE INTO ui_internal_sessions VALUES (?)",
                        [&kb],
                    )
                    .map_err(db_error)?;
            }
            let file = crate::ui_uploads::upload(&mut request, db, &kb)
                .map_err(|e| (400, e.to_string()))?;
            let connection = db.lock().map_err(db_error)?;
            let saved = store::put(
                &connection,
                &format!("knowledge:{id}"),
                file["path"].as_str().unwrap(),
                file.clone(),
            )
            .map_err(db_error)?;
            Ok(
                json!({"ok":true,"filename":saved["filename"],"kb_id":kb,"engine":"native-text","stats":{"files":1,"indexed":saved["content_type"]=="text/plain"}}),
            )
        })();
        match result {
            Ok(v) => server::response(request, 200, v),
            Err((code, error)) => server::response(request, code, json!({"error":error})),
        }
        return Ok(());
    }
    let result = if matches!(method, Method::Post | Method::Put) {
        webui::body(&mut request).map_err(|_| (400, "Invalid JSON".into()))
    } else {
        Ok(json!({}))
    }
    .and_then(|p| operation(db, &path, &method, p));
    match result {
        Ok(v) => server::response(request, 200, v),
        Err((code, error)) => server::response(request, code, json!({"error":error})),
    }
    Ok(())
}

pub(crate) fn knowledge_chat(id: &str) -> String {
    use sha2::{Digest, Sha256};
    format!(
        "kb-{}",
        &format!("{:x}", Sha256::digest(id.as_bytes()))[..32]
    )
}
pub(crate) fn knowledge_context(db: &Database, id: &str) -> ApiResult<Value> {
    let connection = db.lock().map_err(db_error)?;
    let files = store::list(&connection, &format!("knowledge:{id}")).map_err(db_error)?;
    Ok(json!(files.iter().map(|v|json!({"path":format!("knowledge/{}",v["path"].as_str().unwrap_or("")),"filename":v["filename"]})).collect::<Vec<_>>()))
}
pub(crate) fn read_knowledge(db: &Database, id: &str, path: &str) -> std::io::Result<String> {
    let key = path
        .strip_prefix("knowledge/")
        .ok_or_else(|| std::io::Error::other("Invalid knowledge path"))?;
    let owned = store::get(
        &*db.lock()
            .map_err(|_| std::io::Error::other("database unavailable"))?,
        &format!("knowledge:{id}"),
        key,
    )
    .map_err(std::io::Error::other)?;
    if owned.is_none() {
        return Err(std::io::Error::other(
            "Knowledge file belongs to another agent",
        ));
    }
    let (source, metadata) = crate::ui_uploads::pinned(db, &knowledge_chat(id), key)?;
    crate::documents::read_verified(
        &source,
        metadata["sha256"].as_str(),
        &std::sync::atomic::AtomicBool::new(false),
    )
}

/// Retrieve only this agent's current source set; no caller-supplied KB can widen it.
pub(crate) fn search_knowledge(
    db: &Database,
    id: &str,
    args: &Value,
    cancel: &std::sync::atomic::AtomicBool,
) -> std::io::Result<Value> {
    search_knowledge_scoped(db, id, args, cancel, None)
}
pub(crate) fn search_knowledge_scoped(
    db: &Database,
    id: &str,
    args: &Value,
    cancel: &std::sync::atomic::AtomicBool,
    scope: Option<&kageha_knowledge::scope::Scope>,
) -> std::io::Result<Value> {
    use kageha_knowledge::{Document, Lexical, Retriever};
    agent(db, id).map_err(|(_, e)| std::io::Error::other(e))?;
    if args["search_mode"]
        .as_str()
        .is_some_and(|v| !matches!(v, "auto" | "lexical"))
    {
        return Err(std::io::Error::other("This adapter supports lexical retrieval; semantic and remote retrievers are not active"));
    }
    let filter = args["kb_id"].as_str().unwrap_or("");
    let private = format!("agent-{id}");
    if !filter.is_empty()
        && filter != private
        && !scope.is_some_and(|s| s.agent == id && s.shared.iter().any(|b| b.id == filter))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "Knowledge base is outside this run's scope",
        ));
    }
    if scope.is_some_and(|s| s.agent != id) {
        return Err(std::io::Error::other("Knowledge agent scope mismatch"));
    }
    let query = args["query"]
        .as_str()
        .ok_or_else(|| std::io::Error::other("query required"))?;
    let limit = match args.get("top_k") {
        None => 5,
        Some(v) => {
            v.as_u64()
                .filter(|v| *v > 0 && *v <= 20)
                .ok_or_else(|| std::io::Error::other("top_k must be 1–20"))? as usize
        }
    };
    let files = store::list(
        &*db.lock()
            .map_err(|_| std::io::Error::other("database unavailable"))?,
        &format!("knowledge:{id}"),
    )
    .map_err(std::io::Error::other)?;
    if files.len() > 2048 {
        return Err(std::io::Error::other(
            "Knowledge corpus exceeds retrieval limits",
        ));
    }
    let mut documents = Vec::new();
    let mut unindexed = Vec::new();
    let control = kageha_contracts::execution::Control {
        cancelled: cancel,
        deadline: None,
    };
    for file in files
        .into_iter()
        .filter(|_| filter.is_empty() || filter == private)
    {
        control.check()?;
        let key = file["path"]
            .as_str()
            .ok_or_else(|| std::io::Error::other("Knowledge path missing"))?;
        let (source, metadata) = crate::ui_uploads::pinned(db, &knowledge_chat(id), key)?;
        let hash = metadata["sha256"]
            .as_str()
            .ok_or_else(|| std::io::Error::other("Knowledge hash missing"))?;
        let index = store::get(
            &*db.lock()
                .map_err(|_| std::io::Error::other("database unavailable"))?,
            &format!("knowledge-index:{id}"),
            key,
        )
        .map_err(std::io::Error::other)?;
        let chunks = if let Some(index) = index {
            if index["sha256"] != hash {
                return Err(std::io::Error::other("Knowledge index source changed"));
            }
            serde_json::from_value(index["chunks"].clone())?
        } else if file["content_type"]
            .as_str()
            .is_some_and(|v| matches!(v, "text/plain" | "application/json"))
        {
            let text = crate::documents::read_verified(&source, Some(hash), cancel)?;
            kageha_knowledge::chunks_from_text(hash, &text)?
        } else {
            Vec::new()
        };
        if chunks.is_empty() {
            unindexed.push(file["filename"].clone());
        }
        documents.push(Document {
            id: key.into(),
            owner: id.into(),
            source: format!("knowledge/{key}"),
            sha256: hash.into(),
            chunks,
            tombstone: false,
        });
    }
    if let Some(scope) = scope {
        let mut shared = scope.clone();
        shared
            .shared
            .retain(|b| filter.is_empty() || filter == b.id);
        if !shared.shared.is_empty() {
            documents.extend(crate::ui_knowledge::store(db)?.documents(&shared, &control)?);
        }
    }
    let hits = Lexical.search(id, &documents, query, limit, &control)?;
    Ok(
        json!({"ok":true,"agent_id":id,"kb_id":format!("agent-{id}"),"query":query,"hits":hits,"unindexed_sources":unindexed,"capabilities":kageha_knowledge::capabilities()}),
    )
}

#[cfg(test)]
mod owned_tests {
    use super::*;
    #[test]
    fn default_effective_access_is_consistent_with_execution() {
        let main = effective_persona(json!({"id":"main","knowledge":{"inherit_workspace":false},"skills_restricted_to_pinned":true,"tool_policy":{"deny":["integration_read"]}}));
        assert_eq!(main["knowledge"]["inherit_workspace"], true);
        assert_eq!(main["skills_restricted_to_pinned"], false);
        assert_eq!(main["tool_policy"]["deny"], json!([]));
        let specialist = json!({"id":"research","tool_policy":{"deny":["integration_read"]}});
        assert_eq!(effective_persona(specialist.clone()), specialist);
    }
    #[test]
    fn owned_routes_keep_protections_cas_history_and_team_validation() {
        let db =
            crate::agent_host::DatabaseState::open(rusqlite::Connection::open_in_memory().unwrap())
                .unwrap();
        assert_eq!(
            operation(&db, "/api/agents/main", &Method::Delete, json!({}))
                .unwrap_err()
                .0,
            400
        );
        assert_eq!(
            operation(
                &db,
                "/api/agents/main",
                &Method::Put,
                json!({"system_prompt":"replace default"})
            )
            .unwrap_err()
            .0,
            400
        );
        operation(
            &db,
            "/api/agents/main",
            &Method::Put,
            json!({"knowledge":{"inherit_workspace":false,"extra_kb_ids":[]}}),
        )
        .unwrap();
        let saved = operation(
            &db,
            "/api/agents",
            &Method::Post,
            json!({"id":"research","name":"Researcher","system_prompt":"Original","builtin":true}),
        )
        .unwrap();
        assert_eq!(saved["agent"]["builtin"], false);
        assert_eq!(
            operation(
                &db,
                "/api/teams",
                &Method::Post,
                json!({"id":"reviewers","name":"Team","agents":["missing"]})
            )
            .unwrap_err()
            .0,
            400
        );
        operation(
            &db,
            "/api/teams",
            &Method::Post,
            json!({"id":"reviewers","name":"Team","agents":["research"]}),
        )
        .unwrap();
        assert_eq!(
            operation(
                &db,
                "/api/agents/research",
                &Method::Put,
                json!({"system_prompt":"stale","expected_revision":0})
            )
            .unwrap_err()
            .0,
            409
        );
        operation(
            &db,
            "/api/agents/research",
            &Method::Put,
            json!({"system_prompt":"Edited","expected_revision":1}),
        )
        .unwrap();
        operation(
            &db,
            "/api/agents/research",
            &Method::Delete,
            json!({"expected_revision":2}),
        )
        .unwrap();
        assert!(agent(&db, "research").is_err());
        let history =
            operation(&db, "/api/agents/research/history", &Method::Get, json!({})).unwrap();
        assert_eq!(history["history"].as_array().unwrap().len(), 2);
        let restored = operation(
            &db,
            "/api/agents/research/rollback",
            &Method::Post,
            json!({"snapshot_id":"1","expected_revision":3}),
        )
        .unwrap();
        assert_eq!(restored["agent"]["revision"], 4);
        assert_eq!(restored["agent"]["system_prompt"], "Original");
        assert!(
            crate::ui_store::get(&db.lock().unwrap(), "agent", "research")
                .unwrap()
                .is_none()
        );
        operation(
            &db,
            "/api/agents",
            &Method::Post,
            json!({"id":"export","name":"Unreserved id"}),
        )
        .unwrap();
        assert_eq!(
            operation(&db, "/api/agents/export", &Method::Get, json!({})).unwrap()["persona"]["id"],
            "export"
        );
    }
}
