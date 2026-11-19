use super::*;
use crate::audited_session_tests::{Fixture, CHAT};
use crate::model_router::{RoleProviders, TestModels};
use crate::ui_reflection::reflect_pending;
use kageha_learning::skills::PromotionState;
use kageha_learning::{JournalEntry, LearningPort, OutcomeStatus, SecretValues};
use kageha_runtime::test_support::{text_reply, ScriptedModel};
use kageha_skills::Package;
use serde_json::Value;
use std::sync::Arc;

/// A value held by the tests' credential vault ([`vault`]).
const VAULT_VALUE: &str = "vault-value-7f3a9c2e1b";

/// The tests' credential vault: a `SecretSource` holding [`VAULT_VALUE`].
fn vault() -> SecretValues {
    SecretValues::new([VAULT_VALUE])
}

const SCRIPT: &str = "from pathlib import Path\nPath('outputs').mkdir(exist_ok=True)\nPath('outputs/report.md').write_text('# Weekly report\\n')\n";
const PASSING_TEST: &str = "set -e\npython3 scripts/make_report.py\ngrep -q 'Weekly report' outputs/report.md\n";

struct Setup {
    fixture: Fixture,
    _directory: tempfile::TempDir,
    store: Arc<SqliteLearning>,
}

fn setup() -> Setup {
    let fixture = Fixture::new();
    let directory = tempfile::tempdir().unwrap();
    let store = Arc::new(SqliteLearning::open(&directory.path().join("learning.sqlite")).unwrap());
    Setup { fixture, _directory: directory, store }
}

/// Makes `replies` the planner's replies, through the `RoleProviders` test seam.
fn planner(setup: &Setup, replies: &[String]) -> Arc<ScriptedModel> {
    let model = Arc::new(ScriptedModel::new(replies.iter().map(text_reply)));
    let roles = RoleProviders { fakes: TestModels { planner: Some(model.clone()), reviewer: None }, ..Default::default() };
    *setup.fixture.db.reflection.test_roles.lock().unwrap() = Some(roles);
    model
}

fn no_lessons() -> String {
    json!({"lessons":[]}).to_string()
}

fn draft(name: &str, test_sh: &str) -> String {
    json!({"name":name,"description":"Write the weekly markdown report to outputs/report.md.",
        "instructions":"Use this when the user asks for the weekly report.\n\nRun `python3 scripts/make_report.py` from the workspace root; it writes outputs/report.md.",
        "script":null,"test_sh":test_sh}).to_string()
}

/// Request `request` of the chat: `python3 make_report.py` ran with exit code 0 and
/// produced `outputs/report.md`, which is the evidence of the `report` outcome when
/// `verified`. Closes the request, which queues its reflection.
fn script_request(setup: &Setup, request: &str, script: &str, verified: bool) {
    let db = &setup.fixture.db;
    let workspace = ui_workspace::open_for_chat(db, CHAT).unwrap();
    // Workspace writes update the manifest, so the changes are registered as returned.
    let changes = [
        workspace.write("make_report.py", script.as_bytes()).unwrap(),
        workspace.write("outputs/report.md", format!("# Weekly report ({request})\n").as_bytes()).unwrap(),
    ];
    ui_workspace::register_changes(db, CHAT, request, &changes).unwrap();
    ui_task_completion::record(db, CHAT, request,
        &json!({"outcomes":[{"id":"report","description":"The weekly report","kind":"artifact"}]}),
        &ui_task_completion::provable_kinds(["write_file"])).unwrap();
    if verified {
        ui_task_completion::attach(db, CHAT, request, &json!({"outcome_id":"report","reference":"workspace/outputs/report.md"})).unwrap();
    }
    setup.store.journal(request, JournalEntry { kind: JournalKind::ShellCommand, at: 1_000, body: json!({
        "call_id":"c1","command":"python3 make_report.py","background":false,"exit_code":0,"duration_ms":40,
        "files_changed":["workspace/make_report.py","workspace/outputs/report.md"]}) }).unwrap();
    setup.store.close_request(request, RequestSummary {
        agent: "main".into(), chat_id: CHAT.into(), task_type: Some("weekly-report".into()),
        status: if verified { "verified" } else { "partial" }.into(), model_calls: 4, cost_usd: 0.0, cached_tokens: 0,
        outcomes: vec![OutcomeStatus { id: "report".into(), status: if verified { "verified" } else { "unmet" }.into() }],
        started: 1_000, ended: 2_000,
    }).unwrap();
}

