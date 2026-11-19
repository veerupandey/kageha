//! General open-web search for the `web_search` tool.
//!
//! The backend comes from `search.yaml` (`~/.kageha/search.yaml`, then the workspace's
//! `.kageha/search.yaml`), the turn's search preference and the `KAGEHA_WEB_SEARCH` /
//! `KAGEHA_WEB_SEARCH_ORDER` environment overrides. Tavily and Brave need an API key from
//! the credential vault (or `TAVILY_API_KEY` / `BRAVE_API_KEY`); DuckDuckGo's HTML endpoint
//! is the keyless fallback. Keys are only ever sent as request headers and never appear
//! in results, notes or errors.
use crate::webui::Database;
use serde_json::{json, Value};
use std::{io, path::{Path, PathBuf}, sync::atomic::AtomicBool, time::Duration};
use zeroize::Zeroizing;

pub(crate) const MAX_RESULTS: usize = 10;
pub(crate) const MAX_SNIPPET_CHARS: usize = 500;
const MAX_TITLE_CHARS: usize = 300;
const MAX_CONFIG_BYTES: u64 = 64 * 1024;
const RESPONSE_LIMIT: usize = 4 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(20);
/// Same default auto order as the Settings → Search panel. Entries this tool cannot
/// use (Z.AI, Exa, Perplexity, Gemini) are skipped.
const DEFAULT_ORDER: &[&str] = &["zai", "brave", "tavily", "exa", "perplexity", "gemini", "ddg"];
const NEXT: &str = "Results are leads, not evidence. Read a result URL with web_fetch before citing it.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Backend { Tavily, Brave, Ddg }
impl Backend {
    fn parse(id: &str) -> Option<Self> {
        match id.trim().to_ascii_lowercase().as_str() {
            "tavily" => Some(Self::Tavily),
            "brave" => Some(Self::Brave),
            "ddg" | "duckduckgo" => Some(Self::Ddg),
            _ => None,
        }
    }
    pub(crate) fn id(self) -> &'static str {
        match self { Self::Tavily => "tavily", Self::Brave => "brave", Self::Ddg => "ddg" }
    }
    /// Vault credential name and environment variable holding the API key.
    fn credential(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::Tavily => Some(("tavily", "TAVILY_API_KEY")),
            Self::Brave => Some(("brave", "BRAVE_API_KEY")),
            Self::Ddg => None,
        }
    }
}

/// The effective search configuration: a pinned backend id (or `auto`) and the auto order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Config { pub backend: String, pub order: Vec<String> }
impl Config {
    /// Layers `search.yaml` documents in order (later files win per field), then the
    /// turn preference, then the environment overrides.
    pub(crate) fn resolve(files: &[String], preference: Option<&str>, env: impl Fn(&str) -> Option<String>) -> Self {
        let mut config = Self { backend: "auto".into(), order: DEFAULT_ORDER.iter().map(|id| (*id).into()).collect() };
        for text in files {
            let Ok(document) = serde_yaml::from_str::<serde_yaml::Value>(text) else { continue };
            if let Some(backend) = document.get("backend").and_then(serde_yaml::Value::as_str).filter(|v| !v.trim().is_empty()) {
                config.backend = backend.trim().into();
            }
            if let Some(order) = document.get("order").and_then(serde_yaml::Value::as_sequence) {
                let order: Vec<String> = order.iter().filter_map(serde_yaml::Value::as_str).map(|id| id.trim().to_owned()).collect();
                if !order.is_empty() { config.order = order; }
            }
        }
        // Model-native pickers (codex, antigravity, native) do not choose this tool's engine.
        if let Some(preference) = preference.map(str::trim).filter(|v| v == &"auto" || DEFAULT_ORDER.contains(v)) {
            config.backend = preference.into();
        }
        if let Some(backend) = env("KAGEHA_WEB_SEARCH").filter(|v| !v.trim().is_empty()) {
            config.backend = backend.trim().into();
        }
        if let Some(order) = env("KAGEHA_WEB_SEARCH_ORDER") {
            let order: Vec<String> = order.split(',').map(str::trim).filter(|id| !id.is_empty()).map(str::to_owned).collect();
            if !order.is_empty() { config.order = order; }
        }
        config
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Selection { pub backend: Backend, pub note: Option<String> }

/// Picks the backend: a usable pinned backend, else the first usable entry of the auto
/// order, else DuckDuckGo. A keyed backend is usable only when `has_key` says so.
pub(crate) fn select(config: &Config, mut has_key: impl FnMut(Backend) -> bool) -> Selection {
    let mut usable = |backend: Backend| backend == Backend::Ddg || has_key(backend);
    let pinned = config.backend.trim().to_ascii_lowercase();
    let mut note = None;
    if !matches!(pinned.as_str(), "" | "auto") {
        match Backend::parse(&pinned) {
            Some(backend) if usable(backend) => return Selection { backend, note: None },
            Some(_) => note = Some(format!("The configured {pinned} search has no API key.")),
            None => note = Some(format!("The configured {pinned} search is not available to web_search.")),
        }
    }
    let backend = config.order.iter().filter_map(|id| Backend::parse(id)).find(|backend| usable(*backend)).unwrap_or(Backend::Ddg);
    Selection { backend, note: note.map(|note| format!("{note} Using {} instead.", backend.id())) }
}

pub(crate) fn config_paths(workspace: &Path) -> Vec<PathBuf> {
    let home = std::env::var_os("KAGEHA_HOME").map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".kageha")));
    home.map(|home| home.join("search.yaml")).into_iter().chain([workspace.join(".kageha/search.yaml")]).collect()
}

