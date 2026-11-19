//! Bounded recovery for public background reads. Interactive inputs never use this path.
use kageha_contracts::execution::Control;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    io,
    sync::{atomic::AtomicBool, Mutex},
    time::{Duration, Instant},
};

#[cfg(test)]
use std::sync::LazyLock;

type Blockers = Mutex<HashMap<String, (String, u8)>>;

const SOURCE_CHARS: usize = 128_000;
const WINDOW_CHARS: usize = 8000;
const SOURCE_COUNT: usize = 32;
const SOURCE_TTL: Duration = Duration::from_secs(3600);

struct Source {
    id: String,
    scope: String,
    requested_url: String,
    text: String,
    page: Value,
    truncated: bool,
    created: Instant,
}

#[derive(Default)]
struct SourceCache {
    entries: VecDeque<Source>,
}

#[cfg(test)]
static SOURCES: LazyLock<Mutex<SourceCache>> = LazyLock::new(Mutex::default);

pub(crate) fn source_scope(chat: &str, turn: &str, agent: &Value, access: &Value) -> String {
    format!(
        "{:x}",
        Sha256::digest(json!([chat, turn, agent, access]).to_string().as_bytes())
    )
}

fn source_error(code: &str, summary: &str) -> io::Error {
    io::Error::other(json!({"error_code":code,"retryable":false,"summary":summary}).to_string())
}

const SOURCE_KIND: &str = "research-source";

fn stored_source(
    db: &rusqlite::Connection,
    chat: &str,
    scope: &str,
    args: &Value,
) -> io::Result<Option<Source>> {
    use rusqlite::OptionalExtension;
    let id = args["source_id"].as_str();
    let body: Option<String> = db.query_row(
        "SELECT body FROM ui_documents WHERE kind=?1 AND json_extract(body,'$.chat')=?2 AND json_extract(body,'$.scope')=?3 AND length(CAST(body AS BLOB))<=1048576 AND ((?4 IS NOT NULL AND id=?4) OR (?4 IS NULL AND json_extract(body,'$.requested_url')=?5)) ORDER BY updated DESC LIMIT 1",
        rusqlite::params![SOURCE_KIND,chat,scope,id,args["url"].as_str()], |row| row.get(0),
    ).optional().map_err(io::Error::other)?;
    let Some(body) = body else {
        return Ok(None);
    };
    let body: Value = serde_json::from_str(&body)?;
    let text = body["text"]
        .as_str()
        .filter(|text| text.chars().count() <= SOURCE_CHARS);
    let saved_id = body["id"]
        .as_str()
        .filter(|saved| id.is_none_or(|id| *saved == id));
    match (text, saved_id, body["requested_url"].as_str()) {
        (Some(text),Some(id),Some(requested_url)) if body["page"].is_object() => Ok(Some(Source {
            id:id.into(),scope:scope.into(),requested_url:requested_url.into(),text:text.into(),
            page:body["page"].clone(),truncated:body["truncated"] == true,created:Instant::now(),
        })),
        _ => Err(source_error("research_source_expired", "The retained source record is invalid. Fetch its public URL again to obtain a new source_id.")),
    }
}

fn save_source(db: &rusqlite::Connection, chat: &str, source: &Source) -> io::Result<()> {
    let tx = db.unchecked_transaction().map_err(io::Error::other)?;
    crate::ui_store::put(&tx,SOURCE_KIND,&source.id,json!({"id":source.id,"chat":chat,"scope":source.scope,
        "requested_url":source.requested_url,"text":source.text,"page":source.page,"truncated":source.truncated})).map_err(io::Error::other)?;
    let expired = {
        let mut query = tx.prepare("SELECT id FROM ui_documents WHERE kind=? ORDER BY updated DESC,id DESC LIMIT -1 OFFSET ?").map_err(io::Error::other)?;
        let rows = query
            .query_map(rusqlite::params![SOURCE_KIND, SOURCE_COUNT], |row| {
                row.get::<_, String>(0)
            })
            .map_err(io::Error::other)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(io::Error::other)?
    };
    for id in expired {
        for table in ["ui_documents", "ui_document_history"] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE kind=? AND id=?"),
                rusqlite::params![SOURCE_KIND, id],
            )
            .map_err(io::Error::other)?;
        }
    }
    tx.commit().map_err(io::Error::other)
}