/// Whether skill tests can run here: the OS sandbox and a `python3` inside it.
fn sandbox_ready() -> bool {
    let probe = BTreeMap::from([(TEST_PATH.to_owned(), b"python3 -c 'print(1)'\n".to_vec())]);
    match sandbox_test(&probe) {
        Ok(()) => true,
        Err(reason) => {
            eprintln!("skipping sandbox-dependent skill promotion checks: {reason}");
            false
        }
    }
}

fn skill_events(setup: &Setup) -> Vec<Value> {
    let connection = setup.fixture.db.lock().unwrap();
    let mut query = connection.prepare("SELECT turn_id,payload FROM ui_events WHERE chat_id=? AND event='event' AND json_extract(payload,'$.kind')='skill_learned' ORDER BY sequence").unwrap();
    let rows = query.query_map([CHAT], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))).unwrap();
    rows.map(|row| {
        let (turn, payload) = row.unwrap();
        let mut payload: Value = serde_json::from_str(&payload).unwrap();
        payload["stored_turn"] = json!(turn);
        payload
    }).collect()
}

fn registry(setup: &Setup) -> Registry {
    ui_skills::owned(&setup.fixture.db).unwrap()
}

/// A promotion record: script, state, skill, revision and reason.
type Row = (String, PromotionState, Option<String>, Option<u64>, Option<String>);

fn promotions(setup: &Setup, request: &str) -> Vec<Row> {
    setup.store.promotions(request).unwrap().into_iter()
        .map(|row| (row.script, row.state, row.skill, row.revision, row.reason)).collect()
}

#[test]
fn a_verified_script_run_becomes_a_tested_learned_skill() {
    let setup = setup();
    script_request(&setup, "r1", SCRIPT, true);
    let model = planner(&setup, &[no_lessons(), draft("Make Report", PASSING_TEST)]);
    assert_eq!(reflect_pending(&setup.fixture.db, &setup.store, &vault()), 1);
    assert_eq!(setup.store.reflection_row("r1").unwrap().unwrap().state, "done");

    // The planner drafted the skill from the real script and the verified output.
    let requests = model.requests();
    assert_eq!(requests.len(), 2);
    let request = &requests[1];
    assert_eq!((request.role, request.tools.len(), request.messages[0]["content"].as_str()), (Role::Planner, 0, Some(SKILL_PROMPT)));
    let input: Value = serde_json::from_str(request.messages[1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(input["script"], SCRIPT);
    assert_eq!(input["command"], "python3 make_report.py");
    assert_eq!(input["package_script_path"], "scripts/make_report.py");
    assert_eq!(input["verified_outputs"], json!(["workspace/outputs/report.md"]));

    if !sandbox_ready() {
        let rows = promotions(&setup, "r1");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1, PromotionState::Dropped);
        assert!(registry(&setup).get("make-report").unwrap().is_none());
        return;
    }
    assert_eq!(promotions(&setup, "r1"), [("make_report.py".into(), PromotionState::Learned, Some("make-report".into()), Some(1), None)]);
    let registry = registry(&setup);
    let record = registry.get("make-report").unwrap().unwrap();
    assert_eq!((record.source.as_str(), record.revision, record.archived), ("learned", 1, false));
    assert_eq!(record.skill.metadata["source"], "learned");
    assert_eq!(record.skill.metadata["learned_from"], "r1");
    assert_eq!(registry.resource_paths("make-report").unwrap(), ["SKILL.md", "scripts/make_report.py", "test.sh"]);
    assert_eq!(registry.instructions("make-report").unwrap().lines().last(), Some("Run `python3 scripts/make_report.py` from the workspace root; it writes outputs/report.md."));
    let events = skill_events(&setup);
    assert_eq!(events.len(), 1, "{events:#?}");
    let payload = &events[0]["payload"];
    assert_eq!((payload["name"].as_str(), payload["revision"].as_u64(), payload["source"].as_str()), (Some("make-report"), Some(1), Some("learned")));
    assert_eq!((payload["script"].as_str(), payload["from_script"].as_str()), (Some("scripts/make_report.py"), Some("make_report.py")));
    assert_eq!(events[0]["stored_turn"], "r1");

    // Reflecting the request again does not test the script twice.
    assert!(promote(&setup.fixture.db, &setup.store, "r1", &vault()).is_empty());
}

#[test]
fn a_failing_test_stores_nothing() {
    let setup = setup();
    script_request(&setup, "r1", SCRIPT, true);
    planner(&setup, &[no_lessons(), draft("make-report", "echo 'report missing' >&2\nexit 3\n")]);
    reflect_pending(&setup.fixture.db, &setup.store, &vault());
    let rows = promotions(&setup, "r1");
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].1, rows[0].2.as_deref(), rows[0].3), (PromotionState::Dropped, Some("make-report"), None));
    if sandbox_ready() {
        let reason = rows[0].4.as_deref().unwrap();
        assert!(reason.contains("test.sh exited with 3") && reason.contains("report missing"), "{reason}");
    }
    assert!(registry(&setup).get("make-report").unwrap().is_none());
    assert!(skill_events(&setup).is_empty());
}

