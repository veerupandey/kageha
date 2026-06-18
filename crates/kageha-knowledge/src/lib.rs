//! Reusable document retrieval. Hosts supply authorized, verified records through
//! portable types; this library has no WebUI, database or vendor dependency.
pub mod scope;
pub mod store;
use kageha_contracts::execution::Control;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
};
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Chunk {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub heading: String,
    #[serde(default)]
    pub parent_id: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Document {
    pub id: String,
    pub owner: String,
    pub source: String,
    pub sha256: String,
    pub chunks: Vec<Chunk>,
    #[serde(default)]
    pub tombstone: bool,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Hit {
    pub document_id: String,
    pub chunk_id: String,
    pub source: String,
    pub sha256: String,
    pub text: String,
    pub heading: String,
    pub score: f64,
}
pub trait Retriever {
    fn search(
        &self,
        actor: &str,
        documents: &[Document],
        query: &str,
        limit: usize,
        control: &Control<'_>,
    ) -> io::Result<Vec<Hit>>;
}
pub struct Lexical;
pub fn capabilities() -> serde_json::Value {
    serde_json::json!({"engine":"rust-lexical","lexical_search":true,"semantic_search":false,"citations":true,"vendor_indexes_imported":false})
}
fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|s| s.len() > 1)
        .map(str::to_lowercase)
        .collect()
}
impl Retriever for Lexical {
    fn search(
        &self,
        actor: &str,
        documents: &[Document],
        query: &str,
        limit: usize,
        control: &Control<'_>,
    ) -> io::Result<Vec<Hit>> {
        control.check()?;
        if actor.is_empty()
            || query.len() > 4096
            || limit == 0
            || limit > 20
            || documents.len() > 2048
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Retrieval request exceeds limits",
            ));
        }
        let terms: BTreeSet<_> = tokens(query).into_iter().collect();
        if terms.is_empty() || terms.len() > 64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Query requires 1–64 terms",
            ));
        }
        let mut rows = Vec::new();
        let mut frequencies = BTreeMap::<String, usize>::new();
        let mut bytes = 0usize;
        let mut identities = BTreeSet::new();
        for doc in documents
            .iter()
            .filter(|d| d.owner == actor && !d.tombstone)
        {
            if doc.id.is_empty()
                || doc.id.len() > 4096
                || doc.source.len() > 4096
                || !identities.insert(doc.id.clone())
            {
                return Err(io::Error::other("Invalid or ambiguous document citation"));
            }
            let mut chunk_ids = BTreeSet::new();
            if doc.chunks.len() > 512 {
                return Err(io::Error::other("Document has too many chunks"));
            }
            for chunk in &doc.chunks {
                control.check()?;
                if chunk.id.is_empty()
                    || chunk.id.len() > 256
                    || chunk.heading.len() > 4096
                    || !chunk_ids.insert(chunk.id.clone())
                {
                    return Err(io::Error::other("Invalid or ambiguous chunk citation"));
                }
                bytes = bytes.saturating_add(chunk.text.len());
                if bytes > 64 * 1024 * 1024 || chunk.text.len() > 256 * 1024 {
                    return Err(io::Error::other("Retrieval corpus exceeds limits"));
                }
                let words = tokens(&chunk.text);
                let mut counts = BTreeMap::<String, usize>::new();
                for word in &words {
                    if terms.contains(word) {
                        *counts.entry(word.clone()).or_default() += 1;
                    }
                }
                for word in counts.keys() {
                    *frequencies.entry(word.clone()).or_default() += 1;
                }
                rows.push((doc, chunk, words.len().max(1), counts));
            }
        }
        let n = rows.len() as f64;
        let average = rows.iter().map(|r| r.2).sum::<usize>().max(1) as f64 / n.max(1.0);
        let mut hits = Vec::new();
        for (doc, chunk, length, counts) in rows {
            control.check()?;
            let mut score = 0.0;
            for (word, count) in counts {
                let df = *frequencies.get(&word).unwrap_or(&0) as f64;
                let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
                let tf = count as f64;
                score += idf * tf * 2.2 / (tf + 1.2 * (0.25 + 0.75 * length as f64 / average));
            }
            if score > 0.0 {
                hits.push(Hit {
                    document_id: doc.id.clone(),
                    chunk_id: chunk.id.clone(),
                    source: doc.source.clone(),
                    sha256: doc.sha256.clone(),
                    text: chunk.text.chars().take(4000).collect(),
                    heading: chunk.heading.clone(),
                    score,
                });
            }
        }
        hits.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then(a.document_id.cmp(&b.document_id))
                .then(a.chunk_id.cmp(&b.chunk_id))
        });
        hits.truncate(limit);
        control.check()?;
        Ok(hits)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    fn doc(id: &str, owner: &str, text: &str) -> Document {
        Document {
            id: id.into(),
            owner: owner.into(),
            source: format!("knowledge/{id}"),
            sha256: "a".repeat(64),
            chunks: vec![Chunk {
                id: format!("{id}-chunk"),
                text: text.into(),
                heading: "Runbook".into(),
                parent_id: None,
            }],
            tombstone: false,
        }
    }
    #[test]
    fn acceptance_queries_retrieve_cited_records_without_peer_or_deleted_data() {
        let mut gone = doc("deleted", "alice", "rollback violet");
        gone.tombstone = true;
        let docs = vec![
            doc(
                "release",
                "alice",
                "Deployment rollback requires the violet checkpoint",
            ),
            doc("budget", "alice", "Budget limit is eight credits"),
            doc("secret", "bob", "rollback violet secret"),
            gone,
        ];
        let cancel = AtomicBool::new(false);
        let control = Control {
            cancelled: &cancel,
            deadline: None,
        };
        for (query, expected) in [
            ("deployment rollback", "release"),
            ("budget credits", "budget"),
        ] {
            let hits = Lexical.search("alice", &docs, query, 5, &control).unwrap();
            assert_eq!(hits[0].document_id, expected);
            assert!(hits
                .iter()
                .all(|h| h.document_id != "secret" && h.document_id != "deleted"));
            assert_eq!(hits[0].heading, "Runbook");
        }
        assert!(Lexical
            .search("alice", &docs, "unmatchedterm", 5, &control)
            .unwrap()
            .is_empty());
    }
    #[test]
    fn ambiguous_citations_and_expired_deadlines_are_rejected() {
        let document = doc("same", "alice", "violet checkpoint");
        let cancel = AtomicBool::new(false);
        let control = Control {
            cancelled: &cancel,
            deadline: None,
        };
        assert!(Lexical
            .search(
                "alice",
                &[document.clone(), document.clone()],
                "checkpoint",
                5,
                &control
            )
            .is_err());
        let expired = Control {
            cancelled: &cancel,
            deadline: Some(std::time::Instant::now()),
        };
        assert!(Lexical
            .search("alice", &[document], "checkpoint", 5, &expired)
            .is_err());
    }
    #[test]
    fn bounded_inputs_and_cancelled_queries_fail() {
        let cancel = AtomicBool::new(false);
        let control = Control {
            cancelled: &cancel,
            deadline: None,
        };
        assert!(Lexical.search("a", &[], " ", 5, &control).is_err());
        assert!(Lexical.search("a", &[], "term", 21, &control).is_err());
        cancel.store(true, std::sync::atomic::Ordering::Release);
        assert!(Lexical.search("a", &[], "term", 1, &control).is_err());
    }
}

