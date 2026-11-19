//! The same dedicated Linux session helper is used on the host and over pinned OpenSSH.
use crate::webui::Database;
use kageha_contracts::execution::Control;
use kageha_execution::remote::{Config, Remote};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

#[derive(Clone)]
pub(crate) enum Endpoint {
    Local {
        program: PathBuf,
        workspace: PathBuf,
    },
    Ssh(Config),
}

pub(crate) struct Session {
    pub endpoint: Endpoint,
    pub key: String,
    pub mode: &'static str,
}

pub(crate) struct Running {
    pub epoch: String,
    pub browser: String,
    pub workspace: PathBuf,
}

impl Endpoint {
    pub fn command(&self, args: &[&str], workspace: bool) -> io::Result<Command> {
        match self {
            Self::Local {
                program,
                workspace: root,
            } => {
                let mut command = Command::new(program);
                command.args(args).env_clear();
                if workspace {
                    command.current_dir(root);
                }
                for name in [
                    "HOME",
                    "PATH",
                    "USER",
                    "XDG_RUNTIME_DIR",
                    "XDG_STATE_HOME",
                    "DBUS_SESSION_BUS_ADDRESS",
                    "KAGEHA_LINUX_CHROMIUM",
                ] {
                    if let Some(value) = std::env::var_os(name) {
                        command.env(name, value);
                    }
                }
                Ok(command)
            }
            Self::Ssh(config) => Remote::new(config.clone())?.helper(args, workspace),
        }
    }

    pub fn call(
        &self,
        args: &[&str],
        workspace: bool,
        input: Vec<u8>,
        cancel: &AtomicBool,
        timeout: u64,
        limit: usize,
    ) -> io::Result<Vec<u8>> {
        let control = Control {
            cancelled: cancel,
            deadline: Some(Instant::now() + Duration::from_secs(timeout)),
        };
        let output = kageha_workspace::process::run(
            &mut self.command(args, workspace)?,
            input,
            &control,
            limit,
        )?;
        if !output.success {
            return Err(io::Error::other(format!(
                "Linux session helper could not complete {}",
                args.first().copied().unwrap_or("request")
            )));
        }
        Ok(output.bytes)
    }

    pub fn capabilities(&self) -> io::Result<Vec<String>> {
        let output = self.call(&["probe"], false, vec![], &AtomicBool::new(false), 10, 1024)?;
        Ok(String::from_utf8_lossy(&output)
            .lines()
            .map(str::to_owned)
            .collect())
    }
}

pub(crate) fn local_mode() -> Option<&'static str> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    match std::env::var("KAGEHA_LINUX_BROWSER").as_deref() {
        Ok("headless") => Some("headless"),
        Ok("desktop") => Some("desktop"),
        _ => None,
    }
}

pub(crate) fn mode(remote: &Value) -> Option<&'static str> {
    if remote.is_null() {
        return local_mode();
    }
    if remote["enable_desktop"] == true {
        Some("desktop")
    } else if remote["enable_browser"] == true {
        Some("headless")
    } else {
        None
    }
}

pub(crate) fn remote_endpoint(remote: &Value) -> io::Result<Endpoint> {
    Ok(Endpoint::Ssh(Config::from_legacy(remote)?))
}

pub(crate) fn for_chat(
    db: &Database,
    chat: &str,
    remote: &Value,
    create_workspace: bool,
) -> io::Result<Option<Session>> {
    let Some(mode) = mode(remote) else {
        return Ok(None);
    };
    let endpoint = if remote.is_null() {
        let program = std::env::var_os("KAGEHA_LINUX_SESSION_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/usr/local/bin/kageha-linux-session"));
        if !program.is_absolute() || !program.is_file() {
            return Err(io::Error::other("Install the Kageha Linux session helper"));
        }
        let workspace = if create_workspace {
            crate::ui_workspace::open_for_chat(db, chat)?
                .root()
                .to_owned()
        } else if let Some(root) = crate::ui_workspace::existing_root(db, chat)? {
            root.canonicalize()?
        } else {
            crate::ui_uploads::root_path(db, chat)?.join(kageha_workspace::task::WORKSPACE_DIR)
        };
        Endpoint::Local { program, workspace }
    } else {
        remote_endpoint(remote)?
    };
    let identity = match &endpoint {
        Endpoint::Local { workspace, .. } => json!({"workspace":workspace}),
        Endpoint::Ssh(config) => json!({"host":config.target(),"workspace":config.workdir}),
    };
    let key = identifier(&json!({"chat":chat,"endpoint":identity}));
    Ok(Some(Session {
        endpoint,
        key,
        mode,
    }))
}

pub(crate) fn identifier(value: &Value) -> String {
    format!("{:x}", Sha256::digest(value.to_string().as_bytes()))[..32].to_owned()
}

impl Session {
    pub fn provider(&self) -> &'static str {
        match self.endpoint {
            Endpoint::Local { .. } => "linux_host",
            Endpoint::Ssh(_) => "ssh_desktop",
        }
    }

    pub fn running(&self, start: bool, cancel: &AtomicBool) -> io::Result<Running> {
        let args = if start {
            vec!["ensure", &self.key, self.mode]
        } else {
            vec!["status", &self.key]
        };
        let bytes = self.endpoint.call(
            &args,
            start,
            vec![],
            cancel,
            if start { 25 } else { 7 },
            8192,
        )?;
        let status = String::from_utf8(bytes).map_err(io::Error::other)?;
        let (identity, workspace) = status
            .trim_end_matches('\n')
            .split_once('\n')
            .ok_or_else(|| io::Error::other("Linux session did not report its workspace"))?;
        let (epoch, browser) = identity
            .split_once(' ')
            .ok_or_else(|| io::Error::other("Linux session did not report Chromium identity"))?;
        let workspace = PathBuf::from(workspace);
        if uuid::Uuid::parse_str(epoch).is_err()
            || !browser
                .strip_prefix("/devtools/browser/")
                .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
            || !workspace.is_absolute()
        {
            return Err(io::Error::other("Invalid Chromium identity or workspace"));
        }
        Ok(Running {
            epoch: epoch.into(),
            browser: browser.into(),
            workspace,
        })
    }

    pub fn operation(
        &self,
        op: &str,
        epoch: &str,
        args: &[&str],
        cancel: &AtomicBool,
        limit: usize,
    ) -> io::Result<Vec<u8>> {
        let mut request = vec![op, &self.key, epoch];
        request.extend(args);
        self.endpoint
            .call(&request, false, vec![], cancel, 10, limit)
    }

    pub fn stop(&self) -> io::Result<()> {
        self.endpoint
            .call(
                &["stop", &self.key],
                false,
                vec![],
                &AtomicBool::new(false),
                15,
                1024,
            )
            .map(|_| ())
    }
}