#[test]
fn an_unverified_run_or_an_invalid_draft_is_not_learned() {
    let setup = setup();
    script_request(&setup, "r1", SCRIPT, false);
    let model = planner(&setup, &[no_lessons()]);
    reflect_pending(&setup.fixture.db, &setup.store, &vault());
    // Only the lessons call: the run's output is not evidence, so there is no candidate.
    assert_eq!(model.requests().len(), 1);
    assert!(promotions(&setup, "r1").is_empty());

    script_request(&setup, "r2", SCRIPT, true);
    planner(&setup, &[no_lessons(), "Here is a nice skill for you.".into()]);
    reflect_pending(&setup.fixture.db, &setup.store, &vault());
    assert_eq!(promotions(&setup, "r2"), [("make_report.py".into(), PromotionState::Dropped, None, None,
        Some("The planner's skill draft was not a JSON object".into()))]);
    assert!(registry(&setup).list().unwrap().is_empty());
    assert!(skill_events(&setup).is_empty());
}

#[test]
fn a_secret_in_the_script_drops_it_before_drafting() {
    let setup = setup();
    script_request(&setup, "r1", &format!("TOKEN = '{VAULT_VALUE}'\n{SCRIPT}"), true);
    let model = planner(&setup, &[no_lessons(), draft("make-report", PASSING_TEST)]);
    reflect_pending(&setup.fixture.db, &setup.store, &vault());
    assert_eq!(model.requests().len(), 1, "the planner never sees the script");
    assert_eq!(promotions(&setup, "r1"), [("make_report.py".into(), PromotionState::Dropped, None, None,
        Some("The script contains a secret (vault_value)".into()))]);

    // A secret the planner put in the test drops the package.
    script_request(&setup, "r2", SCRIPT, true);
    planner(&setup, &[no_lessons(), draft("make-report", &format!("export TOKEN={VAULT_VALUE}\n{PASSING_TEST}"))]);
    reflect_pending(&setup.fixture.db, &setup.store, &vault());
    assert_eq!(promotions(&setup, "r2"), [("make_report.py".into(), PromotionState::Dropped, Some("make-report".into()), None,
        Some("The skill's test.sh contains a secret (vault_value)".into()))]);
    assert!(registry(&setup).list().unwrap().is_empty());
    // The drop records name the kind of match, never the value.
    let recorded = format!("{:?}{:?}", promotions(&setup, "r1"), promotions(&setup, "r2"));
    assert!(!recorded.contains(VAULT_VALUE), "{recorded}");

    // A secret pattern drops the script without any vault value.
    let token = format!("ghp_{}", "A1b2".repeat(9));
    script_request(&setup, "r3", &format!("TOKEN = '{token}'\n{SCRIPT}"), true);
    planner(&setup, &[no_lessons(), draft("make-report", PASSING_TEST)]);
    reflect_pending(&setup.fixture.db, &setup.store, &kageha_learning::NoSecrets);
    assert_eq!(promotions(&setup, "r3"), [("make_report.py".into(), PromotionState::Dropped, None, None,
        Some("The script contains a secret (github_token)".into()))]);
}

