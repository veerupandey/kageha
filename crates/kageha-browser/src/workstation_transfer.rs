//! Browser and daemon file paths can differ on a workstation.
use crate::cdp::Result;
use kageha_contracts::execution::Control;
use kageha_execution::remote::{Config, Remote};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

#[derive(Clone, Deserialize)]
pub(crate) struct Transfer {
    pub transfer: PathBuf,
    #[serde(default = "docker_path")]
    pub guest_transfer: PathBuf,
    #[serde(default)]
    pub remote: Option<Config>,
}

fn docker_path() -> PathBuf {
    PathBuf::from("/browser-transfer")
}

impl Transfer {
    pub fn guest(&self, local: &Path) -> Result<PathBuf> {
        Ok(self.guest_transfer.join(
            local
                .strip_prefix(&self.transfer)
                .map_err(|_| "workstation_transfer_path_invalid")?,
        ))
    }

    fn relative(&self, local: &Path) -> Result<String> {
        let path = local
            .strip_prefix(&self.transfer)
            .map_err(|_| "workstation_transfer_path_invalid")?;
        let path = path.to_str().ok_or("workstation_transfer_path_invalid")?;
        Ok(format!(".kageha-browser-transfer/{path}"))
    }

    pub fn stage(&self, local: &Path, deadline: Instant) -> Result<()> {
        let Some(config) = self.remote.clone() else {
            return Ok(());
        };
        let bytes = std::fs::read(local).map_err(|_| "script_upload_copy_failed")?;
        let control = Control {
            cancelled: &AtomicBool::new(false),
            deadline: Some(deadline),
        };
        Remote::new(config)
            .and_then(|ssh| {
                ssh.upload(
                    &self.relative(local).map_err(std::io::Error::other)?,
                    bytes,
                    &control,
                )
            })
            .map_err(|_| "workstation_upload_failed")?;
        Ok(())
    }

    pub fn retrieve(&self, local: &Path, deadline: Duration, limit: usize) -> Result<Vec<u8>> {
        let config = self
            .remote
            .clone()
            .ok_or("workstation_transfer_path_invalid")?;
        let control = Control {
            cancelled: &AtomicBool::new(false),
            deadline: Some(Instant::now() + deadline),
        };
        Remote::new(config)
            .and_then(|ssh| {
                ssh.snapshot(
                    &self.relative(local).map_err(std::io::Error::other)?,
                    limit,
                    &control,
                )
            })
            .map(|file| file.bytes)
            .map_err(|_| "workstation_download_failed")
    }
}
