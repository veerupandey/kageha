//! Stateless background reads. These never use an interactive profile.
use crate::cdp::{field, Result};
use ego_tree::iter::Edge;
use encoding_rs::{Encoding, UTF_8};
use scraper::{Html, Selector};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;
use std::process::Stdio;
use std::sync::LazyLock;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio_tungstenite::tungstenite::http::Uri;

// Download allowance is independent of the much smaller model-facing text budget.
// Command output also includes curl metadata or the Lightpanda JSON envelope.
const MAX_RESPONSE_BYTES: u64 = 10 * 1024 * 1024;
const MAX_OUTPUT: u64 = MAX_RESPONSE_BYTES + 64 * 1024;
pub(crate) const MAX_CHARS: usize = 8000;
pub(crate) const MAX_SOURCE_CHARS: usize = 128_000;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReadPage {
    pub backend: Backend,
    pub url: String,
    #[serde(default = "default_limit")]
    pub max_chars: usize,
    #[serde(default)]
    pub offset: usize,
    #[serde(default)]
    pub title_contains: String,
    #[serde(default)]
    pub retain_text: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Backend {
    Http,
    Lightpanda,
    Chromium,
}

fn default_limit() -> usize {
    6000
}

pub(crate) fn validate_url(url: &str) -> Result<()> {
    let uri: Uri = url.parse().map_err(|_| "research_invalid_url")?;
    if url.len() > 2048
        || !matches!(uri.scheme_str(), Some("http" | "https"))
        || !uri
            .authority()
            .is_some_and(|authority| !authority.as_str().contains('@'))
    {
        return Err("research_invalid_url");
    }
    Ok(())
}

pub(crate) fn page(
    backend: &str,
    url: &str,
    status: u64,
    content_type: &str,
    format: &str,
    title: &str,
    text: &str,
    max_chars: usize,
) -> Result<Value> {
    validate_url(url)?;
    let mut chars = text.chars();
    let text: String = chars.by_ref().take(max_chars.min(MAX_CHARS)).collect();
    Ok(
        json!({"backend": backend, "url": url, "status": status, "ok": (200..400).contains(&status),
              "content_type": content_type.chars().take(200).collect::<String>(), "format": format,
              "title": title.chars().take(200).collect::<String>(), "text": text,
              "truncated": chars.next().is_some()}),
    )
}

/// Internal host retention is separate from the small model-facing preview.
pub(crate) fn retain_text(page: &mut Value, text: &str, requested: bool) {
    if !requested {
        return;
    }
    let mut chars = text.chars();
    page["source_text"] = json!(chars.by_ref().take(MAX_SOURCE_CHARS).collect::<String>());
    page["source_truncated"] = json!(chars.next().is_some());
}

async fn bounded_output(command: &mut Command) -> Result<Vec<u8>> {
    let mut child = command
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| "research_backend_unavailable")?;
    let mut output = Vec::new();
    child
        .stdout
        .take()
        .ok_or("research_read_failed")?
        .take(MAX_OUTPUT + 1)
        .read_to_end(&mut output)
        .await
        .map_err(|_| "research_read_failed")?;
    if output.len() as u64 > MAX_OUTPUT {
        child.kill().await.ok();
        return Err("research_response_too_large");
    }
    if !child
        .wait()
        .await
        .map_err(|_| "research_read_failed")?
        .success()
    {
        return Err("research_read_failed");
    }
    Ok(output)
}

pub(crate) async fn lightpanda(executable: &Path, input: &ReadPage) -> Result<Value> {
    let response_limit = MAX_RESPONSE_BYTES.to_string();
    let output = bounded_output(
        Command::new(executable)
            .env("LIGHTPANDA_DISABLE_TELEMETRY", "true")
            .args([
                "fetch",
                &input.url,
                "--json",
                "--dump",
                "markdown",
                "--wait-until",
                "load",
                "--with-frames",
                "--terminate-ms",
                "12000",
                "--http-timeout",
                "12000",
                "--http-max-response-size",
                &response_limit,
            ]),
    )
    .await?;
    let result: Value = serde_json::from_slice(&output).map_err(|_| "research_invalid_reply")?;
    page(
        "lightpanda",
        field(&result, "url")?,
        result["http_status"]
            .as_u64()
            .ok_or("research_read_failed")?,
        "text/markdown",
        "markdown",
        "",
        result["content"].as_str().unwrap_or(""),
        input.max_chars,
    )
}

