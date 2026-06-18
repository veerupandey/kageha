use kageha_contracts::execution::*;
use kageha_workspace::{DocumentTools, DocumentWorker, ReadWorkspace, TEXT_LIMIT};
use serde_json::json;
use std::{
    fs,
    os::unix::fs::symlink,
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};
fn control(cancel: &AtomicBool) -> Control<'_> {
    Control {
        cancelled: cancel,
        deadline: Some(Instant::now() + Duration::from_secs(2)),
    }
}
#[test]
fn root_capability_bounds_and_internal_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("note"), "hello").unwrap();
    fs::write(dir.path().join("secret"), "outside").unwrap();
    symlink(root.join("note"), root.join("internal")).unwrap();
    symlink(dir.path().join("secret"), root.join("external")).unwrap();
    let workspace = ReadWorkspace::new(&root).unwrap();
    let cancel = AtomicBool::new(false);
    let control = control(&cancel);
    assert_eq!(workspace.read_text("internal", &control).unwrap(), "hello");
    assert!(workspace.read_text("external", &control).is_err());
    assert!(workspace.read_text("../secret", &control).is_err());
    assert!(workspace.read_text(".", &control).is_err());
    fs::write(root.join("max"), vec![b'a'; TEXT_LIMIT]).unwrap();
    assert_eq!(
        workspace.read_text("max", &control).unwrap().len(),
        TEXT_LIMIT
    );
    fs::write(root.join("over"), vec![b'a'; TEXT_LIMIT + 1]).unwrap();
    assert!(workspace.read_text("over", &control).is_err());
    fs::write(root.join("binary"), [0xff, 0x00]).unwrap();
    assert!(workspace.read_text("binary", &control).is_err());
}
#[test]
fn replacement_after_approval_cannot_redirect_a_read() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("note"), "approved").unwrap();
    let workspace = ReadWorkspace::new(dir.path()).unwrap();
    let call = ToolCall {
        id: "read".into(),
        name: "read_file".into(),
        arguments: json!({"path":"note"}),
    };
    let context = ToolContext {
        session: "chat".into(),
        turn: "turn".into(),
    };
    let binding = workspace.binding(&context, &call).unwrap();
    fs::write(dir.path().join("new"), "replacement").unwrap();
    fs::rename(dir.path().join("new"), dir.path().join("note")).unwrap();
    assert!(workspace
        .execute_authorized_in(&context, &call, &binding, &control(&AtomicBool::new(false)))
        .is_err());
}
#[test]
fn root_rename_cannot_rebind_the_capability() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("note"), "original root").unwrap();
    let workspace = ReadWorkspace::new(&root).unwrap();
    fs::rename(&root, dir.path().join("previous")).unwrap();
    fs::create_dir(&root).unwrap();
    fs::write(root.join("note"), "different root").unwrap();
    assert_eq!(
        workspace
            .read_text("note", &control(&AtomicBool::new(false)))
            .unwrap(),
        "original root"
    );
}
#[test]
fn fifo_is_rejected_without_blocking() {
    use std::os::unix::ffi::OsStrExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pipe");
    let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let start = Instant::now();
    assert!(ReadWorkspace::new(dir.path())
        .unwrap()
        .read_text("pipe", &control(&AtomicBool::new(false)))
        .is_err());
    assert!(start.elapsed() < Duration::from_secs(1));
}
#[test]
fn document_worker_consumes_verified_bytes_and_has_its_own_cli() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("note.html");
    fs::write(&path, "<h1>Hello</h1><script>hidden</script><p>worker</p>").unwrap();
    let worker = Path::new(env!("CARGO_BIN_EXE_kageha-workspace"));
    let adapter = DocumentTools::new(dir.path(), worker).unwrap();
    let call = ToolCall {
        id: "doc".into(),
        name: "read_document".into(),
        arguments: json!({"path":"note.html"}),
    };
    let cancel = AtomicBool::new(false);
    let control = control(&cancel);
    let reply = adapter.execute(&call, &control).unwrap();
    assert!(reply.content.contains("Hello"));
    assert!(!reply.content.contains("hidden"));
    let parser = DocumentWorker::new(worker).unwrap();
    assert_eq!(
        parser
            .extract(
                b"unknown extension text".to_vec(),
                "very-long-unusual-extension",
                &control
            )
            .unwrap(),
        "unknown extension text"
    );
    assert!(parser.read(&path, Some(&"0".repeat(64)), &control).is_err());
    let snapshot = kageha_workspace::file_snapshot(&path, None, &control).unwrap();
    let hash = snapshot.sha256;
    fs::write(&path, "changed").unwrap();
    assert!(parser.read(&path, Some(&hash), &control).is_err());
}
#[test]
fn parser_limits_office_expansion_and_text_output() {
    use std::io::{Cursor, Write};
    let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
    archive
        .start_file(
            "word/document.xml",
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated),
        )
        .unwrap();
    archive
        .write_all(&vec![b'a'; 16 * 1024 * 1024 + 1])
        .unwrap();
    let bytes = archive.finish().unwrap().into_inner();
    assert!(kageha_workspace::documents::extract_bytes(&bytes, "docx").is_err());
    assert!(
        kageha_workspace::documents::extract_bytes(&vec![b'a'; 1024 * 1024 + 1], "txt").is_err()
    );
}
