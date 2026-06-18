//! Saved shell state: a chat's working directory and non-secret exported variables
//! (spec autonomous-agent-loop, design section 13, Requirement 10.2).
//!
//! After each foreground command that finished in the chat's session, [`ShellManager`]
//! runs [`CAPTURE`] in the same session and writes the result to
//! `workspace/.kageha/shell-env` ([`SHELL_ENV`], JSON, mode 0600). When the chat gets a new
//! session (the old one broke, became a background job, or the shell service or the
//! server restarted), the new session changes to the saved directory and exports the saved
//! variables before its first command ([`restore_command`]).
//!
//! What is saved:
//!
//! - `cwd`: the directory from `pwd -P`, relative to the workspace root, only when it is
//!   inside the workspace.
//! - `env`: exported variables whose value differs from the session's base environment
//!   ([`super::shell_environment`]), except those in [`SKIPPED`] (set by the shell itself or
//!   tied to one session) and every secret-named variable ([`super::is_secret_name`]:
//!   `*_TOKEN`, `*_KEY`, `*_SECRET`, `KAGEHA_SERVER_TOKEN`).
//!
//! Secret-named variables never reach the file, and their values never leave the shell:
//! [`CAPTURE`] filters the names with a `case` pattern before it prints any value, and the
//! Host filters again when it parses the output and when it restores a file. The file is
//! inside the session's write roots, so a command could edit it; restoring only runs
//! `cd` and `export` with validated names and single-quoted values in the same sandboxed
//! session, so an edited file can't do more than the session itself could.
//!
//! [`ShellManager`]: super::ShellManager
use super::{is_secret_name, single_quoted};
use crate::task::{TaskWorkspace, SHELL_ENV};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io,
    path::{Component, Path, PathBuf},
};

/// Version of the saved file format.
pub const VERSION: u32 = 1;
/// Largest saved file accepted, and the bound on the captured output.
pub const MAX_STATE_BYTES: usize = 256 * 1024;
/// Most variables saved.
pub const MAX_VARIABLES: usize = 256;
/// The `call_id` of the capture command (it names spill files, which a capture over the
/// spill threshold would create; such a capture is not saved and its spill is removed).
pub const CAPTURE_CALL_ID: &str = "kg-shell-state";

/// Exported variables that are never saved: set by the shell on its own or derived from the
/// session's paths, so a new session gets its own.
pub const SKIPPED: &[&str] = &[
    "PWD",
    "OLDPWD",
    "SHLVL",
    "_",
    "LOGNAME",
    "HOME",
    "TMPDIR",
    "TMPPREFIX",
    "CFFIXED_USER_HOME",
    "CLANG_MODULE_CACHE_PATH",
    "KAGEHA_WORKSPACE",
    "KAGEHA_API_SOCKET",
];

/// Prints `pwd -P`, a newline, then `NAME=value\0` for every exported variable whose name
/// is not secret-named. Works in `zsh -f` and `bash --norc`. Names come from `env` and are
/// limited to `[A-Za-z_][A-Za-z0-9_]*` by `sed`, so `eval` only ever expands a variable.
/// Variables whose name the shell has unset (a continuation line of a multi-line value that
/// looks like `NAME=`) are skipped by the `${NAME+x}` check.
pub const CAPTURE: &str = concat!(
    "pwd -P; ",
    "for __kg_n in $(env | sed -n 's/^\\([A-Za-z_][A-Za-z0-9_]*\\)=.*/\\1/p'); do ",
    "case \"$__kg_n\" in ",
    "*_[Tt][Oo][Kk][Ee][Nn]|*_[Kk][Ee][Yy]|*_[Ss][Ee][Cc][Rr][Ee][Tt]|__kg_*) ;; ",
    "*) eval \"__kg_s=\\${$__kg_n+x}; __kg_v=\\${$__kg_n-}\"; ",
    "[ -n \"$__kg_s\" ] && printf '%s=%s\\0' \"$__kg_n\" \"$__kg_v\";; ",
    "esac; done; unset __kg_n __kg_s __kg_v"
);

