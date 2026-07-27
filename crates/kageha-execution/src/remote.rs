//! Explicit OpenSSH adapter. Shell scripts execute remotely; local argv never goes through a shell.
use kageha_contracts::execution::Control;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    io,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub id: String,
    pub host: String,
    #[serde(default)]
    pub user: String,
    #[serde(default = "port")]
    pub port: u16,
    #[serde(default)]
    pub identity: String,
    #[serde(default)]
    pub known_hosts: String,
    #[serde(default)]
    pub workdir: String,
    #[serde(default)]
    pub ssh_program: Option<PathBuf>,
    #[serde(default)]
    pub accept_new_host_keys: bool,
    #[serde(default)]
    pub sandbox: Option<kageha_sandbox::ProcessSandbox>,
}
fn port() -> u16 {
    22
}
fn token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && !value.starts_with('-')
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-:".contains(&b))
}
impl Config {
    pub fn validate(&self) -> io::Result<()> {
        if !token(&self.host)
            || (!self.user.is_empty() && !token(&self.user))
            || self.port == 0
            || self.id.is_empty()
            || self.id.len() > 128
            || !self
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        {
            return Err(io::Error::other("Invalid SSH identity or endpoint"));
        }
        for path in [&self.identity, &self.known_hosts, &self.workdir] {
            if path.len() > 4096 || path.chars().any(char::is_control) {
                return Err(io::Error::other("Invalid SSH path"));
            }
        }
        if [&self.identity, &self.known_hosts]
            .iter()
            .any(|p| !p.is_empty() && !Path::new(p).is_absolute())
        {
            return Err(io::Error::other(
                "SSH key and known-host paths must be absolute",
            ));
        }
        if kageha_sandbox::required()? && self.sandbox.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Managed policy requires an SSH process sandbox",
            ));
        }
        Ok(())
    }
    pub fn from_legacy(value: &Value) -> io::Result<Self> {
        let config:Self=serde_json::from_value(json!({"id":value["id"],"host":value["host"],"user":value["user"].as_str().unwrap_or(""),"port":value["port"].as_u64().unwrap_or(22),"identity":value["identity"].as_str().unwrap_or(""),"known_hosts":value["known_hosts"].as_str().unwrap_or(""),"workdir":value["workdir"].as_str().unwrap_or(""),"ssh_program":value["ssh_program"],"sandbox":value["sandbox"]})).map_err(|_|io::Error::other("Invalid SSH configuration"))?;
        config.validate()?;
        Ok(config)
    }
    /// Opaque commands can affect anything accessible to this SSH account. Changing a label/workdir/key cannot escape an unresolved receipt.
    pub fn target(&self) -> String {
        crate::digest(
            &json!({"host":self.host.to_ascii_lowercase(),"user":self.user,"port":self.port}),
        )
    }
}
pub struct Remote {
    pub config: Config,
    program: PathBuf,
}

