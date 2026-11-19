//! Attachments in the Task_Workspace (spec autonomous-agent-loop, task 31.2; design
//! section 18; Requirement 20.1): the chat's uploads are copied to `workspace/inputs/` at
//! turn start and after a new upload, every tool can read them, and they are never the
//! agent's artifacts.
use crate::audited_session_tests::{main_persona, Fixture, REQUEST};
use crate::native_chat::ToolHost;
use crate::ui_task_completion::proof;
use crate::{ui_uploads, ui_workspace, webui::Database};
use kageha_workflow::completion::OutcomeKind;
use rusqlite::params;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

// Shell sessions outlive a fixture and are keyed by chat ID.
const CHAT: &str = "workspace-input-fixture";

fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Stores an upload the way `ui_uploads::save` does: `<chat root>/inputs/<id>-<name>`
/// and a `ui_uploads` row. Returns its chat-relative path.
fn upload(db: &Database, id: &str, name: &str, bytes: &[u8]) -> String {
    let root = ui_uploads::root(db, CHAT).unwrap();
    std::fs::create_dir_all(root.join("inputs")).unwrap();
    let path = format!("inputs/{id}-{name}");
    std::fs::write(root.join(&path), bytes).unwrap();
    db.lock().unwrap().execute(
        "INSERT INTO ui_uploads(chat_id,path,filename,mime,sha256,size) VALUES (?,?,?,?,?,?)",
        params![CHAT, path, name, ui_uploads::mime(name), sha(bytes), bytes.len()],
    ).unwrap();
    path
}

