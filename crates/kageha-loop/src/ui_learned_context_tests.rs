use super::*;
use crate::audited_session_tests::{main_persona, Fixture, CHAT};
use crate::turn::Start;
use kageha_learning::{SqliteLearning, CONTEXT_HEADER};
use kageha_runtime::test_support::{text_reply, tool_call, tool_calls_reply, ScriptedModel};
use std::{path::Path, sync::{atomic::AtomicBool, Arc}};

const PROMPT: &str = "Extract the tables from the scanned PDF report";

fn installed(fixture: &Fixture) -> (tempfile::TempDir, Arc<SqliteLearning>) {
    let directory = tempfile::tempdir().unwrap();
    crate::ui_learning::install(&fixture.db, &directory.path().join("learning.sqlite")).unwrap();
    let store = crate::ui_learning::store(&fixture.db).unwrap();
    (directory, store)
}

/// Inserts a lesson straight into the store file and returns its id.
fn lesson(directory: &Path, agent: &str, task_type: &str, trigger: &str, action: &str, status: &str) -> i64 {
    let connection = rusqlite::Connection::open(directory.join("learning.sqlite")).unwrap();
    connection.execute(
        "INSERT INTO learn_lessons(agent,task_type,\"trigger\",action,status,support,shown,created) VALUES(?,?,?,?,?,'[\"r1\"]',0,?)",
        rusqlite::params![agent, task_type, trigger, action, status,
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64],
    ).unwrap();
    connection.last_insert_rowid()
}

fn shown(directory: &Path, id: i64) -> i64 {
    rusqlite::Connection::open(directory.join("learning.sqlite")).unwrap()
        .query_row("SELECT shown FROM learn_lessons WHERE id=?", [id], |r| r.get(0)).unwrap()
}

fn user(text: &str) -> Vec<Value> {
    vec![json!({"role":"user","content":text})]
}

#[test]
fn the_block_is_built_from_active_lessons_and_its_uses_are_journaled_and_decided_at_close() {
    let fixture = Fixture::new();
    let (directory, store) = installed(&fixture);
    let ocr = lesson(directory.path(), "main", "pdf-reports", "A PDF has no text layer", "run OCR with `ocrmypdf` before extraction", "active");
    lesson(directory.path(), "main", "pdf-reports", "A PDF report is scanned", "guess the numbers", "candidate");
    let host = fixture.tools(main_persona(Value::Null));

    let block = message(&host, &user(PROMPT)).expect("a relevant lesson is shown");
    assert_eq!(block["role"], "system");
    let text = block["content"].as_str().unwrap();
    assert_eq!(text, format!("{CONTEXT_HEADER}\nL-{ocr} (pdf-reports): When a PDF has no text layer, run OCR with `ocrmypdf` before extraction."));
    assert!(!text.contains("guess the numbers"));

    let key = crate::ui_task_completion::scope(&fixture.db, &fixture.turn).unwrap();
    assert_eq!(shown(directory.path(), ocr), 1);
    assert_eq!(store.lesson_uses(&key).unwrap(), [(ocr, None)]);

    crate::ui_learning::finish_turn(&fixture.db, CHAT, &fixture.turn, 0, "main", "verified", 0);
    let events = store.events(&key, 0, 100).unwrap();
    let used: Vec<_> = events.iter().filter(|event| event.kind == JournalKind::LessonUsed).collect();
    assert_eq!(used.len(), 1, "{events:#?}");
    assert_eq!(used[0].body, json!({"lesson_id":ocr,"via":"context"}));
    // Verified is decided when the request closes.
    assert_eq!(store.lesson_uses(&key).unwrap(), [(ocr, Some(true))]);
    assert_eq!(store.run(&key).unwrap().unwrap().status, "verified");
}

#[test]
fn no_store_or_nothing_relevant_gives_no_block() {
    let fixture = Fixture::new();
    let host = fixture.tools(main_persona(Value::Null));
    assert!(message(&host, &user(PROMPT)).is_none());
    let (directory, _store) = installed(&fixture);
    lesson(directory.path(), "main", "email", "A Gmail search is empty", "widen the date range", "active");
    assert!(message(&host, &user(PROMPT)).is_none());
}

