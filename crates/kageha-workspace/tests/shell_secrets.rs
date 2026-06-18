//! Property test: secrets never reach the shell environment (Property 17, shell part).
//!
//! Value rule. Besides the name filter, no output value may equal the value of a
//! secret-named host variable, with two exceptions that are not leaks:
//!
//! - the output variable is a non-secret host variable copied under its own name (an
//!   operator passthrough such as `TZ`, or the validated host `LANG`) and the host happens
//!   to hold the same string in a secret-named variable too. The value reached the shell
//!   because of the non-secret variable, which the operator chose to forward;
//! - the output value is the fixed allowlist value for that name, the one produced from an
//!   empty host environment (for example `TERM=dumb` when some `X_TOKEN` is also `dumb`).
//!
//! Put differently: every output value is either the host-independent baseline value for
//! its name or the host's value of that same, non-secret name. A secret value can never be
//! copied into a different name.
use kageha_workspace::shell::{
    host_environment, sandbox_available, shell_environment, ShellPaths, ShellSession, SYSTEM_PATH,
};
use proptest::prelude::*;
use std::{collections::BTreeMap, path::Path, time::Duration};

/// Prefixes for generated names (the empty string makes bare suffixes such as `_TOKEN`).
const PREFIXES: &[&str] = &[
    "GITHUB", "OPENAI", "AWS", "api", "Api", "KAGEHA", "KAGEHA_SERVER", "MY", "SLACK", "",
];
/// Suffixes: secret patterns, their lookalikes, and neutral endings.
const SUFFIXES: &[&str] = &[
    "_TOKEN", "_KEY", "_SECRET", "_token", "_Key", "_Secret", "Token", "TOKEN", "KEY", "_API_KEY",
    "_ACCESS_KEY", "_TOKENIZER", "TOKENIZER", "_KEYCHAIN", "KEYCHAIN", "MONKEY", "_MONKEY",
    "SECRETARY", "_SECRETS", "_TOKENS", "_KEYS", "_KEY_ID", "_HOME", "_DIR", "",
];
/// Whole names: the server token, allowlisted names a host could try to override, and
/// ordinary variables.
const WHOLE_NAMES: &[&str] = &[
    "KAGEHA_SERVER_TOKEN", "kageha_server_token", "Kageha_Server_Token", "KAGEHA_API_TOKEN",
    "PATH", "HOME", "LANG", "TERM", "TMPDIR", "KAGEHA_WORKSPACE", "KAGEHA_API_SOCKET",
    "CARGO_HOME", "RUSTUP_HOME", "npm_config_prefix", "UV_CACHE_DIR", "TZ", "SSH_AUTH_SOCK",
    "TOKENIZER", "KEYCHAIN", "MONKEY", "SECRETARY", "EDITOR",
];
/// Shared values, so that secret and non-secret variables often hold the same string and
/// some secret values coincide with fixed allowlist values.
const VALUES: &[&str] = &[
    "sk-live-shared", "ghp_shared", "value", "", "dumb", "en_US.UTF-8", "de_DE.UTF-8", "/w/chat",
    "/tc/cargo", "/usr/bin:/bin",
];

const WORKSPACE: &str = "/w/chat";
const TOOLCHAINS: &str = "/tc";

/// Independent oracle for the secret-name pattern (case-insensitive).
fn secret_like(name: &str) -> bool {
    let n = name.to_ascii_uppercase();
    n == "KAGEHA_SERVER_TOKEN" || n.ends_with("_TOKEN") || n.ends_with("_KEY") || n.ends_with("_SECRET")
}

/// Applies a per-character case mask (bit set: flip ASCII case).
fn mix_case(name: &str, mask: u64) -> String {
    name.chars()
        .enumerate()
        .map(|(i, c)| {
            if mask >> (i % 64) & 1 == 1 {
                if c.is_ascii_uppercase() { c.to_ascii_lowercase() } else { c.to_ascii_uppercase() }
            } else {
                c
            }
        })
        .collect()
}

fn name_strategy() -> impl Strategy<Value = String> {
    let composed = (prop::sample::select(PREFIXES), prop::sample::select(SUFFIXES))
        .prop_map(|(p, s)| format!("{p}{s}"))
        .prop_filter("non-empty name", |n| !n.is_empty());
    let base = prop_oneof![
        3 => composed,
        2 => prop::sample::select(WHOLE_NAMES).prop_map(str::to_string),
        1 => "[A-Za-z][A-Za-z0-9_]{0,10}",
    ];
    // A third of names keep their spelling; the rest get a random case mix.
    (base, prop_oneof![1 => Just(0u64), 2 => any::<u64>()]).prop_map(|(n, m)| mix_case(&n, m))
}

fn value_strategy() -> impl Strategy<Value = String> {
    prop_oneof![
        2 => prop::sample::select(VALUES).prop_map(str::to_string),
        1 => "[A-Za-z0-9_./:@=-]{0,16}",
    ]
}

/// A host environment and a passthrough list drawn partly from the host's own names.
fn case_strategy() -> impl Strategy<Value = (BTreeMap<String, String>, Vec<String>)> {
    prop::collection::btree_map(name_strategy(), value_strategy(), 0..24).prop_flat_map(|host| {
        let keys: Vec<String> = host.keys().cloned().collect();
        let pick = (any::<bool>(), any::<prop::sample::Index>(), name_strategy()).prop_map(
            move |(from_host, index, fresh)| {
                if from_host && !keys.is_empty() { keys[index.index(keys.len())].clone() } else { fresh }
            },
        );
        (Just(host), prop::collection::vec(pick, 0..16))
    })
}

