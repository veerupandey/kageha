//! Candidates for ordinary browser turns. Only operator text supplies input values.
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

use crate::{browser_client::Client, ui_tools::Tools};
use kageha_contracts::execution::ModelRequest;

pub(super) fn operator_intent(host: &Tools<'_>) -> Option<String> {
    use kageha_runtime::lifecycle::RunStore;
    let store = crate::runtime_host::store(host.db).ok()?;
    let run = store.find_turn(host.turn).ok()??;
    let intent = kageha_runtime::review_context::user_intent(store.as_ref(), &run).ok()?;
    let source: Value = serde_json::from_str(&intent).ok()?;
    source["user_steering"]
        .as_array()
        .and_then(|items| items.last())
        .unwrap_or(&source["user_objective"])
        .as_str()
        .map(str::to_owned)
}

pub(super) fn context(
    host: &Tools<'_>,
    client: &mut Client,
    request: &ModelRequest,
) -> Option<(Value, BTreeMap<String, Value>, String, Value)> {
    let intent = operator_intent(host)?;
    let facts = values(&intent);
    if facts.is_empty()
        && !intent.split(|c: char| !c.is_alphanumeric()).any(|w| {
            [
                "open", "visit", "navigate", "follow", "expand", "switch", "next", "check",
                "enable", "filter",
            ]
            .iter()
            .any(|v| w.eq_ignore_ascii_case(v))
        })
    {
        return None;
    }
    let selection = match client.selection() {
        Ok(v) if v["target_id"].is_string() => v,
        _ => return None,
    };
    let scope = crate::browser_client::target(&selection);
    let pending = client.call(
        "pending_actions",
        json!({"browser_id":selection["browser_id"],"target_id":selection["target_id"]}),
        None,
    );
    if !pending
        .ok()
        .and_then(|v| v["receipts"].as_array().map(Vec::is_empty))
        .unwrap_or(false)
    {
        super::store_route(
            host.db,
            &host.browser_decisions.state_key,
            json!({"status":"pending_action","minimum_score":null}),
        )
        .ok();
        return None;
    }
    // A successful read just returned this exact observation. After input take a
    // fresh snapshot in the same selected tab before considering another step.
    let last_result = request
        .messages
        .iter()
        .rev()
        .find(|m| m["role"] == "tool")
        .and_then(|m| serde_json::from_str::<Value>(m["content"].as_str()?).ok());
    if last_result
        .as_ref()
        .is_some_and(|v| v.get("error").is_some() || v.get("error_code").is_some())
    {
        return None;
    }
    let cached = last_result
        .as_ref()
        .and_then(|v| {
            host.observations
                .lock()
                .ok()?
                .get(v["observation_id"].as_str()?)
                .cloned()
        })
        .filter(|v| {
            v["target_id"] == scope["target_id"]
                && v["binding_id"] == selection["binding_id"]
                && v["service_epoch"] == client.epoch
        });
    let snapshot = if let Some(snapshot) = cached {
        snapshot
    } else {
        let anchored = host
            .observations
            .lock()
            .ok()
            .is_some_and(|cache| cache.matches_selected(&selection, &client.epoch));
        if !anchored
            && !last_result.as_ref().is_some_and(|v| {
                v["tabs"].is_array()
                    && v["binding_id"] == selection["binding_id"]
                    && v["target_id"] == scope["target_id"]
            })
        {
            return None;
        }
        let read = json!({"id":format!("decision-observe-{}",uuid::Uuid::new_v4()),"name":"browser_snapshot","arguments":{}});
        let Some(snapshot) = crate::native_chat::ToolHost::invoke(host, &read)
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .and_then(|v| {
                host.observations
                    .lock()
                    .ok()?
                    .get(v["observation_id"].as_str()?)
                    .cloned()
            })
            .filter(|v| {
                v["binding_id"] == selection["binding_id"] && v["target_id"] == scope["target_id"]
            })
        else {
            return None;
        };
        snapshot
    };
    if snapshot["browser_access"].is_object() {
        return None;
    }
    Some((snapshot, facts, intent, selection))
}

pub(super) fn values(intent: &str) -> BTreeMap<String, Value> {
    let mut facts = BTreeMap::new();
    let mut chars = intent.char_indices().peekable();
    while let Some((start, open)) = chars.next() {
        let close = match open {
            '"' => '"',
            '“' => '”',
            '‘' => '’',
            '\'' if start == 0 || !intent[..start].ends_with(char::is_alphanumeric) => '\'',
            _ => continue,
        };
        let begin = start + open.len_utf8();
        let Some((end, _)) = chars.by_ref().find(|(_, c)| *c == close) else {
            break;
        };
        let value = &intent[begin..end];
        if !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control) {
            facts.insert(format!("user[{begin}..{end}]"), json!(value));
        }
        if facts.len() >= 32 {
            break;
        }
    }
    for entry in intent.split(['\n', ',', ';']).take(32) {
        if let Some((key, value)) = entry.trim().split_once([':', '=']) {
            let value = value.trim();
            if !key.is_empty()
                && key.len() <= 64
                && key.chars().all(|c| c.is_alphanumeric() || c == ' ')
                && !value.starts_with("//")
                && !value.is_empty()
                && value.len() <= 512
                && !value.contains(['"', '\'', '“', '”'])
            {
                facts.insert(key.to_owned(), json!(value));
            }
        }
    }
    if facts.is_empty() {
        let lower = intent.to_ascii_lowercase();
        if let Some(start) = lower.find("search for ") {
            let rest = &intent[start + "search for ".len()..];
            let end = rest
                .to_ascii_lowercase()
                .find(" and ")
                .unwrap_or(rest.len());
            let term = rest[..end].trim().trim_end_matches(['.', '!']);
            if !term.is_empty() && term.len() <= 256 && !term.chars().any(char::is_control) {
                facts.insert("explicit search query".to_owned(), json!(term));
            }
        }
    }
    facts
}