fn invoke(host: &dyn ToolHost, name: &str, arguments: Value) -> std::io::Result<String> {
    host.invoke(&json!({"id":"call","name":name,"arguments":arguments}))
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nscreenshot bytes";
const NOTES: &[u8] = b"Release notes for the attachment test\n";

#[test]
fn uploads_are_in_the_workspace_readable_by_every_tool_and_never_artifacts() {
    let fixture = Fixture::for_chat(CHAT);
    let image = upload(&fixture.db, "a1b2", "screen.png", PNG);
    let notes = upload(&fixture.db, "c3d4", "notes.txt", NOTES);
    fixture.db.lock().unwrap().execute("INSERT OR REPLACE INTO ui_turn_uploads(chat_id,request_id,paths) VALUES (?,?,?)",
        params![CHAT, REQUEST, json!([image, notes]).to_string()]).unwrap();

    // Turn start: both uploads are in `workspace/inputs/` with the same bytes.
    let placed = ui_workspace::place_inputs(&fixture.db, CHAT).unwrap();
    assert_eq!(placed.iter().map(|input| (input.key.as_str(), input.path.as_str())).collect::<Vec<_>>(),
        [(notes.as_str(), "inputs/notes.txt"), (image.as_str(), "inputs/screen.png")]);
    let workspace = ui_workspace::open_for_chat(&fixture.db, CHAT).unwrap();
    assert_eq!(sha(&std::fs::read(workspace.root().join("inputs/screen.png")).unwrap()), sha(PNG));
    assert_eq!(sha(&std::fs::read(workspace.root().join("inputs/notes.txt")).unwrap()), sha(NOTES));
    // Placing again at the next turn changes nothing.
    assert_eq!(ui_workspace::place_inputs(&fixture.db, CHAT).unwrap(), placed);

    // Not changes, not artifacts, not evidence.
    assert!(workspace.scan_changes().unwrap().is_empty());
    let artifacts: i64 = fixture.db.lock().unwrap()
        .query_row("SELECT count(*) FROM ui_turn_artifacts WHERE chat_id=?", [CHAT], |row| row.get(0)).unwrap();
    assert_eq!(artifacts, 0);
    assert!(!proof(&fixture.db, CHAT, &fixture.turn, &OutcomeKind::Artifact, "workspace/inputs/notes.txt").unwrap());

    // The turn's note names both paths of each attachment.
    let note = ui_workspace::inputs_note(&fixture.db, CHAT, REQUEST).unwrap();
    let text = note["content"].as_str().unwrap();
    assert_eq!(note["role"], "system");
    assert!(text.contains(&format!("`{image}` is `inputs/screen.png`")) && text.contains(&format!("`{notes}` is `inputs/notes.txt`")), "{text}");
    assert_eq!(ui_workspace::inputs_note(&fixture.db, CHAT, "other-request"), None);

    // read_file takes the workspace path, the original chat path and `workspace/inputs/...`.
    let host = fixture.tools(main_persona(Value::Null));
    for path in ["inputs/notes.txt", "workspace/inputs/notes.txt"] {
        let text = invoke(&host, "read_file", json!({"path":path})).unwrap();
        assert!(text.contains("Release notes for the attachment test"), "{path}: {text}");
    }
    // The stored form still reads the pinned upload. That path extracts text in the
    // document worker process, which the test binary doesn't serve, so check the pin.
    invoke(&host, "read_file", json!({"path":notes})).unwrap();
    assert_eq!(ui_uploads::pinned(&fixture.db, CHAT, &notes).unwrap().1["sha256"], sha(NOTES));
    // list_workspace_files shows `inputs/` and marks the user's files.
    let listing: Value = serde_json::from_str(&invoke(&host, "list_workspace_files", json!({})).unwrap()).unwrap();
    let listing = &listing["workspace"];
    assert!(listing["directories"].as_array().unwrap().contains(&json!("inputs")), "{listing}");
    let entry = listing["files"].as_array().unwrap().iter().find(|file| file["path"] == "inputs/screen.png").unwrap();
    assert_eq!(entry["input"], true);

    // The sandboxed shell lists and reads them; doing so changes no file.
    if crate::ui_shell::available() {
        let result: Value = serde_json::from_str(&invoke(&host, "shell", json!({"command":"ls inputs && cat inputs/notes.txt"})).unwrap()).unwrap();
        assert_eq!(result["exit_code"], 0, "{result}");
        assert_eq!(result["stdout"], "notes.txt\nscreen.png\nRelease notes for the attachment test\n", "{result}");
        assert_eq!(result["files_changed"], json!([]), "{result}");
    } else {
        eprintln!("skipping the shell check: OS process sandbox unavailable");
    }

    // An input the agent edits is its own file from then on, registered like any other.
    let edited: Value = serde_json::from_str(&invoke(&host, "write_file", json!({"path":"inputs/notes.txt","content":"edited"})).unwrap()).unwrap();
    assert_eq!(edited["artifact_path"], "workspace/inputs/notes.txt");
    assert_eq!(std::fs::read(ui_uploads::root(&fixture.db, CHAT).unwrap().join(&notes)).unwrap(), NOTES, "the upload is unchanged");
    assert_eq!(ui_workspace::place_inputs(&fixture.db, CHAT).unwrap().len(), 1);
    assert_eq!(std::fs::read(workspace.root().join("inputs/notes.txt")).unwrap(), b"edited");
}

#[test]
fn an_upload_during_a_running_turn_is_placed_right_away() {
    let fixture = Fixture::for_chat(CHAT);
    let workspace = ui_workspace::open_for_chat(&fixture.db, CHAT).unwrap();
    upload(&fixture.db, "e5f6", "first.txt", b"first");
    ui_workspace::place_new_inputs(&fixture.db, CHAT);
    assert_eq!(std::fs::read(workspace.root().join("inputs/first.txt")).unwrap(), b"first");

    // The turn is running; the user attaches another file with a name already taken.
    workspace.write("inputs/later.txt", b"the agent's file").unwrap();
    let later = upload(&fixture.db, "a7b8", "later.txt", b"attached mid-turn");
    ui_workspace::place_new_inputs(&fixture.db, CHAT);
    assert_eq!(std::fs::read(workspace.root().join("inputs/a7b8-later.txt")).unwrap(), b"attached mid-turn");
    assert_eq!(std::fs::read(workspace.root().join("inputs/later.txt")).unwrap(), b"the agent's file");
    let host = fixture.tools(main_persona(Value::Null));
    // In the workspace it has the stored name; that path also reads the pinned upload.
    let text = invoke(&host, "read_file", json!({"path":"workspace/inputs/a7b8-later.txt"})).unwrap();
    assert!(text.contains("attached mid-turn"), "{text}");
    invoke(&host, "read_file", json!({"path":later})).unwrap();
    assert!(workspace.scan_changes().unwrap().is_empty());
}
