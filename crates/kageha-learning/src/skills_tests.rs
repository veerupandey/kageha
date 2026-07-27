use super::*;
use serde_json::json;

fn invocation(command: &str) -> Option<(String, String, Vec<String>)> {
    script_invocation(command).map(|run| (run.program, run.script, run.args))
}

fn shell(seq: u64, body: Value) -> StoredEvent {
    StoredEvent {
        seq,
        kind: JournalKind::ShellCommand,
        body,
        at: seq,
    }
}

fn verified(references: &[&str]) -> BTreeSet<String> {
    references.iter().map(|reference| reference.to_string()).collect()
}

#[test]
fn simple_script_commands_are_recognized() {
    assert_eq!(
        invocation("python3 make_report.py"),
        Some(("python3".into(), "make_report.py".into(), vec![]))
    );
    assert_eq!(
        invocation("  python ./tools/build.py --title 'Q3 report' -n 2 "),
        Some((
            "python".into(),
            "tools/build.py".into(),
            vec!["--title".into(), "Q3 report".into(), "-n".into(), "2".into()]
        ))
    );
    assert_eq!(
        invocation("node scripts/fetch.mjs"),
        Some(("node".into(), "scripts/fetch.mjs".into(), vec![]))
    );
    assert_eq!(
        invocation("bash run.sh x"),
        Some(("bash".into(), "run.sh".into(), vec!["x".into()]))
    );
    assert_eq!(
        invocation("sh run.sh"),
        Some(("sh".into(), "run.sh".into(), vec![]))
    );
    assert_eq!(
        invocation("uv run report.py"),
        Some(("uv run".into(), "report.py".into(), vec![]))
    );
    assert_eq!(
        invocation("uv run python3 report.py in.csv"),
        Some(("uv run python".into(), "report.py".into(), vec!["in.csv".into()]))
    );
}

#[test]
fn other_commands_are_not_promoted() {
    for command in [
        "python3 -m http.server",
        "python3 -c 'print(1)'",
        "python3 make_report.py > out.txt",
        "python3 make_report.py && rm -rf outputs",
        "cd tools; python3 build.py",
        "FOO=1 python3 make_report.py",
        "python3 /tmp/make_report.py",
        "python3 ../make_report.py",
        "python3 .kageha/x.py",
        "python3 make_report.sh",
        "node make_report.py",
        "bash make_report.py",
        "uv pip install pdfplumber",
        "uv run",
        "python3 'unclosed.py",
        "python3 $SCRIPT",
        "ruby make_report.rb",
        "make report",
        "",
    ] {
        assert_eq!(invocation(command), None, "{command}");
    }
}

#[test]
fn verified_runs_are_candidates_and_others_are_not() {
    let events = [
        // Verified artifact output.
        shell(
            1,
            json!({"call_id":"c1","command":"python3 make_report.py","exit_code":0,
                "files_changed":["workspace/make_report.py","workspace/outputs/report.md"]}),
        ),
        // Ran, but its output is not evidence.
        shell(
            2,
            json!({"call_id":"c2","command":"python3 scratch.py","exit_code":0,"files_changed":["workspace/notes/x.md"]}),
        ),
        // Failed.
        shell(
            3,
            json!({"call_id":"c3","command":"python3 broken.py","exit_code":1,"files_changed":["workspace/outputs/report.md"]}),
        ),
        // The script only rewrote itself.
        shell(
            4,
            json!({"call_id":"c4","command":"bash self.sh","exit_code":0,"files_changed":["workspace/self.sh"]}),
        ),
        // A background start has no exit code yet.
        shell(
            5,
            json!({"call_id":"c5","command":"node serve.js","background":true,"job_id":"j1"}),
        ),
        // Its call id is validation evidence.
        shell(
            6,
            json!({"call_id":"c6","command":"bash check.sh","exit_code":0,"files_changed":[]}),
        ),
        // Not a script.
        shell(
            7,
            json!({"call_id":"c7","command":"cp a outputs/report.md","exit_code":0,"files_changed":["workspace/outputs/report.md"]}),
        ),
    ];
    let refs = verified(&["workspace/outputs/report.md", "workspace/self.sh", "c6"]);
    let runs = script_runs(&events, &refs);
    let summary: Vec<(u64, &str, Vec<String>)> = runs
        .iter()
        .map(|run| (run.seq, run.invocation.script.as_str(), run.evidence.clone()))
        .collect();
    assert_eq!(
        summary,
        [
            (6, "check.sh", vec!["c6".to_owned()]),
            (1, "make_report.py", vec!["workspace/outputs/report.md".to_owned()]),
        ]
    );
    assert!(script_runs(&events, &BTreeSet::new()).is_empty());
}

#[test]
fn candidates_are_distinct_scripts_capped_at_the_limit() {
    let run = |seq, script: &str| {
        shell(
            seq,
            json!({"command":format!("python3 {script}"),"exit_code":0,"files_changed":["workspace/outputs/a.md"]}),
        )
    };
    // A job completion entry (no call id) counts too.
    let events = [run(1, "a.py"), run(2, "b.py"), run(3, "a.py"), run(4, "c.py")];
    let runs = script_runs(&events, &verified(&["workspace/outputs/a.md"]));
    let scripts: Vec<(u64, &str)> = runs.iter().map(|run| (run.seq, run.invocation.script.as_str())).collect();
    assert_eq!(scripts.len(), MAX_SKILL_CANDIDATES);
    assert_eq!(scripts, [(4, "c.py"), (3, "a.py")]);
}

