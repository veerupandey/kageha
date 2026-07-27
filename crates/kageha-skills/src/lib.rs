//! Declarative skills are data, never an authority grant or an executable plugin.
//! This library is independent of the WebUI, agent runtime and vendor clients.
pub mod registry;
use kageha_contracts::execution::Control;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::{Component, Path},
};
fn error(s: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, s)
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_'))
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub body: String,
    pub license: String,
    pub compatibility: String,
    pub metadata: Value,
    pub allowed_tools: Vec<String>,
    pub triggers: Vec<String>,
    pub paths: Vec<String>,
    pub disable_model_invocation: bool,
    pub fast_paths: Value,
    pub fast_path_when: Vec<String>,
    pub content_sha256: String,
}
fn strings(value: &Value, split: bool) -> io::Result<Vec<String>> {
    let result = if value.is_null() {
        vec![]
    } else if let Some(s) = value.as_str() {
        if split {
            s.split_whitespace().map(str::to_owned).collect()
        } else {
            vec![s.into()]
        }
    } else {
        value
            .as_array()
            .ok_or_else(|| error("Skill metadata must be text or a list"))?
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| error("Skill metadata list must contain text"))
            })
            .collect::<io::Result<Vec<_>>>()?
    };
    if result.len() > 256
        || result.iter().any(|s| {
            s.len() > 4096
                || s.chars()
                    .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
        })
    {
        return Err(error("Skill metadata exceeds limits"));
    }
    Ok(result)
}
fn flag(meta: &Value, names: &[&str]) -> io::Result<Option<bool>> {
    for name in names {
        if let Some(v) = meta.get(*name) {
            return v
                .as_bool()
                .map(Some)
                .ok_or_else(|| error("Skill invocation flags must be boolean"));
        }
    }
    Ok(None)
}
impl Skill {
    pub fn parse(fallback_name: &str, text: &str) -> io::Result<Self> {
        if text.len() > 65536 || text.contains('\0') {
            return Err(error(
                "Skill instructions exceed 64 KiB or contain null bytes",
            ));
        }
        let (meta, body) = if text.starts_with("---\n") || text.starts_with("---\r\n") {
            let start = text.find('\n').unwrap() + 1;
            let mut end = None;
            let mut offset = start;
            for line in text[start..].split_inclusive('\n') {
                if line.trim_end_matches(['\n', '\r']) == "---" {
                    end = Some((offset, offset + line.len()));
                    break;
                }
                offset += line.len();
            }
            let (end, body) = end.ok_or_else(|| error("Skill frontmatter is not closed"))?;
            let meta: Value = serde_yaml::from_str(&text[start..end])
                .map_err(|_| error("Invalid skill YAML frontmatter"))?;
            if !meta.is_object() || serde_json::to_vec(&meta)?.len() > 128 * 1024 {
                return Err(error("Invalid or oversized skill frontmatter"));
            }
            (meta, text[body..].to_owned())
        } else {
            (json!({}), text.to_owned())
        };
        let name = meta["name"].as_str().unwrap_or(fallback_name).to_owned();
        if !valid_name(&name) {
            return Err(error("Invalid skill name"));
        }
        let description = meta["description"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| {
                body.lines()
                    .map(str::trim)
                    .map(|l| l.trim_start_matches('#').trim())
                    .find(|s| !s.is_empty())
                    .unwrap_or(&name)
                    .chars()
                    .take(200)
                    .collect()
            });
        if description.is_empty() || description.len() > 1024 {
            return Err(error("Skill description exceeds limits"));
        }
        let license = meta["license"].as_str().unwrap_or("").to_owned();
        let compatibility = meta["compatibility"].as_str().unwrap_or("").to_owned();
        if license.len() > 1024 || compatibility.len() > 500 {
            return Err(error("Skill compatibility metadata exceeds limits"));
        }
        let metadata = meta.get("metadata").cloned().unwrap_or(json!({}));
        if !metadata.is_object() {
            return Err(error("Skill metadata must be an object"));
        }
        let disabled = flag(
            &meta,
            &["disable-model-invocation", "disable_model_invocation"],
        )?
        .unwrap_or(false)
            || flag(
                &meta,
                &["allow_implicit_invocation", "allow-implicit-invocation"],
            )? == Some(false)
            || flag(&meta["policy"], &["allow_implicit_invocation"])? == Some(false);
        Ok(Self {
            name,
            description,
            body,
            license,
            compatibility,
            metadata,
            allowed_tools: strings(
                meta.get("allowed-tools")
                    .or_else(|| meta.get("allowed_tools"))
                    .unwrap_or(&Value::Null),
                true,
            )?,
            triggers: strings(&meta["triggers"], false)?,
            paths: strings(
                meta.get("paths")
                    .or_else(|| meta.get("globs"))
                    .unwrap_or(&Value::Null),
                false,
            )?,
            disable_model_invocation: disabled,
            fast_paths: meta
                .get("fast-path")
                .or_else(|| meta.get("fast_path"))
                .cloned()
                .unwrap_or(json!({})),
            fast_path_when: strings(
                meta.get("fast-path-when")
                    .or_else(|| meta.get("fast_path_when"))
                    .unwrap_or(&Value::Null),
                false,
            )?,
            content_sha256: hash(text.as_bytes()),
        })
    }
    pub fn peer_shareable(&self) -> bool {
        self.metadata["peer_shareable"] == true
            && self.allowed_tools.iter().all(|t| {
                matches!(
                    t.as_str(),
                    "read_file"
                        | "knowledge_search"
                        | "knowledge_list"
                        | "memory_get"
                        | "memory_search"
                )
            })
    }
}
#[derive(Clone, Debug)]
pub struct Package {
    pub skill: Skill,
    pub files: BTreeMap<String, Vec<u8>>,
    pub sha256: String,
}
impl Package {
    pub fn read(root: &Path, control: &Control<'_>) -> io::Result<Self> {
        let reader = kageha_workspace::ReadWorkspace::new(root)?;
        let mut pending = vec![root.to_owned()];
        let mut files = BTreeMap::new();
        let mut total = 0usize;
        let mut directories = 0usize;
        while let Some(dir) = pending.pop() {
            control.check()?;
            directories += 1;
            if directories > 256 {
                return Err(error("Too many skill directories"));
            }
            for entry in std::fs::read_dir(&dir)? {
                let entry = entry?;
                let kind = entry.file_type()?;
                if kind.is_symlink() {
                    return Err(error("Skill packages cannot contain symlinks"));
                }
                let path = entry.path();
                let relative = path
                    .strip_prefix(root)
                    .map_err(|_| error("Skill resource escaped root"))?;
                if !relative
                    .components()
                    .all(|c| matches!(c, Component::Normal(_)))
                    || relative.components().count() > 16
                {
                    return Err(error("Invalid skill resource path"));
                }
                if kind.is_dir() {
                    pending.push(path);
                    if pending.len() > 256 {
                        return Err(error("Skill package has too many directories"));
                    }
                    continue;
                }
                if !kind.is_file() || files.len() >= 256 {
                    return Err(error("Skill package exceeds resource limits"));
                }
                let name = relative
                    .to_str()
                    .ok_or_else(|| error("Skill resource path must be UTF-8"))?;
                let data = reader.snapshot(name, 2 * 1024 * 1024, control)?.bytes;
                total += data.len();
                if total > 8 * 1024 * 1024 {
                    return Err(error("Skill package exceeds 8 MiB"));
                }
                files.insert(name.into(), data);
            }
        }
        Self::from_files(
            root.file_name().and_then(|v| v.to_str()).unwrap_or("skill"),
            files,
        )
    }
    pub fn from_files(fallback: &str, files: BTreeMap<String, Vec<u8>>) -> io::Result<Self> {
        if files.len() > 256 {
            return Err(error("Too many skill resources"));
        }
        let mut total = 0usize;
        for (path, bytes) in &files {
            if path.is_empty()
                || path.len() > 4096
                || !Path::new(path)
                    .components()
                    .all(|c| matches!(c, Component::Normal(_)))
                || bytes.len() > 2 * 1024 * 1024
            {
                return Err(error("Invalid skill resource"));
            }
            total += bytes.len();
            if total > 8 * 1024 * 1024 {
                return Err(error("Skill package exceeds 8 MiB"));
            }
        }
        let text = std::str::from_utf8(
            files
                .get("SKILL.md")
                .ok_or_else(|| error("SKILL.md is missing"))?,
        )
        .map_err(|_| error("Skill instructions must be UTF-8"))?;
        let skill = Skill::parse(fallback, text)?;
        let required = strings(&skill.metadata["required_resources"], false)?;
        for path in required {
            if ![
                "references/",
                "scripts/",
                "assets/",
                "templates/",
                "resources/",
            ]
            .iter()
            .any(|p| path.starts_with(p))
            {
                return Err(error("Invalid required resource path"));
            }
            let bytes = files
                .get(&path)
                .filter(|v| !v.is_empty())
                .ok_or_else(|| error("A required skill resource is missing"))?;
            if path.ends_with(".json") {
                let _: Value = serde_json::from_slice(bytes)
                    .map_err(|_| error("Required skill JSON is invalid"))?;
            }
        }
        let hashes: BTreeMap<_, _> = files.iter().map(|(k, v)| (k, hash(v))).collect();
        let sha256 = hash(&serde_json::to_vec(&hashes)?);
        Ok(Self {
            skill,
            files,
            sha256,
        })
    }
    pub fn validate(&self) -> io::Result<()> {
        if self.files.len() > 256
            || self.files.values().any(|v| v.len() > 2 * 1024 * 1024)
            || self.files.values().map(Vec::len).sum::<usize>() > 8 * 1024 * 1024
        {
            return Err(error("Skill package exceeds limits"));
        }
        let verified = Self::from_files(&self.skill.name, self.files.clone())?;
        if self.sha256 != verified.sha256 || self.skill != verified.skill {
            return Err(error("Skill package metadata changed"));
        }
        Ok(())
    }
    pub fn plain(name: &str, text: &str) -> io::Result<Self> {
        Self::from_files(
            name,
            BTreeMap::from([("SKILL.md".into(), text.as_bytes().to_vec())]),
        )
    }
    pub fn required_runtimes(&self) -> BTreeSet<String> {
        let mut result = BTreeSet::new();
        if self.skill.metadata["legacy_python_helpers"] == true
            || self.files.keys().any(|s| s.ends_with(".py"))
        {
            result.insert("python".into());
        }
        result
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    #[test]
    fn manifests_preserve_declarative_metadata_and_peer_guards() {
        let raw="---\nname: source_reader\ndescription: Read sources\nallowed-tools: read_file\ntriggers: [source lookup]\npaths: [src/**]\ndisable-model-invocation: true\nmetadata:\n  peer_shareable: true\n---\nBody instructions";
        let s = Skill::parse("folder", raw).unwrap();
        assert_eq!(s.name, "source_reader");
        assert_eq!(s.body, "Body instructions");
        assert_eq!(s.triggers, vec!["source lookup"]);
        assert!(s.disable_model_invocation);
        assert!(s.peer_shareable());
        let dangerous = raw.replace("allowed-tools: read_file", "allowed-tools: browser_*");
        assert!(!Skill::parse("folder", &dangerous).unwrap().peer_shareable());
        assert!(Skill::parse("folder", "---\nname: x\n").is_err());
    }
    #[test]
    fn resource_packages_validate_and_never_follow_links() {
        let root = tempfile::tempdir().unwrap();
        let skill = root.path().join("source_reader");
        std::fs::create_dir_all(skill.join("references")).unwrap();
        std::fs::write(skill.join("SKILL.md"),"---\nname: source_reader\nmetadata:\n  required_resources: [references/source.json]\n---\nUse the source").unwrap();
        std::fs::write(skill.join("references/source.json"), "{}").unwrap();
        let control = Control {
            cancelled: &AtomicBool::new(false),
            deadline: None,
        };
        let package = Package::read(&skill, &control).unwrap();
        assert_eq!(package.files.len(), 2);
        std::os::unix::fs::symlink("/etc/passwd", skill.join("references/escape")).unwrap();
        assert!(Package::read(&skill, &control).is_err());
    }
}
