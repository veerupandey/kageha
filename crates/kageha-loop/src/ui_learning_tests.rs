use super::*;
use crate::audited_session_tests::{main_persona, Fixture, CHAT};
use kageha_runtime::test_support::{text_reply, tool_call, tool_calls_reply, ScriptedModel};

fn installed(fixture: &Fixture) -> (tempfile::TempDir, Arc<SqliteLearning>) {
    let directory = tempfile::tempdir().unwrap();
    install(&fixture.db, &directory.path().join("learning.sqlite")).unwrap();
    let store = store(&fixture.db).unwrap();
    (directory, store)
}

fn task_status(frames: &[Value]) -> Option<String> {
    frames.iter().rev().find(|frame| frame["kind"] == "task_completion")
        .and_then(|frame| frame["status"].as_str().map(str::to_owned))
}

#[test]
fn agent_can_save_and_retire_a_lesson_with_request_evidence() {
    use crate::native_chat::ToolHost;
    let fixture = Fixture::new();
    let (directory, store) = installed(&fixture);
    let host = fixture.tools(main_persona(Value::Null));
    let key = request_key(&fixture.db, &fixture.turn);
    observe(&fixture.db, &fixture.turn, &json!({"kind":"tools","calls":[{"id":"read-1","name":"read_file","arguments":{"path":"workspace/notes.md"}}]}));
    observe(&fixture.db, &fixture.turn, &json!({"kind":"tool_results","results":[{"tool_call_id":"read-1","is_error":false,"content":"notes"}]}));
    let long_id = "call_Xa93Bc65De12Fg98Hi45Jk67Lm23No89Pq";
    let uuid_id = "550e8400-e29b-41d4-a716-446655440000";
    observe(&fixture.db, &fixture.turn, &json!({"kind":"tools","calls":[{"id":long_id,"name":"read_file","arguments":{"path":"workspace/notes.md"}},{"id":uuid_id,"name":"read_file","arguments":{"path":"workspace/notes.md"}}]}));
    observe(&fixture.db, &fixture.turn, &json!({"kind":"tool_results","results":[{"tool_call_id":long_id,"is_error":false,"content":"notes"},{"tool_call_id":uuid_id,"is_error":false,"content":"notes"}]}));
    assert!(host.specs().iter().any(|s| s["function"]["name"] == "lesson_save"));
    let alias = {
        let conn = fixture.db.lock().unwrap();
        conn.execute("INSERT INTO ui_events(chat_id,turn_id,event,payload) VALUES(?,?,'tool_card',?)",
            rusqlite::params![CHAT,fixture.turn,json!({"id":"read-1","name":"read_file","status":"done"}).to_string()]).unwrap();
        format!("ev-{}", conn.last_insert_rowid())
    };
    let call = json!({"id":"save-1","name":"lesson_save","arguments":{"task_type":"notes",
        "trigger":"Meeting notes are missing","action":"Create the notes before reading",
        "evidence":[alias,long_id,uuid_id]}});
    let saved: Value = serde_json::from_str(&host.invoke(&call).unwrap()).unwrap();
    assert_eq!(saved["status"], "candidate");
    let search = json!({"id":"recall","name":"lesson_search","arguments":{"query":"meeting notes"}});
    let candidate: Value = serde_json::from_str(&host.invoke(&search).unwrap()).unwrap();
    assert!(candidate["context"].as_str().unwrap().contains("[unverified candidate]"));
    assert!(crate::ui_learned_context::message(&host, &[json!({"role":"user","content":"meeting notes"})]).is_none());
    fixture.db.learning.offer(Message::Close { key: key.clone(), summary: RequestSummary {
        agent: "main".into(), chat_id: CHAT.into(), task_type: Some("notes".into()),
        status: "verified".into(), model_calls: 1, cost_usd: 0.0, cached_tokens: 0,
        outcomes: vec![], started: 1, ended: 2,
    }});
    assert!(flush(&fixture.db, FLUSH_WAIT));
    let later = Fixture::new();
    install(&later.db, &directory.path().join("learning.sqlite")).unwrap();
    let later_host = later.tools(main_persona(Value::Null));
    let context = crate::ui_learned_context::message(&later_host, &[json!({"role":"user","content":"meeting notes"})]).unwrap();
    assert!(context["content"].as_str().unwrap().contains("Create the notes"));
    assert!(crate::ui_learned_context::message(&later_host, &[json!({"role":"user","content":"calculate orbital velocity"})]).is_none());
    let found: Value = serde_json::from_str(&later_host.invoke(&search).unwrap()).unwrap();
    assert_eq!(found["lesson_ids"], json!([saved["id"]]));
    let id = saved["id"].as_i64().unwrap();
    host.invoke(&json!({"id":"retire-1","name":"lesson_retire","arguments":{"id":id,"reason":"Outdated"}})).unwrap();
    let found: Value = serde_json::from_str(&later_host.invoke(&search).unwrap()).unwrap();
    assert_eq!(found["lesson_ids"], json!([]));
    assert_eq!(store.lessons("main").unwrap()[0].status, kageha_learning::LessonStatus::Retired);
}

