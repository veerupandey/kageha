//! Sandbox write denials in a failed command's stderr (Requirement 2.6).
//!
//! What the denial looks like depends on the sandbox:
//!
//! - macOS `sandbox-exec` fails a denied write with `EPERM`. Programs print it as
//!   `Operation not permitted`: `touch: /p: Operation not permitted`, zsh
//!   `zsh:1: operation not permitted: /p`, Python `PermissionError: [Errno 1] Operation not
//!   permitted: '/p'`, Node `Error: EPERM: operation not permitted, open '/p'`.
//! - Linux `bwrap` fails a write to a read-only bind with `EROFS`
//!   (`touch: cannot touch '/p': Read-only file system`). A path that is not mounted in the
//!   sandbox at all fails with `ENOENT` (`No such file or directory`). `ENOENT` is also an
//!   ordinary missing file, so it only counts under a caller-named *hidden root*: a host
//!   directory the sandbox does not mount, such as the user's home directory on Linux.
//!
//! A line counts only when it names an absolute path outside every write root. Errors on
//! paths inside the roots, such as `chmod` on a file the user doesn't own, are not sandbox
//! denials. Paths are compared after resolving symlinks of their longest existing prefix,
//! so `/tmp/x` and `/private/tmp/x` compare equal on macOS.
use std::path::{Component, Path, PathBuf};

/// Case-insensitive texts of `EPERM` and `EROFS`.
const DENIAL_TEXTS: &[&str] = &["operation not permitted", "read-only file system"];
/// Error names, matched as whole words.
const DENIAL_CODES: &[&str] = &["EPERM", "EROFS"];
const MISSING_TEXT: &str = "no such file or directory";
const MISSING_CODE: &str = "ENOENT";
/// Most paths reported for one command.
pub const MAX_DENIED_PATHS: usize = 16;
/// Lines longer than this are not parsed (they are command output, not error messages).
const MAX_LINE: usize = 8 * 1024;

/// The absolute paths outside `write_roots` that `stderr` says the sandbox refused, in
/// first-seen order, at most [`MAX_DENIED_PATHS`]. Each path is reported as printed.
/// `hidden_roots` are host directories the sandbox doesn't mount; `No such file or
/// directory` on a path under one of them also counts as a denial. Pure apart from
/// resolving symlinks.
pub fn sandbox_denials(stderr: &str, write_roots: &[PathBuf], hidden_roots: &[PathBuf]) -> Vec<PathBuf> {
    let roots: Vec<PathBuf> = write_roots.iter().map(|root| resolve(root)).collect();
    let hidden: Vec<PathBuf> = hidden_roots.iter().map(|root| resolve(root)).collect();
    let mut found: Vec<(PathBuf, PathBuf)> = Vec::new();
    for line in stderr.lines().filter(|line| line.len() <= MAX_LINE) {
        let lower = line.to_ascii_lowercase();
        let denied = marker(line, &lower, DENIAL_TEXTS, DENIAL_CODES);
        let missing = match (denied, hidden.is_empty()) {
            (None, false) => marker(line, &lower, &[MISSING_TEXT], &[MISSING_CODE]),
            _ => None,
        };
        let Some(at) = denied.or(missing) else { continue };
        for path in line_paths(line, at) {
            let resolved = resolve(&path);
            if roots.iter().any(|root| resolved.starts_with(root))
                || (missing.is_some() && !hidden.iter().any(|root| resolved.starts_with(root)))
                || found.iter().any(|(_, seen)| *seen == resolved)
            {
                continue;
            }
            found.push((path, resolved));
            if found.len() == MAX_DENIED_PATHS {
                return found.into_iter().map(|(path, _)| path).collect();
            }
        }
    }
    found.into_iter().map(|(path, _)| path).collect()
}

/// Where the first denial marker starts and ends in `line`, if it has one.
fn marker(line: &str, lower: &str, texts: &[&str], codes: &[&str]) -> Option<(usize, usize)> {
    texts
        .iter()
        .find_map(|text| lower.find(text).map(|at| (at, at + text.len())))
        .or_else(|| codes.iter().find_map(|code| word(line, code).map(|at| (at, at + code.len()))))
}

/// The byte offset of `code` in `line` as a whole word.
fn word(line: &str, code: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    line.match_indices(code).map(|(at, _)| at).find(|&at| {
        let before = at.checked_sub(1).map(|i| bytes[i]);
        let after = bytes.get(at + code.len()).copied();
        !before.is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_')
            && !after.is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_')
    })
}

