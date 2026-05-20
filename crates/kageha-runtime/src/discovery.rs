use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Deserialize)]
pub struct DiscoveryRequest {
    version: u32,
    query_words: HashSet<String>,
    normalized_query: String,
    image_only: bool,
    limit: usize,
    min_score: usize,
    tools: Vec<Tool>,
}

#[derive(Deserialize)]
struct Tool {
    name: String,
    aliases: Vec<String>,
    words: HashSet<String>,
}

#[derive(Serialize)]
pub struct DiscoveryReply {
    version: u32,
    indices: Vec<usize>,
}

pub fn rank_tools(request: DiscoveryRequest) -> Result<DiscoveryReply, &'static str> {
    if request.version != 1 {
        return Err("unsupported discovery version");
    }
    let mut ranked = Vec::new();
    if !request.query_words.is_empty() {
        for (index, tool) in request.tools.iter().enumerate() {
            if request.image_only && (tool.name.contains("tts") || tool.name.contains("speech")) {
                continue;
            }
            let overlap = request.query_words.intersection(&tool.words).count();
            let name_hits = request
                .query_words
                .iter()
                .filter(|word| tool.name.contains(word.as_str()))
                .count();
            let alias_hits = request
                .query_words
                .iter()
                .filter(|word| {
                    tool.aliases
                        .iter()
                        .any(|alias| alias.contains(word.as_str()))
                })
                .count();
            let exact = std::iter::once(&tool.name)
                .chain(&tool.aliases)
                .any(|term| {
                    request.query_words.contains(term) || request.normalized_query.contains(term)
                });
            let score = overlap + 2 * (name_hits + alias_hits) + 10 * usize::from(exact);
            if score >= request.min_score.max(1) {
                ranked.push((std::cmp::Reverse(score), &tool.name, index));
            }
        }
    }
    ranked.sort_unstable();
    Ok(DiscoveryReply {
        version: 1,
        indices: ranked
            .into_iter()
            .take(request.limit.clamp(1, 20))
            .map(|(_, _, index)| index)
            .collect(),
    })
}
