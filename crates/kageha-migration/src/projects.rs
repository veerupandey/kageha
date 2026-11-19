//! Read-only bounded project and session-organization snapshots from a copied legacy home.
use crate::{
    documents::{Document, Domain},
    *,
};
impl Importer {
    pub(crate) fn projects_document(&self) -> io::Result<Document> {
        let bytes = read_at(&self.directory, Path::new("projects.json"), FILE_LIMIT)?;
        let catalog: Value =
            serde_json::from_slice(&bytes).map_err(|_| error("Invalid projects.json"))?;
        let mut files = BTreeMap::from([("projects.json".into(), digest(&bytes))]);
        let mut sessions = BTreeMap::new();
        let mut size = bytes.len();
        let root = self.source.join("sessions");
        match fs::symlink_metadata(&root) {
            Ok(meta) => {
                if !meta.is_dir() || meta.file_type().is_symlink() {
                    return Err(error("Historical sessions must be a real directory"));
                }
                for (index, entry) in fs::read_dir(root)?.enumerate() {
                    if index >= 10000 {
                        return Err(error("Too many historical project chats"));
                    }
                    let entry = entry?;
                    let name = entry
                        .file_name()
                        .into_string()
                        .map_err(|_| error("Invalid chat identity"))?;
                    if name.starts_with('.') {
                        continue;
                    }
                    if !identifier(&name) || !entry.file_type()?.is_dir() {
                        return Err(error("Historical chat must be a real identified directory"));
                    }
                    let mut meta = serde_json::Map::new();
                    let mut originals = serde_json::Map::new();
                    for filename in ["meta.json", "metadata.json", "session.json"] {
                        let path = Path::new("sessions").join(&name).join(filename);
                        let bytes = match read_at(&self.directory, &path, 1024 * 1024) {
                            Ok(b) => b,
                            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                            Err(e) => return Err(e),
                        };
                        size += bytes.len();
                        if size > SESSION_LIMIT {
                            return Err(error("Historical project metadata exceeds limits"));
                        }
                        let value: Value = serde_json::from_slice(&bytes)
                            .map_err(|_| error("Invalid project chat metadata"))?;
                        let object = value
                            .as_object()
                            .ok_or_else(|| error("Project chat metadata must be an object"))?;
                        if object.get("session_id").is_some_and(|v| v != &name) {
                            return Err(error("Project chat identity mismatch"));
                        }
                        for (key, value) in object {
                            if matches!(key.as_str(), "project_id" | "project" | "tagged_turns")
                                && meta
                                    .get(key)
                                    .filter(|v| !v.is_null())
                                    .is_some_and(|old| !value.is_null() && old != value)
                            {
                                return Err(error("Historical project metadata files conflict"));
                            }
                            if !value.is_null() {
                                meta.entry(key.clone()).or_insert_with(|| value.clone());
                            }
                        }
                        originals.insert(filename.into(), value);
                        files.insert(path.to_string_lossy().into(), digest(&bytes));
                    }
                    if !originals.is_empty() {
                        meta.insert("_legacy_metadata_files".into(), Value::Object(originals));
                        sessions.insert(name, Value::Object(meta));
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let archive = kageha_workspace::projects::import::Archive::from_legacy(catalog, sessions)?;
        Ok(Document {
            format_version: 1,
            domain: Domain::Projects,
            id: "catalog".into(),
            source_id: self.document_source(Domain::Projects),
            source_digest: digest(&serde_json::to_vec(&files)?),
            value: Some(serde_json::to_value(archive)?),
            revisions: vec![],
            source_files: files,
        })
    }
}