/// The saved state of one chat's shell session.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedShell {
    pub version: u32,
    /// Working directory relative to the workspace root; empty for the root.
    pub cwd: String,
    /// Non-secret exported variables that differ from the base environment.
    pub env: BTreeMap<String, String>,
}

/// A variable name the Host saves and restores: a shell identifier that is not
/// secret-named and not in [`SKIPPED`].
pub fn savable_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    let identifier = bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_');
    identifier
        && name.len() <= 128
        && !name.starts_with("__kg_")
        && !name.starts_with("__CF")
        && !is_secret_name(name)
        && !SKIPPED.contains(&name)
}

/// The workspace-relative form of `dir`, when it is inside `workspace` (both canonical).
fn relative_dir(workspace: &Path, dir: &Path) -> Option<String> {
    let rel = dir.strip_prefix(workspace).ok()?;
    let parts: Option<Vec<&str>> = rel
        .components()
        .map(|part| match part {
            Component::Normal(name) => name.to_str(),
            _ => None,
        })
        .collect();
    Some(parts?.join("/"))
}

/// Parses the output of [`CAPTURE`]. `base` is the session's base environment: a variable
/// with its base value is not saved. `None` when the output is not a capture.
pub fn parse_capture(
    stdout: &str,
    workspace: &Path,
    base: &BTreeMap<String, String>,
) -> Option<SavedShell> {
    let (dir, rest) = stdout.split_once('\n')?;
    let cwd = relative_dir(workspace, Path::new(dir)).unwrap_or_default();
    let mut env = BTreeMap::new();
    for entry in rest.split('\0') {
        let Some((name, value)) = entry.split_once('=') else {
            continue;
        };
        if !savable_name(name) || base.get(name).is_some_and(|base| base == value) {
            continue;
        }
        if env.len() == MAX_VARIABLES {
            break;
        }
        env.insert(name.to_owned(), value.to_owned());
    }
    Some(SavedShell { version: VERSION, cwd, env })
}

/// The saved directory as an absolute path, when it is a plain relative path that still
/// resolves inside `workspace`.
pub fn saved_dir(saved: &SavedShell, workspace: &Path) -> Option<PathBuf> {
    if saved.cwd.is_empty() {
        return None;
    }
    let plain = saved
        .cwd
        .split('/')
        .all(|part| !part.is_empty() && part != "." && part != "..");
    if !plain {
        return None;
    }
    let dir = workspace.join(&saved.cwd).canonicalize().ok()?;
    (dir.starts_with(workspace) && dir.is_dir()).then_some(dir)
}

/// The command that brings a new session to `saved`: `cd` to the saved directory (when it
/// still exists inside the workspace) and `export` the savable variables. `None` when there
/// is nothing to restore.
pub fn restore_command(saved: &SavedShell, workspace: &Path) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(dir) = saved_dir(saved, workspace) {
        parts.push(format!("cd {}", single_quoted(&dir.to_string_lossy())));
    }
    let exports: Vec<String> = saved
        .env
        .iter()
        .filter(|(name, value)| savable_name(name) && !value.contains('\0'))
        .take(MAX_VARIABLES)
        .map(|(name, value)| format!("{name}={}", single_quoted(value)))
        .collect();
    if !exports.is_empty() {
        parts.push(format!("export {}", exports.join(" ")));
    }
    (!parts.is_empty()).then(|| parts.join("; "))
}

/// Writes `saved` to [`SHELL_ENV`] (mode 0600). Secret-named and skipped variables are
/// dropped again first.
pub fn save(task: &TaskWorkspace, saved: &SavedShell) -> io::Result<()> {
    let mut clean = saved.clone();
    clean.env.retain(|name, _| savable_name(name));
    let bytes = serde_json::to_vec(&clean).map_err(io::Error::other)?;
    if bytes.len() > MAX_STATE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "The shell state exceeds 256 KiB and was not saved",
        ));
    }
    task.write_host_file(SHELL_ENV, &bytes)
        .map(|_| ())
        .map_err(|e| io::Error::other(e.to_string()))
}

