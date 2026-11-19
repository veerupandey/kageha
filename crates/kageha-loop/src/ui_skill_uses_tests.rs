use super::*;
use crate::audited_session_tests::{main_persona, Fixture, CHAT};
use crate::ui_learning::{finish_turn, install, store, turn_status};
use kageha_learning::skill_uses::skill_used_body;
use kageha_learning::{JournalEntry, JournalKind, LearningPort, OutcomeStatus};
use kageha_runtime::test_support::{text_reply, tool_call, tool_calls_reply, ScriptedModel};
use kageha_contracts::execution::AssistantMessage;
use kageha_skills::Package;
use serde_json::Value;
use std::{collections::BTreeMap as Map, sync::Arc, time::{SystemTime, UNIX_EPOCH}};

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}

fn installed(fixture: &Fixture) -> (tempfile::TempDir, Arc<SqliteLearning>) {
    let directory = tempfile::tempdir().unwrap();
    install(&fixture.db, &directory.path().join("learning.sqlite")).unwrap();
    (directory, store(&fixture.db).unwrap())
}

fn install_skill(fixture: &Fixture, body: &str) -> Record {
    let package = Package::plain("digest", &format!("---\nname: digest\ndescription: Summarize the inbox into a digest file.\n---\n{body}")).unwrap();
    ui_skills::owned(&fixture.db).unwrap().install(&package, "learned", None).unwrap()
}

fn summary(status: &str, outcomes: Vec<OutcomeStatus>) -> RequestSummary {
    RequestSummary { agent: "main".into(), chat_id: CHAT.into(), task_type: None, status: status.into(), model_calls: 1,
        cost_usd: 0.0, cached_tokens: 0, outcomes, started: 1, ended: 2 }
}

fn task_status(frames: &[Value]) -> Option<String> {
    frames.iter().rev().find(|frame| frame["kind"] == "task_completion")
        .and_then(|frame| frame["status"].as_str().map(str::to_owned))
}

fn outcomes() -> Value {
    json!({"outcomes":[{"id":"digest","description":"Inbox digest file","kind":"artifact","verify":"outputs/digest.md exists"}]})
}

/// Runs the scripted turn and closes its request. Returns the request id.
fn run_and_close(fixture: &Fixture, replies: Vec<AssistantMessage>) -> String {
    let host = fixture.tools(main_persona(Value::Null));
    let model = ScriptedModel::new(replies);
    let (_, frames) = fixture.run(&model, &host);
    let status = turn_status(false, "completed", task_status(&frames).as_deref());
    finish_turn(&fixture.db, CHAT, &fixture.turn, 0, "main", status, now_ms());
    ui_task_completion::scope(&fixture.db, &fixture.turn).unwrap()
}

#[test]
fn a_skill_read_is_a_use_that_succeeds_when_later_evidence_is_verified() {
    let fixture = Fixture::new();
    let (_directory, store) = installed(&fixture);
    let record = install_skill(&fixture, "Write outputs/digest.md.");
    let request = run_and_close(&fixture, vec![
        tool_calls_reply([tool_call("record-1", "record_required_outcomes", outcomes())]),
        tool_calls_reply([tool_call("skill-1", "skill_read", json!({"name":"digest"}))]),
        tool_calls_reply([tool_call("write-1", "write_file", json!({"path":"outputs/digest.md","content":"# Digest\n"}))]),
        tool_calls_reply([tool_call("evidence-1", "record_outcome_evidence",
            json!({"outcome_id":"digest","reference":"workspace/outputs/digest.md"}))]),
        text_reply("Saved the digest as outputs/digest.md."),
    ]);
    let used = store.skill_use("digest", &request).unwrap().expect("the read is a use");
    assert_eq!((used.revision, used.success), (record.revision, Some(true)));
    let events = store.events(&request, 0, 100).unwrap();
    let entry = events.iter().find(|event| event.kind == JournalKind::SkillUsed).expect("skill_used is journaled");
    assert_eq!(entry.body, skill_used_body("digest", record.revision, "skill-1", "skill_read"));
    let write = events.iter().find(|event| event.body["call_id"] == "write-1" && event.kind == JournalKind::Tool).unwrap();
    assert_eq!(write.body["produced"], json!(["workspace/outputs/digest.md"]));
    assert!(entry.seq < write.seq);
}