pub struct FileSnapshot {
    pub bytes: Vec<u8>,
    pub sha256: String,
}
pub fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
pub fn relative(path: &str) -> io::Result<()> {
    if path.is_empty()
        || path.len() > 4096
        || path.starts_with('/')
        || path.split('/').any(|v| matches!(v, "" | "." | ".."))
        || path.chars().any(char::is_control)
    {
        return Err(io::Error::other(
            "Expected a relative remote workspace path",
        ));
    }
    Ok(())
}
impl Remote {
    pub fn new(config: Config) -> io::Result<Self> {
        config.validate()?;
        let program = if let Some(path) = &config.ssh_program {
            if !path.is_absolute() {
                return Err(io::Error::other("SSH executable must be absolute"));
            }
            path.canonicalize()?
        } else {
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .map(|p| p.join("ssh"))
                .find(|p| p.is_file())
                .ok_or_else(|| io::Error::other("OpenSSH executable is unavailable"))?
                .canonicalize()?
        };
        if !program.is_file() {
            return Err(io::Error::other("OpenSSH executable is unavailable"));
        }
        Ok(Self { config, program })
    }
    pub fn fingerprint(&self) -> String {
        crate::digest(&json!({"configuration":self.config,"program":self.program}))
    }
    fn root(&self) -> String {
        let value = &self.config.workdir;
        if value.is_empty() {
            "$HOME/.kageha/work".into()
        } else if let Some(value) = value.strip_prefix("~/") {
            format!("$HOME/{}", quote(value))
        } else {
            quote(value)
        }
    }
    fn command(&self, script: &str) -> io::Result<Command> {
        let target = if self.config.user.is_empty() {
            self.config.host.clone()
        } else {
            format!("{}@{}", self.config.user, self.config.host)
        };
        let mut args = vec![
            "-F".into(),
            "/dev/null".into(),
            "-o".into(),
            "BatchMode=yes".into(),
            "-o".into(),
            format!(
                "StrictHostKeyChecking={}",
                if self.config.accept_new_host_keys {
                    "accept-new"
                } else {
                    "yes"
                }
            ),
            "-o".into(),
            "ConnectTimeout=8".into(),
            "-o".into(),
            "ForwardAgent=no".into(),
            "-o".into(),
            "ForwardX11=no".into(),
            "-o".into(),
            "ControlMaster=no".into(),
            "-o".into(),
            "ControlPath=none".into(),
            "-o".into(),
            "ControlPersist=no".into(),
            "-p".into(),
            self.config.port.to_string(),
        ];
        if !self.config.identity.is_empty() {
            args.extend([
                "-o".into(),
                "IdentitiesOnly=yes".into(),
                "-i".into(),
                self.config.identity.clone(),
            ]);
        }
        if !self.config.known_hosts.is_empty() {
            args.extend([
                "-o".into(),
                "GlobalKnownHostsFile=/dev/null".into(),
                "-o".into(),
                "UpdateHostKeys=no".into(),
                "-o".into(),
                format!(
                    "UserKnownHostsFile=\"{}\"",
                    self.config
                        .known_hosts
                        .replace('\\', "\\\\")
                        .replace('"', "\\\"")
                        .replace('%', "%%")
                ),
            ]);
        }
        args.extend(["--".into(), target, script.into()]);
        let mut command = if let Some(sandbox) = &self.config.sandbox {
            sandbox.command(
                self.program
                    .to_str()
                    .ok_or_else(|| io::Error::other("Invalid SSH executable"))?,
                &args,
            )?
        } else {
            let mut cmd = Command::new(&self.program);
            cmd.args(args);
            cmd
        };
        command.env_clear();
        for key in ["PATH", "HOME", "LANG", "SSH_AUTH_SOCK"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        Ok(command)
    }

    pub fn helper(&self, args: &[&str], workspace: bool) -> io::Result<Command> {
        let invocation = args.iter().map(|arg| quote(arg)).collect::<Vec<_>>().join(" ");
        let command = format!("exec /usr/local/bin/kageha-linux-session {invocation}");
        if workspace {
            let root = self.root();
            self.command(&format!("mkdir -p -- {root} && cd -- {root} && {command}"))
        } else {
            self.command(&command)
        }
    }
    pub fn execute(
        &self,
        script: &str,
        input: Vec<u8>,
        control: &Control<'_>,
        timeout: u64,
    ) -> io::Result<Value> {
        if script.is_empty() || script.len() > 128 * 1024 || script.contains('\0') {
            return Err(io::Error::other("A bounded remote command is required"));
        }
        control.check()?;
        let output = self.run(script, input, control, timeout, 1024 * 1024, true)?;
        Ok(json!({"ok":output.success,"exit_code":output.exit_code,
            "stdout":String::from_utf8_lossy(&output.bytes),"host_id":self.config.id,"execution_target":"remote"}))
    }

    fn run(
        &self,
        script: &str,
        input: Vec<u8>,
        control: &Control<'_>,
        timeout: u64,
        limit: usize,
        create_workspace: bool,
    ) -> io::Result<kageha_workspace::process::Output> {
        let root = self.root();
        let setup = if create_workspace { format!("mkdir -p -- {root} && ") } else { String::new() };
        self.run_command(&format!("{setup}cd -- {root} && {{ {script}\n}}"), input, control, timeout, limit)
    }

    fn run_command(&self, script: &str, input: Vec<u8>, control: &Control<'_>, timeout: u64, limit: usize) -> io::Result<kageha_workspace::process::Output> {
        let mut command = self.command(script)?;
        let deadline = Instant::now() + Duration::from_secs(timeout.clamp(1, 120));
        let control = Control {
            cancelled: control.cancelled,
            deadline: Some(control.deadline.unwrap_or(deadline).min(deadline)),
        };
        let output = kageha_workspace::process::run(&mut command, input, &control, limit)?;
        if output.exit_code.is_none() || output.exit_code == Some(255) {
            return Err(io::Error::other(
                "SSH transport did not establish a known remote outcome",
            ));
        }
        Ok(output)
    }

    pub fn probe(&self, control: &Control<'_>) -> io::Result<Value> {
        let script = format!(r#"root={root}
hostname; uname -s
if command -v bash >/dev/null 2>&1; then printf 'bash\n'; else printf 'no-bash\n'; fi
if test -d "$root" && test -w "$root"; then printf 'writable\n'
elif test -e "$root"; then printf 'not-writable\n'
else
    parent="$root"
    while ! test -e "$parent" && test "$parent" != '.'; do parent=$(dirname -- "$parent"); done
    if test -d "$parent" && test -w "$parent"; then printf 'can-create\n'; else printf 'not-writable\n'; fi
fi
printf '%s\n' "$root""#, root = self.root());
        let output = self.run_command(&script, vec![], control, 15, 8192)?;
        let text = String::from_utf8(output.bytes).map_err(io::Error::other)?;
        let fields: Vec<_> = text.lines().collect();
        if !output.success || fields.len() != 5 { return Err(io::Error::other("Invalid SSH probe response")); }
        Ok(json!({"hostname":fields[0],"uname":fields[1],"bash":fields[2]=="bash",
            "workdir_state":fields[3],"workdir_writable":fields[3]=="writable","workdir":fields[4]}))
    }
    pub fn read(&self, path: &str, control: &Control<'_>) -> io::Result<String> {
        let snapshot = self.snapshot(path, 65536, control)?;
        String::from_utf8(snapshot.bytes).map_err(|_| io::Error::other("Remote file is not UTF-8 text; use remote_download"))
    }

    pub fn snapshot(&self, path: &str, limit: usize, control: &Control<'_>) -> io::Result<FileSnapshot> {
        relative(path)?;
        if limit > kageha_workspace::DOCUMENT_LIMIT {
            return Err(io::Error::other("Invalid remote file size limit"));
        }
        let script = format!(r#"root=$(pwd -P) || exit 2
file=$(realpath {path}) || exit 2
case "$file" in "$root"/*) ;; *) exit 2;; esac
test -f "$file" || exit 2
if test -d "/proc/$$/fd"; then
    exec 3<"$file" || exit 2
    file="/proc/$$/fd/3"
    case "$(readlink -- "$file")" in "$root"/*) ;; *) exit 2;; esac
    test -f "$file" || exit 2
else
    exec 3<"$file" || exit 2
fi
if command -v sha256sum >/dev/null 2>&1; then header=$(sha256sum < "$file"); else header=$(shasum -a 256 < "$file"); fi || exit 2
printf '%s\n' "${{header%% *}}"
head -c {count} <&3"#, path = quote(&format!("./{path}")), count = limit + 1);
        let mut output = self.run(&script, vec![], control, 60, limit + 66, false)?;
        if !output.success || output.bytes.len() < 65 || output.bytes[64] != b'\n' {
            return Err(io::Error::other("Remote file unavailable or outside the workspace"));
        }
        let bytes = output.bytes.split_off(65);
        if bytes.len() > limit {
            return Err(io::Error::other(format!("Remote file exceeds {limit} bytes")));
        }
        let sha256 = crate::hash(&bytes);
        if output.bytes[..64] != *sha256.as_bytes() {
            return Err(io::Error::other("Remote file changed while reading; checksum did not match"));
        }
        Ok(FileSnapshot { bytes, sha256 })
    }

    pub fn list(&self, path: Option<&str>, control: &Control<'_>) -> io::Result<Value> {
        let path = path.filter(|path| *path != ".");
        if let Some(path) = path { relative(path)?; }
        let script = format!(r#"root=$(pwd -P) || exit 2
dir=$(realpath {path}) || exit 2
case "$dir" in "$root"|"$root"/*) ;; *) exit 2;; esac
cd -- "$dir" || exit 2
case "$(pwd -P)" in "$root"|"$root"/*) ;; *) exit 2;; esac
count=0
for item in ./* ./.[!.]* ./..?*; do
    if test -L "$item"; then kind=symlink
    elif test -f "$item"; then kind=file
    elif test -d "$item"; then kind=directory
    else continue; fi
    printf '%s\000%s\000' "$kind" "${{item#./}}"
    count=$((count + 1))
    test "$count" -lt 201 || break
done
exit 0"#, path = quote(&format!("./{}", path.unwrap_or("."))));
        let output = self.run(&script, vec![], control, 20, 128 * 1024, false)?;
        if !output.success { return Err(io::Error::other("Remote directory unavailable or outside the workspace")); }
        if output.bytes.is_empty() {
            return Ok(json!({"path":path.unwrap_or("."),"entries":[],"truncated":false}));
        }
        let bytes = output.bytes.strip_suffix(&[0]).ok_or_else(|| io::Error::other("Incomplete remote directory response"))?;
        let fields: Vec<_> = bytes.split(|byte| *byte == 0).collect();
        if fields.len() % 2 != 0 { return Err(io::Error::other("Invalid remote directory response")); }
        let entries: Vec<_> = fields.chunks_exact(2).take(200).filter_map(|pair| {
            let kind = std::str::from_utf8(pair[0]).ok()?;
            let name = std::str::from_utf8(pair[1]).ok()?;
            Some(json!({"kind":kind,"name":name}))
        }).collect();
        Ok(json!({"path":path.unwrap_or("."),"entries":entries,"truncated":fields.len() > 400 || entries.len() * 2 != fields.len()}))
    }
    pub fn upload(&self, path: &str, bytes: Vec<u8>, control: &Control<'_>) -> io::Result<Value> {
        relative(path)?;
        if path.len() > 1024 {
            return Err(io::Error::other(
                "Remote upload destination exceeds 1024 bytes",
            ));
        }
        if bytes.len() > kageha_workspace::DOCUMENT_LIMIT {
            return Err(io::Error::other("Upload exceeds 16 MiB"));
        }
        let mut pieces = path.split('/').collect::<Vec<_>>();
        let basename = pieces.pop().unwrap();
        let hash = crate::hash(&bytes);
        let mut script = "umask 077; root=$(pwd -P); parent=\"$root\"; ".to_owned();
        for piece in pieces {
            script+=&format!("next=\"$parent\"/{p}; if test -e \"$next\" || test -L \"$next\"; then next=$(realpath \"$next\") || exit 2; case \"$next\" in \"$root\"|\"$root\"/*) ;; *) exit 2;; esac; test -d \"$next\" || exit 2; else mkdir -- \"$next\" || exit 2; fi; parent=\"$next\"; ",p=quote(piece));
        }
        script+=&format!("test ! -d \"$parent\"/{name} || exit 2; umask 077; tmp=$(mktemp \"$parent/.kageha-upload.XXXXXX\") || exit 2; trap 'rm -f -- \"$tmp\"' EXIT; cat >\"$tmp\" || exit 2; if command -v sha256sum >/dev/null 2>&1; then actual=$(sha256sum \"$tmp\"); else actual=$(shasum -a 256 \"$tmp\"); fi; actual=${{actual%% *}}; test \"$actual\" = {hash} || exit 2; mv -f -- \"$tmp\" \"$parent\"/{name} && printf '%s' {hash}",name=quote(basename),hash=quote(&hash));
        let output = self.execute(&script, bytes, control, 60)?;
        if output["ok"] != true || output["stdout"] != hash {
            return Err(io::Error::other(
                "Remote upload was not verified; inspect destination before retrying",
            ));
        }
        Ok(json!({"ok":true,"path":path,"sha256":hash,"host_id":self.config.id}))
    }
}