fn read_configs(paths: &[PathBuf]) -> Vec<String> {
    use std::io::Read;
    let mut seen = std::collections::BTreeSet::new();
    paths.iter().filter(|path| seen.insert(path.canonicalize().unwrap_or_else(|_| path.to_path_buf())))
        .filter_map(|path| {
            let mut text = String::new();
            std::fs::File::open(path).ok()?.take(MAX_CONFIG_BYTES).read_to_string(&mut text).ok()?;
            Some(text)
        }).collect()
}

/// The backend's API key from the credential vault, or its environment variable when
/// the vault does not manage that name. Lookup failures count as no key.
pub(crate) fn api_key(db: &rusqlite::Connection, backend: Backend, env: impl Fn(&str) -> Option<String>) -> Option<Zeroizing<String>> {
    let (name, variable) = backend.credential()?;
    let saved = Zeroizing::new(crate::ui_settings::secret(db, name).unwrap_or_default());
    if !saved.trim().is_empty() {
        return Some(saved);
    }
    if crate::ui_credentials::managed(db, name).unwrap_or(false) {
        return None;
    }
    Some(Zeroizing::new(kageha_models::catalog::credential(variable, env))).filter(|key| !key.trim().is_empty())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Hit { pub title: String, pub url: String, pub snippet: String }

fn collapse(text: &str) -> String { text.split_whitespace().collect::<Vec<_>>().join(" ") }
fn bounded(text: &str, chars: usize) -> String {
    if text.chars().count() <= chars { return text.into(); }
    let mut out: String = text.chars().take(chars - 1).collect();
    out.push('…');
    out
}

fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let entity = rest.find(';').filter(|end| *end <= 10).map(|end| (&rest[1..end], end));
        let decoded = entity.and_then(|(name, end)| {
            let ch = match name {
                "amp" => Some('&'), "lt" => Some('<'), "gt" => Some('>'), "quot" => Some('"'),
                "apos" => Some('\''), "nbsp" => Some(' '),
                _ => name.strip_prefix("#x").or_else(|| name.strip_prefix("#X"))
                    .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                    .or_else(|| name.strip_prefix('#').and_then(|dec| dec.parse().ok()))
                    .and_then(char::from_u32),
            }?;
            Some((ch, end))
        });
        match decoded {
            Some((ch, end)) => { out.push(ch); rest = &rest[end + 1..]; }
            None => { out.push('&'); rest = &rest[1..]; }
        }
    }
    out.push_str(rest);
    out
}

/// Removes markup and decodes entities, for HTML snippets.
fn html_text(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => { in_tag = false; text.push(' '); }
            _ if !in_tag => text.push(ch),
            _ => {}
        }
    }
    collapse(&decode_entities(&text))
}

