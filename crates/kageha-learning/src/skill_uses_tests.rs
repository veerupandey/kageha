use super::*;
use crate::{JournalEntry, LearningPort};
use serde_json::json;

fn store() -> (tempfile::TempDir, SqliteLearning) {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteLearning::open(&root.path().join("learning.sqlite")).unwrap();
    (root, store)
}

fn event(seq: u64, kind: JournalKind, body: Value) -> StoredEvent {
    StoredEvent { seq, kind, body, at: 0 }
}

#[test]
fn a_read_is_recorded_once_per_request_and_decided_later() {
    let (_root, store) = store();
    assert!(store.record_skill_use("digest", 3, "req-1", 1_000).unwrap());
    // A second read in the same request keeps the first.
    assert!(!store.record_skill_use("digest", 4, "req-1", 2_000).unwrap());
    let used = store.skill_use("digest", "req-1").unwrap().unwrap();
    assert_eq!((used.revision, used.success, used.at), (3, None, 1_000));
    // Undecided uses are not results.
    assert!(store.recent_skill_results("digest", 0, 10).unwrap().is_empty());
    assert!(store.decide_skill_use("digest", "req-1", true).unwrap());
    assert!(!store.decide_skill_use("other", "req-1", true).unwrap());
    assert_eq!(store.skill_uses("req-1").unwrap()[0].success, Some(true));
    // Deciding again (a resumed request closed again) replaces the result.
    store.decide_skill_use("digest", "req-1", false).unwrap();
    assert_eq!(store.recent_skill_results("digest", 0, 10).unwrap(), [false]);
    assert!(store.record_skill_use("", 1, "req-1", 1).is_err());
}

#[test]
fn recent_results_are_newest_first_and_start_at_the_content_revision() {
    let (_root, store) = store();
    for n in 0..12u64 {
        let request = format!("req-{n}");
        store.record_skill_use("digest", if n < 2 { 1 } else { 5 }, &request, 1_000 + n).unwrap();
        store.decide_skill_use("digest", &request, n % 2 == 0).unwrap();
    }
    let all = store.recent_skill_results("digest", 0, 100).unwrap();
    assert_eq!(all.len(), 12);
    assert_eq!(&all[..2], [false, true]);
    // Uses of the earlier package (revision 1) are not in the window of revision 5.
    assert_eq!(store.recent_skill_results("digest", 5, 100).unwrap().len(), 10);
    assert_eq!(store.recent_skill_results("digest", 5, 3).unwrap().len(), 3);
}

#[test]
fn the_degrade_rule_needs_a_full_window_and_six_successes() {
    assert!(!should_degrade(&[false; 9]));
    assert!(should_degrade(&[false; 10]));
    let five = [true, false, true, false, true, false, true, false, true, false];
    assert!(should_degrade(&five));
    let six = [true, true, false, true, false, true, false, true, false, true];
    assert!(!should_degrade(&six));
    // Only the newest 10 count.
    let mut older_successes = five.to_vec();
    older_successes.extend([true; 10]);
    assert!(should_degrade(&older_successes));
}

#[test]
fn a_use_succeeds_on_verified_evidence_produced_after_the_read() {
    let verified = |produced: Vec<Option<u64>>| OutcomeEvidence { verified: true, produced };
    // Evidence produced after the read.
    assert!(use_succeeded(Some(5), &[verified(vec![Some(9)])], "partial"));
    // Only evidence from before the read.
    assert!(!use_succeeded(Some(5), &[verified(vec![Some(3)])], "verified"));
    // Unverified outcomes do not count.
    let unverified = OutcomeEvidence { verified: false, produced: vec![Some(9)] };
    assert!(!use_succeeded(Some(5), &[unverified.clone()], "verified"));
    assert!(use_succeeded(Some(5), &[unverified, verified(vec![Some(2), Some(7)])], "partial"));
    // An answer (no evidence) is produced at the end; unplaced evidence counts as later.
    assert!(use_succeeded(Some(5), &[verified(vec![])], "verified"));
    assert!(use_succeeded(Some(5), &[verified(vec![None])], "verified"));
    assert!(use_succeeded(None, &[verified(vec![Some(1)])], "verified"));
    // Without outcomes the final status decides.
    assert!(use_succeeded(Some(5), &[], "verified"));
    assert!(use_succeeded(Some(5), &[], "not_required"));
    assert!(!use_succeeded(Some(5), &[], "partial"));
    assert!(!use_succeeded(Some(5), &[], "blocked"));
}

#[test]
fn journal_positions_of_reads_and_evidence() {
    let events = [
        event(1, JournalKind::SkillUsed, json!({"skill":"shown","tool":"context"})),
        event(2, JournalKind::Tool, json!({"call_id":"w-1","status":"done","produced":["workspace/outputs/a.md"]})),
        event(3, JournalKind::SkillUsed, skill_used_body("digest", 2, "s-1", "skill_read")),
        event(4, JournalKind::Tool, json!({"call_id":"s-1","status":"done"})),
        event(5, JournalKind::ShellCommand, json!({"call_id":"sh-1","exit_code":0,"files_changed":["workspace/outputs/b.md"]})),
        event(6, JournalKind::Tool, json!({"call_id":"sh-1","status":"done","produced":[{"artifact_path":"workspace/outputs/b.md"}]})),
        event(7, JournalKind::SkillUsed, skill_used_body("digest", 2, "s-2", "load_skill")),
        event(8, JournalKind::Tool, json!({"call_id":"w-2","status":"failed","produced":["workspace/outputs/a.md"]})),
    ];
    let reads = skill_read_positions(&events);
    assert_eq!(reads, BTreeMap::from([("digest".to_owned(), 3)]));
    let produced = evidence_positions(&events);
    assert_eq!(produced["workspace/outputs/a.md"], 2, "a failed call produced nothing");
    assert_eq!(produced["workspace/outputs/b.md"], 6);
    assert_eq!(produced["sh-1"], 6);
    assert!(!produced.contains_key("w-2"));
}

#[test]
fn reads_written_to_the_journal_are_found_again() {
    let (_root, store) = store();
    store.journal("req", JournalEntry { kind: JournalKind::SkillUsed, body: skill_used_body("digest", 1, "s-1", "skill_read"), at: 1 }).unwrap();
    store.journal("req", JournalEntry { kind: JournalKind::Tool, body: json!({"call_id":"v-1","status":"done"}), at: 2 }).unwrap();
    let events = store.events("req", 0, 10).unwrap();
    let read = skill_read_positions(&events)["digest"];
    let evidence = evidence_positions(&events)["v-1"];
    assert!(use_succeeded(Some(read), &[OutcomeEvidence { verified: true, produced: vec![Some(evidence)] }], "verified"));
}
