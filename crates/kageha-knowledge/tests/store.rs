use kageha_contracts::execution::Control;
use kageha_knowledge::{
    scope::{Config, Scope},
    store::Store,
    Lexical, Retriever,
};
use serde_json::json;
use std::sync::{atomic::AtomicBool, Arc, Barrier};
fn attach(store: &Store, workspace: &str) -> Scope {
    Scope::resolve(
        workspace,
        "reader",
        &Config::parse(&json!({})).unwrap(),
        &["notes".into()],
        |id| store.binding(workspace, id),
    )
    .unwrap()
}
#[test]
fn revisions_tombstones_restart_and_generation_fencing() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("component/store.sqlite");
    let mut s = Store::open(&path).unwrap();
    assert!(s.create("w", "agent-private", "fixed").is_err());
    s.create("w", "notes", "fixed").unwrap();
    assert!(s.create("w", "notes", "fixed").is_err());
    let first = s
        .put(
            "w",
            "notes",
            "notes.md",
            b"violet checkpoint",
            "text/plain",
            None,
        )
        .unwrap();
    assert_eq!(first.revision, 1);
    assert_eq!(
        s.put(
            "w",
            "notes",
            "notes.md",
            b"violet checkpoint",
            "text/plain",
            None
        )
        .unwrap()
        .revision,
        1
    );
    let scope = attach(&s, "w");
    let cancel = AtomicBool::new(false);
    let control = Control {
        cancelled: &cancel,
        deadline: None,
    };
    let docs = s.documents(&scope, &control).unwrap();
    assert_eq!(
        Lexical
            .search("reader", &docs, "violet", 5, &control)
            .unwrap()
            .len(),
        1
    );
    assert!(s
        .put("w", "notes", "notes.md", b"wrong", "text/plain", Some(0))
        .is_err());
    s.put(
        "w",
        "notes",
        "notes.md",
        b"amber checkpoint",
        "text/plain",
        Some(1),
    )
    .unwrap();
    assert_eq!(
        String::from_utf8(s.scoped_source(&scope, "notes", "notes.md").unwrap().bytes).unwrap(),
        "amber checkpoint"
    );
    s.remove("w", "notes", "notes.md").unwrap();
    assert!(s.documents(&scope, &control).unwrap().is_empty());
    drop(s);
    let mut s = Store::open(&path).unwrap();
    assert!(s.names("w", "notes").unwrap().is_empty());
    s.put(
        "w",
        "notes",
        "notes.md",
        b"restored explicitly",
        "text/plain",
        Some(3),
    )
    .unwrap();
    s.delete("w", "notes").unwrap();
    assert!(s.documents(&scope, &control).is_err());
    assert_eq!(s.create("w", "notes", "fixed").unwrap().generation, 2);
    s.put(
        "w",
        "notes",
        "notes.md",
        b"new generation secret",
        "text/plain",
        None,
    )
    .unwrap();
    assert!(s.documents(&scope, &control).is_err());
    assert!(s.scoped_source(&scope, "notes", "notes.md").is_err());
    let db = rusqlite::Connection::open(&path).unwrap();
    let count: i64 = db
        .query_row(
            "SELECT count(*) FROM source_history WHERE workspace='w' AND generation=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 4);
}
#[test]
fn workspace_isolation_and_compare_exchange_writers() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("component/store.sqlite");
    let mut s = Store::open(&path).unwrap();
    s.create("one", "notes", "fixed").unwrap();
    s.create("two", "notes", "fixed").unwrap();
    s.put(
        "one",
        "notes",
        "notes.txt",
        b"one original",
        "text/plain",
        None,
    )
    .unwrap();
    s.put(
        "two",
        "notes",
        "notes.txt",
        b"two private",
        "text/plain",
        None,
    )
    .unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let results = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for text in [b"first winner".as_slice(), b"second winner".as_slice()] {
            let p = &path;
            let barrier = barrier.clone();
            handles.push(scope.spawn(move || {
                let mut s = Store::open(p).unwrap();
                barrier.wait();
                s.put("one", "notes", "notes.txt", text, "text/plain", Some(1))
                    .is_ok()
            }));
        }
        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(results.iter().filter(|v| **v).count(), 1);
    assert_eq!(
        s.source("two", "notes", "notes.txt").unwrap().bytes,
        b"two private"
    );
    assert_eq!(s.source("one", "notes", "notes.txt").unwrap().revision, 2);
    let control = Control {
        cancelled: &AtomicBool::new(false),
        deadline: None,
    };
    let docs = s.documents(&attach(&s, "one"), &control).unwrap();
    assert!(docs
        .iter()
        .all(|d| !d.chunks.iter().any(|c| c.text.contains("two private"))));
}
#[test]
fn unsupported_schema_invalid_paths_and_tampered_bytes_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("component/store.sqlite");
    let mut s = Store::open(&path).unwrap();
    s.create("w", "notes", "hierarchical").unwrap();
    assert!(s
        .put("w", "notes", "../escape", b"x", "text/plain", None)
        .is_err());
    assert!(s
        .put("w", "notes", "bad.txt", &[255], "text/plain", None)
        .is_err());
    s.put("w", "notes", "notes.txt", b"safe", "text/plain", None)
        .unwrap();
    drop(s);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute("UPDATE sources SET body=X'626164'", []).unwrap();
    db.execute("UPDATE source_stats SET chunks=999999", [])
        .unwrap();
    drop(db);
    let s = Store::open(&path).unwrap();
    assert!(s.source("w", "notes", "notes.txt").is_err());
    assert!(s.metadata("w", "notes").is_err());
    drop(s);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute("UPDATE knowledge_schema SET version=99", [])
        .unwrap();
    drop(db);
    assert!(Store::open(&path).is_err());
}