fn web_url(raw: &str) -> Option<String> {
    let url = url::Url::parse(raw.trim()).ok()?;
    (matches!(url.scheme(), "http" | "https") && url.host_str().is_some() && url.username().is_empty() && url.password().is_none())
        .then(|| url.into())
}

fn hit(title: &str, url: &str, snippet: &str) -> Option<Hit> {
    let url = web_url(url)?;
    let title = collapse(title);
    Some(Hit { title: bounded(if title.is_empty() { &url } else { &title }, MAX_TITLE_CHARS), snippet: bounded(&collapse(snippet), MAX_SNIPPET_CHARS), url })
}

/// Tavily `/search` response: `{results: [{title, url, content}]}`.
pub(crate) fn parse_tavily(body: &Value) -> Vec<Hit> {
    body["results"].as_array().into_iter().flatten()
        .filter_map(|row| hit(row["title"].as_str().unwrap_or(""), row["url"].as_str()?, row["content"].as_str().unwrap_or("")))
        .collect()
}

/// Brave web search response: `{web: {results: [{title, url, description}]}}`. Titles and
/// descriptions carry `<strong>` highlighting.
pub(crate) fn parse_brave(body: &Value) -> Vec<Hit> {
    body["web"]["results"].as_array().into_iter().flatten()
        .filter_map(|row| hit(&html_text(row["title"].as_str().unwrap_or("")), row["url"].as_str()?, &html_text(row["description"].as_str().unwrap_or(""))))
        .collect()
}

/// The publisher URL behind a DuckDuckGo result link (`//duckduckgo.com/l/?uddg=…`).
/// Ads and other DuckDuckGo-internal links resolve to `None`.
fn ddg_target(href: &str) -> Option<String> {
    let href = decode_entities(href.trim());
    let absolute = if href.starts_with("//") { format!("https:{href}") } else { href };
    let url = url::Url::parse(&absolute).ok()?;
    let internal = |url: &url::Url| url.host_str().is_some_and(|host| host == "duckduckgo.com" || host.ends_with(".duckduckgo.com"));
    let target = if internal(&url) && url.path() == "/l/" {
        url::Url::parse(&url.query_pairs().find(|(key, _)| key == "uddg")?.1).ok()?
    } else { url };
    (!internal(&target)).then(|| target.into())
}

fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let start = tag.find(&format!("{name}=\""))? + name.len() + 2;
    Some(&tag[start..start + tag[start..].find('"')?])
}

fn ddg_hit(html: &str, at: usize, block_end: usize) -> Option<Hit> {
    let tag_start = html[..at].rfind('<')?;
    let tag_end = at + html[at..].find('>')?;
    let url = ddg_target(attribute(&html[tag_start..tag_end], "href")?)?;
    let title_start = tag_end + 1;
    let title_end = title_start + html[title_start..].find("</a>")?;
    let block = &html[title_end..block_end.max(title_end)];
    let snippet = block.find("result__snippet").and_then(|at| {
        let open = at + block[at..].find('>')? + 1;
        let close = ["</a>", "</div>", "</td>"].iter().filter_map(|end| block[open..].find(end)).min()?;
        Some(html_text(&block[open..open + close]))
    }).unwrap_or_default();
    hit(&html_text(&html[title_start..title_end]), &url, &snippet)
}

/// DuckDuckGo HTML endpoint results: `a.result__a` title links and `.result__snippet` text.
pub(crate) fn parse_ddg(html: &str) -> Vec<Hit> {
    let anchors: Vec<usize> = html.match_indices("class=\"result__a\"").map(|(at, _)| at).collect();
    anchors.iter().enumerate()
        .filter_map(|(n, &at)| ddg_hit(html, at, anchors.get(n + 1).copied().unwrap_or(html.len())))
        .collect()
}

