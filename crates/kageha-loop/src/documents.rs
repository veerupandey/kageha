//! WebUI hosting compatibility for the independent workspace document worker.
pub use kageha_workspace::documents::{extract, extract_bytes};
use std::{io, path::Path, sync::atomic::AtomicBool};
pub(crate) fn read_verified(
    path: &Path,
    sha256: Option<&str>,
    cancel: &AtomicBool,
) -> io::Result<String> {
    kageha_workspace::DocumentWorker::new(&std::env::current_exe()?)?.read(
        path,
        sha256,
        &kageha_contracts::execution::Control {
            cancelled: cancel,
            deadline: None,
        },
    )
}
