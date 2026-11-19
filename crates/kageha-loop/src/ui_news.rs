use crate::{native_chat::ToolHost, ui_tools::Tools, webui::emit};
use serde_json::{json, Value};
use std::io;
use std::sync::atomic::Ordering;

const FEEDS: &[(&str, &str, &[&str])] = &[
    (
        "CBC News",
        "https://www.cbc.ca/webfeed/rss/rss-topstories",
        &["www.cbc.ca"],
    ),
    (
        "BBC News",
        "https://feeds.bbci.co.uk/news/world/rss.xml",
        &["www.bbc.co.uk", "www.bbc.com"],
    ),
    (
        "Global News",
        "https://globalnews.ca/canada/feed/",
        &["globalnews.ca"],
    ),
];

// Only unqualified requests for a general briefing get the default source set.
// Topic, locality, source and format instructions stay with the ordinary agent.
pub(crate) fn is_briefing(message: &str) -> bool {
    let normalized = message
        .trim()
        .trim_end_matches(['?', '.', '!'])
        .to_lowercase()
        .replace('’', "'");
    matches!(
        normalized.as_str(),
        "what's the news today"
            | "what is the news today"
            | "what's in the news today"
            | "today's news"
            | "give me today's news"
            | "today's headlines"
    )
}

fn publisher_url(link: &str, hosts: &[&str]) -> Option<String> {
    let mut url = url::Url::parse(link).ok()?;
    if url.scheme() != "https"
        || !url.host_str().is_some_and(|h| hosts.contains(&h))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || !url.path().starts_with("/news/")
        || url.path() == "/news/"
    {
        return None;
    }
    url.set_query(None);
    url.set_fragment(None);
    Some(url.into())
}

fn terms(text: &str) -> std::collections::HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.chars().count() >= 3)
        .map(str::to_lowercase)
        .collect()
}

fn overlaps(expected: &str, actual: &str) -> bool {
    let expected = terms(expected);
    let actual = terms(actual);
    let shared = expected.intersection(&actual).count();
    !expected.is_empty() && shared >= expected.len().min(2) && shared * 3 >= expected.len() * 2
}

fn identity(raw: &str) -> Option<(String, String)> {
    let url = url::Url::parse(raw).ok()?;
    if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    let host = url.host_str()?.trim_start_matches("www.");
    let host = if matches!(host, "bbc.co.uk" | "bbc.com") {
        "bbc.com"
    } else {
        host
    };
    Some((host.into(), url.path().trim_end_matches('/').into()))
}

pub(crate) fn article_source(
    page: &Value,
    requested: &str,
    expected_title: Option<&str>,
) -> Option<Value> {
    if page["ok"] != true || page["article_text"].as_str()?.trim().is_empty() {
        return None;
    }
    let effective = page["url"].as_str()?;
    let resolved = identity(effective)?;
    if identity(requested)? != resolved
        || page["canonical_url"]
            .as_str()
            .is_some_and(|url| identity(url).as_ref() != Some(&resolved))
    {
        return None;
    }
    let title = page["article_title"].as_str()?.trim();
    if title.is_empty() || expected_title.is_some_and(|expected| !overlaps(expected, title)) {
        return None;
    }
    Some(json!({"url":effective,"title":title}))
}

pub(crate) fn search(host: &Tools<'_>, query: &str) -> io::Result<String> {
    let mut results = Vec::new();
    let mut checked = 0;
    let mut unavailable = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for &(publisher, feed, hosts) in FEEDS {
        if host.cancel.load(Ordering::Relaxed) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "Cancelled"));
        }
        let call = json!({"id":crate::browser_client::unique_id(),"name":"web_fetch","arguments":{"url":feed}});
        let page = host
            .invoke(&call)
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok());
        let Some(page) = page.filter(|page| page["format"] == "rss" && page["items"].is_array())
        else {
            unavailable.push(publisher);
            continue;
        };
        checked += 1;
        for item in page["items"].as_array().into_iter().flatten() {
            let (Some(title), Some(link)) = (item["title"].as_str(), item["link"].as_str()) else {
                continue;
            };
            if !overlaps(query, title) {
                continue;
            }
            if let Some(url) = publisher_url(link, hosts).filter(|url| seen.insert(url.clone())) {
                results.push(json!({"url":url,"title":title,"publisher":publisher,"published":item["pubDate"]}));
            }
        }
    }
    results.truncate(12);
    Ok(json!({"coverage":"Recent headlines from BBC News, CBC News and Global News publisher feeds",
        "evidence":"discovery_only","results":results,"feeds_checked":checked,"unavailable_publishers":unavailable,
        "status":if checked==0 {"unavailable"} else if results.is_empty() {"no_matches"} else {"ok"},
        "next":"Read a direct URL with web_fetch and pass its title as expected_title. A headline is not article evidence."}).to_string())
}