fn paths_with(passthrough: Vec<String>) -> ShellPaths {
    let mut paths = ShellPaths::layout(Path::new(WORKSPACE), Path::new(TOOLCHAINS));
    paths.passthrough = passthrough;
    paths
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1000))]

    // Feature: autonomous-agent-loop, Property 17: Secrets excluded
    /// **Validates: Requirements 19.6, 2.4**
    #[test]
    fn shell_environment_excludes_secrets((host, passthrough) in case_strategy()) {
        let paths = paths_with(passthrough.clone());
        let env = shell_environment(&host, &paths);
        let baseline = shell_environment(&BTreeMap::new(), &paths_with(Vec::new()));

        // No secret-pattern name, whatever the host or the passthrough list holds.
        for name in env.keys() {
            prop_assert!(!secret_like(name), "secret name {name:?} in {env:?}");
        }

        // No secret value is copied into another name (see the module docs for the rule).
        for (name, value) in &env {
            let from_baseline = baseline.get(name) == Some(value);
            let own_host_value = !secret_like(name) && host.get(name) == Some(value);
            prop_assert!(
                from_baseline || own_host_value,
                "{name}={value:?} is neither its fixed value nor its own host value"
            );
            for (secret, secret_value) in host.iter().filter(|(n, _)| secret_like(n)) {
                prop_assert!(
                    value != secret_value || from_baseline || own_host_value,
                    "{name} carries the value of {secret}"
                );
            }
        }

        // The required allowlist is always present with workspace/toolchain-derived values.
        let tc = TOOLCHAINS;
        let required = [
            ("HOME", format!("{WORKSPACE}/.kageha/home")),
            ("TMPDIR", format!("{WORKSPACE}/.kageha/tmp")),
            ("KAGEHA_WORKSPACE", WORKSPACE.to_string()),
            ("KAGEHA_API_SOCKET", format!("{WORKSPACE}/.kageha/api.sock")),
            ("CARGO_HOME", format!("{tc}/cargo")),
            ("RUSTUP_HOME", format!("{tc}/rustup")),
            ("npm_config_prefix", format!("{tc}/npm")),
            ("npm_config_cache", format!("{tc}/npm-cache")),
            ("UV_CACHE_DIR", format!("{tc}/uv-cache")),
            ("PIP_CACHE_DIR", format!("{tc}/pip-cache")),
            ("PYTHONUSERBASE", format!("{tc}/python")),
            ("VIRTUAL_ENV", format!("{tc}/python/venv")),
            ("PIP_DISABLE_PIP_VERSION_CHECK", "1".to_string()),
            ("TERM", "dumb".to_string()),
        ];
        for (name, expected) in &required {
            prop_assert_eq!(env.get(*name), Some(expected), "{}", name);
        }
        prop_assert!(env.contains_key("LANG"));
        prop_assert_eq!(
            &env["PATH"],
            &format!("{tc}/python/venv/bin:{tc}/bin:{tc}/cargo/bin:{tc}/npm/bin:{tc}/python/bin:{SYSTEM_PATH}")
        );

        // Non-vacuity: a non-secret passthrough name the host defines is forwarded unless
        // an allowlisted variable of that name takes precedence.
        for name in passthrough.iter().filter(|n| !secret_like(n) && !baseline.contains_key(*n)) {
            if let Some(value) = host.get(name) {
                prop_assert_eq!(env.get(name), Some(value), "{} not forwarded", name);
            }
        }
    }
}

/// Runs `env` inside a real sandboxed session whose host process holds secrets, including
/// in the passthrough list. Skipped when the OS sandbox is unavailable.
#[test]
fn sandboxed_shell_env_has_no_host_secrets() {
    if !sandbox_available() {
        eprintln!("skipping: OS process sandbox unavailable");
        return;
    }
    let secrets = [
        ("KG_PROBE_TOKEN", "kg-secret-token-5e1c9"),
        ("Kg_Probe_Api_Key", "kg-secret-key-0b7d2"),
        ("kg_probe_secret", "kg-secret-secret-a93f4"),
        ("KAGEHA_SERVER_TOKEN", "kg-secret-server-71c08"),
    ];
    // Only this test touches the process environment; std serializes env access.
    for (name, value) in secrets {
        std::env::set_var(name, value);
    }
    std::env::set_var("KG_PROBE_PLAIN", "kg-plain-value");
    assert_eq!(host_environment()["KG_PROBE_TOKEN"], "kg-secret-token-5e1c9");

    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let mut paths = ShellPaths::prepare(&workspace, &dir.path().join("toolchains")).unwrap();
    paths.passthrough = secrets
        .iter()
        .map(|(n, _)| n.to_string())
        .chain(["KG_PROBE_PLAIN".to_string()])
        .collect();
    let mut shell = ShellSession::open(paths).unwrap();
    let out = shell.run("env", "call", Duration::from_secs(20)).unwrap();
    shell.close();
    assert_eq!(out.exit_code, 0, "{out:?}");

    for line in out.stdout.lines() {
        let name = line.split('=').next().unwrap_or_default();
        assert!(!secret_like(name), "secret name in shell env: {line}");
    }
    for (name, value) in secrets {
        assert!(!out.stdout.contains(value), "value of {name} reached the shell");
    }
    // The passthrough path works, so the absence above is not an artefact of an empty env.
    assert!(out.stdout.lines().any(|l| l == "KG_PROBE_PLAIN=kg-plain-value"), "{}", out.stdout);
    assert!(out.stdout.lines().any(|l| l.starts_with("KAGEHA_API_SOCKET=")));
}
