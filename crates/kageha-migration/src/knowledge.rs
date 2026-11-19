//! Legacy private agent knowledge: active manifests, original bytes and extracted
//! chunks are portable; embeddings and vendor indexes are deliberately not copied.
use crate::{
    documents::{Document, Domain},
    *,
};
use kageha_knowledge::Chunk;
fn basename(value: &str) -> io::Result<String> {
    let name = Path::new(value)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| error("Invalid knowledge source name"))?;
    if name.is_empty()
        || name.len() > 240
        || name == "."
        || name == ".."
        || name.chars().any(char::is_control)
    {
        return Err(error("Invalid knowledge filename"));
    }
    Ok(name.into())
}
impl Importer {
    pub(crate) fn knowledge_ids(&self,domain:Domain) -> io::Result<BTreeSet<String>> {
        let mut ids = BTreeSet::new();
        for (index, entry) in fs::read_dir(&self.source)?.enumerate() {
            if index >= 10000 {
                return Err(error("Too many knowledge root entries"));
            }
            let entry = entry?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| error("Invalid knowledge identity"))?;
            match domain {
                Domain::Knowledge=>if let Some(id)=name.strip_prefix("agent-") {
                    if !documents::agent_id(id)||!entry.file_type()?.is_dir(){return Err(error("Private knowledge store must be a real agent directory"));}
                    ids.insert(id.into());
                },
                Domain::WorkspaceKnowledge=>{
                    if name.starts_with("agent-")||name.starts_with("agent_")||name.starts_with("memory_")||name=="agent_memory"||name.starts_with('.') {continue;}
                    if !kageha_knowledge::scope::valid_id(&name)||!entry.file_type()?.is_dir(){return Err(error("Invalid workspace knowledge directory"));}
                    // Non-KB utility directories do not become data merely by naming.
                    match read_at(&self.directory,&Path::new(&name).join("manifest.yaml"),1024*1024) {
                        Ok(_)=>{ids.insert(name);},Err(e) if e.kind()==io::ErrorKind::NotFound=>{},Err(e)=>return Err(e),
                    }
                },
                _=>return Err(error("Invalid knowledge domain")),
            }
            if ids.len() > 10000 {
                return Err(error("Too many knowledge stores"));
            }
        }
        Ok(ids)
    }
    pub(crate) fn knowledge_document(&self, domain:Domain,id: &str) -> io::Result<Document> {
        let expected=if matches!(domain,Domain::Knowledge){format!("agent-{id}")}else{id.to_owned()};
        let base = PathBuf::from(&expected);
        let path = base.join("manifest.yaml");
        let manifest_bytes = read_at(&self.directory, &path, 1024 * 1024)?;
        let manifest: Value = serde_yaml::from_slice(&manifest_bytes)
            .map_err(|_| error("Invalid knowledge manifest YAML"))?;
        if manifest["id"]!=expected {return Err(error("Knowledge manifest identity mismatch"));}
        if matches!(domain,Domain::Knowledge) {
            if manifest.get("persona_id").is_some_and(|v|v!=id)||manifest.get("scope").is_some_and(|v|v!="persona"){return Err(error("Knowledge manifest identity/scope mismatch"));}
        }else if manifest.get("persona_id").is_some()||manifest.get("scope").is_some_and(|v|v!="workspace") {
            return Err(error("Private knowledge cannot be imported as shared workspace data"));
        }
        if serde_json::to_vec(&manifest)?.len() > 2 * 1024 * 1024 {
            return Err(error("Expanded knowledge manifest exceeds limit"));
        }
        let sources = manifest["sources"]
            .as_array()
            .filter(|v| v.len() <= 512)
            .ok_or_else(|| error("Knowledge manifest sources are missing or oversized"))?;
        let mut names = BTreeSet::new();
        for source in sources {
            let name = basename(
                source
                    .as_str()
                    .ok_or_else(|| error("Invalid knowledge source"))?,
            )?;
            if !names.insert(name) {
                return Err(error("Ambiguous knowledge source basenames"));
            }
        }
        let mut fingerprints = BTreeMap::new();
        fingerprints.insert(path.to_string_lossy().into_owned(), digest(&manifest_bytes));
        self.blob("manifest.yaml", &manifest_bytes)?;
        let mut by_source = BTreeMap::<String, Vec<Chunk>>::new();
        let chunk_path = base.join("chunks.jsonl");
        let mut total = manifest_bytes.len();
        match read_at(&self.directory, &chunk_path, FILE_LIMIT) {
            Ok(bytes) => {
                total += bytes.len();
                fingerprints.insert(chunk_path.to_string_lossy().into(), digest(&bytes));
                self.blob("chunks.jsonl", &bytes)?;
                let text = std::str::from_utf8(&bytes)
                    .map_err(|_| error("Knowledge chunks must be UTF-8"))?;
                let mut chunk_ids = BTreeSet::new();
                for (index, line) in text.lines().filter(|l| !l.trim().is_empty()).enumerate() {
                    if index >= 262144 || line.len() > 512 * 1024 {
                        return Err(error("Knowledge chunk archive exceeds limits"));
                    }
                    let raw: Value = serde_json::from_str(line)
                        .map_err(|_| error("Invalid extracted knowledge chunk"))?;
                    let source = basename(
                        raw["source"]
                            .as_str()
                            .ok_or_else(|| error("Chunk source is missing"))?,
                    )?;
                    // Manifest membership is the deletion boundary. Never reintroduce
                    // orphaned chunks left by an interrupted old reindex/delete.
                    if !names.contains(&source) {
                        continue;
                    }
                    let chunk: Chunk =
                        serde_json::from_value(raw).map_err(|_| error("Invalid chunk fields"))?;
                    if chunk.id.is_empty()
                        || chunk.id.len() > 256
                        || chunk.text.len() > 256 * 1024
                        || chunk.heading.len() > 4096
                        || !chunk_ids.insert(chunk.id.clone())
                    {
                        return Err(error("Invalid or duplicate knowledge chunk identity"));
                    }
                    let rows = by_source.entry(source).or_default();
                    rows.push(chunk);
                    if rows.len() > 512 {
                        return Err(error("Knowledge source has too many chunks"));
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let mut files = Vec::new();
        for name in names {
            let relative = base.join("raw").join(&name);
            let bytes = read_at(&self.directory, &relative, FILE_LIMIT)?;
            total = total.saturating_add(bytes.len());
            if total > SESSION_LIMIT {
                return Err(error("Knowledge store exceeds import byte limits"));
            }
            let sha256 = digest(&bytes);
            fingerprints.insert(relative.to_string_lossy().into(), sha256.clone());
            let owned = self.blob(&name, &bytes)?;
            let mut chunks = by_source.remove(&name).unwrap_or_default();
            if chunks.is_empty()
                && Path::new(&name)
                    .extension()
                    .and_then(|v| v.to_str())
                    .is_some_and(|v| {
                        matches!(
                            v.to_ascii_lowercase().as_str(),
                            "txt" | "md" | "csv" | "json"
                        )
                    })
            {
                let text = std::str::from_utf8(&bytes)
                    .map_err(|_| error("Text knowledge source is not UTF-8"))?;
                chunks = kageha_knowledge::chunks_from_text(&sha256, text)?;
            }
            files.push(json!({"filename":name,"sha256":sha256,"bytes":bytes.len(),"owned_path":owned,"chunks":chunks}));
        }
        let source_digest = digest(&serde_json::to_vec(&fingerprints)?);
        Ok(Document {
            format_version: 1,
            domain,
            id: id.into(),
            source_id: self.document_source(domain),
            source_digest,
            value: Some(
                json!({"manifest":manifest,"files":files,"capabilities":kageha_knowledge::capabilities()}),
            ),
            revisions: vec![],
            source_files: fingerprints,
        })
    }
}