pub(crate) async fn http(executable: Option<&Path>, input: &ReadPage) -> Result<Value> {
    let Some(executable) = executable else {
        return native_http(input).await;
    };
    let response_limit = MAX_RESPONSE_BYTES.to_string();
    let output = bounded_output(Command::new(executable).args([
        "-q",
        "--silent",
        "--globoff",
        "--location",
        "--max-redirs",
        "8",
        "--max-time",
        "15",
        "--proto",
        "=http,https",
        "--proto-redir",
        "=http,https",
        "--compressed",
        "--user-agent",
        "KagehaAgent/0.4 (+https://github.com/kageha; research fetch; compatible; like Firefox)",
        "--header",
        "Accept: text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        "--header",
        "Accept-Language: en-US,en;q=0.9",
        "--max-filesize",
        &response_limit,
        "--write-out",
        "\n%{json}",
        "--url",
        &input.url,
    ]))
    .await?;
    let split = output
        .iter()
        .rposition(|byte| *byte == b'\n')
        .ok_or("research_invalid_reply")?;
    let metadata: Value =
        serde_json::from_slice(&output[split + 1..]).map_err(|_| "research_invalid_reply")?;
    if split as u64 > MAX_RESPONSE_BYTES {
        return Err("research_response_too_large");
    }
    let kind = metadata["content_type"].as_str().unwrap_or("");
    let url = field(&metadata, "url_effective")?;
    let status = metadata["http_code"]
        .as_u64()
        .ok_or("research_invalid_reply")?;
    http_page(input, url, status, kind, &output[..split])
}

async fn native_http(input: &ReadPage) -> Result<Value> {
    static CLIENT: LazyLock<Option<reqwest::Client>> = LazyLock::new(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::limited(8))
            .user_agent("KagehaAgent/0.4")
            .build()
            .ok()
    });
    let client = CLIENT.as_ref().ok_or("research_backend_unavailable")?;
    let mut response = client
        .get(&input.url)
        .header(
            "Accept",
            "application/json,text/html,application/xhtml+xml,text/plain,*/*;q=0.8",
        )
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                "deadline_exceeded"
            } else {
                "research_read_failed"
            }
        })?;
    let status = response.status().as_u16().into();
    let url = response.url().to_string();
    let kind = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "research_read_failed")? {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES as usize {
            return Err("research_response_too_large");
        }
        body.extend_from_slice(&chunk);
    }
    http_page(input, &url, status, &kind, &body)
}

fn http_page(input: &ReadPage, url: &str, status: u64, kind: &str, bytes: &[u8]) -> Result<Value> {
    let encoding = kind
        .split(';')
        .skip(1)
        .filter_map(|part| part.split_once('='))
        .find(|(key, _)| key.trim().eq_ignore_ascii_case("charset"))
        .and_then(|(_, label)| {
            Encoding::for_label(label.trim().trim_matches(['"', '\'']).as_bytes())
        })
        .unwrap_or(UTF_8);
    let (body, _, _) = encoding.decode(bytes);
    if (200..300).contains(&status) && kind.to_ascii_lowercase().contains("json") {
        let mut result = page(
            "http",
            url,
            status,
            kind,
            "json",
            "",
            &body,
            input.max_chars,
        )?;
        if greenhouse_board(url) {
            let records = greenhouse_jobs(&body, input)?;
            result["format"] = json!("job_feed");
            result["text"] = json!("Structured Greenhouse job feed. Use jobs and next_offset.");
            result["truncated"] = json!(records["next_offset"].is_number());
            result["feed"] = records;
        } else {
            serde_json::from_str::<Value>(&body).map_err(|_| "research_invalid_json")?;
            retain_text(&mut result, &body, input.retain_text);
        }
        return Ok(result);
    }
    if (200..300).contains(&status) && kind.to_ascii_lowercase().contains("xml") {
        if let Ok(feed) = quick_xml::de::from_str::<Rss>(&body) {
            let mut result = page(
                "http",
                url,
                status,
                kind,
                "rss",
                "",
                "RSS article index",
                input.max_chars,
            )?;
            result["items"] = json!(feed.channel.item.into_iter().take(12).collect::<Vec<_>>());
            return Ok(result);
        }
    }
    if (200..400).contains(&status)
        && (kind.to_ascii_lowercase().contains("html")
            || body
                .trim_start()
                .get(..9)
                .is_some_and(|start| start.eq_ignore_ascii_case("<!doctype")))
    {
        let document = Html::parse_document(&body);
        let (title, text, links) = extract_html(&document);
        let mut result = page(
            "http",
            url,
            status,
            kind,
            "extract",
            &title,
            &text,
            input.max_chars,
        )?;
        result["links"] = json!(links);
        retain_text(&mut result, &text, input.retain_text);
        if let Some(article) = article_body(&document, input.max_chars.min(MAX_CHARS)) {
            result["article_text"] = json!(article);
            let headline = Selector::parse("meta[property='og:title']").unwrap();
            result["article_title"] = json!(document
                .select(&headline)
                .next()
                .and_then(|tag| tag.value().attr("content"))
                .unwrap_or(&title)
                .chars()
                .take(300)
                .collect::<String>());
            let canonical = Selector::parse("link[rel='canonical']").unwrap();
            if let Some(link) = document
                .select(&canonical)
                .next()
                .and_then(|tag| tag.value().attr("href"))
                .and_then(|href| url::Url::parse(url).ok()?.join(href).ok())
            {
                result["canonical_url"] = json!(link.as_str());
            }
        }
        Ok(result)
    } else {
        let mut result = page(
            "http",
            url,
            status,
            kind,
            "text",
            "",
            &body,
            input.max_chars,
        )?;
        retain_text(&mut result, &body, input.retain_text);
        Ok(result)
    }
}