/// Paired wiring evaluation with a deterministic model that deliberately follows a
/// supplied note. This verifies delivery, tool dispatch and outputs, not LLM quality.
#[test]
fn learned_context_changes_only_the_relevant_hosted_task_in_a_paired_fixture() {
    use crate::native_chat::ToolHost;
    use kageha_contracts::execution::{AssistantMessage, Control, ModelPort, ModelRequest};
    use std::sync::Mutex;
    struct ContextModel { requests: Mutex<Vec<ModelRequest>> }
    impl ModelPort for ContextModel {
        fn complete(&self, request: ModelRequest, _: &Control<'_>) -> io::Result<AssistantMessage> {
            let result = request.messages.iter().rev().find(|m| m["role"] == "tool")
                .and_then(|m| m["content"].as_str()).map(str::to_owned);
            let related = request.messages.iter().any(|m| m["role"] == "user" && m["content"].as_str().is_some_and(|s| s.contains("meeting notes")));
            let learned = request.messages.iter().any(|m| m["role"] == "system" && m["content"].as_str().is_some_and(|s| s.contains("use the concise style")));
            self.requests.lock().unwrap().push(request);
            if let Some(result) = result { return Ok(text_reply(result)); }
            let path = if !related { "workspace/styles/astronomy.txt" } else if learned { "workspace/styles/concise.txt" } else { "workspace/styles/verbose.txt" };
            Ok(tool_calls_reply([tool_call("style-read", "read_file", json!({"path":path}))]))
        }
    }
    let source = Fixture::new();
    let (directory, store) = installed(&source);
    let host = source.tools(main_persona(Value::Null));
    observe(&source.db, &source.turn, &json!({"kind":"tools","calls":[{"id":"evidence","name":"read_file","arguments":{"path":"workspace/style.txt"}}]}));
    observe(&source.db, &source.turn, &json!({"kind":"tool_results","results":[{"tool_call_id":"evidence","is_error":false,"content":"concise"}]}));
    host.invoke(&json!({"id":"save","name":"lesson_save","arguments":{"task_type":"notes","trigger":"When preparing meeting notes","action":"use the concise style","evidence":["evidence"]}})).unwrap();
    let key = request_key(&source.db, &source.turn);
    source.db.learning.offer(Message::Close { key, summary: RequestSummary { agent:"main".into(),chat_id:CHAT.into(),task_type:Some("notes".into()),status:"verified".into(),model_calls:1,cost_usd:0.0,cached_tokens:0,outcomes:vec![],started:1,ended:2 } });
    assert!(flush(&source.db, FLUSH_WAIT));
    assert_eq!(store.lessons("main").unwrap()[0].status, kageha_learning::LessonStatus::Active);
    for (related, enabled, expected_path, expected_text) in [
        (true,true,"workspace/styles/concise.txt","concise style"),
        (true,false,"workspace/styles/verbose.txt","verbose style"),
        (false,true,"workspace/styles/astronomy.txt","astronomy style"),
        (false,false,"workspace/styles/astronomy.txt","astronomy style"),
    ] {
        let fixture = Fixture::new();
        install(&fixture.db, &directory.path().join("learning.sqlite")).unwrap();
        let host = fixture.tools(main_persona(Value::Null));
        let workspace = crate::ui_workspace::open_for_chat(&fixture.db, CHAT).unwrap();
        for (path, content) in [("styles/concise.txt","concise style"),("styles/verbose.txt","verbose style"),("styles/astronomy.txt","astronomy style")] {
            workspace.write(path, content.as_bytes()).unwrap();
        }
        let user = if related { "Read the meeting notes style and reply with its contents" } else { "Read astronomy data and reply with its contents" };
        let mut messages = vec![json!({"role":"user","content":user})];
        if enabled {
            let learned = crate::ui_learned_context::message(&host, &messages);
            assert_eq!(learned.is_some(), related);
            messages.extend(learned);
        }
        let model = ContextModel { requests:Mutex::new(Vec::new()) };
        let mut frames = Vec::new();
        let start = crate::turn::Start {version:1,max_model_calls:10,messages};
        let answer = crate::native_chat::execute_hosted_with_model(host.workspace, start, &model, &host, host.cancel, |frame| { frames.push(frame.clone()); Ok(()) }).unwrap();
        assert_eq!(answer.trim(), expected_text);
        assert_eq!(std::fs::read_to_string(workspace.root().join(expected_path.trim_start_matches("workspace/"))).unwrap(), expected_text);
        let calls: Vec<_> = frames.iter().filter(|f|f["kind"] == "tools").flat_map(|f|f["calls"].as_array().into_iter().flatten()).collect();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["name"], "read_file");
        assert_eq!(calls[0]["arguments"]["path"], expected_path);
    }
}

/// Records an artifact outcome, fails to read the file, writes it, reads it again and
/// attaches it as evidence.
fn failing_then_succeeding() -> ScriptedModel {
    ScriptedModel::new([
        tool_calls_reply([tool_call("record-1", "record_required_outcomes", json!({"task_type":"notes","outcomes":[
            {"id":"notes","description":"Meeting notes file","kind":"artifact","verify":"outputs/notes.md exists"}]}))]),
        tool_calls_reply([tool_call("read-1", "read_file", json!({"path":"workspace/outputs/notes.md"}))]),
        tool_calls_reply([tool_call("write-1", "write_file", json!({"path":"outputs/notes.md","content":"# Notes\n- api_key=sk-live-abcdefghijklmnop1234\n"}))]),
        tool_calls_reply([tool_call("read-2", "read_file", json!({"path":"workspace/outputs/notes.md"}))]),
        tool_calls_reply([tool_call("evidence-1", "record_outcome_evidence",
            json!({"outcome_id":"notes","reference":"workspace/outputs/notes.md"}))]),
        text_reply("Saved the notes as outputs/notes.md."),
    ])
}

fn finish(fixture: &Fixture, frames: &[Value], started: u64) {
    let status = turn_status(false, "completed", task_status(frames).as_deref());
    finish_turn(&fixture.db, CHAT, &fixture.turn, 0, "main", status, started);
}