/// The bounded tool result: at most [`MAX_RESULTS`] unique URLs.
pub(crate) fn response(backend: Backend, query: &str, hits: Vec<Hit>, note: Option<&str>) -> Value {
    let mut seen = std::collections::HashSet::new();
    let results: Vec<Value> = hits.into_iter().filter(|hit| seen.insert(hit.url.clone())).take(MAX_RESULTS)
        .map(|hit| json!({"title":hit.title,"url":hit.url,"snippet":hit.snippet})).collect();
    let mut value = json!({"backend":backend.id(),"query":query,"results":results,"next":NEXT});
    if let Some(note) = note { value["note"] = json!(note); }
    value
}

fn checked(backend: Backend, result: io::Result<crate::native_io::Http>) -> io::Result<Vec<u8>> {
    let result = result?;
    if !(200..300).contains(&result.status) || (backend == Backend::Ddg && result.status != 200) {
        // DuckDuckGo answers 202 with a challenge page instead of results.
        return Err(io::Error::other(format!("{} search returned HTTP {}", backend.id(), result.status)));
    }
    Ok(result.bytes)
}

fn fetch(backend: Backend, key: Option<&str>, query: &str, cancel: &AtomicBool) -> io::Result<Vec<Hit>> {
    let encode = |value: &str| url::form_urlencoded::byte_serialize(value.as_bytes()).collect::<String>();
    match backend {
        Backend::Tavily => {
            let key = key.ok_or_else(|| io::Error::other("tavily search has no API key"))?;
            let headers = [("Authorization".to_owned(), format!("Bearer {key}")), ("Content-Type".to_owned(), "application/json".to_owned())];
            let body = serde_json::to_vec(&json!({"query":query,"max_results":MAX_RESULTS,"search_depth":"basic","include_answer":false}))?;
            let bytes = checked(backend, crate::native_io::http("https://api.tavily.com/search", "POST", &headers, body, cancel, TIMEOUT, RESPONSE_LIMIT))?;
            Ok(parse_tavily(&serde_json::from_slice(&bytes).map_err(|_| io::Error::other("tavily search returned an unreadable response"))?))
        }
        Backend::Brave => {
            let key = key.ok_or_else(|| io::Error::other("brave search has no API key"))?;
            let headers = [("Accept".to_owned(), "application/json".to_owned()), ("X-Subscription-Token".to_owned(), key.to_owned())];
            let url = format!("https://api.search.brave.com/res/v1/web/search?q={}&count={MAX_RESULTS}", encode(query));
            let bytes = checked(backend, crate::native_io::http(&url, "GET", &headers, vec![], cancel, TIMEOUT, RESPONSE_LIMIT))?;
            Ok(parse_brave(&serde_json::from_slice(&bytes).map_err(|_| io::Error::other("brave search returned an unreadable response"))?))
        }
        Backend::Ddg => {
            let headers = [("Content-Type".to_owned(), "application/x-www-form-urlencoded".to_owned()),
                ("User-Agent".to_owned(), "Mozilla/5.0 (compatible; Kageha/0.1)".to_owned())];
            let bytes = checked(backend, crate::native_io::http("https://html.duckduckgo.com/html/", "POST", &headers, format!("q={}", encode(query)).into_bytes(), cancel, TIMEOUT, RESPONSE_LIMIT))?;
            Ok(parse_ddg(&String::from_utf8_lossy(&bytes)))
        }
    }
}

