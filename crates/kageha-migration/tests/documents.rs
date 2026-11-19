use kageha_migration::{
    documents::{Document, DocumentSink, Domain},
    Importer,
};
use serde_json::json;
use std::{collections::BTreeMap, fs, io};
#[derive(Default)]
struct Sink {
    documents: BTreeMap<String, Document>,
    writes: usize,
    lose_ack: bool,
}
impl DocumentSink for Sink {
    fn apply(&mut self, d: &Document) -> io::Result<()> {
        if self.documents.contains_key(&d.id) {
            return Ok(());
        }
        self.documents.insert(d.id.clone(), d.clone());
        self.writes += 1;
        if self.lose_ack {
            self.lose_ack = false;
            return Err(io::Error::other("lost acknowledgement"));
        }
        Ok(())
    }
    fn verify(&mut self, d: &Document) -> io::Result<()> {
        if self
            .documents
            .get(&d.id)
            .is_some_and(|v| v.source_digest == d.source_digest)
        {
            Ok(())
        } else {
            Err(io::Error::other("missing receipt"))
        }
    }
}
#[test]
fn yaml_agents_preserve_source_revision_ids_and_deleted_definitions() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    fs::create_dir_all(source.join(".history/research.ai")).unwrap();
    fs::create_dir_all(source.join(".history/deleted.ai")).unwrap();
    let yaml="schema_version: 1\nid: research.ai\nname: Research Agent\nsystem_prompt: Preserve constraints\npinned_skills: [research]\ntool_policy:\n  allow: [read_file]\n  deny: [bash]\n";
    fs::write(source.join("research.ai.yaml"), yaml).unwrap();
    fs::write(
        source.join(".history/research.ai/20261106T080000.000000Z-update-123.yaml"),
        yaml.replace("Preserve constraints", "Older instructions"),
    )
    .unwrap();
    fs::write(
        source.join(".history/deleted.ai/deleted.yaml"),
        yaml.replace("research.ai", "deleted.ai"),
    )
    .unwrap();
    let mut sink = Sink::default();
    let dest = root.path().join("dest");
    let report = Importer::open(&source, &dest)
        .unwrap()
        .run_documents(Domain::Agents, &mut sink)
        .unwrap();
    assert_eq!(report.imported, 2);
    assert!(sink.documents["deleted.ai"].value.is_none());
    assert_eq!(
        sink.documents["research.ai"].revisions[0].id,
        "20261106T080000.000000Z-update-123.yaml"
    );
    assert_eq!(
        sink.documents["research.ai"].value.as_ref().unwrap()["tool_policy"]["deny"],
        json!(["bash"])
    );
    assert_eq!(
        Importer::open(&source, &dest)
            .unwrap()
            .run_documents(Domain::Agents, &mut sink)
            .unwrap()
            .already_present,
        2
    );
}
#[test]
fn document_ack_recovery_and_changed_source_guard_share_history_journal() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    let path = source.join("agent.yaml");
    fs::write(&path, "id: agent\nname: Agent\n").unwrap();
    let dest = root.path().join("dest");
    let mut sink = Sink {
        lose_ack: true,
        ..Sink::default()
    };
    assert!(Importer::open(&source, &dest)
        .unwrap()
        .run_documents(Domain::Agents, &mut sink)
        .is_err());
    Importer::open(&source, &dest)
        .unwrap()
        .run_documents(Domain::Agents, &mut sink)
        .unwrap();
    assert_eq!(sink.writes, 1);
    fs::write(&path, "id: agent\nname: Changed\n").unwrap();
    assert!(Importer::open(&source, &dest)
        .unwrap()
        .run_documents(Domain::Agents, &mut sink)
        .is_err());
    assert_eq!(sink.writes, 1);
}
#[test]
fn task_state_preserves_outcomes_budget_and_old_evidence_without_execution() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    fs::create_dir_all(source.join("chat")).unwrap();
    let state = json!({"version":1,"revision":7,"objective":"Complete task","stages":[{"id":"one","description":"Work","status":"active","depends_on":[],"outcome_ids":["goal"],"evidence_refs":["old-call"]}],"goals":[{"id":"goal","description":"Required result","passes":true}],"budget":{"usd_spent":1.25},"browser_blocker":{"id":"stale-receipt"}});
    fs::write(source.join("chat/task_state.json"), state.to_string()).unwrap();
    let mut sink = Sink::default();
    Importer::open(&source, &root.path().join("dest"))
        .unwrap()
        .run_documents(Domain::TaskPlans, &mut sink)
        .unwrap();
    assert_eq!(sink.documents["chat"].value.as_ref(), Some(&state));
    assert_eq!(sink.writes, 1);
}
#[test]
fn unsafe_or_unsupported_configuration_is_not_silently_accepted() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    let path = source.join("agent.yaml");
    for value in [
        json!({"id":"wrong","name":"Agent"}),
        json!({"id":"agent","name":"Agent","schema_version":2}),
        json!({"id":"agent","name":"Agent","tool_policy":{"risk_class_overrides":{"read_file":true}}}),
    ] {
        fs::write(&path, value.to_string()).unwrap();
        assert!(Importer::open(&source, &root.path().join("dest"))
            .unwrap()
            .run_documents(Domain::Agents, &mut Sink::default())
            .is_err());
    }
    fs::remove_file(path).unwrap();
    fs::write(
        source.join(".default-knowledge.yaml"),
        "inherit_workspace: false\nextra_kb_ids: []\n",
    )
    .unwrap();
    let mut sink = Sink::default();
    Importer::open(&source, &root.path().join("overlay"))
        .unwrap()
        .run_documents(Domain::Agents, &mut sink)
        .unwrap();
    assert_eq!(
        sink.documents["main"].value.as_ref().unwrap()["inherit_workspace"],
        false
    );
}