#[test]
fn imported_sources_extractions_and_receipts_survive_native_changes() {
    use kageha_knowledge::{store::{ImportBase,ImportSource},Chunk};
    let root=tempfile::tempdir().unwrap();let path=root.path().join("component/store.sqlite");let mut store=Store::open(&path).unwrap();
    let original=ImportBase{id:"legacy".into(),source_id:"source-one".into(),source_digest:"a".repeat(64),manifest:json!({"id":"legacy","engine":"zvec"}),files:vec![ImportSource{name:"manual.pdf".into(),bytes:b"%PDF original bytes".to_vec(),mime:"application/pdf".into(),chunks:vec![Chunk{id:"legacy-chunk".into(),text:"Violet checkpoint restores the deployment".into(),heading:"Recovery".into(),parent_id:Some("original-parent".into())}]}]};
    assert!(store.import_base("workspace",&original).unwrap());assert!(!store.import_base("workspace",&original).unwrap());
    let scope=Scope::resolve("workspace","reader",&Config{inherit:true,extras:vec![]},&["legacy".into()],|id|store.binding("workspace",id)).unwrap();
    let control=Control{cancelled:&AtomicBool::new(false),deadline:None};let docs=store.documents(&scope,&control).unwrap();assert_eq!(docs[0].chunks[0].id,"legacy-chunk");assert_eq!(docs[0].chunks[0].parent_id.as_deref(),Some("original-parent"));
    let (source, chunks)=store.scoped_content(&scope,"legacy","manual.pdf").unwrap(); assert!(source.bytes.starts_with(b"%PDF")); assert_eq!(chunks[0].id,"legacy-chunk");
    store.put("workspace","legacy","native.txt",b"Operator added source","text/plain",None).unwrap();store.remove("workspace","legacy","manual.pdf").unwrap();assert!(!store.import_base("workspace",&original).unwrap());assert_eq!(store.names("workspace","legacy").unwrap(),vec!["native.txt"]);
    store.delete("workspace","legacy").unwrap();drop(store);
    let mut store=Store::open(&path).unwrap();assert!(!store.import_base("workspace",&original).unwrap());assert!(store.base("workspace","legacy").is_err());
    let mut changed=original.clone();changed.files[0].bytes=b"changed source".to_vec();assert!(store.import_base("workspace",&changed).is_err());
    let db=rusqlite::Connection::open(&path).unwrap();db.execute("UPDATE source_history SET body=X'626164' WHERE kb='legacy' AND name='manual.pdf'",[]).unwrap();drop(db);assert!(store.verify_import("workspace",&original).is_err());
}
#[test]
fn imports_do_not_adopt_or_overwrite_existing_native_identities() {
    use kageha_knowledge::store::ImportBase;
    let root=tempfile::tempdir().unwrap();let mut store=Store::open(&root.path().join("component/store.sqlite")).unwrap();store.create("w","existing","fixed").unwrap();
    let document=ImportBase{id:"existing".into(),source_id:"source".into(),source_digest:"a".repeat(64),manifest:json!({}),files:vec![]};
    assert!(store.import_base("w",&document).is_err());store.delete("w","existing").unwrap();assert!(store.import_base("w",&document).is_err());
}
