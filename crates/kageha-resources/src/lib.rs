//! Explicit resource roots shared by embedded and installed hosts.
//! Never searches a compile-time checkout or reads application credentials.
use std::{
    io,
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct Resources {
    root: PathBuf,
}
impl Resources {
    pub fn open(root: &Path) -> io::Result<Self> {
        let root = root.canonicalize()?;
        if !root.is_dir() {
            return Err(io::Error::other("Resource root must be a directory"));
        }
        Ok(Self { root })
    }
    /// Explicit configuration wins. Installed layout is bin/../share/kageha.
    /// Absence is an error; never substitute files from another checkout.
    pub fn locate(explicit: Option<&Path>, executable: &Path) -> io::Result<Self> {
        let root = match explicit {
            Some(root) => root.to_owned(),
            None => executable
                .parent()
                .and_then(Path::parent)
                .ok_or_else(|| io::Error::other("Cannot locate installation root"))?
                .join("share/kageha"),
        };
        Self::open(&root)
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn resolve(&self, relative: &Path) -> io::Result<PathBuf> {
        if relative.as_os_str().is_empty()
            || !relative
                .components()
                .all(|p| matches!(p, Component::Normal(_)))
        {
            return Err(io::Error::other("Expected a contained resource path"));
        }
        let path = self.root.join(relative).canonicalize()?;
        if !path.starts_with(&self.root) {
            return Err(io::Error::other("Resource escapes installed root"));
        }
        Ok(path)
    }
    pub fn file(&self, relative: &Path) -> io::Result<PathBuf> {
        let path = self.resolve(relative)?;
        if !path.is_file() {
            return Err(io::Error::other("Expected a resource file"));
        }
        Ok(path)
    }
    pub fn directory(&self, relative: &Path) -> io::Result<PathBuf> {
        let path = self.resolve(relative)?;
        if !path.is_dir() {
            return Err(io::Error::other("Expected a resource directory"));
        }
        Ok(path)
    }
    pub fn webui(&self) -> io::Result<PathBuf> {
        self.file(Path::new("webui/index.html"))?;
        self.directory(Path::new("webui"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn installed_and_explicit_resources_do_not_need_the_build_checkout() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("share/kageha");
        std::fs::create_dir_all(root.join("webui")).unwrap();
        std::fs::write(root.join("webui/index.html"), "fixture").unwrap();
        let executable = temp.path().join("bin/kageha");
        let resources = Resources::locate(None, &executable).unwrap();
        assert_eq!(
            resources.webui().unwrap(),
            root.join("webui").canonicalize().unwrap()
        );
        assert!(Resources::locate(Some(&temp.path().join("missing")), &executable).is_err());
        assert!(resources.file(Path::new("webui")).is_err());
        assert!(resources.resolve(Path::new("../outside")).is_err());
        assert!(resources.resolve(Path::new("/absolute")).is_err());
    }
    #[test]
    #[cfg(unix)]
    fn resource_symlinks_cannot_escape_the_installation() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("resources");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(temp.path().join("private"), "not a resource").unwrap();
        std::os::unix::fs::symlink(temp.path().join("private"), root.join("escape")).unwrap();
        assert!(Resources::open(&root)
            .unwrap()
            .file(Path::new("escape"))
            .is_err());
    }
}