pub(crate) fn erase_chat(db: &rusqlite::Connection, chat: &str) -> rusqlite::Result<()> {
    for table in ["ui_documents", "ui_document_history"] {
        db.execute(
            &format!("DELETE FROM {table} WHERE kind=? AND json_extract(body,'$.chat')=?"),
            rusqlite::params![SOURCE_KIND, chat],
        )?;
    }
    Ok(())
}

/// Retained extraction is durable, but it is never placed in model context or
/// reopened across a different chat, turn, agent, or access fingerprint.
pub(crate) fn progressive_stored(
    db: &crate::webui::Database,
    chat: &str,
    scope: &str,
    args: &Value,
    cancelled: &AtomicBool,
    read: impl FnOnce() -> io::Result<Value>,
) -> io::Result<Value> {
    validate_window(args)?;
    let control = Control {
        cancelled,
        deadline: None,
    };
    control.check()?;
    let saved = stored_source(
        &*db.lock()
            .map_err(|_| io::Error::other("Source storage unavailable"))?,
        chat,
        scope,
        args,
    )?;
    let mut cache = SourceCache::default();
    if let Some(source) = saved {
        cache.entries.push_back(source);
    }
    if let Some(page) = cache.lookup(scope, args)? {
        control.check()?;
        return Ok(page);
    }
    let page = read()?;
    control.check()?;
    let result = cache.retain(scope, args, page)?;
    if let Some(source) = cache.entries.back() {
        save_source(
            &*db.lock()
                .map_err(|_| io::Error::other("Source storage unavailable"))?,
            chat,
            source,
        )?;
    }
    Ok(result)
}

impl SourceCache {
    fn prune(&mut self) {
        self.entries
            .retain(|source| source.created.elapsed() < SOURCE_TTL);
    }

    fn lookup(&mut self, scope: &str, args: &Value) -> io::Result<Option<Value>> {
        self.prune();
        let id = args["source_id"].as_str().filter(|id| !id.is_empty());
        let url = args["url"].as_str();
        let source = self.entries.iter().find(|source| {
            source.scope == scope
                && match id {
                    Some(id) => source.id == id,
                    None => url == Some(source.requested_url.as_str()),
                }
        });
        if id.is_some() && source.is_none() {
            return Err(source_error("research_source_expired", "This retained source is unavailable in the current chat, turn, or access scope. Fetch its public URL again to obtain a new source_id."));
        }
        if let Some(source) = source {
            if url.is_some_and(|url| {
                url != source.requested_url && Some(url) != source.page["url"].as_str()
            }) {
                return Err(source_error(
                    "research_source_mismatch",
                    "The source_id does not belong to this URL. Use its original URL or omit url.",
                ));
            }
            let mut page = source.window(args)?;
            page["source_cached"] = json!(true);
            return Ok(Some(page));
        }
        Ok(None)
    }

    fn retain(&mut self, scope: &str, args: &Value, mut page: Value) -> io::Result<Value> {
        self.prune();
        // Structured feeds retain their existing record offsets and must never
        // reinterpret them as character offsets into serialized JSON.
        if matches!(page["format"].as_str(), Some("job_feed" | "rss")) {
            return Ok(page);
        }
        let full = page
            .as_object_mut()
            .and_then(|page| page.remove("source_text"));
        let inherited_truncation = page
            .as_object_mut()
            .and_then(|page| page.remove("source_truncated"))
            .and_then(|value| value.as_bool())
            .unwrap_or_else(|| page["truncated"] == true);
        let full = full
            .as_ref()
            .and_then(Value::as_str)
            .or_else(|| page["text"].as_str())
            .unwrap_or("");
        let mut chars = full.chars();
        let text: String = chars.by_ref().take(SOURCE_CHARS).collect();
        let truncated = inherited_truncation || chars.next().is_some();
        let source = Source {
            id: format!("source-{}", uuid::Uuid::new_v4()),
            scope: scope.into(),
            requested_url: args["url"].as_str().unwrap_or("").into(),
            text,
            page,
            truncated,
            created: Instant::now(),
        };
        let mut result = source.window(args)?;
        result["source_cached"] = json!(false);
        self.entries.push_back(source);
        while self.entries.len() > SOURCE_COUNT {
            self.entries.pop_front();
        }
        Ok(result)
    }
}

