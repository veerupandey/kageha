//! Durable chat questions. Answers resume the waiting tool without granting input authority.
use crate::{
    ui_store::{ApiResult, db_error},
    webui::{self, Database},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    io,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

pub(crate) fn initialize(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS ui_questions(
        id TEXT PRIMARY KEY, chat TEXT NOT NULL, turn TEXT NOT NULL,
        question TEXT NOT NULL, options TEXT NOT NULL, response TEXT
    )",
    )
}

fn payload(id: &str, chat: &str, turn: &str, question: &str, options: Value) -> Value {
    json!({"approval_id":id,"action":"ask_human","risk_class":"hitl",
        "session_id":chat,"turn_id":turn,"question":question,"detail":question,"options":options})
}

pub(crate) fn pending(db: &Connection, chat: &str) -> rusqlite::Result<Option<Value>> {
    let row = db
        .query_row(
            "SELECT q.id,q.turn,q.question,q.options FROM ui_questions q
         WHERE q.chat=? AND q.response IS NULL ORDER BY q.rowid DESC LIMIT 1",
            [chat],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            },
        )
        .optional()?;
    let Some((id, turn, question, options)) = row else {
        return Ok(None);
    };
    if !active(db, chat, &turn)? {
        return Ok(None);
    }
    Ok(Some(payload(
        &id,
        chat,
        &turn,
        &question,
        serde_json::from_str(&options).unwrap_or(json!([])),
    )))
}

fn active(db: &Connection, chat: &str, turn: &str) -> rusqlite::Result<bool> {
    let mut query = db
        .prepare("SELECT request_id FROM native_chat_turns WHERE chat_id=? AND status='running'")?;
    let ids = query
        .query_map([chat], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ids.iter().any(|id| webui::turn_id(chat, id) == turn))
}

fn non_secret(text: &str) -> bool {
    let lower = text.to_ascii_lowercase().replace(['-', '_'], " ");
    let sensitive_word = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|word| {
            matches!(
                word,
                "secret" | "token" | "credential" | "credentials" | "pin" | "otp" | "cvv"
            )
        });
    !sensitive_word
        && ![
            "password",
            "passcode",
            "passphrase",
            "api key",
            "private key",
            "secret key",
            "seed phrase",
            "verification code",
            "security code",
            "sign in code",
            "login code",
            "one time",
            "recovery code",
            "code sent",
            "enter the code",
            "read me the code",
        ]
        .iter()
        .any(|word| lower.contains(word))
}

