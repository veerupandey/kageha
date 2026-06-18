//! Local embedding retrieval. SQLite remains the source of truth for access and deletions.
use serde_json::{json, Value};
use std::{io, sync::OnceLock, time::Duration};
use zvec_rust::*;

pub const MAX_CANDIDATES: usize = 128;

pub fn enabled() -> bool {
    std::env::var("KAGEHA_LOCAL_SEMANTIC_MEMORY").as_deref() == Ok("1")
}

fn error(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

struct EmbeddingConfig {
    endpoint: reqwest::Url,
    model: String,
    dimensions: Option<usize>,
}

impl EmbeddingConfig {
    fn local() -> io::Result<Self> {
        let file = std::env::var_os("KAGEHA_EMBEDDING_CONFIG")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|home| std::path::PathBuf::from(home).join(".kageha/models.yaml"))
            });
        let mut yaml = Value::Null;
        if let Some(path) = file {
            match std::fs::read(path) {
                Ok(bytes) => yaml = serde_yaml::from_slice(&bytes).map_err(error)?,
                Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                Err(err) => return Err(err),
            }
        }
        let configured = |env: &str, value: &Value| {
            std::env::var(env)
                .ok()
                .or_else(|| value.as_str().map(str::to_owned))
                .map(|text| text.trim().to_owned())
                .filter(|text| !text.is_empty())
        };
        let settings = &yaml["embedding"];
        let provider = configured("KAGEHA_EMBEDDING_PROVIDER", &settings["provider"]);
        let model = configured("KAGEHA_EMBEDDING_MODEL", &settings["model"]).ok_or_else(|| {
            io::Error::other("Set KAGEHA_EMBEDDING_MODEL or embedding.model in models.yaml")
        })?;
        let base = configured("KAGEHA_EMBEDDING_BASE_URL", &settings["base_url"])
            .or_else(|| {
                provider.as_ref().and_then(|id| {
                    yaml["providers"][id]["base_url"]
                        .as_str()
                        .map(str::to_owned)
                })
            })
            .ok_or_else(|| io::Error::other("Configure a local embedding base URL"))?;
        let mut endpoint = reqwest::Url::parse(&base).map_err(error)?;
        let loopback = match endpoint.host_str() {
            Some(host) => host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback()),
            _ => false,
        };
        if !loopback
            || !matches!(endpoint.scheme(), "http" | "https")
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(io::Error::other(
                "Local embeddings require a loopback URL without credentials",
            ));
        }
        let path = endpoint.path().trim_end_matches('/');
        if !path.ends_with("/embeddings") {
            endpoint.set_path(&format!("{path}/embeddings"));
        }
        let dimensions = std::env::var("KAGEHA_EMBEDDING_DIMENSIONS")
            .ok()
            .or_else(|| settings["dimensions"].as_u64().map(|n| n.to_string()))
            .map(|value| {
                value
                    .parse::<usize>()
                    .ok()
                    .filter(|n| (1..=4096).contains(n))
                    .ok_or_else(|| io::Error::other("Invalid embedding dimensions"))
            })
            .transpose()?;
        Ok(Self {
            endpoint,
            model,
            dimensions,
        })
    }
}

async fn embed(config: &EmbeddingConfig, text: &[String]) -> io::Result<Vec<Vec<f32>>> {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    let client = CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("local HTTP client")
    });
    let mut vectors = Vec::with_capacity(text.len());
    for batch in text.chunks(16) {
        let response = client
            .post(config.endpoint.clone())
            .json(&json!({"model":config.model,"input":batch}))
            .send()
            .await
            .map_err(error)?
            .error_for_status()
            .map_err(error)?;
        let data: Value = response.json().await.map_err(error)?;
        let mut indexed: Vec<_> = data["data"]
            .as_array()
            .filter(|data| data.len() == batch.len())
            .ok_or_else(|| io::Error::other("Unexpected local embedding response"))?
            .iter()
            .collect();
        indexed.sort_by_key(|row| row["index"].as_u64());
        for (index, row) in indexed.into_iter().enumerate() {
            if row["index"].as_u64() != Some(index as u64) {
                return Err(io::Error::other("Invalid embedding index"));
            }
            let vector: Vec<f32> =
                serde_json::from_value(row["embedding"].clone()).map_err(error)?;
            if vector.is_empty()
                || vector.len() > 4096
                || config
                    .dimensions
                    .is_some_and(|dimensions| vector.len() != dimensions)
                || !vector.iter().all(|value| value.is_finite())
                || vector.iter().all(|value| *value == 0.0)
            {
                return Err(io::Error::other("Invalid local embedding vector"));
            }
            vectors.push(vector);
        }
    }
    Ok(vectors)
}

pub async fn rank(query: &str, candidates: &[String], limit: usize) -> io::Result<Vec<usize>> {
    if candidates.is_empty() || limit == 0 {
        return Ok(vec![]);
    }
    if candidates.len() > MAX_CANDIDATES {
        return Err(io::Error::other("Semantic candidate limit exceeded"));
    }
    let config = EmbeddingConfig::local()?;
    let mut input = Vec::with_capacity(candidates.len() + 1);
    input.push(query.chars().take(1024).collect());
    input.extend(
        candidates
            .iter()
            .map(|body| body.chars().take(1536).collect()),
    );
    let embeddings = embed(&config, &input).await?;
    tokio::task::spawn_blocking(move || rank_vectors(&embeddings, limit))
        .await
        .map_err(error)?
}