/// Reads [`SHELL_ENV`]. `None` when there is none or it can't be used (wrong version,
/// invalid JSON, too large).
pub fn load(task: &TaskWorkspace) -> Option<SavedShell> {
    let bytes = task.read_host_file(SHELL_ENV, MAX_STATE_BYTES).ok()??;
    let mut saved: SavedShell = serde_json::from_slice(&bytes).ok()?;
    if saved.version != VERSION {
        return None;
    }
    saved.env.retain(|name, _| savable_name(name));
    Some(saved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn capture_output_is_parsed_relative_to_the_workspace_without_secrets_or_base_values() {
        let workspace = Path::new("/w/chat/workspace");
        let base = BTreeMap::from([("PATH".to_owned(), "/base".to_owned()), ("LANG".to_owned(), "C".to_owned())]);
        let out = "/w/chat/workspace/sub/dir\nPATH=/venv/bin:/base\0LANG=C\0FOO=a b\nc\0GITHUB_TOKEN=t\0my_key=k\0PWD=/x\0__CF_USER_TEXT_ENCODING=0\0EMPTY=\0";
        let saved = parse_capture(out, workspace, &base).unwrap();
        assert_eq!(saved.cwd, "sub/dir");
        assert_eq!(
            saved.env,
            BTreeMap::from([
                ("EMPTY".to_owned(), String::new()),
                ("FOO".to_owned(), "a b\nc".to_owned()),
                ("PATH".to_owned(), "/venv/bin:/base".to_owned()),
            ])
        );
        // A directory outside the workspace is not saved.
        assert_eq!(parse_capture("/tmp\n", workspace, &base).unwrap().cwd, "");
        assert_eq!(parse_capture("no newline", workspace, &base), None);
    }

    #[test]
    fn restore_command_quotes_values_and_skips_bad_names_and_paths() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(workspace.join("sub")).unwrap();
        let saved = SavedShell {
            version: VERSION,
            cwd: "sub".into(),
            env: BTreeMap::from([
                ("FOO".to_owned(), "it's $(x)".to_owned()),
                ("bad name".to_owned(), "x".to_owned()),
                ("API_KEY".to_owned(), "k".to_owned()),
            ]),
        };
        let command = restore_command(&saved, &workspace).unwrap();
        assert_eq!(
            command,
            format!("cd '{}/sub'; export FOO='it'\\''s $(x)'", workspace.display())
        );
        for cwd in ["../x", "/etc", "missing", "a/./b"] {
            let moved = SavedShell { cwd: cwd.into(), env: BTreeMap::new(), version: VERSION };
            assert_eq!(restore_command(&moved, &workspace), None, "{cwd}");
        }
    }

    proptest! {
        /// Property 17 (Secrets excluded), shell-env part: whatever the shell prints, no
        /// secret-named variable is parsed, saved or restored.
        /// **Validates: Requirements 19.6, 2.4, 10.2**
        #[test]
        fn secret_named_variables_are_never_saved(
            names in proptest::collection::vec("[A-Za-z_][A-Za-z0-9_]{0,10}(_TOKEN|_key|_Secret|_Token|)", 1..12),
            value in "[ -<>-~]{0,16}",
        ) {
            let workspace = Path::new("/w");
            let out = format!("/w\n{}", names.iter().map(|n| format!("{n}={value}\0")).collect::<String>());
            let saved = parse_capture(&out, workspace, &BTreeMap::new()).unwrap();
            for name in saved.env.keys() {
                prop_assert!(!is_secret_name(name), "{name}");
            }
            let restored = restore_command(&saved, workspace).unwrap_or_default();
            for name in names.iter().filter(|n| is_secret_name(n)) {
                prop_assert!(!restored.contains(&format!("{name}=")), "{name} in {restored}");
            }
        }
    }
}