#[test]
fn a_failed_then_repaired_call_is_journaled_and_the_request_is_closed() {
    let fixture = Fixture::new();
    let (_directory, store) = installed(&fixture);
    let host = fixture.tools(main_persona(Value::Null));
    let model = failing_then_succeeding();
    let started = now_ms();

    let (message, frames) = fixture.run(&model, &host);
    assert_eq!(message, "Saved the notes as outputs/notes.md.");
    assert_eq!(task_status(&frames).as_deref(), Some("verified"));
    finish(&fixture, &frames, started);

    let key = crate::ui_task_completion::scope(&fixture.db, &fixture.turn).unwrap();
    let events = store.events(&key, 0, 100).unwrap();
    let kinds: Vec<_> = events.iter().map(|event| event.kind).collect();
    use JournalKind::*;
    assert_eq!(kinds, [Tool, Failure, Tool, Tool, Repair, Tool, Tool, Gate], "{events:#?}");
    let body = |kind| &events.iter().find(|event| event.kind == kind).unwrap().body;
    let failure = body(Failure);
    assert_eq!((failure["call_id"].as_str(), failure["tool"].as_str()), (Some("read-1"), Some("read_file")));
    assert_eq!(failure["origin_turn"], kageha_learning::lessons::lesson_origin(&fixture.turn));
    assert!(!failure["error_code"].as_str().unwrap().is_empty());
    let repair = body(Repair);
    assert_eq!((repair["failed_call_id"].as_str(), repair["repair_call_id"].as_str()), (Some("read-1"), Some("read-2")));
    assert_eq!(repair["between"], json!(["write-1"]));
    let read = &events[5].body;
    assert_eq!((read["call_id"].as_str(), read["status"].as_str(), read["effect"].as_str()), (Some("read-2"), Some("done"), Some("read")));
    // The excerpt of the file read back is scrubbed.
    let excerpt = read["excerpt"].as_str().unwrap();
    assert!(excerpt.len() <= EXCERPT_BYTES && !excerpt.contains("sk-live-abcdefghijklmnop1234"), "{excerpt}");
    assert_eq!(events[0].body["status"], "done");
    assert_eq!(body(Gate)["status"], "ready");
    assert!(events.windows(2).all(|pair| pair[0].seq < pair[1].seq));

    let run = store.run(&key).unwrap().expect("the request is closed");
    assert_eq!((run.status.as_str(), run.agent.as_str(), run.chat_id.as_str()), ("verified", "main", CHAT));
    assert_eq!(run.task_type.as_deref(), Some("notes"));
    assert_eq!(run.outcomes, [OutcomeStatus { id: "notes".into(), status: "verified".into() }]);
    let used: u64 = fixture.db.lock().unwrap()
        .query_row("SELECT used_model_calls FROM ui_request_budgets WHERE id=?", [&key], |row| row.get(0)).unwrap();
    // This scripted turn runs outside the request budget's charging scope, so the
    // model frames the journal saw are the count.
    assert!(run.model_calls >= used);
    assert_eq!(run.model_calls, model.requests().len() as u64);
    assert_eq!(run.cost_usd, 0.0);
    assert!(run.started <= run.ended);
    assert_eq!(store.reflection_state(&key).unwrap().map(|(state, _)| state).as_deref(), Some("queued"));
}

#[test]
fn completed_run_reflection_reaches_a_later_relevant_request_without_secrets() {
    use crate::model_router::{RoleProviders, TestModels};
    use kageha_learning::{LessonStatus, SecretValues};

    let first = Fixture::new();
    let (directory, store) = installed(&first);
    let host = first.tools(main_persona(Value::Null));
    let (message, frames) = first.run(&failing_then_succeeding(), &host);
    assert_eq!(message, "Saved the notes as outputs/notes.md.");
    assert_eq!(task_status(&frames).as_deref(), Some("verified"));
    finish(&first, &frames, now_ms());
    let request = crate::ui_task_completion::scope(&first.db, &first.turn).unwrap();
    let repair = store.events(&request, 0, 100).unwrap().into_iter()
        .find(|event| event.kind == JournalKind::Repair).unwrap();
    let unsafe_value = "portal-passphrase-41";
    let response = json!({"lessons":[
        {"task_type":"notes","trigger":"Meeting notes file is missing","action":"Create the notes file before reading it","evidence":[repair.seq]},
        {"task_type":"notes","trigger":"Meeting notes login","action":format!("Use {unsafe_value}"),"evidence":[repair.seq]},
        {"task_type":"other","trigger":"Unrelated evidence","action":"Claim an unsupported result","evidence":[999999]}
    ]}).to_string();
    let model = Arc::new(ScriptedModel::new([text_reply(&response)]));
    *first.db.reflection.test_roles.lock().unwrap() = Some(RoleProviders {
        fakes: TestModels { planner: Some(model), reviewer: None }, ..Default::default()
    });
    assert_eq!(crate::ui_reflection::reflect_pending(&first.db, &store, &SecretValues::new([unsafe_value])), 1);
    let lessons = store.lessons("main").unwrap();
    assert_eq!(lessons.len(), 1);
    assert_eq!(lessons[0].status, LessonStatus::Active);
    assert!(!format!("{lessons:?}").contains(unsafe_value));

    // A later request opens the same learning store rather than inserting a
    // lesson directly, and sees only relevant, validated content.
    let later = Fixture::new();
    install(&later.db, &directory.path().join("learning.sqlite")).unwrap();
    let later_host = later.tools(main_persona(Value::Null));
    let context = crate::ui_learned_context::message(&later_host,
        &[json!({"role":"user","content":"Prepare meeting notes for the team"})]).unwrap();
    let content = context["content"].as_str().unwrap();
    assert!(content.contains("Create the notes file before reading it"), "{content}");
    assert!(!content.contains(unsafe_value) && !content.contains("unsupported result"));
    assert!(crate::ui_learned_context::message(&later_host,
        &[json!({"role":"user","content":"Calculate orbital velocity"})]).is_none());
}

