use kageha_resources::Resources;
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

const HELP: &str = "kageha up --workspace DIR --state-dir DIR [--resources DIR] [--bin-dir DIR] [--bind IP:PORT] [--token-file FILE]\nkageha doctor [--resources DIR] [--bin-dir DIR]\nkageha migrate (--legacy-sessions DIR | --legacy-agents DIR | --legacy-task-plans DIR | --legacy-agent-knowledge DIR | --legacy-workspace-knowledge DIR | --legacy-projects DIR) [--workspace DIR] --state-dir DIR [--bin-dir DIR]\nInstalled resources: bin/../share/kageha. No Python or Node is launched.\nMigration covers conversation history, agent definitions/revisions, private/shared knowledge, projects/chat tags and review-only task plans; other stores remain.";
fn invalid(message: &str) -> io::Error {
    io::Error::other(message)
}
fn options(args: impl Iterator<Item = String>) -> io::Result<BTreeMap<String, String>> {
    let mut args = args;
    let mut values = BTreeMap::new();
    while let Some(key) = args.next() {
        if !matches!(
            key.as_str(),
            "--workspace"
                | "--state-dir"
                | "--resources"
                | "--bin-dir"
                | "--bind"
                | "--token-file"
                | "--legacy-sessions"
                | "--legacy-agents"
                | "--legacy-task-plans"
                | "--legacy-agent-knowledge"
                | "--legacy-projects"
                | "--legacy-workspace-knowledge"
        ) || values.contains_key(&key)
        {
            return Err(invalid("Unknown or duplicate option; use --help"));
        }
        let value = args
            .next()
            .filter(|v| !v.is_empty() && !v.starts_with("--"))
            .ok_or_else(|| invalid("Missing option value"))?;
        values.insert(key, value);
    }
    Ok(values)
}
fn private_directory(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(invalid("State directory must be a real directory"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(invalid("State directory must be private (0700)"));
        }
    }
    Ok(())
}
fn read_token(path: &Path) -> io::Result<String> {
    use std::io::Read;
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 258 {
        return Err(invalid("Invalid server token file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(invalid("Server token file must be private (0600)"));
        }
    }
    let mut value = String::new();
    fs::File::open(path)?.take(259).read_to_string(&mut value)?;
    let value = value.trim_end_matches(['\r', '\n']);
    if !(32..=256).contains(&value.len())
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(invalid("Invalid server token"));
    }
    Ok(value.into())
}
fn server_token(state: &Path, explicit: Option<&str>) -> io::Result<String> {
    if let Some(path) = explicit {
        return read_token(Path::new(path));
    }
    if let Ok(value) = std::env::var("KAGEHA_SERVER_TOKEN") {
        if !(32..=256).contains(&value.len())
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err(invalid("Invalid configured server token"));
        }
        return Ok(value);
    }
    let path = state.join("server.token");
    use std::io::Write;
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    match opts.open(&path) {
        Ok(mut file) => {
            let value = format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            );
            file.write_all(value.as_bytes())?;
            file.sync_all()?;
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    read_token(&path)
}
fn executable(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|m| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            m.is_file() && m.permissions().mode() & 0o111 != 0
        }
        #[cfg(not(unix))]
        {
            m.is_file()
        }
    })
}
/// Execute the standalone launcher CLI. `up` replaces the current process on Unix;
/// embedded applications should use the component libraries directly.
pub fn run(mut args: impl Iterator<Item = String>) -> Result<(), Box<dyn std::error::Error>> {
    let command = args.next().unwrap_or_else(|| "--help".into());
    if matches!(command.as_str(), "--help" | "-h") {
        println!("{HELP}");
        return Ok(());
    }
    if !matches!(command.as_str(), "up" | "doctor" | "migrate") {
        return Err(invalid("Unknown command; use --help").into());
    }
    let values = options(args)?;
    let own = std::env::current_exe()?;
    let bin = values
        .get("--bin-dir")
        .map(PathBuf::from)
        .unwrap_or_else(|| own.parent().unwrap().into());
    let host = bin.join("kageha-loop");
    if command == "migrate" {
        if values.keys().any(|key| {
            !matches!(
                key.as_str(),
                "--legacy-sessions"
                    | "--legacy-agents"
                    | "--legacy-task-plans"
                | "--legacy-agent-knowledge"
                | "--legacy-projects"
                | "--legacy-workspace-knowledge"
                    | "--workspace"
                    | "--state-dir"
                    | "--bin-dir"
            )
        }) {
            return Err(
                invalid("migrate accepts one legacy domain, --state-dir and --bin-dir").into(),
            );
        }
        let domains = [
            "--legacy-sessions",
            "--legacy-agents",
            "--legacy-task-plans",
            "--legacy-agent-knowledge",
            "--legacy-workspace-knowledge",
            "--legacy-projects",
        ]
        .into_iter()
        .filter(|key| values.contains_key(*key))
        .collect::<Vec<_>>();
        if domains.len() != 1 {
            return Err(invalid("migrate requires exactly one of --legacy-sessions, --legacy-agents, --legacy-task-plans or --legacy-agent-knowledge/--legacy-workspace-knowledge/--legacy-projects").into());
        }
        let source = &values[domains[0]];
        let state = values
            .get("--state-dir")
            .ok_or_else(|| invalid("migrate requires --state-dir"))?;
        if !executable(&host) {
            return Err(invalid("kageha-loop executable is unavailable").into());
        }
        if values.contains_key("--workspace") && domains[0]!="--legacy-workspace-knowledge" {return Err(invalid("--workspace is used only with workspace knowledge migration").into());}
        let mut child = Command::new(host);
        match domains[0] {
            "--legacy-projects" => {child.arg("migrate-projects");}
            "--legacy-workspace-knowledge" => {child.arg("migrate-workspace-knowledge");}
            "--legacy-sessions" => {
                child.arg("migrate-history");
            }
            flag => {
                child
                    .arg("migrate-documents")
                    .arg(if flag == "--legacy-agents" {
                        "agents"
                    } else if flag == "--legacy-agent-knowledge" {
                        "agent-knowledge"
                    } else {
                        "task-plans"
                    });
            }
        }
        child.arg(source).arg(state);
        if domains[0]=="--legacy-workspace-knowledge" {child.arg(values.get("--workspace").ok_or_else(||invalid("Workspace knowledge migration requires --workspace"))?);}
        return replace_host(child);
    }
    if command == "up" && values.keys().any(|key| key.starts_with("--legacy-")) {
        return Err(invalid("Use the explicit migrate command for legacy data").into());
    }
    let explicit = values
        .get("--resources")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("KAGEHA_RESOURCE_DIR").map(PathBuf::from));
    let resources = Resources::locate(explicit.as_deref(), &own);
    if command == "doctor" {
        if values
            .keys()
            .any(|k| !matches!(k.as_str(), "--resources" | "--bin-dir"))
        {
            return Err(invalid("doctor accepts only --resources and --bin-dir").into());
        }
        let webui = resources.as_ref().ok().and_then(|r| r.webui().ok());
        let models = resources
            .as_ref()
            .ok()
            .and_then(|r| r.file(Path::new("models.yaml")).ok());
        let ready = executable(&host) && webui.is_some() && models.is_some();
        println!(
            "{}",
            json!({"ready":ready,"backend":"rust","python_required":false,"node_required_at_runtime":false,"checks":{"host_executable":executable(&host),"webui":webui.is_some(),"model_catalog":models.is_some()},"optional_components":{"browser":executable(&bin.join("kageha-browser")),"computer":executable(&bin.join("kageha-computer"))},"migration_complete":false})
        );
        if !ready {
            return Err(invalid(
                "Composition is missing required installed resources or host binary",
            )
            .into());
        }
        return Ok(());
    }
    let workspace = Path::new(
        values
            .get("--workspace")
            .ok_or_else(|| invalid("up requires --workspace"))?,
    )
    .canonicalize()?;
    if !workspace.is_dir() {
        return Err(invalid("Workspace must be a directory").into());
    }
    let state = PathBuf::from(
        values
            .get("--state-dir")
            .ok_or_else(|| invalid("up requires --state-dir"))?,
    );
    let resources = resources?;
    let webui = resources.webui()?;
    let catalog = resources.file(Path::new("models.yaml"))?;
    if !executable(&host) {
        return Err(invalid("kageha-loop executable is unavailable").into());
    }
    let bind = values
        .get("--bind")
        .map(String::as_str)
        .unwrap_or("127.0.0.1:8788");
    bind.parse::<std::net::SocketAddr>()
        .map_err(|_| invalid("Bind must be an IP:port"))?;
    private_directory(&state)?;
    let token = server_token(&state, values.get("--token-file").map(String::as_str))?;
    let mut child = Command::new(host);
    child
        .args(["serve"])
        .arg(workspace)
        .arg(&state)
        .arg(bind)
        .env("KAGEHA_SERVER_TOKEN", token)
        .env("KAGEHA_WEBUI_DIST", webui)
        .env("KAGEHA_RESOURCE_DIR", resources.root());
    if std::env::var_os("KAGEHA_MODELS_FILE").is_none() {
        child.env("KAGEHA_MODELS_FILE", catalog);
    }
    replace_host(child)
}
// exec preserves signal delivery and the host's existing worker executable paths.
fn replace_host(mut child: Command) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        return Err(child.exec().into());
    }
    #[cfg(not(unix))]
    {
        if !child.status()?.success() {
            return Err(invalid("Rust host failed").into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn options_reject_duplicate_unknown_and_missing_values() {
        for args in [
            vec!["--workspace"],
            vec!["--wat", "x"],
            vec!["--workspace", "a", "--workspace", "b"],
        ] {
            assert!(options(args.into_iter().map(str::to_owned)).is_err());
        }
    }
    #[test]
    fn token_is_private_stable_and_never_overwritten() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        private_directory(&state).unwrap();
        // Explicit file path avoids depending on the test runner's environment.
        let path = state.join("token");
        let mut f = fs::OpenOptions::new();
        f.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            f.mode(0o600);
        }
        use std::io::Write;
        f.open(&path)
            .unwrap()
            .write_all(b"fixture-token-with-at-least-thirty-two-bytes")
            .unwrap();
        let first = read_token(&path).unwrap();
        assert_eq!(server_token(&state, path.to_str()).unwrap(), first);
        assert_eq!(fs::read_to_string(path).unwrap(), first);
    }
    #[test]
    fn token_file_bounds_include_optional_line_endings() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("token");
        fs::write(&path, format!("{}\r\n", "A".repeat(256))).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert_eq!(read_token(&path).unwrap(), "A".repeat(256));
        fs::write(&path, "A".repeat(257)).unwrap();
        assert!(read_token(&path).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&path, root.path().join("link")).unwrap();
            assert!(read_token(&root.path().join("link")).is_err());
        }
    }
}
