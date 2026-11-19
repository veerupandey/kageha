//! A private pipe ties Chromium's lifetime to its owning daemon, including
//! when the daemon exits before its normal shutdown code can run.
use crate::cdp::Result;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};

pub struct OwnedChromium {
    child: Child,
    lease: Option<ChildStdin>,
    pub profile: PathBuf,
}

impl OwnedChromium {
    pub async fn spawn(executable: &Path, root: &Path) -> Result<Self> {
        let worker = std::env::current_exe().map_err(|_| "browser_launch_failed")?;
        let mut child = Command::new(worker)
            .arg("--watch-owned-chromium")
            .arg(executable)
            .arg(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| "browser_launch_failed")?;
        let lease = child.stdin.take().ok_or("browser_launch_failed")?;
        let mut output = BufReader::new(child.stdout.take().ok_or("browser_launch_failed")?);
        let mut name = String::new();
        output
            .read_line(&mut name)
            .await
            .map_err(|_| "browser_launch_failed")?;
        let name = name.trim_end();
        if !name.starts_with("chromium-")
            || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err("browser_launch_failed");
        }
        Ok(Self {
            child,
            lease: Some(lease),
            profile: root.join(name),
        })
    }

    pub fn alive(&mut self) -> Result<bool> {
        self.child
            .try_wait()
            .map(|status| status.is_none())
            .map_err(|_| "browser_process_error")
    }

    pub async fn wait(&mut self) -> Result<()> {
        self.child
            .wait()
            .await
            .map(|_| ())
            .map_err(|_| "browser_process_error")
    }

    pub async fn stop(&mut self) -> Result<()> {
        self.lease.take();
        tokio::time::timeout(Duration::from_secs(3), self.wait())
            .await
            .map_err(|_| "browser_cleanup_pending")?
    }
}

pub async fn watch(executable: &Path, root: &Path) -> io::Result<()> {
    let profile = tempfile::Builder::new()
        .prefix("chromium-")
        .tempdir_in(root)?;
    let mut user_data = std::ffi::OsString::from("--user-data-dir=");
    user_data.push(profile.path());
    let mut child = Command::new(executable)
        .args([
            "--headless",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-background-networking",
            "--disable-extensions",
            "--disable-popup-blocking",
            "--site-per-process",
            "--disable-breakpad",
            "--remote-debugging-address=127.0.0.1",
            "--remote-debugging-port=0",
        ])
        .arg(user_data)
        .arg("about:blank")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let name = profile
        .path()
        .file_name()
        .expect("temporary directory name");
    let announced =
        writeln!(io::stdout(), "{}", name.to_string_lossy()).and_then(|_| io::stdout().flush());
    if announced.is_ok() {
        let mut stdin = tokio::io::stdin();
        let mut lease = [0u8; 1];
        tokio::select! {
            status = child.wait() => { status?; return Ok(()); },
            _ = stdin.read(&mut lease) => {},
        }
    }
    // Signal only the unreaped child spawned above. Its PID cannot be
    // reused while the child is still live or awaiting reaping.
    if let Some(pid) = child.id() {
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    }
    if tokio::time::timeout(Duration::from_secs(1), child.wait())
        .await
        .is_err()
    {
        child.kill().await?;
    }
    Ok(())
}
