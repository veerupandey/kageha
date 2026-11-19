//! Delegated worker workspaces (spec autonomous-agent-loop, task 28.1; design section 17;
//! Requirements 3.1, 15.1 and 19.5): where a worker's files go, what it can read of its
//! parent's workspace, and what its shell session can reach.
use crate::ui_shell::{shell, Escalation};
use crate::ui_task_completion::proof;
use crate::ui_tools::{read_parent, test_free_disk, workspace_change};
use crate::{ui_uploads, ui_workspace, webui::Database};
use kageha_workflow::completion::OutcomeKind;
use kageha_workspace::shell::ShellManager;
use kageha_workspace::task::TaskWorkspace;
use serde_json::{json, Value};
use std::{
    io,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::AtomicBool,
};

const PARENT: &str = "parent";
const WORKER: &str = "worker";
const JOB: &str = "0123456789abcdef0123456789abcdef";
const TURN: &str = "worker-turn";

struct Fixture {
    directory: tempfile::TempDir,
    db: Database,
}

/// A parent chat with one delegated worker chat whose job pins `worker_workspace`.
fn fixture(worker_workspace: Value) -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let connection = rusqlite::Connection::open(directory.path().join("native.sqlite")).unwrap();
    connection.execute_batch("CREATE TABLE native_chat_turns(sequence INTEGER PRIMARY KEY,chat_id TEXT,request_id TEXT,message TEXT,answer TEXT,status TEXT);").unwrap();
    crate::webui::initialize(&connection).unwrap();
    connection.execute("INSERT INTO ui_sessions(id,title,created_at,updated_at) VALUES (?1,'Parent',1,1),(?2,'Worker',1,1)",
        [PARENT, WORKER]).unwrap();
    connection.execute("INSERT INTO ui_jobs VALUES (?1,?2,?3,'main','Write the report','running',1,1,NULL,'request',1)",
        [JOB, PARENT, WORKER]).unwrap();
    connection.execute("INSERT INTO ui_run_specs(chat_id,request_id,spec) VALUES (?1,'job',?2)",
        [WORKER, &json!({"worker_workspace":worker_workspace}).to_string()]).unwrap();
    Fixture { db: crate::agent_host::DatabaseState::open(connection).unwrap(), directory }
}

impl Fixture {
    fn workspace(&self, chat: &str) -> TaskWorkspace {
        let workspace = ui_workspace::open_for_chat(&self.db, chat).unwrap();
        test_free_disk::set(workspace.root(), u64::MAX);
        workspace
    }
    fn write(&self, chat: &str, path: &str, content: &str) -> io::Result<Value> {
        workspace_change(&self.db, chat, TURN, "write_file", &json!({"path":path,"content":content}))
            .map(|text| serde_json::from_str(&text).unwrap())
    }
    fn shell(&self, backend: &ShellManager, command: &str) -> io::Result<Value> {
        let escalation = Escalation { allowlist: Some(self.directory.path().join("allowlist.json")), granted_roots: vec![], hidden_roots: vec![] };
        shell(backend, &escalation, &self.db, WORKER, TURN, "call_1", &json!({"command":command}), &AtomicBool::new(false))
            .map(|text| serde_json::from_str(&text).unwrap())
    }
    fn artifact(&self, chat: &str, path: &str) -> bool {
        proof(&self.db, chat, TURN, &OutcomeKind::Artifact, path).unwrap()
    }
}

fn code(error: io::Error) -> String {
    serde_json::from_str::<Value>(&error.to_string()).map_or_else(|_| error.to_string(), |detail| detail["error_code"].as_str().unwrap_or_default().to_owned())
}

fn sandboxed() -> bool {
    let available = kageha_workspace::shell::sandbox_available();
    if !available {
        eprintln!("skipping: OS process sandbox unavailable");
    }
    available
}