fn read(host: &Tools<'_>, url: &str, expected_title: Option<&str>) -> io::Result<Option<Value>> {
    if host.cancel.load(Ordering::Relaxed) {
        return Ok(None);
    }
    let id = crate::browser_client::unique_id();
    emit(
        host.db,
        host.chat,
        host.turn,
        "tool_card",
        json!({"id":id,"name":"web_fetch","status":"running"}),
    )?;
    let result = host.invoke(&json!({"id":id,"name":"web_fetch","arguments":{"url":url,"expected_title":expected_title}}));
    let data = result
        .as_ref()
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok());
    let source = data
        .as_ref()
        .and_then(|page| page.get("article_source"))
        .cloned();
    let success = data.as_ref().is_some_and(|page| {
        page["ok"] == true
            && if expected_title.is_some() {
                source.is_some()
            } else {
                page["format"] == "rss"
            }
    });
    let mut card = json!({"id":id,"name":"web_fetch","status":if success {"done"} else {"failed"},
        "result_preview":if success { format!("Read {url}") } else { "Could not verify readable source content.".into() }});
    if let Some(source) = source {
        card["observedSource"] = source;
    }
    emit(host.db, host.chat, host.turn, "tool_card", card)?;
    Ok(data.filter(|_| success))
}

fn read_publisher(
    host: &Tools<'_>,
    publisher: &str,
    feed: &str,
    hosts: &[&str],
) -> io::Result<Vec<Value>> {
    let Some(index) = read(host, feed, None)? else {
        return Ok(vec![]);
    };
    let now = chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now());
    let mut articles = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (url, date, title) in index["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let date = chrono::DateTime::parse_from_rfc2822(item["pubDate"].as_str()?).ok()?;
            let age = now.signed_duration_since(date);
            if age < chrono::TimeDelta::hours(-1)
                || age > chrono::TimeDelta::hours(36)
                || item["category"].as_array().is_some_and(|cats| {
                    cats.iter().any(|cat| {
                        cat.as_str()
                            .is_some_and(|cat| cat.eq_ignore_ascii_case("advertisement"))
                    })
                })
            {
                return None;
            }
            let url = publisher_url(item["link"].as_str()?, hosts)?;
            seen.insert(url.clone())
                .then_some((url, date, item["title"].as_str()?))
        })
        .take(3)
    {
        if let Some(page) = read(host, &url, Some(title))? {
            articles.push(json!({"publisher":publisher,
                "url":page["article_source"]["url"],
                "title":page["article_source"]["title"],"published":date.to_rfc3339(),
                "excerpt":page["article_text"].as_str().unwrap_or("").chars().take(1100).collect::<String>()}));
            if articles.len() == 2 {
                break;
            }
        }
    }
    Ok(articles)
}

pub(crate) fn collect(host: &Tools<'_>) -> io::Result<Vec<Value>> {
    emit(
        host.db,
        host.chat,
        host.turn,
        "event",
        json!({
            "kind":"assistant_progress","label":"Reading sources…","interesting":false,
            "payload":{"text":"I’ll read recent articles from several publishers and link the stories in the roundup.","origin":"host"}
        }),
    )?;
    std::thread::scope(|scope| {
        let reads: Vec<_> = FEEDS
            .iter()
            .map(|&(name, feed, hosts)| {
                scope.spawn(move || read_publisher(host, name, feed, hosts))
            })
            .collect();
        let mut articles = Vec::new();
        for read in reads {
            articles.extend(
                read.join()
                    .map_err(|_| io::Error::other("News reader stopped"))??,
            );
        }
        Ok(articles)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_evidence_requires_matching_article_content_and_identity() {
        let url = "https://publisher.example/news/border-wall";
        let expected = "Supreme Court clears border wall construction";
        let mut page = json!({
            "ok": true, "url": url, "title": "Publisher news feed",
            "format": "rss", "items": [{"title": expected, "link": url}]
        });
        assert!(article_source(&page, url, Some(expected)).is_none());

        page["article_text"] = json!("Article body extracted from the publisher.");
        page["article_title"] = json!("22 gifts editors are giving in 2024");
        page["canonical_url"] = json!("https://publisher.example/shop/gifts");
        assert!(article_source(&page, url, None).is_none());
        page["canonical_url"] = json!(url);
        assert!(article_source(&page, url, Some(expected)).is_none());
        page["article_title"] = json!("Supreme Court clears border wall construction in Texas");
        assert_eq!(
            article_source(&page, url, Some(expected)).unwrap()["url"],
            url
        );
        page["url"] = json!("https://publisher.example/news/other");
        assert!(article_source(&page, url, Some(expected)).is_none());
    }

    #[test]
    fn defaults_only_for_unqualified_briefings() {
        assert!(is_briefing("What’s the news today"));
        assert!(is_briefing(" What is the news today? "));
        assert!(!is_briefing("What’s the news today about energy?"));
        assert!(!is_briefing(
            "Summarize what she said about today's headlines"
        ));
    }

    #[test]
    fn news_index_cannot_redirect_research_to_an_unrelated_origin() {
        let hosts = &["www.cbc.ca"];
        assert_eq!(
            publisher_url("https://www.cbc.ca/news/story?cmp=rss#top", hosts).as_deref(),
            Some("https://www.cbc.ca/news/story")
        );
        for link in [
            "https://www.cbc.ca.evil.example/news/story",
            "https://user@www.cbc.ca/news/story",
            "https://www.cbc.ca:8443/news/story",
            "https://www.cbc.ca/news/",
            "https://www.cbc.ca/webfeed/rss/rss-topstories",
        ] {
            assert!(publisher_url(link, hosts).is_none(), "{link}");
        }
    }
}
