use kageha_migration::{
    documents::{Document, DocumentSink, Domain},
    Importer,
};
use serde_json::json;
use std::{fs, io};
#[derive(Default)]
struct Sink {
    saved: Option<Document>,
    writes: usize,
    lose_ack: bool,
}
impl DocumentSink for Sink {
    fn apply(&mut self, d: &Document) -> io::Result<()> {
        if self.saved.is_none() {
            self.saved = Some(d.clone());
            self.writes += 1;
        }
        if std::mem::take(&mut self.lose_ack) {
            return Err(io::Error::other("lost acknowledgment"));
        }
        Ok(())
    }
    fn verify(&mut self, d: &Document) -> io::Result<()> {
        assert_eq!(self.saved.as_ref().unwrap().source_digest, d.source_digest);
        Ok(())
    }
}
fn fixture(root: &std::path::Path) -> std::path::PathBuf {
    let source = root.join("source");
    fs::create_dir_all(source.join("sessions/chat-one")).unwrap();
    fs::write(source.join("projects.json"),json!({"legacy-id":{"id":"legacy-id","name":"Legacy Name","created_at":1,"updated_at":2,"tags":["original"],"unknown":"preserve"}}).to_string()).unwrap();
    fs::write(
        source.join("sessions/chat-one/meta.json"),
        json!({"title":"Earlier title"}).to_string(),
    )
    .unwrap();
    fs::write(source.join("sessions/chat-one/session.json"),json!({"project_id":"legacy-id","tagged_turns":{"0":{"session_id":"chat-one","turn_index":0,"project_id":"legacy-id","tag":"evidence"}}}).to_string()).unwrap();
    source
}
#[test]
fn project_snapshots_preserve_metadata_and_recover_without_duplicate_projection() {
    let root = tempfile::tempdir().unwrap();
    let source = fixture(root.path());
    let state = root.path().join("journal");
    let mut sink = Sink {
        lose_ack: true,
        ..Sink::default()
    };
    assert!(Importer::open(&source, &state)
        .unwrap()
        .run_documents(Domain::Projects, &mut sink)
        .is_err());
    assert_eq!(sink.writes, 1);
    assert_eq!(
        Importer::open(&source, &state)
            .unwrap()
            .run_documents(Domain::Projects, &mut sink)
            .unwrap()
            .imported,
        1
    );
    assert_eq!(sink.writes, 1);
    let doc = sink.saved.as_ref().unwrap();
    assert_eq!(
        doc.value.as_ref().unwrap()["original"]["catalog"]["legacy-id"]["unknown"],
        "preserve"
    );
    assert_eq!(
        doc.value.as_ref().unwrap()["sessions"]["chat-one"]["project"],
        "legacy-id"
    );
    assert_eq!(doc.source_files.len(), 3);
    fs::write(source.join("sessions/chat-one/session.json"), "{}").unwrap();
    assert!(Importer::open(&source, &state)
        .unwrap()
        .run_documents(Domain::Projects, &mut sink)
        .is_err());
}
#[test]
fn project_import_rejects_symlinks_and_conflicting_metadata() {
    let root = tempfile::tempdir().unwrap();
    let source = fixture(root.path());
    let mut sink = Sink::default();
    fs::write(
        source.join("sessions/chat-one/meta.json"),
        json!({"project_id":"different"}).to_string(),
    )
    .unwrap();
    assert!(Importer::open(&source, &root.path().join("journal"))
        .unwrap()
        .run_documents(Domain::Projects, &mut sink)
        .is_err());
    fs::remove_file(source.join("sessions/chat-one/meta.json")).unwrap();
    std::os::unix::fs::symlink("session.json", source.join("sessions/chat-one/meta.json")).unwrap();
    assert!(Importer::open(&source, &root.path().join("journal"))
        .unwrap()
        .run_documents(Domain::Projects, &mut sink)
        .is_err());
    assert_eq!(sink.writes, 0);
}