#[test]
fn a_shared_workers_files_land_in_the_parents_agents_dir_and_are_its_artifacts() {
    let fixture = fixture(json!({"isolation":"shared","keep_worktree":true,"base_ref":"HEAD"}));
    let parent = fixture.workspace(PARENT);
    let worker = fixture.workspace(WORKER);
    let expected = ui_uploads::root(&fixture.db, PARENT).unwrap().join("workspace/agents").join(JOB);
    assert_eq!(worker.root(), expected);
    assert!(worker.root().join("todo.md").is_file() && worker.root().join(".kageha/manifest.sqlite").is_file());

    let written = fixture.write(WORKER, "outputs/report.md", "# Report").unwrap();
    assert_eq!(written["artifact_path"], "workspace/outputs/report.md");
    assert_eq!(std::fs::read_to_string(expected.join("outputs/report.md")).unwrap(), "# Report");
    // The worker's chat root holds no workspace of its own.
    assert!(!ui_uploads::root(&fixture.db, WORKER).unwrap().join("workspace").exists());
    // The file is the worker's artifact, verified from its bytes, and not the parent's.
    assert!(fixture.artifact(WORKER, "workspace/outputs/report.md"));
    assert!(!fixture.artifact(PARENT, "workspace/outputs/report.md"));
    let (file, metadata) = ui_uploads::pinned(&fixture.db, WORKER, "workspace/outputs/report.md").unwrap();
    assert_eq!((file, metadata["bytes"].as_u64()), (expected.join("outputs/report.md").canonicalize().unwrap(), Some(8)));

    // The parent's scan skips the nested workspace, and its writes can't enter it.
    std::fs::write(expected.join("notes/from-shell.md"), "worker shell output").unwrap();
    let changes = parent.scan_changes().unwrap();
    assert!(changes.iter().all(|change| !change.path.starts_with("agents/")), "{changes:?}");
    assert_eq!(worker.scan_changes().unwrap().into_iter().map(|change| change.path).collect::<Vec<_>>(), ["notes/from-shell.md"]);
    let refused = fixture.write(PARENT, &format!("agents/{JOB}/outputs/report.md"), "overwritten").unwrap_err();
    assert_eq!(code(refused), "path_outside_workspace");
    assert_eq!(std::fs::read_to_string(expected.join("outputs/report.md")).unwrap(), "# Report");
}

#[test]
fn a_worker_reads_its_parents_workspace_under_parent_and_cannot_write_there() {
    let fixture = fixture(json!({"isolation":"shared","keep_worktree":true,"base_ref":"HEAD"}));
    let parent = fixture.workspace(PARENT);
    fixture.workspace(WORKER);
    parent.write("outputs/brief.md", b"The brief").unwrap();
    parent.write_host_file(kageha_workspace::task::API_TOKEN, b"parent-token").unwrap();
    fixture.write(WORKER, "outputs/report.md", "mine").unwrap();

    let read = |path: &str| read_parent(&fixture.db, WORKER, path, None).unwrap();
    let text = read("parent/outputs/brief.md").expect("a worker path").unwrap();
    assert!(text.contains("The brief"), "{text}");
    assert!(read("workspace/parent/outputs/brief.md").unwrap().unwrap().contains("The brief"));
    // Windows work as for the worker's own files.
    let window = read_parent(&fixture.db, WORKER, "parent/outputs/brief.md", Some(4)).unwrap().unwrap().unwrap();
    assert!(window.starts_with("[outputs/brief.md bytes 4-9 of 9; end of file]") && window.ends_with("brief"), "{window}");
    // The parent's Host state and its workers' workspaces are not readable, nor is anything outside.
    for path in ["parent/.kageha/api-token", &format!("parent/agents/{JOB}/outputs/report.md"), "parent/../../x", "parent/outputs/missing.md"] {
        let error = read(path).unwrap().unwrap_err();
        let expected = if path.ends_with("missing.md") { "file_not_found" } else { "path_outside_workspace" };
        assert_eq!(code(error), expected, "{path}");
    }
    // Other chats have no parent prefix.
    assert!(read_parent(&fixture.db, PARENT, "parent/outputs/brief.md", None).unwrap().is_none());

    // Writes and edits under `parent/` are refused, and the parent's file is unchanged.
    for path in ["parent/outputs/brief.md", "workspace/parent/new.md", "./parent", "notes/../parent/x.md"] {
        assert_eq!(code(fixture.write(WORKER, path, "changed").unwrap_err()), "path_outside_workspace", "{path}");
    }
    let edit = workspace_change(&fixture.db, WORKER, TURN, "edit_file",
        &json!({"path":"parent/outputs/brief.md","edits":[{"old":"brief","new":"plan"}]})).unwrap_err();
    assert_eq!(code(edit), "path_outside_workspace");
    assert_eq!(std::fs::read_to_string(parent.root().join("outputs/brief.md")).unwrap(), "The brief");
    // The parent itself may still use a `parent/` directory of its own.
    assert_eq!(fixture.write(PARENT, "parent/notes.md", "fine").unwrap()["artifact_path"], "workspace/parent/notes.md");
}

