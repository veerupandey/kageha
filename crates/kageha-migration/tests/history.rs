use kageha_migration::{HistorySink, Importer, Session};
use serde_json::json;
use std::{collections::BTreeMap, fs, io, path::Path};
#[derive(Default)]
struct Sink {
    sessions: BTreeMap<String, Session>,
    writes: usize,
    lose_ack: bool,
    fail_on: Option<String>,
}
impl HistorySink for Sink {
    fn apply(&mut self, s: &Session) -> io::Result<()> {
        if self.fail_on.as_deref() == Some(&s.id) {
            return Err(io::Error::other("fixture interruption"));
        }
        if let Some(old) = self.sessions.get(&s.id) {
            if old.source_digest != s.source_digest {
                return Err(io::Error::other("conflict"));
            }
            return Ok(());
        }
        self.sessions.insert(s.id.clone(), s.clone());
        self.writes += 1;
        if self.lose_ack {
            self.lose_ack = false;
            return Err(io::Error::other("lost projection acknowledgement"));
        }
        Ok(())
    }
    fn verify(&mut self, s: &Session) -> io::Result<()> {
        if self
            .sessions
            .get(&s.id)
            .is_some_and(|old| old.source_digest == s.source_digest)
        {
            Ok(())
        } else {
            Err(io::Error::other("missing projection"))
        }
    }
}
fn fixture(root: &Path, id: &str) {
    let chat = root.join(id);
    fs::create_dir_all(chat.join("inputs")).unwrap();
    fs::write(chat.join("inputs/proof.txt"), b"portable attachment").unwrap();
    let records = [
        json!({"role":"user","text":"keep","turn_id":"one","attachments":["inputs/proof.txt"]}),
        json!({"role":"assistant","text":"deleted","turn_id":"gone"}),
        json!({"role":"tool","content":"raw provenance"}),
    ];
    fs::write(
        chat.join("chat.jsonl"),
        records
            .into_iter()
            .map(|r| r.to_string() + "\n")
            .collect::<String>(),
    )
    .unwrap();
    fs::write(
        chat.join("meta.json"),
        json!({"title":"Historical title","pinned":true,"deleted_history_turn_ids":["gone"]})
            .to_string(),
    )
    .unwrap();
}
#[test]
fn repeat_import_preserves_ids_deletions_provenance_and_owned_attachments() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    fixture(&source, "chat-one");
    let before = fs::read(source.join("chat-one/chat.jsonl")).unwrap();
    let dest = root.path().join("destination");
    let mut sink = Sink::default();
    {
        let mut importer = Importer::open(&source, &dest).unwrap();
        let report = importer.run(&mut sink).unwrap();
        assert_eq!(report.imported, 1);
        assert_eq!(report.messages, 1);
        assert_eq!(report.attachments, 1);
    }
    let s = sink.sessions.get("chat-one").unwrap();
    assert_eq!(s.id, "chat-one");
    assert_eq!(s.metadata["title"], "Historical title");
    assert_eq!(fs::read(&s.history_snapshot).unwrap(), before);
    assert!(s.source_files.contains_key("meta.json"));
    assert_eq!(
        fs::read(&s.attachments[0].owned_path).unwrap(),
        b"portable attachment"
    );
    let report = Importer::open(&source, &dest)
        .unwrap()
        .run(&mut sink)
        .unwrap();
    assert_eq!(report.already_present, 1);
    assert_eq!(sink.writes, 1);
    assert_eq!(
        fs::read(source.join("chat-one/chat.jsonl")).unwrap(),
        before
    );
    fs::remove_dir_all(&source).unwrap();
    assert_eq!(
        fs::read(&sink.sessions["chat-one"].attachments[0].owned_path).unwrap(),
        b"portable attachment"
    );
}
#[test]
fn lost_projection_acknowledgement_does_not_duplicate_mutation() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    fixture(&source, "chat");
    let dest = root.path().join("destination");
    let mut sink = Sink {
        lose_ack: true,
        ..Sink::default()
    };
    assert!(Importer::open(&source, &dest)
        .unwrap()
        .run(&mut sink)
        .is_err());
    assert_eq!(sink.writes, 1);
    assert_eq!(
        Importer::open(&source, &dest)
            .unwrap()
            .run(&mut sink)
            .unwrap()
            .imported,
        1
    );
    assert_eq!(sink.writes, 1);
}
#[test]
fn restart_resumes_between_sessions_and_refuses_changed_or_missing_sources() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    fixture(&source, "a");
    fixture(&source, "b");
    let dest = root.path().join("destination");
    let mut sink = Sink {
        fail_on: Some("b".into()),
        ..Sink::default()
    };
    assert!(Importer::open(&source, &dest)
        .unwrap()
        .run(&mut sink)
        .is_err());
    assert_eq!(sink.writes, 1);
    sink.fail_on = None;
    let report = Importer::open(&source, &dest)
        .unwrap()
        .run(&mut sink)
        .unwrap();
    assert_eq!((report.imported, report.already_present), (1, 1));
    assert_eq!(sink.writes, 2);
    fs::write(source.join("a/inputs/proof.txt"), "changed").unwrap();
    assert!(Importer::open(&source, &dest)
        .unwrap()
        .run(&mut sink)
        .unwrap_err()
        .to_string()
        .contains("changed"));
    assert_eq!(sink.writes, 2);
    fs::remove_dir_all(source.join("a")).unwrap();
    assert!(Importer::open(&source, &dest)
        .unwrap()
        .run(&mut sink)
        .unwrap_err()
        .to_string()
        .contains("missing"));
}
#[test]
fn malformed_records_and_corrupted_owned_files_are_not_successful_imports() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    fixture(&source, "chat");
    let dest = root.path().join("destination");
    let mut sink = Sink::default();
    Importer::open(&source, &dest)
        .unwrap()
        .run(&mut sink)
        .unwrap();
    fs::write(&sink.sessions["chat"].attachments[0].owned_path, "tampered").unwrap();
    assert!(Importer::open(&source, &dest)
        .unwrap()
        .run(&mut sink)
        .is_err());
    fs::write(source.join("chat/chat.jsonl"), "invalid json\n").unwrap();
    assert!(Importer::open(&source, &root.path().join("other"))
        .unwrap()
        .run(&mut Sink::default())
        .is_err());
}
#[test]
fn path_escape_symlinks_overlapping_roots_and_concurrent_writers_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    fixture(&source, "chat");
    let dest = root.path().join("destination");
    let importer = Importer::open(&source, &dest).unwrap();
    assert!(Importer::open(&source, &dest).is_err());
    drop(importer);
    assert!(Importer::open(&source, &source.join("inside")).is_err());
    assert!(!source.join("inside").exists());
    fs::remove_file(source.join("chat/inputs/proof.txt")).unwrap();
    std::os::unix::fs::symlink(
        root.path().join("outside"),
        source.join("chat/inputs/proof.txt"),
    )
    .unwrap();
    assert!(Importer::open(&source, &dest)
        .unwrap()
        .run(&mut Sink::default())
        .is_err());
    fs::write(
        source.join("chat/chat.jsonl"),
        json!({"role":"user","text":"x","attachments":["../outside"]}).to_string(),
    )
    .unwrap();
    assert!(Importer::open(&source, &dest)
        .unwrap()
        .run(&mut Sink::default())
        .is_err());
}
#[test]
fn newer_journal_versions_are_not_silently_downgraded() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    fixture(&source, "chat");
    let dest = root.path().join("destination");
    drop(Importer::open(&source, &dest).unwrap());
    let db = rusqlite::Connection::open(dest.join("migration.sqlite")).unwrap();
    db.execute("UPDATE migration_versions SET version=2", [])
        .unwrap();
    drop(db);
    assert!(Importer::open(&source, &dest)
        .err()
        .unwrap()
        .to_string()
        .contains("version"));
}