#[test]
fn the_same_name_again_adds_a_revision_and_keeps_the_earlier_one() {
    if !sandbox_ready() {
        return;
    }
    let setup = setup();
    script_request(&setup, "r1", SCRIPT, true);
    planner(&setup, &[no_lessons(), draft("make-report", PASSING_TEST)]);
    reflect_pending(&setup.fixture.db, &setup.store, &vault());
    script_request(&setup, "r2", &format!("# v2\n{SCRIPT}"), true);
    planner(&setup, &[no_lessons(), draft("make-report", PASSING_TEST)]);
    reflect_pending(&setup.fixture.db, &setup.store, &vault());
    assert_eq!(promotions(&setup, "r2"), [("make_report.py".into(), PromotionState::Learned, Some("make-report".into()), Some(2), None)]);
    let registry = registry(&setup);
    let versions: Vec<(u64, String, Value)> = registry.versions("make-report").unwrap().into_iter()
        .map(|record| (record.revision, record.source, record.skill.metadata["learned_from"].clone())).collect();
    assert_eq!(versions, [(1, "learned".into(), json!("r1")), (2, "learned".into(), json!("r2"))]);
    assert!(registry.resource("make-report", "scripts/make_report.py").unwrap().starts_with(b"# v2\n"));
    let revisions: Vec<_> = skill_events(&setup).iter().map(|event| event["payload"]["revision"].clone()).collect();
    assert_eq!(revisions, [json!(1), json!(2)]);
}

#[test]
fn a_name_used_by_a_skill_that_was_not_learned_gets_a_suffix() {
    let setup = setup();
    let operator = Package::plain("make-report", "The operator's own report instructions").unwrap();
    registry(&setup).install(&operator, "operator", None).unwrap();
    {
        let registry = registry(&setup);
        assert_eq!(learned_target(&registry, "make-report").unwrap(), ("make-report-learned".into(), 0));
    }
    if !sandbox_ready() {
        return;
    }
    script_request(&setup, "r1", SCRIPT, true);
    planner(&setup, &[no_lessons(), draft("make-report", PASSING_TEST)]);
    reflect_pending(&setup.fixture.db, &setup.store, &vault());
    assert_eq!(promotions(&setup, "r1"), [("make_report.py".into(), PromotionState::Learned, Some("make-report-learned".into()), Some(1), None)]);
    let registry = registry(&setup);
    let kept = registry.get("make-report").unwrap().unwrap();
    assert_eq!((kept.source.as_str(), kept.revision, kept.skill.body.as_str()), ("operator", 1, "The operator's own report instructions"));
    assert_eq!(registry.get("make-report-learned").unwrap().unwrap().source, "learned");
}

#[test]
fn a_learned_skill_the_user_deleted_is_not_learned_again() {
    let setup = setup();
    let learned = Package::plain("make-report", "Learned earlier").unwrap();
    let mut owned = registry(&setup);
    owned.install(&learned, LEARNED_SOURCE, None).unwrap();
    owned.change("make-report", "delete", None).unwrap();
    assert!(learned_target(&owned, "make-report").unwrap_err().contains("deleted by the user"));
    // An archived (disabled) learned skill gets new revisions and stays archived.
    owned.change("make-report", "restore", None).unwrap();
    owned.change("make-report", "archive", None).unwrap();
    assert_eq!(learned_target(&owned, "make-report").unwrap(), ("make-report".into(), 4));
}
