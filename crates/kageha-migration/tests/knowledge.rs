use kageha_migration::{
    documents::{Document, DocumentSink, Domain},
    Importer,
};
use serde_json::{json, Value};
use std::{collections::BTreeMap, fs, io, path::Path};
#[derive(Default)]
struct Sink {
    documents: BTreeMap<String, Document>,
    writes: usize,
    lose_ack: bool,
}
impl DocumentSink for Sink {
    fn apply(&mut self, d: &Document) -> io::Result<()> {
        if !self.documents.contains_key(&d.id) {
            self.writes += 1;
            self.documents.insert(d.id.clone(), d.clone());
        }
        if std::mem::take(&mut self.lose_ack) {
            return Err(io::Error::other("lost acknowledgement"));
        }
        Ok(())
    }
    fn verify(&mut self, d: &Document) -> io::Result<()> {
        assert_eq!(self.documents[&d.id].source_digest, d.source_digest);
        Ok(())
    }
}
fn fixture(root: &Path) -> std::path::PathBuf {
    let source = root.join("knowledge");
    let kb = source.join("agent-research.ai");
    fs::create_dir_all(kb.join("raw")).unwrap();
    fs::write(kb.join("manifest.yaml"),"id: agent-research.ai\nscope: persona\npersona_id: research.ai\nengine: zvec\nsources: [guide.md]\n").unwrap();
    fs::write(
        kb.join("raw/guide.md"),
        "Violet checkpoint is needed for rollback",
    )
    .unwrap();
    fs::write(
        kb.join("raw/deleted.md"),
        "DELETED secret must not reappear",
    )
    .unwrap();
    let chunks = [
        json!({"id":"kept","source":"/old/machine/raw/guide.md","text":"Violet checkpoint is needed for rollback","heading":"Runbook","parent_id":"parent-1"}),
        json!({"id":"orphan","source":"deleted.md","text":"DELETED secret must not reappear"}),
    ];
    fs::write(
        kb.join("chunks.jsonl"),
        chunks
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .unwrap();
    source
}
#[test]
fn active_sources_chunks_identity_and_owned_bytes_survive_repeat_import() {
    let root = tempfile::tempdir().unwrap();
    let source = fixture(root.path());
    let mut sink = Sink::default();
    let dest = root.path().join("dest");
    assert_eq!(
        Importer::open(&source, &dest)
            .unwrap()
            .run_documents(Domain::Knowledge, &mut sink)
            .unwrap()
            .imported,
        1
    );
    let value = sink.documents["research.ai"].value.as_ref().unwrap();
    let files = value["files"].as_array().unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0]["chunks"][0]["id"], "kept");
    assert_eq!(files[0]["chunks"][0]["parent_id"], "parent-1");
    assert_eq!(files[0]["chunks"].as_array().unwrap().len(), 1);
    let owned = files[0]["owned_path"].as_str().unwrap().to_string();
    assert!(Path::new(&owned).starts_with(dest.canonicalize().unwrap()));
    assert_eq!(
        Importer::open(&source, &dest)
            .unwrap()
            .run_documents(Domain::Knowledge, &mut sink)
            .unwrap()
            .already_present,
        1
    );
    assert_eq!(sink.writes, 1);
    fs::rename(&source, root.path().join("hidden-source")).unwrap();
    assert!(fs::read_to_string(owned).unwrap().contains("Violet"));
}
#[test]
fn acknowledgement_recovery_and_changed_or_tampered_sources_fail_closed() {
    let root = tempfile::tempdir().unwrap();
    let source = fixture(root.path());
    let dest = root.path().join("dest");
    let mut sink = Sink {
        lose_ack: true,
        ..Default::default()
    };
    assert!(Importer::open(&source, &dest)
        .unwrap()
        .run_documents(Domain::Knowledge, &mut sink)
        .is_err());
    Importer::open(&source, &dest)
        .unwrap()
        .run_documents(Domain::Knowledge, &mut sink)
        .unwrap();
    assert_eq!(sink.writes, 1);
    let owned = sink.documents["research.ai"].value.as_ref().unwrap()["files"][0]["owned_path"]
        .as_str()
        .unwrap();
    fs::write(owned, "tampered").unwrap();
    assert!(Importer::open(&source, &dest)
        .unwrap()
        .run_documents(Domain::Knowledge, &mut sink)
        .is_err());
    fs::write(
        source.join("agent-research.ai/raw/guide.md"),
        "changed source",
    )
    .unwrap();
    assert!(Importer::open(&source, &dest)
        .unwrap()
        .run_documents(Domain::Knowledge, &mut sink)
        .is_err());
}
#[test]
fn missing_bytes_mismatched_owner_and_symlinks_are_rejected() {
    for case in ["missing", "owner", "symlink", "duplicate"] {
        let root = tempfile::tempdir().unwrap();
        let source = fixture(root.path());
        let kb = source.join("agent-research.ai");
        match case {
            "missing" => fs::remove_file(kb.join("raw/guide.md")).unwrap(),
            "owner" => fs::write(
                kb.join("manifest.yaml"),
                "id: agent-research.ai\npersona_id: another\nsources: []\n",
            )
            .unwrap(),
            "symlink" => {
                fs::rename(kb.join("raw"), root.path().join("outside")).unwrap();
                std::os::unix::fs::symlink(root.path().join("outside"), kb.join("raw")).unwrap();
            }
            _ => {
                let line = json!({"id":"same","source":"guide.md","text":"text"}).to_string();
                fs::write(kb.join("chunks.jsonl"), format!("{line}\n{line}")).unwrap();
            }
        }
        assert!(
            Importer::open(&source, &root.path().join("dest"))
                .unwrap()
                .run_documents(Domain::Knowledge, &mut Sink::default())
                .is_err(),
            "{case}"
        );
    }
}

#[test]
fn workspace_manifests_keep_private_stores_separate() {
    let root=tempfile::tempdir().unwrap();let source=fixture(root.path());let public=source.join("runbooks");fs::create_dir_all(public.join("raw")).unwrap();
    fs::write(public.join("manifest.yaml"),"id: runbooks\nengine: zvec\nsources: [notes.txt]\n").unwrap();fs::write(public.join("raw/notes.txt"),"Shared rollback notes").unwrap();
    let mut sink=Sink::default();Importer::open(&source,&root.path().join("shared")).unwrap().run_documents(Domain::WorkspaceKnowledge,&mut sink).unwrap();assert_eq!(sink.documents.len(),1);assert!(sink.documents.contains_key("runbooks"));assert!(!sink.documents.contains_key("research.ai"));
    let mut private=Sink::default();Importer::open(&source,&root.path().join("private")).unwrap().run_documents(Domain::Knowledge,&mut private).unwrap();assert_eq!(private.documents.len(),1);assert!(private.documents.contains_key("research.ai"));
    fs::write(public.join("manifest.yaml"),"id: runbooks\nscope: persona\npersona_id: research.ai\nsources: []\n").unwrap();assert!(Importer::open(&source,&root.path().join("unsafe")).unwrap().run_documents(Domain::WorkspaceKnowledge,&mut Sink::default()).is_err());
}
