//! Explicit embedding routes share model transport and credential authority.
use super::*;
use std::collections::BTreeMap;

pub trait Embedder: Send + Sync {
    fn embed(&self, inputs: &[&str], control: &Control<'_>) -> io::Result<Vec<Vec<f64>>>;
}

pub struct EmbeddingAdapter {
    pub(super) model: Arc<HttpModel>,
    pub(super) index: usize,
}

impl Embedder for EmbeddingAdapter {
    fn embed(&self, inputs: &[&str], control: &Control<'_>) -> io::Result<Vec<Vec<f64>>> {
        control.check()?;
        if tokio::runtime::Handle::try_current().is_ok() {
            return Err(io::Error::other(
                "Invoke model ports on blocking execution threads",
            ));
        }
        if inputs.is_empty()
            || inputs.len() > 32
            || inputs.iter().any(|text| text.is_empty())
            || inputs.iter().map(|text| text.len()).sum::<usize>() > 262144
        {
            return Err(io::Error::other("Embedding request exceeds input limits"));
        }
        let provider = &self.model.providers[self.index];
        let value = self.model.request_json(
            provider,
            self.model
                .references
                .get(self.index)
                .and_then(Option::as_ref),
            json!({"model":provider.model,"input":inputs,"encoding_format":"float"}),
            control,
        )?;
        vectors(&value, inputs.len())
    }
}

fn vectors(value: &Value, count: usize) -> io::Result<Vec<Vec<f64>>> {
    let invalid = || io::Error::other("Invalid embedding response");
    let rows = value["data"]
        .as_array()
        .filter(|rows| rows.len() == count)
        .ok_or_else(invalid)?;
    let mut by_index = BTreeMap::new();
    let mut dimension = None;
    for row in rows {
        let index = row["index"]
            .as_u64()
            .filter(|i| *i < count as u64)
            .ok_or_else(invalid)?;
        let mut vector: Vec<f64> =
            serde_json::from_value(row["embedding"].clone()).map_err(|_| invalid())?;
        if vector.is_empty() || vector.len() > 4096 || dimension.is_some_and(|d| d != vector.len())
        {
            return Err(invalid());
        }
        dimension = Some(vector.len());
        let norm = vector.iter().map(|x| x * x).sum::<f64>().sqrt();
        if !norm.is_finite() || norm == 0.0 {
            return Err(invalid());
        }
        vector.iter_mut().for_each(|x| *x /= norm);
        if by_index.insert(index, vector).is_some() {
            return Err(invalid());
        }
    }
    Ok(by_index.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Read, Write},
        net::TcpListener,
        sync::atomic::AtomicBool,
    };

    #[test]
    fn embedding_wire_orders_vectors_and_never_accepts_chat_calls() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1/embeddings", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(socket);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert!(line.starts_with("POST /v1/embeddings "));
            let mut length = 0;
            let mut authorized = false;
            loop {
                line.clear();
                assert_ne!(reader.read_line(&mut line).unwrap(), 0);
                if line == "\r\n" {
                    break;
                }
                if let Some((key, value)) = line.split_once(':') {
                    if key.eq_ignore_ascii_case("content-length") {
                        length = value.trim().parse().unwrap();
                    }
                    if key.eq_ignore_ascii_case("authorization") {
                        authorized = value.trim() == "Bearer fixture";
                    }
                }
            }
            assert!(authorized);
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let request: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(request["input"], json!(["objective", "excerpt"]));
            assert_eq!(request["model"], "vectors");
            let body =
                json!({"data":[{"index":1,"embedding":[0,4]},{"index":0,"embedding":[3,0]}]})
                    .to_string();
            write!(
                reader.get_mut(),
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let model = Arc::new(
            HttpModel::new(vec![Provider {
                url,
                key: "fixture".into(),
                model: "vectors".into(),
                protocol: "openai_embeddings".into(),
            }])
            .unwrap(),
        );
        let cancelled = AtomicBool::new(false);
        let control = Control {
            cancelled: &cancelled,
            deadline: Some(Instant::now() + Duration::from_secs(6)),
        };
        assert!(model.select_chat_adapter(0).is_none());
        assert!(model
            .complete(
                ModelRequest {
                    messages: vec![],
                    tools: vec![],
                    role: Default::default(),
                },
                &control
            )
            .is_err());
        let adapter = model.select_embedding_adapter(0).unwrap();
        assert_eq!(
            adapter.embed(&["objective", "excerpt"], &control).unwrap(),
            vec![vec![1.0, 0.0], vec![0.0, 1.0]]
        );
        assert!(vectors(&json!({"data":[{"index":0,"embedding":[0]}]}), 1).is_err());
        server.join().unwrap();
    }
}
