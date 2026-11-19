//! Vendor-neutral import of the existing layered model catalog. No application database dependency.
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    io::{self, Read},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Default)]
pub struct Catalog {
    pub models: Vec<Value>,
    pub roles: Value,
}
/// Application-owned dotenv configuration, never installed into the process environment.
/// Process variables take precedence; earlier files take precedence over later files, as in Python.
#[derive(Default)]
pub struct Environment(Vec<PathBuf>);
impl Environment {
    pub fn load(paths: &[PathBuf]) -> io::Result<Self> {
        Self::read(paths)?;
        Ok(Self(paths.to_vec()))
    }
    fn read(paths: &[PathBuf]) -> io::Result<std::collections::BTreeMap<String, String>> {
        let mut values = std::collections::BTreeMap::new();
        for path in paths {
            match std::fs::symlink_metadata(path) {
                Ok(meta) if !meta.is_file() => {
                    return Err(io::Error::other("Model environment must be a regular file"))
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
                _ => {}
            }
            let file = match std::fs::File::open(path) {
                Ok(v) => v,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            let mut bytes = Vec::new();
            file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
            if bytes.len() > 1024 * 1024 {
                return Err(io::Error::other("Model environment exceeds limit"));
            }
            for entry in dotenvy::from_read_iter(std::io::Cursor::new(bytes)) {
                let (key, value) =
                    entry.map_err(|_| io::Error::other("Invalid model environment file"))?;
                values.entry(key).or_insert(value);
            }
        }
        Ok(values)
    }
    pub fn get(&self, name: &str) -> Option<String> {
        std::env::var(name)
            .ok()
            .or_else(|| Self::read(&self.0).ok()?.remove(name))
    }
    /// Explicit source-file lookup for an operator-directed import. Does not
    /// silently substitute the migration process's environment.
    pub fn file_value(&self, name: &str) -> io::Result<Option<String>> {
        Ok(Self::read(&self.0)?.remove(name))
    }
}
pub fn default_env_paths(workspace: &Path) -> Vec<PathBuf> {
    if let Some(path) = std::env::var_os("KAGEHA_MODEL_ENV_FILE") {
        return vec![path.into()];
    }
    vec![workspace.join(".env")]
}

pub fn supported(protocol: &str) -> bool {
    matches!(protocol, "openai_compat" | "anthropic_compat" | "codex_app_server")
}

/// Resolve a base URL or an already complete completion endpoint without vendor-name branches.
pub fn completion_url(base: &str, protocol: &str) -> io::Result<String> {
    if protocol == "codex_app_server" || !supported(protocol) && !crate::decisions::is_typed_protocol(protocol) {
        return Err(io::Error::other("Unsupported provider protocol"));
    }
    let mut url = reqwest::Url::parse(base.trim())
        .map_err(|_| io::Error::other("Invalid provider endpoint"))?;
    if !matches!(url.scheme(), "https" | "http")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.query().is_some()
    {
        return Err(io::Error::other("Invalid provider endpoint"));
    }
    let path = url.path().trim_end_matches('/');
    let endpoint = if matches!(protocol, "openai_decisions" | "gemini_decisions") {
        if path.ends_with("/decisions") {
            path.to_owned()
        } else {
            format!("{path}/decisions")
        }
    } else if protocol == "clef" {
        path.to_owned()
    } else if protocol == "system_one" {
        if path.ends_with("/systemone") {
            path.to_owned()
        } else {
            format!("{path}/systemone")
        }
    } else if protocol == "anthropic_compat" {
        if path.ends_with("/messages") {
            path.to_owned()
        } else if path.ends_with("/v1") {
            format!("{path}/messages")
        } else {
            format!("{path}/v1/messages")
        }
    } else if path.ends_with("/chat/completions") {
        path.to_owned()
    } else {
        format!("{path}/chat/completions")
    };
    url.set_path(&endpoint);
    Ok(url.to_string())
}

pub fn credential(name: &str, lookup: impl Fn(&str) -> Option<String>) -> String {
    let aliases: &[&str] = match name {
        "CLOUDFLARE_API_TOKEN" => &["CLOUDFLARE_API_KEY", "CF_API_TOKEN", "CF_API_KEY"],
        "FAL_KEY" => &["FAL_API_KEY"],
        _ => &[],
    };
    std::iter::once(name)
        .chain(aliases.iter().copied())
        .filter(|n| !n.is_empty())
        .find_map(|n| lookup(n).filter(|v| !v.is_empty()))
        .unwrap_or_default()
}

// Leave unresolved variables visible; never turn an account-scoped URL into a different valid destination.
fn expand(value: &str, lookup: &impl Fn(&str) -> Option<String>) -> String {
    let bytes = value.as_bytes();
    let mut result = String::new();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'$' {
            let start = at;
            at += 1;
            let brace = bytes.get(at) == Some(&b'{');
            if brace {
                at += 1;
            }
            let name_start = at;
            while at < bytes.len() && (bytes[at].is_ascii_alphanumeric() || bytes[at] == b'_') {
                at += 1;
            }
            let end = at;
            if end > name_start && (!brace || bytes.get(at) == Some(&b'}')) {
                if brace {
                    at += 1;
                }
                result.push_str(
                    &lookup(&value[name_start..end]).unwrap_or_else(|| value[start..at].into()),
                );
            } else {
                result.push_str(&value[start..at]);
            }
        } else {
            let c = value[at..].chars().next().unwrap();
            result.push(c);
            at += c.len_utf8();
        }
    }
    result
}

fn merge(target: &mut Value, source: Value) {
    if let (Some(a), Some(b)) = (target.as_object_mut(), source.as_object()) {
        for (key, value) in b {
            if key == "models" && value.is_array() {
                let rows = a.entry(key).or_insert_with(|| json!([]));
                if !rows.is_array() {
                    *rows = json!([]);
                }
                let rows = rows.as_array_mut().unwrap();
                for row in value.as_array().unwrap() {
                    if let Some(id) = row["id"].as_str() {
                        if let Some(old) = rows.iter_mut().find(|v| v["id"] == id) {
                            *old = row.clone();
                        } else {
                            rows.push(row.clone());
                        }
                    }
                }
            } else if let Some(old) = a.get_mut(key) {
                merge(old, value.clone());
            } else {
                a.insert(key.clone(), value.clone());
            }
        }
    } else {
        *target = source;
    }
}
impl Catalog {
    /// Build one adapter from an operator-selected catalog ID. Credentials stay outside catalog records.
    pub fn provider(
        &self,
        id: &str,
        lookup: impl Fn(&str) -> Option<String>,
    ) -> io::Result<crate::Provider> {
        let row = self
            .models
            .iter()
            .find(|v| v["id"] == id)
            .ok_or_else(|| io::Error::other("Unknown catalog model"))?;
        let protocol = row["protocol"].as_str().unwrap_or("");
        let base = row["base_url"].as_str().unwrap_or("");
        if base.contains('$') {
            return Err(io::Error::other(
                "Provider endpoint environment variable is unresolved",
            ));
        }
        let provider = crate::Provider {
            url: completion_url(base, protocol)?,
            key: credential(row["api_key_env"].as_str().unwrap_or(""), lookup),
            model: row["model"]
                .as_str()
                .ok_or_else(|| io::Error::other("Catalog model name missing"))?
                .into(),
            protocol: protocol.into(),
        };
        let endpoint = reqwest::Url::parse(&provider.url)
            .map_err(|_| io::Error::other("Invalid provider endpoint"))?;
        let local = matches!(
            endpoint.host_str(),
            Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
        );
        if !local
            && !row["api_key_env"].as_str().unwrap_or("").is_empty()
            && provider.key.is_empty()
        {
            return Err(io::Error::other(
                "Configured provider credential is unavailable",
            ));
        }
        provider.validate()?;
        Ok(provider)
    }
    pub fn load(paths: &[PathBuf], lookup: impl Fn(&str) -> Option<String>) -> io::Result<Self> {
        let mut data = json!({});
        for path in paths {
            let mut bytes = Vec::new();
            std::fs::File::open(path)?
                .take(4 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() > 4 * 1024 * 1024 {
                return Err(io::Error::other("Model catalog exceeds limit"));
            }
            let chunk: Value = serde_yaml::from_slice(&bytes)
                .map_err(|_| io::Error::other("Invalid model catalog YAML"))?;
            if !chunk.is_null() && !chunk.is_object() {
                return Err(io::Error::other("Model catalog must be a mapping"));
            }
            if chunk.is_object() {
                merge(&mut data, chunk);
            }
        }
        let mut models = Vec::new();
        let mut roles = data["roles"].as_object().cloned().unwrap_or_default();
        let explicit: BTreeSet<_> = roles.keys().cloned().collect();
        for entry in data["models"].as_array().into_iter().flatten() {
            let Some(id) = entry["id"]
                .as_str()
                .filter(|v| !v.is_empty() && v.len() <= 256)
            else {
                continue;
            };
            let provider = entry["provider"]
                .as_str()
                .ok_or_else(|| io::Error::other("Model provider missing"))?;
            let config = &data["providers"][provider];
            let mut row = entry.clone();
            row.as_object_mut().unwrap().retain(|key, _| {
                matches!(
                    key.as_str(),
                    "id" | "provider"
                        | "model"
                        | "roles"
                        | "usd_per_1k"
                        | "usd_per_1k_input"
                        | "usd_per_1k_output"
                        | "usd_per_1k_cached_input"
                        | "usd_per_1k_cache_write"
                        | "input_usd_per_1k"
                        | "output_usd_per_1k"
                        | "cached_input_usd_per_1k"
                        | "cache_write_usd_per_1k"
                        | "capabilities"
                        | "request_capabilities"
                        | "context_window"
                        | "display_name"
                        | "pricing_note"
                )
            });
            // Copy connection references, never resolved keys, into portable catalog records.
            for field in ["base_url", "protocol", "api_key_env"] {
                row[field] = entry
                    .get(field)
                    .filter(|v| v.is_string())
                    .or_else(|| config.get(field))
                    .cloned()
                    .unwrap_or(Value::Null);
            }
            row["base_url"] = json!(expand(row["base_url"].as_str().unwrap_or(""), &lookup));
            // Retained Gemini catalogs can use Google's documented OpenAI
            // compatibility surface without changing the consumer model port.
            if row["protocol"] == "gemini" {
                if let Ok(mut endpoint) =
                    reqwest::Url::parse(row["base_url"].as_str().unwrap_or(""))
                {
                    if endpoint.scheme() == "https"
                        && endpoint.host_str() == Some("generativelanguage.googleapis.com")
                        && matches!(endpoint.path().trim_end_matches('/'), "/v1beta" | "/v1")
                        && endpoint.query().is_none()
                        && endpoint.username().is_empty()
                        && endpoint.password().is_none()
                        && endpoint.fragment().is_none()
                    {
                        endpoint
                            .set_path(&format!("{}/openai", endpoint.path().trim_end_matches('/')));
                        row["base_url"] = json!(endpoint.to_string());
                        row["protocol"] = json!("openai_compat");
                    }
                }
            }
            row["label"] = json!(entry["display_name"]
                .as_str()
                .filter(|v| !v.is_empty())
                .unwrap_or(id));
            row["vision"] = json!(vision(entry));
            for role in entry["roles"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                if !explicit.contains(role) {
                    let ladder = roles.entry(role).or_insert_with(|| json!([]));
                    if let Some(ladder) = ladder.as_array_mut() {
                        if !ladder.iter().any(|v| v == id) {
                            ladder.push(json!(id));
                        }
                    }
                }
            }
            models.push(row);
        }
        Ok(Self {
            models,
            roles: Value::Object(roles),
        })
    }
}
/// The context window in tokens of a catalog or settings model entry: its optional
/// `context_window` field, else the conservative
/// [`DEFAULT_CONTEXT_WINDOW`](kageha_contracts::execution::DEFAULT_CONTEXT_WINDOW).
/// Zero, negative and non-integer values are treated as absent.
pub fn context_window(entry: &Value) -> u64 {
    entry["context_window"]
        .as_u64()
        .filter(|tokens| *tokens > 0)
        .unwrap_or(kageha_contracts::execution::DEFAULT_CONTEXT_WINDOW)
}
/// Whether a catalog or settings model entry accepts image input: its `vision` flag, else
/// `vision` in its `capabilities` list (Requirement 20.2).
pub fn vision(entry: &Value) -> bool {
    entry["vision"].as_bool().unwrap_or_else(|| {
        entry["capabilities"]
            .as_array()
            .is_some_and(|all| all.iter().any(|c| c == "vision"))
    })
}
/// Same precedence as the retained Python catalog, with an explicit file override for service hosts.
pub fn default_paths(workspace: &Path) -> Vec<PathBuf> {
    if let Some(path) = std::env::var_os("KAGEHA_MODELS_FILE") {
        return vec![path.into()];
    }
    let home = std::env::var_os("KAGEHA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|v| PathBuf::from(v).join(".kageha")));
    let root = std::env::var_os("KAGEHA_RESOURCE_ROOT")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::current_exe().ok().and_then(|p| {
                p.parent()
                    .and_then(Path::parent)
                    .map(|p| p.join("share/kageha"))
            })
        });
    let mut paths: Vec<PathBuf> = root.into_iter().map(|p| p.join("models.yaml")).collect();
    // Source execution uses resources from the explicitly selected workspace,
    // never a path captured from the checkout at compile time.
    if !paths.iter().any(|p| p.is_file()) {
        paths.push(if workspace.join("resources/models.yaml").is_file() {
            workspace.join("resources/models.yaml")
        } else {
            workspace.join("models.yaml")
        });
    }
    if let Some(home) = home {
        paths.push(home.join("models.yaml"));
    }
    paths.push(workspace.join(".kageha/models.yaml"));
    let mut seen = BTreeSet::new();
    paths
        .into_iter()
        .filter(|p| p.is_file())
        .filter(|p| seen.insert(p.canonicalize().unwrap_or(p.clone())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn layered_catalog_keeps_inherited_connections_and_gemini_compatibility() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("base.yaml");
        let overlay = dir.path().join("user.yaml");
        std::fs::write(&base,"providers:\n  gateway:\n    protocol: openai_compat\n    base_url: https://gateway.example/v1\n    api_key_env: GATEWAY_KEY\n  google:\n    protocol: gemini\n    base_url: https://generativelanguage.googleapis.com/v1beta\n    api_key_env: GEMINI_API_KEY\nmodels: []\n").unwrap();
        std::fs::write(&overlay,"models:\n  - {id: inherited, provider: gateway, model: worker}\n  - {id: overridden, provider: gateway, model: worker, base_url: https://override.example/v1}\n  - {id: google, provider: google, model: gemini-test}\n").unwrap();
        let catalog = Catalog::load(&[base, overlay], |_| None).unwrap();
        assert_eq!(catalog.models[0]["base_url"], "https://gateway.example/v1");
        assert_eq!(catalog.models[1]["base_url"], "https://override.example/v1");
        assert_eq!(catalog.models[2]["protocol"], "openai_compat");
        assert_eq!(
            catalog.models[2]["base_url"],
            "https://generativelanguage.googleapis.com/v1beta/openai"
        );
    }
    #[test]
    fn endpoints_preserve_gateway_prefixes_and_complete_urls() {
        for (base, protocol, expected) in [
            (
                "https://api.anthropic.com",
                "anthropic_compat",
                "https://api.anthropic.com/v1/messages",
            ),
            (
                "https://gateway.test/provider",
                "anthropic_compat",
                "https://gateway.test/provider/v1/messages",
            ),
            (
                "https://gateway.test/provider/v1/",
                "anthropic_compat",
                "https://gateway.test/provider/v1/messages",
            ),
            (
                "https://gateway.test/v1/messages",
                "anthropic_compat",
                "https://gateway.test/v1/messages",
            ),
            (
                "http://localhost:8787/v1",
                "openai_compat",
                "http://localhost:8787/v1/chat/completions",
            ),
            (
                "https://api.test/v1/chat/completions",
                "openai_compat",
                "https://api.test/v1/chat/completions",
            ),
        ] {
            assert_eq!(completion_url(base, protocol).unwrap(), expected);
        }
        assert!(completion_url("https://user:secret@api.test", "openai_compat").is_err());
        assert!(completion_url("https://api.test?key=secret", "openai_compat").is_err());
        assert!(completion_url("https://api.test", "gemini").is_err());
    }
    #[test]
    fn layers_override_by_id_and_preserve_metadata_and_explicit_ladders() {
        let mut data = json!({"providers":{"p":{"base_url":"https://old","protocol":"openai_compat"}},"models":[{"id":"a","provider":"p","model":"old"}]});
        merge(
            &mut data,
            json!({"providers":{"p":{"base_url":"https://new"}},"models":[{"id":"a","provider":"p","model":"new"},{"id":"b","provider":"p","model":"b"}]}),
        );
        assert_eq!(data["models"].as_array().unwrap().len(), 2);
        assert_eq!(data["models"][0]["model"], "new");
        assert_eq!(data["providers"]["p"]["protocol"], "openai_compat");
        let catalog = Catalog::load(
            &[PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/models.yaml")],
            |_| None,
        )
        .unwrap();
        assert_eq!(catalog.models.len(), 2);
        assert!(catalog.models.iter().any(|v| v["provider"] == "messages"));
        assert_eq!(
            catalog.roles["default"],
            json!(["messages-model", "local-model"])
        );
        assert_eq!(catalog.roles["worker"], json!(["local-model"]));
        assert!(catalog.provider("messages-model", |_| None).is_err());
        let provider = catalog
            .provider("messages-model", |_| Some("fixture-key".into()))
            .unwrap();
        assert_eq!(provider.url, "https://gateway.example/provider/v1/messages");
        assert_eq!(provider.model, "upstream-messages");
        assert!(catalog
            .models
            .iter()
            .any(|v| v["context_window"].as_u64().unwrap_or(0) > 0));
        let flag = |id: &str| catalog.models.iter().find(|v| v["id"] == id).unwrap()["vision"].clone();
        assert_eq!((flag("local-model"), flag("messages-model")), (json!(true), json!(false)));
    }
    #[test]
    fn the_vision_flag_overrides_the_capability_list() {
        assert!(vision(&json!({"capabilities":["vision"]})));
        assert!(!vision(&json!({"capabilities":["vision"],"vision":false})));
        assert!(vision(&json!({"vision":true})));
        assert!(!vision(&json!({})));
    }
    #[test]
    fn context_window_uses_the_entry_or_a_conservative_default() {
        assert_eq!(context_window(&json!({"context_window":200000})), 200_000);
        for missing in [json!({}), json!({"context_window":0}), json!({"context_window":"big"})] {
            assert_eq!(context_window(&missing), 128_000);
        }
    }
    #[test]
    fn environment_aliases_and_missing_destination_variables() {
        assert_eq!(
            credential("CLOUDFLARE_API_TOKEN", |n| (n == "CF_API_TOKEN")
                .then(|| "alias".into())),
            "alias"
        );
        assert_eq!(
            expand("https://api.test/${ACCOUNT}/$REGION", &|n| (n == "ACCOUNT")
                .then(|| "123".into())),
            "https://api.test/123/$REGION"
        );
    }
}
#[cfg(test)]
mod credential_source_tests {
    use super::*;
    #[test]
    fn explicit_dotenv_sources_are_read_again_instead_of_retaining_startup_keys() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "kageha-model-source-{}-{unique}",
            std::process::id()
        ));
        let name = format!("KAGEHA_DISPOSABLE_{unique}");
        std::fs::write(&path, format!("{name}=first-disposable\n")).unwrap();
        let source = Environment::load(&[path.clone()]).unwrap();
        assert_eq!(source.get(&name).as_deref(), Some("first-disposable"));
        std::fs::write(&path, format!("{name}=second-disposable\n")).unwrap();
        assert_eq!(source.get(&name).as_deref(), Some("second-disposable"));
        std::fs::remove_file(path).unwrap();
        assert!(source.get(&name).is_none());
    }
}