/// Runs one search with the configured backend. A failing keyed backend falls back to
/// DuckDuckGo once; the result's `note` says so.
pub(crate) fn search(db: &Database, workspace: &Path, preference: Option<&str>, query: &str, cancel: &AtomicBool) -> io::Result<String> {
    let env = crate::ui_settings::model_env;
    let config = Config::resolve(&read_configs(&config_paths(workspace)), preference, env);
    let (selection, key) = {
        let connection = db.lock().map_err(|_| io::Error::other("Search settings unavailable"))?;
        let mut keys = std::collections::HashMap::new();
        let selection = select(&config, |backend| {
            let key = api_key(&connection, backend, env);
            let found = key.is_some();
            if let Some(key) = key { keys.insert(backend.id(), key); }
            found
        });
        let key = keys.remove(selection.backend.id());
        (selection, key)
    };
    let mut note = selection.note;
    let hits = match fetch(selection.backend, key.as_deref().map(String::as_str), query, cancel) {
        Ok(hits) => Ok((selection.backend, hits)),
        Err(error) if error.kind() == io::ErrorKind::Interrupted => Err(error),
        Err(error) if selection.backend != Backend::Ddg => {
            note = Some(format!("{}{error}; used ddg instead.", note.map(|note| format!("{note} ")).unwrap_or_default()));
            fetch(Backend::Ddg, None, query, cancel).map(|hits| (Backend::Ddg, hits))
        }
        Err(error) => Err(error),
    };
    match hits {
        Ok((backend, hits)) => Ok(response(backend, query, hits, note.as_deref()).to_string()),
        Err(error) if error.kind() == io::ErrorKind::Interrupted => Err(error),
        Err(error) => Err(io::Error::other(json!({
            "error_code":"web_search_unavailable","retryable":true,
            "summary":format!("Web search failed: {error}. Retry later, or open a known URL with web_fetch or the browser."),
        }).to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| pairs.iter().find(|(key, _)| *key == name).map(|(_, value)| (*value).to_owned())
    }
    const YAML: &str = "# Search settings\nbackend: auto\norder:\n- zai\n- tavily\n- brave\n- exa\n- ddg\nvision: auto\nimage_generation: auto\n";

    #[test]
    fn config_layers_files_preference_and_environment() {
        let config = Config::resolve(&[YAML.into()], None, env(&[]));
        assert_eq!((config.backend.as_str(), config.order.len()), ("auto", 5));
        assert_eq!(config.order[1], "tavily");
        let repo = "backend: brave\n".to_owned();
        assert_eq!(Config::resolve(&[YAML.into(), repo.clone()], None, env(&[])).backend, "brave");
        assert_eq!(Config::resolve(&[YAML.into(), repo.clone()], Some("ddg"), env(&[])).backend, "ddg");
        assert_eq!(Config::resolve(&[YAML.into(), repo.clone()], Some("codex"), env(&[])).backend, "brave");
        let pinned = Config::resolve(&[YAML.into(), repo], Some("ddg"), env(&[("KAGEHA_WEB_SEARCH", "tavily"), ("KAGEHA_WEB_SEARCH_ORDER", "brave, ddg")]));
        assert_eq!((pinned.backend.as_str(), pinned.order.clone()), ("tavily", vec!["brave".to_owned(), "ddg".to_owned()]));
        // Malformed files are ignored; defaults remain.
        assert_eq!(Config::resolve(&["backend: [".into()], None, env(&[])), Config::resolve(&[], None, env(&[])));
    }

    #[test]
    fn backend_selection_follows_config_and_keys() {
        let auto = Config::resolve(&[YAML.into()], None, env(&[]));
        assert_eq!(select(&auto, |backend| backend == Backend::Tavily), Selection { backend: Backend::Tavily, note: None });
        assert_eq!(select(&auto, |_| false), Selection { backend: Backend::Ddg, note: None });
        assert_eq!(select(&auto, |backend| backend == Backend::Brave).backend, Backend::Brave);
        // The default order prefers Brave over Tavily.
        assert_eq!(select(&Config::resolve(&[], None, env(&[])), |_| true).backend, Backend::Brave);
        // A pinned backend without a key falls back and says why; the key is never named.
        let pinned = Config::resolve(&["backend: tavily".into()], None, env(&[]));
        let selection = select(&pinned, |_| false);
        assert_eq!(selection.backend, Backend::Ddg);
        assert!(selection.note.unwrap().contains("tavily search has no API key"));
        assert_eq!(select(&pinned, |backend| backend == Backend::Tavily).backend, Backend::Tavily);
        let unsupported = select(&Config::resolve(&["backend: zai".into()], None, env(&[])), |_| false);
        assert_eq!(unsupported.backend, Backend::Ddg);
        assert!(unsupported.note.unwrap().contains("not available"));
    }

    #[test]
    fn keys_come_from_the_vault_or_environment() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        crate::ui_store::initialize(&db).unwrap();
        assert!(api_key(&db, Backend::Tavily, env(&[])).is_none());
        assert_eq!(api_key(&db, Backend::Tavily, env(&[("TAVILY_API_KEY", "tvly-fixture")])).as_deref().map(String::as_str), Some("tvly-fixture"));
        assert!(api_key(&db, Backend::Brave, env(&[("TAVILY_API_KEY", "tvly-fixture")])).is_none());
        assert!(api_key(&db, Backend::Ddg, env(&[("TAVILY_API_KEY", "tvly-fixture")])).is_none());
    }

    #[test]
    fn parses_tavily_results() {
        let body = json!({"query":"rust","results":[
            {"title":"Rust Programming Language","url":"https://www.rust-lang.org/","content":"A language empowering\n everyone.","score":0.9},
            {"title":"No URL","content":"skipped"},
            {"title":"Bad scheme","url":"javascript:alert(1)","content":"skipped"}]});
        assert_eq!(parse_tavily(&body), vec![Hit { title: "Rust Programming Language".into(), url: "https://www.rust-lang.org/".into(), snippet: "A language empowering everyone.".into() }]);
    }

    #[test]
    fn parses_brave_results() {
        let body = json!({"type":"search","web":{"results":[
            {"title":"The <strong>Rust</strong> Book","url":"https://doc.rust-lang.org/book/","description":"Learn <strong>Rust</strong> &amp; more."}]}});
        assert_eq!(parse_brave(&body), vec![Hit { title: "The Rust Book".into(), url: "https://doc.rust-lang.org/book/".into(), snippet: "Learn Rust & more.".into() }]);
    }

    #[test]
    fn parses_ddg_html_results_and_skips_ads() {
        let html = r#"<html><body>
<div class="result results_links results_links_deep result--ad"><h2 class="result__title"><a rel="nofollow" class="result__a" href="https://duckduckgo.com/y.js?ad_domain=example.com&amp;u3=x">Sponsored</a></h2>
<a class="result__snippet" href="https://duckduckgo.com/y.js?x">Ad text</a></div>
<div class="result results_links results_links_deep web-result"><h2 class="result__title">
<a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fwww.rust%2Dlang.org%2Flearn%3Fa%3D1&amp;rut=abc">Learn <b>Rust</b> &#x27;today&#x27;</a></h2>
<a class="result__url" href="//duckduckgo.com/l/?uddg=x">www.rust-lang.org/learn</a>
<a class="result__snippet" href="//duckduckgo.com/l/?uddg=x">Get started with <b>Rust</b> &amp; Cargo.</a></div>
<div class="result results_links web-result"><h2 class="result__title"><a rel="nofollow" class="result__a" href="https://example.org/direct">Direct</a></h2></div>
</body></html>"#;
        assert_eq!(parse_ddg(html), vec![
            Hit { title: "Learn Rust 'today'".into(), url: "https://www.rust-lang.org/learn?a=1".into(), snippet: "Get started with Rust & Cargo.".into() },
            Hit { title: "Direct".into(), url: "https://example.org/direct".into(), snippet: String::new() },
        ]);
        assert!(parse_ddg("<html>No results.</html>").is_empty());
    }

    #[test]
    fn response_is_bounded_and_deduplicated() {
        let long = "word ".repeat(400);
        let mut hits: Vec<Hit> = (0..15).map(|n| Hit { title: format!("Result {n}"), url: format!("https://example.com/{n}"), snippet: long.clone() }).collect();
        hits.insert(1, hits[0].clone());
        let hits: Vec<Hit> = hits.into_iter().map(|hit| super::hit(&hit.title, &hit.url, &hit.snippet).unwrap()).collect();
        let value = response(Backend::Ddg, "q", hits, Some("note"));
        let results = value["results"].as_array().unwrap();
        assert_eq!(results.len(), MAX_RESULTS);
        assert_eq!(results[1]["url"], "https://example.com/1");
        assert!(results.iter().all(|row| row["snippet"].as_str().unwrap().chars().count() <= MAX_SNIPPET_CHARS));
        assert_eq!((value["backend"].as_str(), value["query"].as_str(), value["note"].as_str()), (Some("ddg"), Some("q"), Some("note")));
        assert!(value["next"].as_str().unwrap().contains("web_fetch"));
        assert!(response(Backend::Tavily, "q", vec![], None).get("note").is_none());
    }
}
