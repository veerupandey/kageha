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
                messages: vec![json!({"role":"user","content":"Continue the existing draft"})],
                tools: vec![],
                role: Default::default(),
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
            ModelActivity::Failed { status: Some(429), .. },
            ModelActivity::Backoff { seconds: 1 },
            ModelActivity::Requesting { fallback: false, .. }
        ]
    ));
    server.join().unwrap();
}

#[test]
fn permanent_primary_failure_is_reported_once_and_fallback_is_reused() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1/chat/completions", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let mut models = Vec::new();
        for _ in 0..3 {
            let (mut socket, _) = listener.accept().unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" { break; }
                if let Some((name, value)) = line.split_once(':') {
                    if name.eq_ignore_ascii_case("content-length") {
                        length = value.trim().parse::<usize>().unwrap();
                    }
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let primary = body["model"] == "primary";
            models.push(body["model"].clone());
            let status = if primary { "401 Unauthorized" } else { "200 OK" };
            let body = if primary { json!({"error":"private-provider-body"}) } else {
                json!({"choices":[{"message":{"role":"assistant","content":"Recovered"}}]})
            }.to_string();
            write!(socket,"HTTP/1.1 {status}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",body.len()).unwrap();
        }
        assert_eq!(models, vec![json!("primary"), json!("backup"), json!("backup")]);
    });
    let activity = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&activity);
    let model = HttpModel::new(["primary", "backup"].into_iter().map(|name| Provider {
        url: url.clone(), key: String::new(), model: name.into(), protocol: "openai_compat".into(),
    }).collect()).unwrap().with_activity(Some(Arc::new(move |event| observed.lock().unwrap().push(event))));
    let cancelled = AtomicBool::new(false);
    for _ in 0..2 {
        let reply = model.complete(ModelRequest {
            messages: vec![json!({"role":"user","content":"Continue"})], tools: vec![], role: Default::default(),
        }, &Control { cancelled: &cancelled, deadline: Some(Instant::now() + Duration::from_secs(5)) }).unwrap();
        assert_eq!(reply.content, "Recovered");
    }
    assert_eq!(activity.lock().unwrap().iter().filter(|event|
        matches!(event, ModelActivity::Failed { status: Some(401), category: "authentication", .. })
    ).count(), 1);
    server.join().unwrap();
}

#[test]
fn unsupported_models_and_transient_outages_have_bounded_attempts() {
    // 1 attempt plus 3 retries for a transient outage; waits are skipped here.
    for (status, expected) in [(404, 1), (503, 4)] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1/chat/completions", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            for _ in 0..expected {
                let (mut socket, _) = listener.accept().unwrap();
                socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut reader = BufReader::new(socket.try_clone().unwrap());
                let mut length = 0;
                loop {
                    let mut line = String::new(); reader.read_line(&mut line).unwrap();
                    if line == "\r\n" { break; }
                    if let Some((name, value)) = line.split_once(':') {
                        if name.eq_ignore_ascii_case("content-length") { length = value.trim().parse::<usize>().unwrap(); }
                    }
                }
                reader.read_exact(&mut vec![0; length]).unwrap();
                write!(socket, "HTTP/1.1 {status} Failure\r\nRetry-After: 1\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}").unwrap();
            }
        });
        let events = Arc::new(Mutex::new(Vec::new())); let observed = events.clone();
        let model = HttpModel::new(vec![Provider { url, key: String::new(), model: "fixture".into(), protocol: "openai_compat".into() }]).unwrap()
            .with_activity(Some(Arc::new(move |event| observed.lock().unwrap().push(event))))
            .with_wait(Arc::new(|_, control: &Control<'_>| control.check()));
        let cancel = AtomicBool::new(false);
        // The deadline leaves room for the computed 1 + 4 + 16 s of (skipped) waits.
        let result = model.complete(ModelRequest { messages: vec![json!({"role":"user","content":"test"})], tools: vec![], role: Default::default() }, &Control { cancelled: &cancel, deadline: Some(Instant::now()+Duration::from_secs(60)) });
        assert!(result.is_err());
        assert_eq!(events.lock().unwrap().iter().filter(|e| matches!(e, ModelActivity::Failed { .. })).count(), expected);
        server.join().unwrap();
    }
}

#[test]
fn total_deadline_stops_a_slow_provider_without_trying_more_routes() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1/chat/completions", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (_socket, _) = listener.accept().unwrap();
        std::thread::sleep(Duration::from_millis(700));
    });
    let model = HttpModel::new(vec![Provider { url, key: String::new(), model: "slow".into(), protocol: "openai_compat".into() }]).unwrap();
    let cancel = AtomicBool::new(false); let started = Instant::now();
    let error = model.complete(ModelRequest { messages: vec![json!({"role":"user","content":"test"})], tools: vec![], role: Default::default() }, &Control { cancelled: &cancel, deadline: Some(started+Duration::from_millis(150)) }).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(started.elapsed() < Duration::from_millis(600));
    server.join().unwrap();
}
