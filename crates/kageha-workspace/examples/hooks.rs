//! Framework-independent hook composition. The host supplies authorization and effects.
use kageha_contracts::execution::Control;
use kageha_workspace::hooks::{Executor, Hook, Outcome, Set};
use serde_json::{json, Value};
use std::{io, path::Path, sync::atomic::AtomicBool};
struct ApprovedService;
impl Executor for ApprovedService {
    fn execute(
        &self,
        _hook: &Hook,
        _payload: &Value,
        control: &Control<'_>,
    ) -> io::Result<Outcome> {
        control.check()?;
        // Real hosts authorize, journal dispatch, call a process/service, and return a receipt.
        Ok(Outcome {
            extra_context: "Use the retained project conventions.".into(),
            ..Outcome::default()
        })
    }
}
fn main() -> io::Result<()> {
    let set = Set::parse(
        &json!({"hooks":{"PreToolUse":[{"command":"host-owned-adapter","matcher":"read_*"}]}}),
        Path::new("/configured/hooks.json"),
        "example",
    )?;
    let cancelled = AtomicBool::new(false);
    let outcome = set.run(
        "preToolUse",
        "read_file",
        &json!({"arguments":{"path":"notes.md"}}),
        &ApprovedService,
        &Control {
            cancelled: &cancelled,
            deadline: None,
        },
    )?;
    assert_eq!(outcome.ran, 1);
    assert!(outcome.allowed);
    println!("Independent workspace hook composition passed");
    Ok(())
}
