use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

pub(crate) const MAX_RESULTS: usize = 5;
const MAX_CANDIDATES: usize = 128;

pub(crate) struct Entry {
    pub id: String,
    pub name: String,
    pub description: String,
    pub kind: &'static str,
    pub parameters: String,
}

impl Entry {
    pub(crate) fn tool(spec: &Value) -> Option<Self> {
        let id = spec["function"]["name"].as_str()?.to_owned();
        Some(Self {
            name: spec["discovery_alias"].as_str().unwrap_or(&id).into(),
            id,
            description: spec["function"]["description"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(500)
                .collect(),
            kind: "tool",
            parameters: spec["function"]["parameters"]["properties"]
                .as_object()
                .into_iter()
                .flat_map(|properties| properties.iter())
                .take(24)
                .map(|(name, value)| {
                    format!(
                        "{} {}",
                        name,
                        value["description"]
                            .as_str()
                            .unwrap_or("")
                            .chars()
                            .take(100)
                            .collect::<String>()
                    )
                })
                .collect::<Vec<_>>()
                .join(" "),
        })
    }

    fn corpus(&self) -> String {
        format!("{} {} {}", self.name, self.description, self.parameters)
    }

    pub(crate) fn result(&self) -> Value {
        json!({"id":self.id,"name":self.name,"kind":self.kind,"description":self.description})
    }
}

fn tokens(text: &str) -> Vec<String> {
    let mut token = String::new();
    let mut result = Vec::new();
    let mut previous_lower = false;
    for ch in text.chars() {
        if !ch.is_alphanumeric() || (ch.is_uppercase() && previous_lower) {
            if !token.is_empty() {
                result.push(std::mem::take(&mut token));
            }
        }
        if ch.is_alphanumeric() {
            token.extend(ch.to_lowercase());
        }
        previous_lower = ch.is_lowercase();
    }
    if !token.is_empty() {
        result.push(token);
    }
    result.into_iter().map(|word| match word.as_str() {
        // Common provider vocabulary should not depend on whether a query says
        // "mail", "email" or "mailbox", or on plural inflection. Do not merge
        // operation verbs: searching mail and sending mail have different effects.
        "mail" | "mailbox" | "emails" => "email".to_owned(),
        "messages" => "message".to_owned(),
        "integrations" => "integration".to_owned(),
        "connectors" => "connector".to_owned(),
        "plugins" => "plugin".to_owned(),
        "skills" => "skill".to_owned(),
        "tools" => "tool".to_owned(),
        _ => word,
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_vocabulary_and_plural_forms_find_matching_operations() {
        let entries = vec![
            Entry { id:"read".into(), name:"outlook::read".into(), description:"Search connected email messages".into(), kind:"tool", parameters:String::new() },
            Entry { id:"send".into(), name:"other::send".into(), description:"Send a calendar invitation".into(), kind:"tool", parameters:String::new() },
        ];
        let (_, matches) = rank(&entries, "mailbox message");
        assert_eq!(matches.first(), Some(&0));
        let (_, exact) = rank(&entries, "other::send");
        assert_eq!(exact, vec![1]);
    }

    #[test]
    fn provider_and_operation_match_before_a_repeated_single_keyword() {
        let mut entries = vec![
            Entry { id:"generic".into(), name:"other::email".into(), description:"Email email email email email".into(), kind:"tool", parameters:String::new() },
            Entry { id:"gmail_search".into(), name:"gmail::search".into(), description:"Search messages".into(), kind:"tool", parameters:String::new() },
        ];
        // A large catalog can make each individual provider/operation term common
        // while a repeated, less relevant keyword has a much higher BM25 score.
        for i in 0..10 {
            entries.push(Entry { id:format!("gmail_{i}"), name:format!("gmail::other_{i}"), description:String::new(), kind:"tool", parameters:String::new() });
            entries.push(Entry { id:format!("search_{i}"), name:format!("other::search_{i}"), description:String::new(), kind:"tool", parameters:String::new() });
        }
        let (_, matches) = rank(&entries, "search my gmail email");
        assert_eq!(matches.first(), Some(&1));
    }
}

pub(crate) fn rank(entries: &[Entry], query: &str) -> (&'static str, Vec<usize>) {
    if let Some(index) = entries.iter().position(|entry| {
        entry.id.eq_ignore_ascii_case(query) || entry.name.eq_ignore_ascii_case(query)
    }) {
        return ("exact", vec![index]);
    }
    // Conversational filler should not determine match coverage. Keep operational
    // words such as "search" and "send" distinct, since they imply different effects.
    let terms: HashSet<_> = tokens(query).into_iter().filter(|term| !matches!(term.as_str(),
        "a" | "an" | "the" | "for" | "to" | "of" | "in" | "on" | "with" | "using" | "my" | "me" | "please" | "and"
    )).collect();
    if terms.is_empty() || entries.is_empty() {
        return ("bm25", vec![]);
    }
    let corpus: Vec<_> = entries
        .iter()
        .map(|entry| tokens(&entry.corpus()))
        .collect();
    let average =
        (corpus.iter().map(Vec::len).sum::<usize>() as f64 / corpus.len() as f64).max(1.0);
    let document_frequency: HashMap<_, _> = terms
        .iter()
        .map(|term| {
            (
                term,
                corpus
                    .iter()
                    .filter(|document| document.contains(term))
                    .count() as f64,
            )
        })
        .collect();
    let mut scored: Vec<_> = corpus
        .iter()
        .enumerate()
        .filter_map(|(index, document)| {
            let normalization = 1.2 * (0.25 + 0.75 * document.len() as f64 / average);
            let mut overlap = 0;
            let score: f64 = terms
                .iter()
                .map(|term| {
                    let count = document.iter().filter(|word| *word == term).count() as f64;
                    if count == 0.0 {
                        return 0.0;
                    }
                    overlap += 1;
                    let frequency = document_frequency[term];
                    let idf =
                        (1.0 + (corpus.len() as f64 - frequency + 0.5) / (frequency + 0.5)).ln();
                    idf * count * 2.2 / (count + normalization)
                })
                .sum();
            (overlap > 0).then_some((index, score, overlap))
        })
        .collect();
    // Prefer coverage of the requested provider and operation over repetition of
    // a single keyword; BM25 remains the tie-breaker for equally complete matches.
    scored.sort_by(|left, right| right.2.cmp(&left.2).then(right.1.total_cmp(&left.1)).then(left.0.cmp(&right.0)));
    let weak = scored
        .first()
        .is_none_or(|entry| entry.2 * 2 < terms.len() || entry.1 < 0.35);
    #[cfg(feature = "zvec")]
    if weak && entries.len() <= MAX_CANDIDATES && kageha_memory::semantic::enabled() {
        let candidates: Vec<_> = entries.iter().map(Entry::corpus).collect();
        if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            if let Ok(indices) = runtime.block_on(kageha_memory::semantic::rank_catalog(
                query,
                &candidates,
                MAX_RESULTS,
            )) {
                if !indices.is_empty() {
                    return ("zvec", indices);
                }
            }
        }
    }
    let _ = (weak, MAX_CANDIDATES);
    (
        "bm25",
        scored
            .into_iter()
            .take(MAX_RESULTS)
            .map(|entry| entry.0)
            .collect(),
    )
}