/// Task 31.2: a worker gets no inputs of its own; it reads its parent's attachments
/// under `parent/inputs/`, and its shell reads them in the parent's workspace.
#[test]
fn a_worker_reads_its_parents_attachments_under_parent_inputs() {
    let fixture = fixture(json!({"isolation":"shared","keep_worktree":true,"base_ref":"HEAD"}));
    let parent = fixture.workspace(PARENT);
    let worker = fixture.workspace(WORKER);
    let root = ui_uploads::root(&fixture.db, PARENT).unwrap();
    std::fs::create_dir_all(root.join("inputs")).unwrap();
    std::fs::write(root.join("inputs/0a1b-brief.txt"), "Attached brief").unwrap();
    let hash = format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(b"Attached brief"));
    fixture.db.lock().unwrap().execute("INSERT INTO ui_uploads(chat_id,path,filename,mime,sha256,size) VALUES (?,'inputs/0a1b-brief.txt','brief.txt','text/plain',?,14)",
        [PARENT, &hash]).unwrap();
    assert_eq!(ui_workspace::place_inputs(&fixture.db, PARENT).unwrap().len(), 1);
    assert!(ui_workspace::place_inputs(&fixture.db, WORKER).unwrap().is_empty());
    assert!(!worker.root().join("inputs").exists());
    let text = read_parent(&fixture.db, WORKER, "parent/inputs/brief.txt", None).unwrap().unwrap().unwrap();
    assert!(text.contains("Attached brief"), "{text}");
    if sandboxed() {
        let backend = ShellManager::new(fixture.directory.path().join("toolchains"));
        let shown = fixture.shell(&backend, &format!("cat '{}'", parent.root().join("inputs/brief.txt").display())).unwrap();
        assert_eq!(shown["stdout"], "Attached brief");
    }
}

#[test]
fn a_workers_shell_runs_in_its_workspace_reads_the_parent_and_cannot_write_it() {
    if !sandboxed() {
        return;
    }
    let fixture = fixture(json!({"isolation":"shared","keep_worktree":true,"base_ref":"HEAD"}));
    let parent = fixture.workspace(PARENT);
    let worker = fixture.workspace(WORKER);
    parent.write("outputs/brief.md", b"The brief").unwrap();
    parent.write_host_file(kageha_workspace::task::API_TOKEN, b"parent-token").unwrap();
    let backend = ShellManager::new(fixture.directory.path().join("toolchains"));

    let pwd = fixture.shell(&backend, "pwd -P").unwrap();
    assert_eq!(pwd["stdout"].as_str().unwrap().trim_end(), worker.root().to_str().unwrap());
    let brief = fixture.shell(&backend, &format!("cat '{}'", parent.root().join("outputs/brief.md").display())).unwrap();
    assert_eq!(brief["stdout"], "The brief");
    // The parent's connector API token is hidden from the worker's shell.
    let token = fixture.shell(&backend, &format!("cat '{}'", parent.root().join(".kageha/api-token").display()));
    let shown = match token { Ok(result) => { assert_ne!(result["exit_code"], 0); result.to_string() } Err(error) => error.to_string() };
    assert!(!shown.contains("parent-token"), "{shown}");
    // Writing the parent's workspace is refused by the sandbox.
    let target = parent.root().join("outputs/hacked.txt");
    let denied = fixture.shell(&backend, &format!("echo x > '{}'", target.display())).unwrap_err();
    assert_eq!(code(denied), "policy_denied");
    assert!(!target.exists());
    // Its own writes are its artifacts.
    let own = fixture.shell(&backend, "mkdir -p outputs && echo y > outputs/shell.txt").unwrap();
    assert_eq!(own["files_changed"][0]["artifact_path"], "workspace/outputs/shell.txt");
    assert!(fixture.artifact(WORKER, "workspace/outputs/shell.txt"));
    assert!(parent.scan_changes().unwrap().iter().all(|change| !change.path.starts_with("agents/")));
    // Background jobs belong to the worker, with their logs in its workspace.
    let job = shell(&backend, &Escalation { allowlist: None, granted_roots: vec![], hidden_roots: vec![] }, &fixture.db, WORKER, TURN,
        "call_2", &json!({"command":"echo bg","background":true}), &AtomicBool::new(false)).unwrap();
    let job: Value = serde_json::from_str(&job).unwrap();
    let owner: String = fixture.db.lock().unwrap()
        .query_row("SELECT chat_id FROM ui_shell_jobs WHERE job_id=?", [job["job_id"].as_str().unwrap()], |row| row.get(0)).unwrap();
    assert_eq!(owner, WORKER);
    assert!(worker.host_path(job["log_path"].as_str().unwrap()).unwrap().starts_with(worker.root()));
    // The parent has no session in the worker's.
    assert!(backend.session_dir(PARENT).is_none());
}

