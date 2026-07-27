use super::*;
use crate::LearningPort;
use serde_json::json;

const DAY: u64 = 24 * 3600 * 1000;
const NOW: u64 = 400 * DAY;

fn store() -> (tempfile::TempDir, SqliteLearning) {
    let root = tempfile::tempdir().unwrap();
    let store = SqliteLearning::open(&root.path().join("learning.sqlite")).unwrap();
    (root, store)
}

/// Inserts a lesson and returns its id.
fn lesson(
    store: &SqliteLearning,
    agent: &str,
    task_type: &str,
    trigger: &str,
    action: &str,
    status: &str,
    support: &[&str],
    created: u64,
) -> i64 {
    let db = store.lock().unwrap();
    db.execute(
        "INSERT INTO learn_lessons(agent,task_type,\"trigger\",action,status,support,shown,created) VALUES(?,?,?,?,?,?,0,?)",
        params![agent, task_type, trigger, action, status, json!(support).to_string(), created as i64],
    )
    .unwrap();
    db.last_insert_rowid()
}

fn shown(store: &SqliteLearning, id: i64) -> i64 {
    store
        .lock()
        .unwrap()
        .query_row("SELECT shown FROM learn_lessons WHERE id=?", [id], |r| r.get(0))
        .unwrap()
}

fn skill(name: &str, revision: u64, description: &str) -> SkillCandidate {
    SkillCandidate {
        name: name.into(),
        revision,
        description: description.into(),
        task_type: None,
        degraded: false,
    }
}

const OCR_TRIGGER: &str = "A PDF has no text layer";
const OCR_ACTION: &str = "run OCR with `ocrmypdf` before extraction";

#[test]
fn the_block_has_the_fixed_format_and_lists_the_ids_it_shows() {
    let (_root, store) = store();
    let id = lesson(&store, "main", "pdf-reports", OCR_TRIGGER, OCR_ACTION, "active", &["r1"], NOW);
    let skills = [skill("gmail-digest", 3, "Summarize a Gmail thread to markdown")];
    let context = store
        .context_at("main", "Extract the tables from this PDF report and summarize my Gmail thread", None, &skills, NOW)
        .unwrap();
    let lines: Vec<&str> = context.text.lines().collect();
    assert_eq!(lines[0], CONTEXT_HEADER);
    assert!(lines.contains(&format!("L-{id} (pdf-reports): When a PDF has no text layer, run OCR with `ocrmypdf` before extraction.").as_str()), "{}", context.text);
    assert!(lines.contains(&"S-gmail-digest v3: Summarize a Gmail thread to markdown. Use skill_read to load."), "{}", context.text);
    assert_eq!(lines.len(), 3);
    assert_eq!(context.lesson_ids, [id]);
    assert_eq!(context.skill_ids, ["gmail-digest"]);
}

#[test]
fn relevant_lessons_with_a_matching_task_type_rank_first() {
    let (_root, store) = store();
    let unrelated = lesson(&store, "main", "email", "A Gmail search returns nothing", "widen the date range", "active", &["r1", "r2", "r3"], NOW);
    let relevant = lesson(&store, "main", "charts", "A PDF report has scanned pages", "run OCR first", "active", &["r1"], NOW - 60 * DAY);
    let matching = lesson(&store, "main", "pdf-reports", "A PDF report has scanned pages", "run OCR first", "active", &["r1"], NOW - 60 * DAY);
    let context = store
        .context_at("main", "Build a summary of this scanned PDF report", Some("pdf-reports"), &[], NOW)
        .unwrap();
    // The relevant lesson of the request's task type beats the equally relevant one of
    // another type; the lesson that shares no words and no task type is not shown.
    assert_eq!(context.lesson_ids, [matching, relevant], "{}", context.text);
    assert!(!context.lesson_ids.contains(&unrelated));

    // A task-type match alone is enough to be shown.
    let context = store.context_at("main", "zzz", Some("email"), &[], NOW).unwrap();
    assert_eq!(context.lesson_ids, [unrelated]);
    // Nothing relevant: no block.
    let context = store.context_at("main", "zzz", None, &[], NOW).unwrap();
    assert_eq!(context, LearnedContext::default());
}

#[test]
fn only_active_lessons_of_the_agent_and_main_are_shown() {
    let (_root, store) = store();
    let text = "Convert the PDF report";
    let active = lesson(&store, "main", "pdf", "A PDF report", "use pdftotext", "active", &["r1"], NOW);
    let own = lesson(&store, "writer", "pdf", "A PDF report", "use pdfplumber", "active", &["r1"], NOW);
    lesson(&store, "main", "pdf", "A PDF report", "candidate action", "candidate", &["r1"], NOW);
    lesson(&store, "main", "pdf", "A PDF report", "retired action", "retired", &["r1"], NOW);
    let other = lesson(&store, "other", "pdf", "A PDF report", "use other agent tool", "active", &["r1"], NOW);

    let mut writer = store.context_at("writer", text, None, &[], NOW).unwrap().lesson_ids;
    writer.sort();
    assert_eq!(writer, [active, own]);
    assert_eq!(store.context_at("main", text, None, &[], NOW).unwrap().lesson_ids, [active]);
    let all = store.context_at("other", text, None, &[], NOW).unwrap();
    assert!(all.lesson_ids.contains(&other) && !all.text.contains("candidate action") && !all.text.contains("retired action"));
    // The trait entry point ranks lessons the same way.
    assert_eq!(store.context_for("main", text, None).unwrap().lesson_ids, [active]);
}