pub(crate) fn ask(
    db: &Database,
    chat: &str,
    turn: &str,
    args: &Value,
    cancel: &AtomicBool,
) -> io::Result<String> {
    let question = args["question"].as_str().map(str::trim)
        .filter(|s| !s.is_empty() && s.len() <= 1024 && non_secret(s))
        .ok_or_else(|| io::Error::other("Ask a short question about preferences or scope. Credentials and verification codes belong in the browser."))?;
    let options = if args["options"].is_null() {
        vec![]
    } else {
        args["options"]
            .as_array()
            .filter(|rows| rows.len() <= 8)
            .ok_or_else(|| io::Error::other("Provide up to eight answer options"))?
            .iter()
            .enumerate()
            .map(|(index, v)| {
                v.as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty() && s.len() <= 256 && non_secret(s))
                    .ok_or_else(|| io::Error::other(json!({
                        "error_code":"invalid_arguments", "retryable":false,
                        "summary":format!("Answer option {} must be non-empty, at most 256 bytes, and must not mention credentials, secrets or verification codes. Ask the user to sign in directly in the browser; remove credential-handling options. No question was shown.", index + 1)
                    }).to_string()))
            })
            .collect::<io::Result<Vec<_>>>()?
    };
    let id = format!("question-{}", uuid::Uuid::new_v4().simple());
    {
        let mut c = db
            .lock()
            .map_err(|_| io::Error::other("Database unavailable"))?;
        let tx = c.transaction().map_err(io::Error::other)?;
        if cancel.load(Ordering::Relaxed) || !active(&tx, chat, turn).map_err(io::Error::other)? {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Turn no longer active",
            ));
        }
        tx.execute(
            "INSERT INTO ui_questions VALUES (?,?,?,?,?,NULL)",
            params![id, chat, turn, question, json!(options).to_string()],
        )
        .map_err(io::Error::other)?;
        webui::insert_event(&tx, chat, turn, "event", json!({"kind":"approval_required",
            "label":"Waiting for your answer","payload":payload(&id,chat,turn,question,json!(options))})).map_err(io::Error::other)?;
        tx.commit().map_err(io::Error::other)?;
    }
    crate::ui_session_events::notify();
    kageha_contracts::request_budget::waiting(|| {
    while !cancel.load(Ordering::Relaxed) {
        {
            let c = db
                .lock()
                .map_err(|_| io::Error::other("Database unavailable"))?;
            let response: Option<String> = c
                .query_row("SELECT response FROM ui_questions WHERE id=?", [&id], |r| {
                    r.get(0)
                })
                .map_err(io::Error::other)?;
            if let Some(response) = response {
                return Ok(response);
            }
            if !active(&c, chat, turn).map_err(io::Error::other)? {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Err(io::Error::new(
        io::ErrorKind::Interrupted,
        "Question cancelled",
    ))
    })
}

pub(crate) fn answer(db: &Database, chat: &str, id: &str, args: &Value) -> ApiResult<Value> {
    let answer = if args["skip"] == true {
        None
    } else {
        Some(
            args["answer"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty() && s.len() <= 4096)
                .ok_or((400, "Enter an answer or skip the question".into()))?,
        )
    };
    let response =
        json!({"status":if answer.is_some() {"answered"} else {"skipped"},"answer":answer});
    let encoded = response.to_string();
    let mut c = db.lock().map_err(db_error)?;
    let tx = c.transaction().map_err(db_error)?;
    let (turn, previous): (String, Option<String>) = tx
        .query_row(
            "SELECT turn,response FROM ui_questions WHERE chat=? AND id=?",
            params![chat, id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(db_error)?
        .ok_or((404, "Question not found".into()))?;
    if previous.as_deref() == Some(&encoded) {
        return Ok(json!({"ok":true}));
    }
    if previous.is_some() || !active(&tx, chat, &turn).map_err(db_error)? {
        return Err((
            409,
            "This question is no longer waiting for a response".into(),
        ));
    }
    tx.execute(
        "UPDATE ui_questions SET response=? WHERE id=?",
        params![encoded, id],
    )
    .map_err(db_error)?;
    webui::insert_event(&tx, chat, &turn, "event", json!({"kind":"approval_resolved","label":"Continuing…",
        "payload":{"approval_id":id,"action":"ask_human","answer":answer,"deferred":answer.is_none()}})).map_err(db_error)?;
    tx.commit().map_err(db_error)?;
    crate::ui_session_events::notify();
    Ok(json!({"ok":true}))
}

pub(crate) fn history(
    db: &Connection,
    chat: &str,
    turn: &str,
) -> rusqlite::Result<Vec<(String, String)>> {
    let mut query = db.prepare(
        "SELECT question,json_extract(response,'$.answer') FROM ui_questions
         WHERE chat=? AND turn=? AND json_extract(response,'$.status')='answered'
         ORDER BY rowid",
    )?;
    let rows = query.query_map(params![chat, turn], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn questions_may_request_missing_address_but_not_secrets() {
        assert!(non_secret("What are your home address and postal code?"));
        assert!(!non_secret("What is your password?"));
        assert!(!non_secret("What is your verification code?"));
    }

    #[test]
    fn missing_address_question_waits_and_returns_answer_to_the_same_turn() {
        let connection = Connection::open_in_memory().unwrap();
        connection.execute_batch("CREATE TABLE native_chat_turns(sequence INTEGER PRIMARY KEY,chat_id TEXT,request_id TEXT,message TEXT,answer TEXT,status TEXT);").unwrap();
        webui::initialize(&connection).unwrap();
        connection.execute("INSERT INTO native_chat_turns(chat_id,request_id,status) VALUES ('chat','request','running')", []).unwrap();
        let db = crate::agent_host::DatabaseState::open(connection).unwrap();
        let turn = webui::turn_id("chat", "request");
        let worker_db = db.clone();
        let worker_turn = turn.clone();
        let cancelled = std::sync::Arc::new(AtomicBool::new(false));
        let worker_cancel = cancelled.clone();
        let worker = std::thread::spawn(move || ask(&worker_db, "chat", &worker_turn,
            &json!({"question":"What are your home address and postal code?"}), &worker_cancel));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let question = loop {
            if let Some(value) = pending(&db.lock().unwrap(), "chat").unwrap() { break Some(value); }
            if std::time::Instant::now() >= deadline { break None; }
            std::thread::sleep(Duration::from_millis(10));
        };
        if question.is_none() { cancelled.store(true, Ordering::Relaxed); }
        let question = question.expect("question should be visible while the turn waits");
        answer(&db, "chat", question["approval_id"].as_str().unwrap(),
            &json!({"answer":"123 Example St, Vancouver, BC V1A 2B3"})).unwrap();
        let response: Value = serde_json::from_str(&worker.join().unwrap().unwrap()).unwrap();
        assert_eq!(response["status"], "answered");
        assert_eq!(response["answer"], "123 Example St, Vancouver, BC V1A 2B3");
        assert!(pending(&db.lock().unwrap(), "chat").unwrap().is_none());
        assert_eq!(history(&db.lock().unwrap(), "chat", &turn).unwrap().len(), 1);
    }

    #[test]
    fn invalid_secret_options_explain_the_fix_without_showing_a_question() {
        let db=crate::agent_host::DatabaseState::open(Connection::open_in_memory().unwrap()).unwrap();
        let error=ask(&db,"chat","turn",&json!({"question":"How should I proceed?",
            "options":["Sign in in the browser", "Use my account credentials"]}),&AtomicBool::new(false)).unwrap_err();
        let body:Value=serde_json::from_str(&error.to_string()).unwrap();
        assert_eq!(body["error_code"],"invalid_arguments");
        let summary=body["summary"].as_str().unwrap();
        assert!(summary.contains("option 2") && summary.contains("remove credential-handling options"));
    }
    #[test]
    fn follow_up_history_includes_only_answers_from_its_chat_and_turn() {
        let db = Connection::open_in_memory().unwrap();
        initialize(&db).unwrap();
        for (id, chat, turn, response) in [
            (
                "1",
                "chat",
                "turn",
                Some(json!({"status":"answered","answer":"Canada, remote"})),
            ),
            ("2", "chat", "turn", Some(json!({"status":"skipped"}))),
            ("3", "chat", "turn", None),
            (
                "4",
                "other",
                "turn",
                Some(json!({"status":"answered","answer":"Private answer"})),
            ),
            (
                "5",
                "chat",
                "other",
                Some(json!({"status":"answered","answer":"Different turn"})),
            ),
        ] {
            db.execute(
                "INSERT INTO ui_questions VALUES (?,?,?,'Where?','[]',?)",
                params![id, chat, turn, response.map(|value| value.to_string())],
            )
            .unwrap();
        }
        assert_eq!(
            history(&db, "chat", "turn").unwrap(),
            vec![("Where?".into(), "Canada, remote".into())]
        );
    }
}
