//! Portable specialist definitions/revisions and interrupted task-plan snapshots.
use super::*;
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Domain {
    Agents,
    TaskPlans,
    Knowledge,
    WorkspaceKnowledge,
    Projects,
}
impl Domain {
    pub fn name(self) -> &'static str {
        match self {
            Self::Agents => "agents",
            Self::TaskPlans => "task-plans",
            Self::Knowledge => "agent-knowledge",
            Self::WorkspaceKnowledge => "workspace-knowledge",
            Self::Projects => "projects",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Revision {
    pub id: String,
    pub value: Value,
    pub sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Document {
    pub format_version: u32,
    pub domain: Domain,
    pub id: String,
    pub source_id: String,
    pub source_digest: String,
    pub value: Option<Value>,
    pub revisions: Vec<Revision>,
    pub source_files: BTreeMap<String, String>,
}
pub trait DocumentSink {
    fn apply(&mut self, document: &Document) -> io::Result<()>;
    fn verify(&mut self, document: &Document) -> io::Result<()>;
}
pub use kageha_contracts::agents::valid_id as agent_id;
fn agent(value: &Value, id: &str) -> io::Result<()> {
    if !value.is_object()
        || value["id"] != id
        || !agent_id(id)
        || id == "main"
        || value["builtin"] == true
    {
        return Err(error("Invalid or protected imported agent identity"));
    }
    if value.get("schema_version").is_some_and(|v| v != 1) {
        return Err(error("Unsupported agent schema version"));
    }
    for (key, max) in [("name", 200), ("system_prompt", 65536)] {
        let v = value[key].as_str().unwrap_or("");
        if (key == "name" && v.trim().is_empty()) || v.len() > max {
            return Err(error("Invalid agent instructions/name"));
        }
    }
    for field in ["pinned_skills"] {
        if value.get(field).is_some_and(|v| {
            !v.is_null()
                && !v
                    .as_array()
                    .is_some_and(|rows| rows.len() <= 256 && rows.iter().all(|v| v.is_string()))
        }) {
            return Err(error("Invalid agent skill references"));
        }
    }
    for field in ["tool_policy", "model_preferences", "knowledge"] {
        if value
            .get(field)
            .is_some_and(|v| !v.is_object() && !v.is_null())
        {
            return Err(error("Invalid agent configuration"));
        }
    }
    for field in ["allow", "deny"] {
        if value["tool_policy"].get(field).is_some_and(|v| {
            !v.is_null()
                && !v
                    .as_array()
                    .is_some_and(|rows| rows.len() <= 512 && rows.iter().all(|v| v.is_string()))
        }) {
            return Err(error("Invalid imported tool policy"));
        }
    }
    kageha_policy::specialist::validate(&value["tool_policy"])?;
    kageha_knowledge::scope::Config::parse(&value["knowledge"])?;
    for field in ["builtin", "skills_restricted_to_pinned"] {
        if value
            .get(field)
            .is_some_and(|v| !v.is_null() && !v.is_boolean())
        {
            return Err(error("Invalid agent policy flag"));
        }
    }
    Ok(())
}
fn task(value: &Value) -> io::Result<()> {
    if !value.is_object() || value.get("version").is_some_and(|v| v != 1) {
        return Err(error("Unsupported task-state version"));
    }
    if value["objective"].as_str().is_none_or(|s| s.len() > 65536) {
        return Err(error("Task objective is missing or oversized"));
    }
    if value
        .get("revision")
        .is_some_and(|v| v.as_i64().is_none_or(|v| v < 0))
    {
        return Err(error("Unsupported task revision"));
    }
    let stages = value["stages"]
        .as_array()
        .filter(|v| v.len() <= 64)
        .ok_or_else(|| error("Task stages are missing or oversized"))?;
    let mut ids = BTreeSet::new();
    for stage in stages {
        let id = stage["id"]
            .as_str()
            .filter(|id| identifier(id))
            .ok_or_else(|| error("Invalid task stage identity"))?;
        if !ids.insert(id) {
            return Err(error("Duplicate task stage identity"));
        }
        if stage["description"].as_str().is_none_or(|s| s.len() > 2000) {
            return Err(error("Invalid task stage description"));
        }
        if !matches!(
            stage["status"].as_str(),
            Some(
                "pending"
                    | "active"
                    | "blocked"
                    | "done"
                    | "failed"
                    | "skipped"
                    | "waiting"
                    | "superseded"
            )
        ) {
            return Err(error("Unsupported task stage status"));
        }
    }
    for stage in stages {
        if stage.get("depends_on").is_some_and(|v| {
            !v.as_array().is_some_and(|v| {
                v.iter()
                    .all(|id| id.as_str().is_some_and(|s| ids.contains(s)))
            })
        }) {
            return Err(error("Task dependencies refer to missing stages"));
        }
    }
    let mut done = BTreeSet::new();
    for _ in 0..=ids.len() {
        for stage in stages {
            if stage["depends_on"]
                .as_array()
                .into_iter()
                .flatten()
                .all(|v| v.as_str().is_some_and(|s| done.contains(s)))
            {
                done.insert(stage["id"].as_str().unwrap());
            }
        }
    }
    if done.len() != ids.len() {
        return Err(error("Task dependencies contain a cycle"));
    }
    let goals = value.get("goals").and_then(Value::as_array);
    if value.get("goals").is_some_and(|v| !v.is_array()) || goals.is_some_and(|v| v.len() > 128) {
        return Err(error("Invalid required outcomes"));
    }
    let mut goals_seen = BTreeSet::new();
    for goal in goals.into_iter().flatten() {
        let id = goal["id"]
            .as_str()
            .filter(|id| identifier(id))
            .ok_or_else(|| error("Invalid required outcome identity"))?;
        if !goals_seen.insert(id) || goal["description"].as_str().is_none_or(|s| s.len() > 2000) {
            return Err(error("Invalid required outcome"));
        }
    }
    for stage in stages {
        if stage.get("outcome_ids").is_some_and(|v| {
            !v.as_array().is_some_and(|v| {
                v.iter()
                    .all(|id| id.as_str().is_some_and(|s| goals_seen.contains(s)))
            })
        }) {
            return Err(error("Task stage lost its required outcome"));
        }
    }
    Ok(())
}
fn source_key(domain: Domain, path: &Path) -> String {
    let mut key = domain.name().as_bytes().to_vec();
    key.push(0);
    key.extend_from_slice(path.as_os_str().as_bytes());
    digest(&key)
}
impl Importer {
    pub(crate) fn document_source(&self, domain: Domain) -> String {
        source_key(domain, &self.source)
    }
    fn document_ids(&self, domain: Domain) -> io::Result<BTreeSet<String>> {
        let mut ids = BTreeSet::new();
        match domain {
            Domain::Projects => { self.projects_document()?; ids.insert("catalog".into()); }
            Domain::Knowledge | Domain::WorkspaceKnowledge => return self.knowledge_ids(domain),
            Domain::Agents => {
                for entry in fs::read_dir(&self.source)? {
                    let entry = entry?;
                    let path = entry.path();
                    let name = entry
                        .file_name()
                        .into_string()
                        .map_err(|_| error("Invalid agent filename"))?;
                    if name == ".default-knowledge.yaml" {
                        ids.insert("main".into());
                        continue;
                    }
                    if path.extension().is_some_and(|v| v == "yaml") && !name.starts_with('.') {
                        let id = path
                            .file_stem()
                            .and_then(|v| v.to_str())
                            .filter(|id| agent_id(id) && *id != "main")
                            .ok_or_else(|| error("Invalid or protected agent filename"))?;
                        ids.insert(id.to_owned());
                    }
                }
                let history = self.source.join(".history");
                if history.exists() {
                    if fs::symlink_metadata(&history)?.file_type().is_symlink() {
                        return Err(error("Agent history root cannot be a symlink"));
                    }
                    for entry in fs::read_dir(history)? {
                        let entry = entry?;
                        if !entry.file_type()?.is_dir() {
                            return Err(error("Invalid agent revision directory"));
                        }
                        let id = entry
                            .file_name()
                            .into_string()
                            .map_err(|_| error("Invalid agent revision identity"))?;
                        if !agent_id(&id) || id == "main" {
                            return Err(error("Invalid or protected agent revision identity"));
                        }
                        ids.insert(id);
                    }
                }
            }
            Domain::TaskPlans => {
                for entry in fs::read_dir(&self.source)? {
                    let entry = entry?;
                    if entry.file_type()?.is_dir() && entry.path().join("task_state.json").exists()
                    {
                        let id = entry
                            .file_name()
                            .into_string()
                            .map_err(|_| error("Invalid task session identity"))?;
                        if !identifier(&id) {
                            return Err(error("Invalid task session identity"));
                        }
                        ids.insert(id);
                    }
                }
            }
        }
        if ids.len() > 10000 {
            return Err(error("Too many documents"));
        }
        Ok(ids)
    }
    fn document(&self, domain: Domain, id: &str) -> io::Result<Document> {
        let mut files = BTreeMap::new();
        let mut revisions = Vec::new();
        let mut expanded_bytes = 0usize;
        let mut source_bytes = 0usize;
        let relative = match domain {
            Domain::Projects => return self.projects_document(),
            Domain::Knowledge | Domain::WorkspaceKnowledge => return self.knowledge_document(domain,id),
            Domain::Agents if id == "main" => PathBuf::from(".default-knowledge.yaml"),
            Domain::Agents => PathBuf::from(format!("{id}.yaml")),
            Domain::TaskPlans => Path::new(id).join("task_state.json"),
        };
        let value = match read_at(&self.directory, &relative, 1024 * 1024) {
            Ok(bytes) => {
                source_bytes += bytes.len();
                files.insert(relative.to_string_lossy().into_owned(), digest(&bytes));
                let value: Value = match domain {
                    Domain::Agents | Domain::Knowledge | Domain::WorkspaceKnowledge => {
                        serde_yaml::from_slice(&bytes).map_err(|_| error("Invalid agent YAML"))?
                    }
                    Domain::TaskPlans | Domain::Projects => serde_json::from_slice(&bytes)
                        .map_err(|_| error("Invalid task-state JSON"))?,
                };
                expanded_bytes += serde_json::to_vec(&value)?.len();
                if expanded_bytes > 2 * 1024 * 1024 {
                    return Err(error("Expanded document exceeds limit"));
                }
                match domain {
                    Domain::Agents if id == "main" => {
                        if !value.is_object() {
                            return Err(error("Default knowledge overlay must be an object"));
                        }
                    }
                    Domain::Knowledge | Domain::WorkspaceKnowledge => return Err(error("Invalid knowledge projection path")),
                    Domain::Agents => agent(&value, id)?,
                    Domain::TaskPlans => task(&value)?,
                    Domain::Projects => return Err(error("Invalid project projection path")),
                };
                self.blob(relative.file_name().unwrap().to_str().unwrap(), &bytes)?;
                Some(value)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound && matches!(domain, Domain::Agents) => {
                None
            }
            Err(e) => return Err(e),
        };
        if matches!(domain, Domain::Agents) {
            let history = self.source.join(".history").join(id);
            if history.exists() {
                let mut paths = fs::read_dir(&history)?
                    .take(501)
                    .map(|e| e.map(|e| e.file_name()))
                    .collect::<io::Result<Vec<_>>>()?;
                paths.sort();
                if paths.len() > 500 {
                    return Err(error("Too many agent revisions"));
                }
                for name in paths {
                    let path = PathBuf::from(".history").join(id).join(&name);
                    if path.extension().is_none_or(|v| v != "yaml") {
                        return Err(error("Unsupported agent revision file"));
                    }
                    let bytes = read_at(&self.directory, &path, 262144)?;
                    source_bytes += bytes.len();
                    let value: Value = serde_yaml::from_slice(&bytes)
                        .map_err(|_| error("Invalid agent revision YAML"))?;
                    let size = serde_json::to_vec(&value)?.len();
                    expanded_bytes += size;
                    if size > 2 * 1024 * 1024
                        || expanded_bytes > SESSION_LIMIT
                        || source_bytes > SESSION_LIMIT
                    {
                        return Err(error("Agent revision archive exceeds byte limits"));
                    }
                    agent(&value, id)?;
                    let sha256 = digest(&bytes);
                    files.insert(path.to_string_lossy().into(), sha256.clone());
                    self.blob(
                        name.to_str()
                            .ok_or_else(|| error("Invalid revision name"))?,
                        &bytes,
                    )?;
                    revisions.push(Revision {
                        id: name.to_string_lossy().into(),
                        value,
                        sha256,
                    });
                }
            }
        }
        let source_id = self.document_source(domain);
        let source_digest = digest(&serde_json::to_vec(&files)?);
        Ok(Document {
            format_version: 1,
            domain,
            id: id.into(),
            source_id,
            source_digest,
            value,
            revisions,
            source_files: files,
        })
    }
    pub fn run_documents(
        &mut self,
        domain: Domain,
        sink: &mut dyn DocumentSink,
    ) -> io::Result<Report> {
        let ids = self.document_ids(domain)?;
        let source_id = self.document_source(domain);
        let prior = self
            .journal
            .prepare("SELECT chat FROM history_import_v1 WHERE source=?")
            .map_err(sql)?
            .query_map([&source_id], |r| r.get::<_, String>(0))
            .map_err(sql)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql)?;
        if prior.iter().any(|id| !ids.contains(id)) {
            return Err(error("Previously imported document is missing"));
        }
        let mut report = Report::default();
        for id in ids {
            let document = self.document(domain, &id)?;
            let imported = self.project_record(
                &source_id,
                &id,
                &document.source_digest,
                serde_json::to_string(&document)?,
                |applied| {
                    if !applied {
                        if self.document(domain, &id)?.source_digest != document.source_digest {
                            return Err(error("Document changed before projection"));
                        }
                        sink.apply(&document)?;
                    }
                    sink.verify(&document)
                },
            )?;
            if imported {
                report.imported += 1;
            } else {
                report.already_present += 1;
            }
        }
        Ok(report)
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn source_keys_use_exact_path_bytes_and_domain_namespaces() {
        use std::os::unix::ffi::OsStringExt;
        let a = std::path::PathBuf::from(std::ffi::OsString::from_vec(vec![b's', 0xff]));
        let b = std::path::PathBuf::from(std::ffi::OsString::from_vec(vec![b's', 0xfe]));
        assert_eq!(a.to_string_lossy(), b.to_string_lossy());
        assert_ne!(
            super::source_key(super::Domain::Agents, &a),
            super::source_key(super::Domain::Agents, &b)
        );
        assert_ne!(
            super::source_key(super::Domain::Agents, &a),
            super::source_key(super::Domain::TaskPlans, &a)
        );
    }
}