/// Rebuild a portable text index without vendor embeddings. IDs derive from the
/// source hash supplied by the owner; text boundaries remain valid UTF-8.
pub fn chunks_from_text(sha256: &str, text: &str) -> io::Result<Vec<Chunk>> {
    if text.len() > 16 * 1024 * 1024 {
        return Err(io::Error::other("Text exceeds indexing limit"));
    }
    let mut chunks = Vec::new();
    let mut chars = text.chars().peekable();
    while chars.peek().is_some() {
        if chunks.len() >= 512 {
            return Err(io::Error::other("Text requires too many chunks"));
        }
        let index = chunks.len();
        let text = chars.by_ref().take(8000).collect();
        chunks.push(Chunk {
            id: format!("{sha256}:{index}"),
            text,
            heading: String::new(),
            parent_id: None,
        });
    }
    Ok(chunks)
}

/// Markdown sections retain heading ancestry. Oversized sections still use
/// bounded text chunks; original bytes remain owned by the storage adapter.
pub fn chunk_document(sha256: &str, text: &str, mode: &str) -> io::Result<Vec<Chunk>> {
    if mode == "fixed" {
        return chunks_from_text(sha256, text);
    }
    if mode != "hierarchical" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Unsupported chunking mode",
        ));
    }
    if text.len() > 16 * 1024 * 1024 {
        return Err(io::Error::other("Text exceeds indexing limit"));
    }
    let mut sections = Vec::new();
    let mut heading = Vec::<(usize, String)>::new();
    let mut buffer = String::new();
    let mut current = String::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        let level = trimmed.bytes().take_while(|b| *b == b'#').count();
        if (1..=6).contains(&level) && trimmed.as_bytes().get(level) == Some(&b' ') {
            if !buffer.is_empty() {
                sections.push((current.clone(), std::mem::take(&mut buffer)));
            }
            heading.retain(|(n, _)| *n < level);
            heading.push((level, trimmed[level + 1..].trim().to_owned()));
            current = heading
                .iter()
                .map(|(_, s)| s.as_str())
                .collect::<Vec<_>>()
                .join(" / ");
            if current.len() > 4096 || sections.len() > 512 {
                return Err(io::Error::other(
                    "Heading hierarchy exceeds indexing limits",
                ));
            }
        }
        buffer.push_str(line);
        buffer.push('\n');
    }
    if !buffer.is_empty() {
        sections.push((current, buffer));
    }
    let mut chunks = Vec::new();
    for (index, (heading, body)) in sections.into_iter().enumerate() {
        let parent = format!("{sha256}:section:{index}");
        for (part, mut chunk) in chunks_from_text(sha256, &body)?.into_iter().enumerate() {
            chunk.id = format!("{parent}:{part}");
            chunk.heading = heading.clone();
            chunk.parent_id = Some(parent.clone());
            chunks.push(chunk);
            if chunks.len() > 512 {
                return Err(io::Error::other("Text requires too many chunks"));
            }
        }
    }
    Ok(chunks)
}
#[cfg(test)]
mod hierarchy_tests {
    #[test]
    fn heading_ancestry_is_preserved_without_inventing_semantic_search() {
        let text="# Deploy\nOwner is operations.\n## Rollback\nUse violet checkpoint.\n# Billing\nEight credits.";
        let chunks = super::chunk_document("hash", text, "hierarchical").unwrap();
        assert_eq!(chunks[1].heading, "Deploy / Rollback");
        assert!(chunks[1].text.contains("violet"));
        assert!(chunks[1].parent_id.is_some());
        assert!(super::chunk_document("hash", text, "unknown").is_err());
    }
}
