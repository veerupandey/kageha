//! Explicit absent-adapter implementation for browser-only native hosts.
use crate::webui::Database;
use serde_json::{json, Value};
use std::{io, sync::atomic::AtomicBool};
pub(crate) fn configured() -> bool {
    false
}
pub(crate) fn helper_down() -> bool {
    false
}
pub(crate) fn relaunch_helper() -> io::Result<()> {
    Ok(())
}
pub(crate) fn status(_chat: &str) -> Value {
    json!({"configured":false,"available":false,"reason":"Desktop adapter is not included in this host build"})
}
pub(crate) fn tool(
    _db: &Database,
    _chat: &str,
    _turn: &str,
    _call: &Value,
    _cancel: &AtomicBool,
) -> io::Result<String> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Desktop adapter is not included in this browser-only host",
    ))
}
pub(crate) fn handle(
    request: tiny_http::Request,
    _db: &Database,
    _chat: &str,
    _operation: &str,
) -> io::Result<()> {
    crate::server::response(
        request,
        503,
        json!({"error":"Desktop adapter is not included in this browser-only host"}),
    );
    Ok(())
}