impl Source {
    fn window(&self, args: &Value) -> io::Result<Value> {
        let total = self.text.chars().count();
        let mut offset = args["offset"].as_u64().unwrap_or(0).min(total as u64) as usize;
        let phrase = args["search_phrase"]
            .as_str()
            .filter(|phrase| !phrase.is_empty());
        if phrase.is_some_and(|phrase| phrase.chars().count() > 200) {
            return Err(source_error(
                "invalid_params",
                "search_phrase must contain at most 200 characters.",
            ));
        }
        let mut found = None;
        if let Some(phrase) = phrase {
            let byte_offset = self
                .text
                .char_indices()
                .nth(offset)
                .map(|(index, _)| index)
                .unwrap_or(self.text.len());
            if let Some(index) = self.text[byte_offset..].find(phrase) {
                offset += self.text[byte_offset..byte_offset + index].chars().count();
                found = Some(offset);
            }
        }
        let mut page = self.page.clone();
        let text: String = if phrase.is_some() && found.is_none() {
            String::new()
        } else {
            self.text.chars().skip(offset).take(WINDOW_CHARS).collect()
        };
        let end = offset + text.chars().count();
        page["text"] = json!(text);
        page["source_id"] = json!(self.id);
        page["offset"] = json!(offset);
        page["next_offset"] =
            json!((end < total && !(phrase.is_some() && found.is_none())).then_some(end));
        page["source_total_chars"] = json!(total);
        page["source_truncated"] = json!(self.truncated);
        page["truncated"] = json!(end < total || self.truncated);
        if phrase.is_some() {
            page["match_offset"] = json!(found);
            page["search_complete"] = json!(!self.truncated);
        }
        Ok(page)
    }
}

#[cfg(test)]
fn progressive(
    scope: &str,
    args: &Value,
    cancelled: &AtomicBool,
    read: impl FnOnce() -> io::Result<Value>,
) -> io::Result<Value> {
    let control = Control {
        cancelled,
        deadline: None,
    };
    control.check()?;
    validate_window(args)?;
    if let Some(page) = SOURCES.lock().unwrap().lookup(scope, args)? {
        return Ok(page);
    }
    let page = read()?;
    control.check()?;
    SOURCES.lock().unwrap().retain(scope, args, page)
}

fn validate_window(args: &Value) -> io::Result<()> {
    if args.get("offset").is_some_and(|value| !value.is_u64())
        || args.get("source_id").is_some_and(|value| {
            !value
                .as_str()
                .is_some_and(|id| !id.is_empty() && id.len() <= 128)
        })
        || args.get("search_phrase").is_some_and(|value| {
            !value
                .as_str()
                .is_some_and(|phrase| !phrase.is_empty() && phrase.chars().count() <= 200)
        })
    {
        return Err(source_error("invalid_params", "Use a nonnegative character offset, a valid source_id, and a search_phrase of 1–200 characters."));
    }
    Ok(())
}

fn code(error: &io::Error) -> Option<String> {
    let value: Value = serde_json::from_str(&error.to_string()).ok()?;
    value["error_code"]
        .as_str()
        .or_else(|| value["detail"]["code"].as_str())
        .or_else(|| value["error"]["code"].as_str())
        .map(str::to_owned)
}

fn recoverable(code: &str) -> bool {
    matches!(
        code,
        "research_response_too_large" | "research_read_failed" | "research_backend_unavailable"
    )
}

