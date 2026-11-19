// Files sent by the owner in the paired WhatsApp conversation.
export function initIncomingFiles(db) {
  db.exec(`CREATE TABLE IF NOT EXISTS incoming_files(
    id TEXT PRIMARY KEY, name TEXT NOT NULL, mime TEXT NOT NULL, bytes BLOB NOT NULL,
    task INTEGER, command TEXT, session TEXT, path TEXT)`);
}

export function bindIncomingFiles(db, task, command = null) {
  db.prepare(`UPDATE incoming_files SET task=?,command=? WHERE id IN (
    SELECT id FROM incoming_files WHERE task IS NULL AND (
      session IS NULL OR session=(SELECT session FROM tasks WHERE id=?)
    ) ORDER BY rowid LIMIT 8)`).run(task, command, task);
}

export async function uploadIncomingFiles(db, task, session, api, command = null) {
  const files = db.prepare(
    "SELECT * FROM incoming_files WHERE task=? AND command IS ? ORDER BY rowid",
  ).all(task, command);
  const paths = [];
  for (const file of files) {
    let path = file.session === session && file.path;
    if (!path) {
      const form = new FormData();
      form.append("file", new Blob([file.bytes], { type: file.mime }), file.name);
      const upload = await api("/api/sessions/" + encodeURIComponent(session) + "/upload", form);
      path = upload.path;
      if (!path) throw Error("Missing attachment receipt");
      db.prepare("UPDATE incoming_files SET session=?,path=? WHERE id=?").run(session, path, file.id);
    }
    paths.push(path);
  }
  return paths;
}