// Cache catalog metadata only. Memory and chat excerpts use the ephemeral rank path.
pub async fn rank_catalog(
    query: &str,
    candidates: &[String],
    limit: usize,
) -> io::Result<Vec<usize>> {
    use sha2::{Digest, Sha256};
    use std::sync::{Arc, Mutex};
    type Cache = std::collections::VecDeque<([u8; 32], Arc<VectorIndex>)>;
    static CACHE: Mutex<Cache> = Mutex::new(std::collections::VecDeque::new());
    if candidates.is_empty() || candidates.len() > MAX_CANDIDATES || limit == 0 {
        return Ok(vec![]);
    }
    let config = EmbeddingConfig::local()?;
    let documents: Vec<String> = candidates
        .iter()
        .map(|body| body.chars().take(1536).collect())
        .collect();
    let key: [u8; 32] = Sha256::digest(
        serde_json::to_vec(&json!([
            config.endpoint.as_str(),
            config.model,
            config.dimensions,
            documents
        ]))
        .map_err(error)?,
    )
    .into();
    let cached = CACHE
        .lock()
        .map_err(error)?
        .iter()
        .find(|(id, _)| *id == key)
        .map(|(_, index)| Arc::clone(index));
    let query = embed(&config, &[query.chars().take(1024).collect()])
        .await?
        .remove(0);
    let index = if let Some(index) = cached {
        index
    } else {
        let vectors = embed(&config, &documents).await?;
        let index = Arc::new(
            tokio::task::spawn_blocking(move || VectorIndex::build(vectors))
                .await
                .map_err(error)??,
        );
        let mut cache = CACHE.lock().map_err(error)?;
        if let Some((_, existing)) = cache.iter().find(|(id, _)| *id == key) {
            Arc::clone(existing)
        } else {
            cache.push_front((key, Arc::clone(&index)));
            cache.truncate(4);
            index
        }
    };
    tokio::task::spawn_blocking(move || index.search(&query, limit))
        .await
        .map_err(error)?
}

fn rank_vectors(embeddings: &[Vec<f32>], limit: usize) -> io::Result<Vec<usize>> {
    VectorIndex::build(embeddings[1..].to_vec())?.search(&embeddings[0], limit)
}

struct VectorIndex {
    collection: Collection,
    _dir: tempfile::TempDir,
    vectors: Vec<Vec<f32>>,
}

impl VectorIndex {
    fn build(vectors: Vec<Vec<f32>>) -> io::Result<Self> {
        static INITIALIZED: OnceLock<std::result::Result<(), String>> = OnceLock::new();
        INITIALIZED
            .get_or_init(|| initialize(None).map_err(|e| e.to_string()))
            .as_ref()
            .map_err(error)?;
        let dimensions = vectors.first().map(Vec::len).unwrap_or(0);
        if dimensions == 0 || vectors.iter().any(|vector| vector.len() != dimensions) {
            return Err(io::Error::other("Inconsistent embedding dimensions"));
        }
        let schema = CollectionSchema::builder("memory")
            .add_vector_field(
                "embedding",
                DataType::VectorFp32,
                dimensions as _,
                IndexParams::flat(MetricType::Cosine).map_err(error)?,
            )
            .build()
            .map_err(error)?;
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("collection");
        let path = path
            .to_str()
            .ok_or_else(|| io::Error::other("Invalid index path"))?;
        let collection = Collection::create_and_open(path, &schema, None).map_err(error)?;
        let documents = vectors
            .iter()
            .enumerate()
            .map(|(index, vector)| {
                let mut doc = Doc::new().map_err(error)?;
                doc.set_pk(&index.to_string());
                doc.add_vector_f32("embedding", vector).map_err(error)?;
                Ok(doc)
            })
            .collect::<io::Result<Vec<_>>>()?;
        collection
            .insert(&documents.iter().collect::<Vec<_>>())
            .map_err(error)?;
        Ok(Self {
            collection,
            _dir: dir,
            vectors,
        })
    }

    fn search(&self, vector: &[f32], limit: usize) -> io::Result<Vec<usize>> {
        if vector.len() != self.vectors[0].len() {
            return Err(io::Error::other("Query and index dimensions differ"));
        }
        let query = SearchQuery::new("embedding", vector, limit.min(self.vectors.len()) as _)
            .map_err(error)?;
        let results = self.collection.query(&query).map_err(error)?;
        let norm = |values: &[f32]| {
            values
                .iter()
                .map(|&value| f64::from(value).powi(2))
                .sum::<f64>()
                .sqrt()
        };
        let query_norm = norm(vector);
        Ok(results
            .iter()
            .filter_map(|doc| {
                let index: usize = doc.get_pk()?.parse().ok()?;
                let candidate = self.vectors.get(index)?;
                let dot: f64 = vector
                    .iter()
                    .zip(candidate)
                    .map(|(&left, &right)| f64::from(left) * f64::from(right))
                    .sum();
                (dot / (query_norm * norm(candidate)) >= 0.35).then_some(index)
            })
            .collect())
    }
}
