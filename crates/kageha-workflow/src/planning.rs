//! Portable reviewed plans and evidence-backed progress. Storage and evidence
//! come from host-owned ports, never a browser, provider or application database.
mod graph;
mod view;
pub use graph::{validate as validate_graph, Step as GraphStep};
pub use view::{board, markdown};
#[cfg(feature = "sqlite")]
pub mod sqlite;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{fmt, sync::Arc};
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    Invalid,
    Conflict,
    NotFound,
    Unavailable,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Error {
    pub code: Code,
    pub message: String,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(f)
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;
pub fn failure(code: Code, message: impl Into<String>) -> Error {
    Error {
        code,
        message: message.into(),
    }
}
pub fn digest(value: &Value) -> String {
    format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
}
fn text<'a>(v: &'a Value, name: &str, max: usize) -> Result<&'a str> {
    v[name]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= max && !s.contains('\0'))
        .ok_or_else(|| failure(Code::Invalid, format!("Invalid {name}")))
}
pub fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
pub fn validate(value: &Value) -> Result<()> {
    if !value.is_object() || value.to_string().len() > 131072 {
        return Err(failure(Code::Invalid, "Plan is too large or not an object"));
    }
    let steps = value["steps"]
        .as_array()
        .filter(|s| !s.is_empty() && s.len() <= 64)
        .ok_or_else(|| failure(Code::Invalid, "Plan must contain one to 64 steps"))?;
    for step in steps {
        if !identifier(text(step, "id", 64)?) {
            return Err(failure(Code::Invalid, "Invalid step id"));
        }
        text(step, "title", 2000)?;
    }
    let graph: Vec<GraphStep> = serde_json::from_value(json!(steps))
        .map_err(|_| failure(Code::Invalid, "Each step needs depends_on: []"))?;
    validate_graph(&graph).map_err(|e| failure(Code::Invalid, e))?;
    if let Some(outcomes) = value.get("outcomes").filter(|v| !v.is_null()) {
        let rows = outcomes
            .as_array()
            .filter(|v| v.len() <= 64)
            .ok_or_else(|| failure(Code::Invalid, "Invalid plan outcomes"))?;
        let mut ids = std::collections::HashSet::new();
        for outcome in rows {
            let id = text(outcome, "id", 128)?;
            if !identifier(id) || !ids.insert(id) {
                return Err(failure(Code::Invalid, "Outcome ids must be unique"));
            }
            text(outcome, "text", 4000)?;
            if outcome.get("required").is_some_and(|v| !v.is_boolean()) {
                return Err(failure(Code::Invalid, "Outcome required must be boolean"));
            }
        }
    }
    Ok(())
}
pub fn preserve_outcomes(old: &Value, next: &Value) -> Result<()> {
    for outcome in old["outcomes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|o| o["required"] != false)
    {
        if !next["outcomes"].as_array().is_some_and(|v| {
            v.iter().any(|o| {
                o["id"] == outcome["id"] && o["text"] == outcome["text"] && o["required"] != false
            })
        }) {
            return Err(failure(
                Code::Conflict,
                "The revised plan must retain its required outcomes",
            ));
        }
    }
    Ok(())
}
/// Model/user plan drafts cannot self-approve or import completion claims.
pub fn draft(body: &Value) -> Result<Value> {
    validate(body)?;
    let mut document = body.clone();
    for k in ["revision", "approved", "turn_id"] {
        document.as_object_mut().unwrap().remove(k);
    }
    for step in document["steps"].as_array_mut().unwrap() {
        step["status"] = json!("pending");
        step["evidence"] = json!([]);
    }
    for outcome in document
        .get_mut("outcomes")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
    {
        outcome["done"] = json!(false);
        outcome["evidence"] = json!([]);
    }
    validate(&document)?;
    Ok(document)
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Key {
    pub session: String,
    pub turn: String,
}
impl Key {
    pub fn validate(&self) -> Result<()> {
        if [&self.session, &self.turn]
            .iter()
            .any(|s| s.is_empty() || s.len() > 256 || s.chars().any(char::is_control))
        {
            return Err(failure(Code::Invalid, "Invalid plan ownership scope"));
        }
        Ok(())
    }
}
/// True only for a confirmed successful, non-planning tool result in this exact
/// scope. It proves a tool fact; it is not proof of a server-side commitment.
pub trait EvidencePort {
    fn successful(&self, key: &Key, call: &str) -> Result<bool>;
}

/// Replace an unfinished route through an approved plan while retaining its history.
pub fn revise(
    current: &Value,
    key: &Key,
    args: &Value,
    evidence_port: &dyn EvidencePort,
) -> Result<Value> {
    validate(current)?;
    key.validate()?;
    if current["approved"] != true || args["expected_revision"].as_u64().is_none()
        || args["expected_revision"] != current["revision"]
    {
        return Err(failure(Code::Conflict, "Read the current approved plan revision."));
    }
    let id = text(args, "step_id", 64)?;
    let replacement = text(args, "replacement_id", 64)?;
    let title = text(args, "title", 2000)?;
    let reason = text(args, "reason", 1000)?;
    let evidence = text(args, "evidence_id", 256)?;
    if !evidence_port.successful(key, evidence)? {
        return Err(failure(Code::Invalid, "Revision needs a confirmed tool result in this turn."));
    }
    let mut next = current.clone();
    let steps = next["steps"].as_array_mut().unwrap();
    if steps.iter().any(|step| step["id"] == replacement) {
        return Err(failure(Code::Conflict, "Use a new replacement step ID."));
    }
    let old = steps.iter_mut().find(|step| step["id"] == id)
        .ok_or_else(|| failure(Code::NotFound, "Step not found"))?;
    if !matches!(old["status"].as_str(), Some("pending" | "in_progress" | "blocked")) {
        return Err(failure(Code::Conflict, "Only unfinished steps can be replaced."));
    }
    let mut replacement_step = old.clone();
    replacement_step["id"] = json!(replacement);
    replacement_step["title"] = json!(title);
    replacement_step["status"] = json!("pending");
    replacement_step["evidence"] = json!([]);
    old["status"] = json!("superseded");
    old["revision_reason"] = json!(reason);
    old["revision_evidence"] = json!(evidence);
    for step in steps.iter_mut().filter(|step| step["status"] != "completed" && step["status"] != "superseded") {
        for dependency in step["depends_on"].as_array_mut().into_iter().flatten() {
            if dependency == id { *dependency = json!(replacement); }
        }
    }
    steps.push(replacement_step);
    next["notice"] = json!(format!("Plan revised: {reason}"));
    validate(&next)?;
    Ok(next)
}

/// Common progress rules used by the WebUI adapter and independent service.
pub fn progress(
    current: &Value,
    key: &Key,
    args: &Value,
    evidence_port: &dyn EvidencePort,
) -> Result<Value> {
    validate(current)?;
    key.validate()?;
    if current["approved"] != true {
        return Err(failure(Code::Conflict, "No approved plan for this turn"));
    }
    let id = text(args, "step_id", 64)?;
    let status = text(args, "status", 32)?;
    if !matches!(status, "in_progress" | "completed" | "blocked") {
        return Err(failure(Code::Invalid, "Invalid step status"));
    }
    let prior = current["steps"].as_array().unwrap();
    let step = prior
        .iter()
        .find(|s| s["id"] == id)
        .ok_or_else(|| failure(Code::NotFound, "Step not found"))?;
    if step["status"] == "superseded" || (step["status"] == "completed" && status == "completed") {
        return Err(failure(Code::Conflict, "This step is already closed."));
    }
    for dep in step["depends_on"].as_array().into_iter().flatten() {
        if !prior
            .iter()
            .any(|s| s["id"] == *dep && s["status"] == "completed")
        {
            return Err(failure(
                Code::Conflict,
                "Complete this step's dependencies first",
            ));
        }
    }
    let evidence = args
        .get("evidence")
        .map(|v| {
            v.as_array()
                .cloned()
                .ok_or_else(|| failure(Code::Invalid, "Evidence must be a list"))
        })
        .transpose()?
        .unwrap_or_default();
    if evidence.len() > 128 {
        return Err(failure(Code::Invalid, "Too many evidence references"));
    }
    if status == "completed" {
        if evidence.is_empty() {
            return Err(failure(
                Code::Invalid,
                "Completed steps need successful tool-call ids as evidence",
            ));
        }
        for call in &evidence {
            let id = call
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 256)
                .ok_or_else(|| failure(Code::Invalid, "Invalid evidence reference"))?;
            if !evidence_port.successful(key, id)? {
                return Err(failure(
                    Code::Invalid,
                    "Evidence must refer to a successful tool result in this turn",
                ));
            }
        }
    }
    let mut next = current.clone();
    let item = next["steps"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|s| s["id"] == id)
        .unwrap();
    item["status"] = json!(status);
    item["evidence"] = json!(evidence);
    if let Some(ids) = args.get("outcome_ids") {
        let ids = ids
            .as_array()
            .filter(|ids| ids.len() <= 64)
            .ok_or_else(|| failure(Code::Invalid, "Invalid outcome references"))?;
        if !ids.is_empty() && status != "completed" {
            return Err(failure(
                Code::Invalid,
                "Only completed steps can satisfy outcomes",
            ));
        }
        for id in ids {
            let row = next["outcomes"]
                .as_array_mut()
                .and_then(|rows| rows.iter_mut().find(|r| r["id"] == *id))
                .ok_or_else(|| failure(Code::Invalid, "Unknown outcome reference"))?;
            row["done"] = json!(true);
            row["evidence"] = json!(evidence);
        }
    }
    // Completion is invalidated if any supporting step is subsequently reopened.
    if status != "completed" {
        for outcome in next
            .get_mut("outcomes")
            .and_then(Value::as_array_mut)
            .into_iter()
            .flatten()
        {
            if outcome["evidence"].as_array().is_some_and(|refs| {
                step["evidence"]
                    .as_array()
                    .is_some_and(|old| refs.iter().any(|r| old.contains(r)))
            }) {
                outcome["done"] = json!(false);
                outcome["evidence"] = json!([]);
            }
        }
    }
    validate(&next)?;
    Ok(next)
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    pub key: Key,
    pub revision: u64,
    pub approved: bool,
    pub document: Value,
}
impl Record {
    pub fn value(&self) -> Value {
        let mut v = self.document.clone();
        v["revision"] = json!(self.revision);
        v["approved"] = json!(self.approved);
        v["turn_id"] = json!(self.key.turn);
        v
    }
    pub fn complete(&self) -> bool {
        self.approved && board(&self.value())["complete"] == true
    }
}
#[derive(Clone, Debug)]
pub struct Operation {
    pub id: String,
    pub digest: String,
}
/// Commit must atomically compare revisions and persist an immutable operation
/// receipt. Exact retries return the old receipt; changed reuse must fail.
pub trait Store: Send + Sync {
    fn load(&self, key: &Key) -> Result<Option<Record>>;
    fn receipt(&self, key: &Key, operation: &Operation) -> Result<Option<Record>>;
    fn commit(&self, expected: Option<u64>, next: &Record, operation: &Operation)
        -> Result<Record>;
}
fn next_revision(value: u64) -> Result<u64> {
    value
        .checked_add(1)
        .ok_or_else(|| failure(Code::Invalid, "Plan revision overflow"))
}
pub struct Service {
    store: Arc<dyn Store>,
}
impl Service {
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self { store }
    }
    fn checked(key: &Key, record: Record) -> Result<Record> {
        if &record.key != key || record.revision == 0 {
            return Err(failure(
                Code::Conflict,
                "Planning adapter returned another scope or invalid revision",
            ));
        }
        validate(&record.document)?;
        Ok(record)
    }
    pub fn load(&self, key: &Key) -> Result<Option<Record>> {
        key.validate()?;
        self.store
            .load(key)?
            .map(|r| Self::checked(key, r))
            .transpose()
    }
    fn cached(&self, key: &Key, operation: &Operation) -> Result<Option<Record>> {
        self.store
            .receipt(key, operation)?
            .map(|r| Self::checked(key, r))
            .transpose()
    }
    fn commit(
        &self,
        expected: Option<u64>,
        next: &Record,
        operation: &Operation,
    ) -> Result<Record> {
        Self::checked(&next.key, next.clone())?;
        let actual = Self::checked(&next.key, self.store.commit(expected, next, operation)?)?;
        if actual.revision != next.revision
            || actual.approved != next.approved
            || actual.document != next.document
        {
            return Err(failure(
                Code::Conflict,
                "Planning adapter returned a different committed record",
            ));
        }
        Ok(actual)
    }
    fn operation(key: &Key, id: &str, command: &Value) -> Result<Operation> {
        key.validate()?;
        if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
            return Err(failure(Code::Invalid, "Invalid plan operation identity"));
        }
        Ok(Operation {
            id: id.into(),
            digest: digest(command),
        })
    }
    pub fn propose(
        &self,
        key: &Key,
        id: &str,
        expected: Option<u64>,
        body: &Value,
    ) -> Result<Record> {
        let op = Self::operation(key, id, &json!(["propose", expected, body]))?;
        if let Some(r) = self.cached(key, &op)? {
            return Ok(r);
        }
        validate(body)?;
        let old = self.load(key)?;
        if old.as_ref().map(|r| r.revision) != expected {
            return Err(failure(
                Code::Conflict,
                "Plan changed; read its current revision",
            ));
        }
        if let Some(old) = &old {
            if old.approved {
                return Err(failure(Code::Conflict, "Design is locked after approval"));
            }
            preserve_outcomes(&old.document, body)?;
        }
        let document = draft(body)?;
        self.commit(
            expected,
            &Record {
                key: key.clone(),
                revision: next_revision(expected.unwrap_or(0))?,
                approved: false,
                document,
            },
            &op,
        )
    }
    pub fn approve(
        &self,
        key: &Key,
        id: &str,
        revision: u64,
        document_digest: &str,
    ) -> Result<Record> {
        let op = Self::operation(key, id, &json!(["approve", revision, document_digest]))?;
        if let Some(r) = self.cached(key, &op)? {
            return Ok(r);
        }
        let mut r = self
            .load(key)?
            .ok_or_else(|| failure(Code::NotFound, "No plan has been created"))?;
        if r.revision != revision || digest(&r.document) != document_digest || r.approved {
            return Err(failure(
                Code::Conflict,
                "Plan approval no longer matches the current design",
            ));
        }
        r.revision = next_revision(r.revision)?;
        r.approved = true;
        self.store.commit(Some(revision), &r, &op)
    }
    pub fn update(
        &self,
        key: &Key,
        id: &str,
        args: &Value,
        evidence: &dyn EvidencePort,
    ) -> Result<Record> {
        let op = Self::operation(key, id, &json!(["update", args]))?;
        if let Some(r) = self.cached(key, &op)? {
            return Ok(r);
        }
        let prior = self
            .load(key)?
            .ok_or_else(|| failure(Code::NotFound, "No plan has been created"))?;
        if args["expected_revision"].as_u64() != Some(prior.revision) {
            return Err(failure(
                Code::Conflict,
                "Plan changed; read its current revision",
            ));
        }
        let next = progress(&prior.value(), key, args, evidence)?;
        let mut record = prior.clone();
        record.document = next;
        record.revision = next_revision(record.revision)?;
        self.store.commit(Some(prior.revision), &record, &op)
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod tests {
    use super::*;
    struct Evidence;
    impl EvidencePort for Evidence {
        fn successful(&self, key: &Key, call: &str) -> Result<bool> {
            Ok(key.session == "chat" && key.turn == "turn" && call == "confirmed")
        }
    }
    fn body() -> Value {
        json!({"objective":"Deliver reviewed result","steps":[{"id":"read","title":"Read","depends_on":[]},{"id":"deliver","title":"Deliver","depends_on":["read"]}],"outcomes":[{"id":"required","text":"Deliver result","required":true}]})
    }
    fn key() -> Key {
        Key {
            session: "chat".into(),
            turn: "turn".into(),
        }
    }
    #[test]
    fn revision_keeps_verified_work_and_redirects_pending_dependencies() {
        let mut plan = draft(&body()).unwrap();
        plan["approved"] = json!(true);
        plan["revision"] = json!(3);
        plan["steps"][0]["status"] = json!("completed");
        plan["steps"][0]["evidence"] = json!(["confirmed"]);
        plan["steps"].as_array_mut().unwrap().push(json!({
            "id":"verify","title":"Verify delivery","depends_on":["deliver"],"status":"pending"
        }));
        let mut args = json!({"step_id":"deliver","replacement_id":"alternate","title":"Deliver through alternate channel",
            "reason":"The original route is unavailable","evidence_id":"confirmed","expected_revision":3});
        let revised = revise(&plan, &key(), &args, &Evidence).unwrap();
        assert_eq!(revised["steps"][0], plan["steps"][0]);
        assert_eq!(revised["outcomes"], plan["outcomes"]);
        assert_eq!(revised["steps"][1]["status"], "superseded");
        assert_eq!(revised["steps"][2]["depends_on"], json!(["alternate"]));
        assert_eq!(board(&revised)["total"], 3);
        args["expected_revision"] = json!(2);
        assert!(revise(&plan, &key(), &args, &Evidence).is_err());
        args["expected_revision"] = json!(3);
        args["step_id"] = json!("read");
        assert!(revise(&plan, &key(), &args, &Evidence).is_err());
    }
    #[test]
    fn plan_revisions_required_outcomes_and_evidence_survive_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plans.sqlite");
        {
            let service = Service::new(Arc::new(sqlite::SqliteStore::open(&path).unwrap()));
            let mut forged = body();
            forged["approved"] = json!(true);
            forged["steps"][0]["status"] = json!("completed");
            forged["outcomes"][0]["done"] = json!(true);
            let first = service.propose(&key(), "propose", None, &forged).unwrap();
            assert!(!first.approved);
            assert_eq!(first.document["steps"][0]["status"], "pending");
            assert!(!first.complete());
            assert_eq!(first.document["outcomes"][0]["done"], false);
            assert_eq!(
                service
                    .propose(&key(), "propose", None, &forged)
                    .unwrap()
                    .revision,
                1
            );
            assert!(service.propose(&key(), "propose", None, &body()).is_err());
            assert!(service
                .propose(
                    &key(),
                    "drop",
                    Some(1),
                    &json!({"steps":[{"id":"try","title":"Try","depends_on":[]}]})
                )
                .is_err());
            assert!(service.approve(&key(), "wrong", 1, "changed").is_err());
            let approved = service
                .approve(&key(), "approve", 1, &digest(&first.document))
                .unwrap();
            assert_eq!(approved.revision, 2);
            assert!(approved.approved);
            assert!(service
                .propose(&key(), "rewrite", Some(2), &body())
                .is_err());
            for args in [
                json!({"expected_revision":2,"step_id":"deliver","status":"completed","evidence":["confirmed"]}),
                json!({"expected_revision":2,"step_id":"read","status":"completed","evidence":[]}),
                json!({"expected_revision":2,"step_id":"read","status":"completed","evidence":["invented"]}),
            ] {
                assert!(service.update(&key(), "invalid", &args, &Evidence).is_err());
            }
            let args = json!({"expected_revision":2,"step_id":"read","status":"completed","evidence":["confirmed"]});
            assert_eq!(
                service
                    .update(&key(), "read", &args, &Evidence)
                    .unwrap()
                    .revision,
                3
            );
            assert_eq!(
                service
                    .update(&key(), "read", &args, &Evidence)
                    .unwrap()
                    .revision,
                3
            );
            assert!(service.update(&key(), "stale", &args, &Evidence).is_err());
            let done=service.update(&key(),"deliver",&json!({"expected_revision":3,"step_id":"deliver","status":"completed","evidence":["confirmed"],"outcome_ids":["required"]}),&Evidence).unwrap();
            assert!(done.complete());
            assert!(service
                .store
                .load(&Key {
                    session: "other".into(),
                    turn: "turn".into()
                })
                .unwrap()
                .is_none());
        }
        let service = Service::new(Arc::new(sqlite::SqliteStore::open(&path).unwrap()));
        let record = service.load(&key()).unwrap().unwrap();
        assert!(record.complete());
        assert_eq!(record.revision, 4);
        let reopened = service
            .update(
                &key(),
                "reopen",
                &json!({"expected_revision":4,"step_id":"read","status":"blocked"}),
                &Evidence,
            )
            .unwrap();
        assert!(!reopened.complete());
        assert_eq!(reopened.document["outcomes"][0]["done"], false);
    }
    #[test]
    fn plan_store_rejects_second_writers_foreign_tables_and_symlinks() {
        use std::os::unix::fs::symlink;
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("plans.sqlite");
        let store = sqlite::SqliteStore::open(&path).unwrap();
        assert!(sqlite::SqliteStore::open(&path).is_err());
        drop(store);
        let link = d.path().join("link.sqlite");
        symlink(&path, &link).unwrap();
        assert!(sqlite::SqliteStore::open(&link).is_err());
        let foreign = d.path().join("foreign.sqlite");
        let file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&foreign)
            .unwrap();
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .unwrap();
        drop(file);
        let db = rusqlite::Connection::open(&foreign).unwrap();
        db.execute_batch("CREATE TABLE application_owned(id TEXT);")
            .unwrap();
        drop(db);
        assert!(sqlite::SqliteStore::open(&foreign).is_err());
    }
}

