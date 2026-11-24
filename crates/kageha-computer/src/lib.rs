//! Framework-independent computer control. Host policy stays outside the OS adapter.
pub mod client;
pub mod control;
mod legacy;
pub mod mcp;
mod plan;

// Compatibility entry points for the existing v1 helper transport.
pub use legacy::{dispatch, instance_nonce, request_permission};

/// An OS adapter is serialized by its controller. It must never retry input.
/// An error after entering `call` is conservatively an unknown outcome unless
/// the adapter returns independent, structured dispatch evidence.
pub trait DesktopBackend {
    fn call(&mut self, method: &str, args: serde_json::Value) -> Result<serde_json::Value, String>;
    /// Availability, revision and held-input metadata for intervention checks.
    /// Production adapters must provide it; None is for isolated test backends.
    fn input_activity(&mut self) -> Result<Option<serde_json::Value>, String> {
        Ok(None)
    }
    fn supports_background_accessibility(&self) -> bool {
        false
    }
    fn background_activity(&mut self, _pid: u64, _window: u64) -> Result<serde_json::Value, String> {
        Err("Background Accessibility is unavailable".into())
    }
    fn clear_background_activity(&mut self) {}
    /// Stable physical desktop identity, required for production adapters.
    /// None is reserved for test adapters without real OS effects.
    fn desktop_id(&self) -> Option<&str> {
        None
    }
}

/// Current macOS adapter. Public Apple APIs are still reached via a statically
/// linked Swift bridge; the controller and transport require no Python.
#[derive(Default)]
pub struct NativeBackend {
    generation: u64,
    // The existing AppKit bridge has process-global AX state. Keep this adapter
    // on its owner thread; framework hosts should use the child-process client.
    _owner_thread: std::marker::PhantomData<std::rc::Rc<()>>,
}
impl DesktopBackend for NativeBackend {
    fn supports_background_accessibility(&self) -> bool {
        cfg!(all(target_os = "macos", feature = "native-macos"))
    }
    fn background_activity(&mut self, pid: u64, window: u64) -> Result<serde_json::Value, String> {
        self.call("input_activity", serde_json::json!({"pid":pid,"window_id":window}))
    }
    fn clear_background_activity(&mut self) {
        let _ = self.call("input_activity", serde_json::json!({"clear_background":true}));
    }
    fn input_activity(&mut self) -> Result<Option<serde_json::Value>, String> {
        self.call("input_activity", serde_json::json!({})).map(Some)
    }
    fn desktop_id(&self) -> Option<&str> {
        Some("macos-user-desktop")
    }
    fn call(&mut self, method: &str, args: serde_json::Value) -> Result<serde_json::Value, String> {
        #[cfg(target_os = "macos")]
        if unsafe { libc::pthread_main_np() } != 1 {
            return Err("native AppKit adapter requires its process main thread; use ComputerClient from framework worker threads".into());
        }
        let reply = dispatch(
            serde_json::json!({"id":0,"method":method,"args":args}),
            &mut self.generation,
        );
        if let Some(error) = reply.get("error") {
            if reply["execution_phase"] == "not_dispatched" {
                return Ok(
                    serde_json::json!({"ok":false,"execution_phase":"not_dispatched","error":error}),
                );
            }
            Err(error["message"]
                .as_str()
                .unwrap_or("native error")
                .to_owned())
        } else {
            Ok(reply["result"].clone())
        }
    }
}