#[derive(Deserialize)]
struct Rss {
    channel: RssChannel,
}

#[derive(Deserialize)]
struct RssChannel {
    #[serde(default)]
    item: Vec<RssItem>,
}

#[derive(Deserialize, serde::Serialize)]
struct RssItem {
    #[serde(default)]
    title: String,
    #[serde(default)]
    link: String,
    #[serde(default, rename = "pubDate")]
    published: String,
    #[serde(default)]
    category: Vec<String>,
}

fn article_body(doc: &Html, limit: usize) -> Option<String> {
    let kind = Selector::parse("meta[property='og:type'][content='article']").unwrap();
    let scope = Selector::parse("article, [itemprop='articleBody']").unwrap();
    let paragraphs = Selector::parse("article p, [itemprop='articleBody'] p").unwrap();
    let main = Selector::parse("main p").unwrap();
    let selector = if doc.select(&scope).next().is_some() {
        &paragraphs
    } else if doc.select(&kind).next().is_some() {
        &main
    } else {
        return None;
    };
    let mut text = String::new();
    for paragraph in doc.select(selector) {
        let line = paragraph.text().collect::<String>();
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.chars().count() >= 50 {
            text.push_str(&line);
            text.push('\n');
            if text.len() >= limit {
                break;
            }
        }
    }
    (text.chars().count() >= 240).then(|| text.chars().take(limit).collect())
}

fn greenhouse_board(url: &str) -> bool {
    let Ok(url) = url::Url::parse(url) else {
        return false;
    };
    let parts: Vec<_> = url.path().trim_matches('/').split('/').collect();
    url.host_str() == Some("boards-api.greenhouse.io")
        && matches!(parts.as_slice(), ["v1", "boards", _, "jobs"])
}

fn greenhouse_jobs(body: &str, input: &ReadPage) -> Result<Value> {
    let value: Value = serde_json::from_str(body).map_err(|_| "research_invalid_reply")?;
    let jobs = value["jobs"].as_array().ok_or("research_invalid_reply")?;
    let query = input.title_contains.trim().to_lowercase();
    let matching: Vec<_> = jobs
        .iter()
        .filter(|job| {
            job["title"]
                .as_str()
                .is_some_and(|title| title.to_lowercase().contains(&query))
        })
        .collect();
    let offset = input.offset.min(matching.len());
    let end = (offset + 20).min(matching.len());
    let records: Vec<_> = matching[offset..end]
        .iter()
        .map(|job| {
            let clipped = |value: &Value| {
                value
                    .as_str()
                    .map(|text| text.chars().take(200).collect::<String>())
            };
            let url = job["absolute_url"]
                .as_str()
                .filter(|url| url.starts_with("https://") && validate_url(url).is_ok());
            json!({
                "id": job["id"].as_u64(), "title": clipped(&job["title"]),
                "location": clipped(&job["location"]["name"]),
                "url": url, "updated_at": clipped(&job["updated_at"])
            })
        })
        .collect();
    Ok(
        json!({"total_jobs":jobs.len(),"matches":matching.len(),"jobs":records,"offset":offset,
        "next_offset":(end < matching.len()).then_some(end)}),
    )
}