#[test]
fn request_text_reads_the_last_user_message() {
    let messages = vec![
        json!({"role":"user","content":"first"}),
        json!({"role":"assistant","content":"reply"}),
        json!({"role":"user","content":[{"type":"text","text":"scanned PDF"},{"type":"image_url","image_url":{"url":"x"}}]}),
    ];
    assert_eq!(request_text(&messages), "scanned PDF");
    assert_eq!(request_text(&[]), "");
}

#[test]
fn every_step_of_a_turn_sends_the_same_block_at_the_same_position() {
    let fixture = Fixture::new();
    let (directory, _store) = installed(&fixture);
    lesson(directory.path(), "main", "pdf-reports", "A PDF has no text layer", "run OCR first", "active");
    let host = fixture.tools(main_persona(Value::Null));
    // As `webui::execute` assembles a new turn: system guidance, then the block.
    let mut messages = vec![json!({"role":"system","content":"Guidance."})];
    messages.extend(user(PROMPT));
    let block = message(&host, &messages).unwrap();
    messages.push(block.clone());
    let model = ScriptedModel::new([
        tool_calls_reply([tool_call("read-1", "read_file", json!({"path":"workspace/report.md"}))]),
        tool_calls_reply([tool_call("read-2", "read_file", json!({"path":"workspace/tables.md"}))]),
        text_reply("No report file exists yet."),
        text_reply("No report file exists yet."),
        text_reply("No report file exists yet."),
    ]);
    let cancel = AtomicBool::new(false);
    let start = Start { version: 1, max_model_calls: 8, messages };
    crate::native_chat::execute_hosted_with_model(host.workspace, start, &model, &host, &cancel, |_| Ok(())).unwrap();

    let requests = model.requests();
    assert!(requests.len() >= 3, "{}", requests.len());
    let positions = |request: &kageha_contracts::execution::ModelRequest| -> Vec<usize> {
        request.messages.iter().enumerate()
            .filter(|(_, message)| message["content"].as_str().is_some_and(|text| text.starts_with(CONTEXT_HEADER)))
            .map(|(index, _)| index)
            .collect()
    };
    // One block, at the same position and with the same text, in every step.
    let first = positions(&requests[0]);
    assert_eq!(first.len(), 1, "{:#?}", requests[0].messages);
    for request in &requests {
        assert_eq!(positions(request), first, "{:#?}", request.messages);
        assert_eq!(request.messages[first[0]], block);
    }
}

/// Closes `request` in the store with `status` and the `pdf-reports` task type.
fn closed(store: &SqliteLearning, request: &str, status: &str) {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    kageha_learning::LearningPort::close_request(store, request, kageha_learning::RequestSummary {
        agent: "main".into(), chat_id: CHAT.into(), task_type: Some("pdf-reports".into()), status: status.into(),
        model_calls: 1, cost_usd: 0.0, cached_tokens: 0, outcomes: Vec::new(), started: now - 1000, ended: now,
    }).unwrap();
}

#[test]
fn closing_the_tenth_request_that_showed_an_underperforming_lesson_retires_it() {
    let fixture = Fixture::new();
    let (directory, store) = installed(&fixture);
    let ocr = lesson(directory.path(), "main", "pdf-reports", "A PDF has no text layer", "run OCR with `ocrmypdf` before extraction", "active");
    // Nine earlier requests showed the lesson and were not verified; ten without it were.
    for n in 0..9 {
        let request = format!("earlier-{n}");
        store.record_lesson_uses(&request, &[ocr]).unwrap();
        closed(&store, &request, "partial");
        store.decide_lesson_uses(&request, false).unwrap();
    }
    for n in 0..10 {
        closed(&store, &format!("without-{n}"), "verified");
    }
    let host = fixture.tools(main_persona(Value::Null));
    assert!(message(&host, &user(PROMPT)).is_some(), "the lesson is still shown");
    // The tenth request closes unverified on the Host's close path.
    crate::ui_learning::finish_turn(&fixture.db, CHAT, &fixture.turn, 0, "main", "partial", 0);
    let status: (String, Option<i64>) = rusqlite::Connection::open(directory.path().join("learning.sqlite")).unwrap()
        .query_row("SELECT status,retired_at FROM learn_lessons WHERE id=?", [ocr], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
    assert_eq!(status.0, "retired");
    assert!(status.1.is_some());
    // A retired lesson is not shown again.
    assert!(store.context_with("main", PROMPT, Some("pdf-reports"), &[]).unwrap().lesson_ids.is_empty());
}
