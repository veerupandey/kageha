//! Property test for `TaskWorkspace` path confinement.
use kageha_workspace::task::TaskWorkspace;
use proptest::prelude::*;
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::symlink,
    path::{Component, Path, PathBuf},
};

/// Segments for model-supplied paths. `s1`..`s4` name the symlinks a case may create.
const PATH_SEGMENTS: &[&str] = &[
    "..", "..", ".", "", "a", "b", "s1", "s2", "s3", "s4", "todo.md", "notes", ".kageha", "é",
    "日本語", "🦀.txt", "outside", "workspace-sibling", "secret",
];

/// Segments for relative symlink targets.
const TARGET_SEGMENTS: &[&str] = &[
    "..", "..", ".", "a", "b", "s1", "s2", "notes", "outside", "secret", "workspace-sibling",
    "chat", "workspace",
];

/// Where a case may place a symlink, relative to the root. `a/b` always exists.
const LINK_LOCATIONS: &[&str] = &["s1", "s2", "a/s3", "a/b/s4"];

#[derive(Debug, Clone)]
enum Target {
    /// A relative target built from `TARGET_SEGMENTS`.
    Relative(String),
    /// The canonical root joined with a suffix (inside).
    RootAbsolute(String),
    /// The root reached through the non-canonical temp path (`/var` vs `/private/var` on macOS).
    RootNonCanonical(String),
    /// The `outside` directory next to the chat directory.
    Outside,
    /// The `outside/secret` file.
    OutsideFile,
    /// `chat/workspace-sibling`, whose path shares the root's string prefix.
    Sibling,
    /// The file system root.
    FsRoot,
}

fn joined(segments: &'static [&'static str], max: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(segments), 1..=max).prop_map(|parts| parts.join("/"))
}

fn rel_path() -> impl Strategy<Value = String> {
    prop_oneof![
        8 => (any::<bool>(), joined(PATH_SEGMENTS, 8))
            .prop_map(|(lead, path)| if lead { format!("/{path}") } else { path }),
        1 => "\\PC{0,24}",
    ]
}

fn target() -> impl Strategy<Value = Target> {
    prop_oneof![
        6 => joined(TARGET_SEGMENTS, 5).prop_map(Target::Relative),
        2 => prop::sample::select(&["", "notes", "a/b", "s1", "../outside"][..])
            .prop_map(|s| Target::RootAbsolute(s.into())),
        1 => prop::sample::select(&["", "notes"][..])
            .prop_map(|s| Target::RootNonCanonical(s.into())),
        1 => Just(Target::Outside),
        1 => Just(Target::OutsideFile),
        1 => Just(Target::Sibling),
        1 => Just(Target::FsRoot),
    ]
}

fn links() -> impl Strategy<Value = Vec<(&'static str, Target)>> {
    prop::collection::vec((prop::sample::select(LINK_LOCATIONS), target()), 0..=4)
}

/// Every entry under `base` outside `root`, keyed by path, with its kind and contents.
fn snapshot(base: &Path, root: &Path) -> BTreeMap<PathBuf, String> {
    fn visit(dir: &Path, root: &Path, out: &mut BTreeMap<PathBuf, String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path == root {
                continue;
            }
            let meta = fs::symlink_metadata(&path).unwrap();
            let value = if meta.file_type().is_symlink() {
                format!("link:{}", fs::read_link(&path).unwrap().display())
            } else if meta.is_dir() {
                visit(&path, root, out);
                "dir".into()
            } else {
                format!("file:{:?}", fs::read(&path).unwrap())
            };
            out.insert(path, value);
        }
    }
    let mut out = BTreeMap::new();
    visit(base, root, &mut out);
    out
}

/// The returned path has no `.`/`..` components and its longest existing ancestor
/// canonicalizes to a path inside `root`.
fn assert_inside(path: &Path, root: &Path, rel: &str) -> Result<(), TestCaseError> {
    prop_assert!(path.starts_with(root), "{rel:?} -> {path:?} not under {root:?}");
    prop_assert!(
        path.components()
            .all(|c| !matches!(c, Component::ParentDir | Component::CurDir)),
        "{rel:?} -> {path:?} is not normalized"
    );
    let canonical = path
        .ancestors()
        .find_map(|ancestor| ancestor.canonicalize().ok())
        .unwrap();
    prop_assert!(
        canonical.starts_with(root),
        "{rel:?} -> {path:?} canonicalizes to {canonical:?}, outside {root:?}"
    );
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    // Feature: autonomous-agent-loop, Property 1: Workspace path confinement
    // **Validates: Requirements 3.2, 3.3**
    #[test]
    fn workspace_path_confinement(
        links in links(),
        paths in prop::collection::vec(rel_path(), 1..=4),
        bytes in prop::collection::vec(any::<u8>(), 0..16),
    ) {
        let temp = tempfile::tempdir().unwrap();
        let raw_base = temp.path().to_path_buf();
        let base = raw_base.canonicalize().unwrap();
        fs::create_dir_all(base.join("outside")).unwrap();
        fs::write(base.join("outside/secret"), "secret").unwrap();
        fs::create_dir_all(base.join("chat/workspace-sibling")).unwrap();
        fs::write(base.join("chat/workspace-sibling/secret"), "sibling").unwrap();
        let workspace = TaskWorkspace::open(&base.join("chat")).unwrap();
        let root = workspace.root().to_path_buf();
        fs::create_dir_all(root.join("a/b")).unwrap();

        for (location, target) in &links {
            let link = root.join(location);
            if fs::symlink_metadata(&link).is_ok() {
                continue;
            }
            let target = match target {
                Target::Relative(rel) => PathBuf::from(rel),
                Target::RootAbsolute(suffix) => root.join(suffix),
                Target::RootNonCanonical(suffix) => {
                    raw_base.join("chat/workspace").join(suffix)
                }
                Target::Outside => base.join("outside"),
                Target::OutsideFile => base.join("outside/secret"),
                Target::Sibling => base.join("chat/workspace-sibling"),
                Target::FsRoot => PathBuf::from("/"),
            };
            symlink(&target, &link).unwrap();
        }

        let before = snapshot(&base, &root);
        for rel in &paths {
            match workspace.resolve(rel) {
                Ok(path) => assert_inside(&path, &root, rel)?,
                Err(e) => prop_assert!(
                    matches!(e.code(), "path_outside_workspace" | "invalid_path"),
                    "{rel:?}: unexpected resolve error {e}"
                ),
            }
            if let Ok(change) = workspace.write(rel, &bytes) {
                let written = root.join(&change.path);
                assert_inside(&written, &root, rel)?;
                let meta = fs::symlink_metadata(&written).unwrap();
                prop_assert!(meta.is_file(), "{rel:?} -> {written:?} is not a regular file");
                prop_assert_eq!(fs::read(&written).unwrap(), bytes.clone());
            }
            prop_assert_eq!(&snapshot(&base, &root), &before, "{:?} changed files outside the root", rel);
        }
    }
}