fn draft(script: Value) -> Value {
    json!({"name":"Make Report!","description":"Build the weekly\nreport","instructions":"Use this to build the weekly report.\n\nRun `python3 scripts/make_report.py`.",
        "script":script,"test_sh":"set -e\npython3 scripts/make_report.py\ntest -s outputs/report.md\n"})
}

#[test]
fn drafts_parse_and_build_a_valid_package() {
    let parsed = parse_skill_draft(&draft(json!(null))).unwrap();
    assert_eq!(parsed.description, "Build the weekly report");
    assert_eq!(parsed.script, None);
    let run = script_invocation("python3 tools/make_report.py").unwrap();
    let name = skill_name(&parsed.name, &run);
    assert_eq!(name, "make-report");
    let original = b"print('report')\n";
    let built = build_skill(&name, &parsed, original, &run, "request-1").unwrap();
    assert!(!built.generalized);
    assert_eq!(built.script_path, "scripts/make_report.py");
    let files = &built.package.files;
    assert_eq!(files.keys().collect::<Vec<_>>(), ["SKILL.md", "scripts/make_report.py", "test.sh"]);
    assert_eq!(files["scripts/make_report.py"], original);
    let skill = &built.package.skill;
    assert_eq!((skill.name.as_str(), skill.description.as_str()), ("make-report", "Build the weekly report"));
    assert_eq!(skill.metadata["source"], "learned");
    assert_eq!(skill.metadata["learned_from"], "request-1");
    assert_eq!(skill.metadata["command"], "python3 scripts/make_report.py");
    assert!(skill.body.starts_with("Use this to build the weekly report."));
    built.package.validate().unwrap();

    // A drafted script replaces the original; an invalid one keeps it.
    let generalized = parse_skill_draft(&draft(json!("import sys\nprint(sys.argv)\n"))).unwrap();
    assert!(build_skill(&name, &generalized, original, &run, "r").unwrap().generalized);
    for invalid in [json!(""), json!("  "), json!(7), json!("x\0y"), json!("x".repeat(MAX_SCRIPT_BYTES + 1))] {
        assert_eq!(parse_skill_draft(&draft(invalid)).unwrap().script, None);
    }
}

#[test]
fn drafts_without_required_fields_are_rejected() {
    assert!(parse_skill_draft(&json!([])).is_err());
    for key in ["description", "instructions", "test_sh"] {
        let mut value = draft(json!(null));
        value.as_object_mut().unwrap().remove(key);
        assert!(parse_skill_draft(&value).unwrap_err().contains(key));
    }
    let mut long = draft(json!(null));
    long["description"] = json!("word ".repeat(100));
    assert!(parse_skill_draft(&long).is_err());
}

#[test]
fn names_fall_back_to_the_script_and_get_a_learned_suffix() {
    let run = script_invocation("python3 Make_Report.py").unwrap();
    assert_eq!(skill_name("", &run), "make_report");
    assert_eq!(skill_name("---", &run), "make_report");
    assert_eq!(skill_name("PDF → Markdown", &run), "pdf-markdown");
    let odd = script_invocation("python3 ___.py").unwrap();
    assert_eq!(skill_name("", &odd), "learned-script");
    assert_eq!(learned_name("make-report"), "make-report-learned");
    let long = "a".repeat(64);
    let suffixed = learned_name(&long);
    assert!(suffixed.len() <= 64 && suffixed.ends_with(LEARNED_SUFFIX) && valid_name(&suffixed));
}

#[test]
fn promotions_are_recorded_per_request_and_script() {
    let directory = tempfile::tempdir().unwrap();
    let store = SqliteLearning::open(&directory.path().join("learning.sqlite")).unwrap();
    assert!(store.promotions("r1").unwrap().is_empty());
    store
        .record_promotion("r1", &SkillPromotion::dropped("a.py", Some("a"), "test.sh exited with 1"))
        .unwrap();
    store
        .record_promotion("r1", &SkillPromotion::learned("b.py", "b", 2))
        .unwrap();
    // A later record of the same script replaces the earlier one.
    store
        .record_promotion("r1", &SkillPromotion::learned("a.py", "a", 1))
        .unwrap();
    let rows = store.promotions("r1").unwrap();
    let summary: Vec<_> = rows
        .iter()
        .map(|row| (row.script.as_str(), row.state, row.skill.as_deref(), row.revision, row.reason.as_deref()))
        .collect();
    assert_eq!(
        summary,
        [
            ("a.py", PromotionState::Learned, Some("a"), Some(1), None),
            ("b.py", PromotionState::Learned, Some("b"), Some(2), None),
        ]
    );
    assert!(store.promotions("r2").unwrap().is_empty());
    drop(store);
    // The table survives reopening.
    let store = SqliteLearning::open(&directory.path().join("learning.sqlite")).unwrap();
    assert_eq!(store.promotions("r1").unwrap().len(), 2);
}