fn extract_html(document: &Html) -> (String, String, Vec<(String, String)>) {
    let title = document
        .select(&Selector::parse("title").expect("static title selector"))
        .next()
        .map(|element| element.text().collect::<String>())
        .unwrap_or_default();
    let mut text = String::new();
    let mut links = Vec::new();
    let mut link = None;
    let mut skipped = None;
    if let Some(body) = document
        .select(&Selector::parse("body").expect("static body selector"))
        .next()
    {
        for edge in body.traverse() {
            match edge {
                Edge::Close(node) if skipped == Some(node.id()) => skipped = None,
                _ if skipped.is_some() => {}
                Edge::Open(node) => {
                    if let Some(element) = node.value().as_element() {
                        match element.name() {
                            "script" | "style" | "noscript" | "svg" | "iframe" | "nav"
                            | "footer" | "aside" => {
                                skipped = Some(node.id());
                            }
                            "a" if links.len() < 12 => {
                                link = element
                                    .attr("href")
                                    .filter(|href| {
                                        !href.is_empty()
                                            && href.len() <= 320
                                            && !href.chars().any(char::is_control)
                                    })
                                    .map(|href| (href, text.len(), node.id()));
                            }
                            "p" | "br" | "div" | "li" | "tr" | "h1" | "h2" | "h3" | "h4"
                            | "section" | "article" => {
                                text.push('\n');
                            }
                            _ => {}
                        }
                    } else if let Some(part) = node.value().as_text() {
                        let part = part.trim();
                        if !part.is_empty() {
                            text.push_str(part);
                            text.push(' ');
                        }
                    }
                }
                Edge::Close(node) => {
                    if let Some((href, start, _)) = link.filter(|(_, _, id)| *id == node.id()) {
                        let label: String = text[start..]
                            .split_whitespace()
                            .collect::<Vec<_>>()
                            .join(" ")
                            .chars()
                            .take(80)
                            .collect();
                        if !label.is_empty() {
                            links.push((label, href.to_owned()));
                        }
                        link = None;
                    }
                }
            }
        }
    }
    (title, text.trim().to_owned(), links)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn retained_extraction_is_separate_from_the_model_preview() {
        let mut input: ReadPage = serde_json::from_value(json!({"backend":"http","url":"https://example.com","retain_text":true})).unwrap();
        let text = format!("{}🌲needle", "é".repeat(9000));
        let body = format!("<title>Long source</title><body><p>{text}</p></body>");
        let retained = http_page(&input, &input.url, 200, "text/html", body.as_bytes()).unwrap();
        assert_eq!(retained["text"].as_str().unwrap().chars().count(), input.max_chars);
        assert_eq!(retained["source_text"], text);
        assert_eq!(retained["source_truncated"], false);
        input.retain_text = false;
        assert!(http_page(&input, &input.url, 200, "text/html", body.as_bytes()).unwrap().get("source_text").is_none());
        let mut page = json!({});
        retain_text(&mut page, &"🌲".repeat(MAX_SOURCE_CHARS + 1), true);
        assert_eq!(page["source_text"].as_str().unwrap().chars().count(), MAX_SOURCE_CHARS);
        assert_eq!(page["source_truncated"], true);
        assert!(serde_json::to_vec(&page).unwrap().len() < 1024 * 1024);
    }

    #[test]
    fn news_index_is_structured_and_article_body_is_distinct() {
        let input: ReadPage = serde_json::from_value(json!({
            "backend": "http", "url": "https://example.com/news"
        }))
        .unwrap();
        let rss = br#"<rss><channel><item><title>Example &amp; test</title><link>https://example.com/news/a?x=1&amp;y=2</link><pubDate>Mon, 09 Nov 2026 16:40:46 +0000</pubDate></item></channel></rss>"#;
        let index = http_page(&input, &input.url, 200, "application/rss+xml", rss).unwrap();
        assert_eq!(index["format"], "rss");
        assert_eq!(
            index["items"][0]["link"],
            "https://example.com/news/a?x=1&y=2"
        );
        assert_eq!(
            index["items"][0]["pubDate"],
            "Mon, 09 Nov 2026 16:40:46 +0000"
        );
        assert!(index["article_text"].is_null());
        let line = "The report describes a verified public update with enough context to read.";
        let article = format!(
            "<title>Example</title><nav>Header clutter</nav><article>{}</article>",
            format!("<p>{line}</p>").repeat(4)
        );
        let page = http_page(&input, &input.url, 200, "text/html", article.as_bytes()).unwrap();
        let content = page["article_text"].as_str().unwrap();
        assert!(content.contains(line));
        assert!(!content.contains("Header clutter"));
        let index = format!("<main>{}</main>", format!("<p>{line}</p>").repeat(4));
        assert!(article_body(&Html::parse_document(&index), 1000).is_none());
    }

    #[tokio::test]
    async fn http_works_without_an_external_adapter() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 2048];
            stream.read(&mut request).await.unwrap();
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\nConnection: close\r\n\r\n{\"ok\":true}").await.unwrap();
        });
        let input = serde_json::from_value(json!({"url":url,"backend":"http"})).unwrap();
        let result = http(None, &input).await.unwrap();
        assert_eq!(result["status"], 200);
        assert_eq!(result["format"], "json");
        assert_eq!(result["text"], "{\"ok\":true}");
        server.await.unwrap();
    }

    #[test]
    fn job_feed_filters_full_response_and_paginates_matches() {
        let url = "https://boards-api.greenhouse.io/v1/boards/example/jobs";
        let mut input: ReadPage = serde_json::from_value(json!({
            "url":url,"backend":"http","max_chars":8000,"title_contains":"machine learning"
        }))
        .unwrap();
        let mut jobs = vec![json!({"title":"Other role","content":"a".repeat(10_000)})];
        jobs.extend((1..=21).map(|id| {
            json!({
                "id":id,"title":"Machine Learning Engineer","location":{"name":"Canada"}
            })
        }));
        let body = json!({"jobs":jobs}).to_string();
        let first = http_page(&input, url, 200, "application/json", body.as_bytes()).unwrap();
        assert_eq!(first["format"], "job_feed");
        assert_eq!(first["feed"]["matches"], 21);
        assert_eq!(first["feed"]["jobs"][0]["id"], 1);
        assert_eq!(first["feed"]["next_offset"], 20);
        input.offset = 20;
        let last = http_page(&input, url, 200, "application/json", body.as_bytes()).unwrap();
        assert_eq!(last["feed"]["jobs"][0]["id"], 21);
        assert!(last["feed"]["next_offset"].is_null());
    }

    #[tokio::test]
    async fn large_downloads_keep_a_small_text_preview_and_reject_overflow() {
        // Exercise actual transports: modern HTML can exceed 1 MiB mostly due
        // to scripts, even though the agent only needs a short body excerpt.
        let article = "Useful public source text. ".repeat(500);
        let body = format!(
            "<title>Large page</title><script>{}</script><main>{article}</main>",
            "x".repeat(1024 * 1024)
        );
        for executable in [None, Some(Path::new("/usr/bin/curl"))] {
            if executable.is_some_and(|path| !path.exists()) {
                continue;
            }
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/", listener.local_addr().unwrap());
            let response = body.clone();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 2048];
                stream.read(&mut request).await.unwrap();
                let header = format!("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", response.len());
                stream.write_all(header.as_bytes()).await.unwrap();
                stream.write_all(response.as_bytes()).await.unwrap();
            });
            let input = serde_json::from_value(json!({"url":url,"backend":"http","max_chars":200}))
                .unwrap();
            let result = http(executable, &input).await.unwrap();
            let text = result["text"].as_str().unwrap();
            assert!(text.contains("Useful public source text"));
            assert!(text.chars().count() <= 200);
            assert_eq!(result["truncated"], true);
            server.await.unwrap();
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0; 2048];
            stream.read(&mut request).await.unwrap();
            let response = vec![b'x'; MAX_RESPONSE_BYTES as usize + 1];
            let header = format!("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", response.len());
            stream.write_all(header.as_bytes()).await.unwrap();
            // The reader is allowed to close as soon as its bound is reached.
            let _ = stream.write_all(&response).await;
        });
        let input = serde_json::from_value(json!({"url":url,"backend":"http"})).unwrap();
        assert_eq!(
            http(None, &input).await.unwrap_err(),
            "research_response_too_large"
        );
        server.await.unwrap();
    }
}
