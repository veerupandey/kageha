use kageha_contracts::{
    execution::{Control, ModelPort, ModelRequest, ModelUsage},
    request_budget::{self, BudgetPort},
};
use kageha_models::{HttpModel, Provider};
use serde_json::{json, Value};
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    net::TcpListener,
    sync::{atomic::AtomicBool, Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Default)]
struct Recorder(Mutex<Vec<ModelUsage>>);
impl BudgetPort for Recorder {
    fn before_model_invocation(&self) -> io::Result<()> {
        Ok(())
    }
    fn record_usage(&self, usage: &ModelUsage) -> io::Result<()> {
        self.0.lock().unwrap().push(*usage);
        Ok(())
    }
}

/// Serves one response and returns the request body it received.
fn serve(content_type: &'static str, body: String) -> (String, std::thread::JoinHandle<Value>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1/fixture", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut reader = BufReader::new(socket.try_clone().unwrap());
        let mut length = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                if name.eq_ignore_ascii_case("content-length") {
                    length = value.trim().parse::<usize>().unwrap();
                }
            }
        }
        let mut request = vec![0; length];
        reader.read_exact(&mut request).unwrap();
        write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",body.len()).unwrap();
        serde_json::from_slice(&request).unwrap()
    });
    (url, server)
}

fn complete(model: HttpModel, recorder: &Arc<Recorder>) -> String {
    let cancelled = AtomicBool::new(false);
    let reply = request_budget::scoped(recorder.clone(), || {
        model.complete(
            ModelRequest { messages: vec![json!({"role":"user","content":"Hello"})], tools: vec![], role: Default::default() },
            &Control { cancelled: &cancelled, deadline: Some(Instant::now() + Duration::from_secs(5)) },
        )
    })
    .unwrap();
    reply.content.as_str().unwrap().to_owned()
}

fn provider(url: String, protocol: &str) -> Provider {
    Provider { url, key: String::new(), model: "fixture".into(), protocol: protocol.into() }
}

#[test]
fn openai_response_records_cached_tokens_per_call() {
    let body = json!({"choices":[{"message":{"role":"assistant","content":"Hi"}}],
        "usage":{"prompt_tokens":1500,"completion_tokens":2,"prompt_tokens_details":{"cached_tokens":1280}}});
    let (url, server) = serve("application/json", body.to_string());
    let recorder = Arc::new(Recorder::default());
    let model = HttpModel::new(vec![provider(url, "openai_compat")]).unwrap();
    assert_eq!(complete(model, &recorder), "Hi");
    server.join().unwrap();
    assert_eq!(*recorder.0.lock().unwrap(), vec![ModelUsage {
        input_tokens: Some(1500), output_tokens: Some(2), cached_tokens: Some(1280) }]);
}

#[test]
fn openai_stream_requests_usage_and_records_cached_tokens() {
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"},\"finish_reason\":\"stop\"}]}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":64,\"completion_tokens\":1,\"prompt_tokens_details\":{\"cached_tokens\":32}}}\n\n",
        "data: [DONE]\n\n"
    );
    let (url, server) = serve("text/event-stream", body.into());
    let recorder = Arc::new(Recorder::default());
    let model = HttpModel::new(vec![provider(url, "openai_compat")])
        .unwrap()
        .with_activity(Some(Arc::new(|_: kageha_models::ModelActivity| {})));
    assert_eq!(complete(model, &recorder), "Hi");
    let request = server.join().unwrap();
    assert_eq!(request["stream"], true);
    assert_eq!(request["stream_options"]["include_usage"], true);
    assert_eq!(recorder.0.lock().unwrap()[0].cached_tokens, Some(32));
}

#[test]
fn anthropic_response_records_cache_read_tokens() {
    let body = json!({"role":"assistant","stop_reason":"end_turn","content":[{"type":"text","text":"Hi"}],
        "usage":{"input_tokens":12,"output_tokens":3,"cache_creation_input_tokens":0,"cache_read_input_tokens":2048}});
    let (url, server) = serve("application/json", body.to_string());
    let recorder = Arc::new(Recorder::default());
    let model = HttpModel::new(vec![provider(url, "anthropic_compat")]).unwrap();
    assert_eq!(complete(model, &recorder), "Hi");
    assert!(server.join().unwrap().get("stream_options").is_none());
    assert_eq!(*recorder.0.lock().unwrap(), vec![ModelUsage {
        input_tokens: Some(12), output_tokens: Some(3), cached_tokens: Some(2048) }]);
}

#[test]
fn unreported_usage_records_an_empty_call() {
    let body = json!({"choices":[{"message":{"role":"assistant","content":"Hi"}}]});
    let (url, server) = serve("application/json", body.to_string());
    let recorder = Arc::new(Recorder::default());
    let model = HttpModel::new(vec![provider(url, "openai_compat")]).unwrap();
    assert_eq!(complete(model, &recorder), "Hi");
    server.join().unwrap();
    assert_eq!(*recorder.0.lock().unwrap(), vec![ModelUsage::default()]);
}
