//! Optional operator-provided plugin catalog metadata. This is a read-only
//! inventory, not a plugin runtime, an installation source, or an access grant.
use serde_json::Value;
use std::{collections::BTreeSet, fs::File, io::{self, Read}, path::Path, sync::atomic::AtomicBool, time::Duration};

const MAX_BYTES: u64 = 128 * 1024;
const MAX_ENTRIES: usize = 128;
const OFFICIAL: &str = "https://raw.githubusercontent.com/openai/plugins/main/.agents/plugins/marketplace.json";

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Plugin {
    pub name: String,
    pub description: String,
    pub source: Option<String>,
}

/// Read only a small JSON array of metadata objects. Unknown fields are ignored;
/// paths, commands, secrets, and install instructions are never returned.
pub(crate) fn read(path: &Path) -> io::Result<Vec<Plugin>> {
    let mut text = String::new();
    File::open(path)?.take(MAX_BYTES + 1).read_to_string(&mut text)?;
    if text.len() as u64 > MAX_BYTES { return Err(io::Error::other("Plugin catalog is too large")); }
    let value: Value = serde_json::from_str(&text).map_err(|_| io::Error::other("Invalid plugin catalog"))?;
    let rows = value.as_array().or_else(|| value["plugins"].as_array())
        .filter(|rows| rows.len() <= MAX_ENTRIES).ok_or_else(|| io::Error::other("Invalid plugin catalog"))?;
    let mut names = BTreeSet::new();
    let mut result = Vec::new();
    for row in rows {
        let name = row["name"].as_str().filter(|s| !s.is_empty() && s.len() <= 80
            && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')))
            .ok_or_else(|| io::Error::other("Invalid plugin catalog name"))?;
        if !names.insert(name.to_ascii_lowercase()) { return Err(io::Error::other("Duplicate plugin catalog name")); }
        let description = row["description"].as_str().filter(|s| !s.trim().is_empty() && s.chars().count() <= 500
            && !s.chars().any(char::is_control)).ok_or_else(|| io::Error::other("Invalid plugin catalog description"))?;
        let source = match row.get("source") {
            None | Some(Value::Null) => None,
            Some(Value::String(source)) => {
                let url = url::Url::parse(source).map_err(|_| io::Error::other("Invalid plugin catalog source"))?;
                if url.scheme() != "https" || url.host_str().is_none() || !url.username().is_empty()
                    || url.password().is_some() || url.query().is_some() || url.fragment().is_some()
                    || source.len() > 2048 { return Err(io::Error::other("Invalid plugin catalog source")); }
                Some(url.to_string())
            }
            _ => return Err(io::Error::other("Invalid plugin catalog source")),
        };
        result.push(Plugin { name: name.into(), description: description.into(), source });
    }
    Ok(result)
}

/// The environment variable is controlled by the host operator, not by the model.
pub(crate) fn configured(fetch_official: bool, cancel: &AtomicBool) -> (&'static str, Vec<Plugin>) {
    if let Some(path) = std::env::var_os("KAGEHA_PLUGIN_CATALOG_FILE") {
        if !Path::new(&path).is_absolute() { return ("unavailable", vec![]); }
        return match read(Path::new(&path)) {
            Ok(plugins) => ("available", plugins),
            Err(_) => ("unavailable", vec![]),
        };
    }
    if !fetch_official { return ("not_checked", vec![]); }
    fetch_with(|| {
        let response = crate::native_io::http(OFFICIAL, "GET", &[], vec![], cancel,
            Duration::from_secs(5), MAX_BYTES as usize)?;
        if response.status != 200 { return Err(io::Error::other("Plugin catalog unavailable")); }
        Ok(response.bytes)
    })
}

fn fetch_with(fetch: impl FnOnce() -> io::Result<Vec<u8>>) -> (&'static str, Vec<Plugin>) {
    match fetch().and_then(|bytes| parse_official(&bytes)) {
        Ok(plugins) => ("available", plugins),
        Err(_) => ("unavailable", vec![]),
    }
}

fn parse_official(bytes: &[u8]) -> io::Result<Vec<Plugin>> {
    if bytes.len() as u64 > MAX_BYTES { return Err(io::Error::other("Plugin catalog is too large")); }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| io::Error::other("Invalid plugin catalog"))?;
    let rows = value["plugins"].as_array().filter(|v| v.len() <= MAX_ENTRIES)
        .ok_or_else(|| io::Error::other("Invalid plugin catalog"))?;
    let mut names = BTreeSet::new();
    let mut result = Vec::new();
    for row in rows {
        if row["policy"]["installation"] != "AVAILABLE" { continue; }
        let Some(name) = row["name"].as_str().filter(|name| !name.is_empty() && name.len() <= 80
            && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')) else { continue };
        if row["source"]["source"] != "local" || row["source"]["path"] != format!("./plugins/{name}")
            || !names.insert(name.to_owned()) { continue; }
        let category = row["category"].as_str().filter(|s| s.len() <= 80 && !s.chars().any(char::is_control)).unwrap_or("Other");
        result.push(Plugin { name: name.into(), description: format!("Official OpenAI plugin {name}; category {category}."),
            source: Some(format!("https://github.com/openai/plugins/tree/main/plugins/{name}")) });
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    #[test]
    fn metadata_only_catalog_is_bounded_and_validated() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        write!(file, "{}", serde_json::json!({"plugins":[{"name":"mail-tools","description":"Search mail","source":"https://example.test/plugin","command":"must-not-run"}]})).unwrap();
        let entries = read(file.path()).unwrap();
        assert_eq!(entries, vec![Plugin { name: "mail-tools".into(), description: "Search mail".into(), source: Some("https://example.test/plugin".into()) }]);
        std::fs::write(file.path(), r#"[{"name":"a","description":"Mail","source":"https://x.test/?token=secret"}]"#).unwrap();
        assert!(read(file.path()).is_err());
        std::fs::write(file.path(), format!("[{}]", " ".repeat(MAX_BYTES as usize))).unwrap();
        assert!(read(file.path()).is_err());
    }
    #[test]
    fn official_catalog_is_bounded_and_failures_are_not_reported_as_empty_results() {
        let body = serde_json::json!({"plugins":[
            {"name":"outlook-email","source":{"source":"local","path":"./plugins/outlook-email"},"policy":{"installation":"AVAILABLE"},"category":"Communication"},
            {"name":"bad","source":{"source":"local","path":"../../private"},"policy":{"installation":"AVAILABLE"}},
            {"name":"disabled","source":{"source":"local","path":"./plugins/disabled"},"policy":{"installation":"NOT_AVAILABLE"}}
        ]}).to_string().into_bytes();
        let (status, plugins) = fetch_with(|| Ok(body));
        assert_eq!(status, "available");
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "outlook-email");
        assert!(plugins[0].source.as_deref().unwrap().starts_with("https://github.com/openai/plugins/"));
        assert_eq!(fetch_with(|| Err(io::Error::new(io::ErrorKind::TimedOut, "timeout"))).0, "unavailable");
        assert_eq!(fetch_with(|| Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))).0, "unavailable");
        assert_eq!(fetch_with(|| Ok(vec![b' '; MAX_BYTES as usize + 1])).0, "unavailable");
    }
    #[test]
    #[ignore = "requires network access to the official plugin catalog"]
    fn official_live_smoke() {
        let (status, plugins) = configured(true, &AtomicBool::new(false));
        assert_eq!(status, "available");
        assert!(plugins.iter().any(|plugin| plugin.name == "gmail"));
    }
}
