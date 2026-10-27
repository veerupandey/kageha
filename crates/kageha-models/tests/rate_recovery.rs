use kageha_contracts::execution::{Control, ModelPort, ModelRequest};
use kageha_models::{HttpModel, ModelActivity, Provider};
use serde_json::json;
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    sync::{atomic::AtomicBool, Arc, Mutex},
    time::{Duration, Instant},
};

#[test]
fn rate_limit_retries_only_the_model_request_and_honors_retry_after() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/v1/chat/completions",
        listener.local_addr().unwrap()
    );
    let server = std::thread::spawn(move || {
        let mut arrivals = Vec::new();
        for index in 0..2 {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            arrivals.push(Instant::now());
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
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(
                body["messages"][0]["content"],
                "Continue the existing draft"
            );
            let (status, headers, body) = if index == 0 {
                (
                    "429 Too Many Requests",
                    "Retry-After: 1\r\n",
                    json!({"error":"temporary"}).to_string(),
                )
            } else {
                ("200 OK","",json!({"choices":[{"message":{"role":"assistant","content":"Recovered","tool_calls":[]}}]}).to_string())
            };
            write!(socket,"HTTP/1.1 {status}\r\nContent-Type: application/json\r\nConnection: close\r\n{headers}Content-Length: {}\r\n\r\n{body}",body.len()).unwrap();
        }
        assert!(arrivals[1].duration_since(arrivals[0]) >= Duration::from_secs(1));
    });
    let activity = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&activity);
    let model = HttpModel::new(vec![Provider {
        url,
        key: String::new(),
        model: "fixture".into(),
        protocol: "openai_compat".into(),
    }])
    .unwrap()
    .with_activity(Some(Arc::new(move |event| observed.lock().unwrap().push(event))));
    let cancelled = AtomicBool::new(false);
    let reply = model
        .complete(
            ModelRequest {
                role: Default::default(),
                messages: vec![json!({"role":"user","content":"Continue the existing draft"})],
                tools: vec![],
            },
            &Control {
                cancelled: &cancelled,
                deadline: Some(Instant::now() + Duration::from_secs(8)),
            },
        )
        .unwrap();
    assert_eq!(reply.content, "Recovered");
    assert!(reply.tool_calls.is_empty());
    assert!(matches!(
        activity.lock().unwrap().as_slice(),
        [
            ModelActivity::Requesting { fallback: false, .. },
            ModelActivity::Backoff { seconds: 1 },
            ModelActivity::Requesting { fallback: false, .. }
        ]
    ));
    server.join().unwrap();
}