#[test]
fn success_on_another_browser_control_does_not_teach_a_false_repair() {
    let fixture = Fixture::new();
    let (_directory, store) = installed(&fixture);
    let turn = "browser-targets";
    let emit = |id: &str, name: &str, args: Value, failed: bool| {
        observe(&fixture.db, turn, &json!({"kind":"tools","calls":[{"id":id,"name":name,"arguments":args}]}));
        let content = if failed { r#"{"error_code":"field_unavailable","summary":"Field unavailable"}"# } else { r#"{"status":"ok"}"# };
        observe(&fixture.db, turn, &json!({"kind":"tool_results","results":[{"tool_call_id":id,"is_error":failed,"content":content}]}));
    };
    emit("f1", "browser_fill", json!({"target":"e1","observation_id":"old","text":"first"}), true);
    emit("observe", "browser_snapshot", json!({}), false);
    emit("f2", "browser_fill", json!({"target":"e2","observation_id":"old","text":"second"}), false);
    // e1 on a new snapshot can be a different element or even another tab.
    emit("f3", "browser_fill", json!({"target":"e1","observation_id":"new","text":"third"}), false);
    // An unbound action also cannot be assumed to refer to the original target.
    emit("f4", "browser_fill", json!({"target":"e1","text":"fourth"}), false);
    assert!(flush(&fixture.db, FLUSH_WAIT));
    assert!(!store.events(turn, 0, 50).unwrap().iter().any(|event| event.kind == JournalKind::Repair));
    let signature = failure_signature("browser_fill", r#"{"error_code":"field_unavailable","summary":"Field unavailable"}"#, true).unwrap();
    assert!(store.fixes(&signature).unwrap().is_empty());
}

#[test]
fn private_connector_and_browser_results_do_not_enter_learning_journal_or_fixes() {
    let fixture = Fixture::new();
    let (_directory, store) = installed(&fixture);
    let turn = "private-results";
    let code = "927641";
    let private_text = "Confidential interview invitation";
    let calls = [
        ("mail", "integration_read", json!({"integration":"gmail","operation":"message","id":"m1"}), false),
        ("mcp", "mcp_call", json!({"server":"mail","tool":"get"}), false),
        ("mcp-direct", "mcp_mail__read", json!({"url":format!("https://example.test/reset?code={code}"),"path":private_text}), false),
        ("snapshot", "browser_snapshot", json!({}), false),
        ("failure", "browser_fill", json!({"target":"e1","observation_id":"o1"}), true),
        ("navigation", "browser_open", json!({"url":format!("https://example.test/reset?code={code}")}), false),
        ("repair", "browser_fill", json!({"target":"e1","observation_id":"o1"}), false),
    ];
    for (id, name, args, failed) in calls {
        observe(&fixture.db, turn, &json!({"kind":"tools","calls":[{"id":id,"name":name,"arguments":args}]}));
        let content = json!({"error_code":"field_unavailable","summary":format!("{private_text} {code}"),"text":format!("{private_text} {code}")}).to_string();
        observe(&fixture.db, turn, &json!({"kind":"tool_results","results":[{"tool_call_id":id,"is_error":failed,"content":content}]}));
    }
    assert!(flush(&fixture.db, FLUSH_WAIT));
    let events = store.events(turn, 0, 50).unwrap();
    let rendered = format!("{events:?}");
    assert!(!rendered.contains(code) && !rendered.contains(private_text), "{rendered}");
    let failure = events.iter().find(|event| event.kind == JournalKind::Failure).unwrap();
    assert_eq!(failure.body["error_code"], "field_unavailable");
    assert_eq!(failure.body["excerpt"], "[sensitive result omitted]");
    let signature = failure_signature("browser_fill", &json!({"error_code":"field_unavailable","summary":format!("{private_text} {code}")}).to_string(), true).unwrap();
    let fixes = store.fixes(&signature).unwrap();
    assert!(fixes.is_empty(), "Browser repairs are journaled, not inferred from nearby calls");
    let learned = format!("{fixes:?}");
    assert!(!learned.contains(code) && !learned.contains(private_text) && !learned.contains("example.test"), "{learned}");
}

#[test]
fn an_unwritable_learning_store_does_not_affect_the_turn() {
    // A store that cannot be opened leaves journaling off.
    let fixture = Fixture::new();
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("not-a-directory");
    std::fs::write(&file, b"").unwrap();
    assert!(install(&fixture.db, &file.join("learning.sqlite")).is_err());
    assert!(store(&fixture.db).is_none());
    let host = fixture.tools(main_persona(Value::Null));
    let (message, frames) = fixture.run(&failing_then_succeeding(), &host);
    assert_eq!(message, "Saved the notes as outputs/notes.md.");
    finish(&fixture, &frames, now_ms());

    // A store whose tables are gone fails every write; the turn is the same.
    let fixture = Fixture::new();
    let (directory, store) = installed(&fixture);
    rusqlite::Connection::open(directory.path().join("learning.sqlite")).unwrap()
        .execute_batch("DROP TABLE learn_events; DROP TABLE learn_runs;").unwrap();
    let host = fixture.tools(main_persona(Value::Null));
    let model = failing_then_succeeding();
    let (message, frames) = fixture.run(&model, &host);
    assert_eq!(message, "Saved the notes as outputs/notes.md.");
    assert_eq!(task_status(&frames).as_deref(), Some("verified"));
    assert_eq!(model.remaining(), 0);
    finish(&fixture, &frames, now_ms());
    let key = crate::ui_task_completion::scope(&fixture.db, &fixture.turn).unwrap();
    assert!(store.events(&key, 0, 10).is_err() && store.run(&key).is_err());
}

#[test]
fn a_worker_journals_under_the_parent_request_and_does_not_close_it() {
    let fixture = Fixture::new();
    let (directory, store) = installed(&fixture);
    let parent = crate::ui_task_completion::scope(&fixture.db, &fixture.turn).unwrap();
    // A delegated worker inherits the parent's request budget id (`ui_jobs`).
    let worker_turn = crate::webui::turn_id("worker-chat", "worker-request");
    let spec = crate::runtime_host::spec(directory.path(), "worker-chat", "worker-request", "worker-run", "main",
        "Run the tests", 1, Some(CHAT), None, vec![], json!({"request_budget_id":parent}), 8);
    crate::runtime_host::prepare(&fixture.db.lock().unwrap(), &spec).unwrap();
    crate::webui::emit(&fixture.db, "worker-chat", &worker_turn, "event", json!({"kind":"shell_command",
        "payload":{"call_id":"w-1","command":"GITHUB_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz0123 make test","background":false,"exit_code":0,"files_changed":["outputs/report.txt"]}})).unwrap();
    finish_turn(&fixture.db, "worker-chat", &worker_turn, 1, "main", "verified", now_ms());
    assert!(flush(&fixture.db, FLUSH_WAIT));

    let events = store.events(&parent, 0, 10).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, JournalKind::ShellCommand);
    let body = &events[0].body;
    assert_eq!((body["call_id"].as_str(), body["exit_code"].as_i64()), (Some("w-1"), Some(0)));
    assert_eq!(body["files_changed"], json!(["outputs/report.txt"]));
    assert_eq!(body["command"], "GITHUB_TOKEN=[redacted] make test");
    assert!(store.run(&parent).unwrap().is_none(), "a worker must not close the parent's request");
    assert!(store.run(&worker_turn).unwrap().is_none());
    assert!(store.events(&worker_turn, 0, 10).unwrap().is_empty());

    // The parent's own turn end closes it.
    finish_turn(&fixture.db, CHAT, &fixture.turn, 0, "main", "not_required", now_ms());
    assert_eq!(store.run(&parent).unwrap().unwrap().status, "not_required");
}

#[test]
fn a_worker_without_request_persistence_journals_under_the_pinned_parent_request_id() {
    let fixture = Fixture::new();
    let (directory, store) = installed(&fixture);
    let parent = crate::ui_task_completion::scope(&fixture.db, &fixture.turn).unwrap();
    // With persistence off there is no budget id to inherit; `ui_jobs` pins the parent's
    // request id instead.
    let worker_turn = crate::webui::turn_id("worker-chat", "worker-request");
    let spec = crate::runtime_host::spec(directory.path(), "worker-chat", "worker-request", "worker-run", "main",
        "Run the tests", 1, Some(CHAT), None, vec![], json!({"persistence":{"enabled":false},"parent_request_id":parent}), 8);
    crate::runtime_host::prepare(&fixture.db.lock().unwrap(), &spec).unwrap();
    assert_eq!(request_key(&fixture.db, &worker_turn), parent);
    assert_eq!(crate::ui_task_completion::scope(&fixture.db, &worker_turn).unwrap(), worker_turn, "the worker keeps its own contract scope");
    crate::webui::emit(&fixture.db, "worker-chat", &worker_turn, "event", json!({"kind":"shell_command",
        "payload":{"call_id":"w-2","command":"make test","background":false,"exit_code":0,"files_changed":[]}})).unwrap();
    finish_turn(&fixture.db, "worker-chat", &worker_turn, 1, "main", "verified", now_ms());
    assert!(flush(&fixture.db, FLUSH_WAIT));
    let events = store.events(&parent, 0, 10).unwrap();
    assert_eq!(events.iter().map(|event| (event.kind, event.body["call_id"].as_str())).collect::<Vec<_>>(),
        [(JournalKind::ShellCommand, Some("w-2"))]);
    assert!(store.events(&worker_turn, 0, 10).unwrap().is_empty());
    assert!(store.run(&parent).unwrap().is_none(), "a worker must not close the parent's request");
    // A turn with neither id journals under its own turn id.
    assert_eq!(request_key(&fixture.db, "unknown-turn"), "unknown-turn");
}

#[test]
fn host_events_map_to_journal_kinds() {
    let fixture = Fixture::new();
    let (_directory, store) = installed(&fixture);
    let emit = |kind: &str, payload: Value| crate::webui::emit(&fixture.db, CHAT, &fixture.turn, "event",
        json!({"kind":kind,"interesting":true,"payload":payload})).unwrap();
    emit("escalation", json!({"kind":"escalation","from":"normal","to":"change_method","stale_steps":3}));
    emit("recovery", json!({"kind":"recovery","phase":"model","cause":"invalid_tool_call","status":"retrying"}));
    emit("recovery", json!({"phase":"restart","status":"resolved","tool_call_id":"c-1","tool":"read_file"}));
    emit("model_fallback", json!({"role":"planner","from":"a","to":"b","cause":"unavailable"}));
    // Repeats of the runtime frame and the bare restart event are not journaled twice.
    emit("completion_check", json!({"kind":"completion_check","status":"continue","evidence":[]}));
    emit("component_restarted", json!({"component":"runtime"}));
    emit("completion_check", json!({"status":"continue","evidence":[],"required":1,"instruction":"Attach proof."}));
    emit("outcome_blocked", json!({"outcome_id":"submit","category":"missing_access","reason":"No login","references":["c-1"]}));
    emit("todo_board", json!({}));
    crate::webui::emit(&fixture.db, CHAT, &fixture.turn, "tool_card", json!({"id":"c-1","name":"read_file","status":"done"})).unwrap();
    assert!(flush(&fixture.db, FLUSH_WAIT));

    let events = store.events(&fixture.turn, 0, 20).unwrap();
    let kinds: Vec<_> = events.iter().map(|event| event.kind).collect();
    use JournalKind::*;
    assert_eq!(kinds, [Escalation, Recovery, Recovery, Recovery, Gate, Outcome]);
    assert_eq!((events[0].body["to"].as_str(), events[0].body.get("kind")), (Some("change_method"), None));
    let causes: Vec<_> = events[1..4].iter().map(|event| event.body["cause"].as_str().unwrap()).collect();
    assert_eq!(causes, ["invalid_tool_call", "restart", "model_fallback"]);
    assert_eq!(events[4].body["required"], 1);
    assert_eq!((events[5].body["event"].as_str(), events[5].body["detail"]["outcome_id"].as_str()), (Some("outcome_blocked"), Some("submit")));
}

#[test]
fn request_status_follows_the_turn_and_the_task_status() {
    assert_eq!(turn_status(true, "completed", Some("verified")), "cancelled");
    assert_eq!(turn_status(false, "cancelled", None), "cancelled");
    assert_eq!(turn_status(false, "failed", Some("verified")), "failed");
    for status in ["verified", "partial", "blocked", "budget_limited", "not_required"] {
        assert_eq!(turn_status(false, "completed", Some(status)), status);
    }
    assert_eq!(turn_status(false, "completed", None), "not_required");
    assert_eq!(turn_status(false, "unverified", Some("blocked")), "blocked");
    assert_eq!(turn_status(false, "unverified", Some("verified")), "partial");
    assert_eq!(turn_status(false, "unverified", None), "blocked");
}

#[test]
fn operations_match_the_same_tool_and_main_argument() {
    assert_eq!(operation("shell", &json!({"command":"python scripts/extract.py in.pdf"})), "shell python");
    assert_eq!(operation("shell", &json!({"command":"cd work && PYTHONPATH=. /usr/bin/python3 -m pytest"})), "shell python3");
    assert_eq!(operation("shell", &json!({"command":"sudo env A=1 uv pip install pdfplumber"})), "shell uv");
    assert_eq!(operation("shell", &json!({"command":"cd work"})), "shell cd");
    assert_eq!(operation("read_file", &json!({"path":"workspace/a.md"})), "read_file workspace/a.md");
    assert_eq!(operation("web_fetch", &json!({"url":"https://example.com/a"})), "web_fetch https://example.com/a");
    assert_eq!(operation("integration_read", &json!({"integration":"gmail","operation":"search","query":"x"})), "integration_read gmail:search");
    assert_eq!(operation("record_outcome_evidence", &json!({"outcome_id":"a"})), "record_outcome_evidence");
}

#[test]
fn token_like_values_are_scrubbed_and_ordinary_text_is_kept() {
    for (text, expected) in [
        ("Authorization: Bearer abc.def-123", "Authorization: Bearer [redacted]"),
        ("OPENAI_API_KEY=sk-proj-0123456789abcdef make", "OPENAI_API_KEY=[redacted] make"),
        (r#"{"password":"hunter2","user":"ana"}"#, r#"{"password":"[redacted]","user":"ana"}"#),
        ("token ghp_abcdefghijklmnopqrstuvwxyz0123 used", "token [redacted] used"),
        ("jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJl", "jwt [redacted]"),
        ("key AbCdEfGh0123456789AbCdEfGh0123456789", "key [redacted]"),
        ("Read workspace/outputs/report-2024.md: 12 lines", "Read workspace/outputs/report-2024.md: 12 lines"),
        (r#"{"error_code":"invalid_token","summary":"Sign in again"}"#, r#"{"error_code":"invalid_token","summary":"Sign in again"}"#),
        ("/Users/ana/Projects/kageha/crates/kageha-loop/src/ui_learning.rs", "/Users/ana/Projects/kageha/crates/kageha-loop/src/ui_learning.rs"),
    ] {
        assert_eq!(scrub(text), expected, "{text}");
    }
    let long = "é".repeat(EXCERPT_BYTES);
    let cut = excerpt(&long);
    assert!(cut.len() <= EXCERPT_BYTES && long.starts_with(&cut));
}

// Failure index (task 22.1).

/// The content the runtime returned to the model for `call_id`.
fn result_content(frames: &[Value], call_id: &str) -> String {
    frames.iter().filter(|frame| frame["kind"] == "tool_results")
        .flat_map(|frame| frame["results"].as_array().cloned().unwrap_or_default())
        .find(|result| result["tool_call_id"] == call_id)
        .and_then(|result| result["content"].as_str().map(str::to_owned))
        .unwrap_or_else(|| panic!("no result for {call_id}"))
}

fn shell_result(exit_code: i64, stderr: &str) -> String {
    json!({"exit_code":exit_code,"stdout":"","stderr":stderr,"duration_ms":5}).to_string()
}

/// One `shell` call of `turn` as `webui::execute` and `HostedTools::execute` journal it:
/// the `tools` frame, the Host's known-fix lookup on the result, and the `tool_results`
/// frame with the fix attached. Returns the fix.
fn shell(fixture: &Fixture, turn: &str, id: &str, command: &str, exit_code: i64, stderr: &str) -> Option<Value> {
    let arguments = json!({"command":command});
    observe(&fixture.db, turn, &json!({"kind":"tools","calls":[{"id":id,"name":"shell","arguments":arguments}]}));
    let call = ToolCall { id: id.into(), name: "shell".into(), arguments };
    let content = shell_result(exit_code, stderr);
    let fix = known_fix(&fixture.db, turn, &call, &content, false);
    let content = fix.as_ref().map_or(content.clone(), |fix| attach_known_fix(&content, fix));
    observe(&fixture.db, turn, &json!({"kind":"tool_results","results":[{"tool_call_id":id,"is_error":false,"content":content}]}));
    fix
}

const MISSING_A: &str = "Traceback (most recent call last):\n  File \"/Users/ana/work-1/extract.py\", line 1, in <module>\n\
    ModuleNotFoundError: No module named 'pdfplumber' (/Users/ana/work-1/extract.py:1)\n";
const MISSING_B: &str = "Traceback (most recent call last):\n  File \"/tmp/run-77/extract.py\", line 3, in <module>\n\
    ModuleNotFoundError: No module named 'pdfplumber' (/tmp/run-77/extract.py:3)\n";

#[test]
fn failure_signatures_steps_and_attached_fixes() {
    let sig = |tool: &str, content: &str, is_error| failure_signature(tool, content, is_error);
    // A command that exited non-zero: `exit_<code>` and the last stderr line, normalized.
    let a = sig("shell", &shell_result(1, MISSING_A), false).unwrap();
    assert_eq!((a.error_code.as_str(), a.message.as_str()), ("exit_1", "modulenotfounderror: no module named 'pdfplumber' (<path>:<n>)"));
    assert_eq!(a.digest(), sig("shell", &shell_result(1, MISSING_B), false).unwrap().digest());
    assert_ne!(a.digest(), sig("shell", &shell_result(2, MISSING_B), false).unwrap().digest());
    assert_ne!(a.digest(), sig("shell", &shell_result(1, "No module named 'numpy'"), false).unwrap().digest());
    assert!(sig("shell", &shell_result(0, "warning"), false).is_none());
    assert!(sig("read_file", "file text", false).is_none());
    // Errors: the envelope's code and summary, or the text itself.
    let error = sig("read_file", r#"Tool failed: {"error_code":"not_found","retryable":false,"summary":"File /w/a/x.md not found"}"#, true).unwrap();
    assert_eq!((error.error_code.as_str(), error.message.as_str()), ("not_found", "file <path> not found"));
    let plain = sig("mcp_call", "Tool failed: boom 42", true).unwrap();
    assert_eq!((plain.error_code.as_str(), plain.message.as_str()), ("tool_failed", "sensitive tool failed"));

    assert_eq!(step("shell", &json!({"command":"uv pip install\n  pdfplumber"})), "shell: uv pip install pdfplumber");
    assert_eq!(step("write_file", &json!({"path":"outputs/a.md","content":"x"})), "write_file: outputs/a.md");
    assert_eq!(step("todo_update", &json!({})), "todo_update");
    assert_eq!(step("shell", &json!({"command":"API_KEY=sk-proj-0123456789abcdef make"})), "shell: API_KEY=[redacted] make");

    let fix = json!({"steps":["shell: uv pip install pdfplumber"],"successes":2,"last_seen":7});
    let proof_failure = r#"Tool failed: {"error_code":"invalid_outcome_evidence","reason_code":"missing_proof","next_tool":"amend_required_outcome"}"#;
    assert_eq!(attach_known_fix(proof_failure, &fix), proof_failure);
    let wrapped = json!({"tool_result":proof_failure,"hook_context":"hook"}).to_string();
    assert_eq!(attach_known_fix(&wrapped, &fix), wrapped);
    let failure = r#"Tool failed: {"error_code":"not_found","summary":"x"}"#;
    let attached = attach_known_fix(failure, &fix);
    let body: Value = serde_json::from_str(attached.strip_prefix("Tool failed: ").unwrap()).unwrap();
    assert_eq!((body["error_code"].as_str(), &body["known_fix"]), (Some("not_found"), &fix));
    assert!(attached.starts_with(r#"Tool failed: {"error_code":"not_found","summary":"x","known_fix":"#));
    let shell: Value = serde_json::from_str(&attach_known_fix(&shell_result(1, "e"), &fix)).unwrap();
    assert_eq!((shell["exit_code"].as_i64(), &shell["known_fix"]), (Some(1), &fix));
    assert_eq!(attach_known_fix(&attached, &json!({"steps":[]})), attached, "a fix is attached once");
    assert_eq!(attach_known_fix("Tool failed: boom", &fix), format!("Tool failed: boom\nknown_fix: {fix}"));
    // A spilled failure keeps its fix in the excerpt.
    let large = attach_known_fix(&shell_result(1, &"noise\n".repeat(20_000)), &fix);
    let excerpt: Value = serde_json::from_str(&crate::ui_tool_spill::excerpt(&large, "x.txt")).unwrap();
    assert_eq!(excerpt["known_fix"], fix);
}

#[test]
fn a_repaired_missing_dependency_is_suggested_until_it_keeps_failing() {
    let fixture = Fixture::new();
    let (_directory, store) = installed(&fixture);
    // Run 1: the script fails on a missing module, the agent installs it, the script runs.
    assert_eq!(shell(&fixture, "turn-a", "a1", "python3 extract.py in.pdf", 1, MISSING_A), None);
    assert_eq!(shell(&fixture, "turn-a", "a2", "uv pip install pdfplumber", 0, ""), None);
    assert_eq!(shell(&fixture, "turn-a", "a3", "python3 extract.py in.pdf", 0, ""), None);
    assert!(flush(&fixture.db, FLUSH_WAIT));
    let signature = failure_signature("shell", &shell_result(1, MISSING_A), false).unwrap();
    let fixes = store.fixes(&signature).unwrap();
    assert_eq!(fixes.len(), 1);
    assert_eq!((fixes[0].steps.as_slice(), fixes[0].successes, fixes[0].failures),
        (["shell: uv pip install pdfplumber".to_owned()].as_slice(), 1, 0));
    let failure = store.events("turn-a", 0, 20).unwrap().into_iter().find(|event| event.kind == JournalKind::Failure).unwrap();
    assert_eq!(failure.body["signature"], signature.digest());

    // Run 2: the same failure in another directory gets the fix.
    let fix = shell(&fixture, "turn-b", "b1", "python3 extract.py other.pdf", 1, MISSING_B).expect("known fix");
    assert_eq!((&fix["steps"], &fix["successes"]), (&json!(["shell: uv pip install pdfplumber"]), &json!(1)));
    assert_eq!(fix["last_seen"].as_u64(), Some(fixes[0].last_seen));
    // The fix runs and the failure repeats: 1 success, 1 failure is still a ratio of 0.5.
    shell(&fixture, "turn-b", "b2", "uv pip install pdfplumber", 0, "");
    assert!(shell(&fixture, "turn-b", "b3", "python3 extract.py other.pdf", 1, MISSING_B).is_some());
    assert!(flush(&fixture.db, FLUSH_WAIT));
    assert!(shell(&fixture, "turn-b", "b4", "uv pip install pdfplumber", 0, "").is_none());
    assert!(shell(&fixture, "turn-b", "b5", "python3 extract.py other.pdf", 1, MISSING_B).is_some());
    assert!(flush(&fixture.db, FLUSH_WAIT));
    // 1 of 3 is below 0.5: no longer suggested.
    assert_eq!(shell(&fixture, "turn-b", "b6", "python3 extract.py other.pdf", 1, MISSING_B), None);
    assert!(flush(&fixture.db, FLUSH_WAIT));
    let fixes = store.fixes(&signature).unwrap();
    assert_eq!((fixes.len(), fixes[0].successes, fixes[0].failures), (1, 1, 2));
}

#[test]
fn a_second_run_through_the_host_gets_the_known_fix_in_its_failure() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("learning.sqlite");
    let first = Fixture::new();
    install(&first.db, &path).unwrap();
    let (_, frames) = first.run(&failing_then_succeeding(), &first.tools(main_persona(Value::Null)));
    assert!(!result_content(&frames, "read-1").contains("known_fix"));
    assert!(flush(&first.db, FLUSH_WAIT));
    let failed = result_content(&frames, "read-1");
    let signature = failure_signature("read_file", &failed, true).unwrap();
    let fixes = store(&first.db).unwrap().fixes(&signature).unwrap();
    assert_eq!(fixes.iter().map(|fix| (fix.steps.clone(), fix.successes)).collect::<Vec<_>>(),
        [(vec!["write_file: outputs/notes.md".to_owned()], 1)]);

    // Another chat workspace, the same learning store.
    let second = Fixture::new();
    install(&second.db, &path).unwrap();
    let (message, frames) = second.run(&failing_then_succeeding(), &second.tools(main_persona(Value::Null)));
    assert_eq!(message, "Saved the notes as outputs/notes.md.");
    let content = result_content(&frames, "read-1");
    let body: Value = serde_json::from_str(content.strip_prefix("Tool failed: ").unwrap()).unwrap();
    assert_eq!(body["known_fix"]["steps"], json!(["write_file: outputs/notes.md"]), "{content}");
    assert_eq!(body["known_fix"]["successes"], 1);
    assert!(body["error_code"].is_string());
    // The second repair counts too.
    assert!(flush(&second.db, FLUSH_WAIT));
    assert_eq!(store(&second.db).unwrap().fixes(&signature).unwrap()[0].successes, 2);
}

#[test]
fn a_failed_fix_lookup_leaves_the_tool_result_unchanged() {
    let fixture = Fixture::new();
    let (directory, store) = installed(&fixture);
    rusqlite::Connection::open(directory.path().join("learning.sqlite")).unwrap()
        .execute_batch("DROP TABLE learn_failures;").unwrap();
    let signature = failure_signature("shell", &shell_result(1, MISSING_A), false).unwrap();
    assert!(store.known_fix(&signature).is_err());
    let host = fixture.tools(main_persona(Value::Null));
    let (message, frames) = fixture.run(&failing_then_succeeding(), &host);
    assert_eq!(message, "Saved the notes as outputs/notes.md.");
    assert_eq!(task_status(&frames).as_deref(), Some("verified"));
    let content = result_content(&frames, "read-1");
    assert!(content.starts_with("Tool failed: {") && !content.contains("known_fix"), "{content}");
    // The journal itself still works.
    assert!(flush(&fixture.db, FLUSH_WAIT));
    let key = crate::ui_task_completion::scope(&fixture.db, &fixture.turn).unwrap();
    assert!(store.events(&key, 0, 20).unwrap().iter().any(|event| event.kind == JournalKind::Repair));
}

// Vault values (task 23.2).

/// A vault that cannot be read.
struct Locked;
impl SecretSource for Locked {
    fn values(&self) -> io::Result<Vec<zeroize::Zeroizing<String>>> {
        Err(io::Error::other("The credential vault could not be read: locked"))
    }
}

#[test]
fn vault_values_are_redacted_from_entries_and_fixes_holding_them_are_not_learned() {
    // Not token-like, so only the vault scan can find it.
    const VALUE: &str = "plain-vault-entry-ab";
    let fixture = Fixture::new();
    let directory = tempfile::tempdir().unwrap();
    install_with(&fixture.db, &directory.path().join("learning.sqlite"), Box::new(kageha_learning::SecretValues::new([VALUE]))).unwrap();
    let store = store(&fixture.db).unwrap();
    let stderr = format!("{MISSING_A}login {VALUE} rejected\n");
    assert_eq!(scrub(&stderr), stderr, "the pattern scrub keeps the value");
    shell(&fixture, "turn-v", "v1", "python3 extract.py in.pdf", 1, &stderr);
    shell(&fixture, "turn-v", "v2", &format!("curl -u ana:{VALUE} https://example.com/setup"), 0, "");
    shell(&fixture, "turn-v", "v3", "python3 extract.py in.pdf", 0, "");
    crate::webui::emit(&fixture.db, CHAT, &fixture.turn, "event", json!({"kind":"shell_command",
        "payload":{"call_id":"s-1","command":format!("login --user ana {VALUE}"),"background":false,"exit_code":0}})).unwrap();
    assert!(flush(&fixture.db, FLUSH_WAIT));

    let events = store.events("turn-v", 0, 20).unwrap();
    let failure = events.iter().find(|event| event.kind == JournalKind::Failure).unwrap();
    assert!(failure.body["excerpt"].as_str().unwrap().contains("login [redacted] rejected"), "{:?}", failure.body);
    let tool = events.iter().find(|event| event.body["call_id"] == "v2").unwrap();
    assert!(tool.body["excerpt"].is_string());
    // The repair is journaled; the fix whose step holds the value is not learned.
    assert!(events.iter().any(|event| event.kind == JournalKind::Repair));
    let signature = failure_signature("shell", &shell_result(1, &stderr), false).unwrap();
    assert!(store.fixes(&signature).unwrap().is_empty());
    let key = crate::ui_task_completion::scope(&fixture.db, &fixture.turn).unwrap();
    let command = store.events(&key, 0, 20).unwrap().into_iter().find(|event| event.kind == JournalKind::ShellCommand).unwrap();
    assert_eq!(command.body["command"], "login --user ana [redacted]");
    let stored = format!("{events:?}{command:?}");
    assert!(!stored.contains(VALUE), "{stored}");
}

#[test]
fn an_unreadable_vault_writes_no_entries_and_the_turn_is_unaffected() {
    let fixture = Fixture::new();
    let directory = tempfile::tempdir().unwrap();
    install_with(&fixture.db, &directory.path().join("learning.sqlite"), Box::new(Locked)).unwrap();
    let store = store(&fixture.db).unwrap();
    let host = fixture.tools(main_persona(Value::Null));
    let (message, frames) = fixture.run(&failing_then_succeeding(), &host);
    assert_eq!(message, "Saved the notes as outputs/notes.md.");
    finish(&fixture, &frames, now_ms());
    let key = crate::ui_task_completion::scope(&fixture.db, &fixture.turn).unwrap();
    assert!(store.events(&key, 0, 20).unwrap().is_empty());
    // The run row holds no entry text and is still written, so reflection runs.
    assert!(store.run(&key).unwrap().is_some());
}

#[test]
fn browser_failures_never_receive_legacy_inferred_fixes() {
    let fixture = Fixture::new();
    let (_directory, store) = installed(&fixture);
    let content = r#"Tool failed: {"error_code":"tool_failed","summary":"This browser profile has no managed tab"}"#;
    let signature = failure_signature("browser_snapshot", content, true).unwrap();
    store.record_repair(&signature, &["amend_required_outcome: unrelated_province".into(), "browser_open".into()], "legacy fix").unwrap();
    let call = kageha_runtime::execution::ToolCall { id:"snapshot-failure".into(), name:"browser_snapshot".into(), arguments:json!({}) };
    assert!(known_fix(&fixture.db, &fixture.turn, &call, content, true).is_none());
    assert_eq!(store.fixes(&signature).unwrap().len(), 1, "Existing learning records remain intact");
}
