//! Caller-owned storage; no runtime, WebUI, SQLite, browser or provider needed.
use kageha_workflow::planning::*;
use serde_json::json;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
#[derive(Default)]
struct Memory {
    rows: HashMap<(String, String), Record>,
    receipts: HashMap<(String, String, String), (String, Record)>,
}
#[derive(Default)]
struct MemoryStore(Mutex<Memory>);
impl Store for MemoryStore {
    fn load(&self, key: &Key) -> Result<Option<Record>> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .rows
            .get(&(key.session.clone(), key.turn.clone()))
            .cloned())
    }
    fn receipt(&self, key: &Key, op: &Operation) -> Result<Option<Record>> {
        let m = self.0.lock().unwrap();
        let Some((digest, r)) =
            m.receipts
                .get(&(key.session.clone(), key.turn.clone(), op.id.clone()))
        else {
            return Ok(None);
        };
        if digest != &op.digest {
            return Err(failure(Code::Conflict, "Operation changed"));
        }
        Ok(Some(r.clone()))
    }
    fn commit(&self, expected: Option<u64>, next: &Record, op: &Operation) -> Result<Record> {
        let mut m = self.0.lock().unwrap();
        let identity = (
            next.key.session.clone(),
            next.key.turn.clone(),
            op.id.clone(),
        );
        if let Some((digest, r)) = m.receipts.get(&identity) {
            if digest != &op.digest {
                return Err(failure(Code::Conflict, "Operation changed"));
            }
            return Ok(r.clone());
        }
        let key = (next.key.session.clone(), next.key.turn.clone());
        if m.rows.get(&key).map(|r| r.revision) != expected {
            return Err(failure(Code::Conflict, "Revision changed"));
        }
        m.rows.insert(key, next.clone());
        m.receipts
            .insert(identity, (op.digest.clone(), next.clone()));
        Ok(next.clone())
    }
}
struct Evidence;
impl EvidencePort for Evidence {
    fn successful(&self, key: &Key, call: &str) -> Result<bool> {
        Ok(key.session == "framework" && key.turn == "task" && call == "receipt:confirmed")
    }
}
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let service = Service::new(Arc::new(MemoryStore::default()));
    let key = Key {
        session: "framework".into(),
        turn: "task".into(),
    };
    let plan=service.propose(&key,"proposal",None,&json!({"objective":"Deliver evidence","steps":[{"id":"deliver","title":"Deliver evidence","depends_on":[]}],"outcomes":[{"id":"result","text":"Result delivered","required":true}]}))?;
    // The embedding framework obtains the operator decision before this call.
    let approved = service.approve(&key, "decision", plan.revision, &digest(&plan.document))?;
    let args = json!({"expected_revision":approved.revision,"step_id":"deliver","status":"completed","evidence":["receipt:confirmed"],"outcome_ids":["result"]});
    let result = service.update(&key, "progress", &args, &Evidence)?;
    assert!(result.complete());
    assert_eq!(
        service.update(&key, "progress", &args, &Evidence)?.revision,
        result.revision
    );
    println!("Independent plan embedding passed");
    Ok(())
}
