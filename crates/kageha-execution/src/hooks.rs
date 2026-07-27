//! Host-independent hook execution. The caller owns approval, scope and event
//! selection; this executor owns dispatch and durable, non-replayable receipts.
use crate::{hash, journal::Journal};
use kageha_contracts::execution::{Control, ToolContext, ToolReply};
use kageha_workspace::{
    hooks::{Hook, Outcome},
    ReadWorkspace,
};
use serde_json::{json, Value};
use std::{io, path::Path, sync::OnceLock, time::Duration};

/// Exact input presented for authorization by every host. Keep this shape stable
/// so existing WebUI hook receipts remain readable after extraction.
pub fn context(hook: &Hook, payload: &Value, workspace: &Path) -> io::Result<Value> {
    let body = serde_json::to_vec(payload)?;
    Ok(
        json!({"hook":hook,"payload_sha256":hash(&body),"workspace":workspace,
        "tool_call_id":payload["tool_call_id"],"url":if hook.command.is_empty(){hook.http.as_str()}else{hook.config_path.to_str().unwrap_or("project_hook")}}),
    )
}
/// A captured command cannot be swapped while a decision is pending. Symlinked
/// configurations and changed bytes are rejected before dispatch.
pub fn verify(hook: &Hook, control: &Control<'_>) -> io::Result<()> {
    let parent = hook
        .config_path
        .parent()
        .ok_or_else(|| io::Error::other("Hook configuration has no directory"))?;
    let name = hook
        .config_path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| io::Error::other("Hook configuration has no file name"))?;
    let current = ReadWorkspace::new(parent)?.snapshot(name, 256 * 1024, control)?;
    if current.sha256 != hook.config_sha256 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Hook configuration changed since it was captured",
        ));
    }
    Ok(())
}
pub fn execute(
    journal: &Journal,
    scope: &ToolContext,
    hook: &Hook,
    payload: &Value,
    workspace: &Path,
    control: &Control<'_>,
    authorize: impl FnOnce(&Value) -> io::Result<bool>,
) -> io::Result<Outcome> {
    execute_inner(
        journal, scope, hook, payload, workspace, control, authorize, true,
    )
}
/// Execute a host-owned immutable snapshot. The WebUI deliberately keeps a turn's
/// captured configuration even if a later edit applies to future turns. The host
/// still approves the exact serialized hook and payload; model input cannot select it.
pub fn execute_captured(
    journal: &Journal,
    scope: &ToolContext,
    hook: &Hook,
    payload: &Value,
    workspace: &Path,
    control: &Control<'_>,
    authorize: impl FnOnce(&Value) -> io::Result<bool>,
) -> io::Result<Outcome> {
    execute_inner(
        journal, scope, hook, payload, workspace, control, authorize, false,
    )
}
fn execute_inner(
    journal: &Journal,
    scope: &ToolContext,
    hook: &Hook,
    payload: &Value,
    workspace: &Path,
    control: &Control<'_>,
    authorize: impl FnOnce(&Value) -> io::Result<bool>,
    verify_source: bool,
) -> io::Result<Outcome> {
    control.check()?;
    if !payload.is_object()
        || serde_json::to_vec(payload)?.len() > 100000
        || !hook.timeout_s.is_finite()
        || hook.timeout_s <= 0.0
        || hook.timeout_s > hook.timeout_limit()
    {
        return Err(io::Error::other("Invalid hook invocation"));
    }
    if !hook.http.is_empty() && hook.command.is_empty() {
        kageha_integrations::valid_url(&hook.http)?;
    }
    let context = context(hook, payload, workspace)?;
    let identity = hash(
        format!(
            "{}\0{}\0{}",
            payload["tool_call_id"],
            hook.fingerprint(),
            hook.event
        )
        .as_bytes(),
    );
    let target = hash(format!("project-hook\0{}", hook.resource()).as_bytes());
    let digest = hash(context.to_string().as_bytes());
    if let Some(previous) = journal.completed(scope, &identity, &target, &digest)? {
        return serde_json::from_str(&previous.content).map_err(io::Error::other);
    }
    if verify_source {
        verify(hook, control)?;
    }
    if !authorize(&context)? {
        return Err(kageha_contracts::policy::stop(
            "The configured project hook was declined",
        ));
    }
    if verify_source {
        verify(hook, control)?;
    }
    let dispatched = hook.control(control);
    // A receipt is reserved only after validation/approval. Any failure after
    // reservation is uncertain, including deadline/cancellation and HTTP errors.
    let result = journal.run(scope, &identity, &target, &digest, &dispatched, || {
        let body = serde_json::to_vec(payload)?;
        let outcome = if !hook.command.is_empty() {
            // Materialize only the already-approved, hash-bound plugin snapshot.
            // TempDir provides a private root and removes it on every exit path.
            let root=if hook.plugin_files.is_empty() {None} else {
                if !hook.config_path.starts_with("/plugin/") || hook.plugin_files.len()>64 {return Err(io::Error::other("Invalid plugin hook resources"));}
                let dir=tempfile::Builder::new().prefix("kageha-plugin-hook-").tempdir()?;
                let mut total=0usize;
                for (name,contents) in &hook.plugin_files {
                    let path=Path::new(name);
                    if name.is_empty() || name.len()>1024 || path.components().any(|part|!matches!(part,std::path::Component::Normal(_))) {return Err(io::Error::other("Unsafe plugin hook resource path"));}
                    total=total.checked_add(contents.len()).ok_or_else(||io::Error::other("Plugin hook resource limit exceeded"))?;
                    if contents.len()>65536 || total>256*1024 || contents.contains('\0') {return Err(io::Error::other("Plugin hook resources exceed limits"));}
                    let destination=dir.path().join(path);
                    std::fs::create_dir_all(destination.parent().ok_or_else(||io::Error::other("Invalid plugin hook resource"))?)?;
                    use std::io::Write;
                    let mut file=std::fs::OpenOptions::new().write(true).create_new(true).open(&destination)?;
                    file.write_all(contents.as_bytes())?;
                    #[cfg(unix)] {
                        use std::os::unix::fs::PermissionsExt;
                        file.set_permissions(std::fs::Permissions::from_mode(0o700))?;
                    }
                }
                Some(dir)
            };
            let mut command = std::process::Command::new("/bin/sh");
            command
                .arg("-c")
                .arg(&hook.command)
                .current_dir(workspace)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("KAGEHA_HOOK_EVENT", &hook.event)
                .env(
                    "KAGEHA_HOOK_PAYLOAD",
                    std::str::from_utf8(&body).map_err(io::Error::other)?,
                )
                .env("KAGEHA_PROJECT_ROOT", workspace);
            if let Some(root)=&root {
                command.env("PLUGIN_ROOT",root.path()).env("CODEX_PLUGIN_ROOT",root.path()).env("CLAUDE_PLUGIN_ROOT",root.path());
                // The package's declared interpreter (e.g. node) is resolved
                // through the operator's normal executable search path only.
                if let Some(path)=std::env::var_os("PATH") {command.env("PATH",path);}
            }
            // Plugin hooks use the standard stdin JSON payload. Existing local
            // project hooks keep their established environment-only contract.
            let input=if hook.config_path.starts_with("/plugin/") {body.clone()} else {vec![]};
            let out = kageha_workspace::process::run(&mut command, input, &dispatched, 65536)?;
            kageha_workspace::hooks::output(&out.bytes, out.exit_code)?
        } else {
            static HTTP: OnceLock<std::result::Result<kageha_integrations::HttpClient, String>> =
                OnceLock::new();
            let http = HTTP
                .get_or_init(|| {
                    kageha_integrations::HttpClient::new(Default::default())
                        .map_err(|e| e.to_string())
                })
                .as_ref()
                .map_err(|e| io::Error::other(e.clone()))?;
            let out = http.request(
                &hook.http,
                "POST",
                &[("Content-Type".into(), "application/json".into())],
                body,
                &dispatched,
                Duration::from_secs_f64(hook.timeout_s),
                65536,
            )?;
            if !(200..300).contains(&out.status) {
                return Err(io::Error::other(
                    "Hook endpoint returned an unsuccessful response after dispatch",
                ));
            }
            kageha_workspace::hooks::output(&out.bytes, None)?
        };
        Ok(ToolReply::success(serde_json::to_string(&outcome)?))
    })?;
    serde_json::from_str(&result.content).map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    fn fixture() -> (tempfile::TempDir, Hook, Journal) {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("hooks.json");
        let raw = json!([{"event":"preToolUse","command":"printf 'effect\\n' >> effects; printf '{\"extra_context\":\"captured\"}'"}]);
        std::fs::write(&path, raw.to_string()).unwrap();
        let hook =
            kageha_workspace::hooks::Set::parse(&raw, &path, &hash(raw.to_string().as_bytes()))
                .unwrap()
                .hooks
                .remove(0);
        let j = Journal::open(&d.path().join("state/hooks.sqlite")).unwrap();
        (d, hook, j)
    }
    #[test]
    fn approved_hook_receipt_reuses_result_without_reexecution() {
        let (d, h, j) = fixture();
        let flag = AtomicBool::new(false);
        let c = Control {
            cancelled: &flag,
            deadline: None,
        };
        let s = ToolContext {
            session: "s".into(),
            turn: "t".into(),
        };
        let p = json!({"tool_call_id":"c"});
        let out = execute(&j, &s, &h, &p, d.path(), &c, |_| Ok(true)).unwrap();
        assert_eq!(out.extra_context, "captured");
        let out = execute(&j, &s, &h, &p, d.path(), &c, |_| {
            panic!("cached hook must not request another decision")
        })
        .unwrap();
        assert_eq!(out.extra_context, "captured");
        assert_eq!(
            std::fs::read_to_string(d.path().join("effects")).unwrap(),
            "effect\n"
        );
    }
    #[test]
    fn captured_host_configuration_stays_immutable_for_the_existing_turn() {
        let (d, h, j) = fixture();
        let flag = AtomicBool::new(false);
        let c = Control {
            cancelled: &flag,
            deadline: None,
        };
        let scope = ToolContext {
            session: "s".into(),
            turn: "t".into(),
        };
        std::fs::write(&h.config_path, "[]").unwrap();
        let out = execute_captured(
            &j,
            &scope,
            &h,
            &json!({"tool_call_id":"c"}),
            d.path(),
            &c,
            |context| {
                assert_eq!(context["hook"]["command"], h.command);
                Ok(true)
            },
        )
        .unwrap();
        assert_eq!(out.extra_context, "captured");
    }
    #[test]
    fn changed_configuration_after_approval_never_dispatches() {
        let (d, h, j) = fixture();
        let flag = AtomicBool::new(false);
        let c = Control {
            cancelled: &flag,
            deadline: None,
        };
        let s = ToolContext {
            session: "s".into(),
            turn: "t".into(),
        };
        assert!(execute(
            &j,
            &s,
            &h,
            &json!({"tool_call_id":"c"}),
            d.path(),
            &c,
            |_| {
                std::fs::write(&h.config_path, "[]")?;
                Ok(true)
            }
        )
        .is_err());
        assert!(!d.path().join("effects").exists());
        assert!(j.receipts(Some("s")).unwrap().is_empty());
    }
}
