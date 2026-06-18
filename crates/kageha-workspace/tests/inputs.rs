//! User attachments in the Task_Workspace's `inputs/` (Requirement 20.1, task 31.2).
use kageha_workspace::shell::{sandbox_available, ShellPaths, ShellSession};
use kageha_workspace::task::{InputSource, TaskWorkspace};
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// An upload as the Host stores it: `<chat root>/inputs/<id>-<name>`, outside the workspace.
struct Upload {
    key: String,
    name: String,
    fallback: String,
    file: PathBuf,
    sha256: String,
}

impl Upload {
    fn new(chat: &Path, id: &str, name: &str, bytes: &[u8]) -> Self {
        let stored = format!("{id}-{name}");
        fs::create_dir_all(chat.join("inputs")).unwrap();
        let file = chat.join("inputs").join(&stored);
        fs::write(&file, bytes).unwrap();
        Self { key: format!("inputs/{stored}"), name: name.into(), fallback: stored, file, sha256: sha(bytes) }
    }
    fn source(&self) -> InputSource<'_> {
        InputSource { key: &self.key, name: &self.name, fallback: &self.fallback, file: &self.file, sha256: &self.sha256 }
    }
}

fn chat() -> (tempfile::TempDir, PathBuf, TaskWorkspace) {
    let dir = tempfile::tempdir().unwrap();
    let chat = dir.path().join("chat");
    let workspace = TaskWorkspace::open(&chat).unwrap();
    let chat = chat.canonicalize().unwrap();
    (dir, chat, workspace)
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nnot really an image";

#[test]
fn uploads_are_copied_to_inputs_and_are_not_changes() {
    let (_dir, chat, workspace) = chat();
    let image = Upload::new(&chat, "a1", "screen.png", PNG);
    let notes = Upload::new(&chat, "b2", "notes.txt", b"hello inputs\n");
    for upload in [&image, &notes] {
        let placed = workspace.place_input(&upload.source()).unwrap().unwrap();
        assert_eq!(placed.path, format!("inputs/{}", upload.name));
        assert_eq!(placed.key, upload.key);
        let copy = workspace.root().join(&placed.path);
        assert_eq!(sha(&fs::read(&copy).unwrap()), upload.sha256);
        // A copy, not a hard link: writes in the workspace can't reach the Host's upload.
        assert_ne!(fs::metadata(&copy).unwrap().ino(), fs::metadata(&upload.file).unwrap().ino());
    }
    assert!(workspace.scan_changes().unwrap().is_empty());
    let inputs: Vec<_> = workspace.inputs().unwrap().into_iter().map(|input| input.path).collect();
    assert_eq!(inputs, ["inputs/notes.txt", "inputs/screen.png"]);
    // The manifest records them, so later scans stay quiet; placing again changes nothing.
    assert!(workspace.manifest_entries().unwrap().iter().any(|entry| entry.path == "inputs/screen.png"));
    assert_eq!(workspace.place_input(&image.source()).unwrap().unwrap().path, "inputs/screen.png");
    assert!(workspace.scan_changes().unwrap().is_empty());
    // Both the original upload and the workspace copy stay readable.
    assert_eq!(fs::read(&notes.file).unwrap(), b"hello inputs\n");
}

#[test]
fn a_taken_name_uses_the_stored_name() {
    let (_dir, chat, workspace) = chat();
    workspace.write("inputs/report.pdf", b"the agent's own file").unwrap();
    let upload = Upload::new(&chat, "c3", "report.pdf", b"%PDF-1.4 user file");
    let placed = workspace.place_input(&upload.source()).unwrap().unwrap();
    assert_eq!(placed.path, "inputs/c3-report.pdf");
    assert_eq!(fs::read(workspace.root().join("inputs/report.pdf")).unwrap(), b"the agent's own file");
    // A second upload with the same original name gets its own stored name.
    let again = Upload::new(&chat, "d4", "report.pdf", b"%PDF-1.4 second");
    assert_eq!(workspace.place_input(&again.source()).unwrap().unwrap().path, "inputs/d4-report.pdf");
    assert!(workspace.scan_changes().unwrap().is_empty());
}

#[test]
fn a_changed_input_becomes_an_agent_file_and_a_deleted_one_comes_back() {
    let (_dir, chat, workspace) = chat();
    let shell_edit = Upload::new(&chat, "e5", "data.csv", b"a,b\n1,2\n");
    let tool_edit = Upload::new(&chat, "f6", "draft.md", b"# Draft\n");
    let deleted = Upload::new(&chat, "g7", "photo.jpg", b"\xff\xd8\xffjpeg");
    for upload in [&shell_edit, &tool_edit, &deleted] {
        workspace.place_input(&upload.source()).unwrap().unwrap();
    }
    // An in-place shell write is reported like any other change and releases the input.
    use std::io::Write;
    fs::OpenOptions::new().append(true).open(workspace.root().join("inputs/data.csv")).unwrap().write_all(b"3,4\n").unwrap();
    let changes = workspace.scan_changes().unwrap();
    assert_eq!(changes.iter().map(|change| change.path.as_str()).collect::<Vec<_>>(), ["inputs/data.csv"]);
    assert_eq!(fs::read(&shell_edit.file).unwrap(), b"a,b\n1,2\n", "the Host's upload is unchanged");
    // A write_file/edit_file with other bytes releases it too.
    workspace.write("inputs/draft.md", b"# Draft\nedited\n").unwrap();
    // A deleted input is placed again, still not as a change.
    fs::remove_file(workspace.root().join("inputs/photo.jpg")).unwrap();
    assert!(workspace.scan_changes().unwrap().is_empty());
    assert_eq!(workspace.place_input(&deleted.source()).unwrap().unwrap().path, "inputs/photo.jpg");
    assert!(workspace.scan_changes().unwrap().is_empty());
    // Released inputs are the agent's: never placed again, never overwritten.
    assert_eq!(workspace.place_input(&shell_edit.source()).unwrap(), None);
    assert_eq!(workspace.place_input(&tool_edit.source()).unwrap(), None);
    assert_eq!(fs::read(workspace.root().join("inputs/data.csv")).unwrap(), b"a,b\n1,2\n3,4\n");
    let inputs: Vec<_> = workspace.inputs().unwrap().into_iter().map(|input| input.path).collect();
    assert_eq!(inputs, ["inputs/photo.jpg"]);
}

#[test]
fn bytes_that_differ_from_the_upload_hash_are_not_placed() {
    let (_dir, chat, workspace) = chat();
    let mut upload = Upload::new(&chat, "h8", "scan.png", PNG);
    upload.sha256 = sha(b"other bytes");
    assert!(workspace.place_input(&upload.source()).is_err());
    assert!(!workspace.root().join("inputs/scan.png").exists());
    assert!(workspace.inputs().unwrap().is_empty());
    // Names that aren't one plain segment are refused.
    let mut escape = Upload::new(&chat, "i9", "x.txt", b"x");
    escape.name = "../x.txt".into();
    assert!(workspace.place_input(&escape.source()).is_err());
}

#[test]
fn the_sandboxed_shell_lists_and_reads_inputs() {
    if !sandbox_available() {
        eprintln!("skipping: OS process sandbox unavailable");
        return;
    }
    let (dir, chat, workspace) = chat();
    let image = Upload::new(&chat, "j1", "shot.png", PNG);
    let notes = Upload::new(&chat, "k2", "notes.txt", b"read me from the shell\n");
    workspace.place_input(&image.source()).unwrap().unwrap();
    workspace.place_input(&notes.source()).unwrap().unwrap();
    let paths = ShellPaths::prepare(workspace.root(), &dir.path().join("toolchains")).unwrap();
    let mut shell = ShellSession::open(paths).unwrap();
    let t = Duration::from_secs(20);
    let listed = shell.run("ls inputs", "call", t).unwrap();
    assert_eq!(listed.exit_code, 0, "{listed:?}");
    assert_eq!(listed.stdout, "notes.txt\nshot.png\n");
    let read = shell.run("cat inputs/notes.txt", "call", t).unwrap();
    assert_eq!((read.exit_code, read.stdout.as_str()), (0, "read me from the shell\n"), "{read:?}");
    let size = shell.run("wc -c < inputs/shot.png", "call", t).unwrap();
    assert_eq!((size.exit_code, size.stdout.trim()), (0, PNG.len().to_string().as_str()), "{size:?}");
    shell.close();
}
