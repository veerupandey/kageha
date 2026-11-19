use serde_json::{json, Value};
#[cfg(all(target_os = "macos", feature = "native-macos"))]
use std::ffi::{CStr, CString};

use crate::plan;

const METHODS: &[&str] = &[
    "hello",
    "check_permissions",
    "input_activity",
    "list_apps",
    "list_windows",
    "get_window_state",
    "capture_window",
    "click",
    "type_text",
    "press_key",
    "set_value",
    "scroll",
    "drag",
    "perform_secondary_action",
    "launch_app",
    "execute_batch",
    "prepare_batch",
    "select_text",
    "focus_window",
    "focus_element",
];

fn validate_batch(args: &Value) -> Result<(), &'static str> {
    let steps = args
        .get("steps")
        .and_then(Value::as_array)
        .ok_or("batch requires steps")?;
    if steps.is_empty() || steps.len() > 24 {
        return Err("batch requires 1..24 steps");
    }
    let mut target = None;
    for step in steps {
        let method = step
            .get("method")
            .and_then(Value::as_str)
            .ok_or("batch method required")?;
        let a = step
            .get("args")
            .filter(|v| v.is_object())
            .ok_or("batch args required")?;
        let pid = a
            .get("pid")
            .and_then(Value::as_u64)
            .filter(|v| *v > 0 && *v <= i32::MAX as u64)
            .ok_or("positive pid required")?;
        let wid = a
            .get("window_id")
            .and_then(Value::as_u64)
            .filter(|v| *v > 0 && *v <= u32::MAX as u64)
            .ok_or("positive window id required")?;
        if target.is_some_and(|v| v != (pid, wid)) {
            return Err("batch must use one exact window");
        }
        target = Some((pid, wid));
        let text = |field: &str| a.get(field).is_some_and(Value::is_string);
        let numeric = |field: &str| {
            a.get(field)
                .and_then(Value::as_f64)
                .is_some_and(|v| v.is_finite() && v >= 0.0)
        };
        let valid = match method {
            "click" => text("element_token") || (numeric("x") && numeric("y")),
            "set_value" => text("element_token") && text("value"),
            "type_text" => text("text"),
            "press_key" => text("key"),
            "scroll" => a
                .get("direction")
                .and_then(Value::as_str)
                .is_some_and(|s| ["up", "down", "left", "right"].contains(&s)),
            "drag" => ["from_x", "from_y", "to_x", "to_y"]
                .iter()
                .all(|f| numeric(f)),
            _ => false,
        };
        if !valid {
            return Err("unsupported or malformed batch step; no input dispatched");
        }
    }
    Ok(())
}

#[cfg(all(target_os = "macos", feature = "native-macos"))]
extern "C" {
    fn kageha_platform_call(request: *const std::os::raw::c_char) -> *mut std::os::raw::c_char;
    fn kageha_platform_free(response: *mut std::os::raw::c_char);
}

fn platform(request: &Value) -> Result<Value, String> {
    #[cfg(all(target_os = "macos", feature = "native-macos"))]
    {
        let encoded = CString::new(request.to_string()).map_err(|_| "invalid request")?;
        let raw = unsafe { kageha_platform_call(encoded.as_ptr()) };
        if raw.is_null() {
            return Err("platform returned an empty response".into());
        }
        let response = unsafe { CStr::from_ptr(raw) }.to_bytes().to_vec();
        unsafe {
            kageha_platform_free(raw);
        }
        serde_json::from_slice(&response).map_err(|e| e.to_string())
    }
    #[cfg(not(all(target_os = "macos", feature = "native-macos")))]
    {
        let _ = request;
        Err("native adapter requires macOS and the native-macos build feature".into())
    }
}

pub fn instance_nonce() -> &'static str {
    static NONCE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NONCE.get_or_init(|| {
        use std::io::Read;
        let mut bytes = [0u8; 16];
        std::fs::File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut bytes))
            .expect("OS random source required");
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    })
}

pub fn dispatch(request: Value, generation: &mut u64) -> Value {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let error =
        |code: &str, message: &str| json!({"id": id, "error": {"code":code,"message":message}});
    if !request.is_object() || !id.is_u64() {
        return error("invalid_request", "request requires an unsigned numeric id");
    }
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    if !METHODS.contains(&method) {
        return error(
            "unsupported_method",
            "method is not supported by this native backend",
        );
    }
    if method == "hello" {
        return json!({"id":id,"result":{"protocol":1,"backend":"kageha-rust","version":env!("CARGO_PKG_VERSION"),"capabilities":METHODS}});
    }
    let args = request.get("args").cloned().unwrap_or_else(|| json!({}));
    if !args.is_object() {
        return error("invalid_args", "args must be an object");
    }
    if method == "prepare_batch" {
        return match plan::prepare(&args) {
            Ok(result) => json!({"id":id,"result":result}),
            Err(message) => error("invalid_batch", &message),
        };
    }
    let mut forwarded = json!({"method":method,"args":args});
    if method == "execute_batch" {
        if let Err(message) = validate_batch(&forwarded["args"]) {
            return error("invalid_batch", message);
        }
        let session = forwarded["args"]
            .get("session")
            .cloned()
            .unwrap_or(json!("default"));
        if let Some(steps) = forwarded["args"]["steps"].as_array_mut() {
            for step in steps {
                step["args"]["session"] = session.clone();
            }
        }
    }
    if method == "get_window_state" {
        *generation = generation.wrapping_add(1);
        forwarded["args"]["native_snapshot_id"] =
            json!(format!("s{}-{:016x}", instance_nonce(), generation));
    }
    match platform(&forwarded) {
        Ok(result) if result.get("error").is_some() => {
            let mut reply = error(
                "platform_error",
                result["error"]
                    .as_str()
                    .unwrap_or("native operation failed"),
            );
            if result["execution_phase"] == "not_dispatched" {
                reply["execution_phase"] = json!("not_dispatched");
            }
            reply
        }
        Ok(result) => json!({"id":id,"result":result}),
        Err(message) => error("platform_error", &message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn batch_rejects_later_bad_target_before_dispatch() {
        let a = json!({"steps":[{"method":"press_key","args":{"pid":7,"window_id":9,"key":"a"}}, {"method":"press_key","args":{"pid":7,"window_id":10,"key":"b"}}]});
        assert!(validate_batch(&a).is_err());
    }
    #[test]
    fn batch_accepts_observed_field_values() {
        assert!(validate_batch(&json!({"steps":[{"method":"set_value","args":{"pid":7,"window_id":9,"element_token":"fresh:1","value":"example"}}]})).is_ok());
    }
    #[test]
    fn batch_rejects_arbitrary_methods() {
        assert!(validate_batch(
            &json!({"steps":[{"method":"launch_app","args":{"pid":7,"window_id":9}}]})
        )
        .is_err());
    }
}

/// Explicit host/user gesture only; absent from agent RPC method catalogs.
pub fn request_permission(kind: &str) -> Result<Value, String> {
    if !["screen_recording", "accessibility", "input_monitoring"].contains(&kind) {
        return Err("unsupported permission".into());
    }
    platform(&json!({"method":"request_permission","args":{"kind":kind}}))
}