/// Absolute paths named on one error line whose marker spans `at`.
///
/// Quoted paths win (`'/p'`, `"/p"`, `‘/p’`). Otherwise the path is the text between the
/// last `": /"` and `": <marker>"` (`touch: /a b: Operation not permitted`), or the rest of
/// the line after `<marker>: ` (zsh). As a last resort every word starting with `/` counts.
fn line_paths(line: &str, (start, end): (usize, usize)) -> Vec<PathBuf> {
    let absolute = |text: &str| {
        let text = text.trim();
        (text.starts_with('/') && text.len() > 1).then(|| PathBuf::from(text))
    };
    let quoted: Vec<PathBuf> = line
        .split(['\'', '"', '`', '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}'])
        .skip(1)
        .step_by(2)
        .filter_map(absolute)
        .collect();
    if !quoted.is_empty() {
        return quoted;
    }
    let before = line[..start].trim_end().trim_end_matches(':');
    let leading = before
        .rfind(": /")
        .map(|at| &before[at + 2..])
        .or_else(|| before.starts_with('/').then_some(before))
        .and_then(absolute);
    if let Some(path) = leading {
        return vec![path];
    }
    if let Some(path) = line[end..].strip_prefix(": ").and_then(absolute) {
        return vec![path];
    }
    line.split_whitespace()
        .map(|word| word.trim_end_matches([':', ',', ';', ')', ']', '.']))
        .filter_map(absolute)
        .collect()
}

/// `path` with its longest existing prefix canonicalized, so symlinked roots compare
/// equal. A path with `..` after a missing component is left as given.
pub fn resolve(path: &Path) -> PathBuf {
    let mut existing = path;
    let mut rest = Vec::new();
    loop {
        if let Ok(canonical) = existing.canonicalize() {
            return rest.into_iter().rev().fold(canonical, |mut path, part| {
                path.push(part);
                path
            });
        }
        match (existing.parent(), existing.components().next_back()) {
            (Some(parent), Some(Component::Normal(name))) => {
                rest.push(name.to_owned());
                existing = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write roots in a temp directory, and an outside directory that exists.
    struct Roots {
        _dir: tempfile::TempDir,
        roots: Vec<PathBuf>,
        workspace: PathBuf,
        outside: PathBuf,
    }

    fn roots() -> Roots {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().join("workspace");
        let cache = dir.path().join("toolchains");
        let outside = dir.path().join("home/Desktop");
        for path in [&workspace, &cache, &outside] {
            std::fs::create_dir_all(path).unwrap();
        }
        Roots { roots: vec![workspace.clone(), cache], workspace, outside, _dir: dir }
    }

    fn denials(stderr: &str, roots: &Roots) -> Vec<String> {
        sandbox_denials(stderr, &roots.roots, &[])
            .into_iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn macos_sandbox_exec_messages_name_the_denied_path() {
        let r = roots();
        let out = r.outside.display();
        let stderr = format!(
            "zsh:1: operation not permitted: {out}/kg a\n\
             touch: {out}/kg_b: Operation not permitted\n\
             PermissionError: [Errno 1] Operation not permitted: '{out}/kg_c'\n\
             Error: EPERM: operation not permitted, open '{out}/kg_d'\n\
             mkdir: {out}/new dir: Operation not permitted\n\
             touch: {out}/kg_b: Operation not permitted\n"
        );
        assert_eq!(denials(&stderr, &r), [
            format!("{out}/kg a"), format!("{out}/kg_b"), format!("{out}/kg_c"),
            format!("{out}/kg_d"), format!("{out}/new dir"),
        ]);
    }

    #[test]
    fn bwrap_read_only_messages_name_the_denied_path() {
        let r = roots();
        let stderr = "touch: cannot touch '/etc/kg.conf': Read-only file system\n\
                      mkdir: cannot create directory \u{2018}/usr/local/kg\u{2019}: Read-only file system\n\
                      bash: line 1: /opt/kg/out.txt: Read-only file system\n\
                      OSError: [Errno 30] Read-only file system: '/srv/kg'\n\
                      cp: cannot create regular file '/var/kg': EROFS\n";
        assert_eq!(denials(stderr, &r), ["/etc/kg.conf", "/usr/local/kg", "/opt/kg/out.txt", "/srv/kg", "/var/kg"]);
    }

    #[test]
    fn paths_inside_write_roots_and_unrelated_errors_are_not_denials() {
        let r = roots();
        let ws = r.workspace.display();
        let stderr = format!(
            "chmod: {ws}/owned-by-root: Operation not permitted\n\
             touch: relative/path: Operation not permitted\n\
             cat: /nonexistent-kg/x: No such file or directory\n\
             grep: /etc/shadow: Permission denied\n\
             note: EPERMANENT is not a code\n"
        );
        assert!(denials(&stderr, &r).is_empty());
        // Symlinked spellings of a root (macOS /tmp -> /private/tmp) are inside it too.
        let link = r._dir.path().join("link");
        std::os::unix::fs::symlink(&r.workspace, &link).unwrap();
        assert!(denials(&format!("touch: {}/x: Operation not permitted", link.display()), &r).is_empty());
    }

    #[test]
    fn missing_paths_count_only_under_hidden_roots() {
        let r = roots();
        let hidden = r.outside.parent().unwrap().to_path_buf();
        let under = format!("{}/notes/todo.txt", r.outside.display());
        let stderr = format!("bash: {under}: No such file or directory\ncat: /nonexistent-kg/x: No such file or directory\n");
        assert_eq!(sandbox_denials(&stderr, &r.roots, &[hidden]), [PathBuf::from(&under)]);
        assert!(sandbox_denials(&stderr, &r.roots, &[]).is_empty());
    }
}
