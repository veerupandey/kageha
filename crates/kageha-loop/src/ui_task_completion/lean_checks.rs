use super::*;
use crate::audited_session_tests::{main_persona, Fixture, CHAT};
use kageha_runtime::test_support::ScriptedModel;
use std::sync::{Arc, atomic::AtomicBool};

#[test]
fn declared_answer_and_evidenced_blocker_require_no_role_model_calls() {
    let fixture = Fixture::new();
    let mut tools = fixture.tools(main_persona(json!({})));
    let model = Arc::new(ScriptedModel::new([]));
    tools.role_providers.fakes.planner = Some(model.clone());
    tools.role_providers.fakes.reviewer = Some(model.clone());
    record(&fixture.db, CHAT, &fixture.turn,
        &json!({"outcomes":[{"id":"fetch","description":"Read the dataset","kind":"answer"}]}),
        &provable_kinds([])).unwrap();
    let control = Control { cancelled: &AtomicBool::new(false), deadline: None };
    assert_eq!(tools.task_completion_check("Dataset summary", &control).unwrap().status, CompletionStatus::Ready);
    crate::webui::insert_event(&fixture.db.lock().unwrap(), CHAT, &fixture.turn, "tool_card",
        json!({"id":"call-401","name":"web_fetch","status":"failed","result_preview":"401 Unauthorized"})).unwrap();
    let result = tools.task_report_blocker(&json!({"outcome_id":"fetch","category":"missing_access",
        "reason":"The dataset requires a login.","references":["call-401"]})).unwrap();
    assert_eq!(result["blocked"], true);
    assert!(model.requests().is_empty());
}

#[test]
fn unresolved_original_submission_overrides_an_affirmative_answer() {
    let fixture = Fixture::new();
    let host = fixture.tools(main_persona(json!({})));
    let root = scope(&fixture.db, &fixture.turn).unwrap();
    let key = format!("{CHAT}/submission-1");
    let mut record = json!({"chat":CHAT, "turn":fixture.turn, "scope":root,
        "request_id":"submission-1", "verified":false});
    let store = |value| ui_store::put(&fixture.db.lock().unwrap(), "application-submission", &key, value).unwrap();
    store(record.clone());
    let model = ScriptedModel::new([kageha_runtime::test_support::text_reply("Successfully submitted the application.")]);
    let (answer, _) = fixture.run(&model, &host);
    assert!(!answer.contains("Successfully submitted"));
    assert!(answer.contains("original receipt"));
    record["verified"] = json!(true);
    store(record);
    let check = host.task_completion_check("Confirmed", &Control { cancelled: &AtomicBool::new(false), deadline: None }).unwrap();
    assert_eq!(check.status, CompletionStatus::Ready);
    assert!(!check.uncertain_effect);
}
