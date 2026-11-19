//! Structural cutover audit. A runnable Rust path is not proof that Python is gone.
use serde_json::{json, Value};
use std::{fs, io, path::Path};

pub fn audit(root: &Path) -> io::Result<Value> {
    fn scan(
        root: &Path,
        dir: &Path,
        python: &mut Vec<String>,
        links: &mut Vec<String>,
    ) -> io::Result<()> {
        if !dir.exists() {
            return Ok(());
        }
        let meta = fs::symlink_metadata(dir)?;
        if meta.file_type().is_symlink() {
            links.push(dir.strip_prefix(root).unwrap().to_string_lossy().into());
            return Ok(());
        }
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name();
            if matches!(
                name.to_str(),
                Some(
                    "node_modules"
                        | "target"
                        | "dist"
                        | "build"
                        | "__pycache__"
                        | ".venv"
                        | ".git"
                        | ".cache"
                        | "research"
                        | "bench"
                        | "results"
                )
            ) {
                continue;
            }
            let path = entry.path();
            let kind = entry.file_type()?;
            let relative = path.strip_prefix(root).unwrap();
            if kind.is_symlink() {
                links.push(relative.to_string_lossy().into());
            } else if kind.is_dir() {
                scan(root, &path, python, links)?;
            } else if path.extension().is_some_and(|e| e == "py")
                && !relative.starts_with("sdk/python")
            {
                python.push(relative.to_string_lossy().into());
            }
        }
        Ok(())
    }
    let mut python = Vec::new();
    let mut links = Vec::new();
    for dir in [
        "src",
        "tests",
        "scripts",
        "resources",
        "frontend",
        "crates",
        "sdk",
        "examples",
        "docker",
        ".github",
        "migration",
        "xtask",
    ] {
        scan(root, &root.join(dir), &mut python, &mut links)?;
    }
    // Include root-level utilities as well as the preserved legacy package tree.
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_file() && entry.path().extension().is_some_and(|e| e == "py") {
            python.push(entry.file_name().to_string_lossy().into());
        }
    }
    python.sort();
    python.dedup();
    links.sort();
    links.dedup();
    let legacy_manifests = [
        "pyproject.toml",
        "setup.py",
        "setup.cfg",
        "requirements.txt",
        "uv.lock",
        "poetry.lock",
    ]
    .into_iter()
    .filter(|p| root.join(p).exists())
    .collect::<Vec<_>>();
    Ok(
        json!({"schema_version":1,"python_free_backend_ready":python.is_empty()&&links.is_empty()&&legacy_manifests.is_empty(),"python_sdk_exception":"sdk/python","python_files_outside_sdk":python,"source_symlinks":links,"legacy_backend_manifests":legacy_manifests,"note":"File-boundary check only. Independent install, command execution and resource containment acceptance remain required."}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn optional_sdk_is_allowed_but_backend_helpers_and_manifests_are_not() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("sdk/python")).unwrap();
        fs::write(root.path().join("sdk/python/client.py"), "").unwrap();
        assert_eq!(
            audit(root.path()).unwrap()["python_free_backend_ready"],
            true
        );
        fs::create_dir(root.path().join("resources")).unwrap();
        fs::write(root.path().join("resources/helper.py"), "").unwrap();
        fs::write(root.path().join("pyproject.toml"), "").unwrap();
        let result = audit(root.path()).unwrap();
        assert_eq!(result["python_free_backend_ready"], false);
        assert_eq!(
            result["python_files_outside_sdk"],
            json!(["resources/helper.py"])
        );
        assert_eq!(
            result["legacy_backend_manifests"],
            json!(["pyproject.toml"])
        );
    }
    #[test]
    #[cfg(unix)]
    fn compatibility_links_cannot_hide_python() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        std::os::unix::fs::symlink("../elsewhere", root.path().join("src/legacy")).unwrap();
        let result = audit(root.path()).unwrap();
        assert_eq!(result["python_free_backend_ready"], false);
        assert_eq!(result["source_symlinks"], json!(["src/legacy"]));
    }
}