#[cfg(test)]
mod port_tests {
    use super::*;
    struct WrongScope;
    impl Store for WrongScope {
        fn load(&self, _: &Key) -> Result<Option<Record>> {
            Ok(Some(Record {
                key: Key {
                    session: "foreign".into(),
                    turn: "foreign".into(),
                },
                revision: 1,
                approved: false,
                document: json!({"steps":[{"id":"s","title":"Secret","depends_on":[]}]}),
            }))
        }
        fn receipt(&self, _: &Key, _: &Operation) -> Result<Option<Record>> {
            Ok(None)
        }
        fn commit(&self, _: Option<u64>, _: &Record, _: &Operation) -> Result<Record> {
            panic!("Invalid scope must never reach commit")
        }
    }
    #[test]
    fn incompatible_store_scope_is_rejected_before_plan_admission() {
        let service = Service::new(Arc::new(WrongScope));
        let key = Key {
            session: "caller".into(),
            turn: "task".into(),
        };
        assert!(service.load(&key).is_err());
        assert!(service
            .propose(
                &key,
                "p",
                Some(1),
                &json!({"steps":[{"id":"s","title":"Work","depends_on":[]}]})
            )
            .is_err());
    }
    #[test]
    fn normalized_drafts_cannot_overflow_the_portable_record_limit() {
        let mut body = json!({"steps":[{"id":"s","title":"Work","depends_on":[]}],"padding":""});
        body["padding"] = json!("x".repeat(131072 - body.to_string().len()));
        assert!(validate(&body).is_ok());
        assert!(draft(&body).is_err());
    }
    #[test]
    fn absent_and_legacy_null_outcomes_remain_compatible() {
        for body in [
            json!({"steps":[{"id":"s","title":"Work","depends_on":[]}]}),
            json!({"steps":[{"id":"s","title":"Work","depends_on":[]}],"outcomes":null}),
        ] {
            assert!(validate(&body).is_ok());
            assert!(draft(&body).is_ok());
        }
    }
}