pub(crate) fn readable(page: &Value) -> bool {
    page["ok"] == true
        && page["text"]
            .as_str()
            .is_some_and(|text| !text.trim().is_empty())
}

fn exhausted(url: &str, reason: &str, render_error: Option<&io::Error>, cached: bool) -> io::Error {
    io::Error::other(json!({
        "error_code":"research_recovery_exhausted", "url":url, "retryable":false,
        "attempted_backends":["http","chromium"], "http_error_code":reason,
        "render_error_code":render_error.and_then(code), "cached":cached,
        "summary":"Both public-page reading methods failed. Do not repeat this fetch unchanged. Continue with another source, or use the visible browser to inspect this page if the task needs it. Other work can continue."
    }).to_string())
}

pub(crate) fn fetch(
    url: &str,
    cancelled: &AtomicBool,
    blockers: &Blockers,
    mut read: impl FnMut(&str, u64) -> io::Result<Value>,
) -> io::Result<Value> {
    let control = Control {
        cancelled,
        deadline: None,
    };
    control.check()?;
    let key = format!("web_fetch:{url}");
    if let Some((reason, _)) = blockers.lock().unwrap().get(&key) {
        return Err(exhausted(url, reason, None, true));
    }
    let first = read("http", 16);
    control.check()?;
    let reason = match first {
        Ok(page) if readable(&page) => return Ok(page),
        Ok(page)
            if page["status"] == 200
                && page["content_type"]
                    .as_str()
                    .is_some_and(|kind| kind.to_ascii_lowercase().contains("html")) =>
        {
            "research_empty_html".to_owned()
        }
        Ok(page) => return Ok(page),
        Err(error) => {
            if matches!(
                error.kind(),
                io::ErrorKind::Interrupted | io::ErrorKind::TimedOut
            ) {
                return Err(error);
            }
            match code(&error).filter(|code| recoverable(code)) {
                Some(code) => code,
                None => return Err(error),
            }
        }
    };
    control.check()?;
    let rendered = read("chromium", 20);
    control.check()?;
    match rendered {
        Ok(mut page) => {
            if !readable(&page)
                && page["status"]
                    .as_u64()
                    .is_some_and(|status| (200..400).contains(&status))
            {
                blockers.lock().unwrap().insert(key, (reason.clone(), 1));
                let error =
                    io::Error::other(json!({"error_code":"research_unreadable"}).to_string());
                return Err(exhausted(url, &reason, Some(&error), false));
            }
            page["recovery"] = json!({"attempted_backends":["http","chromium"],
                "reason":reason,"recovered":readable(&page)});
            Ok(page)
        }
        Err(error) => {
            // Cancellation, deadlines and protocol failures must not become a
            // permanent source failure or cause another request.
            if matches!(
                error.kind(),
                io::ErrorKind::Interrupted | io::ErrorKind::TimedOut
            ) {
                return Err(error);
            }
            if !code(&error).is_some_and(|code| {
                recoverable(&code)
                    || matches!(
                        code.as_str(),
                        "research_unreadable" | "research_source_unavailable"
                    )
            }) {
                return Err(error);
            }
            blockers.lock().unwrap().insert(key, (reason.clone(), 1));
            Err(exhausted(url, &reason, Some(&error), false))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    fn failure(code: &str) -> io::Error {
        io::Error::other(json!({"error_code":code}).to_string())
    }
    fn page() -> Value {
        json!({"ok":true,"status":200,"text":"Readable source","content_type":"text/html"})
    }

    #[test]
    fn retained_extraction_survives_reopen_without_network_and_deletes_with_chat() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sources.sqlite");
        let open = || {
            let db = rusqlite::Connection::open(&path).unwrap();
            crate::ui_store::initialize(&db).unwrap();
            crate::agent_host::DatabaseState::open(db).unwrap()
        };
        let cancelled = AtomicBool::new(false);
        let scope = source_scope("chat-one", "turn-one", &json!("main"), &json!({}));
        let db = open();
        let args = json!({"url":"https://example.com/durable"});
        let mut full = page();
        full["source_text"] = json!(format!("{}🌲needle", "é".repeat(9000)));
        full["source_truncated"] = json!(false);
        let first =
            progressive_stored(&db, "chat-one", &scope, &args, &cancelled, || Ok(full)).unwrap();
        assert!(first.get("source_text").is_none());
        assert_eq!(
            first["text"].as_str().unwrap().chars().count(),
            WINDOW_CHARS
        );
        let id = first["source_id"].clone();
        drop(db);
        let db = open();
        let result = progressive_stored(
            &db,
            "chat-one",
            &scope,
            &json!({"source_id":id,"offset":9000}),
            &cancelled,
            || panic!("restart redownloaded source"),
        )
        .unwrap();
        assert_eq!(result["text"], "🌲needle");
        assert_eq!(result["source_id"], id);
        assert_eq!(result["source_cached"], true);
        assert!(progressive_stored(
            &db,
            "chat-two",
            &scope,
            &json!({"source_id":id}),
            &cancelled,
            || panic!("cross-chat lookup performed network")
        )
        .is_err());
        assert!(progressive_stored(
            &db,
            "chat-one",
            "changed-access",
            &json!({"source_id":id}),
            &cancelled,
            || panic!("access change performed network")
        )
        .is_err());
        erase_chat(&db.lock().unwrap(), "chat-one").unwrap();
        assert!(progressive_stored(
            &db,
            "chat-one",
            &scope,
            &json!({"source_id":id}),
            &cancelled,
            || panic!("deleted source performed network")
        )
        .is_err());
        for table in ["ui_documents", "ui_document_history"] {
            assert_eq!(
                db.lock()
                    .unwrap()
                    .query_row(
                        &format!("SELECT COUNT(*) FROM {table} WHERE kind=?"),
                        [SOURCE_KIND],
                        |row| row.get::<_, usize>(0)
                    )
                    .unwrap(),
                0
            );
        }
    }

    #[test]
    fn durable_source_retention_purges_old_records_and_history_together() {
        let db =
            crate::agent_host::DatabaseState::open(rusqlite::Connection::open_in_memory().unwrap())
                .unwrap();
        crate::ui_store::initialize(&db.lock().unwrap()).unwrap();
        let cancelled = AtomicBool::new(false);
        for index in 0..SOURCE_COUNT + 3 {
            progressive_stored(
                &db,
                "chat",
                "scope",
                &json!({"url":format!("https://example.com/{index}")}),
                &cancelled,
                || Ok(page()),
            )
            .unwrap();
        }
        for table in ["ui_documents", "ui_document_history"] {
            assert_eq!(
                db.lock()
                    .unwrap()
                    .query_row(
                        &format!("SELECT COUNT(*) FROM {table} WHERE kind=?"),
                        [SOURCE_KIND],
                        |row| row.get::<_, usize>(0)
                    )
                    .unwrap(),
                SOURCE_COUNT
            );
        }
    }

    #[test]
    fn retained_windows_and_phrase_search_use_unicode_offsets_without_redownload() {
        let scope = source_scope(
            "unicode-chat",
            &uuid::Uuid::new_v4().to_string(),
            &json!("main"),
            &json!({}),
        );
        let cancelled = AtomicBool::new(false);
        let args = json!({"url":"https://example.com/unicode"});
        let text = format!("{}🌲needle café終", "é".repeat(9000));
        let mut full = page();
        full["source_text"] = json!(text);
        full["source_truncated"] = json!(false);
        let first = progressive(&scope, &args, &cancelled, || Ok(full)).unwrap();
        assert_eq!(
            first["text"].as_str().unwrap().chars().count(),
            WINDOW_CHARS
        );
        assert_eq!(first["next_offset"], WINDOW_CHARS);
        assert!(first.get("source_text").is_none());
        let id = first["source_id"].clone();
        let second = progressive(
            &scope,
            &json!({"source_id":id,"offset":9000}),
            &cancelled,
            || panic!("source window downloaded again"),
        )
        .unwrap();
        assert_eq!(second["text"], "🌲needle café終");
        assert!(second["next_offset"].is_null());
        let found = progressive(
            &scope,
            &json!({"source_id":id,"search_phrase":"needle"}),
            &cancelled,
            || panic!("search downloaded again"),
        )
        .unwrap();
        assert_eq!(found["match_offset"], 9001);
        assert_eq!(found["text"], "needle café終");
        let repeated =
            progressive(&scope, &args, &cancelled, || panic!("URL downloaded twice")).unwrap();
        assert_eq!(repeated["source_id"], id);
    }

    #[test]
    fn retained_sources_are_bounded_expire_and_reject_other_scopes() {
        let mut cache = SourceCache::default();
        let args = json!({"url":"https://example.com"});
        let mut full = page();
        full["source_text"] = json!("🌲".repeat(SOURCE_CHARS + 1));
        full["source_truncated"] = json!(false);
        let first = cache.retain("scope-one", &args, full).unwrap();
        assert_eq!(first["source_total_chars"], SOURCE_CHARS);
        assert_eq!(first["source_truncated"], true);
        let by_id = json!({"source_id":first["source_id"]});
        assert_eq!(
            code(&cache.lookup("scope-two", &by_id).unwrap_err()).as_deref(),
            Some("research_source_expired")
        );
        let missing = cache
            .lookup(
                "scope-one",
                &json!({"source_id":first["source_id"],"search_phrase":"absent"}),
            )
            .unwrap()
            .unwrap();
        assert_eq!(missing["text"], "");
        assert_eq!(missing["search_complete"], false);
        assert!(missing["match_offset"].is_null());
        assert!(cache
            .lookup(
                "scope-one",
                &json!({"source_id":first["source_id"],"url":"https://other.test"})
            )
            .is_err());
        cache.entries[0].created = Instant::now() - SOURCE_TTL;
        assert!(cache.lookup("scope-one", &by_id).is_err());
        for index in 0..SOURCE_COUNT + 3 {
            cache
                .retain(
                    "scope-one",
                    &json!({"url":format!("https://example.com/{index}")}),
                    page(),
                )
                .unwrap();
        }
        assert_eq!(cache.entries.len(), SOURCE_COUNT);
    }

    #[test]
    fn retained_source_identity_is_scoped_to_chat_turn_agent_and_access() {
        let mut cache = SourceCache::default();
        let scope = source_scope(
            "chat-one",
            "turn-one",
            &json!("main"),
            &json!({"profile":"public"}),
        );
        let result = cache
            .retain(&scope, &json!({"url":"https://example.com"}), page())
            .unwrap();
        let args = json!({"source_id":result["source_id"]});
        for other in [
            source_scope(
                "chat-two",
                "turn-one",
                &json!("main"),
                &json!({"profile":"public"}),
            ),
            source_scope(
                "chat-one",
                "turn-two",
                &json!("main"),
                &json!({"profile":"public"}),
            ),
            source_scope(
                "chat-one",
                "turn-one",
                &json!("worker"),
                &json!({"profile":"public"}),
            ),
            source_scope(
                "chat-one",
                "turn-one",
                &json!("main"),
                &json!({"profile":"changed"}),
            ),
        ] {
            assert_eq!(
                code(&cache.lookup(&other, &args).unwrap_err()).as_deref(),
                Some("research_source_expired")
            );
        }
        assert!(cache.lookup(&scope, &args).unwrap().is_some());
    }

    #[test]
    fn job_feed_offsets_are_not_reinterpreted_as_text_offsets() {
        let mut cache = SourceCache::default();
        let feed = json!({"format":"job_feed","text":"Structured jobs","feed":{"offset":20,"next_offset":40}});
        assert_eq!(
            cache
                .retain(
                    "scope",
                    &json!({"url":"https://example.com/jobs","offset":20}),
                    feed.clone()
                )
                .unwrap(),
            feed
        );
        assert!(cache.entries.is_empty());
    }

    #[test]
    fn oversized_or_failed_http_uses_one_bounded_render_and_records_recovery() {
        for reason in [
            "research_response_too_large",
            "research_read_failed",
            "research_backend_unavailable",
        ] {
            let mut calls = vec![];
            let result = fetch(
                "https://example.com",
                &AtomicBool::new(false),
                &Blockers::default(),
                |backend, timeout| {
                    calls.push((backend.to_owned(), timeout));
                    if backend == "http" {
                        Err(failure(reason))
                    } else {
                        Ok(page())
                    }
                },
            )
            .unwrap();
            assert_eq!(calls, vec![("http".into(), 16), ("chromium".into(), 20)]);
            assert_eq!(result["recovery"]["reason"], reason);
            assert_eq!(result["recovery"]["recovered"], true);
        }
    }

    #[test]
    fn readable_http_and_http_404_do_not_launch_a_browser() {
        for expected in [page(), json!({"ok":false,"status":404,"text":"Missing"})] {
            let mut calls = 0;
            let result = fetch(
                "https://example.com",
                &AtomicBool::new(false),
                &Blockers::default(),
                |backend, _| {
                    assert_eq!(backend, "http");
                    calls += 1;
                    Ok(expected.clone())
                },
            )
            .unwrap();
            assert_eq!(calls, 1);
            assert_eq!(result, expected);
        }
    }

    #[test]
    fn empty_html_is_rendered_and_nested_error_codes_are_supported() {
        for first in [
            Ok(json!({"ok":true,"status":200,"text":" ","content_type":"Text/HTML"})),
            Err(io::Error::other(
                json!({"detail":{"code":"research_response_too_large"}}).to_string(),
            )),
        ] {
            let mut first = Some(first);
            let result = fetch(
                "https://example.com",
                &AtomicBool::new(false),
                &Blockers::default(),
                |backend, _| {
                    if backend == "http" {
                        first.take().unwrap()
                    } else {
                        Ok(page())
                    }
                },
            )
            .unwrap();
            assert_eq!(result["recovery"]["recovered"], true);
        }
    }

    #[test]
    fn exhausted_url_is_cached_but_other_sources_remain_available() {
        let blockers = Blockers::default();
        let cancelled = AtomicBool::new(false);
        let mut calls = 0;
        let error = fetch(
            "https://example.com/a",
            &cancelled,
            &blockers,
            |backend, _| {
                calls += 1;
                Err(failure(if backend == "http" {
                    "research_response_too_large"
                } else {
                    "research_unreadable"
                }))
            },
        )
        .unwrap_err();
        let detail: Value = serde_json::from_str(&error.to_string()).unwrap();
        assert_eq!(calls, 2);
        assert_eq!(detail["error_code"], "research_recovery_exhausted");
        assert_eq!(detail["render_error_code"], "research_unreadable");
        assert_eq!(detail["cached"], false);
        let error = fetch("https://example.com/a", &cancelled, &blockers, |_, _| {
            panic!("exhausted URL was dispatched again")
        })
        .unwrap_err();
        assert_eq!(
            serde_json::from_str::<Value>(&error.to_string()).unwrap()["cached"],
            true
        );
        assert!(
            fetch("https://example.com/b", &cancelled, &blockers, |_, _| Ok(
                page()
            ))
            .is_ok()
        );
    }

    #[test]
    fn unreadable_render_and_unavailable_render_are_exhausted_once() {
        for rendered in [
            Ok(json!({"ok":true,"status":200,"text":" "})),
            Ok(json!({"ok":true,"status":302,"text":""})),
            Err(failure("research_source_unavailable")),
        ] {
            let blockers = Blockers::default();
            let cancelled = AtomicBool::new(false);
            let mut rendered = Some(rendered);
            let mut calls = 0;
            let error = fetch(
                "https://example.com",
                &cancelled,
                &blockers,
                |backend, _| {
                    calls += 1;
                    if backend == "http" {
                        Err(failure("research_response_too_large"))
                    } else {
                        rendered.take().unwrap()
                    }
                },
            )
            .unwrap_err();
            assert_eq!(calls, 2);
            let detail: Value = serde_json::from_str(&error.to_string()).unwrap();
            assert_eq!(detail["error_code"], "research_recovery_exhausted");
            assert!(matches!(
                detail["render_error_code"].as_str(),
                Some("research_unreadable" | "research_source_unavailable")
            ));
            assert!(
                fetch("https://example.com", &cancelled, &blockers, |_, _| panic!(
                    "exhausted render retried"
                ))
                .is_err()
            );
        }
    }

    #[test]
    fn rendered_http_404_preserves_existing_status_handler() {
        let blockers = Blockers::default();
        let result = fetch(
            "https://example.com",
            &AtomicBool::new(false),
            &blockers,
            |backend, _| {
                if backend == "http" {
                    Err(failure("research_response_too_large"))
                } else {
                    Ok(json!({"ok":false,"status":404,"text":"Not found"}))
                }
            },
        )
        .unwrap();
        assert_eq!(result["status"], 404);
        assert_eq!(result["recovery"]["recovered"], false);
        assert!(blockers.lock().unwrap().is_empty());
        let mut calls = 0;
        let error = fetch(
            "https://example.com",
            &AtomicBool::new(false),
            &blockers,
            |backend, _| {
                assert_eq!(backend, "http");
                calls += 1;
                Err(failure("research_source_unavailable"))
            },
        )
        .unwrap_err();
        assert_eq!(code(&error).as_deref(), Some("research_source_unavailable"));
        assert_eq!(calls, 1);
    }

    #[test]
    fn cancellation_before_or_between_reads_never_dispatches_more_work() {
        let cancelled = AtomicBool::new(true);
        assert_eq!(
            fetch(
                "https://example.com",
                &cancelled,
                &Blockers::default(),
                |_, _| panic!("cancelled read")
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::Interrupted
        );
        cancelled.store(false, Ordering::Release);
        let blockers = Blockers::default();
        let mut calls = 0;
        assert_eq!(
            fetch("https://example.com", &cancelled, &blockers, |_, _| {
                calls += 1;
                cancelled.store(true, Ordering::Release);
                Err(failure("research_response_too_large"))
            })
            .unwrap_err()
            .kind(),
            io::ErrorKind::Interrupted
        );
        assert_eq!(calls, 1);
        assert!(blockers.lock().unwrap().is_empty());
    }

    #[test]
    fn unknown_failures_and_deadlines_are_preserved_and_not_cached() {
        for fail_on in ["http", "chromium"] {
            for kind in [
                io::ErrorKind::Other,
                io::ErrorKind::TimedOut,
                io::ErrorKind::Interrupted,
            ] {
                let blockers = Blockers::default();
                let mut calls = 0;
                let error = fetch(
                    "https://example.com",
                    &AtomicBool::new(false),
                    &blockers,
                    |backend, _| {
                        calls += 1;
                        if backend == fail_on {
                            Err(io::Error::new(kind, "unrecognized protocol failure"))
                        } else {
                            Err(failure("research_response_too_large"))
                        }
                    },
                )
                .unwrap_err();
                assert_eq!(error.kind(), kind);
                assert_eq!(error.to_string(), "unrecognized protocol failure");
                assert_eq!(calls, if fail_on == "http" { 1 } else { 2 });
                assert!(blockers.lock().unwrap().is_empty());
            }
        }
    }

    #[test]
    fn invalid_requests_and_structured_deadlines_do_not_trigger_recovery() {
        for rejected in [
            "invalid_params",
            "research_invalid_url",
            "permission_denied",
            "deadline_exceeded",
            "research_response_too_large_fake",
        ] {
            let blockers = Blockers::default();
            let mut calls = 0;
            let error = fetch(
                "https://example.com",
                &AtomicBool::new(false),
                &blockers,
                |backend, _| {
                    assert_eq!(backend, "http");
                    calls += 1;
                    Err(failure(rejected))
                },
            )
            .unwrap_err();
            assert_eq!(calls, 1);
            assert_eq!(code(&error).as_deref(), Some(rejected));
            assert!(blockers.lock().unwrap().is_empty());
        }
    }
}