#[test]
fn evidence_produced_before_the_read_does_not_make_the_use_a_success() {
    let fixture = Fixture::new();
    let (_directory, store) = installed(&fixture);
    install_skill(&fixture, "Write outputs/digest.md.");
    let request = run_and_close(&fixture, vec![
        tool_calls_reply([tool_call("record-1", "record_required_outcomes", outcomes())]),
        tool_calls_reply([tool_call("write-1", "write_file", json!({"path":"outputs/digest.md","content":"# Digest\n"}))]),
        tool_calls_reply([tool_call("evidence-1", "record_outcome_evidence",
            json!({"outcome_id":"digest","reference":"workspace/outputs/digest.md"}))]),
        // The old alias is a read too.
        tool_calls_reply([tool_call("skill-1", "load_skill", json!({"name":"digest"}))]),
        text_reply("Saved the digest as outputs/digest.md."),
    ]);
    assert_eq!(store.run(&request).unwrap().unwrap().status, "verified");
    assert_eq!(store.skill_use("digest", &request).unwrap().unwrap().success, Some(false));
}

#[test]
fn a_request_without_outcomes_is_decided_by_its_final_status() {
    let fixture = Fixture::new();
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteLearning::open(&directory.path().join("learning.sqlite")).unwrap();
    for (request, status, expected) in [("r-verified", "verified", true), ("r-none", "not_required", true),
        ("r-partial", "partial", false), ("r-blocked", "blocked", false)] {
        store.journal(request, JournalEntry { kind: JournalKind::SkillUsed, at: 1,
            body: skill_used_body("digest", 1, "s-1", "skill_read") }).unwrap();
        store.record_skill_use("digest", 1, request, 1).unwrap();
        let decisions = decide(&fixture.db, &store, request, &summary(status, vec![])).unwrap();
        assert_eq!((decisions.len(), decisions[0].success), (1, expected), "{status}");
        assert_eq!(store.skill_use("digest", request).unwrap().unwrap().success, Some(expected));
    }
}

#[test]
fn five_successes_in_ten_uses_degrade_the_skill_and_restore_the_previous_revision() {
    let fixture = Fixture::new();
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteLearning::open(&directory.path().join("learning.sqlite")).unwrap();
    let first = install_skill(&fixture, "Version one.");
    let second = install_skill(&fixture, "Version two.");
    // Nine decided uses of version two, five of them successful.
    for n in 0..9u64 {
        let request = format!("r-{n}");
        store.record_skill_use("digest", second.revision, &request, 1_000 + n).unwrap();
        store.decide_skill_use("digest", &request, n < 5).unwrap();
    }
    // A failed tenth use: five of the last ten succeeded.
    store.record_skill_use("digest", second.revision, "r-9", now_ms()).unwrap();
    let decisions = decide(&fixture.db, &store, "r-9", &summary("partial", vec![])).unwrap();
    let degraded = decisions[0].degraded.as_ref().expect("the skill is degraded");
    assert!(!decisions[0].success);
    assert_eq!(degraded.restored_from, Some(first.revision));
    let registry = ui_skills::owned(&fixture.db).unwrap();
    let active = registry.get("digest").unwrap().unwrap();
    assert!(active.degraded);
    assert_eq!(active.package_sha256, first.package_sha256);
    assert!(registry.instructions("digest").unwrap().ends_with("Version one."));
    // A degraded skill is not suggested, but stays listed for the user and readable by name.
    let persona = main_persona(Value::Null);
    assert!(ui_skills::model_catalog_for(&fixture.db, &persona, true).unwrap().is_empty());
    assert!(!ui_skills::context(&fixture.db, &persona, true).unwrap().contains("digest"));
    assert!(registry.list().unwrap().iter().any(|record| record.skill.name == "digest"));
    assert!(ui_skills::load_for_model(&fixture.db, &persona, "digest", true).unwrap().text.contains("Version one."));
    let emitted: i64 = fixture.db.lock().unwrap().query_row(
        "SELECT COUNT(*) FROM ui_events WHERE chat_id=? AND json_extract(payload,'$.kind')='skill_degraded'", [CHAT],
        |row| row.get(0)).unwrap();
    assert_eq!(emitted, 1);
    // Another failure does not degrade it again.
    store.record_skill_use("digest", active.revision, "r-10", now_ms() + 1).unwrap();
    assert!(decide(&fixture.db, &store, "r-10", &summary("partial", vec![])).unwrap()[0].degraded.is_none());
}

