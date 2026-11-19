//! Versioned native WebUI documents. Schema additions never rewrite chat rows.
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};
pub(crate) type ApiResult<T> = Result<T, (u16, String)>;
pub(crate) fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
pub(crate) fn initialize(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS ui_documents(kind TEXT NOT NULL,id TEXT NOT NULL,body TEXT NOT NULL,revision INTEGER NOT NULL,updated REAL NOT NULL,PRIMARY KEY(kind,id));
 CREATE TABLE IF NOT EXISTS ui_document_history(kind TEXT NOT NULL,id TEXT NOT NULL,revision INTEGER NOT NULL,body TEXT NOT NULL,updated REAL NOT NULL,PRIMARY KEY(kind,id,revision));
 CREATE TABLE IF NOT EXISTS ui_tool_evidence(chat_id TEXT NOT NULL,turn_id TEXT NOT NULL,call_id TEXT NOT NULL,name TEXT NOT NULL,receipt_id TEXT,body TEXT NOT NULL,PRIMARY KEY(chat_id,turn_id,call_id));
 CREATE TABLE IF NOT EXISTS ui_internal_sessions(id TEXT PRIMARY KEY);
 CREATE TABLE IF NOT EXISTS ui_secrets(provider TEXT PRIMARY KEY,value TEXT NOT NULL);")
}
pub(crate) fn get(db: &Connection, kind: &str, id: &str) -> rusqlite::Result<Option<Value>> {
    db.query_row(
        "SELECT body FROM ui_documents WHERE kind=? AND id=?",
        params![kind, id],
        |row| row.get::<_, String>(0),
    )
    .optional()?
    .map(|text| {
        serde_json::from_str(&text)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
    })
    .transpose()
}
pub(crate) fn list(db: &Connection, kind: &str) -> rusqlite::Result<Vec<Value>> {
    let mut q = db.prepare("SELECT body FROM ui_documents WHERE kind=? ORDER BY id")?;
    let rows = q
        .query_map([kind], |r| r.get::<_, String>(0))?
        .map(|text| {
            serde_json::from_str(&text?)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
        })
        .collect();
    rows
}
pub(crate) fn put(db: &Connection, kind: &str, id: &str, value: Value) -> rusqlite::Result<Value> {
    if db.is_autocommit() {
        let tx = db.unchecked_transaction()?;
        let value = put_inner(&tx, kind, id, value)?;
        tx.commit()?;
        Ok(value)
    } else {
        put_inner(db, kind, id, value)
    }
}
fn put_inner(db: &Connection, kind: &str, id: &str, mut value: Value) -> rusqlite::Result<Value> {
    let revision: i64 = db.query_row(
        "SELECT COALESCE(MAX(revision),0)+1 FROM ui_document_history WHERE kind=? AND id=?",
        params![kind, id],
        |r| r.get(0),
    )?;
    value["revision"] = revision.into();
    db.execute(
        "INSERT INTO ui_document_history VALUES (?,?,?,?,?)",
        params![kind, id, revision, value.to_string(), now()],
    )?;
    db.execute("INSERT INTO ui_documents VALUES (?,?,?,?,?) ON CONFLICT(kind,id) DO UPDATE SET body=excluded.body,revision=excluded.revision,updated=excluded.updated",params![kind,id,value.to_string(),revision,now()])?;
    Ok(value)
}
pub(crate) fn db_error(e: impl std::fmt::Display) -> (u16, String) {
    (500, format!("Native storage error: {e}"))
}
pub(crate) fn text<'a>(v: &'a Value, key: &str, max: usize) -> ApiResult<&'a str> {
    v[key]
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.len() <= max)
        .ok_or((400, format!("{key} is required (maximum {max} bytes)")))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn document_history_and_current_definition_commit_together() {
        let db = Connection::open_in_memory().unwrap();
        initialize(&db).unwrap();
        put(
            &db,
            "agent",
            "research",
            serde_json::json!({"name":"First"}),
        )
        .unwrap();
        {
            let tx = db.unchecked_transaction().unwrap();
            put(
                &tx,
                "agent",
                "research",
                serde_json::json!({"name":"Rollback"}),
            )
            .unwrap();
        }
        assert_eq!(
            get(&db, "agent", "research").unwrap().unwrap()["name"],
            "First"
        );
        let saved = put(
            &db,
            "agent",
            "research",
            serde_json::json!({"name":"Second"}),
        )
        .unwrap();
        assert_eq!(saved["revision"], 2);
    }
}