fn git(directory: &Path, args: &[&str]) -> String {
    let output = Command::new("/usr/bin/git").current_dir(directory)
        .args(["-c", "user.name=Kageha", "-c", "user.email=kageha@example.invalid", "-c", "commit.gpgsign=false"])
        .args(args).output().unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn a_worktree_worker_uses_its_checkout_and_leaves_no_host_files_in_it() {
    if !Path::new("/usr/bin/git").is_file() || std::env::var_os("KAGEHA_GIT_BIN").is_some() {
        eprintln!("skipping: /usr/bin/git unavailable");
        return;
    }
    let options = json!({"isolation":"worktree","keep_worktree":true,"base_ref":"HEAD"});
    let fixture = fixture(options.clone());
    let source = fixture.directory.path().join("project");
    std::fs::create_dir_all(&source).unwrap();
    git(&source, &["init", "-q"]);
    std::fs::write(source.join("README.md"), "# Project\n").unwrap();
    git(&source, &["add", "README.md"]);
    git(&source, &["commit", "-q", "-m", "initial"]);
    // The job's pinned run names the source; the worktree is created as at run start.
    let run = crate::runtime_host::spec(&source, WORKER, "job", JOB, "main", "Edit the project", 1, Some(PARENT), None,
        vec![], json!({"worker_workspace":options}), 8);
    crate::runtime_host::prepare(&fixture.db.lock().unwrap(), &run).unwrap();
    let parsed = kageha_workspace::worker::Options::parse(&options).unwrap();
    let flag = AtomicBool::new(false);
    let manager = kageha_workspace::worker::Manager::new(&source,
        &fixture.directory.path().join("components/workspace/worktrees.sqlite"), Path::new("/usr/bin/git")).unwrap();
    let snapshot = manager.prepare(JOB, &parsed, &kageha_contracts::execution::Control { cancelled: &flag, deadline: None }).unwrap();
    let checkout: PathBuf = snapshot.root.unwrap();

    let worker = fixture.workspace(WORKER);
    assert_eq!(worker.root(), checkout.canonicalize().unwrap());
    assert!(!worker.state_inside());
    let state = ui_uploads::root(&fixture.db, WORKER).unwrap().join(ui_workspace::WORKTREE_STATE_DIR);
    assert_eq!(worker.state_dir(), state);
    assert!(state.join("manifest.sqlite").is_file());
    let status = |dir: &Path| git(dir, &["status", "--porcelain=v1", "--untracked-files=all", "--ignored=matching"]);
    assert_eq!(status(&checkout), "", "opening the workspace adds nothing to the checkout");
    // The checkout's files are not the worker's changes; what it writes is its artifact.
    assert!(worker.scan_changes().unwrap().is_empty());
    let written = fixture.write(WORKER, "src/feature.rs", "pub fn feature() {}\n").unwrap();
    assert_eq!(written["artifact_path"], "workspace/src/feature.rs");
    assert!(checkout.join("src/feature.rs").is_file());
    assert!(fixture.artifact(WORKER, "workspace/src/feature.rs"));
    // The worker reads its parent's workspace the same way.
    fixture.workspace(PARENT).write("outputs/brief.md", b"brief").unwrap();
    assert!(read_parent(&fixture.db, WORKER, "parent/outputs/brief.md", None).unwrap().unwrap().unwrap().contains("brief"));
    if sandboxed() {
        let backend = ShellManager::new(fixture.directory.path().join("toolchains"));
        let pwd = fixture.shell(&backend, "pwd -P").unwrap();
        assert_eq!(pwd["stdout"].as_str().unwrap().trim_end(), worker.root().to_str().unwrap());
        let made = fixture.shell(&backend, "echo done > made-by-shell.txt && test -d \"$HOME\" && echo \"$KAGEHA_API_SOCKET\"").unwrap();
        assert_eq!(made["stdout"].as_str().unwrap().trim_end(), state.join("api.sock").to_str().unwrap());
        assert_eq!(made["files_changed"][0]["artifact_path"], "workspace/made-by-shell.txt");
    }
    let mut lines: Vec<_> = status(&checkout).lines().map(str::to_owned).collect();
    lines.sort();
    let mut expected = vec!["?? src/feature.rs".to_owned()];
    if sandboxed() {
        expected.insert(0, "?? made-by-shell.txt".into());
    }
    assert_eq!(lines, expected, "only the worker's own files are in the checkout");
    assert!(!checkout.join(".kageha").exists() && !checkout.join("todo.md").exists());
}