#[test]
fn degraded_skills_are_not_offered() {
    let (_root, store) = store();
    let mut broken = skill("pdf-tables", 2, "Extract tables from a PDF report");
    broken.degraded = true;
    let skills = [broken, skill("pdf-ocr", 1, "OCR a scanned PDF report")];
    let context = store.context_at("main", "Read the tables of this PDF report", None, &skills, NOW).unwrap();
    assert_eq!(context.skill_ids, ["pdf-ocr"]);
    assert!(!context.text.contains("pdf-tables"));
}

#[test]
fn skills_that_succeed_more_rank_higher() {
    let (_root, store) = store();
    {
        let db = store.lock().unwrap();
        for (index, (skill, success)) in [("csv-good", 1), ("csv-good", 1), ("csv-bad", 0), ("csv-bad", 0)].iter().enumerate() {
            db.execute(
                "INSERT INTO learn_skill_uses(skill,revision,request_id,success,at) VALUES(?,1,?,?,?)",
                params![skill, format!("r{index}"), success, NOW as i64],
            )
            .unwrap();
        }
    }
    let skills = [skill("csv-bad", 1, "Clean a CSV export"), skill("csv-good", 1, "Clean a CSV export")];
    let context = store.context_at("main", "Clean this CSV export", None, &skills, NOW).unwrap();
    assert_eq!(context.skill_ids, ["csv-good", "csv-bad"]);
}

#[test]
fn the_block_stays_within_4_kb_without_cutting_lines_and_is_deterministic() {
    let (_root, store) = store();
    let action = format!("run the converter {}", "with care ".repeat(50));
    for index in 0..40 {
        lesson(&store, "main", "pdf", &format!("A PDF report number {index} fails"), &action, "active", &["r1"], NOW - index * DAY);
    }
    let context = store.context_at("main", "The PDF report fails", None, &[], NOW).unwrap();
    assert!(context.text.len() <= MAX_CONTEXT_BYTES, "{}", context.text.len());
    let lines: Vec<&str> = context.text.lines().collect();
    assert_eq!(lines[0], CONTEXT_HEADER);
    assert_eq!(lines.len() - 1, context.lesson_ids.len());
    assert!(context.lesson_ids.len() < 40);
    // Every shown line is whole.
    for (line, id) in lines[1..].iter().zip(&context.lesson_ids) {
        assert!(line.starts_with(&format!("L-{id} (pdf): When a PDF report number")) && line.ends_with("with care."), "{line}");
    }
    // The same inputs give the same block.
    assert_eq!(store.context_at("main", "The PDF report fails", None, &[], NOW).unwrap(), context);

    // A line that does not fit is dropped, and a smaller lower-ranked one still fits.
    let huge = skill("pdf-huge", 1, &"PDF report ".repeat(500));
    let small = skill("pdf-small", 1, "PDF");
    let ranked = rank(&[], &[huge, small], &BTreeMap::new(), "PDF report", None, NOW);
    assert_eq!(ranked.skill_ids, ["pdf-small"]);
}

#[test]
fn shown_lessons_are_recorded_once_per_request_and_decided_at_close() {
    let (_root, store) = store();
    let first = lesson(&store, "main", "pdf", "A PDF report", "use pdftotext", "active", &["r1"], NOW);
    let second = lesson(&store, "main", "pdf", "A PDF report", "use pdfplumber", "active", &["r1"], NOW);
    assert_eq!(store.record_lesson_uses("req-1", &[first, second]).unwrap(), 2);
    // Showing them again in the same request (a later turn) counts nothing new; an
    // unknown id is ignored.
    assert_eq!(store.record_lesson_uses("req-1", &[first, 999]).unwrap(), 0);
    assert_eq!(store.record_lesson_uses("req-2", &[first]).unwrap(), 1);
    assert_eq!((shown(&store, first), shown(&store, second)), (2, 1));
    assert_eq!(store.lesson_uses("req-1").unwrap(), [(first, None), (second, None)]);

    assert_eq!(store.decide_lesson_uses("req-1", true).unwrap(), 2);
    assert_eq!(store.lesson_uses("req-1").unwrap(), [(first, Some(true)), (second, Some(true))]);
    assert_eq!(store.lesson_uses("req-2").unwrap(), [(first, None)]);
    // Closed again after a resume: the latest status wins.
    store.decide_lesson_uses("req-1", false).unwrap();
    assert_eq!(store.lesson_uses("req-1").unwrap(), [(first, Some(false)), (second, Some(false))]);
}
