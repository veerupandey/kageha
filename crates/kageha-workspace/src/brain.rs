//! Bounded workspace instructions and declarative project recipes. No execution.
use crate::ReadWorkspace;
use kageha_contracts::execution::Control;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
};
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Rule {
    pub name: String,
    pub body: String,
    pub globs: Vec<String>,
    pub path: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Brain {
    pub project_root: PathBuf,
    pub root_file: String,
    pub root_text: String,
    pub rules: Vec<Rule>,
    pub command_names: Vec<String>,
}
fn trim(text: &str, limit: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= limit {
        text.into()
    } else {
        format!(
            "{}\n…[truncated]",
            text.chars()
                .take(limit.saturating_sub(20))
                .collect::<String>()
                .trim_end()
        )
    }
}
fn read(
    reader: &ReadWorkspace,
    path: &str,
    limit: usize,
    control: &Control<'_>,
) -> io::Result<Option<String>> {
    control.check()?;
    let mut file = match reader.open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut bytes = Vec::new();
    file.by_ref()
        .take((limit * 4 + 4) as u64)
        .read_to_end(&mut bytes)?;
    control.check()?;
    Ok(Some(trim(&String::from_utf8_lossy(&bytes), limit)))
}
fn files(
    root: &Path,
    folder: &str,
    recursive: bool,
    control: &Control<'_>,
) -> io::Result<Vec<String>> {
    let base = root.join(folder);
    match fs::symlink_metadata(&base) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e),
        Ok(meta) if !meta.is_dir() || meta.file_type().is_symlink() => {
            return Err(io::Error::other(
                "Project instruction directory must be a real directory",
            ))
        }
        _ => {}
    }
    let mut pending = vec![base];
    let mut found = Vec::new();
    let mut entries = 0;
    while let Some(dir) = pending.pop() {
        control.check()?;
        for entry in fs::read_dir(dir)? {
            entries += 1;
            if entries > 4096 {
                return Err(io::Error::other(
                    "Project instruction discovery exceeds limits",
                ));
            }
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                return Err(io::Error::other(
                    "Project instruction discovery cannot follow links",
                ));
            }
            let path = entry.path();
            if kind.is_dir() {
                if recursive {
                    if path
                        .strip_prefix(root)
                        .map_err(io::Error::other)?
                        .components()
                        .count()
                        > 16
                    {
                        return Err(io::Error::other(
                            "Project instruction nesting exceeds limits",
                        ));
                    }
                    pending.push(path);
                }
                continue;
            }
            if kind.is_file()
                && path.extension().and_then(|v| v.to_str()).is_some_and(|v| {
                    matches!(v.to_ascii_lowercase().as_str(), "md" | "mdc" | "txt")
                })
            {
                found.push(
                    path.strip_prefix(root)
                        .map_err(io::Error::other)?
                        .to_str()
                        .ok_or_else(|| io::Error::other("Project instruction paths must be UTF-8"))?
                        .into(),
                );
            }
        }
    }
    found.sort();
    Ok(found)
}
fn rule(path: &str, text: &str) -> io::Result<Option<Rule>> {
    let normalized = text.replace("\r\n", "\n");
    let text = normalized.as_str();
    let mut body = text;
    let mut globs = Vec::new();
    if text.starts_with("---\n") {
        let end = text[4..]
            .find("\n---")
            .ok_or_else(|| io::Error::other("Unclosed project rule metadata"))?
            + 4;
        let meta: serde_json::Value =
            serde_yaml::from_str(&text[4..end]).map_err(io::Error::other)?;
        let value = meta.get("globs").or_else(|| meta.get("glob"));
        if let Some(value) = value {
            globs = if let Some(s) = value.as_str() {
                s.split(',').map(|s| s.trim().to_owned()).collect()
            } else {
                value
                    .as_array()
                    .ok_or_else(|| io::Error::other("Invalid rule globs"))?
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| io::Error::other("Invalid rule glob"))
                    })
                    .collect::<io::Result<Vec<_>>>()?
            };
        }
        if globs.len() > 64
            || globs
                .iter()
                .any(|s| s.len() > 1024 || glob::Pattern::new(s).is_err())
        {
            return Err(io::Error::other(
                "Invalid or oversized project rule patterns",
            ));
        }
        body = &text[(end + 4).min(text.len())..];
    }
    let body = trim(body, 4000);
    if body.is_empty() {
        return Ok(None);
    }
    Ok(Some(Rule {
        name: Path::new(path)
            .file_stem()
            .and_then(|v| v.to_str())
            .unwrap_or("rule")
            .into(),
        body,
        globs,
        path: path.into(),
    }))
}
impl Brain {
    pub fn load(root: &Path, control: &Control<'_>) -> io::Result<Option<Self>> {
        let root = root.canonicalize()?;
        let reader = ReadWorkspace::new(&root)?;
        let mut brain = Self {
            project_root: root.clone(),
            root_file: String::new(),
            root_text: String::new(),
            rules: vec![],
            command_names: vec![],
        };
        for path in [
            "AGENTS.md",
            "KAGEHA.md",
            "CLAUDE.md",
            ".cursorrules",
            ".cursor/rules.md",
        ] {
            if let Some(text) = read(&reader, path, 12000, control)? {
                brain.root_file = path.into();
                brain.root_text = text;
                break;
            }
        }
        if brain.root_file.is_empty() {
            let mut parts = Vec::new();
            for path in files(&root, ".cursor/rules", true, control)?
                .into_iter()
                .take(12)
            {
                if let Some(text) = read(&reader, &path, 4000, control)? {
                    if !text.is_empty() {
                        parts.push(format!(
                            "### {}\n{text}",
                            Path::new(&path).file_name().unwrap().to_string_lossy()
                        ));
                    }
                }
            }
            if !parts.is_empty() {
                brain.root_file = ".cursor/rules/".into();
                brain.root_text = trim(&parts.join("\n\n"), 12000);
            }
        }
        for path in files(&root, ".kageha/rules", true, control)? {
            if let Some(text) = read(&reader, &path, 8192, control)? {
                if let Some(rule) = rule(&path, &text)? {
                    brain.rules.push(rule);
                }
            }
            if brain.rules.len() >= 24 {
                break;
            }
        }
        for path in files(&root, ".kageha/commands", false, control)? {
            if Path::new(&path).extension().and_then(|v| v.to_str()) != Some("mdc") {
                brain.command_names.push(
                    Path::new(&path)
                        .file_stem()
                        .unwrap()
                        .to_string_lossy()
                        .into(),
                );
            }
        }
        if brain.root_text.is_empty() && brain.rules.is_empty() {
            Ok(None)
        } else {
            Ok(Some(brain))
        }
    }
    pub fn render(&self, touched: &[String]) -> String {
        let mut parts = vec![
            "## Project instructions".into(),
            format!("Project root: `{}`", self.project_root.display()),
        ];
        if !self.root_text.is_empty() {
            parts.push(format!("### From {}\n\n{}", self.root_file, self.root_text));
        }
        for rule in &self.rules {
            if touched.is_empty()
                || rule.globs.is_empty()
                || rule.globs.iter().any(|g| {
                    glob::Pattern::new(g).is_ok_and(|p| {
                        touched.iter().any(|s| {
                            let path = s.replace('\\', "/");
                            p.matches(&path)
                                || Path::new(&path)
                                    .file_name()
                                    .and_then(|v| v.to_str())
                                    .is_some_and(|s| p.matches(s))
                        })
                    })
                })
            {
                parts.push(format!(
                    "#### {}{}\n\n{}",
                    rule.name,
                    if rule.globs.is_empty() {
                        String::new()
                    } else {
                        format!(" (globs: {})", rule.globs.join(", "))
                    },
                    rule.body
                ));
            }
        }
        if !self.command_names.is_empty() {
            parts.push(format!(
                "### Project command recipes\nDefined under `.kageha/commands/`: {}",
                self.command_names
                    .iter()
                    .map(|s| format!("`{s}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        trim(&parts.join("\n\n"), 24000)
    }
    pub fn command(root: &Path, name: &str, control: &Control<'_>) -> io::Result<Option<String>> {
        let name = name.trim().trim_start_matches('/');
        let name = name.strip_prefix("project:").unwrap_or(name);
        if name.is_empty()
            || name.len() > 128
            || name.contains(['/', '\\'])
            || name.contains("..")
            || name.chars().any(char::is_control)
        {
            return Err(io::Error::other("Invalid project recipe name"));
        }
        let reader = ReadWorkspace::new(root)?;
        for ext in ["md", "txt"] {
            if let Some(text) = read(
                &reader,
                &format!(".kageha/commands/{name}.{ext}"),
                12000,
                control,
            )? {
                return Ok(Some(text));
            }
        }
        Ok(None)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    #[test]
    fn precedence_rules_globs_and_recipes_are_bounded_and_read_only() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join(".kageha/rules")).unwrap();
        fs::create_dir_all(root.path().join(".kageha/commands")).unwrap();
        fs::write(root.path().join("AGENTS.md"), "Primary instructions").unwrap();
        fs::write(
            root.path().join("CLAUDE.md"),
            "Secondary must not duplicate",
        )
        .unwrap();
        fs::write(
            root.path().join(".kageha/rules/rust.md"),
            "---\nglobs: ['*.rs']\n---\nRust conventions",
        )
        .unwrap();
        fs::write(
            root.path().join(".kageha/commands/check.md"),
            "Check the supplied evidence",
        )
        .unwrap();
        let control = Control {
            cancelled: &AtomicBool::new(false),
            deadline: None,
        };
        let brain = Brain::load(root.path(), &control).unwrap().unwrap();
        assert_eq!(brain.root_file, "AGENTS.md");
        assert!(brain.render(&[]).contains("Rust conventions"));
        assert!(!brain
            .render(&["file.py".into()])
            .contains("Rust conventions"));
        assert!(brain
            .render(&["src/file.rs".into()])
            .contains("Rust conventions"));
        assert_eq!(
            Brain::command(root.path(), "/project:check", &control)
                .unwrap()
                .unwrap(),
            "Check the supplied evidence"
        );
        assert!(Brain::command(root.path(), "../secret", &control).is_err());
    }
    #[test]
    fn instruction_links_outside_workspace_are_denied() {
        let root = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("/etc/passwd", root.path().join("AGENTS.md")).unwrap();
        let control = Control {
            cancelled: &AtomicBool::new(false),
            deadline: None,
        };
        assert!(Brain::load(root.path(), &control).is_err());
    }
}