#[test]
fn six_successes_in_ten_uses_keep_the_skill() {
    let fixture = Fixture::new();
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteLearning::open(&directory.path().join("learning.sqlite")).unwrap();
    let record = install_skill(&fixture, "Version one.");
    for n in 0..9u64 {
        let request = format!("r-{n}");
        store.record_skill_use("digest", record.revision, &request, 1_000 + n).unwrap();
        store.decide_skill_use("digest", &request, n < 6).unwrap();
    }
    store.record_skill_use("digest", record.revision, "r-9", now_ms()).unwrap();
    let decisions = decide(&fixture.db, &store, "r-9", &summary("partial", vec![])).unwrap();
    assert!(decisions[0].degraded.is_none());
    assert!(!ui_skills::owned(&fixture.db).unwrap().get("digest").unwrap().unwrap().degraded);
}

#[test]
fn a_learned_python_skill_is_offered_when_the_turn_has_shell() {
    let fixture = Fixture::new();
    let files = Map::from([
        ("SKILL.md".to_owned(), b"---\nname: report\ndescription: Build the weekly report.\nmetadata:\n  required_resources: [scripts/report.py]\n---\nRun `python3 scripts/report.py`.".to_vec()),
        ("scripts/report.py".to_owned(), b"print('report')\n".to_vec()),
    ]);
    let package = Package::from_files("report", files).unwrap();
    let record = ui_skills::owned(&fixture.db).unwrap().install(&package, "learned", None).unwrap();
    assert!(record.required_runtimes.contains("python"));
    let persona = main_persona(Value::Null);
    let names = |shell| ui_skills::model_catalog_for(&fixture.db, &persona, shell).unwrap()
        .into_iter().map(|(name, _)| name).collect::<Vec<_>>();
    assert!(names(false).is_empty());
    assert_eq!(names(true), ["report"]);
    assert!(ui_skills::load_for_model(&fixture.db, &persona, "report", false).is_err());
    let loaded = ui_skills::load_for_model(&fixture.db, &persona, "report", true).unwrap();
    assert!(loaded.text.contains("skills/report/scripts/report.py"), "{}", loaded.text);
    assert_eq!(loaded.revision, record.revision);
    assert_eq!(ui_skills::resource(&fixture.db, &persona, "skills/report/scripts/report.py", true).unwrap(), "print('report')\n");
    assert!(ui_skills::resource(&fixture.db, &persona, "skills/report/scripts/report.py", false).is_err());
    assert!(ui_skills::context(&fixture.db, &persona, true).unwrap().contains("\"report\""));
    assert!(!ui_skills::context(&fixture.db, &persona, false).unwrap().contains("\"report\""));
    // A runtime the shell cannot provide stays filtered.
    let mut other = record.clone();
    other.required_runtimes.insert("ruby".into());
    assert!(!ui_skills::runtimes_available(&other, true));
    assert!(ui_skills::runtimes_available(&record, true) && !ui_skills::runtimes_available(&record, false));
}
