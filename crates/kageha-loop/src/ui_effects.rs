//! Dispatch intent is durable before external effects; recovery never blindly replays.
use crate::webui::Database;
use serde_json::{json, Value};
use std::io;
pub(crate) fn run(
    db: &Database,
    turn: &str,
    call: &Value,
    scope: &Value,
    effect: impl FnOnce() -> io::Result<Value>,
) -> io::Result<Value> {
    use sha2::{Digest, Sha256};
    let id = call["id"]
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| io::Error::other("Tool call ID required"))?;
    let digest = format!(
        "{:x}",
        Sha256::digest(
            json!({"name":call["name"],"arguments":call["arguments"],"scope":scope}).to_string()
        )
    );

    use kageha_runtime::lifecycle::RunStore;
    let store = crate::runtime_host::store(db).map_err(|(_, e)| io::Error::other(e))?;
    if let Some(receipt) = store
        .effect_begin(turn, id, &digest)
        .map_err(|error| if error.code==kageha_contracts::ErrorCode::Uncertain {kageha_contracts::execution::uncertain_stop("Earlier dispatch is unverified; stop and inspect its original receipt without replay")}else{io::Error::other(error)})?
    {
        return Ok(receipt);
    }
    let result = effect()?;
    store
        .effect_finish(turn, id, &digest, result.clone())
        .map_err(|_|kageha_contracts::execution::uncertain_stop("Input may have completed but its receipt could not be published. Stop and inspect the original receipt without replay."))?;

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn receipt_publication_failure_preserves_uncertain_outcome() {
        let root = tempfile::tempdir().unwrap();
        let db = crate::agent_host::DatabaseState::open(
            rusqlite::Connection::open(root.path().join("app.sqlite")).unwrap(),
        ).unwrap();
        let call = json!({"id":"one","name":"remote_exec","arguments":{"command":"effect"}});
        let error = run(&db, "turn", &call, &json!({"host":"test"}), || {
            // Simulate storage failing after the external mutation has returned.
            let journal = rusqlite::Connection::open(root.path().join("components/runtime.sqlite")).map_err(io::Error::other)?;
            journal.execute_batch("CREATE TRIGGER fail_publication BEFORE UPDATE ON effects BEGIN SELECT RAISE(FAIL,'publication unavailable'); END;").map_err(io::Error::other)?;
            Ok(json!({"ok":true}))
        }).unwrap_err();
        assert!(kageha_contracts::execution::is_uncertain(&error));
        assert!(kageha_contracts::execution::is_uncertain(
            &run(&db, "turn", &call, &json!({"host":"test"}), || panic!(
                "must not replay dispatched mutation"
            ))
            .unwrap_err()
        ));
    }
    #[test]
    fn uncertain_dispatch_is_never_replayed() {
        let db = crate::agent_host::DatabaseState::open(
            rusqlite::Connection::open_in_memory().unwrap(),
        ).unwrap();
        let call = json!({"id":"one","name":"remote_exec","arguments":{"command":"effect"}});
        let scope = json!({"host":"test"});
        assert!(run(&db, "turn", &call, &scope, || Err(io::Error::other(
            "transport died after dispatch"
        )))
        .is_err());
        let invoked = std::cell::Cell::new(false);
        assert!(run(&db, "turn", &call, &scope, || {
            invoked.set(true);
            Ok(json!({}))
        })
        .is_err());
        assert!(!invoked.get());
    }
    #[test]
    fn completed_effect_reuses_exact_receipt() {
        let db = crate::agent_host::DatabaseState::open(
            rusqlite::Connection::open_in_memory().unwrap(),
        ).unwrap();
        let call = json!({"id":"one","name":"image","arguments":{}});
        assert_eq!(
            run(&db, "turn", &call, &json!({}), || Ok(
                json!({"path":"saved"})
            ))
            .unwrap()["path"],
            "saved"
        );
        assert_eq!(
            run(&db, "turn", &call, &json!({}), || panic!(
                "must not run twice"
            ))
            .unwrap()["path"],
            "saved"
        );
    }
}
