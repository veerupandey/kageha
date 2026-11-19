//! The learning store cannot reach the approval policy or tool permissions (spec
//! autonomous-agent-loop, task 23.2; design section 15 "Safety"; Requirement 15.6).
//!
//! Lessons and skills reach the model only as data in the learned-context block. These
//! checks keep it that way: `kageha-policy` and its path dependencies do not depend on
//! `kageha-learning`, and the approval code (`ui_approvals.rs`, the `kageha-policy`
//! sources: approval classes, the approval journal and the specialist tool filter) and
//! the Host's tool permission filter (`HostedTools::allowed` in `ui_tools.rs`) name
//! neither the learning crate, the Host's learning modules nor the `learn_*` tables.
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

const FORBIDDEN: [&str; 4] = ["kageha_learning", "ui_learning", "ui_reflection", "learn_"];

fn crates() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// The crate directories `manifest` names as path dependencies.
fn path_dependencies(directory: &Path) -> Vec<PathBuf> {
    read(&directory.join("Cargo.toml"))
        .lines()
        .filter_map(|line| line.split("path = \"").nth(1)?.split('"').next())
        .map(|path| directory.join(path))
        .collect()
}

fn assert_isolated(name: &str, text: &str) {
    for word in FORBIDDEN {
        assert!(!text.contains(word), "{name} mentions {word}");
    }
}

#[test]
fn the_policy_crate_does_not_depend_on_the_learning_crate() {
    let mut pending = vec![crates().join("kageha-policy")];
    let mut seen = BTreeSet::new();
    while let Some(directory) = pending.pop() {
        let directory = directory.canonicalize().unwrap();
        if !seen.insert(directory.clone()) {
            continue;
        }
        let manifest = read(&directory.join("Cargo.toml"));
        assert!(!manifest.contains("kageha-learning"), "{} depends on kageha-learning", directory.display());
        pending.extend(path_dependencies(&directory));
    }
    // kageha-policy itself and kageha-contracts at least.
    assert!(seen.len() >= 2, "{seen:?}");
}

#[test]
fn approvals_and_the_tool_permission_filter_never_read_the_learning_store() {
    let source = crates().join("kageha-loop/src");
    assert_isolated("ui_approvals.rs", &read(&source.join("ui_approvals.rs")));
    let policy = crates().join("kageha-policy/src");
    let mut checked = 0;
    for entry in std::fs::read_dir(&policy).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|extension| extension == "rs") {
            assert_isolated(&path.display().to_string(), &read(&path));
            checked += 1;
        }
    }
    assert!(checked >= 2, "kageha-policy has no sources at {}", policy.display());

    // The Host's tool permission filter: every tool list and dispatch asks it.
    let tools = read(&source.join("ui_tools.rs"));
    let start = tools.find("    fn allowed(&self, name: &str) -> bool {").expect("HostedTools::allowed");
    let body = &tools[start..start + tools[start..].find("\n    }\n").expect("end of HostedTools::allowed")];
    assert!(body.contains("kageha_policy::specialist::evaluate"), "{body}");
    assert_isolated("HostedTools::allowed", body);
}
