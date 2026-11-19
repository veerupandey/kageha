//! Unrecognized turn failures (spec autonomous-agent-loop, Requirement 20.11, design
//! section 18 "Error visibility", task 31.1).
//!
//! A turn whose error matches no known failure (`webui::execute` maps it to
//! `execution_failed`) used to end with only a generic message. [`report`] gives the
//! failure an id, logs `turn failed failure_id=<id> chat=<chat> turn=<turn> error=<error>`
//! to stderr (the server's log) and returns a safe summary, `failure_detail`, for the
//! turn result and its `done` event.
//!
//! Both texts are redacted: token-like values first (`ui_learning::scrub`), then values
//! held in the credential vault (`ui_learning_secrets::VaultSecrets`). When the vault
//! cannot be read the detail is withheld; the log keeps the pattern-redacted text.
use crate::webui::Database;
use kageha_learning::secrets::{redact_values, SecretSource};
use std::error::Error;

/// Longest `failure_detail`, in characters.
pub(crate) const DETAIL_CHARS: usize = 500;

/// An unrecognized failure as logged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Report {
    pub id: String,
    /// The redacted error, at most [`DETAIL_CHARS`] characters; `None` when the vault
    /// could not be read.
    pub detail: Option<String>,
}

/// `error` and its sources, joined with `: `. A source whose text the chain already
/// holds (an error that formats its cause into its message) is not repeated.
pub(crate) fn chain(error: &(dyn Error + 'static)) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let cause_text = cause.to_string();
        if !cause_text.is_empty() && !text.contains(&cause_text) {
            text.push_str(": ");
            text.push_str(&cause_text);
        }
        source = cause.source();
    }
    text
}

/// `text` without token-like values and vault values, or `Err` with the pattern-redacted
/// text when the vault could not be read.
fn redact(db: &Database, text: &str) -> Result<String, String> {
    let scrubbed = crate::ui_learning::scrub(text);
    match crate::ui_learning_secrets::VaultSecrets::new(db).values() {
        Ok(values) => Ok(redact_values(&scrubbed, &values).unwrap_or(scrubbed)),
        Err(_) => Err(scrubbed),
    }
}

/// Keep a policy stop reason visible without publishing the tool payload that
/// accompanied a post-action failure.
pub(crate) fn policy_reason(db: &Database, error: &std::io::Error) -> String {
    let message = error.to_string();
    let envelope = serde_json::from_str::<serde_json::Value>(&message).ok();
    let reason = envelope.as_ref().and_then(|value| value["hook_error"].as_str()).unwrap_or(&message);
    redact(db, reason).ok().map(|text| text.chars().take(DETAIL_CHARS).collect())
        .unwrap_or_else(|| "A policy check stopped this turn. Inspect its recorded activity.".into())
}

/// The log line of a failure. Line breaks in the error are escaped so the entry stays
/// on one line.
pub(crate) fn log_line(id: &str, chat: &str, turn: &str, error: &str) -> String {
    let error = error.replace('\r', "\\r").replace('\n', "\\n");
    format!("turn failed failure_id={id} chat={chat} turn={turn} error={error}")
}

/// Logs the unrecognized failure `error` of `turn` and returns its id and detail.
pub(crate) fn report(db: &Database, chat: &str, turn: &str, error: &(dyn Error + 'static)) -> Report {
    let id = uuid::Uuid::new_v4().to_string();
    let (logged, detail) = match redact(db, &chain(error)) {
        Ok(text) => (text.clone(), Some(text.chars().take(DETAIL_CHARS).collect())),
        Err(scrubbed) => (scrubbed, None),
    };
    let line = log_line(&id, chat, turn, &logged);
    eprintln!("{line}");
    #[cfg(test)]
    test_hooks::LOGGED.lock().unwrap_or_else(|poison| poison.into_inner()).push(line);
    Report { id, detail }
}

/// Test-only setup failures and the log lines [`report`] wrote.
#[cfg(test)]
pub(crate) mod test_hooks {
    use std::{collections::HashMap, io, sync::Mutex};
    pub(crate) static LOGGED: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static INJECTED: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

    /// The setup error `turn` fails with: `message` caused by an error whose text is `cause`.
    #[derive(Debug)]
    struct Injected { message: String, cause: io::Error }
    impl std::fmt::Display for Injected {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.message) }
    }
    impl std::error::Error for Injected {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> { Some(&self.cause) }
    }

    pub(crate) fn inject(turn: &str, message: &str, cause: &str) {
        let mut injected = INJECTED.lock().unwrap();
        injected.get_or_insert_with(HashMap::new).insert(turn.to_owned(), format!("{message}\u{0}{cause}"));
    }

    /// The error injected for `turn`, once.
    pub(crate) fn take(turn: &str) -> Option<io::Error> {
        let stored = INJECTED.lock().unwrap().as_mut()?.remove(turn)?;
        let (message, cause) = stored.split_once('\u{0}')?;
        Some(io::Error::other(Injected { message: message.to_owned(), cause: io::Error::other(cause.to_owned()) }))
    }

    pub(crate) fn logged(turn: &str) -> Vec<String> {
        LOGGED.lock().unwrap().iter().filter(|line| line.contains(&format!(" turn={turn} "))).cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database() -> (crate::audited_session_tests::Fixture, Database) {
        let fixture = crate::audited_session_tests::Fixture::new();
        let db = fixture.db.clone();
        (fixture, db)
    }

    #[test]
    fn the_chain_joins_sources_once() {
        let inner = std::io::Error::other("disk full");
        let outer = std::io::Error::other(format!("checkpoint write failed: {inner}"));
        assert_eq!(chain(&outer), "checkpoint write failed: disk full");
        test_hooks::inject("t-chain", "workspace open failed", "permission denied");
        let injected = test_hooks::take("t-chain").unwrap();
        assert_eq!(chain(&injected), "workspace open failed: permission denied");
        assert!(test_hooks::take("t-chain").is_none());
    }

    #[test]
    fn report_logs_one_line_and_redacts_tokens_and_vault_values() {
        let (_directory, db) = database();
        crate::ui_credentials::save(&db.lock().unwrap(), "provider-a", "vault-held-value-42").unwrap();
        let error = std::io::Error::other(
            "provider rejected api_key=sk-abcdefghijklmnop1234567890\nwith vault-held-value-42 inside");
        let report = report(&db, "chat-1", "turn-redact", &error);
        let detail = report.detail.unwrap();
        assert!(!detail.contains("sk-abcdefghijklmnop1234567890") && !detail.contains("vault-held-value-42"), "{detail}");
        assert!(detail.starts_with("provider rejected api_key=") && detail.contains("[redacted]"), "{detail}");
        let lines = test_hooks::logged("turn-redact");
        assert_eq!(lines.len(), 1, "{lines:?}");
        let line = &lines[0];
        assert!(line.starts_with(&format!("turn failed failure_id={} chat=chat-1 turn=turn-redact error=provider rejected", report.id)), "{line}");
        assert!(!line.contains('\n') && !line.contains("vault-held-value-42") && !line.contains("sk-abcdef"), "{line}");
        assert!(uuid::Uuid::parse_str(&report.id).is_ok());
    }

    #[test]
    fn the_detail_keeps_the_first_500_characters() {
        let (_directory, db) = database();
        let text = "é ".repeat(400);
        let detail = report(&db, "chat", "turn-length", &std::io::Error::other(text.clone())).detail.unwrap();
        assert_eq!(detail.chars().count(), DETAIL_CHARS);
        assert!(text.starts_with(&detail));
    }
}