fn names_target(intent: &str, target: &str) -> bool {
    !target.is_empty()
        && intent.match_indices(target).any(|(start, _)| {
            !intent[..start].ends_with(char::is_alphanumeric)
                && !intent[start + target.len()..].starts_with(char::is_alphanumeric)
        })
}

fn named_url(intent: &str, url: &str) -> bool {
    intent
        .split_whitespace()
        .any(|token| token.trim_matches(['"', '\'', '<', '>', '“', '”']) == url)
}

fn next_page(current: &url::Url, next: &url::Url) -> bool {
    if current.origin() != next.origin()
        || current.path() != next.path()
        || next.fragment().is_some()
    {
        return false;
    }
    let split = |url: &url::Url| {
        let (page, others): (Vec<_>, Vec<_>) = url
            .query_pairs()
            .into_owned()
            .partition(|(k, _)| k == "page");
        let index = match page.as_slice() {
            [] => Some(1u32),
            [(_, n)] => n.parse::<u32>().ok(),
            _ => None,
        };
        (index, others)
    };
    let (index, filters) = split(current);
    let (target, new_filters) = split(next);
    index
        .and_then(|n| n.checked_add(1))
        .is_some_and(|n| Some(n) == target)
        && filters == new_filters
}

pub(super) fn candidates(
    intent: &str,
    snapshot: &Value,
    selection: &Value,
    controls: &mut Vec<Value>,
    facts: &mut BTreeMap<String, Value>,
) -> BTreeSet<String> {
    let mut grants = BTreeSet::new();
    let lower = intent.to_lowercase();
    let words: BTreeSet<_> = lower.split(|c: char| !c.is_alphanumeric()).collect();
    let check = ["check", "enable", "filter"]
        .iter()
        .any(|word| words.contains(word));
    let navigate = ["open", "visit", "navigate", "follow", "expand", "switch"]
        .iter()
        .any(|word| words.contains(word));
    let base = snapshot["url"]
        .as_str()
        .and_then(|s| url::Url::parse(s).ok());
    controls.retain(|c| {
        let name = c["name"].as_str().unwrap_or("");
        let named = names_target(&lower, &name.to_lowercase());
        if matches!(c["role"].as_str(), Some("checkbox" | "radio" | "switch")) {
            if check && named {
                facts.insert(format!("requested filter: {name}"), json!(true));
                return true;
            }
            return false;
        }
        if navigate || words.contains("next") {
            let link = c["role"] == "link"
                && c["tag"] == "A"
                && c["href"]
                    .as_str()
                    .and_then(|href| base.as_ref()?.join(href).ok())
                    .is_some_and(|url| {
                        matches!(url.scheme(), "https" | "http")
                            && (named_url(intent, url.as_str())
                                || (words.contains("next")
                                    && base
                                        .as_ref()
                                        .is_some_and(|current| next_page(current, &url))))
                    });
            if link || (named && words.contains("expand") && c["tag"] == "SUMMARY") {
                if let Some(target) = c["ref"].as_str() {
                    grants.insert(target.to_owned());
                }
            }
        }
        true
    });
    if words.contains("tab") && navigate {
        // Resolve the destination in the switch clause, including the current tab.
        // Two matches are ambiguous even if just one of them is inactive.
        let start = intent.to_ascii_lowercase().rfind("switch ").unwrap_or(0);
        let clause = &intent[start..];
        let tab_clause = clause.to_lowercase();
        let matches: Vec<_> = selection["tabs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|tab| {
                names_target(
                    &tab_clause,
                    &tab["title"].as_str().unwrap_or("").to_lowercase(),
                ) || named_url(clause, tab["url"].as_str().unwrap_or(""))
            })
            .take(2)
            .collect();
        if let [tab] = matches.as_slice() {
            if let Some(id) = tab["target_id"]
                .as_str()
                .filter(|_| tab["target_id"] != selection["target_id"])
            {
                let target = format!("tab:{id}");
                grants.insert(target.clone());
                controls.push(json!({"ref":target,"name":tab["title"],"role":"managed_tab"}));
            }
        }
    }
    grants
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn values_have_operator_sources() {
        let facts = values("Search for ‘wireless keyboards’ and filter by 'In stock'");
        assert!(facts.values().any(|v| v == "wireless keyboards"));
        assert!(facts.values().any(|v| v == "In stock"));
        assert!(values("look at this page").is_empty());
        assert_eq!(values("name: Alice")["name"], "Alice");
    }
    #[test]
    fn pagination_preserves_origin_path_and_filters() {
        let page = url::Url::parse("https://example.test/search?q=test&page=2").unwrap();
        for (href, expected) in [
            ("?q=test&page=3", true),
            ("?q=other&page=3", false),
            ("/delete?q=test&page=3", false),
            ("//other.test/search?q=test&page=3", false),
            ("?q=test&page=2", false),
            ("?q=test&page=3&page=4", false),
        ] {
            assert_eq!(next_page(&page, &page.join(href).unwrap()), expected);
        }
    }
    #[test]
    fn page_labels_cannot_grant_clicks_and_ambiguous_tabs_stay_regular() {
        let mut controls = vec![json!({"ref":"e1","role":"button","tag":"BUTTON","name":"Next"})];
        let selection = json!({"target_id":"first","tabs":[{"target_id":"a","title":"Orders"},{"target_id":"b","title":"Orders"}]});
        assert!(candidates(
            "Next, switch to tab Orders",
            &json!({"url":"https://example.test/"}),
            &selection,
            &mut controls,
            &mut BTreeMap::new()
        )
        .is_empty());
    }
}
