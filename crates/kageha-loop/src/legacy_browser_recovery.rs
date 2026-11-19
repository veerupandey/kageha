//! Migration adapter for the existing Kageha event store. Browser readback runs
//! in the standalone Rust browser; this host adapter owns only the legacy projection.
//! Never deletes history, replays input, or retires clicks, keys, uploads or submits.
use crate::browser_client::{self, Client};
use fs2::FileExt;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::{self, Read},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn error(message: &str) -> Box<dyn std::error::Error> {
    Box::new(io::Error::other(message))
}
fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn private_json(path: &Path) -> Result<Value> {
    browser_client::private(path)?;
    let mut bytes = Vec::new();
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 16 * 1024 * 1024 {
        return Err(error("Recovery evidence exceeds limit"));
    }
    serde_json::from_slice(&bytes).map_err(|_| error("Recovery evidence is invalid"))
}
// Compatibility with Python's sorted, Unicode JSON execution-identity hash.
// Do not trust a masked tool-card preview as the original input value.
fn canonical(value: &Value) -> String {
    match value {
        Value::Object(object) => {
            let mut keys: Vec<_> = object.keys().collect();
            keys.sort();
            format!(
                "{{{}}}",
                keys.into_iter()
                    .map(|k| format!(
                        "{}: {}",
                        serde_json::to_string(k).unwrap(),
                        canonical(&object[k])
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        Value::Array(values) => format!(
            "[{}]",
            values.iter().map(canonical).collect::<Vec<_>>().join(", ")
        ),
        _ => value.to_string(),
    }
}
fn latest(db: &Connection, session: &str, id: Option<&str>) -> Result<Vec<Value>> {
    let mut q=db.prepare("WITH stages AS (SELECT payload_json,row_number() OVER(PARTITION BY json_extract(payload_json,'$.receipt.id') ORDER BY rowid DESC) latest FROM events WHERE session_id=? AND json_extract(payload_json,'$.source_kind')='browser_action_receipt') SELECT json_extract(payload_json,'$.receipt') FROM stages WHERE latest=1 AND (? IS NULL OR json_extract(payload_json,'$.receipt.id')=?)")?;
    let rows = q.query_map(params![session, id, id], |r| r.get::<_, String>(0))?;
    rows.map(|r| serde_json::from_str(&r?).map_err(|_| error("Invalid browser receipt")))
        .collect()
}
fn pending(row: &Value) -> bool {
    row["action"] == "fill"
        && row["backend"] == "rust"
        && matches!(
            row["state"].as_str(),
            Some("dispatching" | "returned" | "uncertain")
        )
}
fn source(
    db: &Connection,
    root: &Path,
    session: &str,
    row: &Value,
) -> Result<Option<(String, Option<String>, String)>> {
    let attempt = row["attempt_id"]
        .as_str()
        .ok_or_else(|| error("Missing attempt identity"))?;
    let data:Option<(String,String,String,String,String,String)>=db.query_row("SELECT turn_id,tool_call_id,tool_name,arguments_hash,state,arguments_json FROM tool_attempts WHERE id=? AND session_id=?",params![attempt,session],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional()?;
    let Some((turn, call, name, expected_hash, state, stored)) = data else {
        return Ok(None);
    };
    if !matches!(state.as_str(), "uncertain" | "completed" | "failed")
        || !matches!(name.as_str(), "browser_fill" | "browser_batch")
    {
        return Ok(None);
    }
    if !crate::server::identifier(&turn) {
        return Err(error("Invalid source turn"));
    }
    let home = root
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| error("Invalid legacy database path"))?;
    let checkpoint = private_json(
        &home
            .join("sessions")
            .join(session)
            .join("checkpoints")
            .join(format!("rust-{turn}.json")),
    )?;
    let args = checkpoint["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|m| m["tool_calls"].as_array().into_iter().flatten())
        .find(|c| c["id"] == call && c["name"] == name)
        .map(|c| c["arguments"].clone())
        .ok_or_else(|| error("Original approved input is unavailable"))?;
    let args = if let Some(s) = args.as_str() {
        serde_json::from_str(s).map_err(|_| error("Invalid source arguments"))?
    } else {
        args
    };
    let intent_recovered = hash(canonical(&args).as_bytes()) == expected_hash;
    if !intent_recovered {
        let stored: Value = serde_json::from_str(&stored)?;
        if args != stored {
            return Err(error("Checkpoint and stored input identities differ"));
        }
    }
    let input = if name == "browser_batch" {
        let steps: Value = serde_json::from_str(
            args["actions_json"]
                .as_str()
                .ok_or_else(|| error("Missing batch arguments"))?,
        )
        .map_err(|_| error("Invalid batch arguments"))?;
        let index = row["step"]
            .as_u64()
            .ok_or_else(|| error("Missing batch step"))? as usize;
        steps
            .as_array()
            .and_then(|s| s.get(index))
            .filter(|s| s["action"] == "fill")
            .cloned()
            .ok_or_else(|| error("The original step is not a field fill"))?
    } else {
        args
    };
    if !input["frame_id"].is_null() {
        return Ok(None);
    }
    let target = input["target"]
        .as_str()
        .ok_or_else(|| error("Missing original target"))?;
    let text = input["text"]
        .as_str()
        .ok_or_else(|| error("Missing original value"))?;
    if target.is_empty()
        || target.len() > 512
        || text.len() > 16384
        || text.contains('\0')
        || text.contains("[REDACTED]")
        || target
            .strip_prefix('e')
            .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
    {
        return Ok(None);
    }
    if !intent_recovered && text != "***" {
        return Err(error("Original input identity does not match its journal"));
    }
    Ok(Some((
        target.into(),
        if intent_recovered {
            Some(text.into())
        } else {
            None
        },
        expected_hash,
    )))
}
fn lease_signature(path: &Path, session: &str) -> Result<Option<(String, f64, i64)>> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o022 != 0 {
        return Err(error("Unsafe legacy authority store"));
    }
    let db = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    Ok(db.query_row("SELECT owner,expires,COALESCE(generation,0) FROM leases LEFT JOIN lease_generations USING(session,capability) WHERE session=? AND capability='browser' AND expires>?",params![session,now()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?)
}
fn append(
    db: &mut Connection,
    session: &str,
    before: &Value,
    matches: bool,
    proof: &Value,
) -> Result<bool> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let id = before["id"]
        .as_str()
        .ok_or_else(|| error("Missing receipt ID"))?;
    if latest(&tx, session, Some(id))?.first() != Some(before) {
        return Ok(false);
    }
    let active:Option<(String,String)>=tx.query_row("SELECT id,snapshot_json FROM turns WHERE session_id=? AND status='running' ORDER BY created_at DESC LIMIT 1",[session],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let Some((turn, snapshot)) = active else {
        return Ok(false);
    };
    let mut saved: Value =
        serde_json::from_str(&snapshot).map_err(|_| error("Invalid active turn checkpoint"))?;
    let typed: crate::Snapshot = serde_json::from_value(saved.clone())
        .map_err(|_| error("Unsupported legacy checkpoint"))?;
    if typed.phase.terminal() {
        return Ok(false);
    }
    let mut row = before.clone();
    row["sequence"] = json!(
        before["sequence"]
            .as_u64()
            .ok_or_else(|| error("Invalid receipt sequence"))?
            + 1
    );
    row["state"] = json!(if matches { "verified" } else { "reconciled" });
    row["verification"] = json!(if matches {
        "value_readback"
    } else {
        if proof["intent_recovered"] == false {
            "current_field_observed"
        } else {
            "current_value_differs"
        }
    });
    if matches {
        row["rust_receipt"]["state"] = json!("verified");
    }
    // For a differing field, retain the original Rust dispatch uncertainty.
    // Only the host's convergent-input hold is retired. No success is invented.
    row["recovery_proof"] = proof.clone();
    let payload = json!({"source_kind":"browser_action_receipt","receipt":row});
    let event = crate::Event {
        session_id: session.into(),
        turn_id: turn.clone(),
        sequence: typed.last_sequence + 1,
        kind: "progress".into(),
        payload: payload.as_object().unwrap().clone(),
        created_at: now(),
    };
    let mut reduced = Some(typed);
    crate::reduce(&mut reduced, &event, event.created_at)
        .map_err(|_| error("Invalid recovery event transition"))?;
    let reduced = reduced.unwrap();
    saved["last_sequence"] = json!(reduced.last_sequence);
    saved["updated_at"] = json!(reduced.updated_at);
    let idem = format!("browser-receipt:{id}:{}", row["sequence"].as_u64().unwrap());
    tx.execute("INSERT INTO events(id,session_id,turn_id,sequence,kind,payload_json,created_at,idempotency_key) VALUES (?,?,?,?,?,?,?,?)",params![browser_client::unique_id(),session,turn,event.sequence,event.kind,payload.to_string(),event.created_at,idem])?;
    tx.execute(
        "UPDATE turns SET snapshot_json=?,updated_at=? WHERE id=? AND session_id=?",
        params![saved.to_string(), event.created_at, turn, session],
    )?;
    let checkpoint = hash(format!("{}:{}", turn, event.sequence).as_bytes())[..32].to_owned();
    tx.execute("INSERT OR REPLACE INTO checkpoints(id,session_id,turn_id,sequence,snapshot_json,artifact_path,created_at) VALUES (?,?,?,?,?,'',?)",params![checkpoint,session,turn,event.sequence,saved.to_string(),event.created_at])?;
    tx.commit()?;
    Ok(true)
}

pub fn recover(root: &Path) -> Result<Vec<Value>> {
    browser_client::private(root)?;
    let binding = private_json(&root.join("application-binding.json"))?;
    let session = binding["session_id"]
        .as_str()
        .filter(|s| crate::server::identifier(s))
        .ok_or_else(|| error("Invalid application binding"))?;
    let database = PathBuf::from(
        binding["database"]
            .as_str()
            .ok_or_else(|| error("Missing application database"))?,
    );
    browser_client::private(&database)?;
    let meta = fs::symlink_metadata(&database)?;
    if binding["device"].as_u64() != Some(meta.dev())
        || binding["inode"].as_u64() != Some(meta.ino())
    {
        return Err(error("Application database identity changed"));
    }
    // Ordinary polling must never compete with the agent's input lease.
    let mut db = Connection::open(&database)?;
    db.busy_timeout(Duration::from_secs(2))?;
    let active: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM turns WHERE session_id=? AND status='running')",
        [session],
        |r| r.get(0),
    )?;
    if !active {
        return Ok(Vec::new());
    }
    let candidates: Vec<_> = latest(&db, session, None)?
        .into_iter()
        .filter(pending)
        .take(8)
        .collect();
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let authority = PathBuf::from(
        binding["authority_dir"]
            .as_str()
            .ok_or_else(|| error("Missing authority directory"))?,
    );
    let fence = authority
        .join("browser-dispatch")
        .join(format!("{}.lock", hash(session.as_bytes())));
    browser_client::private(&fence)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&fence)?;
    if lock.try_lock_exclusive().is_err() {
        return Ok(Vec::new());
    }
    let lease = authority.join("capability-leases.sqlite");
    if lease_signature(&lease, session)?.is_some() {
        return Ok(Vec::new());
    }
    let mut results = Vec::new();
    for before in candidates {
        let Some((target, text, intent_hash)) = source(&db, &database, session, &before)? else {
            continue;
        };
        let receipt = &before["rust_receipt"];
        if !receipt["frame_id"].is_null() || !receipt["frame_document_id"].is_null() {
            continue;
        }
        let mut client = Client::connect_readonly(root, session)?;
        let selection = client.selection()?;
        if selection["target_id"] != receipt["target_id"] {
            continue;
        }
        let snapshot=client.call("observe_tab",json!({"browser_id":selection["browser_id"],"target_id":selection["target_id"],"compact":false}),None)?;
        if snapshot["frames"][snapshot["main_frame_id"].as_str().unwrap_or("")]
            != receipt["document_id"]
        {
            continue;
        }
        let salt = uuid::Uuid::new_v4().simple().to_string();
        let intent_recovered = text.is_some();
        let value_digest = if let Some(mut text) = text {
            let digest = hash(&serde_json::to_vec(&(salt.as_str(), text.as_str()))?);
            use zeroize::Zeroize;
            text.zeroize();
            digest
        } else {
            "0".repeat(64)
        };
        let probe=client.call("query_tab",json!({"selection":browser_client::target(&selection),"selector":target,
            "value_probe":{"salt":salt,"value_digest":value_digest,"document_id":receipt["document_id"]}}),None)?;
        if client.selection()?["binding_id"] != selection["binding_id"]
            || lease_signature(&lease, session)?.is_some()
        {
            continue;
        }
        let matched = intent_recovered
            && probe["value_probe"]["value_matches"]
                .as_bool()
                .ok_or_else(|| error("Missing field readback proof"))?;
        let proof = json!({"kind":"convergent_field_readback","input_sent":false,"postcondition_matches":matched,
            "intent_digest":intent_hash,"intent_recovered":intent_recovered,"value_digest":value_digest,"salt":salt,
            "observation_id":snapshot["observation_id"],"target_id":selection["target_id"],
            "document_id":probe["value_probe"]["document_id"],"backend_node_id":probe["value_probe"]["backend_node_id"],
            "previous_epoch":receipt["service_epoch"],"observed_epoch":client.epoch,
            "previous_browser_id":receipt["browser_id"],"observed_browser_id":selection["browser_id"],"observed_at":now()});
        if append(&mut db, session, &before, matched, &proof)? {
            results.push(json!({"receipt_id":before["id"],"state":if matched{"verified"}else{"reconciled"},"postcondition_matches":matched,"input_sent":false}));
        }
    }
    Ok(results)
}
pub fn run(root: &Path, watch: bool) -> Result<()> {
    let lock_path = root.join("legacy-recovery.lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&lock_path)?;
    browser_client::private(&lock_path)?;
    lock.try_lock_exclusive()?;
    loop {
        match recover(root) {
            Ok(rows) => {
                if !rows.is_empty() {
                    println!("{}", json!({"recovered":rows}));
                }
            }
            Err(e) if !watch => return Err(e),
            Err(_) => {}
        }
        if !watch {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn polling_without_pending_fields_never_acquires_the_dispatch_fence() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.path().join("runtime.db");
        let db = Connection::open(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        db.execute_batch("CREATE TABLE turns(session_id TEXT,status TEXT); INSERT INTO turns VALUES ('chat','running'); CREATE TABLE events(session_id TEXT,payload_json TEXT);").unwrap();
        let metadata = fs::metadata(&path).unwrap();
        let binding = dir.path().join("application-binding.json");
        fs::write(&binding,json!({"session_id":"chat","database":path,"device":metadata.dev(),"inode":metadata.ino(),"authority_dir":dir.path().join("missing-authority")}).to_string()).unwrap();
        fs::set_permissions(&binding, fs::Permissions::from_mode(0o600)).unwrap();
        // The fence and authority database intentionally do not exist.
        assert!(recover(dir.path()).unwrap().is_empty());
    }
    #[test]
    fn masked_legacy_evidence_can_only_retire_a_field_hold_not_verify_intent() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let checkpoint = dir.path().join("sessions/chat/checkpoints");
        fs::create_dir_all(&checkpoint).unwrap();
        let file = checkpoint.join("rust-turn.json");
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE tool_attempts(id TEXT,session_id TEXT,turn_id TEXT,tool_call_id TEXT,tool_name TEXT,arguments_hash TEXT,state TEXT,arguments_json TEXT)").unwrap();
        let args = json!({"target":"#city","text":"***"});
        db.execute("INSERT INTO tool_attempts VALUES ('attempt','chat','turn','call','browser_fill','original-private-hash','uncertain',?)",[args.to_string()]).unwrap();
        fs::write(&file,json!({"messages":[{"tool_calls":[{"id":"call","name":"browser_fill","arguments":args}]}]}).to_string()).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        let row = json!({"attempt_id":"attempt"});
        let result = source(&db, &dir.path().join("runtime/runtime.db"), "chat", &row)
            .unwrap()
            .unwrap();
        assert_eq!(result.0, "#city");
        assert!(result.1.is_none());
        let tampered = json!({"target":"#other","text":"***"});
        fs::write(&file,json!({"messages":[{"tool_calls":[{"id":"call","name":"browser_fill","arguments":tampered}]}]}).to_string()).unwrap();
        assert!(source(&db, &dir.path().join("runtime/runtime.db"), "chat", &row).is_err());
    }
    #[test]
    fn readback_appends_once_preserves_goals_and_never_invents_completion() {
        for matched in [false, true] {
            let mut db = Connection::open_in_memory().unwrap();
            db.execute_batch("CREATE TABLE events(id TEXT,session_id TEXT,turn_id TEXT,sequence INTEGER,kind TEXT,payload_json TEXT,created_at REAL,idempotency_key TEXT); CREATE TABLE turns(id TEXT,session_id TEXT,status TEXT,created_at REAL,snapshot_json TEXT,updated_at REAL); CREATE TABLE checkpoints(id TEXT PRIMARY KEY,session_id TEXT,turn_id TEXT,sequence INTEGER,snapshot_json TEXT,artifact_path TEXT,created_at REAL);").unwrap();
            let before = json!({"id":"receipt","action":"fill","backend":"rust","state":"uncertain","sequence":1,"rust_receipt":{"state":"dispatching"}});
            let saved = json!({"session_id":"chat","turn_id":"turn","phase":"executing","last_sequence":1,"goals":[{"id":"preserve-draft"}],"task_state":{"required_outcomes":["review","do-not-submit"]},"extension":{"keep":true}});
            db.execute(
                "INSERT INTO turns VALUES ('turn','chat','running',0,?,0)",
                [saved.to_string()],
            )
            .unwrap();
            db.execute(
                "INSERT INTO events VALUES ('old','chat','turn',1,'progress',?,0,'original')",
                [json!({"source_kind":"browser_action_receipt","receipt":before}).to_string()],
            )
            .unwrap();
            let proof = json!({"kind":"convergent_field_readback","input_sent":false,"postcondition_matches":matched});
            assert!(append(&mut db, "chat", &before, matched, &proof).unwrap());
            assert!(!append(&mut db, "chat", &before, matched, &proof).unwrap());
            let current = latest(&db, "chat", Some("receipt")).unwrap().pop().unwrap();
            assert_eq!(
                current["state"],
                if matched { "verified" } else { "reconciled" }
            );
            assert_eq!(
                current["rust_receipt"]["state"],
                if matched { "verified" } else { "dispatching" }
            );
            let current: String = db
                .query_row("SELECT snapshot_json FROM turns", [], |r| r.get(0))
                .unwrap();
            let current: Value = serde_json::from_str(&current).unwrap();
            for key in ["goals", "task_state", "extension", "phase"] {
                assert_eq!(current[key], saved[key]);
            }
            assert_eq!(
                db.query_row("SELECT count(*) FROM events", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                2
            );
            db.execute("UPDATE turns SET status='completed'", [])
                .unwrap();
            let terminal = latest(&db, "chat", Some("receipt")).unwrap();
            assert!(!append(&mut db, "chat", &terminal[0], matched, &proof).unwrap());
        }
    }
    #[test]
    fn identity_is_python_compatible_without_revealing_values() {
        assert_eq!(
            canonical(&json!({"z":[true,"é"],"a":8000})),
            "{\"a\": 8000, \"z\": [true, \"é\"]}"
        );
        assert!(!pending(
            &json!({"backend":"rust","action":"click","state":"dispatching"})
        ));
        assert!(!pending(
            &json!({"backend":"rust","action":"fill","state":"verified"})
        ));
        assert!(pending(
            &json!({"backend":"rust","action":"fill","state":"dispatching"})
        ));
    }
}
